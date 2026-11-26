//! One-shot registered computation on the original queue. No production runner is
//! provided. Implementing the trait or passing fixtures does NOT qualify isolation.
//! A reviewed host adapter must enforce containment and attest whole-unit absence;
//! a process exit, timeout or dropped handle alone is insufficient. No retries.
use nivra_store::research_jobs::{self as jobs, Job, Spec, State};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevelopmentSplit {
    Training,
    Development,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceLimits {
    pub cpu_millis: u32,
    pub tasks: u32,
    pub workspace_bytes: u64,
    pub file_bytes: u64,
}
/// Trusted registered bytes, never a model's declaration of approval. The host
/// owns dataset provenance/split qualification and registration review separately.
pub struct RegisteredRun {
    pub spec: Spec,
    pub split: DevelopmentSplit,
    pub limits: ResourceLimits,
    pub code: Vec<u8>,
    pub runtime: Vec<u8>,
    pub input: Vec<u8>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIdentity {
    pub job_id: String,
    pub lease: String,
    pub registration_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Launch {
    pub identity: ExecutionIdentity,
    pub spec: Spec,
    pub split: DevelopmentSplit,
    pub limits: ResourceLimits,
    pub started_ms: i64,
    pub deadline_ms: i64,
}
/// Only a separately qualified trusted runner may issue this receipt, after the
/// ENTIRE sandbox/cgroup is absent. Hashes record its evidence, not authenticity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoppedReceipt {
    pub identity: ExecutionIdentity,
    pub evidence_sha256: String,
}
pub enum SandboxPoll {
    Running,
    Exited {
        stopped: StoppedReceipt,
        exit_code: i32,
        output: Vec<u8>,
    },
}
/// One shared integer source for the systemd CPU quota. `hundredths` is the
/// percentage rounded down to the two decimal places systemd accepts; the
/// command formats that value and the proof compares the same value converted
/// to microseconds. A quota below 0.01% is unsupported rather than rounded up
/// beyond the declared CPU budget.
pub fn quota_hundredths(cpu_millis: u32, wall_millis: u32) -> Option<u64> {
    if wall_millis == 0 {
        return None;
    }
    let hundredths = u64::from(cpu_millis).checked_mul(10_000)? / u64::from(wall_millis);
    (hundredths > 0).then_some(hundredths)
}

pub fn quota_usec(cpu_millis: u32, wall_millis: u32) -> Option<u64> {
    quota_hundredths(cpu_millis, wall_millis)?.checked_mul(100)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerError {
    Unsupported,
    Unavailable,
    Ambiguous,
}
/// Trusted capability boundary. `start` receives ONLY registered bytes and limits,
/// not paths, financial authority, credentials, network or arbitrary shell args.
/// The adapter MUST bound output during collection, not only after allocating it.
pub trait SandboxRunner {
    fn start(
        &mut self,
        launch: &Launch,
        registered: &RegisteredRun,
    ) -> std::result::Result<(), RunnerError>;
    fn poll(
        &mut self,
        identity: &ExecutionIdentity,
    ) -> std::result::Result<SandboxPoll, RunnerError>;
    fn terminate(
        &mut self,
        identity: &ExecutionIdentity,
    ) -> std::result::Result<StoppedReceipt, RunnerError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Registration,
    Queue,
    Receipt,
    Result,
    Clock,
    State,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Registration => "registered_worker_registration_invalid",
            Self::Queue => "registered_worker_queue_unavailable",
            Self::Receipt => "registered_worker_receipt_invalid",
            Self::Result => "registered_worker_result_invalid",
            Self::Clock => "registered_worker_clock_invalid",
            Self::State => "registered_worker_state_invalid",
        })
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn canonical<T: Serialize>(v: &T) -> Result<Vec<u8>> {
    serde_json::to_vec(v).map_err(|_| Error::Receipt)
}
fn digest<T: Serialize>(v: &T) -> Result<String> {
    Ok(hash(&canonical(v)?))
}
fn is_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn alias(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
impl RegisteredRun {
    fn validate(&self) -> Result<String> {
        if self.code.is_empty()
            || self.code.len() > 262144
            || self.runtime.is_empty()
            || self.runtime.len()
                > match self.spec.capability {
                    jobs::SpecCapability::ArchiveBars => 128 * 1024 * 1024,
                    _ => 32 * 1024 * 1024,
                }
            || self.input.is_empty()
            || self.input.len() > 16 * 1024 * 1024
            || self.spec.output_bytes > 65536
            || self.spec.script_hash != hash(&self.code)
            || self.spec.runtime_hash != hash(&self.runtime)
            || self.spec.input_hash != hash(&self.input)
            || self.limits.cpu_millis == 0
            || self.limits.cpu_millis > self.spec.wall_millis
            || !(1..=64).contains(&self.limits.tasks)
            || !(1048576..=268435456).contains(&self.limits.workspace_bytes)
            || self.limits.file_bytes == 0
            || self.limits.file_bytes > self.limits.workspace_bytes
        {
            return Err(Error::Registration);
        }
        digest(&(&self.spec, &self.split, &self.limits))
    }
}
const RECEIPTS: &str = "CREATE TABLE IF NOT EXISTS registered_worker_receipts(job TEXT NOT NULL,lease TEXT NOT NULL,kind TEXT NOT NULL,body BLOB NOT NULL,PRIMARY KEY(job,lease,kind))";
fn initialize(db: &Connection) -> Result<()> {
    db.execute_batch(RECEIPTS).map_err(|_| Error::Queue)
}
fn save(db: &Connection, id: &ExecutionIdentity, kind: &str, body: &[u8]) -> Result<()> {
    if body.len() > 65536 {
        return Err(Error::Receipt);
    }
    db.execute(
        "INSERT INTO registered_worker_receipts VALUES(?,?,?,?) ON CONFLICT DO NOTHING",
        params![id.job_id, id.lease, kind, body],
    )
    .map_err(|_| Error::Queue)?;
    let old: Vec<u8> = db
        .query_row(
            "SELECT body FROM registered_worker_receipts WHERE job=? AND lease=? AND kind=?",
            params![id.job_id, id.lease, kind],
            |r| r.get(0),
        )
        .map_err(|_| Error::Queue)?;
    if old != body {
        return Err(Error::Receipt);
    }
    Ok(())
}
fn saved(db: &Connection, id: &ExecutionIdentity, kind: &str) -> Result<Option<Vec<u8>>> {
    db.query_row("SELECT body FROM registered_worker_receipts WHERE job=? AND lease=? AND kind=? AND length(body)<=65536",params![id.job_id,id.lease,kind],|r|r.get(0)).optional().map_err(|_|Error::Queue)
}
fn receipt_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind.len() <= 48
        && kind
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
/// Host-side receipt writer for the shared queue.
pub fn save_receipt(
    db: &Connection,
    job_id: &str,
    lease: &str,
    kind: &str,
    body: &[u8],
) -> Result<()> {
    if !is_hash(job_id) || !is_hash(lease) || !receipt_kind(kind) {
        return Err(Error::Receipt);
    }
    initialize(db)?;
    save(
        db,
        &ExecutionIdentity {
            job_id: job_id.into(),
            lease: lease.into(),
            registration_sha256: String::new(),
        },
        kind,
        body,
    )
}
pub fn read_receipt(
    db: &Connection,
    job_id: &str,
    lease: &str,
    kind: &str,
    max: usize,
) -> Result<Option<Vec<u8>>> {
    if !is_hash(job_id) || !is_hash(lease) || !receipt_kind(kind) || max == 0 || max > 1_048_576 {
        return Err(Error::Receipt);
    }
    initialize(db)?;
    match saved(
        db,
        &ExecutionIdentity {
            job_id: job_id.into(),
            lease: lease.into(),
            registration_sha256: String::new(),
        },
        kind,
    )? {
        Some(body) if body.len() <= max => Ok(Some(body)),
        Some(_) => Err(Error::Receipt),
        None => Ok(None),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metric {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnalysisResult {
    pub identity: ExecutionIdentity,
    pub script_hash: String,
    pub runtime_hash: String,
    pub input_hash: String,
    pub metrics: Vec<Metric>,
    pub observations: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedTaskResult {
    pub task_id: String,
    pub revision: u32,
    pub job_id: String,
    pub lease: String,
    pub result_hash: String,
    pub terminal_at_ms: Option<i64>,
    pub result: AnalysisResult,
}
fn number(s: &str) -> bool {
    if s.is_empty() || s.len() > 64 {
        return false;
    }
    let unsigned = s.strip_prefix('-').unwrap_or(s);
    let mut parts = unsigned.split('.');
    let whole = parts.next().unwrap_or_default();
    !whole.is_empty()
        && whole.bytes().all(|b| b.is_ascii_digit())
        && parts
            .next()
            .is_none_or(|f| !f.is_empty() && f.len() <= 18 && f.bytes().all(|b| b.is_ascii_digit()))
        && parts.next().is_none()
}
fn output(raw: &[u8], launch: &Launch) -> Result<AnalysisResult> {
    if raw.len() > launch.spec.output_bytes as usize || raw.len() > 65536 {
        return Err(Error::Result);
    }
    // Every object has a deny_unknown_fields typed schema; derive rejects repeated
    // fields, including nested identities/metrics. No opaque JSON escape hatch.
    let result: AnalysisResult = serde_json::from_slice(raw).map_err(|_| Error::Result)?;
    if result.identity != launch.identity
        || result.script_hash != launch.spec.script_hash
        || result.runtime_hash != launch.spec.runtime_hash
        || result.input_hash != launch.spec.input_hash
        || result.metrics.len() > 64
        || result.observations.len() > 32
        || result
            .metrics
            .iter()
            .any(|m| !alias(&m.name) || !number(&m.value))
        || result
            .metrics
            .iter()
            .map(|m| &m.name)
            .collect::<BTreeSet<_>>()
            .len()
            != result.metrics.len()
        || result
            .observations
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 1024 || s.chars().any(char::is_control))
    {
        return Err(Error::Result);
    }
    Ok(result)
}

/// Read the immutable worker receipt only after the existing job row is
/// succeeded and its lease/result hash match. No pending, cancelled,
/// ambiguous, or merely present result artifact is exposed.
pub fn read_verified_result(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
) -> Result<Option<VerifiedTaskResult>> {
    let task = jobs::load_scoped(db, task_id, revision).map_err(|_| Error::Queue)?;
    if task.status != jobs::TaskState::Succeeded {
        return Ok(None);
    }
    let receipts_ready: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='registered_worker_receipts')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| Error::Queue)?;
    if !receipts_ready {
        return Err(Error::Receipt);
    }
    let terminal_at_ms = task.terminal_at_ms.ok_or(Error::State)?;
    if terminal_at_ms < task.spec.created_at_ms || terminal_at_ms > task.spec.expires_at_ms {
        return Err(Error::State);
    }
    let lease = task.job.lease.as_deref().ok_or(Error::State)?;
    let result_hash = task.job.result_hash.as_deref().ok_or(Error::State)?;
    if !is_hash(lease) || !is_hash(result_hash) {
        return Err(Error::Receipt);
    }
    let identity = ExecutionIdentity {
        job_id: task.spec.job_id.clone(),
        lease: lease.to_owned(),
        registration_sha256: String::new(),
    };
    let launch_bytes = saved(db, &identity, "launch")?.ok_or(Error::Receipt)?;
    let launch: Launch = serde_json::from_slice(&launch_bytes).map_err(|_| Error::Receipt)?;
    if launch.identity.job_id != task.spec.job_id
        || launch.identity.lease != lease
        || canonical(&launch)? != launch_bytes
        || !is_hash(&launch.identity.registration_sha256)
        || launch.spec != task.job.spec
        || launch.spec.script_hash != task.spec.registered_analysis.code_sha256
        || launch.spec.runtime_hash != task.spec.registered_analysis.runtime_sha256
        || launch.spec.input_hash != task.spec.registered_analysis.input_sha256
        || launch.spec.memory_bytes != task.spec.resources.memory_bytes
        || launch.spec.output_bytes != task.spec.resources.output_bytes
        || launch.spec.wall_millis != task.spec.resources.wall_millis
        || launch.limits.cpu_millis != task.spec.resources.cpu_millis
        || launch.limits.tasks != task.spec.resources.tasks
        || launch.limits.workspace_bytes != task.spec.resources.workspace_bytes
        || launch.limits.file_bytes != task.spec.resources.file_bytes
        || launch.started_ms < task.spec.created_at_ms
        || launch.started_ms >= task.spec.expires_at_ms
        || launch.deadline_ms
            != launch
                .started_ms
                .saturating_add(i64::from(launch.spec.wall_millis))
        || digest(&(&launch.spec, &launch.split, &launch.limits))?
            != launch.identity.registration_sha256
    {
        return Err(Error::Receipt);
    }
    let stopped_bytes = saved(db, &launch.identity, "stopped")?.ok_or(Error::Receipt)?;
    let stopped: StoppedReceipt =
        serde_json::from_slice(&stopped_bytes).map_err(|_| Error::Receipt)?;
    if stopped.identity != launch.identity
        || !is_hash(&stopped.evidence_sha256)
        || canonical(&stopped)? != stopped_bytes
    {
        return Err(Error::Receipt);
    }
    let raw = saved(db, &launch.identity, "result")?.ok_or(Error::Receipt)?;
    if raw.len() > launch.spec.output_bytes as usize
        || raw.len() > 65_536
        || hash(&raw) != result_hash
    {
        return Err(Error::Result);
    }
    let result = output(&raw, &launch)?;
    if result.identity != launch.identity
        || result.script_hash != task.spec.registered_analysis.code_sha256
        || result.runtime_hash != task.spec.registered_analysis.runtime_sha256
        || result.input_hash != task.spec.registered_analysis.input_sha256
    {
        return Err(Error::Result);
    }
    Ok(Some(VerifiedTaskResult {
        task_id: task.spec.task_id,
        revision: task.spec.revision,
        job_id: task.spec.job_id,
        lease: lease.to_owned(),
        result_hash: result_hash.to_owned(),
        terminal_at_ms: Some(terminal_at_ms),
        result,
    }))
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Running,
    AwaitingTermination { reason: String },
    Terminated,
    Succeeded(AnalysisResult),
}
/// In-memory driver of an already durable launch. Dropping this value never
/// cancels, releases a queue slot or proves sandbox termination.
pub struct Worker {
    launch: Launch,
    task_expires_at_ms: Option<i64>,
    last_now: i64,
    refusal: Option<String>,
}
impl Worker {
    fn effective_deadline_ms(&self) -> i64 {
        self.task_expires_at_ms
            .map_or(self.launch.deadline_ms, |expires| {
                self.launch.deadline_ms.min(expires)
            })
    }
}
fn exact_job(db: &mut Connection, launch: &Launch) -> Result<Job> {
    let job = jobs::load(db, &launch.identity.job_id).map_err(|_| Error::Queue)?;
    if job.spec != launch.spec
        || job.lease.as_deref() != Some(launch.identity.lease.as_str())
        || job.until != Some(launch.deadline_ms)
    {
        return Err(Error::State);
    }
    let original = saved(db, &launch.identity, "launch")?.ok_or(Error::Receipt)?;
    if original != canonical(launch)? {
        return Err(Error::Receipt);
    }
    Ok(job)
}
/// Claims before invoking the runner. A failed/ambiguous start still returns a
/// worker whose slot remains held until trusted whole-unit termination.
pub fn start_one(
    db: &mut Connection,
    registered: &RegisteredRun,
    lease: &str,
    runner: &mut impl SandboxRunner,
    clock: impl Fn() -> i64,
) -> Result<Worker> {
    if !is_hash(lease) {
        return Err(Error::Registration);
    }
    let prior = jobs::load(db, &registered.spec.id).map_err(|_| Error::Queue)?;
    if prior.spec != registered.spec || prior.state != State::Pending {
        return Err(Error::Registration);
    }
    initialize(db)?;
    let now = clock();
    if now < 0 {
        return Err(Error::Clock);
    }
    let scoped = jobs::scoped_for_job(db, &registered.spec.id).map_err(|_| Error::Queue)?;
    if scoped.is_none() && registered.spec.capability == jobs::SpecCapability::ArchiveBars {
        return Err(Error::Registration);
    }
    if let Some(task) = &scoped {
        if task.status != jobs::TaskState::Pending {
            return Err(Error::State);
        }
        if now < task.spec.created_at_ms || now >= task.spec.expires_at_ms {
            jobs::refuse_unstarted(
                db,
                &task.spec.task_id,
                task.spec.revision,
                jobs::RefusalReason::DeadlineExpiredBeforeStart,
                now,
            )
            .map_err(|_| Error::Queue)?;
            return Err(Error::State);
        }
        let caps_match = registered.limits.cpu_millis == task.spec.resources.cpu_millis
            && registered.limits.tasks == task.spec.resources.tasks
            && registered.limits.workspace_bytes == task.spec.resources.workspace_bytes
            && registered.limits.file_bytes == task.spec.resources.file_bytes
            && registered.spec.memory_bytes == task.spec.resources.memory_bytes
            && registered.spec.output_bytes == task.spec.resources.output_bytes
            && registered.spec.wall_millis == task.spec.resources.wall_millis;
        if !caps_match {
            jobs::refuse_unstarted(
                db,
                &task.spec.task_id,
                task.spec.revision,
                jobs::RefusalReason::ResourceBindingChanged,
                now,
            )
            .map_err(|_| Error::Queue)?;
            return Err(Error::Registration);
        }
    }
    let registration_sha256 = match registered.validate() {
        Ok(value) => value,
        Err(error) => {
            if let Some(task) = &scoped {
                jobs::refuse_unstarted(
                    db,
                    &task.spec.task_id,
                    task.spec.revision,
                    jobs::RefusalReason::RegistrationChanged,
                    now,
                )
                .map_err(|_| Error::Queue)?;
            }
            return Err(error);
        }
    };
    let mut lease = lease.to_owned();
    let mut suffix = 0u32;
    // A previous V2 build could persist an unclaimed launch receipt before a
    // capacity/state refusal. Preserve that orphan byte-for-byte and choose a
    // fresh deterministic lease instead of overwriting or deleting it. The
    // claimed job and the receipt must still describe the same attempt.
    loop {
        let identity = ExecutionIdentity {
            job_id: registered.spec.id.clone(),
            lease: lease.clone(),
            registration_sha256: registration_sha256.clone(),
        };
        if saved(db, &identity, "launch")?.is_none() {
            break;
        }
        let prior = jobs::load(db, &registered.spec.id).map_err(|_| Error::Queue)?;
        if prior.state != State::Pending {
            return Err(Error::State);
        }
        suffix = suffix.saturating_add(1);
        if suffix > 4 {
            return Err(Error::Receipt);
        }
        lease = hash(format!("{lease}:orphan-launch:{suffix}").as_bytes());
    }
    let launch = Launch {
        identity: ExecutionIdentity {
            job_id: registered.spec.id.clone(),
            lease: lease.clone(),
            registration_sha256,
        },
        spec: registered.spec.clone(),
        split: registered.split.clone(),
        limits: registered.limits.clone(),
        started_ms: now,
        deadline_ms: now
            .checked_add(i64::from(registered.spec.wall_millis))
            .ok_or(Error::Clock)?,
    };
    // Claim first. A capacity/state refusal must leave no launch receipt, or a
    // later retry would deterministically reuse a poisoned intent manifest.
    let job = jobs::claim(db, &registered.spec.id, &lease, now).map_err(|_| Error::Queue)?;
    if job.spec != launch.spec || job.until != Some(launch.deadline_ms) {
        return Err(Error::Registration);
    }
    if save(db, &launch.identity, "launch", &canonical(&launch)?).is_err() {
        // The claim is durable but the intent receipt is not. The runner was
        // never invoked; terminate the unstarted attempt explicitly instead of
        // hiding the storage error behind an ambiguous Running row.
        jobs::request_cancel(db, &registered.spec.id, now).map_err(|_| Error::Queue)?;
        let receipt = digest(&"registered_launch_receipt_failed")?;
        jobs::record_termination(db, &registered.spec.id, &lease, &receipt, now)
            .map_err(|_| Error::Queue)?;
        return Err(Error::Receipt);
    }
    let mut worker = Worker {
        launch,
        task_expires_at_ms: scoped.map(|task| task.spec.expires_at_ms),
        last_now: now,
        refusal: None,
    };
    let ready = clock();
    if ready < now {
        return Err(Error::Clock);
    }
    worker.last_now = ready;
    let current = exact_job(db, &worker.launch)?;
    if ready >= worker.effective_deadline_ms() || current.state != State::Running {
        worker.refuse(db, "start_deadline_or_state_changed_before_spawn", ready)?;
        return Ok(worker);
    }
    let refusal: Option<String> = runner
        .start(&worker.launch, registered)
        .err()
        .map(|_| "start_failed_or_ambiguous".into());
    let ended = clock();
    if ended < ready {
        return Err(Error::Clock);
    }
    worker.last_now = ended;
    exact_job(db, &worker.launch)?;
    if let Some(reason) = refusal {
        worker.refuse(db, &reason, ended)?;
    } else if ended >= worker.effective_deadline_ms() {
        worker.refuse(db, "start_deadline_expired", ended)?;
    }
    Ok(worker)
}
/// Recovery never calls start or completes a potentially previously observed result.
/// Even an intact result artifact remains advisory until independently reconciled.
pub fn recover(
    db: &mut Connection,
    registered: &RegisteredRun,
    lease: &str,
    clock: impl Fn() -> i64,
) -> Result<Worker> {
    let identity = ExecutionIdentity {
        job_id: registered.spec.id.clone(),
        lease: lease.into(),
        registration_sha256: registered.validate()?,
    };
    if !is_hash(lease) {
        return Err(Error::Registration);
    }
    let bytes = saved(db, &identity, "launch")?.ok_or(Error::Receipt)?;
    let launch: Launch = serde_json::from_slice(&bytes).map_err(|_| Error::Receipt)?;
    if launch.identity != identity
        || launch.spec != registered.spec
        || launch.split != registered.split
        || launch.limits != registered.limits
    {
        return Err(Error::Receipt);
    }
    let now = clock();
    let job = exact_job(db, &launch)?;
    if !matches!(
        job.state,
        State::Running | State::Unknown | State::CancelRequested
    ) || now < launch.started_ms
    {
        return Err(Error::State);
    }
    let scoped = jobs::scoped_for_job(db, &registered.spec.id).map_err(|_| Error::Queue)?;
    let task_expires_at_ms = scoped.map(|task| task.spec.expires_at_ms);
    let mut worker = Worker {
        launch,
        task_expires_at_ms,
        last_now: now,
        refusal: None,
    };
    worker.refuse(db, "recovered_uncertain_launch", now)?;
    Ok(worker)
}
impl Worker {
    pub fn identity(&self) -> &ExecutionIdentity {
        &self.launch.identity
    }
    fn refuse(&mut self, db: &mut Connection, reason: &str, now: i64) -> Result<()> {
        exact_job(db, &self.launch)?;
        let first = match saved(db, &self.launch.identity, "refusal")? {
            Some(bytes) => serde_json::from_slice::<String>(&bytes).map_err(|_| Error::Receipt)?,
            None => reason.to_owned(),
        };
        if ![
            "start_failed_or_ambiguous",
            "start_deadline_expired",
            "start_deadline_or_state_changed_before_spawn",
            "recovered_uncertain_launch",
            "cancel_requested",
            "deadline_or_state_changed",
            "deadline_or_state_changed_during_poll",
            "poll_failed_or_ambiguous",
            "exit_receipt_invalid",
            "invalid_output_or_exit",
            "deadline_or_state_changed_during_import",
        ]
        .contains(&first.as_str())
        {
            return Err(Error::Receipt);
        }
        self.refusal = Some(first.clone());
        // Cancellation intent retains the slot even if persistence of its receipt fails.
        jobs::request_cancel(db, &self.launch.identity.job_id, now).map_err(|_| Error::Queue)?;
        save(db, &self.launch.identity, "refusal", &canonical(&first)?)
    }
    fn stopped(&self, r: &StoppedReceipt) -> Result<()> {
        if r.identity != self.launch.identity || !is_hash(&r.evidence_sha256) {
            return Err(Error::Receipt);
        }
        Ok(())
    }
    fn durable_stopped(&self, db: &Connection) -> Result<Option<StoppedReceipt>> {
        for kind in ["termination", "stopped"] {
            if let Some(bytes) = saved(db, &self.launch.identity, kind)? {
                let receipt: StoppedReceipt =
                    serde_json::from_slice(&bytes).map_err(|_| Error::Receipt)?;
                self.stopped(&receipt)?;
                if canonical(&receipt)? != bytes {
                    return Err(Error::Receipt);
                }
                return Ok(Some(receipt));
            }
        }
        Ok(None)
    }
    fn finish_termination(
        &mut self,
        db: &mut Connection,
        receipt: &StoppedReceipt,
        clock: &impl Fn() -> i64,
    ) -> Result<Outcome> {
        self.stopped(receipt)?;
        exact_job(db, &self.launch)?;
        // Preserve the first trusted whole-unit proof across crashes. Recovery
        // never needs to manufacture a new termination event for the same lease.
        let receipt = self.durable_stopped(db)?.unwrap_or_else(|| receipt.clone());
        save(
            db,
            &self.launch.identity,
            "termination",
            &canonical(&receipt)?,
        )?;
        exact_job(db, &self.launch)?;
        let now = clock();
        if now < self.last_now {
            return Err(Error::Clock);
        }
        self.last_now = now;
        jobs::record_termination(
            db,
            &self.launch.identity.job_id,
            &self.launch.identity.lease,
            &digest(&receipt)?,
            now,
        )
        .map_err(|_| Error::Queue)?;
        Ok(Outcome::Terminated)
    }
    /// One bounded driver step. Any runner error retains the occupied slot.
    pub fn poll(
        &mut self,
        db: &mut Connection,
        runner: &mut impl SandboxRunner,
        clock: impl Fn() -> i64,
        cancel: bool,
    ) -> Result<Outcome> {
        let mut now = clock();
        if now < self.last_now {
            return Err(Error::Clock);
        }
        self.last_now = now;
        let job = exact_job(db, &self.launch)?;
        if self.refusal.is_none()
            && (cancel || now >= self.effective_deadline_ms() || job.state != State::Running)
        {
            self.refuse(
                db,
                if cancel {
                    "cancel_requested"
                } else {
                    "deadline_or_state_changed"
                },
                now,
            )?;
        }
        if let Some(reason) = self.refusal.clone() {
            // A prior SQL failure may have recorded refusal only in memory.
            // Reconcile cancellation intent before accepting termination proof.
            self.refuse(db, &reason, now)?;
            if let Some(receipt) = self.durable_stopped(db)? {
                return self.finish_termination(db, &receipt, &clock);
            }
            let terminated = runner.terminate(&self.launch.identity);
            let ended = clock();
            if ended < now {
                return Err(Error::Clock);
            }
            self.last_now = ended;
            return match terminated {
                Err(_) => Ok(Outcome::AwaitingTermination { reason }),
                Ok(receipt) => self.finish_termination(db, &receipt, &clock),
            };
        }
        let polled = runner.poll(&self.launch.identity);
        let ended = clock();
        if ended < now {
            return Err(Error::Clock);
        }
        self.last_now = ended;
        now = ended;
        let latest = exact_job(db, &self.launch)?;
        if now >= self.effective_deadline_ms() || latest.state != State::Running {
            if let Ok(SandboxPoll::Exited {
                stopped,
                exit_code,
                output: raw,
            }) = &polled
            {
                save(
                    db,
                    &self.launch.identity,
                    "late_result",
                    &canonical(&(
                        raw.len(),
                        hash(&raw[..raw.len().min(65536)]),
                        exit_code,
                        digest(stopped)?,
                        "bounded_prefix_hash_late_result",
                    ))?,
                )?;
                if self.stopped(stopped).is_ok() {
                    save(db, &self.launch.identity, "stopped", &canonical(stopped)?)?;
                }
            }
            self.refuse(db, "deadline_or_state_changed_during_poll", now)?;
            return Ok(Outcome::AwaitingTermination {
                reason: "deadline_or_state_changed_during_poll".into(),
            });
        }
        match polled {
            Ok(SandboxPoll::Running) => Ok(Outcome::Running),
            Err(_) => {
                self.refuse(db, "poll_failed_or_ambiguous", now)?;
                Ok(Outcome::AwaitingTermination {
                    reason: "poll_failed_or_ambiguous".into(),
                })
            }
            Ok(SandboxPoll::Exited {
                stopped,
                exit_code,
                output: raw,
            }) => {
                if self.stopped(&stopped).is_err() {
                    self.refuse(db, "exit_receipt_invalid", now)?;
                    return Err(Error::Receipt);
                }
                let parsed = if exit_code == 0 {
                    output(&raw, &self.launch)
                } else {
                    Err(Error::Result)
                };
                if let Ok(parsed) = parsed {
                    save(db, &self.launch.identity, "stopped", &canonical(&stopped)?)?;
                    // Publish validated original bytes durably BEFORE marking complete.
                    save(db, &self.launch.identity, "result", &raw)?;
                    let latest = exact_job(db, &self.launch)?;
                    let imported = clock();
                    if imported < now {
                        return Err(Error::Clock);
                    }
                    self.last_now = imported;
                    if imported >= self.effective_deadline_ms() || latest.state != State::Running {
                        self.refuse(db, "deadline_or_state_changed_during_import", imported)?;
                        return Ok(Outcome::AwaitingTermination {
                            reason: "deadline_or_state_changed_during_import".into(),
                        });
                    }
                    let mut completed_at = imported;
                    let completed = jobs::complete_with_clock(
                        db,
                        &self.launch.identity.job_id,
                        &self.launch.identity.lease,
                        &hash(&raw),
                        || {
                            completed_at = clock();
                            if completed_at < imported
                                || completed_at >= self.effective_deadline_ms()
                            {
                                -1
                            } else {
                                completed_at
                            }
                        },
                    );
                    if completed_at < imported {
                        return Err(Error::Clock);
                    }
                    self.last_now = completed_at;
                    if completed_at >= self.effective_deadline_ms() {
                        self.refuse(db, "deadline_or_state_changed_during_import", completed_at)?;
                        return Ok(Outcome::AwaitingTermination {
                            reason: "deadline_or_state_changed_during_import".into(),
                        });
                    }
                    completed.map_err(|_| Error::Queue)?;
                    Ok(Outcome::Succeeded(parsed))
                } else {
                    save(
                        db,
                        &self.launch.identity,
                        "rejected_result",
                        &canonical(&(
                            raw.len(),
                            hash(&raw[..raw.len().min(65536)]),
                            "bounded_prefix_hash_invalid_schema_identity_output_or_exit",
                        ))?,
                    )?;
                    self.refuse(db, "invalid_output_or_exit", now)?;
                    self.finish_termination(db, &stopped, &clock)
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostObservation {
    pub network_unshared: bool,
    pub home_unmounted: bool,
    pub inputs_read_only: bool,
    pub cgroup_absent: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExperimentReport {
    pub sandbox_experiment: String,
    pub scientific_acceptance: String,
    pub certification: String,
    pub promotion: String,
    pub execution: String,
    pub live_enabled: bool,
    pub missing: Vec<String>,
}
const SCIENTIFIC_GAPS: &[&str] = &[
    "preregistration",
    "out_of_sample",
    "cost_stress",
    "walk_forward",
    "holdout",
    "shared_core_backtest",
    "historical_bundle",
];
fn metric_value<'a>(result: &'a AnalysisResult, name: &str) -> Option<&'a str> {
    result
        .metrics
        .iter()
        .find(|metric| metric.name == name)
        .map(|metric| metric.value.as_str())
}
/// A measured sandbox run is evidence only. It never certifies or promotes a strategy.
pub fn accept_measured_experiment(
    result: &AnalysisResult,
    script_sha256: &str,
    runtime_sha256: &str,
    input_sha256: &str,
    observed: &HostObservation,
) -> ExperimentReport {
    let mut missing = SCIENTIFIC_GAPS
        .iter()
        .map(|gap| (*gap).to_owned())
        .collect::<Vec<_>>();
    let metrics_hidden = [
        "net_visible",
        "home_visible",
        "host_file_visible",
        "input_writable",
    ]
    .iter()
    .all(|name| metric_value(result, name) == Some("0"));
    let bound = result.script_hash == script_sha256
        && result.runtime_hash == runtime_sha256
        && result.input_hash == input_sha256
        && is_hash(&result.identity.job_id)
        && is_hash(&result.identity.lease)
        && is_hash(&result.identity.registration_sha256)
        && is_hash(script_sha256)
        && is_hash(runtime_sha256)
        && is_hash(input_sha256);
    let contained = observed.network_unshared
        && observed.home_unmounted
        && observed.inputs_read_only
        && observed.cgroup_absent
        && metrics_hidden;
    if !bound {
        missing.push("result_binding".into());
    }
    if !contained {
        missing.push("containment_observation".into());
    }
    ExperimentReport {
        sandbox_experiment: if bound && contained {
            "MEASURED".into()
        } else {
            "REJECTED".into()
        },
        scientific_acceptance: "NOT_ACCEPTED".into(),
        certification: "NOT_CERTIFIED".into(),
        promotion: "NOT_AUTHORIZED".into(),
        execution: "NOT_AUTHORIZED".into(),
        live_enabled: false,
        missing,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};
    struct Fake {
        clock: Rc<Cell<i64>>,
        launch: Option<Launch>,
        starts: usize,
        mode: u8,
        terminate_fail: bool,
        advance: Option<i64>,
    }
    impl Fake {
        fn new(clock: Rc<Cell<i64>>, mode: u8) -> Self {
            Self {
                clock,
                launch: None,
                starts: 0,
                mode,
                terminate_fail: false,
                advance: None,
            }
        }
        fn stopped(&self) -> StoppedReceipt {
            StoppedReceipt {
                identity: self.launch.as_ref().unwrap().identity.clone(),
                evidence_sha256: "d".repeat(64),
            }
        }
    }
    impl SandboxRunner for Fake {
        fn start(&mut self, l: &Launch, _: &RegisteredRun) -> std::result::Result<(), RunnerError> {
            self.starts += 1;
            self.launch = Some(l.clone());
            if self.mode == 1 {
                Err(RunnerError::Ambiguous)
            } else {
                Ok(())
            }
        }
        fn poll(&mut self, _: &ExecutionIdentity) -> std::result::Result<SandboxPoll, RunnerError> {
            if let Some(at) = self.advance {
                self.clock.set(at);
            }
            if self.mode == 2 {
                return Err(RunnerError::Unavailable);
            }
            if self.mode == 3 {
                return Ok(SandboxPoll::Running);
            }
            let l = self.launch.as_ref().unwrap();
            let mut result = AnalysisResult {
                identity: l.identity.clone(),
                script_hash: l.spec.script_hash.clone(),
                runtime_hash: l.spec.runtime_hash.clone(),
                input_hash: l.spec.input_hash.clone(),
                metrics: vec![Metric {
                    name: "count".into(),
                    value: "1".into(),
                }],
                observations: vec!["Synthetic computation only".into()],
            };
            if self.mode == 4 {
                result.identity.lease = "e".repeat(64);
            }
            if self.mode == 5 {
                result.metrics[0].value = "NaN".into();
            }
            let mut raw = canonical(&result).unwrap();
            if self.mode == 6 {
                raw = br#"{"identity":{},"identity":{}}"#.to_vec();
            }
            if self.mode == 7 {
                raw = vec![b'x'; 4097];
            }
            if self.mode == 8 {
                result.metrics.push(result.metrics[0].clone());
                raw = canonical(&result).unwrap();
            }
            if self.mode == 9 {
                raw = String::from_utf8(raw)
                    .unwrap()
                    .replacen("{", "{\"output_path\":\"/tmp/escape\",", 1)
                    .into_bytes();
            }
            let mut stopped = self.stopped();
            if self.mode == 10 {
                stopped.identity.job_id = "f".repeat(64);
            }
            Ok(SandboxPoll::Exited {
                stopped,
                exit_code: if self.mode == 11 { 1 } else { 0 },
                output: raw,
            })
        }
        fn terminate(
            &mut self,
            _: &ExecutionIdentity,
        ) -> std::result::Result<StoppedReceipt, RunnerError> {
            if self.terminate_fail {
                Err(RunnerError::Ambiguous)
            } else {
                Ok(self.stopped())
            }
        }
    }
    fn setup_with_expiry(expires_at_ms: i64) -> (Connection, RegisteredRun, Rc<Cell<i64>>) {
        let mut db = Connection::open_in_memory().unwrap();
        db.execute_batch("PRAGMA synchronous=FULL").unwrap();
        jobs::initialize(&mut db).unwrap();
        let (code, runtime, input) = (
            b"registered-analysis-v1".to_vec(),
            b"reviewed-static-runtime".to_vec(),
            b"development-data".to_vec(),
        );
        let task_id = jobs::scoped_task_id(&"1".repeat(64), "2026-10-31", "shadow-check");
        let job_id = jobs::scoped_job_id(&task_id, 1);
        let spec = Spec {
            id: job_id.clone(),
            review_id: "a".repeat(64),
            script_hash: hash(&code),
            runtime_hash: hash(&runtime),
            input_hash: hash(&input),
            memory_bytes: 64 * 1024 * 1024,
            output_bytes: 4096,
            wall_millis: 1000,
            retry_safe_compute: false,
            max_attempts: 1,
            capability: jobs::SpecCapability::RegisteredAnalysis,
        };
        jobs::register_reviewed(&mut db, &spec.script_hash, &spec.runtime_hash).unwrap();
        jobs::enqueue_scoped(
            &mut db,
            &spec,
            &jobs::ScopedTaskSpec {
                owner_evidence: None,
                version: 1,
                task_id,
                revision: 1,
                task_key: "shadow-check".into(),
                job_id: spec.id.clone(),
                parent_turn_id: "turn-2026-10-31-1".into(),
                session: "2026-10-31".into(),
                book_binding: "1".repeat(64),
                policy_sha256: "2".repeat(64),
                model_id: "gpt-6.1-sol".into(),
                model_revision: 3,
                goal: "Evaluate the registered analysis on the development split".into(),
                registered_analysis: jobs::RegisteredAnalysis {
                    analysis_id: "registered-analysis".into(),
                    dataset_id: "development-v1".into(),
                    code_sha256: spec.script_hash.clone(),
                    runtime_sha256: spec.runtime_hash.clone(),
                    input_sha256: spec.input_hash.clone(),
                },
                capability: jobs::Capability::RegisteredAnalysis,
                resources: jobs::ResourceCaps {
                    cpu_millis: 500,
                    tasks: 2,
                    workspace_bytes: 1_048_576,
                    file_bytes: 4096,
                    memory_bytes: spec.memory_bytes,
                    output_bytes: spec.output_bytes,
                    wall_millis: spec.wall_millis,
                },
                created_at_ms: 100,
                expires_at_ms,
            },
            100,
        )
        .unwrap();
        (
            db,
            RegisteredRun {
                spec,
                split: DevelopmentSplit::Development,
                limits: ResourceLimits {
                    cpu_millis: 500,
                    tasks: 2,
                    workspace_bytes: 1048576,
                    file_bytes: 4096,
                },
                code,
                runtime,
                input,
            },
            Rc::new(Cell::new(100)),
        )
    }
    fn setup() -> (Connection, RegisteredRun, Rc<Cell<i64>>) {
        setup_with_expiry(60_100)
    }
    #[test]
    fn registered_worker_retains_uncertain_slots_and_imports_only_bound_results() {
        let lease = "c".repeat(64);
        let task_id = jobs::scoped_task_id(&"1".repeat(64), "2026-10-31", "shadow-check");
        let (mut scoped_db, scoped_registration, _) = setup();
        let initial = jobs::load_scoped(&mut scoped_db, &task_id, 1).unwrap();
        assert_eq!(initial.status, jobs::TaskState::Pending);
        assert_eq!(
            jobs::enqueue_scoped(
                &mut scoped_db,
                &scoped_registration.spec,
                &initial.spec,
                101,
            )
            .unwrap(),
            initial
        );
        assert!(
            read_verified_result(&mut scoped_db, &task_id, 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            jobs::enqueue_scoped(
                &mut scoped_db,
                &scoped_registration.spec,
                &initial.spec,
                100
            )
            .unwrap(),
            initial
        );
        let mut changed_task = initial.spec.clone();
        changed_task.goal.push_str(" with changed bytes");
        assert_eq!(
            jobs::enqueue_scoped(
                &mut scoped_db,
                &scoped_registration.spec,
                &changed_task,
                100
            )
            .unwrap_err(),
            jobs::Error::Collision
        );
        assert_eq!(
            jobs::active_scoped(&mut scoped_db)
                .unwrap()
                .unwrap()
                .spec
                .task_id,
            initial.spec.task_id
        );
        assert_eq!(
            jobs::list_scoped(&mut scoped_db, "2026-10-31", None, 100)
                .unwrap()
                .len(),
            1
        );
        let cancelled =
            jobs::cancel_scoped(&mut scoped_db, &initial.spec.task_id, 1, 1, 101).unwrap();
        assert_eq!(cancelled.status, jobs::TaskState::Cancelled);
        assert!(cancelled.cancel_receipt.is_some());
        assert_eq!(
            jobs::cancel_scoped(&mut scoped_db, &initial.spec.task_id, 1, 1, 102).unwrap(),
            cancelled
        );
        let mut cancelled_revision = initial.spec.clone();
        cancelled_revision.revision = 2;
        cancelled_revision.job_id = jobs::scoped_job_id(&cancelled_revision.task_id, 2);
        cancelled_revision.created_at_ms = 102;
        cancelled_revision.expires_at_ms = 60_102;
        let mut cancelled_job = scoped_registration.spec.clone();
        cancelled_job.id = cancelled_revision.job_id.clone();
        assert_eq!(
            jobs::enqueue_scoped(&mut scoped_db, &cancelled_job, &cancelled_revision, 102)
                .unwrap()
                .status,
            jobs::TaskState::Pending
        );
        let successor = jobs::active_scoped(&mut scoped_db).unwrap().unwrap();
        assert_eq!(successor.spec.revision, 2);
        assert_eq!(successor.status, jobs::TaskState::Pending);
        assert_eq!(
            jobs::load_scoped(&mut scoped_db, &initial.spec.task_id, 1)
                .unwrap()
                .status,
            jobs::TaskState::Cancelled
        );
        let (mut refusal_db, refusal_registration, _) = setup();
        let pending = jobs::load_scoped(&mut refusal_db, &task_id, 1).unwrap();
        let refused = jobs::refuse_unstarted(
            &mut refusal_db,
            &pending.spec.task_id,
            1,
            jobs::RefusalReason::RegistrationChanged,
            101,
        )
        .unwrap();
        assert_eq!(refused.status, jobs::TaskState::Refused);
        assert!(refused.refusal_receipt.is_some());
        assert!(jobs::active_scoped(&mut refusal_db).unwrap().is_none());
        let mut next_spec = refusal_registration.spec.clone();
        let mut next_task = pending.spec.clone();
        next_task.revision = 2;
        next_task.job_id = jobs::scoped_job_id(&next_task.task_id, 2);
        next_spec.id = next_task.job_id.clone();
        next_task.parent_turn_id = "turn-2026-10-31-2".into();
        next_task.goal.push_str(" revised");
        next_task.created_at_ms = 102;
        next_task.expires_at_ms = 60_102;
        assert_eq!(
            jobs::enqueue_scoped(&mut refusal_db, &next_spec, &next_task, 102)
                .unwrap()
                .status,
            jobs::TaskState::Pending
        );
        assert_eq!(
            jobs::latest_scoped(&mut refusal_db, &next_task.task_id)
                .unwrap()
                .unwrap()
                .spec
                .revision,
            2
        );
        let (mut mismatched_db, mut mismatched, mismatched_clock) = setup();
        let mut mismatched_runner = Fake::new(mismatched_clock.clone(), 0);
        mismatched.input.push(b'x');
        assert!(
            start_one(
                &mut mismatched_db,
                &mismatched,
                &lease,
                &mut mismatched_runner,
                || mismatched_clock.get()
            )
            .is_err()
        );
        assert_eq!(mismatched_runner.starts, 0);
        assert_eq!(
            jobs::load_scoped(&mut mismatched_db, &task_id, 1)
                .unwrap()
                .status,
            jobs::TaskState::Refused
        );
        let (mut db, mut registered, clock) = setup();
        let mut runner = Fake::new(clock.clone(), 0);
        registered.spec.memory_bytes += 1;
        assert!(start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).is_err());
        assert_eq!(runner.starts, 0);
        assert_eq!(
            jobs::load_scoped(&mut db, &task_id, 1).unwrap().status,
            jobs::TaskState::Pending
        );
        registered.spec.memory_bytes -= 1;
        let mut worker =
            start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
        assert_eq!(
            jobs::load(&mut db, &registered.spec.id).unwrap().state,
            State::Running
        );
        assert!(start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).is_err());
        assert_eq!(runner.starts, 1);
        // A state/capacity refusal must leave no launch receipt for the
        // rejected lease. This is the deterministic retry-poison regression.
        let refused_lease = "b".repeat(64);
        assert!(
            start_one(&mut db, &registered, &refused_lease, &mut runner, || clock
                .get())
            .is_err()
        );
        let refused_identity = ExecutionIdentity {
            job_id: registered.spec.id.clone(),
            lease: refused_lease,
            registration_sha256: String::new(),
        };
        assert!(saved(&db, &refused_identity, "launch").unwrap().is_none());
        let identity = worker.identity().clone();
        assert!(matches!(
            worker
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap(),
            Outcome::Succeeded(_)
        ));
        let job = jobs::load(&mut db, &registered.spec.id).unwrap();
        assert_eq!(job.state, State::Succeeded);
        let bytes = saved(&db, &identity, "result").unwrap().unwrap();
        assert_eq!(job.result_hash, Some(hash(&bytes)));
        let verified = read_verified_result(&mut db, &task_id, 1).unwrap().unwrap();
        assert_eq!(verified.job_id, registered.spec.id);
        assert_eq!(verified.result_hash, job.result_hash.unwrap());
        assert_eq!(verified.result.metrics[0].name, "count");
        assert!(recover(&mut db, &registered, &lease, || clock.get()).is_err());
        assert_eq!(runner.starts, 1);
        let (mut expiry_db, expiry_registration, expiry_clock) = setup_with_expiry(106);
        let mut expiry_runner = Fake::new(expiry_clock.clone(), 0);
        let start_clock = expiry_clock.clone();
        let mut expiry_worker = start_one(
            &mut expiry_db,
            &expiry_registration,
            &lease,
            &mut expiry_runner,
            move || {
                let now = start_clock.get() + 1;
                start_clock.set(now);
                now
            },
        )
        .unwrap();
        let poll_clock = expiry_clock.clone();
        assert!(matches!(
            expiry_worker
                .poll(
                    &mut expiry_db,
                    &mut expiry_runner,
                    move || {
                        let now = poll_clock.get() + 1;
                        poll_clock.set(now);
                        now
                    },
                    false
                )
                .unwrap(),
            Outcome::AwaitingTermination { .. }
        ));
        assert_ne!(
            jobs::load(&mut expiry_db, &expiry_registration.spec.id)
                .unwrap()
                .state,
            State::Succeeded
        );
        assert!(
            read_verified_result(&mut expiry_db, &task_id, 1)
                .unwrap()
                .is_none()
        );
        assert_eq!(expiry_runner.starts, 1);
        let (mut fault_db, fault_registration, fault_clock) = setup();
        let mut fault_runner = Fake::new(fault_clock.clone(), 0);
        let mut fault_worker = start_one(
            &mut fault_db,
            &fault_registration,
            &lease,
            &mut fault_runner,
            || fault_clock.get(),
        )
        .unwrap();
        fault_db.execute_batch("CREATE TRIGGER reject_result BEFORE INSERT ON registered_worker_receipts WHEN NEW.kind='result' BEGIN SELECT RAISE(FAIL,'injected storage failure'); END").unwrap();
        assert!(
            fault_worker
                .poll(
                    &mut fault_db,
                    &mut fault_runner,
                    || fault_clock.get(),
                    false
                )
                .is_err()
        );
        assert_eq!(
            jobs::load(&mut fault_db, &fault_registration.spec.id)
                .unwrap()
                .state,
            State::Running
        );
        assert!(
            saved(&fault_db, fault_worker.identity(), "result")
                .unwrap()
                .is_none()
        );
        let (mut import_db, import_registration, _) = setup();
        let import_clock = Rc::new(Cell::new(100));
        let mut import_runner = Fake::new(import_clock.clone(), 0);
        let mut import_worker = start_one(
            &mut import_db,
            &import_registration,
            &lease,
            &mut import_runner,
            || import_clock.get(),
        )
        .unwrap();
        let calls = Cell::new(0);
        assert!(matches!(
            import_worker
                .poll(
                    &mut import_db,
                    &mut import_runner,
                    || {
                        let n = calls.get();
                        calls.set(n + 1);
                        if n < 2 { 100 } else { 1100 }
                    },
                    false
                )
                .unwrap(),
            Outcome::AwaitingTermination { .. }
        ));
        assert_eq!(
            jobs::load(&mut import_db, &import_registration.spec.id)
                .unwrap()
                .state,
            State::CancelRequested
        );
        assert!(
            saved(&import_db, import_worker.identity(), "result")
                .unwrap()
                .is_some()
        );
        // A fresh sample inside completion admission that is past the effective
        // deadline keeps the imported result and requests termination. It does
        // not mark the job succeeded or release the slot.
        let (mut locked_db, locked_registration, locked_clock) = setup();
        let mut locked_runner = Fake::new(locked_clock, 0);
        let mut locked_worker = start_one(
            &mut locked_db,
            &locked_registration,
            &lease,
            &mut locked_runner,
            || 100,
        )
        .unwrap();
        let samples = Cell::new(0);
        assert!(matches!(
            locked_worker
                .poll(
                    &mut locked_db,
                    &mut locked_runner,
                    || {
                        let n = samples.get();
                        samples.set(n + 1);
                        if n < 3 { 100 } else { 1100 }
                    },
                    false
                )
                .unwrap(),
            Outcome::AwaitingTermination { reason } if reason == "deadline_or_state_changed_during_import"
        ));
        assert_eq!(
            jobs::load(&mut locked_db, &locked_registration.spec.id)
                .unwrap()
                .state,
            State::CancelRequested
        );
        assert_eq!(
            locked_worker
                .poll(&mut locked_db, &mut locked_runner, || 100, false)
                .unwrap_err(),
            Error::Clock
        );
        assert!(
            saved(&locked_db, locked_worker.identity(), "result")
                .unwrap()
                .is_some()
        );
        let mut recovered = recover(&mut locked_db, &locked_registration, &lease, || 1100).unwrap();
        locked_runner.terminate_fail = true;
        assert_eq!(
            recovered
                .poll(&mut locked_db, &mut locked_runner, || 1100, false)
                .unwrap(),
            Outcome::Terminated
        );
        assert_eq!(locked_runner.starts, 1);

        // Cancellation SQL failure retains the in-memory driver and occupied
        // slot. The next poll reissues intent, never launch, before termination.
        let (mut cancel_db, cancel_registration, cancel_clock) = setup();
        let mut cancel_runner = Fake::new(cancel_clock, 3);
        let mut cancel_worker = start_one(
            &mut cancel_db,
            &cancel_registration,
            &lease,
            &mut cancel_runner,
            || 100,
        )
        .unwrap();
        cancel_db.execute_batch("CREATE TRIGGER reject_cancel BEFORE UPDATE ON jobs WHEN NEW.state='CancelRequested' BEGIN SELECT RAISE(FAIL,'injected cancel failure'); END").unwrap();
        assert!(
            cancel_worker
                .poll(&mut cancel_db, &mut cancel_runner, || 100, true)
                .is_err()
        );
        assert_eq!(
            jobs::load(&mut cancel_db, &cancel_registration.spec.id)
                .unwrap()
                .state,
            State::Running
        );
        cancel_db
            .execute_batch("DROP TRIGGER reject_cancel")
            .unwrap();
        assert_eq!(
            cancel_worker
                .poll(&mut cancel_db, &mut cancel_runner, || 100, false)
                .unwrap(),
            Outcome::Terminated
        );
        assert_eq!(cancel_runner.starts, 1);

        for mode in 1..=11 {
            let (mut db, registered, clock) = setup();
            let mut runner = Fake::new(clock.clone(), mode);
            runner.terminate_fail = true;
            let mut worker =
                start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
            let outcome = worker.poll(&mut db, &mut runner, || clock.get(), mode == 3);
            assert!(!matches!(outcome, Ok(Outcome::Succeeded(_))));
            let job = jobs::load(&mut db, &registered.spec.id).unwrap();
            assert!(matches!(
                job.state,
                State::CancelRequested | State::Cancelled
            ));
            assert_eq!(job.result_hash, None);
            assert_eq!(runner.starts, 1);
            if job.state == State::CancelRequested {
                assert!(matches!(
                    worker
                        .poll(&mut db, &mut runner, || clock.get(), false)
                        .unwrap(),
                    Outcome::AwaitingTermination { .. }
                ));
                runner.terminate_fail = false;
                assert_eq!(
                    worker
                        .poll(&mut db, &mut runner, || clock.get(), false)
                        .unwrap(),
                    Outcome::Terminated
                );
            }
            assert_eq!(runner.starts, 1);
        }
        let (mut db, registered, clock) = setup();
        let mut runner = Fake::new(clock.clone(), 0);
        let mut worker =
            start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
        runner.advance = Some(1100);
        assert!(matches!(
            worker
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap(),
            Outcome::AwaitingTermination { .. }
        ));
        assert_eq!(
            jobs::load(&mut db, &registered.spec.id).unwrap().state,
            State::CancelRequested
        );
        assert!(saved(&db, worker.identity(), "result").unwrap().is_none());
        assert_eq!(
            worker
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap(),
            Outcome::Terminated
        );
        let (mut db, registered, clock) = setup();
        let mut runner = Fake::new(clock.clone(), 3);
        let worker = start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
        drop(worker);
        let mut restored = recover(&mut db, &registered, &lease, || clock.get()).unwrap();
        assert_eq!(runner.starts, 1);
        runner.terminate_fail = true;
        assert!(matches!(
            restored
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap(),
            Outcome::AwaitingTermination { .. }
        ));
        runner.terminate_fail = false;
        assert_eq!(
            restored
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap(),
            Outcome::Terminated
        );
        let (mut db, registered, clock) = setup();
        let mut runner = Fake::new(clock.clone(), 3);
        let mut worker =
            start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
        clock.set(99);
        assert_eq!(
            worker
                .poll(&mut db, &mut runner, || clock.get(), false)
                .unwrap_err(),
            Error::Clock
        );
        assert_eq!(
            jobs::load(&mut db, &registered.spec.id).unwrap().state,
            State::Running
        );
        let (mut crash_db, crash_registration, crash_clock) = setup();
        let mut crash_runner = Fake::new(crash_clock.clone(), 1);
        let mut crash_worker = start_one(
            &mut crash_db,
            &crash_registration,
            &lease,
            &mut crash_runner,
            || crash_clock.get(),
        )
        .unwrap();
        crash_db.execute_batch("CREATE TRIGGER fail_termination BEFORE UPDATE ON jobs WHEN NEW.state='Cancelled' BEGIN SELECT RAISE(FAIL,'injected crash boundary'); END").unwrap();
        assert!(
            crash_worker
                .poll(
                    &mut crash_db,
                    &mut crash_runner,
                    || crash_clock.get(),
                    false
                )
                .is_err()
        );
        assert!(
            saved(&crash_db, crash_worker.identity(), "termination")
                .unwrap()
                .is_some()
        );
        drop(crash_worker);
        crash_db
            .execute_batch("DROP TRIGGER fail_termination")
            .unwrap();
        let mut recovered = recover(&mut crash_db, &crash_registration, &lease, || {
            crash_clock.get()
        })
        .unwrap();
        crash_runner.terminate_fail = true; // Original durable proof must suffice.
        assert_eq!(
            recovered
                .poll(
                    &mut crash_db,
                    &mut crash_runner,
                    || crash_clock.get(),
                    false
                )
                .unwrap(),
            Outcome::Terminated
        );
        assert_eq!(crash_runner.starts, 1);
        for mode in [1, 2] {
            let (mut db, registered, clock) = setup();
            let mut runner = Fake::new(clock.clone(), mode);
            runner.terminate_fail = true;
            let mut worker =
                start_one(&mut db, &registered, &lease, &mut runner, || clock.get()).unwrap();
            let _ = worker.poll(&mut db, &mut runner, || clock.get(), false);
            let original = saved(&db, worker.identity(), "refusal").unwrap().unwrap();
            drop(worker);
            let mut worker = recover(&mut db, &registered, &lease, || clock.get()).unwrap();
            assert_eq!(
                saved(&db, worker.identity(), "refusal").unwrap().unwrap(),
                original
            );
            runner.terminate_fail = false;
            assert_eq!(
                worker
                    .poll(&mut db, &mut runner, || clock.get(), false)
                    .unwrap(),
                Outcome::Terminated
            );
            assert_eq!(runner.starts, 1);
        }
        let (mut late_db, late_registered, late_clock) = setup();
        let mut late_runner = Fake::new(late_clock, 0);
        let samples = Cell::new(0);
        let late = start_one(
            &mut late_db,
            &late_registered,
            &lease,
            &mut late_runner,
            || {
                let n = samples.get();
                samples.set(n + 1);
                if n == 0 { 100 } else { 1100 }
            },
        )
        .unwrap();
        assert_eq!(late_runner.starts, 0);
        assert_eq!(
            jobs::load(&mut late_db, &late_registered.spec.id)
                .unwrap()
                .state,
            State::CancelRequested
        );
        assert!(
            saved(&late_db, late.identity(), "launch")
                .unwrap()
                .is_some()
        );
        let (_, plan_registered, _) = setup();
        let plan_launch = Launch {
            identity: ExecutionIdentity {
                job_id: plan_registered.spec.id.clone(),
                lease: lease.clone(),
                registration_sha256: "d".repeat(64),
            },
            spec: plan_registered.spec.clone(),
            split: plan_registered.split.clone(),
            limits: plan_registered.limits.clone(),
            started_ms: 100,
            deadline_ms: 1100,
        };
        let plan = crate::bubblewrap::command_plan(
            &plan_launch,
            std::path::Path::new("/var/tmp/nivra-research-plan"),
        )
        .unwrap();
        assert!(plan.argv.iter().any(|arg| arg == "--unshare-net"));
        assert!(plan.argv.iter().any(|arg| arg == "CPUQuota=50.00%"));
        assert!(plan.argv.iter().any(|arg| arg == "--unshare-user"));
        assert!(plan.argv.iter().any(|arg| arg == "--disable-userns"));
        assert!(
            !plan
                .argv
                .iter()
                .any(|arg| arg == "--share-net" || arg.contains("/home"))
        );
        let tail: Vec<_> = plan.argv.iter().rev().take(3).collect();
        assert_eq!(
            tail.into_iter()
                .rev()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["/runtime", "/inputs/code", "/inputs/input"]
        );
        assert!(
            crate::bubblewrap::command_plan(&plan_launch, std::path::Path::new("/home/review"))
                .is_err()
        );
        assert!(
            crate::bubblewrap::command_plan(&plan_launch, std::path::Path::new("/root/review"))
                .is_err()
        );
        assert!(
            crate::bubblewrap::command_plan(&plan_launch, std::path::Path::new("relative"))
                .is_err()
        );
        let mut queued = Connection::open_in_memory().unwrap();
        queued.execute_batch("PRAGMA synchronous=FULL").unwrap();
        jobs::initialize(&mut queued).unwrap();
        let script = hash(b"registered-analysis-v1");
        let runtime_hash = hash(b"reviewed-static-runtime");
        let input_hash = hash(b"development-data");
        jobs::register_reviewed(&mut queued, &script, &runtime_hash).unwrap();
        let tool = crate::AnalysisTool::bind(
            "a".repeat(64),
            std::sync::Arc::new(std::sync::Mutex::new(queued)),
            vec![crate::RegisteredAnalysis {
                id: "cohort".into(),
                script_hash: script.clone(),
                runtime_hash: runtime_hash.clone(),
                memory_bytes: 64 * 1024 * 1024,
                output_bytes: 4096,
                wall_millis: 1000,
            }],
            vec![crate::Dataset {
                id: "day".into(),
                input_hash: input_hash.clone(),
                split: crate::DevelopmentSplit::Development,
            }],
            std::sync::Arc::new(|| 100),
        )
        .unwrap();
        let enqueued = tool
            .enqueue(crate::AnalysisArgs {
                experiment_id: "exp-1".into(),
                analysis_id: "cohort".into(),
                dataset_id: "day".into(),
            })
            .unwrap();
        assert!(!enqueued.execution_started);
        assert!(!enqueued.promotion_authorized);
        assert_eq!(enqueued.state, State::Pending);
        assert!(
            tool.enqueue(crate::AnalysisArgs {
                experiment_id: "exp-2".into(),
                analysis_id: "other".into(),
                dataset_id: "day".into(),
            })
            .is_err()
        );
        let measured = AnalysisResult {
            identity: ExecutionIdentity {
                job_id: "b".repeat(64),
                lease: lease.clone(),
                registration_sha256: "d".repeat(64),
            },
            script_hash: script.clone(),
            runtime_hash: runtime_hash.clone(),
            input_hash: input_hash.clone(),
            metrics: [
                "net_visible",
                "home_visible",
                "host_file_visible",
                "input_writable",
            ]
            .into_iter()
            .map(|name| Metric {
                name: name.into(),
                value: "0".into(),
            })
            .collect(),
            observations: vec!["containment_probe".into()],
        };
        let observed = HostObservation {
            network_unshared: true,
            home_unmounted: true,
            inputs_read_only: true,
            cgroup_absent: true,
        };
        let accepted =
            accept_measured_experiment(&measured, &script, &runtime_hash, &input_hash, &observed);
        assert_eq!(accepted.sandbox_experiment, "MEASURED");
        assert_eq!(accepted.scientific_acceptance, "NOT_ACCEPTED");
        assert_eq!(accepted.certification, "NOT_CERTIFIED");
        assert_eq!(accepted.promotion, "NOT_AUTHORIZED");
        assert!(!accepted.live_enabled);
        assert!(accepted.missing.iter().any(|gap| gap == "holdout"));
        let mut open_network = observed.clone();
        open_network.network_unshared = false;
        assert_eq!(
            accept_measured_experiment(
                &measured,
                &script,
                &runtime_hash,
                &input_hash,
                &open_network
            )
            .sandbox_experiment,
            "REJECTED"
        );
        let mut visible = measured.clone();
        visible.metrics[0].value = "1".into();
        assert_eq!(
            accept_measured_experiment(&visible, &script, &runtime_hash, &input_hash, &observed)
                .sandbox_experiment,
            "REJECTED"
        );
        // Recovery with changed registered split cannot adopt the original launch.
        let mut registered = registered;
        registered.split = DevelopmentSplit::Training;
        assert!(recover(&mut db, &registered, &lease, || 100).is_err());
    }
}
