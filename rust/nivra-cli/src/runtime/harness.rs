//! Role-isolated, one-shot Responses tool rounds over original usage authority.
use super::*;
pub(crate) const MAX_INSTRUMENT_KEYS: usize = 20;
const TOOL_OUTPUT_MAX_BYTES: usize = 64_000;
use planner::{Backend, CallGuard, ModelRole, PlannerConfig, ToolsSettings};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt;
mod tools;
pub(crate) use tools::*;
mod bridge;
pub(crate) use bridge::{Bridge, LaneGrant, LaneRequests, failure_reason};
mod bundle;
use bundle::*;
mod failure_window;
pub(crate) mod research;
pub(crate) use research::{
    LaunchBook, OwnerResearchRuntime, ResearchLaunch, ResearchSettings,
    capture_observation_tick as research_capture_observation,
    helper_entry as research_candidate_helper, observation_status as research_observation_status,
    status as research_status, tick_owner_tasks as research_tick_owner_tasks,
    validate_files as validate_research_files,
};
pub(crate) const DATA_OWNER_PROMPT: &str =
    include_str!("../../../../packaging/rust/prompts/data_owner.md");
pub(crate) const SCORER_PROMPT: &str = include_str!("../../../../packaging/rust/prompts/scorer.md");
pub(crate) const SUPERVISOR_PROMPT: &str =
    include_str!("../../../../packaging/rust/prompts/supervisor.md");
pub(crate) const ORCHESTRATOR_PROMPT: &str =
    include_str!("../../../../packaging/rust/prompts/orchestrator.md");
/// One exact project objective owned by the host and rendered into every role.
pub(crate) const OBJECTIVE: &str = include_str!("../../../../packaging/rust/prompts/objective.md");
/// Test-only helper for the historical prompt-shape tests.
#[cfg(test)]
pub(crate) fn objective() -> &'static str {
    OBJECTIVE.trim()
}
pub(crate) fn instructions(role: &str) -> Result<&'static str> {
    match role {
        "digest" => Ok(DATA_OWNER_PROMPT),
        "scorer" => Ok(SCORER_PROMPT),
        "supervisor" => Ok(SUPERVISOR_PROMPT),
        _ => Err("harness_role_invalid".into()),
    }
}

pub(crate) fn frame_instructions(frame: &Frame) -> Result<String> {
    frame_instructions_with(None, frame)
}

fn frame_instructions_with(cfg: Option<&PlannerConfig>, frame: &Frame) -> Result<String> {
    if frame.decision_evidence && frame.role != "digest" {
        return Err("harness_role_invalid".into());
    }
    let text = if let Some(instructions) = &frame.instructions {
        instructions.clone()
    } else if frame.final_answer_only {
        format!(
            "{}\nTools are disabled for this final answer. Return the strict final JSON; preserve the paper mandate, fee realism and evidence limitations.",
            system_instructions_with(cfg, &frame.role, &frame.citation_ids)?
        )
    } else {
        system_instructions_with(cfg, &frame.role, &frame.citation_ids)?
    };
    Ok(text)
}

pub(crate) fn objective_for(cfg: &PlannerConfig) -> &str {
    cfg.prompt_set()
        .map(|p| p.objective())
        .unwrap_or_else(|| OBJECTIVE.trim())
}

fn system_instructions_with(
    cfg: Option<&PlannerConfig>,
    role: &str,
    ids: &[String],
) -> Result<String> {
    // Explicit session instructions win, then the experiment-profile role
    // prompt. With a policy and neither override, the owner gets its compiled
    // standing prompt.
    let session_system_prompt = if role == "supervisor" {
        cfg.and_then(|cfg| cfg.orchestrator.as_ref())
            .filter(|orchestrator| orchestrator.system_prompt.is_some())
            .map(|orchestrator| {
                orchestrator
                    .session_prompts()
                    .map(|(prompt, _)| prompt.text)
            })
            .transpose()?
    } else {
        None
    };
    let profile_prompt = cfg
        .and_then(PlannerConfig::prompt_set)
        .and_then(|prompts| prompts.role_for(role));
    let owner_default = role == "supervisor" && cfg.is_some() && profile_prompt.is_none();
    let base = session_system_prompt
        .as_deref()
        .or(profile_prompt)
        .unwrap_or_else(|| {
            if owner_default {
                ORCHESTRATOR_PROMPT
            } else {
                instructions(role).unwrap_or("")
            }
        });
    let base = if base.is_empty() {
        instructions(role)?.to_owned()
    } else {
        base.to_owned()
    };
    // Only the retired post-close review roles carried citation IDs.
    if !ids.is_empty() {
        return Err("harness_citation_ids_invalid".into());
    }
    Ok(if matches!(role, "digest" | "scorer" | "supervisor") {
        let objective = cfg.map(objective_for).unwrap_or_else(|| OBJECTIVE.trim());
        format!("{objective}\n\n{base}")
    } else {
        base
    })
}

