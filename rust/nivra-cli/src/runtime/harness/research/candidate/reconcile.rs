use super::*;

pub(super) fn forbidden_pnl_key(value: &Value) -> bool {
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
    let snapshots: Vec<nivra_core::strategy::FeatureSnapshot> =
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
            || nivra_cli::candidate_execution::verify_report_against_manifest(
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
