use super::*;
use nivra_research::archive_bars::{ABI, EVIDENCE, Request};
use nivra_store::market_archive::BarWindow;

pub(crate) const ARCHIVE_BARS_DATASET_ID: &str = "archive_bars";
const RUNTIME_LIMIT: u64 = 128 * 1024 * 1024;
const INPUT_LIMIT: u64 = 4 * 1024 * 1024;
const OUTPUT_LIMIT: u64 = 48_000;

fn runtime_path() -> Result<PathBuf> {
    let bin = std::env::current_exe()?;
    #[cfg(test)]
    if let Some(profile) = bin
        .parent()
        .filter(|dir| dir.ends_with("deps"))
        .and_then(Path::parent)
    {
        if std::env::var_os("NIVRA_ARCHIVE_BAR_PERF").is_some() {
            return Ok(profile.with_file_name("release").join("nivra-analysis"));
        }
        return Ok(profile.join("nivra-analysis"));
    }
    let runtime = bin.with_file_name("nivra-analysis");
    Ok(if runtime.is_file() {
        runtime
    } else {
        bin.with_file_name("nivra-analysis-x86_64-unknown-linux-musl")
    })
}

fn runtime_image() -> Result<Vec<u8>> {
    let path = fs::canonicalize(runtime_path()?).map_err(|_| "archive_bars_worker_unavailable")?;
    crate::read_bounded(&path, RUNTIME_LIMIT).map_err(|_| "archive_bars_worker_unavailable".into())
}

fn ensure_private_dir(path: &Path) -> std::result::Result<(), &'static str> {
    use std::os::unix::fs::PermissionsExt;
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("archive_bars_inputs_unavailable"),
    }
    let meta = fs::symlink_metadata(path).map_err(|_| "archive_bars_inputs_unavailable")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("archive_bars_inputs_invalid");
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| "archive_bars_inputs_unavailable")?;
    Ok(())
}

fn inputs_dir(authority: &Path) -> PathBuf {
    authority.join("archive-bar-inputs")
}

#[allow(clippy::too_many_arguments)]
pub(super) fn enqueue(
    c: &Config,
    cfg: &PlannerConfig,
    settings: &ResearchSettings,
    candidate: &CandidateComputeSettings,
    model_id: &str,
    parent_turn_id: &str,
    args: &BacktestArgs,
    windows: jobs::BacktestWindows,
    mut window: BarWindow,
    now: i64,
) -> Result<Value> {
    if args.archive_daily_window.is_some() || args.as_of_receipt_id.is_some() {
        return Ok(not_dispatched("candidate_strategy_args_conflict"));
    }
    let Some(archive) = &c.market_archive else {
        return Ok(not_dispatched("archive_config_missing"));
    };
    archive.validate()?;
    let Some(costs) = &c.paper_costs else {
        return Ok(not_dispatched("paper_costs_required"));
    };
    if windows.selection_to_ms > now || window.to >= crate::runtime::local(now).date_naive() {
        return Ok(not_dispatched("candidate_archive_bars_date_not_closed"));
    }
    if let Some(keys) = &mut window.instruments {
        keys.sort_unstable();
        keys.dedup();
    }
    let request = Request {
        window,
        splits: windows,
        capital: c.allocation()?,
        costs: costs.clone(),
        params: args.params.clone(),
        sandbox: candidate.sandbox.clone(),
        archive_url: archive.url.clone(),
        archive_password_file: archive.password_file.clone(),
    };
    if !request.valid() {
        return Ok(not_dispatched("candidate_archive_bars_invalid"));
    }
    let input = serde_json::to_vec(&request)?;
    if input.len() as u64 > INPUT_LIMIT {
        return Ok(not_dispatched("candidate_archive_bars_input_limit"));
    }
    let code = args.source.as_bytes();
    let authority = c.directory.join("model");
    let mut db = open_queue(settings, &authority, true)?;
    let session = c.session.to_string();
    let binding = book_binding(c)?;
    let task_id = jobs::scoped_task_id(&binding, &session, &args.task_key);
    let previous =
        jobs::latest_scoped(&mut db, &task_id).map_err(|_| "research_task_state_unavailable")?;
    let code_sha256 = digest(code);
    let input_sha256 = digest(&input);
    let policy_sha256 = digest(&super::super::super::super::planner::policy_bytes(cfg)?);
    let runtime_sha256 = digest(&runtime_image()?);

    let revision = if let Some(prior) = previous {
        if prior.spec.capability != jobs::Capability::ArchiveBars {
            return Ok(not_dispatched("research_task_key_conflict"));
        }
        if !task_state_terminal(prior.status) {
            if prior.spec.parent_turn_id == parent_turn_id
                && prior.spec.model_id == model_id
                && prior.spec.owner_evidence.is_none()
                && prior.spec.policy_sha256 == policy_sha256
                && prior.spec.goal == args.hypothesis
                && prior.spec.registered_analysis.dataset_id == args.dataset_id
                && prior.job.spec.script_hash == code_sha256
                && prior.job.spec.input_hash == input_sha256
                && prior.job.spec.runtime_hash == runtime_sha256
            {
                return Ok(status(&prior));
            }
            return Ok(not_dispatched("research_task_active"));
        }
        prior
            .spec
            .revision
            .checked_add(1)
            .ok_or("research_task_revision_exhausted")?
    } else {
        1
    };
    let job_id = jobs::scoped_job_id(&task_id, revision);
    let unit_wall = if candidate.unit_wall_millis == 0 {
        candidate.wall_millis
    } else {
        candidate.unit_wall_millis
    };
    let unit_cpu = if candidate.unit_cpu_millis == 0 {
        candidate.wall_millis
    } else {
        candidate.unit_cpu_millis
    };
    let wall_millis = candidate.wall_millis.min(unit_wall);
    let resources = jobs::ResourceCaps {
        cpu_millis: unit_cpu.min(wall_millis),
        tasks: candidate.unit_tasks.min(64),
        workspace_bytes: INPUT_LIMIT,
        file_bytes: INPUT_LIMIT,
        memory_bytes: candidate.unit_memory_bytes,
        output_bytes: OUTPUT_LIMIT,
        wall_millis,
    };
    let job = jobs::Spec {
        id: job_id.clone(),
        review_id: digest(parent_turn_id.as_bytes()),
        script_hash: code_sha256.clone(),
        input_hash: input_sha256.clone(),
        runtime_hash: runtime_sha256.clone(),
        memory_bytes: resources.memory_bytes,
        output_bytes: resources.output_bytes,
        wall_millis: resources.wall_millis,
        retry_safe_compute: false,
        max_attempts: 1,
        capability: jobs::SpecCapability::ArchiveBars,
    };
    let task = jobs::ScopedTaskSpec {
        version: 1,
        task_id,
        revision,
        task_key: args.task_key.clone(),
        job_id,
        parent_turn_id: parent_turn_id.to_owned(),
        session,
        book_binding: binding,
        policy_sha256,
        model_id: model_id.to_owned(),
        model_revision: 0,
        goal: args.hypothesis.clone(),
        owner_evidence: None,
        registered_analysis: jobs::RegisteredAnalysis {
            analysis_id: ABI.into(),
            dataset_id: args.dataset_id.clone(),
            code_sha256,
            runtime_sha256,
            input_sha256,
        },
        capability: jobs::Capability::ArchiveBars,
        resources,
        created_at_ms: now,
        expires_at_ms: now
            .checked_add(settings.task_ttl_ms as i64)
            .ok_or("research_task_ttl_invalid")?,
    };
    let directory = inputs_dir(&authority);
    ensure_private_dir(&directory)?;
    crate::runtime::strategy_library::write_new(&directory, &job.script_hash, code)?;
    crate::runtime::strategy_library::write_new(&directory, &job.input_hash, &input)?;
    match jobs::enqueue_scoped_bounded(
        &mut db,
        &job,
        &task,
        now,
        settings.admission_limits.as_ref(),
    )
    .map_err(|_| "research_task_enqueue_refused")?
    {
        jobs::Admission::Admitted(record) => Ok(status(&record)),
        jobs::Admission::Deferred { reason, trials } => Ok(json!({
            "status":"not_dispatched", "reason":reason, "trials":trials,
            "task_id":task.task_id, "revision":task.revision,
        })),
    }
}

