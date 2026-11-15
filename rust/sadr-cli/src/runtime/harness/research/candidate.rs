//! Host-side candidate authoring, asynchronous replay and verified receipts.
//! Source runs only through the existing no-network `sadr_sandbox::score_source`;
//! this module never promotes, activates or writes the paper book.
use super::super::super::experiment_ledger;
use super::*;
use sadr_cli::scorer_compare::{self, ScorerReplayReport, ScorerReplaySettings};
use sadr_core::finance::{Amount, FeeArithmetic};
use sadr_research::worker;
use sadr_store::research_jobs as jobs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

const CANDIDATE_ABI: &str = "scorer_replay_v1";
const CANDIDATE_ENGINE: &str = "sadr_sandbox::score_source";
const CANDIDATE_RESULT_KIND: &str = "ranking_diagnostic";
const CANDIDATE_EVIDENCE_SCOPE: &str = "current_session_selection_window";
const CANDIDATE_HELPER_ABI: &str = "candidate_helper_v1";
/// Bounded failure record written when the helper cannot produce an accepted
/// result within the recorded cap. It carries the code, the hash and the byte
/// count of the rejected payload so the controller can preserve a truthful
/// terminal failure without storing an oversized artifact or claiming success.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateHelperFailure {
    pub version: u32,
    pub abi: String,
    pub job_id: String,
    pub lease: String,
    pub task_id: String,
    pub revision: u32,
    pub candidate_id: String,
    pub code: String,
    pub payload_sha256: String,
    pub payload_bytes: u64,
    pub result_produced_at_ms: i64,
}

const CANDIDATE_HELPER_FAILURE_ABI: &str = "candidate_helper_failure_v1";
const CANDIDATE_PROOF_ABI: &str = "candidate_unit_proof_v1";
const CANDIDATE_MAX_EXIT_DELAY_MS: i64 = 60_000;
const CANDIDATE_GAPS: [&str; 9] = [
    "single_session",
    "not_out_of_sample",
    "no_executable_fills",
    "no_fees",
    "no_cash_replay",
    "forward_returns_are_not_pnl",
    "declared_training_window_not_used_for_fit",
    "shared_runtime_dependencies_not_byte_pinned",
    "host_containment_not_qualified",
];
/// Execution results replace the ranking-only gaps that are now addressed
/// (no executable fills, no fees, no cash replay) with the honest limits of a
/// retrospective, declared-cost replay.
const CANDIDATE_EXECUTION_GAPS: [&str; 9] = [
    "single_session",
    "not_out_of_sample",
    "retrospective_execution_replay",
    "declared_costs_not_broker_fills",
    "research_capital_not_account_funds",
    "forward_returns_are_not_pnl",
    "declared_training_window_not_used_for_fit",
    "shared_runtime_dependencies_not_byte_pinned",
    "host_containment_not_qualified",
];
const CANDIDATE_OBSERVATION_GAPS: [&str; 6] = [
    "prospective_observation_single_episode",
    "virtual_research_quote_not_a_paper_order",
    "research_capital_not_account_funds",
    "no_oos_walk_forward_or_multiple_testing_qualification",
    "no_portfolio_or_aggregate_pnl_claim",
    "original_strategy_measurement_false",
];
const CANDIDATE_RESERVED_PARAMS: [&str; 5] = [
    "params",
    "per_order",
    "rounding_increment",
    "paper_costs",
    "paper_costs_source",
];

fn deserialize_candidate_params<'de, D>(deserializer: D) -> std::result::Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // The strict tool schema carries params as a JSON-object string. Parsing it
    // here keeps the wire shape and the runtime shape identical.
    let text = String::deserialize(deserializer)?;
    let value: Value = serde_json::from_str(&text).map_err(serde::de::Error::custom)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "candidate params must be a JSON object string",
        ))
    }
}

fn deserialize_optional_execution<'de, D>(
    deserializer: D,
) -> std::result::Result<
    Option<Box<sadr_cli::candidate_execution::CandidateExecutionSpecV1>>,
    D::Error,
>
where
    D: serde::Deserializer<'de>,
{
    // The strict tool schema carries the execution spec as a nullable
    // JSON-object string, like params. A malformed string is a tool-argument
    // error before any task row or artifact exists.
    let text = Option::<String>::deserialize(deserializer)?;
    match text {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(|spec| Some(Box::new(spec)))
            .map_err(serde::de::Error::custom),
    }
}

fn deserialize_optional_parent_ref<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Box<jobs::CandidateParentRefV1>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = Option::<String>::deserialize(deserializer)?;
    match text {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(|value| Some(Box::new(value)))
            .map_err(serde::de::Error::custom),
    }
}

fn deserialize_optional_observation<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Box<CandidateObservationArgsV1>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = Option::<String>::deserialize(deserializer)?;
    match text {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(|value| Some(Box::new(value)))
            .map_err(serde::de::Error::custom),
    }
}

/// Model-declared prospective-observation rules. The original candidate supplies
/// source/params/features/result provenance; the model must explicitly declare
/// units, research capital, costs, holding and observation window. There is no
/// silent default capital, fee or holding.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateObservationArgsV1 {
    pub source_task_id: String,
    pub source_revision: u32,
    pub window_from_ms: i64,
    pub window_to_ms: i64,
    pub units: u64,
    pub capital: Amount,
    pub costs: FeeArithmetic,
    pub holding_ms: i64,
    pub max_exit_delay_ms: i64,
    pub decision_delay_ms: i64,
    #[serde(default)]
    pub observation_projection: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateSubmitArgs {
    pub task_key: String,
    pub hypothesis: String,
    pub source: String,
    #[serde(deserialize_with = "deserialize_candidate_params")]
    pub params: Value,
    pub dataset_id: String,
    pub training_from_ms: i64,
    pub training_to_ms: i64,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
    pub horizon_minutes: i64,
    #[serde(default)]
    pub parent_task_id: Option<String>,
    /// Advertised only when the reviewed execution capability is enabled.
    /// Absent or null keeps the historical ranking-only path unchanged.
    #[serde(default, deserialize_with = "deserialize_optional_execution")]
    pub execution: Option<Box<sadr_cli::candidate_execution::CandidateExecutionSpecV1>>,
    /// Optional read-only cross-session parent provenance. Mutually exclusive
    /// with the same-session `parent_task_id` chain.
    #[serde(default, deserialize_with = "deserialize_optional_parent_ref")]
    pub parent_ref: Option<Box<jobs::CandidateParentRefV1>>,
    /// Optional prospective-observation request. The host freezes the snapshot,
    /// research quote tape and study spec before admission; the model never
    /// supplies those bytes.
    #[serde(default, deserialize_with = "deserialize_optional_observation")]
    pub observation: Option<Box<CandidateObservationArgsV1>>,
}

/// The fixed helper's only result. It is advisory input to the controller: the
/// owned-unit proof is what releases the lease. The helper records the actual
/// production time, never a caller-supplied clock.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateHelperOutput {
    pub version: u32,
    pub abi: String,
    pub job_id: String,
    pub lease: String,
    pub task_id: String,
    pub revision: u32,
    pub candidate_id: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_hash: String,
    pub runtime_hash: String,
    pub result_produced_at_ms: i64,
    pub report: ScorerReplayReport,
    pub advisory: sadr_sandbox::BwrapLeaderReceipt,
    /// Present only when the launch carries an execution binding. The helper
    /// runs the pure adapter after the scorer replay; the Python sandbox never
    /// sees these artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<sadr_cli::candidate_execution::CandidateExecutionReportV1>,
    /// Present only for a prospective-observation task. The helper writes the
    /// typed score/entry decision before the controller stores the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<sadr_cli::candidate_execution::ObservationScoreV1>,
}

/// Closed vocabulary for the durable stop detail. `cause` is the authoritative
/// disposition; `outcome` records what the original unit actually did. The pair
/// is validated together so an unknown or contradictory receipt is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CandidateStopOutcome {
    Succeeded,
    Cancelled,
    DeadlineExpired,
    ResultAfterDeadline,
    NoResult,
    InvalidResult,
    ResultTooLarge,
    LaunchFailed,
    Terminated,
}

/// Version 2 of the durable stop receipt. The release authority is the typed
/// owned-unit proof; the helper's result time is separate from the proof's
/// observation time and from the controller's recording time. `cause` is the
/// domain reason (operator cancel, deadline, failure or success), never a
/// reinterpretation of a mutable queue row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateStopped {
    pub version: u32,
    pub kind: String,
    pub job_id: String,
    pub lease: String,
    pub task_id: String,
    pub revision: u32,
    pub candidate_id: String,
    pub unit: String,
    pub proof: worker::CandidateUnitProof,
    pub cause: jobs::CandidateStopCause,
    pub outcome: CandidateStopOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_bytes: Option<u64>,
    pub result_sha256: Option<String>,
    pub result_at_ms: Option<i64>,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_hash: String,
    pub runtime_hash: String,
    pub stopped_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateResultV1 {
    pub version: u32,
    pub task_id: String,
    pub revision: u32,
    pub job_id: String,
    pub lease: String,
    pub candidate_id: String,
    pub lifecycle_candidate_id: String,
    pub abi: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_id: String,
    pub dataset_hash: String,
    pub windows: jobs::CandidateWindows,
    pub horizon_minutes: i64,
    pub runtime_binding: jobs::CandidateRuntimeBinding,
    pub sandbox: jobs::CandidateSandboxPolicy,
    pub unit_policy: jobs::CandidateUnitPolicy,
    pub unit_proof: worker::CandidateUnitProof,
    pub unit_proof_sha256: String,
    pub result_produced_at_ms: i64,
    pub result_kind: String,
    pub evidence_scope: String,
    pub partial_evidence: bool,
    pub gaps: Vec<String>,
    pub advisory: sadr_sandbox::BwrapLeaderReceipt,
    pub qualified: bool,
    pub analysis_status: String,
    pub report: ScorerReplayReport,
    pub report_sha256: String,
    /// Present only for an execution task. The full adapter report embeds the
    /// exact spec, scored signals, action tape, declared stress costs and shared
    /// engine results, so every claimed hash is reproducible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<sadr_cli::candidate_execution::CandidateExecutionReportV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<sadr_cli::candidate_execution::ObservationScoreV1>,
    pub promotion_authorized: bool,
    pub activation_authorized: bool,
    pub edge_qualified: bool,
    pub host_qualified: bool,
}

fn not_dispatched(reason: &str) -> Value {
    json!({"status":"not_dispatched","reason":reason})
}

fn helper_content_sha256(
    candidate: &CandidateComputeSettings,
) -> std::result::Result<String, &'static str> {
    let path = match candidate.helper_path.as_ref() {
        Some(path) => path.clone(),
        None => std::env::current_exe().map_err(|_| "candidate_runtime_unavailable")?,
    };
    let bytes = crate::read_bounded(&path, 256 * 1024 * 1024)
        .map_err(|_| "candidate_runtime_unavailable")?;
    Ok(digest(&bytes))
}

fn candidate_unit_policy(
    candidate: &CandidateComputeSettings,
    helper_sha256: &str,
) -> std::result::Result<jobs::CandidateUnitPolicy, &'static str> {
    let policy = jobs::CandidateUnitPolicy {
        version: 1,
        helper_sha256: helper_sha256.to_owned(),
        helper_abi: CANDIDATE_HELPER_ABI.into(),
        unit_tasks: candidate.unit_tasks,
        unit_memory_bytes: candidate.unit_memory_bytes,
        unit_cpu_millis: if candidate.unit_cpu_millis == 0 {
            candidate.wall_millis
        } else {
            candidate.unit_cpu_millis
        },
        unit_wall_millis: if candidate.unit_wall_millis == 0 {
            candidate.wall_millis
        } else {
            candidate.unit_wall_millis
        },
        unit_open_files: candidate.unit_open_files,
        max_result_bytes: candidate.max_result_bytes,
        restart: "no".into(),
        payload_max_processes: candidate.sandbox.max_processes,
        payload_max_open_files: candidate.sandbox.max_open_files,
        kill_mode: "control-group".into(),
        proof_abi: CANDIDATE_PROOF_ABI.into(),
    };
    let resources = jobs::ResourceCaps {
        cpu_millis: candidate.limits.cpu_millis,
        tasks: 1,
        workspace_bytes: 1_048_576,
        file_bytes: 65_536,
        memory_bytes: candidate.sandbox.memory_mb * 1024 * 1024,
        output_bytes: candidate.sandbox.max_output_bytes as u64,
        wall_millis: candidate.wall_millis,
    };
    if worker::quota_hundredths(policy.unit_cpu_millis, policy.unit_wall_millis).is_none() {
        return Err("candidate_unit_policy_invalid");
    }
    if !policy.validate_for(
        &jobs::CandidateSandboxPolicy {
            abi: CANDIDATE_ABI.into(),
            engine: CANDIDATE_ENGINE.into(),
            network: candidate.sandbox.network,
            cpu_seconds: candidate.sandbox.cpu_seconds,
            memory_mb: candidate.sandbox.memory_mb,
            wall_ms: candidate.sandbox.wall_ms,
            max_output_bytes: candidate.sandbox.max_output_bytes as u64,
            max_keys: candidate.sandbox.max_keys as u64,
            max_processes: candidate.sandbox.max_processes,
            max_open_files: candidate.sandbox.max_open_files,
            executable_scope: "python_and_bwrap_content_only".into(),
            python_sha256: "0".repeat(64),
            bwrap_sha256: "0".repeat(64),
        },
        &resources,
    ) {
        return Err("candidate_unit_policy_invalid");
    }
    Ok(policy)
}

fn candidate_runtime_binding(
    helper_sha256: &str,
    unit_policy: &jobs::CandidateUnitPolicy,
) -> jobs::CandidateRuntimeBinding {
    let scorer_compare_sha256 = digest(include_bytes!("../../../scorer_compare.rs"));
    let sandbox_source_sha256 = digest(include_bytes!("../../../../../sadr-sandbox/src/lib.rs"));
    let unit_policy_sha256 = unit_policy.policy_sha256();
    let runtime_hash = jobs::candidate_runtime_hash(
        &scorer_compare_sha256,
        &sandbox_source_sha256,
        helper_sha256,
        &unit_policy_sha256,
    );
    jobs::CandidateRuntimeBinding {
        kind: "source_fingerprint".into(),
        abi: CANDIDATE_ABI.into(),
        engine: CANDIDATE_ENGINE.into(),
        scorer_compare_sha256,
        sandbox_source_sha256,
        helper_sha256: helper_sha256.to_owned(),
        unit_policy_sha256,
        runtime_hash,
    }
}

fn executable_content_sha256(path: &std::path::Path) -> std::result::Result<String, &'static str> {
    let resolved = fs::canonicalize(path).map_err(|_| "candidate_runtime_unavailable")?;
    let bytes = crate::read_bounded(&resolved, 64 * 1024 * 1024)
        .map_err(|_| "candidate_runtime_unavailable")?;
    Ok(digest(&bytes))
}

fn candidate_sandbox_policy(
    candidate: &CandidateComputeSettings,
) -> std::result::Result<jobs::CandidateSandboxPolicy, &'static str> {
    Ok(jobs::CandidateSandboxPolicy {
        abi: CANDIDATE_ABI.into(),
        engine: CANDIDATE_ENGINE.into(),
        network: candidate.sandbox.network,
        cpu_seconds: candidate.sandbox.cpu_seconds,
        memory_mb: candidate.sandbox.memory_mb,
        wall_ms: candidate.sandbox.wall_ms,
        max_output_bytes: candidate.sandbox.max_output_bytes as u64,
        max_keys: candidate.sandbox.max_keys as u64,
        max_processes: candidate.sandbox.max_processes,
        max_open_files: candidate.sandbox.max_open_files,
        executable_scope: "python_and_bwrap_content_only".into(),
        python_sha256: executable_content_sha256(&candidate.sandbox.python_path)?,
        bwrap_sha256: executable_content_sha256(&candidate.sandbox.bwrap_path)?,
    })
}

fn lifecycle_binding(
    dataset_hash: &str,
    abi: &str,
    execution: Option<&jobs::CandidateExecutionBinding>,
) -> (String, String) {
    match execution {
        Some(execution) => (
            digest(
                format!(
                    "candidate_execution_dataset_v1\0{dataset_hash}\0{}\0{}\0{}",
                    execution.spec_sha256, execution.manifest_sha256, execution.events_sha256
                )
                .as_bytes(),
            ),
            jobs::CANDIDATE_EXECUTION_ABI.into(),
        ),
        None => (dataset_hash.to_owned(), abi.to_owned()),
    }
}

fn experiment_spec(task: &jobs::CandidateTaskV1) -> experiment_ledger::lifecycle::ExperimentSpec {
    let (dataset_hash, feature_abi) = match &task.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => (
            digest(
                format!(
                    "candidate_observation_dataset_v1\0{}\0{}\0{}",
                    target.snapshot_sha256, target.quotes_sha256, target.spec_sha256
                )
                .as_bytes(),
            ),
            sadr_cli::candidate_execution::OBSERVATION_ABI.to_string(),
        ),
        _ => lifecycle_binding(&task.dataset_hash, &task.abi, task.execution.as_ref()),
    };
    experiment_ledger::lifecycle::ExperimentSpec {
        version: 1,
        parent_candidate_id: task.parent_lifecycle_candidate_id.clone(),
        session: task.session.clone(),
        source_sha256: task.source_sha256.clone(),
        params_sha256: task.params_sha256.clone(),
        dataset_hash,
        feature_abi,
        hypothesis: task.hypothesis.clone(),
        training_from_ms: task.windows.training_from_ms,
        training_to_ms: task.windows.training_to_ms,
        selection_from_ms: task.windows.selection_from_ms,
        selection_to_ms: task.windows.selection_to_ms,
        holding_ms: task.horizon_minutes.saturating_mul(60_000),
        max_exit_delay_ms: task.max_exit_delay_ms,
    }
}

pub(crate) fn candidate_summary(record: &jobs::CandidateTaskRecord) -> Value {
    let result_kind = if record.spec.purpose.is_some() {
        sadr_cli::candidate_execution::RESULT_KIND_OBSERVATION
    } else if record.spec.execution.is_some() {
        sadr_cli::candidate_execution::RESULT_KIND_EXECUTED
    } else {
        CANDIDATE_RESULT_KIND
    };
    let mut body = json!({
        "task_id":record.spec.task_id,
        "revision":record.spec.revision,
        "candidate_id":record.spec.candidate_id,
        "lifecycle_candidate_id":record.spec.lifecycle_candidate_id,
        "parent_candidate_id":record.spec.parent_candidate_id,
        "parent_lifecycle_candidate_id":record.spec.parent_lifecycle_candidate_id,
        "parent_task_id":record.spec.parent_task_id,
        "parent_ref":record.spec.parent_ref,
        "task_key":record.spec.task_key,
        "status":task_state_name(record.status),
        "job_id":record.spec.job_id,
        "created_at_ms":record.spec.created_at_ms,
        "expires_at_ms":record.spec.expires_at_ms,
        "terminal_at_ms":record.terminal_at_ms,
        "result_sha256":record.job.result_hash,
        "dataset_id":record.spec.dataset_id,
        "dataset_hash":record.spec.dataset_hash,
        "input_key":record.spec.input_key,
        "source_sha256":record.spec.source_sha256,
        "params_sha256":record.spec.params_sha256,
        "abi":record.spec.abi,
        "runtime_hash":record.spec.runtime_binding.runtime_hash,
        "runtime_binding":record.spec.runtime_binding,
        "sandbox":record.spec.sandbox,
        "unit_policy":record.spec.unit_policy,
        "unit":record.job.lease.as_deref().and_then(worker::candidate_unit_name),
        "windows":record.spec.windows,
        "horizon_minutes":record.spec.horizon_minutes,
        "policy_sha256":record.spec.policy_sha256,
        "model_id":record.spec.model_id,
        "model_revision":record.spec.model_revision,
        "result_kind":result_kind,
        "partial_evidence":true,
        "promotion_authorized":false,
        "activation_authorized":false,
        "edge_qualified":false
    });
    if let Some(execution) = &record.spec.execution {
        body["execution"] = json!(execution);
    }
    if let Some(jobs::CandidatePurposeV1::Observe { target }) = &record.spec.purpose {
        body["purpose"] = json!("observe");
        body["observation"] = json!({
            "source_task_id":target.source_task_id,
            "source_revision":target.source_revision,
            "source_candidate_id":target.source_candidate_id,
            "source_result_sha256":target.source_result_sha256,
            "snapshot_sha256":target.snapshot_sha256,
            "quotes_sha256":target.quotes_sha256,
            "spec_sha256":target.spec_sha256,
            "target_frozen_at_ms":target.target_frozen_at_ms,
            "quote_keys":target.quote_keys,
            "quote_budget_truncated":target.quote_budget_truncated,
            "quote_authority":"research_observation_virtual_only",
            "original_strategy_measurement":false
        });
    }
    body
}

fn lifecycle_view(c: &Config, id: &str) -> Value {
    match experiment_ledger::lifecycle::state(&c.directory, id) {
        Ok(Some(state)) => json!({
            "status":"recorded",
            "candidate_id":id,
            "state":state,
            "activation_authorized":false,
            "edge_qualified":false
        }),
        Ok(None) => json!({
            "status":"not_recorded",
            "candidate_id":id,
            "activation_authorized":false,
            "edge_qualified":false
        }),
        Err(_) => json!({"status":"unavailable","candidate_id":id}),
    }
}

fn bridge_candidate_admission(
    c: &Config,
    task: &jobs::CandidateTaskRecord,
    now: i64,
) -> Result<Value> {
    let spec = experiment_spec(&task.spec);
    let id = experiment_ledger::lifecycle::candidate_id(&spec);
    if id != task.spec.lifecycle_candidate_id {
        return Err("candidate_lifecycle_identity_changed".into());
    }
    if task.spec.parent_ref.is_some() {
        experiment_ledger::lifecycle::admit_spec_verified_parent(&c.directory, spec, now)?;
    } else {
        experiment_ledger::lifecycle::admit_spec(&c.directory, spec, now)?;
    }
    let state = experiment_ledger::lifecycle::state(&c.directory, &id)?
        .unwrap_or_else(|| "proposed".into());
    let state = if state == "proposed" {
        experiment_ledger::lifecycle::record_abi(&c.directory, &id, now)?;
        "sandbox_abi".to_owned()
    } else {
        state
    };
    Ok(json!({
        "status":"admitted",
        "candidate_id":id,
        "state":state,
        "activation_authorized":false,
        "edge_qualified":false
    }))
}

fn bridge_candidate_result(
    c: &Config,
    task: &jobs::CandidateTaskRecord,
    result: &CandidateResultV1,
    now: i64,
) -> Result<Value> {
    // Observation results are recorded as their own prospective lifecycle
    // evidence. They are never relabelled as a retrospective replay or opened
    // as a fixed-basket forward shadow.
    if let Some(observation) = &result.observation {
        let id = &task.spec.lifecycle_candidate_id;
        // Re-reading an already-recorded observation is idempotent. The
        // lifecycle may legitimately be evaluated/insufficient/eligible by the
        // time the public result is read; re-binding the score must not attempt
        // an invalid evaluated -> replayed transition.
        if let Some(state) = experiment_ledger::lifecycle::state(&c.directory, id)?
            && state != "proposed"
            && state != "sandbox_abi"
        {
            return Ok(json!({
                "status":"observation_recorded",
                "candidate_id":id,
                "state":state,
                "observation_scope":observation.observation_scope,
                "activation_authorized":false,
                "edge_qualified":false
            }));
        }
        experiment_ledger::lifecycle::record_observation(
            &c.directory,
            id,
            &serde_json::to_value(observation)?,
            now,
        )?;
        return Ok(json!({
            "status":"observation_recorded",
            "candidate_id":id,
            "state":"replayed",
            "observation_scope":observation.observation_scope,
            "activation_authorized":false,
            "edge_qualified":false
        }));
    }
    let spec = experiment_spec(&task.spec);
    let id = experiment_ledger::lifecycle::candidate_id(&spec);
    if id != task.spec.lifecycle_candidate_id || id != result.lifecycle_candidate_id {
        return Err("candidate_lifecycle_identity_changed".into());
    }
    if task.spec.parent_ref.is_some() {
        experiment_ledger::lifecycle::admit_spec_verified_parent(&c.directory, spec, now)?;
    } else {
        experiment_ledger::lifecycle::admit_spec(&c.directory, spec, now)?;
    }
    let mut state = experiment_ledger::lifecycle::state(&c.directory, &id)?
        .unwrap_or_else(|| "proposed".into());
    if state == "proposed" {
        experiment_ledger::lifecycle::record_abi(&c.directory, &id, now)?;
        state = "sandbox_abi".into();
    }
    if state == "sandbox_abi" {
        let feature_abi = if result.execution.is_some() {
            jobs::CANDIDATE_EXECUTION_ABI
        } else {
            result.abi.as_str()
        };
        let mut wrapper = json!({
            "schema":1,
            "source_sha256":result.source_sha256,
            "params_sha256":result.params_sha256,
            "feature_abi":feature_abi,
            "promotion_authorized":false,
            "scorer_replay":result.report,
            "report_hash":result.report.report_hash,
            "dataset_hash":result.dataset_hash,
            "valid_snapshots":result.report.valid_snapshots,
            "failed_snapshots":result.report.failed_snapshots,
            "min_valid_snapshots":result.report.min_valid_snapshots,
            "result_kind":result.result_kind,
            "evidence_scope":result.evidence_scope,
            "partial_evidence":result.partial_evidence,
            "qualified":result.qualified,
            "analysis_status":result.analysis_status,
            "unit_proof":result.unit_proof,
            "unit_proof_sha256":result.unit_proof_sha256,
            "result_produced_at_ms":result.result_produced_at_ms,
            "advisory":result.advisory,
            "gaps":result.gaps,
            "activation_authorized":false,
            "edge_qualified":false
        });
        if let Some(execution) = &result.execution {
            wrapper["execution_replay"] = json!({
                "result_kind":execution.result_kind,
                "execution_status":execution.execution_status,
                "execution_spec_sha256":execution.execution_spec_sha256,
                "manifest_sha256":execution.manifest_sha256,
                "events_sha256":execution.events_sha256,
                "action_tape_sha256":execution.action_tape_sha256,
                "scored_signals_sha256":execution.scoring.scored_signals_sha256,
                "report_sha256":execution.report_sha256,
                "engine_sha256":execution.engine_sha256,
                "engine_adapter_sha256":execution.shared_engine_adapter_sha256,
                "execution_adapter_sha256":execution.adapter_sha256,
                "summary":execution.summary,
                "cost_stress":execution.cost_stress.iter().map(|stress| json!({
                    "costs_sha256":stress.costs_sha256,
                    "params_sha256":stress.params_sha256,
                    "report_sha256":stress.report_sha256,
                    "summary":stress.summary
                })).collect::<Vec<_>>(),
                "retrospective":true,
                "prospective":false,
                "promotion_authorized":false,
                "activation_authorized":false,
                "edge_qualified":false
            });
            wrapper["execution_status"] = json!(execution.execution_status);
            wrapper["execution_spec_sha256"] = json!(execution.execution_spec_sha256);
            wrapper["execution_manifest_sha256"] = json!(execution.manifest_sha256);
            wrapper["execution_events_sha256"] = json!(execution.events_sha256);
            wrapper["action_tape_sha256"] = json!(execution.action_tape_sha256);
            wrapper["scored_signals_sha256"] = json!(execution.scoring.scored_signals_sha256);
            wrapper["execution_report_sha256"] = json!(execution.report_sha256);
            wrapper["retrospective"] = json!(true);
        }
        experiment_ledger::lifecycle::record_replay(&c.directory, &id, &wrapper, now)?;
        state = "replayed".into();
    }
    // An executable replay is retrospective evidence. It is never opened as a
    // prospective forward shadow automatically; that needs a separate owner
    // decision and is not part of this result.
    if result.execution.is_none()
        && state == "replayed"
        && result.report.qualified()
        && let Some(last) = result.report.ticks.last()
    {
        {
            let marks = last
                .candidate_top_n
                .iter()
                .map(|key| {
                    json!({
                        "key":key,
                        "entry_ms":last.observed_ms,
                        "due_ms":last.observed_ms.saturating_add(task.spec.horizon_minutes.saturating_mul(60_000))
                    })
                })
                .collect::<Vec<_>>();
            if !marks.is_empty() {
                experiment_ledger::lifecycle::open_shadow(&c.directory, &id, &json!(marks), now)?;
                state = "forward_shadow".into();
            }
        }
    }
    Ok(json!({
        "status":"bound",
        "candidate_id":id,
        "state":state,
        "activation_authorized":false,
        "edge_qualified":false
    }))
}

/// Verified calendar walkback for the archive manifest's unused
/// `previous_session` field. The action engine only uses the session window, so
/// a missing calendar falls back to a calendar day and the manifest provenance
/// records that it was not verified as a trading day.
/// Current reviewed candidate runtime binding. Cross-session continuation and
/// observation admission compare this with the recorded source runtime; a
/// changed helper, unit policy, scorer or sandbox is an explicit refusal.
pub(crate) fn current_candidate_runtime(
    cfg: &PlannerConfig,
) -> Result<jobs::CandidateRuntimeBinding> {
    let candidate = cfg
        .research_tools
        .as_ref()
        .and_then(|settings| settings.candidate_compute.as_ref())
        .filter(|candidate| candidate.enabled)
        .ok_or("candidate_compute_disabled")?;
    let helper_sha256 = helper_content_sha256(candidate).map_err(|reason| reason.to_string())?;
    let unit_policy =
        candidate_unit_policy(candidate, &helper_sha256).map_err(|reason| reason.to_string())?;
    Ok(candidate_runtime_binding(&helper_sha256, &unit_policy))
}

pub(crate) fn candidate_previous_trading_session(c: &Config) -> Option<chrono::NaiveDate> {
    let mut day = c.session.pred_opt()?;
    for _ in 0..10 {
        match c.is_trading_day(day) {
            Ok(true) => return Some(day),
            Ok(false) => day = day.pred_opt()?,
            Err(_) => return None,
        }
    }
    None
}

