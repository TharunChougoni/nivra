use super::*;

fn find_source_task(
    queue: &mut Connection,
    session: &str,
    lifecycle_id: &str,
) -> Result<jobs::CandidateTaskRecord> {
    let mut cursor = None;
    let mut scanned = 0usize;
    loop {
        let page = jobs::list_candidates_page(queue, session, None, cursor.as_ref(), 100)
            .map_err(|_| "continuation_original_queue_unavailable")?;
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
        if let Some(found) = page
            .into_iter()
            .find(|row| row.spec.lifecycle_candidate_id == lifecycle_id)
        {
            return Ok(found);
        }
        if !full {
            break;
        }
        // The bounded source scan is explicit: a missing row beyond the bound
        // is `truncated`, never a false `missing` authority claim. An exact
        // 10k page is not truncation unless an actual extra row exists.
        if scanned >= 10_000 {
            let extra = jobs::list_candidates_page(queue, session, None, cursor.as_ref(), 1)
                .map_err(|_| "continuation_original_queue_unavailable")?;
            if extra.is_empty() {
                break;
            }
            return Err("continuation_source_truncated".into());
        }
    }
    Err("continuation_source_task_missing".into())
}

pub(super) fn verify_entry_quote(
    directory: &Path,
    entry: &sadr_cli::candidate_execution::ObservationEntryV1,
    source_manifest_sha256: &str,
) -> Result<()> {
    let quote = &entry.quote;
    if !entry.virtual_observation
        || quote.purpose != "research_observation"
        || quote.evidence_status != "ok"
        || quote.source_ms <= 0
        || quote.receipt_ms <= 0
        || quote.source_ms > quote.receipt_ms
        || quote.receipt_ms > quote.available_at_ms
        || quote.available_at_ms > quote.event_at_ms
        || quote.event_at_ms > entry.decision_at_ms
        || quote.source_age_ms != entry.decision_at_ms.saturating_sub(quote.source_ms)
        || quote.receipt_age_ms != entry.decision_at_ms.saturating_sub(quote.receipt_ms)
        || !(0..=2_000).contains(&quote.source_age_ms)
        || !(0..=2_000).contains(&quote.receipt_age_ms)
        || quote.universe_sha256.as_deref() != Some(source_manifest_sha256)
    {
        return Err("continuation_entry_unverified".into());
    }
    let instrument = source_instrument(directory, &entry.key)?;
    if quote
        .tick
        .as_deref()
        .is_some_and(|tick| tick != instrument.tick.amount().to_string())
    {
        return Err("continuation_entry_unverified".into());
    }
    // The entry receipt must be the frozen entry tape's own archive, not a later
    // receipt or a global multi-key tape hash.
    Ok(())
}

