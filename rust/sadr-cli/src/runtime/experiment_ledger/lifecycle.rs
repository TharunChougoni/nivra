//! Append-only strategy experiment lifecycle (#176).
//!
//! Specs are immutable. Transitions are an event log. `eligible` is not an
//! edge claim and never authorizes activation or paper execution.
use super::*;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const REQUEST_LIMIT: u64 = 256 * 1024;

/// The EXACT closed top-level key set for a continuation receipt. A stored body
/// with an additional key is not this schema, even with a recomputed self-hash;
/// unknown fields are never republished to the model or the controller.
const CONTINUATION_KEYS: &[&str] = &[
    "version",
    "financial_abi",
    "continuation_id",
    "state",
    "source_session",
    "source_task_id",
    "source_revision",
    "source_candidate_id",
    "source_lifecycle_candidate_id",
    "source_result_sha256",
    "source_result_status",
    "source_score_sha256",
    "source_observation_sha256",
    "source_entry_sha256",
    "source_spec_sha256",
    "source_quotes_sha256",
    "source_dataset_sha256",
    "source_runtime",
    "source_book_binding",
    "source_state_hash",
    "source_universe_sha256",
    "source_cohort_key",
    "current_scope",
    "key",
    "entry",
    "entry_quote",
    "exit_due_ms",
    "exit_deadline_ms",
    "exit_quote",
    "exit_status",
    "exit_gap",
    "first_receipt_unknown",
    "execution",
    "net_pnl_if_flat",
    "fees",
    "turnover",
    "flat",
    "accounting_complete",
    "gaps",
    "report_sha256",
    "source_scope",
    "capital_assumption",
    "capital",
    "observation_projection",
    "exit_at_ms",
    "processed_at_ms",
    "recorded_at_ms",
    "measurement_sha256",
];

const SOURCE_RUNTIME_KEYS: &[&str] = &[
    "runtime_hash",
    "helper_sha256",
    "unit_policy_sha256",
    "scorer_compare_sha256",
    "sandbox_source_sha256",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExperimentSpec {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_candidate_id: Option<String>,
    pub session: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_hash: String,
    pub feature_abi: String,
    pub hypothesis: String,
    pub training_from_ms: i64,
    pub training_to_ms: i64,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
    pub holding_ms: i64,
    pub max_exit_delay_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mark {
    key: String,
    entry_ms: i64,
    due_ms: i64,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quote_source_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    quote_receipt_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    disposition: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checks {
    leakage: String,
    oos: String,
    multiple_testing: String,
    reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    replay_report_sha256: Option<String>,
    marks: Vec<Mark>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    checks: Option<Checks>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observation_score: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    observation_measurement: Option<Value>,
    edge_qualified: bool,
    activation_authorized: bool,
}

impl Projection {
    fn blank() -> Self {
        Self {
            version: 1,
            replay_report_sha256: None,
            marks: Vec::new(),
            checks: None,
            observation_score: None,
            observation_measurement: None,
            edge_qualified: false,
            activation_authorized: false,
        }
    }
}

fn known_abi(abi: &str) -> bool {
    matches!(
        abi,
        "cost_aware_momentum_v1"
            | "cash_quote_action_tape_v1"
            | "scorer_replay_v1"
            | "observation_single_entry_v1"
    )
}

fn abi_ok(abi: &str) -> bool {
    !abi.is_empty()
        && abi.len() <= 64
        && abi
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

pub(crate) fn candidate_id(spec: &ExperimentSpec) -> String {
    super::trial_id(&[
        "lifecycle-v1",
        &spec.version.to_string(),
        &spec.session,
        spec.parent_candidate_id.as_deref().unwrap_or(""),
        &spec.source_sha256,
        &spec.params_sha256,
        &spec.dataset_hash,
        &spec.feature_abi,
        &spec.hypothesis,
        &spec.training_from_ms.to_string(),
        &spec.training_to_ms.to_string(),
        &spec.selection_from_ms.to_string(),
        &spec.selection_to_ms.to_string(),
        &spec.holding_ms.to_string(),
        &spec.max_exit_delay_ms.to_string(),
    ])
}

fn validate_spec(spec: &ExperimentSpec) -> Result<()> {
    if spec.version != 1
        || spec.session.is_empty()
        || spec.session.len() > 32
        || spec
            .session
            .bytes()
            .any(|b| !(b.is_ascii_digit() || b == b'-'))
        || !super::valid_hash(&spec.source_sha256)
        || !super::valid_hash(&spec.params_sha256)
        || !super::valid_hash(&spec.dataset_hash)
        || !abi_ok(&spec.feature_abi)
        || spec.hypothesis.trim().is_empty()
        || spec.hypothesis.len() > 1024
        || spec.hypothesis.chars().any(char::is_control)
        || spec.training_from_ms < 0
        || spec.training_from_ms > spec.training_to_ms
        || spec.training_to_ms >= spec.selection_from_ms
        || spec.selection_from_ms > spec.selection_to_ms
        || spec.selection_to_ms < 0
        || !(1..=86_400_000).contains(&spec.holding_ms)
        || !(0..=60_000).contains(&spec.max_exit_delay_ms)
    {
        return Err("lifecycle_spec_invalid".into());
    }
    if let Some(parent) = &spec.parent_candidate_id
        && !super::valid_hash(parent)
    {
        return Err("lifecycle_parent_invalid".into());
    }
    Ok(())
}

/// Persisted lifecycle phase. Unknown historic names fail `parse` so a
/// corrupt transition is refused, while `list` still returns the raw TEXT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LifecycleState {
    Proposed,
    SandboxAbi,
    Replayed,
    ForwardShadow,
    Evaluated,
    Eligible,
    Rejected,
    Insufficient,
    PaperSelected,
    Retired,
}

impl LifecycleState {
    pub(crate) fn parse(raw: &str) -> std::result::Result<Self, &'static str> {
        match raw {
            "proposed" => Ok(Self::Proposed),
            "sandbox_abi" => Ok(Self::SandboxAbi),
            "replayed" => Ok(Self::Replayed),
            "forward_shadow" => Ok(Self::ForwardShadow),
            "evaluated" => Ok(Self::Evaluated),
            "eligible" => Ok(Self::Eligible),
            "rejected" => Ok(Self::Rejected),
            "insufficient" => Ok(Self::Insufficient),
            "paper_selected" => Ok(Self::PaperSelected),
            "retired" => Ok(Self::Retired),
            _ => Err("experiment_lifecycle_state_unknown"),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::SandboxAbi => "sandbox_abi",
            Self::Replayed => "replayed",
            Self::ForwardShadow => "forward_shadow",
            Self::Evaluated => "evaluated",
            Self::Eligible => "eligible",
            Self::Rejected => "rejected",
            Self::Insufficient => "insufficient",
            Self::PaperSelected => "paper_selected",
            Self::Retired => "retired",
        }
    }

    pub(crate) fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Proposed, Self::SandboxAbi)
                | (Self::Proposed, Self::Rejected)
                | (Self::Proposed, Self::Retired)
                | (Self::SandboxAbi, Self::Replayed)
                | (Self::SandboxAbi, Self::Rejected)
                | (Self::SandboxAbi, Self::Retired)
                | (Self::Replayed, Self::ForwardShadow)
                // A prospective observation episode is matured directly from
                // its recorded entry/exit refs; it has no fixed-basket shadow
                // marks to open first. `apply_transition` scopes this edge to
                // observation-purpose specs (#232).
                | (Self::Replayed, Self::Evaluated)
                | (Self::Replayed, Self::Rejected)
                | (Self::Replayed, Self::Retired)
                | (Self::ForwardShadow, Self::Evaluated)
                | (Self::ForwardShadow, Self::Rejected)
                | (Self::ForwardShadow, Self::Retired)
                | (Self::Evaluated, Self::Eligible)
                | (Self::Evaluated, Self::Rejected)
                | (Self::Evaluated, Self::Insufficient)
                | (Self::Evaluated, Self::Retired)
                | (Self::Eligible, Self::PaperSelected)
                | (Self::Eligible, Self::Retired)
                | (Self::Rejected, Self::Retired)
                | (Self::Insufficient, Self::Retired)
                | (Self::PaperSelected, Self::Retired)
        )
    }
}

fn allowed(from: &str, to: &str) -> bool {
    match (LifecycleState::parse(from), LifecycleState::parse(to)) {
        (Ok(from), Ok(to)) => from.can_transition_to(to),
        _ => false,
    }
}

