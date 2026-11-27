//! The generic tool bridge: pi-facing names, their Rust executors and lanes.
use super::Lane;
use super::client::ToolHost;
use super::protocol::Usage;
use crate::runtime::Result;
use crate::runtime::harness::{Bridge, bridge_definitions, failure_reason};
use crate::runtime::planner::{self, PlannerConfig};
use nivra_core::execution::{Instrument, Portfolio};
use nivra_core::market::{InstrumentKind, OptionCatalog};
use serde::Serialize;
use serde_json::{Value, json};

const D: Lane = Lane::Decision;
const R: Lane = Lane::Research;
const V: Lane = Lane::Review;
const G: Lane = Lane::Gate;

pub(crate) struct BridgeTool {
    /// The name pi and the model see.
    pub name: &'static str,
    /// The existing Rust tool that executes it; `None` for `plan` and
    /// `verdict`, which the lane host answers itself.
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

/// One table maps every pi name to its Rust executor and the lanes that may
/// call it, so renaming the Rust internals later is mechanical. `research`
/// stays out of the research lane: a research turn may not queue research for
/// itself. Writes, runs, installs and backtests belong to the research lane.
pub(crate) const TOOLS: &[BridgeTool] = &[
    tool("evidence", "read_evidence", &[D, R, V, G]),
    tool("snapshot", "get_market_snapshot", &[D, R, V, G]),
    tool("prospects", "get_scanner_prospects", &[D, R, V, G]),
    tool("rank", "rank_universe", &[D, R, V, G]),
    tool("features", "get_instrument_features", &[D, R, V, G]),
    tool("intraday", "get_intraday_context", &[D, R, V, G]),
    tool("portfolio", "get_portfolio", &[D, R, V, G]),
    tool("decisions", "get_recent_decisions", &[D, R, V]),
    tool("session", "get_session_status", &[D, R, V, G]),
    tool("analysis", "research_task_enqueue", &[D, R, V]),
    BridgeTool {
        name: "plan",
        rust: None,
        lanes: &[D],
    },
    BridgeTool {
        name: "strategy_select",
        rust: None,
        lanes: &[D],
    },
    BridgeTool {
        name: "verdict",
        rust: None,
        lanes: &[G],
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

fn strategy_select_definition() -> Value {
    json!({
        "name":"strategy_select",
        "description":"Select the action strategy the executor runs: an accepted id from strategies.eligible, a configured fallback as fallback:<name> from strategies.fallback, or strategy_id: null to run no strategy. Appends a lane selection with your reason to the library control log. An owner pin or disable always overrides it, and a runnable accepted version always wins over a fallback. Choosing a strategy never places an order.",
        "parameters":{
            "type":"object",
            "properties":{
                "strategy_id":{
                    "type":["string","null"],
                    "maxLength":64,
                    "description":"Strategy ID (or unique 8+ char prefix) from strategies.eligible, fallback:<name> from strategies.fallback, or null to stand down."
                },
                "reason":{
                    "type":"string",
                    "minLength":1,
                    "maxLength":400,
                    "description":"Why this strategy (or none) fits the current session and regime."
                }
            },
            "required":["strategy_id","reason"],
            "additionalProperties":false
        }
    })
}

fn verdict_definition() -> Value {
    json!({
        "name":"verdict",
        "description":"Finish the screen. escalate=true runs the decision lane now with your prospects, best first; escalate=false lets the triggers pass without a decision turn. The host keeps only prospects whose key is in the current option universe and whose idea matches the contract. It never sizes, plans or trades.",
        "parameters":{
            "type":"object",
            "properties":{
                "escalate":{"type":"boolean"},
                "reason":{"type":"string","minLength":1,"maxLength":400},
                "prospects":{"type":"array","maxItems":MAX_PROSPECTS,"description":"Ranked F&O prospects for the decision lane, best first. Use [] when you do not escalate.","items":{
                    "type":"object",
                    "properties":{
                        "instrument_key":{"type":"string","maxLength":256,"description":"An NSE_FO key from the brief's fo.rows."},
                        "idea":{"type":"string","enum":["buy_call","buy_put"]},
                        "evidence":{"type":"string","minLength":1,"maxLength":MAX_PROSPECT_TEXT,"description":"The numbers: spot or premium move, OI shift, spread in ticks, depth in lots, days to expiry, lot cash against book cash."},
                        "why":{"type":"string","minLength":1,"maxLength":MAX_PROSPECT_TEXT,"description":"Why it deserves a decision turn now."}
                    },
                    "required":["instrument_key","idea","evidence","why"],
                    "additionalProperties":false
                }}
            },
            "required":["escalate","reason","prospects"],
            "additionalProperties":false
        }
    })
}

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
                            "properties":{"target":decimal,"stop":decimal,"time":{"type":["integer","null"],"description":"Absolute Unix epoch milliseconds. For a buy, the time must be in the future. Use null to omit a time exit."}},
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
/// description under the pi name, for every tool this config enables and the
/// lane can serve. `requests` is true when a research lane answers requests.
pub(crate) fn lane_definitions(
    cfg: &PlannerConfig,
    lane: Lane,
    requests: bool,
) -> Result<Vec<Value>> {
    let rust = bridge_definitions(cfg, requests)?;
    Ok(TOOLS
        .iter()
        .filter(|tool| tool.lanes.contains(&lane))
        .filter_map(|tool| match tool.rust {
            None if tool.name == "verdict" => Some(verdict_definition()),
            None if tool.name == "strategy_select" => {
                cfg.runs_strategies().then(strategy_select_definition)
            }
            None => Some(plan_definition()),
            Some(name) => rust.iter().find(|definition| definition["name"] == name).map(
                |definition| {
                    let description = match tool.name {
                        "write" => json!("Write UTF-8 notes or skills, or author a Python ranker at scripts/<name>.py."),
                        "run" => json!("Run a Python ranker at scripts/<name>.py in the no-network sandbox. Supply input as a JSON object encoded as a string, with one feature object per instrument key, and set input_path to null. Read the JSON from sys.argv[1] and print one JSON object mapping input instrument keys to finite scores. Reviewed allowlisted scripts also run offline and may use a workspace input_path or sibling params. All scores are advisory evidence."),
                        _ => definition["description"].clone(),
                    };
                    json!({"name":tool.name,"description":description,"parameters":definition["parameters"]})
                },
            ),
        })
        .collect())
}

/// The last `plan` submitted in a turn. A refused plan supersedes an earlier
/// validated one: the turn's answer is its last attempt.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum PlanAttempt {
    Validated { plan: Value, verdicts: Value },
    Refused { plan: Value, error: String },
}

type UsageCheckpoint<'a> = Box<dyn FnMut(&Usage) + Send + 'a>;

/// Serves one lane turn: the lane allowlist first, then the existing Rust
/// validation and executor, or the existing plan validation for `plan`.
pub(crate) struct LaneHost<'a> {
    pub lane: Lane,
    pub clock: &'a (dyn Fn() -> i64 + Send + Sync),
    pub bridge: Bridge<'a>,
    pub plan_inputs: Option<(&'a Portfolio, &'a [Instrument])>,
    pub plan: Option<PlanAttempt>,
    /// The gate lane's last `verdict` this turn.
    pub verdict: Option<Verdict>,
    /// The option universe a gate prospect must name. Gate turns only.
    pub options: Option<&'a OptionCatalog>,
    /// Called with the turn's usage so far, so a crash still charges it.
    pub on_usage: Option<UsageCheckpoint<'a>>,
}

/// The gate's answer for one batch of F&O triggers.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub(crate) struct Verdict {
    pub escalate: bool,
    pub reason: String,
    /// Validated prospects, best first; empty unless `escalate`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prospects: Vec<Prospect>,
}

