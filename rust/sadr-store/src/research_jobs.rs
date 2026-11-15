//! Durable retry-safe compute jobs. Separate from financial/model allowance journals.
//! No execution, credentials, network, automatic retries or strategy promotion.
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
}
impl CandidateSandboxPolicy {
    fn validate(&self) -> bool {
        self.abi == "scorer_replay_v1"
            && self.engine == "sadr_sandbox::score_source"
            && !self.network
            && (1..=30).contains(&self.cpu_seconds)
            && (32..=512).contains(&self.memory_mb)
            && (1..=30_000).contains(&self.wall_ms)
            && (1..=262_144).contains(&self.max_output_bytes)
            && (1..=4096).contains(&self.max_keys)
            && (8..=256).contains(&self.max_processes)
            && (16..=128).contains(&self.max_open_files)
            && self.executable_scope == "python_and_bwrap_content_only"
            && valid_hash(&self.python_sha256)
            && valid_hash(&self.bwrap_sha256)
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
pub fn scoped_task_id(book_binding: &str, session: &str, task_key: &str) -> String {
    sha256_hex(format!("owner-research-task-v1:{book_binding}:{session}:{task_key}").as_bytes())
}
pub fn scoped_job_id(task_id: &str, revision: u32) -> String {
    sha256_hex(format!("owner-research-job-v1:{task_id}:{revision}").as_bytes())
}
/// Fingerprint of the actual scorer engine sources plus the fixed helper and
/// owned-unit containment policy. This is the runtime identity a candidate job
/// pins; the string labels the convention and the hashes carry the provenance.
pub fn candidate_runtime_hash(
    scorer_compare_sha256: &str,
    sandbox_source_sha256: &str,
    helper_sha256: &str,
    unit_policy_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-runtime-source-v2\0scorer_replay_v1\0sadr_sandbox::score_source\0{scorer_compare_sha256}\0{sandbox_source_sha256}\0{helper_sha256}\0{unit_policy_sha256}"
        )
        .as_bytes(),
    )
}
pub fn candidate_input_hash(params_sha256: &str, dataset_hash: &str) -> String {
    sha256_hex(format!("candidate-input-v1\0{params_sha256}\0{dataset_hash}").as_bytes())
}
/// Execution-only runtime identity over the recorded scorer runtime binding and
/// the exact pinned sandbox policy, including the actual Python/bwrap content
/// hashes and every containment cap. The ranking runtime hash does not cover the
/// interpreter/bwrap bytes or the sandbox caps, so an executable upgrade under
/// the same helper could otherwise reuse an old execution version. This hash is
/// deliberately used only by execution-aware identities; ranking-only task
/// bytes remain unchanged.
pub fn candidate_execution_runtime_hash(
    runtime: &CandidateRuntimeBinding,
    sandbox: &CandidateSandboxPolicy,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-runtime-v1\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            runtime.kind,
            runtime.abi,
            runtime.engine,
            runtime.scorer_compare_sha256,
            runtime.sandbox_source_sha256,
            runtime.helper_sha256,
            runtime.unit_policy_sha256,
            runtime.runtime_hash,
            sandbox.abi,
            sandbox.engine,
            sandbox.network,
            sandbox.cpu_seconds,
            sandbox.memory_mb,
            sandbox.wall_ms,
            sandbox.max_output_bytes,
            sandbox.max_keys,
            sandbox.max_processes,
            sandbox.max_open_files,
            sandbox.executable_scope,
            sandbox.python_sha256,
            sandbox.bwrap_sha256
        )
        .as_bytes(),
    )
}
/// Execution-aware job input identity. The scorer input alone is insufficient:
/// the declared execution rules, the exact frozen manifest/events tape and the
/// actual sandbox runtime determine what the helper may replay, so they are part
/// of the same immutable input allowance. Hypothesis prose, task keys and parent
/// links are excluded.
pub fn candidate_execution_input_hash(
    params_sha256: &str,
    dataset_hash: &str,
    execution: &CandidateExecutionBinding,
    execution_runtime_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-input-v2\0{params_sha256}\0{dataset_hash}\0{}\0{}\0{}\0{}\0{execution_runtime_sha256}",
            execution.abi,
            execution
                .rules_sha256
                .as_deref()
                .unwrap_or(&execution.spec_sha256),
            execution.manifest_sha256,
            execution.events_sha256
        )
        .as_bytes(),
    )
}
/// Content identity excludes the parent and hypothesis prose so an identical
/// resubmission is detectable even when a caller names a different parent or
/// rewrites the description. Declared windows, horizon and ABI are part of the
/// immutable version identity because they change the replay declaration.
pub fn candidate_content_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    windows: &CandidateWindows,
    horizon_minutes: i64,
    abi: &str,
    unit_policy_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-content-v3\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{}\0{}\0{}\0{}\0{horizon_minutes}\0{abi}\0{unit_policy_sha256}",
            windows.training_from_ms,
            windows.training_to_ms,
            windows.selection_from_ms,
            windows.selection_to_ms,
        )
        .as_bytes(),
    )
}
/// Execution-aware content identity. The execution mode, rules, frozen tape and
/// actual sandbox runtime are part of the version so a ranking-only task, an
/// executable task over the same scorer inputs, and an executable task after a
/// Python/bwrap or containment-cap change never collide.
#[allow(clippy::too_many_arguments)]
pub fn candidate_execution_content_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    windows: &CandidateWindows,
    horizon_minutes: i64,
    abi: &str,
    unit_policy_sha256: &str,
    execution: &CandidateExecutionBinding,
    execution_runtime_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-content-v2\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{}\0{}\0{}\0{}\0{horizon_minutes}\0{abi}\0{unit_policy_sha256}\0{}\0{}\0{}\0{}\0{execution_runtime_sha256}",
            windows.training_from_ms,
            windows.training_to_ms,
            windows.selection_from_ms,
            windows.selection_to_ms,
            execution.abi,
            execution.spec_sha256,
            execution.manifest_sha256,
            execution.events_sha256,
        )
        .as_bytes(),
    )
}
/// Admission allowance identity over the frozen computation input. Variants
/// that only change prose, task key, parent, windows or horizon still spend the
/// same input allowance; the dataset hash already binds the frozen snapshots.
pub fn candidate_trial_input_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    abi: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-trial-input-v1\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{abi}"
        )
        .as_bytes(),
    )
}
/// Execution-aware admission allowance identity. Changing only the declared
/// execution rules, the frozen tape or the actual sandbox runtime is a new
/// computation input and therefore spends a new allowance; prose and parent
/// links still do not reset it.
pub fn candidate_execution_trial_input_key(
    source_sha256: &str,
    params_sha256: &str,
    dataset_hash: &str,
    abi: &str,
    execution: &CandidateExecutionBinding,
    execution_runtime_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-execution-trial-input-v2\0{source_sha256}\0{params_sha256}\0{dataset_hash}\0{abi}\0{}\0{}\0{}\0{}\0{execution_runtime_sha256}",
            execution.abi,
            execution.rules_sha256.as_deref().unwrap_or(&execution.spec_sha256),
            execution.manifest_sha256,
            execution.events_sha256
        )
        .as_bytes(),
    )
}
/// Prospective-observation content identity. The original source/params, the
/// frozen feature snapshot, the frozen research-quote tape, the declared
/// observation rules, the recorded runtime/helper/caps and the observation ABI
/// are all part of the immutable version. Prose, task keys and parent metadata
/// are excluded so rewriting them cannot reset compute debt.
#[allow(clippy::too_many_arguments)]
pub fn candidate_observation_content_key(
    source_sha256: &str,
    params_sha256: &str,
    snapshot_sha256: &str,
    quotes_sha256: &str,
    spec_sha256: &str,
    window_from_ms: i64,
    window_to_ms: i64,
    holding_ms: i64,
    max_exit_delay_ms: i64,
    runtime_hash: &str,
    abi: &str,
    unit_policy_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-observation-content-v1\0{source_sha256}\0{params_sha256}\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}\0{window_from_ms}\0{window_to_ms}\0{holding_ms}\0{max_exit_delay_ms}\0{runtime_hash}\0{abi}\0{unit_policy_sha256}"
        )
        .as_bytes(),
    )
}
/// Prospective-observation admission allowance identity. A new snapshot, quote
/// tape, spec or runtime is a new computation input and spends a new allowance;
/// prose, task key and parent links do not.
#[allow(clippy::too_many_arguments)]
pub fn candidate_observation_trial_input_key(
    source_sha256: &str,
    params_sha256: &str,
    snapshot_sha256: &str,
    quotes_sha256: &str,
    spec_sha256: &str,
    runtime_hash: &str,
    abi: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-observation-trial-input-v1\0{source_sha256}\0{params_sha256}\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}\0{runtime_hash}\0{abi}"
        )
        .as_bytes(),
    )
}
/// Observation-aware job input hash. It binds the exact snapshot, quote tape,
/// spec and runtime the helper may read; the original params are already part
/// of the job script identity.
#[allow(clippy::too_many_arguments)]
pub fn candidate_observation_input_hash(
    params_sha256: &str,
    snapshot_sha256: &str,
    quotes_sha256: &str,
    spec_sha256: &str,
    runtime_hash: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-observation-input-v1\0{params_sha256}\0{snapshot_sha256}\0{quotes_sha256}\0{spec_sha256}\0{runtime_hash}"
        )
        .as_bytes(),
    )
}
/// Stable cohort identity used by cross-session continuation and measurement.
/// It deliberately excludes the per-session `book_binding` (which digests the
/// session date and directory) and any snapshot hash, so a new session with a
/// new future snapshot is still the same account/feed/instrument/runtime cohort.
#[allow(clippy::too_many_arguments)]
pub fn candidate_cohort_key(
    account: &str,
    instrument_universe_sha256: &str,
    feed: &str,
    dataset_abi: &str,
    lineage: &str,
    runtime_hash: &str,
    helper_sha256: &str,
    unit_policy_sha256: &str,
    scorer_compare_sha256: &str,
    sandbox_source_sha256: &str,
) -> String {
    sha256_hex(
        format!(
            "candidate-cohort-v1\0{account}\0{instrument_universe_sha256}\0{feed}\0{dataset_abi}\0{lineage}\0{runtime_hash}\0{helper_sha256}\0{unit_policy_sha256}\0{scorer_compare_sha256}\0{sandbox_source_sha256}"
        )
        .as_bytes(),
    )
}
/// Registered input allowance identity. Domain-prefixed so a registered input
/// and a candidate input never share one allowance; total session admission
/// still counts the union of both tables.
pub fn registered_trial_input_key(
    code_sha256: &str,
    runtime_sha256: &str,
    input_sha256: &str,
) -> String {
    sha256_hex(
        format!("registered-trial-input-v1\0{code_sha256}\0{runtime_sha256}\0{input_sha256}")
            .as_bytes(),
    )
}
/// Immutable candidate version identity. It is distinct from the queue task
/// identity even though both bind the same content and parent.
pub fn candidate_version_id(
    book_binding: &str,
    session: &str,
    task_key: &str,
    content_key: &str,
    parent_candidate_id: Option<&str>,
) -> String {
    sha256_hex(
        format!(
            "candidate-version-v1\0{book_binding}\0{session}\0{task_key}\0{content_key}\0{}",
            parent_candidate_id.unwrap_or("")
        )
        .as_bytes(),
    )
}
pub fn candidate_task_id(
    book_binding: &str,
    session: &str,
    task_key: &str,
    content_key: &str,
    parent_candidate_id: Option<&str>,
) -> String {
    sha256_hex(
        format!(
            "candidate-task-v1\0{book_binding}\0{session}\0{task_key}\0{content_key}\0{}",
            parent_candidate_id.unwrap_or("")
        )
        .as_bytes(),
    )
}
pub fn candidate_job_id(task_id: &str, revision: u32) -> String {
    sha256_hex(format!("candidate-job-v1\0{task_id}\0{revision}").as_bytes())
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
            || job.capability != SpecCapability::RegisteredAnalysis
            || self.revision == 0
            || !valid_slug(&self.task_key, 48)
            || !valid_text(&self.parent_turn_id, 256)
            || chrono::NaiveDate::parse_from_str(&self.session, "%Y-%m-%d").is_err()
            || !valid_hash(&self.book_binding)
            || !valid_hash(&self.policy_sha256)
            || !valid_text(&self.model_id, 128)
            || !valid_text(&self.goal, 1024)
            || self.capability != Capability::RegisteredAnalysis
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

fn ensure_scoped_tables(tx: &Transaction<'_>) -> Result<()> {
    let core: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('scripts','jobs','leases','job_events')",
        [],
        |row| row.get(0),
    ).map_err(|_| Error::Storage)?;
    if core != 4 {
        return Err(Error::Storage);
    }
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    let tables: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('scoped_tasks','scoped_task_events')",
        [],
        |r| r.get(0),
    ).map_err(|_| Error::Storage)?;
    match (version, tables) {
        (0, 0) => {}
        (1, 2) => return Ok(()),
        _ => return Err(Error::Storage),
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS scoped_tasks(task_id TEXT NOT NULL,revision INTEGER NOT NULL,job_id TEXT NOT NULL UNIQUE,session TEXT NOT NULL,parent_turn_id TEXT NOT NULL,body TEXT NOT NULL,created_ms INTEGER NOT NULL,cancel_receipt TEXT,refusal_receipt TEXT,PRIMARY KEY(task_id,revision));\
         CREATE TABLE IF NOT EXISTS scoped_task_events(seq INTEGER PRIMARY KEY,task_id TEXT NOT NULL,revision INTEGER NOT NULL,event TEXT NOT NULL,at_ms INTEGER NOT NULL);",
    ).map_err(|_| Error::Storage)?;
    tx.pragma_update(None, "user_version", 1_i64)
        .map_err(|_| Error::Storage)?;
    Ok(())
}
fn scoped_tables_exist(tx: &Transaction<'_>) -> Result<bool> {
    let version: i64 = tx
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| Error::Storage)?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('scoped_tasks','scoped_task_events')",
        [],
        |r| r.get(0),
    ).map_err(|_| Error::Storage)?;
    match (version, count) {
        (0, 0) => Ok(false),
        (1, 2) => Ok(true),
        _ => Err(Error::Storage),
    }
}

