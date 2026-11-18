use super::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn owner_task_enqueue_with_evidence(
    c: &Config,
    cfg: &PlannerConfig,
    model_id: &str,
    model_revision: u64,
    parent_turn_id: &str,
    task_key: &str,
    goal: &str,
    analysis_id: &str,
    dataset_id: &str,
    now: i64,
    owner_evidence: Option<jobs::OwnerEvidence>,
) -> Result<Value> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    settings.validate()?;
    if owner_evidence.is_some() && settings.admission_limits.is_none() {
        return Ok(
            json!({"status":"not_dispatched","reason":"research_compute_policy_required",
            "next_condition":"reviewed_research_tools_admission_limits_required"}),
        );
    }
    if task_key.is_empty()
        || task_key.len() > 48
        || !task_key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || goal.trim().is_empty()
        || goal.len() > 1024
        || goal.chars().any(char::is_control)
        || model_id.trim().is_empty()
        || model_id.len() > 128
        || parent_turn_id.trim().is_empty()
        || parent_turn_id.len() > 256
    {
        return Err("research_task_args_invalid".into());
    }
    let redacted_goal = super::super::super::transcript::redact(&json!(goal));
    let safe_goal = redacted_goal.as_str().ok_or("research_goal_invalid")?;
    if safe_goal != goal {
        return Err("research_goal_secret_refused".into());
    }
    let registration = settings
        .registrations
        .iter()
        .find(|r| r.analysis_id == analysis_id && r.dataset_id == dataset_id)
        .ok_or("research_unregistered_alias")?;
    for (artifact, path, expected) in registration_files(registration) {
        check_file(path, expected, artifact)?;
    }
    let binding = book_binding(c)?;
    let policy_sha256 = digest(&super::super::super::planner::policy_bytes(cfg)?);
    let mut db = open_queue_for_owner(settings, &c.directory.join("model"), true)?;
    register_reviewed_locked(settings, &c.directory.join("model"), &mut db, registration)?;
    let session = c.session.to_string();
    let task_id = jobs::scoped_task_id(&binding, &session, task_key);
    let latest =
        jobs::latest_scoped(&mut db, &task_id).map_err(|_| "research_task_state_unavailable")?;
    let revision = match latest {
        Some(previous)
            if matches!(
                previous.status,
                jobs::TaskState::Pending
                    | jobs::TaskState::Running
                    | jobs::TaskState::Unknown
                    | jobs::TaskState::CancelRequested
            ) =>
        {
            if previous.spec.parent_turn_id == parent_turn_id
                && previous.spec.owner_evidence == owner_evidence
                && previous.spec.goal == safe_goal
                && previous.spec.policy_sha256 == policy_sha256
                && previous.spec.model_id == model_id
                && previous.spec.model_revision == model_revision
                && previous.spec.registered_analysis.analysis_id == analysis_id
                && previous.spec.registered_analysis.dataset_id == dataset_id
                && previous.spec.registered_analysis.code_sha256 == registration.code_sha256
                && previous.spec.registered_analysis.runtime_sha256 == registration.runtime_sha256
                && previous.spec.registered_analysis.input_sha256 == registration.input_sha256
                && previous.spec.resources.memory_bytes == registration.memory_bytes
                && previous.spec.resources.wall_millis == registration.wall_millis
                && previous.spec.resources.cpu_millis == registration.limits.cpu_millis
                && previous.spec.resources.tasks == registration.limits.tasks
                && previous.spec.resources.workspace_bytes == registration.limits.workspace_bytes
                && previous.spec.resources.file_bytes == registration.limits.file_bytes
            {
                super::super::super::experiment_ledger::research::reconcile(c, &mut db, &previous)?;
                return Ok(task_summary(&previous));
            }
            return Err("research_task_active".into());
        }
        Some(previous) => {
            super::super::super::experiment_ledger::research::reconcile(c, &mut db, &previous)?;
            previous
                .spec
                .revision
                .checked_add(1)
                .ok_or("research_task_revision_exhausted")?
        }
        None => 1,
    };
    let job_id = jobs::scoped_job_id(&task_id, revision);
    let job = jobs::Spec {
        id: job_id.clone(),
        review_id: digest(parent_turn_id.as_bytes()),
        script_hash: registration.code_sha256.clone(),
        input_hash: registration.input_sha256.clone(),
        runtime_hash: registration.runtime_sha256.clone(),
        memory_bytes: registration.memory_bytes,
        output_bytes: 48_000,
        wall_millis: registration.wall_millis,
        retry_safe_compute: false,
        max_attempts: 1,
        capability: jobs::SpecCapability::RegisteredAnalysis,
    };
    let expires_at_ms = now
        .checked_add(i64::try_from(settings.task_ttl_ms).map_err(|_| "research_task_ttl_invalid")?)
        .ok_or("research_task_ttl_invalid")?;
    let task = jobs::ScopedTaskSpec {
        version: 1,
        task_id,
        revision,
        task_key: task_key.to_owned(),
        job_id,
        parent_turn_id: parent_turn_id.to_owned(),
        session: c.session.to_string(),
        book_binding: binding,
        policy_sha256,
        model_id: model_id.to_owned(),
        model_revision,
        goal: safe_goal.to_owned(),
        owner_evidence,
        registered_analysis: jobs::RegisteredAnalysis {
            analysis_id: registration.analysis_id.clone(),
            dataset_id: registration.dataset_id.clone(),
            code_sha256: registration.code_sha256.clone(),
            runtime_sha256: registration.runtime_sha256.clone(),
            input_sha256: registration.input_sha256.clone(),
        },
        capability: jobs::Capability::RegisteredAnalysis,
        resources: jobs::ResourceCaps {
            cpu_millis: registration.limits.cpu_millis,
            tasks: registration.limits.tasks,
            workspace_bytes: registration.limits.workspace_bytes,
            file_bytes: registration.limits.file_bytes,
            memory_bytes: registration.memory_bytes,
            output_bytes: 48_000,
            wall_millis: registration.wall_millis,
        },
        created_at_ms: now,
        expires_at_ms,
    };
    let record = match jobs::enqueue_scoped_bounded(
        &mut db,
        &job,
        &task,
        now,
        settings.admission_limits.as_ref(),
    )
    .map_err(|_| "research_task_enqueue_refused")?
    {
        jobs::Admission::Admitted(record) => record,
        jobs::Admission::Deferred { reason, trials } => {
            return Ok(json!({"status":"not_dispatched",
            "reason":reason,"trials":trials,"task_id":task.task_id,"revision":task.revision,
            "next_condition":if reason == "research_session_compute_budget_exhausted" {"next_authorized_session"} else {"new_registered_content_or_reviewed_compute_policy"}}));
        }
    };
    super::super::super::experiment_ledger::research::reconcile(c, &mut db, &record)?;
    Ok(task_summary(&record))
}

