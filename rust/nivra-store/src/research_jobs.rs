//! Durable retry-safe compute jobs. Separate from financial/model allowance journals.
//! No execution, credentials, network, automatic retries or strategy promotion.
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod candidate;
mod keys;
mod queue;
mod scoped;
pub use candidate::*;
pub use keys::*;
pub use queue::*;
pub use scoped::*;

pub const DEFAULT_MAX_EXIT_DELAY_MS: i64 = 60_000;
fn default_max_exit_delay_ms() -> i64 {
    DEFAULT_MAX_EXIT_DELAY_MS
}
fn is_default_max_exit_delay_ms(value: &i64) -> bool {
    *value == DEFAULT_MAX_EXIT_DELAY_MS
}
/// The execution contract a durable job belongs to. Registered analysis is
/// the historical default and is omitted from serialized bytes so existing
/// job hashes and old readers keep their original meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpecCapability {
    #[default]
    RegisteredAnalysis,
    CandidateSource,
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
/// Declared windows for a candidate replay. Training must end before the
/// selection window starts; the ranking scorer never fits a model, so the
/// training window is a lifecycle declaration, not a fitted sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateWindows {
    pub training_from_ms: i64,
    pub training_to_ms: i64,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
}
impl CandidateWindows {
    fn validate(&self) -> bool {
        self.training_from_ms >= 0
            && self.training_from_ms <= self.training_to_ms
            && self.training_to_ms < self.selection_from_ms
            && self.selection_from_ms <= self.selection_to_ms
    }
}
/// Honest execution provenance for the host-side candidate runner. `kind`
/// names the convention: a fingerprint of the actual scorer engine sources,
/// never a label hash standing in for a runtime. The helper and owned-unit
/// policy hashes are part of the runtime identity because a changed helper or
/// containment contract is a different execution engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRuntimeBinding {
    pub kind: String,
    pub abi: String,
    pub engine: String,
    pub scorer_compare_sha256: String,
    pub sandbox_source_sha256: String,
    pub helper_sha256: String,
    pub unit_policy_sha256: String,
    pub runtime_hash: String,
}
impl CandidateRuntimeBinding {
    fn validate(&self) -> bool {
        self.kind == "source_fingerprint"
            && self.abi == "scorer_replay_v1"
            && self.engine == "sadr_sandbox::score_source"
            && valid_hash(&self.scorer_compare_sha256)
            && valid_hash(&self.sandbox_source_sha256)
            && valid_hash(&self.helper_sha256)
            && valid_hash(&self.unit_policy_sha256)
            && valid_hash(&self.runtime_hash)
            && self.runtime_hash
                == candidate_runtime_hash(
                    &self.scorer_compare_sha256,
                    &self.sandbox_source_sha256,
                    &self.helper_sha256,
                    &self.unit_policy_sha256,
                )
    }
}
/// Authoritative reason one candidate attempt stopped. This is a domain cause,
/// not a queue-state guess: an operator cancel is distinct from task TTL or
/// unit wall expiry, and both are distinct from a launch/result failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateStopCause {
    Succeeded,
    OperatorCancel,
    DeadlineExpired,
    Failure,
}
impl CandidateStopCause {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::OperatorCancel => "operator_cancel",
            Self::DeadlineExpired => "deadline_expired",
            Self::Failure => "failure",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "succeeded" => Some(Self::Succeeded),
            "operator_cancel" => Some(Self::OperatorCancel),
            "deadline_expired" => Some(Self::DeadlineExpired),
            "failure" => Some(Self::Failure),
            _ => None,
        }
    }
}
/// Complete containment policy for one owned candidate unit. The queue's
/// `ResourceCaps.tasks` stays the admitted payload cap (one); `unit_tasks` is
/// the systemd TasksMax for the helper, bwrap and its setup threads. These are
/// deliberately separate so a queue slot never silently becomes a Linux task
/// limit or vice versa. `max_result_bytes` is the reviewed result cap captured
/// at submit; a later config change never rewrites an existing task's cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateUnitPolicy {
    pub version: u32,
    pub helper_sha256: String,
    pub helper_abi: String,
    pub unit_tasks: u32,
    pub unit_memory_bytes: u64,
    pub unit_cpu_millis: u32,
    pub unit_wall_millis: u32,
    pub unit_open_files: u64,
    #[serde(
        default = "default_candidate_max_result_bytes",
        skip_serializing_if = "is_default_candidate_max_result_bytes"
    )]
    pub max_result_bytes: u32,
    #[serde(
        default = "default_candidate_restart",
        skip_serializing_if = "is_default_candidate_restart"
    )]
    pub restart: String,
    pub payload_max_processes: u64,
    pub payload_max_open_files: u64,
    pub kill_mode: String,
    pub proof_abi: String,
}
pub const CANDIDATE_RESULT_MAX_BYTES: u32 = 1_048_576;
pub const CANDIDATE_RESULT_MIN_BYTES: u32 = 1_024;
pub const fn default_candidate_max_result_bytes() -> u32 {
    CANDIDATE_RESULT_MAX_BYTES
}
pub fn is_default_candidate_max_result_bytes(value: &u32) -> bool {
    *value == CANDIDATE_RESULT_MAX_BYTES
}
pub fn default_candidate_restart() -> String {
    "no".into()
}
pub fn is_default_candidate_restart(value: &str) -> bool {
    value == "no"
}
impl CandidateUnitPolicy {
    pub fn validate_for(&self, sandbox: &CandidateSandboxPolicy, resources: &ResourceCaps) -> bool {
        let memory = sandbox.memory_mb.saturating_mul(1024 * 1024);
        self.version == 1
            && valid_hash(&self.helper_sha256)
            && self.helper_abi == "candidate_helper_v1"
            && (8..=256).contains(&self.unit_tasks)
            && self.unit_memory_bytes >= memory.saturating_add(64 * 1024 * 1024)
            && self.unit_memory_bytes <= 4 * 1024 * 1024 * 1024
            && self.unit_cpu_millis > 0
            && self.unit_cpu_millis <= self.unit_wall_millis
            && self.unit_wall_millis >= sandbox.wall_ms as u32
            && self.unit_wall_millis <= 600_000
            && self.unit_open_files >= sandbox.max_open_files.saturating_add(16)
            && self.unit_open_files <= 4096
            && (CANDIDATE_RESULT_MIN_BYTES..=CANDIDATE_RESULT_MAX_BYTES)
                .contains(&self.max_result_bytes)
            && self.restart == "no"
            && self.payload_max_processes == sandbox.max_processes
            && self.payload_max_open_files == sandbox.max_open_files
            && self.kill_mode == "control-group"
            && self.proof_abi == "candidate_unit_proof_v1"
            && resources.tasks == 1
            && u64::from(resources.tasks) <= u64::from(self.unit_tasks)
    }
    pub fn policy_sha256(&self) -> String {
        sha256_hex(serde_json::to_vec(self).unwrap_or_default().as_slice())
    }
}
/// Pinned sandbox policy. The two hash fields are content hashes of the
/// resolved application-selected interpreter and bwrap executables, captured
/// at submit and revalidated before launch and result verification. They do
/// not claim that shared libraries or the host containment are byte-pinned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSandboxPolicy {
    pub abi: String,
    pub engine: String,
    pub network: bool,
    pub cpu_seconds: u64,
    pub memory_mb: u64,
    pub wall_ms: u64,
    pub max_output_bytes: u64,
    pub max_keys: u64,
    pub max_processes: u64,
    pub max_open_files: u64,
    pub executable_scope: String,
    pub python_sha256: String,
    pub bwrap_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_sha256: Option<String>,
}
impl CandidateSandboxPolicy {
    fn validate(&self) -> bool {
        self.abi == "scorer_replay_v1"
            && self.engine == "sadr_sandbox::score_source"
            && !self.network
            && (1..=30).contains(&self.cpu_seconds)
            && (32..=2048).contains(&self.memory_mb)
            && (1..=30_000).contains(&self.wall_ms)
            && (1..=262_144).contains(&self.max_output_bytes)
            && (1..=4096).contains(&self.max_keys)
            && (8..=256).contains(&self.max_processes)
            && (16..=128).contains(&self.max_open_files)
            && self.executable_scope == "python_and_bwrap_content_only"
            && valid_hash(&self.python_sha256)
            && valid_hash(&self.bwrap_sha256)
            && !(self.env_sha256.is_some() && self.env_dir.is_none())
            && self
                .env_dir
                .as_deref()
                .is_none_or(|dir| dir.starts_with('/') && !dir.contains(".."))
            && self.env_sha256.as_deref().is_none_or(valid_hash)
    }
}
pub const CANDIDATE_EXECUTION_ABI: &str = "cash_quote_action_tape_v1";

