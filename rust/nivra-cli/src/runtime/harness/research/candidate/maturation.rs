use super::*;

/// One matured observation episode. It is an independent episode with a
/// declared research capital assumption; it is never summed with another
/// episode or presented as a portfolio return. `measured_at_ms` is read-time
/// metadata and is excluded from `measurement_sha256`; the hash binds only the
/// immutable source refs and the durable recorded measurement.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservationMaturationV1 {
    pub version: u32,
    /// The recorded shared-engine financial ABI. A reader must not trust a
    /// financial body that does not name the exact ABI it was produced by.
    pub financial_abi: String,
    pub candidate_id: String,
    pub task_id: String,
    pub revision: u32,
    pub key: String,
    pub observation_sha256: String,
    pub exit_due_ms: i64,
    pub exit_deadline_ms: i64,
    pub exit_quote: Option<nivra_cli::candidate_execution::FullQuoteRefV1>,
    pub exit_status: String,
    pub exit_gap: Option<Value>,
    pub first_receipt_unknown: bool,
    pub execution: Option<nivra_cli::candidate_execution::ExecutableSummary>,
    pub net_pnl_if_flat: Option<Amount>,
    pub fees: Option<Amount>,
    pub turnover: Option<Amount>,
    pub flat: bool,
    pub accounting_complete: bool,
    pub gaps: Vec<String>,
    pub report_sha256: Option<String>,
    pub source_scope: String,
    pub capital_assumption: bool,
    /// The ORIGINAL declared research capital from the frozen observation spec.
    /// It is an assumption, never account funds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capital: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_projection: Option<String>,
    pub measured_at_ms: i64,
    pub measurement_sha256: String,
}

#[allow(clippy::too_many_arguments)]
fn observation_measurement(
    task: &jobs::CandidateTaskRecord,
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
    exit_due_ms: i64,
    exit_deadline_ms: i64,
    exit_quote: Option<nivra_cli::candidate_execution::FullQuoteRefV1>,
    exit_status: &str,
    exit_gap: Option<Value>,
    first_receipt_unknown: bool,
    execution: Option<nivra_cli::candidate_execution::ExecutableSummary>,
    report_sha256: Option<String>,
    gaps: Vec<String>,
    capital: Option<Amount>,
    observation_projection: Option<String>,
    now: i64,
) -> ObservationMaturationV1 {
    let executed = exit_status == "executed";
    let (net_pnl_if_flat, fees, turnover) = match (&execution, executed) {
        (Some(summary), true) if summary.flat && summary.accounting_complete => (
            summary.net_pnl_if_flat,
            Some(summary.fees),
            Some(summary.turnover),
        ),
        (Some(summary), _) => (None, Some(summary.fees), Some(summary.turnover)),
        _ => (None, None, None),
    };
    let flat = execution.as_ref().is_some_and(|summary| summary.flat);
    let accounting_complete = execution
        .as_ref()
        .is_some_and(|summary| summary.accounting_complete);
    let mut measurement = ObservationMaturationV1 {
        version: 1,
        financial_abi: OBSERVATION_FINANCIAL_ABI.into(),
        candidate_id: task.spec.candidate_id.clone(),
        task_id: task.spec.task_id.clone(),
        revision: task.spec.revision,
        key: score
            .entry
            .as_ref()
            .map(|entry| entry.key.clone())
            .or_else(|| score.key.clone())
            .unwrap_or_default(),
        observation_sha256: score.observation_sha256.clone(),
        exit_due_ms,
        exit_deadline_ms,
        exit_quote,
        exit_status: exit_status.into(),
        exit_gap,
        first_receipt_unknown,
        execution,
        net_pnl_if_flat,
        fees,
        turnover,
        flat,
        accounting_complete,
        gaps,
        report_sha256,
        source_scope: "episode_key_only".into(),
        capital_assumption: true,
        capital,
        observation_projection,
        measured_at_ms: now,
        measurement_sha256: String::new(),
    };
    // Read-time metadata is outside the content hash. Re-reading the same
    // durable measurement at a later clock must not mint a new identity.
    let mut content = measurement.clone();
    content.measured_at_ms = 0;
    content.measurement_sha256.clear();
    measurement.measurement_sha256 =
        nivra_cli::candidate_execution::observation_artifact_sha256(&content).unwrap_or_default();
    measurement
}

/// Outcome of one maturation tick. A prospective observation is not terminal
/// before its due time, and a missing quote inside the grace window is a
/// pending capture, not a fabricated zero or a permanent unknown.
#[derive(Debug)]
pub(crate) enum MaturationDecision {
    Waiting {
        due_ms: i64,
    },
    PendingCapture {
        due_ms: i64,
        deadline_ms: i64,
        gap: Value,
    },
    Matured(Value),
    Unknown(Value),
}

/// A current maturation body that the canonical reader has re-derived from the
/// verified source. The field is private: only this module can mint the token,
/// so the production lifecycle writer cannot be called with unchecked JSON.
pub(crate) struct ValidatedObservationMeasurement(Value);

impl experiment_ledger::lifecycle::ObservationMeasurementBody for ValidatedObservationMeasurement {
    fn body(&self) -> &Value {
        &self.0
    }
}