pub(crate) fn scoped_task(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<(Connection, jobs::TaskRecord)> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    let mut db = open_queue_for_owner(settings, &c.directory.join("model"), false)?;
    let record =
        jobs::load_scoped(&mut db, task_id, revision).map_err(|_| "research_task_unavailable")?;
    if record.spec.session != c.session.to_string() || record.spec.book_binding != book_binding(c)?
    {
        return Err("research_task_unavailable".into());
    }
    Ok((db, record))
}

pub(crate) fn owner_task_status(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    _now: i64,
) -> Result<Value> {
    let (mut db, record) = scoped_task(c, cfg, task_id, revision)?;
    super::super::super::experiment_ledger::research::reconcile(c, &mut db, &record)?;
    Ok(task_summary(&record))
}

pub(crate) fn owner_task_result(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    _now: i64,
) -> Result<Value> {
    let (mut db, record) = scoped_task(c, cfg, task_id, revision)?;
    let result = worker::read_verified_result(&mut db, task_id, revision)
        .map_err(|_| "research_result_unavailable")?;
    let Some(result) = result else {
        return Ok(
            json!({"task":task_summary(&record),"result":null,"result_status":"not_verified"}),
        );
    };
    let mut result_value =
        serde_json::to_value(result).map_err(|_| "research_result_unavailable")?;
    result_value = super::super::super::transcript::redact(&result_value);
    Ok(json!({"task":task_summary(&record),"result":result_value,"result_status":"verified"}))
}

pub(crate) fn owner_task_cancel(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    now: i64,
) -> Result<Value> {
    let (mut db, _) = scoped_task(c, cfg, task_id, revision)?;
    let record = jobs::cancel_scoped(&mut db, task_id, revision, revision, now)
        .map_err(|_| "research_task_cancel_refused")?;
    super::super::super::experiment_ledger::research::reconcile(c, &mut db, &record)?;
    Ok(task_summary(&record))
}
