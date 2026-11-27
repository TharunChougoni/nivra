use super::*;
use nivra_sandbox::SandboxConfig;

pub(crate) const DEFAULT_TASK_TTL_MS: u64 = 6 * 60 * 60 * 1000;
pub(crate) const DEFAULT_TERMINATION_GRACE_MS: u64 = 30_000;

pub(crate) fn default_task_ttl_ms() -> u64 {
    DEFAULT_TASK_TTL_MS
}

pub(crate) fn is_default_task_ttl_ms(value: &u64) -> bool {
    *value == DEFAULT_TASK_TTL_MS
}

pub(crate) fn default_termination_grace_ms() -> u64 {
    DEFAULT_TERMINATION_GRACE_MS
}

pub(crate) fn is_default_termination_grace_ms(value: &u64) -> bool {
    *value == DEFAULT_TERMINATION_GRACE_MS
}

pub(crate) fn cleanup_grace_expired(
    awaiting_termination: bool,
    now: i64,
    grace_ms: u64,
    cleanup_deadline: &mut Option<i64>,
) -> bool {
    if !awaiting_termination {
        return false;
    }
    let grace = i64::try_from(grace_ms).unwrap_or(i64::MAX);
    let deadline = *cleanup_deadline.get_or_insert_with(|| now.saturating_add(grace));
    now >= deadline
}

