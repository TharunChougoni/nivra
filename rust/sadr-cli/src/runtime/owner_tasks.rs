//! Owner work is projected from original domain evidence, never queued here.
use super::workspace::WorkspaceRefV1;
use super::*;
use sadr_core::postclose::{ReviewOutput, SealedEvidence};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Earliest post-close owner-task cutoff config validation accepts: one minute
/// after the default session close. The runtime cutoff check
/// (`settings_cutoff`) compares against the configured close, so a book with a
/// later close still refuses a cutoff at or before it (#234).
pub(crate) const MIN_CUTOFF_MINUTE: u32 = sadr_core::execution::DEFAULT_SESSION_END_MINUTE + 1;
/// Latest owner-task cutoff: the last minute of the day.
pub(crate) const MAX_CUTOFF_MINUTE: u32 = 1439;
/// Upper bound on rows in one controller observation capture receipt. Settings
/// validation and the receipt write site share it.
pub(crate) const OBSERVATION_CAPTURE_MAX_ROWS: usize = 10_000;
/// Upper bound on the raw bytes of one controller observation capture receipt.
pub(crate) const OBSERVATION_CAPTURE_MAX_BYTES: usize = 2_000_000;
/// Most tool turns one research lane run may be configured for (#242).
pub(crate) const MAX_RESEARCH_TOOL_TURNS: u32 = 256;
/// Longest research lane run timeout config validation accepts: six hours (#242).
pub(crate) const MAX_RESEARCH_TIMEOUT_MS: u64 = 21_600_000;

pub(crate) const REVIEW_PROMPT: &str = "Review only the inline sealed session evidence. No tools, orders, directives, policy changes, code, network or workspace writes are permitted. Return exactly JSON {version:1,review_id,sealed_sha256,review:{observations,hypotheses,uncertainties,memory}}. Cite only supplied evidence IDs. Memory must repeat a cited observation or hypothesis. Distinguish observed outcomes from hypotheses and missing evidence. This is advisory paper-session research, never financial authority.";

/// Pinned research prompt. It is bound into policy only when the reviewed
/// research capability is enabled, so every historical policy byte and hash
/// stays unchanged while research is absent.
pub(crate) const RESEARCH_PROMPT: &str = "Research only the inline host-selected original evidence, the trading lane's inline research_request question when present, and pinned workspace text. Return exactly JSON {version:1,evidence_id,summary,observations,cited_ids,workspace_refs}. Cite only the supplied evidence_id or pinned workspace paths as workspace:<path>. Observations are advisory notes, never orders, TradePlan, prices, fills, cash, fees, P&L, policy changes, credentials, code execution or broker authority. Distinguish verified source facts from hypotheses and missing evidence. Never claim a trade, promotion, activation or profit. This is paper-only research, never financial authority.";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OwnerTasksSettings {
    pub version: u32,
    pub review: ReviewSettings,
    /// Optional reviewed research capability. Absent keeps every historical
    /// policy byte, prompt binding and tool frame unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research: Option<OwnerResearchSettings>,
    /// Optional session agenda: explicit pre-open and pre-close owner turns
    /// (#229). Absent keeps every historical policy byte unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agenda: Option<AgendaSettings>,
}
impl Default for OwnerTasksSettings {
    fn default() -> Self {
        Self {
            version: 1,
            review: ReviewSettings::default(),
            research: None,
            agenda: None,
        }
    }
}

/// Which agenda turns the owner gets. Both are off unless configured.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct AgendaSettings {
    pub pre_open: bool,
    pub pre_close: bool,
}

/// One explicit owner agenda turn. Each runs at most once per session through
/// the existing supervisor turn (notes and digest directives only; no order
/// tools), never through the legacy decide / operations / post-close paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgendaTurn {
    /// The session-open turn, at the supervisor `session_open_minute`.
    PreOpen,
    /// The pre-close turn, at the supervisor `pre_close_minute`. Distinct from
    /// the post-close sealed review.
    PreClose,
}
impl AgendaTurn {
    /// The supervisor trigger (and prompt) the turn runs as.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::PreOpen => "session_open",
            Self::PreClose => "pre_close",
        }
    }
}

/// Pure admission input for agenda turns.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AgendaInput {
    pub settings: Option<AgendaSettings>,
    pub in_session: bool,
    pub book_runnable: bool,
    pub minute: u32,
    pub open_minute: u32,
    pub pre_close_minute: u32,
    pub open_done: bool,
    pub pre_close_done: bool,
    pub cooled: bool,
    pub budget_left: bool,
}

/// Typed agenda admission outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgendaState {
    Disabled,
    /// Not admitted now: outside the session, book not runnable, cooling down
    /// or out of budget.
    Blocked,
    /// Before the next agenda minute.
    Waiting,
    Due(AgendaTurn),
    /// Every configured agenda turn already reached a terminal outcome.
    Done,
}

