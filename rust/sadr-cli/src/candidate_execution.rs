//! Pure execution adapter from model-scored signals and owner-declared research
//! rules to the shared cash action-tape engine.
//!
//! This module does not run a scorer, read live data, call a broker, or qualify
//! prospective authoring. It consumes frozen manifest/events bytes and scores.
//! Every replay is retrospective evidence; no field here authorizes prospective
//! authoring, promotion or activation.
//!
//! Generated content-addressed artifacts (execution spec, action tape,
//! parameters, scored signals, cost arithmetic and nested shared reports) use
//! one canonical JSON boundary encoding: serialize to `serde_json::Value`,
//! recursively sort every object's keys with `Value::sort_all_objects`, reject
//! non-integer JSON numbers, then serialize compactly with `serde_json::to_vec`.
//! Arrays keep their order. This makes a claimed hash reproducible from the
//! report's embedded JSON payload without knowing Rust struct field order.
//! Raw manifest/events journals are never re-encoded: their SHA-256 values are
//! over the exact input bytes supplied.

use crate::scorer_compare::ScorerReplayReport;
use crate::strategy_actions::{self, Entry, Parameters};
use sadr_core::{
    execution::{Instrument, Portfolio, Quote},
    finance::{Amount, CashOrderGrid, FeeArithmetic, Price},
    historical::EventKind,
    market::{MAX_FINANCIAL_AGE_MS, Segment},
    replay::digest,
};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const EXECUTION_ABI: &str = "cash_quote_action_tape_v1";
/// New study ABI for one prospective observation episode. It is deliberately
/// distinct from the retrospective cash action tape: one entry, one top key,
/// first signal only, entry at the signal and a time exit.
pub const OBSERVATION_ABI: &str = "observation_single_entry_v1";
pub const VIRTUAL_TAPE_ABI: &str = "virtual_observation_tape_v1";
pub const RESULT_KIND_OBSERVATION: &str = "forward_observation_score";
pub const OBSERVATION_SCOPE: &str = "prospective_observation";
pub const RESULT_KIND_EXECUTED: &str = "executable_replay";
pub const RESULT_KIND_UNAVAILABLE: &str = "execution_unavailable";
pub const ARTIFACT_ENCODING: &str = "canonical_json_sorted_keys_v1";
pub const MAX_COST_STRESS: usize = 4;
pub const MAX_ENTRIES: u32 = 5_000;
pub const MAX_SELECTION_TOP_N: u32 = 200;
pub const MAX_ENTRY_PRICE_OFFSET_TICKS: u32 = 100;
const MAX_SIGNALS: usize = 10_000;
const MAX_ORDERED_KEYS: usize = 10_000;
const MAX_SIGNAL_KEYS: usize = 2_000_000;

/// Canonical JSON bytes for a generated content-addressed artifact.
///
/// The encoding is explicit and independent of `serde_json`'s map feature:
/// object keys are sorted recursively with [`Value::sort_all_objects`], arrays
/// keep their order, non-integer JSON numbers are refused (financial artifacts
/// use exact decimal strings and integer counts), and the result is compact
/// UTF-8 JSON with no insignificant whitespace. Non-finite floats must be
/// refused by typed validation before this encoder: `serde_json::to_value`
/// maps NaN/Infinity to null, so this generic boundary cannot recover them. This is the only encoding used
/// for generated artifact hashes. A report consumer can therefore reproduce a
/// claimed SHA-256 from the embedded JSON payload without knowing Rust field
/// order. Raw manifest/events bytes are hashed exactly as supplied and are not
/// passed through this helper.
pub fn artifact_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    let mut value = serde_json::to_value(value)
        .map_err(|_| "candidate_execution_artifact_invalid".to_string())?;
    value.sort_all_objects();
    reject_non_integer_numbers(&value)?;
    serde_json::to_vec(&value).map_err(|_| "candidate_execution_artifact_invalid".to_string())
}

/// SHA-256 of [`artifact_bytes`].
pub fn artifact_sha256<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    Ok(digest(&artifact_bytes(value)?))
}

/// Canonical JSON bytes for a prospective-observation artifact. Unlike the
/// retrospective execution encoder, a legitimate ranking score is a finite
/// `f64`; financial money remains exact decimal strings and counts remain
/// integers. Non-finite floats are refused rather than serialized.
pub fn observation_artifact_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>, String> {
    let mut value = serde_json::to_value(value)
        .map_err(|_| "candidate_observation_artifact_invalid".to_string())?;
    value.sort_all_objects();
    reject_non_finite_numbers(&value)?;
    serde_json::to_vec(&value).map_err(|_| "candidate_observation_artifact_invalid".to_string())
}

pub fn observation_artifact_sha256<T: Serialize + ?Sized>(value: &T) -> Result<String, String> {
    Ok(digest(&observation_artifact_bytes(value)?))
}

fn reject_non_finite_numbers(value: &Value) -> Result<(), String> {
    match value {
        Value::Number(number) => {
            if number.is_f64() && !number.as_f64().is_some_and(f64::is_finite) {
                Err("candidate_observation_artifact_float_invalid".into())
            } else {
                Ok(())
            }
        }
        Value::Object(object) => object.values().try_for_each(reject_non_finite_numbers),
        Value::Array(array) => array.iter().try_for_each(reject_non_finite_numbers),
        _ => Ok(()),
    }
}

/// Hash of the declared execution rules with hypothesis and parent prose
/// removed. Admission uses this identity so rewriting only the description or
/// parent link cannot reset the compute budget. The full `spec_sha256` still
/// binds the exact artifact and provenance.
pub fn execution_rules_sha256(spec: &CandidateExecutionSpecV1) -> Result<String, String> {
    let mut value =
        serde_json::to_value(spec).map_err(|_| "candidate_execution_spec_invalid".to_string())?;
    if let Some(parameters) = value.get_mut("parameters").and_then(Value::as_object_mut) {
        let _ = parameters.remove("hypothesis");
        let _ = parameters.remove("parent_candidate_id");
    }
    artifact_sha256(&value)
}

fn reject_non_integer_numbers(value: &Value) -> Result<(), String> {
    match value {
        Value::Number(number) if !number.is_i64() && !number.is_u64() => {
            Err("candidate_execution_artifact_float_unsupported".into())
        }
        Value::Object(object) => object.values().try_for_each(reject_non_integer_numbers),
        Value::Array(array) => array.iter().try_for_each(reject_non_integer_numbers),
        _ => Ok(()),
    }
}

macro_rules! execution_enum {
    ($name:ident { $($variant:ident => $text:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name {
            $($variant),+
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let value = String::deserialize(deserializer)?;
                Ok(match value.as_str() {
                    $($text => Self::$variant,)+
                    _ => Self::Unsupported,
                })
            }
        }
    };
}

execution_enum!(ExecutionSegment { Cash => "cash", Unsupported => "unsupported" });
execution_enum!(EntryAt { Signal => "signal", Unsupported => "unsupported" });
execution_enum!(ExitRule { Time => "time", Unsupported => "unsupported" });

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScoredSignal {
    pub at_ms: i64,
    pub ordered_keys: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateExecutionSpecV1 {
    pub version: u32,
    pub segment: ExecutionSegment,
    pub parameters: Parameters,
    pub units: u64,
    pub max_entries: u32,
    pub selection_top_n: u32,
    pub entry_price_offset_ticks: u32,
    pub entry_at: EntryAt,
    pub exit: ExitRule,
    pub first_signal_per_key: bool,
    #[serde(default)]
    pub cost_stress: Vec<FeeArithmetic>,
}

/// Reference to the first captured observation of a `(key, source, receipt)`
/// tuple. `event_at_ms` and `available_at_ms` are the original publication
/// clocks; a later identical republication never replaces them or renews the
/// receipt. `source_age_ms`/`receipt_age_ms` are measured at the signal.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct QuoteRef {
    pub event_at_ms: i64,
    pub available_at_ms: i64,
    pub source_ms: i64,
    pub receipt_ms: i64,
    pub bid: Price,
    pub ask: Price,
    pub bid_units: u64,
    pub ask_units: u64,
    pub lower_circuit_limit: Amount,
    pub upper_circuit_limit: Amount,
    pub volume: u64,
    pub source_age_ms: i64,
    pub receipt_age_ms: i64,
}

/// Full original quote provenance for one prospective-observation entry or
/// exit. Every field is copied from the original receipt; `archive_sha256` is
/// the frozen tape artifact hash, never a rewritten receipt body.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FullQuoteRefV1 {
    pub key: String,
    pub source_ms: i64,
    pub receipt_ms: i64,
    pub event_at_ms: i64,
    pub available_at_ms: i64,
    pub bid: Price,
    pub ask: Price,
    pub bid_units: u64,
    pub ask_units: u64,
    pub lower_circuit_limit: Amount,
    pub upper_circuit_limit: Amount,
    pub volume: u64,
    pub purpose: String,
    pub source_age_ms: i64,
    pub receipt_age_ms: i64,
    pub evidence_status: String,
    pub archive_sha256: String,
    /// Instrument identity copied from the verified source universe. Absent on
    /// historical refs written before this field existed; never invented.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick: Option<String>,
    /// SHA-256 of the exact verified source universe bytes when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub universe_sha256: Option<String>,
}

/// One prospective observation entry. `decision_at_ms` is the real helper
/// completion clock; `signal_ms` is the frozen target time. The quote provenance
/// remains the original T0 receipt.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservationEntryV1 {
    pub key: String,
    pub units: u64,
    pub max_price: Price,
    pub signal_ms: i64,
    pub decision_at_ms: i64,
    pub quote: FullQuoteRefV1,
    pub virtual_observation: bool,
}

/// New study ABI for one observation episode. It wraps the existing cash
/// parameter/entry/exit rules but fixes the projection to one entry and one
/// top-ranked key. `original_strategy_measurement` is always false: this is a
/// single-entry projection of the original algorithm, not a claim that the
/// original multi-entry strategy was measured unchanged.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationExecutionSpecV1 {
    pub version: u32,
    pub abi: String,
    pub parameters: Parameters,
    pub units: u64,
    pub max_entries: u32,
    pub selection_top_n: u32,
    pub entry_price_offset_ticks: u32,
    pub entry_at: EntryAt,
    pub exit: ExitRule,
    pub first_signal_per_key: bool,
    pub original_strategy_measurement: bool,
    pub observation_projection: String,
}

