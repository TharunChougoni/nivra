use super::*;

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
