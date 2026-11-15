//! Durable supervisor state, bounded skip/failure records, and park recovery.
use super::super::rollover;
use super::AppliedHashes;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, sync::Mutex};

pub(super) const STATE_PATH: &str = "model/supervisor-state.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct State {
    pub(super) session: String,
    pub(super) last_run_ms: Option<i64>,
    pub(super) runs: u32,
    pub(super) luna_no_decide: u32,
    pub(super) failures: u32,
    /// A deterministic preflight failure blocks identical owner contracts,
    /// rather than consuming new decision turns against the same local bug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tool_failure: Option<ToolFailure>,
    pub(super) last_fill_count: usize,
    pub(super) decides: u32,
    pub(super) seen_decides: u32,
    pub(super) open_fired: bool,
    pub(super) pre_close_fired: bool,
    pub(super) skips: Vec<Skip>,
    /// Consecutive turns refused because the note set changed while in flight.
    #[serde(default)]
    pub(super) note_reruns: u32,
    #[serde(default)]
    pub(super) applied: Option<AppliedHashes>,
    #[serde(default)]
    pub(super) change_fired: Option<AppliedHashes>,
    #[serde(default)]
    pub(super) change_last_ms: Option<i64>,
    /// Durable stop after a charged allowance is exhausted; recovery requires
    /// an increased allowance above the preserved charged usage.
    #[serde(default)]
    pub(super) parked: Option<Parked>,
    /// Last failed compaction turn with its bounded reason and time.
    #[serde(default)]
    pub(super) compaction_failed: Option<CompactionFailure>,
    /// Highest session-log sequence reconciled for typed lifecycle results.
    #[serde(default)]
    pub(super) lifecycle_result_seq: u64,
    /// Measured bytes of the full provider request beyond the JSON-escaped
    /// rendered prompt (instructions, tools and envelope) from the last provider
    /// call. Runtime evidence, never configuration; absent keeps the historical
    /// prompt-only bound (#224).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) request_envelope_bytes: Option<u64>,
    /// A provider-incomplete response on a near-cap request is context
    /// exhaustion awaiting one bounded compaction retry (#224).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) context_exhaustion: Option<ContextExhaustion>,
    /// Optional owner failure backoff evidence. Absent keeps old state bytes
    /// and the historical wake-only failure streak.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_failure_ms: Option<i64>,
    /// The exact model-selection profile/revision that failed. A backoff never
    /// leaks to another profile or revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) last_failure_binding: Option<String>,
    /// Consecutive actual owner decision/supervisor failures under
    /// `last_failure_binding`. Review, compaction and host lifecycle failures
    /// never advance this gating counter. Absent keeps old state bytes and
    /// never retro-guesses a streak from the broad `failures` counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) owner_failure_streak: Option<u32>,
}