/// One prospective observation score. `authoring_at_ms` is the ORIGINAL
/// candidate admission time; `target_frozen_at_ms` is the separate observation
/// freeze. `score_produced_at_ms` and `decision_at_ms` are the real helper
/// completion clock after the scorer, never a replay result time or last tick.
/// The key is chosen after scoring from the actual first ranked key.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservationScoreV1 {
    pub version: u32,
    pub abi: String,
    pub candidate_id: String,
    pub task_id: String,
    pub revision: u32,
    pub source_task_id: String,
    pub source_revision: u32,
    pub source_candidate_id: String,
    pub source_result_sha256: String,
    pub key: Option<String>,
    pub authoring_at_ms: i64,
    pub target_frozen_at_ms: i64,
    pub features_at_ms: i64,
    pub quote_at_ms: Option<i64>,
    pub score_produced_at_ms: i64,
    pub decision_at_ms: i64,
    pub exit_due_ms: i64,
    pub quote: Option<FullQuoteRefV1>,
    pub entry: Option<ObservationEntryV1>,
    pub status: String,
    pub score: Option<f64>,
    pub score_metric: String,
    pub score_sha256: String,
    pub observation_sha256: String,
    pub observation_scope: String,
    pub policy_sha256: String,
    pub gaps: Vec<String>,
}

/// Typed result of selecting the post-score key and its frozen T0 quote. A
/// missing or stale quote is an explicit unavailability, never a later fetch.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservationActionV1 {
    pub key: Option<String>,
    pub entry: Option<Entry>,
    pub quote: Option<FullQuoteRefV1>,
    pub status: String,
    pub rejection: Option<String>,
}

/// One canonical serialized spelling per rejection reason. Serde and
/// [`AdapterRejectionReason::as_str`] must stay identical; report summaries use
/// the same strings. Malformed frozen artifacts are refused before this enum is
/// constructed (see [`execute`]), so there is no dead per-key "invalid quote"
/// variant and no unbounded opaque error payload.
/// Explicit typed validation for a prospective-observation score. A required
/// post-score target must carry a finite `f64`; `None` is only legal for the
/// explicit `scored_no_score` status. This runs BEFORE any serialization or
/// hash so a NaN/Infinity cannot be silently converted to JSON null.
pub fn validate_observation_score(score: &ObservationScoreV1) -> Result<(), String> {
    if score.version != 1
        || score.abi != OBSERVATION_ABI
        || score.observation_scope != OBSERVATION_SCOPE
        || score.status.is_empty()
    {
        return Err("candidate_observation_score_invalid".into());
    }
    if score.status == "scored_no_score" {
        if score.score.is_some() {
            return Err("candidate_observation_score_invalid".into());
        }
    } else if !score.score.is_some_and(f64::is_finite) {
        return Err("candidate_observation_score_invalid".into());
    }
    if score.entry.is_some() && score.quote.is_none() {
        return Err("candidate_observation_score_invalid".into());
    }
    Ok(())
}

/// One shared pure mapping from the executable quote selector's outcome to the
/// recorded observation status and source gap. The helper, the result verifier
/// and the stored reader all use this mapping, so an unsupported instrument or
/// invalid price is a consistent explicit unavailability rather than a
/// "Succeeded but unverified" contradiction.
pub fn observation_status_for_action(
    action: &ObservationActionV1,
) -> (&'static str, Option<&'static str>) {
    if action.entry.is_some() {
        return ("scored_entry", None);
    }
    match action.status.as_str() {
        "entry_quote_stale" => (
            "scored_missed_freshness",
            Some("score_completed_after_freshness_window"),
        ),
        "entry_instrument_unsupported" => (
            "entry_instrument_unsupported",
            Some("entry_instrument_unsupported"),
        ),
        "entry_segment_unsupported" => (
            "entry_instrument_unsupported",
            Some("entry_segment_unsupported"),
        ),
        "entry_price_invalid" => ("entry_price_invalid", Some("entry_price_invalid")),
        "no_ranked_key" => ("scored_no_ranked_key", Some("no_ranked_key")),
        _ => ("entry_quote_unavailable", Some("entry_quote_unavailable")),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdapterRejectionReason {
    #[serde(rename = "entry_quote_unavailable")]
    QuoteUnavailable,
    #[serde(rename = "entry_quote_stale")]
    QuoteStale,
    #[serde(rename = "entry_instrument_unsupported")]
    InstrumentUnsupported,
    #[serde(rename = "entry_segment_unsupported")]
    SegmentUnsupported,
    #[serde(rename = "entry_price_invalid")]
    PriceInvalid,
}

impl AdapterRejectionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuoteUnavailable => "entry_quote_unavailable",
            Self::QuoteStale => "entry_quote_stale",
            Self::InstrumentUnsupported => "entry_instrument_unsupported",
            Self::SegmentUnsupported => "entry_segment_unsupported",
            Self::PriceInvalid => "entry_price_invalid",
        }
    }
}