fn task_state(state: State) -> TaskState {
    match state {
        State::Pending => TaskState::Pending,
        State::Running => TaskState::Running,
        State::Unknown => TaskState::Unknown,
        State::CancelRequested => TaskState::CancelRequested,
        State::Terminated => TaskState::Terminated,
        State::Succeeded => TaskState::Succeeded,
        State::Cancelled => TaskState::Cancelled,
        State::Refused => TaskState::Refused,
    }
}

fn scoped_tx(tx: &Transaction<'_>, task_id: &str, revision: u32) -> Result<Option<TaskRecord>> {
    if !valid_hash(task_id) || revision == 0 {
        return Err(Error::Invalid);
    }
    type ScopedRow = (String, Option<String>, Option<String>, String, String, i64);
    let row: Option<ScopedRow> = tx.query_row(
        "SELECT body,cancel_receipt,refusal_receipt,session,parent_turn_id,created_ms FROM scoped_tasks WHERE task_id=? AND revision=? AND length(body)<=16384",
        params![task_id, revision],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
    ).optional().map_err(|_| Error::Storage)?;
    let Some((body, cancel, refusal, session, parent_turn_id, created_ms)) = row else {
        return Ok(None);
    };
    let spec: ScopedTaskSpec = serde_json::from_str(&body).map_err(|_| Error::Invalid)?;
    if spec.task_id != task_id || spec.revision != revision {
        return Err(Error::Invalid);
    }
    let job = read(tx, &spec.job_id)?.ok_or(Error::Missing)?;
    spec.validate(&job.spec, created_ms)
        .map_err(|_| Error::Invalid)?;
    if spec.session != session || spec.parent_turn_id != parent_turn_id {
        return Err(Error::Invalid);
    }
    let cancel_receipt: Option<CancelReceipt> = cancel
        .map(|body| serde_json::from_str(&body).map_err(|_| Error::Invalid))
        .transpose()?;
    let refusal_receipt: Option<RefusalReceipt> = refusal
        .map(|body| serde_json::from_str(&body).map_err(|_| Error::Invalid))
        .transpose()?;
    if cancel_receipt.as_ref().is_some_and(|r| {
        r.task_id != task_id
            || r.revision != revision
            || r.expected_revision != revision
            || r.lease != job.lease
    }) || refusal_receipt.as_ref().is_some_and(|r| {
        r.task_id != task_id
            || r.revision != revision
            || r.job_id != spec.job_id
            || job.state != State::Refused
            || job.attempts != 0
    }) || (cancel_receipt.is_some() && refusal_receipt.is_some())
    {
        return Err(Error::Invalid);
    }
    if (job.state == State::Refused) != refusal_receipt.is_some() {
        return Err(Error::Invalid);
    }
    let status = task_state(job.state);
    let terminal_at_ms = if matches!(
        status,
        TaskState::Succeeded | TaskState::Terminated | TaskState::Cancelled | TaskState::Refused
    ) {
        tx.query_row(
            "SELECT max(at_ms) FROM (SELECT at_ms FROM job_events WHERE job=? AND (event='completed' OR event LIKE 'terminated:%') UNION ALL SELECT at_ms FROM scoped_task_events WHERE task_id=? AND revision=? AND event IN ('cancelled','refused'))",
            params![spec.job_id, task_id, revision],
            |r| r.get(0),
        ).optional().map_err(|_| Error::Storage)?.flatten()
    } else {
        None
    };
    Ok(Some(TaskRecord {
        spec,
        status,
        job,
        cancel_receipt,
        refusal_receipt,
        terminal_at_ms,
    }))
}
fn scoped_event(
    tx: &Transaction<'_>,
    task_id: &str,
    revision: u32,
    name: &str,
    now: i64,
) -> Result<()> {
    let last: Option<i64> = tx
        .query_row(
            "SELECT max(at_ms) FROM scoped_task_events WHERE task_id=? AND revision=?",
            params![task_id, revision],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if last.is_some_and(|last| now < last) {
        return Err(Error::Invalid);
    }
    tx.execute(
        "INSERT INTO scoped_task_events(task_id,revision,event,at_ms) VALUES(?,?,?,?)",
        params![task_id, revision, name, now],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}

/// Atomically create a scoped task and its existing compute job in the
/// caller-validated, already initialized queue. This performs only additive
/// schema creation; it never initializes, replaces, or resets the queue.
pub fn enqueue_scoped(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &ScopedTaskSpec,
    now: i64,
) -> Result<TaskRecord> {
    match enqueue_scoped_bounded(db, job_spec, task_spec, now, None)? {
        Admission::Admitted(record) => Ok(*record),
        Admission::Deferred { .. } => Err(Error::Capacity),
    }
}
/// Check compute limits and enqueue in the same immediate transaction. Aliases,
/// task keys, goals and owner turns cannot evade a content-bound input allowance.
pub fn enqueue_scoped_bounded(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &ScopedTaskSpec,
    now: i64,
    limits: Option<&AdmissionLimits>,
) -> Result<Admission> {
    if let Some(limits) = limits {
        limits.validate()?;
    }
    task_spec.validate(job_spec, task_spec.created_at_ms)?;
    if now < task_spec.created_at_ms {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_scoped_tables(&tx)?;
    let body = serde_json::to_string(task_spec).map_err(|_| Error::Invalid)?;
    if let Some(old) = scoped_tx(&tx, &task_spec.task_id, task_spec.revision)? {
        if old.spec == *task_spec && old.job.spec == *job_spec {
            return Ok(Admission::Admitted(Box::new(old)));
        }
        return Err(Error::Collision);
    }
    if task_spec.created_at_ms != now {
        return Err(Error::Invalid);
    }
    let latest: Option<u32> = tx
        .query_row(
            "SELECT max(revision) FROM scoped_tasks WHERE task_id=?",
            [&task_spec.task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten();
    match latest {
        None if task_spec.revision != 1 => return Err(Error::State),
        Some(revision) => {
            let previous = scoped_tx(&tx, &task_spec.task_id, revision)?.ok_or(Error::Missing)?;
            if previous.spec.task_key != task_spec.task_key
                || previous.spec.session != task_spec.session
                || previous.spec.book_binding != task_spec.book_binding
            {
                return Err(Error::Collision);
            }
            if !matches!(
                previous.status,
                TaskState::Succeeded
                    | TaskState::Terminated
                    | TaskState::Cancelled
                    | TaskState::Refused
            ) || task_spec.revision != revision.saturating_add(1)
            {
                return Err(Error::State);
            }
        }
        None => {}
    }
    if let Some(limits) = limits {
        tx.execute_batch("CREATE INDEX IF NOT EXISTS scoped_tasks_session ON scoped_tasks(session); CREATE TABLE IF NOT EXISTS scoped_admission_deferrals(task_id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(task_id,revision));").map_err(|_| Error::Storage)?;
        let (registered_total, registered_same): (u32, u32) = tx.query_row(
            "SELECT count(*),coalesce(sum(json_extract(body,'$.registered_analysis.code_sha256')=?3 AND json_extract(body,'$.registered_analysis.runtime_sha256')=?4 AND json_extract(body,'$.registered_analysis.input_sha256')=?5),0) FROM scoped_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2",
            params![task_spec.session, task_spec.book_binding, task_spec.registered_analysis.code_sha256, task_spec.registered_analysis.runtime_sha256, task_spec.registered_analysis.input_sha256],
            |r| Ok((r.get(0)?,r.get(1)?))).map_err(|_| Error::Storage)?;
        // Session admission counts both task families. Per-input admission is
        // family-scoped: a registered input key is domain-prefixed and never
        // matches a candidate input key, so no cross-domain same-input query is
        // meaningful here.
        let candidate_total: u32 = if candidate_tables_exist(&tx)? {
            tx.query_row(
                "SELECT count(*) FROM candidate_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2",
                params![task_spec.session, task_spec.book_binding],
                |r| r.get(0),
            )
            .map_err(|_| Error::Storage)?
        } else {
            0
        };
        let total = registered_total.saturating_add(candidate_total);
        let same = registered_same;
        let deferred = if total >= limits.max_session_trials {
            Some(("research_session_compute_budget_exhausted", total))
        } else if same >= limits.max_trials_per_input {
            Some(("research_unchanged_input_budget_exhausted", same))
        } else {
            None
        };
        if let Some((reason, trials)) = deferred {
            let body = serde_json::to_string(&serde_json::json!({"reason":reason,"trials":trials,"requested":task_spec,"observed_ms":now,"next_condition":if total >= limits.max_session_trials {"next_authorized_session"}else{"new_registered_content_or_reviewed_compute_policy"}})).map_err(|_| Error::Invalid)?;
            tx.execute("INSERT OR IGNORE INTO scoped_admission_deferrals(task_id,revision,body) VALUES(?,?,?)",params![task_spec.task_id,task_spec.revision,body]).map_err(|_| Error::Storage)?;
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(Admission::Deferred { reason, trials });
        }
    }
    if read(&tx, &job_spec.id)?.is_some() {
        return Err(Error::Collision);
    }
    let job = enqueue_tx(&tx, job_spec, now)?;
    tx.execute(
        "INSERT INTO scoped_tasks(task_id,revision,job_id,session,parent_turn_id,body,created_ms) VALUES(?,?,?,?,?,?,?)",
        params![task_spec.task_id, task_spec.revision, task_spec.job_id, task_spec.session, task_spec.parent_turn_id, body, now],
    ).map_err(|_| Error::Collision)?;
    scoped_event(&tx, &task_spec.task_id, task_spec.revision, "enqueued", now)?;
    let record = scoped_tx(&tx, &task_spec.task_id, task_spec.revision)?.ok_or(Error::Missing)?;
    if record.job != job {
        return Err(Error::Storage);
    }
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(Admission::Admitted(Box::new(record)))
}

/// Load one task revision. Missing or malformed state is never treated as a
/// new request; callers must surface the refusal.
pub fn load_scoped(db: &mut Connection, task_id: &str, revision: u32) -> Result<TaskRecord> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Err(Error::Missing);
    }
    let result = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing);
    tx.commit().map_err(|_| Error::Storage)?;
    result
}

pub fn latest_scoped(db: &mut Connection, task_id: &str) -> Result<Option<TaskRecord>> {
    if !valid_hash(task_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let revision: Option<u32> = tx
        .query_row(
            "SELECT max(revision) FROM scoped_tasks WHERE task_id=?",
            [task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten();
    let result = revision
        .map(|revision| scoped_tx(&tx, task_id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn scoped_for_job(db: &mut Connection, job_id: &str) -> Result<Option<TaskRecord>> {
    if !valid_hash(job_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let key: Option<(String, u32)> = tx
        .query_row(
            "SELECT task_id,revision FROM scoped_tasks WHERE job_id=?",
            [job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let result = key
        .map(|(id, revision)| scoped_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn list_scoped(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    limit: usize,
) -> Result<Vec<TaskRecord>> {
    list_scoped_page(db, session, parent_turn_id, None, limit)
}

pub fn list_scoped_page(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<TaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !(1..=100).contains(&limit)
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(
        "SELECT task_id,revision FROM scoped_tasks WHERE session=? AND (? IS NULL OR parent_turn_id=?) AND (? IS NULL OR created_ms>? OR (created_ms=? AND (task_id>? OR (task_id=? AND revision>?)))) ORDER BY created_ms,task_id,revision LIMIT ?",
    ).map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                session,
                parent_turn_id,
                parent_turn_id,
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| scoped_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

pub fn active_scoped(db: &mut Connection) -> Result<Option<TaskRecord>> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !scoped_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let key: Option<(String, u32)> = tx.query_row(
        "SELECT t.task_id,t.revision FROM scoped_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state IN ('Pending','Running','Unknown','CancelRequested') ORDER BY CASE j.state WHEN 'Running' THEN 0 WHEN 'Unknown' THEN 1 WHEN 'CancelRequested' THEN 2 ELSE 3 END,t.created_ms,t.task_id,t.revision LIMIT 1",
        [],
        |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(|_| Error::Storage)?;
    let record = key
        .map(|(id, revision)| scoped_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(record)
}

pub fn cancel_scoped(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    expected_revision: u32,
    now: i64,
) -> Result<TaskRecord> {
    if revision != expected_revision || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_scoped_tables(&tx)?;
    let mut old = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    if let Some(receipt) = &old.cancel_receipt {
        if receipt.expected_revision == expected_revision {
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(old);
        }
        return Err(Error::Collision);
    }
    if old.refusal_receipt.is_some()
        || matches!(
            old.status,
            TaskState::Succeeded | TaskState::Terminated | TaskState::Cancelled
        )
    {
        return Err(Error::State);
    }
    let next = match old.job.state {
        State::Pending => "Cancelled",
        State::Running | State::Unknown => "CancelRequested",
        State::CancelRequested => "CancelRequested",
        _ => return Err(Error::State),
    };
    if old.job.state != State::CancelRequested {
        tx.execute(
            "UPDATE jobs SET state=? WHERE id=?",
            params![next, old.spec.job_id],
        )
        .map_err(|_| Error::Storage)?;
        event(
            &tx,
            &old.spec.job_id,
            "cancel_requested",
            now,
            old.job.attempts,
        )?;
    }
    let receipt = CancelReceipt {
        task_id: old.spec.task_id.clone(),
        revision,
        expected_revision,
        requested_ms: now,
        lease: old.job.lease.clone(),
        job_state: next.to_owned(),
    };
    tx.execute(
        "UPDATE scoped_tasks SET cancel_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    scoped_event(&tx, task_id, revision, "cancel_requested", now)?;
    if next == "Cancelled" {
        scoped_event(&tx, task_id, revision, "cancelled", now)?;
    }
    old = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(old)
}

pub fn refuse_unstarted(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    reason: RefusalReason,
    now: i64,
) -> Result<TaskRecord> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_scoped_tables(&tx)?;
    let old = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    if let Some(receipt) = &old.refusal_receipt {
        return if receipt.reason == reason {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    if old.job.state != State::Pending || old.job.attempts != 0 || old.cancel_receipt.is_some() {
        return Err(Error::State);
    }
    let receipt = RefusalReceipt {
        task_id: old.spec.task_id.clone(),
        revision,
        job_id: old.spec.job_id.clone(),
        reason,
        at_ms: now,
    };
    tx.execute(
        "UPDATE scoped_tasks SET refusal_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    tx.execute(
        "UPDATE jobs SET state='Refused' WHERE id=? AND state='Pending' AND attempts=0",
        [&old.spec.job_id],
    )
    .map_err(|_| Error::Storage)?;
    scoped_event(&tx, task_id, revision, "refused", now)?;
    let result = scoped_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}
impl CandidateTaskV1 {
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
        let execution_valid = self
            .execution
            .as_ref()
            .is_none_or(CandidateExecutionBinding::validate);
        let purpose_valid = match &self.purpose {
            None => true,
            Some(CandidatePurposeV1::Observe { target }) => {
                target.validate()
                    && self.execution.is_none()
                    && target.snapshot_sha256 == self.dataset_hash
                    && target.runtime.source_sha256 == self.source_sha256
                    && target.runtime.params_sha256 == self.params_sha256
                    && target.runtime.runtime_hash == self.runtime_binding.runtime_hash
                    && target.runtime.helper_sha256 == self.runtime_binding.helper_sha256
                    && target.runtime.unit_policy_sha256 == self.runtime_binding.unit_policy_sha256
                    && target.runtime.scorer_compare_sha256
                        == self.runtime_binding.scorer_compare_sha256
                    && target.runtime.sandbox_source_sha256
                        == self.runtime_binding.sandbox_source_sha256
                    && target.holding_ms == self.horizon_minutes.saturating_mul(60_000)
                    && (0..=60_000).contains(&self.max_exit_delay_ms)
                    && target.max_exit_delay_ms == self.max_exit_delay_ms
                    && target.window_from_ms == self.windows.selection_from_ms
                    && target.window_to_ms <= self.windows.selection_to_ms
                    && target.signal_ms == self.created_at_ms
                    && target.target_frozen_at_ms == self.created_at_ms
            }
        };
        let parent_valid = match (&self.parent_ref, &self.parent_task_id) {
            (None, _) => true,
            (Some(parent), None) => {
                // A cross-session parent is a versioned CONTINUATION, not a
                // claim that the new source/params are the same algorithm. The
                // original parent hashes stay in the parent_ref provenance and
                // the new source/params key the actual input debt. A
                // prospective observation still reuses the original algorithm
                // exactly, so its source/params must match the parent.
                parent.validate()
                    && self.parent_candidate_id.as_deref() == Some(parent.candidate_id.as_str())
                    && self.parent_lifecycle_candidate_id.as_deref()
                        == Some(parent.lifecycle_candidate_id.as_str())
                    && parent.runtime_hash == self.runtime_binding.runtime_hash
                    && (self.purpose.is_none()
                        || (parent.source_sha256 == self.source_sha256
                            && parent.params_sha256 == self.params_sha256))
            }
            (Some(_), Some(_)) => false,
        };
        let execution_runtime_sha256 =
            candidate_execution_runtime_hash(&self.runtime_binding, &self.sandbox);
        let observation_keys = match &self.purpose {
            Some(CandidatePurposeV1::Observe { target }) => Some((
                candidate_observation_content_key(
                    &self.source_sha256,
                    &self.params_sha256,
                    &self.dataset_hash,
                    &target.quotes_sha256,
                    &target.spec_sha256,
                    target.window_from_ms,
                    target.window_to_ms,
                    target.holding_ms,
                    target.max_exit_delay_ms,
                    &self.runtime_binding.runtime_hash,
                    &target.runtime.replay_abi,
                    &self.unit_policy.policy_sha256(),
                ),
                candidate_observation_trial_input_key(
                    &self.source_sha256,
                    &self.params_sha256,
                    &self.dataset_hash,
                    &target.quotes_sha256,
                    &target.spec_sha256,
                    &self.runtime_binding.runtime_hash,
                    &target.runtime.replay_abi,
                ),
                candidate_observation_input_hash(
                    &self.params_sha256,
                    &self.dataset_hash,
                    &target.quotes_sha256,
                    &target.spec_sha256,
                    &self.runtime_binding.runtime_hash,
                ),
            )),
            _ => None,
        };
        let (expected_content_key, expected_input_key, expected_job_input_hash) =
            match (&self.execution, observation_keys) {
                (_, Some(keys)) => keys,
                (Some(execution), None) if execution_valid => (
                    candidate_execution_content_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.windows,
                        self.horizon_minutes,
                        &self.abi,
                        &self.unit_policy.policy_sha256(),
                        execution,
                        &execution_runtime_sha256,
                    ),
                    candidate_execution_trial_input_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.abi,
                        execution,
                        &execution_runtime_sha256,
                    ),
                    candidate_execution_input_hash(
                        &self.params_sha256,
                        &self.dataset_hash,
                        execution,
                        &execution_runtime_sha256,
                    ),
                ),
                (Some(_), None) => (String::new(), String::new(), String::new()),
                (None, None) => (
                    candidate_content_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.windows,
                        self.horizon_minutes,
                        &self.abi,
                        &self.unit_policy.policy_sha256(),
                    ),
                    candidate_trial_input_key(
                        &self.source_sha256,
                        &self.params_sha256,
                        &self.dataset_hash,
                        &self.abi,
                    ),
                    candidate_input_hash(&self.params_sha256, &self.dataset_hash),
                ),
            };
        if self.version != 1
            || !execution_valid
            || !purpose_valid
            || !parent_valid
            || !valid_hash(&self.task_id)
            || !valid_hash(&self.job_id)
            || !valid_hash(&self.candidate_id)
            || !valid_hash(&self.lifecycle_candidate_id)
            || !valid_hash(&self.content_key)
            || !valid_hash(&self.input_key)
            || self.revision != 1
            || !valid_slug(&self.task_key, 48)
            || !valid_text(&self.parent_turn_id, 256)
            || chrono::NaiveDate::parse_from_str(&self.session, "%Y-%m-%d").is_err()
            || !valid_hash(&self.book_binding)
            || !valid_hash(&self.policy_sha256)
            || !valid_text(&self.model_id, 128)
            || !valid_text(&self.hypothesis, 1024)
            || !valid_hash(&self.source_sha256)
            || !valid_hash(&self.params_sha256)
            || !valid_hash(&self.dataset_hash)
            || !valid_slug(&self.dataset_id, 64)
            || !self.windows.validate()
            || !(1..=1440).contains(&self.horizon_minutes)
            || self.abi != "scorer_replay_v1"
            || !self.runtime_binding.validate()
            || !self.sandbox.validate()
            || !self
                .unit_policy
                .validate_for(&self.sandbox, &self.resources)
            || self.runtime_binding.helper_sha256 != self.unit_policy.helper_sha256
            || self.runtime_binding.unit_policy_sha256 != self.unit_policy.policy_sha256()
            || self.task_id
                != candidate_task_id(
                    &self.book_binding,
                    &self.session,
                    &self.task_key,
                    &self.content_key,
                    self.parent_candidate_id.as_deref(),
                )
            || self.candidate_id
                != candidate_version_id(
                    &self.book_binding,
                    &self.session,
                    &self.task_key,
                    &self.content_key,
                    self.parent_candidate_id.as_deref(),
                )
            || self.job_id != candidate_job_id(&self.task_id, self.revision)
            || self.job_id != job.id
            || self.content_key != expected_content_key
            || self.input_key != expected_input_key
            || job.capability != SpecCapability::CandidateSource
            || job.script_hash != self.source_sha256
            || job.input_hash != expected_job_input_hash
            || job.runtime_hash != self.runtime_binding.runtime_hash
            || job.retry_safe_compute
            || job.max_attempts != 1
            || caps.memory_bytes != job.memory_bytes
            || caps.output_bytes != job.output_bytes
            || caps.wall_millis != job.wall_millis
            || caps.cpu_millis == 0
            || caps.cpu_millis > caps.wall_millis
            || caps.tasks != 1
            || caps.workspace_bytes != 1_048_576
            || caps.file_bytes != 65_536
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
            || (self.parent_candidate_id.is_some() != self.parent_lifecycle_candidate_id.is_some())
            || (self.parent_ref.is_none()
                && self.purpose.is_none()
                && (self.parent_candidate_id.is_some() != self.parent_task_id.is_some()))
            || (self.parent_ref.is_some() && self.parent_task_id.is_some())
            || self
                .parent_candidate_id
                .as_deref()
                .is_some_and(|id| !valid_hash(id))
            || self
                .parent_lifecycle_candidate_id
                .as_deref()
                .is_some_and(|id| !valid_hash(id))
            || self
                .parent_task_id
                .as_deref()
                .is_some_and(|id| !valid_hash(id))
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

fn ensure_candidate_tables(tx: &Transaction<'_>) -> Result<()> {
    ensure_scoped_tables(tx)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS candidate_tasks(task_id TEXT NOT NULL,revision INTEGER NOT NULL,job_id TEXT NOT NULL UNIQUE,session TEXT NOT NULL,parent_turn_id TEXT NOT NULL,content_key TEXT NOT NULL,input_key TEXT NOT NULL,body TEXT NOT NULL,created_ms INTEGER NOT NULL,cancel_receipt TEXT,refusal_receipt TEXT,PRIMARY KEY(task_id,revision));\
         CREATE INDEX IF NOT EXISTS candidate_tasks_session ON candidate_tasks(session);\
         CREATE INDEX IF NOT EXISTS candidate_tasks_input ON candidate_tasks(session,input_key);\
         CREATE TABLE IF NOT EXISTS candidate_task_events(seq INTEGER PRIMARY KEY,task_id TEXT NOT NULL,revision INTEGER NOT NULL,event TEXT NOT NULL,at_ms INTEGER NOT NULL);\
         CREATE TABLE IF NOT EXISTS candidate_artifacts(hash TEXT PRIMARY KEY,kind TEXT NOT NULL,body BLOB NOT NULL,created_ms INTEGER NOT NULL);",
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}

fn candidate_tables_exist(tx: &Transaction<'_>) -> Result<bool> {
    if !scoped_tables_exist(tx)? {
        return Ok(false);
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name IN ('candidate_tasks','candidate_task_events','candidate_artifacts')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    Ok(count == 3)
}

fn candidate_tx(
    tx: &Transaction<'_>,
    task_id: &str,
    revision: u32,
) -> Result<Option<CandidateTaskRecord>> {
    if !valid_hash(task_id) || revision == 0 {
        return Err(Error::Invalid);
    }
    type CandidateRow = (
        String,
        Option<String>,
        Option<String>,
        String,
        String,
        String,
        String,
        i64,
    );
    let row: Option<CandidateRow> = tx
        .query_row(
            "SELECT body,cancel_receipt,refusal_receipt,session,parent_turn_id,content_key,input_key,created_ms FROM candidate_tasks WHERE task_id=? AND revision=? AND length(body)<=16384",
            params![task_id, revision],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let Some((body, cancel, refusal, session, parent_turn_id, content_key, input_key, created_ms)) =
        row
    else {
        return Ok(None);
    };
    let spec: CandidateTaskV1 = serde_json::from_str(&body).map_err(|_| Error::Invalid)?;
    if spec.task_id != task_id || spec.revision != revision {
        return Err(Error::Invalid);
    }
    let job = read(tx, &spec.job_id)?.ok_or(Error::Missing)?;
    spec.validate(&job.spec, created_ms)
        .map_err(|_| Error::Invalid)?;
    if spec.session != session
        || spec.parent_turn_id != parent_turn_id
        || spec.content_key != content_key
        || spec.input_key != input_key
    {
        return Err(Error::Invalid);
    }
    let cancel_receipt: Option<CancelReceipt> = cancel
        .map(|body| serde_json::from_str(&body).map_err(|_| Error::Invalid))
        .transpose()?;
    let refusal_receipt: Option<RefusalReceipt> = refusal
        .map(|body| serde_json::from_str(&body).map_err(|_| Error::Invalid))
        .transpose()?;
    if cancel_receipt.as_ref().is_some_and(|r| {
        r.task_id != task_id
            || r.revision != revision
            || r.expected_revision != revision
            || r.lease != job.lease
    }) || refusal_receipt.as_ref().is_some_and(|r| {
        r.task_id != task_id
            || r.revision != revision
            || r.job_id != spec.job_id
            || job.state != State::Refused
            || job.attempts != 0
    }) || (cancel_receipt.is_some() && refusal_receipt.is_some())
    {
        return Err(Error::Invalid);
    }
    if (job.state == State::Refused) != refusal_receipt.is_some() {
        return Err(Error::Invalid);
    }
    let status = task_state(job.state);
    let terminal_at_ms = if matches!(
        status,
        TaskState::Succeeded | TaskState::Terminated | TaskState::Cancelled | TaskState::Refused
    ) {
        tx.query_row(
            "SELECT max(at_ms) FROM (SELECT at_ms FROM job_events WHERE job=? AND (event IN ('completed','completed_reconciled') OR event LIKE 'terminated:%') UNION ALL SELECT at_ms FROM candidate_task_events WHERE task_id=? AND revision=? AND event IN ('cancelled','refused'))",
            params![spec.job_id, task_id, revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten()
    } else {
        None
    };
    Ok(Some(CandidateTaskRecord {
        spec,
        status,
        job,
        cancel_receipt,
        refusal_receipt,
        terminal_at_ms,
    }))
}

fn candidate_event(
    tx: &Transaction<'_>,
    task_id: &str,
    revision: u32,
    name: &str,
    now: i64,
) -> Result<()> {
    let last: Option<i64> = tx
        .query_row(
            "SELECT max(at_ms) FROM candidate_task_events WHERE task_id=? AND revision=?",
            params![task_id, revision],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if last.is_some_and(|last| now < last) {
        return Err(Error::Invalid);
    }
    tx.execute(
        "INSERT INTO candidate_task_events(task_id,revision,event,at_ms) VALUES(?,?,?,?)",
        params![task_id, revision, name, now],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}

fn latest_candidate_key_tx(
    tx: &Transaction<'_>,
    session: &str,
    book_binding: &str,
    task_key: &str,
) -> Result<Option<CandidateTaskRecord>> {
    let key: Option<(String, u32)> = tx
        .query_row(
            "SELECT task_id,revision FROM candidate_tasks WHERE session=? AND json_extract(body,'$.book_binding')=? AND json_extract(body,'$.task_key')=? ORDER BY created_ms DESC,rowid DESC LIMIT 1",
            params![session, book_binding, task_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    match key {
        Some((id, revision)) => candidate_tx(tx, &id, revision),
        None => Ok(None),
    }
}

pub fn latest_candidate_for_key(
    db: &mut Connection,
    session: &str,
    book_binding: &str,
    task_key: &str,
) -> Result<Option<CandidateTaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !valid_hash(book_binding)
        || task_key.is_empty()
        || task_key.len() > 48
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let result = latest_candidate_key_tx(&tx, session, book_binding, task_key)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

/// Atomically create a candidate task, its shared compute job and the three
/// immutable input artifacts. Admission is evaluated before any artifact is
/// written, so a deferred or colliding submission leaves no orphan blob.
pub fn enqueue_candidate_bounded(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &CandidateTaskV1,
    artifacts: &[(&str, &[u8])],
    now: i64,
    limits: Option<&AdmissionLimits>,
) -> Result<CandidateAdmission> {
    if let Some(limits) = limits {
        limits.validate()?;
    }
    task_spec.validate(job_spec, task_spec.created_at_ms)?;
    if now < task_spec.created_at_ms {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let body = serde_json::to_string(task_spec).map_err(|_| Error::Invalid)?;
    if let Some(old) = candidate_tx(&tx, &task_spec.task_id, task_spec.revision)? {
        if old.spec == *task_spec && old.job.spec == *job_spec {
            return Ok(CandidateAdmission::Admitted(Box::new(old)));
        }
        return Err(Error::Collision);
    }
    if task_spec.created_at_ms != now {
        return Err(Error::Invalid);
    }
    if let Some(previous) = latest_candidate_key_tx(
        &tx,
        &task_spec.session,
        &task_spec.book_binding,
        &task_spec.task_key,
    )? {
        if previous.spec.content_key == task_spec.content_key {
            return Err(Error::Collision);
        }
        if matches!(
            previous.status,
            TaskState::Pending | TaskState::Unknown | TaskState::CancelRequested
        ) {
            return Err(Error::State);
        }
        if task_spec.parent_candidate_id.as_deref() != Some(previous.spec.candidate_id.as_str())
            || task_spec.parent_task_id.as_deref() != Some(previous.spec.task_id.as_str())
            || task_spec.parent_lifecycle_candidate_id.as_deref()
                != Some(previous.spec.lifecycle_candidate_id.as_str())
        {
            return Err(Error::Collision);
        }
    } else if task_spec.parent_candidate_id.is_some()
        && task_spec.parent_ref.is_none()
        && task_spec.purpose.is_none()
    {
        return Err(Error::State);
    }
    if let Some(limits) = limits {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS scoped_admission_deferrals(task_id TEXT NOT NULL,revision INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(task_id,revision));").map_err(|_| Error::Storage)?;
        let (registered_total, candidate_total): (u32, u32) = tx
            .query_row(
                "SELECT (SELECT count(*) FROM scoped_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2),(SELECT count(*) FROM candidate_tasks WHERE session=?1 AND json_extract(body,'$.book_binding')=?2)",
                params![task_spec.session, task_spec.book_binding],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| Error::Storage)?;
        let total = registered_total.saturating_add(candidate_total);
        let same: u32 = tx
            .query_row(
                "SELECT count(*) FROM candidate_tasks WHERE session=? AND json_extract(body,'$.book_binding')=? AND input_key=?",
                params![task_spec.session, task_spec.book_binding, task_spec.input_key],
                |r| r.get(0),
            )
            .map_err(|_| Error::Storage)?;
        let deferred = if total >= limits.max_session_trials {
            Some(("research_session_compute_budget_exhausted", total))
        } else if same >= limits.max_trials_per_input {
            Some(("research_unchanged_input_budget_exhausted", same))
        } else {
            None
        };
        if let Some((reason, trials)) = deferred {
            let body = serde_json::to_string(&serde_json::json!({"reason":reason,"trials":trials,"requested":task_spec,"observed_ms":now,"next_condition":if total >= limits.max_session_trials {"next_authorized_session"} else {"new_candidate_input_or_reviewed_compute_policy"}})).map_err(|_| Error::Invalid)?;
            tx.execute(
                "INSERT OR IGNORE INTO scoped_admission_deferrals(task_id,revision,body) VALUES(?,?,?)",
                params![task_spec.task_id, task_spec.revision, body],
            )
            .map_err(|_| Error::Storage)?;
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(CandidateAdmission::Deferred { reason, trials });
        }
    }
    if read(&tx, &job_spec.id)?.is_some() {
        return Err(Error::Collision);
    }
    if task_spec.execution.is_some() {
        for kind in ["execution_spec", "execution_manifest", "execution_events"] {
            if !artifacts.iter().any(|(name, _)| *name == kind) {
                return Err(Error::Invalid);
            }
        }
    }
    if let Some(CandidatePurposeV1::Observe { target }) = &task_spec.purpose {
        for kind in ["observation_spec", "observation_quotes"] {
            if !artifacts.iter().any(|(name, _)| *name == kind) {
                return Err(Error::Invalid);
            }
        }
        let _ = target;
    }
    for &(kind, artifact) in artifacts {
        let hash = put_candidate_artifact_tx(&tx, kind, artifact, now)?;
        let expected = match kind {
            "source" => Some(&task_spec.source_sha256),
            "params" => Some(&task_spec.params_sha256),
            "dataset" => Some(&task_spec.dataset_hash),
            "execution_spec" => task_spec
                .execution
                .as_ref()
                .map(|execution| &execution.spec_sha256),
            "execution_manifest" => task_spec
                .execution
                .as_ref()
                .map(|execution| &execution.manifest_sha256),
            "execution_events" => task_spec
                .execution
                .as_ref()
                .map(|execution| &execution.events_sha256),
            "observation_spec" => match &task_spec.purpose {
                Some(CandidatePurposeV1::Observe { target }) => Some(&target.spec_sha256),
                _ => None,
            },
            "observation_quotes" => match &task_spec.purpose {
                Some(CandidatePurposeV1::Observe { target }) => Some(&target.quotes_sha256),
                _ => None,
            },
            _ => None,
        };
        if expected.is_some_and(|expected| expected != &hash) {
            return Err(Error::Invalid);
        }
        if task_spec.execution.is_none()
            && matches!(
                kind,
                "execution_spec" | "execution_manifest" | "execution_events"
            )
        {
            return Err(Error::Invalid);
        }
        if task_spec.purpose.is_none() && matches!(kind, "observation_spec" | "observation_quotes")
        {
            return Err(Error::Invalid);
        }
    }
    let job = enqueue_tx(&tx, job_spec, now)?;
    tx.execute(
        "INSERT INTO candidate_tasks(task_id,revision,job_id,session,parent_turn_id,content_key,input_key,body,created_ms) VALUES(?,?,?,?,?,?,?,?,?)",
        params![
            task_spec.task_id,
            task_spec.revision,
            task_spec.job_id,
            task_spec.session,
            task_spec.parent_turn_id,
            task_spec.content_key,
            task_spec.input_key,
            body,
            now
        ],
    )
    .map_err(|_| Error::Collision)?;
    candidate_event(&tx, &task_spec.task_id, task_spec.revision, "enqueued", now)?;
    let record =
        candidate_tx(&tx, &task_spec.task_id, task_spec.revision)?.ok_or(Error::Missing)?;
    if record.job != job {
        return Err(Error::Storage);
    }
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(CandidateAdmission::Admitted(Box::new(record)))
}

pub fn enqueue_candidate(
    db: &mut Connection,
    job_spec: &Spec,
    task_spec: &CandidateTaskV1,
    now: i64,
) -> Result<CandidateTaskRecord> {
    match enqueue_candidate_bounded(db, job_spec, task_spec, &[], now, None)? {
        CandidateAdmission::Admitted(record) => Ok(*record),
        CandidateAdmission::Deferred { .. } => Err(Error::Capacity),
    }
}

pub fn load_candidate(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
) -> Result<CandidateTaskRecord> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Err(Error::Missing);
    }
    let result = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing);
    tx.commit().map_err(|_| Error::Storage)?;
    result
}

pub fn latest_candidate(db: &mut Connection, task_id: &str) -> Result<Option<CandidateTaskRecord>> {
    if !valid_hash(task_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let revision: Option<u32> = tx
        .query_row(
            "SELECT max(revision) FROM candidate_tasks WHERE task_id=?",
            [task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?
        .flatten();
    let result = revision
        .map(|revision| candidate_tx(&tx, task_id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn list_candidates(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    list_candidates_page(db, session, parent_turn_id, None, limit)
}

pub fn list_candidates_page(
    db: &mut Connection,
    session: &str,
    parent_turn_id: Option<&str>,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !(1..=100).contains(&limit)
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(
        "SELECT task_id,revision FROM candidate_tasks WHERE session=? AND (? IS NULL OR parent_turn_id=?) AND (? IS NULL OR created_ms>? OR (created_ms=? AND (task_id>? OR (task_id=? AND revision>?)))) ORDER BY created_ms,task_id,revision LIMIT ?",
    ).map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                session,
                parent_turn_id,
                parent_turn_id,
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| candidate_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

/// Page candidate rows newest-first. The controller uses this for the true
/// newest-N projection; paging oldest-first and reversing a bounded prefix is
/// not the same thing once the queue exceeds the page bound.
pub fn list_candidates_desc_page(
    db: &mut Connection,
    session: &str,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    if chrono::NaiveDate::parse_from_str(session, "%Y-%m-%d").is_err()
        || !(1..=100).contains(&limit)
    {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx.prepare(
        "SELECT task_id,revision FROM candidate_tasks WHERE session=? AND (? IS NULL OR created_ms<? OR (created_ms=? AND (task_id<? OR (task_id=? AND revision<?)))) ORDER BY created_ms DESC,task_id DESC,revision DESC LIMIT ?",
    ).map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                session,
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| candidate_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

pub fn active_candidate(db: &mut Connection) -> Result<Option<CandidateTaskRecord>> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    // A live attempt occupies the original queue slot and is never skipped or
    // replaced. Only when nothing is live may a Pending row be selected.
    let key: Option<(String, u32)> = tx.query_row(
        "SELECT t.task_id,t.revision FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state IN ('Running','Unknown','CancelRequested') ORDER BY CASE j.state WHEN 'Running' THEN 0 WHEN 'Unknown' THEN 1 ELSE 2 END,t.created_ms,t.task_id,t.revision LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional().map_err(|_| Error::Storage)?;
    let key = match key {
        Some(key) => Some(key),
        None => tx
            .query_row(
                "SELECT t.task_id,t.revision FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state='Pending' ORDER BY t.created_ms,t.task_id,t.revision LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Storage)?,
    };
    let record = key
        .map(|(id, revision)| candidate_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(record)
}

/// Page Pending candidate rows oldest-first. The controller scans this page to
/// find the oldest launchable row without mutating an ineligible predecessor's
/// original book binding, lease or task row.
pub fn pending_candidates_page(
    db: &mut Connection,
    after: Option<&TaskCursor>,
    limit: usize,
) -> Result<Vec<CandidateTaskRecord>> {
    if !(1..=100).contains(&limit) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(Vec::new());
    }
    let mut statement = tx
        .prepare(
            "SELECT t.task_id,t.revision FROM candidate_tasks t JOIN jobs j ON j.id=t.job_id WHERE t.refusal_receipt IS NULL AND j.state='Pending' AND (? IS NULL OR t.created_ms>? OR (t.created_ms=? AND (t.task_id>? OR (t.task_id=? AND t.revision>?)))) ORDER BY t.created_ms,t.task_id,t.revision LIMIT ?",
        )
        .map_err(|_| Error::Storage)?;
    let ids: Vec<(String, u32)> = statement
        .query_map(
            params![
                after.map(|c| c.created_at_ms),
                after.map_or(0, |c| c.created_at_ms),
                after.map(|c| c.created_at_ms),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.task_id.as_str()),
                after.map(|c| c.revision),
                limit as i64
            ],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?
        .collect::<std::result::Result<_, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    let records = ids
        .into_iter()
        .map(|(id, revision)| candidate_tx(&tx, &id, revision)?.ok_or(Error::Missing))
        .collect::<Result<Vec<_>>>()?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(records)
}

/// A candidate row written before the owned-unit policy existed is a legacy
/// experimental schema. Callers must fail closed and require a reviewed
/// migration; they must never tell an operator to recreate the database.
pub fn candidate_requires_migration(db: &Connection) -> Result<bool> {
    let exists: i64 = db
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='candidate_tasks'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if exists == 0 {
        return Ok(false);
    }
    let legacy: i64 = db
        .query_row(
            "SELECT count(*) FROM candidate_tasks WHERE json_extract(body,'$.unit_policy') IS NULL",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    Ok(legacy > 0)
}

pub fn candidate_for_job(db: &mut Connection, job_id: &str) -> Result<Option<CandidateTaskRecord>> {
    if !valid_hash(job_id) {
        return Err(Error::Invalid);
    }
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    if !candidate_tables_exist(&tx)? {
        tx.rollback().map_err(|_| Error::Storage)?;
        return Ok(None);
    }
    let key: Option<(String, u32)> = tx
        .query_row(
            "SELECT task_id,revision FROM candidate_tasks WHERE job_id=?",
            [job_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let result = key
        .map(|(id, revision)| candidate_tx(&tx, &id, revision))
        .transpose()?
        .flatten();
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

pub fn cancel_candidate(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    expected_revision: u32,
    now: i64,
) -> Result<CandidateTaskRecord> {
    if revision != expected_revision || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let mut old = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    if let Some(receipt) = &old.cancel_receipt {
        if receipt.expected_revision == expected_revision {
            tx.commit().map_err(|_| Error::Storage)?;
            return Ok(old);
        }
        return Err(Error::Collision);
    }
    if old.refusal_receipt.is_some()
        || matches!(
            old.status,
            TaskState::Succeeded | TaskState::Terminated | TaskState::Cancelled
        )
    {
        return Err(Error::State);
    }
    let next = match old.job.state {
        State::Pending => "Cancelled",
        State::Running | State::Unknown => "CancelRequested",
        State::CancelRequested => "CancelRequested",
        _ => return Err(Error::State),
    };
    if old.job.state != State::CancelRequested {
        tx.execute(
            "UPDATE jobs SET state=? WHERE id=?",
            params![next, old.spec.job_id],
        )
        .map_err(|_| Error::Storage)?;
        event(
            &tx,
            &old.spec.job_id,
            "cancel_requested",
            now,
            old.job.attempts,
        )?;
    }
    let receipt = CancelReceipt {
        task_id: old.spec.task_id.clone(),
        revision,
        expected_revision,
        requested_ms: now,
        lease: old.job.lease.clone(),
        job_state: next.to_owned(),
    };
    tx.execute(
        "UPDATE candidate_tasks SET cancel_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    candidate_event(&tx, task_id, revision, "cancel_requested", now)?;
    if next == "Cancelled" {
        candidate_event(&tx, task_id, revision, "cancelled", now)?;
    }
    old = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(old)
}

pub fn refuse_unstarted_candidate(
    db: &mut Connection,
    task_id: &str,
    revision: u32,
    reason: RefusalReason,
    now: i64,
) -> Result<CandidateTaskRecord> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let old = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    if let Some(receipt) = &old.refusal_receipt {
        return if receipt.reason == reason {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    if old.job.state != State::Pending || old.job.attempts != 0 || old.cancel_receipt.is_some() {
        return Err(Error::State);
    }
    let receipt = RefusalReceipt {
        task_id: old.spec.task_id.clone(),
        revision,
        job_id: old.spec.job_id.clone(),
        reason,
        at_ms: now,
    };
    tx.execute(
        "UPDATE candidate_tasks SET refusal_receipt=? WHERE task_id=? AND revision=?",
        params![
            serde_json::to_string(&receipt).map_err(|_| Error::Invalid)?,
            task_id,
            revision
        ],
    )
    .map_err(|_| Error::Storage)?;
    tx.execute(
        "UPDATE jobs SET state='Refused' WHERE id=? AND state='Pending' AND attempts=0",
        [&old.spec.job_id],
    )
    .map_err(|_| Error::Storage)?;
    candidate_event(&tx, task_id, revision, "refused", now)?;
    let result = candidate_tx(&tx, task_id, revision)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(result)
}

/// Terminalize a claimed candidate attempt with host sandbox evidence. The
/// receipt must describe the existing score_source kill-and-wait contract, or
/// explicitly record that no sandbox was started. `cause` is the authoritative
/// domain reason: an operator cancel receipt always wins over a later expiry or
/// failure classification, so a recorded human cancel is never mislabelled.
pub fn record_candidate_termination(
    db: &mut Connection,
    id: &str,
    lease: &str,
    receipt: &str,
    cause: CandidateStopCause,
    now: i64,
) -> Result<Job> {
    if !valid_hash(lease)
        || !valid_hash(receipt)
        || now < 0
        || cause == CandidateStopCause::Succeeded
    {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.spec.capability != SpecCapability::CandidateSource {
        return Err(Error::Invalid);
    }
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    let operator =
        cause == CandidateStopCause::OperatorCancel || operator_cancel_recorded(&tx, id)?;
    let label = format!("terminated:{}:{receipt}", cause.as_str());
    if matches!(old.state, State::Terminated | State::Cancelled) {
        let prior: Option<String> = tx
            .query_row(
                "SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",
                params![id, old.attempts],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| Error::Storage)?;
        return if prior.as_deref() == Some(&label) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    let next = match old.state {
        State::Running | State::Unknown | State::CancelRequested => {
            if operator {
                "Cancelled"
            } else {
                "Terminated"
            }
        }
        _ => return Err(Error::State),
    };
    tx.execute("UPDATE jobs SET state=? WHERE id=?", params![next, id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, &label, now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}

fn operator_cancel_recorded(tx: &Transaction<'_>, job_id: &str) -> Result<bool> {
    let receipt: Option<Option<String>> = tx
        .query_row(
            "SELECT cancel_receipt FROM candidate_tasks WHERE job_id=?",
            [job_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    Ok(receipt.flatten().is_some())
}

fn termination_cause(
    tx: &Transaction<'_>,
    job_id: &str,
    attempt: u32,
) -> Result<Option<CandidateStopCause>> {
    let event: Option<String> = tx
        .query_row(
            "SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",
            params![job_id, attempt],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    Ok(event
        .as_deref()
        .and_then(|value| value.strip_prefix("terminated:"))
        .and_then(|value| value.split_once(':'))
        .and_then(|(cause, _)| CandidateStopCause::parse(cause)))
}

fn candidate_artifact_limit(kind: &str) -> Result<usize> {
    match kind {
        "source" => Ok(262_144),
        "params" => Ok(65_536),
        "dataset" => Ok(4 * 1024 * 1024),
        "execution_spec" => Ok(65_536),
        "execution_manifest" => Ok(512 * 1024),
        "execution_events" => Ok(16 * 1024 * 1024),
        "observation_spec" => Ok(65_536),
        "observation_quotes" => Ok(2_000_000),
        "result" => Ok(1_048_576),
        "stopped" => Ok(65_536),
        "launch" => Ok(65_536),
        "quotes" => Ok(1_048_576),
        _ => Err(Error::Invalid),
    }
}

const CANDIDATE_ARTIFACT_TOTAL_ROWS: i64 = 4096;
const CANDIDATE_ARTIFACT_TOTAL_BYTES: i64 = 64 * 1024 * 1024;

fn put_candidate_artifact_tx(
    tx: &Transaction<'_>,
    kind: &str,
    body: &[u8],
    now: i64,
) -> Result<String> {
    let limit = candidate_artifact_limit(kind)?;
    if body.is_empty() || body.len() > limit || now < 0 {
        return Err(Error::Invalid);
    }
    let hash = sha256_hex(body);
    let existing: Option<(String, Vec<u8>)> = tx
        .query_row(
            "SELECT kind,body FROM candidate_artifacts WHERE hash=?",
            params![&hash],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    if let Some((stored_kind, stored)) = existing {
        return if stored_kind == kind && stored == body {
            Ok(hash)
        } else {
            Err(Error::Collision)
        };
    }
    let (rows, bytes): (i64, i64) = tx
        .query_row(
            "SELECT count(*),coalesce(sum(length(body)),0) FROM candidate_artifacts",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(|_| Error::Storage)?;
    if rows >= CANDIDATE_ARTIFACT_TOTAL_ROWS
        || bytes.saturating_add(body.len() as i64) > CANDIDATE_ARTIFACT_TOTAL_BYTES
    {
        return Err(Error::Capacity);
    }
    tx.execute(
        "INSERT INTO candidate_artifacts(hash,kind,body,created_ms) VALUES(?,?,?,?)",
        params![hash, kind, body, now],
    )
    .map_err(|_| Error::Storage)?;
    Ok(hash)
}

pub fn put_candidate_artifact(
    db: &mut Connection,
    kind: &str,
    body: &[u8],
    now: i64,
) -> Result<String> {
    let tx = immediate(db)?;
    ensure_candidate_tables(&tx)?;
    let hash = put_candidate_artifact_tx(&tx, kind, body, now)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(hash)
}

/// Store one artifact under a caller-supplied recorded cap. The global artifact
/// limit remains the upper bound; the task's original cap is the lower bound so
/// a later config change cannot accept or reject an existing task differently.
pub fn put_candidate_artifact_bounded(
    db: &mut Connection,
    kind: &str,
    body: &[u8],
    max: usize,
    now: i64,
) -> Result<String> {
    let limit = candidate_artifact_limit(kind)?;
    if max == 0 || max > limit || body.is_empty() || body.len() > max {
        return Err(Error::Invalid);
    }
    put_candidate_artifact(db, kind, body, now)
}

pub fn read_candidate_artifact(
    db: &Connection,
    hash: &str,
    kind: &str,
    max: usize,
) -> Result<Vec<u8>> {
    if !valid_hash(hash) || max == 0 || max > 16 * 1024 * 1024 {
        return Err(Error::Invalid);
    }
    let limit = candidate_artifact_limit(kind)?;
    if max > limit {
        return Err(Error::Invalid);
    }
    let row: Option<(String, Vec<u8>)> = db
        .query_row(
            "SELECT kind,body FROM candidate_artifacts WHERE hash=? AND length(body)<=?",
            params![hash, limit as i64],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|_| Error::Storage)?;
    let Some((stored_kind, body)) = row else {
        return Err(Error::Missing);
    };
    if stored_kind != kind || body.len() > max {
        return Err(Error::Invalid);
    }
    Ok(body)
}

pub fn initialize(db: &mut Connection) -> Result<()> {
    let tx = immediate(db)?;
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if count != 0 {
        return Err(Error::Storage);
    }
    tx.execute_batch("CREATE TABLE scripts(code TEXT NOT NULL,runtime TEXT NOT NULL,PRIMARY KEY(code,runtime));
        CREATE TABLE jobs(id TEXT PRIMARY KEY,spec TEXT NOT NULL,state TEXT NOT NULL,attempts INTEGER NOT NULL,lease TEXT,until_ms INTEGER,result TEXT);
        CREATE TABLE leases(token TEXT PRIMARY KEY,job TEXT NOT NULL,attempt INTEGER NOT NULL);
        CREATE TABLE job_events(seq INTEGER PRIMARY KEY,job TEXT NOT NULL,event TEXT NOT NULL,at_ms INTEGER NOT NULL,attempt INTEGER NOT NULL);").map_err(|_|Error::Storage)?;
    tx.commit().map_err(|_| Error::Storage)
}
/// Caller must be the reviewed trusted registry owner. Models never receive this API.
pub fn register_reviewed(db: &mut Connection, code: &str, runtime: &str) -> Result<()> {
    if !valid_hash(code) || !valid_hash(runtime) {
        return Err(Error::Invalid);
    }
    db.execute(
        "INSERT OR IGNORE INTO scripts VALUES(?,?)",
        params![code, runtime],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}
fn immediate(db: &mut Connection) -> Result<Transaction<'_>> {
    let sync: i64 = db
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(|_| Error::Storage)?;
    if sync < 2 {
        return Err(Error::Storage);
    }
    db.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| Error::Storage)
}
fn event(tx: &Transaction<'_>, id: &str, name: &str, now: i64, attempt: u32) -> Result<()> {
    let last: Option<i64> = tx
        .query_row("SELECT max(at_ms) FROM job_events WHERE job=?", [id], |r| {
            r.get(0)
        })
        .map_err(|_| Error::Storage)?;
    if last.is_some_and(|last| now < last) {
        return Err(Error::Invalid);
    }
    tx.execute(
        "INSERT INTO job_events(job,event,at_ms,attempt) VALUES(?,?,?,?)",
        params![id, name, now, attempt],
    )
    .map_err(|_| Error::Storage)?;
    Ok(())
}
pub fn enqueue(db: &mut Connection, spec: &Spec, now: i64) -> Result<Job> {
    spec.validate()?;
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let job = enqueue_tx(&tx, spec, now)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}

fn enqueue_tx(tx: &Transaction<'_>, spec: &Spec, now: i64) -> Result<Job> {
    spec.validate()?;
    if now < 0 {
        return Err(Error::Invalid);
    }
    if let Some(old) = read(tx, &spec.id)? {
        if &old.spec == spec {
            return Ok(old);
        }
        return Err(Error::Collision);
    }
    let trials: i64 = tx
        .query_row(
            "SELECT count(*) FROM jobs WHERE json_extract(spec,'$.review_id')=?",
            [&spec.review_id],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if trials >= 4 {
        return Err(Error::Capacity);
    }
    if spec.capability == SpecCapability::RegisteredAnalysis {
        let registered: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM scripts WHERE code=? AND runtime=?)",
                params![spec.script_hash, spec.runtime_hash],
                |r| r.get(0),
            )
            .map_err(|_| Error::Storage)?;
        if !registered {
            return Err(Error::Unregistered);
        }
    }
    let count: i64 = tx
        .query_row(
            "SELECT count(*) FROM jobs WHERE state IN ('Pending','Running','Unknown','CancelRequested')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if count >= 500 {
        return Err(Error::Capacity);
    }
    tx.execute(
        "INSERT INTO jobs VALUES(?,?,'Pending',0,NULL,NULL,NULL)",
        params![
            spec.id,
            serde_json::to_string(spec).map_err(|_| Error::Invalid)?
        ],
    )
    .map_err(|_| Error::Storage)?;
    event(tx, &spec.id, "enqueued", now, 0)?;
    let job = read(tx, &spec.id)?.ok_or(Error::Missing)?;
    Ok(job)
}
pub fn load(db: &mut Connection, id: &str) -> Result<Job> {
    let tx = db.transaction().map_err(|_| Error::Storage)?;
    let job = read(&tx, id)?.ok_or(Error::Missing);
    tx.rollback().map_err(|_| Error::Storage)?;
    job
}
pub fn claim(db: &mut Connection, id: &str, lease: &str, now: i64) -> Result<Job> {
    if !valid_hash(lease) || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.state != State::Pending {
        return Err(Error::State);
    }
    if old.attempts >= old.spec.max_attempts {
        return Err(Error::Attempts);
    }
    tx.execute(
        "INSERT INTO leases VALUES(?,?,?)",
        params![lease, id, old.attempts + 1],
    )
    .map_err(|_| Error::Lease)?;
    let running: i64 = tx
        .query_row(
            "SELECT count(*) FROM jobs WHERE state IN ('Running','Unknown','CancelRequested')",
            [],
            |r| r.get(0),
        )
        .map_err(|_| Error::Storage)?;
    if running > 0 {
        return Err(Error::Capacity);
    }
    let until = now
        .checked_add(i64::from(old.spec.wall_millis))
        .ok_or(Error::Invalid)?;
    tx.execute(
        "UPDATE jobs SET state='Running',attempts=attempts+1,lease=?,until_ms=? WHERE id=?",
        params![lease, until, id],
    )
    .map_err(|_| Error::Storage)?;
    event(&tx, id, "claimed", now, old.attempts + 1)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
pub fn expire(db: &mut Connection, id: &str, now: i64) -> Result<Job> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.state != State::Running || old.until.is_none_or(|until| now < until) {
        return Err(Error::State);
    }
    tx.execute("UPDATE jobs SET state='Unknown' WHERE id=?", [id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, "lease_expired_unknown", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
/// Record cancellation intent only. Running/unknown workers retain their slot.
pub fn request_cancel(db: &mut Connection, id: &str, now: i64) -> Result<Job> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if matches!(old.state, State::Cancelled | State::CancelRequested) {
        return Ok(old);
    }
    let next = match old.state {
        State::Pending | State::Terminated => "Cancelled",
        State::Running | State::Unknown => "CancelRequested",
        _ => return Err(Error::State),
    };
    tx.execute("UPDATE jobs SET state=? WHERE id=?", params![next, id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, "cancel_requested", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
/// The trusted qualified worker must stop the WHOLE sandbox before calling this.
/// A digest records its receipt; fixtures/caller declarations do not qualify containment.
pub fn record_termination(
    db: &mut Connection,
    id: &str,
    lease: &str,
    receipt: &str,
    now: i64,
) -> Result<Job> {
    if !valid_hash(lease) || !valid_hash(receipt) || now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    let label = format!("terminated:{receipt}");
    if matches!(old.state, State::Terminated | State::Cancelled) {
        let prior:Option<String>=tx.query_row("SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",params![id,old.attempts],|r|r.get(0)).optional().map_err(|_|Error::Storage)?;
        return if prior.as_deref() == Some(&label) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    let next = match old.state {
        State::Unknown => "Terminated",
        State::CancelRequested => "Cancelled",
        _ => return Err(Error::State),
    };
    tx.execute("UPDATE jobs SET state=? WHERE id=?", params![next, id])
        .map_err(|_| Error::Storage)?;
    event(&tx, id, &label, now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
pub fn retry_compute(db: &mut Connection, id: &str, now: i64) -> Result<Job> {
    if now < 0 {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.state != State::Terminated || !old.spec.retry_safe_compute {
        return Err(Error::State);
    }
    if old.attempts >= old.spec.max_attempts {
        return Err(Error::Attempts);
    }
    tx.execute(
        "UPDATE jobs SET state='Pending',lease=NULL,until_ms=NULL WHERE id=?",
        [id],
    )
    .map_err(|_| Error::Storage)?;
    event(&tx, id, "explicit_compute_retry", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
pub fn complete(db: &mut Connection, id: &str, lease: &str, result: &str, now: i64) -> Result<Job> {
    complete_with_clock(db, id, lease, result, || now)
}
/// Production workers sample the acceptance clock after acquiring the writer
/// lock. Waiting for SQLite must not extend an expired compute lease.
pub fn complete_with_clock(
    db: &mut Connection,
    id: &str,
    lease: &str,
    result: &str,
    clock: impl FnOnce() -> i64,
) -> Result<Job> {
    if !valid_hash(lease) || !valid_hash(result) {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let now = clock();
    if now < 0 {
        return Err(Error::Invalid);
    }
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    if old.state == State::Succeeded {
        return if old.result_hash.as_deref() == Some(result) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    if old.state != State::Running
        || old
            .until
            .is_none_or(|until| now >= until || now < until - i64::from(old.spec.wall_millis))
    {
        return Err(Error::Lease);
    }
    tx.execute(
        "UPDATE jobs SET state='Succeeded',result=? WHERE id=?",
        params![result, id],
    )
    .map_err(|_| Error::Storage)?;
    event(&tx, id, "completed", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
/// Complete a claimed attempt from an authentic result produced before the
/// compute deadline. The result time proves the compute finished in bounds; the
/// durable event is still recorded at the real acceptance time. Never backdate
/// the database event by feeding the result time as `now`.
///
/// Race ordering is explicit and transactional:
/// - an operator cancel receipt recorded before this transaction always wins;
/// - a host expiry cleanup that only set `CancelRequested` (no cancel receipt)
///   does not erase a timely result;
/// - a `Terminated` row may still reconcile a timely result unless its recorded
///   stop cause was an operator cancel.
pub fn complete_reconciled(
    db: &mut Connection,
    id: &str,
    lease: &str,
    result: &str,
    result_at_ms: i64,
    now: i64,
) -> Result<Job> {
    if !valid_hash(lease) || !valid_hash(result) || result_at_ms < 0 || now < result_at_ms {
        return Err(Error::Invalid);
    }
    let tx = immediate(db)?;
    let old = read(&tx, id)?.ok_or(Error::Missing)?;
    if old.lease.as_deref() != Some(lease) {
        return Err(Error::Lease);
    }
    if old.state == State::Succeeded {
        return if old.result_hash.as_deref() == Some(result) {
            Ok(old)
        } else {
            Err(Error::Collision)
        };
    }
    if operator_cancel_recorded(&tx, id)? {
        return Err(Error::State);
    }
    if !matches!(
        old.state,
        State::Running | State::Unknown | State::CancelRequested | State::Terminated
    ) {
        return Err(Error::State);
    }
    if old.state == State::Terminated
        && termination_cause(&tx, id, old.attempts)? == Some(CandidateStopCause::OperatorCancel)
    {
        return Err(Error::State);
    }
    let until = old.until.ok_or(Error::State)?;
    let started = until.saturating_sub(i64::from(old.spec.wall_millis));
    if result_at_ms < started || result_at_ms >= until {
        return Err(Error::Lease);
    }
    let changed = tx
        .execute(
            "UPDATE jobs SET state='Succeeded',result=? WHERE id=? AND state IN ('Running','Unknown','CancelRequested','Terminated')",
            params![result, id],
        )
        .map_err(|_| Error::Storage)?;
    if changed != 1 {
        return Err(Error::State);
    }
    event(&tx, id, "completed_reconciled", now, old.attempts)?;
    let job = read(&tx, id)?.ok_or(Error::Missing)?;
    tx.commit().map_err(|_| Error::Storage)?;
    Ok(job)
}
fn read(tx: &Transaction<'_>, id: &str) -> Result<Option<Job>> {
    if !valid_hash(id) {
        return Err(Error::Invalid);
    }
    type StoredRow = (
        String,
        String,
        u32,
        Option<String>,
        Option<i64>,
        Option<String>,
    );
    let row: Option<StoredRow>=tx.query_row("SELECT spec,state,attempts,lease,until_ms,result FROM jobs WHERE id=? AND length(spec)<=16384",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional().map_err(|_|Error::Storage)?;
    let Some((spec, state, attempts, lease, until, result_hash)) = row else {
        return Ok(None);
    };
    let spec: Spec = serde_json::from_str(&spec).map_err(|_| Error::Invalid)?;
    spec.validate()?;
    let state = match state.as_str() {
        "Pending" => State::Pending,
        "Running" => State::Running,
        "Unknown" => State::Unknown,
        "CancelRequested" => State::CancelRequested,
        "Terminated" => State::Terminated,
        "Succeeded" => State::Succeeded,
        "Cancelled" => State::Cancelled,
        "Refused" => State::Refused,
        _ => return Err(Error::Invalid),
    };
    if spec.id != id
        || attempts > spec.max_attempts
        || lease.as_ref().is_some_and(|s| !valid_hash(s))
        || result_hash.as_ref().is_some_and(|s| !valid_hash(s))
    {
        return Err(Error::Invalid);
    }
    let lease_valid = lease.is_some()
        && until.is_some_and(|at| at >= i64::from(spec.wall_millis))
        && attempts > 0;
    if match state {
        State::Pending => lease.is_some() || until.is_some() || result_hash.is_some(),
        State::Running | State::Unknown | State::CancelRequested | State::Terminated => {
            !lease_valid || result_hash.is_some()
        }
        State::Succeeded => !lease_valid || result_hash.is_none(),
        State::Cancelled => {
            result_hash.is_some()
                || if lease.is_none() {
                    until.is_some()
                } else {
                    !lease_valid
                }
        }
        State::Refused => {
            lease.is_some() || until.is_some() || result_hash.is_some() || attempts != 0
        }
    } {
        return Err(Error::Invalid);
    }
    if let Some(token) = lease.as_ref() {
        let owner: Option<(String, u32)> = tx
            .query_row(
                "SELECT job,attempt FROM leases WHERE token=?",
                [token],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| Error::Storage)?;
        if owner != Some((id.to_owned(), attempts)) {
            return Err(Error::Invalid);
        }
    }
    if state == State::Terminated || (state == State::Cancelled && lease.is_some()) {
        let receipt:Option<String>=tx.query_row("SELECT event FROM job_events WHERE job=? AND attempt=? AND event LIKE 'terminated:%' ORDER BY seq DESC LIMIT 1",params![id,attempts],|r|r.get(0)).optional().map_err(|_|Error::Storage)?;
        let parsed = receipt.as_deref().and_then(|value| {
            let value = value.strip_prefix("terminated:")?;
            match value.split_once(':') {
                Some((cause, hash)) => Some((CandidateStopCause::parse(cause), hash)),
                None => Some((None, value)),
            }
        });
        let Some((cause, hash)) = parsed else {
            return Err(Error::Invalid);
        };
        if !valid_hash(hash) {
            return Err(Error::Invalid);
        }
        // Candidate rows written by this build always carry the typed stop
        // cause; the registered worker keeps its historical receipt-only label.
        if spec.capability == SpecCapability::CandidateSource && cause.is_none() {
            return Err(Error::Invalid);
        }
    }
    Ok(Some(Job {
        spec,
        state,
        attempts,
        lease,
        until,
        result_hash,
    }))
}