impl State {
    pub(super) fn fresh(session: &str) -> Self {
        Self {
            session: session.to_owned(),
            last_run_ms: None,
            runs: 0,
            luna_no_decide: 0,
            failures: 0,
            tool_failure: None,
            last_fill_count: 0,
            decides: 0,
            seen_decides: 0,
            open_fired: false,
            pre_close_fired: false,
            skips: Vec::new(),
            note_reruns: 0,
            applied: None,
            change_fired: None,
            change_last_ms: None,
            parked: None,
            compaction_failed: None,
            lifecycle_result_seq: 0,
            request_envelope_bytes: None,
            context_exhaustion: None,
            last_failure_ms: None,
            last_failure_binding: None,
            owner_failure_streak: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct ToolFailure {
    pub(super) reason: String,
    pub(super) at_ms: i64,
    pub(super) hashes: AppliedHashes,
}

impl State {
    pub(super) fn block_tool_failure(&mut self, reason: &str, now: i64, hashes: &AppliedHashes) {
        let code = reason.strip_prefix("supervisor_").unwrap_or(reason);
        if matches!(
            code,
            "harness_tool_definitions_changed"
                | "harness_tool_invalid"
                | "harness_owner_tools_invalid"
                | "harness_request_invalid"
                | "harness_final_tools_invalid"
                | "harness_citation_ids_invalid"
        ) {
            self.tool_failure = Some(ToolFailure {
                reason: code.to_owned(),
                at_ms: now,
                hashes: hashes.clone(),
            });
        }
    }
    pub(super) fn tool_contract_blocked(&self, hashes: &AppliedHashes) -> bool {
        self.tool_failure
            .as_ref()
            .is_some_and(|failure| &failure.hashes == hashes)
    }
}

/// A failed compaction leaves the last valid log in place; the reason stays
/// visible until a later compaction succeeds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct CompactionFailure {
    pub(super) reason: String,
    pub(super) at_ms: i64,
}

/// A provider-incomplete response on a request at or over the configured cap is
/// context exhaustion: the same oversized history fails again until it is
/// compacted. One in-cycle compaction retry is allowed, then a bounded backoff
/// replaces the every-cycle loop (#224).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ContextExhaustion {
    /// Bounded provider failure code, for example `model_response_incomplete`.
    pub(super) reason: String,
    /// Allowlisted provider-reported cause, never provider text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) incomplete_reason: Option<String>,
    /// Measured full provider request size at the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) request_bytes: Option<u64>,
    pub(super) max_request_bytes: u64,
    pub(super) at_ms: i64,
    /// Compaction retries already started for this exhaustion.
    #[serde(default)]
    pub(super) retries: u32,
}

/// A failed model turn with no allowance left is not a periodic retry: park the
/// supervisor until a reviewed allowance increase creates charged headroom (#197).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Parked {
    pub(super) reason: String,
    pub(super) at_ms: i64,
    /// Budget configured when parked. Older records have no baseline and use
    /// current charged usage as their only recovery bound.
    #[serde(default)]
    pub(super) budget_at_park: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Skip {
    at_ms: i64,
    reason: String,
}

fn io_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn state_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(STATE_PATH)
}

/// Read the state file without writing. A missing file starts a fresh session,
/// while an existing but unreadable or corrupt file is refused instead of
/// silently resetting the handoff counts (#156). A file for another session is
/// a fresh session.
fn read_state(root: &std::path::Path, session: &str) -> std::result::Result<State, &'static str> {
    match fs::read(state_path(root)) {
        Ok(bytes) => {
            let state: State =
                serde_json::from_slice(&bytes).map_err(|_| "supervisor_state_corrupt")?;
            Ok(if state.session == session {
                state
            } else {
                State::fresh(session)
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::fresh(session)),
        Err(_) => Err("supervisor_state_unreadable"),
    }
}

fn write_state(root: &std::path::Path, state: &State) -> std::result::Result<(), &'static str> {
    let path = state_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "supervisor_state_unwritable")?;
    }
    let value = serde_json::to_value(state).map_err(|_| "supervisor_state_unwritable")?;
    rollover::atomic(&path, &value).map_err(|_| "supervisor_state_unwritable")
}

/// One load-modify-save critical section. Two threads (Luna's worker and the
/// controller tick) can no longer overwrite each other's handoff counts between
/// separate lock acquisitions (#156).
pub(super) fn with_state<R>(
    root: &std::path::Path,
    session: &str,
    update: impl FnOnce(&mut State) -> R,
) -> std::result::Result<R, &'static str> {
    let _guard = io_lock();
    let mut state = read_state(root, session)?;
    let result = update(&mut state);
    write_state(root, &state)?;
    Ok(result)
}

pub(super) fn load(
    root: &std::path::Path,
    session: &str,
) -> std::result::Result<State, &'static str> {
    let _guard = io_lock();
    read_state(root, session)
}

/// Durable, bounded sidecar for a state/store failure. A corrupt state file is
/// never overwritten; the operator and the next wake still get a reason and time.
pub(super) fn record_state_error(root: &std::path::Path, session: &str, reason: &str) {
    let path = root.join("model/supervisor-error.json");
    let value = json!({"session":session,"reason":reason,"at_ms":super::super::millis()});
    if rollover::atomic(&path, &value).is_err() {
        eprintln!("supervisor_state_error_unrecorded reason={reason}");
    }
}