pub(crate) fn bar_worker_run(authority: &Path, task: &jobs::TaskRecord) -> Result<RegisteredRun> {
    let caps = &task.spec.resources;
    let dir = inputs_dir(authority);
    let code = read_checked(
        &dir.join(&task.job.spec.script_hash),
        &task.job.spec.script_hash,
        Artifact::Code,
    )?;
    let input = crate::read_bounded(&dir.join(&task.job.spec.input_hash), INPUT_LIMIT)
        .map_err(|_| "archive_bars_input_unavailable")?;
    let runtime = runtime_image()?;
    if digest(&input) != task.job.spec.input_hash || digest(&runtime) != task.job.spec.runtime_hash
    {
        return Err("research_registered_bytes_changed".into());
    }
    Ok(RegisteredRun {
        spec: task.job.spec.clone(),
        split: worker::DevelopmentSplit::Development,
        limits: ResourceLimits {
            cpu_millis: caps.cpu_millis,
            tasks: caps.tasks,
            workspace_bytes: caps.workspace_bytes,
            file_bytes: caps.file_bytes,
        },
        code,
        runtime,
        input,
    })
}

pub(super) fn load(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<Option<(Connection, jobs::TaskRecord)>> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    let mut db = open_queue(settings, &c.directory.join("model"), false)?;
    match jobs::load_scoped(&mut db, task_id, revision) {
        Ok(record) if record.spec.capability == jobs::Capability::ArchiveBars => {
            if record.spec.session != c.session.to_string()
                || record.spec.book_binding != book_binding(c)?
            {
                return Err("research_task_unavailable".into());
            }
            Ok(Some((db, record)))
        }
        Ok(_) | Err(jobs::Error::Missing) => Ok(None),
        Err(_) => Err("research_task_state_unavailable".into()),
    }
}

pub(super) fn status(record: &jobs::TaskRecord) -> Value {
    let mut task = task_summary(record);
    task["abi"] = json!(ABI);
    task["evidence"] = json!(EVIDENCE);
    task
}

pub(super) fn result(db: &mut Connection, record: &jobs::TaskRecord) -> Result<Value> {
    let task = status(record);
    let verified = worker::read_verified_result(db, &record.spec.task_id, record.spec.revision)
        .map_err(|_| "research_result_unavailable")?;
    let Some(verified) = verified else {
        return Ok(json!({"task":task,"result":null,"result_status":"not_verified"}));
    };
    let (hash, chunks) = verified
        .result
        .observations
        .split_first()
        .ok_or("archive_bars_report_invalid")?;
    let report = chunks.concat();
    if digest(report.as_bytes()) != *hash {
        return Err("archive_bars_report_invalid".into());
    }
    let report: Value = serde_json::from_str(&report)?;
    Ok(json!({
        "task":task,"result_status":"verified","report_sha256":hash,"report":report,
        "acceptance":{"verdict":"research_only","reason":"bar_level_evidence"}
    }))
}
