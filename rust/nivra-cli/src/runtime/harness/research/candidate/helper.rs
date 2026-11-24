use super::*;
use nivra_core::strategy::FeatureSnapshot;
use nivra_sandbox::{BwrapLeaderReceipt, ScorerSandboxConfig};

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

fn verify_helper_launch(
    launch: &worker::CandidateLaunch,
    verify_self: bool,
) -> std::result::Result<(), &'static str> {
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
    Ok(())
}

struct StagedReplayInputs {
    source: Vec<u8>,
    params: Vec<u8>,
    dataset: Vec<u8>,
}

impl StagedReplayInputs {
    fn read(stage: &Path) -> std::result::Result<Self, &'static str> {
        Ok(Self {
            source: read_stage_file(&stage.join("source"), 262_144)?,
            params: read_stage_file(&stage.join("params"), 65_536)?,
            dataset: read_stage_file(&stage.join("dataset"), 4 * 1024 * 1024)?,
        })
    }

    fn matches(&self, launch: &worker::CandidateLaunch) -> bool {
        digest(&self.source) == launch.source_sha256
            && digest(&self.params) == launch.params_sha256
            && digest(&self.dataset) == launch.dataset_hash
    }

    fn snapshots(&self) -> std::result::Result<Vec<FeatureSnapshot>, &'static str> {
        let snapshots: Vec<FeatureSnapshot> = serde_json::from_slice(&self.dataset)
            .map_err(|_| "candidate_helper_dataset_invalid")?;
        if snapshots.is_empty() {
            return Err("candidate_helper_dataset_invalid");
        }
        Ok(snapshots)
    }

    fn replay(
        &self,
        settings: &ScorerReplaySettings,
        sandbox: &ScorerSandboxConfig,
        snapshots: &[FeatureSnapshot],
    ) -> std::result::Result<(ScorerReplayReport, BwrapLeaderReceipt), &'static str> {
        scorer_compare::replay_candidate(settings, sandbox, &self.source, &self.params, snapshots)
            .map_err(|_| "candidate_helper_replay_failed")
    }
}

fn verified_replay(
    launch: &worker::CandidateLaunch,
) -> std::result::Result<(ScorerReplaySettings, ScorerSandboxConfig), &'static str> {
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
    let sandbox = ScorerSandboxConfig {
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
    Ok((replay, sandbox))
}

fn helper_output(
    launch: &worker::CandidateLaunch,
    result_produced_at_ms: i64,
    report: ScorerReplayReport,
    advisory: BwrapLeaderReceipt,
) -> CandidateHelperOutput {
    CandidateHelperOutput {
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
        result_produced_at_ms,
        report,
        advisory,
        execution: None,
        observation: None,
    }
}

/// The helper has no access to the queue, config, credentials, or paper book.
pub(crate) fn run_helper_at(
    launch: &worker::CandidateLaunch,
    clock: impl Fn() -> i64,
    verify_self: bool,
) -> std::result::Result<CandidateHelperOutput, &'static str> {
    verify_helper_launch(launch, verify_self)?;
    let stage = Path::new(&launch.stage_dir);
    let inputs = StagedReplayInputs::read(stage)?;
    if !inputs.matches(launch) {
        return Err("candidate_helper_input_changed");
    }
    let (replay, sandbox) = verified_replay(launch)?;
    let snapshots = inputs.snapshots()?;
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
            let spec: nivra_cli::candidate_execution::CandidateExecutionSpecV1 =
                serde_json::from_slice(&spec_bytes)
                    .map_err(|_| "candidate_helper_execution_spec_invalid")?;
            nivra_cli::candidate_execution::validate_spec(&spec)
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
    let (report, advisory) = inputs.replay(&replay, &sandbox, &snapshots)?;
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
            let signals = nivra_cli::candidate_execution::signals_from_ranking(&report, &spec)
                .map_err(|_| "candidate_helper_execution_signals_invalid")?;
            let execution =
                nivra_cli::candidate_execution::execute(&signals, &spec, &manifest, &events)
                    .map_err(|_| "candidate_helper_execution_failed")?;
            nivra_cli::candidate_execution::verify_report_against_manifest(
                &execution, &manifest, &events,
            )
            .map_err(|_| "candidate_helper_execution_invalid")?;
            Some(execution)
        }
        None => None,
    };
    Ok(CandidateHelperOutput {
        execution,
        ..helper_output(launch, clock(), report, advisory)
    })
}

