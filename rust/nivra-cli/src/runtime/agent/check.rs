//! `nivra agent-check`: one offline decision turn, its research request and
//! the next decision turn, end to end through the real agent process against
//! pi's scripted fake provider. No model network, no book, no trading.
use super::lanes::{LaneRunner, TurnContext};
use super::protocol::TurnStatus;
use super::{AgentClient, AgentSettings, ModelSpec};
use crate::runtime::planner::{Backend, ModelRole, PlannerConfig};
use crate::runtime::{Config, Result, lane_queue, millis};
use nivra_core::execution::{Instrument, Portfolio};
use nivra_core::finance::Amount;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Agent settings JSON, the same shape as the planner's `agent` block.
    #[arg(long)]
    config: PathBuf,
    /// Offline fixture: paper and planner config, evidence bundle, scripted model turns.
    #[arg(long)]
    fixture: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    /// Runtime config; `directory` is replaced by a fresh scratch directory.
    paper: Value,
    planner: Value,
    bundle: Value,
    cash: String,
    facts: Value,
    /// The agent's `--fake` script: scripted model responses per lane turn.
    script: Value,
    #[serde(default)]
    snapshot: Option<Value>,
}

/// Serves only the fixture's frozen quote snapshot; never a model.
pub(crate) struct FixtureBackend(pub Option<Value>);

impl Backend for FixtureBackend {
    fn call(&mut self, _role: &ModelRole, _prompt: &str) -> Result<String> {
        Err("agent_check_backend_has_no_model".into())
    }
    fn snapshot(&mut self, _bundle: &Value, keys: &[String], _at: i64) -> Option<Result<Value>> {
        self.0.clone().map(|mut snapshot| {
            snapshot["requested_keys"] = json!(keys);
            Ok(snapshot)
        })
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes = crate::read_bounded(path, 4_000_000).map_err(|_| "agent_check_unreadable")?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Resident and peak memory of a process, from `/proc/<pid>/status` in kB.
pub(crate) fn memory_kb(pid: u32) -> Value {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kb| kb.parse::<u64>().ok())
    };
    json!({"rss_kb":field("VmRSS:"),"peak_kb":field("VmHWM:")})
}

fn check(args: &Args) -> Result<(bool, Value)> {
    let settings: AgentSettings = read_json(&args.config)?;
    settings.validate()?;
    let fixture: Fixture = read_json(&args.fixture)?;
    let scratch = std::env::temp_dir().join(format!(
        "nivra-agent-check-{}-{}",
        std::process::id(),
        millis()
    ));
    let book = scratch.join("book");
    std::fs::create_dir_all(&book)?;
    let mut paper = fixture.paper;
    paper["directory"] = json!(book);
    let c: Config = serde_json::from_value(paper).map_err(|_| "agent_check_paper_invalid")?;
    let cfg: PlannerConfig =
        serde_json::from_value(fixture.planner).map_err(|_| "agent_check_planner_invalid")?;
    let script = scratch.join("script.json");
    std::fs::write(&script, serde_json::to_vec(&fixture.script)?)?;

    let mut client = AgentClient::spawn(&settings, &["--fake".into(), script.into()])?;
    let pid = client.pid().ok_or("agent_spawn_failed")?;
    let ready = client.ready.clone();
    let idle = memory_kb(pid);
    let mut portfolio = Portfolio::new();
    portfolio.costs = c.paper_costs.clone();
    portfolio.cash = Amount::parse(&fixture.cash)?;
    let instruments: Vec<Instrument> = serde_json::from_value(fixture.bundle["allowlist"].clone())?;
    let mut context = TurnContext {
        bundle: fixture.bundle,
        backend: Box::new(FixtureBackend(fixture.snapshot)),
        portfolio,
        instruments,
        web: Box::new(crate::runtime::web_tools::Network),
    };
    let clock = millis;
    let mut runner = LaneRunner::new(&mut client, &settings, &c, &cfg, &clock);
    runner.model_override = Some(ModelSpec {
        provider: "fake".into(),
        id: "scripted".into(),
        thinking: Some("off".into()),
    });
    let trigger = json!({"kind":"schedule","reason":"agent-check"});
    let first = runner.decision(trigger.clone(), fixture.facts.clone(), &mut context)?;
    let research = runner.research_requests(&mut context)?;
    let delivered_before = lane_queue::trading_updates(&c, millis())?.len();
    let second = runner.decision(trigger, fixture.facts, &mut context)?;
    let delivered_after = lane_queue::trading_updates(&c, millis())?.len();
    let busy = memory_kb(pid);
    drop(runner);
    client.shutdown()?;

    let summary = |turn: &super::lanes::LaneTurn| {
        json!({
            "turn_id":turn.outcome.id,"lane":turn.outcome.lane,"status":turn.outcome.status,
            "error":turn.outcome.error,"elapsed_ms":turn.outcome.elapsed_ms,"usage":turn.outcome.usage,
            "model":turn.outcome.model,"provider":turn.outcome.provider,
            "tool_calls":turn.outcome.tool_calls,"events":turn.outcome.events.len(),
            "plan_verdicts":turn.plan.as_ref().map(|plan| plan["verdicts"].clone()),
            "session_file":turn.outcome.session_file,
        })
    };
    let ok = first.outcome.status == TurnStatus::Ok
        && first.plan.is_some()
        && first
            .outcome
            .tool_calls
            .iter()
            .any(|call| call.name == "research" && call.ok)
        && !research.is_empty()
        && research
            .iter()
            .all(|turn| turn.outcome.status == TurnStatus::Ok)
        && delivered_before > 0
        && delivered_after == 0
        && second.outcome.status == TurnStatus::Ok;
    Ok((
        ok,
        json!({
            "agent_check": if ok {"ok"} else {"failed"},
            "scratch":scratch,
            "agent":{"pi_version":ready.pi_version,"node":ready.node,"startup_ms":ready.startup_ms,
                "memory_idle":idle,"memory_after_turns":busy},
            "decision":summary(&first),
            "research":research.iter().map(summary).collect::<Vec<_>>(),
            "research_updates_delivered":delivered_before,
            "next_decision":summary(&second),
        }),
    ))
}

pub(crate) fn run(args: Args) -> ExitCode {
    match check(&args) {
        Ok((ok, report)) => {
            println!("{report}");
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("agent_check_failed: {error}");
            ExitCode::FAILURE
        }
    }
}
