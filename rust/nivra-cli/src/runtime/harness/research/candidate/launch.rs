use super::*;

fn private_mode(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o777
}

fn ensure_private_dir(path: &Path) -> std::result::Result<(), &'static str> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err("candidate_stage_unavailable"),
    }
    let meta = fs::symlink_metadata(path).map_err(|_| "candidate_stage_unavailable")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("candidate_stage_invalid");
    }
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|_| "candidate_stage_unavailable")?;
    let meta = fs::symlink_metadata(path).map_err(|_| "candidate_stage_unavailable")?;
    if private_mode(&meta) & 0o077 != 0 {
        return Err("candidate_stage_invalid");
    }
    Ok(())
}

pub(super) fn write_new_private(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> std::result::Result<(), &'static str> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() || private_mode(&meta) != mode {
            return Err("candidate_stage_invalid");
        }
        let mut existing = Vec::new();
        fs::File::open(path)
            .and_then(|mut file| file.read_to_end(&mut existing))
            .map_err(|_| "candidate_stage_unavailable")?;
        return if existing == bytes {
            Ok(())
        } else {
            Err("candidate_stage_invalid")
        };
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "candidate_stage_unavailable")?;
    file.write_all(bytes)
        .map_err(|_| "candidate_stage_unavailable")?;
    file.sync_all().map_err(|_| "candidate_stage_unavailable")?;
    drop(file);
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| "candidate_stage_unavailable")?;
    let mut check = Vec::new();
    fs::File::open(path)
        .and_then(|mut file| file.read_to_end(&mut check))
        .map_err(|_| "candidate_stage_unavailable")?;
    if check != bytes {
        return Err("candidate_stage_invalid");
    }
    Ok(())
}

fn stage_executable(
    root: &Path,
    source: &Path,
    expected: &str,
    name: &str,
) -> std::result::Result<PathBuf, &'static str> {
    ensure_private_dir(root)?;
    let executables = root.join("executables");
    ensure_private_dir(&executables)?;
    let path = executables.join(format!("{name}-{expected}"));
    if path.exists() {
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| "candidate_stage_unavailable")?;
        if digest(&bytes) != expected {
            return Err("candidate_stage_invalid");
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| "candidate_stage_unavailable")?;
        if !meta.is_file() || meta.file_type().is_symlink() || private_mode(&meta) != 0o500 {
            return Err("candidate_stage_invalid");
        }
        return Ok(path);
    }
    let resolved = fs::canonicalize(source).map_err(|_| "candidate_runtime_unavailable")?;
    let bytes = crate::read_bounded(&resolved, 64 * 1024 * 1024)
        .map_err(|_| "candidate_runtime_unavailable")?;
    if digest(&bytes) != expected {
        return Err("candidate_runtime_unavailable");
    }
    write_new_private(&path, &bytes, 0o500)?;
    Ok(path)
}

fn stage_helper(
    root: &Path,
    source: &Path,
    helper_sha256: &str,
) -> std::result::Result<PathBuf, &'static str> {
    ensure_private_dir(root)?;
    let helpers = root.join("helpers");
    ensure_private_dir(&helpers)?;
    let path = helpers.join(helper_sha256);
    if path.exists() {
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .and_then(|mut file| file.read_to_end(&mut bytes))
            .map_err(|_| "candidate_stage_unavailable")?;
        if digest(&bytes) != helper_sha256 {
            return Err("candidate_stage_invalid");
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| "candidate_stage_unavailable")?;
        if !meta.is_file() || meta.file_type().is_symlink() || private_mode(&meta) != 0o500 {
            return Err("candidate_stage_invalid");
        }
        return Ok(path);
    }
    let resolved = fs::canonicalize(source).map_err(|_| "candidate_runtime_unavailable")?;
    let bytes = crate::read_bounded(&resolved, 256 * 1024 * 1024)
        .map_err(|_| "candidate_runtime_unavailable")?;
    if digest(&bytes) != helper_sha256 {
        return Err("candidate_runtime_unavailable");
    }
    write_new_private(&path, &bytes, 0o500)?;
    Ok(path)
}

