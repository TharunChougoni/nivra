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
use super::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::*;

const ROOT: &str = "model/session";
pub(crate) const ROOT_PREFIX: &str = ROOT;
const MAX_LOG_BYTES: usize = 64 * 1024 * 1024;
const MAX_REPLY_BYTES: usize = 32 * 1024;
/// Bound on one replayed turn request. The owner re-reads detail with tools, so
/// replay keeps continuity fields instead of embedding an old data payload.
const MAX_REPLAY_REQUEST_BYTES: usize = 4_096;
/// Bound on the replayed committed conversation. Oldest exchanges are omitted
/// first; the original log is never rewritten.
const MAX_REPLAY_CONVERSATION_BYTES: usize = 12_288;
/// Bound on the latest compaction summary text carried into the prompt.
const MAX_REPLAY_SUMMARY_BYTES: usize = 6_144;
/// Continuity fields kept when one replayed request is over its bound.
const REPLAY_CONTINUITY_KEYS: [&str; 14] = [
    "digest",
    "digest_id",
    "digest_tool_result_count",
    "facts_sha256",
    "evidence_input_hash",
    "resume_binding_sha256",
    "host_decision",
    "model_selection",
    "cash",
    "fees",
    "applied_hashes",
    "current_hashes",
    "owner_applied_hashes",
    "trigger",
];

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