/// Freeze and admit one prospective observation over a verified original
/// candidate. The original supplies source/params/features/result provenance;
/// the model declares the observation units, research capital, costs, holding
/// and window. The snapshot, research quote tape and study spec are frozen
/// before admission and run through the same queue/lease/helper/unit.
#[allow(clippy::too_many_arguments)]
fn candidate_observation_submit(
    c: &Config,
    cfg: &PlannerConfig,
    model_id: &str,
    model_revision: u64,
    parent_turn_id: &str,
    args: CandidateSubmitArgs,
    now: i64,
    owner_evidence: Option<jobs::OwnerEvidence>,
) -> Result<Value> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    settings.validate()?;
    let candidate = settings
        .candidate_compute
        .as_ref()
        .filter(|candidate| candidate.enabled)
        .ok_or("candidate_compute_disabled")?;
    let observation = candidate
        .observation
        .as_ref()
        .filter(|observation| observation.enabled)
        .ok_or("candidate_observation_disabled")?;
    observation.validate()?;
    if settings.admission_limits.is_none() {
        return Ok(not_dispatched("research_compute_policy_required"));
    }
    let Some(obs) = args.observation.as_ref() else {
        return Ok(not_dispatched("candidate_observation_args_missing"));
    };
    if args.parent_task_id.is_some() || args.parent_ref.is_some() || args.execution.is_some() {
        return Ok(not_dispatched(
            "candidate_observation_parent_or_execution_invalid",
        ));
    }
    if now < 0
        || model_id.trim().is_empty()
        || model_id.len() > 128
        || parent_turn_id.trim().is_empty()
        || parent_turn_id.len() > 256
    {
        return Err("candidate_task_args_invalid".into());
    }
    if args.task_key.is_empty()
        || args.task_key.len() > 48
        || !args
            .task_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Ok(not_dispatched("candidate_task_key_invalid"));
    }
    if args.hypothesis.trim().is_empty()
        || args.hypothesis.len() > 1024
        || args.hypothesis.chars().any(char::is_control)
    {
        return Ok(not_dispatched("candidate_hypothesis_invalid"));
    }
    if super::super::super::transcript::redact(&json!(args.hypothesis)).as_str()
        != Some(args.hypothesis.as_str())
    {
        return Ok(not_dispatched("candidate_hypothesis_secret_refused"));
    }
    if obs.source_task_id.len() != 64
        || !obs
            .source_task_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || obs.source_revision != 1
        || obs.window_from_ms < 0
        || obs.window_from_ms > now
        || obs.window_from_ms > obs.window_to_ms
        || obs.units == 0
        || obs.units > observation.max_units
        || obs.capital <= Amount::ZERO
        || obs.capital > observation.max_capital
        || obs.costs.validate().is_err()
        || obs.holding_ms < 1
        || obs.holding_ms > observation.max_holding_ms
        || obs.holding_ms % 60_000 != 0
        || !(0..=60_000).contains(&obs.max_exit_delay_ms)
        || !(0..=observation.max_decision_delay_ms).contains(&obs.decision_delay_ms)
        || obs
            .observation_projection
            .as_deref()
            .is_some_and(|projection| {
                !matches!(projection, "single_entry" | "top_n_1_single_entry")
            })
    {
        return Ok(not_dispatched("candidate_observation_args_invalid"));
    }
    let horizon_minutes = obs.holding_ms / 60_000;
    if !(1..=1440).contains(&horizon_minutes)
        || candidate.replay.horizons_minutes.len() != 1
        || candidate.replay.horizons_minutes[0] != horizon_minutes
    {
        return Ok(not_dispatched("candidate_horizon_invalid"));
    }
    if !candidate
        .datasets
        .iter()
        .any(|dataset| dataset.dataset_id == args.dataset_id)
    {
        return Ok(not_dispatched("candidate_dataset_unknown"));
    }
    let binding = book_binding(c)?;
    let session = c.session.to_string();
    let policy_sha256 = digest(&super::super::super::planner::policy_bytes(cfg)?);
    let mut db = open_queue_for_owner(settings, &c.directory.join("model"), true)?;
    let original = match jobs::load_candidate(&mut db, &obs.source_task_id, obs.source_revision) {
        Ok(task) => task,
        Err(_) => return Ok(not_dispatched("candidate_observation_source_missing")),
    };
    if original.spec.session != session || original.spec.book_binding != binding {
        return Ok(not_dispatched("candidate_observation_source_scope"));
    }
    if original.spec.purpose.is_some() {
        return Ok(not_dispatched("candidate_observation_source_purpose"));
    }
    if original.status != jobs::TaskState::Succeeded {
        return Ok(not_dispatched("candidate_observation_source_ineligible"));
    }
    match read_verified_candidate_result(&mut db, &obs.source_task_id, obs.source_revision) {
        Ok(Some(result)) if result.observation.is_none() => {}
        _ => return Ok(not_dispatched("candidate_observation_source_unverified")),
    }
    let sandbox = match candidate_sandbox_policy(candidate) {
        Ok(sandbox) => sandbox,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let helper_sha256 = match helper_content_sha256(candidate) {
        Ok(hash) => hash,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let unit_policy = match candidate_unit_policy(candidate, &helper_sha256) {
        Ok(policy) => policy,
        Err(reason) => return Ok(not_dispatched(reason)),
    };
    let runtime_binding = candidate_runtime_binding(&helper_sha256, &unit_policy);
    // The observation must run on the admitted original runtime. A changed
    // helper, unit policy, scorer, sandbox or interpreter is an explicit
    // refusal, never a silent re-execution of the old helper.
    if runtime_binding != original.spec.runtime_binding
        || sandbox != original.spec.sandbox
        || unit_policy != original.spec.unit_policy
    {
        return Ok(not_dispatched("candidate_observation_runtime_changed"));
    }
    let source =
        match jobs::read_candidate_artifact(&db, &original.spec.source_sha256, "source", 262_144) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(not_dispatched("candidate_observation_source_missing")),
        };
    let params =
        match jobs::read_candidate_artifact(&db, &original.spec.params_sha256, "params", 65_536) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(not_dispatched("candidate_observation_source_missing")),
        };
    if digest(&source) != original.spec.source_sha256
        || digest(&params) != original.spec.params_sha256
    {
        return Ok(not_dispatched("candidate_observation_source_changed"));
    }
    let previous_session = candidate_previous_trading_session(c);
    let snapshots = match super::super::super::paper_replay::scorer_snapshots_bounded(
        &c.directory,
        c.session,
        previous_session,
        candidate.replay.max_snapshots,
        candidate.replay.max_age_ms,
        candidate.replay.lookback_samples,
        now,
    ) {
        Ok(snapshots) => snapshots,
        Err(_) => return Ok(not_dispatched("candidate_dataset_unavailable")),
    };
    let selection = snapshots
        .into_iter()
        .filter(|snapshot| {
            snapshot.observed_ms >= obs.window_from_ms
                && snapshot.observed_ms <= obs.window_to_ms.min(now)
        })
        .collect::<Vec<_>>();
    if selection.len() < candidate.replay.min_valid_snapshots {
        return Ok(not_dispatched("candidate_dataset_insufficient"));
    }
    let snapshot_bytes = serde_json::to_vec(&selection)?;
    if snapshot_bytes.len() > 4 * 1024 * 1024 {
        return Ok(not_dispatched("candidate_dataset_too_large"));
    }
    let keys = selection
        .iter()
        .flat_map(|snapshot| snapshot.rows.iter().map(|row| row.key.clone()))
        .collect::<BTreeSet<_>>();
    if keys.is_empty() {
        return Ok(not_dispatched("candidate_observation_keys_missing"));
    }
    // `max_quotes` is the frozen key budget, not the quote-row limit. The
    // post-score top key is unknown at freeze time, so silently dropping
    // lexicographically later keys could hide the actual top key. An
    // over-budget key set is refused explicitly instead.
    if keys.len() > observation.max_quotes {
        return Ok(json!({
            "status":"not_dispatched",
            "reason":"candidate_observation_key_budget_exceeded",
            "keys":keys.len(),
            "max_quotes":observation.max_quotes,
            "next_condition":"reviewed_observation_key_budget"
        }));
    }
    let tape = match super::super::super::paper_replay::virtual_observation_tape_bounded(
        &c.directory,
        c.session,
        previous_session,
        &keys,
        0,
        now,
        obs.capital,
        observation.max_quote_events,
        observation.max_observations,
        observation.max_tape_bytes,
        now,
        super::super::super::paper_replay::TapeMode::EntryFreeze,
    ) {
        Ok(tape) => tape,
        Err(gap) => {
            return Ok(json!({
                "status":"not_dispatched",
                "reason":gap.reason,
                "gap":gap.gap,
                "next_condition":"new_original_quote_evidence_or_reviewed_observation_budget"
            }));
        }
    };
    let quote_budget_truncated = tape.budget_truncated;
    // FREEZE-TIME target metrics. These are recorded inside the hash-pinned
    // quote bundle so a later reader can verify the target's quote count,
    // truncation flag and declared decision delay against the ORIGINAL frozen
    // bytes, never against the current config, clock or caller target.
    let bundle = json!({
        "version":1,
        "status":"ok",
        "purpose":tape.purpose.as_str(),
        "mode":"entry_freeze",
        "archive_sha256":tape.archive_sha256,
        "universe_sha256":tape.universe_sha256,
        "receipt_ms":tape.receipt_ms,
        "budget_truncated":tape.budget_truncated,
        "coverage_gap":tape.coverage_gap,
        "manifest_sha256":tape.manifest_sha256,
        "events_sha256":tape.events_sha256,
        "target_metrics":{
            "version":1,
            "quote_keys":keys.len(),
            "quote_budget_truncated":quote_budget_truncated,
            "decision_delay_ms":obs.decision_delay_ms,
            "window_from_ms":obs.window_from_ms,
            "window_to_ms":obs.window_to_ms,
            "holding_ms":obs.holding_ms,
            "max_exit_delay_ms":obs.max_exit_delay_ms,
            "signal_ms":now
        },
        "manifest":String::from_utf8(tape.manifest.clone()).map_err(|_| "candidate_observation_tape_invalid")?,
        "events":String::from_utf8(tape.events.clone()).map_err(|_| "candidate_observation_tape_invalid")?
    });
    let bundle_bytes = serde_json::to_vec(&bundle)?;
    if bundle_bytes.len() > 2_000_000 {
        return Ok(not_dispatched("candidate_observation_tape_too_large"));
    }
    // If the original was an executable multi-entry candidate, the observation
    // is explicitly labelled a top-N-1 single-entry projection rather than
    // claiming the original rules were measured unchanged.
    let mut projection = obs
        .observation_projection
        .clone()
        .unwrap_or_else(|| "single_entry".into());
    if let Some(execution) = &original.spec.execution
        && let Ok(bytes) =
            jobs::read_candidate_artifact(&db, &execution.spec_sha256, "execution_spec", 65_536)
        && let Ok(spec) = serde_json::from_slice::<
            sadr_cli::candidate_execution::CandidateExecutionSpecV1,
        >(&bytes)
        && (spec.max_entries > 1 || spec.selection_top_n > 1)
    {
        projection = "top_n_1_single_entry".into();
    }
    let parameters = sadr_cli::strategy_actions::Parameters {
        version: 1,
        parent_candidate_id: None,
        hypothesis: "prospective single-entry observation".into(),
        training_from_ms: original.spec.windows.training_from_ms,
        training_to_ms: original.spec.windows.training_to_ms,
        selection_from_ms: obs.window_from_ms,
        selection_to_ms: obs
            .window_to_ms
            .max(now.saturating_add(obs.decision_delay_ms)),
        holding_ms: obs.holding_ms,
        max_exit_delay_ms: obs.max_exit_delay_ms,
        capital: obs.capital,
        costs: obs.costs.clone(),
    };
    let spec = sadr_cli::candidate_execution::ObservationExecutionSpecV1 {
        version: 1,
        abi: sadr_cli::candidate_execution::OBSERVATION_ABI.into(),
        parameters,
        units: obs.units,
        max_entries: 1,
        selection_top_n: 1,
        entry_price_offset_ticks: 0,
        entry_at: sadr_cli::candidate_execution::EntryAt::Signal,
        exit: sadr_cli::candidate_execution::ExitRule::Time,
        first_signal_per_key: true,
        original_strategy_measurement: false,
        observation_projection: projection,
    };
    if let Err(reason) = sadr_cli::candidate_execution::validate_observation_spec(&spec) {
        return Ok(not_dispatched(&reason));
    }
    let spec_bytes = sadr_cli::candidate_execution::artifact_bytes(&spec)?;
    let snapshot_sha256 = digest(&snapshot_bytes);
    let quotes_sha256 = digest(&bundle_bytes);
    let spec_sha256 = digest(&spec_bytes);
    let target = jobs::ObservationTargetV1 {
        version: 1,
        source_task_id: original.spec.task_id.clone(),
        source_revision: original.spec.revision,
        source_candidate_id: original.spec.candidate_id.clone(),
        source_lifecycle_candidate_id: original.spec.lifecycle_candidate_id.clone(),
        source_result_sha256: original
            .job
            .result_hash
            .clone()
            .ok_or("candidate_observation_source_unverified")?,
        source_runtime_hash: original.spec.runtime_binding.runtime_hash.clone(),
        authoring_at_ms: original.spec.created_at_ms,
        policy_sha256: policy_sha256.clone(),
        signal_ms: now,
        target_frozen_at_ms: now,
        window_from_ms: obs.window_from_ms,
        window_to_ms: obs.window_to_ms,
        holding_ms: obs.holding_ms,
        max_exit_delay_ms: obs.max_exit_delay_ms,
        decision_delay_ms: obs.decision_delay_ms,
        snapshot_sha256: snapshot_sha256.clone(),
        quotes_sha256: quotes_sha256.clone(),
        spec_sha256: spec_sha256.clone(),
        quote_keys: u32::try_from(keys.len()).unwrap_or(u32::MAX),
        quote_budget_truncated,
        runtime: jobs::ObservationRuntimeBindingV1 {
            version: 1,
            replay_abi: sadr_cli::candidate_execution::OBSERVATION_ABI.into(),
            quote_authority: "research_observation_virtual_only".into(),
            source_sha256: original.spec.source_sha256.clone(),
            params_sha256: original.spec.params_sha256.clone(),
            runtime_hash: runtime_binding.runtime_hash.clone(),
            helper_sha256: runtime_binding.helper_sha256.clone(),
            unit_policy_sha256: runtime_binding.unit_policy_sha256.clone(),
            scorer_compare_sha256: runtime_binding.scorer_compare_sha256.clone(),
            sandbox_source_sha256: runtime_binding.sandbox_source_sha256.clone(),
        },
    };
    if !target.validate() {
        return Ok(not_dispatched("candidate_observation_target_invalid"));
    }
    let content_key = jobs::candidate_observation_content_key(
        &original.spec.source_sha256,
        &original.spec.params_sha256,
        &snapshot_sha256,
        &quotes_sha256,
        &spec_sha256,
        obs.window_from_ms,
        obs.window_to_ms,
        obs.holding_ms,
        obs.max_exit_delay_ms,
        &runtime_binding.runtime_hash,
        sadr_cli::candidate_execution::OBSERVATION_ABI,
        &unit_policy.policy_sha256(),
    );
    let input_key = jobs::candidate_observation_trial_input_key(
        &original.spec.source_sha256,
        &original.spec.params_sha256,
        &snapshot_sha256,
        &quotes_sha256,
        &spec_sha256,
        &runtime_binding.runtime_hash,
        sadr_cli::candidate_execution::OBSERVATION_ABI,
    );
    let lifecycle_dataset_hash = digest(
        format!(
            "candidate_observation_dataset_v1\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}"
        )
        .as_bytes(),
    );
    let experiment = experiment_ledger::lifecycle::ExperimentSpec {
        version: 1,
        parent_candidate_id: Some(original.spec.lifecycle_candidate_id.clone()),
        session: session.clone(),
        source_sha256: original.spec.source_sha256.clone(),
        params_sha256: original.spec.params_sha256.clone(),
        dataset_hash: lifecycle_dataset_hash,
        feature_abi: sadr_cli::candidate_execution::OBSERVATION_ABI.into(),
        hypothesis: args.hypothesis.clone(),
        training_from_ms: original.spec.windows.training_from_ms,
        training_to_ms: original.spec.windows.training_to_ms,
        selection_from_ms: obs.window_from_ms,
        selection_to_ms: spec.parameters.selection_to_ms,
        holding_ms: obs.holding_ms,
        max_exit_delay_ms: obs.max_exit_delay_ms,
    };
    let lifecycle_candidate_id = experiment_ledger::lifecycle::candidate_id(&experiment);
    if experiment_ledger::lifecycle::state(&c.directory, &original.spec.lifecycle_candidate_id)?
        .is_none()
    {
        return Ok(not_dispatched(
            "candidate_observation_source_lifecycle_missing",
        ));
    }
    let task_id = jobs::candidate_task_id(
        &binding,
        &session,
        &args.task_key,
        &content_key,
        Some(original.spec.candidate_id.as_str()),
    );
    let candidate_id = jobs::candidate_version_id(
        &binding,
        &session,
        &args.task_key,
        &content_key,
        Some(original.spec.candidate_id.as_str()),
    );
    let job_id = jobs::candidate_job_id(&task_id, 1);
    let job = jobs::Spec {
        id: job_id.clone(),
        review_id: digest(format!("candidate-observation-review:{task_id}").as_bytes()),
        script_hash: original.spec.source_sha256.clone(),
        input_hash: jobs::candidate_observation_input_hash(
            &original.spec.params_sha256,
            &snapshot_sha256,
            &quotes_sha256,
            &spec_sha256,
            &runtime_binding.runtime_hash,
        ),
        runtime_hash: runtime_binding.runtime_hash.clone(),
        memory_bytes: candidate.sandbox.memory_mb * 1024 * 1024,
        output_bytes: candidate.sandbox.max_output_bytes as u64,
        wall_millis: candidate.wall_millis,
        retry_safe_compute: false,
        max_attempts: 1,
        capability: jobs::SpecCapability::CandidateSource,
    };
    let resources = jobs::ResourceCaps {
        cpu_millis: job.wall_millis,
        tasks: 1,
        workspace_bytes: 1_048_576,
        file_bytes: 65_536,
        memory_bytes: job.memory_bytes,
        output_bytes: job.output_bytes,
        wall_millis: job.wall_millis,
    };
    let expires_at_ms = now
        .checked_add(i64::try_from(settings.task_ttl_ms).map_err(|_| "candidate_task_ttl_invalid")?)
        .ok_or("candidate_task_ttl_invalid")?;
    let task = jobs::CandidateTaskV1 {
        version: 1,
        task_id: task_id.clone(),
        revision: 1,
        task_key: args.task_key.clone(),
        job_id,
        candidate_id,
        lifecycle_candidate_id,
        parent_candidate_id: Some(original.spec.candidate_id.clone()),
        parent_lifecycle_candidate_id: Some(original.spec.lifecycle_candidate_id.clone()),
        parent_task_id: None,
        parent_turn_id: parent_turn_id.to_owned(),
        session,
        book_binding: binding,
        policy_sha256,
        model_id: model_id.to_owned(),
        model_revision,
        hypothesis: args.hypothesis,
        source_sha256: original.spec.source_sha256.clone(),
        params_sha256: original.spec.params_sha256.clone(),
        dataset_id: args.dataset_id,
        dataset_hash: snapshot_sha256.clone(),
        content_key,
        input_key,
        windows: jobs::CandidateWindows {
            training_from_ms: original.spec.windows.training_from_ms,
            training_to_ms: original.spec.windows.training_to_ms,
            selection_from_ms: obs.window_from_ms,
            selection_to_ms: spec.parameters.selection_to_ms,
        },
        horizon_minutes,
        max_exit_delay_ms: obs.max_exit_delay_ms,
        abi: CANDIDATE_ABI.into(),
        owner_evidence,
        execution: None,
        purpose: Some(jobs::CandidatePurposeV1::Observe {
            target: Box::new(target.clone()),
        }),
        parent_ref: None,
        runtime_binding,
        sandbox,
        unit_policy,
        resources,
        created_at_ms: now,
        expires_at_ms,
    };
    let artifacts: Vec<(&str, &[u8])> = vec![
        ("source", source.as_slice()),
        ("params", params.as_slice()),
        ("dataset", snapshot_bytes.as_slice()),
        ("observation_spec", spec_bytes.as_slice()),
        ("observation_quotes", bundle_bytes.as_slice()),
    ];
    let record = match jobs::enqueue_candidate_bounded(
        &mut db,
        &job,
        &task,
        &artifacts,
        now,
        settings.admission_limits.as_ref(),
    )
    .map_err(|_| "candidate_task_enqueue_refused")?
    {
        jobs::CandidateAdmission::Admitted(record) => *record,
        jobs::CandidateAdmission::Deferred { reason, trials } => {
            return Ok(json!({
                "status":"not_dispatched",
                "reason":reason,
                "trials":trials,
                "task_id":task.task_id,
                "revision":task.revision,
                "next_condition":if reason == "research_session_compute_budget_exhausted" {"next_authorized_session"} else {"new_candidate_input_or_reviewed_compute_policy"}
            }));
        }
    };
    let lifecycle = bridge_candidate_admission(c, &record, now).unwrap_or_else(|_| {
        json!({
            "status":"unavailable",
            "reason":"candidate_lifecycle_unavailable",
            "repair_required":true,
            "activation_authorized":false
        })
    });
    Ok(json!({
        "status":"queued",
        "purpose":"observe",
        "execution":"not_started",
        "task":candidate_summary(&record),
        "lifecycle":lifecycle,
        "observation_binding":{
            "version":target.version,
            "source_task_id":target.source_task_id,
            "source_revision":target.source_revision,
            "source_candidate_id":target.source_candidate_id,
            "source_result_sha256":target.source_result_sha256,
            "snapshot_sha256":target.snapshot_sha256,
            "quotes_sha256":target.quotes_sha256,
            "spec_sha256":target.spec_sha256,
            "quote_keys":target.quote_keys,
            "quote_observations":tape.observations,
            "quote_receipts":tape.receipts,
            "quote_budget_truncated":target.quote_budget_truncated,
            "quote_authority":"research_observation_virtual_only",
            "original_strategy_measurement":false,
            "observation_projection":spec.observation_projection
        }
    }))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_submit(
    c: &Config,
    cfg: &PlannerConfig,
    model_id: &str,
    model_revision: u64,
    parent_turn_id: &str,
    args: CandidateSubmitArgs,
    now: i64,
    owner_evidence: Option<jobs::OwnerEvidence>,
) -> Result<Value> {
    if args.observation.is_some() {
        return candidate_observation_submit(
            c,
            cfg,
            model_id,
            model_revision,
            parent_turn_id,
            args,
            now,
            owner_evidence,
        );
    }
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    settings.validate()?;
    let candidate = settings
        .candidate_compute
        .as_ref()
        .filter(|candidate| candidate.enabled)
        .ok_or("candidate_compute_disabled")?;
    if settings.admission_limits.is_none() {
        return Ok(not_dispatched("research_compute_policy_required"));
    }
    if args.parent_ref.is_some() && args.parent_task_id.is_some() {
        return Ok(not_dispatched("candidate_parent_conflict"));
    }
    if now < 0
        || model_id.trim().is_empty()
        || model_id.len() > 128
        || parent_turn_id.trim().is_empty()
        || parent_turn_id.len() > 256
    {
        return Err("candidate_task_args_invalid".into());
    }
    if args.task_key.is_empty()
        || args.task_key.len() > 48
        || !args
            .task_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Ok(not_dispatched("candidate_task_key_invalid"));
    }
    if args.hypothesis.trim().is_empty()
        || args.hypothesis.len() > 1024
        || args.hypothesis.chars().any(char::is_control)
    {
        return Ok(not_dispatched("candidate_hypothesis_invalid"));
    }
    if super::super::super::transcript::redact(&json!(args.hypothesis)).as_str()
        != Some(args.hypothesis.as_str())
    {
        return Ok(not_dispatched("candidate_hypothesis_secret_refused"));
    }
    if args.source.is_empty()
        || args.source.len() > candidate.max_source_bytes as usize
        || args
            .source
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t' | '\r'))
    {
        return Ok(not_dispatched("candidate_source_invalid"));
    }
    if super::super::super::transcript::redact(&json!(args.source)).as_str()
        != Some(args.source.as_str())
    {
        return Ok(not_dispatched("candidate_source_secret_refused"));
    }
    if !args.params.is_object() {
        return Ok(not_dispatched("candidate_params_invalid"));
    }
    if CANDIDATE_RESERVED_PARAMS
        .iter()
        .any(|key| args.params.get(*key).is_some())
    {
        return Ok(not_dispatched("candidate_params_reserved"));
    }
    if super::super::super::transcript::redact(&args.params) != args.params {
        return Ok(not_dispatched("candidate_params_secret_refused"));
    }
    let params_bytes = serde_json::to_vec(&args.params)?;
    if params_bytes.len() > candidate.max_params_bytes as usize {
        return Ok(not_dispatched("candidate_params_invalid"));
    }
    if !candidate
        .datasets
        .iter()
        .any(|dataset| dataset.dataset_id == args.dataset_id)
    {
        return Ok(not_dispatched("candidate_dataset_unknown"));
    }
    let windows = jobs::CandidateWindows {
        training_from_ms: args.training_from_ms,
        training_to_ms: args.training_to_ms,
        selection_from_ms: args.selection_from_ms,
        selection_to_ms: args.selection_to_ms,
    };
    if windows.training_from_ms < 0
        || windows.training_from_ms > windows.training_to_ms
        || windows.training_to_ms >= windows.selection_from_ms
        || windows.selection_from_ms > windows.selection_to_ms
    {
        return Ok(not_dispatched("candidate_windows_invalid"));
    }
    if !(1..=1440).contains(&args.horizon_minutes) {
        return Ok(not_dispatched("candidate_horizon_invalid"));
    }
    if candidate.replay.horizons_minutes.len() != 1
        || candidate.replay.horizons_minutes[0] != args.horizon_minutes
    {
        return Ok(not_dispatched("candidate_horizon_unsupported"));
    }
    if args.parent_task_id.as_deref().is_some_and(|parent| {
        parent.len() != 64 || !parent.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        return Ok(not_dispatched("candidate_parent_invalid"));
    }
    // Optional reviewed after-cost cash replay. The capability is host policy;
    // a request without the grant is refused before any artifact or task row.
    let execution_settings = candidate
        .execution
        .as_ref()
        .filter(|execution| execution.enabled);
    if args.execution.is_some() && execution_settings.is_none() {
        return Ok(not_dispatched("candidate_execution_disabled"));
    }
    if let (Some(spec), Some(execution)) = (args.execution.as_ref(), execution_settings) {
        execution.validate()?;
        if let Err(reason) = sadr_cli::candidate_execution::validate_spec(spec) {
            return Ok(not_dispatched(&reason));
        }
        if spec.parameters.training_from_ms != windows.training_from_ms
            || spec.parameters.training_to_ms != windows.training_to_ms
            || spec.parameters.selection_from_ms != windows.selection_from_ms
            || spec.parameters.selection_to_ms != windows.selection_to_ms
            || spec.parameters.holding_ms != args.horizon_minutes.saturating_mul(60_000)
        {
            return Ok(not_dispatched("candidate_execution_windows_invalid"));
        }
        if spec.parameters.hypothesis != args.hypothesis {
            return Ok(not_dispatched("candidate_execution_parameters_invalid"));
        }
        if spec.parameters.capital > execution.max_capital
            || spec.max_entries > execution.max_entries
            || spec.cost_stress.len() > execution.max_cost_stress as usize
            || spec.selection_top_n as usize > candidate.replay.top_n
        {
            return Ok(not_dispatched("candidate_execution_policy_exceeded"));
        }
    }
    let previous_session = candidate_previous_trading_session(c);
    let snapshots = match super::super::super::paper_replay::scorer_snapshots_bounded(
        &c.directory,
        c.session,
        previous_session,
        candidate.replay.max_snapshots,
        candidate.replay.max_age_ms,
        candidate.replay.lookback_samples,
        now,
    ) {
        Ok(snapshots) => snapshots,
        Err(_) => return Ok(not_dispatched("candidate_dataset_unavailable")),
    };
    let selection = snapshots
        .into_iter()
        .filter(|snapshot| {
            snapshot.observed_ms >= windows.selection_from_ms
                && snapshot.observed_ms <= windows.selection_to_ms
        })
        .collect::<Vec<_>>();
    if selection.len() < candidate.replay.min_valid_snapshots {
        return Ok(not_dispatched("candidate_dataset_insufficient"));
    }
    let dataset_bytes = serde_json::to_vec(&selection)?;
    if dataset_bytes.len() > 4 * 1024 * 1024 {
        return Ok(not_dispatched("candidate_dataset_too_large"));
    }
    // Freeze the executable quote tape from the same read-only original-receipt
    // reader as the scorer dataset. Only data observed at or before this
    // submission enters the tape; a later exit quote is never fetched.
    let execution_tape = match (args.execution.as_ref(), execution_settings) {
        (Some(spec), Some(execution)) => {
            match super::super::super::paper_replay::execution_tape_bounded(
                &c.directory,
                c.session,
                previous_session,
                spec.parameters.capital,
                execution.max_quote_events,
                execution.max_observations,
                execution.max_tape_bytes,
                now,
            ) {
                Ok(tape) => Some(tape),
                Err(reason) => return Ok(not_dispatched(&reason)),
            }
        }
        _ => None,
    };
    let binding = book_binding(c)?;
    let policy_sha256 = digest(&super::super::super::planner::policy_bytes(cfg)?);
    let session = c.session.to_string();
    let source_sha256 = digest(args.source.as_bytes());
    let params_sha256 = digest(&params_bytes);
    let dataset_hash = digest(&dataset_bytes);
    let sandbox = match candidate_sandbox_policy(candidate) {
        Ok(sandbox) => sandbox,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let helper_sha256 = match helper_content_sha256(candidate) {
        Ok(hash) => hash,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let unit_policy = match candidate_unit_policy(candidate, &helper_sha256) {
        Ok(policy) => policy,
        Err(reason) => return Ok(not_dispatched(reason)),
    };
    let runtime_binding = candidate_runtime_binding(&helper_sha256, &unit_policy);
    // Execution-only identity: the recorded runtime binding plus the actual
    // pinned Python/bwrap bytes and every containment cap. A ranking task keeps
    // its historical bytes; an executable task after a runtime/cap change is a
    // new content version and a new input allowance.
    let execution_runtime_sha256 =
        jobs::candidate_execution_runtime_hash(&runtime_binding, &sandbox);
    let execution_binding = match (args.execution.as_ref(), execution_tape.as_ref()) {
        (Some(spec), Some(tape)) => {
            let spec_bytes = sadr_cli::candidate_execution::artifact_bytes(spec)?;
            Some((
                jobs::CandidateExecutionBinding {
                    version: 1,
                    abi: sadr_cli::candidate_execution::EXECUTION_ABI.into(),
                    spec_sha256: digest(&spec_bytes),
                    manifest_sha256: tape.manifest_sha256.clone(),
                    events_sha256: tape.events_sha256.clone(),
                    rules_sha256: Some(sadr_cli::candidate_execution::execution_rules_sha256(
                        spec,
                    )?),
                },
                spec_bytes,
            ))
        }
        _ => None,
    };
    let (content_key, input_key) = match &execution_binding {
        Some((execution, _)) => (
            jobs::candidate_execution_content_key(
                &source_sha256,
                &params_sha256,
                &dataset_hash,
                &windows,
                args.horizon_minutes,
                CANDIDATE_ABI,
                &unit_policy.policy_sha256(),
                execution,
                &execution_runtime_sha256,
            ),
            jobs::candidate_execution_trial_input_key(
                &source_sha256,
                &params_sha256,
                &dataset_hash,
                CANDIDATE_ABI,
                execution,
                &execution_runtime_sha256,
            ),
        ),
        None => (
            jobs::candidate_content_key(
                &source_sha256,
                &params_sha256,
                &dataset_hash,
                &windows,
                args.horizon_minutes,
                CANDIDATE_ABI,
                &unit_policy.policy_sha256(),
            ),
            jobs::candidate_trial_input_key(
                &source_sha256,
                &params_sha256,
                &dataset_hash,
                CANDIDATE_ABI,
            ),
        ),
    };
    let mut db = open_queue_for_owner(settings, &c.directory.join("model"), true)?;
    let latest = jobs::latest_candidate_for_key(&mut db, &session, &binding, &args.task_key)
        .map_err(|_| "candidate_task_state_unavailable")?;
    let resolved_parent_ref = match args.parent_ref.as_deref() {
        Some(reference) => {
            if latest.is_some() {
                return Ok(not_dispatched("candidate_parent_ref_requires_new_chain"));
            }
            match super::carry::resolve_parent_ref(c, cfg, reference) {
                Ok(value) => Some(value),
                Err(reason) => {
                    let code = reason.to_string();
                    return Ok(not_dispatched(&code));
                }
            }
        }
        None => None,
    };
    let (parent_candidate_id, parent_lifecycle_candidate_id, parent_task_id) =
        if let Some(resolved) = &resolved_parent_ref {
            (
                resolved["candidate_id"].as_str().map(str::to_owned),
                resolved["lifecycle_candidate_id"]
                    .as_str()
                    .map(str::to_owned),
                None,
            )
        } else {
            match args.parent_task_id.as_deref() {
                Some(parent_task_id) => {
                    let Some(previous) = latest else {
                        return Ok(not_dispatched("candidate_parent_missing"));
                    };
                    if previous.spec.task_id != parent_task_id {
                        return Ok(json!({
                            "status":"not_dispatched",
                            "reason":"candidate_parent_not_latest",
                            "latest_task_id":previous.spec.task_id,
                            "latest_candidate_id":previous.spec.candidate_id,
                            "next_condition":"submit_the_latest_parent_task_id"
                        }));
                    }
                    if previous.spec.content_key == content_key {
                        return Ok(not_dispatched("candidate_task_duplicate_content"));
                    }
                    match previous.status {
                        jobs::TaskState::Pending => {
                            return Ok(not_dispatched("candidate_parent_pending"));
                        }
                        jobs::TaskState::Unknown | jobs::TaskState::CancelRequested => {
                            return Ok(not_dispatched("candidate_parent_ambiguous"));
                        }
                        _ => {}
                    }
                    (
                        Some(previous.spec.candidate_id),
                        Some(previous.spec.lifecycle_candidate_id),
                        Some(previous.spec.task_id),
                    )
                }
                None => {
                    if let Some(previous) = latest {
                        if previous.spec.content_key == content_key {
                            let lifecycle =
                                lifecycle_view(c, &previous.spec.lifecycle_candidate_id);
                            return Ok(json!({
                                "status":"existing",
                                "execution":"not_started",
                                "task":candidate_summary(&previous),
                                "lifecycle":lifecycle
                            }));
                        }
                        if previous.status == jobs::TaskState::Pending {
                            return Ok(not_dispatched("candidate_parent_pending"));
                        }
                        if matches!(
                            previous.status,
                            jobs::TaskState::Unknown | jobs::TaskState::CancelRequested
                        ) {
                            return Ok(not_dispatched("candidate_parent_ambiguous"));
                        }
                        return Ok(json!({
                            "status":"not_dispatched",
                            "reason":"candidate_task_identity_changed",
                            "existing_task_id":previous.spec.task_id,
                            "existing_candidate_id":previous.spec.candidate_id,
                            "next_condition":"submit_parent_task_id_for_a_new_version"
                        }));
                    }
                    (None, None, None)
                }
            }
        };
    if let Some(spec) = args.execution.as_ref()
        && spec
            .parameters
            .parent_candidate_id
            .as_deref()
            .is_some_and(|parent| Some(parent) != parent_candidate_id.as_deref())
    {
        return Ok(not_dispatched("candidate_execution_parameters_invalid"));
    }
    let task_id = jobs::candidate_task_id(
        &binding,
        &session,
        &args.task_key,
        &content_key,
        parent_candidate_id.as_deref(),
    );
    let job_id = jobs::candidate_job_id(&task_id, 1);
    let (lifecycle_dataset_hash, lifecycle_feature_abi) = lifecycle_binding(
        &dataset_hash,
        CANDIDATE_ABI,
        execution_binding.as_ref().map(|(execution, _)| execution),
    );
    // The declared exit delay is the model's own execution parameter (or the
    // historical default for a ranking-only candidate). It is part of the
    // lifecycle identity and must not be replaced by a fixed 60s.
    let declared_max_exit_delay = args
        .execution
        .as_ref()
        .map(|execution| execution.parameters.max_exit_delay_ms)
        .unwrap_or(CANDIDATE_MAX_EXIT_DELAY_MS);
    if !(0..=60_000).contains(&declared_max_exit_delay) {
        return Ok(not_dispatched("candidate_execution_windows_invalid"));
    }
    let experiment = experiment_ledger::lifecycle::ExperimentSpec {
        version: 1,
        parent_candidate_id: parent_lifecycle_candidate_id.clone(),
        session: session.clone(),
        source_sha256: source_sha256.clone(),
        params_sha256: params_sha256.clone(),
        dataset_hash: lifecycle_dataset_hash,
        feature_abi: lifecycle_feature_abi,
        hypothesis: args.hypothesis.clone(),
        training_from_ms: windows.training_from_ms,
        training_to_ms: windows.training_to_ms,
        selection_from_ms: windows.selection_from_ms,
        selection_to_ms: windows.selection_to_ms,
        holding_ms: args.horizon_minutes.saturating_mul(60_000),
        max_exit_delay_ms: declared_max_exit_delay,
    };
    let lifecycle_candidate_id = experiment_ledger::lifecycle::candidate_id(&experiment);
    if let Some(parent_lifecycle) = &parent_lifecycle_candidate_id {
        if let Some(reference) = args.parent_ref.as_deref() {
            // A cross-session parent lives in the VERIFIED source ledger. The
            // successor never requires (or copies) a local row for it.
            if let Err(reason) = super::carry::verify_source_lifecycle(c, reference) {
                return Ok(not_dispatched(&reason.to_string()));
            }
        } else if experiment_ledger::lifecycle::state(&c.directory, parent_lifecycle)?.is_none() {
            return Ok(not_dispatched("candidate_parent_lifecycle_missing"));
        }
    }
    let job = jobs::Spec {
        id: job_id.clone(),
        review_id: digest(format!("candidate-review:{task_id}").as_bytes()),
        script_hash: source_sha256.clone(),
        input_hash: match &execution_binding {
            Some((execution, _)) => jobs::candidate_execution_input_hash(
                &params_sha256,
                &dataset_hash,
                execution,
                &execution_runtime_sha256,
            ),
            None => jobs::candidate_input_hash(&params_sha256, &dataset_hash),
        },
        runtime_hash: runtime_binding.runtime_hash.clone(),
        memory_bytes: candidate.sandbox.memory_mb * 1024 * 1024,
        output_bytes: candidate.sandbox.max_output_bytes as u64,
        wall_millis: candidate.wall_millis,
        retry_safe_compute: false,
        max_attempts: 1,
        capability: jobs::SpecCapability::CandidateSource,
    };
    let resources = jobs::ResourceCaps {
        cpu_millis: job.wall_millis,
        tasks: 1,
        workspace_bytes: 1_048_576,
        file_bytes: 65_536,
        memory_bytes: job.memory_bytes,
        output_bytes: job.output_bytes,
        wall_millis: job.wall_millis,
    };
    let expires_at_ms = now
        .checked_add(i64::try_from(settings.task_ttl_ms).map_err(|_| "candidate_task_ttl_invalid")?)
        .ok_or("candidate_task_ttl_invalid")?;
    let task = jobs::CandidateTaskV1 {
        version: 1,
        task_id: task_id.clone(),
        revision: 1,
        task_key: args.task_key.clone(),
        job_id,
        candidate_id: jobs::candidate_version_id(
            &binding,
            &session,
            &args.task_key,
            &content_key,
            parent_candidate_id.as_deref(),
        ),
        lifecycle_candidate_id,
        parent_candidate_id,
        parent_lifecycle_candidate_id,
        parent_task_id,
        parent_turn_id: parent_turn_id.to_owned(),
        session,
        book_binding: binding,
        policy_sha256,
        model_id: model_id.to_owned(),
        model_revision,
        hypothesis: args.hypothesis,
        source_sha256,
        params_sha256,
        dataset_id: args.dataset_id,
        dataset_hash,
        content_key,
        input_key,
        windows,
        horizon_minutes: args.horizon_minutes,
        max_exit_delay_ms: declared_max_exit_delay,
        abi: CANDIDATE_ABI.into(),
        owner_evidence,
        execution: execution_binding
            .as_ref()
            .map(|(execution, _)| execution.clone()),
        purpose: None,
        parent_ref: args.parent_ref.as_deref().cloned(),
        runtime_binding,
        sandbox,
        unit_policy,
        resources,
        created_at_ms: now,
        expires_at_ms,
    };
    let mut artifacts: Vec<(&str, &[u8])> = vec![
        ("source", args.source.as_bytes()),
        ("params", params_bytes.as_slice()),
        ("dataset", dataset_bytes.as_slice()),
    ];
    if let (Some((_, spec_bytes)), Some(tape)) =
        (execution_binding.as_ref(), execution_tape.as_ref())
    {
        artifacts.push(("execution_spec", spec_bytes.as_slice()));
        artifacts.push(("execution_manifest", tape.manifest.as_slice()));
        artifacts.push(("execution_events", tape.events.as_slice()));
    }
    let record = match jobs::enqueue_candidate_bounded(
        &mut db,
        &job,
        &task,
        &artifacts,
        now,
        settings.admission_limits.as_ref(),
    )
    .map_err(|_| "candidate_task_enqueue_refused")?
    {
        jobs::CandidateAdmission::Admitted(record) => *record,
        jobs::CandidateAdmission::Deferred { reason, trials } => {
            return Ok(json!({
                "status":"not_dispatched",
                "reason":reason,
                "trials":trials,
                "task_id":task.task_id,
                "revision":task.revision,
                "next_condition":if reason == "research_session_compute_budget_exhausted" {"next_authorized_session"} else {"new_candidate_input_or_reviewed_compute_policy"}
            }));
        }
    };
    let lifecycle = bridge_candidate_admission(c, &record, now).unwrap_or_else(|_| {
        json!({
            "status":"unavailable",
            "reason":"candidate_lifecycle_unavailable",
            "repair_required":true,
            "activation_authorized":false
        })
    });
    let mut response = json!({
        "status":"queued",
        "execution":"not_started",
        "task":candidate_summary(&record),
        "lifecycle":lifecycle
    });
    if let (Some((binding, _)), Some(tape)) = (execution_binding.as_ref(), execution_tape.as_ref())
    {
        response["execution_binding"] = json!({
            "version":binding.version,
            "abi":binding.abi,
            "spec_sha256":binding.spec_sha256,
            "manifest_sha256":binding.manifest_sha256,
            "events_sha256":binding.events_sha256,
            "protection_receipts":tape.protection_receipts,
            "protection_observations":tape.protection_observations,
            "screen_receipts":tape.screen_receipts
        });
    }
    Ok(response)
}

fn candidate_load_error(db: &Connection, error: jobs::Error) -> &'static str {
    if error == jobs::Error::Invalid && jobs::candidate_requires_migration(db).unwrap_or(false) {
        "candidate_legacy_schema_migration_required"
    } else {
        "candidate_task_unavailable"
    }
}

fn candidate_task(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<(Connection, jobs::CandidateTaskRecord)> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    let mut db = open_queue_for_owner(settings, &c.directory.join("model"), false)?;
    let record = jobs::load_candidate(&mut db, task_id, revision)
        .map_err(|error| candidate_load_error(&db, error))?;
    if record.spec.session != c.session.to_string() || record.spec.book_binding != book_binding(c)?
    {
        return Err("candidate_task_unavailable".into());
    }
    Ok((db, record))
}

pub(crate) fn candidate_status(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    _now: i64,
) -> Result<Value> {
    let (mut db, record) = candidate_task(c, cfg, task_id, revision)?;
    let lifecycle = reconcile_candidate(c, cfg, &mut db, &record)
        .map_err(|_| "candidate_lifecycle_unavailable")?;
    let mut response = json!({"task":candidate_summary(&record),"lifecycle":lifecycle});
    // An Observation task is a legitimate observation EPISODE, not a broken
    // authoring parent. Its own exact `source_observation_ref` is the
    // applicable provenance, so the authoring-parent status is NOT APPLICABLE
    // and the otherwise-valid context stays COMPLETE. This matches
    // `carry_state` and `resolve_one`.
    if record.spec.purpose.is_some() {
        response["parent_ref_status"] = json!("not_applicable");
        response["parent_ref_reason"] = json!("observation_parent_has_source_observation_ref");
        response["context_status"] = json!("complete");
        return Ok(response);
    }
    match super::carry::host_parent_ref(c, cfg, &mut db, &record) {
        Ok(reference) => {
            response["parent_ref"] = reference;
            response["parent_ref_status"] = json!("verified");
        }
        Err(error) => {
            response["parent_ref_status"] = json!("unavailable");
            response["parent_ref_reason"] = json!(bounded_context_reason(error.as_ref()));
            response["context_status"] = json!("partial");
        }
    }
    Ok(response)
}

/// Explicit, host-built read-only selector for one prospective-observation
/// EPISODE. It is distinct from the authoring-parent ref: the observation task
/// is O, while the authoring candidate is A. A query by this ref returns the
/// verified continuation recorded for EXACTLY this O, never an arbitrary
/// sibling observation of the same A.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceObservationRefV1 {
    pub version: u32,
    pub source_session: String,
    pub observation_task_id: String,
    pub observation_revision: u32,
    pub observation_job_id: String,
    pub observation_lifecycle_candidate_id: String,
    pub observation_result_sha256: Option<String>,
    pub observation_result_status: String,
    pub entry_sha256: Option<String>,
    pub authoring_task_id: String,
    pub authoring_parent_lifecycle_candidate_id: String,
    pub read_only: bool,
    pub activation_authorized: bool,
}