fn load(
    db: &rusqlite::Connection,
    candidate_id: &str,
) -> Result<(ExperimentSpec, String, Projection)> {
    let spec_body: String = db
        .query_row(
            "SELECT body FROM experiment_lifecycle_specs WHERE candidate_id=?",
            [candidate_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or("lifecycle_candidate_missing")?;
    let (state, projection): (String, String) = db
        .query_row(
            "SELECT state,projection FROM experiment_lifecycle_state WHERE candidate_id=?",
            [candidate_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or("lifecycle_state_missing")?;
    let spec: ExperimentSpec = serde_json::from_str(&spec_body)?;
    let projection: Projection = serde_json::from_str(&projection)?;
    if projection.edge_qualified || projection.activation_authorized {
        return Err("lifecycle_authority_invalid".into());
    }
    Ok((spec, state, projection))
}

fn receipt(
    candidate_id: &str,
    spec: &ExperimentSpec,
    state: &str,
    projection: &Projection,
) -> Value {
    json!({
        "candidate_id": candidate_id,
        "parent_candidate_id": spec.parent_candidate_id,
        "session": spec.session,
        "state": state,
        "feature_abi": spec.feature_abi,
        "hypothesis": spec.hypothesis,
        "source_sha256": spec.source_sha256,
        "params_sha256": spec.params_sha256,
        "dataset_hash": spec.dataset_hash,
        "training_from_ms": spec.training_from_ms,
        "training_to_ms": spec.training_to_ms,
        "selection_from_ms": spec.selection_from_ms,
        "selection_to_ms": spec.selection_to_ms,
        "holding_ms": spec.holding_ms,
        "max_exit_delay_ms": spec.max_exit_delay_ms,
        "replay_report_sha256": projection.replay_report_sha256,
        "marks": projection.marks,
        "checks": projection.checks,
        "observation_score": projection.observation_score,
        "observation_measurement": projection.observation_measurement,
        "scientific_state": scientific_state(state, projection.observation_measurement.is_some()),
        "edge_qualified": false,
        "activation_authorized": false
    })
}

/// Only a prospective-observation spec may skip forward shadow marks.
fn is_observation(spec: &ExperimentSpec) -> bool {
    spec.feature_abi == sadr_cli::candidate_execution::OBSERVATION_ABI
}

/// Scientific vocabulary for a lifecycle state. An observation episode parked
/// at `evaluated` with its sealed after-cost measurement is
/// `observation_measured`: fixed-basket checks never ran for it, so it is not
/// reported as if they had found insufficient evidence (#232). It grants no
/// activation authority.
pub(crate) fn scientific_state(state: &str, observation_measured: bool) -> &'static str {
    match state {
        "eligible" | "paper_selected" => "eligible",
        "rejected" => "rejected",
        "retired" => "retired",
        "evaluated" if observation_measured => "observation_measured",
        _ => "insufficient_evidence",
    }
}

fn apply_transition(
    directory: &Path,
    candidate_id: &str,
    to: &str,
    event: &str,
    evidence: &Value,
    now: i64,
    update: impl FnOnce(&ExperimentSpec, &str, &mut Projection) -> Result<()>,
) -> Result<Value> {
    if now < 0 {
        return Err("lifecycle_clock_invalid".into());
    }
    let evidence_sha = sadr_core::replay::digest(serde_json::to_string(evidence)?.as_bytes());
    let mut db = super::open_directory(directory)?;
    let (spec, from, mut projection) = load(&db, candidate_id)?;
    if from == to {
        let last: Option<String> = db
            .query_row(
                "SELECT evidence_sha256 FROM experiment_lifecycle_events WHERE candidate_id=? ORDER BY id DESC LIMIT 1",
                [candidate_id],
                |row| row.get(0),
            )
            .optional()?;
        if last.as_deref() == Some(evidence_sha.as_str()) {
            return Ok(receipt(candidate_id, &spec, &from, &projection));
        }
    }
    if !allowed(&from, to) || (from == "replayed" && to == "evaluated" && !is_observation(&spec)) {
        return Err("lifecycle_transition_invalid".into());
    }
    update(&spec, &from, &mut projection)?;
    projection.edge_qualified = false;
    projection.activation_authorized = false;
    let projection_body = serde_json::to_string(&projection)?;
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE experiment_lifecycle_state SET state=?, projection=?, updated_at_ms=? WHERE candidate_id=?",
        params![to, projection_body, now, candidate_id],
    )?;
    tx.execute(
        "INSERT INTO experiment_lifecycle_events(candidate_id,from_state,to_state,event,evidence_sha256,at_ms) VALUES(?,?,?,?,?,?)",
        params![candidate_id, from, to, event, evidence_sha, now],
    )?;
    tx.commit()?;
    Ok(receipt(candidate_id, &spec, to, &projection))
}

pub(crate) fn admit_spec(directory: &Path, spec: ExperimentSpec, now: i64) -> Result<Value> {
    admit_spec_inner(directory, spec, now, false)
}

/// Admit a spec whose typed cross-session parent was already verified read-only
/// against the predecessor queue/carry chain. The predecessor lifecycle row is
/// not copied; the current ledger records only its verified parent id.
pub(crate) fn admit_spec_verified_parent(
    directory: &Path,
    spec: ExperimentSpec,
    now: i64,
) -> Result<Value> {
    admit_spec_inner(directory, spec, now, true)
}

fn admit_spec_inner(
    directory: &Path,
    spec: ExperimentSpec,
    now: i64,
    parent_verified: bool,
) -> Result<Value> {
    validate_spec(&spec)?;
    if now < 0 {
        return Err("lifecycle_clock_invalid".into());
    }
    if !parent_verified && let Some(parent) = &spec.parent_candidate_id {
        let db = super::open_directory(directory)?;
        let exists: Option<String> = db
            .query_row(
                "SELECT candidate_id FROM experiment_lifecycle_specs WHERE candidate_id=?",
                [parent],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err("lifecycle_parent_missing".into());
        }
    }
    let id = candidate_id(&spec);
    let body = serde_json::to_string(&spec)?;
    let mut db = super::open_directory(directory)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT body FROM experiment_lifecycle_specs WHERE candidate_id=?",
            [&id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != body {
            return Err("lifecycle_identity_changed".into());
        }
        let (_, state, projection) = load(&db, &id)?;
        return Ok(receipt(&id, &spec, &state, &projection));
    }
    let projection = Projection::blank();
    let projection_body = serde_json::to_string(&projection)?;
    let evidence_sha = sadr_core::replay::digest(body.as_bytes());
    let tx = db.transaction()?;
    tx.execute(
        "INSERT INTO experiment_lifecycle_specs(candidate_id,body,recorded_at_ms) VALUES(?,?,?)",
        params![id, body, now],
    )?;
    tx.execute(
        "INSERT INTO experiment_lifecycle_state(candidate_id,state,projection,updated_at_ms) VALUES(?,?,?,?)",
        params![id, LifecycleState::Proposed.as_str(), projection_body, now],
    )?;
    tx.execute(
        "INSERT INTO experiment_lifecycle_events(candidate_id,from_state,to_state,event,evidence_sha256,at_ms) VALUES(?,?,?,?,?,?)",
        params![id, Option::<String>::None, LifecycleState::Proposed.as_str(), "admitted", evidence_sha, now],
    )?;
    tx.commit()?;
    Ok(receipt(
        &id,
        &spec,
        LifecycleState::Proposed.as_str(),
        &projection,
    ))
}

pub(crate) fn record_abi(directory: &Path, id: &str, now: i64) -> Result<Value> {
    let db = super::open_directory(directory)?;
    let (spec, _, _) = load(&db, id)?;
    if !known_abi(&spec.feature_abi) {
        return Err("lifecycle_abi_unknown".into());
    }
    apply_transition(
        directory,
        id,
        "sandbox_abi",
        "abi_admitted",
        &json!({"feature_abi": spec.feature_abi}),
        now,
        |_, _, _| Ok(()),
    )
}

fn replay_binds_spec(spec: &ExperimentSpec, report: &Value) -> Result<()> {
    if report["schema"].as_u64() != Some(1) {
        return Err("lifecycle_replay_invalid".into());
    }
    if report["promotion_authorized"] == true {
        return Err("lifecycle_promotion_refused".into());
    }
    let source = report["source_sha256"].as_str().unwrap_or("");
    let params = report["params_sha256"].as_str().unwrap_or("");
    if source != spec.source_sha256 || params != spec.params_sha256 {
        return Err("lifecycle_replay_binding_mismatch".into());
    }
    if let Some(abi) = report["feature_abi"].as_str()
        && abi != spec.feature_abi
    {
        return Err("lifecycle_replay_abi_mismatch".into());
    }
    Ok(())
}

/// Record one verified prospective-observation score as lifecycle evidence.
/// This appends to the existing lifecycle ledger; it creates no new table,
/// trial authority or copied journal.
pub(crate) fn record_observation(
    directory: &Path,
    id: &str,
    score: &Value,
    now: i64,
) -> Result<Value> {
    let score_sha = sadr_core::replay::digest(serde_json::to_string(score)?.as_bytes());
    apply_transition(
        directory,
        id,
        "replayed",
        "observation_bound",
        &json!({"observation_sha256": score_sha}),
        now,
        |_, _, projection| {
            projection.observation_score = Some(score.clone());
            Ok(())
        },
    )
}

/// Private sealing module. The trait is not nameable outside this module, so
/// only the concrete validated token types granted here can satisfy the
/// production writer bounds. A sibling module cannot mint a new implementor.
mod sealed {
    pub trait Sealed {}
}

/// A measurement body that a canonical source validator has already checked.
/// The trait is sealed: only the concrete token types below (whose fields are
/// private to the validating modules) can implement it, so raw JSON cannot be
/// recorded through the production writer.
pub(crate) trait ObservationMeasurementBody: sealed::Sealed {
    fn body(&self) -> &Value;
}

impl sealed::Sealed for crate::runtime::harness::research::ValidatedObservationMeasurement {}

/// Record the matured after-cost measurement for one observation episode.
pub(crate) fn record_observation_measurement<T: ObservationMeasurementBody>(
    directory: &Path,
    id: &str,
    token: &T,
    now: i64,
) -> Result<Value> {
    let measurement = token.body();
    // Read-time metadata is outside the durable evidence identity, so a later
    // read of the same measurement is idempotent instead of a new transition.
    let mut stable = measurement.clone();
    if let Some(object) = stable.as_object_mut() {
        object.insert("measured_at_ms".into(), json!(0));
        object.insert("measurement_sha256".into(), json!(""));
    }
    let measurement_sha = sadr_core::replay::digest(serde_json::to_string(&stable)?.as_bytes());
    apply_transition(
        directory,
        id,
        "evaluated",
        "observation_matured",
        &json!({"measurement_sha256": measurement_sha}),
        now,
        |spec, _, projection| {
            if !is_observation(spec) {
                return Err("lifecycle_observation_purpose_required".into());
            }
            projection.observation_measurement = Some(measurement.clone());
            Ok(())
        },
    )
}

pub(crate) fn record_replay(directory: &Path, id: &str, report: &Value, now: i64) -> Result<Value> {
    let report_sha = sadr_core::replay::digest(serde_json::to_string(report)?.as_bytes());
    apply_transition(
        directory,
        id,
        "replayed",
        "replay_bound",
        &json!({"report_sha256": report_sha}),
        now,
        |spec, _, projection| {
            replay_binds_spec(spec, report)?;
            projection.replay_report_sha256 = Some(report_sha.clone());
            Ok(())
        },
    )
}

fn parse_marks(spec: &ExperimentSpec, raw: &Value) -> Result<Vec<Mark>> {
    let rows = raw.as_array().ok_or("lifecycle_marks_invalid")?;
    if rows.is_empty() || rows.len() > 5_000 {
        return Err("lifecycle_marks_invalid".into());
    }
    let mut marks = Vec::with_capacity(rows.len());
    for row in rows {
        let key = row["key"].as_str().ok_or("lifecycle_marks_invalid")?;
        let entry_ms = row["entry_ms"].as_i64().ok_or("lifecycle_marks_invalid")?;
        let due_ms = row["due_ms"].as_i64().ok_or("lifecycle_marks_invalid")?;
        if key.is_empty()
            || key.len() > 64
            || key.chars().any(char::is_control)
            || entry_ms < spec.selection_from_ms
            || entry_ms > spec.selection_to_ms
            || due_ms
                != entry_ms
                    .checked_add(spec.holding_ms)
                    .ok_or("lifecycle_marks_invalid")?
        {
            return Err("lifecycle_marks_invalid".into());
        }
        marks.push(Mark {
            key: key.to_owned(),
            entry_ms,
            due_ms,
            status: "pending".into(),
            quote_source_ms: None,
            quote_receipt_ms: None,
            disposition: None,
        });
    }
    Ok(marks)
}

pub(crate) fn open_shadow(directory: &Path, id: &str, marks: &Value, now: i64) -> Result<Value> {
    apply_transition(
        directory,
        id,
        "forward_shadow",
        "shadow_opened",
        marks,
        now,
        |spec, _, projection| {
            projection.marks = parse_marks(spec, marks)?;
            Ok(())
        },
    )
}

fn apply_quotes(
    spec: &ExperimentSpec,
    projection: &mut Projection,
    quotes: &Value,
    now: i64,
) -> Result<()> {
    let rows = quotes["quotes"]
        .as_array()
        .ok_or("lifecycle_quotes_invalid")?;
    if rows.len() > 10_000 {
        return Err("lifecycle_quotes_invalid".into());
    }
    for row in rows {
        if row["interpolated"].as_bool() == Some(true) {
            return Err("lifecycle_interpolated_quote_refused".into());
        }
        let key = row["key"].as_str().ok_or("lifecycle_quotes_invalid")?;
        let source_ms = row["source_ms"]
            .as_i64()
            .ok_or("lifecycle_quotes_invalid")?;
        let receipt_ms = row["receipt_ms"]
            .as_i64()
            .ok_or("lifecycle_quotes_invalid")?;
        if key.is_empty() || source_ms < 0 || receipt_ms < source_ms {
            return Err("lifecycle_quotes_invalid".into());
        }
        for mark in &mut projection.marks {
            if mark.status != "pending" || mark.key != key {
                continue;
            }
            let latest = mark
                .due_ms
                .checked_add(spec.max_exit_delay_ms)
                .ok_or("lifecycle_quotes_invalid")?;
            if receipt_ms >= mark.due_ms && receipt_ms <= latest {
                mark.status = "matured".into();
                mark.quote_source_ms = Some(source_ms);
                mark.quote_receipt_ms = Some(receipt_ms);
                mark.disposition = None;
            }
        }
    }
    if let Some(end) = quotes["end_of_evidence_ms"].as_i64() {
        if end < 0 {
            return Err("lifecycle_quotes_invalid".into());
        }
        for mark in &mut projection.marks {
            if mark.status != "pending" {
                continue;
            }
            let latest = mark
                .due_ms
                .checked_add(spec.max_exit_delay_ms)
                .ok_or("lifecycle_quotes_invalid")?;
            if end > latest {
                mark.status = "unknown_gap".into();
                mark.disposition = Some("source_passed_window_without_quote".into());
            } else if now >= latest {
                // The finite source has no acceptable quote inside the declared
                // exit window. Terminate honestly instead of leaving the mark
                // pending forever or inventing a future quote.
                mark.status = "unknown_gap".into();
                mark.disposition = Some(if end < mark.due_ms {
                    "source_ended_before_due".into()
                } else {
                    "missing_quote_window_closed".into()
                });
            }
        }
    }
    Ok(())
}

fn family_count(directory: &Path, spec: &ExperimentSpec, self_id: &str) -> Result<usize> {
    let db = super::open_directory(directory)?;
    let mut query = db.prepare(
        "SELECT s.candidate_id,s.body,st.state FROM experiment_lifecycle_specs s JOIN experiment_lifecycle_state st USING(candidate_id)",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut count = 0usize;
    for row in rows {
        let (id, body, state) = row?;
        if id == self_id || state == "retired" {
            continue;
        }
        let other: ExperimentSpec = serde_json::from_str(&body)?;
        if other.source_sha256 == spec.source_sha256
            && other.params_sha256 == spec.params_sha256
            && other.training_from_ms == spec.training_from_ms
            && other.training_to_ms == spec.training_to_ms
            && other.session == spec.session
        {
            count = count.saturating_add(1);
        }
    }
    Ok(count.saturating_add(1))
}

fn run_checks(
    directory: &Path,
    id: &str,
    spec: &ExperimentSpec,
    projection: &Projection,
) -> Checks {
    let leakage = if spec.training_to_ms < spec.selection_from_ms {
        "pass"
    } else {
        "fail"
    };
    let matured = projection
        .marks
        .iter()
        .filter(|mark| mark.status == "matured")
        .count();
    let pending = projection.marks.iter().any(|mark| mark.status == "pending");
    let oos = if pending || projection.marks.is_empty() {
        "incomplete"
    } else if matured == projection.marks.len() {
        "pass"
    } else {
        "incomplete"
    };
    let family = family_count(directory, spec, id).unwrap_or(usize::MAX);
    let multiple = if family == 1 { "pass" } else { "unadjusted" };
    let reason = if leakage != "pass" {
        "leakage_training_overlaps_selection"
    } else if oos != "pass" {
        "oos_marks_incomplete"
    } else if multiple != "pass" {
        "multiple_testing_unadjusted"
    } else {
        "checks_passed"
    };
    Checks {
        leakage: leakage.into(),
        oos: oos.into(),
        multiple_testing: multiple.into(),
        reason: reason.into(),
    }
}

fn conclude(directory: &Path, id: &str, now: i64) -> Result<Value> {
    let db = super::open_directory(directory)?;
    let (spec, state, projection) = load(&db, id)?;
    if state != "forward_shadow" && state != "evaluated" {
        return Err("lifecycle_transition_invalid".into());
    }
    // A prospective-observation episode is concluded by its own recorded
    // after-cost measurement, not by fixed-basket ranking marks. Re-reading it
    // is idempotent and never an error on later controller ticks.
    if state == "evaluated" && projection.observation_measurement.is_some() {
        return Ok(receipt(id, &spec, &state, &projection));
    }
    if projection.marks.is_empty() || projection.marks.iter().any(|mark| mark.status == "pending") {
        return Err("lifecycle_marks_pending".into());
    }
    let evaluated = if state == "forward_shadow" {
        apply_transition(
            directory,
            id,
            "evaluated",
            "marks_complete",
            &json!({"marks": projection.marks}),
            now,
            |_, _, _| Ok(()),
        )?
    } else {
        receipt(id, &spec, &state, &projection)
    };
    let db = super::open_directory(directory)?;
    let (spec, state, projection) = load(&db, id)?;
    if state != "evaluated" {
        return Ok(evaluated);
    }
    let checks = run_checks(directory, id, &spec, &projection);
    let next = match checks.reason.as_str() {
        "checks_passed" => "eligible",
        "leakage_training_overlaps_selection" => "rejected",
        _ => "insufficient",
    };
    apply_transition(
        directory,
        id,
        next,
        "scientific_checks",
        &serde_json::to_value(&checks)?,
        now,
        |_, _, projection| {
            projection.checks = Some(checks.clone());
            Ok(())
        },
    )
}

pub(crate) fn mature(directory: &Path, id: &str, quotes: &Value, now: i64) -> Result<Value> {
    let db = super::open_directory(directory)?;
    let (_, state, _) = load(&db, id)?;
    if state != "forward_shadow" {
        return Err("lifecycle_transition_invalid".into());
    }
    let mut db = super::open_directory(directory)?;
    let (spec, _, mut projection) = load(&db, id)?;
    apply_quotes(&spec, &mut projection, quotes, now)?;
    projection.edge_qualified = false;
    projection.activation_authorized = false;
    let projection_body = serde_json::to_string(&projection)?;
    let evidence_sha = sadr_core::replay::digest(serde_json::to_string(quotes)?.as_bytes());
    let tx = db.transaction()?;
    tx.execute(
        "UPDATE experiment_lifecycle_state SET projection=?, updated_at_ms=? WHERE candidate_id=?",
        params![projection_body, now, id],
    )?;
    tx.execute(
        "INSERT INTO experiment_lifecycle_events(candidate_id,from_state,to_state,event,evidence_sha256,at_ms) VALUES(?,?,?,?,?,?)",
        params![id, "forward_shadow", "forward_shadow", "marks_updated", evidence_sha, now],
    )?;
    tx.commit()?;
    if projection.marks.iter().all(|mark| mark.status != "pending") {
        return conclude(directory, id, now);
    }
    Ok(receipt(id, &spec, "forward_shadow", &projection))
}

/// Pure mechanical projection for a conclusion that was already committed but
/// not yet transitioned. A crash between the terminal mark update and
/// `conclude` leaves `forward_shadow` with no pending marks; an `evaluated`
/// row may also be half-finished. Retrying is idempotent because the original
/// ledger is the only authority and a terminal state returns `None`.
pub(crate) fn conclude_if_ready(directory: &Path, id: &str, now: i64) -> Result<Option<Value>> {
    let db = super::open_directory(directory)?;
    let (_, state, projection) = load(&db, id)?;
    match state.as_str() {
        "forward_shadow" => {
            if projection.marks.is_empty()
                || projection.marks.iter().any(|mark| mark.status == "pending")
            {
                return Ok(None);
            }
            Ok(Some(conclude(directory, id, now)?))
        }
        "evaluated" => Ok(Some(conclude(directory, id, now)?)),
        _ => Ok(None),
    }
}

/// Checked read-only lifecycle row used for cross-session parent provenance.
/// A missing ledger or row is `None`; an unreadable/corrupt ledger is an
/// explicit error, never silently treated as "no parent".
pub(crate) fn verified_parent(
    directory: &Path,
    id: &str,
) -> Result<Option<(ExperimentSpec, String)>> {
    verified_parent_with(directory, id, false)
}

/// Immutable authority read of one CLOSED predecessor lifecycle row.
pub(crate) fn verified_parent_immutable(
    directory: &Path,
    id: &str,
) -> Result<Option<(ExperimentSpec, String)>> {
    verified_parent_with(directory, id, true)
}

fn verified_parent_with(
    directory: &Path,
    id: &str,
    immutable: bool,
) -> Result<Option<(ExperimentSpec, String)>> {
    let file = super::file_path(directory);
    if !file.is_file() {
        return Ok(None);
    }
    let db = if immutable {
        super::super::open_ledger_immutable(&file)?
    } else {
        rusqlite::Connection::open_with_flags(
            &file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    };
    db.busy_timeout(std::time::Duration::ZERO)?;
    let body: Option<String> = db
        .query_row(
            "SELECT body FROM experiment_lifecycle_specs WHERE candidate_id=?",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else {
        return Ok(None);
    };
    let state: Option<String> = db
        .query_row(
            "SELECT state FROM experiment_lifecycle_state WHERE candidate_id=?",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let state = state.ok_or("lifecycle_state_missing")?;
    let spec: ExperimentSpec = serde_json::from_str(&body)?;
    Ok(Some((spec, state)))
}

/// Read the persisted lifecycle phase without creating or migrating the
/// ledger. A missing file or candidate is `None`, never an invented state.
pub(crate) fn state(directory: &Path, id: &str) -> Result<Option<String>> {
    let file = super::file_path(directory);
    if !file.is_file() {
        return Ok(None);
    }
    let db = rusqlite::Connection::open_with_flags(
        &file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    let state: Option<String> = db
        .query_row(
            "SELECT state FROM experiment_lifecycle_state WHERE candidate_id=?",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(state)
}

/// Pending marks for a forward-shadow candidate. These are the original
/// declared entry/due clocks; no quote or fill is synthesized here.
pub(crate) fn pending_marks(directory: &Path, id: &str) -> Result<Vec<Value>> {
    let db = super::open_directory(directory)?;
    let (_, state, projection) = load(&db, id)?;
    if state != "forward_shadow" {
        return Ok(Vec::new());
    }
    Ok(projection
        .marks
        .iter()
        .filter(|mark| mark.status == "pending")
        .map(|mark| {
            json!({
                "key":mark.key,
                "entry_ms":mark.entry_ms,
                "due_ms":mark.due_ms
            })
        })
        .collect())
}

pub(crate) fn select_paper(directory: &Path, id: &str, now: i64) -> Result<Value> {
    apply_transition(
        directory,
        id,
        "paper_selected",
        "paper_selected",
        &json!({"activation_authorized": false}),
        now,
        |_, _, _| Ok(()),
    )
}

pub(crate) fn retire(directory: &Path, id: &str, reason: &str, now: i64) -> Result<Value> {
    if reason.is_empty()
        || reason.len() > 96
        || reason
            .bytes()
            .any(|b| !(b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    {
        return Err("lifecycle_reason_invalid".into());
    }
    apply_transition(
        directory,
        id,
        "retired",
        "retired",
        &json!({"reason": reason}),
        now,
        |_, _, _| Ok(()),
    )
}

fn numeric_score(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.as_f64().is_some_and(|n| n.is_finite()),
        _ => false,
    }
}

pub(crate) fn ingest_workspace(
    directory: &Path,
    settings: &super::super::workspace::WorkspaceSettings,
    report_rel: &str,
    spec_fields: ExperimentSpec,
    now: i64,
) -> Result<Value> {
    use super::super::workspace::{WorkspaceOp, execute};
    settings.validate()?;
    let read = execute(
        directory,
        settings,
        WorkspaceOp::Read(report_rel.to_owned()),
    )?;
    let text = read["text"].as_str().ok_or("lifecycle_ingest_unreadable")?;
    let report: Value = serde_json::from_str(text).map_err(|_| "lifecycle_ingest_invalid")?;
    if report["execution_authorized"] == true {
        return Err("lifecycle_ingest_execution_refused".into());
    }
    if report["authority"] != "advisory_scores_not_orders" || report["status"] != "ok" {
        return Err("lifecycle_ingest_not_advisory".into());
    }
    if report.get("orders").is_some() || report.get("fills").is_some() {
        return Err("lifecycle_ingest_orders_refused".into());
    }
    let scores = report["scores"]
        .as_object()
        .ok_or("lifecycle_ingest_scores_invalid")?;
    if scores.is_empty() || !scores.values().all(numeric_score) {
        return Err("lifecycle_ingest_scores_invalid".into());
    }
    let script = report["script"]
        .as_str()
        .ok_or("lifecycle_ingest_script_missing")?;
    let script_read = execute(directory, settings, WorkspaceOp::Read(script.to_owned()))?;
    let source_sha256 = script_read["sha256"]
        .as_str()
        .ok_or("lifecycle_ingest_script_missing")?
        .to_owned();
    let params_sha256 = match report["params_source"].as_str() {
        Some("input") => return Err("lifecycle_ingest_params_missing".into()),
        Some(path) => {
            let params = execute(directory, settings, WorkspaceOp::Read(path.to_owned()))?;
            params["sha256"]
                .as_str()
                .ok_or("lifecycle_ingest_params_missing")?
                .to_owned()
        }
        None => sadr_core::replay::digest(b""),
    };
    let mut spec = spec_fields;
    spec.source_sha256 = source_sha256;
    spec.params_sha256 = params_sha256;
    spec.dataset_hash = read["sha256"]
        .as_str()
        .ok_or("lifecycle_ingest_unreadable")?
        .to_owned();
    if spec.feature_abi.is_empty() {
        spec.feature_abi = if script == "scanner/cost_aware_momentum.py" {
            "cost_aware_momentum_v1".into()
        } else {
            return Err("lifecycle_ingest_abi_unknown".into());
        };
    }
    if script == "scanner/cost_aware_momentum.py" && spec.feature_abi != "cost_aware_momentum_v1" {
        return Err("lifecycle_ingest_abi_mismatch".into());
    }
    let admitted = admit_spec(directory, spec, now)?;
    let id = admitted["candidate_id"]
        .as_str()
        .ok_or("lifecycle_candidate_missing")?
        .to_owned();
    let admitted_state = admitted["state"].as_str().unwrap_or("");
    if admitted_state == LifecycleState::Proposed.as_str() {
        record_abi(directory, &id, now)
    } else {
        Ok(admitted)
    }
}

/// Presence-aware measurement read from the RAW stored projection. An absent
/// key (or an SQL NULL column) is `None`, the authoritative "not matured yet".
/// A PRESENT value is `Some`, including explicit JSON null, the string "null",
/// a number, array or bool, so the canonical reader can mark it unavailable
/// instead of silently treating a corrupt body as not matured.
fn canonical_measurement(raw_projection: &str) -> Option<Value> {
    serde_json::from_str::<Value>(raw_projection)
        .ok()
        .and_then(|value| value.get("observation_measurement").cloned())
}

fn library_item(
    id: &str,
    spec: &ExperimentSpec,
    state: &str,
    projection: &Projection,
    measurement: Option<Value>,
) -> Value {
    let measurement_present = measurement.is_some();
    json!({
        "candidate_id": id,
        "parent_candidate_id": spec.parent_candidate_id,
        "session": spec.session,
        "source_sha256": spec.source_sha256,
        "input_sha256": spec.params_sha256,
        "hypothesis": spec.hypothesis,
        "strategy_abi": spec.feature_abi,
        "feature_abi": spec.feature_abi,
        "origin_session": spec.session,
        "continued": false,
        "report_kind": if spec.feature_abi == "cash_quote_action_tape_v1" {
            "executable_replay"
        } else if spec.feature_abi == "observation_single_entry_v1" {
            "forward_observation_score"
        } else {
            "ranking_diagnostic"
        },
        "holding_horizon": spec.holding_ms.to_string(),
        "state": state,
        "result_sha256": projection.replay_report_sha256,
        "observation_score": projection.observation_score,
        "observation_measurement": measurement,
        "observation_measurement_present": measurement_present,
        "scientific_state": scientific_state(state, measurement_present),
        "activation_authorized": false,
        "source": "experiment_lifecycle"
    })
}

/// Direct bounded lookup of ONE lifecycle item by candidate id. This avoids the
/// 10k library page entirely, so a target outside the page is an authoritative
/// `None` instead of a false `not_matured`.
pub(crate) fn observation_item(directory: &Path, id: &str) -> Result<Option<Value>> {
    let file = super::file_path(directory);
    if !file.is_file() {
        return Ok(None);
    }
    let db = rusqlite::Connection::open_with_flags(
        &file,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    db.busy_timeout(std::time::Duration::ZERO)?;
    if !table_present(&db, "experiment_lifecycle_specs")?
        || !table_present(&db, "experiment_lifecycle_state")?
    {
        return Ok(None);
    }
    let row: Option<(String, String, String)> = db
        .query_row(
            "SELECT s.body,st.state,st.projection FROM experiment_lifecycle_specs s JOIN experiment_lifecycle_state st USING(candidate_id) WHERE s.candidate_id=?",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|_| "candidate_lifecycle_unavailable")?;
    let Some((body, state, projection)) = row else {
        return Ok(None);
    };
    let spec: ExperimentSpec = serde_json::from_str(&body)?;
    let measurement = canonical_measurement(&projection);
    let projection: Projection = serde_json::from_str(&projection)?;
    Ok(Some(library_item(
        id,
        &spec,
        &state,
        &projection,
        measurement,
    )))
}

const LIBRARY_PAGE: usize = 256;
const LIBRARY_MAX: usize = 10_000;

fn table_present(db: &Connection, name: &str) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|_| "candidate_lifecycle_unavailable".into())
}

/// Whether this ledger carries the lifecycle tables. A legacy ledger without
/// them is an EMPTY library only when the original queue independently proves
/// no candidate admissions; otherwise the projection is ambiguous.
pub(crate) fn lifecycle_tables_present(directory: &Path, immutable: bool) -> Result<bool> {
    let file = super::file_path(directory);
    if !file.is_file() {
        return Ok(false);
    }
    let db = if immutable {
        super::super::open_ledger_immutable(&file)?
    } else {
        rusqlite::Connection::open_with_flags(
            &file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    };
    db.busy_timeout(std::time::Duration::ZERO)?;
    Ok(table_present(&db, "experiment_lifecycle_specs")?
        && table_present(&db, "experiment_lifecycle_state")?)
}

fn empty_library() -> Value {
    json!({"status":"not_recorded","items":[],"truncated":false,"activation_authorized":false,"source":"experiment_lifecycle"})
}

/// Read the lifecycle library with bounded keyset paging. The previous reader
/// stopped at 256 rows and silently dropped later observations; this reader
/// walks the full bounded range and reports truncation truthfully. A ledger
/// without the lifecycle tables is a valid legacy ledger with no lifecycle
/// records; a table that exists but cannot be read is unavailable.
pub(crate) fn library(directory: &Path, session: &str) -> Result<Value> {
    library_bounded(directory, session, LIBRARY_MAX)
}

pub(crate) fn library_bounded(directory: &Path, session: &str, max: usize) -> Result<Value> {
    library_bounded_with(directory, session, max, false)
}

/// Immutable read of a CLOSED predecessor lifecycle ledger. A non-empty WAL is
/// an explicit refusal and no `-wal`/`-shm` sidecar is created.
pub(crate) fn library_bounded_immutable(
    directory: &Path,
    session: &str,
    max: usize,
) -> Result<Value> {
    library_bounded_with(directory, session, max, true)
}

fn library_bounded_with(
    directory: &Path,
    session: &str,
    max: usize,
    immutable: bool,
) -> Result<Value> {
    let file = super::file_path(directory);
    if !file.is_file() {
        return Ok(empty_library());
    }
    let db = if immutable {
        super::super::open_ledger_immutable(&file)?
    } else {
        rusqlite::Connection::open_with_flags(
            &file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    };
    db.busy_timeout(std::time::Duration::ZERO)?;
    if !table_present(&db, "experiment_lifecycle_specs")?
        || !table_present(&db, "experiment_lifecycle_state")?
    {
        return Ok(empty_library());
    }
    let mut items = Vec::new();
    let mut truncated = false;
    let mut after: Option<String> = None;
    // Filter the requested session BEFORE the bounded page limit. Filtering in
    // Rust after the limit made a page of other-session rows look like a
    // truncated target-session library.
    let session_pattern = (!session.is_empty()).then(|| format!("\"session\":\"{session}\""));
    loop {
        let mut query = db.prepare(
            "SELECT s.candidate_id,s.body,st.state,st.projection FROM experiment_lifecycle_specs s JOIN experiment_lifecycle_state st USING(candidate_id) WHERE (?1 IS NULL OR s.candidate_id>?1) AND (?3 IS NULL OR instr(s.body,?3)>0) ORDER BY s.candidate_id LIMIT ?2",
        ).map_err(|_| "candidate_lifecycle_unavailable")?;
        let rows = query
            .query_map(
                params![after, (LIBRARY_PAGE + 1) as i64, session_pattern],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|_| "candidate_lifecycle_unavailable")?;
        let mut page = 0usize;
        for row in rows {
            let (id, body, state, projection) =
                row.map_err(|_| "candidate_lifecycle_unavailable")?;
            page += 1;
            after = Some(id.clone());
            if items.len() >= max {
                truncated = true;
                break;
            }
            let spec: ExperimentSpec = serde_json::from_str(&body)?;
            if !session.is_empty() && spec.session != session {
                continue;
            }
            let measurement = canonical_measurement(&projection);
            let projection: Projection = serde_json::from_str(&projection)?;
            items.push(library_item(&id, &spec, &state, &projection, measurement));
        }
        if page < LIBRARY_PAGE + 1 || items.len() >= max {
            break;
        }
    }
    if !truncated && items.len() >= max {
        // Exact `max` is not truncation unless an actual extra row exists.
        let extra: Option<String> = db
            .query_row(
                "SELECT candidate_id FROM experiment_lifecycle_specs WHERE candidate_id>? AND (?2 IS NULL OR instr(body,?2)>0) ORDER BY candidate_id LIMIT 1",
                params![after.unwrap_or_default(), session_pattern],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "candidate_lifecycle_unavailable")?;
        truncated = extra.is_some();
    }
    Ok(json!({
        "status": if items.is_empty() { "not_recorded" } else { "ready" },
        "items": items,
        "truncated": truncated,
        "activation_authorized": false,
        "source": "experiment_lifecycle"
    }))
}

/// Authority read of a CLOSED predecessor ledger: a missing ledger is
/// unavailable, and a non-empty WAL is an explicit refusal.
pub(crate) fn library_checked_immutable(directory: &Path, session: &str) -> Result<Value> {
    if !super::file_path(directory).is_file() {
        return Err("candidate_lifecycle_unavailable".into());
    }
    library_bounded_immutable(directory, session, LIBRARY_MAX)
}

/// Read the current lifecycle library, treating a missing ledger as an empty
/// projection. An existing but corrupt ledger is still unavailable.
pub(crate) fn library_optional(directory: &Path, session: &str) -> Result<Value> {
    if !super::file_path(directory).is_file() {
        return Ok(
            json!({"status":"not_recorded","items":[],"activation_authorized":false,"source":"experiment_lifecycle"}),
        );
    }
    library(directory, session)
}

/// Canonical content hash for one cross-session continuation body. Read-time
/// and processing clocks are outside the immutable evidence, so a repeat tick
/// re-reads the same identity instead of minting a new receipt.
pub(crate) fn continuation_content_sha256(value: &Value) -> Result<String> {
    let mut stable = value.clone();
    let object = stable
        .as_object_mut()
        .ok_or("candidate_continuation_invalid")?;
    object.insert("processed_at_ms".into(), json!(0));
    object.insert("recorded_at_ms".into(), json!(0));
    object.insert("measurement_sha256".into(), json!(""));
    Ok(sadr_core::replay::digest(
        serde_json::to_string(&stable)?.as_bytes(),
    ))
}

pub(crate) fn validate_continuation_body(value: &Value) -> Result<()> {
    // Closed top-level schema. An extra key is a different body, never a
    // forward-compatible extension of this version.
    if !value.as_object().is_some_and(|map| {
        map.keys()
            .all(|key| CONTINUATION_KEYS.contains(&key.as_str()))
    }) {
        return Err("candidate_continuation_invalid".into());
    }
    // The nested source runtime is a closed five-hash projection.
    if !value["source_runtime"].as_object().is_some_and(|map| {
        map.len() == SOURCE_RUNTIME_KEYS.len()
            && map
                .keys()
                .all(|key| SOURCE_RUNTIME_KEYS.contains(&key.as_str()))
    }) {
        return Err("candidate_continuation_invalid".into());
    }
    let id = value["continuation_id"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    let measurement = value["measurement_sha256"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    let scope = value["current_scope"]
        .as_object()
        .ok_or("candidate_continuation_invalid")?;
    let scope_hash_ok = |key: &str| value["current_scope"][key].as_str().is_some_and(valid_hash);
    // The source-result status is an EXPLICIT closed enum. A verified status
    // requires the exact hash; an unavailable status requires the KEY to be
    // PRESENT with an explicit NULL value. A missing key is a different schema,
    // never an implicit explicit-null, and a missing/unknown status is invalid.
    let source_result_key_present = value
        .as_object()
        .is_some_and(|map| map.contains_key("source_result_sha256"));
    let source_result_ok = match (
        value["source_result_status"].as_str(),
        value["source_result_sha256"].as_str(),
    ) {
        (Some("verified"), Some(hash)) => valid_hash(hash),
        (Some("unavailable"), None) => {
            source_result_key_present && value["source_result_sha256"].is_null()
        }
        _ => false,
    };
    if !valid_hash(id)
        || !valid_hash(measurement)
        || !matches!(value["state"].as_str(), Some("evaluated" | "unknown"))
        || value["source_scope"] != "continuation_original_episode"
        || value["capital_assumption"] != true
        || value["edge_qualified"] == true
        || value["activation_authorized"] == true
        || value["source_session"]
            .as_str()
            .is_none_or(|session| session.is_empty() || session.len() > 32)
        || value["source_lifecycle_candidate_id"]
            .as_str()
            .is_none_or(|id| !valid_hash(id))
        || value["source_task_id"]
            .as_str()
            .is_none_or(|task| task.is_empty() || task.len() > 256)
        || value["source_revision"]
            .as_u64()
            .is_none_or(|revision| revision == 0)
        || value["exit_due_ms"].as_i64().is_none()
        || value["exit_deadline_ms"].as_i64().is_none()
        || value["current_scope"]["current_session"]
            .as_str()
            .is_none_or(|session| session.is_empty() || session.len() > 32)
        || value["current_scope"]["version"].as_u64() != Some(1)
        || value["current_scope"]["account"]
            .as_str()
            .is_none_or(|account| account.is_empty() || account.len() > 128)
        || !scope_hash_ok("cohort_key")
        || !scope_hash_ok("runtime_hash")
        || !scope_hash_ok("source_ledger_spec_sha256")
        || !source_result_ok
        || scope.len() != 6
    {
        return Err("candidate_continuation_invalid".into());
    }
    if continuation_content_sha256(value)? != measurement {
        return Err("candidate_continuation_hash_mismatch".into());
    }
    // The self-hash is not identity: recompute the canonical identity digest
    // from the original source facts and the CURRENT recording scope.
    if crate::runtime::harness::research::continuation_identity_sha256(value)? != id {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    Ok(())
}

fn ledger_session(directory: &Path) -> Option<&str> {
    directory
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.parse::<chrono::NaiveDate>().is_ok())
}

/// A continuation body that the canonical source validator has already checked.
/// The production writer accepts only this sealed token, never raw JSON.
pub(crate) trait ContinuationBody: sealed::Sealed {
    fn body(&self) -> &Value;
}

impl sealed::Sealed for crate::runtime::harness::research::ValidatedContinuation {}

/// Append one CURRENT continuation receipt to the existing per-session
/// lifecycle ledger. The original predecessor ledger is never written. The same
/// identity is idempotent; a different body for that identity is refused.
pub(crate) fn record_continuation<T: ContinuationBody>(
    directory: &Path,
    token: &T,
    now: i64,
) -> Result<Value> {
    let continuation = token.body();
    if now < 0 {
        return Err("lifecycle_clock_invalid".into());
    }
    validate_continuation_body(continuation)?;
    // The recording scope must name the ledger's OWN session, not a later
    // successor's session. A body copied into the wrong ledger is refused.
    if let Some(session) = ledger_session(directory)
        && continuation["current_scope"]["current_session"].as_str() != Some(session)
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let id = continuation["continuation_id"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    let measurement = continuation["measurement_sha256"]
        .as_str()
        .ok_or("candidate_continuation_invalid")?;
    // The ledger row is the authoritative recording clock. Normalize the
    // body's read-time fields to that clock so a later reader can bind body
    // metadata to the actual row timestamp; the content hash deliberately
    // excludes both fields, so identity is unchanged.
    let mut recorded = continuation.clone();
    recorded["recorded_at_ms"] = json!(now);
    recorded["processed_at_ms"] = json!(now);
    let body = serde_json::to_string(&recorded)?;
    let mut db = super::open_directory(directory)?;
    // Serialize the read/insert decision in one IMMEDIATE transaction. A
    // concurrent stale inserter either sees the first committed row or gets an
    // explicit busy/deferred result; it never overwrites a different receipt.
    let tx = match db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(error) if is_busy(&error) => return Err("candidate_continuation_busy".into()),
        Err(_) => return Err("candidate_continuation_unavailable".into()),
    };
    let inserted = match tx.execute(
        "INSERT OR IGNORE INTO experiment_continuations(continuation_id,body,recorded_at_ms) VALUES(?,?,?)",
        params![id, body, now],
    ) {
        Ok(inserted) => inserted,
        Err(error) if is_busy(&error) => return Err("candidate_continuation_busy".into()),
        Err(_) => return Err("candidate_continuation_unavailable".into()),
    };
    if inserted == 0 {
        let existing: String = tx
            .query_row(
                "SELECT body FROM experiment_continuations WHERE continuation_id=?",
                [id],
                |row| row.get(0),
            )
            .map_err(|_| "candidate_continuation_unavailable")?;
        let stored: Value = serde_json::from_str(&existing)?;
        if continuation_content_sha256(&stored)? != continuation_content_sha256(continuation)? {
            return Err("continuation_identity_changed".into());
        }
        tx.commit()?;
        return Ok(stored);
    }
    tx.execute(
        "INSERT INTO experiment_lifecycle_events(candidate_id,from_state,to_state,event,evidence_sha256,at_ms) VALUES(?,?,?,?,?,?)",
        params![id, Option::<String>::None, "continued", "observation_continuation", measurement, now],
    )?;
    tx.commit()?;
    Ok(recorded)
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// Read every CURRENT continuation receipt in this session's existing lifecycle
/// ledger. A missing ledger or an older ledger without the continuation table is
/// empty; a corrupt ledger or an invalid body is unavailable. A sqlite_master
/// read failure is never silently treated as "no table".
pub(crate) fn continuations(directory: &Path) -> Result<Vec<Value>> {
    Ok(continuations_with_recorded(directory, false)?
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

/// Immutable read of a CLOSED predecessor continuation table. A non-empty WAL
/// is an explicit refusal and no sidecar is created.
pub(crate) fn continuations_immutable(directory: &Path) -> Result<Vec<Value>> {
    Ok(continuations_with_recorded(directory, true)?
        .into_iter()
        .map(|(_, value)| value)
        .collect())
}

/// Read continuations together with the AUTHORITATIVE ledger row timestamp.
/// Selection must use the row clock, not a body field that a forged body can
/// rewrite.
pub(crate) fn continuations_with_recorded(
    directory: &Path,
    immutable: bool,
) -> Result<Vec<(i64, Value)>> {
    let file = super::file_path(directory);
    if !file.is_file() {
        return Ok(Vec::new());
    }
    let db = if immutable {
        super::super::open_ledger_immutable(&file)?
    } else {
        rusqlite::Connection::open_with_flags(
            &file,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?
    };
    db.busy_timeout(std::time::Duration::ZERO)?;
    if !table_present(&db, "experiment_continuations")? {
        return Ok(Vec::new());
    }
    let mut query = db
        .prepare(
            "SELECT continuation_id,body,recorded_at_ms FROM experiment_continuations ORDER BY continuation_id",
        )
        .map_err(|_| "candidate_continuation_unavailable")?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| "candidate_continuation_unavailable")?;
    let mut values = Vec::new();
    for row in rows {
        let (row_id, body, recorded_at_ms) =
            row.map_err(|_| "candidate_continuation_unavailable")?;
        let value: Value =
            serde_json::from_str(&body).map_err(|_| "candidate_continuation_invalid")?;
        validate_continuation_body(&value)?;
        if value["continuation_id"].as_str() != Some(row_id.as_str())
            || recorded_at_ms < 0
            || value["recorded_at_ms"].as_i64() != Some(recorded_at_ms)
            || value["processed_at_ms"]
                .as_i64()
                .is_none_or(|processed| processed > recorded_at_ms)
        {
            return Err("candidate_continuation_identity_mismatch".into());
        }
        if let Some(session) = ledger_session(directory)
            && value["current_scope"]["current_session"].as_str() != Some(session)
        {
            return Err("candidate_continuation_identity_mismatch".into());
        }
        values.push((recorded_at_ms, value));
    }
    Ok(values)
}

/// Latest CURRENT continuation for one ORIGINAL episode in this session.
#[allow(dead_code)] // Compatibility reader; the verified lineage resolver is canonical.
pub(crate) fn continuation_for(
    directory: &Path,
    session: &str,
    source_lifecycle_candidate_id: &str,
) -> Result<Option<Value>> {
    Ok(continuations(directory)?.into_iter().find(|value| {
        value["source_lifecycle_candidate_id"] == source_lifecycle_candidate_id
            && value["current_scope"]["current_session"] == session
    }))
}

/// Read-only fold of the current lifecycle library over a predecessor
/// lifecycle directory. It merges by candidate id, marks the origin session and
/// continuation, and never copies rows, creates a trial or writes either ledger.
#[allow(dead_code)] // Read-only helper used by tests and external projections.
pub(crate) fn fold_library(
    directory: &Path,
    predecessor: Option<&Path>,
    session: &str,
) -> Result<Value> {
    // The current session may legitimately have no ledger yet; a missing
    // PREDECESSOR ledger is unavailable, never an empty fold.
    let current = library(directory, session)?;
    let Some(predecessor) = predecessor else {
        return Ok(current);
    };
    let previous = library_checked_immutable(predecessor, "")?;
    let mut items = current["items"].as_array().cloned().unwrap_or_default();
    let mut seen = items
        .iter()
        .filter_map(|item| item["candidate_id"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    let current_continuations = continuations(directory)?;
    for mut item in previous["items"].as_array().cloned().unwrap_or_default() {
        let Some(id) = item["candidate_id"].as_str().map(str::to_owned) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        item["continued"] = json!(true);
        // The CURRENT successor continuation is the latest verified state of the
        // ORIGINAL episode. It is attached by original identity, never copied as
        // a new candidate or counted as a second trial.
        if let Some(continuation) = current_continuations.iter().find(|continuation| {
            continuation["source_lifecycle_candidate_id"] == id.as_str()
                && continuation["current_scope"]["current_session"] == session
        }) {
            item["continuation"] = continuation.clone();
            item["continuation_recorded_in_session"] =
                continuation["current_scope"]["current_session"].clone();
            // The verified continuation is the EFFECTIVE state; the ORIGINAL
            // recorded body stays separately available as history truth.
            item["effective_observation_measurement"] = continuation.clone();
            if item["observation_measurement"].is_null() {
                item["observation_measurement"] = continuation.clone();
            }
        }
        item["projection_sha256"] = json!(sadr_core::replay::digest(
            serde_json::to_string(&item).unwrap_or_default().as_bytes()
        ));
        items.push(item);
    }
    Ok(json!({
        "status": if items.is_empty() { "not_recorded" } else { "ready" },
        "items": items,
        "truncated": current["truncated"].as_bool().unwrap_or(false) || previous["truncated"].as_bool().unwrap_or(false),
        "folded": true,
        "copied_rows": false,
        "continuations": current_continuations.len(),
        "activation_authorized": false,
        "source": "experiment_lifecycle_read_only_fold"
    }))
}

/// Read-only fold across the FULL verified predecessor chain. It merges every
/// hop's lifecycle library by candidate id exactly once and attaches the newest
/// verified continuation of each ORIGINAL episode from whichever hop recorded
/// it. No row is copied, no ledger is written and no predecessor is migrated.
#[allow(dead_code)] // Read-only helper used by tests and external projections.
pub(crate) fn fold_library_chain(
    c: &super::super::Config,
    cfg: &crate::runtime::planner::PlannerConfig,
    session: &str,
) -> Result<Value> {
    let chain = crate::runtime::harness::research::predecessor_chain_verified_immutable(c)?;
    crate::runtime::harness::research::verify_lineage_prefix(c, cfg, &chain)?;
    let continuations =
        crate::runtime::harness::research::verified_continuations_from_chain(c, cfg, &chain)?;
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut truncated = false;
    for (index, (hop_session, directory, _)) in chain.iter().enumerate() {
        let library = if index == 0 {
            library_bounded(directory, session, LIBRARY_MAX)?
        } else {
            library_bounded_immutable(directory, "", LIBRARY_MAX)?
        };
        truncated |= library["truncated"].as_bool().unwrap_or(false);
        for mut item in library["items"].as_array().cloned().unwrap_or_default() {
            let Some(id) = item["candidate_id"].as_str().map(str::to_owned) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            if index > 0 {
                item["continued"] = json!(true);
                item["origin_session"] = json!(hop_session);
            }
            if let Some(continuation) = continuations
                .iter()
                .find(|continuation| continuation["source_lifecycle_candidate_id"] == id.as_str())
            {
                item["continuation"] = continuation.clone();
                item["continuation_recorded_in_session"] =
                    continuation["current_scope"]["current_session"].clone();
                // The verified continuation is the EFFECTIVE state; the
                // ORIGINAL recorded body stays separately available.
                item["effective_observation_measurement"] = continuation.clone();
                if item["observation_measurement"].is_null() {
                    item["observation_measurement"] = continuation.clone();
                }
            }
            item["projection_sha256"] = json!(sadr_core::replay::digest(
                serde_json::to_string(&item).unwrap_or_default().as_bytes()
            ));
            items.push(item);
        }
    }
    Ok(json!({
        "status": if items.is_empty() { "not_recorded" } else { "ready" },
        "items": items,
        "truncated": truncated,
        "folded": true,
        "copied_rows": false,
        "continuations": continuations.len(),
        "activation_authorized": false,
        "source": "experiment_lifecycle_verified_chain_read_only_fold"
    }))
}

pub(crate) fn merge_into_library(research: Option<Value>, lifecycle: &Value) -> Option<Value> {
    let extra = match lifecycle["items"].as_array() {
        Some(items) if !items.is_empty() => items.clone(),
        _ => return research,
    };
    let mut body = match research {
        None | Some(Value::Null) => {
            json!({"status":"ready","items":[],"activation_authorized":false,"source":"experiment_lifecycle"})
        }
        Some(value) => value,
    };
    if let Some(items) = body["items"].as_array_mut() {
        items.extend(extra);
    } else {
        body["items"] = json!(extra);
    }
    body["activation_authorized"] = json!(false);
    if body["status"] == "disabled" || body["status"] == "not_recorded" {
        body["status"] = json!("ready");
    }
    Some(body)
}

fn ingest_from_request(directory: &Path, request: &Value, now: i64) -> Result<Value> {
    let report = request["report"]
        .as_str()
        .ok_or("lifecycle_ingest_report_missing")?;
    let settings = match request.get("workspace") {
        Some(value) if !value.is_null() => serde_json::from_value(value.clone())?,
        _ => serde_json::from_value(json!({"enabled": true}))?,
    };
    let spec = ExperimentSpec {
        version: 1,
        parent_candidate_id: request["parent_candidate_id"]
            .as_str()
            .map(ToOwned::to_owned),
        session: request["session"]
            .as_str()
            .ok_or("lifecycle_spec_invalid")?
            .to_owned(),
        source_sha256: "0".repeat(64),
        params_sha256: "0".repeat(64),
        dataset_hash: "0".repeat(64),
        feature_abi: request["feature_abi"].as_str().unwrap_or("").to_owned(),
        hypothesis: request["hypothesis"]
            .as_str()
            .ok_or("lifecycle_spec_invalid")?
            .to_owned(),
        training_from_ms: request["training_from_ms"]
            .as_i64()
            .ok_or("lifecycle_spec_invalid")?,
        training_to_ms: request["training_to_ms"]
            .as_i64()
            .ok_or("lifecycle_spec_invalid")?,
        selection_from_ms: request["selection_from_ms"]
            .as_i64()
            .ok_or("lifecycle_spec_invalid")?,
        selection_to_ms: request["selection_to_ms"]
            .as_i64()
            .ok_or("lifecycle_spec_invalid")?,
        holding_ms: request["holding_ms"]
            .as_i64()
            .ok_or("lifecycle_spec_invalid")?,
        max_exit_delay_ms: request["max_exit_delay_ms"]
            .as_i64()
            .ok_or("lifecycle_spec_invalid")?,
    };
    ingest_workspace(directory, &settings, report, spec, now)
}

pub(crate) fn dispatch(directory: &Path, request: &Value, now: i64) -> Result<Value> {
    let now = request["at_ms"].as_i64().unwrap_or(now);
    match request["action"].as_str() {
        Some("admit") => {
            let spec: ExperimentSpec = serde_json::from_value(request["spec"].clone())?;
            admit_spec(directory, spec, now)
        }
        Some("ingest_workspace") => ingest_from_request(directory, request, now),
        Some("record_abi") => record_abi(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            now,
        ),
        Some("record_replay") => record_replay(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            &request["report"],
            now,
        ),
        Some("open_shadow") => open_shadow(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            &request["marks"],
            now,
        ),
        Some("mature") => mature(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            request,
            now,
        ),
        Some("evaluate") => conclude(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            now,
        ),
        Some("select_paper") => select_paper(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            now,
        ),
        Some("retire") => retire(
            directory,
            request["candidate_id"]
                .as_str()
                .ok_or("lifecycle_candidate_missing")?,
            request["reason"].as_str().unwrap_or(""),
            now,
        ),
        Some("show") => {
            if let Some(id) = request["candidate_id"].as_str() {
                let db = super::open_directory(directory)?;
                let (spec, state, projection) = load(&db, id)?;
                Ok(receipt(id, &spec, &state, &projection))
            } else {
                library(directory, request["session"].as_str().unwrap_or(""))
            }
        }
        _ => Err("lifecycle_action_invalid".into()),
    }
}

pub(crate) fn entry(book: &Path, request: &Path, output: &Path) -> Result<Value> {
    let file = std::fs::File::open(request).map_err(|_| "lifecycle_request_unreadable")?;
    if file.metadata()?.len() > REQUEST_LIMIT {
        return Err("lifecycle_request_too_large".into());
    }
    let mut bytes = Vec::new();
    file.take(REQUEST_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > REQUEST_LIMIT {
        return Err("lifecycle_request_too_large".into());
    }
    let request: Value = serde_json::from_slice(&bytes).map_err(|_| "lifecycle_request_invalid")?;
    let now = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "lifecycle_clock_invalid")?
            .as_millis(),
    )
    .map_err(|_| "lifecycle_clock_invalid")?;
    let result = dispatch(book, &request, now)?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .map_err(|_| "lifecycle_output_exists_or_unwritable")?;
    let mut file = file;
    serde_json::to_writer_pretty(&file, &result)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::super::super::workspace::{WorkspaceOp, WorkspaceSettings, execute};
    use super::*;

    fn hash(label: &str) -> String {
        sadr_core::replay::digest(label.as_bytes())
    }

    fn spec(hypothesis: &str) -> ExperimentSpec {
        ExperimentSpec {
            version: 1,
            parent_candidate_id: None,
            session: "2026-10-16".into(),
            source_sha256: hash("source"),
            params_sha256: hash("params"),
            dataset_hash: hash("dataset"),
            feature_abi: "cash_quote_action_tape_v1".into(),
            hypothesis: hypothesis.into(),
            training_from_ms: 1_000,
            training_to_ms: 1_999,
            selection_from_ms: 2_000,
            selection_to_ms: 3_000,
            holding_ms: 1_000,
            max_exit_delay_ms: 2_000,
        }
    }

    fn report_for(spec: &ExperimentSpec) -> Value {
        json!({
            "schema": 1,
            "source_sha256": spec.source_sha256,
            "params_sha256": spec.params_sha256,
            "feature_abi": spec.feature_abi,
            "promotion_authorized": false,
            "scientific_state": "insufficient_evidence"
        })
    }

    fn advance(dir: &Path, spec: ExperimentSpec, now: i64) -> Value {
        let admitted = admit_spec(dir, spec.clone(), now).unwrap();
        let id = admitted["candidate_id"].as_str().unwrap().to_owned();
        record_abi(dir, &id, now + 1).unwrap();
        record_replay(dir, &id, &report_for(&spec), now + 2).unwrap();
        open_shadow(
            dir,
            &id,
            &json!([{"key":"SYNTHETIC|A","entry_ms":2000,"due_ms":3000}]),
            now + 3,
        )
        .unwrap();
        mature(
            dir,
            &id,
            &json!({"quotes":[{"key":"SYNTHETIC|A","source_ms":3000,"receipt_ms":3000,"interpolated":false}]}),
            now + 4,
        )
        .unwrap()
    }

    #[test]
    fn lifecycle_admits_ingests_and_enforces_scientific_gates() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path();
        let leaked = ExperimentSpec {
            training_to_ms: 2_000,
            ..spec("overlap")
        };
        assert_eq!(
            admit_spec(book, leaked, 10).unwrap_err().to_string(),
            "lifecycle_spec_invalid"
        );
        let first = advance(book, spec("first cost-aware tape"), 20);
        assert_eq!(first["state"], "eligible");
        assert_eq!(first["scientific_state"], "eligible");
        assert_eq!(first["activation_authorized"], false);
        assert_eq!(first["edge_qualified"], false);
        assert_eq!(first["checks"]["multiple_testing"], "pass");
        let first_id = first["candidate_id"].as_str().unwrap().to_owned();
        // Terminal maturity is eligible research evidence with the original
        // candidate id and result hash; no queue or provider call is created.
        let (c, mut research_cfg) = super::super::super::decision_owner::owner_fixture_config(book);
        research_cfg.owner_tasks = Some(super::super::super::owner_tasks::OwnerTasksSettings {
            version: 1,
            agenda: None,
            review: super::super::super::owner_tasks::ReviewSettings::default(),
            research: Some(super::super::super::owner_tasks::OwnerResearchSettings {
                idle_trigger: None,
                enabled: true,
                cutoff_minute: 1020,
                max_evidence: 4,
                quote_capture: None,
                observation_capture: None,
                lane: Some(Default::default()),
            }),
        });
        research_cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
        let due =
            super::super::super::supervisor::research_due_evidence(&c, &research_cfg).unwrap();
        assert!(due.iter().any(|evidence| {
            evidence.kind == super::super::super::owner_tasks::ResearchEvidenceKind::Maturity
                && evidence.task_id == first_id
                && evidence.maturity_state.as_deref() == Some("eligible")
        }));
        assert_eq!(
            record_replay(
                book,
                &first_id,
                &report_for(&spec("first cost-aware tape")),
                30
            )
            .unwrap_err()
            .to_string(),
            "lifecycle_transition_invalid"
        );
        let selected = select_paper(book, &first_id, 40).unwrap();
        assert_eq!(selected["state"], "paper_selected");
        assert_eq!(selected["activation_authorized"], false);
        let second = advance(book, spec("second look at the same bytes"), 50);
        assert_eq!(second["state"], "insufficient");
        assert_eq!(second["checks"]["multiple_testing"], "unadjusted");
        assert_eq!(second["scientific_state"], "insufficient_evidence");
        assert_eq!(second["activation_authorized"], false);

        let settings: WorkspaceSettings = serde_json::from_value(json!({"enabled": true})).unwrap();
        super::super::super::workspace::ensure(book, &settings).unwrap();
        execute(
            book,
            &settings,
            WorkspaceOp::Write {
                path: "reports/scores.json".into(),
                text: json!({
                    "script":"scanner/cost_aware_momentum.py",
                    "status":"ok",
                    "scores":{"SYNTHETIC|A":1.25},
                    "score_count":1,
                    "params_source":"scanner/cost_aware_momentum.params.json",
                    "execution_authorized":false,
                    "evidence_only":true,
                    "authority":"advisory_scores_not_orders"
                })
                .to_string(),
            },
        )
        .unwrap();
        let ingested = dispatch(
            book,
            &json!({
                "action":"ingest_workspace",
                "report":"reports/scores.json",
                "session":"2026-10-16",
                "hypothesis":"seeded scanner scores stay advisory",
                "training_from_ms":1000,
                "training_to_ms":1999,
                "selection_from_ms":2000,
                "selection_to_ms":3000,
                "holding_ms":1000,
                "max_exit_delay_ms":2000,
                "at_ms":60
            }),
            60,
        )
        .unwrap();
        assert_eq!(ingested["state"], "sandbox_abi");
        assert_eq!(ingested["feature_abi"], "cost_aware_momentum_v1");
        assert_eq!(ingested["activation_authorized"], false);
        assert_eq!(ingested["scientific_state"], "insufficient_evidence");
        execute(
            book,
            &settings,
            WorkspaceOp::Write {
                path: "reports/orders.json".into(),
                text: json!({
                    "script":"scanner/cost_aware_momentum.py",
                    "status":"ok",
                    "scores":{"SYNTHETIC|A":1.25},
                    "authority":"advisory_scores_not_orders",
                    "execution_authorized":true
                })
                .to_string(),
            },
        )
        .unwrap();
        assert_eq!(
            dispatch(
                book,
                &json!({
                    "action":"ingest_workspace",
                    "report":"reports/orders.json",
                    "session":"2026-10-16",
                    "hypothesis":"must refuse execution authority",
                    "training_from_ms":1000,
                    "training_to_ms":1999,
                    "selection_from_ms":2000,
                    "selection_to_ms":3000,
                    "holding_ms":1000,
                    "max_exit_delay_ms":2000,
                    "at_ms":70
                }),
                70,
            )
            .unwrap_err()
            .to_string(),
            "lifecycle_ingest_execution_refused"
        );
        let listed = library(book, "2026-10-16").unwrap();
        assert_eq!(listed["activation_authorized"], false);
        assert!(
            listed["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["candidate_id"] == ingested["candidate_id"])
        );
        let merged = merge_into_library(
            Some(json!({"status":"disabled","items":[],"activation_authorized":false})),
            &listed,
        )
        .unwrap();
        assert_eq!(merged["status"], "ready");
        assert_eq!(merged["activation_authorized"], false);
        assert_eq!(
            retire(book, &first_id, "operator_withdrawn", 80).unwrap()["state"],
            "retired"
        );
        // #232: only an observation-purpose spec may take Replayed -> Evaluated,
        // and a parked measured observation is not reported as failed checks.
        let replayed = |label: &str, abi: &str, now: i64| {
            let spec = ExperimentSpec {
                feature_abi: abi.into(),
                ..spec(label)
            };
            let id = admit_spec(book, spec.clone(), now).unwrap()["candidate_id"]
                .as_str()
                .unwrap()
                .to_owned();
            record_abi(book, &id, now + 1).unwrap();
            record_replay(book, &id, &report_for(&spec), now + 2).unwrap();
            id
        };
        let tape = replayed("replay only", "cash_quote_action_tape_v1", 90);
        let refused = apply_transition(
            book,
            &tape,
            "evaluated",
            "observation_matured",
            &json!({"measurement_sha256": hash("m")}),
            95,
            |_, _, _| Ok(()),
        );
        assert_eq!(
            refused.unwrap_err().to_string(),
            "lifecycle_transition_invalid"
        );
        let observed = replayed(
            "prospective observation",
            sadr_cli::candidate_execution::OBSERVATION_ABI,
            100,
        );
        let measured = apply_transition(
            book,
            &observed,
            "evaluated",
            "observation_matured",
            &json!({"measurement_sha256": hash("m")}),
            105,
            |_, _, projection| {
                projection.observation_measurement = Some(json!({"status":"verified"}));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(measured["state"], "evaluated");
        assert_eq!(measured["scientific_state"], "observation_measured");
        assert_eq!(measured["checks"], Value::Null);
        assert_eq!(measured["activation_authorized"], false);
        assert_eq!(
            conclude_if_ready(book, &observed, 106).unwrap().unwrap()["scientific_state"],
            "observation_measured"
        );
        assert_eq!(
            scientific_state("evaluated", false),
            "insufficient_evidence"
        );
    }
}
