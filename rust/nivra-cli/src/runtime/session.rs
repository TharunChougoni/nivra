//! Persistent orchestrator session: append-only framed log, operator notes and
//! per-turn action receipts.
//!
//! The log is one writer (the session orchestrator). Chat instances and the
//! dashboard only read it with [`read_frames`], which stops at the last complete
//! frame: a truncated tail never yields a partial record and never errors.
//!
//! Notes live in their own store, outside the compactable log. Every mutation
//! bumps the note-set version; a turn records the version it was built from and
//! its action commit is refused when the set changed (#143).
// The pi cutover left these modules read-only in production builds: every
// writer below is compiled for tests only, together with what it needs.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

const ROOT: &str = "model/session";
pub(crate) const ROOT_PREFIX: &str = ROOT;
const MAX_LOG_BYTES: usize = 64 * 1024 * 1024;

fn io_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// One turn, result, compaction summary or end-of-day handoff record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Record {
    pub seq: u64,
    pub at_ms: i64,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Body {
    Turn { turn: Turn },
    Result { result: TurnResult },
    Summary { summary: Summary },
    Handoff { handoff: Handoff },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Turn {
    pub id: String,
    pub session: String,
    pub trigger: String,
    /// Hash of the system prompt and trigger template this turn was built from.
    pub prompt_version: String,
    /// Note-set version this turn was built from. A commit refuses if it moved.
    pub notes_version: u64,
    pub fills_at_start: usize,
    pub started_at_ms: i64,
    /// Bounded trigger facts sent to the model. Untrusted observations only.
    pub request: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TurnResult {
    pub turn_id: String,
    pub finished_at_ms: i64,
    /// `ok`, `failed`, `stale_notes` or `abandoned_no_replay`.
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub reply: String,
    pub actions: Vec<Action>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Action {
    pub kind: String,
    pub id: String,
    pub digest: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Summary {
    /// The last log sequence this summary covers. Prompts start after it.
    pub up_to_seq: u64,
    pub text: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Handoff {
    pub target_session: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    pub seed: Value,
    pub created_at_ms: i64,
}

fn frame_header() -> usize {
    4 + 32
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn decode_frame(bytes: &[u8]) -> std::result::Result<Record, &'static str> {
    serde_json::from_slice(bytes).map_err(|_| "session_record_invalid")
}

/// Why a checked session-log read failed. Static codes only: no path, OS error
/// text or raw payload is exposed to the operational projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LogReadError {
    Missing,
    Unreadable,
    Oversize,
    NonRegular,
    Symlink,
}

/// Bounded session-log read. The size is checked from metadata before the open
/// and again from the open descriptor, then the read is capped with
/// `take(limit + 1)`, so an oversized or growing file is never read whole.
/// `follow_symlink` keeps the legacy archive reader permissive while the
/// checked operational reader refuses links explicitly.
fn read_log_bounded(
    path: &Path,
    limit: u64,
    follow_symlink: bool,
) -> std::result::Result<Vec<u8>, LogReadError> {
    if !follow_symlink {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LogReadError::Symlink);
            }
            Ok(metadata) if !metadata.is_file() => return Err(LogReadError::NonRegular),
            Ok(metadata) if metadata.len() > limit => return Err(LogReadError::Oversize),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LogReadError::Missing);
            }
            Err(_) => return Err(LogReadError::Unreadable),
        }
    }
    let flags = if follow_symlink {
        libc::O_NONBLOCK
    } else {
        libc::O_NOFOLLOW | libc::O_NONBLOCK
    };
    let file = match OpenOptions::new().read(true).custom_flags(flags).open(path) {
        Ok(file) => file,
        Err(error) if !follow_symlink && error.raw_os_error() == Some(libc::ELOOP) => {
            return Err(LogReadError::Symlink);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(LogReadError::Missing);
        }
        Err(_) => return Err(LogReadError::Unreadable),
    };
    let metadata = file.metadata().map_err(|_| LogReadError::Unreadable)?;
    if !metadata.is_file() {
        return Err(LogReadError::NonRegular);
    }
    if metadata.len() > limit {
        return Err(LogReadError::Oversize);
    }
    let mut raw = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|_| LogReadError::Unreadable)?;
    if raw.len() as u64 > limit {
        return Err(LogReadError::Oversize);
    }
    Ok(raw)
}