pub(crate) fn stage_candidate(
    settings: &ResearchSettings,
    candidate: &CandidateComputeSettings,
    launch: &worker::CandidateLaunch,
    source: &[u8],
    params: &[u8],
    dataset: &[u8],
    execution: Option<(&[u8], &[u8], &[u8])>,
) -> std::result::Result<(), &'static str> {
    let root = &settings.stage_root;
    ensure_private_dir(root)?;
    let stage = Path::new(&launch.stage_dir);
    ensure_private_dir(stage)?;
    if digest(source) != launch.source_sha256
        || digest(params) != launch.params_sha256
        || digest(dataset) != launch.dataset_hash
    {
        return Err("candidate_input_changed");
    }
    write_new_private(&stage.join("source"), source, 0o600)?;
    write_new_private(&stage.join("params"), params, 0o600)?;
    write_new_private(&stage.join("dataset"), dataset, 0o600)?;
    match (&launch.execution, execution) {
        (Some(binding), Some((spec, manifest, events))) => {
            if digest(spec) != binding.spec_sha256
                || digest(manifest) != binding.manifest_sha256
                || digest(events) != binding.events_sha256
            {
                return Err("candidate_input_changed");
            }
            write_new_private(&stage.join("execution_spec"), spec, 0o600)?;
            write_new_private(&stage.join("execution_manifest"), manifest, 0o600)?;
            write_new_private(&stage.join("execution_events"), events, 0o600)?;
        }
        (None, None) => {}
        _ => return Err("candidate_stage_invalid"),
    }
    let helper_source = match candidate.helper_path.as_ref() {
        Some(path) => path.clone(),
        None => std::env::current_exe().map_err(|_| "candidate_runtime_unavailable")?,
    };
    let helper = stage_helper(root, &helper_source, &launch.unit_policy.helper_sha256)?;
    if helper != Path::new(&launch.helper_path) {
        return Err("candidate_stage_invalid");
    }
    let python = stage_executable(
        root,
        &candidate.sandbox.python_path,
        &launch.sandbox.python_sha256,
        "python",
    )?;
    let bwrap = stage_executable(
        root,
        &candidate.sandbox.bwrap_path,
        &launch.sandbox.bwrap_sha256,
        "bwrap",
    )?;
    if python != Path::new(&launch.python_path) || bwrap != Path::new(&launch.bwrap_path) {
        return Err("candidate_stage_invalid");
    }
    let launch_bytes = serde_json::to_vec(launch).map_err(|_| "candidate_launch_invalid")?;
    write_new_private(&stage.join("launch.json"), &launch_bytes, 0o600)?;
    Ok(())
}

pub(crate) fn machine_id_from(
    mut read: impl FnMut(&str) -> std::io::Result<String>,
) -> std::result::Result<String, &'static str> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(text) = read(path) {
            let id = text.trim();
            if !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control) {
                return Ok(id.to_owned());
            }
        }
    }
    Err("candidate_machine_unavailable")
}

pub(super) fn machine_id() -> std::result::Result<String, &'static str> {
    machine_id_from(|path| fs::read_to_string(path))
}

#[cfg(test)]
pub(crate) fn prepare_launch(
    settings: &ResearchSettings,
    task: &jobs::CandidateTaskRecord,
    lease: &str,
    now: i64,
) -> std::result::Result<worker::CandidateLaunch, &'static str> {
    let machine = machine_id()?;
    prepare_launch_with_machine(settings, task, lease, now, &machine)
}