/// ONE shared pure binding check between a recorded observation score and the
/// action re-derived from the exact frozen artifacts. It compares the FULL
/// quote provenance for EVERY status, including `None`/stale/unavailable, not
/// just a successful entry. A status-only match would let a stale or fabricated
/// quote ride along with a non-entry score.
pub fn validate_observation_action_binding(
    score: &ObservationScoreV1,
    action: &ObservationActionV1,
) -> Result<(), String> {
    if score.key != action.key || score.quote != action.quote {
        return Err("candidate_observation_quote_unverified".into());
    }
    match (&score.entry, &action.entry) {
        (Some(recorded), Some(expected)) => {
            // The entry's own quote is part of the provenance. Comparing only
            // `score.quote` would let a forged `entry.quote.bid/ask` ride along
            // with an otherwise valid entry.
            if recorded.key != expected.key
                || recorded.units != expected.units
                || recorded.max_price != expected.max_price
                || expected.at_ms != score.decision_at_ms
                || action.status != "entry"
                || action.quote.as_ref() != Some(&recorded.quote)
            {
                return Err("candidate_observation_action_invalid".into());
            }
        }
        (None, None) => {
            let (expected_status, _) = observation_status_for_action(action);
            if score.status != expected_status {
                return Err("candidate_observation_action_invalid".into());
            }
        }
        _ => return Err("candidate_observation_action_invalid".into()),
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AdapterRejection {
    pub key: String,
    pub signal_ms: i64,
    pub reason: AdapterRejectionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_quote: Option<QuoteRef>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EntryDecision {
    pub entry_index: usize,
    pub signal_ms: i64,
    pub key: String,
    pub units: u64,
    pub max_price: Price,
    pub quote: QuoteRef,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExecutableSummary {
    pub cash: Amount,
    pub fees: Amount,
    pub turnover: Amount,
    pub fills: usize,
    pub entry_fills: usize,
    pub exit_fills: usize,
    pub net_pnl_if_flat: Option<Amount>,
    pub marked_equity: Option<Amount>,
    pub unknown_marks: Vec<String>,
    pub residual_positions: Vec<String>,
    pub engine_outcomes: BTreeMap<String, usize>,
    pub adapter_rejections: BTreeMap<String, usize>,
    pub flat: bool,
    pub accounting_complete: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CostStressResult {
    pub costs: FeeArithmetic,
    pub costs_sha256: String,
    /// The exact stress parameters passed to the shared evaluator. Embedded so
    /// `params_sha256` is reproducible from the report payload.
    pub parameters: Parameters,
    pub params_sha256: String,
    pub report_sha256: String,
    /// The exact shared evaluator report. Embedded so `report_sha256` is
    /// reproducible from the report payload.
    pub report: Value,
    pub summary: ExecutableSummary,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutableEvidence {
    pub protection_events: usize,
    /// Raw executable observations, including identical republications.
    pub executable_observations: usize,
    /// First captured observations per `(key, source, receipt)`. Absent on
    /// historical reports produced before this accounting was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique_executable_observations: Option<usize>,
    /// Identical republications that were retained as raw evidence but are not
    /// independent samples. Absent on historical reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identical_republications: Option<usize>,
    pub scanner_events: usize,
    pub scanner_observations: usize,
    pub missing_evidence: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ScoringBinding {
    pub scored_signals_sha256: String,
    pub signal_count: usize,
    pub first_signal_ms: Option<i64>,
    pub last_signal_ms: Option<i64>,
    pub ranking_evaluated: bool,
}

/// Whether the shared engine actually ran. `Unavailable` is structurally
/// distinct from an executed replay with zero fills: unavailable reports carry
/// no measured cash/fee/PnL/equity summary at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    Executed,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateExecutionReportV1 {
    pub schema: u32,
    pub result_kind: String,
    pub execution_status: ExecutionStatus,
    pub feature_abi: String,
    pub artifact_encoding: String,
    pub candidate_id: String,
    pub execution_spec: Value,
    pub execution_spec_sha256: String,
    pub scoring: ScoringBinding,
    /// The exact scored signals hashed by `scoring.scored_signals_sha256`.
    pub scored_signals: Vec<ScoredSignal>,
    pub evidence: ExecutableEvidence,
    pub action_tape: Vec<Entry>,
    pub action_tape_sha256: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub parameters_sha256: String,
    pub manifest_sha256: String,
    pub events_sha256: String,
    pub adapter_sha256: String,
    pub shared_engine_adapter_sha256: String,
    pub engine_sha256: String,
    pub report_sha256: Option<String>,
    pub as_of_ms: i64,
    pub attempts: usize,
    pub max_entries: u32,
    pub truncated: bool,
    pub decision_timeline: Vec<EntryDecision>,
    pub rejections: Vec<AdapterRejection>,
    /// Research capital assumption, never measured account cash. Measured cash
    /// exists only inside `summary` when `execution_status == Executed`.
    pub initial_capital_assumption: Amount,
    pub summary: Option<ExecutableSummary>,
    pub cost_stress: Vec<CostStressResult>,
    pub report: Option<Value>,
    pub replay_executed: bool,
    pub retrospective: bool,
    pub prospective: bool,
    pub prospective_authoring: bool,
    pub no_lookahead_qualified: bool,
    pub holdout_qualified: bool,
    pub edge_qualified: bool,
    pub promotion_authorized: bool,
    pub activation_authorized: bool,
    pub scientific_state: String,
    pub assumptions: Vec<String>,
    pub limitations: Vec<String>,
}

/// First captured observation for a `(key, source, receipt)` tuple. Later
/// identical republications are ignored so they cannot renew the receipt,
/// original capture provenance or depth; changed critical metadata refuses the
/// whole artifact before the engine runs.
#[derive(Clone)]
struct ExecutableQuote {
    key: String,
    event_at_ms: i64,
    available_at_ms: i64,
    quote: Quote,
    lower_circuit_limit: Amount,
    upper_circuit_limit: Amount,
    volume: u64,
    purpose: &'static str,
    archive_sha256: String,
    symbol: String,
    series: String,
    tick: String,
    universe_sha256: Option<String>,
}

struct ActionTape {
    entries: Vec<Entry>,
    decisions: Vec<EntryDecision>,
    rejections: Vec<AdapterRejection>,
    attempts: usize,
    truncated: bool,
}

#[derive(Debug, Deserialize)]
struct SharedReportView {
    schema: u32,
    candidate_id: String,
    source_sha256: String,
    params_sha256: String,
    feature_abi: String,
    manifest_sha256: String,
    events_sha256: String,
    engine_sha256: String,
    adapter_sha256: String,
    as_of_ms: i64,
    outcomes: Vec<Value>,
    portfolio: Portfolio,
    fees: Amount,
    turnover: Amount,
    net_pnl_if_flat: Option<Amount>,
    marked_equity: Option<Amount>,
    missing_mark_keys: Vec<String>,
    scientific_state: String,
    promotion_authorized: bool,
}

/// Strict frozen-artifact policy: every Protection observation is validated
/// before the shared engine runs. A malformed observation (bad clocks, crossed
/// or non-positive quote, non-positive depth/circuits, tick misalignment,
/// missing instrument identity, regressed receipt, or changed critical metadata
/// for the same `(key, source, receipt)`) refuses the whole artifact with a
/// fixed error. It is never downgraded to a per-key rejection or silently
/// skipped. `EventKind::Screen` observations are ranking-only and never
/// executable; `EventKind::Entry` events are refused.
pub fn execute(
    signals: &[ScoredSignal],
    spec: &CandidateExecutionSpecV1,
    manifest: &[u8],
    events: &[u8],
) -> Result<CandidateExecutionReportV1, String> {
    validate_spec(spec)?;
    validate_signals(signals, &spec.parameters)?;
    if manifest.len() > 512 * 1024 || events.len() > 16 * 1024 * 1024 {
        return Err("candidate_execution_input_limit".into());
    }
    let view = strategy_actions::archive_view(manifest, events)?;
    let (executable, evidence) = executable_quotes(
        &view,
        strategy_actions::QuoteAuthorityV1::ProtectionOnly,
        None,
    )?;
    let tape = build_tape(signals, spec, &view, &executable)?;
    let source = artifact_bytes(&tape.entries)?;
    if source.len() > 2 * 1024 * 1024 {
        return Err("candidate_execution_input_limit".into());
    }
    let (_, base_params_bytes) = parameters_with_costs(&spec.parameters, &spec.parameters.costs)?;
    if base_params_bytes.len() > 16 * 1024 {
        return Err("candidate_execution_input_limit".into());
    }
    let source_hash = digest(&source);
    let params_hash = digest(&base_params_bytes);
    let candidate_id = digest(format!("{EXECUTION_ABI}\0{source_hash}\0{params_hash}").as_bytes());
    let execution_spec =
        serde_json::to_value(spec).map_err(|_| "candidate_execution_spec_invalid")?;
    let execution_spec_sha256 = artifact_sha256(&execution_spec)?;
    let scoring = ScoringBinding {
        scored_signals_sha256: artifact_sha256(signals)?,
        signal_count: signals.len(),
        first_signal_ms: signals.first().map(|signal| signal.at_ms),
        last_signal_ms: signals.last().map(|signal| signal.at_ms),
        ranking_evaluated: false,
    };
    let as_of_ms = view.tape.last().map(|event| event.at).unwrap_or(0);
    let adapter_sha256 = digest(include_bytes!("candidate_execution.rs"));
    let shared_engine_adapter_sha256 = digest(include_bytes!("strategy_actions.rs"));
    let engine_sha256 = sadr_core::replay::engine_fingerprint();
    let manifest_sha256 = digest(manifest);
    let events_sha256 = digest(events);
    if tape.entries.is_empty() {
        return Ok(CandidateExecutionReportV1 {
            schema: 1,
            result_kind: RESULT_KIND_UNAVAILABLE.into(),
            execution_status: ExecutionStatus::Unavailable,
            feature_abi: EXECUTION_ABI.into(),
            artifact_encoding: ARTIFACT_ENCODING.into(),
            candidate_id,
            execution_spec,
            execution_spec_sha256,
            scoring,
            scored_signals: signals.to_vec(),
            evidence,
            action_tape: Vec::new(),
            action_tape_sha256: source_hash.clone(),
            source_sha256: source_hash,
            params_sha256: params_hash.clone(),
            parameters_sha256: params_hash,
            manifest_sha256,
            events_sha256,
            adapter_sha256,
            shared_engine_adapter_sha256,
            engine_sha256,
            report_sha256: None,
            as_of_ms,
            attempts: tape.attempts,
            max_entries: spec.max_entries,
            truncated: tape.truncated,
            decision_timeline: Vec::new(),
            rejections: tape.rejections,
            initial_capital_assumption: spec.parameters.capital,
            summary: None,
            cost_stress: Vec::new(),
            report: None,
            replay_executed: false,
            retrospective: true,
            prospective: false,
            prospective_authoring: false,
            no_lookahead_qualified: false,
            holdout_qualified: false,
            edge_qualified: false,
            promotion_authorized: false,
            activation_authorized: false,
            scientific_state: "insufficient_evidence".into(),
            assumptions: assumptions(),
            limitations: limitations(),
        });
    }
    let report = strategy_actions::evaluate(&source, &base_params_bytes, manifest, events)?;
    let report_view = shared_report_view(&report)?;
    verify_shared_report(&report_view, &source, &base_params_bytes, manifest, events)?;
    let summary = summarize(&report_view, &tape.rejections, spec.parameters.capital)?;
    let report_candidate_id = report_view.candidate_id;
    let report_source_sha256 = report_view.source_sha256;
    let report_params_sha256 = report_view.params_sha256;
    let report_as_of_ms = report_view.as_of_ms;
    let report_sha256 = artifact_sha256(&report)?;
    let mut cost_stress = Vec::new();
    for costs in &spec.cost_stress {
        let (stress_parameters, stress_params_bytes) =
            parameters_with_costs(&spec.parameters, costs)?;
        if !same_except_costs(&spec.parameters, &stress_parameters)? {
            return Err("candidate_execution_stress_parameters_invalid".into());
        }
        if stress_params_bytes.len() > 16 * 1024 {
            return Err("candidate_execution_input_limit".into());
        }
        let stress_report =
            strategy_actions::evaluate(&source, &stress_params_bytes, manifest, events)?;
        let stress_view = shared_report_view(&stress_report)?;
        verify_shared_report(
            &stress_view,
            &source,
            &stress_params_bytes,
            manifest,
            events,
        )?;
        let stress_summary = summarize(&stress_view, &tape.rejections, spec.parameters.capital)?;
        let stress_report_sha256 = artifact_sha256(&stress_report)?;
        cost_stress.push(CostStressResult {
            costs: costs.clone(),
            costs_sha256: artifact_sha256(costs)?,
            parameters: stress_parameters,
            params_sha256: stress_view.params_sha256,
            report_sha256: stress_report_sha256,
            report: stress_report,
            summary: stress_summary,
        });
    }
    Ok(CandidateExecutionReportV1 {
        schema: 1,
        result_kind: RESULT_KIND_EXECUTED.into(),
        execution_status: ExecutionStatus::Executed,
        feature_abi: EXECUTION_ABI.into(),
        artifact_encoding: ARTIFACT_ENCODING.into(),
        candidate_id: report_candidate_id,
        execution_spec,
        execution_spec_sha256,
        scoring,
        scored_signals: signals.to_vec(),
        evidence,
        action_tape: tape.entries,
        action_tape_sha256: source_hash,
        source_sha256: report_source_sha256,
        params_sha256: report_params_sha256.clone(),
        parameters_sha256: report_params_sha256,
        manifest_sha256,
        events_sha256,
        adapter_sha256,
        shared_engine_adapter_sha256,
        engine_sha256,
        report_sha256: Some(report_sha256),
        as_of_ms: report_as_of_ms,
        attempts: tape.attempts,
        max_entries: spec.max_entries,
        truncated: tape.truncated,
        decision_timeline: tape.decisions,
        rejections: tape.rejections,
        initial_capital_assumption: spec.parameters.capital,
        summary: Some(summary),
        cost_stress,
        report: Some(report),
        replay_executed: true,
        retrospective: true,
        prospective: false,
        prospective_authoring: false,
        no_lookahead_qualified: false,
        holdout_qualified: false,
        edge_qualified: false,
        promotion_authorized: false,
        activation_authorized: false,
        scientific_state: "insufficient_evidence".into(),
        assumptions: assumptions(),
        limitations: limitations(),
    })
}

/// Validate the observation study ABI. One entry, one top key, first signal,
/// entry at the signal and a time exit are fixed; the original multi-entry
/// strategy is never claimed to be measured unchanged.
pub fn validate_observation_spec(spec: &ObservationExecutionSpecV1) -> Result<(), String> {
    if spec.version != 1
        || spec.abi != OBSERVATION_ABI
        || spec.max_entries != 1
        || spec.selection_top_n != 1
        || !spec.first_signal_per_key
        || spec.entry_at != EntryAt::Signal
        || spec.exit != ExitRule::Time
        || spec.entry_price_offset_ticks != 0
        || spec.original_strategy_measurement
        || !matches!(
            spec.observation_projection.as_str(),
            "single_entry" | "top_n_1_single_entry"
        )
        || spec.units == 0
        || i64::try_from(spec.units).is_err()
        || !strategy_actions::parameters_valid(&spec.parameters)
    {
        return Err("candidate_observation_spec_invalid".into());
    }
    Ok(())
}

fn full_quote_ref(quote: &ExecutableQuote, signal_ms: i64) -> FullQuoteRefV1 {
    FullQuoteRefV1 {
        key: quote.key.clone(),
        source_ms: quote.quote.source,
        receipt_ms: quote.quote.receipt,
        event_at_ms: quote.event_at_ms,
        available_at_ms: quote.available_at_ms,
        bid: quote.quote.bid,
        ask: quote.quote.ask,
        bid_units: quote.quote.bid_units,
        ask_units: quote.quote.ask_units,
        lower_circuit_limit: quote.lower_circuit_limit,
        upper_circuit_limit: quote.upper_circuit_limit,
        volume: quote.volume,
        purpose: quote.purpose.into(),
        source_age_ms: signal_ms - quote.quote.source,
        receipt_age_ms: signal_ms - quote.quote.receipt,
        evidence_status: "ok".into(),
        archive_sha256: quote.archive_sha256.clone(),
        symbol: Some(quote.symbol.clone()),
        series: Some(quote.series.clone()),
        tick: Some(quote.tick.clone()),
        universe_sha256: quote.universe_sha256.clone(),
    }
}

fn select_observation_quote(
    quotes: &[ExecutableQuote],
    signal_ms: i64,
) -> Option<&ExecutableQuote> {
    quotes
        .iter()
        .rev()
        .find(|quote| quote.event_at_ms <= signal_ms && quote.purpose == "research_observation")
}

/// Select the post-score key (`ordered_keys[0]`) and its frozen T0 research
/// quote. The chosen key is never preselected from a static basket and a
/// missing/stale quote is an explicit unavailability, never a later fetch.
pub fn observation_action(
    signal: &ScoredSignal,
    spec: &ObservationExecutionSpecV1,
    manifest: &[u8],
    events: &[u8],
) -> Result<ObservationActionV1, String> {
    observation_action_with_provenance(signal, spec, manifest, events, &digest(events), None)
}

/// Same selection as [`observation_action`] with the ORIGINAL archived receipt
/// SHA and verified universe SHA supplied by the frozen tape. The score must
/// copy these exact values; a digest of re-encoded events is not the source
/// receipt identity.
pub fn observation_action_with_provenance(
    signal: &ScoredSignal,
    spec: &ObservationExecutionSpecV1,
    manifest: &[u8],
    events: &[u8],
    _archive_sha256: &str,
    universe_sha256: Option<&str>,
) -> Result<ObservationActionV1, String> {
    validate_observation_spec(spec)?;
    validate_signals(std::slice::from_ref(signal), &spec.parameters)?;
    if manifest.len() > 512 * 1024 || events.len() > 16 * 1024 * 1024 {
        return Err("candidate_observation_input_limit".into());
    }
    let view = strategy_actions::archive_view_virtual(manifest, events)?;
    let (executable, _) = executable_quotes(
        &view,
        strategy_actions::QuoteAuthorityV1::VirtualObservationOnly,
        universe_sha256,
    )?;
    let Some(key) = signal.ordered_keys.first().cloned() else {
        return Ok(ObservationActionV1 {
            key: None,
            entry: None,
            quote: None,
            status: "no_ranked_key".into(),
            rejection: Some("observation_no_ranked_key".into()),
        });
    };
    let Some(instrument) = view.instruments.get(&key) else {
        return Ok(ObservationActionV1 {
            key: Some(key),
            entry: None,
            quote: None,
            status: "entry_instrument_unsupported".into(),
            rejection: Some("entry_instrument_unsupported".into()),
        });
    };
    if matches!(
        Segment::from_key(&key),
        Some(Segment::NseFo | Segment::NseIndex)
    ) {
        return Ok(ObservationActionV1 {
            key: Some(key),
            entry: None,
            quote: None,
            status: "entry_segment_unsupported".into(),
            rejection: Some("entry_segment_unsupported".into()),
        });
    }
    let Some(quote) = executable
        .get(&key)
        .and_then(|quotes| select_observation_quote(quotes, signal.at_ms))
    else {
        return Ok(ObservationActionV1 {
            key: Some(key),
            entry: None,
            quote: None,
            status: "entry_quote_unavailable".into(),
            rejection: Some("entry_quote_unavailable".into()),
        });
    };
    let reference = full_quote_ref(quote, signal.at_ms);
    if reference.source_ms > reference.receipt_ms
        || reference.receipt_ms > reference.available_at_ms
        || reference.available_at_ms > reference.event_at_ms
        || reference.event_at_ms > signal.at_ms
        || reference.source_age_ms < 0
        || reference.receipt_age_ms < 0
        || !quote.quote.fresh(signal.at_ms, MAX_FINANCIAL_AGE_MS)
    {
        return Ok(ObservationActionV1 {
            key: Some(key),
            entry: None,
            quote: Some(reference),
            status: "entry_quote_stale".into(),
            rejection: Some("entry_quote_stale".into()),
        });
    }
    let max_price = match max_price_for(instrument, &quote.quote, spec.entry_price_offset_ticks) {
        Ok(price) => price,
        Err(_) => {
            return Ok(ObservationActionV1 {
                key: Some(key),
                entry: None,
                quote: Some(reference),
                status: "entry_price_invalid".into(),
                rejection: Some("entry_price_invalid".into()),
            });
        }
    };
    Ok(ObservationActionV1 {
        key: Some(key.clone()),
        entry: Some(Entry {
            at_ms: signal.at_ms,
            key,
            units: spec.units,
            max_price,
        }),
        quote: Some(reference),
        status: "entry".into(),
        rejection: None,
    })
}

/// Verify a recorded virtual-observation shared-engine report. Historical
/// validation uses the recorded engine/adapter hashes; the helper additionally
/// compares them with the current bytes before it writes a new result.
pub fn verify_virtual_observation_report(
    report: &Value,
    entry: &Entry,
    spec: &ObservationExecutionSpecV1,
    manifest: &[u8],
    events: &[u8],
    current: bool,
) -> Result<(), String> {
    validate_observation_spec(spec)?;
    let source = artifact_bytes(std::slice::from_ref(entry))?;
    let parameters = artifact_bytes(&spec.parameters)?;
    let source_sha256 = digest(&source);
    let params_sha256 = digest(&parameters);
    let expected_candidate_id =
        digest(format!("{VIRTUAL_TAPE_ABI}\0{source_sha256}\0{params_sha256}").as_bytes());
    if report["schema"] != 1
        || report["feature_abi"] != VIRTUAL_TAPE_ABI
        || report["quote_authority"] != "research_observation_virtual_only"
        || report["not_protection"] != true
        || report["scientific_state"] != "insufficient_evidence"
        || report["promotion_authorized"] != false
        || report["candidate_id"] != expected_candidate_id
        || report["source_sha256"] != source_sha256
        || report["params_sha256"] != params_sha256
        || report["manifest_sha256"] != digest(manifest)
        || report["events_sha256"] != digest(events)
        || report["source"]
            != serde_json::to_value(std::slice::from_ref(entry))
                .map_err(|_| "candidate_observation_report_invalid")?
        || report["outcomes"].as_array().map(Vec::len) != Some(1)
        || report["outcomes"][0]["entry_quote_source_ms"].is_null()
        || report["outcomes"][0]["entry_quote_receipt_ms"].is_null()
        || report["outcomes"][0]["entry_quote_purpose"] != "research_observation"
    {
        return Err("candidate_observation_report_invalid".into());
    }
    if current
        && (report["engine_sha256"] != sadr_core::replay::engine_fingerprint()
            || report["adapter_sha256"] != digest(include_bytes!("strategy_actions.rs")))
    {
        return Err("candidate_observation_report_invalid".into());
    }
    // EXIT PROVENANCE: both the entry and the FIRST eligible exit must match
    // the ORIGINAL per-quote source/receipt/purpose recorded by the frozen
    // tape. Caller strings, a global last-key SHA or a whole-tape digest are
    // never accepted as receipt authority.
    let outcome = &report["outcomes"][0];
    let key = outcome["key"]
        .as_str()
        .ok_or("candidate_observation_report_invalid")?;
    let manifest_value: Value =
        serde_json::from_slice(manifest).map_err(|_| "candidate_observation_report_invalid")?;
    let entry_ref = &manifest_value["provenance"]["entry_per_key"][key];
    let exit_ref = &manifest_value["provenance"]["exit_per_key"][key];
    if report["source"][0]["key"] != outcome["key"]
        || entry_ref["archive_sha256"].as_str().is_none()
        || exit_ref["archive_sha256"].as_str().is_none()
        || outcome["entry_quote_source_ms"] != entry_ref["source_ms"]
        || outcome["entry_quote_receipt_ms"] != entry_ref["receipt_ms"]
        || outcome["entry_quote_purpose"] != entry_ref["purpose"]
        || outcome["exit_quote_source_ms"] != exit_ref["source_ms"]
        || outcome["exit_quote_receipt_ms"] != exit_ref["receipt_ms"]
        || outcome["exit_quote_purpose"] != exit_ref["purpose"]
        || !matches!(
            outcome["exit_quote_purpose"].as_str(),
            Some("research_observation" | "protection")
        )
        || outcome["exit_at_ms"].as_i64().is_none()
        || outcome["due_ms"].as_i64().is_none()
        || outcome["deadline_ms"].as_i64().is_none()
        || outcome["exit_at_ms"].as_i64() < outcome["due_ms"].as_i64()
        || outcome["exit_at_ms"].as_i64() > outcome["deadline_ms"].as_i64()
        || outcome["exit_quote_source_ms"].as_i64() < outcome["due_ms"].as_i64()
        || outcome["exit_quote_receipt_ms"].as_i64() < outcome["due_ms"].as_i64()
        || outcome["exit_quote_source_ms"].as_i64() > outcome["exit_quote_receipt_ms"].as_i64()
        || outcome["exit_quote_receipt_ms"].as_i64() > outcome["exit_at_ms"].as_i64()
    {
        return Err("candidate_observation_report_invalid".into());
    }
    let view = shared_report_view(report)?;
    let summary = summarize(&view, &[], spec.parameters.capital)?;
    if summary
        .net_pnl_if_flat
        .is_some_and(|_| !summary.flat || !summary.accounting_complete)
        || report["net_pnl_if_flat"]
            != serde_json::to_value(summary.net_pnl_if_flat)
                .map_err(|_| "candidate_observation_report_invalid")?
    {
        return Err("candidate_observation_report_invalid".into());
    }
    Ok(())
}

/// Summary of a verified virtual-observation shared-engine report. The same
/// cash/fee/fill/depth engine computes it; after-cost fields remain null unless
/// the episode is executed, flat and accounting-complete.
pub fn virtual_observation_summary(
    report: &Value,
    capital: Amount,
) -> Result<ExecutableSummary, String> {
    let view = shared_report_view(report)?;
    summarize(&view, &[], capital)
}

/// Derive the only admissible scored signals from a ranking report.
///
/// The scorer's ticks are the selection evidence. Training-window ticks and
/// failed ticks are excluded; duplicate horizon ticks for one snapshot are
/// folded only when they carry the identical ranking. A requested top-N that
/// does not fit the actual ranking output is refused rather than truncated.
pub fn signals_from_ranking(
    report: &ScorerReplayReport,
    spec: &CandidateExecutionSpecV1,
) -> Result<Vec<ScoredSignal>, String> {
    validate_spec(spec)?;
    let mut by_snapshot = BTreeMap::<i64, Vec<String>>::new();
    for tick in &report.ticks {
        if tick.observed_ms < spec.parameters.selection_from_ms
            || tick.observed_ms > spec.parameters.selection_to_ms
            || tick.status != "ok"
        {
            continue;
        }
        if tick.candidate_top_n.len() < spec.selection_top_n as usize {
            return Err("candidate_execution_ranking_insufficient".into());
        }
        if tick.candidate_top_n.is_empty() {
            continue;
        }
        match by_snapshot.get(&tick.observed_ms) {
            Some(existing) if existing != &tick.candidate_top_n => {
                return Err("candidate_execution_ranking_duplicate_tick".into());
            }
            Some(_) => {}
            None => {
                by_snapshot.insert(tick.observed_ms, tick.candidate_top_n.clone());
            }
        }
    }
    let signals = by_snapshot
        .into_iter()
        .map(|(at_ms, ordered_keys)| ScoredSignal {
            at_ms,
            ordered_keys,
        })
        .collect::<Vec<_>>();
    validate_signals(&signals, &spec.parameters)?;
    Ok(signals)
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn verify_quote_reference(quote: &QuoteRef, signal_ms: i64) -> Result<(), String> {
    if quote.event_at_ms > signal_ms
        || quote.available_at_ms > quote.event_at_ms
        || quote.source_ms <= 0
        || quote.receipt_ms <= 0
        || quote.source_ms > quote.receipt_ms
        || quote.receipt_ms > quote.available_at_ms
        || quote.bid > quote.ask
        || quote.bid_units == 0
        || quote.ask_units == 0
        || quote.lower_circuit_limit <= Amount::ZERO
        || quote.upper_circuit_limit <= Amount::ZERO
        || quote.lower_circuit_limit > quote.upper_circuit_limit
        || quote.bid.amount() < quote.lower_circuit_limit
        || quote.ask.amount() > quote.upper_circuit_limit
        || quote.source_age_ms != signal_ms - quote.source_ms
        || quote.receipt_age_ms != signal_ms - quote.receipt_ms
    {
        return Err("candidate_execution_report_invalid".into());
    }
    Ok(())
}

/// Verify a recorded execution report without re-running the fill engine.
///
/// Every claimed hash is reproduced from the embedded payload, the summary is
/// recomputed from the recorded shared report and adapter rejections, and the
/// status/result-kind pair is checked structurally. Current executable bytes
/// are deliberately not consulted: historical validity rests on the recorded
/// policy, input hashes and unit proof. The helper separately re-verifies the
/// current engine/adapter bytes when it produces a new report.
pub fn verify_report(report: &CandidateExecutionReportV1) -> Result<(), String> {
    let spec: CandidateExecutionSpecV1 = serde_json::from_value(report.execution_spec.clone())
        .map_err(|_| "candidate_execution_report_invalid")?;
    validate_spec(&spec)?;
    let expected_candidate_id = digest(
        format!(
            "{EXECUTION_ABI}\0{}\0{}",
            report.action_tape_sha256, report.params_sha256
        )
        .as_bytes(),
    );
    if report.schema != 1
        || report.feature_abi != EXECUTION_ABI
        || report.artifact_encoding != ARTIFACT_ENCODING
        || !report.retrospective
        || report.prospective
        || report.prospective_authoring
        || report.no_lookahead_qualified
        || report.holdout_qualified
        || report.edge_qualified
        || report.promotion_authorized
        || report.activation_authorized
        || report.scientific_state != "insufficient_evidence"
        || report.max_entries == 0
        || report.max_entries > MAX_ENTRIES
        || report.attempts > MAX_ENTRIES as usize
        || report.attempts > report.max_entries as usize
        || report.attempts != report.rejections.len() + report.action_tape.len()
        || (report.truncated && report.attempts != report.max_entries as usize)
        || report.candidate_id != expected_candidate_id
        || report.as_of_ms < 0
        || report.scoring.ranking_evaluated
        || !valid_hash(&report.adapter_sha256)
        || !valid_hash(&report.shared_engine_adapter_sha256)
        || !valid_hash(&report.engine_sha256)
        || !valid_hash(&report.manifest_sha256)
        || !valid_hash(&report.events_sha256)
    {
        return Err("candidate_execution_report_invalid".into());
    }
    if report.execution_spec_sha256 != artifact_sha256(&report.execution_spec)?
        || report.params_sha256 != artifact_sha256(&spec.parameters)?
        || report.parameters_sha256 != report.params_sha256
        || report.initial_capital_assumption != spec.parameters.capital
        || report.max_entries != spec.max_entries
        || report.scoring.scored_signals_sha256 != artifact_sha256(&report.scored_signals)?
        || report.scoring.signal_count != report.scored_signals.len()
        || report.scoring.first_signal_ms
            != report.scored_signals.first().map(|signal| signal.at_ms)
        || report.scoring.last_signal_ms != report.scored_signals.last().map(|signal| signal.at_ms)
        || report.action_tape_sha256 != artifact_sha256(&report.action_tape)?
        || report.source_sha256 != report.action_tape_sha256
        || report.action_tape.len() > report.max_entries as usize
        || report
            .action_tape
            .windows(2)
            .any(|pair| pair[0].at_ms > pair[1].at_ms)
        || report.action_tape.iter().any(|entry| {
            entry.units != spec.units
                || entry.at_ms < spec.parameters.selection_from_ms
                || entry.at_ms > spec.parameters.selection_to_ms
        })
        || report.decision_timeline.len() != report.action_tape.len()
        || report
            .decision_timeline
            .iter()
            .zip(&report.action_tape)
            .enumerate()
            .any(|(index, (decision, entry))| {
                decision.entry_index != index
                    || decision.signal_ms != entry.at_ms
                    || decision.key != entry.key
                    || decision.units != entry.units
                    || decision.max_price != entry.max_price
                    || verify_quote_reference(&decision.quote, decision.signal_ms).is_err()
            })
        || report
            .rejections
            .windows(2)
            .any(|pair| pair[0].signal_ms > pair[1].signal_ms)
        || report.rejections.iter().any(|rejection| {
            rejection.key.is_empty()
                || rejection.key.len() > 128
                || rejection.signal_ms < spec.parameters.selection_from_ms
                || rejection.signal_ms > spec.parameters.selection_to_ms
                || !report.scored_signals.iter().any(|signal| {
                    signal.at_ms == rejection.signal_ms
                        && signal.ordered_keys.contains(&rejection.key)
                })
                || rejection.latest_quote.as_ref().is_some_and(|quote| {
                    verify_quote_reference(quote, rejection.signal_ms).is_err()
                })
        })
        || match (
            report.evidence.unique_executable_observations,
            report.evidence.identical_republications,
        ) {
            (None, None) => false,
            (Some(unique), Some(republications)) => {
                unique > report.evidence.executable_observations
                    || unique.checked_add(republications)
                        != Some(report.evidence.executable_observations)
            }
            _ => true,
        }
        || report.evidence.missing_evidence
            != if report.evidence.executable_observations == 0 {
                vec!["execution_or_protection".to_string()]
            } else {
                Vec::new()
            }
    {
        return Err("candidate_execution_report_invalid".into());
    }
    validate_signals(&report.scored_signals, &spec.parameters)?;
    match report.execution_status {
        ExecutionStatus::Unavailable => {
            if report.result_kind != RESULT_KIND_UNAVAILABLE
                || report.replay_executed
                || report.summary.is_some()
                || report.report.is_some()
                || report.report_sha256.is_some()
                || !report.cost_stress.is_empty()
                || !report.action_tape.is_empty()
                || !report.decision_timeline.is_empty()
            {
                return Err("candidate_execution_report_invalid".into());
            }
        }
        ExecutionStatus::Executed => {
            if report.result_kind != RESULT_KIND_EXECUTED
                || !report.replay_executed
                || report.action_tape.is_empty()
            {
                return Err("candidate_execution_report_invalid".into());
            }
            let summary = report
                .summary
                .as_ref()
                .ok_or("candidate_execution_report_invalid")?;
            let shared = report
                .report
                .as_ref()
                .ok_or("candidate_execution_report_invalid")?;
            let report_sha256 = report
                .report_sha256
                .as_ref()
                .ok_or("candidate_execution_report_invalid")?;
            if *report_sha256 != artifact_sha256(shared)? {
                return Err("candidate_execution_report_invalid".into());
            }
            let shared_view = shared_report_view(shared)?;
            verify_shared_report_recorded(
                &shared_view,
                &report.source_sha256,
                &report.params_sha256,
                &report.manifest_sha256,
                &report.events_sha256,
            )?;
            if shared_view.candidate_id != report.candidate_id
                || shared_view.as_of_ms != report.as_of_ms
                || report.engine_sha256 != shared_view.engine_sha256
                || report.shared_engine_adapter_sha256 != shared_view.adapter_sha256
                || summarize(&shared_view, &report.rejections, spec.parameters.capital)? != *summary
                || report.cost_stress.len() != spec.cost_stress.len()
            {
                return Err("candidate_execution_report_invalid".into());
            }
            for (stress, costs) in report.cost_stress.iter().zip(&spec.cost_stress) {
                if stress.costs != *costs
                    || stress.costs_sha256 != artifact_sha256(&stress.costs)?
                    || stress.params_sha256 != artifact_sha256(&stress.parameters)?
                    || stress.parameters.costs != stress.costs
                    || !same_except_costs(&spec.parameters, &stress.parameters)?
                    || stress.report_sha256 != artifact_sha256(&stress.report)?
                {
                    return Err("candidate_execution_report_invalid".into());
                }
                let stress_view = shared_report_view(&stress.report)?;
                verify_shared_report_recorded(
                    &stress_view,
                    &report.source_sha256,
                    &stress.params_sha256,
                    &report.manifest_sha256,
                    &report.events_sha256,
                )?;
                if stress_view.engine_sha256 != shared_view.engine_sha256
                    || stress_view.adapter_sha256 != shared_view.adapter_sha256
                    || stress.summary
                        != summarize(&stress_view, &report.rejections, spec.parameters.capital)?
                {
                    return Err("candidate_execution_report_invalid".into());
                }
            }
        }
    }
    Ok(())
}

/// Verify a recorded report against the exact frozen manifest/events artifacts.
/// It rebuilds only the deterministic action tape (never a second fill loop) and
/// checks the recorded evidence, decisions and rejections byte-for-byte.
pub fn verify_report_against_manifest(
    report: &CandidateExecutionReportV1,
    manifest: &[u8],
    events: &[u8],
) -> Result<(), String> {
    verify_report(report)?;
    if digest(manifest) != report.manifest_sha256 || digest(events) != report.events_sha256 {
        return Err("candidate_execution_report_invalid".into());
    }
    let spec: CandidateExecutionSpecV1 = serde_json::from_value(report.execution_spec.clone())
        .map_err(|_| "candidate_execution_report_invalid")?;
    let view = strategy_actions::archive_view(manifest, events)?;
    let (executable, evidence) = executable_quotes(
        &view,
        strategy_actions::QuoteAuthorityV1::ProtectionOnly,
        None,
    )?;
    if !evidence_matches(&report.evidence, &evidence) {
        return Err("candidate_execution_report_invalid".into());
    }
    let tape = build_tape(&report.scored_signals, &spec, &view, &executable)?;
    let expected_source_sha256 = digest(&artifact_bytes(&tape.entries)?);
    let expected_candidate_id = digest(
        format!(
            "{EXECUTION_ABI}\0{expected_source_sha256}\0{}",
            report.params_sha256
        )
        .as_bytes(),
    );
    if report.as_of_ms != view.tape.last().map(|event| event.at).unwrap_or(0)
        || report.action_tape_sha256 != expected_source_sha256
        || report.candidate_id != expected_candidate_id
        || tape.entries != report.action_tape
        || tape.decisions != report.decision_timeline
        || tape.rejections != report.rejections
        || tape.attempts != report.attempts
        || tape.truncated != report.truncated
    {
        return Err("candidate_execution_report_invalid".into());
    }
    Ok(())
}

/// Compare the legacy evidence fields exactly and the optional raw/unique
/// republication accounting when the report recorded it. Historical reports
/// without the optional fields remain valid.
fn evidence_matches(recorded: &ExecutableEvidence, rebuilt: &ExecutableEvidence) -> bool {
    recorded.protection_events == rebuilt.protection_events
        && recorded.executable_observations == rebuilt.executable_observations
        && recorded.scanner_events == rebuilt.scanner_events
        && recorded.scanner_observations == rebuilt.scanner_observations
        && recorded.missing_evidence == rebuilt.missing_evidence
        && match (
            recorded.unique_executable_observations,
            recorded.identical_republications,
        ) {
            (None, None) => true,
            (Some(unique), Some(republications)) => {
                Some(unique) == rebuilt.unique_executable_observations
                    && Some(republications) == rebuilt.identical_republications
            }
            _ => false,
        }
}

/// Validate the typed execution rules without running the engine. The CLI uses
/// this before admission; the helper repeats it over the staged artifact.
pub fn validate_spec(spec: &CandidateExecutionSpecV1) -> Result<(), String> {
    if spec.version != 1 {
        return Err("candidate_execution_spec_invalid".into());
    }
    if spec.segment != ExecutionSegment::Cash {
        return Err("candidate_execution_unsupported_segment".into());
    }
    if spec.entry_at != EntryAt::Signal {
        return Err("candidate_execution_unsupported_entry_rule".into());
    }
    if spec.exit != ExitRule::Time {
        return Err("candidate_execution_unsupported_exit_rule".into());
    }
    if spec.units == 0
        || i64::try_from(spec.units).is_err()
        || !(1..=MAX_ENTRIES).contains(&spec.max_entries)
        || !(1..=MAX_SELECTION_TOP_N).contains(&spec.selection_top_n)
        || spec.entry_price_offset_ticks > MAX_ENTRY_PRICE_OFFSET_TICKS
        || spec.cost_stress.len() > MAX_COST_STRESS
    {
        return Err("candidate_execution_spec_invalid".into());
    }
    if !strategy_actions::parameters_valid(&spec.parameters) {
        return Err("candidate_execution_parameters_invalid".into());
    }
    for costs in &spec.cost_stress {
        costs
            .validate()
            .map_err(|_| "candidate_execution_parameters_invalid")?;
    }
    Ok(())
}

fn validate_signals(signals: &[ScoredSignal], parameters: &Parameters) -> Result<(), String> {
    if signals.len() > MAX_SIGNALS {
        return Err("candidate_execution_input_limit".into());
    }
    let mut total_keys = 0usize;
    let mut previous = None;
    for signal in signals {
        if signal.at_ms < parameters.selection_from_ms || signal.at_ms > parameters.selection_to_ms
        {
            return Err("candidate_execution_signal_out_of_range".into());
        }
        if previous.is_some_and(|at| signal.at_ms < at) {
            return Err("candidate_execution_signals_invalid".into());
        }
        if signal
            .at_ms
            .checked_add(parameters.holding_ms)
            .and_then(|due| due.checked_add(parameters.max_exit_delay_ms))
            .is_none()
        {
            return Err("candidate_execution_signal_out_of_range".into());
        }
        previous = Some(signal.at_ms);
        if signal.ordered_keys.len() > MAX_ORDERED_KEYS {
            return Err("candidate_execution_input_limit".into());
        }
        total_keys = total_keys
            .checked_add(signal.ordered_keys.len())
            .ok_or("candidate_execution_input_limit")?;
        if total_keys > MAX_SIGNAL_KEYS {
            return Err("candidate_execution_input_limit".into());
        }
        let mut seen = BTreeSet::new();
        for key in &signal.ordered_keys {
            if key.is_empty() || key.len() > 128 || !seen.insert(key) {
                return Err("candidate_execution_signal_invalid".into());
            }
        }
    }
    Ok(())
}

fn executable_quotes(
    view: &strategy_actions::ArchiveView,
    authority: strategy_actions::QuoteAuthorityV1,
    universe_sha256: Option<&str>,
) -> Result<(BTreeMap<String, Vec<ExecutableQuote>>, ExecutableEvidence), String> {
    let mut quotes: BTreeMap<String, Vec<ExecutableQuote>> = BTreeMap::new();
    // First captured observation per (key, source, receipt). A later identical
    // republish must not replace this provenance or refresh depth.
    let mut first_capture: BTreeMap<(String, i64, i64), ExecutableQuote> = BTreeMap::new();
    // Mirrors the shared engine's per-key receipt/source monotonicity guard so a
    // corrupt tape is refused even when no entry reaches the engine.
    let mut last_quote: BTreeMap<String, Quote> = BTreeMap::new();
    let mut protection_events = 0usize;
    let mut executable_observations = 0usize;
    let mut unique_executable_observations = 0usize;
    let mut scanner_events = 0usize;
    let mut scanner_observations = 0usize;
    for event in &view.tape {
        match event.kind {
            EventKind::Screen => {
                scanner_events += 1;
                scanner_observations += event.observations.len();
            }
            EventKind::VirtualObservation => {
                if authority != strategy_actions::QuoteAuthorityV1::VirtualObservationOnly {
                    return Err("candidate_execution_virtual_observation_unauthorized".into());
                }
                for observation in &event.observations {
                    let instrument = view
                        .instruments
                        .get(&observation.key)
                        .ok_or("candidate_execution_quote_identity")?;
                    let quote = &observation.quote;
                    if quote.source <= 0
                        || quote.receipt <= 0
                        || quote.source > quote.receipt
                        || quote.receipt > event.available_at
                        || event.available_at > event.at
                        || quote.bid > quote.ask
                        || quote.bid_units == 0
                        || quote.ask_units == 0
                        || observation.lower_circuit_limit <= Amount::ZERO
                        || observation.upper_circuit_limit <= Amount::ZERO
                        || observation.lower_circuit_limit > observation.upper_circuit_limit
                        || quote.bid.amount() < observation.lower_circuit_limit
                        || quote.ask.amount() > observation.upper_circuit_limit
                    {
                        return Err("candidate_execution_quote_invalid".into());
                    }
                    let grid = CashOrderGrid::new(1, instrument.tick)
                        .map_err(|_| "candidate_execution_quote_invalid")?;
                    if grid.validate(1, quote.bid).is_err() || grid.validate(1, quote.ask).is_err()
                    {
                        return Err("candidate_execution_quote_invalid".into());
                    }
                    if let Some(last) = last_quote.get(&observation.key) {
                        if quote.receipt < last.receipt || quote.source < last.source {
                            return Err("candidate_execution_quote_regressed".into());
                        }
                        if quote.receipt == last.receipt && *quote != *last {
                            return Err("candidate_execution_quote_metadata_changed".into());
                        }
                    }
                    last_quote.insert(observation.key.clone(), quote.clone());
                    let tuple = (observation.key.clone(), quote.source, quote.receipt);
                    if let Some(first) = first_capture.get(&tuple) {
                        if first.quote != *quote
                            || first.lower_circuit_limit != observation.lower_circuit_limit
                            || first.upper_circuit_limit != observation.upper_circuit_limit
                            || first.volume != observation.volume
                        {
                            return Err("candidate_execution_quote_metadata_changed".into());
                        }
                        executable_observations += 1;
                        continue;
                    }
                    let captured = ExecutableQuote {
                        key: observation.key.clone(),
                        event_at_ms: event.at,
                        available_at_ms: event.available_at,
                        quote: quote.clone(),
                        lower_circuit_limit: observation.lower_circuit_limit,
                        upper_circuit_limit: observation.upper_circuit_limit,
                        volume: observation.volume,
                        purpose: "research_observation",
                        archive_sha256: observation
                            .archive_sha256
                            .as_deref()
                            .map(str::to_owned)
                            .or_else(|| view.per_key_archive_sha.get(&observation.key).cloned())
                            .or_else(|| view.single_key_archive_sha.clone())
                            .ok_or("candidate_execution_quote_provenance_unsupported_multi_key")?,
                        symbol: instrument.symbol.clone(),
                        series: instrument.series.clone(),
                        tick: instrument.tick.amount().to_string(),
                        universe_sha256: universe_sha256.map(str::to_owned),
                    };
                    first_capture.insert(tuple, captured.clone());
                    quotes
                        .entry(observation.key.clone())
                        .or_default()
                        .push(captured);
                    executable_observations += 1;
                    unique_executable_observations += 1;
                }
            }
            EventKind::Protection => {
                // Protection receipts are retained as exit-only evidence in a
                // virtual tape; they are counted separately from entries.
                protection_events += 1;
                for observation in &event.observations {
                    let instrument = view
                        .instruments
                        .get(&observation.key)
                        .ok_or("candidate_execution_quote_identity")?;
                    let quote = &observation.quote;
                    if quote.source <= 0
                        || quote.receipt <= 0
                        || quote.source > quote.receipt
                        || quote.receipt > event.available_at
                        || event.available_at > event.at
                        || quote.bid > quote.ask
                        || quote.bid_units == 0
                        || quote.ask_units == 0
                        || observation.lower_circuit_limit <= Amount::ZERO
                        || observation.upper_circuit_limit <= Amount::ZERO
                        || observation.lower_circuit_limit > observation.upper_circuit_limit
                        || quote.bid.amount() < observation.lower_circuit_limit
                        || quote.ask.amount() > observation.upper_circuit_limit
                    {
                        return Err("candidate_execution_quote_invalid".into());
                    }
                    let grid = CashOrderGrid::new(1, instrument.tick)
                        .map_err(|_| "candidate_execution_quote_invalid")?;
                    if grid.validate(1, quote.bid).is_err() || grid.validate(1, quote.ask).is_err()
                    {
                        return Err("candidate_execution_quote_invalid".into());
                    }
                    if let Some(last) = last_quote.get(&observation.key) {
                        if quote.receipt < last.receipt || quote.source < last.source {
                            return Err("candidate_execution_quote_regressed".into());
                        }
                        if quote.receipt == last.receipt && *quote != *last {
                            return Err("candidate_execution_quote_metadata_changed".into());
                        }
                    }
                    last_quote.insert(observation.key.clone(), quote.clone());
                    let tuple = (observation.key.clone(), quote.source, quote.receipt);
                    if let Some(first) = first_capture.get(&tuple) {
                        if first.quote != *quote
                            || first.lower_circuit_limit != observation.lower_circuit_limit
                            || first.upper_circuit_limit != observation.upper_circuit_limit
                            || first.volume != observation.volume
                        {
                            return Err("candidate_execution_quote_metadata_changed".into());
                        }
                        // Identical republication: keep the first capture's
                        // provenance and depth. It cannot renew the receipt,
                        // original capture or available depth.
                        executable_observations += 1;
                        continue;
                    }
                    let captured = ExecutableQuote {
                        key: observation.key.clone(),
                        event_at_ms: event.at,
                        available_at_ms: event.available_at,
                        quote: quote.clone(),
                        lower_circuit_limit: observation.lower_circuit_limit,
                        upper_circuit_limit: observation.upper_circuit_limit,
                        volume: observation.volume,
                        purpose: "protection",
                        archive_sha256: observation
                            .archive_sha256
                            .as_deref()
                            .map(str::to_owned)
                            .or_else(|| view.per_key_archive_sha.get(&observation.key).cloned())
                            .or_else(|| view.single_key_archive_sha.clone())
                            .ok_or("candidate_execution_quote_provenance_unsupported_multi_key")?,
                        symbol: instrument.symbol.clone(),
                        series: instrument.series.clone(),
                        tick: instrument.tick.amount().to_string(),
                        universe_sha256: universe_sha256.map(str::to_owned),
                    };
                    first_capture.insert(tuple, captured.clone());
                    quotes
                        .entry(observation.key.clone())
                        .or_default()
                        .push(captured);
                    executable_observations += 1;
                    unique_executable_observations += 1;
                }
            }
            EventKind::Entry => {
                return Err("candidate_execution_unsupported_event_kind".into());
            }
        }
    }
    let missing_evidence = if executable_observations == 0 {
        vec!["execution_or_protection".to_string()]
    } else {
        Vec::new()
    };
    Ok((
        quotes,
        ExecutableEvidence {
            protection_events,
            executable_observations,
            unique_executable_observations: Some(unique_executable_observations),
            identical_republications: Some(
                executable_observations.saturating_sub(unique_executable_observations),
            ),
            scanner_events,
            scanner_observations,
            missing_evidence,
        },
    ))
}

fn build_tape(
    signals: &[ScoredSignal],
    spec: &CandidateExecutionSpecV1,
    view: &strategy_actions::ArchiveView,
    executable: &BTreeMap<String, Vec<ExecutableQuote>>,
) -> Result<ActionTape, String> {
    let max_entries =
        usize::try_from(spec.max_entries).map_err(|_| "candidate_execution_spec_invalid")?;
    let top_n =
        usize::try_from(spec.selection_top_n).map_err(|_| "candidate_execution_spec_invalid")?;
    let mut attempted = BTreeSet::new();
    let mut attempts = 0usize;
    let mut entries = Vec::new();
    let mut decisions = Vec::new();
    let mut rejections = Vec::new();
    let mut truncated = false;
    'signals: for signal in signals {
        let limit = top_n.min(signal.ordered_keys.len());
        for key in &signal.ordered_keys[..limit] {
            if spec.first_signal_per_key && attempted.contains(key) {
                continue;
            }
            if attempts >= max_entries {
                truncated = true;
                break 'signals;
            }
            if spec.first_signal_per_key {
                attempted.insert(key.clone());
            }
            attempts += 1;
            let Some(instrument) = view.instruments.get(key) else {
                rejections.push(AdapterRejection {
                    key: key.clone(),
                    signal_ms: signal.at_ms,
                    reason: AdapterRejectionReason::InstrumentUnsupported,
                    latest_quote: None,
                });
                continue;
            };
            if matches!(
                Segment::from_key(key),
                Some(Segment::NseFo | Segment::NseIndex)
            ) {
                rejections.push(AdapterRejection {
                    key: key.clone(),
                    signal_ms: signal.at_ms,
                    reason: AdapterRejectionReason::SegmentUnsupported,
                    latest_quote: None,
                });
                continue;
            }
            let Some(quote) = executable
                .get(key)
                .and_then(|quotes| select_quote(quotes, signal.at_ms))
            else {
                rejections.push(AdapterRejection {
                    key: key.clone(),
                    signal_ms: signal.at_ms,
                    reason: AdapterRejectionReason::QuoteUnavailable,
                    latest_quote: None,
                });
                continue;
            };
            let quote_ref = quote_reference(quote, signal.at_ms);
            if !quote.quote.fresh(signal.at_ms, MAX_FINANCIAL_AGE_MS) {
                rejections.push(AdapterRejection {
                    key: key.clone(),
                    signal_ms: signal.at_ms,
                    reason: AdapterRejectionReason::QuoteStale,
                    latest_quote: Some(quote_ref),
                });
                continue;
            }
            let max_price =
                match max_price_for(instrument, &quote.quote, spec.entry_price_offset_ticks) {
                    Ok(price) => price,
                    Err(_) => {
                        rejections.push(AdapterRejection {
                            key: key.clone(),
                            signal_ms: signal.at_ms,
                            reason: AdapterRejectionReason::PriceInvalid,
                            latest_quote: Some(quote_ref),
                        });
                        continue;
                    }
                };
            let entry_index = entries.len();
            entries.push(Entry {
                at_ms: signal.at_ms,
                key: key.clone(),
                units: spec.units,
                max_price,
            });
            decisions.push(EntryDecision {
                entry_index,
                signal_ms: signal.at_ms,
                key: key.clone(),
                units: spec.units,
                max_price,
                quote: quote_ref,
            });
        }
    }
    Ok(ActionTape {
        entries,
        decisions,
        rejections,
        attempts,
        truncated,
    })
}

fn select_quote(quotes: &[ExecutableQuote], signal_ms: i64) -> Option<&ExecutableQuote> {
    let index = quotes.partition_point(|quote| quote.event_at_ms <= signal_ms);
    index.checked_sub(1).map(|index| &quotes[index])
}

fn quote_reference(quote: &ExecutableQuote, signal_ms: i64) -> QuoteRef {
    QuoteRef {
        event_at_ms: quote.event_at_ms,
        available_at_ms: quote.available_at_ms,
        source_ms: quote.quote.source,
        receipt_ms: quote.quote.receipt,
        bid: quote.quote.bid,
        ask: quote.quote.ask,
        bid_units: quote.quote.bid_units,
        ask_units: quote.quote.ask_units,
        lower_circuit_limit: quote.lower_circuit_limit,
        upper_circuit_limit: quote.upper_circuit_limit,
        volume: quote.volume,
        source_age_ms: signal_ms - quote.quote.source,
        receipt_age_ms: signal_ms - quote.quote.receipt,
    }
}

fn max_price_for(
    instrument: &Instrument,
    quote: &Quote,
    offset_ticks: u32,
) -> Result<Price, String> {
    let offset = Amount::from_unsigned(u64::from(offset_ticks));
    let step = offset
        .checked_mul(instrument.tick.amount())
        .map_err(|_| "candidate_execution_price_invalid")?;
    let max_price = quote
        .ask
        .amount()
        .checked_add(step)
        .map_err(|_| "candidate_execution_price_invalid")?;
    let price = Price::try_from(max_price).map_err(|_| "candidate_execution_price_invalid")?;
    if !price
        .is_multiple_of(instrument.tick)
        .map_err(|_| "candidate_execution_price_invalid")?
    {
        return Err("candidate_execution_price_invalid".into());
    }
    Ok(price)
}

fn parameters_with_costs(
    template: &Parameters,
    costs: &FeeArithmetic,
) -> Result<(Parameters, Vec<u8>), String> {
    let mut value =
        serde_json::to_value(template).map_err(|_| "candidate_execution_parameters_invalid")?;
    let object = value
        .as_object_mut()
        .ok_or("candidate_execution_parameters_invalid")?;
    let _ = object.insert(
        "costs".into(),
        serde_json::to_value(costs).map_err(|_| "candidate_execution_parameters_invalid")?,
    );
    let parameters: Parameters =
        serde_json::from_value(value).map_err(|_| "candidate_execution_parameters_invalid")?;
    let bytes = artifact_bytes(&parameters)?;
    Ok((parameters, bytes))
}

fn same_except_costs(base: &Parameters, candidate: &Parameters) -> Result<bool, String> {
    let mut base =
        serde_json::to_value(base).map_err(|_| "candidate_execution_parameters_invalid")?;
    let mut candidate =
        serde_json::to_value(candidate).map_err(|_| "candidate_execution_parameters_invalid")?;
    base.sort_all_objects();
    candidate.sort_all_objects();
    if let Some(object) = base.as_object_mut() {
        let _ = object.remove("costs");
    }
    if let Some(object) = candidate.as_object_mut() {
        let _ = object.remove("costs");
    }
    Ok(base == candidate)
}

fn shared_report_view(report: &Value) -> Result<SharedReportView, String> {
    serde_json::from_value(report.clone()).map_err(|_| "candidate_execution_report_invalid".into())
}

fn verify_shared_report(
    view: &SharedReportView,
    source: &[u8],
    parameters: &[u8],
    manifest: &[u8],
    events: &[u8],
) -> Result<(), String> {
    if view.schema != 1
        || view.feature_abi != EXECUTION_ABI
        || view.scientific_state != "insufficient_evidence"
        || view.promotion_authorized
        || view.source_sha256 != digest(source)
        || view.params_sha256 != digest(parameters)
        || view.manifest_sha256 != digest(manifest)
        || view.events_sha256 != digest(events)
        || view.engine_sha256 != sadr_core::replay::engine_fingerprint()
        || view.adapter_sha256 != digest(include_bytes!("strategy_actions.rs"))
    {
        return Err("candidate_execution_report_invalid".into());
    }
    let candidate_id = digest(
        format!(
            "{EXECUTION_ABI}\0{}\0{}",
            view.source_sha256, view.params_sha256
        )
        .as_bytes(),
    );
    if view.candidate_id != candidate_id {
        return Err("candidate_execution_report_invalid".into());
    }
    Ok(())
}

/// Recorded-value verification for historical reads. It checks the exact input
/// bindings and the candidate identity but does not compare the engine or
/// adapter hashes with the current host executable; those were verified when
/// the report was produced and are bound by the recorded unit proof.
fn verify_shared_report_recorded(
    view: &SharedReportView,
    source_sha256: &str,
    params_sha256: &str,
    manifest_sha256: &str,
    events_sha256: &str,
) -> Result<(), String> {
    if view.schema != 1
        || view.feature_abi != EXECUTION_ABI
        || view.scientific_state != "insufficient_evidence"
        || view.promotion_authorized
        || view.source_sha256 != source_sha256
        || view.params_sha256 != params_sha256
        || view.manifest_sha256 != manifest_sha256
        || view.events_sha256 != events_sha256
        || !valid_hash(&view.engine_sha256)
        || !valid_hash(&view.adapter_sha256)
    {
        return Err("candidate_execution_report_invalid".into());
    }
    let candidate_id =
        digest(format!("{EXECUTION_ABI}\0{source_sha256}\0{params_sha256}").as_bytes());
    if view.candidate_id != candidate_id {
        return Err("candidate_execution_report_invalid".into());
    }
    Ok(())
}

fn summarize(
    view: &SharedReportView,
    rejections: &[AdapterRejection],
    capital: Amount,
) -> Result<ExecutableSummary, String> {
    let mut engine_outcomes = BTreeMap::new();
    let mut missing_exit = false;
    for outcome in &view.outcomes {
        let state = outcome
            .get("state")
            .and_then(Value::as_str)
            .ok_or("candidate_execution_report_invalid")?;
        *engine_outcomes.entry(state.to_string()).or_insert(0usize) += 1;
        missing_exit |= state == "missing_exit_evidence";
    }
    let adapter_rejections = rejection_counts(rejections);
    let residual_positions: Vec<String> = view.portfolio.positions.keys().cloned().collect();
    let unknown_marks = view.missing_mark_keys.clone();
    let flat = residual_positions.is_empty();
    let accounting_complete = flat && unknown_marks.is_empty() && !missing_exit;
    // The recorded P&L/equity must obey the same nullability the shared engine
    // promises: a residual position or missing mark can never be turned into a
    // fake positive flat P&L or a fabricated marked equity.
    let expected_net_pnl = if flat {
        Some(
            view.portfolio
                .cash
                .checked_sub(capital)
                .map_err(|_| "candidate_execution_report_invalid")?,
        )
    } else {
        None
    };
    if view.net_pnl_if_flat != expected_net_pnl {
        return Err("candidate_execution_report_invalid".into());
    }
    let expected_marked_equity = if unknown_marks.is_empty() {
        Some(
            view.portfolio
                .equity()
                .map_err(|_| "candidate_execution_report_invalid")?,
        )
    } else {
        None
    };
    if view.marked_equity != expected_marked_equity {
        return Err("candidate_execution_report_invalid".into());
    }
    let mut fees = Amount::ZERO;
    let mut turnover = Amount::ZERO;
    for fill in &view.portfolio.fills {
        fees = fees
            .checked_add(fill.fee)
            .map_err(|_| "candidate_execution_report_invalid")?;
        turnover = turnover
            .checked_add(
                fill.price
                    .amount()
                    .checked_mul(Amount::from_unsigned(fill.units))
                    .map_err(|_| "candidate_execution_report_invalid")?,
            )
            .map_err(|_| "candidate_execution_report_invalid")?;
    }
    if view.fees != fees || view.turnover != turnover {
        return Err("candidate_execution_report_invalid".into());
    }
    let mut warnings = Vec::new();
    if !flat {
        warnings.push("residual_positions".into());
    }
    if !unknown_marks.is_empty() {
        warnings.push("unknown_marks".into());
    }
    if missing_exit {
        warnings.push("missing_exit_evidence".into());
    }
    if !rejections.is_empty() {
        warnings.push("adapter_rejections".into());
    }
    if engine_outcomes
        .get("entry_execution_rejected")
        .copied()
        .unwrap_or(0)
        > 0
    {
        warnings.push("entry_execution_rejected".into());
    }
    Ok(ExecutableSummary {
        cash: view.portfolio.cash,
        fees: view.fees,
        turnover: view.turnover,
        fills: view.portfolio.fills.len(),
        entry_fills: view.portfolio.fills.iter().filter(|fill| fill.buy).count(),
        exit_fills: view.portfolio.fills.iter().filter(|fill| !fill.buy).count(),
        net_pnl_if_flat: view.net_pnl_if_flat,
        marked_equity: view.marked_equity,
        unknown_marks,
        residual_positions,
        engine_outcomes,
        adapter_rejections,
        flat,
        accounting_complete,
        warnings,
    })
}

fn rejection_counts(rejections: &[AdapterRejection]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for rejection in rejections {
        *counts
            .entry(rejection.reason.as_str().to_string())
            .or_insert(0usize) += 1;
    }
    counts
}

fn assumptions() -> Vec<String> {
    vec![
        "initial_capital_assumption_is_research_capital_not_an_account_balance".into(),
        "research_capital_and_costs_are_explicit_assumptions_not_account_funds".into(),
        "baseline_latency_is_zero_and_actual_latency_is_unknown".into(),
        "recorded_quotes_are_not_a_promise_of_actual_broker_fills".into(),
        "no_live_orders_provider_calls_or_credentials_are_used".into(),
        "scanner_and_ohlc_prices_are_not_executable_evidence".into(),
        "cost_stress_reruns_change_only_fee_arithmetic".into(),
        "frozen_quote_source_and_receipt_clocks_are_never_renewed".into(),
    ]
}

fn limitations() -> Vec<String> {
    vec![
        "declared_action_schedule_does_not_establish_prospective_authoring_or_no_lookahead".into(),
        "capture_identity_and_cost_assumptions_are_not_provider_certification".into(),
        "no_oos_walk_forward_holdout_or_multiple_testing_qualification".into(),
        "execution_replay_is_retrospective_evidence_not_forward_authorization".into(),
        "execution_unavailable_has_no_measured_cash_fees_pnl_or_equity".into(),
        "cost_stress_is_not_run_when_execution_is_unavailable".into(),
        "residual_positions_or_missing_exit_evidence_leave_pnl_and_equity_unset".into(),
        "scorer_ranking_diagnostics_are_separate_and_not_recomputed_here".into(),
    ]
}