fn record_observation_terminal(
    c: &Config,
    task: &jobs::CandidateTaskRecord,
    spec: &nivra_cli::candidate_execution::ObservationExecutionSpecV1,
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
    measurement: &ObservationMaturationV1,
    now: i64,
) -> Result<Value> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.spec.purpose else {
        return Err("candidate_observation_target_missing".into());
    };
    let value = serde_json::to_value(measurement)?;
    // The WRITER re-derives the body BEFORE the first INSERT. A forged body
    // with a correct self-hash and a valid source exit quote cannot poison an
    // empty lifecycle table.
    let context = StoredObservationContext {
        source: c.quote_source(),
        session: c.session,
        previous_session: candidate_previous_trading_session(c),
        task,
        score,
        spec,
        target,
        kind: StoredObservationKind::CurrentMaturation,
        immutable: false,
        recorded_at_ms: None,
        source_runtime_hash: None,
        source_cohort_key: None,
        current_runtime_hash: None,
        current_cohort_key: None,
        now,
    };
    verify_stored_observation(&context, &value)?;
    let token = ValidatedObservationMeasurement(value.clone());
    experiment_ledger::lifecycle::record_observation_measurement(
        &c.directory,
        &task.spec.lifecycle_candidate_id,
        &token,
        now,
    )?;
    Ok(value)
}

/// One verified virtual-observation exit evaluation. The original entry ref and
/// the FIRST eligible original exit receipt are evaluated once by the existing
/// shared cash/fee/fill/freshness engine. The returned provenance is the
/// recorded proof; no later clock, price or holding is substituted.
pub(crate) struct ObservationExitEvaluation {
    pub exit_quote: nivra_cli::candidate_execution::FullQuoteRefV1,
    pub exit_status: String,
    pub exit_gap: Option<Value>,
    pub first_receipt_unknown: bool,
    pub execution: Option<nivra_cli::candidate_execution::ExecutableSummary>,
    pub report_sha256: Option<String>,
    pub gaps: Vec<String>,
    pub exit_at_ms: i64,
}