pub(crate) fn task_state_terminal(state: jobs::TaskState) -> bool {
    matches!(
        state,
        jobs::TaskState::Terminated
            | jobs::TaskState::Succeeded
            | jobs::TaskState::Cancelled
            | jobs::TaskState::Refused
    )
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Registration {
    pub analysis_id: String,
    pub dataset_id: String,
    pub code_file: PathBuf,
    pub runtime_file: PathBuf,
    pub input_file: PathBuf,
    pub code_sha256: String,
    pub runtime_sha256: String,
    pub input_sha256: String,
    pub memory_bytes: u64,
    pub wall_millis: u32,
    pub limits: ResourceLimits,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateDataset {
    pub dataset_id: String,
    pub kind: CandidateDatasetKind,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum CandidateDatasetKind {
    CurrentSessionFeatures,
    ArchiveWindow {
        from_session: chrono::NaiveDate,
        to_session: chrono::NaiveDate,
    },
    ArchiveBars(nivra_store::market_archive::BarWindow),
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateStrategySettings {
    pub enabled: bool,
    pub abi: String,
    pub max_quote_events: usize,
    pub max_observations: usize,
    pub max_tape_bytes: u64,
    /// Wall budget for one replay. The lane worker also runs decision turns,
    /// so a longer replay stops with `strategy_replay_wall_budget`.
    #[serde(default = "default_strategy_wall_ms")]
    pub max_wall_ms: u64,
    #[serde(
        default = "default_archive_window_days",
        skip_serializing_if = "is_default_archive_window_days"
    )]
    pub max_archive_window_days: i64,
    #[serde(
        default = "default_recent_archive_sessions",
        skip_serializing_if = "is_default_recent_archive_sessions"
    )]
    pub recent_archive_sessions: usize,
}
pub(crate) const fn default_archive_window_days() -> i64 {
    14
}
fn is_default_archive_window_days(days: &i64) -> bool {
    *days == default_archive_window_days()
}
pub(crate) const fn default_recent_archive_sessions() -> usize {
    3
}
fn is_default_recent_archive_sessions(sessions: &usize) -> bool {
    *sessions == default_recent_archive_sessions()
}
fn default_strategy_wall_ms() -> u64 {
    20_000
}
impl CandidateStrategySettings {
    pub(crate) fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.abi != crate::runtime::strategy::ABI
            || !(1..=120_000).contains(&self.max_wall_ms)
            || !(1..=60).contains(&self.max_archive_window_days)
            || !(3..=20).contains(&self.recent_archive_sessions)
            || !(1..=10_000).contains(&self.max_quote_events)
            || !(1..=100_000).contains(&self.max_observations)
            || !(1..=nivra_cli::strategy_archive::MAX_EXECUTION_TAPE_BYTES as u64)
                .contains(&self.max_tape_bytes)
        {
            return Err("candidate_strategy_invalid");
        }
        Ok(())
    }
}
/// The `backtest` tool's policy. `unit_*` caps bound an archive bar job.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateComputeSettings {
    pub enabled: bool,
    pub sandbox: SandboxConfig,
    pub max_source_bytes: u32,
    pub max_params_bytes: u32,
    pub wall_millis: u32,
    #[serde(default = "default_unit_tasks")]
    pub unit_tasks: u32,
    #[serde(default = "default_unit_memory_bytes")]
    pub unit_memory_bytes: u64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub unit_cpu_millis: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub unit_wall_millis: u32,
    pub datasets: Vec<CandidateDataset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<CandidateStrategySettings>,
}
fn default_unit_tasks() -> u32 {
    16
}
fn default_unit_memory_bytes() -> u64 {
    512 * 1024 * 1024
}
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}
impl CandidateComputeSettings {
    pub(crate) fn validate(&self) -> std::result::Result<(), &'static str> {
        self.sandbox
            .validate()
            .map_err(|_| "candidate_sandbox_invalid")?;
        if let Some(strategy) = &self.strategy {
            strategy.validate()?;
        }
        if self.sandbox.network
            || self.sandbox.max_output_bytes > 65_536
            || self.max_source_bytes == 0
            || self.max_source_bytes > 262_144
            || self.max_params_bytes == 0
            || self.max_params_bytes > 65_536
            || !(1_000..=600_000).contains(&self.wall_millis)
            || self.sandbox.wall_ms > u64::from(self.wall_millis)
            || !(8..=256).contains(&self.unit_tasks)
            || self.unit_memory_bytes
                < self
                    .sandbox
                    .memory_mb
                    .saturating_mul(1024 * 1024)
                    .saturating_add(64 * 1024 * 1024)
            || self.unit_memory_bytes > 4 * 1024 * 1024 * 1024
            || (self.unit_cpu_millis != 0 && self.unit_cpu_millis > self.wall_millis)
            || (self.unit_wall_millis != 0
                && (self.unit_wall_millis < self.sandbox.wall_ms as u32
                    || self.unit_wall_millis > 600_000))
            || self.datasets.is_empty()
            || self.datasets.len() > 8
        {
            return Err("candidate_compute_invalid");
        }
        let max_archive_days = self
            .strategy
            .as_ref()
            .map_or_else(default_archive_window_days, |settings| {
                settings.max_archive_window_days
            });
        let mut aliases = BTreeSet::new();
        for dataset in &self.datasets {
            if dataset.dataset_id.is_empty()
                || dataset.dataset_id.len() > 64
                || !dataset
                    .dataset_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
                || dataset.dataset_id == super::RECENT_ARCHIVE_DATASET_ID
                || !aliases.insert(&dataset.dataset_id)
            {
                return Err("candidate_dataset_invalid");
            }
            if let CandidateDatasetKind::ArchiveWindow {
                from_session,
                to_session,
            } = dataset.kind
                && !(0..max_archive_days).contains(&(to_session - from_session).num_days())
            {
                return Err("candidate_dataset_invalid");
            }
            if let CandidateDatasetKind::ArchiveBars(window) = &dataset.kind
                && !window.validate()
            {
                return Err("candidate_dataset_invalid");
            }
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResearchSettings {
    pub queue: PathBuf,
    pub stage_root: PathBuf,
    #[serde(
        default = "default_task_ttl_ms",
        skip_serializing_if = "is_default_task_ttl_ms"
    )]
    pub task_ttl_ms: u64,
    #[serde(
        default = "default_termination_grace_ms",
        skip_serializing_if = "is_default_termination_grace_ms"
    )]
    pub termination_grace_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_max_candidates: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_limits: Option<jobs::AdmissionLimits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_max_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_compute: Option<CandidateComputeSettings>,
    /// Archived-receipt reads for the research lane's `quotes` tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_capture: Option<QuoteCaptureSettings>,
    pub registrations: Vec<Registration>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct QuoteCaptureSettings {
    pub enabled: bool,
    pub max_rows: usize,
}
impl Default for QuoteCaptureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rows: 64,
        }
    }
}
impl ResearchSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if self
            .quote_capture
            .as_ref()
            .is_some_and(|quotes| !(1..=256).contains(&quotes.max_rows))
        {
            return Err("research_quote_capture_invalid".into());
        }
        let candidate_enabled = self
            .candidate_compute
            .as_ref()
            .is_some_and(|candidate| candidate.enabled);
        if let Some(limits) = &self.admission_limits {
            limits
                .validate()
                .map_err(|_| "research_admission_limits_invalid")?;
        }
        if let Some(candidate) = &self.candidate_compute {
            candidate.validate()?;
            if candidate.enabled && self.admission_limits.is_none() {
                return Err("research_compute_policy_required".into());
            }
        }
        if self
            .library_max_candidates
            .is_some_and(|n| !(1..=256).contains(&n))
            || self
                .library_max_bytes
                .is_some_and(|n| !(4096..=1_048_576).contains(&n))
            || !self.queue.is_absolute()
            || !self.stage_root.is_absolute()
            || !(60_000..=24 * 60 * 60 * 1000).contains(&self.task_ttl_ms)
            || self.termination_grace_ms > 300_000
            || (self.registrations.is_empty() && !candidate_enabled)
            || self.registrations.len() > 32
        {
            return Err("research_settings_invalid".into());
        }
        nivra_store::private_path::file_path_ok(&self.queue)
            .map_err(|_| "research_queue_path_invalid")?;
        let mut ids = BTreeSet::new();
        for r in &self.registrations {
            if !ids.insert((&r.analysis_id, &r.dataset_id))
                || r.memory_bytes == 0
                || r.memory_bytes > 512 * 1024 * 1024
                || r.wall_millis == 0
                || r.wall_millis > 60000
                || r.limits.tasks == 0
                || r.limits.tasks > 16
                || r.limits.workspace_bytes > 64 * 1024 * 1024
                || r.limits.file_bytes > 65536
            {
                return Err("research_limits_invalid".into());
            }
            for p in [&r.code_file, &r.runtime_file, &r.input_file] {
                nivra_store::private_path::file_path_ok(p)
                    .map_err(|_| "research_registration_path_invalid")?;
            }
            for hash in [&r.code_sha256, &r.runtime_sha256, &r.input_sha256] {
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err("research_registration_hash_invalid".into());
                }
            }
        }
        Ok(())
    }
}