fn encode_frame(record: &Record) -> Result<Vec<u8>> {
    let body = serde_json::to_vec(record)?;
    let len = u32::try_from(body.len()).map_err(|_| "session_record_too_large")?;
    let mut frame = Vec::with_capacity(frame_header() + body.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&sha256(&body));
    frame.extend_from_slice(&body);
    Ok(frame)
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

/// Prompt reference for a committed review or research turn (#230). Decision
/// prompts see that the turn happened, its identity, outcome and the durable
/// artifact refs in `actions`, but not the sealed evidence, notes block,
/// evidence payload or full reply. The log itself is never rewritten; this
/// only shrinks prompt reconstruction.
fn owner_task_reference(turn: &Turn, result: &TurnResult) -> Option<Value> {
    let owner_task = turn.request.get("owner_task");
    if owner_task.is_none() && !matches!(turn.trigger.as_str(), "review" | "research") {
        return None;
    }
    let reference = owner_task.map(|task| json!({"kind":task["kind"],"identity":task["identity"]}));
    Some(json!({
        "turn": {
            "id": turn.id,
            "session": turn.session,
            "trigger": turn.trigger,
            "started_at_ms": turn.started_at_ms,
            "request": {"owner_task": reference},
        },
        "result": {
            "turn_id": result.turn_id,
            "finished_at_ms": result.finished_at_ms,
            "outcome": result.outcome,
            "reason": result.reason,
            "actions": result.actions,
            "reply_omitted": true,
            "reply_sha256": sadr_core::replay::digest(result.reply.as_bytes()),
            "reply_bytes": result.reply.len(),
        },
    }))
}

impl PendingProjection {
    /// True only when the log provably holds no pending turn: it is absent or
    /// complete without one. Corrupt, partial or unreadable logs are never idle.
    pub(crate) fn idle(&self) -> bool {
        matches!(
            self.source,
            PendingSource::Missing | PendingSource::NoPending
        )
    }
}

/// Strict pending projection for operational health. `turn` is the checked
/// durable evidence when the valid prefix proves it, even if the source is
/// otherwise unavailable; `source` says whether that evidence is authoritative.
#[derive(Clone, Debug)]
pub(crate) struct PendingProjection {
    pub(crate) source: PendingSource,
    pub(crate) turn: Option<Turn>,
}

impl PendingProjection {
    pub(crate) fn is_available(&self) -> bool {
        matches!(
            self.source,
            PendingSource::Missing | PendingSource::NoPending | PendingSource::Pending
        )
    }
}

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
    Corrupt(&'static str),
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
            scan.outcome = FrameOutcome::Corrupt("session_frame_length_invalid");
            break;
        }
        let Some(end) = start.checked_add(len) else {
            scan.outcome = FrameOutcome::Corrupt("session_frame_length_invalid");
            break;
        };
        if end > raw.len() {
            // The ABI does not checksum the length. A complete JSON value cannot
            // be an interrupted body, even when its declared length exceeds EOF.
            scan.outcome = match serde_json::from_slice::<Value>(&raw[start..]) {
                Err(error) if error.is_eof() => FrameOutcome::IncompleteTail,
                _ => FrameOutcome::Corrupt("session_frame_length_invalid"),
            };
            break;
        }
        if raw[cursor + 4..start] != sha256(&raw[start..end])[..] {
            scan.outcome = FrameOutcome::Corrupt("session_frame_checksum_invalid");
            break;
        }
        let record = match decode_frame(&raw[start..end]) {
            Ok(record) => record,
            Err(reason) => {
                scan.outcome = FrameOutcome::Corrupt(reason);
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

    /// Append one record and durably commit it. The process-wide lock serialises
    /// the orchestrator; a partial tail left by a crash is truncated first.
    pub(crate) fn append(&self, at_ms: i64, body: Body) -> Result<Record> {
        let _guard = io_lock();
        fs::create_dir_all(&self.directory)?;
        let path = self.path();
        let mut file = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err("session_log_invalid".into());
        }
        if metadata.len() > MAX_LOG_BYTES as u64 {
            return Err("session_log_too_large".into());
        }
        let mut raw = Vec::new();
        (&mut file)
            .take(MAX_LOG_BYTES as u64 + 1)
            .read_to_end(&mut raw)?;
        if raw.len() > MAX_LOG_BYTES {
            return Err("session_log_too_large".into());
        }
        let scan = scan_frames(&raw);
        if let FrameOutcome::Corrupt(reason) = scan.outcome {
            return Err(reason.into());
        }
        if scan
            .records
            .windows(2)
            .any(|pair| pair[1].seq <= pair[0].seq)
        {
            return Err("session_log_sequence_invalid".into());
        }
        let seq = match scan.records.last() {
            Some(record) => record
                .seq
                .checked_add(1)
                .ok_or("session_log_sequence_exhausted")?,
            None => 1,
        };
        let record = Record { seq, at_ms, body };
        let encoded = encode_frame(&record)?;
        if encoded.len() > MAX_LOG_BYTES - scan.valid_bytes {
            return Err("session_log_too_large".into());
        }
        if matches!(scan.outcome, FrameOutcome::IncompleteTail) {
            file.set_len(scan.valid_bytes as u64)?;
            file.sync_all()?;
            eprintln!(
                "session_log_repaired_tail dropped_bytes={}",
                raw.len() - scan.valid_bytes
            );
        }
        file.write_all(&encoded)?;
        file.sync_all()?;
        if let Some(parent) = path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(record)
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

    /// Identity of the latest durable turn result and its independently
    /// committed action receipts. Missing results remain explicitly absent.
    fn latest_receipt_reference(&self, root: &Path, session: &str) -> Value {
        let records = self.records();
        let Some((result_seq, result)) = records.iter().rev().find_map(|record| {
            if let Body::Result { result } = &record.body {
                Some((record.seq, result))
            } else {
                None
            }
        }) else {
            return json!({"available":false,"reason":"no_completed_turn_receipt"});
        };
        let turn_seq = records.iter().find_map(|record| match &record.body {
            Body::Turn { turn } if turn.id == result.turn_id => Some(record.seq),
            _ => None,
        });
        let Some(turn_seq) = turn_seq.filter(|turn_seq| *turn_seq < result_seq) else {
            return json!({"available":false,"reason":"completed_turn_receipt_invalid"});
        };
        let actions = actions_for(root, session, &result.turn_id);
        json!({"available":true,"turn_id":result.turn_id,"turn_seq":turn_seq,"result_seq":result_seq,"result_sha256":super::digest(&serde_json::to_vec(result).unwrap_or_default()),"outcome":result.outcome,"action_receipts":actions.iter().map(|action| json!({"kind":action.kind,"id":action.id,"digest":action.digest})).collect::<Vec<_>>()})
    }

    /// A turn with no result record. On resume the caller checks its action
    /// receipt first: an action exists -> write only the result; no action ->
    /// replay the turn with the same id (#141). This permissive accessor keeps
    /// legacy callers working; operational health uses [`Self::pending_checked`]
    /// so an unreadable source is never mistaken for "no pending".
    pub(crate) fn pending(&self) -> Option<Turn> {
        pending_from_records(&self.records())
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
            FrameOutcome::Corrupt(_) => PendingSource::Corrupt,
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

    pub(crate) fn last_seq(&self) -> u64 {
        self.records().last().map_or(0, |record| record.seq)
    }

    /// Turns recorded after the latest compaction summary. A session compacts
    /// when this crosses its budget, never by rewriting the log (#144).
    pub(crate) fn turns_since_summary(&self) -> usize {
        let after = self.summary().map_or(0, |summary| summary.up_to_seq);
        self.records()
            .into_iter()
            .filter(|record| record.seq > after)
            .filter(|record| matches!(record.body, Body::Turn { .. }))
            .count()
    }

    /// Measured persistent context: the latest summary text plus the serialized
    /// reconstructed conversation after it. The static prompt, notes, seed,
    /// current facts and reserved result space are covered by configured
    /// headroom. This is a byte proxy for context, not a provider token count.
    pub(crate) fn context_bytes(&self) -> u64 {
        serde_json::to_vec(&self.conversation()).map_or(u64::MAX, |bytes| bytes.len() as u64)
    }

    /// Reconstruct the committed conversation after the latest compaction
    /// summary. Turns are emitted in log order and paired with their terminal
    /// result. A pending turn is excluded until its result is durable. Host
    /// envelope fields are removed from requests so the next turn cannot
    /// recursively embed previous conversation state. Replay is bounded: a
    /// request keeps only continuity fields, the digest keeps only its brief,
    /// and the oldest exchanges are omitted past the replay budget. The log
    /// itself is never rewritten.
    pub(crate) fn conversation(&self) -> Value {
        let records = self.records();
        let summary = records.iter().rev().find_map(|record| match &record.body {
            Body::Summary { summary } => Some(summary.clone()),
            _ => None,
        });
        let after = summary.as_ref().map_or(0, |summary| summary.up_to_seq);
        let results: std::collections::HashMap<&str, (u64, &TurnResult)> = records
            .iter()
            .filter_map(|record| match &record.body {
                Body::Result { result } if record.seq > after => {
                    Some((result.turn_id.as_str(), (record.seq, result)))
                }
                _ => None,
            })
            .fold(std::collections::HashMap::new(), |mut map, (id, value)| {
                // A turn has one terminal result. Retain its first durable
                // result if an old or damaged log contains duplicates.
                map.entry(id).or_insert(value);
                map
            });
        let mut exchanges: Vec<Value> = records
            .iter()
            .filter_map(|record| {
                if record.seq <= after {
                    return None;
                }
                let Body::Turn { turn } = &record.body else {
                    return None;
                };
                let (result_seq, result) = results.get(turn.id.as_str()).copied()?;
                if result_seq <= record.seq {
                    return None;
                }
                if let Some(exchange) = owner_task_reference(turn, result) {
                    return Some(exchange);
                }
                let mut turn = turn.clone();
                if let Some(request) = turn.request.as_object_mut() {
                    for key in [
                        "session_context",
                        "recent_turns",
                        "summary",
                        "seed",
                        "digest_tool_results",
                    ] {
                        request.remove(key);
                    }
                    if let Some(digest) = request.get_mut("digest") {
                        let brief = digest.get("brief").cloned().unwrap_or(Value::Null);
                        let coverage = digest.get("coverage").cloned().unwrap_or(Value::Null);
                        *digest = json!({"brief":brief,"coverage":coverage});
                    }
                }
                if serde_json::to_vec(&turn.request)
                    .map(|bytes| bytes.len())
                    .unwrap_or(usize::MAX)
                    > MAX_REPLAY_REQUEST_BYTES
                    && let Some(request) = turn.request.as_object_mut()
                {
                    request.retain(|key, _| REPLAY_CONTINUITY_KEYS.contains(&key.as_str()));
                    request.insert("replay_bounded".into(), json!(true));
                }
                Some(json!({"turn":turn,"result":result}))
            })
            .collect();
        let mut omitted = 0usize;
        while exchanges.len() > 1
            && serde_json::to_vec(&exchanges)
                .map(|bytes| bytes.len())
                .unwrap_or(usize::MAX)
                > MAX_REPLAY_CONVERSATION_BYTES
        {
            exchanges.remove(0);
            omitted += 1;
        }
        let summary = summary.map(|mut summary| {
            if summary.text.len() > MAX_REPLAY_SUMMARY_BYTES {
                let mut at = summary.text.len() - MAX_REPLAY_SUMMARY_BYTES;
                while !summary.text.is_char_boundary(at) {
                    at += 1;
                }
                summary.text = format!("[earlier summary truncated] {}", &summary.text[at..]);
            }
            summary
        });
        super::transcript::redact(&json!({
            "schema_version": 1,
            "summary": summary,
            "exchanges": exchanges,
            "omitted_exchanges": omitted,
        }))
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

    /// Turns after the latest summary, oldest first, bounded.
    pub(crate) fn recent_turns(&self, limit: usize) -> Vec<Turn> {
        let after = self.summary().map_or(0, |summary| summary.up_to_seq);
        let mut turns: Vec<Turn> = self
            .records()
            .into_iter()
            .filter(|record| record.seq > after)
            .filter_map(|record| match record.body {
                Body::Turn { turn } => Some(turn),
                _ => None,
            })
            .collect();
        if turns.len() > limit {
            turns.drain(..turns.len() - limit);
        }
        turns
    }
}

// ---------------------------------------------------------------------------
// Action receipts
// ---------------------------------------------------------------------------

fn action_directory(root: &Path, session: &str) -> PathBuf {
    root.join(ROOT).join(session).join("actions")
}

fn action_path(root: &Path, session: &str, turn_id: &str) -> PathBuf {
    action_directory(root, session).join(format!("{turn_id}.json"))
}

/// A directive (or any future session action) is committed at most once for a
/// turn. A replay finds the receipt and writes only the result (#141).
pub(crate) fn commit_action(
    root: &Path,
    session: &str,
    turn_id: &str,
    action: &Action,
) -> Result<()> {
    let _guard = io_lock();
    let path = action_path(root, session, turn_id);
    let mut actions: Vec<Action> = match fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| "session_action_receipt_invalid")?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    if actions
        .iter()
        .any(|existing| existing.id == action.id || existing.digest == action.digest)
    {
        return Err("session_action_already_committed".into());
    }
    actions.push(action.clone());
    fs::create_dir_all(action_directory(root, session))?;
    super::rollover::atomic(&path, &serde_json::to_value(&actions)?)?;
    Ok(())
}

/// Permissive reader for archived projections. Control paths use
/// [`checked_actions_for`] so a corrupt receipt is never mistaken for absence.
pub(crate) fn actions_for(root: &Path, session: &str, turn_id: &str) -> Vec<Action> {
    fs::read(action_path(root, session, turn_id))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Checked action-receipt read for control paths. A missing receipt is empty;
/// an unreadable or corrupt receipt is an explicit refusal.
pub(crate) fn checked_actions_for(
    root: &Path,
    session: &str,
    turn_id: &str,
) -> std::result::Result<Vec<Action>, &'static str> {
    match fs::read(action_path(root, session, turn_id)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| "session_action_receipt_invalid"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err("session_action_receipt_unreadable"),
    }
}

pub(crate) fn action(kind: &str, id: &str, value: &Value) -> Action {
    Action {
        kind: kind.into(),
        id: id.into(),
        digest: super::digest(&serde_json::to_vec(value).unwrap_or_default()),
    }
}

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

impl NoteSet {
    pub(crate) fn active(&self) -> Vec<Note> {
        self.notes
            .iter()
            .filter(|note| matches!(note.state, NoteState::Waiting | NoteState::Pinned))
            .cloned()
            .collect()
    }

    /// Pinned block sent with every turn. Notes stay outside the compactable log.
    pub(crate) fn prompt_block(&self) -> Value {
        json!(
            self.active()
                .into_iter()
                .map(|note| json!({
                    "id": note.id,
                    "text": note.text,
                    "state": note.state,
                    "keep": note.keep,
                    "revision": note.revision,
                }))
                .collect::<Vec<_>>()
        )
    }
}

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

    fn write(&self, notes: &NoteSet) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        super::rollover::atomic(&self.path, &serde_json::to_value(notes)?)?;
        Ok(())
    }

    pub(crate) fn snapshot(&self) -> NoteSet {
        let _guard = io_lock();
        self.read()
    }

    pub(crate) fn version(&self) -> u64 {
        self.snapshot().version
    }

    fn update(
        &self,
        now: i64,
        change: impl FnOnce(&mut NoteSet) -> Result<bool>,
    ) -> Result<NoteSet> {
        let _guard = io_lock();
        let mut notes = self.read();
        if change(&mut notes)? {
            notes.version = notes.version.saturating_add(1);
            for note in &mut notes.notes {
                note.updated_at_ms = now;
            }
            self.write(&notes)?;
        }
        Ok(notes)
    }

    #[cfg(test)]
    pub(crate) fn add(&self, text: &str, keep: bool, now: i64) -> Result<Note> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > 2000 {
            return Err("session_note_invalid".into());
        }
        let id = format!(
            "note-{}",
            &super::digest(format!("{text}|{now}").as_bytes())[..16]
        );
        let note = Note {
            id: id.clone(),
            text: text.to_owned(),
            state: NoteState::Waiting,
            keep,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            pinned_in: None,
            updated_by: "operator".into(),
        };
        self.update(now, |notes| {
            if notes.notes.iter().any(|n| n.id == id) {
                return Ok(false);
            }
            notes.notes.push(note.clone());
            Ok(true)
        })?;
        Ok(note)
    }

    /// Pin every waiting note the turn is about to see. One version bump covers
    /// the whole set, so the turn records the version that includes its own pin.
    pub(crate) fn pin_waiting(&self, turn_id: &str, now: i64) -> Result<u64> {
        let notes = self.update(now, |notes| {
            let mut changed = false;
            for note in &mut notes.notes {
                if note.state == NoteState::Waiting {
                    note.state = NoteState::Pinned;
                    note.pinned_in = Some(turn_id.to_owned());
                    note.updated_by = "host".into();
                    changed = true;
                }
            }
            Ok(changed)
        })?;
        Ok(notes.version)
    }

    /// Mark done/superseded only for the revision this turn saw.
    pub(crate) fn mark(&self, id: &str, revision: u64, state: NoteState, now: i64) -> Result<u64> {
        if !matches!(state, NoteState::Done | NoteState::Superseded) {
            return Err("session_note_mark_invalid".into());
        }
        let notes = self.update(now, |notes| {
            let note = notes
                .notes
                .iter_mut()
                .find(|note| note.id == id)
                .ok_or("session_note_missing")?;
            if note.revision != revision {
                return Err("session_note_revision_changed".into());
            }
            if matches!(note.state, NoteState::Cleared | NoteState::Expired) {
                return Err("session_note_already_closed".into());
            }
            if note.state == state {
                return Ok(false);
            }
            note.state = state;
            note.updated_by = "orchestrator".into();
            Ok(true)
        })?;
        Ok(notes.version)
    }

    /// At close, non-keep notes expire. Keep notes and already-closed notes are
    /// untouched; the caller copies keep notes into tomorrow's seed (#144).
    pub(crate) fn expire_at_close(&self, now: i64) -> Result<Vec<String>> {
        let mut expired = Vec::new();
        self.update(now, |notes| {
            for note in &mut notes.notes {
                if !note.keep && matches!(note.state, NoteState::Waiting | NoteState::Pinned) {
                    note.state = NoteState::Expired;
                    note.updated_by = "host".into();
                    expired.push(note.id.clone());
                }
            }
            Ok(!expired.is_empty())
        })?;
        Ok(expired)
    }

    /// Today's keep notes for tomorrow's seed, independent of the handoff call.
    pub(crate) fn keep_for_seed(&self) -> Vec<Note> {
        self.snapshot()
            .notes
            .into_iter()
            .filter(|note| {
                note.keep && !matches!(note.state, NoteState::Cleared | NoteState::Expired)
            })
            .collect()
    }
}

