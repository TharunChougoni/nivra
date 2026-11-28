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
    Ok(bin.with_file_name("nivra-analysis"))
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
    let Some(request) = bar_request(
        archive,
        window,
        windows,
        c.allocation()?,
        costs.clone(),
        args.params.clone(),
        candidate,
    ) else {
        return Ok(not_dispatched("candidate_archive_bars_invalid"));
    };
    let input = serde_json::to_vec(&request)?;
    if input.len() as u64 > INPUT_LIMIT {
        return Ok(not_dispatched("candidate_archive_bars_input_limit"));
    }
    let authority = c.directory.join("model");
    let mut db = open_queue(settings, &authority, true)?;
    let admitted = admit(&mut Admit {
        settings,
        candidate,
        db: &mut db,
        authority: &authority,
        binding: book_binding(c)?,
        session: c.session.to_string(),
        task_key: &args.task_key,
        parent_turn_id,
        model_id,
        goal: &args.hypothesis,
        dataset_id: &args.dataset_id,
        policy_sha256: digest(&super::super::super::super::planner::policy_bytes(cfg)?),
        code: args.source.as_bytes(),
        input: &input,
        limits: settings.admission_limits.as_ref(),
        now,
    })?;
    Ok(match admitted {
        Admitted::Queued(record) => status(&record),
        Admitted::Refused(value) => value,
    })
}

/// One bar job to admit to a queue. The book lane and the autonomous loop
/// share it; each brings its own queue, authority and binding.
pub(crate) struct Admit<'a> {
    pub settings: &'a ResearchSettings,
    pub candidate: &'a CandidateComputeSettings,
    pub db: &'a mut Connection,
    pub authority: &'a Path,
    pub binding: String,
    pub session: String,
    pub task_key: &'a str,
    pub parent_turn_id: &'a str,
    pub model_id: &'a str,
    pub goal: &'a str,
    pub dataset_id: &'a str,
    pub policy_sha256: String,
    pub code: &'a [u8],
    pub input: &'a [u8],
    pub limits: Option<&'a jobs::AdmissionLimits>,
    pub now: i64,
}

pub(crate) enum Admitted {
    Queued(Box<jobs::TaskRecord>),
    /// Not dispatched; the value says why.
    Refused(Value),
}

/// The archive bar request for a window, its splits and the sandbox, with
/// the archive connection only the worker reads.
pub(crate) fn bar_request(
    archive: &crate::runtime::MarketArchiveSettings,
    window: BarWindow,
    splits: jobs::BacktestWindows,
    capital: Amount,
    costs: nivra_core::finance::FeeArithmetic,
    params: Value,
    candidate: &CandidateComputeSettings,
) -> Option<Request> {
    let request = Request {
        qualification: None,
        window,
        splits,
        capital,
        costs,
        params,
        sandbox: candidate.sandbox.clone(),
        archive_url: archive.url.clone(),
        archive_password_file: archive.password_file.clone(),
    };
    request.valid().then_some(request)
}

/// The SHA-256 of the analysis runner image every bar job executes.
pub(crate) fn runtime_sha256() -> Result<String> {
    Ok(digest(&runtime_image()?))
}

pub(crate) fn admit(q: &mut Admit<'_>) -> Result<Admitted> {
    let (settings, candidate, now) = (q.settings, q.candidate, q.now);
    let (parent_turn_id, model_id) = (q.parent_turn_id, q.model_id);
    let task_id = jobs::scoped_task_id(&q.binding, &q.session, q.task_key);
    let previous =
        jobs::latest_scoped(q.db, &task_id).map_err(|_| "research_task_state_unavailable")?;
    let code_sha256 = digest(q.code);
    let input_sha256 = digest(q.input);
    let policy_sha256 = q.policy_sha256.clone();
    let runtime_sha256 = digest(&runtime_image()?);

    let revision = if let Some(prior) = previous {
        if prior.spec.capability != jobs::Capability::ArchiveBars {
            return Ok(Admitted::Refused(not_dispatched(
                "research_task_key_conflict",
            )));
        }
        if !task_state_terminal(prior.status) {
            if prior.spec.parent_turn_id == parent_turn_id
                && prior.spec.model_id == model_id
                && prior.spec.owner_evidence.is_none()
                && prior.spec.policy_sha256 == policy_sha256
                && prior.spec.goal == q.goal
                && prior.spec.registered_analysis.dataset_id == q.dataset_id
                && prior.job.spec.script_hash == code_sha256
                && prior.job.spec.input_hash == input_sha256
                && prior.job.spec.runtime_hash == runtime_sha256
            {
                return Ok(Admitted::Queued(Box::new(prior)));
            }
            return Ok(Admitted::Refused(not_dispatched("research_task_active")));
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
        task_key: q.task_key.to_owned(),
        job_id,
        parent_turn_id: parent_turn_id.to_owned(),
        session: q.session.clone(),
        book_binding: q.binding.clone(),
        policy_sha256,
        model_id: model_id.to_owned(),
        model_revision: 0,
        goal: q.goal.to_owned(),
        owner_evidence: None,
        registered_analysis: jobs::RegisteredAnalysis {
            analysis_id: ABI.into(),
            dataset_id: q.dataset_id.to_owned(),
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
    let directory = inputs_dir(q.authority);
    ensure_private_dir(&directory)?;
    crate::runtime::strategy_library::write_new(&directory, &job.script_hash, q.code)?;
    crate::runtime::strategy_library::write_new(&directory, &job.input_hash, q.input)?;
    match jobs::enqueue_scoped_bounded(q.db, &job, &task, now, q.limits)
        .map_err(|_| "research_task_enqueue_refused")?
    {
        jobs::Admission::Admitted(record) => Ok(Admitted::Queued(record)),
        jobs::Admission::Deferred { reason, trials } => Ok(Admitted::Refused(json!({
            "status":"not_dispatched", "reason":reason, "trials":trials,
            "task_id":task.task_id, "revision":task.revision,
        }))),
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