fn skip_false(value: &bool) -> bool {
    !*value
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerTools {
    pub research: bool,
    /// One bounded owner research turn over existing evidence. Omitted unless
    /// the reviewed research capability admitted this exact frame.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub research_turn: bool,
    /// Optional archived-receipt quote reader for a research turn. Omitted
    /// unless the reviewed quote-capture setting is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub quote_capture: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate: bool,
    /// Advertise the optional execution-spec argument only when the reviewed
    /// execution capability is enabled. Omitted from serialized frames when
    /// false so archived request bytes stay unchanged.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate_execution: bool,
    /// Advertise the optional prospective-observation argument only when the
    /// reviewed observation capability is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate_observation: bool,
    /// Advertise the optional typed cross-session parent reference only when
    /// the candidate capability is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate_parent_ref: bool,
    pub workspace: bool,
    /// Config-gated orchestrator web tools (#222). Omitted from serialized
    /// frames unless enabled so archived request bytes stay unchanged. The two
    /// availability flags record exactly which search/news tools were offered,
    /// so the provider-call reconstruction stays byte-identical.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub web: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub web_search: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub web_news: bool,
    /// Trading decision turns may post a typed request to the research lane
    /// (#239). Omitted unless the research lane is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub research_request: bool,
}
impl OwnerTools {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Frame {
    pub role: String,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
    #[serde(default, skip_serializing_if = "OwnerTools::is_empty")]
    pub owner_tools: OwnerTools,
    #[serde(default)]
    pub citation_ids: Vec<String>,
    /// Host-forced final answer. Tools are empty and the provider call omits them.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub final_answer_only: bool,
    /// Host-forced sealed owner review: no tools and review-only instructions.
    /// Omitted unless true so archived and legacy request bytes stay unchanged.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub review_only: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub decision_evidence: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_instrument_keys: Option<usize>,
    /// Host-supplied exact instructions for roles whose template is configured
    /// outside the compiled prompts (the scorer). Never includes credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Non-default call-time retrieval bounds. Absent keeps the historical
    /// default tool schemas byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_limits: Option<planner::ToolsSettings>,
}
mod digest;

/// Two independent readings for one admitted model turn: the actual observation
/// wall (model-visible timestamps) and the monotonic duration budget. They are
/// never mixed into a fabricated UTC value.
#[derive(Clone, Copy)]
pub(crate) struct DomainClock<'a, 'b> {
    guard: &'a CallGuard<'b>,
}

impl<'a, 'b> DomainClock<'a, 'b> {
    pub(crate) fn new(guard: &'a CallGuard<'b>) -> Self {
        Self { guard }
    }
    pub(crate) fn wall_ms(&self) -> i64 {
        self.guard.wall_ms()
    }
}

#[cfg(test)]
mod call_time_tool_tests;
#[cfg(test)]
mod lane_tool_tests;

/// Everything a call-time tool reads besides its own arguments.
pub(crate) struct ToolScope<'a> {
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    pub name: &'a str,
    pub role: &'a ModelRole,
    pub bundle: &'a Value,
    pub backend: &'a mut dyn Backend,
    pub guard: &'a CallGuard<'a>,
    pub archive: &'a Path,
    pub web_budget: &'a mut super::web_tools::WebToolBudget,
    pub web_transport: &'a mut dyn super::web_tools::Transport,
    pub run_id: &'a str,
    pub turn: usize,
    pub tool_limits: &'a ToolsSettings,
}

/// Execute one validated call. `key` names the call's archive files.
fn execute_tool(scope: &mut ToolScope<'_>, id: &str, key: &str, tool: Tool) -> Result<Value> {
    let clock = DomainClock::new(scope.guard);
    let now = || clock.wall_ms();
    let (c, cfg, name, role, bundle, guard, tool_limits) = (
        scope.c,
        scope.cfg,
        scope.name,
        scope.role,
        scope.bundle,
        scope.guard,
        scope.tool_limits,
    );
    match tool {
        Tool::Evidence(id) => Ok(json!({"evidence_id":id,"data":evidence(bundle,&id)})),
        Tool::Snapshot(keys) => match scope.backend.snapshot(bundle, &keys, now()) {
            Some(result) => result,
            None => snapshot(c, bundle, &keys, scope.archive, key),
        },
        Tool::Features(keys) => instrument_features(bundle, &keys),
        Tool::Context(keys) => scope
            .backend
            .intraday_context(&keys, now())
            .unwrap_or_else(|| Err("decision_evidence_unavailable".into())),
        Tool::OwnerResearchEnqueue(args) => owner_research_task(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::Enqueue(args),
            now(),
        ),
        Tool::OwnerResearchStatus(task_id, revision) => owner_research_task(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::Status(task_id, revision),
            now(),
        ),
        Tool::OwnerResearchResult(task_id, revision) => owner_research_task(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::Result(task_id, revision),
            now(),
        ),
        Tool::OwnerResearchCancel(task_id, revision) => owner_research_task(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::Cancel(task_id, revision),
            now(),
        ),
        Tool::CandidateSubmit(args) => owner_candidate_operation(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::CandidateSubmit(args),
            now(),
        ),
        Tool::CandidateStatus(task_id, revision) => owner_candidate_operation(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::CandidateStatus(task_id, revision),
            now(),
        ),
        Tool::CandidateResult(task_id, revision, observation_ref) => owner_candidate_operation(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::CandidateResult(task_id, revision, observation_ref),
            now(),
        ),
        Tool::CandidateCancel(task_id, revision) => owner_candidate_operation(
            c,
            cfg,
            name,
            role,
            bundle,
            guard,
            OwnerResearchOperation::CandidateCancel(task_id, revision),
            now(),
        ),
        Tool::QuoteCapture(keys, from_ms, to_ms) => {
            if !owner_research_turn(cfg, name, bundle, guard) {
                Err("harness_owner_research_turn_scope_invalid".into())
            } else {
                research_quote_capture(c, cfg, keys, from_ms, to_ms, now())
            }
        }
        Tool::ResearchRequest(args) => {
            match (
                research_request_allowed(cfg, name, bundle, guard),
                guard.invocation_id,
            ) {
                (true, Some(turn_id)) => super::lane_queue::post_request(
                    c,
                    cfg,
                    turn_id,
                    &args.question,
                    args.refs,
                    now(),
                ),
                _ => Err("harness_research_request_scope_invalid".into()),
            }
        }
        Tool::Scanner(limit, sort) => scanner_prospects(bundle, limit, &sort),
        Tool::Ranking(args) => rank_universe(bundle, &args),
        Tool::Portfolio => Ok(portfolio_view(bundle, now(), tool_limits.fill_limit)),
        Tool::History(limit) => Ok(recent_decisions(bundle, limit)),
        Tool::Session => Ok(session_status(c, bundle, name, role, now())),
        Tool::Workspace(op) => {
            if !owner_workspace_tools_allowed(cfg, name, bundle, guard) {
                Err("harness_workspace_scope_invalid".into())
            } else {
                let scope = if owner_research_turn(cfg, name, bundle, guard) {
                    super::workspace::WorkspaceScope::Research
                } else {
                    super::workspace::WorkspaceScope::Decision
                };
                super::workspace::execute_scoped(
                    &c.directory,
                    cfg.workspace.as_ref().ok_or("harness_workspace_disabled")?,
                    scope,
                    op,
                )
            }
        }
        Tool::Web(op) => {
            if !owner_web_tools_allowed(cfg, name, bundle, guard) {
                Err("harness_web_tools_scope_invalid".into())
            } else if let Some(settings) = cfg.web_tools.as_ref().filter(|web| web.enabled) {
                super::web_tools::execute_with(
                    &mut *scope.web_transport,
                    settings,
                    &mut *scope.web_budget,
                    &super::web_tools::CallContext {
                        session: c.session.to_string(),
                        run: scope.run_id.to_string(),
                        turn: scope.turn,
                        call: id.to_owned(),
                    },
                    op,
                    now(),
                    Some(&c.directory.join("model")),
                )
            } else {
                Err("harness_web_tools_disabled".into())
            }
        }
    }
}
