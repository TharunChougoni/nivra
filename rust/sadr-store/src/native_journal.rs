//! Cooperative original-journal owner. No file creation, schema initialization or session authority.
use crate::journal::{self, JournalSource, ModelCaps, ModelJournal};
use rusqlite::{Connection, OpenFlags};
use std::{
    fmt, fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expected {
    pub device: u64,
    pub inode: u64,
    pub high_water: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    Busy,
    Journal(journal::JournalError),
    Call(crate::native_call::Error),
    Lease(crate::native_lease::Error),
    Blocked,
    Grant(crate::native_grant::Error),
    Stream(sadr_core::native_stream::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => f.write_str("native_original_invalid"),
            Self::Busy => f.write_str("native_original_busy"),
            Self::Journal(e) => e.fmt(f),
            Self::Call(e) => e.fmt(f),
            Self::Lease(e) => e.fmt(f),
            Self::Blocked => f.write_str("native_original_blocked"),
            Self::Grant(e) => e.fmt(f),
            Self::Stream(e) => e.fmt(f),
        }
    }
}
impl std::error::Error for Error {}
fn private(m: &fs::Metadata, owner: u32) -> bool {
    m.is_file() && m.uid() == owner && m.mode() & 0o7777 == 0o600 && m.nlink() == 1
}
fn directory(path: &Path) -> Result<fs::File, Error> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error::Invalid);
    }
    for p in path.ancestors() {
        if p.file_name().is_some_and(|n| {
            [".hermes", ".grok", ".codex", ".openclaw"]
                .iter()
                .any(|s| n == *s)
        }) || p.join(".git").symlink_metadata().is_ok()
        {
            return Err(Error::Invalid);
        }
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut file = fs::File::from(
        rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Error::Invalid)?,
    );
    for c in path.components() {
        if let Component::Normal(n) = c {
            file = fs::File::from(
                rustix::fs::openat(&file, n, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Error::Invalid)?,
            );
        }
    }
    let m = file.metadata().map_err(|_| Error::Invalid)?;
    let uid = fs::metadata("/proc/self")
        .map_err(|_| Error::Invalid)?
        .uid();
    if m.uid() != uid || m.mode() & 0o7777 != 0o700 {
        return Err(Error::Invalid);
    }
    Ok(file)
}
fn sidecars(path: &Path, uid: u32) -> Result<(), Error> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_os_string();
        name.push(suffix);
        match fs::symlink_metadata(PathBuf::from(name)) {
            Ok(m) if suffix != "-journal" && private(&m, uid) => {}
            Ok(_) => return Err(Error::Invalid),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Invalid),
        }
    }
    Ok(())
}
pub struct Owner {
    db: Connection,
    baseline: Option<ModelJournal>,
    blocked: bool,
    path: PathBuf,
    anchor: fs::File,
    parent: (u64, u64),
    expected: Expected,
    account: String,
    caps: ModelCaps,
}
impl fmt::Debug for Owner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OriginalNativeJournalOwner(redacted, no authority)")
    }
}
impl Owner {
    /// Expected inode/high-water must come from trusted original-journal provenance.
    /// Locks serialize cooperating Rust owners only; SQLite transactions remain authoritative.
    pub fn open(
        path: &Path,
        expected: Expected,
        account: &str,
        caps: ModelCaps,
    ) -> Result<Self, Error> {
        if expected.high_water < 0 {
            return Err(Error::Invalid);
        }
        let parent = directory(path.parent().ok_or(Error::Invalid)?)?;
        let p = parent.metadata().map_err(|_| Error::Invalid)?;
        let fd = rustix::fs::openat(
            &parent,
            path.file_name().ok_or(Error::Invalid)?,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error::Invalid)?;
        let anchor = fs::File::from(fd);
        let m = anchor.metadata().map_err(|_| Error::Invalid)?;
        if !private(&m, p.uid())
            || m.len() == 0
            || (m.dev(), m.ino()) != (expected.device, expected.inode)
        {
            return Err(Error::Invalid);
        }
        anchor.try_lock().map_err(|e| match e {
            fs::TryLockError::WouldBlock => Error::Busy,
            _ => Error::Invalid,
        })?;
        sidecars(path, p.uid())?;
        let named = path.symlink_metadata().map_err(|_| Error::Invalid)?;
        if !private(&named, p.uid())
            || (named.dev(), named.ino()) != (expected.device, expected.inode)
        {
            return Err(Error::Invalid);
        }
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| Error::Invalid)?;
        db.busy_timeout(std::time::Duration::ZERO)
            .map_err(|_| Error::Invalid)?;
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .map_err(|_| Error::Invalid)?;
        let mut owner = Self {
            baseline: None,
            blocked: false,
            db,
            path: path.into(),
            anchor,
            parent: (p.dev(), p.ino()),
            expected,
            account: account.into(),
            caps,
        };
        owner.baseline = Some(owner.snapshot()?);
        Ok(owner)
    }
    pub fn verify_file(&self) -> Result<(), Error> {
        let parent = directory(self.path.parent().ok_or(Error::Invalid)?)?;
        let p = parent.metadata().map_err(|_| Error::Invalid)?;
        if (p.dev(), p.ino()) != self.parent
            || self.db.path().map(Path::new) != Some(self.path.as_path())
        {
            return Err(Error::Invalid);
        }
        for m in [
            self.path.symlink_metadata().map_err(|_| Error::Invalid)?,
            self.anchor.metadata().map_err(|_| Error::Invalid)?,
        ] {
            if !private(&m, p.uid())
                || m.len() == 0
                || (m.dev(), m.ino()) != (self.expected.device, self.expected.inode)
            {
                return Err(Error::Invalid);
            }
        }
        sidecars(&self.path, p.uid())?;
        Ok(())
    }
    pub fn snapshot(&self) -> Result<ModelJournal, Error> {
        if self.blocked {
            return Err(Error::Blocked);
        }
        self.verify_file()?;
        let result = journal::reopen_model_usage(
            &self.db,
            &self.account,
            self.caps,
            self.caps,
            JournalSource::Original {
                high_water: self.expected.high_water,
            },
        )
        .map_err(Error::Journal)?;
        self.verify_file()?;
        if self.baseline.as_ref().is_some_and(|b| b != &result) {
            return Err(Error::Invalid);
        }
        Ok(result)
    }
    pub fn invocation_authorized(&self) -> bool {
        false
    }
}

