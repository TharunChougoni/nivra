//! Bounded asynchronous diagnostic journal. No financial/session/process-health authority.
use nivra_core::{native, time::Timestamp};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Visitor},
    ser::SerializeMap,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fmt, fs,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};
const MAX_BYTES: u64 = 64_000_000;
const MAX_RECORD_BYTES: usize = 8192;
const PHASES: &[&str] = &["protection", "scheduler", "projection", "halt"];
const STATUSES: &[&str] = &[
    "OBSERVED",
    "BLOCKED",
    "RECOVERY_REQUIRED",
    "STOPPED",
    "WAITED",
    "assessed",
    "protection_exception",
];
const SPAN_NAMES: &[&str] = &[
    "journal_claim",
    "market_request",
    "archive_check",
    "journal_capture",
    "assessment",
    "journal_result",
    "history_check",
    "halt",
    "scheduler_wait",
    "projection",
];
const PREDICATES: &[&str] = &[
    "unavailable",
    "aged",
    "halted",
    "entry_gate",
    "calendar_closed",
    "protection_exception",
    "paper_protection_uncertain",
    "paper_protection_capacity",
];
fn fixed_labels(phase: &str, status: &str, predicate: Option<&str>) -> bool {
    PHASES.contains(&phase)
        && STATUSES.contains(&status)
        && predicate.is_none_or(|p| PREDICATES.contains(&p))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("timing_journal_unavailable")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Protection,
    Supervision,
}
impl Role {
    fn name(self) -> &'static str {
        match self {
            Self::Protection => "protection",
            Self::Supervision => "supervision",
        }
    }
}
#[derive(Clone, Default)]
struct Spans(Vec<(String, f64)>);
impl Serialize for Spans {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}
impl<'de> Deserialize<'de> for Spans {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Spans;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("bounded measured spans")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<Spans, M::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = m.next_entry::<String, f64>()? {
                    if out.len() == 10 || out.iter().any(|(name, _)| name == &k) {
                        return Err(serde::de::Error::custom("invalid spans"));
                    }
                    out.push((k, v));
                }
                Ok(Spans(out))
            }
        }
        d.deserialize_map(V)
    }
}
fn version() -> String {
    "protection-timing-v1".into()
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(default = "version")]
    version: String,
    phase: String,
    status: String,
    started_at: String,
    started_ist: String,
    elapsed_ms: f64,
    spans_ms: Spans,
    #[serde(default)]
    observation_age_seconds: Option<f64>,
    #[serde(default)]
    predicate: Option<String>,
    #[serde(default)]
    capture_received_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    receipt_age_seconds: Option<f64>,
}
impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TimingRecord(redacted, no authority)")
    }
}
fn stamp(s: &str) -> Result<Timestamp, Error> {
    if !(20..=40).contains(&s.len()) {
        return Err(Error);
    }
    Timestamp::parse_isoformat_offset_seconds(s).map_err(|_| Error)
}
fn bounded(v: f64, max: f64) -> bool {
    v.is_finite() && (0.0..=max).contains(&v)
}
impl Record {
    pub(crate) fn measurement_labels_valid(
        input: &crate::protection_timing::Measurements<'_>,
    ) -> bool {
        input.spans_ms.len() <= 10
            && fixed_labels(input.phase, input.status, input.predicate)
            && input
                .spans_ms
                .iter()
                .all(|(name, _)| SPAN_NAMES.contains(name))
    }
    pub(crate) fn from_measurement(
        fields: crate::protection_timing::Fields,
    ) -> Result<Self, Error> {
        let mut names = BTreeSet::new();
        if fields.spans_ms.iter().any(|(name, _)| !names.insert(name)) {
            return Err(Error);
        }
        let record = Self {
            version: version(),
            phase: fields.phase,
            status: fields.status,
            started_at: fields.started_at,
            started_ist: fields.started_ist,
            elapsed_ms: fields.elapsed_ms,
            spans_ms: Spans(fields.spans_ms),
            observation_age_seconds: fields.observation_age_seconds,
            predicate: fields.predicate,
            capture_received_at: fields.capture_received_at,
            completed_at: fields.completed_at,
            receipt_age_seconds: fields.receipt_age_seconds,
        };
        record.validate()?;
        Ok(record)
    }
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > MAX_RECORD_BYTES {
            return Err(Error);
        }
        native::decode(raw).map_err(|_| Error)?;
        let r: Self = serde_json::from_slice(raw).map_err(|_| Error)?;
        r.validate()?;
        Ok(r)
    }
    fn validate(&self) -> Result<(), Error> {
        if self.version != "protection-timing-v1"
            || !fixed_labels(&self.phase, &self.status, self.predicate.as_deref())
            || !bounded(self.elapsed_ms, 86_400_000.)
            || self.spans_ms.0.len() > 10
        {
            return Err(Error);
        }
        for (name, value) in &self.spans_ms.0 {
            if !SPAN_NAMES.contains(&name.as_str()) || !bounded(*value, 86_400_000.) {
                return Err(Error);
            }
        }
        if self
            .observation_age_seconds
            .is_some_and(|v| !v.is_finite() || v.abs() > 1e12)
            || stamp(&self.started_at)? != stamp(&self.started_ist)?
        {
            return Err(Error);
        }
        match (
            &self.capture_received_at,
            &self.completed_at,
            self.receipt_age_seconds,
        ) {
            (None, None, None) => (),
            (Some(receipt), Some(completed), Some(age)) => {
                let actual = crate::protection_timing::micros_to_seconds(
                    stamp(completed)?
                        .elapsed_micros_since(stamp(receipt)?)
                        .map_err(|_| Error)?,
                );
                if !age.is_finite() || age.abs() > 1e12 || (age - actual).abs() > 1e-6 {
                    return Err(Error);
                }
            }
            _ => return Err(Error),
        }
        Ok(())
    }
    fn raw(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        #[derive(Serialize)]
        struct TimingEvent<'a> {
            r#type: &'static str,
            record: &'a Record,
        }
        let mut raw = serde_json::to_vec(&TimingEvent {
            r#type: "timing",
            record: self,
        })
        .map_err(|_| Error)?;
        raw.push(b'\n');
        if raw.len() > MAX_RECORD_BYTES {
            return Err(Error);
        }
        Ok(raw)
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn dir(path: &Path) -> Result<fs::File, Error> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error);
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut file =
        fs::File::from(rustix::fs::open("/", flags, rustix::fs::Mode::empty()).map_err(|_| Error)?);
    for c in path.components() {
        if let Component::Normal(name) = c {
            file = fs::File::from(
                rustix::fs::openat(&file, name, flags, rustix::fs::Mode::empty())
                    .map_err(|_| Error)?,
            );
        }
    }
    let m = file.metadata().map_err(|_| Error)?;
    if !m.is_dir() || m.uid() != owner()? || m.mode() & 0o077 != 0 {
        return Err(Error);
    }
    Ok(file)
}
fn owner() -> Result<u32, Error> {
    Ok(fs::metadata("/proc/self").map_err(|_| Error)?.uid())
}
fn private(m: &fs::Metadata) -> Result<bool, Error> {
    Ok(m.is_file() && m.uid() == owner()? && m.mode() & 0o077 == 0 && m.nlink() == 1)
}
fn id(m: &fs::Metadata) -> (u64, u64) {
    (m.dev(), m.ino())
}
struct FileGuard {
    path: PathBuf,
    file: fs::File,
    parent: fs::File,
    parent_id: (u64, u64),
    file_id: (u64, u64),
    bytes: u64,
}
impl FileGuard {
    fn create(directory: &Path, role: Role) -> Result<Self, Error> {
        let parent = dir(directory)?;
        let parent_id = id(&parent.metadata().map_err(|_| Error)?);
        let name = format!("ledger.db.{}-timings.jsonl", role.name());
        let flags = rustix::fs::OFlags::WRONLY
            | rustix::fs::OFlags::CREATE
            | rustix::fs::OFlags::EXCL
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC;
        let file = fs::File::from(
            rustix::fs::openat(
                &parent,
                name.as_str(),
                flags,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .map_err(|_| Error)?,
        );
        let file_id = id(&file.metadata().map_err(|_| Error)?);
        let g = Self {
            path: directory.join(name),
            file,
            parent,
            parent_id,
            file_id,
            bytes: 0,
        };
        g.verify()?;
        Ok(g)
    }
    fn verify(&self) -> Result<(), Error> {
        if id(&dir(self.path.parent().ok_or(Error)?)?
            .metadata()
            .map_err(|_| Error)?)
            != self.parent_id
            || id(&self.parent.metadata().map_err(|_| Error)?) != self.parent_id
        {
            return Err(Error);
        }
        for m in [
            self.path.symlink_metadata().map_err(|_| Error)?,
            self.file.metadata().map_err(|_| Error)?,
        ] {
            if !private(&m)? || id(&m) != self.file_id || m.len() != self.bytes {
                return Err(Error);
            }
        }
        Ok(())
    }
    fn write(&mut self, raw: &[u8]) -> Result<(), Error> {
        self.verify()?;
        if self
            .bytes
            .checked_add(raw.len() as u64)
            .is_none_or(|n| n > MAX_BYTES)
        {
            return Err(Error);
        }
        let mut remaining = raw;
        while !remaining.is_empty() {
            let n = self.file.write(remaining).map_err(|_| Error)?;
            if n == 0 {
                return Err(Error);
            }
            self.bytes += n as u64;
            remaining = &remaining[n..];
        }
        self.verify()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CloseReport {
    pub written: u64,
    pub dropped: u64,
    pub failed: bool,
    pub drained: bool,
    pub complete: bool,
}
struct State {
    written: AtomicU64,
    dropped: AtomicU64,
    failed: AtomicBool,
    closed: AtomicBool,
    stop: AtomicBool,
    admission: Mutex<()>,
    done: Mutex<bool>,
    signal: Condvar,
}
impl State {
    fn new() -> Self {
        Self {
            written: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            failed: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            admission: Mutex::new(()),
            done: Mutex::new(false),
            signal: Condvar::new(),
        }
    }
    fn report(&self) -> CloseReport {
        let done = self.done.lock().is_ok_and(|g| *g);
        let failed = self.failed.load(Ordering::SeqCst);
        let dropped = self.dropped.load(Ordering::SeqCst);
        CloseReport {
            written: self.written.load(Ordering::SeqCst),
            dropped,
            failed,
            drained: done && !failed,
            complete: done && !failed && dropped == 0,
        }
    }
}
pub struct TimingJournal {
    sender: SyncSender<Vec<u8>>,
    state: Arc<State>,
    path: PathBuf,
}
impl fmt::Debug for TimingJournal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TimingJournal(redacted, no authority)")
    }
}
fn line(value: Value) -> Result<Vec<u8>, Error> {
    let mut raw = serde_json::to_vec(&value).map_err(|_| Error)?;
    raw.push(b'\n');
    Ok(raw)
}
impl TimingJournal {
    /// Claims one new file only. Existing paths and failed artifacts are never repaired or removed.
    pub fn create(
        directory: &Path,
        role: Role,
        policy_hash: &str,
        capacity: usize,
    ) -> Result<Self, Error> {
        Self::create_inner(directory, role, policy_hash, capacity, |_| {})
    }
    fn create_inner(
        directory: &Path,
        role: Role,
        policy_hash: &str,
        capacity: usize,
        hook: impl Fn(&[u8]) + Send + 'static,
    ) -> Result<Self, Error> {
        if !hash(policy_hash) || !(1..=1024).contains(&capacity) {
            return Err(Error);
        }
        let mut file = FileGuard::create(directory, role)?;
        let path = file.path.clone();
        let header = line(
            json!({"type":"timing_header","version":1,"role":role,"policy_hash":policy_hash}),
        )?;
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let state = Arc::new(State::new());
        let worker = Arc::clone(&state);
        thread::Builder::new()
            .name(format!("nivra-{}-timings", role.name()))
            .spawn(move || {
                let result = (|| {
                    file.write(&header)?;
                    file.file.sync_all().map_err(|_| Error)?;
                    ready_tx.send(()).map_err(|_| Error)?;
                    drain(&mut file, &receiver, &worker, &hook)?;
                    let terminal = line(json!({
                        "type": "timing_terminal",
                        "written": worker.written.load(Ordering::SeqCst),
                        "dropped": worker.dropped.load(Ordering::SeqCst)
                    }))?;
                    file.write(&terminal)?;
                    file.file.sync_all().map_err(|_| Error)?;
                    Ok::<_, Error>(())
                })();
                if result.is_err() {
                    worker.failed.store(true, Ordering::SeqCst);
                }
                if let Ok(mut done) = worker.done.lock() {
                    *done = true;
                    worker.signal.notify_all();
                }
            })
            .map_err(|_| Error)?;
        if ready_rx.recv_timeout(Duration::from_secs(1)).is_err() {
            state.stop.store(true, Ordering::SeqCst);
            return Err(Error);
        }
        Ok(Self {
            sender,
            state,
            path,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Nonblocking queue admission. Disk I/O happens exclusively on the worker.
    pub fn submit(&self, record: &Record) {
        self.admit(record.raw());
    }
    fn admit(&self, raw: Result<Vec<u8>, Error>) {
        let Ok(_admission) = self.state.admission.lock() else {
            self.state.failed.store(true, Ordering::SeqCst);
            self.state.dropped.fetch_add(1, Ordering::SeqCst);
            return;
        };
        let result = raw.and_then(|raw| {
            if self.state.closed.load(Ordering::SeqCst) || self.state.failed.load(Ordering::SeqCst)
            {
                return Err(Error);
            }
            self.sender.try_send(raw).map_err(|_| Error)
        });
        if result.is_err() {
            self.state.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }
    /// Invalid/unbounded host records are counted as omissions, without serializing
    /// arbitrary error text into the journal.
    pub fn submit_json(&self, raw: &[u8]) {
        self.admit(Record::parse(raw).and_then(|record| record.raw()));
    }
    /// A zero duration samples without waiting. Waiting is capped at two seconds;
    /// a blocked OS write remains owned by the detached worker and reports incomplete.
    pub fn close(&self, timeout: Duration) -> CloseReport {
        if let Ok(_admission) = self.state.admission.lock() {
            self.state.closed.store(true, Ordering::SeqCst);
            self.state.stop.store(true, Ordering::SeqCst);
        } else {
            self.state.failed.store(true, Ordering::SeqCst);
            self.state.stop.store(true, Ordering::SeqCst);
        }
        let deadline = Instant::now() + timeout.min(Duration::from_secs(2));
        if let Ok(mut done) = self.state.done.lock() {
            while !*done {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    break;
                };
                match self.state.signal.wait_timeout(done, remaining) {
                    Ok((next, _)) => done = next,
                    Err(_) => break,
                }
            }
        }
        self.state.report()
    }
}
impl Drop for TimingJournal {
    fn drop(&mut self) {
        self.state.closed.store(true, Ordering::SeqCst);
        self.state.stop.store(true, Ordering::SeqCst);
    }
}
fn drain(
    file: &mut FileGuard,
    receiver: &Receiver<Vec<u8>>,
    state: &State,
    hook: &impl Fn(&[u8]),
) -> Result<(), Error> {
    loop {
        let raw = if state.stop.load(Ordering::SeqCst) {
            match receiver.try_recv() {
                Ok(raw) => raw,
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => break,
            }
        } else {
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(raw) => raw,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        };
        hook(&raw);
        file.write(&raw)?;
        let written = state.written.fetch_add(1, Ordering::SeqCst) + 1;
        if written.is_multiple_of(16) {
            file.file.sync_all().map_err(|_| Error)?;
        }
    }
    Ok(())
}
#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum Event {
    #[serde(rename = "timing_header")]
    Header {
        version: u64,
        role: Role,
        policy_hash: String,
    },
    #[serde(rename = "timing")]
    Timing { record: Box<Record> },
    #[serde(rename = "timing_terminal")]
    Terminal { written: u64, dropped: u64 },
}
fn snapshot(m: &fs::Metadata) -> (u64, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
/// Read-only stable snapshot. Measurements remain diagnostics, never health/freshness authority.
pub fn summarize_timings(path: &Path) -> Result<Value, Error> {
    let role = match path.file_name().and_then(|n| n.to_str()) {
        Some("ledger.db.protection-timings.jsonl") => Role::Protection,
        Some("ledger.db.supervision-timings.jsonl") => Role::Supervision,
        _ => return Err(Error),
    };
    let parent = dir(path.parent().ok_or(Error)?)?;
    let parent_id = id(&parent.metadata().map_err(|_| Error)?);
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    let file = fs::File::from(
        rustix::fs::openat(
            &parent,
            path.file_name().ok_or(Error)?,
            flags,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error)?,
    );
    let before = file.metadata().map_err(|_| Error)?;
    if !private(&before)? || before.len() > MAX_BYTES {
        return Err(Error);
    }
    let mut raw = Vec::new();
    (&file)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| Error)?;
    if raw.len() as u64 != before.len() {
        return Err(Error);
    }
    let result = summarize_bytes(&raw, role)?;
    let after = file.metadata().map_err(|_| Error)?;
    let named = path.symlink_metadata().map_err(|_| Error)?;
    if !private(&after)?
        || !private(&named)?
        || snapshot(&before) != snapshot(&after)
        || snapshot(&before) != snapshot(&named)
        || id(&dir(path.parent().ok_or(Error)?)?
            .metadata()
            .map_err(|_| Error)?)
            != parent_id
    {
        return Err(Error);
    }
    Ok(result)
}
fn summarize_bytes(raw: &[u8], expected_role: Role) -> Result<Value, Error> {
    if raw.len() as u64 > MAX_BYTES {
        return Err(Error);
    }
    let mut header = None;
    let mut terminal = None;
    let mut records = Vec::new();
    for line in raw.split_inclusive(|b| *b == b'\n') {
        if line.len() > MAX_RECORD_BYTES || terminal.is_some() {
            return Err(Error);
        }
        if !line.ends_with(b"\n") {
            break;
        }
        native::decode(line).map_err(|_| Error)?;
        let event: Event = serde_json::from_slice(line).map_err(|_| Error)?;
        match event {
            Event::Header {
                version,
                role,
                policy_hash,
            } if header.is_none() && version == 1 && hash(&policy_hash) => {
                header = Some((role, policy_hash))
            }
            Event::Timing { record } if header.is_some() => {
                record.validate()?;
                if records.len() >= 100000 {
                    return Err(Error);
                }
                records.push(*record);
            }
            Event::Terminal { written, dropped }
                if header.is_some() && written == records.len() as u64 && dropped <= 1_000_000 =>
            {
                terminal = Some(dropped)
            }
            _ => return Err(Error),
        }
    }
    let (role, policy_hash) = header.ok_or(Error)?;
    if role != expected_role {
        return Err(Error);
    }
    let complete = terminal == Some(0);
    let phases: BTreeSet<_> = records.iter().map(|r| r.phase.as_str()).collect();
    let mut phase_stats = serde_json::Map::new();
    let mut worst = serde_json::Map::new();
    for phase in phases {
        let group: Vec<_> = records.iter().filter(|r| r.phase == phase).collect();
        let mut elapsed: Vec<_> = group.iter().map(|r| r.elapsed_ms).collect();
        elapsed.sort_by(f64::total_cmp);
        let len = elapsed.len();
        let median = if len.is_multiple_of(2) {
            (elapsed[len / 2 - 1] + elapsed[len / 2]) / 2.
        } else {
            elapsed[len / 2]
        };
        let mut predicates = serde_json::Map::new();
        for p in group
            .iter()
            .filter_map(|r| r.predicate.as_ref())
            .collect::<BTreeSet<_>>()
        {
            predicates.insert(
                p.clone(),
                json!(
                    group
                        .iter()
                        .filter(|r| r.predicate.as_ref() == Some(p))
                        .count()
                ),
            );
        }
        let mut spans = serde_json::Map::new();
        for s in group
            .iter()
            .flat_map(|r| r.spans_ms.0.iter().map(|(k, _)| k))
            .collect::<BTreeSet<_>>()
        {
            let maximum = group
                .iter()
                .flat_map(|r| r.spans_ms.0.iter().filter(|(k, _)| k == s).map(|(_, v)| *v))
                .fold(0., f64::max);
            spans.insert(s.clone(), json!(maximum));
        }
        phase_stats.insert(phase.into(),json!({"count":len,"median_ms":median,"max_ms":elapsed[len-1],"p95_ms":elapsed[(len*95).div_ceil(100)-1],"predicates":predicates,"spans_max_ms":spans}));
        let record = group
            .iter()
            .copied()
            .reduce(|a, b| if b.elapsed_ms > a.elapsed_ms { b } else { a })
            .ok_or(Error)?;
        let dominant = record
            .spans_ms
            .0
            .iter()
            .reduce(|a, b| if b.1 > a.1 { b } else { a })
            .map(|(k, _)| k);
        worst.insert(phase.into(),json!({"started_at":record.started_at,"elapsed_ms":record.elapsed_ms,"status":record.status,"predicate":record.predicate,"spans_ms":record.spans_ms,"dominant_measured_span":dominant,"observation_age_seconds":record.observation_age_seconds,"capture_received_at":record.capture_received_at,"completed_at":record.completed_at,"receipt_age_seconds":record.receipt_age_seconds}));
    }
    let ages: Vec<_> = records
        .iter()
        .filter_map(|r| r.observation_age_seconds)
        .collect();
    let receipts: Vec<_> = records
        .iter()
        .filter_map(|r| r.receipt_age_seconds)
        .collect();
    let ticks: Vec<_> = records
        .iter()
        .filter(|r| r.phase == "protection" || r.phase == "halt")
        .collect();
    let gaps: Vec<_> = ticks
        .windows(2)
        .map(|pair| {
            stamp(&pair[1].started_at)?
                .elapsed_micros_since(stamp(&pair[0].started_at)?)
                .map(|n| crate::protection_timing::micros_to_seconds(n) * 1000.)
                .map_err(|_| Error)
        })
        .collect::<Result<_, _>>()?;
    fn max(values: &[f64]) -> Option<f64> {
        values.iter().copied().reduce(f64::max)
    }
    fn outside(values: &[f64]) -> usize {
        values.iter().filter(|v| !(0.0..=2.0).contains(*v)).count()
    }
    Ok(
        json!({"role":role,"policy_hash":policy_hash,"records":records.len(),"state":if complete{"COMPLETE"}else{"INCOMPLETE"},"dropped":terminal,"phases":phase_stats,"process_health":"UNVERIFIED","nested_spans":true,"diagnosis":{"scope":"observed-records-only","historical_root_cause":"UNKNOWN","evidence_complete":complete,"nested_spans_additive":false,"worst_records_by_phase":worst,"observation_age":{"measured_records":ages.len(),"missing_records":records.len()-ages.len(),"max_seconds":max(&ages),"outside_two_second_gate":outside(&ages)},"receipt_age":{"measured_records":receipts.len(),"missing_protection_records":ticks.iter().filter(|r|r.receipt_age_seconds.is_none()).count(),"max_seconds":max(&receipts),"outside_two_second_gate":outside(&receipts),"basis":"diagnostic-completion-minus-archived-receipt"},"protection_start_gaps":{"basis":"wall-clock-record-starts","observed_pairs":gaps.len(),"max_ms":max(&gaps),"over_two_seconds":gaps.iter().filter(|v|**v>2000.).count(),"backward_clock_pairs":gaps.iter().filter(|v|**v<0.).count(),"continuity":"UNVERIFIED"}}}),
    )
}