pub(super) fn record_skip(root: &std::path::Path, session: &str, now: i64, reason: &str) {
    let result =
        with_state(root, session, |state| {
            if state.skips.last().is_some_and(|skip| {
                skip.reason == reason && now.saturating_sub(skip.at_ms) < 60_000
            }) {
                return;
            }
            state.skips.push(Skip {
                at_ms: now,
                reason: reason.to_owned(),
            });
            let excess = state.skips.len().saturating_sub(32);
            state.skips.drain(..excess);
            eprintln!("supervisor_skipped reason={reason}");
        });
    if let Err(error) = result {
        eprintln!("supervisor_skip_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}

pub(super) fn allowance_recovery_allowed(
    parked: &Parked,
    current_allowance: u32,
    charged_usage: usize,
    usage_valid: bool,
) -> bool {
    parked.reason == "supervisor_budget_exhausted"
        && usage_valid
        && current_allowance as usize > charged_usage
        && parked
            .budget_at_park
            .is_some_and(|prior_allowance| current_allowance > prior_allowance)
}

pub(super) fn park(
    root: &std::path::Path,
    session: &str,
    now: i64,
    reason: &str,
    budget_at_park: u32,
) {
    let result = with_state(root, session, |state| {
        if state.parked.is_none() {
            state.parked = Some(Parked {
                reason: reason.to_owned(),
                at_ms: now,
                budget_at_park: Some(budget_at_park),
            });
        }
    });
    if let Err(error) = result {
        eprintln!("supervisor_park_unrecorded reason={error}");
        record_state_error(root, session, error);
    } else {
        eprintln!("supervisor_parked reason={reason}");
    }
}

pub(super) fn record_failure(root: &std::path::Path, session: &str, now: i64, reason: &str) {
    record_failure_with_evidence(root, session, now, reason, &Value::Null);
}

/// Bounded provider evidence retained beside the failure reason: the
/// incomplete cause and measured request/token sizes, never provider text (#224).
const FAILURE_EVIDENCE_FIELDS: &[&str] = &[
    "incomplete_reason",
    "request_bytes",
    "prompt_bytes",
    "max_output_tokens",
    "tokens_in",
    "tokens_out",
    "http_status",
];

pub(super) fn record_failure_with_evidence(
    root: &std::path::Path,
    session: &str,
    now: i64,
    reason: &str,
    evidence: &Value,
) {
    let path = root.join("model/supervisor-failure.json");
    let mut value = json!({"session":session,"reason":reason,"at_ms":now});
    if let (Value::Object(record), Value::Object(fields)) = (&mut value, evidence) {
        for key in FAILURE_EVIDENCE_FIELDS {
            if let Some(field) = fields.get(*key) {
                record.insert((*key).into(), field.clone());
            }
        }
    }
    if rollover::atomic(&path, &value).is_err() {
        eprintln!("supervisor_failure_unrecorded reason={reason}");
    }
}

/// Persist the measured provider-request envelope from the last provider call.
pub(super) fn record_request_envelope(root: &std::path::Path, session: &str, envelope_bytes: u64) {
    let result = with_state(root, session, |state| {
        state.request_envelope_bytes = Some(envelope_bytes);
    });
    if let Err(error) = result {
        eprintln!("supervisor_request_envelope_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}

/// Record context exhaustion from an incomplete provider response. A prior
/// retry count is preserved so a failed retry cannot restart the loop.
pub(super) fn record_context_exhaustion(
    root: &std::path::Path,
    session: &str,
    exhaustion: ContextExhaustion,
) {
    let result = with_state(root, session, |state| {
        let retries = state
            .context_exhaustion
            .as_ref()
            .map_or(0, |prior| prior.retries);
        state.context_exhaustion = Some(ContextExhaustion {
            retries,
            ..exhaustion
        });
    });
    if let Err(error) = result {
        eprintln!("supervisor_context_exhaustion_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}

pub(super) fn clear_context_exhaustion(root: &std::path::Path, session: &str) {
    let result = with_state(root, session, |state| state.context_exhaustion = None);
    if let Err(error) = result {
        eprintln!("supervisor_context_clear_unrecorded reason={error}");
        record_state_error(root, session, error);
    }
}