impl SourceObservationRefV1 {
    pub(crate) fn validate(&self) -> bool {
        let hash = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        self.version == 1
            && chrono::NaiveDate::parse_from_str(&self.source_session, "%Y-%m-%d").is_ok()
            && hash(&self.observation_task_id)
            && self.observation_revision == 1
            && hash(&self.observation_job_id)
            && hash(&self.observation_lifecycle_candidate_id)
            && match &self.observation_result_sha256 {
                Some(value) => hash(value) && self.observation_result_status == "verified",
                None => self.observation_result_status == "unavailable",
            }
            && self.entry_sha256.as_deref().is_none_or(hash)
            && hash(&self.authoring_task_id)
            && hash(&self.authoring_parent_lifecycle_candidate_id)
            && self.read_only
            && !self.activation_authorized
    }
}

pub(crate) fn source_observation_ref_from_task(
    record: &jobs::CandidateTaskRecord,
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
) -> Result<Value> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &record.spec.purpose else {
        return Err("candidate_observation_target_missing".into());
    };
    let entry_sha256 = score
        .entry
        .as_ref()
        .map(|entry| serde_json::to_vec(entry).map(|bytes| digest(&bytes)))
        .transpose()?;
    let reference = SourceObservationRefV1 {
        version: 1,
        source_session: record.spec.session.clone(),
        observation_task_id: record.spec.task_id.clone(),
        observation_revision: record.spec.revision,
        observation_job_id: record.spec.job_id.clone(),
        observation_lifecycle_candidate_id: record.spec.lifecycle_candidate_id.clone(),
        observation_result_sha256: record.job.result_hash.clone(),
        observation_result_status: if record.job.result_hash.is_some() {
            "verified".into()
        } else {
            "unavailable".into()
        },
        entry_sha256,
        authoring_task_id: target.source_task_id.clone(),
        authoring_parent_lifecycle_candidate_id: target.source_lifecycle_candidate_id.clone(),
        read_only: true,
        activation_authorized: false,
    };
    if !reference.validate() {
        return Err("candidate_observation_ref_invalid".into());
    }
    Ok(serde_json::to_value(reference)?)
}

fn source_observation_ref(
    record: &jobs::CandidateTaskRecord,
    result: &CandidateResultV1,
) -> Result<Value> {
    let score = result
        .observation
        .as_ref()
        .ok_or("candidate_observation_score_missing")?;
    source_observation_ref_from_task(record, score)
}

/// Resolve one EXACT source-observation ref read-only through the verified
/// carry chain. It never opens an arbitrary source path, never launches or
/// replays work and never attaches a sibling observation's receipt.
pub(crate) fn candidate_result_for_observation_ref(
    c: &Config,
    cfg: &PlannerConfig,
    reference: &Value,
    now: i64,
) -> Result<Value> {
    // The explicit source-observation selector is available only when the
    // reviewed observation capability is configured. Omission keeps every
    // historical tool call/schema byte unchanged.
    let observation_enabled = cfg
        .research_tools
        .as_ref()
        .and_then(|settings| settings.candidate_compute.as_ref())
        .and_then(|candidate| candidate.observation.as_ref())
        .is_some_and(|observation| observation.enabled);
    if !observation_enabled {
        return Err("candidate_observation_disabled".into());
    }
    let reference: SourceObservationRefV1 = serde_json::from_value(reference.clone())
        .map_err(|_| "candidate_observation_ref_invalid")?;
    if !reference.validate() {
        return Err("candidate_observation_ref_invalid".into());
    }
    let chain = super::carry::predecessor_chain_verified_immutable(c)?;
    super::continuation::verify_lineage_prefix(c, cfg, &chain)?;
    let Some((_, source_directory, _)) = chain
        .iter()
        .find(|(session, _, _)| session == &reference.source_session)
    else {
        return Err("candidate_observation_ref_unavailable".into());
    };
    let mut queue =
        super::carry::open_original_queue_read_only_strict(c, cfg, &reference.source_session)?;
    let task = jobs::load_candidate(
        &mut queue,
        &reference.observation_task_id,
        reference.observation_revision,
    )
    .map_err(|_| "candidate_observation_ref_unavailable")?;
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.spec.purpose else {
        return Err("candidate_observation_ref_not_observation".into());
    };
    if task.spec.session != reference.source_session
        || task.spec.task_id != reference.observation_task_id
        || task.spec.revision != reference.observation_revision
        || task.spec.job_id != reference.observation_job_id
        || task.spec.lifecycle_candidate_id != reference.observation_lifecycle_candidate_id
        || target.source_task_id != reference.authoring_task_id
        || target.source_lifecycle_candidate_id != reference.authoring_parent_lifecycle_candidate_id
        || task.job.result_hash != reference.observation_result_sha256
    {
        return Err("candidate_observation_ref_identity_mismatch".into());
    }
    verify_task_book_binding(c, &chain, &task)?;
    let result = read_verified_candidate_result(
        &mut queue,
        &reference.observation_task_id,
        reference.observation_revision,
    )?
    .ok_or("candidate_observation_ref_result_unavailable")?;
    let score = result
        .observation
        .as_ref()
        .ok_or("candidate_observation_ref_result_unavailable")?;
    let entry_sha256 = score
        .entry
        .as_ref()
        .map(|entry| serde_json::to_vec(entry).map(|bytes| digest(&bytes)))
        .transpose()?;
    if entry_sha256 != reference.entry_sha256 {
        return Err("candidate_observation_ref_identity_mismatch".into());
    }
    let continuations = super::continuation::verified_continuations_from_chain(c, cfg, &chain)?;
    let continuation = continuations.iter().find(|continuation| {
        continuation["source_lifecycle_candidate_id"]
            == reference.observation_lifecycle_candidate_id.as_str()
    });
    // The native selector resolves the OBSERVATION O, but its provenance is the
    // ORIGINAL AUTHORING task A. Load and fully verify A against the target pins
    // before publishing any source result or parent ref. A tampered A result or
    // a target pointing at another Observation is UNVERIFIED context, never the
    // pinned authoring parent.
    let authoring =
        load_verified_authoring_parent(c, &chain, &mut queue, &reference.source_session, target);
    let (authoring_result, source_result_status, source_result_reason) = match authoring {
        Ok(authoring) => (Some(candidate_result_view(&authoring)), "verified", None),
        Err(error) => {
            let code = bounded_context_reason(error.as_ref());
            if code == "candidate_result_unverified" {
                (None, "unavailable", Some(code))
            } else {
                (None, "unverified", Some(code))
            }
        }
    };
    let context_status = if source_result_status == "verified" {
        "complete"
    } else {
        "partial"
    };
    let authoring_parent_ref = if source_result_status == "verified" {
        json!({
            "source_session":reference.source_session,
            "task_id":target.source_task_id,
            "revision":target.source_revision,
            "candidate_id":target.source_candidate_id,
            "lifecycle_candidate_id":target.source_lifecycle_candidate_id,
            "result_sha256":target.source_result_sha256,
            "runtime_hash":target.source_runtime_hash,
            "policy_sha256":target.policy_sha256,
            "read_only":true,
            "activation_authorized":false
        })
    } else {
        Value::Null
    };
    let mut response = json!({
        "source_observation_ref":serde_json::to_value(&reference)?,
        "source_session":reference.source_session,
        "observation_task_id":reference.observation_task_id,
        "observation_revision":reference.observation_revision,
        "observation_job_id":reference.observation_job_id,
        "observation_lifecycle_candidate_id":reference.observation_lifecycle_candidate_id,
        "source_result":authoring_result,
        "source_result_status":source_result_status,
        "authoring_parent_ref":authoring_parent_ref,
        "authoring_parent_ref_status":source_result_status,
        "read_only":true,
        "activation_authorized":false,
        "as_of_ms":now
    });
    if let Some(reason) = source_result_reason {
        response["source_result_reason"] = json!(reason);
    }
    match continuation {
        Some(continuation) => {
            response["parent_continuation"] = continuation.clone();
            response["parent_continuation_recorded_in_session"] =
                continuation["current_scope"]["current_session"].clone();
            response["parent_continuation_status"] = json!("verified");
        }
        None => {
            response["parent_continuation_status"] = json!("not_recorded");
        }
    }
    // The native selector reports the AUTHORING source replay status. A
    // changed/unavailable A is an explicit unavailable replay, never an
    // authoritative absence and never a copied financial result.
    response["replay"] = json!({
        "status": if source_result_status == "verified" { "available" } else { "unavailable" },
        "reason": source_result_reason,
        "source": "authoring_parent_a"
    });
    if context_status == "partial" {
        response["context_status"] = json!("partial");
    } else {
        response["context_status"] = json!("complete");
    }
    let _ = source_directory;
    Ok(response)
}

pub(crate) fn candidate_result(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    now: i64,
) -> Result<Value> {
    let (mut db, record) = candidate_task(c, cfg, task_id, revision)?;
    // Verify the recorded result before any lifecycle transition. A corrupt
    // binding is reported as an unverified result, never as a lifecycle fault.
    let result = read_verified_candidate_result(&mut db, task_id, revision)
        .map_err(|_| "candidate_result_unverified")?;
    let lifecycle = reconcile_candidate(c, cfg, &mut db, &record)
        .map_err(|_| "candidate_lifecycle_unavailable")?;
    let Some(result) = result else {
        return Ok(json!({
            "task":candidate_summary(&record),
            "result":null,
            "result_status":"not_verified",
            "lifecycle":lifecycle
        }));
    };
    let mut response = json!({
        "task":candidate_summary(&record),
        "result":candidate_result_view(&result),
        "result_status":"verified",
        "lifecycle":lifecycle
    });
    let mut context_status = "complete";
    if record.spec.purpose.is_none() {
        match super::carry::host_parent_ref(c, cfg, &mut db, &record) {
            Ok(reference) => {
                response["parent_ref"] = reference;
                response["parent_ref_status"] = json!("verified");
            }
            Err(error) => {
                // The own result may be verified, but the cross-session parent
                // context is explicitly unavailable. This is not the same fact
                // as "there is no parent".
                response["parent_ref_status"] = json!("unavailable");
                response["parent_ref_reason"] = json!(bounded_context_reason(error.as_ref()));
                context_status = "partial";
            }
        }
    }
    // The continuation identity is the OBSERVATION episode O, never the
    // authoring parent A. One authoring candidate may have several observation
    // episodes; only the exact O's verified receipt may be attached.
    if record.spec.purpose.is_some() {
        response["source_observation_ref"] = source_observation_ref(&record, &result)?;
        let observation_id = record.spec.lifecycle_candidate_id.clone();
        match super::verified_continuations(c, cfg) {
            Ok(continuations) => {
                if let Some(continuation) = continuations.iter().find(|continuation| {
                    continuation["source_lifecycle_candidate_id"] == observation_id.as_str()
                }) {
                    response["parent_continuation"] = continuation.clone();
                    response["parent_continuation_recorded_in_session"] =
                        continuation["current_scope"]["current_session"].clone();
                    response["parent_continuation_status"] = json!("verified");
                } else {
                    response["parent_continuation_status"] = json!("not_recorded");
                }
            }
            Err(error) => {
                response["parent_continuation_status"] = json!("unavailable");
                response["parent_continuation_reason"] =
                    json!(bounded_context_reason(error.as_ref()));
                context_status = "partial";
            }
        }
    } else {
        // An authoring candidate A has no observation episode by itself. It is
        // NOT the continuation identity; a single arbitrary O receipt must
        // never be attached to it. A bounded LIST of legitimate observation
        // episodes is safe and lets the model select the exact one.
        response["parent_continuation_status"] = json!("not_applicable");
        response["parent_continuation_reason"] = json!("no_source_observation_episode");
        let mut related = Vec::new();
        let mut related_truncated = false;
        let mut source_scan_truncated = false;
        let mut related_partial = false;
        let mut related_source_error_reason: Option<&'static str> = None;
        let mut scanned = 0usize;
        let mut cursor = None;
        let mut exhausted = false;
        let current_binding = book_binding(c).ok();
        let mut note_source_error = |code: &'static str| {
            related_partial = true;
            if related_source_error_reason.is_none() {
                related_source_error_reason = Some(code);
            }
        };
        for _ in 0..10 {
            let Ok(page) = jobs::list_candidates_page(
                &mut db,
                &record.spec.session,
                None,
                cursor.as_ref(),
                100,
            ) else {
                note_source_error("candidate_observation_task_unavailable");
                break;
            };
            if page.is_empty() {
                exhausted = true;
                break;
            }
            let full = page.len() == 100;
            scanned = scanned.saturating_add(page.len());
            cursor = page.last().map(|row| jobs::TaskCursor {
                created_at_ms: row.spec.created_at_ms,
                task_id: row.spec.task_id.clone(),
                revision: row.spec.revision,
            });
            for row in page {
                // A shared queue may hold a foreign book/account row. It is
                // never this authoring task's related episode and its ref is
                // never exposed.
                if current_binding.as_deref() != Some(row.spec.book_binding.as_str()) {
                    note_source_error("candidate_observation_book_binding_mismatch");
                    continue;
                }
                let Some(jobs::CandidatePurposeV1::Observe { target }) = &row.spec.purpose else {
                    continue;
                };
                // The related episode must point at THIS exact authoring task,
                // not merely echo its lifecycle id. A forged metadata-only
                // target is not a legitimate observation of A.
                if target.source_lifecycle_candidate_id != record.spec.lifecycle_candidate_id
                    || target.source_task_id != record.spec.task_id
                    || target.source_revision != record.spec.revision
                    || target.source_candidate_id != record.spec.candidate_id
                    || record.job.result_hash.as_deref()
                        != Some(target.source_result_sha256.as_str())
                {
                    continue;
                }
                let source_ref =
                    read_verified_candidate_result(&mut db, &row.spec.task_id, row.spec.revision)
                        .ok()
                        .flatten()
                        .and_then(|result| {
                            result.observation.as_ref().and_then(|score| {
                                source_observation_ref_from_task(&row, score).ok()
                            })
                        });
                related.push(json!({
                    "task_id":row.spec.task_id,
                    "revision":row.spec.revision,
                    "lifecycle_candidate_id":row.spec.lifecycle_candidate_id,
                    "source_session":row.spec.session,
                    "result_sha256":row.job.result_hash,
                    "source_observation_ref":source_ref,
                    "read_only":true
                }));
                // Buffer ONE row past the display bound: exactly 16 matches is
                // complete; only an actual 17th match is truncation.
                if related.len() > 16 {
                    related.truncate(16);
                    related_truncated = true;
                    break;
                }
            }
            if related_truncated {
                break;
            }
            if !full {
                exhausted = true;
                break;
            }
        }
        // The 10-page scan is bounded. If it ended on a FULL page, an extra row
        // may remain; probe exactly one row so a short list is never labelled
        // complete when the source scan was truncated.
        if !exhausted && !related_truncated && scanned >= 1_000 {
            match jobs::list_candidates_page(
                &mut db,
                &record.spec.session,
                None,
                cursor.as_ref(),
                1,
            ) {
                Ok(extra) if !extra.is_empty() => {
                    source_scan_truncated = true;
                }
                Ok(_) => {}
                Err(_) => note_source_error("candidate_observation_task_unavailable"),
            }
        }
        // A source error is never hidden behind a resource-bound truncation.
        let related_status = if related_partial {
            "partial"
        } else if related_truncated || source_scan_truncated {
            "truncated"
        } else {
            "available"
        };
        response["related_observations"] = json!(related);
        response["related_observations_status"] = json!(related_status);
        response["related_observations_truncated"] = json!(related_truncated);
        response["related_observations_partial"] = json!(related_partial);
        response["related_observations_source_scan_truncated"] = json!(source_scan_truncated);
        response["related_observations_scanned"] = json!(scanned);
        response["related_observations_limit"] = json!(16);
        response["related_observations_reason"] = if related_truncated {
            json!("observation_ref_limit")
        } else if source_scan_truncated {
            json!("source_scan_truncated")
        } else {
            Value::Null
        };
        response["related_observations_source_error_reason"] = json!(related_source_error_reason);
    }
    if let Some(jobs::CandidatePurposeV1::Observe { target }) = &record.spec.purpose {
        // Direct bounded lookup by candidate id: a target outside the 10k
        // library page is an authoritative absence, not a false not_matured.
        let (maturation, maturation_unavailable) =
            match experiment_ledger::lifecycle::observation_item(
                &c.directory,
                &record.spec.lifecycle_candidate_id,
            ) {
                Ok(Some(item)) => {
                    let measurement = &item["observation_measurement"];
                    let present = item["observation_measurement_present"]
                        .as_bool()
                        .unwrap_or(measurement.is_object());
                    if !present && measurement.is_null() {
                        // Absent/NULL is the authoritative not-yet-matured state.
                        (None, false)
                    } else if measurement.is_object() {
                        (Some(measurement.clone()), false)
                    } else {
                        // A PRESENT non-object body is corrupt evidence, never a
                        // silent not_matured and never raw financial data.
                        (None, true)
                    }
                }
                Ok(None) => (None, false),
                Err(_) => (None, true),
            };
        // Read the ORIGINAL verified result through the original queue locator.
        // This is a typed read-only source reference, never a caller hash. A
        // read error is UNAVAILABLE, not the authoritative absence of a result.
        // The SAME central loader used by the native observation-ref path pins
        // A's task/revision/candidate/lifecycle/result/policy/runtime/source/
        // params identity and refuses an Observation task as an authoring parent.
        let (source_result, source_result_status, source_result_reason) =
            match super::carry::open_original_queue_read_only_strict(c, cfg, &record.spec.session) {
                Ok(mut source_db) => {
                    match load_verified_authoring_parent(
                        c,
                        &[],
                        &mut source_db,
                        &record.spec.session,
                        target,
                    ) {
                        Ok(authoring) => {
                            let view = candidate_result_view(&authoring);
                            response["source_result"] = view.clone();
                            (Some(view), "verified", None)
                        }
                        Err(error) => {
                            // ANY unavailable/unverified authoring parent is an
                            // explicitly PARTIAL context. The observation's own
                            // result may still be verified, but A's result is
                            // never copied and the missing context is never
                            // reported as complete.
                            context_status = "partial";
                            let code = bounded_context_reason(error.as_ref());
                            if code == "candidate_result_unverified" {
                                (None, "unavailable", Some(code))
                            } else {
                                (None, "unverified", Some(code))
                            }
                        }
                    }
                }
                Err(error) => {
                    context_status = "partial";
                    (
                        None,
                        "unavailable",
                        Some(bounded_context_reason(error.as_ref())),
                    )
                }
            };
        response["source_result_status"] = json!(source_result_status);
        response["authoring_parent_ref_status"] = json!(source_result_status);
        response["authoring_parent_ref"] = if source_result_status == "verified" {
            json!({
                "source_session":record.spec.session,
                "task_id":target.source_task_id,
                "revision":target.source_revision,
                "candidate_id":target.source_candidate_id,
                "lifecycle_candidate_id":target.source_lifecycle_candidate_id,
                "result_sha256":target.source_result_sha256,
                "runtime_hash":target.source_runtime_hash,
                "policy_sha256":target.policy_sha256,
                "target_frozen_at_ms":target.target_frozen_at_ms,
                "window_from_ms":target.window_from_ms,
                "window_to_ms":target.window_to_ms,
                "holding_ms":target.holding_ms,
                "max_exit_delay_ms":target.max_exit_delay_ms,
                "snapshot_sha256":target.snapshot_sha256,
                "quotes_sha256":target.quotes_sha256,
                "spec_sha256":target.spec_sha256,
                "quote_keys":target.quote_keys,
                "quote_budget_truncated":target.quote_budget_truncated,
                "read_only":true,
                "activation_authorized":false
            })
        } else {
            Value::Null
        };
        if let Some(reason) = &source_result_reason {
            response["source_result_reason"] = json!(reason);
        }
        response["measurement"] = super::measure::measure_observation_with_status(
            c,
            cfg,
            &record,
            &result,
            maturation.as_ref(),
            source_result.as_ref(),
            source_result_status,
            source_result_reason,
            maturation_unavailable,
            now,
        );
    }
    if context_status == "partial" {
        response["context_status"] = json!("partial");
    }
    Ok(response)
}

/// Keep only a bounded static reason code. A raw error string, path or account
/// payload is never projected to the model.
pub(crate) fn bounded_context_reason(error: &dyn std::error::Error) -> &'static str {
    let raw = error.to_string();
    if raw.len() <= 64
        && !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        // The static codes are compile-time literals; map back to a literal so
        // the caller cannot receive a heap string built from arbitrary bytes.
        match raw.as_str() {
            "candidate_result_unverified" => "candidate_result_unverified",
            "candidate_lifecycle_unavailable" => "candidate_lifecycle_unavailable",
            "source_continue_ineligible" => "source_continue_ineligible",
            "source_continue_account_mismatch" => "source_continue_account_mismatch",
            "source_continue_result_unverified" => "source_continue_result_unverified",
            "source_continue_identity_mismatch" => "source_continue_identity_mismatch",
            "source_continue_cohort_changed" => "source_continue_cohort_changed",
            "source_continue_runtime_changed" => "source_continue_runtime_changed",
            "source_continue_lifecycle_unavailable" => "source_continue_lifecycle_unavailable",
            "source_continue_lifecycle_missing" => "source_continue_lifecycle_missing",
            "research_carry_session_not_verified" => "research_carry_session_not_verified",
            "research_queue_unavailable" => "research_queue_unavailable",
            "research_queue_identity_changed" => "research_queue_identity_changed",
            "research_queue_identity_invalid" => "research_queue_identity_invalid",
            "research_original_binding_missing" => "research_original_binding_missing",
            "research_queue_wal_uncheckpointed" => "research_queue_wal_uncheckpointed",
            "source_book_wal_uncheckpointed" => "source_book_wal_uncheckpointed",
            "source_lifecycle_wal_uncheckpointed" => "source_lifecycle_wal_uncheckpointed",
            "candidate_continuation_source_unverified" => {
                "candidate_continuation_source_unverified"
            }
            "candidate_continuation_identity_mismatch" => {
                "candidate_continuation_identity_mismatch"
            }
            "candidate_continuation_invalid" => "candidate_continuation_invalid",
            "candidate_observation_source_unavailable" => {
                "candidate_observation_source_unavailable"
            }
            "candidate_observation_source_unverified" => "candidate_observation_source_unverified",
            "candidate_observation_source_not_authoring" => {
                "candidate_observation_source_not_authoring"
            }
            "candidate_observation_target_invalid" => "candidate_observation_target_invalid",
            _ => "candidate_context_unavailable",
        }
    } else {
        "candidate_context_unavailable"
    }
}

pub(crate) fn candidate_cancel(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    now: i64,
) -> Result<Value> {
    let (mut db, _) = candidate_task(c, cfg, task_id, revision)?;
    let record = jobs::cancel_candidate(&mut db, task_id, revision, revision, now)
        .map_err(|_| "candidate_task_cancel_refused")?;
    Ok(json!({
        "task":candidate_summary(&record),
        "cancellation":"requested",
        "terminal":task_state_terminal(record.status),
        "promotion_authorized":false,
        "activation_authorized":false
    }))
}

fn candidate_result_view(result: &CandidateResultV1) -> Value {
    let mut body = candidate_result_view_body(result);
    if let Some(observation) = &result.observation {
        body["observation"] = json!(observation);
        body["result_kind"] = json!(result.result_kind);
    }
    body
}

fn candidate_result_view_body(result: &CandidateResultV1) -> Value {
    let ticks = result
        .report
        .ticks
        .iter()
        .rev()
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|tick| {
            json!({
                "observed_ms":tick.observed_ms,
                "status":tick.status,
                "failure":tick.failure,
                "top_n_overlap":tick.top_n_overlap,
                "kendall":tick.kendall,
                "spearman":tick.spearman,
                "default_top_n":tick.default_top_n,
                "candidate_top_n":tick.candidate_top_n
            })
        })
        .collect::<Vec<_>>();
    let horizons = result
        .report
        .horizons
        .iter()
        .map(|horizon| {
            json!({
                "horizon_minutes":horizon.horizon_minutes,
                "ticks":horizon.ticks,
                "selected_rows":horizon.selected_rows,
                "missing_forward_marks":horizon.missing_forward_marks,
                "mean_forward_return_bps":horizon.mean_forward_return_bps,
                "cost_adjusted":horizon.cost_adjusted
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "schema":result.report.schema,
        "status":result.report.status,
        "reason":result.report.reason,
        "scorer_sha256":result.report.scorer_sha256,
        "params_sha256":result.report.params_sha256,
        "params_effective_sha256":result.report.params_effective_sha256,
        "dataset_hash":result.report.dataset_hash,
        "snapshots":result.report.snapshots,
        "valid_snapshots":result.report.valid_snapshots,
        "failed_snapshots":result.report.failed_snapshots,
        "min_valid_snapshots":result.report.min_valid_snapshots,
        "top_n":result.report.top_n,
        "mean_top_n_overlap":result.report.mean_top_n_overlap,
        "mean_kendall":result.report.mean_kendall,
        "mean_spearman":result.report.mean_spearman,
        "ticks":ticks,
        "ticks_omitted":result.report.ticks.len().saturating_sub(10),
        "horizons":horizons,
        "report_hash":result.report.report_hash
    });
    let mut body = json!({
        "version":result.version,
        "task_id":result.task_id,
        "revision":result.revision,
        "job_id":result.job_id,
        "candidate_id":result.candidate_id,
        "lifecycle_candidate_id":result.lifecycle_candidate_id,
        "abi":result.abi,
        "source_sha256":result.source_sha256,
        "params_sha256":result.params_sha256,
        "dataset_id":result.dataset_id,
        "dataset_hash":result.dataset_hash,
        "windows":result.windows,
        "horizon_minutes":result.horizon_minutes,
        "runtime_binding":result.runtime_binding,
        "sandbox":result.sandbox,
        "unit_policy":result.unit_policy,
        "unit_proof":result.unit_proof,
        "unit_proof_sha256":result.unit_proof_sha256,
        "result_produced_at_ms":result.result_produced_at_ms,
        "result_kind":result.result_kind,
        "evidence_scope":result.evidence_scope,
        "partial_evidence":result.partial_evidence,
        "gaps":result.gaps,
        "advisory":result.advisory,
        "qualified":result.qualified,
        "analysis_status":result.analysis_status,
        "report":report,
        "report_sha256":result.report_sha256,
        "promotion_authorized":result.promotion_authorized,
        "activation_authorized":result.activation_authorized,
        "edge_qualified":result.edge_qualified,
        "host_qualified":result.host_qualified
    });
    if let Some(execution) = &result.execution {
        body["execution"] = json!(execution);
        body["execution_status"] = json!(execution.execution_status);
        body["execution_result_kind"] = json!(execution.result_kind);
        body["action_tape_sha256"] = json!(execution.action_tape_sha256);
        body["execution_spec_sha256"] = json!(execution.execution_spec_sha256);
        body["execution_manifest_sha256"] = json!(execution.manifest_sha256);
        body["execution_events_sha256"] = json!(execution.events_sha256);
    }
    body
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_stopped(
    launch: &worker::CandidateLaunch,
    proof: worker::CandidateUnitProof,
    cause: jobs::CandidateStopCause,
    outcome: CandidateStopOutcome,
    failure_code: Option<&str>,
    result_sha256: Option<String>,
    result_at_ms: Option<i64>,
    result_bytes: Option<u64>,
    stopped_at_ms: i64,
) -> CandidateStopped {
    let checked_at_ms = proof.checked_at_ms;
    let failure_code = failure_code.map(|code| {
        code.chars()
            .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '_')
            .take(64)
            .collect::<String>()
    });
    CandidateStopped {
        version: 2,
        kind: "candidate_unit_stopped".into(),
        job_id: launch.job_id.clone(),
        lease: launch.lease.clone(),
        task_id: launch.task_id.clone(),
        revision: launch.revision,
        candidate_id: launch.candidate_id.clone(),
        unit: launch.unit.clone(),
        proof,
        cause,
        outcome,
        failure_code: failure_code.filter(|code| !code.is_empty()),
        result_bytes,
        result_sha256,
        result_at_ms,
        source_sha256: launch.source_sha256.clone(),
        params_sha256: launch.params_sha256.clone(),
        dataset_hash: launch.dataset_hash.clone(),
        runtime_hash: launch.runtime_binding.runtime_hash.clone(),
        stopped_at_ms: stopped_at_ms.max(checked_at_ms),
    }
}

fn stopped_hash(stopped: &CandidateStopped) -> std::result::Result<String, &'static str> {
    let bytes = serde_json::to_vec(stopped).map_err(|_| "candidate_stopped_invalid")?;
    Ok(digest(&bytes))
}