/// The shared engine could not produce a verifiable report for an otherwise
/// eligible original exit. The original receipt is retained as provenance; the
/// caller records an explicit unknown, never a fabricated zero.
pub(crate) struct ObservationExitUnavailable {
    pub exit_quote: nivra_cli::candidate_execution::FullQuoteRefV1,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum ObservationExitOutcome {
    Evaluated(ObservationExitEvaluation),
    MeasurementUnavailable(ObservationExitUnavailable),
}

pub(crate) fn evaluate_observation_exit(
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
    spec: &nivra_cli::candidate_execution::ObservationExecutionSpecV1,
    exit_tape: &super::super::super::super::paper_replay::VirtualTape,
) -> Result<ObservationExitOutcome> {
    let entry = score
        .entry
        .as_ref()
        .ok_or("candidate_observation_entry_missing")?;
    let exit_event: Value = serde_json::from_slice(&exit_tape.events)
        .map_err(|_| "candidate_observation_exit_tape_invalid")?;
    let exit_observation = exit_event["observations"]
        .as_array()
        .and_then(|observations| {
            observations
                .iter()
                .find(|observation| observation["key"] == entry.key)
        })
        .ok_or("candidate_observation_exit_tape_invalid")?;
    let exit_at = exit_tape.event_at_ms;
    let exit_manifest: Value = serde_json::from_slice(&exit_tape.manifest)
        .map_err(|_| "candidate_observation_manifest_invalid")?;
    let instruments: Vec<nivra_core::execution::Instrument> =
        serde_json::from_value(exit_manifest["universe"].clone())
            .map_err(|_| "candidate_observation_manifest_invalid")?;
    let instrument = instruments
        .iter()
        .find(|instrument| instrument.key == entry.key)
        .ok_or("candidate_observation_exit_instrument_missing")?;
    // The ORIGINAL per-quote archived receipt SHA and purpose. A global
    // last-key SHA or a whole-tape digest is never substituted.
    let exit_archive_sha = exit_observation["archive_sha256"]
        .as_str()
        .or_else(|| {
            exit_manifest["provenance"]["per_key"][entry.key.as_str()]["archive_sha256"].as_str()
        })
        .ok_or("candidate_observation_exit_receipt_missing")?
        .to_owned();
    // The normalized typed purpose (research_observation/protection), not the
    // raw archive label `execution_or_protection`.
    let exit_purpose = exit_tape.purpose.as_str().to_owned();
    let universe_sha256 = exit_manifest["provenance"]["universe_sha256"]
        .as_str()
        .unwrap_or(exit_tape.universe_sha256.as_str())
        .to_owned();
    let exit_quote = nivra_cli::candidate_execution::FullQuoteRefV1 {
        key: entry.key.clone(),
        source_ms: exit_observation["quote"]["source"]
            .as_i64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        receipt_ms: exit_observation["quote"]["receipt"]
            .as_i64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        event_at_ms: exit_event["at"]
            .as_i64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        available_at_ms: exit_event["available_at"]
            .as_i64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        bid: serde_json::from_value(exit_observation["quote"]["bid"].clone())
            .map_err(|_| "candidate_observation_exit_tape_invalid")?,
        ask: serde_json::from_value(exit_observation["quote"]["ask"].clone())
            .map_err(|_| "candidate_observation_exit_tape_invalid")?,
        bid_units: exit_observation["quote"]["bid_units"]
            .as_u64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        ask_units: exit_observation["quote"]["ask_units"]
            .as_u64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        lower_circuit_limit: serde_json::from_value(
            exit_observation["lower_circuit_limit"].clone(),
        )
        .map_err(|_| "candidate_observation_exit_tape_invalid")?,
        upper_circuit_limit: serde_json::from_value(
            exit_observation["upper_circuit_limit"].clone(),
        )
        .map_err(|_| "candidate_observation_exit_tape_invalid")?,
        volume: exit_observation["volume"]
            .as_u64()
            .ok_or("candidate_observation_exit_tape_invalid")?,
        purpose: exit_purpose.clone(),
        source_age_ms: exit_at
            - exit_observation["quote"]["source"]
                .as_i64()
                .ok_or("candidate_observation_exit_tape_invalid")?,
        receipt_age_ms: exit_at
            - exit_observation["quote"]["receipt"]
                .as_i64()
                .ok_or("candidate_observation_exit_tape_invalid")?,
        evidence_status: "ok".into(),
        archive_sha256: exit_archive_sha.clone(),
        symbol: Some(instrument.symbol.clone()),
        series: Some(instrument.series.clone()),
        tick: Some(instrument.tick.amount().to_string()),
        universe_sha256: Some(universe_sha256.clone()),
    };
    let entry_event = json!({
        "at":entry.quote.event_at_ms,
        "available_at":entry.quote.available_at_ms,
        "kind":"virtual_observation",
        "observations":[{
            "key":entry.key,
            "quote":{
                "bid":entry.quote.bid,
                "ask":entry.quote.ask,
                "bid_units":entry.quote.bid_units,
                "ask_units":entry.quote.ask_units,
                "source":entry.quote.source_ms,
                "receipt":entry.quote.receipt_ms
            },
            "lower_circuit_limit":entry.quote.lower_circuit_limit,
            "upper_circuit_limit":entry.quote.upper_circuit_limit,
            "volume":entry.quote.volume,
            "archive_sha256":entry.quote.archive_sha256,
            "purpose":"research_observation"
        }]
    });
    let mut events = serde_json::to_string(&entry_event)?.into_bytes();
    events.push(b'\n');
    events.extend_from_slice(&exit_tape.events);
    let mut manifest: Value = exit_manifest;
    manifest["events_sha256"] = json!(digest(&events));
    manifest["max_observation_gap_ms"] = json!((exit_at - entry.quote.event_at_ms).max(1));
    manifest["provenance"]["purpose"] = json!(format!(
        "virtual_observation_entry_with_{}_exit",
        exit_purpose
    ));
    manifest["provenance"]["quote_authority"] = json!("research_observation_virtual_only");
    manifest["provenance"]["not_protection"] = json!(true);
    let mut entry_per_key = serde_json::Map::new();
    entry_per_key.insert(
        entry.key.clone(),
        json!({
            "archive_sha256":entry.quote.archive_sha256,
            "source_ms":entry.quote.source_ms,
            "receipt_ms":entry.quote.receipt_ms,
            "purpose":"research_observation"
        }),
    );
    manifest["provenance"]["entry_per_key"] = Value::Object(entry_per_key);
    let mut exit_per_key = serde_json::Map::new();
    exit_per_key.insert(
        entry.key.clone(),
        json!({
            "archive_sha256":exit_archive_sha,
            "source_ms":exit_quote.source_ms,
            "receipt_ms":exit_quote.receipt_ms,
            "purpose":exit_purpose
        }),
    );
    manifest["provenance"]["exit_per_key"] = Value::Object(exit_per_key);
    // The combined entry+exit tape has two original receipts for one key, so a
    // single global archive SHA is ambiguous. Per-quote provenance is canonical.
    manifest["provenance"]["archive_sha256"] = Value::Null;
    manifest["provenance"]["exit_archive_sha256"] = json!(exit_quote.archive_sha256);
    let manifest = serde_json::to_vec(&manifest)?;
    nivra_cli::strategy_archive::validate_archive(&manifest, &events)?;
    let recorded_entry = nivra_cli::strategy_actions::Entry {
        at_ms: entry.decision_at_ms,
        key: entry.key.clone(),
        units: entry.units,
        max_price: entry.max_price,
    };
    let source =
        nivra_cli::candidate_execution::artifact_bytes(std::slice::from_ref(&recorded_entry))?;
    let parameters = nivra_cli::candidate_execution::artifact_bytes(&spec.parameters)?;
    let report = match nivra_cli::strategy_actions::evaluate_virtual_observation(
        &source,
        &parameters,
        &manifest,
        &events,
    ) {
        Ok(report) => report,
        Err(_) => {
            return Ok(ObservationExitOutcome::MeasurementUnavailable(
                ObservationExitUnavailable { exit_quote },
            ));
        }
    };
    nivra_cli::candidate_execution::verify_virtual_observation_report(
        &report,
        &recorded_entry,
        spec,
        &manifest,
        &events,
        false,
    )?;
    let summary = nivra_cli::candidate_execution::virtual_observation_summary(
        &report,
        spec.parameters.capital,
    )?;
    let exit_status = if summary.flat && summary.accounting_complete && summary.fills >= 2 {
        "executed"
    } else if !summary.flat {
        "not_flat"
    } else if !summary.accounting_complete {
        "accounting_incomplete"
    } else {
        "execution_unavailable"
    };
    let mut gaps = Vec::new();
    if exit_status != "executed" {
        gaps.push(exit_status.to_string());
    }
    if !summary.unknown_marks.is_empty() {
        gaps.push("missing_mark".into());
    }
    if !summary.residual_positions.is_empty() {
        gaps.push("residual_positions".into());
    }
    if exit_tape.budget_truncated {
        gaps.push("observation_coverage_budget_truncated".into());
    }
    let report_sha256 = nivra_cli::candidate_execution::observation_artifact_sha256(&report)?;
    Ok(ObservationExitOutcome::Evaluated(
        ObservationExitEvaluation {
            exit_quote,
            exit_status: exit_status.into(),
            exit_gap: exit_tape.coverage_gap.clone(),
            first_receipt_unknown: false,
            execution: Some(summary),
            report_sha256: Some(report_sha256),
            gaps,
            exit_at_ms: exit_at,
        },
    ))
}

/// The recorded financial ABI for a shared-engine observation episode. A body
/// without this exact ABI is never trusted for PnL: the reader cannot prove
/// which rules produced it, so it stays an explicit unavailable.
pub(crate) const OBSERVATION_FINANCIAL_ABI: &str = "shared_engine_v1";

pub(crate) fn no_financial_claim(recorded: &Value) -> bool {
    recorded["execution"].is_null()
        && recorded["net_pnl_if_flat"].is_null()
        && recorded["fees"].is_null()
        && recorded["turnover"].is_null()
        && recorded["report_sha256"].is_null()
        && recorded["flat"] == false
        && recorded["accounting_complete"] == false
}

/// The closed stored-observation outcome kinds. A CURRENT maturation predates
/// the explicit `state` field (the historical wire format); a cross-session
/// continuation names its state. Both are read by ONE canonical validator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StoredObservationKind {
    CurrentMaturation,
    Continuation,
}