pub(super) fn prepare_launch_with_machine(
    settings: &ResearchSettings,
    task: &jobs::CandidateTaskRecord,
    lease: &str,
    now: i64,
    machine: &str,
) -> std::result::Result<worker::CandidateLaunch, &'static str> {
    let root = &settings.stage_root;
    let stage_dir = root.join(format!("candidate-{lease}"));
    let helper_path = root
        .join("helpers")
        .join(&task.spec.unit_policy.helper_sha256);
    let python_path = root
        .join("executables")
        .join(format!("python-{}", task.spec.sandbox.python_sha256));
    let bwrap_path = root
        .join("executables")
        .join(format!("bwrap-{}", task.spec.sandbox.bwrap_sha256));
    let unit = worker::candidate_unit_name(lease).ok_or("candidate_launch_invalid")?;
    let deadline_ms = now
        .checked_add(i64::from(task.job.spec.wall_millis))
        .ok_or("candidate_launch_invalid")?;
    Ok(worker::CandidateLaunch {
        version: 2,
        job_id: task.spec.job_id.clone(),
        lease: lease.to_owned(),
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
                .ok_or("candidate_runtime_unavailable")?
                .replay,
        )
        .map_err(|_| "candidate_launch_invalid")?,
        stage_dir: stage_dir.to_string_lossy().into_owned(),
        helper_path: helper_path.to_string_lossy().into_owned(),
        python_path: python_path.to_string_lossy().into_owned(),
        bwrap_path: bwrap_path.to_string_lossy().into_owned(),
        unit,
        machine: machine.to_owned(),
        started_ms: now,
        deadline_ms,
        task_deadline_ms: deadline_ms.min(task.spec.expires_at_ms),
        execution: task.spec.execution.clone(),
        observation: match &task.spec.purpose {
            Some(jobs::CandidatePurposeV1::Observe { target }) => Some(target.as_ref().clone()),
            _ => None,
        },
    })
}

fn verify_launch(launch: &worker::CandidateLaunch, task: &jobs::CandidateTaskRecord) -> bool {
    launch.version == 2
        && launch.job_id == task.spec.job_id
        && launch.lease == task.job.lease.clone().unwrap_or_default()
        && launch.task_id == task.spec.task_id
        && launch.revision == task.spec.revision
        && launch.candidate_id == task.spec.candidate_id
        && launch.lifecycle_candidate_id == task.spec.lifecycle_candidate_id
        && launch.source_sha256 == task.spec.source_sha256
        && launch.params_sha256 == task.spec.params_sha256
        && launch.dataset_id == task.spec.dataset_id
        && launch.dataset_hash == task.spec.dataset_hash
        && launch.abi == task.spec.abi
        && launch.runtime_binding == task.spec.runtime_binding
        && launch.sandbox == task.spec.sandbox
        && launch.unit_policy == task.spec.unit_policy
        && launch.limits == task.spec.resources
        && launch.unit == worker::candidate_unit_name(&launch.lease).unwrap_or_default()
        && launch.started_ms >= task.spec.created_at_ms
        && launch.started_ms < task.spec.expires_at_ms
        && launch.deadline_ms
            == launch
                .started_ms
                .saturating_add(i64::from(task.job.spec.wall_millis))
        && launch.task_deadline_ms == launch.deadline_ms.min(task.spec.expires_at_ms)
        && launch.task_deadline_ms <= task.spec.expires_at_ms
        && launch.execution == task.spec.execution
        && launch.observation
            == match &task.spec.purpose {
                Some(jobs::CandidatePurposeV1::Observe { target }) => Some(target.as_ref().clone()),
                _ => None,
            }
        && launch.replay_json.len() <= 8192
        && serde_json::from_str::<Value>(&launch.replay_json).is_ok()
}

pub(super) fn read_launch(
    db: &Connection,
    task: &jobs::CandidateTaskRecord,
) -> std::result::Result<worker::CandidateLaunch, &'static str> {
    let lease = task
        .job
        .lease
        .as_deref()
        .ok_or("candidate_recovery_required")?;
    let bytes = worker::read_receipt(db, &task.spec.job_id, lease, "candidate_launch", 65_536)
        .map_err(|_| "candidate_recovery_required")?
        .ok_or("candidate_recovery_required")?;
    let launch: worker::CandidateLaunch =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_recovery_required")?;
    if serde_json::to_vec(&launch).map_err(|_| "candidate_recovery_required")? != bytes
        || !verify_launch(&launch, task)
    {
        return Err("candidate_recovery_required");
    }
    Ok(launch)
}