/// Read every complete frame. An incomplete header, an incomplete body, a bad
/// checksum or invalid JSON stops the scan; nothing partial is returned and no
/// error escapes, so a chat reader can always answer from what exists (#145).
/// The read itself is bounded, so a pre-existing oversized archive cannot be
/// loaded into memory; complete-prefix semantics are unchanged.
pub(crate) fn read_frames(path: &Path) -> Vec<Record> {
    let Ok(raw) = read_log_bounded(path, MAX_LOG_BYTES as u64, true) else {
        return Vec::new();
    };
    scan_frames(&raw).records
}

/// Checked pending-turn source. `missing` and `no_pending` are known absences;
/// `corrupt`, `incomplete`, `unreadable`, `oversize`, `non_regular` and
/// `symlink` are unavailable sources and must never be reported as idle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingSource {
    Missing,
    NoPending,
    Pending,
    Corrupt,
    Incomplete,
    Unreadable,
    Oversize,
    NonRegular,
    Symlink,
}

impl PendingProjection {}

/// Strict pending projection for operational health. `turn` is the checked
/// durable evidence when the valid prefix proves it, even if the source is
/// otherwise unavailable; `source` says whether that evidence is authoritative.
#[derive(Clone, Debug)]
pub(crate) struct PendingProjection {
    pub(crate) source: PendingSource,
    pub(crate) turn: Option<Turn>,
}

impl PendingProjection {}

fn pending_from_records(records: &[Record]) -> Option<Turn> {
    let last = records.iter().rev().find_map(|record| match &record.body {
        Body::Turn { turn } => Some(turn.clone()),
        _ => None,
    })?;
    if records
        .iter()
        .any(|record| matches!(&record.body, Body::Result { result } if result.turn_id == last.id))
    {
        None
    } else {
        Some(last)
    }
}

#[derive(Debug)]
enum FrameOutcome {
    Complete,
    IncompleteTail,
    Corrupt,
}

struct FrameScan {
    records: Vec<Record>,
    valid_bytes: usize,
    outcome: FrameOutcome,
}

fn scan_frames(raw: &[u8]) -> FrameScan {
    let mut scan = FrameScan {
        records: Vec::new(),
        valid_bytes: 0,
        outcome: FrameOutcome::Complete,
    };
    while scan.valid_bytes < raw.len() {
        let cursor = scan.valid_bytes;
        if raw.len() - cursor < frame_header() {
            scan.outcome = FrameOutcome::IncompleteTail;
            break;
        }
        let len =
            u32::from_be_bytes(raw[cursor..cursor + 4].try_into().unwrap_or([0, 0, 0, 0])) as usize;
        let start = cursor + frame_header();
        if len == 0 || len > MAX_LOG_BYTES - frame_header() {
            scan.outcome = FrameOutcome::Corrupt;
            break;
        }
        let Some(end) = start.checked_add(len) else {
            scan.outcome = FrameOutcome::Corrupt;
            break;
        };
        if end > raw.len() {
            // The ABI does not checksum the length. A complete JSON value cannot
            // be an interrupted body, even when its declared length exceeds EOF.
            scan.outcome = match serde_json::from_slice::<Value>(&raw[start..]) {
                Err(error) if error.is_eof() => FrameOutcome::IncompleteTail,
                _ => FrameOutcome::Corrupt,
            };
            break;
        }
        if raw[cursor + 4..start] != sha256(&raw[start..end])[..] {
            scan.outcome = FrameOutcome::Corrupt;
            break;
        }
        let record = match decode_frame(&raw[start..end]) {
            Ok(record) => record,
            Err(_) => {
                scan.outcome = FrameOutcome::Corrupt;
                break;
            }
        };
        scan.records.push(record);
        scan.valid_bytes = end;
    }
    scan
}

#[derive(Clone, Debug)]
pub(crate) struct SessionLog {
    directory: PathBuf,
}

