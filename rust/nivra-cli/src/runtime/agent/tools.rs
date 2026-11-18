//! The generic tool bridge: pi-facing names, their Rust executors and lanes.
use super::Lane;
use super::client::ToolHost;
use crate::runtime::Result;
use crate::runtime::harness::{Bridge, bridge_definitions, failure_reason};
use crate::runtime::planner::{self, PlannerConfig};
use nivra_core::execution::{Instrument, Portfolio};
use serde_json::{Value, json};

const D: Lane = Lane::Decision;
const R: Lane = Lane::Research;
const V: Lane = Lane::Review;

pub(crate) struct BridgeTool {
    /// The name pi and the model see.
    pub name: &'static str,
    /// The existing Rust tool that executes it; `None` for `plan`.
    pub rust: Option<&'static str>,
    pub lanes: &'static [Lane],
}

const fn tool(name: &'static str, rust: &'static str, lanes: &'static [Lane]) -> BridgeTool {
    BridgeTool {
        name,
        rust: Some(rust),
        lanes,
    }
}

/// One table maps every pi name to its Rust executor, so renaming the Rust
/// internals later is mechanical. `research` stays out of the research lane:
/// a research turn may not queue research for itself.
pub(crate) const TOOLS: &[BridgeTool] = &[
    tool("evidence", "read_evidence", &[D, R, V]),
    tool("snapshot", "get_market_snapshot", &[D, R, V]),
    tool("prospects", "get_scanner_prospects", &[D, R, V]),
    tool("rank", "rank_universe", &[D, R, V]),
    tool("features", "get_instrument_features", &[D, R, V]),
    tool("intraday", "get_intraday_context", &[D, R, V]),
    tool("portfolio", "get_portfolio", &[D, R, V]),
    tool("decisions", "get_recent_decisions", &[D, R, V]),
    tool("session", "get_session_status", &[D, R, V]),
    tool("analysis", "research_task_enqueue", &[D, R, V]),
    BridgeTool {
        name: "plan",
        rust: None,
        lanes: &[D],
    },
    tool("research", "request_research", &[D]),
    tool("research_status", "research_task_status", &[D, R, V]),
    tool("research_result", "research_task_result", &[D, R, V]),
    tool("research_cancel", "research_task_cancel", &[R]),
    tool("quotes", "research_quote_capture", &[R]),
    tool("run", "workspace_run", &[R]),
    tool("backtest", "candidate_submit", &[R]),
    tool("backtest_status", "candidate_status", &[R]),
    tool("backtest_result", "candidate_result", &[R]),
    tool("backtest_cancel", "candidate_cancel", &[R]),
    tool("list", "workspace_list", &[D, R, V]),
    tool("read", "workspace_read", &[D, R, V]),
    tool("write", "workspace_write", &[R]),
    tool("install", "workspace_install", &[R]),
    tool("search", "web_search", &[D, R]),
    tool("fetch", "web_fetch", &[D, R]),
    tool("news", "news_lookup", &[D, R]),
];

fn plan_definition() -> Value {
    let decimal = json!({"type":["string","null"],"maxLength":32});
    json!({
        "name":"plan",
        "description":"Submit this turn's paper TradePlan. The host validates identity, allowlist, cash, fees, sizes and exits and returns per-action verdicts; it never places an order. Use actions:[] to hold or abstain. If the plan is refused, fix the reported problem and submit again.",
        "parameters":{
            "type":"object",
            "properties":{
                "actions":{"type":"array","maxItems":200,"items":{
                    "type":"object",
                    "properties":{
                        "instrument_key":{"type":"string","maxLength":256},
                        "side":{"type":"string","enum":["buy","sell","revise_exit"]},
                        "rupees":decimal,
                        "units":{"type":["integer","null"],"minimum":0},
                        "max_price":decimal,
                        "rationale":{"type":"string","maxLength":2048},
                        "exit_intent":{
                            "type":"object",
                            "properties":{"target":decimal,"stop":decimal,"time":{"type":["integer","null"]}},
                            "required":["target","stop","time"],
                            "additionalProperties":false
                        }
                    },
                    "required":["instrument_key","side","rupees","units","max_price","rationale","exit_intent"],
                    "additionalProperties":false
                }},
                "stance":{"type":"string","maxLength":256},
                "notes":{"type":"string","maxLength":8192},
                "candidate_reasons":{"type":"array","maxItems":200,"items":{
                    "type":"object",
                    "properties":{"instrument_key":{"type":"string"},"reason":{"type":"string"}},
                    "required":["instrument_key","reason"],
                    "additionalProperties":false
                }}
            },
            "required":["actions","stance","notes"],
            "additionalProperties":false
        }
    })
}

/// The lane's tools as pi custom-tool specs: the existing Rust schema and
/// description under the pi name, for every tool this config enables.
pub(crate) fn lane_definitions(cfg: &PlannerConfig, lane: Lane) -> Result<Vec<Value>> {
    let rust = bridge_definitions(cfg)?;
    Ok(TOOLS
        .iter()
        .filter(|tool| tool.lanes.contains(&lane))
        .filter_map(|tool| match tool.rust {
            None => Some(plan_definition()),
            Some(name) => rust.iter().find(|definition| definition["name"] == name).map(
                |definition| {
                    json!({"name":tool.name,"description":definition["description"],"parameters":definition["parameters"]})
                },
            ),
        })
        .collect())
}

/// Serves one lane turn: the lane allowlist first, then the existing Rust
/// validation and executor, or the existing plan validation for `plan`.
pub(crate) struct LaneHost<'a> {
    pub lane: Lane,
    pub bridge: Bridge<'a>,
    pub plan_inputs: Option<(&'a Portfolio, &'a [Instrument])>,
    /// The last plan Rust validated in this turn, with its verdicts.
    pub plan: Option<Value>,
}

impl LaneHost<'_> {
    fn plan(&mut self, args: &Value) -> std::result::Result<Value, String> {
        let (portfolio, instruments) = self.plan_inputs.ok_or("agent_plan_unavailable")?;
        let verdicts = planner::validate(&args.to_string(), portfolio, instruments)
            .map_err(|error| failure_reason(error.as_ref()))?;
        self.plan = Some(json!({"plan":args,"verdicts":verdicts}));
        Ok(json!({
            "status":"validated",
            "verdicts":verdicts,
            "execution_authorized":false,
            "note":"A validated plan is a proposal. The host decides execution against a fresh quote."
        }))
    }
}

impl ToolHost for LaneHost<'_> {
    fn call(
        &mut self,
        call_id: &str,
        name: &str,
        args: &Value,
    ) -> std::result::Result<Value, String> {
        let tool = TOOLS
            .iter()
            .find(|tool| tool.name == name && tool.lanes.contains(&self.lane))
            .ok_or("agent_tool_not_in_lane")?;
        match tool.rust {
            Some(rust) => self.bridge.call(call_id, rust, args),
            None => self.plan(args),
        }
    }
}