/// The observation path has only an entry tape. It cannot read exit quotes.
pub(crate) fn run_observation_at(
    launch: &worker::CandidateLaunch,
    clock: impl Fn() -> i64,
    verify_self: bool,
) -> std::result::Result<CandidateHelperOutput, &'static str> {
    verify_helper_launch(launch, verify_self)?;
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
    let inputs = StagedReplayInputs::read(stage)?;
    let spec_bytes = read_stage_file(&stage.join("observation_spec"), 65_536)?;
    let bundle_bytes = read_stage_file(&stage.join("observation_quotes"), 2_000_000)?;
    if !inputs.matches(launch)
        || digest(&spec_bytes) != target.spec_sha256
        || digest(&bundle_bytes) != target.quotes_sha256
    {
        return Err("candidate_helper_input_changed");
    }
    let spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes)
            .map_err(|_| "candidate_helper_observation_spec_invalid")?;
    nivra_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_helper_observation_spec_invalid")?;
    let snapshots = inputs.snapshots()?;
    if snapshots
        .iter()
        .any(|snapshot| snapshot.observed_ms > target.signal_ms)
        || snapshots
            .last()
            .is_some_and(|snapshot| snapshot.observed_ms < target.window_from_ms)
    {
        return Err("candidate_helper_dataset_invalid");
    }
    let (replay, sandbox) = verified_replay(launch)?;
    let (report, advisory) = inputs.replay(&replay, &sandbox, &snapshots)?;
    // Real completion clock, sampled after the replay returned. Never the last
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
    let mut action: Option<nivra_cli::candidate_execution::ObservationActionV1> = None;
    if let Some(tick) = ranked {
        let signal = nivra_cli::candidate_execution::ScoredSignal {
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
                let selected = nivra_cli::candidate_execution::observation_action_with_provenance(
                    &signal,
                    &spec,
                    manifest,
                    events,
                    &original_archive_sha256,
                    bundle["universe_sha256"].as_str(),
                )
                .map_err(|_| "candidate_helper_observation_action_invalid")?;
                let (mapped_status, mapped_gap) =
                    nivra_cli::candidate_execution::observation_status_for_action(&selected);
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
        (Some(entry), Some(quote)) => Some(nivra_cli::candidate_execution::ObservationEntryV1 {
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
    let mut score = nivra_cli::candidate_execution::ObservationScoreV1 {
        version: 1,
        abi: nivra_cli::candidate_execution::OBSERVATION_ABI.into(),
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
        observation_scope: nivra_cli::candidate_execution::OBSERVATION_SCOPE.into(),
        policy_sha256: target.policy_sha256.clone(),
        gaps,
    };
    nivra_cli::candidate_execution::validate_observation_score(&score)
        .map_err(|_| "candidate_helper_observation_score_invalid")?;
    score.observation_sha256 = nivra_cli::candidate_execution::observation_artifact_sha256(&score)
        .map_err(|_| "candidate_helper_observation_hash_invalid")?;
    Ok(CandidateHelperOutput {
        observation: Some(score),
        ..helper_output(launch, score_produced_at_ms, report, advisory)
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

pub(super) fn record_stopped_termination(
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

pub(super) fn finish_unstarted(
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
                || nivra_cli::candidate_execution::verify_report(execution).is_err()
            {
                return false;
            }
            let Ok(spec) = serde_json::from_value::<
                nivra_cli::candidate_execution::CandidateExecutionSpecV1,
            >(execution.execution_spec.clone()) else {
                return false;
            };
            let Ok(signals) =
                nivra_cli::candidate_execution::signals_from_ranking(&output.report, &spec)
            else {
                return false;
            };
            signals == execution.scored_signals
        }
        _ => false,
    }
}

pub(super) fn verify_helper_output(
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
            if nivra_cli::candidate_execution::validate_observation_score(score).is_err() {
                return false;
            }
            if score.version != 1
                || score.abi != nivra_cli::candidate_execution::OBSERVATION_ABI
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
                || score.observation_scope != nivra_cli::candidate_execution::OBSERVATION_SCOPE
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
                    != nivra_cli::candidate_execution::observation_artifact_sha256(
                        &nivra_cli::candidate_execution::ObservationScoreV1 {
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
                        && quote.source_age_ms <= nivra_core::market::MAX_FINANCIAL_AGE_MS
                        && quote.receipt_age_ms <= nivra_core::market::MAX_FINANCIAL_AGE_MS
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
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
    report: &ScorerReplayReport,
) -> std::result::Result<(), &'static str> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.purpose else {
        return Err("candidate_result_invalid");
    };
    nivra_cli::candidate_execution::validate_observation_score(score)
        .map_err(|_| "candidate_result_invalid")?;
    if score.version != 1
        || score.abi != nivra_cli::candidate_execution::OBSERVATION_ABI
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
        || score.observation_scope != nivra_cli::candidate_execution::OBSERVATION_SCOPE
        || score.policy_sha256 != target.policy_sha256
        || score.score_sha256 != report.report_hash
        || score.score != report.mean_top_n_overlap
        || !score.score.is_none_or(f64::is_finite)
        || score.observation_sha256
            != nivra_cli::candidate_execution::observation_artifact_sha256(
                &nivra_cli::candidate_execution::ObservationScoreV1 {
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
                || quote.source_age_ms > nivra_core::market::MAX_FINANCIAL_AGE_MS
                || quote.receipt_age_ms > nivra_core::market::MAX_FINANCIAL_AGE_MS
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
        nivra_cli::candidate_execution::RESULT_KIND_OBSERVATION
    } else if execution.is_some() {
        nivra_cli::candidate_execution::RESULT_KIND_EXECUTED
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
    let evidence_scope = candidate_evidence_scope(&task.spec, is_observation);
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

pub(super) fn verify_helper_failure(
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

pub(super) fn store_result_artifact(
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
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
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
    let spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes).map_err(|_| "candidate_result_unverified")?;
    nivra_cli::candidate_execution::validate_observation_spec(&spec)
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
    let signal = nivra_cli::candidate_execution::ScoredSignal {
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
    let action = nivra_cli::candidate_execution::observation_action_with_provenance(
        &signal,
        &spec,
        manifest,
        events,
        &original_archive_sha256,
        bundle["universe_sha256"].as_str(),
    )
    .map_err(|_| "candidate_result_unverified")?;
    nivra_cli::candidate_execution::validate_observation_action_binding(score, &action)
        .map_err(|_| "candidate_result_unverified")?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn verify_candidate_result_envelope(
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
        nivra_cli::candidate_execution::RESULT_KIND_OBSERVATION
    } else if task.execution.is_some() {
        nivra_cli::candidate_execution::RESULT_KIND_EXECUTED
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
            && nivra_cli::candidate_execution::verify_report(execution).is_ok()
            && serde_json::from_value::<nivra_cli::candidate_execution::CandidateExecutionSpecV1>(
                execution.execution_spec.clone(),
            )
            .is_ok_and(|spec| {
                spec.parameters.training_from_ms == task.windows.training_from_ms
                    && spec.parameters.training_to_ms == task.windows.training_to_ms
                    && spec.parameters.selection_from_ms == task.windows.selection_from_ms
                    && spec.parameters.selection_to_ms == task.windows.selection_to_ms
                    && spec.parameters.holding_ms == task.horizon_minutes.saturating_mul(60_000)
                    && spec.parameters.max_exit_delay_ms == task.max_exit_delay_ms
                    && nivra_cli::candidate_execution::signals_from_ranking(&result.report, &spec)
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
            || nivra_cli::candidate_execution::verify_report_against_manifest(
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
        || result.evidence_scope != candidate_evidence_scope(task, is_observation)
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
