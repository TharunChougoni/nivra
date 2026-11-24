//! Role-isolated, one-shot Responses tool rounds over original usage authority.
use super::*;
pub(crate) const MAX_INSTRUMENT_KEYS: usize = 20;
const TOOL_OUTPUT_MAX_BYTES: usize = 64_000;
use planner::{Backend, CallGuard, PlannerConfig, ToolsSettings};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt;
mod tools;
pub(crate) use tools::*;
mod bridge;
pub(crate) use bridge::{Bridge, LaneGrant, LaneRequests, failure_reason};
mod bundle;
use bundle::*;
pub(crate) mod research;
pub(crate) use research::{
    LaunchBook, ResearchLaunch, ResearchRuntime, ResearchSettings,
    helper_entry as research_candidate_helper, observation_status as research_observation_status,
    status as research_status, tick_tasks as research_tick_tasks,
    validate_files as validate_research_files,
};
fn skip_false(value: &bool) -> bool {
    !*value
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GrantedTools {
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

#[cfg(test)]
mod call_time_tool_tests;
#[cfg(test)]
mod lane_tool_tests;

/// Everything a call-time data tool reads besides its own arguments.
pub(crate) struct ToolScope<'a> {
    pub c: &'a Config,
    pub bundle: &'a Value,
    pub backend: &'a mut dyn Backend,
    pub guard: &'a CallGuard<'a>,
    pub archive: &'a Path,
    pub tool_limits: &'a ToolsSettings,
}

/// Execute one validated read-only data tool. The lane bridge runs every tool
/// whose authority a lane grants before it reaches this executor.
fn execute_tool(scope: &mut ToolScope<'_>, key: &str, tool: Tool) -> Result<Value> {
    let now = scope.guard.wall_ms();
    let (c, bundle) = (scope.c, scope.bundle);
    match tool {
        Tool::Evidence(id) => Ok(json!({"evidence_id":id,"data":evidence(bundle,&id)})),
        Tool::Snapshot(keys) => match scope.backend.snapshot(bundle, &keys, now) {
            Some(result) => result,
            None => snapshot(c, bundle, &keys, scope.archive, key),
        },
        Tool::Features(keys) => instrument_features(bundle, &keys),
        Tool::Context(keys) => scope
            .backend
            .intraday_context(&keys, now)
            .unwrap_or_else(|| Err("decision_evidence_unavailable".into())),
        Tool::Scanner(limit, sort) => scanner_prospects(bundle, limit, &sort),
        Tool::Ranking(args) => rank_universe(bundle, &args),
        Tool::Portfolio => Ok(portfolio_view(bundle, now, scope.tool_limits.fill_limit)),
        Tool::History(limit) => Ok(recent_decisions(bundle, limit)),
        Tool::Session => Ok(session_status(c, bundle, now)),
        _ => Err("harness_tool_not_granted".into()),
    }
}
