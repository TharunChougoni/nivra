//! Original receipt-preserving SQLite document archive. No HTTP, model calls or source authentication.
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, limits::Limit};
use sadr_core::research_evidence::Capture;
use std::{
    fmt, fs,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

const TABLE: &str = "CREATE TABLE evidence_captures (id TEXT PRIMARY KEY, document TEXT NOT NULL, payload TEXT NOT NULL)";
const INDEX: &str = "CREATE INDEX evidence_document ON evidence_captures(document)";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("evidence_archive_unavailable")
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    records: u32,
    bytes: u64,
}
impl Limits {
    pub fn new(records: u32, bytes: u64) -> Result<Self, Error> {
        if !(1..=100000).contains(&records) || !(1..=1000000000).contains(&bytes) {
            return Err(Error);
        }
        Ok(Self { records, bytes })
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Open {
    CreateNew,
    Original { device: u64, inode: u64 },
}
struct Guard {
    path: PathBuf,
    file: fs::File,
    parent: fs::File,
    device: u64,
    inode: u64,
    parent_id: (u64, u64),
    uid: u32,
}
fn directory(path: &Path) -> Result<fs::File, Error> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(Error);
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut current =
        fs::File::from(rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Error)?);
    for c in path.components() {
        if let Component::Normal(name) = c {
            current = fs::File::from(
                rustix::fs::openat(&current, name, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Error)?,
            );
        }
    }
    let m = current.metadata().map_err(|_| Error)?;
    let uid = fs::metadata("/proc/self").map_err(|_| Error)?.uid();
    if !m.is_dir() || m.uid() != uid || m.mode() & 0o7777 != 0o700 {
        return Err(Error);
    }
    Ok(current)
}
fn private(m: &fs::Metadata, uid: u32) -> bool {
    m.is_file() && m.uid() == uid && m.mode() & 0o7777 == 0o600 && m.nlink() == 1
}
impl Guard {
    fn bind(path: &Path, mode: Open) -> Result<Self, Error> {
        let parent_path = path.parent().ok_or(Error)?;
        let parent = directory(parent_path)?;
        let pm = parent.metadata().map_err(|_| Error)?;
        let mut flags = rustix::fs::OFlags::RDWR
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC;
        if matches!(mode, Open::CreateNew) {
            flags |= rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL;
        }
        let file = fs::File::from(
            rustix::fs::openat(
                &parent,
                path.file_name().ok_or(Error)?,
                flags,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .map_err(|_| Error)?,
        );
        let m = file.metadata().map_err(|_| Error)?;
        if !private(&m, pm.uid()) {
            return Err(Error);
        }
        if let Open::Original { device, inode } = mode
            && ((m.dev(), m.ino()) != (device, inode) || m.len() == 0)
        {
            return Err(Error);
        }
        let g = Self {
            path: path.into(),
            file,
            parent,
            device: m.dev(),
            inode: m.ino(),
            parent_id: (pm.dev(), pm.ino()),
            uid: pm.uid(),
        };
        g.verify()?;
        Ok(g)
    }
    fn verify(&self) -> Result<(), Error> {
        let named = directory(self.path.parent().ok_or(Error)?)?
            .metadata()
            .map_err(|_| Error)?;
        let held = self.parent.metadata().map_err(|_| Error)?;
        if (named.dev(), named.ino()) != self.parent_id
            || (held.dev(), held.ino()) != self.parent_id
        {
            return Err(Error);
        }
        for m in [
            self.path.symlink_metadata().map_err(|_| Error)?,
            self.file.metadata().map_err(|_| Error)?,
        ] {
            if !private(&m, self.uid) || (m.dev(), m.ino()) != (self.device, self.inode) {
                return Err(Error);
            }
        }
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut p = self.path.as_os_str().to_os_string();
            p.push(suffix);
            match fs::symlink_metadata(PathBuf::from(p)) {
                Ok(m) if suffix != "-journal" && private(&m, self.uid) => {}
                Ok(_) => return Err(Error),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(Error),
            }
        }
        Ok(())
    }
}
pub struct Archive {
    db: Connection,
    limits: Limits,
    guard: Guard,
    failed: bool,
}
impl fmt::Debug for Archive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EvidenceArchive(redacted, no authority)")
    }
}
fn schema(db: &Connection) -> Result<(), Error> {
    let mut s = db.prepare("PRAGMA database_list").map_err(|_| Error)?;
    let names = s
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|_| Error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Error)?;
    if names != ["main"] {
        return Err(Error);
    }
    let mut s = db
        .prepare("SELECT type,name,sql FROM main.sqlite_schema ORDER BY name")
        .map_err(|_| Error)?;
    let rows = s
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|_| Error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| Error)?;
    let expected = vec![
        (
            "table".into(),
            "evidence_captures".into(),
            Some(TABLE.into()),
        ),
        (
            "index".into(),
            "evidence_document".into(),
            Some(INDEX.into()),
        ),
        (
            "index".into(),
            "sqlite_autoindex_evidence_captures_1".into(),
            None,
        ),
    ];
    if rows != expected {
        return Err(Error);
    }
    Ok(())
}
fn row(db: &Connection, sql: &str, id: &str) -> Result<Option<(String, String, String)>, Error> {
    db.query_row(sql, [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()
        .map_err(|_| Error)
}
fn verify_row(row: (String, String, String)) -> Result<Capture, Error> {
    let c = Capture::parse(row.2.as_bytes()).map_err(|_| Error)?;
    if c.hash() != row.0 || c.document_hash() != row.1 || c.canonical_json() != row.2.as_bytes() {
        return Err(Error);
    }
    Ok(c)
}
impl Archive {
    /// Explicit new file or original device/inode only. Never recreates a missing original.
    /// An interrupted fresh initialization is retained and requires inspection, never deleted.
    pub fn open(path: &Path, mode: Open, limits: Limits) -> Result<Self, Error> {
        let guard = Guard::bind(path, mode)?;
        let db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| Error)?;
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .map_err(|_| Error)?;
        db.busy_timeout(Duration::from_secs(1)).map_err(|_| Error)?;
        db.set_limit(Limit::SQLITE_LIMIT_LENGTH, 262144)
            .map_err(|_| Error)?;
        db.execute_batch(
            "PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; PRAGMA read_uncommitted=OFF;",
        )
        .map_err(|_| Error)?;
        guard.verify()?;
        if matches!(mode, Open::CreateNew) {
            db.execute_batch("PRAGMA journal_mode=WAL;")
                .map_err(|_| Error)?;
            db.execute(TABLE, []).map_err(|_| Error)?;
            db.execute(INDEX, []).map_err(|_| Error)?;
        } else {
            schema(&db)?;
            let journal: String = db
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .map_err(|_| Error)?;
            if journal != "wal" {
                return Err(Error);
            }
        }
        schema(&db)?;
        guard.verify()?;
        Ok(Self {
            db,
            limits,
            guard,
            failed: false,
        })
    }
    fn check(&mut self) -> Result<(), Error> {
        if self.failed {
            return Err(Error);
        }
        if self.guard.verify().is_err() {
            self.failed = true;
            return Err(Error);
        }
        if !self.db.is_autocommit() {
            return Err(Error);
        }
        schema(&self.db)
    }
    pub fn ingest(&mut self, capture: &Capture) -> Result<Capture, Error> {
        self.check()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Error)?;
        let result = (|| {
            schema(&tx)?;
            self.guard.verify()?;
            let old = row(
                &tx,
                "SELECT id,document,payload FROM main.evidence_captures WHERE id=?1",
                capture.hash(),
            )?;
            if let Some(old) = old {
                if verify_row(old)?.canonical_json() != capture.canonical_json() {
                    return Err(Error);
                }
            } else {
                let latest=tx.query_row("SELECT id,document,payload FROM main.evidence_captures ORDER BY rowid DESC LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(|_|Error)?;
                if let Some(latest) = latest
                    && verify_row(latest)?.received_at_micros() > capture.received_at_micros()
                {
                    return Err(Error);
                }
                let (count,bytes):(i64,i64)=tx.query_row("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM main.evidence_captures",[],|r|Ok((r.get(0)?,r.get(1)?))).map_err(|_|Error)?;
                if count < 0
                    || bytes < 0
                    || count >= i64::from(self.limits.records)
                    || (bytes as u64)
                        .checked_add(capture.canonical_json().len() as u64)
                        .is_none_or(|n| n > self.limits.bytes)
                {
                    return Err(Error);
                }
                let payload = std::str::from_utf8(capture.canonical_json()).map_err(|_| Error)?;
                tx.execute(
                    "INSERT INTO main.evidence_captures VALUES(?1,?2,?3)",
                    rusqlite::params![capture.hash(), capture.document_hash(), payload],
                )
                .map_err(|_| Error)?;
            }
            let first=row(&tx,"SELECT id,document,payload FROM main.evidence_captures WHERE document=?1 ORDER BY rowid LIMIT 1",capture.document_hash())?.ok_or(Error)?;
            let original = verify_row(first)?;
            if original.document_hash() != capture.document_hash() {
                return Err(Error);
            }
            self.guard.verify()?;
            Ok(original)
        })();
        let result = match result {
            Ok(c) => tx.commit().map(|()| c).map_err(|_| Error),
            Err(e) => match tx.rollback() {
                Ok(()) => Err(e),
                Err(_) => Err(Error),
            },
        };
        if !self.db.is_autocommit() || self.guard.verify().is_err() {
            self.failed = true;
            return Err(Error);
        }
        result
    }
    pub fn load(&mut self, id: &str) -> Result<Capture, Error> {
        self.check()?;
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error);
        }
        let tx = self.db.transaction().map_err(|_| Error)?;
        let result = schema(&tx)
            .and_then(|()| {
                row(
                    &tx,
                    "SELECT id,document,payload FROM main.evidence_captures WHERE id=?1",
                    id,
                )
            })
            .and_then(|r| verify_row(r.ok_or(Error)?));
        let rollback = tx.rollback();
        let checked = self.check();
        if rollback.is_err() {
            self.failed = true;
            return Err(Error);
        }
        checked?;
        result
    }
}