impl Owner {
    pub fn blocked(&self) -> bool {
        self.blocked
    }
    fn guard(&mut self) -> Result<ModelJournal, Error> {
        let result = self.snapshot();
        if result.is_err() {
            self.blocked = true;
        }
        result
    }
    fn adopt(
        &mut self,
        attempts: &[journal::ModelAttempt],
        high: i64,
        metric_rows: u64,
    ) -> Result<(), Error> {
        self.verify_file()?;
        let actual = journal::reopen_model_usage(
            &self.db,
            &self.account,
            self.caps,
            self.caps,
            JournalSource::Original { high_water: high },
        )
        .map_err(Error::Journal)?;
        if actual.attempts() != attempts
            || self.baseline.as_ref().is_none_or(|b| {
                metric_rows != actual.metric_rows()
                    || b.extraction_rows() != actual.extraction_rows()
            })
        {
            return Err(Error::Invalid);
        }
        self.verify_file()?;
        self.expected.high_water = high;
        self.baseline = Some(actual);
        Ok(())
    }
    /// Original snapshot must be the owner's current exact snapshot; no rebase/adoption.
    /// Errors latch this owner locally. A fresh owner still requires trusted provenance;
    /// this latch is not a persistent controller incident lock or restart authority.
    pub fn prepare(
        &mut self,
        input: crate::native_call::Input<'_>,
    ) -> Result<Option<crate::native_call::Prepared>, Error> {
        if input.eligible_count == 0 {
            return Ok(None);
        }
        let before = self.guard()?;
        let result = (|| {
            if input.original != &before {
                return Err(Error::Invalid);
            }
            let prepared = crate::native_call::prepare(&mut self.db, input)
                .map_err(Error::Call)?
                .ok_or(Error::Invalid)?;
            let mut expected = before.attempts().to_vec();
            expected.push(prepared.reserved_attempt().clone());
            self.adopt(
                &expected,
                prepared.reserved_attempt().id(),
                before.metric_rows(),
            )?;
            Ok(Some(prepared))
        })();
        if result.is_err() {
            self.blocked = true;
        }
        result
    }
    pub fn complete(
        &mut self,
        expected: &journal::ModelAttempt,
        outcome: &journal::TerminalOutcome,
        lease: &crate::native_lease::Lease,
    ) -> Result<(), Error> {
        let before = self.guard()?;
        let result = (|| {
            lease.verify().map_err(Error::Lease)?;
            let mut rows = before.attempts().to_vec();
            let row = rows
                .iter_mut()
                .find(|a| a.id() == expected.id())
                .ok_or(Error::Invalid)?;
            if row != expected {
                return Err(Error::Invalid);
            }
            *row = journal::terminal_projection(expected, outcome).map_err(Error::Journal)?;
            journal::finish_model_attempt(&mut self.db, expected, outcome)
                .map_err(Error::Journal)?;
            self.adopt(&rows, self.expected.high_water, before.metric_rows())?;
            lease.verify().map_err(Error::Lease)
        })();
        if result.is_err() {
            self.blocked = true;
        }
        result
    }
}