pub(crate) fn project_agenda(input: &AgendaInput) -> AgendaState {
    let Some(settings) = input.settings.filter(|s| s.pre_open || s.pre_close) else {
        return AgendaState::Disabled;
    };
    let open_pending = settings.pre_open && !input.open_done;
    let close_pending = settings.pre_close && !input.pre_close_done;
    if !open_pending && !close_pending {
        return AgendaState::Done;
    }
    if !input.in_session || !input.book_runnable || !input.cooled || !input.budget_left {
        return AgendaState::Blocked;
    }
    // A missed open is skipped once pre-close is due, so the open turn never
    // runs late beside the close turn.
    if close_pending && input.minute >= input.pre_close_minute {
        return AgendaState::Due(AgendaTurn::PreClose);
    }
    if open_pending && input.minute >= input.open_minute && input.minute < input.pre_close_minute {
        return AgendaState::Due(AgendaTurn::PreOpen);
    }
    AgendaState::Waiting
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ReviewSettings {
    pub enabled: bool,
    pub cutoff_minute: u32,
}
impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            cutoff_minute: 1080,
        }
    }
}
/// Reviewed opt-in for one bounded research turn over existing original
/// evidence. It is not a queue, agenda, second model role or trading cap.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OwnerResearchSettings {
    pub enabled: bool,
    pub cutoff_minute: u32,
    pub max_evidence: usize,
    /// Optional read-only archived-receipt quote reader. Omitted when absent so
    /// old policy bytes are unchanged; it never reaches the provider or OHLC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote_capture: Option<OwnerResearchQuoteCaptureSettings>,
    /// Optional controller-only observation capture grant. This is distinct
    /// from the archive-only `quote_capture` reader: when enabled and the book
    /// is eligible, the controller may use the existing application quote
    /// client to append original research-observation receipts. It never
    /// changes the archive-only reader's meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_capture: Option<ControllerObservationCaptureSettings>,
    /// The research lane profile: its own worker slot, model, budget and
    /// schedule (#237, #238). Required when research is enabled; omitted when
    /// absent so historical policy bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<ResearchLaneSettings>,
    /// Optional quiet-trading trigger (#244). Absent or disabled keeps the
    /// historical request/evidence-only admission and policy bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_trigger: Option<IdleTriggerSettings>,
}
impl Default for OwnerResearchSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: None,
            idle_trigger: None,
        }
    }
}
/// Longest quiet window the idle trigger accepts: one full NSE session.
pub(crate) const MAX_IDLE_QUIET_MINUTES: u32 = 375;
/// Marks research due when trading has been quiet during market hours: no
/// fill for `quiet_minutes`, anchored at the market open before the first
/// fill. It only adds a reason to start research; every lane cap, the
/// minimum interval, the pending turn and the request capacity still apply,
/// and it never touches the trading lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct IdleTriggerSettings {
    pub enabled: bool,
    pub quiet_minutes: u32,
}
impl Default for IdleTriggerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            quiet_minutes: 30,
        }
    }
}
impl IdleTriggerSettings {
    /// True when the market is open and the last fill (or the open, before
    /// the first fill) is at least `quiet_minutes` old.
    pub(crate) fn quiet(
        &self,
        now_ms: i64,
        market_open_ms: Option<i64>,
        last_fill_ms: Option<i64>,
    ) -> bool {
        let Some(open) = market_open_ms.filter(|open| now_ms >= *open) else {
            return false;
        };
        let anchor = last_fill_ms.map_or(open, |fill| fill.max(open));
        self.enabled && now_ms.saturating_sub(anchor) >= i64::from(self.quiet_minutes) * 60_000
    }
}
/// Reviewed profile of the research lane. It is never a trading cap: it bounds
/// only the research lane's own model turns, which run beside the trading lane
/// and can never occupy its worker slot or charge its allowance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResearchLaneSettings {
    /// `None` uses the orchestrator's active `model select` profile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub max_tool_turns: u32,
    pub timeout_ms: u64,
    pub max_turns_per_session: u32,
    /// Daily call allowance of the separate `research` usage bucket.
    pub max_calls_per_session: u32,
    /// Provider input+output tokens charged to the `research` bucket.
    pub max_tokens_per_session: u64,
    /// Minimum time between two research turn starts.
    pub min_interval_ms: i64,
    /// `false` admits research only after the session close.
    pub during_market: bool,
    /// Queued plus running trading research requests.
    pub max_open_requests: usize,
    pub request_ttl_ms: i64,
}
impl Default for ResearchLaneSettings {
    fn default() -> Self {
        Self {
            model_id: None,
            max_tool_turns: 8,
            timeout_ms: 600_000,
            max_turns_per_session: 6,
            max_calls_per_session: 48,
            max_tokens_per_session: 2_000_000,
            min_interval_ms: 900_000,
            during_market: false,
            max_open_requests: 4,
            request_ttl_ms: 21_600_000,
        }
    }
}
impl ResearchLaneSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if self
            .model_id
            .as_deref()
            .is_some_and(|id| id.is_empty() || id.len() > 128 || id.chars().any(char::is_control))
            || !(1..=MAX_RESEARCH_TOOL_TURNS).contains(&self.max_tool_turns)
            || !(10_000..=MAX_RESEARCH_TIMEOUT_MS).contains(&self.timeout_ms)
            || !(1..=64).contains(&self.max_turns_per_session)
            || !(1..=1_000).contains(&self.max_calls_per_session)
            || !(1_000..=100_000_000).contains(&self.max_tokens_per_session)
            || !(0..=86_400_000).contains(&self.min_interval_ms)
            || !(1..=32).contains(&self.max_open_requests)
            || !(60_000..=86_400_000).contains(&self.request_ttl_ms)
        {
            return Err("research_lane_settings_invalid".into());
        }
        Ok(())
    }
}
impl OwnerResearchSettings {
    pub(crate) fn validate(&self, cfg: &planner::PlannerConfig) -> Result<()> {
        if !(MIN_CUTOFF_MINUTE..=MAX_CUTOFF_MINUTE).contains(&self.cutoff_minute)
            || !(1..=8).contains(&self.max_evidence)
        {
            return Err("owner_research_settings_invalid".into());
        }
        if let Some(quotes) = &self.quote_capture {
            quotes.validate()?;
            if quotes.enabled && !self.enabled {
                return Err("owner_research_quote_capture_invalid".into());
            }
        }
        if let Some(capture) = &self.observation_capture {
            capture.validate()?;
            if capture.enabled && !self.enabled {
                return Err("owner_research_observation_capture_invalid".into());
            }
            if capture.enabled
                && !cfg.research_tools.as_ref().is_some_and(|settings| {
                    settings
                        .candidate_compute
                        .as_ref()
                        .and_then(|candidate| candidate.observation.as_ref())
                        .is_some_and(|observation| observation.enabled)
                })
            {
                return Err("owner_research_observation_capture_invalid".into());
            }
        }
        if let Some(lane) = &self.lane {
            lane.validate()?;
        }
        if let Some(idle) = &self.idle_trigger
            && (!(1..=MAX_IDLE_QUIET_MINUTES).contains(&idle.quiet_minutes)
                || (idle.enabled && (!self.enabled || self.lane.is_none())))
        {
            return Err("research_idle_trigger_invalid".into());
        }
        if self.enabled && self.lane.is_none() {
            return Err("research_lane_required".into());
        }
        if self.enabled {
            let candidate = cfg.research_tools.as_ref().is_some_and(|settings| {
                settings.admission_limits.is_some()
                    && settings
                        .candidate_compute
                        .as_ref()
                        .is_some_and(|candidate| candidate.enabled)
            });
            let workspace = cfg
                .workspace
                .as_ref()
                .is_some_and(|workspace| workspace.enabled);
            if !cfg.orchestrator_owns_decisions()
                || !cfg
                    .role(super::orchestrator_model::Trigger::Supervisor)
                    .enabled
                || (!candidate && !workspace)
            {
                return Err("owner_research_requires_decision_owner".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct OwnerResearchQuoteCaptureSettings {
    pub enabled: bool,
    pub max_rows: usize,
}
impl Default for OwnerResearchQuoteCaptureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_rows: 64,
        }
    }
}
impl OwnerResearchQuoteCaptureSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.max_rows) {
            return Err("owner_research_quote_capture_invalid".into());
        }
        Ok(())
    }
}
/// Reviewed controller-only observation capture grant. It is not a trading
/// risk cap, broker identity, credential or model role: it bounds how often the
/// existing controller quote client may append original research receipts for
/// an already-admitted observation.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ControllerObservationCaptureSettings {
    pub enabled: bool,
    pub max_keys: usize,
    pub max_calls_per_session: u32,
    pub min_interval_ms: i64,
    pub timeout_ms: u64,
    pub max_rows: usize,
}
impl Default for ControllerObservationCaptureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_keys: 64,
            max_calls_per_session: 64,
            min_interval_ms: 60_000,
            timeout_ms: 5_000,
            max_rows: 512,
        }
    }
}
impl ControllerObservationCaptureSettings {
    pub(crate) fn validate(&self) -> Result<()> {
        if !(1..=1_000).contains(&self.max_keys)
            || !(1..=10_000).contains(&self.max_calls_per_session)
            || !(1_000..=3_600_000).contains(&self.min_interval_ms)
            || !(1_000..=30_000).contains(&self.timeout_ms)
            || !(1..=OBSERVATION_CAPTURE_MAX_ROWS).contains(&self.max_rows)
        {
            return Err("owner_research_observation_capture_invalid".into());
        }
        Ok(())
    }
}
impl OwnerTasksSettings {
    pub(crate) fn validate(&self, cfg: &planner::PlannerConfig) -> Result<()> {
        if self.version != 1
            || !(MIN_CUTOFF_MINUTE..=MAX_CUTOFF_MINUTE).contains(&self.review.cutoff_minute)
        {
            return Err("owner_tasks_settings_invalid".into());
        }
        if self.review.enabled
            && (!cfg.orchestrator_owns_decisions()
                || !cfg
                    .role(super::orchestrator_model::Trigger::Supervisor)
                    .enabled
                || cfg.post_close.as_ref().is_some_and(|s| s.enabled))
        {
            return Err("owner_review_requires_decision_owner".into());
        }
        if let Some(research) = &self.research {
            research.validate(cfg)?;
        }
        if self.agenda.is_some_and(|a| a.pre_open || a.pre_close)
            && (!cfg.orchestrator_owns_decisions()
                || !cfg
                    .role(super::orchestrator_model::Trigger::Supervisor)
                    .enabled)
        {
            return Err("owner_agenda_requires_decision_owner".into());
        }
        Ok(())
    }
}
pub(crate) fn bind_policy(cfg: &planner::PlannerConfig, value: &mut Value) {
    let Some(settings) = cfg.owner_tasks.as_ref() else {
        return;
    };
    if settings.review.enabled {
        value["owner_tasks"]["review_prompt_sha256"] = json!(digest(REVIEW_PROMPT.as_bytes()));
    }
    if settings
        .research
        .as_ref()
        .is_some_and(|research| research.enabled)
    {
        value["owner_tasks"]["research_prompt_sha256"] = json!(digest(RESEARCH_PROMPT.as_bytes()));
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewIdentity {
    pub version: u32,
    pub session: String,
    pub sealed_sha256: String,
    pub policy_sha256: String,
    pub prompt_version: String,
}
impl ReviewIdentity {
    pub(crate) fn id(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
}
/// Stable identity of one original evidence record. `evidence_id` is derived
/// only from immutable source facts, never from mutable workspace text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResearchIdentity {
    pub version: u32,
    pub session: String,
    pub evidence_id: String,
    pub policy_sha256: String,
    pub prompt_version: String,
}
impl ResearchIdentity {
    pub(crate) fn id(&self) -> Result<String> {
        Ok(digest(&serde_json::to_vec(self)?))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResearchEvidenceKind {
    CandidateResult,
    RegisteredResult,
    Failure,
    Maturity,
    /// A trading-lane `research_request`: `task_id` is the message id and
    /// `result_sha256` the hash of its canonical body (#239).
    TradingRequest,
}
impl ResearchEvidenceKind {
    fn order(self) -> u8 {
        match self {
            Self::TradingRequest => 0,
            Self::CandidateResult => 1,
            Self::RegisteredResult => 2,
            Self::Failure => 3,
            Self::Maturity => 4,
        }
    }
}
/// One verified terminal source selected for a research turn. `workspace_refs`
/// are pinned original references, never copied text or model-supplied hashes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResearchEvidenceV1 {
    pub version: u32,
    pub kind: ResearchEvidenceKind,
    pub source_session: String,
    pub task_id: String,
    pub revision: u32,
    pub result_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle_candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maturity_state: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_refs: Vec<WorkspaceRefV1>,
}
impl ResearchEvidenceV1 {
    /// Stable original evidence ID. Workspace refs are deliberately excluded:
    /// they are presentation context, not source identity.
    pub(crate) fn evidence_id(&self) -> Result<String> {
        #[derive(Serialize)]
        struct Stable<'a> {
            version: u32,
            kind: ResearchEvidenceKind,
            source_session: &'a str,
            task_id: &'a str,
            revision: u32,
            result_sha256: &'a str,
            lifecycle_candidate_id: &'a Option<String>,
            maturity_state: &'a Option<String>,
        }
        Ok(digest(&serde_json::to_vec(&Stable {
            version: self.version,
            kind: self.kind,
            source_session: &self.source_session,
            task_id: &self.task_id,
            revision: self.revision,
            result_sha256: &self.result_sha256,
            lifecycle_candidate_id: &self.lifecycle_candidate_id,
            maturity_state: &self.maturity_state,
        })?))
    }
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.source_session.is_empty()
            || self.source_session.len() > 32
            || chrono::NaiveDate::parse_from_str(&self.source_session, "%Y-%m-%d").is_err()
            || self.task_id.is_empty()
            || self.task_id.len() > 256
            || !sha256_hex(&self.result_sha256)
            || self.revision == 0
            || self.workspace_refs.len() > 16
        {
            return Err("owner_research_evidence_invalid".into());
        }
        for reference in &self.workspace_refs {
            reference.validate()?;
        }
        Ok(())
    }
    pub(crate) fn kind_order(&self) -> u8 {
        self.kind.order()
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResearchReceipt {
    pub version: u32,
    pub evidence_id: String,
    pub output_sha256: String,
    pub model_id: String,
    pub completed_ms: i64,
    pub status: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResearchOutputV1 {
    pub version: u32,
    pub evidence_id: String,
    pub summary: String,
    #[serde(default)]
    pub observations: Vec<String>,
    #[serde(default)]
    pub cited_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workspace_refs: Vec<String>,
}
impl ResearchOutputV1 {
    pub(crate) fn validate(
        raw: &str,
        task: &OwnerTask,
        pinned_paths: &BTreeSet<String>,
    ) -> Result<Self> {
        if raw.len() > 65_536 {
            return Err("owner_research_output_bound".into());
        }
        let value = sadr_core::native::decode(raw.as_bytes())
            .map_err(|_| "owner_research_output_invalid")?;
        if forbidden_financial_key(&value) {
            return Err("owner_research_output_financial_refused".into());
        }
        let output: Self =
            serde_json::from_value(value).map_err(|_| "owner_research_output_invalid")?;
        let identity = task
            .research_identity()
            .ok_or("owner_research_binding_invalid")?;
        if output.version != 1
            || output.evidence_id != identity.evidence_id
            || output.summary.trim().is_empty()
            || output.summary.len() > 4096
            || output
                .summary
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            || output.observations.len() > 16
            || output.cited_ids.len() > 16
            || output.workspace_refs.len() > 16
        {
            return Err("owner_research_output_invalid".into());
        }
        for observation in &output.observations {
            if observation.trim().is_empty()
                || observation.len() > 1024
                || observation
                    .chars()
                    .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            {
                return Err("owner_research_output_invalid".into());
            }
        }
        let mut cited = BTreeSet::new();
        for id in &output.cited_ids {
            if id.is_empty()
                || id.len() > 160
                || !(id == &identity.evidence_id
                    || id
                        .strip_prefix("workspace:")
                        .is_some_and(|path| pinned_paths.contains(path)))
                || !cited.insert(id)
            {
                return Err("owner_research_output_citation_invalid".into());
            }
        }
        for path in &output.workspace_refs {
            if !pinned_paths.contains(path) {
                return Err("owner_research_output_citation_invalid".into());
            }
        }
        Ok(output)
    }
}
fn forbidden_financial_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(
                key.as_str(),
                "order"
                    | "orders"
                    | "trade_plan"
                    | "tradeplan"
                    | "fills"
                    | "fill"
                    | "units"
                    | "quantity"
                    | "price"
                    | "cash"
                    | "cash_inr"
                    | "fees"
                    | "fees_inr"
                    | "net_pnl"
                    | "net_pnl_if_flat"
                    | "realized_pnl"
                    | "realised_pnl"
                    | "paper_pnl"
                    | "capital"
                    | "broker"
                    | "account"
                    | "credential"
                    | "credentials"
            ) || forbidden_financial_key(value)
        }),
        Value::Array(values) => values.iter().any(forbidden_financial_key),
        _ => false,
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum OwnerTask {
    Review {
        identity: ReviewIdentity,
        cutoff_ms: i64,
    },
    Research {
        identity: ResearchIdentity,
        evidence: ResearchEvidenceV1,
    },
}
fn sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl OwnerTask {
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Review { .. } => "review",
            Self::Research { .. } => "research",
        }
    }
    pub(crate) fn review_identity(&self) -> Option<&ReviewIdentity> {
        match self {
            Self::Review { identity, .. } => Some(identity),
            Self::Research { .. } => None,
        }
    }
    pub(crate) fn research_identity(&self) -> Option<&ResearchIdentity> {
        match self {
            Self::Review { .. } => None,
            Self::Research { identity, .. } => Some(identity),
        }
    }
    pub(crate) fn research_evidence(&self) -> Option<&ResearchEvidenceV1> {
        match self {
            Self::Review { .. } => None,
            Self::Research { evidence, .. } => Some(evidence),
        }
    }
    pub(crate) fn id(&self) -> Result<String> {
        match self {
            Self::Review { identity, .. } => identity.id(),
            Self::Research { identity, .. } => identity.id(),
        }
    }
    /// Validate the evidence a turn recorded. Historical turns keep their
    /// original prompt and policy bindings; only a new provider invocation must
    /// match the current reviewed prompt and policy.
    pub(crate) fn from_turn(turn: &session::Turn) -> Result<Option<Self>> {
        let Some(raw) = turn.request.get("owner_task") else {
            if turn.trigger == "review" {
                return Err("owner_review_binding_invalid".into());
            }
            if turn.trigger == "research" {
                return Err("owner_research_binding_invalid".into());
            }
            return Ok(None);
        };
        let task: Self = serde_json::from_value(raw.clone())
            .map_err(|_| "owner_task_binding_invalid".to_string())?;
        match &task {
            Self::Review { identity, .. } => {
                let sealed: SealedEvidence =
                    serde_json::from_value(turn.request["sealed_evidence"].clone())
                        .map_err(|_| "owner_review_binding_invalid")?;
                sealed
                    .validate()
                    .map_err(|_| "owner_review_binding_invalid")?;
                let review_id = identity.id()?;
                if turn.trigger != "review"
                    || identity.version != 1
                    || identity.session != turn.session
                    || sealed.input.session.to_string() != turn.session
                    || identity.sealed_sha256 != sealed.sha256
                    || identity.prompt_version != turn.prompt_version
                    || !sha256_hex(&identity.prompt_version)
                    || !sha256_hex(&identity.sealed_sha256)
                    || !sha256_hex(&identity.policy_sha256)
                    || turn
                        .request
                        .get("review_id")
                        .is_some_and(|id| id.as_str() != Some(review_id.as_str()))
                {
                    return Err("owner_review_binding_invalid".into());
                }
            }
            Self::Research { identity, evidence } => {
                evidence.validate()?;
                let evidence_id = evidence.evidence_id()?;
                let research_id = identity.id()?;
                if turn.trigger != "research"
                    || identity.version != 1
                    || identity.session != turn.session
                    || evidence.source_session != turn.session
                    || identity.evidence_id != evidence_id
                    || identity.prompt_version != turn.prompt_version
                    || !sha256_hex(&identity.prompt_version)
                    || !sha256_hex(&identity.policy_sha256)
                    || turn.request.get("turn_id").and_then(Value::as_str) != Some(turn.id.as_str())
                    || turn
                        .request
                        .get("research_id")
                        .is_some_and(|id| id.as_str() != Some(research_id.as_str()))
                {
                    return Err("owner_research_binding_invalid".into());
                }
            }
        }
        Ok(Some(task))
    }
    /// A new provider invocation is admitted only under the current reviewed
    /// prompt and policy bytes.
    pub(crate) fn validate_current(&self, cfg: &planner::PlannerConfig) -> Result<()> {
        let policy = digest(&planner::policy_bytes(cfg)?);
        match self {
            Self::Review { identity, .. } => {
                if identity.prompt_version != digest(REVIEW_PROMPT.as_bytes())
                    || identity.policy_sha256 != policy
                {
                    return Err("owner_review_binding_invalid".into());
                }
            }
            Self::Research { identity, .. } => {
                if identity.prompt_version != digest(RESEARCH_PROMPT.as_bytes())
                    || identity.policy_sha256 != policy
                {
                    return Err("owner_research_binding_invalid".into());
                }
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerReviewOutput {
    pub version: u32,
    pub review_id: String,
    pub sealed_sha256: String,
    pub review: ReviewOutput,
}
impl OwnerReviewOutput {
    pub(crate) fn validate(raw: &str, task: &OwnerTask, sealed: &SealedEvidence) -> Result<Self> {
        if raw.len() > 65_536 {
            return Err("owner_review_output_bound".into());
        }
        let value =
            sadr_core::native::decode(raw.as_bytes()).map_err(|_| "owner_review_output_invalid")?;
        let output: Self =
            serde_json::from_value(value).map_err(|_| "owner_review_output_invalid")?;
        if output.version != 1
            || output.review_id != task.id()?
            || output.sealed_sha256 != sealed.sha256
        {
            return Err("owner_review_output_invalid".into());
        }
        sadr_core::postclose::validate_output(&serde_json::to_string(&output.review)?, sealed)?;
        Ok(output)
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewReceipt {
    pub version: u32,
    pub review_id: String,
    pub sealed_sha256: String,
    pub output_sha256: String,
    pub model_id: String,
    pub completed_ms: i64,
    pub status: String,
}

pub(crate) fn validate_review_bundle(
    c: &Config,
    cfg: &planner::PlannerConfig,
    bundle: &Value,
    invocation_id: Option<&str>,
) -> Result<bool> {
    if bundle["summary"].get("owner_task").is_none() {
        return Ok(false);
    }
    if bundle["summary"]["owner_task"]["kind"] != "review" {
        return Ok(false);
    }
    let facts = &bundle["summary"];
    let id = facts["turn_id"]
        .as_str()
        .ok_or("owner_review_binding_invalid")?;
    let turn = session::Turn {
        id: id.into(),
        session: c.session.to_string(),
        trigger: "review".into(),
        prompt_version: digest(REVIEW_PROMPT.as_bytes()),
        notes_version: 0,
        decides_at_start: 0,
        fills_at_start: 0,
        started_at_ms: 0,
        request: facts.clone(),
    };
    let task = OwnerTask::from_turn(&turn)?.ok_or("owner_review_binding_invalid")?;
    task.validate_current(cfg)?;
    let sealed: SealedEvidence = serde_json::from_value(facts["sealed_evidence"].clone())?;
    let recorded_revision = facts["model_selection"]["revision"].as_u64().unwrap_or(0);
    let current_revision = super::model_selection::resolve(c, cfg)
        .map_err(|_| "owner_review_binding_invalid")?
        .as_ref()
        .map_or(0, |selected| selected.revision);
    if !cfg.owner_tasks.as_ref().is_some_and(|s| s.review.enabled)
        || !cfg.orchestrator_owns_decisions()
        || bundle["trigger"]["kind"] != "review"
        || invocation_id != Some(id)
        || recorded_revision != current_revision
        || facts["model_id"].as_str().is_none_or(|id| id.is_empty())
        || sealed.input.session_binding != book_binding(c)?
        || facts["review_id"].as_str() != Some(task.id()?.as_str())
    {
        return Err("owner_review_binding_invalid".into());
    }
    Ok(true)
}

/// Validate a frame that claims to be a research turn. Decision and review
/// frames remain on their existing validators; this never accepts a forged
/// decision claim or a research turn with a mismatched invocation.
pub(crate) fn validate_research_bundle(
    c: &Config,
    cfg: &planner::PlannerConfig,
    bundle: &Value,
    invocation_id: Option<&str>,
) -> Result<bool> {
    if bundle["summary"].get("owner_task").is_none() {
        return Ok(false);
    }
    if bundle["summary"]["owner_task"]["kind"] != "research" {
        return Ok(false);
    }
    let facts = &bundle["summary"];
    let id = facts["turn_id"]
        .as_str()
        .ok_or("owner_research_binding_invalid")?;
    let turn = session::Turn {
        id: id.into(),
        session: c.session.to_string(),
        trigger: "research".into(),
        prompt_version: digest(RESEARCH_PROMPT.as_bytes()),
        notes_version: 0,
        decides_at_start: 0,
        fills_at_start: 0,
        started_at_ms: 0,
        request: facts.clone(),
    };
    let task = OwnerTask::from_turn(&turn)?.ok_or("owner_research_binding_invalid")?;
    task.validate_current(cfg)?;
    let evidence = task
        .research_evidence()
        .ok_or("owner_research_binding_invalid")?;
    let recorded_revision = facts["model_selection"]["revision"].as_u64().unwrap_or(0);
    let current_revision = super::model_selection::resolve(c, cfg)
        .map_err(|_| "owner_research_binding_invalid")?
        .as_ref()
        .map_or(0, |selected| selected.revision);
    if !cfg
        .owner_tasks
        .as_ref()
        .and_then(|s| s.research.as_ref())
        .is_some_and(|research| research.enabled)
        || !cfg.orchestrator_owns_decisions()
        || bundle["trigger"]["kind"] != "research"
        || invocation_id != Some(id)
        || recorded_revision != current_revision
        || facts["model_id"].as_str().is_none_or(|id| id.is_empty())
        || facts["research_id"].as_str() != Some(task.id()?.as_str())
        || facts["evidence_id"].as_str() != Some(evidence.evidence_id()?.as_str())
        || evidence.source_session != c.session.to_string()
    {
        return Err("owner_research_binding_invalid".into());
    }
    Ok(true)
}

#[derive(Clone, Copy)]
pub(crate) struct ScheduleInput {
    pub enabled: bool,
    pub closed: bool,
    pub report_present: bool,
    pub parked: bool,
    pub busy: bool,
    pub attempted: bool,
    /// A recorded attempt belongs to a different report or cannot be read.
    pub attempt_conflict: bool,
    pub attempt_invalid: bool,
    /// The controller clock is on the configured session date.
    pub session_day: bool,
    pub now_ms: i64,
    pub close_ms: i64,
    pub cutoff_ms: i64,
    /// Optional owner failure backoff expiry. Absent keeps old scheduling.
    pub backoff_until_ms: Option<i64>,
}
/// Typed post-close review schedule state (#229). Adding a variant fails to
/// compile until `as_str` and the projection handle it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReviewState {
    Disabled,
    Invalid,
    Attempted,
    Expired,
    Parked,
    Busy,
    AwaitingSealedReport,
    Deferred,
    Due,
}
impl ReviewState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Invalid => "invalid",
            Self::Attempted => "attempted",
            Self::Expired => "expired",
            Self::Parked => "parked",
            Self::Busy => "busy",
            Self::AwaitingSealedReport => "awaiting_sealed_report",
            Self::Deferred => "deferred",
            Self::Due => "due",
        }
    }
}
impl PartialEq<&str> for ReviewState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
/// Typed research lane schedule state (#229).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ResearchState {
    Disabled,
    Pending,
    Expired,
    Parked,
    Deferred,
    Due,
    Invalid,
    AwaitingEvidence,
}
impl ResearchState {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Pending => "pending",
            Self::Expired => "expired",
            Self::Parked => "parked",
            Self::Deferred => "deferred",
            Self::Due => "due",
            Self::Invalid => "invalid",
            Self::AwaitingEvidence => "awaiting_evidence",
        }
    }
}
impl PartialEq<&str> for ResearchState {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct OwnerSchedule {
    pub version: u32,
    pub as_of_ms: i64,
    pub review_due: bool,
    pub review_state: ReviewState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_reason: Option<&'static str>,
    /// Real expiry for a deferred review, when one is known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_until_ms: Option<i64>,
}
pub(crate) fn project(input: &ScheduleInput) -> OwnerSchedule {
    let deferred_until = input.backoff_until_ms.filter(|until| input.now_ms < *until);
    let (state, reason) = if !input.enabled {
        (ReviewState::Disabled, None)
    } else if input.attempt_invalid {
        (ReviewState::Invalid, Some("owner_review_binding_invalid"))
    } else if input.attempt_conflict {
        (ReviewState::Invalid, Some("owner_review_report_changed"))
    } else if input.attempted {
        (ReviewState::Attempted, None)
    } else if !input.session_day {
        (
            ReviewState::Invalid,
            Some("owner_review_session_date_mismatch"),
        )
    } else if input.now_ms > input.cutoff_ms {
        (ReviewState::Expired, None)
    } else if input.parked {
        (ReviewState::Parked, None)
    } else if input.busy {
        (ReviewState::Busy, None)
    } else if !input.closed || !input.report_present || input.now_ms < input.close_ms {
        (ReviewState::AwaitingSealedReport, None)
    } else if deferred_until.is_some() {
        (ReviewState::Deferred, Some("supervisor_failure_backoff"))
    } else {
        (ReviewState::Due, None)
    };
    OwnerSchedule {
        version: 1,
        as_of_ms: input.now_ms,
        review_due: state == ReviewState::Due,
        review_state: state,
        review_reason: reason,
        deferred_until_ms: if state == ReviewState::Deferred {
            deferred_until
        } else {
            None
        },
    }
}

/// Pure admission input of the research lane. Admission and the read-only
/// dashboard schedule both use this one projection, so they cannot disagree.
/// It owns no queue, file, lease or worker and never starts a provider call.
#[derive(Clone)]
pub(crate) struct ResearchScheduleInput {
    pub enabled: bool,
    /// A research lane turn is in flight or awaiting recovery.
    pub pending: bool,
    /// False while the book is halted or stopped.
    pub book_runnable: bool,
    /// The first exhausted research lane cap, if any.
    pub capped: Option<&'static str>,
    /// Market close when `during_market` is false and the market is open.
    pub window_opens_ms: Option<i64>,
    /// Earliest next turn start allowed by `min_interval_ms`.
    pub next_allowed_ms: Option<i64>,
    /// Queued trading research requests.
    pub requests: usize,
    /// The configured quiet-trading trigger fired (#244).
    pub idle_due: bool,
    pub source_error: Option<&'static str>,
    pub evidence_ids: Vec<String>,
    pub unknown: Vec<&'static str>,
    pub now_ms: i64,
    pub cutoff_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ResearchSchedule {
    pub version: u32,
    pub as_of_ms: i64,
    pub research_due: bool,
    pub research_state: ResearchState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub research_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_until_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff_ms: Option<i64>,
    pub evidence_count: usize,
    /// Queued trading research requests; a request is served before evidence.
    pub request_count: usize,
    /// Public scalar workspace context captured with the SELECTED evidence at
    /// the producer completion sample. Raw refs, paths and file text are never
    /// retained or serialized. `None` means the source was not captured; a
    /// captured empty workspace is `Some(0)` with a valid empty-array hash.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_ref_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_sha256: Option<String>,
}
/// Schedule reason of a research turn started by the quiet-trading trigger.
pub(crate) const IDLE_TRIGGER_REASON: &str = "research_lane_idle_trigger";
pub(crate) fn project_research(input: &ResearchScheduleInput) -> ResearchSchedule {
    let window = input.window_opens_ms.filter(|until| input.now_ms < *until);
    let interval = input.next_allowed_ms.filter(|until| input.now_ms < *until);
    let deferred_until = window.or(interval);
    let (state, reason) = if !input.enabled {
        (ResearchState::Disabled, None)
    } else if input.pending {
        (ResearchState::Pending, None)
    } else if input.now_ms >= input.cutoff_ms {
        (ResearchState::Expired, None)
    } else if !input.book_runnable {
        (
            ResearchState::Parked,
            Some("research_lane_book_not_runnable"),
        )
    } else if let Some(cap) = input.capped {
        (ResearchState::Parked, Some(cap))
    } else if window.is_some() {
        (ResearchState::Deferred, Some("research_lane_market_hours"))
    } else if interval.is_some() {
        (ResearchState::Deferred, Some("research_lane_min_interval"))
    } else if input.requests > 0 || !input.evidence_ids.is_empty() {
        (ResearchState::Due, None)
    } else if input.idle_due {
        (ResearchState::Due, Some(IDLE_TRIGGER_REASON))
    } else if let Some(reason) = input.source_error {
        (ResearchState::Invalid, Some(reason))
    } else {
        (
            ResearchState::AwaitingEvidence,
            input
                .unknown
                .first()
                .copied()
                .or(Some("owner_research_no_eligible_evidence")),
        )
    };
    ResearchSchedule {
        version: 1,
        as_of_ms: input.now_ms,
        research_due: state == ResearchState::Due,
        research_state: state,
        research_reason: reason,
        evidence_id: (state == ResearchState::Due && input.requests == 0)
            .then(|| input.evidence_ids.first().cloned())
            .flatten(),
        deferred_until_ms: if state == ResearchState::Deferred {
            deferred_until
        } else {
            None
        },
        cutoff_ms: Some(input.cutoff_ms),
        evidence_count: input.evidence_ids.len(),
        request_count: input.requests,
        workspace_ref_count: None,
        workspace_snapshot_sha256: None,
    }
}