pub(super) fn read_stopped(
    db: &Connection,
    launch: &worker::CandidateLaunch,
) -> std::result::Result<Option<CandidateStopped>, &'static str> {
    let bytes = worker::read_receipt(
        db,
        &launch.job_id,
        &launch.lease,
        "candidate_stopped",
        65_536,
    )
    .map_err(|_| "candidate_stopped_receipt_failed")?;
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let stopped: CandidateStopped =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_stopped_invalid")?;
    let outcome_ok = match (stopped.cause, stopped.outcome) {
        (jobs::CandidateStopCause::Succeeded, CandidateStopOutcome::Succeeded) => {
            stopped.result_sha256.is_some()
                && stopped.result_at_ms.is_some()
                && stopped.failure_code.is_none()
        }
        (jobs::CandidateStopCause::OperatorCancel, CandidateStopOutcome::Cancelled) => true,
        (
            jobs::CandidateStopCause::DeadlineExpired,
            CandidateStopOutcome::DeadlineExpired | CandidateStopOutcome::ResultAfterDeadline,
        ) => true,
        (
            jobs::CandidateStopCause::Failure,
            CandidateStopOutcome::NoResult
            | CandidateStopOutcome::InvalidResult
            | CandidateStopOutcome::ResultTooLarge
            | CandidateStopOutcome::LaunchFailed
            | CandidateStopOutcome::Terminated,
        ) => true,
        _ => false,
    };
    let result_pair = stopped.cause != jobs::CandidateStopCause::Succeeded
        || (stopped.result_sha256.is_some() && stopped.result_at_ms.is_some());
    let failure_code_ok = stopped.failure_code.as_deref().is_none_or(|code| {
        !code.is_empty()
            && code.len() <= 64
            && code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    });
    if serde_json::to_vec(&stopped).map_err(|_| "candidate_stopped_invalid")? != bytes
        || stopped.version != 2
        || stopped.kind != "candidate_unit_stopped"
        || stopped.job_id != launch.job_id
        || stopped.lease != launch.lease
        || stopped.task_id != launch.task_id
        || stopped.revision != launch.revision
        || stopped.candidate_id != launch.candidate_id
        || stopped.unit != launch.unit
        || !stopped.proof.verify(launch)
        || !outcome_ok
        || !result_pair
        || !failure_code_ok
        || stopped
            .result_at_ms
            .is_some_and(|at| at < launch.started_ms)
        || stopped
            .result_bytes
            .is_some_and(|bytes| bytes == 0 || bytes > 64 * 1024 * 1024)
        || stopped.source_sha256 != launch.source_sha256
        || stopped.params_sha256 != launch.params_sha256
        || stopped.dataset_hash != launch.dataset_hash
        || stopped.runtime_hash != launch.runtime_binding.runtime_hash
        || stopped.stopped_at_ms < stopped.proof.checked_at_ms
    {
        return Err("candidate_stopped_invalid");
    }
    Ok(Some(stopped))
}

pub(super) fn read_stage_file(
    path: &Path,
    max: usize,
) -> std::result::Result<Vec<u8>, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let owner = fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(u32::MAX);
    let parent = path.parent().ok_or("candidate_helper_input_invalid")?;
    let parent_meta =
        fs::symlink_metadata(parent).map_err(|_| "candidate_helper_input_unavailable")?;
    if !parent_meta.is_dir()
        || parent_meta.file_type().is_symlink()
        || parent_meta.uid() != owner
        || parent_meta.permissions().mode() & 0o077 != 0
    {
        return Err("candidate_helper_input_invalid");
    }
    // One fd owns the identity check and the bytes. `O_NOFOLLOW` refuses a
    // symlink swapped in after the parent check; `O_NONBLOCK` prevents a FIFO
    // substitution from blocking the helper.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "candidate_helper_input_unavailable")?;
    let meta = file
        .metadata()
        .map_err(|_| "candidate_helper_input_unavailable")?;
    if !meta.is_file()
        || meta.len() == 0
        || meta.len() > max as u64
        || meta.uid() != owner
        || meta.permissions().mode() & 0o077 != 0
        || meta.permissions().mode() & 0o400 == 0
    {
        return Err("candidate_helper_input_invalid");
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| "candidate_helper_input_unavailable")?;
    if bytes.len() as u64 != meta.len() {
        return Err("candidate_helper_input_invalid");
    }
    Ok(bytes)
}

pub(super) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
