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
/// The systemd CPU quota as a percentage rounded down to the two decimal
/// places systemd accepts. A quota below 0.01% is unsupported rather than
/// rounded up beyond the declared CPU budget.
pub fn quota_hundredths(cpu_millis: u32, wall_millis: u32) -> Option<u64> {
    if wall_millis == 0 {
        return None;
    }
    let hundredths = u64::from(cpu_millis).checked_mul(10_000)? / u64::from(wall_millis);
    (hundredths > 0).then_some(hundredths)
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

mod experiment;
mod result;
pub use experiment::*;
pub use result::*;

#[cfg(test)]
mod tests;