/// Prospects one escalation may carry.
pub(crate) const MAX_PROSPECTS: usize = 5;
const MAX_PROSPECT_TEXT: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Idea {
    BuyCall,
    BuyPut,
}

/// One F&O prospect the gate hands the decision lane. The contract terms come
/// from the option universe, never from the model.
#[derive(Clone, Debug, PartialEq, Serialize, serde::Deserialize)]
pub(crate) struct Prospect {
    pub rank: usize,
    pub instrument_key: String,
    pub idea: Idea,
    pub strike: String,
    pub expiry: String,
    pub lot_size: u64,
    pub evidence: String,
    pub why: String,
}

/// Keep the prospects that name a contract in the option universe with a
/// matching idea, in the gate's order, at most `MAX_PROSPECTS`. Every other
/// entry is dropped with its reason.
pub(crate) fn validate_prospects(
    raw: &[Value],
    catalog: Option<&OptionCatalog>,
) -> (Vec<Prospect>, Vec<Value>) {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Raw {
        instrument_key: String,
        idea: Idea,
        evidence: String,
        why: String,
    }
    let text = |value: &str| {
        let value = value.trim();
        (!value.is_empty() && value.chars().count() <= MAX_PROSPECT_TEXT).then(|| {
            crate::runtime::transcript::redact(&json!(value))
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
    };
    let mut kept: Vec<Prospect> = Vec::new();
    let mut dropped = Vec::new();
    for entry in raw {
        let key = entry["instrument_key"].as_str().unwrap_or_default();
        let reason = match serde_json::from_value::<Raw>(entry.clone()) {
            Err(_) => "prospect_invalid",
            Ok(_) if kept.len() >= MAX_PROSPECTS => "prospect_limit",
            Ok(raw) => match (
                catalog.and_then(|c| c.contracts.get(&raw.instrument_key)),
                text(&raw.evidence).zip(text(&raw.why)),
            ) {
                (None, _) => "prospect_not_in_option_universe",
                (_, None) => "prospect_invalid",
                (Some(_), _) if kept.iter().any(|p| p.instrument_key == raw.instrument_key) => {
                    "prospect_duplicate"
                }
                (Some(contract), Some((evidence, why))) => {
                    let idea = match contract.option {
                        InstrumentKind::Call => Idea::BuyCall,
                        _ => Idea::BuyPut,
                    };
                    if idea != raw.idea {
                        "prospect_idea_mismatch"
                    } else {
                        kept.push(Prospect {
                            rank: kept.len() + 1,
                            instrument_key: contract.key.clone(),
                            idea,
                            strike: contract.strike.amount().to_string(),
                            expiry: contract.expiry.to_string(),
                            lot_size: contract.lot_size,
                            evidence,
                            why,
                        });
                        continue;
                    }
                }
            },
        };
        dropped.push(
            json!({"instrument_key":key.chars().take(64).collect::<String>(),"reason":reason}),
        );
    }
    (kept, dropped)
}

impl LaneHost<'_> {
    fn verdict(&mut self, args: &Value) -> std::result::Result<Value, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            escalate: bool,
            reason: String,
            #[serde(default)]
            prospects: Vec<Value>,
        }
        let args: Args =
            serde_json::from_value(args.clone()).map_err(|_| "agent_verdict_invalid")?;
        let reason = args.reason.trim();
        if reason.is_empty() || reason.chars().count() > 400 {
            return Err("agent_verdict_invalid".into());
        }
        let (prospects, dropped) = if args.escalate {
            validate_prospects(&args.prospects, self.options)
        } else {
            (Vec::new(), Vec::new())
        };
        let verdict = Verdict {
            escalate: args.escalate,
            reason: crate::runtime::transcript::redact(&json!(reason))
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            prospects,
        };
        self.verdict = Some(verdict.clone());
        let kept: Vec<&str> = verdict
            .prospects
            .iter()
            .map(|p| p.instrument_key.as_str())
            .collect();
        Ok(
            json!({"status":"recorded","escalate":verdict.escalate,"prospects":kept,"dropped":dropped}),
        )
    }

    fn strategy_select(&mut self, args: &Value) -> std::result::Result<Value, String> {
        if !self.bridge.cfg.runs_strategies() {
            return Err("strategy_execution_disabled".into());
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Args {
            strategy_id: Option<String>,
            reason: String,
        }
        let parsed: Args =
            serde_json::from_value(args.clone()).map_err(|_| "agent_strategy_select_invalid")?;
        let reason = parsed.reason.trim();
        if reason.is_empty() || reason.chars().count() > 400 {
            return Err("agent_strategy_select_invalid".into());
        }
        let (redacted, findings) = crate::runtime::transcript::redact_with_findings(&json!(reason));
        if !findings.matches.is_empty() || redacted.as_str() != Some(reason) {
            crate::runtime::diagnostics::secret_guard(
                &self.bridge.c.directory,
                (self.clock)(),
                "strategy_select",
                "blocked",
                Some(self.bridge.run_id),
                &findings,
            );
            return Err("agent_strategy_select_secret_refused".into());
        }
        let target = match parsed.strategy_id.as_deref().map(str::trim) {
            Some("") => return Err("agent_strategy_select_invalid".into()),
            Some(s) if s.eq_ignore_ascii_case("none") => None,
            other => other,
        };
        let root = crate::runtime::strategy_library::root(self.bridge.c)
            .map_err(|_| "strategy_library_unavailable".to_owned())?;
        let resolved = crate::runtime::strategy_library::Library::at(root)
            .select_by_lane(
                target,
                &self
                    .bridge
                    .cfg
                    .strategy_policy()
                    .map_err(|e| failure_reason(e.as_ref()))?,
                reason,
                Some(self.bridge.run_id),
                (self.clock)(),
            )
            .map_err(|e| failure_reason(e.as_ref()))?;
        let selected_id = resolved
            .lane_selection
            .as_ref()
            .and_then(|s| s.strategy_id.as_deref());
        let overridden = resolved.pinned.is_some() && resolved.active.as_deref() != selected_id;
        Ok(json!({
            "status": "recorded",
            "selected": resolved.lane_selection,
            "active": resolved.active,
            "owner_pinned": resolved.pinned,
            "overridden_by_owner_pin": overridden,
        }))
    }

    fn plan(&mut self, args: &Value) -> std::result::Result<Value, String> {
        let (portfolio, instruments) = self.plan_inputs.ok_or("agent_plan_unavailable")?;
        let strategy_owns_cash =
            crate::runtime::wide::strategy_run::supervision_state(self.bridge.c, self.bridge.cfg)
                .strategy_active();
        match planner::validate_for_decision(
            &args.to_string(),
            portfolio,
            instruments,
            (self.clock)(),
            strategy_owns_cash,
        ) {
            Ok(verdicts) => {
                if verdicts
                    .iter()
                    .any(|v| v.reason == "action_strategy_owns_cash_entries")
                {
                    let error = "agent_paper_strategy_owns_cash_entries".to_owned();
                    self.plan = Some(PlanAttempt::Refused {
                        plan: args.clone(),
                        error: error.clone(),
                    });
                    return Err(error);
                }
                let verdicts = json!(verdicts);
                self.plan = Some(PlanAttempt::Validated {
                    plan: args.clone(),
                    verdicts: verdicts.clone(),
                });
                Ok(json!({
                    "status":"validated",
                    "verdicts":verdicts,
                    "execution_authorized":false,
                    "note":"A validated plan is a proposal. The host decides execution against a fresh quote."
                }))
            }
            Err(error) => {
                let error = failure_reason(error.as_ref());
                self.plan = Some(PlanAttempt::Refused {
                    plan: args.clone(),
                    error: error.clone(),
                });
                Err(error)
            }
        }
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
            None if tool.name == "verdict" => self.verdict(args),
            None if tool.name == "strategy_select" => self.strategy_select(args),
            None => self.plan(args),
        }
    }
    fn usage(&mut self, usage: &Usage) {
        if let Some(record) = self.on_usage.as_mut() {
            record(usage);
        }
    }
}