/// Tools a parallel chat instance may use. Every write-capable tool is removed
/// in the host before the request is built, not by prompt text (#145).
pub(crate) fn read_only_tools(tools: &[String]) -> Vec<String> {
    tools
        .iter()
        .filter(|tool| !write_tool(tool))
        .cloned()
        .collect()
}

pub(crate) fn write_tool(tool: &str) -> bool {
    matches!(
        tool,
        "sandbox_write"
            | "replay"
            | "run_registered_analysis"
            | "write_note"
            | "workspace_write"
            | "workspace_run"
            | "workspace_install"
    )
}

/// Build a read-only chat role: no write tools and no analysis sandbox. The
/// caller still uses the plane session log/notes/book as prompt facts.
pub(crate) fn read_only_role(role: &mut super::planner::ModelRole) {
    role.tools = read_only_tools(&role.tools);
    role.max_analysis_tools = 0;
}

/// Refuse an action commit built on a note set that has since changed (#143).
pub(crate) fn check_notes_version(root: &Path, session: &str, expected: u64) -> Result<()> {
    let current = Notes::new(root, session).version();
    if current == expected {
        Ok(())
    } else {
        Err("session_notes_changed".into())
    }
}

// ---------------------------------------------------------------------------
// Prompts, seeds and handoff
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NoteMark {
    pub id: String,
    pub revision: u64,
}