pub(crate) fn write_stopped(
    db: &Connection,
    launch: &worker::CandidateLaunch,
    stopped: &CandidateStopped,
) -> std::result::Result<(), &'static str> {
    let bytes = serde_json::to_vec(stopped).map_err(|_| "candidate_stopped_invalid")?;
    worker::save_receipt(
        db,
        &launch.job_id,
        &launch.lease,
        "candidate_stopped",
        &bytes,
    )
    .map_err(|_| "candidate_stopped_receipt_failed")
}

fn private_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

fn ensure_private_dir(path: &Path) -> std::result::Result<(), &'static str> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("candidate_stage_unavailable"),
    }
    let meta = fs::symlink_metadata(path).map_err(|_| "candidate_stage_unavailable")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("candidate_stage_invalid");
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| "candidate_stage_unavailable")?;
    let meta = fs::symlink_metadata(path).map_err(|_| "candidate_stage_unavailable")?;
    if private_mode(&meta) & 0o077 != 0 {
        return Err("candidate_stage_invalid");
    }
    Ok(())
}

fn write_new_private(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> std::result::Result<(), &'static str> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() || private_mode(&meta) != mode {
            return Err("candidate_stage_invalid");
        }
        let mut existing = Vec::new();
        fs::File::open(path)
            .and_then(|mut file| file.read_to_end(&mut existing))
            .map_err(|_| "candidate_stage_unavailable")?;
        return if existing == bytes {
            Ok(())
        } else {
            Err("candidate_stage_invalid")
        };
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "candidate_stage_unavailable")?;
    file.write_all(bytes)
        .map_err(|_| "candidate_stage_unavailable")?;
    file.sync_all().map_err(|_| "candidate_stage_unavailable")?;
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| "candidate_stage_unavailable")?;
    let mut check = Vec::new();
    fs::File::open(path)
        .and_then(|mut file| file.read_to_end(&mut check))
        .map_err(|_| "candidate_stage_unavailable")?;
    if check != bytes {
        return Err("candidate_stage_invalid");
    }
    Ok(())
}

fn stage_executable(
    root: &Path,
    source: &Path,
    expected: &str,
    name: &str,
) -> std::result::Result<PathBuf, &'static str> {
    ensure_private_dir(root)?;
    let executables = root.join("executables");
    ensure_private_dir(&executables)?;
    let path = executables.join(format!("{name}-{expected}"));
    if path.exists() {
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| "candidate_stage_unavailable")?;
        if digest(&bytes) != expected {
            return Err("candidate_stage_invalid");
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| "candidate_stage_unavailable")?;
        if !meta.is_file() || meta.file_type().is_symlink() || private_mode(&meta) != 0o500 {
            return Err("candidate_stage_invalid");
        }
        return Ok(path);
    }
    let resolved = fs::canonicalize(source).map_err(|_| "candidate_runtime_unavailable")?;
    let bytes = crate::read_bounded(&resolved, 64 * 1024 * 1024)
        .map_err(|_| "candidate_runtime_unavailable")?;
    if digest(&bytes) != expected {
        return Err("candidate_runtime_unavailable");
    }
    write_new_private(&path, &bytes, 0o500)?;
    Ok(path)
}

fn stage_helper(
    root: &Path,
    source: &Path,
    helper_sha256: &str,
) -> std::result::Result<PathBuf, &'static str> {
    ensure_private_dir(root)?;
    let helpers = root.join("helpers");
    ensure_private_dir(&helpers)?;
    let path = helpers.join(helper_sha256);
    if path.exists() {
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| "candidate_stage_unavailable")?;
        if digest(&bytes) != helper_sha256 {
            return Err("candidate_stage_invalid");
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| "candidate_stage_unavailable")?;
        if !meta.is_file() || meta.file_type().is_symlink() || private_mode(&meta) != 0o500 {
            return Err("candidate_stage_invalid");
        }
        return Ok(path);
    }
    let resolved = fs::canonicalize(source).map_err(|_| "candidate_runtime_unavailable")?;
    let bytes = crate::read_bounded(&resolved, 256 * 1024 * 1024)
        .map_err(|_| "candidate_runtime_unavailable")?;
    if digest(&bytes) != helper_sha256 {
        return Err("candidate_runtime_unavailable");
    }
    write_new_private(&path, &bytes, 0o500)?;
    Ok(path)
}

pub(crate) fn stage_candidate(
    settings: &ResearchSettings,
    candidate: &CandidateComputeSettings,
    launch: &worker::CandidateLaunch,
    source: &[u8],
    params: &[u8],
    dataset: &[u8],
    execution: Option<(&[u8], &[u8], &[u8])>,
) -> std::result::Result<(), &'static str> {
    let root = &settings.stage_root;
    ensure_private_dir(root)?;
    let stage = Path::new(&launch.stage_dir);
    ensure_private_dir(stage)?;
    if digest(source) != launch.source_sha256
        || digest(params) != launch.params_sha256
        || digest(dataset) != launch.dataset_hash
    {
        return Err("candidate_input_changed");
    }
    write_new_private(&stage.join("source"), source, 0o600)?;
    write_new_private(&stage.join("params"), params, 0o600)?;
    write_new_private(&stage.join("dataset"), dataset, 0o600)?;
    match (&launch.execution, execution) {
        (Some(binding), Some((spec, manifest, events))) => {
            if digest(spec) != binding.spec_sha256
                || digest(manifest) != binding.manifest_sha256
                || digest(events) != binding.events_sha256
            {
                return Err("candidate_input_changed");
            }
            write_new_private(&stage.join("execution_spec"), spec, 0o600)?;
            write_new_private(&stage.join("execution_manifest"), manifest, 0o600)?;
            write_new_private(&stage.join("execution_events"), events, 0o600)?;
        }
        (None, None) => {}
        _ => return Err("candidate_stage_invalid"),
    }
    let helper_source = match candidate.helper_path.as_ref() {
        Some(path) => path.clone(),
        None => std::env::current_exe().map_err(|_| "candidate_runtime_unavailable")?,
    };
    let helper = stage_helper(root, &helper_source, &launch.unit_policy.helper_sha256)?;
    if helper != Path::new(&launch.helper_path) {
        return Err("candidate_stage_invalid");
    }
    let python = stage_executable(
        root,
        &candidate.sandbox.python_path,
        &launch.sandbox.python_sha256,
        "python",
    )?;
    let bwrap = stage_executable(
        root,
        &candidate.sandbox.bwrap_path,
        &launch.sandbox.bwrap_sha256,
        "bwrap",
    )?;
    if python != Path::new(&launch.python_path) || bwrap != Path::new(&launch.bwrap_path) {
        return Err("candidate_stage_invalid");
    }
    let launch_bytes = serde_json::to_vec(launch).map_err(|_| "candidate_launch_invalid")?;
    write_new_private(&stage.join("launch.json"), &launch_bytes, 0o600)?;
    Ok(())
}

pub(crate) fn machine_id_from(
    mut read: impl FnMut(&str) -> std::io::Result<String>,
) -> std::result::Result<String, &'static str> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(text) = read(path) {
            let id = text.trim();
            if !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control) {
                return Ok(id.to_owned());
            }
        }
    }
    Err("candidate_machine_unavailable")
}

fn machine_id() -> std::result::Result<String, &'static str> {
    machine_id_from(|path| fs::read_to_string(path))
}

#[cfg(test)]
pub(crate) fn prepare_launch(
    settings: &ResearchSettings,
    task: &jobs::CandidateTaskRecord,
    lease: &str,
    now: i64,
) -> std::result::Result<worker::CandidateLaunch, &'static str> {
    let machine = machine_id()?;
    prepare_launch_with_machine(settings, task, lease, now, &machine)
}

fn prepare_launch_with_machine(
    settings: &ResearchSettings,
    task: &jobs::CandidateTaskRecord,
    lease: &str,
    now: i64,
    machine: &str,
) -> std::result::Result<worker::CandidateLaunch, &'static str> {
    let root = &settings.stage_root;
    let stage_dir = root.join(format!("candidate-{lease}"));
    let helper_path = root
        .join("helpers")
        .join(&task.spec.unit_policy.helper_sha256);
    let python_path = root
        .join("executables")
        .join(format!("python-{}", task.spec.sandbox.python_sha256));
    let bwrap_path = root
        .join("executables")
        .join(format!("bwrap-{}", task.spec.sandbox.bwrap_sha256));
    let unit = worker::candidate_unit_name(lease).ok_or("candidate_launch_invalid")?;
    let deadline_ms = now
        .checked_add(i64::from(task.job.spec.wall_millis))
        .ok_or("candidate_launch_invalid")?;
    Ok(worker::CandidateLaunch {
        version: 2,
        job_id: task.spec.job_id.clone(),
        lease: lease.to_owned(),
        task_id: task.spec.task_id.clone(),
        revision: task.spec.revision,
        candidate_id: task.spec.candidate_id.clone(),
        lifecycle_candidate_id: task.spec.lifecycle_candidate_id.clone(),
        source_sha256: task.spec.source_sha256.clone(),
        params_sha256: task.spec.params_sha256.clone(),
        dataset_id: task.spec.dataset_id.clone(),
        dataset_hash: task.spec.dataset_hash.clone(),
        abi: task.spec.abi.clone(),
        runtime_binding: task.spec.runtime_binding.clone(),
        sandbox: task.spec.sandbox.clone(),
        unit_policy: task.spec.unit_policy.clone(),
        limits: task.spec.resources.clone(),
        replay_json: serde_json::to_string(
            &settings
                .candidate_compute
                .as_ref()
                .ok_or("candidate_runtime_unavailable")?
                .replay,
        )
        .map_err(|_| "candidate_launch_invalid")?,
        stage_dir: stage_dir.to_string_lossy().into_owned(),
        helper_path: helper_path.to_string_lossy().into_owned(),
        python_path: python_path.to_string_lossy().into_owned(),
        bwrap_path: bwrap_path.to_string_lossy().into_owned(),
        unit,
        machine: machine.to_owned(),
        started_ms: now,
        deadline_ms,
        task_deadline_ms: deadline_ms.min(task.spec.expires_at_ms),
        execution: task.spec.execution.clone(),
        observation: match &task.spec.purpose {
            Some(jobs::CandidatePurposeV1::Observe { target }) => Some(target.as_ref().clone()),
            _ => None,
        },
    })
}

fn verify_launch(launch: &worker::CandidateLaunch, task: &jobs::CandidateTaskRecord) -> bool {
    launch.version == 2
        && launch.job_id == task.spec.job_id
        && launch.lease == task.job.lease.clone().unwrap_or_default()
        && launch.task_id == task.spec.task_id
        && launch.revision == task.spec.revision
        && launch.candidate_id == task.spec.candidate_id
        && launch.lifecycle_candidate_id == task.spec.lifecycle_candidate_id
        && launch.source_sha256 == task.spec.source_sha256
        && launch.params_sha256 == task.spec.params_sha256
        && launch.dataset_id == task.spec.dataset_id
        && launch.dataset_hash == task.spec.dataset_hash
        && launch.abi == task.spec.abi
        && launch.runtime_binding == task.spec.runtime_binding
        && launch.sandbox == task.spec.sandbox
        && launch.unit_policy == task.spec.unit_policy
        && launch.limits == task.spec.resources
        && launch.unit == worker::candidate_unit_name(&launch.lease).unwrap_or_default()
        && launch.started_ms >= task.spec.created_at_ms
        && launch.started_ms < task.spec.expires_at_ms
        && launch.deadline_ms
            == launch
                .started_ms
                .saturating_add(i64::from(task.job.spec.wall_millis))
        && launch.task_deadline_ms == launch.deadline_ms.min(task.spec.expires_at_ms)
        && launch.task_deadline_ms <= task.spec.expires_at_ms
        && launch.execution == task.spec.execution
        && launch.observation
            == match &task.spec.purpose {
                Some(jobs::CandidatePurposeV1::Observe { target }) => Some(target.as_ref().clone()),
                _ => None,
            }
        && launch.replay_json.len() <= 8192
        && serde_json::from_str::<Value>(&launch.replay_json).is_ok()
}

fn read_launch(
    db: &Connection,
    task: &jobs::CandidateTaskRecord,
) -> std::result::Result<worker::CandidateLaunch, &'static str> {
    let lease = task
        .job
        .lease
        .as_deref()
        .ok_or("candidate_recovery_required")?;
    let bytes = worker::read_receipt(db, &task.spec.job_id, lease, "candidate_launch", 65_536)
        .map_err(|_| "candidate_recovery_required")?
        .ok_or("candidate_recovery_required")?;
    let launch: worker::CandidateLaunch =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_recovery_required")?;
    if serde_json::to_vec(&launch).map_err(|_| "candidate_recovery_required")? != bytes
        || !verify_launch(&launch, task)
    {
        return Err("candidate_recovery_required");
    }
    Ok(launch)
}

fn read_stopped(
    db: &Connection,
    launch: &worker::CandidateLaunch,
) -> std::result::Result<Option<CandidateStopped>, &'static str> {
    let bytes = worker::read_receipt(
        db,
        &launch.job_id,
        &launch.lease,
        "candidate_stopped",
        65_536,
    )
    .map_err(|_| "candidate_stopped_receipt_failed")?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let stopped: CandidateStopped =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_stopped_invalid")?;
    let outcome_ok = match (stopped.cause, stopped.outcome) {
        (jobs::CandidateStopCause::Succeeded, CandidateStopOutcome::Succeeded) => {
            stopped.result_sha256.is_some()
                && stopped.result_at_ms.is_some()
                && stopped.failure_code.is_none()
        }
        (jobs::CandidateStopCause::OperatorCancel, CandidateStopOutcome::Cancelled) => true,
        (
            jobs::CandidateStopCause::DeadlineExpired,
            CandidateStopOutcome::DeadlineExpired | CandidateStopOutcome::ResultAfterDeadline,
        ) => true,
        (
            jobs::CandidateStopCause::Failure,
            CandidateStopOutcome::NoResult
            | CandidateStopOutcome::InvalidResult
            | CandidateStopOutcome::ResultTooLarge
            | CandidateStopOutcome::LaunchFailed
            | CandidateStopOutcome::Terminated,
        ) => true,
        _ => false,
    };
    let result_pair = stopped.cause != jobs::CandidateStopCause::Succeeded
        || (stopped.result_sha256.is_some() && stopped.result_at_ms.is_some());
    let failure_code_ok = stopped.failure_code.as_deref().is_none_or(|code| {
        !code.is_empty()
            && code.len() <= 64
            && code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    });
    if serde_json::to_vec(&stopped).map_err(|_| "candidate_stopped_invalid")? != bytes
        || stopped.version != 2
        || stopped.kind != "candidate_unit_stopped"
        || stopped.job_id != launch.job_id
        || stopped.lease != launch.lease
        || stopped.task_id != launch.task_id
        || stopped.revision != launch.revision
        || stopped.candidate_id != launch.candidate_id
        || stopped.unit != launch.unit
        || !stopped.proof.verify(launch)
        || !outcome_ok
        || !result_pair
        || !failure_code_ok
        || stopped
            .result_at_ms
            .is_some_and(|at| at < launch.started_ms)
        || stopped
            .result_bytes
            .is_some_and(|bytes| bytes == 0 || bytes > 64 * 1024 * 1024)
        || stopped.source_sha256 != launch.source_sha256
        || stopped.params_sha256 != launch.params_sha256
        || stopped.dataset_hash != launch.dataset_hash
        || stopped.runtime_hash != launch.runtime_binding.runtime_hash
        || stopped.stopped_at_ms < stopped.proof.checked_at_ms
    {
        return Err("candidate_stopped_invalid");
    }
    Ok(Some(stopped))
}

fn read_stage_file(path: &Path, max: usize) -> std::result::Result<Vec<u8>, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let owner = fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(u32::MAX);
    let parent = path.parent().ok_or("candidate_helper_input_invalid")?;
    let parent_meta =
        fs::symlink_metadata(parent).map_err(|_| "candidate_helper_input_unavailable")?;
    if !parent_meta.is_dir()
        || parent_meta.file_type().is_symlink()
        || parent_meta.uid() != owner
        || parent_meta.permissions().mode() & 0o077 != 0
    {
        return Err("candidate_helper_input_invalid");
    }
    // One fd owns the identity check and the bytes. `O_NOFOLLOW` refuses a
    // symlink swapped in after the parent check; `O_NONBLOCK` prevents a FIFO
    // substitution from blocking the helper.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "candidate_helper_input_unavailable")?;
    let meta = file
        .metadata()
        .map_err(|_| "candidate_helper_input_unavailable")?;
    if !meta.is_file()
        || meta.len() == 0
        || meta.len() > max as u64
        || meta.uid() != owner
        || meta.permissions().mode() & 0o077 != 0
        || meta.permissions().mode() & 0o400 == 0
    {
        return Err("candidate_helper_input_invalid");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| "candidate_helper_input_unavailable")?;
    if bytes.len() as u64 != meta.len() {
        return Err("candidate_helper_input_invalid");
    }
    Ok(bytes)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// Content check for the running helper. The path is opened once with
/// `O_NOFOLLOW` and bounded; a changed byte stream is refused before any
/// untrusted source is read. Tests exercise this directly with a small file so
/// they do not depend on the host helper binary's size.
pub(crate) fn verify_helper_file(
    path: &Path,
    expected: &str,
) -> std::result::Result<(), &'static str> {
    let bytes = crate::read_bounded(path, 256 * 1024 * 1024)
        .map_err(|_| "candidate_helper_self_unavailable")?;
    if digest(&bytes) != expected {
        return Err("candidate_helper_self_changed");
    }
    Ok(())
}

/// Fixed helper body. It reads only the staged launch/input files, re-verifies
/// every bound hash, executes the staged content-addressed interpreter and
/// bwrap copies, and writes one bounded result atomically. It never opens the
/// queue, config, broker, account, data root or live paper book.
pub(crate) fn run_helper_at(
    launch: &worker::CandidateLaunch,
    clock: impl Fn() -> i64,
    verify_self: bool,
) -> std::result::Result<CandidateHelperOutput, &'static str> {
    if launch.version != 2
        || launch.unit != worker::candidate_unit_name(&launch.lease).unwrap_or_default()
    {
        return Err("candidate_helper_launch_invalid");
    }
    if verify_self {
        let executable =
            std::env::current_exe().map_err(|_| "candidate_helper_self_unavailable")?;
        verify_helper_file(&executable, &launch.unit_policy.helper_sha256)?;
    }
    let stage = Path::new(&launch.stage_dir);
    let source = read_stage_file(&stage.join("source"), 262_144)?;
    let params = read_stage_file(&stage.join("params"), 65_536)?;
    let dataset = read_stage_file(&stage.join("dataset"), 4 * 1024 * 1024)?;
    if digest(&source) != launch.source_sha256
        || digest(&params) != launch.params_sha256
        || digest(&dataset) != launch.dataset_hash
    {
        return Err("candidate_helper_input_changed");
    }
    let python = read_stage_file(Path::new(&launch.python_path), 64 * 1024 * 1024)?;
    let bwrap = read_stage_file(Path::new(&launch.bwrap_path), 64 * 1024 * 1024)?;
    if digest(&python) != launch.sandbox.python_sha256
        || digest(&bwrap) != launch.sandbox.bwrap_sha256
    {
        return Err("candidate_helper_runtime_changed");
    }
    let replay: ScorerReplaySettings =
        serde_json::from_str(&launch.replay_json).map_err(|_| "candidate_helper_replay_invalid")?;
    replay
        .validate()
        .map_err(|_| "candidate_helper_replay_invalid")?;
    let snapshots: Vec<sadr_core::strategy::FeatureSnapshot> =
        serde_json::from_slice(&dataset).map_err(|_| "candidate_helper_dataset_invalid")?;
    if snapshots.is_empty() {
        return Err("candidate_helper_dataset_invalid");
    }
    // Only the declared execution policy is read before untrusted scoring. The
    // full frozen quote tape is future data for the adapter, not input to the
    // scorer, so it is not read into the helper heap until after the untrusted
    // replay has completed. This keeps a corrupt or hostile tape from
    // influencing the score phase and preserves the source isolation boundary.
    let execution_policy = match &launch.execution {
        Some(binding) => {
            let spec_bytes = read_stage_file(&stage.join("execution_spec"), 65_536)?;
            if digest(&spec_bytes) != binding.spec_sha256 {
                return Err("candidate_helper_execution_input_changed");
            }
            let spec: sadr_cli::candidate_execution::CandidateExecutionSpecV1 =
                serde_json::from_slice(&spec_bytes)
                    .map_err(|_| "candidate_helper_execution_spec_invalid")?;
            sadr_cli::candidate_execution::validate_spec(&spec)
                .map_err(|_| "candidate_helper_execution_spec_invalid")?;
            let first = snapshots
                .first()
                .map(|snapshot| snapshot.observed_ms)
                .unwrap_or(i64::MAX);
            let last = snapshots
                .last()
                .map(|snapshot| snapshot.observed_ms)
                .unwrap_or(i64::MIN);
            if spec.parameters.selection_from_ms > first
                || spec.parameters.selection_to_ms < last
                || spec.parameters.holding_ms != replay.horizons_minutes[0].saturating_mul(60_000)
                || spec.selection_top_n as usize > replay.top_n
            {
                return Err("candidate_helper_execution_windows_invalid");
            }
            Some(spec)
        }
        None => None,
    };
    let sandbox = sadr_sandbox::ScorerSandboxConfig {
        python_path: launch.python_path.clone().into(),
        bwrap_path: launch.bwrap_path.clone().into(),
        cpu_seconds: launch.sandbox.cpu_seconds,
        memory_mb: launch.sandbox.memory_mb,
        wall_ms: launch.sandbox.wall_ms,
        max_output_bytes: launch.sandbox.max_output_bytes as usize,
        max_keys: launch.sandbox.max_keys as usize,
        max_processes: launch.sandbox.max_processes,
        max_open_files: launch.sandbox.max_open_files,
        network: false,
    };
    let mut advisory = sadr_sandbox::BwrapLeaderReceipt::not_started();
    let report = scorer_compare::replay_scorer_bounded_observed(
        &replay,
        &sandbox,
        &source,
        &params,
        &snapshots,
        None,
        || false,
        |_| Ok(sandbox.clone()),
        |receipt| advisory = receipt.clone(),
    )
    .map_err(|_| "candidate_helper_replay_failed")?;
    let execution = match execution_policy {
        Some(spec) => {
            let binding = launch
                .execution
                .as_ref()
                .ok_or("candidate_helper_execution_input_changed")?;
            let manifest = read_stage_file(&stage.join("execution_manifest"), 512 * 1024)?;
            let events = read_stage_file(&stage.join("execution_events"), 16 * 1024 * 1024)?;
            if digest(&manifest) != binding.manifest_sha256
                || digest(&events) != binding.events_sha256
            {
                return Err("candidate_helper_execution_input_changed");
            }
            let signals = sadr_cli::candidate_execution::signals_from_ranking(&report, &spec)
                .map_err(|_| "candidate_helper_execution_signals_invalid")?;
            let execution =
                sadr_cli::candidate_execution::execute(&signals, &spec, &manifest, &events)
                    .map_err(|_| "candidate_helper_execution_failed")?;
            sadr_cli::candidate_execution::verify_report_against_manifest(
                &execution, &manifest, &events,
            )
            .map_err(|_| "candidate_helper_execution_invalid")?;
            Some(execution)
        }
        None => None,
    };
    Ok(CandidateHelperOutput {
        version: 1,
        abi: "candidate_helper_output_v1".into(),
        job_id: launch.job_id.clone(),
        lease: launch.lease.clone(),
        task_id: launch.task_id.clone(),
        revision: launch.revision,
        candidate_id: launch.candidate_id.clone(),
        source_sha256: launch.source_sha256.clone(),
        params_sha256: launch.params_sha256.clone(),
        dataset_hash: launch.dataset_hash.clone(),
        runtime_hash: launch.runtime_binding.runtime_hash.clone(),
        result_produced_at_ms: clock(),
        report,
        advisory,
        execution,
        observation: None,
    })
}

/// Fixed observation helper body. It reads only the original source/params,
/// the frozen feature snapshot, the frozen research quote tape and the study
/// spec. It never reads an exit quote, the execution tape, the live paper book,
/// host paths or the network. The score completion clock is sampled after the
/// untrusted scorer returns; the key is chosen from the actual post-score
/// ranking, never preselected.
pub(crate) fn run_observation_at(
    launch: &worker::CandidateLaunch,
    clock: impl Fn() -> i64,
    verify_self: bool,
) -> std::result::Result<CandidateHelperOutput, &'static str> {
    if launch.version != 2
        || launch.unit != worker::candidate_unit_name(&launch.lease).unwrap_or_default()
    {
        return Err("candidate_helper_launch_invalid");
    }
    if verify_self {
        let executable =
            std::env::current_exe().map_err(|_| "candidate_helper_self_unavailable")?;
        verify_helper_file(&executable, &launch.unit_policy.helper_sha256)?;
    }
    let target = launch
        .observation
        .as_ref()
        .ok_or("candidate_helper_observation_missing")?;
    if !target.validate()
        || target.snapshot_sha256 != launch.dataset_hash
        || target.runtime.source_sha256 != launch.source_sha256
        || target.runtime.params_sha256 != launch.params_sha256
        || target.runtime.runtime_hash != launch.runtime_binding.runtime_hash
    {
        return Err("candidate_helper_observation_binding_invalid");
    }
    let stage = Path::new(&launch.stage_dir);
    let source = read_stage_file(&stage.join("source"), 262_144)?;
    let params = read_stage_file(&stage.join("params"), 65_536)?;
    let dataset = read_stage_file(&stage.join("dataset"), 4 * 1024 * 1024)?;
    let spec_bytes = read_stage_file(&stage.join("observation_spec"), 65_536)?;
    let bundle_bytes = read_stage_file(&stage.join("observation_quotes"), 2_000_000)?;
    if digest(&source) != launch.source_sha256
        || digest(&params) != launch.params_sha256
        || digest(&dataset) != launch.dataset_hash
        || digest(&spec_bytes) != target.spec_sha256
        || digest(&bundle_bytes) != target.quotes_sha256
    {
        return Err("candidate_helper_input_changed");
    }
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes)
            .map_err(|_| "candidate_helper_observation_spec_invalid")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_helper_observation_spec_invalid")?;
    let snapshots: Vec<sadr_core::strategy::FeatureSnapshot> =
        serde_json::from_slice(&dataset).map_err(|_| "candidate_helper_dataset_invalid")?;
    if snapshots.is_empty()
        || snapshots
            .iter()
            .any(|snapshot| snapshot.observed_ms > target.signal_ms)
        || snapshots
            .last()
            .is_some_and(|snapshot| snapshot.observed_ms < target.window_from_ms)
    {
        return Err("candidate_helper_dataset_invalid");
    }
    let python = read_stage_file(Path::new(&launch.python_path), 64 * 1024 * 1024)?;
    let bwrap = read_stage_file(Path::new(&launch.bwrap_path), 64 * 1024 * 1024)?;
    if digest(&python) != launch.sandbox.python_sha256
        || digest(&bwrap) != launch.sandbox.bwrap_sha256
    {
        return Err("candidate_helper_runtime_changed");
    }
    let replay: ScorerReplaySettings =
        serde_json::from_str(&launch.replay_json).map_err(|_| "candidate_helper_replay_invalid")?;
    replay
        .validate()
        .map_err(|_| "candidate_helper_replay_invalid")?;
    let sandbox = sadr_sandbox::ScorerSandboxConfig {
        python_path: launch.python_path.clone().into(),
        bwrap_path: launch.bwrap_path.clone().into(),
        cpu_seconds: launch.sandbox.cpu_seconds,
        memory_mb: launch.sandbox.memory_mb,
        wall_ms: launch.sandbox.wall_ms,
        max_output_bytes: launch.sandbox.max_output_bytes as usize,
        max_keys: launch.sandbox.max_keys as usize,
        max_processes: launch.sandbox.max_processes,
        max_open_files: launch.sandbox.max_open_files,
        network: false,
    };
    let mut advisory = sadr_sandbox::BwrapLeaderReceipt::not_started();
    let report = scorer_compare::replay_scorer_bounded_observed(
        &replay,
        &sandbox,
        &source,
        &params,
        &snapshots,
        None,
        || false,
        |_| Ok(sandbox.clone()),
        |receipt| advisory = receipt.clone(),
    )
    .map_err(|_| "candidate_helper_replay_failed")?;
    // Real completion clock, sampled after the scorer returned. Never the last
    // feature tick and never a retrospective replay result time.
    let score_produced_at_ms = clock();
    if score_produced_at_ms < launch.started_ms {
        return Err("candidate_helper_clock_invalid");
    }
    let decision_at_ms = score_produced_at_ms;
    // The ranked tick's own observation time is the feature time. The last
    // snapshot may be a later failed tick and must never be relabelled.
    let ranked = report
        .ticks
        .iter()
        .rev()
        .find(|tick| tick.status == "ok" && !tick.candidate_top_n.is_empty());
    let features_at_ms = ranked
        .map(|tick| tick.observed_ms)
        .or_else(|| snapshots.last().map(|snapshot| snapshot.observed_ms))
        .unwrap_or(0);
    let mut gaps = Vec::new();
    let mut status = "scored_no_ranked_key";
    let mut action: Option<sadr_cli::candidate_execution::ObservationActionV1> = None;
    if let Some(tick) = ranked {
        let signal = sadr_cli::candidate_execution::ScoredSignal {
            at_ms: decision_at_ms,
            ordered_keys: tick.candidate_top_n.clone(),
        };
        if !report.mean_top_n_overlap.is_some_and(f64::is_finite) {
            status = "scored_no_score";
            gaps.push("scorer_score_missing_or_nonfinite".to_string());
        } else if decision_at_ms > spec.parameters.selection_to_ms {
            status = "scored_late";
            gaps.push("decision_after_declared_window".to_string());
        } else {
            let bundle: Value = serde_json::from_slice(&bundle_bytes)
                .map_err(|_| "candidate_helper_observation_quotes_invalid")?;
            if bundle["status"] != "ok" {
                status = "entry_quote_unavailable";
                gaps.push("observation_quote_missing".to_string());
            } else {
                let manifest = bundle["manifest"]
                    .as_str()
                    .ok_or("candidate_helper_observation_quotes_invalid")?
                    .as_bytes();
                let events = bundle["events"]
                    .as_str()
                    .ok_or("candidate_helper_observation_quotes_invalid")?
                    .as_bytes();
                if bundle["manifest_sha256"] != digest(manifest)
                    || bundle["events_sha256"] != digest(events)
                {
                    return Err("candidate_helper_observation_quotes_changed");
                }
                let original_archive_sha256 = bundle["archive_sha256"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| digest(events));
                let selected = sadr_cli::candidate_execution::observation_action_with_provenance(
                    &signal,
                    &spec,
                    manifest,
                    events,
                    &original_archive_sha256,
                    bundle["universe_sha256"].as_str(),
                )
                .map_err(|_| "candidate_helper_observation_action_invalid")?;
                let (mapped_status, mapped_gap) =
                    sadr_cli::candidate_execution::observation_status_for_action(&selected);
                status = mapped_status;
                if let Some(gap) = mapped_gap {
                    gaps.push(gap.to_string());
                }
                if bundle["budget_truncated"] == true {
                    gaps.push("observation_coverage_budget_truncated".to_string());
                }
                action = Some(selected);
            }
        }
    } else {
        gaps.push("no_ranked_key".to_string());
    }
    let quote = action.as_ref().and_then(|action| action.quote.clone());
    let entry = match (
        action.as_ref().and_then(|action| action.entry.clone()),
        quote.clone(),
    ) {
        (Some(entry), Some(quote)) => Some(sadr_cli::candidate_execution::ObservationEntryV1 {
            key: entry.key,
            units: entry.units,
            max_price: entry.max_price,
            signal_ms: target.signal_ms,
            decision_at_ms,
            quote,
            virtual_observation: true,
        }),
        _ => None,
    };
    let key = action
        .as_ref()
        .and_then(|action| action.key.clone())
        .or_else(|| ranked.and_then(|tick| tick.candidate_top_n.first().cloned()));
    let mut score = sadr_cli::candidate_execution::ObservationScoreV1 {
        version: 1,
        abi: sadr_cli::candidate_execution::OBSERVATION_ABI.into(),
        candidate_id: launch.candidate_id.clone(),
        task_id: launch.task_id.clone(),
        revision: launch.revision,
        source_task_id: target.source_task_id.clone(),
        source_revision: target.source_revision,
        source_candidate_id: target.source_candidate_id.clone(),
        source_result_sha256: target.source_result_sha256.clone(),
        key,
        authoring_at_ms: target.authoring_at_ms,
        target_frozen_at_ms: target.target_frozen_at_ms,
        features_at_ms,
        quote_at_ms: quote.as_ref().map(|quote| quote.receipt_ms),
        score_produced_at_ms,
        decision_at_ms,
        exit_due_ms: decision_at_ms.saturating_add(target.holding_ms),
        quote,
        entry,
        status: status.into(),
        score: report.mean_top_n_overlap,
        score_metric: "mean_top_n_overlap".into(),
        score_sha256: report.report_hash.clone(),
        observation_sha256: String::new(),
        observation_scope: sadr_cli::candidate_execution::OBSERVATION_SCOPE.into(),
        policy_sha256: target.policy_sha256.clone(),
        gaps,
    };
    sadr_cli::candidate_execution::validate_observation_score(&score)
        .map_err(|_| "candidate_helper_observation_score_invalid")?;
    score.observation_sha256 = sadr_cli::candidate_execution::observation_artifact_sha256(&score)
        .map_err(|_| "candidate_helper_observation_hash_invalid")?;
    Ok(CandidateHelperOutput {
        version: 1,
        abi: "candidate_helper_output_v1".into(),
        job_id: launch.job_id.clone(),
        lease: launch.lease.clone(),
        task_id: launch.task_id.clone(),
        revision: launch.revision,
        candidate_id: launch.candidate_id.clone(),
        source_sha256: launch.source_sha256.clone(),
        params_sha256: launch.params_sha256.clone(),
        dataset_hash: launch.dataset_hash.clone(),
        runtime_hash: launch.runtime_binding.runtime_hash.clone(),
        result_produced_at_ms: score_produced_at_ms,
        report,
        advisory,
        execution: None,
        observation: Some(score),
    })
}

