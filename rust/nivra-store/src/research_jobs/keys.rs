//! Content-addressed task, job and trial keys.
use super::*;

pub fn scoped_task_id(book_binding: &str, session: &str, task_key: &str) -> String {
    sha256_hex(format!("owner-research-task-v1:{book_binding}:{session}:{task_key}").as_bytes())
}
pub fn scoped_job_id(task_id: &str, revision: u32) -> String {
    sha256_hex(format!("owner-research-job-v1:{task_id}:{revision}").as_bytes())
}
/// Fingerprint of the actual scorer engine sources plus the fixed helper and
/// owned-unit containment policy. This is the runtime identity a candidate job
/// pins; the string labels the convention and the hashes carry the provenance.
pub fn candidate_runtime_hash(
    scorer_compare_sha256: &str,
    sandbox_source_sha256: &str,
    helper_sha256: &str,
    unit_policy_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-runtime-source-v2\0scorer_replay_v1\0sadr_sandbox::score_source\0{scorer_compare_sha256}\0{sandbox_source_sha256}\0{helper_sha256}\0{unit_policy_sha256}"
        )
        .as_bytes(),
    )
}
pub fn candidate_input_hash(params_sha256: &str, dataset_hash: &str) -> String {
    sha256_hex(format!("candidate-input-v1\0{params_sha256}\0{dataset_hash}").as_bytes())
}
/// Execution-only runtime identity over the recorded scorer runtime binding and
/// the exact pinned sandbox policy, including the actual Python/bwrap content
/// hashes and every containment cap. The ranking runtime hash does not cover the
/// interpreter/bwrap bytes or the sandbox caps, so an executable upgrade under
/// the same helper could otherwise reuse an old execution version. This hash is
/// deliberately used only by execution-aware identities; ranking-only task
/// bytes remain unchanged.
pub fn candidate_execution_runtime_hash(
    runtime: &CandidateRuntimeBinding,
    sandbox: &CandidateSandboxPolicy,
) -> String {
    let mut payload = format!(
        "candidate-execution-runtime-v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        runtime.kind,
        runtime.abi,
        runtime.engine,
        runtime.scorer_compare_sha256,
        runtime.sandbox_source_sha256,
        runtime.helper_sha256,
        runtime.unit_policy_sha256,
        runtime.runtime_hash,
        sandbox.abi,
        sandbox.engine,
        sandbox.network,
        sandbox.cpu_seconds,
        sandbox.memory_mb,
        sandbox.wall_ms,
        sandbox.max_output_bytes,
        sandbox.max_keys,
        sandbox.max_processes,
        sandbox.max_open_files,
        sandbox.executable_scope,
        sandbox.python_sha256,
        sandbox.bwrap_sha256
    );
    if sandbox.env_dir.is_some() || sandbox.env_sha256.is_some() {
        payload.push_str(&format!(
            "\0{}\0{}",
            sandbox.env_dir.as_deref().unwrap_or(""),
            sandbox.env_sha256.as_deref().unwrap_or("")
        ));
    }
    sha256_hex(payload.as_bytes())
}
/// Execution-aware job input identity. The scorer input alone is insufficient:
/// the declared execution rules, the exact frozen manifest/events tape and the
/// actual sandbox runtime determine what the helper may replay, so they are part
/// of the same immutable input allowance. Hypothesis prose, task keys and parent
/// links are excluded.
pub fn candidate_execution_input_hash(
    params_sha256: &str,
    dataset_hash: &str,
    execution: &CandidateExecutionBinding,
    execution_runtime_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-input-v2\0{params_sha256}\0{dataset_hash}\0{}\0{}\0{}\0{}\0{execution_runtime_sha256}",
            execution.abi,
            execution
                .rules_sha256
                .as_deref()
                .unwrap_or(&execution.spec_sha256),
            execution.manifest_sha256,
            execution.events_sha256
        )
        .as_bytes(),
    )
}
/// Content identity excludes the parent and hypothesis prose so an identical
/// resubmission is detectable even when a caller names a different parent or
/// rewrites the description. Declared windows, horizon and ABI are part of the
/// immutable version identity because they change the replay declaration.
pub fn candidate_content_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    windows: &CandidateWindows,
    horizon_minutes: i64,
    abi: &str,
    unit_policy_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-content-v3\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{}\0{}\0{}\0{}\0{horizon_minutes}\0{abi}\0{unit_policy_sha256}",
            windows.training_from_ms,
            windows.training_to_ms,
            windows.selection_from_ms,
            windows.selection_to_ms,
        )
        .as_bytes(),
    )
}
/// Execution-aware content identity. The execution mode, rules, frozen tape and
/// actual sandbox runtime are part of the version so a ranking-only task, an
/// executable task over the same scorer inputs, and an executable task after a
/// Python/bwrap or containment-cap change never collide.
#[allow(clippy::too_many_arguments)]
pub fn candidate_execution_content_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    windows: &CandidateWindows,
    horizon_minutes: i64,
    abi: &str,
    unit_policy_sha256: &str,
    execution: &CandidateExecutionBinding,
    execution_runtime_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-content-v2\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{}\0{}\0{}\0{}\0{horizon_minutes}\0{abi}\0{unit_policy_sha256}\0{}\0{}\0{}\0{}\0{execution_runtime_sha256}",
            windows.training_from_ms,
            windows.training_to_ms,
            windows.selection_from_ms,
            windows.selection_to_ms,
            execution.abi,
            execution.spec_sha256,
            execution.manifest_sha256,
            execution.events_sha256,
        )
        .as_bytes(),
    )
}
/// Admission allowance identity over the frozen computation input. Variants
/// that only change prose, task key, parent, windows or horizon still spend the
/// same input allowance; the dataset hash already binds the frozen snapshots.
pub fn candidate_trial_input_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    abi: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-trial-input-v1\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{abi}"
        )
        .as_bytes(),
    )
}
/// Execution-aware admission allowance identity. Changing only the declared
/// execution rules, the frozen tape or the actual sandbox runtime is a new
/// computation input and therefore spends a new allowance; prose and parent
/// links still do not reset it.
pub fn candidate_execution_trial_input_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    abi: &str,
    execution: &CandidateExecutionBinding,
    execution_runtime_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-trial-input-v2\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{abi}\0{}\0{}\0{}\0{}\0{execution_runtime_sha256}",
            execution.abi,
            execution.rules_sha256.as_deref().unwrap_or(&execution.spec_sha256),
            execution.manifest_sha256,
            execution.events_sha256
        )
        .as_bytes(),
    )
}
/// Prospective-observation content identity. The original source/params, the
/// frozen feature snapshot, the frozen research-quote tape, the declared
/// observation rules, the recorded runtime/helper/caps and the observation ABI
/// are all part of the immutable version. Prose, task keys and parent metadata
/// are excluded so rewriting them cannot reset compute debt.
#[allow(clippy::too_many_arguments)]
pub fn candidate_observation_content_key(
    source_sha256: &str,
    params_sha256: &str,
    snapshot_sha256: &str,
    quotes_sha256: &str,
    spec_sha256: &str,
    window_from_ms: i64,
    window_to_ms: i64,
    holding_ms: i64,
    max_exit_delay_ms: i64,
    runtime_hash: &str,
    abi: &str,
    unit_policy_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-observation-content-v1\0{source_sha256}\0{params_sha256}\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}\0{window_from_ms}\0{window_to_ms}\0{holding_ms}\0{max_exit_delay_ms}\0{runtime_hash}\0{abi}\0{unit_policy_sha256}"
        )
        .as_bytes(),
    )
}
/// Prospective-observation admission allowance identity. A new snapshot, quote
/// tape, spec or runtime is a new computation input and spends a new allowance;
/// prose, task key and parent links do not.
#[allow(clippy::too_many_arguments)]
pub fn candidate_observation_trial_input_key(
    source_sha256: &str,
    params_sha256: &str,
    snapshot_sha256: &str,
    quotes_sha256: &str,
    spec_sha256: &str,
    runtime_hash: &str,
    abi: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-observation-trial-input-v1\0{source_sha256}\0{params_sha256}\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}\0{runtime_hash}\0{abi}"
        )
        .as_bytes(),
    )
}
/// Observation-aware job input hash. It binds the exact snapshot, quote tape,
/// spec and runtime the helper may read; the original params are already part
/// of the job script identity.
#[allow(clippy::too_many_arguments)]
pub fn candidate_observation_input_hash(
    params_sha256: &str,
    snapshot_sha256: &str,
    quotes_sha256: &str,
    spec_sha256: &str,
    runtime_hash: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-observation-input-v1\0{params_sha256}\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}\0{runtime_hash}"
        )
        .as_bytes(),
    )
}
/// Stable cohort identity used by cross-session continuation and measurement.
/// It deliberately excludes the per-session `book_binding` (which digests the
/// session date and directory) and any snapshot hash, so a new session with a
/// new future snapshot is still the same account/feed/instrument/runtime cohort.
#[allow(clippy::too_many_arguments)]
pub fn candidate_cohort_key(
    account: &str,
    instrument_universe_sha256: &str,
    feed: &str,
    dataset_abi: &str,
    lineage: &str,
    runtime_hash: &str,
    helper_sha256: &str,
    unit_policy_sha256: &str,
    scorer_compare_sha256: &str,
    sandbox_source_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-cohort-v1\0{account}\0{instrument_universe_sha256}\0{feed}\0{dataset_abi}\0{lineage}\0{runtime_hash}\0{helper_sha256}\0{unit_policy_sha256}\0{scorer_compare_sha256}\0{sandbox_source_sha256}"
        )
        .as_bytes(),
    )
}
/// Immutable candidate version identity. It is distinct from the queue task
/// identity even though both bind the same content and parent.
pub fn candidate_version_id(
    book_binding: &str,
    session: &str,
    task_key: &str,
    content_key: &str,
    parent_candidate_id: Option<&str>,
) -> String {
    sha256_hex(
        format!(
            "candidate-version-v1\0{book_binding}\0{session}\0{task_key}\0{content_key}\0{}",
            parent_candidate_id.unwrap_or("")
        )
        .as_bytes(),
    )
}
pub fn candidate_task_id(
    book_binding: &str,
    session: &str,
    task_key: &str,
    content_key: &str,
    parent_candidate_id: Option<&str>,
) -> String {
    sha256_hex(
        format!(
            "candidate-task-v1\0{book_binding}\0{session}\0{task_key}\0{content_key}\0{}",
            parent_candidate_id.unwrap_or("")
        )
        .as_bytes(),
    )
}
pub fn candidate_job_id(task_id: &str, revision: u32) -> String {
    sha256_hex(format!("candidate-job-v1\0{task_id}\0{revision}").as_bytes())
}