/// Open the existing durable research queue under a short identity/creation
/// lock. The lock is released before callers execute or poll a sandbox so
/// enqueue/status/cancel operations can keep using SQLite while work runs.
pub(crate) fn open_queue(
    settings: &ResearchSettings,
    authority: &Path,
    create_if_missing: bool,
) -> Result<Connection> {
    settings.validate()?;
    if !authority.is_absolute() {
        return Err("research_authority_invalid".into());
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(authority)?;
    let pin = settings.queue.with_extension("identity.json");
    let anchor = authority.join(format!(
        "research-queue-{}.json",
        digest(settings.queue.as_os_str().as_encoded_bytes())
    ));
    let lock_path = settings.queue.with_extension("lock");
    let owner = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(lock_path)?;
    owner.try_lock_exclusive()?;
    let _owner = AdvisoryLock::new(owner);

    let exists = settings.queue.exists();
    if pin.exists() != exists || anchor.exists() != exists {
        return Err("research_original_queue_missing".into());
    }
    if !exists && !create_if_missing {
        return Err("research_queue_unavailable".into());
    }
    if exists {
        let metadata = fs::symlink_metadata(&settings.queue)?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o777 != 0o600 {
            return Err("research_queue_identity_changed".into());
        }
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW
        | if exists {
            rusqlite::OpenFlags::empty()
        } else {
            rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        };
    let mut db = Connection::open_with_flags(&settings.queue, flags)?;
    db.busy_timeout(Duration::from_millis(50))?;
    if !exists {
        fs::set_permissions(&settings.queue, fs::Permissions::from_mode(0o600))?;
        jobs::initialize(&mut db).map_err(|_| "research_queue_invalid")?;
        let metadata = fs::symlink_metadata(&settings.queue)?;
        rollover::atomic(
            &pin,
            &json!({"device":metadata.dev(),"inode":metadata.ino()}),
        )?;
        rollover::atomic(
            &anchor,
            &json!({"device":metadata.dev(),"inode":metadata.ino()}),
        )?;
    }
    let metadata = fs::symlink_metadata(&settings.queue)?;
    let identity: Value = serde_json::from_slice(
        &crate::read_bounded(&pin, 4096).map_err(|_| "research_queue_identity_invalid")?,
    )?;
    let bound: Value = serde_json::from_slice(
        &crate::read_bounded(&anchor, 4096).map_err(|_| "research_original_binding_missing")?,
    )?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || identity["device"] != metadata.dev()
        || identity["inode"] != metadata.ino()
        || bound != identity
    {
        return Err("research_queue_identity_changed".into());
    }
    db.pragma_update(None, "synchronous", "FULL")?;
    drop(_owner);
    Ok(db)
}

pub(crate) fn register_reviewed_locked(
    settings: &ResearchSettings,
    authority: &Path,
    db: &mut Connection,
    registration: &Registration,
) -> Result<()> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(settings.queue.with_extension("lock"))?;
    lock.try_lock_exclusive()?;
    let _lock = AdvisoryLock::new(lock);
    let pin = settings.queue.with_extension("identity.json");
    let anchor = authority.join(format!(
        "research-queue-{}.json",
        digest(settings.queue.as_os_str().as_encoded_bytes())
    ));
    let metadata = fs::symlink_metadata(&settings.queue)?;
    let identity: Value = serde_json::from_slice(
        &crate::read_bounded(&pin, 4096).map_err(|_| "research_queue_identity_invalid")?,
    )?;
    let bound: Value = serde_json::from_slice(
        &crate::read_bounded(&anchor, 4096).map_err(|_| "research_original_binding_missing")?,
    )?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || identity["device"] != metadata.dev()
        || identity["inode"] != metadata.ino()
        || bound != identity
    {
        return Err("research_queue_identity_changed".into());
    }
    jobs::register_reviewed(db, &registration.code_sha256, &registration.runtime_sha256)
        .map_err(|_| "research_registration_invalid")?;
    Ok(())
}
/// A registered file has its own read failure code so a missing runtime binary
/// is not misreported as an unreadable dataset (#114).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Artifact {
    Code,
    Runtime,
    Input,
}
impl Artifact {
    const fn name(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Runtime => "runtime",
            Self::Input => "input",
        }
    }
    const fn unreadable(self) -> &'static str {
        match self {
            Self::Code => "research_code_unreadable",
            Self::Runtime => "research_runtime_unreadable",
            Self::Input => "research_input_unreadable",
        }
    }
    const fn cap(self) -> u64 {
        match self {
            Self::Code => 262144,
            Self::Runtime => 32 * 1024 * 1024,
            Self::Input => nivra_cli::strategy_archive::MAX_EXECUTION_TAPE_BYTES as u64,
        }
    }
}
pub(crate) fn registration_files(r: &Registration) -> [(Artifact, &Path, &str); 3] {
    [
        (Artifact::Code, r.code_file.as_path(), &r.code_sha256),
        (
            Artifact::Runtime,
            r.runtime_file.as_path(),
            &r.runtime_sha256,
        ),
        (Artifact::Input, r.input_file.as_path(), &r.input_sha256),
    ]
}
pub(crate) fn read_checked(
    path: &Path,
    expected: &str,
    artifact: Artifact,
) -> std::result::Result<Vec<u8>, &'static str> {
    let bytes = crate::read_bounded(path, artifact.cap()).map_err(|_| artifact.unreadable())?;
    if digest(&bytes) != expected {
        return Err("research_registered_bytes_changed");
    }
    Ok(bytes)
}
/// Bounded streaming check for readiness/projection paths that must not retain a
/// 32 MiB runtime binary in memory on every check.
pub(crate) fn check_file(
    path: &Path,
    expected: &str,
    artifact: Artifact,
) -> std::result::Result<(), &'static str> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| artifact.unreadable())?;
    let meta = file.metadata().map_err(|_| artifact.unreadable())?;
    if !meta.is_file() || meta.len() > artifact.cap() {
        return Err(artifact.unreadable());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let read = file.read(&mut buffer).map_err(|_| artifact.unreadable())?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if actual != expected {
        return Err("research_registered_bytes_changed");
    }
    Ok(())
}
pub(crate) fn verified(path: &Path, expected: &str, artifact: Artifact) -> Result<Vec<u8>> {
    read_checked(path, expected, artifact).map_err(Into::into)
}
/// Read and hash-check every registered file without retaining or exposing paths.
/// Used by preflight and readiness so a stale registration is reported before a
/// post-close review call. The first failure code is stable and file-specific.
pub(crate) fn validate_files(settings: &ResearchSettings) -> std::result::Result<(), &'static str> {
    for r in &settings.registrations {
        for (artifact, path, expected) in registration_files(r) {
            check_file(path, expected, artifact)?;
        }
    }
    Ok(())
}
/// Path-free readiness projection for every registered analysis. Reading the
/// bytes here is intentional: a configured alias is not evidence that its pinned
/// files still exist or match.
pub(crate) fn status(settings: &ResearchSettings) -> Value {
    let mut blocked = false;
    let registrations = settings
        .registrations
        .iter()
        .map(|r| {
            let mut reason = Value::Null;
            let files = registration_files(r)
                .into_iter()
                .map(
                    |(artifact, path, expected)| match check_file(path, expected, artifact) {
                        Ok(()) => json!({"artifact":artifact.name(),"status":"ready"}),
                        Err(code) => {
                            blocked = true;
                            if reason.is_null() {
                                reason = json!(code);
                            }
                            json!({"artifact":artifact.name(),"status":"blocked","reason":code})
                        }
                    },
                )
                .collect::<Vec<_>>();
            json!({"analysis_id":r.analysis_id,"dataset_id":r.dataset_id,
                "status":if reason.is_null() {"ready"} else {"blocked"},
                "reason":reason,"files":files})
        })
        .collect::<Vec<_>>();
    let mut candidate_configured_unverified = false;
    let candidate_compute = settings.candidate_compute.as_ref().map(|candidate| {
        let enabled = candidate.enabled && settings.admission_limits.is_some();
        let unit = if enabled
            && (!candidate.sandbox.python_path.is_file() || !candidate.sandbox.bwrap_path.is_file())
        {
            json!("candidate_sandbox_unavailable")
        } else {
            Value::Null
        };
        let ready = enabled && unit.is_null();
        if enabled && !ready {
            blocked = true;
        }
        if ready {
            candidate_configured_unverified = true;
        }
        json!({
            "status":if ready {"configured"} else if enabled {"blocked"} else {"disabled"},
            "reason":if ready {Value::Null} else if enabled {unit} else {json!("candidate_compute_disabled")},
            "runtime_verified":false,
            "host_qualified":false,
            "network":candidate.sandbox.network,
            "datasets":candidate.datasets.iter().map(|dataset| json!(dataset.dataset_id)).collect::<Vec<_>>(),
            "activation_authorized":false
        })
    });
    json!({
        "status":if blocked {"blocked"} else if candidate_configured_unverified {"configured"} else {"ready"},
        "runtime_verified":false,
        "host_qualified":false,
        "registrations":registrations,
        "candidate_compute":candidate_compute
    })
}