/// Everything the canonical reader needs to re-derive a stored observation from
/// VERIFIED source facts. Nothing here is copied from the stored body.
pub(crate) struct StoredObservationContext<'a> {
    pub source: crate::runtime::paper_replay::QuoteSource<'a>,
    pub session: chrono::NaiveDate,
    pub previous_session: Option<chrono::NaiveDate>,
    /// The VERIFIED observation task that produced `score`. The stored body's
    /// current identity fields must reproduce this task, never a caller or body
    /// supplied id.
    pub task: &'a jobs::CandidateTaskRecord,
    pub score: &'a nivra_cli::candidate_execution::ObservationScoreV1,
    pub spec: &'a nivra_cli::candidate_execution::ObservationExecutionSpecV1,
    pub target: &'a jobs::ObservationTargetV1,
    pub kind: StoredObservationKind,
    pub immutable: bool,
    /// Authoritative ledger row clock for a continuation; `None` for a current
    /// maturation (whose read time is metadata, not evidence).
    pub recorded_at_ms: Option<i64>,
    pub source_runtime_hash: Option<&'a str>,
    pub source_cohort_key: Option<&'a str>,
    pub current_runtime_hash: Option<&'a str>,
    pub current_cohort_key: Option<&'a str>,
    pub now: i64,
}

/// Result of the canonical reader. `matured` is true only for an evaluated
/// outcome with a complete shared-engine execution.
pub(crate) struct VerifiedStoredObservation {
    pub due_ms: i64,
    pub deadline_ms: i64,
    pub state: &'static str,
    pub exit_status: String,
    pub matured: bool,
}

fn stored_measurement_content_hash(value: &Value) -> Option<String> {
    let mut content = value.clone();
    let object = content.as_object_mut()?;
    object.insert("measured_at_ms".into(), json!(0));
    object.insert("measurement_sha256".into(), json!(""));
    nivra_cli::candidate_execution::observation_artifact_sha256(&content).ok()
}

/// The EXACT closed top-level key set for a CURRENT maturation body. A stored
/// body with any additional key is not this schema, even if its self-hash was
/// recomputed: unknown fields are never republished.
const CURRENT_MATURATION_KEYS: &[&str] = &[
    "version",
    "financial_abi",
    "candidate_id",
    "task_id",
    "revision",
    "key",
    "observation_sha256",
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
    "measured_at_ms",
    "measurement_sha256",
];

/// The EXACT closed top-level key set: every allowed key must be PRESENT and no
/// other key may exist. A missing required key is a different schema, not an
/// implicit null; an extra key is never a forward-compatible extension.
fn exact_top_level_keys(value: &Value, allowed: &[&str]) -> bool {
    value.as_object().is_some_and(|map| {
        map.len() == allowed.len() && allowed.iter().all(|key| map.contains_key(*key))
    })
}

fn canonical_amount(value: Option<Amount>) -> std::result::Result<Value, String> {
    serde_json::to_value(value).map_err(|_| "candidate_observation_exit_invalid".into())
}

/// The exact typed tape-gap projection for one producer. A current maturation
/// uses the historical `exit_quote_missing`/`exit_first_receipt_unknown`
/// vocabulary; a continuation keeps the typed cause (including
/// `original_query_unavailable`) and never asserts `original_no_data` for a
/// query failure.
fn expected_tape_gap(
    kind: StoredObservationKind,
    gap: &Value,
) -> (&'static str, bool, &'static str) {
    match gap["kind"].as_str() {
        Some("compacted_receipt") => (
            "exit_first_receipt_unknown",
            true,
            "original_quote_compacted",
        ),
        Some("first_receipt_unknown") => (
            "exit_first_receipt_unknown",
            true,
            "original_first_receipt_unknown",
        ),
        Some("coverage_budget_truncated") => {
            ("exit_quote_missing", false, "original_coverage_truncated")
        }
        Some("missing_quote") => ("exit_quote_missing", false, "original_no_data"),
        Some("quote_query_unavailable") if kind == StoredObservationKind::CurrentMaturation => {
            ("exit_quote_missing", false, "original_query_unavailable")
        }
        Some("quote_query_unavailable") => (
            "exit_measurement_unavailable",
            false,
            "original_query_unavailable",
        ),
        Some(
            "invalid_tape_request"
            | "screen_purpose"
            | "event_encoding"
            | "universe_unavailable"
            | "universe_invalid"
            | "previous_session",
        ) => (
            "exit_measurement_unavailable",
            false,
            "original_tape_invalid",
        ),
        Some("observation_budget" | "tape_bytes") => (
            "exit_measurement_unavailable",
            false,
            "original_tape_too_large",
        ),
        _ => (
            "exit_measurement_unavailable",
            false,
            "original_query_unavailable",
        ),
    }
}

fn no_financial_claim_exact(recorded: &Value) -> bool {
    no_financial_claim(recorded)
        && recorded["marked_equity"].is_null()
        && recorded["residual_positions"].is_null()
        && recorded["unknown_marks"].is_null()
}