pub(crate) fn write_helper_output(
    stage: &Path,
    output: &CandidateHelperOutput,
    max_result_bytes: u32,
) -> std::result::Result<(), &'static str> {
    let bytes = serde_json::to_vec(output).map_err(|_| "candidate_helper_result_invalid")?;
    let cap = max_result_bytes as usize;
    if cap < jobs::CANDIDATE_RESULT_MIN_BYTES as usize
        || cap > jobs::CANDIDATE_RESULT_MAX_BYTES as usize
    {
        return Err("candidate_helper_result_invalid");
    }
    let body = if bytes.len() > cap {
        let failure = helper_failure_record(
            &output.job_id,
            &output.lease,
            &output.task_id,
            output.revision,
            &output.candidate_id,
            "candidate_helper_result_too_large",
            &bytes,
            output.result_produced_at_ms,
        );
        serde_json::to_vec(&failure).map_err(|_| "candidate_helper_result_invalid")?
    } else {
        bytes
    };
    if body.len() > cap {
        return Err("candidate_helper_result_invalid");
    }
    let temporary = stage.join("result.json.tmp");
    write_new_private(&temporary, &body, 0o600)?;
    let final_path = stage.join("result.json");
    if final_path.exists() {
        return Err("candidate_helper_result_exists");
    }
    fs::rename(&temporary, &final_path).map_err(|_| "candidate_helper_result_unavailable")?;
    fs::File::open(stage)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "candidate_helper_result_unavailable")?;
    Ok(())
}

/// Claim the one allowed helper run before any untrusted source is read. The
/// marker is created with `O_EXCL`, written and fsynced, so a systemd restart or
/// a manual unit start cannot execute the payload twice. An existing marker or
/// result is a refusal, never a replay.
pub(crate) fn claim_helper_run(
    launch: &worker::CandidateLaunch,
) -> std::result::Result<(), &'static str> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let stage = Path::new(&launch.stage_dir);
    let meta = fs::symlink_metadata(stage).map_err(|_| "candidate_helper_stage_unavailable")?;
    let owner = fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(u32::MAX);
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != owner
        || meta.permissions().mode() & 0o077 != 0
    {
        return Err("candidate_helper_stage_invalid");
    }
    if stage.join("result.json").exists() {
        return Err("candidate_helper_result_exists");
    }
    let marker = stage.join("run.marker");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&marker)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                "candidate_helper_run_already_started"
            } else {
                "candidate_helper_marker_unavailable"
            }
        })?;
    let body = format!(
        "{}:{}:{}:{}",
        launch.job_id, launch.lease, launch.task_id, launch.started_ms
    );
    file.write_all(body.as_bytes())
        .map_err(|_| "candidate_helper_marker_unavailable")?;
    file.sync_all()
        .map_err(|_| "candidate_helper_marker_unavailable")?;
    drop(file);
    fs::File::open(stage)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| "candidate_helper_marker_unavailable")?;
    Ok(())
}

/// Hidden CLI entry point. Fixed argv only: one launch manifest path.
pub(crate) fn helper_entry(path: &Path) -> Result<()> {
    let bytes = read_stage_file(path, 65_536).map_err(|reason| reason.to_string())?;
    let launch: worker::CandidateLaunch = serde_json::from_slice(&bytes)
        .map_err(|_| "candidate_helper_launch_invalid".to_string())?;
    claim_helper_run(&launch).map_err(|reason| reason.to_string())?;
    let output = if launch.observation.is_some() {
        run_observation_at(&launch, now_ms, true).map_err(|reason| reason.to_string())?
    } else {
        run_helper_at(&launch, now_ms, true).map_err(|reason| reason.to_string())?
    };
    write_helper_output(
        Path::new(&launch.stage_dir),
        &output,
        launch.unit_policy.max_result_bytes,
    )
    .map_err(|reason| reason.to_string())?;
    Ok(())
}

fn record_stopped_termination(
    db: &mut Connection,
    launch: &worker::CandidateLaunch,
    stopped: &CandidateStopped,
    cause: jobs::CandidateStopCause,
    now: i64,
) -> std::result::Result<jobs::Job, &'static str> {
    let hash = stopped_hash(stopped)?;
    jobs::record_candidate_termination(
        db,
        &launch.job_id,
        &launch.lease,
        &hash,
        cause,
        now.max(stopped.stopped_at_ms),
    )
    .map_err(|_| "candidate_termination_failed")
}

fn finish_unstarted(
    db: &mut Connection,
    launch: &worker::CandidateLaunch,
    runner: &mut dyn worker::CandidateUnitRunner,
    outcome: &str,
    now: i64,
) -> std::result::Result<(), &'static str> {
    let proof = runner
        .terminate(launch)
        .map_err(|_| "candidate_recovery_required")?;
    let stopped = candidate_stopped(
        launch,
        proof,
        jobs::CandidateStopCause::Failure,
        CandidateStopOutcome::LaunchFailed,
        Some(outcome),
        None,
        None,
        None,
        now,
    );
    write_stopped(db, launch, &stopped)?;
    record_stopped_termination(db, launch, &stopped, jobs::CandidateStopCause::Failure, now)?;
    Ok(())
}

fn helper_execution_valid(
    launch: &worker::CandidateLaunch,
    output: &CandidateHelperOutput,
) -> bool {
    match (&launch.execution, &output.execution) {
        (None, None) => true,
        (Some(binding), Some(execution)) => {
            if execution.execution_spec_sha256 != binding.spec_sha256
                || execution.manifest_sha256 != binding.manifest_sha256
                || execution.events_sha256 != binding.events_sha256
                || sadr_cli::candidate_execution::verify_report(execution).is_err()
            {
                return false;
            }
            let Ok(spec) = serde_json::from_value::<
                sadr_cli::candidate_execution::CandidateExecutionSpecV1,
            >(execution.execution_spec.clone()) else {
                return false;
            };
            let Ok(signals) =
                sadr_cli::candidate_execution::signals_from_ranking(&output.report, &spec)
            else {
                return false;
            };
            signals == execution.scored_signals
        }
        _ => false,
    }
}

fn verify_helper_output(
    launch: &worker::CandidateLaunch,
    output: &CandidateHelperOutput,
    now: i64,
) -> std::result::Result<(), &'static str> {
    if output.version != 1
        || output.abi != "candidate_helper_output_v1"
        || output.job_id != launch.job_id
        || output.lease != launch.lease
        || output.task_id != launch.task_id
        || output.revision != launch.revision
        || output.candidate_id != launch.candidate_id
        || output.source_sha256 != launch.source_sha256
        || output.params_sha256 != launch.params_sha256
        || output.dataset_hash != launch.dataset_hash
        || output.runtime_hash != launch.runtime_binding.runtime_hash
        || output.result_produced_at_ms < launch.started_ms
        || output.result_produced_at_ms > now
        || !output.advisory.started
        || !output.advisory.leader_reaped
        || output.report.scorer_sha256 != launch.source_sha256
        || output.report.params_sha256 != launch.params_sha256
        || output.report.dataset_hash != launch.dataset_hash
        || output.report.horizons.len() != 1
        || output.report.ticks.len() != output.report.snapshots
        || output
            .report
            .valid_snapshots
            .saturating_add(output.report.failed_snapshots)
            != output.report.snapshots
        || output.report.snapshots == 0
        || output.report.min_valid_snapshots == 0
        || (output.report.status == "ok") != output.report.reason.is_none()
        || !output.report.verify_hash()
        || !helper_execution_valid(launch, output)
        || !helper_observation_valid(launch, output)
    {
        return Err("candidate_result_invalid");
    }
    Ok(())
}

fn helper_observation_valid(
    launch: &worker::CandidateLaunch,
    output: &CandidateHelperOutput,
) -> bool {
    match (&launch.observation, &output.observation) {
        (None, None) => true,
        (Some(target), Some(score)) => {
            if sadr_cli::candidate_execution::validate_observation_score(score).is_err() {
                return false;
            }
            if score.version != 1
                || score.abi != sadr_cli::candidate_execution::OBSERVATION_ABI
                || score.candidate_id != launch.candidate_id
                || score.task_id != launch.task_id
                || score.revision != launch.revision
                || score.source_task_id != target.source_task_id
                || score.source_revision != target.source_revision
                || score.source_candidate_id != target.source_candidate_id
                || score.source_result_sha256 != target.source_result_sha256
                || score.authoring_at_ms != target.authoring_at_ms
                || score.target_frozen_at_ms != target.target_frozen_at_ms
                || score.score_produced_at_ms != output.result_produced_at_ms
                || score.decision_at_ms != output.result_produced_at_ms
                || score.exit_due_ms != score.decision_at_ms.saturating_add(target.holding_ms)
                || score.observation_scope != sadr_cli::candidate_execution::OBSERVATION_SCOPE
                || score.policy_sha256 != target.policy_sha256
                || score.score_sha256 != output.report.report_hash
                || score.score != output.report.mean_top_n_overlap
                || score.features_at_ms
                    != output
                        .report
                        .ticks
                        .iter()
                        .rev()
                        .find(|tick| tick.status == "ok" && !tick.candidate_top_n.is_empty())
                        .map(|tick| tick.observed_ms)
                        .unwrap_or(0)
                || !score.score.is_none_or(f64::is_finite)
                || score.observation_sha256
                    != sadr_cli::candidate_execution::observation_artifact_sha256(
                        &sadr_cli::candidate_execution::ObservationScoreV1 {
                            observation_sha256: String::new(),
                            ..score.clone()
                        },
                    )
                    .unwrap_or_default()
                || !matches!(
                    score.status.as_str(),
                    "scored_entry"
                        | "scored_missed_freshness"
                        | "entry_quote_unavailable"
                        | "entry_instrument_unsupported"
                        | "entry_price_invalid"
                        | "scored_late"
                        | "scored_no_ranked_key"
                        | "scored_no_score"
                )
                || (score.status == "scored_no_score" && score.score.is_some())
                || (score.status != "scored_no_score" && !score.score.is_some_and(f64::is_finite))
            {
                return false;
            }
            match (&score.entry, &score.quote) {
                (Some(entry), Some(quote)) => {
                    entry.virtual_observation
                        && entry.key == quote.key
                        && entry.decision_at_ms == score.decision_at_ms
                        && entry.signal_ms == target.signal_ms
                        && quote.purpose == "research_observation"
                        && quote.source_ms <= quote.receipt_ms
                        && quote.receipt_ms <= quote.available_at_ms
                        && quote.available_at_ms <= quote.event_at_ms
                        && quote.event_at_ms <= score.decision_at_ms
                        && quote.source_age_ms == score.decision_at_ms - quote.source_ms
                        && quote.receipt_age_ms == score.decision_at_ms - quote.receipt_ms
                        && quote.source_age_ms <= sadr_core::market::MAX_FINANCIAL_AGE_MS
                        && quote.receipt_age_ms <= sadr_core::market::MAX_FINANCIAL_AGE_MS
                        && score.status == "scored_entry"
                        && score.key.as_deref() == Some(entry.key.as_str())
                        && score.quote_at_ms == Some(quote.receipt_ms)
                        && score.gaps.is_empty()
                }
                (None, _) => score.status != "scored_entry" && !score.gaps.is_empty(),
                _ => false,
            }
        }
        _ => false,
    }
}

fn observation_score_valid(
    task: &jobs::CandidateTaskV1,
    launch: &worker::CandidateLaunch,
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
    report: &ScorerReplayReport,
) -> std::result::Result<(), &'static str> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.purpose else {
        return Err("candidate_result_invalid");
    };
    sadr_cli::candidate_execution::validate_observation_score(score)
        .map_err(|_| "candidate_result_invalid")?;
    if score.version != 1
        || score.abi != sadr_cli::candidate_execution::OBSERVATION_ABI
        || score.candidate_id != task.candidate_id
        || score.task_id != task.task_id
        || score.revision != task.revision
        || score.source_task_id != target.source_task_id
        || score.source_revision != target.source_revision
        || score.source_candidate_id != target.source_candidate_id
        || score.source_result_sha256 != target.source_result_sha256
        || score.authoring_at_ms != target.authoring_at_ms
        || score.target_frozen_at_ms != target.target_frozen_at_ms
        || score.features_at_ms > target.signal_ms
        || score.features_at_ms < target.window_from_ms
        || score.score_produced_at_ms < launch.started_ms
        || score.decision_at_ms != score.score_produced_at_ms
        || score.exit_due_ms != score.decision_at_ms.saturating_add(target.holding_ms)
        || score.observation_scope != sadr_cli::candidate_execution::OBSERVATION_SCOPE
        || score.policy_sha256 != target.policy_sha256
        || score.score_sha256 != report.report_hash
        || score.score != report.mean_top_n_overlap
        || !score.score.is_none_or(f64::is_finite)
        || score.observation_sha256
            != sadr_cli::candidate_execution::observation_artifact_sha256(
                &sadr_cli::candidate_execution::ObservationScoreV1 {
                    observation_sha256: String::new(),
                    ..score.clone()
                },
            )
            .unwrap_or_default()
        || !matches!(
            score.status.as_str(),
            "scored_entry"
                | "scored_missed_freshness"
                | "entry_quote_unavailable"
                | "entry_instrument_unsupported"
                | "entry_price_invalid"
                | "scored_late"
                | "scored_no_ranked_key"
                | "scored_no_score"
        )
        || (score.status == "scored_no_score" && score.score.is_some())
        || (score.status != "scored_no_score" && !score.score.is_some_and(f64::is_finite))
    {
        return Err("candidate_result_invalid");
    }
    let ranked_tick = report
        .ticks
        .iter()
        .rev()
        .find(|tick| tick.status == "ok" && !tick.candidate_top_n.is_empty());
    let ranked_key = ranked_tick.and_then(|tick| tick.candidate_top_n.first().cloned());
    if score.key != ranked_key
        || ranked_tick.is_some_and(|tick| score.features_at_ms != tick.observed_ms)
    {
        return Err("candidate_result_invalid");
    }
    match (&score.entry, &score.quote) {
        (Some(entry), Some(quote)) => {
            if !entry.virtual_observation
                || entry.key != quote.key
                || entry.signal_ms != target.signal_ms
                || entry.decision_at_ms != score.decision_at_ms
                || quote.purpose != "research_observation"
                || quote.source_ms <= 0
                || quote.receipt_ms <= 0
                || quote.source_ms > quote.receipt_ms
                || quote.receipt_ms > quote.available_at_ms
                || quote.available_at_ms > quote.event_at_ms
                || quote.event_at_ms > score.decision_at_ms
                || quote.source_age_ms != score.decision_at_ms - quote.source_ms
                || quote.receipt_age_ms != score.decision_at_ms - quote.receipt_ms
                || quote.source_age_ms > sadr_core::market::MAX_FINANCIAL_AGE_MS
                || quote.receipt_age_ms > sadr_core::market::MAX_FINANCIAL_AGE_MS
                || quote.evidence_status != "ok"
                || score.status != "scored_entry"
                || score.quote_at_ms != Some(quote.receipt_ms)
                || !score.gaps.is_empty()
            {
                return Err("candidate_result_invalid");
            }
        }
        (None, quote) => {
            if score.status == "scored_entry" || score.gaps.is_empty() {
                return Err("candidate_result_invalid");
            }
            // Early-return statuses have no eligible ranked key/score. They
            // must not carry an unsupported or fabricated quote; only the
            // explicit stale/price-invalid statuses may carry a real receipt,
            // and that receipt is checked against the frozen tape later.
            if matches!(
                score.status.as_str(),
                "scored_no_ranked_key" | "scored_no_score" | "scored_late"
            ) && quote.is_some()
            {
                return Err("candidate_result_invalid");
            }
        }
        _ => return Err("candidate_result_invalid"),
    }
    Ok(())
}

pub(crate) fn candidate_result_envelope(
    launch: &worker::CandidateLaunch,
    task: &jobs::CandidateTaskRecord,
    output: CandidateHelperOutput,
    proof: worker::CandidateUnitProof,
) -> std::result::Result<CandidateResultV1, &'static str> {
    if output.report.horizons[0].horizon_minutes != task.spec.horizon_minutes
        || output.report.horizons[0].cost_adjusted.is_some()
        || !output.report.verify_hash()
    {
        return Err("candidate_result_invalid");
    }
    if output.execution.is_some() != task.spec.execution.is_some() {
        return Err("candidate_result_invalid");
    }
    if let (Some(execution), Some(binding)) = (&output.execution, &task.spec.execution)
        && (execution.execution_spec_sha256 != binding.spec_sha256
            || execution.manifest_sha256 != binding.manifest_sha256
            || execution.events_sha256 != binding.events_sha256)
    {
        return Err("candidate_result_invalid");
    }
    let report_sha256 = output.report.report_hash.clone();
    let execution = output.execution.clone();
    let observation = output.observation.clone();
    let is_observation = task.spec.purpose.is_some();
    if is_observation != observation.is_some() || (is_observation && execution.is_some()) {
        return Err("candidate_result_invalid");
    }
    if let Some(score) = &observation {
        observation_score_valid(&task.spec, launch, score, &output.report)?;
    }
    let qualified = !is_observation && execution.is_none() && output.report.qualified();
    let analysis_status = if is_observation || execution.is_some() {
        if output.report.status == "ok" {
            "insufficient_evidence"
        } else {
            "failed"
        }
    } else if qualified {
        "qualified"
    } else if output.report.status == "ok" {
        "insufficient_evidence"
    } else {
        "failed"
    };
    let result_kind = if is_observation {
        sadr_cli::candidate_execution::RESULT_KIND_OBSERVATION
    } else if execution.is_some() {
        sadr_cli::candidate_execution::RESULT_KIND_EXECUTED
    } else {
        CANDIDATE_RESULT_KIND
    };
    let mut gaps: Vec<String> = if is_observation {
        CANDIDATE_OBSERVATION_GAPS
            .iter()
            .map(|gap| (*gap).to_owned())
            .collect()
    } else if execution.is_some() {
        CANDIDATE_EXECUTION_GAPS
            .iter()
            .map(|gap| (*gap).to_owned())
            .collect()
    } else {
        CANDIDATE_GAPS.iter().map(|gap| (*gap).to_owned()).collect()
    };
    if let Some(score) = &observation {
        for gap in &score.gaps {
            if !gaps.contains(gap) {
                gaps.push(gap.clone());
            }
        }
    }
    let evidence_scope = if is_observation {
        sadr_cli::candidate_execution::OBSERVATION_SCOPE
    } else {
        CANDIDATE_EVIDENCE_SCOPE
    };
    let unit_proof_sha256 = proof.proof_sha256.clone();
    Ok(CandidateResultV1 {
        version: 2,
        task_id: launch.task_id.clone(),
        revision: launch.revision,
        job_id: launch.job_id.clone(),
        lease: launch.lease.clone(),
        candidate_id: launch.candidate_id.clone(),
        lifecycle_candidate_id: launch.lifecycle_candidate_id.clone(),
        abi: launch.abi.clone(),
        source_sha256: launch.source_sha256.clone(),
        params_sha256: launch.params_sha256.clone(),
        dataset_id: launch.dataset_id.clone(),
        dataset_hash: launch.dataset_hash.clone(),
        windows: task.spec.windows.clone(),
        horizon_minutes: task.spec.horizon_minutes,
        runtime_binding: launch.runtime_binding.clone(),
        sandbox: launch.sandbox.clone(),
        unit_policy: launch.unit_policy.clone(),
        unit_proof: proof,
        unit_proof_sha256,
        result_produced_at_ms: output.result_produced_at_ms,
        result_kind: result_kind.into(),
        evidence_scope: evidence_scope.into(),
        partial_evidence: true,
        gaps,
        advisory: output.advisory,
        qualified,
        analysis_status: analysis_status.into(),
        report: output.report,
        report_sha256,
        execution,
        observation,
        promotion_authorized: false,
        activation_authorized: false,
        edge_qualified: false,
        host_qualified: false,
    })
}

#[allow(clippy::too_many_arguments)]
fn helper_failure_record(
    job_id: &str,
    lease: &str,
    task_id: &str,
    revision: u32,
    candidate_id: &str,
    code: &str,
    payload: &[u8],
    at: i64,
) -> CandidateHelperFailure {
    CandidateHelperFailure {
        version: 1,
        abi: CANDIDATE_HELPER_FAILURE_ABI.into(),
        job_id: job_id.to_owned(),
        lease: lease.to_owned(),
        task_id: task_id.to_owned(),
        revision,
        candidate_id: candidate_id.to_owned(),
        code: code.into(),
        payload_sha256: digest(payload),
        payload_bytes: payload.len() as u64,
        result_produced_at_ms: at,
    }
}

fn verify_helper_failure(
    launch: &worker::CandidateLaunch,
    failure: &CandidateHelperFailure,
    now: i64,
) -> std::result::Result<(), &'static str> {
    if failure.version != 1
        || failure.abi != CANDIDATE_HELPER_FAILURE_ABI
        || failure.job_id != launch.job_id
        || failure.lease != launch.lease
        || failure.task_id != launch.task_id
        || failure.revision != launch.revision
        || failure.candidate_id != launch.candidate_id
        || failure.code.is_empty()
        || failure.code.len() > 64
        || !failure
            .code
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || !failure.payload_sha256.len().eq(&64)
        || !failure
            .payload_sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || failure.payload_bytes == 0
        || failure.payload_bytes > 64 * 1024 * 1024
        || failure.result_produced_at_ms < launch.started_ms
        || failure.result_produced_at_ms > now
    {
        return Err("candidate_result_invalid");
    }
    Ok(())
}

fn store_result_artifact(
    db: &mut Connection,
    launch: &worker::CandidateLaunch,
    bytes: &[u8],
    now: i64,
) -> std::result::Result<(String, u64), &'static str> {
    let cap = launch.unit_policy.max_result_bytes as usize;
    if bytes.is_empty() || bytes.len() > cap {
        return Ok((digest(bytes), bytes.len() as u64));
    }
    let hash = jobs::put_candidate_artifact_bounded(db, "result", bytes, cap, now)
        .map_err(|_| "candidate_result_store_failed")?;
    let stored = jobs::read_candidate_artifact(db, &hash, "result", cap)
        .map_err(|_| "candidate_result_store_failed")?;
    if stored != bytes || digest(&stored) != hash {
        return Err("candidate_result_store_failed");
    }
    Ok((hash, stored.len() as u64))
}

/// Pure envelope validation shared by the live completion path, crash
/// recovery and historical reads. It never requires the task to be `Succeeded`
/// first, so a corrupt stored payload is rejected before any terminal state is
/// written. It uses only the recorded task/launch/proof/artifact bytes, never
/// current config or current executable bytes. For an executable task it also
/// reads the stored execution spec/manifest/events and re-verifies the recorded
/// report against those exact frozen bytes, so a tampered execution artifact
/// cannot reconcile a task to `Succeeded`.
fn verify_observation_artifacts(
    db: &Connection,
    task: &jobs::CandidateTaskV1,
    target: &jobs::ObservationTargetV1,
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
    report: &ScorerReplayReport,
) -> std::result::Result<(), &'static str> {
    let spec_bytes =
        jobs::read_candidate_artifact(db, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "candidate_result_unverified")?;
    let bundle_bytes =
        jobs::read_candidate_artifact(db, &target.quotes_sha256, "observation_quotes", 2_000_000)
            .map_err(|_| "candidate_result_unverified")?;
    if digest(&spec_bytes) != target.spec_sha256
        || digest(&bundle_bytes) != target.quotes_sha256
        || target.snapshot_sha256 != task.dataset_hash
    {
        return Err("candidate_result_unverified");
    }
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes).map_err(|_| "candidate_result_unverified")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_result_unverified")?;
    if spec.parameters.training_from_ms != task.windows.training_from_ms
        || spec.parameters.training_to_ms != task.windows.training_to_ms
        || spec.parameters.selection_from_ms != target.window_from_ms
        || spec.parameters.selection_to_ms != task.windows.selection_to_ms
        || spec.parameters.holding_ms != target.holding_ms
        || spec.parameters.max_exit_delay_ms != target.max_exit_delay_ms
        || spec.units != score.entry.as_ref().map_or(spec.units, |entry| entry.units)
    {
        return Err("candidate_result_unverified");
    }
    let bundle: Value =
        serde_json::from_slice(&bundle_bytes).map_err(|_| "candidate_result_unverified")?;
    if bundle["version"] != 1 || bundle["status"] != "ok" {
        return Err("candidate_result_unverified");
    }
    // The frozen bundle is the ONLY authority for the target's metric fields.
    // A caller-supplied in-range count/flag/delay is refused here too.
    if validate_frozen_target_metrics(&bundle, target).is_err() {
        return Err("candidate_result_unverified");
    }
    let manifest = bundle["manifest"]
        .as_str()
        .ok_or("candidate_result_unverified")?
        .as_bytes();
    let events = bundle["events"]
        .as_str()
        .ok_or("candidate_result_unverified")?
        .as_bytes();
    if bundle["manifest_sha256"] != digest(manifest) || bundle["events_sha256"] != digest(events) {
        return Err("candidate_result_unverified");
    }
    let Some(tick) = report
        .ticks
        .iter()
        .rev()
        .find(|tick| tick.status == "ok" && !tick.candidate_top_n.is_empty())
    else {
        // No eligible ranked key: the score may record the explicit no-rank /
        // no-score status, but it must NOT carry a quote. A fabricated quote
        // with a recomputed self-hash is refused here, before measure counts it.
        return if score.entry.is_none()
            && score.quote.is_none()
            && matches!(
                score.status.as_str(),
                "scored_no_ranked_key" | "scored_no_score"
            ) {
            Ok(())
        } else {
            Err("candidate_result_unverified")
        };
    };
    if score.features_at_ms != tick.observed_ms {
        return Err("candidate_result_unverified");
    }
    if score.status == "scored_no_score" {
        return if score.entry.is_none() && score.score.is_none() && score.quote.is_none() {
            Ok(())
        } else {
            Err("candidate_result_unverified")
        };
    }
    let signal = sadr_cli::candidate_execution::ScoredSignal {
        at_ms: score.decision_at_ms,
        ordered_keys: tick.candidate_top_n.clone(),
    };
    if score.decision_at_ms > spec.parameters.selection_to_ms {
        return if score.entry.is_none() && score.quote.is_none() && score.status == "scored_late" {
            Ok(())
        } else {
            Err("candidate_result_unverified")
        };
    }
    let original_archive_sha256 = bundle["archive_sha256"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| digest(events));
    let action = sadr_cli::candidate_execution::observation_action_with_provenance(
        &signal,
        &spec,
        manifest,
        events,
        &original_archive_sha256,
        bundle["universe_sha256"].as_str(),
    )
    .map_err(|_| "candidate_result_unverified")?;
    sadr_cli::candidate_execution::validate_observation_action_binding(score, &action)
        .map_err(|_| "candidate_result_unverified")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn verify_candidate_result_envelope(
    db: &Connection,
    task: &jobs::CandidateTaskV1,
    launch: &worker::CandidateLaunch,
    proof: &worker::CandidateUnitProof,
    result_hash: &str,
    result_at_ms: i64,
    allow_late: bool,
    result_bytes: &[u8],
) -> std::result::Result<CandidateResultV1, &'static str> {
    if digest(result_bytes) != result_hash || !proof.verify(launch) {
        return Err("candidate_result_unverified");
    }
    let result: CandidateResultV1 =
        serde_json::from_slice(result_bytes).map_err(|_| "candidate_result_unverified")?;
    let is_observation = task.purpose.is_some();
    let expected_result_kind = if is_observation {
        sadr_cli::candidate_execution::RESULT_KIND_OBSERVATION
    } else if task.execution.is_some() {
        sadr_cli::candidate_execution::RESULT_KIND_EXECUTED
    } else {
        CANDIDATE_RESULT_KIND
    };
    let expected_gaps: &[&str] = if is_observation {
        &CANDIDATE_OBSERVATION_GAPS
    } else if task.execution.is_some() {
        &CANDIDATE_EXECUTION_GAPS
    } else {
        &CANDIDATE_GAPS
    };
    let observation_valid = match (&task.purpose, &result.observation) {
        (None, None) => true,
        (Some(jobs::CandidatePurposeV1::Observe { target }), Some(score)) => {
            observation_score_valid(task, launch, score, &result.report).is_ok()
                && verify_observation_artifacts(db, task, target, score, &result.report).is_ok()
                && result.execution.is_none()
        }
        _ => false,
    };
    let execution_valid = match (&task.execution, &result.execution) {
        (None, None) => true,
        (Some(binding), Some(execution)) => execution.execution_spec_sha256 == binding.spec_sha256
            && execution.manifest_sha256 == binding.manifest_sha256
            && execution.events_sha256 == binding.events_sha256
            && sadr_cli::candidate_execution::verify_report(execution).is_ok()
            && serde_json::from_value::<sadr_cli::candidate_execution::CandidateExecutionSpecV1>(
                execution.execution_spec.clone(),
            )
            .is_ok_and(|spec| {
                spec.parameters.training_from_ms == task.windows.training_from_ms
                    && spec.parameters.training_to_ms == task.windows.training_to_ms
                    && spec.parameters.selection_from_ms == task.windows.selection_from_ms
                    && spec.parameters.selection_to_ms == task.windows.selection_to_ms
                    && spec.parameters.holding_ms == task.horizon_minutes.saturating_mul(60_000)
                    && spec.parameters.max_exit_delay_ms == task.max_exit_delay_ms
                    && sadr_cli::candidate_execution::signals_from_ranking(&result.report, &spec)
                        .is_ok_and(|signals| signals == execution.scored_signals)
            }),
        _ => false,
    };
    if let (Some(binding), Some(execution)) = (&task.execution, &result.execution) {
        let spec =
            jobs::read_candidate_artifact(db, &binding.spec_sha256, "execution_spec", 65_536)
                .map_err(|_| "candidate_result_unverified")?;
        let manifest = jobs::read_candidate_artifact(
            db,
            &binding.manifest_sha256,
            "execution_manifest",
            512 * 1024,
        )
        .map_err(|_| "candidate_result_unverified")?;
        let events = jobs::read_candidate_artifact(
            db,
            &binding.events_sha256,
            "execution_events",
            16 * 1024 * 1024,
        )
        .map_err(|_| "candidate_result_unverified")?;
        if digest(&spec) != binding.spec_sha256
            || digest(&manifest) != binding.manifest_sha256
            || digest(&events) != binding.events_sha256
            || sadr_cli::candidate_execution::verify_report_against_manifest(
                execution, &manifest, &events,
            )
            .is_err()
        {
            return Err("candidate_result_unverified");
        }
    }
    if serde_json::to_vec(&result).map_err(|_| "candidate_result_unverified")? != result_bytes
        || result.version != 2
        || result.task_id != task.task_id
        || result.revision != task.revision
        || result.job_id != task.job_id
        || result.lease != launch.lease
        || result.candidate_id != task.candidate_id
        || result.lifecycle_candidate_id != task.lifecycle_candidate_id
        || result.abi != task.abi
        || result.source_sha256 != task.source_sha256
        || result.params_sha256 != task.params_sha256
        || result.dataset_id != task.dataset_id
        || result.dataset_hash != task.dataset_hash
        || result.windows != task.windows
        || result.horizon_minutes != task.horizon_minutes
        || result.runtime_binding != task.runtime_binding
        || result.sandbox != task.sandbox
        || result.unit_policy != task.unit_policy
        || result.unit_proof != *proof
        || result.unit_proof_sha256 != proof.proof_sha256
        || result.result_produced_at_ms != result_at_ms
        || result_at_ms < launch.started_ms
        || (!allow_late && result_at_ms >= launch.task_deadline_ms)
        || result.result_kind != expected_result_kind
        || result.evidence_scope
            != if is_observation {
                sadr_cli::candidate_execution::OBSERVATION_SCOPE
            } else {
                CANDIDATE_EVIDENCE_SCOPE
            }
        || !result.partial_evidence
        || result.promotion_authorized
        || result.activation_authorized
        || result.edge_qualified
        || result.host_qualified
        || !execution_valid
        || !observation_valid
        || result.qualified
            != (!is_observation && task.execution.is_none() && result.report.qualified())
        || result.analysis_status
            != if is_observation || task.execution.is_some() {
                if result.report.status == "ok" {
                    "insufficient_evidence"
                } else {
                    "failed"
                }
            } else if result.qualified {
                "qualified"
            } else if result.report.status == "ok" {
                "insufficient_evidence"
            } else {
                "failed"
            }
        || !expected_gaps
            .iter()
            .all(|gap| result.gaps.iter().any(|value| value == gap))
        || result.report_sha256 != result.report.report_hash
        || !result.report.verify_hash()
        || result.report.scorer_sha256 != task.source_sha256
        || result.report.params_sha256 != task.params_sha256
        || result.report.dataset_hash != task.dataset_hash
        || result.report.horizons.len() != 1
        || result.report.horizons[0].horizon_minutes != task.horizon_minutes
        || result.report.horizons[0].cost_adjusted.is_some()
        || result.report.ticks.len() != result.report.snapshots
        || result
            .report
            .valid_snapshots
            .saturating_add(result.report.failed_snapshots)
            != result.report.snapshots
        || (result.report.status == "ok") != result.report.reason.is_none()
        || forbidden_pnl_key(
            &serde_json::to_value(&result).map_err(|_| "candidate_result_unverified")?,
        )
    {
        return Err("candidate_result_unverified");
    }
    Ok(result)
}

