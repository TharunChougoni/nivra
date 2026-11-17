//! Host-side candidate authoring, asynchronous replay and verified receipts.
//! Source runs only through the existing no-network `nivra_sandbox::score_source`;
//! this module never promotes, activates or writes the paper book.
use super::super::super::experiment_ledger;
use super::*;
use nivra_cli::scorer_compare::{self, ScorerReplayReport, ScorerReplaySettings};
use nivra_core::finance::{Amount, FeeArithmetic};
use nivra_research::worker;
use nivra_store::research_jobs as jobs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
mod submit;
pub(crate) use submit::*;
mod result;
pub(crate) use result::*;
mod launch;
pub(crate) use launch::*;
mod helper;
pub(crate) use helper::*;
mod drive;
pub(crate) use drive::*;
mod reconcile;
pub(crate) use reconcile::*;
mod maturation;
pub(crate) use maturation::*;
mod library;
pub(crate) use library::*;
mod status;
pub(crate) use status::*;

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
    Option<Box<nivra_cli::candidate_execution::CandidateExecutionSpecV1>>,
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
    pub execution: Option<Box<nivra_cli::candidate_execution::CandidateExecutionSpecV1>>,
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
    pub advisory: nivra_sandbox::BwrapLeaderReceipt,
    /// Present only when the launch carries an execution binding. The helper
    /// runs the pure adapter after the scorer replay; the Python sandbox never
    /// sees these artifacts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<nivra_cli::candidate_execution::CandidateExecutionReportV1>,
    /// Present only for a prospective-observation task. The helper writes the
    /// typed score/entry decision before the controller stores the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<nivra_cli::candidate_execution::ObservationScoreV1>,
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
    pub advisory: nivra_sandbox::BwrapLeaderReceipt,
    pub qualified: bool,
    pub analysis_status: String,
    pub report: ScorerReplayReport,
    pub report_sha256: String,
    /// Present only for an execution task. The full adapter report embeds the
    /// exact spec, scored signals, action tape, declared stress costs and shared
    /// engine results, so every claimed hash is reproducible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<nivra_cli::candidate_execution::CandidateExecutionReportV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<nivra_cli::candidate_execution::ObservationScoreV1>,
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
    let sandbox_source_sha256 = digest(include_bytes!("../../../../../nivra-sandbox/src/lib.rs"));
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
            nivra_cli::candidate_execution::OBSERVATION_ABI.to_string(),
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
        nivra_cli::candidate_execution::RESULT_KIND_OBSERVATION
    } else if record.spec.execution.is_some() {
        nivra_cli::candidate_execution::RESULT_KIND_EXECUTED
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
