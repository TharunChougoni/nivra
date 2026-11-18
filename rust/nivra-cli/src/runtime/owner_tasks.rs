//! Owner work is projected from original domain evidence, never queued here.
use super::workspace::WorkspaceRefV1;
use super::*;
use nivra_core::postclose::SealedEvidence;
use serde_json::{Value, json};

/// Earliest post-close owner-task cutoff config validation accepts: one minute
/// after the default session close. The runtime cutoff check
/// (`settings_cutoff`) compares against the configured close, so a book with a
/// later close still refuses a cutoff at or before it (#234).
pub(crate) const MIN_CUTOFF_MINUTE: u32 = nivra_core::execution::DEFAULT_SESSION_END_MINUTE + 1;
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
impl IdleTriggerSettings {}
/// Reviewed profile of the research lane. It is never a trading cap: it bounds
/// only the research lane's own model turns, which run beside the trading lane
/// and can never occupy its worker slot or charge its allowance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ResearchLaneSettings {
    /// `None` uses the orchestrator's active model profile.
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
            if !cfg
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
            && !cfg
                .role(super::orchestrator_model::Trigger::Supervisor)
                .enabled
        {
            return Err("owner_review_requires_decision_owner".into());
        }
        if let Some(research) = &self.research {
            research.validate(cfg)?;
        }
        if self.agenda.is_some_and(|a| a.pre_open || a.pre_close)
            && !cfg
                .role(super::orchestrator_model::Trigger::Supervisor)
                .enabled
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
impl ResearchEvidenceKind {}
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