/// Immutable execution binding for a candidate that also requests the
/// after-cost cash replay. The typed execution rules live in the CLI; the queue
/// stores only the ABI and the content hashes of the exact frozen artifacts so
/// the store never owns an untyped execution state machine or a second engine.
/// Omission preserves the historical ranking-only task bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateExecutionBinding {
    pub version: u32,
    pub abi: String,
    pub spec_sha256: String,
    pub manifest_sha256: String,
    pub events_sha256: String,
    /// Hash of the execution rules with hypothesis/parent prose removed. The
    /// admission input key uses this hash so rewriting prose cannot reset the
    /// compute budget, while `spec_sha256` still binds the exact artifact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_sha256: Option<String>,
}
impl CandidateExecutionBinding {
    pub fn validate(&self) -> bool {
        self.version == 1
            && self.abi == CANDIDATE_EXECUTION_ABI
            && valid_hash(&self.spec_sha256)
            && valid_hash(&self.manifest_sha256)
            && valid_hash(&self.events_sha256)
            && self.rules_sha256.as_deref().is_none_or(valid_hash)
    }
}
/// Immutable runtime identity for one prospective observation attempt. It is
/// the original candidate runtime plus the virtual-observation quote authority:
/// the scorer and helper bytes are unchanged, while the adapter that may read
/// research quotes is a distinct, explicitly recorded authority. A changed
/// helper, unit policy, scorer or sandbox is a different observation runtime
/// and is refused rather than silently re-executed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationRuntimeBindingV1 {
    pub version: u32,
    pub replay_abi: String,
    pub quote_authority: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub runtime_hash: String,
    pub helper_sha256: String,
    pub unit_policy_sha256: String,
    pub scorer_compare_sha256: String,
    pub sandbox_source_sha256: String,
}
impl ObservationRuntimeBindingV1 {
    pub fn validate(&self) -> bool {
        self.version == 1
            && self.replay_abi == "observation_single_entry_v1"
            && self.quote_authority == "research_observation_virtual_only"
            && valid_hash(&self.source_sha256)
            && valid_hash(&self.params_sha256)
            && valid_hash(&self.runtime_hash)
            && valid_hash(&self.helper_sha256)
            && valid_hash(&self.unit_policy_sha256)
            && valid_hash(&self.scorer_compare_sha256)
            && valid_hash(&self.sandbox_source_sha256)
    }
}
/// One frozen prospective-observation target. Every field is captured before
/// the helper runs: the original candidate identity/result, the real freeze
/// clock, the declared observation window/holding, and the exact snapshot,
/// quote-tape and execution-spec artifacts. The target never carries a
/// preselected key; the key is chosen after the new score completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationTargetV1 {
    pub version: u32,
    pub source_task_id: String,
    pub source_revision: u32,
    pub source_candidate_id: String,
    pub source_lifecycle_candidate_id: String,
    pub source_result_sha256: String,
    pub source_runtime_hash: String,
    pub authoring_at_ms: i64,
    pub policy_sha256: String,
    pub signal_ms: i64,
    pub target_frozen_at_ms: i64,
    pub window_from_ms: i64,
    pub window_to_ms: i64,
    pub holding_ms: i64,
    pub max_exit_delay_ms: i64,
    pub decision_delay_ms: i64,
    pub snapshot_sha256: String,
    pub quotes_sha256: String,
    pub spec_sha256: String,
    pub quote_keys: u32,
    pub quote_budget_truncated: bool,
    pub runtime: ObservationRuntimeBindingV1,
}
impl ObservationTargetV1 {
    pub fn validate(&self) -> bool {
        self.version == 1
            && valid_hash(&self.source_task_id)
            && self.source_revision == 1
            && valid_hash(&self.source_candidate_id)
            && valid_hash(&self.source_lifecycle_candidate_id)
            && valid_hash(&self.source_result_sha256)
            && valid_hash(&self.source_runtime_hash)
            && self.authoring_at_ms >= 0
            && self.authoring_at_ms <= self.signal_ms
            && valid_hash(&self.policy_sha256)
            && self.signal_ms >= 0
            && self.target_frozen_at_ms == self.signal_ms
            && self.window_from_ms >= 0
            && self.window_from_ms <= self.window_to_ms
            && self.window_to_ms <= self.target_frozen_at_ms
            && (1..=86_400_000).contains(&self.holding_ms)
            && (0..=60_000).contains(&self.max_exit_delay_ms)
            && (0..=60_000).contains(&self.decision_delay_ms)
            && valid_hash(&self.snapshot_sha256)
            && valid_hash(&self.quotes_sha256)
            && valid_hash(&self.spec_sha256)
            && (1..=10_000).contains(&self.quote_keys)
            && self.runtime.validate()
    }
}
/// Optional candidate purpose. `None`/omitted keeps every historical authoring
/// task byte-for-byte unchanged; `Observe` is the only variant and binds the
/// frozen prospective target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CandidatePurposeV1 {
    Observe { target: Box<ObservationTargetV1> },
}
/// Typed cross-session parent reference. It is a read-only provenance pointer to
/// an original terminal candidate, never a lease, queue row or launch request.
/// The source book binding is the predecessor's own recorded binding; it is not
/// compared to the successor's per-session `book_binding`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateParentRefV1 {
    pub version: u32,
    pub source_session: String,
    pub task_id: String,
    pub revision: u32,
    pub candidate_id: String,
    pub lifecycle_candidate_id: String,
    pub source_book_binding: String,
    pub source_state_hash: String,
    pub carry_sha256: String,
    pub account: String,
    pub instrument_universe_sha256: String,
    pub cohort_key: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_hash: String,
    pub content_key: String,
    pub result_sha256: String,
    pub runtime_hash: String,
    pub result_kind: String,
    pub axes: Vec<String>,
}
impl CandidateParentRefV1 {
    pub fn validate(&self) -> bool {
        self.version == 1
            && chrono::NaiveDate::parse_from_str(&self.source_session, "%Y-%m-%d").is_ok()
            && valid_hash(&self.task_id)
            && self.revision == 1
            && valid_hash(&self.candidate_id)
            && valid_hash(&self.lifecycle_candidate_id)
            && valid_hash(&self.source_book_binding)
            && valid_hash(&self.source_state_hash)
            && valid_hash(&self.carry_sha256)
            && !self.account.trim().is_empty()
            && self.account.len() <= 64
            && !self.account.chars().any(char::is_control)
            && valid_hash(&self.instrument_universe_sha256)
            && valid_hash(&self.cohort_key)
            && valid_hash(&self.source_sha256)
            && valid_hash(&self.params_sha256)
            && valid_hash(&self.dataset_hash)
            && valid_hash(&self.content_key)
            && valid_hash(&self.result_sha256)
            && valid_hash(&self.runtime_hash)
            && !self.result_kind.is_empty()
            && self.result_kind.len() <= 64
            && self.axes.len() <= 16
            && self.axes.iter().all(|axis| {
                !axis.is_empty() && axis.len() <= 64 && !axis.chars().any(char::is_control)
            })
    }
}
/// One immutable candidate version bound to one durable queue attempt. A new
/// source version is a new content-addressed task with an explicit parent; a
/// task identity never changes its inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateTaskV1 {
    pub version: u32,
    pub task_id: String,
    pub revision: u32,
    pub task_key: String,
    pub job_id: String,
    pub candidate_id: String,
    pub lifecycle_candidate_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_lifecycle_candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    pub parent_turn_id: String,
    pub session: String,
    pub book_binding: String,
    pub policy_sha256: String,
    pub model_id: String,
    pub model_revision: u64,
    pub hypothesis: String,
    pub source_sha256: String,
    pub params_sha256: String,
    pub dataset_id: String,
    pub dataset_hash: String,
    pub content_key: String,
    /// Admission allowance identity over the frozen computation input only.
    /// Hypothesis prose, task keys and parent links never reset this budget.
    pub input_key: String,
    pub windows: CandidateWindows,
    pub horizon_minutes: i64,
    /// Declared maximum exit delay for this candidate's own episode. It is part
    /// of the lifecycle identity and is propagated to the experiment spec,
    /// maturation deadline and result verification. Omission preserves the
    /// historical 60s for every pre-existing row.
    #[serde(
        default = "default_max_exit_delay_ms",
        skip_serializing_if = "is_default_max_exit_delay_ms"
    )]
    pub max_exit_delay_ms: i64,
    pub abi: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_evidence: Option<OwnerEvidence>,
    /// Present only when the reviewed execution capability was granted and the
    /// submitter requested the after-cost cash replay. The typed rules remain a
    /// CLI artifact; this binds their exact bytes without adding store-side
    /// execution state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<CandidateExecutionBinding>,
    /// Optional prospective-observation purpose. Omitted keeps every historical
    /// authoring task byte identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<CandidatePurposeV1>,
    /// Optional read-only cross-session parent provenance. Mutually exclusive
    /// with the same-session `parent_task_id` chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ref: Option<CandidateParentRefV1>,
    pub runtime_binding: CandidateRuntimeBinding,
    pub sandbox: CandidateSandboxPolicy,
    pub unit_policy: CandidateUnitPolicy,
    pub resources: ResourceCaps,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateTaskRecord {
    pub spec: CandidateTaskV1,
    pub status: TaskState,
    pub job: Job,
    pub cancel_receipt: Option<CancelReceipt>,
    pub refusal_receipt: Option<RefusalReceipt>,
    pub terminal_at_ms: Option<i64>,
}
pub enum CandidateAdmission {
    Admitted(Box<CandidateTaskRecord>),
    Deferred { reason: &'static str, trials: u32 },
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
        if self.capability == SpecCapability::CandidateSource
            && (self.retry_safe_compute || self.max_attempts != 1 || self.output_bytes > 1_048_576)
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