/// Parse the orchestrator's note marks. A mark always names the note revision
/// the model saw; a missing/invalid mark is a model failure, never a silent
/// success that drops or clobbers a note (#143).
pub(crate) fn note_marks(value: &Value) -> Result<(Vec<NoteMark>, Vec<NoteMark>)> {
    fn parse(value: &Value) -> Result<Vec<NoteMark>> {
        let Some(items) = value.as_array() else {
            return Err("session_note_marks_invalid".into());
        };
        if items.len() > 32 {
            return Err("session_note_marks_invalid".into());
        }
        items
            .iter()
            .map(|item| {
                let id = item["id"]
                    .as_str()
                    .filter(|id| !id.is_empty() && id.len() <= 64)
                    .ok_or("session_note_marks_invalid")?;
                let revision = item["revision"]
                    .as_u64()
                    .ok_or("session_note_marks_invalid")?;
                Ok(NoteMark {
                    id: id.to_owned(),
                    revision,
                })
            })
            .collect()
    }
    let done = match value.get("notes_done") {
        None => Vec::new(),
        Some(value) => parse(value)?,
    };
    let superseded = match value.get("notes_superseded") {
        None => Vec::new(),
        Some(value) => parse(value)?,
    };
    Ok((done, superseded))
}

pub(crate) fn prompt_version(system_prompt: &str, trigger_prompt: &str) -> String {
    super::digest(format!("{system_prompt}\0{trigger_prompt}").as_bytes())
}

