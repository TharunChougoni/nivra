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
mod library;
pub(crate) use library::*;
mod continuations;
pub(crate) use continuations::*;

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
    spec.feature_abi == nivra_cli::candidate_execution::OBSERVATION_ABI
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
    let evidence_sha = nivra_core::replay::digest(serde_json::to_string(evidence)?.as_bytes());
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
    let evidence_sha = nivra_core::replay::digest(body.as_bytes());
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
    let score_sha = nivra_core::replay::digest(serde_json::to_string(score)?.as_bytes());
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
    let measurement_sha = nivra_core::replay::digest(serde_json::to_string(&stable)?.as_bytes());
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
    let report_sha = nivra_core::replay::digest(serde_json::to_string(report)?.as_bytes());
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
    let evidence_sha = nivra_core::replay::digest(serde_json::to_string(quotes)?.as_bytes());
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
        None => nivra_core::replay::digest(b""),
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
mod tests;