impl Owner {
    /// Record an original row's measured latency while preserving billing/reservations.
    /// Measurement collision or guard failure latches this owner; no refund or retry.
    pub fn record_latency(
        &mut self,
        expected: &journal::ModelAttempt,
        latency_ms: u64,
        lease: &crate::native_lease::Lease,
    ) -> Result<(), Error> {
        let before = self.guard()?;
        let result = (|| {
            lease.verify().map_err(Error::Lease)?;
            if before.attempts().iter().find(|a| a.id() == expected.id()) != Some(expected) {
                return Err(Error::Invalid);
            }
            let exists: bool = self
                .db
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM model_metrics WHERE call_id=?1)",
                    [expected.id()],
                    |r| r.get(0),
                )
                .map_err(|_| Error::Invalid)?;
            let metric_rows = before
                .metric_rows()
                .checked_add(u64::from(!exists))
                .ok_or(Error::Invalid)?;
            journal::record_model_latency(&mut self.db, expected, latency_ms)
                .map_err(Error::Journal)?;
            self.adopt(before.attempts(), self.expected.high_water, metric_rows)?;
            lease.verify().map_err(Error::Lease)
        })();
        if result.is_err() {
            self.blocked = true;
        }
        result
    }
}

impl Owner {
    /// Reads only the explicit independent namespace associated with the held lease.
    /// No eligible declaration returns before grant/file/owner checks; never clears a latch.
    pub fn prepare_from_grant(
        &mut self,
        input: crate::native_call::FileInput<'_>,
        reader: &mut crate::native_grant::Reader,
    ) -> Result<Option<crate::native_call::Prepared>, Error> {
        if input.eligible_count == 0 {
            return Ok(None);
        }
        let result = (|| {
            let original = self.guard()?;
            input.lease.verify().map_err(Error::Lease)?;
            if reader.namespace() != input.lease.namespace() {
                return Err(Error::Invalid);
            }
            let grant = reader.read().map_err(Error::Grant)?.ok_or(Error::Invalid)?;
            let fingerprint = reader.fingerprint().ok_or(Error::Invalid)?;
            let mut prepared = self.prepare(input.bind(&original, &grant))?;
            // Failure after commit retains the reservation and returns no prepared envelope.
            let current = reader.read().map_err(Error::Grant)?.ok_or(Error::Invalid)?;
            input
                .spec
                .check_grant(
                    &current,
                    input.grant_id,
                    input.now_rfc3339.parse().map_err(|_| Error::Invalid)?,
                )
                .map_err(|e| Error::Call(crate::native_call::Error::Spec(e)))?;
            input.lease.verify().map_err(Error::Lease)?;
            self.verify_file()?;
            if reader.fingerprint() != Some(fingerprint) {
                return Err(Error::Invalid);
            }
            prepared
                .as_mut()
                .ok_or(Error::Invalid)?
                .bind_grant_file(fingerprint);
            Ok(prepared)
        })();
        if result.is_err() {
            self.blocked = true;
        }
        result
    }
}

impl Owner {
    /// Intended before dispatch, on every consumed chunk and before accepting a response.
    /// All clock samples are injected. Failures retain the reservation and latch locally.
    pub fn check_inflight(
        &mut self,
        flight: &mut crate::native_call::InFlight,
        reader: &mut crate::native_grant::Reader,
        lease: &crate::native_lease::Lease,
        now_rfc3339: &str,
        now_ms: u64,
    ) -> Result<(), Error> {
        let result = (|| {
            flight.check_deadline(now_ms).map_err(Error::Stream)?;
            let current = self.guard()?;
            let prepared = &flight.prepared;
            let expected = prepared.reserved_attempt();
            if expected.status() != journal::AttemptStatus::Reserved
                || current.attempts().iter().find(|a| a.id() == expected.id()) != Some(expected)
                || current.account_hash() != prepared.spec().account_hash()
                || expected.model() != prepared.spec().exact_model()
                || prepared.grant_file().is_none()
                || prepared.grant_file() != reader.fingerprint()
                || reader.namespace() != prepared.namespace()
                || lease.namespace() != prepared.namespace()
            {
                return Err(Error::Invalid);
            }
            lease.verify().map_err(Error::Lease)?;
            let grant = reader.read().map_err(Error::Grant)?.ok_or(Error::Invalid)?;
            prepared
                .spec()
                .check_grant(
                    &grant,
                    prepared.grant_id(),
                    now_rfc3339.parse().map_err(|_| Error::Invalid)?,
                )
                .map_err(|e| Error::Call(crate::native_call::Error::Spec(e)))?;
            lease.verify().map_err(Error::Lease)?;
            self.verify_file()
        })();
        if result.is_err() {
            self.blocked = true;
        }
        result
    }
}