fn finish_exited(
    db: &mut Connection,
    launch: &worker::CandidateLaunch,
    task: &jobs::CandidateTaskRecord,
    proof: worker::CandidateUnitProof,
    result: Vec<u8>,
    now: i64,
) -> std::result::Result<(), &'static str> {
    if !proof.verify(launch) {
        return Err("candidate_unit_proof_invalid");
    }
    let record = jobs::load_candidate(db, &launch.task_id, launch.revision)
        .map_err(|_| "candidate_task_state_unavailable")?;
    let operator_cancel = record.cancel_receipt.is_some();
    let stopped_failure = |db: &mut Connection,
                           cause: jobs::CandidateStopCause,
                           outcome: CandidateStopOutcome,
                           failure_code: Option<&str>,
                           result_sha256: Option<String>,
                           result_at_ms: Option<i64>,
                           result_bytes: Option<u64>,
                           at: i64|
     -> std::result::Result<(), &'static str> {
        let stopped = candidate_stopped(
            launch,
            proof.clone(),
            cause,
            outcome,
            failure_code,
            result_sha256,
            result_at_ms,
            result_bytes,
            at,
        );
        write_stopped(db, launch, &stopped)?;
        record_stopped_termination(db, launch, &stopped, cause, at)?;
        Ok(())
    };
    if result.is_empty() {
        return stopped_failure(
            db,
            if operator_cancel {
                jobs::CandidateStopCause::OperatorCancel
            } else {
                jobs::CandidateStopCause::Failure
            },
            if operator_cancel {
                CandidateStopOutcome::Cancelled
            } else {
                CandidateStopOutcome::NoResult
            },
            None,
            None,
            None,
            None,
            now,
        );
    }
    let output = serde_json::from_slice::<CandidateHelperOutput>(&result).ok();
    if let Some(output) = output {
        if verify_helper_output(launch, &output, now).is_err() {
            let (hash, bytes) = store_result_artifact(db, launch, &result, now)?;
            return stopped_failure(
                db,
                if operator_cancel {
                    jobs::CandidateStopCause::OperatorCancel
                } else {
                    jobs::CandidateStopCause::Failure
                },
                if operator_cancel {
                    CandidateStopOutcome::Cancelled
                } else {
                    CandidateStopOutcome::InvalidResult
                },
                Some("candidate_result_invalid"),
                Some(hash),
                Some(output.result_produced_at_ms),
                Some(bytes),
                now,
            );
        }
        if operator_cancel {
            let (hash, bytes) = store_result_artifact(db, launch, &result, now)?;
            return stopped_failure(
                db,
                jobs::CandidateStopCause::OperatorCancel,
                CandidateStopOutcome::Cancelled,
                None,
                Some(hash),
                Some(output.result_produced_at_ms),
                Some(bytes),
                now,
            );
        }
        let within_deadline = output.result_produced_at_ms < launch.task_deadline_ms;
        let envelope = candidate_result_envelope(launch, task, output.clone(), proof.clone())?;
        let envelope_bytes =
            serde_json::to_vec(&envelope).map_err(|_| "candidate_result_invalid")?;
        let cap = launch.unit_policy.max_result_bytes as usize;
        if envelope_bytes.len() > cap {
            return stopped_failure(
                db,
                jobs::CandidateStopCause::Failure,
                CandidateStopOutcome::ResultTooLarge,
                Some("candidate_result_envelope_too_large"),
                Some(digest(&envelope_bytes)),
                Some(envelope.result_produced_at_ms),
                Some(envelope_bytes.len() as u64),
                now,
            );
        }
        let (result_hash, result_bytes) = store_result_artifact(db, launch, &envelope_bytes, now)?;
        let stored = jobs::read_candidate_artifact(db, &result_hash, "result", cap)
            .map_err(|_| "candidate_result_store_failed")?;
        if verify_candidate_result_envelope(
            db,
            &task.spec,
            launch,
            &envelope.unit_proof,
            &result_hash,
            envelope.result_produced_at_ms,
            true,
            &stored,
        )
        .is_err()
        {
            return stopped_failure(
                db,
                jobs::CandidateStopCause::Failure,
                CandidateStopOutcome::InvalidResult,
                Some("candidate_result_unverified"),
                Some(result_hash),
                Some(envelope.result_produced_at_ms),
                Some(result_bytes),
                now,
            );
        }
        if !within_deadline {
            // The payload is authentic (identity, hashes and proof verified
            // above) but was produced after the recorded compute deadline.
            // Classify the timing, never as an invalid result or a success.
            return stopped_failure(
                db,
                jobs::CandidateStopCause::DeadlineExpired,
                CandidateStopOutcome::ResultAfterDeadline,
                Some("candidate_result_after_deadline"),
                Some(result_hash),
                Some(envelope.result_produced_at_ms),
                Some(result_bytes),
                now,
            );
        }
        let stopped = candidate_stopped(
            launch,
            envelope.unit_proof.clone(),
            jobs::CandidateStopCause::Succeeded,
            CandidateStopOutcome::Succeeded,
            None,
            Some(result_hash.clone()),
            Some(envelope.result_produced_at_ms),
            Some(result_bytes),
            now,
        );
        write_stopped(db, launch, &stopped)?;
        match jobs::complete_reconciled(
            db,
            &launch.job_id,
            &launch.lease,
            &result_hash,
            envelope.result_produced_at_ms,
            now.max(stopped.stopped_at_ms),
        ) {
            Ok(_) => return Ok(()),
            Err(_) => {
                let record = jobs::load_candidate(db, &launch.task_id, launch.revision)
                    .map_err(|_| "candidate_task_state_unavailable")?;
                let cause = if record.cancel_receipt.is_some() {
                    jobs::CandidateStopCause::OperatorCancel
                } else {
                    jobs::CandidateStopCause::Failure
                };
                return record_stopped_termination(db, launch, &stopped, cause, now)
                    .map(|_| ())
                    .map_err(|_| "candidate_termination_failed");
            }
        }
    }
    if let Ok(failure) = serde_json::from_slice::<CandidateHelperFailure>(&result)
        && verify_helper_failure(launch, &failure, now).is_ok()
    {
        return stopped_failure(
            db,
            if operator_cancel {
                jobs::CandidateStopCause::OperatorCancel
            } else {
                jobs::CandidateStopCause::Failure
            },
            if operator_cancel {
                CandidateStopOutcome::Cancelled
            } else {
                CandidateStopOutcome::ResultTooLarge
            },
            Some(&failure.code),
            Some(failure.payload_sha256),
            Some(failure.result_produced_at_ms),
            Some(failure.payload_bytes),
            now,
        );
    }
    let (hash, bytes) = store_result_artifact(db, launch, &result, now)?;
    stopped_failure(
        db,
        if operator_cancel {
            jobs::CandidateStopCause::OperatorCancel
        } else {
            jobs::CandidateStopCause::Failure
        },
        if operator_cancel {
            CandidateStopOutcome::Cancelled
        } else {
            CandidateStopOutcome::InvalidResult
        },
        Some("candidate_result_invalid"),
        Some(hash),
        None,
        Some(bytes),
        now,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_candidate_with_unit(
    settings: ResearchSettings,
    authority: PathBuf,
    task_id: String,
    revision: u32,
    operator_cancel: Arc<AtomicBool>,
    clock: impl Fn() -> i64,
    runner: &mut dyn worker::CandidateUnitRunner,
) -> std::result::Result<(), &'static str> {
    drive_candidate_with_unit_and_machine(
        settings,
        authority,
        task_id,
        revision,
        operator_cancel,
        clock,
        runner,
        machine_id,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn drive_candidate_with_unit_and_machine(
    settings: ResearchSettings,
    authority: PathBuf,
    task_id: String,
    revision: u32,
    operator_cancel: Arc<AtomicBool>,
    clock: impl Fn() -> i64,
    runner: &mut dyn worker::CandidateUnitRunner,
    mut machine: impl FnMut() -> std::result::Result<String, &'static str>,
) -> std::result::Result<(), &'static str> {
    let mut db = {
        let mut attempt = 0u32;
        loop {
            match open_queue_for_owner(&settings, &authority, false) {
                Ok(db) => break db,
                Err(_) if attempt < 200 => {
                    attempt = attempt.saturating_add(1);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(_) => return Err("candidate_queue_unavailable"),
            }
        }
    };
    let task = jobs::load_candidate(&mut db, &task_id, revision)
        .map_err(|error| candidate_load_error(&db, error))?;
    if task.status != jobs::TaskState::Pending || task.job.attempts != 0 {
        return Err("candidate_recovery_required");
    }
    let now = clock();
    if now < task.spec.created_at_ms {
        return Err("candidate_clock_invalid");
    }
    if now >= task.spec.expires_at_ms {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::DeadlineExpiredBeforeStart,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    }
    let Some(candidate) = settings
        .candidate_compute
        .as_ref()
        .filter(|candidate| candidate.enabled)
    else {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::ComputePolicyUnavailable,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    };
    if settings.admission_limits.is_none() {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::ComputePolicyUnavailable,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    }
    let helper_sha256 = match helper_content_sha256(candidate) {
        Ok(hash) => hash,
        Err(_) => {
            jobs::refuse_unstarted_candidate(
                &mut db,
                &task_id,
                revision,
                jobs::RefusalReason::RuntimeExecutableChanged,
                now,
            )
            .map_err(|_| "candidate_task_refusal_failed")?;
            return Ok(());
        }
    };
    let unit_policy = match candidate_unit_policy(candidate, &helper_sha256) {
        Ok(policy) => policy,
        Err(_) => {
            jobs::refuse_unstarted_candidate(
                &mut db,
                &task_id,
                revision,
                jobs::RefusalReason::RuntimeExecutableChanged,
                now,
            )
            .map_err(|_| "candidate_task_refusal_failed")?;
            return Ok(());
        }
    };
    let runtime_binding = candidate_runtime_binding(&helper_sha256, &unit_policy);
    let expected_resources = jobs::ResourceCaps {
        cpu_millis: candidate.wall_millis,
        tasks: 1,
        workspace_bytes: 1_048_576,
        file_bytes: 65_536,
        memory_bytes: candidate.sandbox.memory_mb * 1024 * 1024,
        output_bytes: candidate.sandbox.max_output_bytes as u64,
        wall_millis: candidate.wall_millis,
    };
    let current_sandbox = match candidate_sandbox_policy(candidate) {
        Ok(policy) => policy,
        Err(_) => {
            jobs::refuse_unstarted_candidate(
                &mut db,
                &task_id,
                revision,
                jobs::RefusalReason::RuntimeExecutableChanged,
                now,
            )
            .map_err(|_| "candidate_task_refusal_failed")?;
            return Ok(());
        }
    };
    if runtime_binding != task.spec.runtime_binding
        || unit_policy != task.spec.unit_policy
        || current_sandbox != task.spec.sandbox
        || candidate.abi != task.spec.abi
        || task.spec.resources != expected_resources
    {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::RuntimeExecutableChanged,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    }
    if !candidate
        .datasets
        .iter()
        .any(|dataset| dataset.dataset_id == task.spec.dataset_id)
    {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::RegistrationChanged,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    }
    let source =
        match jobs::read_candidate_artifact(&db, &task.spec.source_sha256, "source", 262_144) {
            Ok(source) => source,
            Err(_) => {
                jobs::refuse_unstarted_candidate(
                    &mut db,
                    &task_id,
                    revision,
                    jobs::RefusalReason::InputArtifactUnavailable,
                    now,
                )
                .map_err(|_| "candidate_task_refusal_failed")?;
                return Ok(());
            }
        };
    let params =
        match jobs::read_candidate_artifact(&db, &task.spec.params_sha256, "params", 65_536) {
            Ok(params) => params,
            Err(_) => {
                jobs::refuse_unstarted_candidate(
                    &mut db,
                    &task_id,
                    revision,
                    jobs::RefusalReason::InputArtifactUnavailable,
                    now,
                )
                .map_err(|_| "candidate_task_refusal_failed")?;
                return Ok(());
            }
        };
    let dataset = match jobs::read_candidate_artifact(
        &db,
        &task.spec.dataset_hash,
        "dataset",
        4 * 1024 * 1024,
    ) {
        Ok(dataset) => dataset,
        Err(_) => {
            jobs::refuse_unstarted_candidate(
                &mut db,
                &task_id,
                revision,
                jobs::RefusalReason::InputArtifactUnavailable,
                now,
            )
            .map_err(|_| "candidate_task_refusal_failed")?;
            return Ok(());
        }
    };
    if digest(&source) != task.spec.source_sha256
        || digest(&params) != task.spec.params_sha256
        || digest(&dataset) != task.spec.dataset_hash
    {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::InputArtifactUnavailable,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    }
    let snapshots: Vec<sadr_core::strategy::FeatureSnapshot> =
        match serde_json::from_slice(&dataset) {
            Ok(snapshots) => snapshots,
            Err(_) => {
                jobs::refuse_unstarted_candidate(
                    &mut db,
                    &task_id,
                    revision,
                    jobs::RefusalReason::InputArtifactUnavailable,
                    now,
                )
                .map_err(|_| "candidate_task_refusal_failed")?;
                return Ok(());
            }
        };
    if snapshots.is_empty() {
        jobs::refuse_unstarted_candidate(
            &mut db,
            &task_id,
            revision,
            jobs::RefusalReason::InputArtifactUnavailable,
            now,
        )
        .map_err(|_| "candidate_task_refusal_failed")?;
        return Ok(());
    }
    let execution_artifacts = match &task.spec.execution {
        Some(binding) => {
            let spec = match jobs::read_candidate_artifact(
                &db,
                &binding.spec_sha256,
                "execution_spec",
                65_536,
            ) {
                Ok(spec) => spec,
                Err(_) => {
                    jobs::refuse_unstarted_candidate(
                        &mut db,
                        &task_id,
                        revision,
                        jobs::RefusalReason::InputArtifactUnavailable,
                        now,
                    )
                    .map_err(|_| "candidate_task_refusal_failed")?;
                    return Ok(());
                }
            };
            let manifest = match jobs::read_candidate_artifact(
                &db,
                &binding.manifest_sha256,
                "execution_manifest",
                512 * 1024,
            ) {
                Ok(manifest) => manifest,
                Err(_) => {
                    jobs::refuse_unstarted_candidate(
                        &mut db,
                        &task_id,
                        revision,
                        jobs::RefusalReason::InputArtifactUnavailable,
                        now,
                    )
                    .map_err(|_| "candidate_task_refusal_failed")?;
                    return Ok(());
                }
            };
            let events = match jobs::read_candidate_artifact(
                &db,
                &binding.events_sha256,
                "execution_events",
                16 * 1024 * 1024,
            ) {
                Ok(events) => events,
                Err(_) => {
                    jobs::refuse_unstarted_candidate(
                        &mut db,
                        &task_id,
                        revision,
                        jobs::RefusalReason::InputArtifactUnavailable,
                        now,
                    )
                    .map_err(|_| "candidate_task_refusal_failed")?;
                    return Ok(());
                }
            };
            if digest(&spec) != binding.spec_sha256
                || digest(&manifest) != binding.manifest_sha256
                || digest(&events) != binding.events_sha256
            {
                jobs::refuse_unstarted_candidate(
                    &mut db,
                    &task_id,
                    revision,
                    jobs::RefusalReason::InputArtifactUnavailable,
                    now,
                )
                .map_err(|_| "candidate_task_refusal_failed")?;
                return Ok(());
            }
            Some((spec, manifest, events))
        }
        None => None,
    };
    // Capture the host identity before the durable claim or any dispatch. An
    // unavailable identity refuses the still-Pending task with a static reason;
    // it never creates an empty-machine launch or a lease that cannot be
    // released by the proof path.
    let machine = match machine() {
        Ok(machine)
            if !machine.is_empty()
                && machine.len() <= 128
                && !machine.chars().any(char::is_control) =>
        {
            machine
        }
        _ => {
            jobs::refuse_unstarted_candidate(
                &mut db,
                &task_id,
                revision,
                jobs::RefusalReason::MachineIdentityUnavailable,
                now,
            )
            .map_err(|_| "candidate_task_refusal_failed")?;
            return Ok(());
        }
    };
    let lease = digest(format!("owner-candidate-lease:{task_id}:{revision}").as_bytes());
    // The durable claim is next. A capacity refusal leaves the task Pending
    // with no launch receipt, so the next tick can retry once the slot frees.
    let job = match jobs::claim(&mut db, &task.spec.job_id, &lease, now) {
        Ok(job) => job,
        Err(jobs::Error::Capacity) => return Ok(()),
        Err(_) => return Err("candidate_claim_failed"),
    };
    if job.spec != task.job.spec {
        return Err("candidate_claim_failed");
    }
    let launch = match prepare_launch_with_machine(&settings, &task, &lease, now, &machine) {
        Ok(launch) => launch,
        Err(reason) => {
            // The claim is durable but the immutable intent does not match.
            let fallback = worker::CandidateLaunch {
                version: 2,
                job_id: task.spec.job_id.clone(),
                lease: lease.clone(),
                task_id: task.spec.task_id.clone(),
                revision: task.spec.revision,
                candidate_id: task.spec.candidate_id.clone(),
                lifecycle_candidate_id: task.spec.lifecycle_candidate_id.clone(),
                source_sha256: task.spec.source_sha256.clone(),
                params_sha256: task.spec.params_sha256.clone(),
                dataset_id: task.spec.dataset_id.clone(),
                dataset_hash: task.spec.dataset_hash.clone(),
                abi: task.spec.abi.clone(),
                runtime_binding: task.spec.runtime_binding.clone(),
                sandbox: task.spec.sandbox.clone(),
                unit_policy: task.spec.unit_policy.clone(),
                limits: task.spec.resources.clone(),
                replay_json: serde_json::to_string(
                    &settings
                        .candidate_compute
                        .as_ref()
                        .map(|candidate| candidate.replay.clone())
                        .unwrap_or_default(),
                )
                .unwrap_or_default(),
                stage_dir: settings
                    .stage_root
                    .join(format!("candidate-{lease}"))
                    .to_string_lossy()
                    .into_owned(),
                helper_path: settings
                    .stage_root
                    .join("helpers")
                    .join(&task.spec.unit_policy.helper_sha256)
                    .to_string_lossy()
                    .into_owned(),
                python_path: settings
                    .stage_root
                    .join("executables")
                    .join(format!("python-{}", task.spec.sandbox.python_sha256))
                    .to_string_lossy()
                    .into_owned(),
                bwrap_path: settings
                    .stage_root
                    .join("executables")
                    .join(format!("bwrap-{}", task.spec.sandbox.bwrap_sha256))
                    .to_string_lossy()
                    .into_owned(),
                unit: worker::candidate_unit_name(&lease).unwrap_or_default(),
                machine: machine.clone(),
                started_ms: now,
                deadline_ms: now.saturating_add(i64::from(task.job.spec.wall_millis)),
                task_deadline_ms: now
                    .saturating_add(i64::from(task.job.spec.wall_millis))
                    .min(task.spec.expires_at_ms),
                execution: task.spec.execution.clone(),
                observation: match &task.spec.purpose {
                    Some(jobs::CandidatePurposeV1::Observe { target }) => {
                        Some(target.as_ref().clone())
                    }
                    _ => None,
                },
            };
            finish_unstarted(&mut db, &fallback, runner, reason, clock())?;
            return Ok(());
        }
    };
    let launch_bytes = serde_json::to_vec(&launch).map_err(|_| "candidate_launch_invalid")?;
    if worker::save_receipt(
        &db,
        &launch.job_id,
        &launch.lease,
        "candidate_launch",
        &launch_bytes,
    )
    .is_err()
    {
        // The claim is durable but the intent receipt is not. Never start a
        // physical unit without it; prove the original unit absent and record
        // the unstarted termination, or retain the lease on failure.
        finish_unstarted(
            &mut db,
            &launch,
            runner,
            "launch_receipt_unavailable",
            clock(),
        )?;
        return Err("candidate_launch_receipt_failed");
    }
    let execution_refs = execution_artifacts
        .as_ref()
        .map(|(spec, manifest, events)| (spec.as_slice(), manifest.as_slice(), events.as_slice()));
    if let Err(reason) = stage_candidate(
        &settings,
        candidate,
        &launch,
        &source,
        &params,
        &dataset,
        execution_refs,
    ) {
        finish_unstarted(&mut db, &launch, runner, reason, clock())?;
        return Ok(());
    }
    // Observation artifacts are staged only after the shared source/params/
    // dataset stage succeeded, and are never exposed to the Python sandbox.
    if let Some(target) = &launch.observation {
        let spec =
            jobs::read_candidate_artifact(&db, &target.spec_sha256, "observation_spec", 65_536)
                .map_err(|_| "candidate_stage_invalid")?;
        let quotes = jobs::read_candidate_artifact(
            &db,
            &target.quotes_sha256,
            "observation_quotes",
            2_000_000,
        )
        .map_err(|_| "candidate_stage_invalid")?;
        if digest(&spec) != target.spec_sha256 || digest(&quotes) != target.quotes_sha256 {
            finish_unstarted(&mut db, &launch, runner, "candidate_input_changed", clock())?;
            return Ok(());
        }
        let stage = Path::new(&launch.stage_dir);
        if write_new_private(&stage.join("observation_spec"), &spec, 0o600).is_err()
            || write_new_private(&stage.join("observation_quotes"), &quotes, 0o600).is_err()
        {
            finish_unstarted(&mut db, &launch, runner, "candidate_stage_invalid", clock())?;
            return Ok(());
        }
    }
    match runner.start(&launch) {
        Ok(()) => {}
        Err(worker::RunnerError::Unavailable) => {
            finish_unstarted(
                &mut db,
                &launch,
                runner,
                "unit_unavailable_before_spawn",
                clock(),
            )?;
            return Ok(());
        }
        Err(worker::RunnerError::Ambiguous) | Err(worker::RunnerError::Unsupported) => {
            // The unit may exist. Retain the lease; reconciliation queries or
            // stops only the original owned unit and never relaunches source.
            return Err("candidate_recovery_required");
        }
    }
    let mut last = now;
    loop {
        let at = clock();
        if at < last {
            return Err("candidate_clock_invalid");
        }
        last = at;
        let record = jobs::load_candidate(&mut db, &launch.task_id, launch.revision)
            .map_err(|_| "candidate_task_state_unavailable")?;
        let operator = operator_cancel.load(Ordering::Relaxed) || record.cancel_receipt.is_some();
        let deadline = at >= launch.task_deadline_ms;
        if operator || deadline || record.status != jobs::TaskState::Running {
            if task_state_terminal(record.status) {
                return Err("candidate_recovery_required");
            }
            let proof = runner
                .terminate(&launch)
                .map_err(|_| "candidate_recovery_required")?;
            let ended = clock();
            if ended < at {
                return Err("candidate_clock_invalid");
            }
            let (cause, outcome) = if operator {
                (
                    jobs::CandidateStopCause::OperatorCancel,
                    CandidateStopOutcome::Cancelled,
                )
            } else if deadline {
                (
                    jobs::CandidateStopCause::DeadlineExpired,
                    CandidateStopOutcome::DeadlineExpired,
                )
            } else {
                (
                    jobs::CandidateStopCause::Failure,
                    CandidateStopOutcome::Terminated,
                )
            };
            let stopped = candidate_stopped(
                &launch, proof, cause, outcome, None, None, None, None, ended,
            );
            write_stopped(&db, &launch, &stopped)?;
            record_stopped_termination(&mut db, &launch, &stopped, cause, ended)?;
            return Ok(());
        }
        match runner.poll(&launch) {
            Ok(worker::CandidateUnitPoll::Running) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Ok(worker::CandidateUnitPoll::Exited(exited)) => {
                let ended = clock();
                if ended < at {
                    return Err("candidate_clock_invalid");
                }
                return finish_exited(&mut db, &launch, &task, exited.proof, exited.result, ended);
            }
            Err(_) => return Err("candidate_recovery_required"),
        }
    }
}

fn forbidden_pnl_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "fees_inr"
                        | "cash_inr"
                        | "realized_pnl_if_flat_inr"
                        | "net_pnl"
                        | "paper_pnl"
                        | "fills_inr"
                )
            }) || map.values().any(forbidden_pnl_key)
        }
        Value::Array(values) => values.iter().any(forbidden_pnl_key),
        _ => false,
    }
}

/// Verify the target's frozen metric fields against the ACTUAL hash-pinned
/// quote bundle. `quote_keys`, `quote_budget_truncated` and `decision_delay_ms`
/// are not derivable from the current config or clock; they must reproduce the
/// freeze-time metadata recorded inside the bundle, and the bundle's own
/// manifest must agree with its truncation/coverage flags.
pub(crate) fn validate_frozen_target_metrics(
    bundle: &Value,
    target: &jobs::ObservationTargetV1,
) -> Result<()> {
    if bundle["version"].as_u64() != Some(1) || bundle["status"] != "ok" {
        return Err("candidate_observation_target_unbound".into());
    }
    let manifest = bundle["manifest"]
        .as_str()
        .ok_or("candidate_observation_target_unbound")?;
    let events = bundle["events"]
        .as_str()
        .ok_or("candidate_observation_target_unbound")?;
    if bundle["manifest_sha256"] != digest(manifest.as_bytes())
        || bundle["events_sha256"] != digest(events.as_bytes())
    {
        return Err("candidate_observation_target_unbound".into());
    }
    let manifest: Value =
        serde_json::from_str(manifest).map_err(|_| "candidate_observation_target_unbound")?;
    let per_key = manifest["provenance"]["per_key"]
        .as_object()
        .ok_or("candidate_observation_target_unbound")?;
    let metrics = &bundle["target_metrics"];
    if metrics["version"].as_u64() != Some(1) {
        // An observation archive without freeze-time metrics cannot prove the
        // target's quote count/truncation/delay; it is never trusted. return Err("candidate_observation_target_unbound".into());
    }
    let frozen_keys = metrics["quote_keys"]
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or("candidate_observation_target_unbound")?;
    let frozen_truncated = metrics["quote_budget_truncated"]
        .as_bool()
        .ok_or("candidate_observation_target_unbound")?;
    let frozen_delay = metrics["decision_delay_ms"]
        .as_i64()
        .ok_or("candidate_observation_target_unbound")?;
    let manifest_truncated = manifest["provenance"]["budget_truncated"]
        .as_bool()
        .unwrap_or(false);
    let manifest_coverage = !manifest["provenance"]["coverage_gap"].is_null();
    if target.quote_keys != frozen_keys
        || target.quote_budget_truncated != frozen_truncated
        || target.decision_delay_ms != frozen_delay
        || frozen_truncated != (manifest_truncated || manifest_coverage)
        || (!frozen_truncated && target.quote_keys as usize != per_key.len())
        || metrics["window_from_ms"].as_i64() != Some(target.window_from_ms)
        || metrics["window_to_ms"].as_i64() != Some(target.window_to_ms)
        || metrics["holding_ms"].as_i64() != Some(target.holding_ms)
        || metrics["max_exit_delay_ms"].as_i64() != Some(target.max_exit_delay_ms)
        || metrics["signal_ms"].as_i64() != Some(target.signal_ms)
    {
        return Err("candidate_observation_target_unbound".into());
    }
    Ok(())
}

/// Bind one stored observation task to the target it claims. This is a READ
/// check: the queue row is not trusted just because it was written once. The
/// frozen quote bundle is re-read so the target's metric fields are pinned to
/// the ORIGINAL bytes on every read path.
pub(crate) fn validate_observation_task_binding(
    db: &Connection,
    task: &jobs::CandidateTaskRecord,
) -> Result<()> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.spec.purpose else {
        return Ok(());
    };
    if !target.validate()
        || task.spec.policy_sha256 != target.policy_sha256
        || task.spec.runtime_binding.runtime_hash != target.source_runtime_hash
        || task.spec.runtime_binding.helper_sha256 != target.runtime.helper_sha256
        || task.spec.runtime_binding.unit_policy_sha256 != target.runtime.unit_policy_sha256
        || task.spec.runtime_binding.scorer_compare_sha256 != target.runtime.scorer_compare_sha256
        || task.spec.runtime_binding.sandbox_source_sha256 != target.runtime.sandbox_source_sha256
        || task.spec.source_sha256 != target.runtime.source_sha256
        || task.spec.params_sha256 != target.runtime.params_sha256
        || task.spec.dataset_hash != target.snapshot_sha256
        || task.spec.parent_candidate_id.as_deref() != Some(target.source_candidate_id.as_str())
        || task.spec.parent_lifecycle_candidate_id.as_deref()
            != Some(target.source_lifecycle_candidate_id.as_str())
        || task.spec.windows.selection_from_ms != target.window_from_ms
        || target.window_to_ms > task.spec.windows.selection_to_ms
        || task.spec.horizon_minutes.saturating_mul(60_000) != target.holding_ms
        || task.spec.max_exit_delay_ms != target.max_exit_delay_ms
        || task.spec.abi != "scorer_replay_v1"
        || task.spec.created_at_ms != target.signal_ms
    {
        return Err("candidate_observation_target_unbound".into());
    }
    let bundle_bytes =
        jobs::read_candidate_artifact(db, &target.quotes_sha256, "observation_quotes", 2_000_000)
            .map_err(|_| "candidate_result_unverified")?;
    if digest(&bundle_bytes) != target.quotes_sha256 {
        return Err("candidate_result_unverified".into());
    }
    let bundle: Value =
        serde_json::from_slice(&bundle_bytes).map_err(|_| "candidate_result_unverified")?;
    validate_frozen_target_metrics(&bundle, target)
}

pub(crate) fn read_verified_candidate_result(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
) -> Result<Option<CandidateResultV1>> {
    let task = jobs::load_candidate(db, task_id, revision).map_err(|error| {
        if error == jobs::Error::Invalid && jobs::candidate_requires_migration(db).unwrap_or(false)
        {
            "candidate_legacy_schema_migration_required"
        } else {
            "candidate_result_unverified"
        }
    })?;
    validate_observation_task_binding(db, &task)?;
    if task.status != jobs::TaskState::Succeeded {
        return Ok(None);
    }
    let terminal_at_ms = task.terminal_at_ms.ok_or("candidate_result_unverified")?;
    if terminal_at_ms < task.spec.created_at_ms {
        return Err("candidate_result_unverified".into());
    }
    let lease = task
        .job
        .lease
        .as_deref()
        .ok_or("candidate_result_unverified")?;
    let result_hash = task
        .job
        .result_hash
        .as_deref()
        .ok_or("candidate_result_unverified")?;
    let launch = read_launch(db, &task).map_err(|_| "candidate_result_unverified")?;
    if launch.lease != lease {
        return Err("candidate_result_unverified".into());
    }
    let stopped = read_stopped(db, &launch)
        .map_err(|_| "candidate_result_unverified")?
        .ok_or("candidate_result_unverified")?;
    if stopped.cause != jobs::CandidateStopCause::Succeeded
        || stopped.outcome != CandidateStopOutcome::Succeeded
        || stopped.result_sha256.as_deref() != Some(result_hash)
        || stopped.result_at_ms.is_none()
        || stopped.stopped_at_ms > terminal_at_ms
    {
        return Err("candidate_result_unverified".into());
    }
    let result_at_ms = stopped.result_at_ms.ok_or("candidate_result_unverified")?;
    let cap = task.spec.unit_policy.max_result_bytes as usize;
    let result_bytes = jobs::read_candidate_artifact(db, result_hash, "result", cap)
        .map_err(|_| "candidate_result_unverified")?;
    let result = verify_candidate_result_envelope(
        db,
        &task.spec,
        &launch,
        &stopped.proof,
        result_hash,
        result_at_ms,
        false,
        &result_bytes,
    )
    .map_err(|_| "candidate_result_unverified")?;
    let dataset_bytes =
        jobs::read_candidate_artifact(db, &task.spec.dataset_hash, "dataset", 4 * 1024 * 1024)
            .map_err(|_| "candidate_result_unverified")?;
    if digest(&dataset_bytes) != task.spec.dataset_hash {
        return Err("candidate_result_unverified".into());
    }
    let snapshots: Vec<sadr_core::strategy::FeatureSnapshot> =
        serde_json::from_slice(&dataset_bytes).map_err(|_| "candidate_result_unverified")?;
    if snapshots.len() != result.report.snapshots
        || snapshots
            .iter()
            .zip(result.report.ticks.iter())
            .any(|(snapshot, tick)| snapshot.observed_ms != tick.observed_ms)
    {
        return Err("candidate_result_unverified".into());
    }
    if let Some(execution) = &result.execution {
        let binding = task
            .spec
            .execution
            .as_ref()
            .ok_or("candidate_result_unverified")?;
        let spec =
            jobs::read_candidate_artifact(db, &binding.spec_sha256, "execution_spec", 65_536)
                .map_err(|_| "candidate_result_unverified")?;
        let manifest = jobs::read_candidate_artifact(
            db,
            &binding.manifest_sha256,
            "execution_manifest",
            512 * 1024,
        )
        .map_err(|_| "candidate_result_unverified")?;
        let events = jobs::read_candidate_artifact(
            db,
            &binding.events_sha256,
            "execution_events",
            16 * 1024 * 1024,
        )
        .map_err(|_| "candidate_result_unverified")?;
        if digest(&spec) != binding.spec_sha256
            || digest(&manifest) != binding.manifest_sha256
            || digest(&events) != binding.events_sha256
            || sadr_cli::candidate_execution::verify_report_against_manifest(
                execution, &manifest, &events,
            )
            .is_err()
        {
            return Err("candidate_result_unverified".into());
        }
    } else if task.spec.execution.is_some() {
        return Err("candidate_result_unverified".into());
    }
    Ok(Some(result))
}

