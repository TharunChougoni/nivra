//! Durable retry-safe compute jobs. Separate from financial/model allowance journals.
//! No execution, credentials, network, automatic retries or strategy promotion.
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod keys;
mod queue;
mod scoped;
pub use keys::*;
pub use queue::*;
pub use scoped::*;

/// The execution contract a durable job belongs to. Registered analysis is
/// the historical default and is omitted from serialized bytes so existing
/// job hashes and old readers keep their original meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecCapability {
    #[default]
    RegisteredAnalysis,
    ArchiveBars,
}
impl SpecCapability {
    fn is_registered(&self) -> bool {
        matches!(self, Self::RegisteredAnalysis)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub id: String,
    pub review_id: String,
    pub script_hash: String,
    pub input_hash: String,
    pub runtime_hash: String,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub wall_millis: u32,
    pub retry_safe_compute: bool,
    pub max_attempts: u32,
    #[serde(default, skip_serializing_if = "SpecCapability::is_registered")]
    pub capability: SpecCapability,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredAnalysis {
    pub analysis_id: String,
    pub dataset_id: String,
    pub code_sha256: String,
    pub runtime_sha256: String,
    pub input_sha256: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    RegisteredAnalysis,
    ArchiveBars,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceCaps {
    pub cpu_millis: u32,
    pub tasks: u32,
    pub workspace_bytes: u64,
    pub file_bytes: u64,
    pub memory_bytes: u64,
    pub output_bytes: u64,
    pub wall_millis: u32,
}
/// Exact immutable owner evidence captured before admitting asynchronous work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnerEvidence {
    pub digest_id: String,
    pub owner_work_id: String,
    pub facts_sha256: String,
    pub profile_sha256: Option<String>,
    pub prompt_hashes_sha256: String,
    pub owner_claim_sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionLimits {
    pub max_trials_per_input: u32,
    pub max_session_trials: u32,
}
impl AdmissionLimits {
    pub fn validate(&self) -> Result<()> {
        if self.max_trials_per_input == 0
            || self.max_session_trials == 0
            || self.max_trials_per_input > self.max_session_trials
            || self.max_session_trials > 10_000
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
pub enum Admission {
    Admitted(Box<TaskRecord>),
    Deferred { reason: &'static str, trials: u32 },
}
/// Declared training and selection windows for a backtest. Training ends
/// before selection starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestWindows {
    pub training_from_ms: i64,
    pub training_to_ms: i64,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
}
impl BacktestWindows {
    pub fn validate(&self) -> bool {
        self.training_from_ms >= 0
            && self.training_from_ms <= self.training_to_ms
            && self.training_to_ms < self.selection_from_ms
            && self.selection_from_ms <= self.selection_to_ms
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedTaskSpec {
    pub version: u32,
    pub task_id: String,
    pub revision: u32,
    pub task_key: String,
    pub job_id: String,
    pub parent_turn_id: String,
    pub session: String,
    pub book_binding: String,
    pub policy_sha256: String,
    pub model_id: String,
    pub model_revision: u64,
    pub goal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_evidence: Option<OwnerEvidence>,
    pub registered_analysis: RegisteredAnalysis,
    pub capability: Capability,
    pub resources: ResourceCaps,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelReceipt {
    pub task_id: String,
    pub revision: u32,
    pub expected_revision: u32,
    pub requested_ms: i64,
    pub lease: Option<String>,
    pub job_state: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Running,
    Unknown,
    CancelRequested,
    Terminated,
    Succeeded,
    Cancelled,
    Refused,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub spec: ScopedTaskSpec,
    pub status: TaskState,
    pub job: Job,
    pub cancel_receipt: Option<CancelReceipt>,
    pub refusal_receipt: Option<RefusalReceipt>,
    pub terminal_at_ms: Option<i64>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskCursor {
    pub created_at_ms: i64,
    pub task_id: String,
    pub revision: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    RegistrationChanged,
    ResourceBindingChanged,
    DeadlineExpiredBeforeStart,
    OwnerBoundaryChanged,
    ComputePolicyUnavailable,
    InputArtifactUnavailable,
    RuntimeExecutableChanged,
    MachineIdentityUnavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefusalReceipt {
    pub task_id: String,
    pub revision: u32,
    pub job_id: String,
    pub reason: RefusalReason,
    pub at_ms: i64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Pending,
    Running,
    Unknown,
    CancelRequested,
    Terminated,
    Succeeded,
    Cancelled,
    Refused,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub spec: Spec,
    pub state: State,
    pub attempts: u32,
    pub lease: Option<String>,
    pub until: Option<i64>,
    pub result_hash: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Storage,
    Invalid,
    Unregistered,
    Collision,
    Capacity,
    Missing,
    State,
    Lease,
    Attempts,
}
type Result<T> = std::result::Result<T, Error>;
fn valid_hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl Spec {
    fn validate(&self) -> Result<()> {
        if !valid_hash(&self.review_id)
            || !valid_hash(&self.id)
            || !valid_hash(&self.script_hash)
            || !valid_hash(&self.input_hash)
            || !valid_hash(&self.runtime_hash)
            || self.memory_bytes == 0
            || self.memory_bytes > 4 * 1024 * 1024 * 1024
            || self.output_bytes == 0
            || self.output_bytes > 64 * 1024 * 1024
            || self.wall_millis == 0
            || self.wall_millis > 600000
            || !(1..=4).contains(&self.max_attempts)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

impl ScopedTaskSpec {
    fn validate(&self, job: &Spec, now: i64) -> Result<()> {
        let valid_text = |s: &str, max: usize| {
            !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        let valid_slug = |s: &str, max: usize| {
            !s.is_empty()
                && s.len() <= max
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        };
        let registration = &self.registered_analysis;
        let caps = &self.resources;
        if self.owner_evidence.as_ref().is_some_and(|e| {
            !valid_text(&e.digest_id, 256)
                || !valid_hash(&e.owner_work_id)
                || !valid_hash(&e.facts_sha256)
                || !valid_hash(&e.prompt_hashes_sha256)
                || !valid_hash(&e.owner_claim_sha256)
                || e.profile_sha256.as_deref().is_some_and(|h| !valid_hash(h))
        }) {
            return Err(Error::Invalid);
        }
        if self.version != 1
            || !valid_hash(&self.task_id)
            || !valid_hash(&self.job_id)
            || self.task_id != scoped_task_id(&self.book_binding, &self.session, &self.task_key)
            || self.job_id != scoped_job_id(&self.task_id, self.revision)
            || self.job_id != job.id
            || !matches!(
                (self.capability, job.capability),
                (
                    Capability::RegisteredAnalysis,
                    SpecCapability::RegisteredAnalysis
                ) | (Capability::ArchiveBars, SpecCapability::ArchiveBars)
            )
            || self.revision == 0
            || !valid_slug(&self.task_key, 48)
            || !valid_text(&self.parent_turn_id, 256)
            || chrono::NaiveDate::parse_from_str(&self.session, "%Y-%m-%d").is_err()
            || !valid_hash(&self.book_binding)
            || !valid_hash(&self.policy_sha256)
            || !valid_text(&self.model_id, 128)
            || !valid_text(&self.goal, 1024)
            || !valid_slug(&registration.analysis_id, 64)
            || !valid_slug(&registration.dataset_id, 64)
            || !valid_hash(&registration.code_sha256)
            || !valid_hash(&registration.runtime_sha256)
            || !valid_hash(&registration.input_sha256)
            || registration.code_sha256 != job.script_hash
            || registration.runtime_sha256 != job.runtime_hash
            || registration.input_sha256 != job.input_hash
            || job.retry_safe_compute
            || job.max_attempts != 1
            || caps.memory_bytes != job.memory_bytes
            || caps.output_bytes != job.output_bytes
            || caps.wall_millis != job.wall_millis
            || caps.cpu_millis == 0
            || caps.cpu_millis > caps.wall_millis
            || !(1..=64).contains(&caps.tasks)
            || !(1_048_576..=268_435_456).contains(&caps.workspace_bytes)
            || caps.file_bytes == 0
            || caps.file_bytes > caps.workspace_bytes
            || caps.wall_millis == 0
            || caps.wall_millis > 600_000
            || caps.output_bytes == 0
            || caps.output_bytes > 65_536
            || caps.memory_bytes == 0
            || caps.memory_bytes > 4 * 1024 * 1024 * 1024
            || self.created_at_ms != now
            || self.expires_at_ms <= now
            || self.expires_at_ms > now.checked_add(86_400_000).ok_or(Error::Invalid)?
            || now < 0
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
