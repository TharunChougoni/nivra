use super::*;

fn archived_feature_snapshots(
    path: &Path,
    sha256: &str,
    session: &str,
    replay: &ScorerReplaySettings,
    now: i64,
) -> std::result::Result<Vec<nivra_core::strategy::FeatureSnapshot>, String> {
    let bytes = crate::read_bounded(path, 4 * 1024 * 1024)
        .map_err(|_| "candidate_archived_dataset_unreadable")?;
    if digest(&bytes) != sha256 {
        return Err("candidate_archived_dataset_hash_mismatch".into());
    }
    let snapshots: Vec<nivra_core::strategy::FeatureSnapshot> =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_archived_dataset_invalid")?;
    if snapshots.is_empty()
        || snapshots.len() > 10_000
        || snapshots
            .windows(2)
            .any(|w| w[0].observed_ms >= w[1].observed_ms)
        || snapshots.iter().any(|snapshot| {
            snapshot.observed_ms < 0
                || snapshot.observed_ms > now
                || !archival_candidate_window(session, snapshot.observed_ms)
                || snapshot.lookback_samples != replay.lookback_samples
        })
    {
        return Err("candidate_archived_dataset_invalid".into());
    }
    Ok(snapshots)
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
    if super::super::super::super::transcript::redact(&json!(args.hypothesis)).as_str()
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
    if !candidate.datasets.iter().any(|dataset| {
        dataset.dataset_id == args.dataset_id
            && dataset.kind == CandidateDatasetKind::CurrentSessionFeatures
    }) {
        return Ok(not_dispatched("candidate_dataset_unknown"));
    }
    let binding = book_binding(c)?;
    let session = c.session.to_string();
    let policy_sha256 = digest(&super::super::super::super::planner::policy_bytes(cfg)?);
    let mut db = open_queue(settings, &c.directory.join("model"), true)?;
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
    let sandbox = match candidate_sandbox_policy(&candidate.sandbox) {
        Ok(sandbox) => sandbox,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let helper_sha256 = match helper_content_sha256(candidate) {
        Ok(hash) => hash,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let unit_policy = match candidate_unit_policy(candidate, &candidate.sandbox, &helper_sha256) {
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
    let snapshots = match super::super::super::super::paper_replay::session_feature_snapshots(
        c.quote_source(),
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
    let tape = match super::super::super::super::paper_replay::virtual_observation_tape_bounded(
        c.quote_source(),
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
        super::super::super::super::paper_replay::TapeMode::EntryFreeze,
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
            nivra_cli::candidate_execution::CandidateExecutionSpecV1,
        >(&bytes)
        && (spec.max_entries > 1 || spec.selection_top_n > 1)
    {
        projection = "top_n_1_single_entry".into();
    }
    let parameters = nivra_cli::strategy_actions::Parameters {
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
    let spec = nivra_cli::candidate_execution::ObservationExecutionSpecV1 {
        version: 1,
        abi: nivra_cli::candidate_execution::OBSERVATION_ABI.into(),
        parameters,
        units: obs.units,
        max_entries: 1,
        selection_top_n: 1,
        entry_price_offset_ticks: 0,
        entry_at: nivra_cli::candidate_execution::EntryAt::Signal,
        exit: nivra_cli::candidate_execution::ExitRule::Time,
        first_signal_per_key: true,
        original_strategy_measurement: false,
        observation_projection: projection,
    };
    if let Err(reason) = nivra_cli::candidate_execution::validate_observation_spec(&spec) {
        return Ok(not_dispatched(&reason));
    }
    let spec_bytes = nivra_cli::candidate_execution::artifact_bytes(&spec)?;
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
            replay_abi: nivra_cli::candidate_execution::OBSERVATION_ABI.into(),
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
        nivra_cli::candidate_execution::OBSERVATION_ABI,
        &unit_policy.policy_sha256(),
    );
    let input_key = jobs::candidate_observation_trial_input_key(
        &original.spec.source_sha256,
        &original.spec.params_sha256,
        &snapshot_sha256,
        &quotes_sha256,
        &spec_sha256,
        &runtime_binding.runtime_hash,
        nivra_cli::candidate_execution::OBSERVATION_ABI,
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
        feature_abi: nivra_cli::candidate_execution::OBSERVATION_ABI.into(),
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

#[cfg(test)]
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
    candidate_submit_preemptible(
        c,
        cfg,
        model_id,
        model_revision,
        parent_turn_id,
        args,
        now,
        owner_evidence,
        &|| false,
    )
}

/// [`candidate_submit`] for a lane turn: `preempted` stops a strategy replay
/// when a higher-priority turn is waiting.
#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_submit_preemptible(
    c: &Config,
    cfg: &PlannerConfig,
    model_id: &str,
    model_revision: u64,
    parent_turn_id: &str,
    args: CandidateSubmitArgs,
    now: i64,
    owner_evidence: Option<jobs::OwnerEvidence>,
    preempted: &dyn Fn() -> bool,
) -> Result<Value> {
    if args.observation.is_some() {
        if args.archive_bars.is_some() {
            return Ok(not_dispatched("candidate_strategy_args_conflict"));
        }
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
    if super::super::super::super::transcript::redact(&json!(args.hypothesis)).as_str()
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
    if super::super::super::super::transcript::redact(&json!(args.source)).as_str()
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
    if super::super::super::super::transcript::redact(&args.params) != args.params {
        return Ok(not_dispatched("candidate_params_secret_refused"));
    }
    let params_bytes = serde_json::to_vec(&args.params)?;
    if params_bytes.len() > candidate.max_params_bytes as usize {
        return Ok(not_dispatched("candidate_params_invalid"));
    }
    let dataset = candidate
        .datasets
        .iter()
        .find(|dataset| dataset.dataset_id == args.dataset_id);
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
    let bar_window = match (args.archive_bars.as_deref(), dataset.map(|d| &d.kind)) {
        (Some(window), _) if args.dataset_id == ARCHIVE_BARS_DATASET_ID => Some(window),
        (Some(_), _) => return Ok(not_dispatched("candidate_archive_bars_dataset_conflict")),
        (None, Some(CandidateDatasetKind::ArchiveBars(window))) => Some(window),
        _ => None,
    };
    if let Some(window) = bar_window {
        return archive_bars::enqueue(
            c,
            cfg,
            settings,
            candidate,
            model_id,
            model_revision,
            parent_turn_id,
            &args,
            windows,
            window.clone(),
            now,
            owner_evidence,
        );
    }
    let Some(dataset) = dataset else {
        return Ok(not_dispatched("candidate_dataset_unknown"));
    };
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
    if args.strategy == Some(true) {
        return super::strategy_replay::strategy_backtest(
            c,
            cfg,
            candidate,
            crate::runtime::strategy_library::Author {
                lane: "research".into(),
                model: Some(model_id.to_owned()),
                model_revision: Some(model_revision),
                turn_id: Some(parent_turn_id.to_owned()),
                session: Some(c.session),
            },
            &args,
            now,
            preempted,
        );
    }
    if args.archive_daily_window.is_some() || args.as_of_receipt_id.is_some() {
        return Ok(not_dispatched("candidate_archive_strategy_required"));
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
        if let Err(reason) = nivra_cli::candidate_execution::validate_spec(spec) {
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
    let archived_window =
        archival_candidate_window(&c.session.to_string(), windows.selection_to_ms);
    let snapshots = match &dataset.kind {
        CandidateDatasetKind::CurrentSessionFeatures => {
            super::super::super::super::paper_replay::session_feature_snapshots(
                c.quote_source(),
                c.session,
                previous_session,
                candidate.replay.max_snapshots,
                candidate.replay.max_age_ms,
                candidate.replay.lookback_samples,
                now,
            )
            .map_err(|_| "candidate_dataset_unavailable".to_owned())
        }
        CandidateDatasetKind::ArchivedFeatureSnapshots { path, sha256 } if archived_window => {
            if args.execution.is_some() {
                return Ok(not_dispatched("candidate_archived_execution_unavailable"));
            }
            archived_feature_snapshots(path, sha256, &c.session.to_string(), &candidate.replay, now)
        }
        _ => return Ok(not_dispatched("candidate_dataset_window_mismatch")),
    };
    let snapshots = match snapshots {
        Ok(snapshots) => snapshots,
        Err(reason) => return Ok(not_dispatched(&reason)),
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
    if selection.len() > candidate.replay.max_snapshots {
        return Ok(not_dispatched("candidate_dataset_too_many_snapshots"));
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
            match super::super::super::super::paper_replay::execution_tape_bounded(
                c.quote_source(),
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
    let policy_sha256 = digest(&super::super::super::super::planner::policy_bytes(cfg)?);
    let session = c.session.to_string();
    let source_sha256 = digest(args.source.as_bytes());
    let params_sha256 = digest(&params_bytes);
    let dataset_hash = digest(&dataset_bytes);
    let sandbox = match candidate_sandbox_policy(&candidate.sandbox) {
        Ok(sandbox) => sandbox,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let helper_sha256 = match helper_content_sha256(candidate) {
        Ok(hash) => hash,
        Err(_) => return Ok(not_dispatched("candidate_runtime_unavailable")),
    };
    let unit_policy = match candidate_unit_policy(candidate, &candidate.sandbox, &helper_sha256) {
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
            let spec_bytes = nivra_cli::candidate_execution::artifact_bytes(spec)?;
            Some((
                jobs::CandidateExecutionBinding {
                    version: 1,
                    abi: nivra_cli::candidate_execution::EXECUTION_ABI.into(),
                    spec_sha256: digest(&spec_bytes),
                    manifest_sha256: tape.manifest_sha256.clone(),
                    events_sha256: tape.events_sha256.clone(),
                    rules_sha256: Some(nivra_cli::candidate_execution::execution_rules_sha256(
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
    let mut db = open_queue(settings, &c.directory.join("model"), true)?;
    let latest = jobs::latest_candidate_for_key(&mut db, &session, &binding, &args.task_key)
        .map_err(|_| "candidate_task_state_unavailable")?;
    let resolved_parent_ref = match args.parent_ref.as_deref() {
        Some(reference) => {
            if latest.is_some() {
                return Ok(not_dispatched("candidate_parent_ref_requires_new_chain"));
            }
            match super::super::carry::resolve_parent_ref(c, cfg, reference) {
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
            if let Err(reason) = super::super::carry::verify_source_lifecycle(c, reference) {
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

pub(super) fn candidate_load_error(db: &Connection, error: jobs::Error) -> &'static str {
    if error == jobs::Error::Invalid && jobs::candidate_requires_migration(db).unwrap_or(false) {
        "candidate_legacy_schema_migration_required"
    } else {
        "candidate_task_unavailable"
    }
}

pub(super) fn candidate_task(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<(Connection, jobs::CandidateTaskRecord)> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    let mut db = open_queue(settings, &c.directory.join("model"), false)?;
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
    if let Some((_, record)) = archive_bars::load(c, cfg, task_id, revision)? {
        return Ok(archive_bars::status(&record));
    }
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
    match super::super::carry::host_parent_ref(c, cfg, &mut db, &record) {
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