/// ONE canonical closed-typed reader for a stored mature observation body.
///
/// It derives the financial window, capital and study projection from the
/// VERIFIED original score/spec (never from the stored body), validates the
/// closed state/status/money shape, and re-runs the ONE existing pure shared
/// cash/fee/fill/freshness engine over the FIRST original eligible archived
/// exit in the ORIGINAL `[due, deadline]` window. A forged body cannot choose
/// its own window, exit, capital, projection or PnL: every stored fact must
/// equal the re-derived fact, including the exact typed gap set.
pub(crate) fn verify_stored_observation(
    ctx: &StoredObservationContext<'_>,
    recorded: &Value,
) -> Result<VerifiedStoredObservation> {
    // 1. Closed shape and financial ABI. A body without the exact ABI cannot
    // prove which rules produced it.
    if recorded["version"].as_u64() != Some(1) {
        return Err("candidate_observation_exit_invalid".into());
    }
    if recorded["financial_abi"].as_str() != Some(OBSERVATION_FINANCIAL_ABI) {
        return Err("candidate_observation_financial_abi_unsupported".into());
    }
    // 1b. CENTRAL CURRENT IDENTITY: the body's candidate/task/revision/score
    // identity must reproduce the VERIFIED task and score context. A caller
    // path that forgot this check cannot publish a body for another episode.
    // A continuation names the same original observation task as `source_*`.
    let (body_candidate, body_task, body_revision, body_observation_sha) = match ctx.kind {
        StoredObservationKind::CurrentMaturation => (
            recorded["candidate_id"].as_str(),
            recorded["task_id"].as_str(),
            recorded["revision"].as_u64(),
            recorded["observation_sha256"].as_str(),
        ),
        StoredObservationKind::Continuation => (
            recorded["source_candidate_id"].as_str(),
            recorded["source_task_id"].as_str(),
            recorded["source_revision"].as_u64(),
            recorded["source_observation_sha256"].as_str(),
        ),
    };
    if body_candidate != Some(ctx.task.spec.candidate_id.as_str())
        || body_task != Some(ctx.task.spec.task_id.as_str())
        || body_revision != Some(u64::from(ctx.task.spec.revision))
        || body_observation_sha != Some(ctx.score.observation_sha256.as_str())
    {
        return Err("candidate_observation_identity_mismatch".into());
    }
    // A CURRENT body has an EXACT closed top-level key set. An extra
    // `top_pnl`/`pnl`/`capital_override`/`private_path` field is a different
    // schema even if the self-hash was recomputed; it is never republished.
    if ctx.kind == StoredObservationKind::CurrentMaturation
        && !exact_top_level_keys(recorded, CURRENT_MATURATION_KEYS)
    {
        return Err("candidate_observation_exit_invalid".into());
    }
    // A continuation has a closed typed body; validate its shape, self-hash and
    // canonical identity BEFORE any financial comparison.
    if ctx.kind == StoredObservationKind::Continuation {
        experiment_ledger::lifecycle::validate_continuation_body(recorded)?;
        if recorded["state"].as_str().is_none() {
            return Err("candidate_observation_exit_invalid".into());
        }
    }
    // 2. The window is derived from the ORIGINAL verified entry decision plus
    // the FROZEN spec holding/grace. The stored body may only echo it.
    let due = match &ctx.score.entry {
        Some(entry) => entry
            .decision_at_ms
            .checked_add(ctx.spec.parameters.holding_ms),
        None => ctx
            .score
            .decision_at_ms
            .checked_add(ctx.spec.parameters.holding_ms),
    }
    .ok_or("candidate_observation_exit_invalid")?;
    let deadline = due
        .checked_add(ctx.spec.parameters.max_exit_delay_ms)
        .ok_or("candidate_observation_exit_invalid")?;
    if ctx.spec.parameters.holding_ms != ctx.target.holding_ms
        || ctx.spec.parameters.max_exit_delay_ms != ctx.target.max_exit_delay_ms
        || ctx.spec.parameters.selection_from_ms != ctx.target.window_from_ms
        || ctx.target.window_to_ms > ctx.spec.parameters.selection_to_ms
        || ctx.spec.parameters.capital <= Amount::ZERO
        || ctx.score.exit_due_ms != due
        || recorded["exit_due_ms"].as_i64() != Some(due)
        || recorded["exit_deadline_ms"].as_i64() != Some(deadline)
    {
        return Err("candidate_observation_exit_mismatch".into());
    }
    // 3. Capital and study projection are DERIVED from the frozen spec. A
    // display-only forged capital (for example 999999 while the engine ran on
    // 5000) is refused, not copied into the episode evidence.
    if recorded["capital"] != serde_json::to_value(ctx.spec.parameters.capital)?
        || recorded["observation_projection"] != json!(ctx.spec.observation_projection)
        || recorded["capital_assumption"] != json!(true)
    {
        return Err("candidate_observation_capital_mismatch".into());
    }
    let expected_scope = match ctx.kind {
        StoredObservationKind::CurrentMaturation => "episode_key_only",
        StoredObservationKind::Continuation => "continuation_original_episode",
    };
    if recorded["source_scope"] != json!(expected_scope) {
        return Err("candidate_observation_exit_invalid".into());
    }
    let expected_key = ctx
        .score
        .entry
        .as_ref()
        .map(|entry| entry.key.clone())
        .or_else(|| ctx.score.key.clone())
        .unwrap_or_default();
    if recorded["key"].as_str() != Some(expected_key.as_str()) {
        return Err("candidate_observation_exit_mismatch".into());
    }
    // 4. Closed state. A current body has no explicit state and is evaluated
    // iff it carries an execution summary; a continuation names its state and
    // the name must agree with the outcome shape. An evaluated/unknown FLIP is
    // refused even with a recomputed self-hash.
    let execution_present = recorded["execution"].is_object();
    let state = match (ctx.kind, recorded.get("state").and_then(Value::as_str)) {
        (StoredObservationKind::CurrentMaturation, None) => {
            if execution_present {
                "evaluated"
            } else {
                "unknown"
            }
        }
        (StoredObservationKind::Continuation, Some("evaluated")) if execution_present => {
            "evaluated"
        }
        (StoredObservationKind::Continuation, Some("unknown")) if !execution_present => "unknown",
        _ => return Err("candidate_observation_exit_invalid".into()),
    };
    let exit_status = recorded["exit_status"]
        .as_str()
        .ok_or("candidate_observation_exit_invalid")?;
    if !matches!(
        exit_status,
        "executed"
            | "not_flat"
            | "accounting_incomplete"
            | "execution_unavailable"
            | "entry_unavailable"
            | "exit_scope_unsupported"
            | "exit_first_receipt_unknown"
            | "exit_quote_missing"
            | "exit_measurement_unavailable"
    ) {
        return Err("candidate_observation_exit_invalid".into());
    }
    // 5. The no-entry terminal is legal ONLY when the verified score has no
    // entry. A real entry cannot be relabelled `entry_unavailable`.
    if exit_status == "entry_unavailable" {
        if ctx.kind != StoredObservationKind::CurrentMaturation
            || state != "unknown"
            || ctx.score.entry.is_some()
            || recorded["exit_quote"] != Value::Null
            || recorded["exit_gap"] != Value::Null
            || recorded["first_receipt_unknown"] != json!(false)
            || recorded["exit_at_ms"] != Value::Null
            || !no_financial_claim_exact(recorded)
            || recorded["gaps"] != json!(ctx.score.gaps)
        {
            return Err("candidate_observation_exit_invalid".into());
        }
        return Ok(VerifiedStoredObservation {
            due_ms: due,
            deadline_ms: deadline,
            state,
            exit_status: exit_status.to_owned(),
            matured: false,
        });
    }
    // 6. An unsupported CURRENT scope is legal ONLY as an explicit continuation
    // UNKNOWN whose reason names a real recorded runtime/cohort difference and
    // whose source pins reproduce. A current maturation never invents it.
    if exit_status == "exit_scope_unsupported" {
        let reason = recorded["exit_gap"]["reason"]
            .as_str()
            .ok_or("candidate_observation_exit_invalid")?;
        let source_runtime = ctx
            .source_runtime_hash
            .ok_or("candidate_observation_exit_invalid")?;
        let source_cohort = ctx
            .source_cohort_key
            .ok_or("candidate_observation_exit_invalid")?;
        let current_runtime = ctx
            .current_runtime_hash
            .ok_or("candidate_observation_exit_invalid")?;
        let current_cohort = ctx
            .current_cohort_key
            .ok_or("candidate_observation_exit_invalid")?;
        let runtime_diff = current_runtime != source_runtime;
        let cohort_diff = current_cohort != source_cohort;
        let recorded_at = ctx
            .recorded_at_ms
            .ok_or("candidate_observation_exit_invalid")?;
        let mut expected_gaps = vec![reason.to_owned()];
        if recorded_at > deadline {
            expected_gaps.push("continuation_processed_after_deadline".into());
        }
        expected_gaps.push("no_fabricated_exit_price".into());
        expected_gaps.push("sha_only_gap".into());
        if ctx.kind != StoredObservationKind::Continuation
            || state != "unknown"
            || !((reason == "continuation_runtime_changed" && runtime_diff)
                || (reason == "continuation_cohort_changed" && cohort_diff))
            || recorded["exit_quote"] != Value::Null
            || recorded["exit_at_ms"] != Value::Null
            || recorded["first_receipt_unknown"] != json!(false)
            || !no_financial_claim_exact(recorded)
            || !recorded["exit_gap"].as_object().is_some_and(|map| {
                map.len() == 6
                    && map.keys().all(|key| {
                        matches!(
                            key.as_str(),
                            "kind"
                                | "reason"
                                | "source_runtime_hash"
                                | "recorded_runtime_hash"
                                | "source_cohort_key"
                                | "recorded_cohort_key"
                        )
                    })
            })
            || recorded["exit_gap"]["kind"] != "unsupported_session_scope"
            || recorded["exit_gap"]["source_runtime_hash"] != json!(source_runtime)
            || recorded["exit_gap"]["recorded_runtime_hash"] != json!(current_runtime)
            || recorded["exit_gap"]["source_cohort_key"] != json!(source_cohort)
            || recorded["exit_gap"]["recorded_cohort_key"] != json!(current_cohort)
            || recorded["gaps"] != json!(expected_gaps)
        {
            return Err("candidate_observation_exit_mismatch".into());
        }
        return Ok(VerifiedStoredObservation {
            due_ms: due,
            deadline_ms: deadline,
            state,
            exit_status: exit_status.to_owned(),
            matured: false,
        });
    }
    // 7. Reconstruct the FIRST original eligible exit over the ORIGINAL window
    // `[due, deadline]`. The stored exit time never narrows or moves the query.
    let keys = BTreeSet::from([expected_key]);
    let tape = if ctx.immutable {
        super::super::super::super::paper_replay::virtual_observation_tape_bounded_immutable(
            ctx.source,
            ctx.session,
            ctx.previous_session,
            &keys,
            due,
            deadline,
            ctx.spec.parameters.capital,
            10_000,
            1_000,
            1_048_576,
            ctx.now,
            super::super::super::super::paper_replay::TapeMode::ExitWindow,
        )
    } else {
        super::super::super::super::paper_replay::virtual_observation_tape_bounded(
            ctx.source,
            ctx.session,
            ctx.previous_session,
            &keys,
            due,
            deadline,
            ctx.spec.parameters.capital,
            10_000,
            1_000,
            1_048_576,
            ctx.now,
            super::super::super::super::paper_replay::TapeMode::ExitWindow,
        )
    };
    let tape = match tape {
        Ok(tape) => tape,
        Err(gap) => {
            let (expected_status, first_unknown, reason) = expected_tape_gap(ctx.kind, &gap.gap);
            let mut expected_gaps = match ctx.kind {
                StoredObservationKind::CurrentMaturation => vec![expected_status.to_owned()],
                StoredObservationKind::Continuation => {
                    vec![reason.to_owned(), reason.to_owned()]
                }
            };
            if ctx.kind == StoredObservationKind::Continuation {
                expected_gaps.push("no_fabricated_exit_price".into());
                expected_gaps.push("sha_only_gap".into());
                if ctx
                    .recorded_at_ms
                    .is_some_and(|recorded| recorded > deadline)
                {
                    expected_gaps.push("continuation_processed_after_deadline".into());
                }
            }
            if state != "unknown"
                || exit_status != expected_status
                || recorded["exit_quote"] != Value::Null
                || recorded["exit_gap"] != gap.gap
                || recorded["first_receipt_unknown"] != json!(first_unknown)
                || recorded["exit_at_ms"] != Value::Null
                || !no_financial_claim_exact(recorded)
                || recorded["gaps"] != json!(expected_gaps)
            {
                return Err("candidate_observation_exit_mismatch".into());
            }
            return Ok(VerifiedStoredObservation {
                due_ms: due,
                deadline_ms: deadline,
                state,
                exit_status: exit_status.to_owned(),
                matured: false,
            });
        }
    };
    let evaluation = match evaluate_observation_exit(ctx.score, ctx.spec, &tape)? {
        ObservationExitOutcome::Evaluated(evaluation) => evaluation,
        ObservationExitOutcome::MeasurementUnavailable(unavailable) => {
            let mut expected_gaps = match ctx.kind {
                StoredObservationKind::CurrentMaturation => {
                    vec!["exit_measurement_unavailable".to_owned()]
                }
                StoredObservationKind::Continuation => vec![
                    "exit_measurement_unavailable".to_owned(),
                    "exit_measurement_unavailable".to_owned(),
                ],
            };
            if ctx.kind == StoredObservationKind::Continuation
                && ctx
                    .recorded_at_ms
                    .is_some_and(|recorded| recorded > deadline)
            {
                expected_gaps.push("continuation_processed_after_deadline".into());
            }
            if state != "unknown"
                || exit_status != "exit_measurement_unavailable"
                || recorded["exit_quote"] != serde_json::to_value(&unavailable.exit_quote)?
                || recorded["exit_gap"] != json!({"kind":"exit_measurement_unavailable"})
                || recorded["first_receipt_unknown"] != json!(false)
                || recorded["exit_at_ms"] != Value::Null
                || !no_financial_claim_exact(recorded)
                || recorded["gaps"] != json!(expected_gaps)
            {
                return Err("candidate_observation_exit_mismatch".into());
            }
            return Ok(VerifiedStoredObservation {
                due_ms: due,
                deadline_ms: deadline,
                state,
                exit_status: exit_status.to_owned(),
                matured: false,
            });
        }
    };
    let executed = evaluation.exit_status == "executed";
    let (expected_pnl, expected_fees, expected_turnover) = match (&evaluation.execution, executed) {
        (Some(summary), true) if summary.flat && summary.accounting_complete => (
            summary.net_pnl_if_flat,
            Some(summary.fees),
            Some(summary.turnover),
        ),
        (Some(summary), _) => (None, Some(summary.fees), Some(summary.turnover)),
        _ => (None, None, None),
    };
    let flat = evaluation
        .execution
        .as_ref()
        .is_some_and(|summary| summary.flat);
    let accounting_complete = evaluation
        .execution
        .as_ref()
        .is_some_and(|summary| summary.accounting_complete);
    let mut expected_gaps = evaluation.gaps.clone();
    if ctx.kind == StoredObservationKind::Continuation
        && ctx
            .recorded_at_ms
            .is_some_and(|recorded| recorded > deadline)
    {
        expected_gaps.push("continuation_processed_after_deadline".into());
    }
    let expected_exit_at = match ctx.kind {
        StoredObservationKind::CurrentMaturation => Value::Null,
        StoredObservationKind::Continuation => json!(evaluation.exit_at_ms),
    };
    if state != "evaluated"
        || exit_status != evaluation.exit_status
        || recorded["exit_quote"] != serde_json::to_value(&evaluation.exit_quote)?
        || recorded["execution"] != serde_json::to_value(&evaluation.execution)?
        || recorded["net_pnl_if_flat"] != canonical_amount(expected_pnl)?
        || recorded["fees"] != canonical_amount(expected_fees)?
        || recorded["turnover"] != canonical_amount(expected_turnover)?
        || recorded["flat"] != json!(flat)
        || recorded["accounting_complete"] != json!(accounting_complete)
        || recorded["report_sha256"] != serde_json::to_value(&evaluation.report_sha256)?
        || recorded["exit_at_ms"] != expected_exit_at
        || recorded["first_receipt_unknown"] != json!(evaluation.first_receipt_unknown)
        || recorded["exit_gap"] != serde_json::to_value(&evaluation.exit_gap)?
        || recorded["gaps"] != json!(expected_gaps)
    {
        return Err("candidate_observation_exit_mismatch".into());
    }
    // 8. The recorded self-hash must still reproduce from the exact stored
    // bytes. Read-time metadata is outside the identity.
    let expected_measurement = match ctx.kind {
        StoredObservationKind::CurrentMaturation => stored_measurement_content_hash(recorded),
        StoredObservationKind::Continuation => Some(
            experiment_ledger::lifecycle::continuation_content_sha256(recorded)?,
        ),
    };
    if recorded["measurement_sha256"].as_str() != expected_measurement.as_deref() {
        return Err("candidate_observation_measurement_unbound".into());
    }
    Ok(VerifiedStoredObservation {
        due_ms: due,
        deadline_ms: deadline,
        state,
        exit_status: exit_status.to_owned(),
        matured: evaluation.execution.is_some() && flat && accounting_complete,
    })
}