/// Read the original terminal failure receipt for a non-succeeded candidate.
/// This is evidence only: it never launches, reconciles, cancels or replays the
/// unit, and an unknown/running task is not evidence.
pub(crate) fn read_candidate_stopped_failure(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
) -> Result<Option<CandidateStopped>> {
    let task =
        jobs::load_candidate(db, task_id, revision).map_err(|_| "candidate_result_unverified")?;
    if task.status == jobs::TaskState::Succeeded || !task_state_terminal(task.status) {
        return Ok(None);
    }
    let launch = read_launch(db, &task)?;
    let Some(stopped) = read_stopped(db, &launch)? else {
        return Ok(None);
    };
    if stopped.cause == jobs::CandidateStopCause::Succeeded
        || stopped.outcome == CandidateStopOutcome::Succeeded
    {
        return Ok(None);
    }
    Ok(Some(stopped))
}

/// Load and FULLY verify the original AUTHORING task A pinned by an observation
/// target. The target is host-derived, but it is not trusted on its own: the
/// exact task/revision/candidate/lifecycle/result/policy/runtime/source/params
/// identity must reproduce and the purpose must be the DEFAULT authoring task.
/// A task that is itself an Observation is never an authoring parent.
pub(crate) fn load_verified_authoring_parent(
    c: &Config,
    chain: &[(String, std::path::PathBuf, Value)],
    queue: &mut Connection,
    source_session: &str,
    target: &jobs::ObservationTargetV1,
) -> Result<CandidateResultV1> {
    if !target.validate() {
        return Err("candidate_observation_target_invalid".into());
    }
    let task = jobs::load_candidate(queue, &target.source_task_id, target.source_revision)
        .map_err(|_| "candidate_observation_source_unverified")?;
    // A's OWN origin-session binding, not O's and not the current session's. A
    // foreign shared-queue row is refused before its result is read.
    verify_task_book_binding(c, chain, &task)?;
    // PURPOSE: the default authoring task has no `purpose` field. An
    // Observation task is a different kind of evidence and can never be the
    // authoring parent of another observation.
    if task.spec.purpose.is_some() {
        return Err("candidate_observation_source_not_authoring".into());
    }
    if task.status != jobs::TaskState::Succeeded
        || task.spec.session != source_session
        || task.spec.task_id != target.source_task_id
        || task.spec.revision != target.source_revision
        || task.spec.candidate_id != target.source_candidate_id
        || task.spec.lifecycle_candidate_id != target.source_lifecycle_candidate_id
        || task.spec.created_at_ms != target.authoring_at_ms
        || task.job.result_hash.as_deref() != Some(target.source_result_sha256.as_str())
        || task.spec.policy_sha256 != target.policy_sha256
        || task.spec.runtime_binding.runtime_hash != target.source_runtime_hash
        || task.spec.runtime_binding.helper_sha256 != target.runtime.helper_sha256
        || task.spec.runtime_binding.unit_policy_sha256 != target.runtime.unit_policy_sha256
        || task.spec.runtime_binding.scorer_compare_sha256 != target.runtime.scorer_compare_sha256
        || task.spec.runtime_binding.sandbox_source_sha256 != target.runtime.sandbox_source_sha256
        || task.spec.source_sha256 != target.runtime.source_sha256
        || task.spec.params_sha256 != target.runtime.params_sha256
    {
        return Err("candidate_observation_source_unverified".into());
    }
    // Re-verify the terminal result envelope and the dataset artifact bytes, not
    // just the recorded hash. A different valid result for the same task id is
    // unverified context, never the pinned source.
    let result =
        read_verified_candidate_result(queue, &target.source_task_id, target.source_revision)
            .map_err(|_| "candidate_result_unverified")?
            .ok_or("candidate_result_unverified")?;
    if result.observation.is_some() {
        return Err("candidate_observation_source_not_authoring".into());
    }
    Ok(result)
}

pub(crate) fn reconcile_candidate(
    c: &Config,
    _cfg: &PlannerConfig,
    queue: &mut Connection,
    task: &jobs::CandidateTaskRecord,
) -> Result<Value> {
    if task.status != jobs::TaskState::Succeeded {
        return Ok(json!({
            "status":"not_terminal",
            "task_status":task_state_name(task.status),
            "activation_authorized":false
        }));
    }
    let result = read_verified_candidate_result(queue, &task.spec.task_id, task.spec.revision)?
        .ok_or("candidate_result_unverified")?;
    let now = task
        .terminal_at_ms
        .ok_or("candidate_terminal_time_missing")?;
    bridge_candidate_result(c, task, &result, now)
}

/// Crash/restart reconciliation. It never relaunches source. A durable valid
/// stop plus result completes idempotently; a durable stop without a result is
/// a trusted terminal failure/cancellation; a live attempt is queried or
/// stopped only through its original owned unit and then proven absent.
/// Cheap pre-check for the controller's terminal scan: a `Terminated` row may
/// still carry an authentic timely result and a durable success stop proof. The
/// full payload verification happens in `reconcile_candidate_attempt`.
pub(crate) fn candidate_late_result_ready(
    db: &Connection,
    task: &jobs::CandidateTaskRecord,
) -> bool {
    let Ok(launch) = read_launch(db, task) else {
        return false;
    };
    let Ok(Some(stopped)) = read_stopped(db, &launch) else {
        return false;
    };
    if stopped.cause != jobs::CandidateStopCause::Succeeded
        || stopped.outcome != CandidateStopOutcome::Succeeded
    {
        return false;
    }
    let (Some(result_hash), Some(result_at)) =
        (stopped.result_sha256.as_deref(), stopped.result_at_ms)
    else {
        return false;
    };
    let cap = task.spec.unit_policy.max_result_bytes as usize;
    let Ok(bytes) = jobs::read_candidate_artifact(db, result_hash, "result", cap) else {
        return false;
    };
    verify_candidate_result_envelope(
        db,
        &task.spec,
        &launch,
        &stopped.proof,
        result_hash,
        result_at,
        false,
        &bytes,
    )
    .is_ok()
}

pub(crate) fn reconcile_candidate_attempt(
    _settings: &ResearchSettings,
    queue: &mut Connection,
    task: &jobs::CandidateTaskRecord,
    runner: &mut dyn worker::CandidateUnitRunner,
    clock: impl Fn() -> i64,
) -> Result<Value> {
    let launch = read_launch(queue, task).map_err(|_| "candidate_recovery_required")?;
    let now = clock();
    if now < launch.started_ms {
        return Err("candidate_clock_invalid".into());
    }
    let stopped = read_stopped(queue, &launch).map_err(|_| "candidate_recovery_required")?;
    let terminal = || {
        Ok(json!({
            "status":"terminal",
            "task_status":task_state_name(task.status),
            "activation_authorized":false
        }))
    };
    // A genuine operator cancel is never converted into success. A timely
    // result that lost only to a later deadline/failure termination may still
    // reconcile late from its original stop proof and recorded result bytes.
    if task_state_terminal(task.status) {
        if task.status == jobs::TaskState::Terminated
            && let Some(stopped) = stopped.as_ref()
            && stopped.cause == jobs::CandidateStopCause::Succeeded
            && stopped.outcome == CandidateStopOutcome::Succeeded
            && let (Some(result_hash), Some(result_at)) =
                (stopped.result_sha256.as_deref(), stopped.result_at_ms)
            && result_at < launch.task_deadline_ms
        {
            let cap = task.spec.unit_policy.max_result_bytes as usize;
            if let Ok(bytes) = jobs::read_candidate_artifact(queue, result_hash, "result", cap)
                && verify_candidate_result_envelope(
                    queue,
                    &task.spec,
                    &launch,
                    &stopped.proof,
                    result_hash,
                    result_at,
                    false,
                    &bytes,
                )
                .is_ok()
                && jobs::complete_reconciled(
                    queue,
                    &launch.job_id,
                    &launch.lease,
                    result_hash,
                    result_at,
                    now.max(stopped.stopped_at_ms),
                )
                .is_ok()
            {
                return Ok(json!({
                    "status":"reconciled",
                    "outcome":"succeeded",
                    "task_id":launch.task_id,
                    "revision":launch.revision,
                    "activation_authorized":false
                }));
            }
        }
        return terminal();
    }
    if let Some(stopped) = stopped {
        if stopped.cause == jobs::CandidateStopCause::Succeeded
            && stopped.outcome == CandidateStopOutcome::Succeeded
        {
            let timely = stopped
                .result_at_ms
                .is_some_and(|at| at < launch.task_deadline_ms);
            if timely
                && let (Some(result_hash), Some(result_at)) =
                    (stopped.result_sha256.as_deref(), stopped.result_at_ms)
            {
                let cap = task.spec.unit_policy.max_result_bytes as usize;
                let verified = jobs::read_candidate_artifact(queue, result_hash, "result", cap)
                    .ok()
                    .is_some_and(|bytes| {
                        verify_candidate_result_envelope(
                            queue,
                            &task.spec,
                            &launch,
                            &stopped.proof,
                            result_hash,
                            result_at,
                            false,
                            &bytes,
                        )
                        .is_ok()
                    });
                if verified {
                    match jobs::complete_reconciled(
                        queue,
                        &launch.job_id,
                        &launch.lease,
                        result_hash,
                        result_at,
                        now.max(stopped.stopped_at_ms),
                    ) {
                        Ok(_) => {
                            return Ok(json!({
                                "status":"reconciled",
                                "outcome":"succeeded",
                                "task_id":launch.task_id,
                                "revision":launch.revision,
                                "activation_authorized":false
                            }));
                        }
                        Err(_) => {
                            let cause = if task.cancel_receipt.is_some() {
                                jobs::CandidateStopCause::OperatorCancel
                            } else {
                                jobs::CandidateStopCause::Failure
                            };
                            record_stopped_termination(queue, &launch, &stopped, cause, now)
                                .map_err(|_| "candidate_termination_failed")?;
                            return Ok(json!({
                                "status":"reconciled",
                                "outcome":if cause == jobs::CandidateStopCause::OperatorCancel {"cancelled"} else {"terminated"},
                                "task_id":launch.task_id,
                                "revision":launch.revision,
                                "activation_authorized":false
                            }));
                        }
                    }
                }
            }
            // The durable stop says the unit succeeded, but the recorded
            // payload is missing, corrupt, mismatched or produced too late.
            // Preserve the artifact and record a truthful failure, never a
            // Succeeded row that can never verify.
            let cause = if task.cancel_receipt.is_some() {
                jobs::CandidateStopCause::OperatorCancel
            } else if stopped.result_at_ms.is_none() {
                jobs::CandidateStopCause::Failure
            } else if !timely {
                jobs::CandidateStopCause::DeadlineExpired
            } else {
                jobs::CandidateStopCause::Failure
            };
            record_stopped_termination(queue, &launch, &stopped, cause, now)
                .map_err(|_| "candidate_termination_failed")?;
            return Ok(json!({
                "status":"reconciled",
                "outcome":if cause == jobs::CandidateStopCause::OperatorCancel {"cancelled"} else {"terminated"},
                "task_id":launch.task_id,
                "revision":launch.revision,
                "activation_authorized":false
            }));
        }
        // Honor the recorded durable cause. An operator cancel receipt always
        // wins over a later expiry/failure label, and a Running row is not
        // allowed to erase a durable cancelled outcome.
        let cause = if task.cancel_receipt.is_some() {
            jobs::CandidateStopCause::OperatorCancel
        } else {
            stopped.cause
        };
        record_stopped_termination(queue, &launch, &stopped, cause, now)
            .map_err(|_| "candidate_termination_failed")?;
        return Ok(json!({
            "status":"reconciled",
            "outcome":if cause == jobs::CandidateStopCause::OperatorCancel {"cancelled"} else {"terminated"},
            "task_id":launch.task_id,
            "revision":launch.revision,
            "activation_authorized":false
        }));
    }
    let proof = runner
        .terminate(&launch)
        .map_err(|_| "candidate_recovery_required")?;
    let operator = task.cancel_receipt.is_some();
    let deadline = now >= launch.task_deadline_ms;
    let (cause, outcome) = if operator {
        (
            jobs::CandidateStopCause::OperatorCancel,
            CandidateStopOutcome::Cancelled,
        )
    } else if deadline {
        (
            jobs::CandidateStopCause::DeadlineExpired,
            CandidateStopOutcome::DeadlineExpired,
        )
    } else {
        (
            jobs::CandidateStopCause::Failure,
            CandidateStopOutcome::Terminated,
        )
    };
    let stopped = candidate_stopped(&launch, proof, cause, outcome, None, None, None, None, now);
    write_stopped(queue, &launch, &stopped)?;
    record_stopped_termination(queue, &launch, &stopped, cause, now)?;
    Ok(json!({
        "status":"reconciled",
        "outcome":if cause == jobs::CandidateStopCause::OperatorCancel {"cancelled"} else {"terminated"},
        "task_id":launch.task_id,
        "revision":launch.revision,
        "activation_authorized":false
    }))
}

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
    pub exit_quote: Option<sadr_cli::candidate_execution::FullQuoteRefV1>,
    pub exit_status: String,
    pub exit_gap: Option<Value>,
    pub first_receipt_unknown: bool,
    pub execution: Option<sadr_cli::candidate_execution::ExecutableSummary>,
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
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
    exit_due_ms: i64,
    exit_deadline_ms: i64,
    exit_quote: Option<sadr_cli::candidate_execution::FullQuoteRefV1>,
    exit_status: &str,
    exit_gap: Option<Value>,
    first_receipt_unknown: bool,
    execution: Option<sadr_cli::candidate_execution::ExecutableSummary>,
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
        sadr_cli::candidate_execution::observation_artifact_sha256(&content).unwrap_or_default();
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
    spec: &sadr_cli::candidate_execution::ObservationExecutionSpecV1,
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
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
        source: &c.directory,
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
    pub exit_quote: sadr_cli::candidate_execution::FullQuoteRefV1,
    pub exit_status: String,
    pub exit_gap: Option<Value>,
    pub first_receipt_unknown: bool,
    pub execution: Option<sadr_cli::candidate_execution::ExecutableSummary>,
    pub report_sha256: Option<String>,
    pub gaps: Vec<String>,
    pub exit_at_ms: i64,
}

/// The shared engine could not produce a verifiable report for an otherwise
/// eligible original exit. The original receipt is retained as provenance; the
/// caller records an explicit unknown, never a fabricated zero.
pub(crate) struct ObservationExitUnavailable {
    pub exit_quote: sadr_cli::candidate_execution::FullQuoteRefV1,
}

#[allow(clippy::large_enum_variant)]
pub(crate) enum ObservationExitOutcome {
    Evaluated(ObservationExitEvaluation),
    MeasurementUnavailable(ObservationExitUnavailable),
}

pub(crate) fn evaluate_observation_exit(
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
    spec: &sadr_cli::candidate_execution::ObservationExecutionSpecV1,
    exit_tape: &super::super::super::paper_replay::VirtualTape,
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
    let instruments: Vec<sadr_core::execution::Instrument> =
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
    let exit_quote = sadr_cli::candidate_execution::FullQuoteRefV1 {
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
    sadr_cli::strategy_archive::validate_archive(&manifest, &events)?;
    let recorded_entry = sadr_cli::strategy_actions::Entry {
        at_ms: entry.decision_at_ms,
        key: entry.key.clone(),
        units: entry.units,
        max_price: entry.max_price,
    };
    let source =
        sadr_cli::candidate_execution::artifact_bytes(std::slice::from_ref(&recorded_entry))?;
    let parameters = sadr_cli::candidate_execution::artifact_bytes(&spec.parameters)?;
    let report = match sadr_cli::strategy_actions::evaluate_virtual_observation(
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
    sadr_cli::candidate_execution::verify_virtual_observation_report(
        &report,
        &recorded_entry,
        spec,
        &manifest,
        &events,
        false,
    )?;
    let summary = sadr_cli::candidate_execution::virtual_observation_summary(
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
    let report_sha256 = sadr_cli::candidate_execution::observation_artifact_sha256(&report)?;
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
    pub source: &'a Path,
    pub session: chrono::NaiveDate,
    pub previous_session: Option<chrono::NaiveDate>,
    /// The VERIFIED observation task that produced `score`. The stored body's
    /// current identity fields must reproduce this task, never a caller or body
    /// supplied id.
    pub task: &'a jobs::CandidateTaskRecord,
    pub score: &'a sadr_cli::candidate_execution::ObservationScoreV1,
    pub spec: &'a sadr_cli::candidate_execution::ObservationExecutionSpecV1,
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
    sadr_cli::candidate_execution::observation_artifact_sha256(&content).ok()
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
        super::super::super::paper_replay::virtual_observation_tape_bounded_immutable(
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
            super::super::super::paper_replay::TapeMode::ExitWindow,
        )
    } else {
        super::super::super::paper_replay::virtual_observation_tape_bounded(
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
            super::super::super::paper_replay::TapeMode::ExitWindow,
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
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes).map_err(|_| "candidate_observation_spec_invalid")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
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
    let exit_tape = super::super::super::paper_replay::virtual_observation_tape_bounded(
        &c.directory,
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
        super::super::super::paper_replay::TapeMode::ExitWindow,
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

/// A legacy CURRENT lifecycle ledger without the lifecycle tables is a known
/// empty library ONLY when the original queue independently proves no candidate
/// admissions. With real admissions it is ambiguous and must not be projected
/// as a known zero.
fn current_lifecycle_legacy_ambiguous(c: &Config, cfg: &PlannerConfig) -> Result<bool> {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return Ok(false);
    };
    let file = experiment_ledger::file_path(&c.directory);
    if !file.is_file() {
        return Ok(false);
    }
    // A corrupt ledger is UNAVAILABLE, not a legacy-ambiguous empty library.
    let tables = match experiment_ledger::lifecycle::lifecycle_tables_present(&c.directory, false) {
        Ok(tables) => tables,
        Err(_) => return Ok(false),
    };
    if tables {
        return Ok(false);
    }
    if !settings.queue.is_file() {
        // A missing queue is not proof of zero admissions.
        return Ok(true);
    }
    let queue = rusqlite::Connection::open_with_flags(
        &settings.queue,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "candidate_task_state_unavailable")?;
    super::carry::queue_has_candidate_rows(&queue)
}

/// Structured, honest source-observation enumeration. `items` is the bounded
/// list; `status` distinguishes a complete list from a truncated or partial
/// scan; `scanned`/`limit`/`reason` make the bound explicit so an empty or short
/// list is never published as an authoritative complete result.
pub(crate) struct ObservationRefsProjection {
    pub items: Vec<Value>,
    /// `available` (complete), `truncated` (resource bound) or `partial`
    /// (a source/library error was seen; may also be truncated).
    pub status: &'static str,
    pub truncated: bool,
    pub partial: bool,
    pub scanned: usize,
    pub limit: usize,
    /// The TRUNCATION reason (a resource bound), never a source error.
    pub reason: Option<&'static str>,
    /// The first SOURCE error. Kept separate so a partial scan is never
    /// relabelled as a clean `observation_ref_limit` truncation.
    pub source_error_reason: Option<&'static str>,
}

/// Host-built, read-only source-observation refs from the FULL verified
/// lineage. The model can discover the EXACT original observation episode O
/// without an arbitrary path or a private helper, then resolve it through the
/// native `candidate_result(observation_ref)` selector.
///
/// The display is bounded at `limit`, but the scan looks one row past it: an
/// exact-`limit` list is COMPLETE, and only an actual extra verified ref marks
/// truncation. A source error after `limit` refs is PARTIAL and is never
/// masked by the display bound.
pub(crate) fn observation_refs_from_chain(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, std::path::PathBuf, Value)],
    limit: usize,
) -> Result<ObservationRefsProjection> {
    verify_lineage_prefix(c, cfg, chain)?;
    let current_session = c.session.to_string();
    let mut refs = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;
    let mut partial = false;
    let mut reason: Option<&'static str> = None;
    let mut source_error_reason: Option<&'static str> = None;
    let mut note_source_error = |code: &'static str| {
        partial = true;
        if source_error_reason.is_none() {
            source_error_reason = Some(code);
        }
    };
    'chain: for (session, directory, _) in chain {
        let library = if session == &current_session {
            match experiment_ledger::lifecycle::library(directory, session) {
                Ok(library) => library,
                Err(_) => {
                    note_source_error("candidate_observation_library_unavailable");
                    continue;
                }
            }
        } else {
            match experiment_ledger::lifecycle::library_bounded_immutable(directory, "", 10_000) {
                Ok(library) => library,
                Err(_) => {
                    note_source_error("candidate_observation_library_unavailable");
                    continue;
                }
            }
        };
        if library["truncated"].as_bool().unwrap_or(false) {
            truncated = true;
            reason = Some("source_library_truncated");
        }
        let observation_items = library["items"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|item| super::continuation::is_observation_item(item))
            .collect::<Vec<_>>();
        if observation_items.is_empty() {
            continue;
        }
        // Open the source queue only when this hop actually has an observation
        // item. A successor that recorded only a continuation legitimately has
        // no current queue.
        let mut queue = if session == &current_session {
            let settings = match cfg.research_tools.as_ref() {
                Some(settings) => settings,
                None => {
                    note_source_error("candidate_observation_queue_unavailable");
                    continue;
                }
            };
            match rusqlite::Connection::open_with_flags(
                &settings.queue,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            ) {
                Ok(queue) => queue,
                Err(_) => {
                    note_source_error("candidate_observation_queue_unavailable");
                    continue;
                }
            }
        } else {
            match open_original_queue_read_only_strict(c, cfg, session) {
                Ok(queue) => queue,
                Err(_) => {
                    note_source_error("candidate_observation_queue_unavailable");
                    continue;
                }
            }
        };
        for item in observation_items {
            scanned = scanned.saturating_add(1);
            let Some(id) = item["candidate_id"].as_str() else {
                note_source_error("candidate_observation_identity_missing");
                continue;
            };
            let expected_binding = match expected_source_book_binding(c, chain, session) {
                Ok(binding) => binding,
                Err(_) => {
                    note_source_error("candidate_observation_source_unverified");
                    continue;
                }
            };
            let task = match find_observation_task(&mut queue, session, id, &expected_binding) {
                Ok(task) => task,
                Err(error) => {
                    let code = error.to_string();
                    if code == "candidate_observation_task_truncated" {
                        truncated = true;
                        reason = Some("candidate_observation_task_truncated");
                    } else if code == "candidate_observation_task_unavailable" {
                        note_source_error("candidate_observation_task_unavailable");
                    } else if code == "candidate_observation_book_binding_mismatch" {
                        note_source_error("candidate_observation_book_binding_mismatch");
                    } else {
                        note_source_error("candidate_observation_task_missing");
                    }
                    continue;
                }
            };
            if task.status != jobs::TaskState::Succeeded {
                note_source_error("candidate_observation_task_not_terminal");
                continue;
            }
            let result = match read_verified_candidate_result(
                &mut queue,
                &task.spec.task_id,
                task.spec.revision,
            ) {
                Ok(Some(result)) => result,
                _ => {
                    note_source_error("candidate_observation_result_unverified");
                    continue;
                }
            };
            let Some(score) = result.observation.as_ref() else {
                note_source_error("candidate_observation_score_missing");
                continue;
            };
            let reference = match source_observation_ref_from_task(&task, score) {
                Ok(reference) => reference,
                Err(_) => {
                    note_source_error("candidate_observation_ref_unavailable");
                    continue;
                }
            };
            refs.push(json!({
                "source_session":session,
                "candidate_id":id,
                "observation_task_id":task.spec.task_id,
                "observation_revision":task.spec.revision,
                "source_observation_ref":reference,
                "read_only":true,
                "activation_authorized":false
            }));
            // Look ONE row past the display bound: an exact-limit list is
            // complete, and only an actual extra verified ref is truncation.
            if refs.len() > limit {
                refs.truncate(limit);
                truncated = true;
                if reason.is_none() {
                    reason = Some("observation_ref_limit");
                }
                break 'chain;
            }
        }
    }
    // A source error is never hidden behind a resource-bound truncation.
    let status = if partial {
        "partial"
    } else if truncated {
        "truncated"
    } else {
        "available"
    };
    Ok(ObservationRefsProjection {
        items: refs,
        status,
        truncated,
        partial,
        scanned,
        limit,
        reason,
        source_error_reason,
    })
}

/// Sanitized lifecycle library for the controller/UI projection. Every
/// observation measurement is checked by the canonical reader before it is
/// published; an invalid record is explicit unavailable with no PnL, no exit
/// status and no matured count. Legacy non-observation rows are untouched.
pub(crate) fn checked_lifecycle_library(
    c: &Config,
    cfg: &PlannerConfig,
    now: i64,
) -> Result<Value> {
    let session = c.session.to_string();
    let mut library = experiment_ledger::lifecycle::library(&c.directory, &session)?;
    let chain = predecessor_chain_verified_immutable(c)
        .unwrap_or_else(|_| vec![(session.clone(), c.directory.clone(), Value::Null)]);
    let mut matured = 0usize;
    if let Some(items) = library["items"].as_array_mut() {
        for item in items.iter_mut() {
            if !super::continuation::is_observation_item(item) {
                continue;
            }
            let checked = check_library_observation(c, cfg, &chain, item, now);
            if checked.matured {
                matured = matured.saturating_add(1);
            }
            item["measurement_status"] = json!(checked.status);
            item["measurement_reason"] = json!(checked.reason);
            // The EFFECTIVE verified measurement is published; the ORIGINAL
            // stored body is preserved separately so history truth is never
            // relabelled as the successor's effective state.
            item["original_observation_measurement"] =
                checked.original_measurement.unwrap_or(Value::Null);
            item["original_measurement_status"] = json!(checked.original_status);
            item["original_measurement_reason"] = json!(checked.original_reason);
            item["continuation_status"] = json!(checked.continuation_status);
            item["continuation_reason"] = json!(checked.continuation_reason);
            item["observation_measurement"] = checked.measurement.unwrap_or(Value::Null);
            if let Some(reference) = checked.source_ref {
                item["source_observation_ref"] = reference;
            }
        }
    }
    library["matured"] = json!(matured);
    library["measurement_projection"] = json!("canonical_checked_observation_only");
    Ok(library)
}

/// A bounded library-item projection whose stored measurement has been checked
/// by the canonical reader. An invalid record is explicit unavailable, never a
/// raw financial body or a fake zero.
pub(crate) struct CheckedLibraryObservation {
    /// The EFFECTIVE verified measurement (the verified continuation when one
    /// exists for the item's own episode, otherwise the checked current body).
    pub measurement: Option<Value>,
    pub source_ref: Option<Value>,
    pub status: &'static str,
    pub reason: Option<&'static str>,
    /// True ONLY when the canonical reader verified a complete flat executed
    /// measurement. A verified-but-unmatured unknown is never counted.
    pub matured: bool,
    /// The ORIGINAL stored body before continuation precedence, kept separately
    /// so history truth is never relabelled as the successor's effective state.
    pub original_measurement: Option<Value>,
    pub original_status: &'static str,
    pub original_reason: Option<&'static str>,
    /// Whether a verified continuation for THIS item's episode was found.
    pub continuation_status: &'static str,
    pub continuation_reason: Option<&'static str>,
}

fn find_observation_task(
    db: &mut Connection,
    session: &str,
    lifecycle_id: &str,
    expected_binding: &str,
) -> Result<jobs::CandidateTaskRecord> {
    let mut cursor = None;
    let mut scanned = 0usize;
    let mut saw_foreign = false;
    loop {
        let page = jobs::list_candidates_page(db, session, None, cursor.as_ref(), 100)
            .map_err(|_| "candidate_observation_task_unavailable")?;
        if page.is_empty() {
            break;
        }
        let full = page.len() == 100;
        scanned = scanned.saturating_add(page.len());
        cursor = page.last().map(|row| jobs::TaskCursor {
            created_at_ms: row.spec.created_at_ms,
            task_id: row.spec.task_id.clone(),
            revision: row.spec.revision,
        });
        for row in page {
            if row.spec.lifecycle_candidate_id != lifecycle_id {
                continue;
            }
            // A shared queue may hold a foreign book/account row with the same
            // lifecycle id. It is never the verified episode; keep scanning for
            // the row whose own binding is the verified session binding.
            if row.spec.book_binding != expected_binding {
                saw_foreign = true;
                continue;
            }
            return Ok(row);
        }
        if !full {
            break;
        }
        if scanned >= 10_000 {
            let extra = jobs::list_candidates_page(db, session, None, cursor.as_ref(), 1)
                .map_err(|_| "candidate_observation_task_unavailable")?;
            if extra.is_empty() {
                break;
            }
            return Err("candidate_observation_task_truncated".into());
        }
    }
    if saw_foreign {
        return Err("candidate_observation_book_binding_mismatch".into());
    }
    Err("candidate_observation_task_missing".into())
}

fn read_observation_spec(
    queue: &Connection,
    target: &jobs::ObservationTargetV1,
) -> Result<sadr_cli::candidate_execution::ObservationExecutionSpecV1> {
    let bytes =
        jobs::read_candidate_artifact(queue, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "candidate_observation_spec_missing")?;
    if digest(&bytes) != target.spec_sha256 {
        return Err("candidate_observation_spec_changed".into());
    }
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_observation_spec_invalid")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_observation_spec_invalid")?;
    Ok(spec)
}

/// The VERIFIED book binding for one session. The CURRENT session uses the
/// verified current book; a predecessor uses the `source_binding` recorded by
/// the verified carry hop that points at it. A shared queue row from a foreign
/// book/account is never accepted just because its ids match.
pub(crate) fn expected_source_book_binding(
    c: &Config,
    chain: &[(String, std::path::PathBuf, Value)],
    session: &str,
) -> Result<String> {
    if session == c.session.to_string() {
        return book_binding(c);
    }
    chain
        .iter()
        .find(|(_, _, carry)| carry["source_session"] == session)
        .and_then(|(_, _, carry)| carry["source_binding"].as_str())
        .map(str::to_owned)
        .ok_or_else(|| "candidate_observation_source_unverified".into())
}

/// Bind one queue task's recorded book to the VERIFIED session binding. The
/// check runs BEFORE any source artifact/result is read.
pub(crate) fn verify_task_book_binding(
    c: &Config,
    chain: &[(String, std::path::PathBuf, Value)],
    task: &jobs::CandidateTaskRecord,
) -> Result<()> {
    let expected = expected_source_book_binding(c, chain, &task.spec.session)?;
    if task.spec.book_binding != expected {
        return Err("candidate_observation_book_binding_mismatch".into());
    }
    Ok(())
}

/// Re-derive one CURRENT stored body against the verified task/score/spec.
#[allow(clippy::too_many_arguments)]
fn verify_current_library_body(
    c: &Config,
    queue: &Connection,
    directory: &Path,
    task: &jobs::CandidateTaskRecord,
    target: &jobs::ObservationTargetV1,
    score: &sadr_cli::candidate_execution::ObservationScoreV1,
    measurement: &Value,
    session: &str,
    immutable: bool,
    now: i64,
) -> Result<VerifiedStoredObservation> {
    let spec = read_observation_spec(queue, target)?;
    let previous_session = if immutable {
        session
            .parse::<chrono::NaiveDate>()
            .ok()
            .and_then(|day| day.pred_opt())
    } else {
        candidate_previous_trading_session(c)
    };
    let context = StoredObservationContext {
        source: directory,
        session: session
            .parse()
            .map_err(|_| "candidate_observation_session_invalid")?,
        previous_session,
        task,
        score,
        spec: &spec,
        target,
        kind: StoredObservationKind::CurrentMaturation,
        immutable,
        recorded_at_ms: None,
        source_runtime_hash: None,
        source_cohort_key: None,
        current_runtime_hash: None,
        current_cohort_key: None,
        now,
    };
    verify_stored_observation(&context, measurement)
}

fn continuation_matured(value: &Value) -> bool {
    value["state"] == "evaluated"
        && value["execution"].is_object()
        && value["flat"] == true
        && value["accounting_complete"] == true
}