fn read_existing_seed(path: &Path) -> Result<Option<Value>> {
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("session_seed_invalid".into()),
    };
    if !file
        .metadata()
        .map_err(|_| "session_seed_invalid")?
        .is_file()
    {
        return Err("session_seed_invalid".into());
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| "session_seed_invalid")?;
    if bytes.len() > 1_048_576 {
        return Err("session_seed_invalid".into());
    }
    // A previous attempt may have written valid JSON and then failed its
    // sync. An idempotent retry must still establish file durability.
    file.sync_all().map_err(|_| "session_seed_invalid")?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "session_seed_invalid".into())
}

/// Persist a successor seed once. Existing valid identical JSON makes retries
/// idempotent; a different, unreadable or partial artifact is never replaced.
pub(crate) fn write_seed(root: &Path, session: &str, seed: &Value) -> Result<()> {
    validate_seed(seed, session).map_err(std::io::Error::other)?;
    let directory = root.join(ROOT).join(session);
    fs::create_dir_all(&directory)?;
    let path = directory.join("seed.json");
    let bytes = serde_json::to_vec_pretty(seed)?;
    if bytes.len() > 1_048_576 {
        return Err("session_seed_invalid".into());
    }
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(mut file) => {
            // Keep a partial file as evidence if writing or syncing fails. A
            // later attempt will refuse it rather than silently replacing it.
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::File::open(&directory)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match read_existing_seed(&path)? {
                Some(existing) if existing == *seed => {
                    fs::File::open(&directory)?.sync_all()?;
                    Ok(())
                }
                Some(_) => Err("session_seed_conflict".into()),
                None => Err("session_seed_invalid".into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn seed_for_request(
    root: &Path,
    session: &str,
) -> std::result::Result<Value, &'static str> {
    let path = root.join(ROOT).join(session).join("seed.json");
    let mut file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(_) => return Err("session_seed_unreadable"),
    };
    if !file
        .metadata()
        .map_err(|_| "session_seed_unreadable")?
        .is_file()
    {
        return Err("session_seed_invalid");
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| "session_seed_unreadable")?;
    if bytes.len() > 1_048_576 {
        return Err("session_seed_invalid");
    }
    let seed: Value = serde_json::from_slice(&bytes).map_err(|_| "session_seed_invalid")?;
    validate_seed(&seed, session)?;
    Ok(seed)
}

pub(crate) fn validate_seed(seed: &Value, target: &str) -> std::result::Result<(), &'static str> {
    let Some(object) = seed.as_object() else {
        return Err("session_seed_invalid");
    };
    if !object.contains_key("schema_version") {
        // Older installations wrote the original unversioned seed contract.
        let source = object
            .get("source_session")
            .and_then(Value::as_str)
            .ok_or("session_seed_invalid")?;
        let old_target = object
            .get("target_session")
            .and_then(Value::as_str)
            .ok_or("session_seed_invalid")?;
        let source_date = chrono::NaiveDate::parse_from_str(source, "%Y-%m-%d")
            .map_err(|_| "session_seed_invalid")?;
        let target_date = chrono::NaiveDate::parse_from_str(target, "%Y-%m-%d")
            .map_err(|_| "session_seed_invalid")?;
        return if old_target == target && source_date < target_date {
            Ok(())
        } else {
            Err("session_seed_invalid")
        };
    }
    let version = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or("session_seed_invalid")?;
    if version != 2
        || object.keys().any(|key| {
            ![
                "schema_version",
                "source_session",
                "target_session",
                "source_turn_id",
                "source_receipt",
                "standing_instructions",
                "research_state",
                "keep_notes",
                "positions",
                "directives",
                "unresolved_work_refs",
                "unresolved_alerts",
                "review_state",
                "workspace_state",
            ]
            .contains(&key.as_str())
        })
        || seed["target_session"] != target
        || !seed["source_session"].is_string()
        || !seed["source_turn_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 256)
        || seed["standing_instructions"]
            .as_str()
            .is_none_or(|s| s.len() > 1_048_576)
        || !seed["keep_notes"].is_array()
        || !seed["positions"].is_array()
        || !seed["directives"].is_array()
        || !seed["unresolved_alerts"].is_array()
        || !seed["research_state"].is_object()
        || !seed["unresolved_work_refs"].is_object()
    {
        return Err("session_seed_invalid");
    }
    let source = seed["source_session"]
        .as_str()
        .ok_or("session_seed_invalid")?;
    let source_date = chrono::NaiveDate::parse_from_str(source, "%Y-%m-%d")
        .map_err(|_| "session_seed_invalid")?;
    let target_date = chrono::NaiveDate::parse_from_str(target, "%Y-%m-%d")
        .map_err(|_| "session_seed_invalid")?;
    if source_date >= target_date {
        return Err("session_seed_invalid");
    }
    let receipt = &seed["source_receipt"];
    if receipt["available"] == true {
        if !receipt["turn_id"].is_string()
            || !receipt["turn_seq"].is_u64()
            || receipt["result_seq"]
                .as_u64()
                .zip(receipt["turn_seq"].as_u64())
                .is_none_or(|(r, t)| r <= t)
            || !receipt["result_sha256"]
                .as_str()
                .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            || !receipt["action_receipts"].is_array()
        {
            return Err("session_seed_invalid");
        }
    } else if receipt["available"] != false || !receipt["reason"].is_string() {
        return Err("session_seed_invalid");
    }
    for position in seed["positions"].as_array().into_iter().flatten() {
        let Some(fields) = position.as_object() else {
            return Err("session_seed_invalid");
        };
        if fields.keys().any(|key| {
            !["instrument", "units", "entry", "mark", "marked_at"].contains(&key.as_str())
        }) || !position["instrument"].is_object()
            || !position["units"].is_u64()
            || !position["entry"].is_string()
            || !position["mark"].is_string()
            || !position["marked_at"].is_i64()
        {
            return Err("session_seed_invalid");
        }
        let instrument = position["instrument"]
            .as_object()
            .ok_or("session_seed_invalid")?;
        if instrument
            .keys()
            .any(|key| !["key", "symbol", "series", "tick"].contains(&key.as_str()))
            || !instrument.get("key").is_some_and(Value::is_string)
            || !instrument.get("symbol").is_some_and(Value::is_string)
            || !instrument.get("series").is_some_and(Value::is_string)
            || !instrument.get("tick").is_some_and(Value::is_string)
        {
            return Err("session_seed_invalid");
        }
    }
    if !seed["unresolved_work_refs"]["available"].is_boolean()
        || !seed["unresolved_work_refs"]["refs"].is_array()
        || !seed["research_state"]["available"].is_boolean()
        || (seed["research_state"]["available"] == false
            && !seed["research_state"]["reason"].is_string())
    {
        return Err("session_seed_invalid");
    }
    if let Some(review) = seed.get("review_state") {
        let Some(fields) = review.as_object() else {
            return Err("session_seed_invalid");
        };
        if fields
            .keys()
            .any(|key| !["available", "refs", "reason"].contains(&key.as_str()))
            || !review["available"].is_boolean()
        {
            return Err("session_seed_invalid");
        }
        if review["available"] == true {
            let Some(refs) = review["refs"].as_array() else {
                return Err("session_seed_invalid");
            };
            if refs.is_empty()
                || refs.len() > 8
                || review.get("reason").is_some()
                || refs.iter().any(|reference| {
                    let Some(fields) = reference.as_object() else {
                        return true;
                    };
                    fields.keys().any(|key| {
                        ![
                            "review_id",
                            "sealed_sha256",
                            "artifact_sha256",
                            "status",
                            "completed_ms",
                        ]
                        .contains(&key.as_str())
                    }) || !seed_hash(&reference["review_id"])
                        || !seed_hash(&reference["sealed_sha256"])
                        || !seed_hash(&reference["artifact_sha256"])
                        || !["completed", "failed"]
                            .contains(&reference["status"].as_str().unwrap_or_default())
                        || reference["completed_ms"].as_i64().is_none_or(|at| at <= 0)
                })
            {
                return Err("session_seed_invalid");
            }
        } else if !review["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty() && reason.len() <= 128)
            || review.get("refs").is_some()
        {
            return Err("session_seed_invalid");
        }
    }
    if let Some(workspace) = seed.get("workspace_state") {
        let Some(fields) = workspace.as_object() else {
            return Err("session_seed_invalid");
        };
        if fields
            .keys()
            .any(|key| !["version", "refs", "available", "reason"].contains(&key.as_str()))
            || workspace["version"].as_u64() != Some(1)
            || !workspace["available"].is_boolean()
        {
            return Err("session_seed_invalid");
        }
        if workspace["available"] == true {
            let Some(refs) = workspace["refs"].as_array() else {
                return Err("session_seed_invalid");
            };
            if refs.len() > 16 || workspace.get("reason").is_some() {
                return Err("session_seed_invalid");
            }
            for reference in refs {
                let Some(fields) = reference.as_object() else {
                    return Err("session_seed_invalid");
                };
                if fields.keys().any(|key| {
                    ![
                        "version",
                        "source_session",
                        "path",
                        "sha256",
                        "captured_at_ms",
                        "kind",
                    ]
                    .contains(&key.as_str())
                }) || reference["version"].as_u64() != Some(1)
                    || reference["source_session"].as_str() != seed["source_session"].as_str()
                    || reference["captured_at_ms"]
                        .as_i64()
                        .is_none_or(|at| at <= 0)
                    || !matches!(reference["kind"].as_str(), Some("notes" | "skills"))
                    || !seed_hash(&reference["sha256"])
                    || !reference["path"].as_str().is_some_and(|path| {
                        let first = path.split('/').next().unwrap_or_default();
                        first == reference["kind"].as_str().unwrap_or_default()
                            && path.len() <= 256
                            && super::workspace::normalize_rel(path).is_ok()
                    })
                {
                    return Err("session_seed_invalid");
                }
            }
        } else if !workspace["reason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty() && reason.len() <= 128)
            || workspace
                .get("refs")
                .is_some_and(|refs| refs.as_array().is_none_or(|refs| !refs.is_empty()))
        {
            return Err("session_seed_invalid");
        }
    }
    Ok(())
}

fn seed_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(crate) fn add_seed_fact(
    root: &Path,
    session: &str,
    facts: &mut Value,
) -> std::result::Result<(), &'static str> {
    facts["seed"] = seed_for_request(root, session)?;
    Ok(())
}

/// Build tomorrow's seed from host-owned facts. Never depends on the handoff
/// model reply: keep notes and positions survive a failed handoff (#144).
pub(crate) fn build_seed(root: &Path, session: &str, target: &str, context: &SeedContext) -> Value {
    let keep = Notes::new(root, session).keep_for_seed();
    // Position context is descriptive only. Strip fields which could be
    // mistaken for an executable order or protection instruction.
    let positions = context
        .positions
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let mut safe = serde_json::Map::new();
                    for key in ["units", "entry", "mark", "marked_at"] {
                        if let Some(value) = row.get(key) {
                            safe.insert(key.into(), value.clone());
                        }
                    }
                    if let Some(instrument) = row.get("instrument").and_then(Value::as_object) {
                        let safe_instrument = ["key", "symbol", "series", "tick"]
                            .into_iter()
                            .filter_map(|key| {
                                instrument
                                    .get(key)
                                    .map(|value| (key.to_owned(), value.clone()))
                            })
                            .collect();
                        safe.insert("instrument".into(), Value::Object(safe_instrument));
                    }
                    Value::Object(safe)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let safe_instructions = super::transcript::redact(&json!(context.standing_instructions));
    let source_receipt = SessionLog::new(root, session).latest_receipt_reference(root, session);
    let mut seed = json!({
        "schema_version": 2,
        "source_session": session,
        "target_session": target,
        "source_turn_id": context.source_turn_id,
        "source_receipt": source_receipt,
        "standing_instructions": safe_instructions,
        "research_state": context.research_state,
        "keep_notes": keep,
        "positions": positions,
        "directives": context.directives,
        "unresolved_work_refs": context.unresolved_work_refs,
        "unresolved_alerts": context.alerts,
    });
    // Only a reviewed owner-review policy writes this field. Older binaries
    // reject unknown v2 seed fields, so omission keeps rollback readable.
    if !context.review_state.is_null() {
        seed["review_state"] = context.review_state.clone();
    }
    // Only a reviewed workspace policy writes this field. Older binaries
    // reject unknown v2 seed fields, so omission keeps rollback readable.
    if !context.workspace_state.is_null() {
        seed["workspace_state"] = context.workspace_state.clone();
    }
    seed
}

pub(crate) struct SeedContext {
    pub source_turn_id: String,
    pub standing_instructions: String,
    pub positions: Value,
    pub directives: Value,
    pub alerts: Value,
    /// Explicitly unavailable until a durable research-state source is defined.
    pub research_state: Value,
    pub unresolved_work_refs: Value,
    /// Path-free refs to original owner-review artifacts, or null when the
    /// reviewed owner_tasks policy is absent.
    pub review_state: Value,
    /// Pinned original workspace refs, or null when the reviewed workspace
    /// policy is absent.
    pub workspace_state: Value,
}

pub(crate) fn bound_reply(reply: &str) -> String {
    let safe = super::transcript::redact(&json!(reply));
    let reply = safe.as_str().unwrap_or_default();
    if reply.len() <= MAX_REPLY_BYTES {
        return reply.to_owned();
    }
    let mut cut = MAX_REPLY_BYTES;
    while cut > 0 && !reply.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &reply[..cut])
}