/// Mature one observed episode from the original entry ref and the FIRST
/// eligible original exit receipt in `[due, due+max_exit_delay]`. The shared
/// action engine runs once over the two-event tape. A flat virtual book has no
/// real protection receipt, so an approved research-observation receipt is the
/// exit authority; an original protection receipt remains acceptable when one
/// exists. A missing or compacted exit leaves PnL unknown rather than inventing
/// a zero.
pub(crate) fn mature_observation(
    c: &Config,
    cfg: &PlannerConfig,
    db: &mut Connection,
    task: &jobs::CandidateTaskRecord,
    result: &CandidateResultV1,
    now: i64,
) -> Result<MaturationDecision> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.spec.purpose else {
        return Err("candidate_observation_target_missing".into());
    };
    let Some(score) = &result.observation else {
        return Err("candidate_observation_score_missing".into());
    };
    let spec_bytes =
        jobs::read_candidate_artifact(db, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "candidate_observation_spec_missing")?;
    if digest(&spec_bytes) != target.spec_sha256 {
        return Err("candidate_observation_spec_changed".into());
    }
    let spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes).map_err(|_| "candidate_observation_spec_invalid")?;
    nivra_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_observation_spec_invalid")?;
    let Some(entry) = &score.entry else {
        let due = score
            .decision_at_ms
            .saturating_add(spec.parameters.holding_ms);
        let deadline = due.saturating_add(spec.parameters.max_exit_delay_ms);
        let measurement = observation_measurement(
            task,
            score,
            due,
            deadline,
            None,
            "entry_unavailable",
            None,
            false,
            None,
            None,
            score.gaps.clone(),
            Some(spec.parameters.capital),
            Some(spec.observation_projection.clone()),
            now,
        );
        return Ok(MaturationDecision::Matured(record_observation_terminal(
            c,
            task,
            &spec,
            score,
            &measurement,
            now,
        )?));
    };
    let due = entry
        .decision_at_ms
        .saturating_add(spec.parameters.holding_ms);
    let deadline = due.saturating_add(spec.parameters.max_exit_delay_ms);
    // A pinned decision is never evaluated before its due time. No terminal
    // measurement, lifecycle row or fabricated quote exists in this state.
    if now < due {
        return Ok(MaturationDecision::Waiting { due_ms: due });
    }
    let keys = BTreeSet::from([entry.key.clone()]);
    let Some(observation_settings) = cfg
        .research_tools
        .as_ref()
        .and_then(|settings| settings.candidate_compute.as_ref())
        .and_then(|candidate| candidate.observation.as_ref())
        .filter(|observation| observation.enabled)
    else {
        return Err("candidate_observation_disabled".into());
    };
    let previous_session = candidate_previous_trading_session(c);
    // The actual financial window is [due, deadline]. A read now after the
    // deadline still queries the original window, never a later read clock.
    let query_to = now.min(deadline);
    let exit_tape = super::super::super::super::paper_replay::virtual_observation_tape_bounded(
        c.quote_source(),
        c.session,
        previous_session,
        &keys,
        due,
        query_to,
        spec.parameters.capital,
        observation_settings.max_quote_events,
        observation_settings.max_observations,
        observation_settings.max_tape_bytes,
        now,
        super::super::super::super::paper_replay::TapeMode::ExitWindow,
    );
    let exit_tape = match exit_tape {
        Ok(tape) => tape,
        Err(gap) => {
            let terminal = gap.reason == "observation_quote_compacted"
                || gap.reason == "observation_quote_first_receipt_unknown"
                || now > deadline;
            if !terminal {
                return Ok(MaturationDecision::PendingCapture {
                    due_ms: due,
                    deadline_ms: deadline,
                    gap: gap.gap,
                });
            }
            let exit_status = if gap.reason == "observation_quote_compacted"
                || gap.reason == "observation_quote_first_receipt_unknown"
            {
                "exit_first_receipt_unknown"
            } else {
                "exit_quote_missing"
            };
            let measurement = observation_measurement(
                task,
                score,
                due,
                deadline,
                None,
                exit_status,
                Some(gap.gap),
                exit_status == "exit_first_receipt_unknown",
                None,
                None,
                vec![exit_status.to_string()],
                Some(spec.parameters.capital),
                Some(spec.observation_projection.clone()),
                now,
            );
            return Ok(MaturationDecision::Unknown(record_observation_terminal(
                c,
                task,
                &spec,
                score,
                &measurement,
                now,
            )?));
        }
    };
    let evaluation = match evaluate_observation_exit(score, &spec, &exit_tape)? {
        ObservationExitOutcome::Evaluated(evaluation) => evaluation,
        ObservationExitOutcome::MeasurementUnavailable(unavailable) => {
            let measurement = observation_measurement(
                task,
                score,
                due,
                deadline,
                Some(unavailable.exit_quote),
                "exit_measurement_unavailable",
                Some(json!({"kind":"exit_measurement_unavailable"})),
                false,
                None,
                None,
                vec!["exit_measurement_unavailable".into()],
                Some(spec.parameters.capital),
                Some(spec.observation_projection.clone()),
                now,
            );
            return Ok(MaturationDecision::Unknown(record_observation_terminal(
                c,
                task,
                &spec,
                score,
                &measurement,
                now,
            )?));
        }
    };
    let measurement = observation_measurement(
        task,
        score,
        due,
        deadline,
        Some(evaluation.exit_quote),
        &evaluation.exit_status,
        evaluation.exit_gap,
        evaluation.first_receipt_unknown,
        evaluation.execution,
        evaluation.report_sha256,
        evaluation.gaps,
        Some(spec.parameters.capital),
        Some(spec.observation_projection.clone()),
        now,
    );
    Ok(MaturationDecision::Matured(record_observation_terminal(
        c,
        task,
        &spec,
        score,
        &measurement,
        now,
    )?))
}