pub(super) fn verify_entry_archive(
    queue: &Connection,
    target: &jobs::ObservationTargetV1,
    entry: &sadr_cli::candidate_execution::ObservationEntryV1,
) -> Result<()> {
    let bundle_bytes = jobs::read_candidate_artifact(
        queue,
        &target.quotes_sha256,
        "observation_quotes",
        2_000_000,
    )
    .map_err(|_| "continuation_entry_unverified")?;
    if digest(&bundle_bytes) != target.quotes_sha256 {
        return Err("continuation_entry_unverified".into());
    }
    let bundle: Value =
        serde_json::from_slice(&bundle_bytes).map_err(|_| "continuation_entry_unverified")?;
    if bundle["status"] != "ok" {
        return Err("continuation_entry_unverified".into());
    }
    let manifest = bundle["manifest"]
        .as_str()
        .ok_or("continuation_entry_unverified")?;
    let events = bundle["events"]
        .as_str()
        .ok_or("continuation_entry_unverified")?;
    if bundle["manifest_sha256"] != digest(manifest.as_bytes())
        || bundle["events_sha256"] != digest(events.as_bytes())
    {
        return Err("continuation_entry_unverified".into());
    }
    let manifest: Value =
        serde_json::from_str(manifest).map_err(|_| "continuation_entry_unverified")?;
    // A virtual multi-key tape must use the per-key ORIGINAL receipt SHA. A
    // whole-tape/global SHA is never a receipt identity. The only legacy
    // fallback permitted is a provably SINGLE-instrument, SINGLE-receipt tape
    // whose global archive SHA is the one original receipt.
    let per_key = manifest["provenance"]["per_key"][entry.key.as_str()]["archive_sha256"].as_str();
    let legacy_single = manifest["provenance"]["per_key"]
        .as_object()
        .is_none_or(|map| map.is_empty())
        && manifest["provenance"]["archive_sha256"].as_str().is_some()
        && serde_json::from_str::<Value>(events.trim()).is_ok_and(|event| {
            event["observations"]
                .as_array()
                .is_some_and(|observations| {
                    observations.len() == 1
                        && observations[0]["key"].as_str() == Some(entry.key.as_str())
                })
        });
    let recorded = per_key.or_else(|| {
        legacy_single
            .then(|| manifest["provenance"]["archive_sha256"].as_str())
            .flatten()
    });
    if recorded != Some(entry.quote.archive_sha256.as_str()) {
        return Err("continuation_entry_unverified".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_source_episode(
    c: &Config,
    cfg: &PlannerConfig,
    session: &str,
    directory: &Path,
    carry: &Value,
    item: &Value,
    experiment: &experiment_ledger::lifecycle::ExperimentSpec,
    clock: &EpisodeClock,
) -> Result<SourceEpisode> {
    let lifecycle_id = item["candidate_id"]
        .as_str()
        .ok_or("continuation_source_lifecycle_missing")?
        .to_owned();
    let score_value = item["observation_score"].clone();
    let score: sadr_cli::candidate_execution::ObservationScoreV1 =
        serde_json::from_value(score_value.clone())
            .map_err(|_| "continuation_source_score_invalid")?;
    sadr_cli::candidate_execution::validate_observation_score(&score)
        .map_err(|_| "continuation_source_score_invalid")?;
    if score.entry.as_ref() != Some(&clock.entry) {
        return Err("continuation_source_entry_changed".into());
    }
    let previous_session: chrono::NaiveDate = carry["source_previous_session"]
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .or_else(|| session.parse::<chrono::NaiveDate>().ok()?.pred_opt())
        .ok_or("continuation_source_session_scope")?;
    let mut queue = open_original_queue_read_only_strict(c, cfg, session)?;
    let task = find_source_task(&mut queue, session, &lifecycle_id)?;
    if task.status != jobs::TaskState::Succeeded
        || task.spec.session != session
        || task.spec.lifecycle_candidate_id != lifecycle_id
    {
        return Err("continuation_source_ineligible".into());
    }
    let target = match &task.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.as_ref().clone(),
        _ => return Err("continuation_source_ineligible".into()),
    };
    if !target.validate() {
        return Err("continuation_source_target_invalid".into());
    }
    // The task row must be the SAME immutable source task that authored this
    // observation target, bound to the VERIFIED carry's source book. A shared
    // queue row with the same session/lifecycle id but a different book,
    // policy, parent or source/params identity is refused before the source
    // body or score artifacts are trusted.
    if carry["source_binding"].as_str() != Some(task.spec.book_binding.as_str()) {
        return Err("continuation_source_book_changed".into());
    }
    if task.spec.policy_sha256 != target.policy_sha256 {
        return Err("continuation_source_policy_changed".into());
    }
    if task.spec.parent_candidate_id.as_deref() != Some(target.source_candidate_id.as_str())
        || task.spec.parent_lifecycle_candidate_id.as_deref()
            != Some(target.source_lifecycle_candidate_id.as_str())
    {
        return Err("continuation_source_parent_changed".into());
    }
    if task.spec.source_sha256 != target.runtime.source_sha256
        || task.spec.params_sha256 != target.runtime.params_sha256
        || task.spec.dataset_hash != target.snapshot_sha256
        || task.spec.windows.selection_from_ms != target.window_from_ms
        || target.window_to_ms > task.spec.windows.selection_to_ms
        || task.spec.max_exit_delay_ms != target.max_exit_delay_ms
        || task.spec.abi != "scorer_replay_v1"
    {
        return Err("continuation_source_task_unbound".into());
    }
    let result =
        read_verified_candidate_result(&mut queue, &task.spec.task_id, task.spec.revision)?
            .ok_or("continuation_source_result_unverified")?;
    let Some(recorded_score) = &result.observation else {
        return Err("continuation_source_result_unverified".into());
    };
    if serde_json::to_value(recorded_score).ok() != Some(score_value.clone()) {
        return Err("continuation_ledger_score_mismatch".into());
    }
    let spec_bytes =
        jobs::read_candidate_artifact(&queue, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "continuation_source_spec_missing")?;
    if digest(&spec_bytes) != target.spec_sha256 {
        return Err("continuation_source_spec_changed".into());
    }
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes).map_err(|_| "continuation_source_spec_invalid")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "continuation_source_spec_invalid")?;
    // The frozen study spec is immutable. The CURRENT model/policy cannot
    // override the original holding, grace, costs, capital or purpose.
    if spec.parameters.holding_ms != target.holding_ms
        || spec.parameters.max_exit_delay_ms != target.max_exit_delay_ms
        || spec.parameters.selection_from_ms != target.window_from_ms
        || spec.parameters.selection_to_ms != experiment.selection_to_ms
        || spec.parameters.training_from_ms != experiment.training_from_ms
        || spec.parameters.training_to_ms != experiment.training_to_ms
        || spec.parameters.capital <= sadr_core::finance::Amount::ZERO
    {
        return Err("continuation_source_spec_changed".into());
    }
    if experiment.session != session
        || experiment.feature_abi != sadr_cli::candidate_execution::OBSERVATION_ABI
        || experiment.source_sha256 != task.spec.source_sha256
        || experiment.params_sha256 != task.spec.params_sha256
        || experiment.parent_candidate_id.as_deref()
            != Some(target.source_lifecycle_candidate_id.as_str())
        || experiment.holding_ms != target.holding_ms
        || experiment.max_exit_delay_ms != target.max_exit_delay_ms
        || experiment.selection_from_ms != target.window_from_ms
        || experiment.training_from_ms != task.spec.windows.training_from_ms
        || experiment.training_to_ms != task.spec.windows.training_to_ms
    {
        return Err("continuation_source_lifecycle_changed".into());
    }
    let lifecycle_dataset_hash = digest(
        format!(
            "candidate_observation_dataset_v1\0{}\0{}\0{}",
            target.snapshot_sha256, target.quotes_sha256, target.spec_sha256
        )
        .as_bytes(),
    );
    if experiment.dataset_hash != lifecycle_dataset_hash {
        return Err("continuation_source_lifecycle_changed".into());
    }
    if score.source_task_id != target.source_task_id
        || score.source_revision != target.source_revision
        || score.source_candidate_id != target.source_candidate_id
        || score.source_result_sha256 != target.source_result_sha256
        || score.authoring_at_ms != target.authoring_at_ms
        || score.target_frozen_at_ms != target.target_frozen_at_ms
        || score.key.as_deref() != Some(clock.entry.key.as_str())
    {
        return Err("continuation_source_score_changed".into());
    }
    let runtime = &task.spec.runtime_binding;
    if target.runtime.runtime_hash != runtime.runtime_hash
        || target.runtime.helper_sha256 != runtime.helper_sha256
        || target.runtime.unit_policy_sha256 != runtime.unit_policy_sha256
        || target.runtime.scorer_compare_sha256 != runtime.scorer_compare_sha256
        || target.runtime.sandbox_source_sha256 != runtime.sandbox_source_sha256
        || result.runtime_binding != *runtime
    {
        return Err("continuation_runtime_unverified".into());
    }
    let source_manifest_sha256 = source_universe_manifest_sha256(directory)?;
    verify_entry_quote(directory, &clock.entry, &source_manifest_sha256)?;
    verify_entry_archive(&queue, &target, &clock.entry)?;
    let source_universe_sha256 = instrument_universe_sha256_at(directory)?;
    let source_cohort_key = jobs::candidate_cohort_key(
        &c.account,
        &source_universe_sha256,
        "wide_quotes",
        "scorer_replay_v1",
        "owner_candidate_v1",
        &runtime.runtime_hash,
        &runtime.helper_sha256,
        &runtime.unit_policy_sha256,
        &runtime.scorer_compare_sha256,
        &runtime.sandbox_source_sha256,
    );
    let source_state_hash = source_state_hash(directory)?;
    let carry_state_hash = carry["source_state_hash"]
        .as_str()
        .ok_or("continuation_source_state_unavailable")?;
    if source_state_hash != carry_state_hash {
        return Err("continuation_source_state_changed".into());
    }
    // Flush the original artifacts before the current action/measurement. This
    // is a read-only fsync, not a predecessor write.
    for path in [
        directory.join("paper.sqlite"),
        directory.join("model").join("experiment-trials.sqlite"),
        directory.join("wide-universe.json"),
    ] {
        fsync_source(&path)?;
    }
    // CURRENT runtime/cohort changes are NOT source-integrity failures. They are
    // checked only AFTER the original episode's authority is fully verified, so
    // an explicit unknown can be recorded without trusting an unverified
    // source. They are never authority refusals.
    let current_runtime = current_candidate_runtime(cfg)?;
    let current_runtime_changed = current_runtime.runtime_hash != runtime.runtime_hash
        || current_runtime.helper_sha256 != runtime.helper_sha256
        || current_runtime.unit_policy_sha256 != runtime.unit_policy_sha256
        || current_runtime.scorer_compare_sha256 != runtime.scorer_compare_sha256
        || current_runtime.sandbox_source_sha256 != runtime.sandbox_source_sha256;
    let current_cohort = current_cohort_key(c, cfg)?;
    let current_cohort_changed = current_cohort != source_cohort_key;
    Ok(SourceEpisode {
        session: session.to_owned(),
        directory: directory.to_path_buf(),
        previous_session,
        lifecycle_id,
        task,
        score,
        entry: clock.entry.clone(),
        spec,
        target,
        experiment: experiment.clone(),
        source_state_hash,
        source_universe_sha256,
        source_cohort_key,
        current_runtime_hash: current_runtime.runtime_hash,
        current_cohort_key: current_cohort,
        current_runtime_changed,
        current_cohort_changed,
        due: clock.due,
        deadline: clock.deadline,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_continuation(
    c: &Config,
    cfg: &PlannerConfig,
    episode: &SourceEpisode,
    outcome: ContinuationOutcome,
    now: i64,
) -> Result<Value> {
    let current_runtime = current_candidate_runtime(cfg)?;
    let current_cohort = current_cohort_key(c, cfg)?;
    let scope = ContinuationScopeV1 {
        version: CONTINUATION_VERSION,
        current_session: c.session.to_string(),
        account: c.account.clone(),
        cohort_key: current_cohort,
        runtime_hash: current_runtime.runtime_hash.clone(),
        source_ledger_spec_sha256: digest(&serde_json::to_vec(&episode.experiment)?),
    };
    let scope_sha256 = digest(&serde_json::to_vec(&scope)?);
    let score_value = serde_json::to_value(&episode.score)?;
    let entry_value = serde_json::to_value(&episode.entry)?;
    let source_score_sha256 = digest(&serde_json::to_vec(&score_value)?);
    let source_entry_sha256 = digest(&serde_json::to_vec(&entry_value)?);
    let identity = ContinuationIdentityV1 {
        version: CONTINUATION_VERSION,
        source_session: episode.session.clone(),
        source_lifecycle_candidate_id: episode.lifecycle_id.clone(),
        source_task_id: episode.task.spec.task_id.clone(),
        source_revision: episode.task.spec.revision,
        source_score_sha256: source_score_sha256.clone(),
        source_entry_sha256: source_entry_sha256.clone(),
        scope_sha256,
    };
    let continuation_id = digest(&serde_json::to_vec(&identity)?);
    let runtime = &episode.task.spec.runtime_binding;
    let mut body = json!({
        "version":CONTINUATION_VERSION,
        "financial_abi":super::candidate::OBSERVATION_FINANCIAL_ABI,
        "continuation_id":continuation_id,
        "state":match outcome { ContinuationOutcome::Evaluated(_) => "evaluated", ContinuationOutcome::Unknown{..} => "unknown" },
        "source_session":episode.session,
        "source_task_id":episode.task.spec.task_id,
        "source_revision":episode.task.spec.revision,
        "source_candidate_id":episode.task.spec.candidate_id,
        "source_lifecycle_candidate_id":episode.lifecycle_id,
        "source_result_sha256":episode.task.job.result_hash,
        "source_result_status":if episode.task.job.result_hash.is_some() {"verified"} else {"unavailable"},
        "source_score_sha256":source_score_sha256,
        "source_observation_sha256":episode.score.observation_sha256,
        "source_entry_sha256":source_entry_sha256,
        "source_spec_sha256":episode.target.spec_sha256,
        "source_quotes_sha256":episode.target.quotes_sha256,
        "source_dataset_sha256":episode.target.snapshot_sha256,
        "source_runtime":{
            "runtime_hash":runtime.runtime_hash,
            "helper_sha256":runtime.helper_sha256,
            "unit_policy_sha256":runtime.unit_policy_sha256,
            "scorer_compare_sha256":runtime.scorer_compare_sha256,
            "sandbox_source_sha256":runtime.sandbox_source_sha256
        },
        "source_book_binding":episode.task.spec.book_binding,
        "source_state_hash":episode.source_state_hash,
        "source_universe_sha256":episode.source_universe_sha256,
        "source_cohort_key":episode.source_cohort_key,
        "current_scope":serde_json::to_value(&scope)?,
        "key":episode.entry.key,
        "entry":entry_value,
        "entry_quote":serde_json::to_value(&episode.entry.quote)?,
        "exit_due_ms":episode.due,
        "exit_deadline_ms":episode.deadline,
        "exit_quote":Value::Null,
        "exit_status":"unknown",
        "exit_gap":Value::Null,
        "first_receipt_unknown":false,
        "execution":Value::Null,
        "net_pnl_if_flat":Value::Null,
        "fees":Value::Null,
        "turnover":Value::Null,
        "flat":false,
        "accounting_complete":false,
        "gaps":Vec::<String>::new(),
        "report_sha256":Value::Null,
        "source_scope":CONTINUATION_SCOPE,
        "capital_assumption":true,
        "capital":episode.spec.parameters.capital,
        "observation_projection":episode.spec.observation_projection,
        "exit_at_ms":Value::Null,
        "processed_at_ms":now,
        "recorded_at_ms":now,
        "measurement_sha256":""
    });
    match outcome {
        ContinuationOutcome::Evaluated(evaluation) => {
            let executed = evaluation.exit_status == "executed";
            let (net_pnl, fees, turnover) = match (&evaluation.execution, executed) {
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
            let mut gaps = evaluation.gaps;
            if now > episode.deadline {
                gaps.push("continuation_processed_after_deadline".into());
            }
            body["state"] = json!("evaluated");
            body["exit_quote"] = serde_json::to_value(&evaluation.exit_quote)?;
            body["exit_status"] = json!(evaluation.exit_status);
            body["exit_gap"] = evaluation.exit_gap.unwrap_or(Value::Null);
            body["first_receipt_unknown"] = json!(evaluation.first_receipt_unknown);
            body["execution"] = serde_json::to_value(&evaluation.execution)?;
            body["net_pnl_if_flat"] = serde_json::to_value(net_pnl)?;
            body["fees"] = serde_json::to_value(fees)?;
            body["turnover"] = serde_json::to_value(turnover)?;
            body["flat"] = json!(flat);
            body["accounting_complete"] = json!(accounting_complete);
            body["gaps"] = json!(gaps);
            body["report_sha256"] = serde_json::to_value(&evaluation.report_sha256)?;
            body["exit_at_ms"] = json!(evaluation.exit_at_ms);
        }
        ContinuationOutcome::Unknown {
            exit_status,
            exit_gap,
            first_receipt_unknown,
            gaps,
            reason,
            exit_quote,
            fees,
            turnover,
            report_sha256,
            exit_at_ms,
        } => {
            let mut gaps = gaps;
            gaps.push(reason.into());
            // A known original exit receipt is preserved even when the shared
            // engine cannot complete the measurement. Only a genuinely absent
            // exit quote gets the no-fabricated-price/SHA-only gap.
            if exit_quote.is_none() {
                gaps.push("no_fabricated_exit_price".into());
                gaps.push("sha_only_gap".into());
            }
            gaps.push("continuation_processed_after_deadline".into());
            body["state"] = json!("unknown");
            body["exit_status"] = json!(exit_status);
            body["exit_gap"] = exit_gap.unwrap_or_else(|| json!({"kind":reason}));
            body["first_receipt_unknown"] = json!(first_receipt_unknown);
            body["exit_quote"] = serde_json::to_value(exit_quote)?;
            body["fees"] = serde_json::to_value(fees)?;
            body["turnover"] = serde_json::to_value(turnover)?;
            body["report_sha256"] = serde_json::to_value(report_sha256)?;
            body["exit_at_ms"] = serde_json::to_value(exit_at_ms)?;
            body["gaps"] = json!(gaps);
        }
    }
    body["measurement_sha256"] = json!(experiment_ledger::lifecycle::continuation_content_sha256(
        &body
    )?);
    Ok(body)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_unknown_from_episode(
    c: &Config,
    episode: &SourceEpisode,
    reason: &str,
    now: i64,
) -> Result<Value> {
    let score_value = serde_json::to_value(&episode.score)?;
    let entry_value = serde_json::to_value(&episode.entry)?;
    let source_result_sha256 = episode.task.job.result_hash.clone();
    let source_result_status = if source_result_sha256.is_some() {
        "verified"
    } else {
        "unavailable"
    };
    let scope = ContinuationScopeV1 {
        version: CONTINUATION_VERSION,
        current_session: c.session.to_string(),
        account: c.account.clone(),
        cohort_key: episode.current_cohort_key.clone(),
        runtime_hash: episode.current_runtime_hash.clone(),
        source_ledger_spec_sha256: digest(&serde_json::to_vec(&episode.experiment)?),
    };
    let scope_sha256 = digest(&serde_json::to_vec(&scope)?);
    let source_score_sha256 = digest(&serde_json::to_vec(&score_value)?);
    let source_entry_sha256 = digest(&serde_json::to_vec(&entry_value)?);
    let identity = ContinuationIdentityV1 {
        version: CONTINUATION_VERSION,
        source_session: episode.session.clone(),
        source_lifecycle_candidate_id: episode.lifecycle_id.clone(),
        source_task_id: episode.task.spec.task_id.clone(),
        source_revision: episode.task.spec.revision,
        source_score_sha256: source_score_sha256.clone(),
        source_entry_sha256: source_entry_sha256.clone(),
        scope_sha256,
    };
    let continuation_id = digest(&serde_json::to_vec(&identity)?);
    let runtime = &episode.task.spec.runtime_binding;
    let mut gaps = vec![reason.to_owned()];
    if now > episode.deadline {
        gaps.push("continuation_processed_after_deadline".into());
    }
    gaps.push("no_fabricated_exit_price".into());
    gaps.push("sha_only_gap".into());
    let mut body = json!({
        "version":CONTINUATION_VERSION,
        "financial_abi":super::candidate::OBSERVATION_FINANCIAL_ABI,
        "continuation_id":continuation_id,
        "state":"unknown",
        "source_session":episode.session,
        "source_task_id":episode.task.spec.task_id,
        "source_revision":episode.task.spec.revision,
        "source_candidate_id":episode.task.spec.candidate_id,
        "source_lifecycle_candidate_id":episode.lifecycle_id,
        "source_result_sha256":source_result_sha256,
        "source_result_status":source_result_status,
        "source_score_sha256":source_score_sha256,
        "source_observation_sha256":episode.score.observation_sha256,
        "source_entry_sha256":source_entry_sha256,
        "source_spec_sha256":episode.target.spec_sha256,
        "source_quotes_sha256":episode.target.quotes_sha256,
        "source_dataset_sha256":episode.target.snapshot_sha256,
        "source_runtime":{
            "runtime_hash":runtime.runtime_hash,
            "helper_sha256":runtime.helper_sha256,
            "unit_policy_sha256":runtime.unit_policy_sha256,
            "scorer_compare_sha256":runtime.scorer_compare_sha256,
            "sandbox_source_sha256":runtime.sandbox_source_sha256
        },
        "source_book_binding":episode.task.spec.book_binding,
        "source_state_hash":episode.source_state_hash,
        "source_universe_sha256":episode.source_universe_sha256,
        "source_cohort_key":episode.source_cohort_key,
        "current_scope":serde_json::to_value(&scope)?,
        "key":episode.entry.key,
        "entry":entry_value,
        "entry_quote":serde_json::to_value(&episode.entry.quote)?,
        "exit_due_ms":episode.due,
        "exit_deadline_ms":episode.deadline,
        "exit_quote":Value::Null,
        "exit_status":"exit_scope_unsupported",
        "exit_gap":{
            "kind":"unsupported_session_scope",
            "reason":reason,
            "source_runtime_hash":runtime.runtime_hash,
            "recorded_runtime_hash":episode.current_runtime_hash,
            "source_cohort_key":episode.source_cohort_key,
            "recorded_cohort_key":episode.current_cohort_key
        },
        "first_receipt_unknown":false,
        "execution":Value::Null,
        "net_pnl_if_flat":Value::Null,
        "fees":Value::Null,
        "turnover":Value::Null,
        "flat":false,
        "accounting_complete":false,
        "gaps":gaps,
        "report_sha256":Value::Null,
        "source_scope":CONTINUATION_SCOPE,
        "capital_assumption":true,
        "capital":episode.spec.parameters.capital,
        "observation_projection":episode.spec.observation_projection,
        "exit_at_ms":Value::Null,
        "processed_at_ms":now,
        "recorded_at_ms":now,
        "measurement_sha256":""
    });
    body["measurement_sha256"] = json!(experiment_ledger::lifecycle::continuation_content_sha256(
        &body
    )?);
    Ok(body)
}

pub(super) fn decide_episode(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, PathBuf, Value)],
    episode: &SourceEpisode,
    now: i64,
) -> Result<ContinuationDecision> {
    if now < episode.due {
        return Ok(ContinuationDecision::Waiting {
            due_ms: episode.due,
            gap: None,
        });
    }
    let Some(observation) = observation_settings(cfg) else {
        return Err("candidate_observation_disabled".into());
    };
    let keys = BTreeSet::from([episode.entry.key.clone()]);
    let query_to = now.min(episode.deadline);
    let source_tape =
        super::super::super::super::paper_replay::virtual_observation_tape_bounded_immutable(
            &episode.directory,
            episode
                .session
                .parse()
                .map_err(|_| "continuation_source_session_scope")?,
            Some(episode.previous_session),
            &keys,
            episode.due,
            query_to,
            episode.spec.parameters.capital,
            observation.max_quote_events,
            observation.max_observations,
            observation.max_tape_bytes.min(CONTINUATION_MAX_TAPE_BYTES),
            now,
            super::super::super::super::paper_replay::TapeMode::ExitWindow,
        );
    match source_tape {
        Ok(tape) => match evaluate_observation_exit(&episode.score, &episode.spec, &tape)? {
            ObservationExitOutcome::Evaluated(evaluation) => {
                let body = build_continuation(
                    c,
                    cfg,
                    episode,
                    ContinuationOutcome::Evaluated(evaluation),
                    now,
                )?;
                let recorded = record_continuation_checked(c, cfg, chain, &body, now)?;
                Ok(ContinuationDecision::Evaluated(recorded))
            }
            ObservationExitOutcome::MeasurementUnavailable(unavailable) => {
                if now <= episode.deadline {
                    Ok(ContinuationDecision::Pending {
                        due_ms: episode.due,
                        deadline_ms: episode.deadline,
                        gap: json!({"kind":"exit_measurement_unavailable"}),
                        capture: capture_preflight(c, cfg, now, &episode.entry.key),
                    })
                } else {
                    let body = build_continuation(
                        c,
                        cfg,
                        episode,
                        ContinuationOutcome::Unknown {
                            exit_status: "exit_measurement_unavailable".into(),
                            exit_gap: Some(json!({"kind":"exit_measurement_unavailable"})),
                            first_receipt_unknown: false,
                            gaps: vec!["exit_measurement_unavailable".into()],
                            reason: "exit_measurement_unavailable",
                            exit_quote: Some(unavailable.exit_quote),
                            fees: None,
                            turnover: None,
                            report_sha256: None,
                            exit_at_ms: None,
                        },
                        now,
                    )?;
                    let recorded = record_continuation_checked(c, cfg, chain, &body, now)?;
                    Ok(ContinuationDecision::Unknown(recorded))
                }
            }
        },
        Err(gap) => {
            if now <= episode.deadline {
                return Ok(ContinuationDecision::Pending {
                    due_ms: episode.due,
                    deadline_ms: episode.deadline,
                    gap: gap.gap,
                    capture: capture_preflight(c, cfg, now, &episode.entry.key),
                });
            }
            // The CURRENT successor's capture is NOT authority for an exit in
            // the ORIGINAL source window. Never read the current book or a
            // current-session tick as a substitute for the original receipt.
            // The gap stays explicit; no original_no_data/network promise is
            // invented for an unsupported cross-session scope.
            // Keep the typed CAUSE from the tape gap. A quote-query failure is
            // NOT an absence of data: only a genuine missing receipt may claim
            // original_no_data.
            let (exit_status, first_receipt_unknown, reason) = match gap.gap["kind"].as_str() {
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
            };
            let body = build_continuation(
                c,
                cfg,
                episode,
                ContinuationOutcome::Unknown {
                    exit_status: exit_status.into(),
                    exit_gap: Some(gap.gap),
                    first_receipt_unknown,
                    gaps: vec![reason.into()],
                    reason,
                    exit_quote: None,
                    fees: None,
                    turnover: None,
                    report_sha256: None,
                    exit_at_ms: None,
                },
                now,
            )?;
            let recorded = record_continuation_checked(c, cfg, chain, &body, now)?;
            Ok(ContinuationDecision::Unknown(recorded))
        }
    }
}