/// The stored continuation body must bind to the ACTUAL ledger item identity:
/// the item's own session/lifecycle id and its verified task/score/entry. A
/// genuine X body copied into a Y projection never matches Y.
fn continuation_body_matches_item(
    body: &Value,
    item_score: &sadr_cli::candidate_execution::ObservationScoreV1,
    item_session: &str,
    item_lifecycle_id: &str,
) -> bool {
    let score_sha = serde_json::to_value(item_score)
        .ok()
        .map(|value| digest(&serde_json::to_vec(&value).unwrap_or_default()));
    let entry_sha = item_score
        .entry
        .as_ref()
        .and_then(|entry| serde_json::to_value(entry).ok())
        .map(|value| digest(&serde_json::to_vec(&value).unwrap_or_default()));
    let entry = item_score.entry.as_ref();
    body["source_lifecycle_candidate_id"].as_str() == Some(item_lifecycle_id)
        && body["source_session"].as_str() == Some(item_session)
        && body["source_task_id"].as_str() == Some(item_score.task_id.as_str())
        && body["source_revision"].as_u64() == Some(u64::from(item_score.revision))
        && body["source_candidate_id"].as_str() == Some(item_score.candidate_id.as_str())
        && body["source_observation_sha256"].as_str()
            == Some(item_score.observation_sha256.as_str())
        && body["source_score_sha256"].as_str() == score_sha.as_deref()
        && body["source_entry_sha256"].as_str() == entry_sha.as_deref()
        && entry.is_some_and(|entry| {
            body["entry"] == serde_json::to_value(entry).unwrap_or(Value::Null)
                && body["entry_quote"] == serde_json::to_value(&entry.quote).unwrap_or(Value::Null)
                && body["key"].as_str() == Some(entry.key.as_str())
        })
}

/// Verify ONE lifecycle library item's stored observation measurement against
/// the ORIGINAL verified source. The context is derived from the ACTUAL ledger
/// ITEM identity (session, lifecycle candidate, task, revision, score SHA)
/// first; the stored body may only echo it. A verified continuation for the
/// SAME episode is the effective state even when an older CURRENT record
/// exists; the original record is preserved separately. The source bytes are
/// never repaired.
pub(crate) fn check_library_observation(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, std::path::PathBuf, Value)],
    item: &Value,
    now: i64,
) -> CheckedLibraryObservation {
    let stored = item["observation_measurement"].clone();
    // Storage presence is authoritative. An absent/NULL measurement is the
    // genuine "not matured yet"; a PRESENT non-object body (string, number,
    // array, bool or explicit null) is corrupt evidence and must be
    // UNAVAILABLE, never a false not_matured.
    let present = item["observation_measurement_present"]
        .as_bool()
        .unwrap_or(stored.is_object());
    let stored_is_object = stored.is_object();
    let not_matured = !present && stored.is_null();
    let unavailable = |reason: &'static str| CheckedLibraryObservation {
        measurement: None,
        source_ref: None,
        status: "unavailable",
        reason: Some(reason),
        matured: false,
        original_measurement: stored_is_object.then(|| stored.clone()),
        original_status: if stored_is_object {
            "unavailable"
        } else {
            "not_matured"
        },
        original_reason: Some(reason),
        continuation_status: "not_recorded",
        continuation_reason: None,
    };
    if !not_matured && !stored_is_object {
        return unavailable("candidate_observation_measurement_unverified");
    }
    let session = match item["session"].as_str() {
        Some(session) => session,
        None => return unavailable("candidate_observation_session_missing"),
    };
    let lifecycle_id = match item["candidate_id"].as_str() {
        Some(id) => id,
        None => return unavailable("candidate_observation_identity_missing"),
    };
    // The ITEM's verified score is the identity authority, never the body.
    let item_score: sadr_cli::candidate_execution::ObservationScoreV1 =
        match serde_json::from_value(item["observation_score"].clone()) {
            Ok(score) => score,
            Err(_) => return unavailable("candidate_observation_score_missing"),
        };
    if sadr_cli::candidate_execution::validate_observation_score(&item_score).is_err() {
        return unavailable("candidate_observation_score_missing");
    }
    let (directory, immutable) = if session == c.session.to_string() {
        (c.directory.clone(), false)
    } else {
        match chain.iter().find(|(hop, _, _)| hop == session) {
            Some((_, directory, _)) => (directory.clone(), true),
            None => return unavailable("candidate_observation_source_unverified"),
        }
    };
    let mut queue = if immutable {
        match open_original_queue_read_only_strict(c, cfg, session) {
            Ok(queue) => queue,
            Err(_) => return unavailable("candidate_observation_queue_unavailable"),
        }
    } else {
        let Some(settings) = cfg.research_tools.as_ref() else {
            return unavailable("candidate_observation_queue_unavailable");
        };
        match rusqlite::Connection::open_with_flags(
            &settings.queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ) {
            Ok(queue) => queue,
            Err(_) => return unavailable("candidate_observation_queue_unavailable"),
        }
    };
    let expected_binding = match expected_source_book_binding(c, chain, session) {
        Ok(binding) => binding,
        Err(_) => return unavailable("candidate_observation_source_unverified"),
    };
    let task = match find_observation_task(&mut queue, session, lifecycle_id, &expected_binding) {
        Ok(task) => task,
        Err(error) => {
            let code = error.to_string();
            let reason = match code.as_str() {
                "candidate_observation_task_truncated" => "candidate_observation_task_truncated",
                "candidate_observation_task_unavailable" => {
                    "candidate_observation_task_unavailable"
                }
                "candidate_observation_book_binding_mismatch" => {
                    "candidate_observation_book_binding_mismatch"
                }
                _ => "candidate_observation_task_missing",
            };
            return unavailable(reason);
        }
    };
    // The queue row must reproduce the ITEM's own identity.
    if task.spec.session != session
        || task.spec.lifecycle_candidate_id != lifecycle_id
        || task.spec.candidate_id != item_score.candidate_id
        || task.spec.task_id != item_score.task_id
        || task.spec.revision != item_score.revision
        || task.spec.book_binding != expected_binding
    {
        return unavailable("candidate_observation_identity_mismatch");
    }
    let target = match &task.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) if target.validate() => {
            target.as_ref().clone()
        }
        _ => return unavailable("candidate_observation_target_invalid"),
    };
    if task.status != jobs::TaskState::Succeeded {
        return unavailable("candidate_observation_result_unverified");
    }
    let result =
        match read_verified_candidate_result(&mut queue, &task.spec.task_id, task.spec.revision) {
            Ok(Some(result)) => result,
            _ => return unavailable("candidate_observation_result_unverified"),
        };
    let Some(score) = result.observation.as_ref() else {
        return unavailable("candidate_observation_score_missing");
    };
    if serde_json::to_value(score).ok() != Some(item["observation_score"].clone()) {
        return unavailable("candidate_observation_score_mismatch");
    }
    let source_ref = source_observation_ref_from_task(&task, score).ok();
    if not_matured {
        return CheckedLibraryObservation {
            measurement: None,
            source_ref,
            status: "not_matured",
            reason: None,
            matured: false,
            original_measurement: None,
            original_status: "not_matured",
            original_reason: None,
            continuation_status: "not_recorded",
            continuation_reason: None,
        };
    }
    // A verified continuation for THIS item's episode is the effective state.
    // A broken lineage is an explicit unavailable continuation, never a silent
    // absence and never a reason to trust a stored body on its own.
    let (continuations, continuation_error) = match verified_continuations_from_chain(c, cfg, chain)
    {
        Ok(values) => (values, None),
        Err(error) => (Vec::new(), Some(bounded_context_reason(error.as_ref()))),
    };
    let own_continuation = continuations.iter().find(|value| {
        value["source_lifecycle_candidate_id"] == lifecycle_id && value["source_session"] == session
    });
    let continuation_status = if own_continuation.is_some() {
        "verified"
    } else if continuation_error.is_some() {
        "unavailable"
    } else {
        "not_recorded"
    };
    // The ORIGINAL stored body's own status, computed separately from the
    // effective state so history truth is never relabelled.
    let (original_status, original_reason) = if stored_is_object {
        if stored["continuation_id"].is_string() {
            if continuation_body_matches_item(&stored, &item_score, session, lifecycle_id) {
                let found = own_continuation.is_some_and(|value| {
                    value["measurement_sha256"] == stored["measurement_sha256"]
                });
                if found {
                    ("verified", None)
                } else {
                    (
                        "unavailable",
                        Some("candidate_observation_continuation_unverified"),
                    )
                }
            } else {
                (
                    "unavailable",
                    Some("candidate_observation_identity_mismatch"),
                )
            }
        } else {
            match verify_current_library_body(
                c, &queue, &directory, &task, &target, score, &stored, session, immutable, now,
            ) {
                Ok(_) => ("verified", None),
                Err(_) => (
                    "unavailable",
                    Some("candidate_observation_measurement_unverified"),
                ),
            }
        }
    } else {
        ("not_matured", None)
    };
    if let Some(continuation) = own_continuation {
        if continuation["source_task_id"].as_str() == Some(item_score.task_id.as_str())
            && continuation["source_revision"].as_u64() == Some(u64::from(item_score.revision))
            && continuation["source_observation_sha256"].as_str()
                == Some(item_score.observation_sha256.as_str())
            && continuation["source_candidate_id"].as_str()
                == Some(item_score.candidate_id.as_str())
        {
            return CheckedLibraryObservation {
                measurement: Some(continuation.clone()),
                source_ref,
                status: "verified",
                reason: None,
                matured: continuation_matured(continuation),
                original_measurement: stored_is_object.then(|| stored.clone()),
                original_status,
                original_reason,
                continuation_status: "verified",
                continuation_reason: None,
            };
        }
        return unavailable("candidate_observation_identity_mismatch");
    }
    if stored["continuation_id"].is_string() {
        // No verified continuation for this episode: the stored body is never
        // trusted on its own, and a copied body is an explicit identity error.
        let reason = if continuation_body_matches_item(&stored, &item_score, session, lifecycle_id)
        {
            "candidate_observation_continuation_unverified"
        } else {
            "candidate_observation_identity_mismatch"
        };
        return CheckedLibraryObservation {
            measurement: None,
            source_ref,
            status: "unavailable",
            reason: Some(reason),
            matured: false,
            original_measurement: Some(stored.clone()),
            original_status,
            original_reason,
            continuation_status,
            continuation_reason: continuation_error,
        };
    }
    match verify_current_library_body(
        c, &queue, &directory, &task, &target, score, &stored, session, immutable, now,
    ) {
        Ok(verified) => CheckedLibraryObservation {
            measurement: Some(stored.clone()),
            source_ref,
            status: "verified",
            reason: None,
            matured: verified.matured,
            original_measurement: Some(stored.clone()),
            original_status: "verified",
            original_reason: None,
            continuation_status,
            continuation_reason: continuation_error,
        },
        Err(_) => CheckedLibraryObservation {
            measurement: None,
            source_ref,
            status: "unavailable",
            reason: Some("candidate_observation_measurement_unverified"),
            matured: false,
            original_measurement: Some(stored.clone()),
            original_status,
            original_reason,
            continuation_status,
            continuation_reason: continuation_error,
        },
    }
}

pub(crate) fn observation_status(c: &Config, cfg: &PlannerConfig, now: i64) -> Value {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return json!({"status":"disabled","reason":"research_disabled"});
    };
    let Some(observation) = settings
        .candidate_compute
        .as_ref()
        .and_then(|candidate| candidate.observation.as_ref())
        .filter(|observation| observation.enabled)
    else {
        return json!({"status":"disabled","reason":"candidate_observation_disabled"});
    };
    // A fresh successor may not have created its own queue yet. A missing
    // current queue is an empty current projection; an existing unreadable
    // queue is still unavailable.
    let mut db = if settings.queue.is_file() {
        match rusqlite::Connection::open_with_flags(
            &settings.queue,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) {
            Ok(db) => Some(db),
            Err(_) => {
                return json!({"status":"unavailable","reason":"candidate_task_state_unavailable"});
            }
        }
    } else {
        None
    };
    // Page the original queue instead of silently truncating at 100 rows.
    let session = c.session.to_string();
    let mut rows = Vec::new();
    let mut queue_truncated = false;
    if let Some(db) = db.as_mut() {
        let mut cursor = None;
        for _ in 0..10 {
            let Ok(page) = jobs::list_candidates_page(db, &session, None, cursor.as_ref(), 100)
            else {
                return json!({"status":"unavailable","reason":"candidate_task_state_unavailable"});
            };
            if page.is_empty() {
                break;
            }
            let full = page.len() == 100;
            cursor = page.last().map(|row| jobs::TaskCursor {
                created_at_ms: row.spec.created_at_ms,
                task_id: row.spec.task_id.clone(),
                revision: row.spec.revision,
            });
            rows.extend(page);
            if !full {
                break;
            }
            if rows.len() >= 1_000 {
                // Exact 1000 is not truncated unless an actual extra row exists.
                queue_truncated =
                    jobs::list_candidates_page(db, &session, None, cursor.as_ref(), 1)
                        .map(|page| !page.is_empty())
                        .unwrap_or(true);
                break;
            }
        }
    }
    let observations = rows
        .iter()
        .filter(|row| row.spec.purpose.is_some())
        .collect::<Vec<_>>();
    let pending = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Pending)
        .count();
    let running = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Running)
        .count();
    let unknown = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Unknown)
        .count();
    let cancel_requested = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::CancelRequested)
        .count();
    let succeeded = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Succeeded)
        .count();
    let oldest = observations.iter().map(|row| row.spec.created_at_ms).min();
    let deadline = observations
        .iter()
        .filter(|row| row.status == jobs::TaskState::Pending)
        .map(|row| row.spec.expires_at_ms)
        .min();
    // Fold every verified predecessor lifecycle ledger, not only the nearest
    // one. A broken chain or unreadable ledger is unavailable, never zero.
    let chain = match super::carry::predecessor_chain_verified_immutable(c) {
        Ok(chain) => chain,
        Err(reason) => {
            return json!({
                "status":"unavailable",
                "reason":reason.to_string(),
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        }
    };
    if current_lifecycle_legacy_ambiguous(c, cfg).unwrap_or(true) {
        return json!({
            "status":"unavailable",
            "reason":"candidate_predecessor_lifecycle_legacy_ambiguous",
            "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
            "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
            "trials":super::carry::trial_fold(c, cfg),
            "activation_authorized":false,
            "financial_profit_claim":false,
            "as_of_ms":now
        });
    }
    let mut lifecycle_truncated = false;
    let mut items = match experiment_ledger::lifecycle::library_optional(&c.directory, &session) {
        Ok(library) => {
            lifecycle_truncated |= library["truncated"].as_bool().unwrap_or(false);
            library["items"].as_array().cloned().unwrap_or_default()
        }
        Err(_) => {
            return json!({
                "status":"unavailable",
                "reason":"candidate_lifecycle_unavailable",
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        }
    };
    let mut seen = items
        .iter()
        .filter_map(|item| item["candidate_id"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    for (source_session, directory, _) in &chain {
        if source_session == &session {
            continue;
        }
        let Ok(previous) = experiment_ledger::lifecycle::library_checked_immutable(directory, "")
        else {
            return json!({
                "status":"unavailable",
                "reason":"candidate_predecessor_lifecycle_unavailable",
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        };
        lifecycle_truncated |= previous["truncated"].as_bool().unwrap_or(false);
        for mut item in previous["items"].as_array().cloned().unwrap_or_default() {
            let Some(id) = item["candidate_id"].as_str().map(str::to_owned) else {
                continue;
            };
            if !seen.insert(id) {
                continue;
            }
            item["continued"] = json!(true);
            item["origin_session"] = json!(source_session);
            items.push(item);
        }
    }
    // The CURRENT continuation is the latest verified state of the ORIGINAL
    // episode. It is attached by original identity, never added as a second
    // candidate or counted as a second trial.
    // The canonical verified lineage resolver: a continuation recorded by ANY
    // earlier verified successor is the latest state of the ORIGINAL episode.
    // Its recording scope is preserved; a later successor never re-records it.
    let continuations = match super::verified_continuations(c, cfg) {
        Ok(continuations) => continuations,
        Err(reason) => {
            return json!({
                "status":"unavailable",
                "reason":reason.to_string(),
                "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
                "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]},
                "trials":super::carry::trial_fold(c, cfg),
                "activation_authorized":false,
                "financial_profit_claim":false,
                "as_of_ms":now
            });
        }
    };
    for item in &mut items {
        let Some(id) = item["candidate_id"].as_str() else {
            continue;
        };
        if let Some(continuation) = continuations
            .iter()
            .find(|continuation| continuation["source_lifecycle_candidate_id"] == id)
        {
            item["continuation"] = continuation.clone();
            item["continuation_recorded_in_session"] =
                continuation["current_scope"]["current_session"].clone();
            item["continued"] = json!(true);
            if item["observation_measurement"].is_null() {
                item["observation_measurement"] = continuation.clone();
            }
        }
    }
    let observation_items = items
        .iter()
        .filter(|item| {
            item["feature_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI
                || item["strategy_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI
        })
        .collect::<Vec<_>>();
    // Publish ONLY canonically checked measurements. A forged/invalid stored
    // body is explicit unavailable with no PnL, no exit status and no matured
    // count; it is never passed through raw from the lifecycle projection.
    let mut milestones = Vec::new();
    let mut matured = 0usize;
    for item in &observation_items {
        let checked = check_library_observation(c, cfg, &chain, item, now);
        if checked.matured {
            matured = matured.saturating_add(1);
        }
        let measurement = checked.measurement.clone().unwrap_or(Value::Null);
        // ONLY the canonical reader may mint a verified source ref. A raw
        // observation_score + job hash is not proof that the result artifact
        // re-verifies; a corrupt artifact publishes an unavailable ref status,
        // never a fabricated verified ref.
        let source_ref = checked.source_ref.clone();
        let source_ref_status = if source_ref.is_some() {
            "verified"
        } else if checked.status == "not_matured" {
            "not_matured"
        } else {
            "unavailable"
        };
        let source_ref_reason = if source_ref.is_some() {
            Value::Null
        } else if checked.status == "not_matured" {
            json!("candidate_observation_not_matured")
        } else {
            json!("candidate_observation_source_ref_unverified")
        };
        milestones.push(json!({
            "candidate_id":item["candidate_id"],
            "state":item["state"],
            "original_recorded_state":item["state"],
            "effective_state":if item["continuation"].is_object() { item["continuation"]["state"].clone() } else { item["state"].clone() },
            // The effective lifecycle state is separate from the recorded
            // scientific vocabulary. A continued `evaluated` episode is
            // `insufficient_evidence`, never an invented
            // `continued_evaluated` qualification.
            "effective_scientific_state":if item["continuation"].is_object() {
                experiment_ledger::lifecycle::scientific_state(
                    item["continuation"]["state"].as_str().unwrap_or("unknown"),
                    !item["effective_observation_measurement"].is_null(),
                )
            } else {
                item["scientific_state"].as_str().unwrap_or("unknown")
            },
            "origin_session":item["origin_session"],
            "continued":item["continued"],
            "observation_scope":item["observation_score"]["observation_scope"],
            "status":item["observation_score"]["status"],
            "gaps":item["observation_score"]["gaps"],
            "measurement_status":checked.status,
            "measurement_reason":checked.reason,
            "measurement_scope":measurement["source_scope"],
            "exit_status":measurement["exit_status"],
            "measurement_gaps":measurement["gaps"],
            "measurement_sha256":measurement["measurement_sha256"],
            // The ORIGINAL recorded body is preserved separately from the
            // effective scientific/lifecycle state. A successor never
            // relabels the predecessor's history as its own receipt.
            "original_measurement_sha256":checked.original_measurement.as_ref().map(|body| body["measurement_sha256"].clone()).unwrap_or(Value::Null),
            "original_exit_status":checked.original_measurement.as_ref().map(|body| body["exit_status"].clone()).unwrap_or(Value::Null),
            "original_measurement_status":checked.original_status,
            "original_measurement_reason":checked.original_reason,
            "continuation_status":checked.continuation_status,
            "continuation_reason":checked.continuation_reason,
            "continuation_state":item["continuation"]["state"],
            "continuation_recorded_in_session":item["continuation_recorded_in_session"],
            "processed_at_ms":item["continuation"]["processed_at_ms"],
            "exit_at_ms":item["continuation"]["exit_at_ms"],
            "source_observation_ref":source_ref,
            "source_observation_ref_status":source_ref_status,
            "source_observation_ref_reason":source_ref_reason
        }));
    }
    let trials = super::carry::trial_fold(c, cfg);
    let capture = super::capture::status(c, cfg, now);
    let (
        observation_refs,
        observation_refs_status,
        observation_refs_truncated,
        observation_refs_partial,
        observation_refs_scanned,
        observation_refs_limit,
        observation_refs_reason,
        observation_refs_source_error_reason,
    ) = match observation_refs_from_chain(c, cfg, &chain, 16) {
        Ok(refs) => (
            refs.items,
            refs.status,
            refs.truncated,
            refs.partial,
            refs.scanned,
            refs.limit,
            refs.reason,
            refs.source_error_reason,
        ),
        Err(_) => (
            Vec::new(),
            "unavailable",
            false,
            false,
            0usize,
            16usize,
            Some("observation_refs_unavailable"),
            Some("observation_refs_unavailable"),
        ),
    };
    json!({
        "status":"available",
        "queue":queue_projection(pending, running, unknown, cancel_requested, succeeded, observations.len(), oldest, deadline, now, queue_truncated),
        "lifecycle":{
            "status":"available",
            "recorded":observation_items.len(),
            "matured":matured,
            "truncated":lifecycle_truncated,
            "milestones":milestones
        },
        "cohort":{
            "account":c.account,
            "cohort_key":super::carry::current_cohort_key(c,cfg).ok(),
            "feed":"wide_quotes",
            "dataset_abi":"scorer_replay_v1"
        },
        "trials":trials,
        "observation_refs":observation_refs,
        "observation_refs_status":observation_refs_status,
        "observation_refs_truncated":observation_refs_truncated,
        "observation_refs_partial":observation_refs_partial,
        "observation_refs_scanned":observation_refs_scanned,
        "observation_refs_limit":observation_refs_limit,
        "observation_refs_reason":observation_refs_reason,
        "observation_refs_source_error_reason":observation_refs_source_error_reason,
        "abi":observation.abi,
        "measurement":{
            "evaluation_scope":"independent_episodes",
            "strategy_portfolio_claimed":false,
            "portfolio_return":null,
            "aggregate_net_pnl":null
        },
        "capture":capture,
        "financial_profit_claim":false,
        "activation_authorized":false,
        "as_of_ms":now,
        "source":"original_queue_and_lifecycle_read_only"
    })
}

#[allow(clippy::too_many_arguments)]
fn queue_projection(
    pending: usize,
    running: usize,
    unknown: usize,
    cancel_requested: usize,
    succeeded: usize,
    total: usize,
    oldest: Option<i64>,
    deadline: Option<i64>,
    now: i64,
    truncated: bool,
) -> Value {
    json!({
        "pending":pending,
        "running":running,
        "unknown":unknown,
        "cancel_requested":cancel_requested,
        "succeeded":succeeded,
        "total":total,
        "truncated":truncated,
        "oldest_created_ms":oldest,
        "oldest_age_ms":oldest.map(|created| now.saturating_sub(created)),
        "next_deadline_ms":deadline,
        "deadline_remaining_ms":deadline.map(|deadline| deadline.saturating_sub(now))
    })
}

pub(crate) fn mature_candidates(c: &Config, cfg: &PlannerConfig, now: i64) -> Result<Value> {
    let Some(settings) = cfg.research_tools.as_ref() else {
        return Ok(json!({"status":"disabled","items":[],"activation_authorized":false}));
    };
    if !settings
        .candidate_compute
        .as_ref()
        .is_some_and(|candidate| candidate.enabled)
    {
        return Ok(json!({"status":"disabled","items":[],"activation_authorized":false}));
    }
    // A fresh successor may have no current lifecycle ledger yet. A missing
    // ledger is an empty current projection; an existing corrupt ledger is
    // still unavailable. The predecessor continuation path below records into
    // the CURRENT ledger only.
    if current_lifecycle_legacy_ambiguous(c, cfg).unwrap_or(true) {
        return Ok(json!({
            "status":"unavailable",
            "reason":"candidate_predecessor_lifecycle_legacy_ambiguous",
            "items":[],
            "activation_authorized":false
        }));
    }
    let library = match experiment_ledger::lifecycle::library_optional(
        &c.directory,
        &c.session.to_string(),
    ) {
        Ok(library) => library,
        Err(_) => {
            return Ok(json!({
                "status":"unavailable",
                "reason":"candidate_lifecycle_unavailable",
                "items":[],
                "activation_authorized":false
            }));
        }
    };
    let Some(items) = library["items"].as_array().cloned() else {
        return Ok(json!({
            "status":"unavailable",
            "reason":"candidate_lifecycle_invalid",
            "items":[],
            "activation_authorized":false
        }));
    };
    // A fresh successor may not have created its own queue yet. The original
    // queues are opened separately, read-only, through the verified locator.
    let mut queue = if settings.queue.is_file() {
        Some(
            rusqlite::Connection::open_with_flags(
                &settings.queue,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|_| "candidate_task_state_unavailable")?,
        )
    } else {
        None
    };
    // Page the declared queue bound instead of silently skipping observations
    // after the first 100 rows. A scan cap is an explicit deferral, never a
    // permanent "replayed but unseen" state.
    let session = c.session.to_string();
    let mut candidates = Vec::new();
    let mut scan_truncated = false;
    if let Some(queue) = queue.as_mut() {
        let mut cursor = None;
        loop {
            let page = jobs::list_candidates_page(queue, &session, None, cursor.as_ref(), 100)
                .map_err(|_| "candidate_task_state_unavailable")?;
            if page.is_empty() {
                break;
            }
            let full = page.len() == 100;
            cursor = page.last().map(|row| jobs::TaskCursor {
                created_at_ms: row.spec.created_at_ms,
                task_id: row.spec.task_id.clone(),
                revision: row.spec.revision,
            });
            candidates.extend(page);
            if !full {
                break;
            }
            if candidates.len() >= 10_000 {
                // Exact 10k is not truncation unless an actual extra row
                // exists. Never claim a truncation the queue does not have.
                scan_truncated =
                    jobs::list_candidates_page(queue, &session, None, cursor.as_ref(), 1)
                        .map(|page| !page.is_empty())
                        .unwrap_or(true);
                break;
            }
        }
    }
    let by_lifecycle = candidates
        .iter()
        .filter(|task| task.spec.purpose.is_some())
        .map(|task| (task.spec.lifecycle_candidate_id.clone(), task))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut seen_lifecycles = std::collections::BTreeSet::new();
    let mut matured = Vec::new();
    for item in items {
        let Some(id) = item["candidate_id"].as_str() else {
            continue;
        };
        seen_lifecycles.insert(id.to_owned());
        let state = item["state"].as_str().unwrap_or_default();
        if (item["feature_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI
            || item["strategy_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI)
            && state == "replayed"
        {
            let Some(task) = by_lifecycle
                .get(id)
                .filter(|task| task.status == jobs::TaskState::Succeeded)
            else {
                continue;
            };
            let result = match queue.as_mut().and_then(|queue| {
                read_verified_candidate_result(queue, &task.spec.task_id, task.spec.revision)
                    .ok()
                    .flatten()
            }) {
                Some(result) => result,
                _ => {
                    matured.push(json!({
                        "candidate_id":id,
                        "state":"unavailable",
                        "reason":"candidate_observation_result_unverified",
                        "activation_authorized":false,
                        "edge_qualified":false
                    }));
                    continue;
                }
            };
            match queue
                .as_mut()
                .map(|queue| mature_observation(c, cfg, queue, task, &result, now))
                .unwrap_or_else(|| Err("candidate_task_state_unavailable".into()))
            {
                Ok(MaturationDecision::Waiting { due_ms }) => matured.push(json!({
                    "candidate_id":id,
                    "state":"waiting",
                    "due_ms":due_ms,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(MaturationDecision::PendingCapture {
                    due_ms,
                    deadline_ms,
                    gap,
                }) => matured.push(json!({
                    "candidate_id":id,
                    "state":"pending_capture",
                    "due_ms":due_ms,
                    "deadline_ms":deadline_ms,
                    "gap":gap,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(MaturationDecision::Matured(measurement)) => matured.push(json!({
                    "candidate_id":id,
                    "state":"evaluated",
                    "observation":measurement,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(MaturationDecision::Unknown(measurement)) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unknown",
                    "observation":measurement,
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Err(error) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_observation_maturation_unavailable",
                    "detail":error.to_string().chars().take(256).collect::<String>(),
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
            }
            continue;
        }
        if state == "evaluated" {
            match experiment_ledger::lifecycle::conclude_if_ready(&c.directory, id, now) {
                Ok(Some(receipt)) => matured.push(json!({
                    "candidate_id":id,
                    "state":receipt["state"],
                    "marks":receipt["marks"],
                    "checks":receipt["checks"],
                    "observation":receipt["observation_measurement"],
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(None) => {}
                Err(error) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_conclusion_unavailable",
                    "detail":error.to_string().chars().take(256).collect::<String>(),
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
            }
            continue;
        }
        if state != "forward_shadow" {
            continue;
        }
        let marks = experiment_ledger::lifecycle::pending_marks(&c.directory, id)?;
        if marks.is_empty() {
            match experiment_ledger::lifecycle::conclude_if_ready(&c.directory, id, now) {
                Ok(Some(receipt)) => matured.push(json!({
                    "candidate_id":id,
                    "state":receipt["state"],
                    "marks":receipt["marks"],
                    "checks":receipt["checks"],
                    "observation":receipt["observation_measurement"],
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
                Ok(None) => {}
                Err(error) => matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_conclusion_unavailable",
                    "detail":error.to_string().chars().take(256).collect::<String>(),
                    "activation_authorized":false,
                    "edge_qualified":false
                })),
            }
            continue;
        }
        let mut keys = BTreeSet::new();
        let mut min_due = i64::MAX;
        let mut max_due = i64::MIN;
        for mark in &marks {
            if let (Some(key), Some(due)) = (mark["key"].as_str(), mark["due_ms"].as_i64()) {
                keys.insert(key.to_owned());
                min_due = min_due.min(due);
                max_due = max_due.max(due);
            }
        }
        if keys.is_empty() || min_due == i64::MAX {
            continue;
        }
        let declared_exit_delay = item["max_exit_delay_ms"]
            .as_i64()
            .unwrap_or(CANDIDATE_MAX_EXIT_DELAY_MS);
        let quotes = match super::super::super::paper_replay::candidate_quotes(
            &c.directory,
            c.session,
            &keys,
            min_due,
            max_due.saturating_add(declared_exit_delay),
            10_000,
            now,
        ) {
            Ok(quotes) => quotes,
            Err(_) => {
                matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_quote_evidence_unavailable",
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            }
        };
        match experiment_ledger::lifecycle::mature(&c.directory, id, &quotes, now) {
            Ok(receipt) => matured.push(json!({
                "candidate_id":id,
                "state":receipt["state"],
                "marks":receipt["marks"],
                "checks":receipt["checks"],
                "coverage":quotes["coverage"],
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Err(error) => matured.push(json!({
                "candidate_id":id,
                "state":"unavailable",
                "reason":"candidate_maturation_unavailable",
                "detail":error.to_string().chars().take(256).collect::<String>(),
                "activation_authorized":false,
                "edge_qualified":false
            })),
        }
    }
    // A replayed observation whose library row fell outside the bounded
    // library page is still eligible by its own queue row + lifecycle state.
    for task in candidates.iter().filter(|task| {
        task.spec.purpose.is_some()
            && task.status == jobs::TaskState::Succeeded
            && !seen_lifecycles.contains(&task.spec.lifecycle_candidate_id)
    }) {
        let id = task.spec.lifecycle_candidate_id.clone();
        if experiment_ledger::lifecycle::state(&c.directory, &id)
            .ok()
            .flatten()
            .as_deref()
            != Some("replayed")
        {
            continue;
        }
        let result = match queue.as_mut().and_then(|queue| {
            read_verified_candidate_result(queue, &task.spec.task_id, task.spec.revision)
                .ok()
                .flatten()
        }) {
            Some(result) => result,
            _ => {
                matured.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_observation_result_unverified",
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            }
        };
        match queue
            .as_mut()
            .map(|queue| mature_observation(c, cfg, queue, task, &result, now))
            .unwrap_or_else(|| Err("candidate_task_state_unavailable".into()))
        {
            Ok(MaturationDecision::Waiting { due_ms }) => matured.push(json!({
                "candidate_id":id,
                "state":"waiting",
                "due_ms":due_ms,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Ok(MaturationDecision::PendingCapture {
                due_ms,
                deadline_ms,
                gap,
            }) => matured.push(json!({
                "candidate_id":id,
                "state":"pending_capture",
                "due_ms":due_ms,
                "deadline_ms":deadline_ms,
                "gap":gap,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Ok(MaturationDecision::Matured(measurement)) => matured.push(json!({
                "candidate_id":id,
                "state":"evaluated",
                "observation":measurement,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Ok(MaturationDecision::Unknown(measurement)) => matured.push(json!({
                "candidate_id":id,
                "state":"unknown",
                "observation":measurement,
                "activation_authorized":false,
                "edge_qualified":false
            })),
            Err(error) => matured.push(json!({
                "candidate_id":id,
                "state":"unavailable",
                "reason":"candidate_observation_maturation_unavailable",
                "detail":error.to_string().chars().take(256).collect::<String>(),
                "activation_authorized":false,
                "edge_qualified":false
            })),
        }
    }
    // Cross-session continuation: the CURRENT successor matures an ORIGINAL
    // open observation episode from the verified predecessor chain and appends
    // its own continuation receipt. It never writes the predecessor.
    match mature_predecessors(c, cfg, now) {
        Ok(items) => matured.extend(items),
        Err(error) => matured.push(json!({
            "candidate_id":Value::Null,
            "state":"unavailable",
            "reason":"candidate_predecessor_continuation_unavailable",
            "detail":error.to_string().chars().take(256).collect::<String>(),
            "activation_authorized":false,
            "edge_qualified":false
        })),
    }
    Ok(json!({
        "status":if matured.is_empty(){"idle"}else{"matured"},
        "items":matured,
        "scanned":candidates.len(),
        "truncated":scan_truncated,
        "deferred":if scan_truncated {json!(candidates.len())} else {Value::Null},
        "activation_authorized":false
    }))
}