impl SessionLog {
    pub(crate) fn new(root: &Path, session: &str) -> Self {
        Self {
            directory: root.join(ROOT).join(session),
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.join("log.bin")
    }

    pub(crate) fn records(&self) -> Vec<Record> {
        read_frames(&self.path())
    }

    pub(crate) fn turns(&self) -> Vec<Turn> {
        self.records()
            .into_iter()
            .filter_map(|record| match record.body {
                Body::Turn { turn } => Some(turn),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn results(&self) -> Vec<TurnResult> {
        self.records()
            .into_iter()
            .filter_map(|record| match record.body {
                Body::Result { result } => Some(result),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn result_for(&self, turn_id: &str) -> Option<TurnResult> {
        self.results()
            .into_iter()
            .find(|result| result.turn_id == turn_id)
    }

    /// Strict checked pending projection. It never repairs, truncates or
    /// follows the log, and distinguishes a missing log from a complete log
    /// with no pending turn and from every unavailable source.
    pub(crate) fn pending_checked(&self) -> PendingProjection {
        let scan = match read_log_bounded(&self.path(), MAX_LOG_BYTES as u64, false) {
            Ok(raw) => scan_frames(&raw),
            Err(error) => {
                return PendingProjection {
                    source: match error {
                        LogReadError::Missing => PendingSource::Missing,
                        LogReadError::Unreadable => PendingSource::Unreadable,
                        LogReadError::Oversize => PendingSource::Oversize,
                        LogReadError::NonRegular => PendingSource::NonRegular,
                        LogReadError::Symlink => PendingSource::Symlink,
                    },
                    turn: None,
                };
            }
        };
        let turn = pending_from_records(&scan.records);
        let source = match scan.outcome {
            FrameOutcome::Complete => {
                if turn.is_some() {
                    PendingSource::Pending
                } else {
                    PendingSource::NoPending
                }
            }
            // A complete durable turn is provable even with a partial tail
            // after it. Otherwise the tail could become a future turn, so the
            // absence of a pending turn is not provable.
            FrameOutcome::IncompleteTail => {
                if turn.is_some() {
                    PendingSource::Pending
                } else {
                    PendingSource::Incomplete
                }
            }
            FrameOutcome::Corrupt => PendingSource::Corrupt,
        };
        PendingProjection { source, turn }
    }

    /// Latest compaction summary, if any. Prompts start from its `up_to_seq`.
    pub(crate) fn summary(&self) -> Option<Summary> {
        self.records()
            .into_iter()
            .rev()
            .find_map(|record| match record.body {
                Body::Summary { summary } => Some(summary),
                _ => None,
            })
    }

    pub(crate) fn handoff(&self) -> Option<Handoff> {
        self.records()
            .into_iter()
            .rev()
            .find_map(|record| match record.body {
                Body::Handoff { handoff } => Some(handoff),
                _ => None,
            })
    }
}

// ---------------------------------------------------------------------------
// Action receipts
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Operator notes
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoteState {
    Waiting,
    Pinned,
    Done,
    Superseded,
    Cleared,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Note {
    pub id: String,
    pub text: String,
    pub state: NoteState,
    pub keep: bool,
    /// Changes when the note text/keep changes; a mark must echo the revision it
    /// saw so an edit is never clobbered (#143).
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_in: Option<String>,
    /// Last actor to change this note: `operator`, `orchestrator` or `host`.
    #[serde(default = "actor_host")]
    pub updated_by: String,
}

fn actor_host() -> String {
    "host".into()
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct NoteSet {
    /// Bumped on every add, clear, keep edit, pin and mark. A turn records it.
    pub version: u64,
    pub notes: Vec<Note>,
}

impl NoteSet {}

#[derive(Clone, Debug)]
pub(crate) struct Notes {
    path: PathBuf,
}

impl Notes {
    pub(crate) fn new(root: &Path, session: &str) -> Self {
        Self {
            path: root.join(ROOT).join(session).join("notes.json"),
        }
    }

    fn read(&self) -> NoteSet {
        fs::read(&self.path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) fn snapshot(&self) -> NoteSet {
        let _guard = io_lock();
        self.read()
    }
}

// ---------------------------------------------------------------------------
// Prompts, seeds and handoff
// ---------------------------------------------------------------------------