pub(crate) fn task_state_name(state: jobs::TaskState) -> &'static str {
    match state {
        jobs::TaskState::Pending => "pending",
        jobs::TaskState::Running => "running",
        jobs::TaskState::Unknown => "unknown",
        jobs::TaskState::CancelRequested => "cancel_requested",
        jobs::TaskState::Terminated => "terminated",
        jobs::TaskState::Succeeded => "succeeded",
        jobs::TaskState::Cancelled => "cancelled",
        jobs::TaskState::Refused => "refused",
    }
}

pub(crate) fn task_summary(record: &jobs::TaskRecord) -> Value {
    json!({"task_id":record.spec.task_id,"revision":record.spec.revision,
        "experiment_candidate_id":super::super::super::experiment_ledger::research::candidate_id(&record.spec),
        "scientific_state":"insufficient_evidence","edge_qualified":false,
        "status":task_state_name(record.status),"job_id":record.spec.job_id,
        "created_at_ms":record.spec.created_at_ms,"expires_at_ms":record.spec.expires_at_ms,
        "terminal_at_ms":record.terminal_at_ms,"result_sha256":record.job.result_hash,
        "analysis_id":record.spec.registered_analysis.analysis_id,
        "dataset_id":record.spec.registered_analysis.dataset_id,
        "policy_sha256":record.spec.policy_sha256,"model_id":record.spec.model_id,
        "model_revision":record.spec.model_revision})
}
