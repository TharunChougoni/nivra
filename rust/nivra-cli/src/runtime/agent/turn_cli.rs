//! `nivra agent-turn`: one live lane turn against the current controller state,
//! for the owner to try a model on the VM. Shadow semantics: the book is read
//! only, a plan is validated and printed, nothing is executed. The turn's
//! usage is charged to the lane's daily allowance like any other turn.
use super::brief::{self, Brief, Facts, SessionFacts};
use super::lanes::{self, LaneRunner, TurnContext};
use super::protocol::TurnStatus;
use super::service::{LaneBackend, ranking};
use super::tools::lane_definitions;
use super::triggers::{Phase, Trigger, TriggerKind};
use super::{Agent, AgentClient, Lane};
use crate::runtime::planner::{self, PlannerConfig};
use crate::runtime::{Config, Result, millis, minute, rollover, wide};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum LaneArg {
    Decision,
    Research,
    Review,
    Gate,
}

#[derive(clap::Args)]
pub(crate) struct Args {
    /// The paper runtime config the controller runs with.
    #[arg(long)]
    config: PathBuf,
    #[arg(long, value_enum)]
    lane: LaneArg,
    /// Give a manual research turn a specific task instead of draining queued requests.
    #[arg(long)]
    research_question: Option<String>,
    /// Print the brief and the published tools; start no agent and call no model.
    #[arg(long)]
    dry: bool,
}

fn turn(args: &Args) -> Result<(bool, Value)> {
    let lane = match args.lane {
        LaneArg::Decision => Lane::Decision,
        LaneArg::Research => Lane::Research,
        LaneArg::Review => Lane::Review,
        LaneArg::Gate => Lane::Gate,
    };
    let question = args.research_question.as_deref().map(str::trim);
    if question.is_some_and(|question| {
        lane != Lane::Research || question.is_empty() || question.len() > 2048
    }) {
        return Err("agent_research_question_invalid".into());
    }
    let bytes = crate::read_bounded(&args.config, 16_384).map_err(|_| "config_unreadable")?;
    let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
    c.validate()?;
    let _lock = if args.dry {
        None
    } else {
        Some(rollover::controller_lock(&c)?)
    };
    let c = rollover::current(&c)?;
    let cfg = PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_missing")?,
        c.session,
    )?;
    let settings = cfg.agent.clone().ok_or("agent_settings_missing")?;
    settings.lane(lane)?;
    let db = crate::runtime::open_book_current_read_only(&c.directory.join("paper.sqlite"))?;
    let portfolio = wide::stored_portfolio(&db, &c)?.ok_or("agent_turn_book_unavailable")?;
    let instruments = wide::history_instruments(&c)?;
    let now = millis();
    let live = match cfg.discovery.as_ref() {
        Some(discovery) => {
            let scanner: nivra_core::execution::Scanner = db
                .query_row("SELECT body FROM scanner WHERE id=1", [], |r| {
                    r.get::<_, String>(0)
                })
                .ok()
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            ranking(&crate::runtime::discovery::project(
                &scanner,
                &instruments,
                discovery,
                now,
            ))
        }
        None => Vec::new(),
    };
    let triggers = [Trigger::new(TriggerKind::Schedule, "agent_turn", now)];
    let memory = lanes::memory(&c, lane);
    let facts = Facts {
        session: SessionFacts {
            date: c.session.to_string(),
            phase: Phase::at(
                minute(now),
                c.execution.session_start_minute,
                settings.triggers().decision.pre_close_minute,
                c.execution.session_end_minute,
                false,
            ),
            minute: minute(now),
            now_ms: now,
        },
        portfolio: &portfolio,
        ranking: &live,
        memory: memory.as_ref(),
        execution: "shadow_never_executed",
    };
    let lane_settings = settings.lane(lane)?;
    let spent = lanes::spend(&c).remove(&lane).unwrap_or_default();
    let budget = brief::budget(
        lane,
        &spent,
        lane_settings.daily_calls,
        lane_settings.daily_cost_usd,
        (0.0, settings.triggers().decision.top_k),
    );
    let task = match lane {
        Lane::Decision => Value::Null,
        Lane::Research => match question {
            Some(question) => json!({"kind":"manual","question":question}),
            None => json!({"kind":"manual"}),
        },
        Lane::Review => json!({"kind":"review","session":c.session.to_string()}),
        Lane::Gate => json!({"kind":"gate"}),
    };
    let brief = Brief::build(&facts, budget, task, &triggers);
    if args.dry {
        let tools: Vec<Value> = lane_definitions(
            &cfg,
            lane,
            lane == Lane::Decision && settings.lanes.contains_key(&Lane::Research),
        )?
        .into_iter()
        .map(|tool| tool["name"].clone())
        .collect();
        return Ok((
            true,
            json!({"lane":lane,"model":lane_settings.model,"tools":tools,
            "brief":serde_json::from_str::<Value>(&brief.render()?)?}),
        ));
    }
    if matches!(lane, Lane::Decision | Lane::Gate)
        && (crate::runtime::local(now).date_naive() != c.session
            || !brief.session.phase.trading()
            || portfolio.halted
            || portfolio.stopped
            || portfolio.closed
            || (live.is_empty()
                && !portfolio.positions.values().any(|position| {
                    now.checked_sub(position.marked_at).is_some_and(|age| {
                        (0..=nivra_core::market::MAX_FINANCIAL_AGE_MS).contains(&age)
                    })
                })))
    {
        return Err("agent_no_eligible_evidence".into());
    }
    if lane == Lane::Review
        && (!portfolio.closed || !lanes::sealed_review_source(&c, &c.directory, c.session))
    {
        return Err("agent_review_requires_sealed_report".into());
    }
    let (bundle, _) = planner::call_time_input(&c, &cfg, &portfolio, &instruments, &|| now)?;
    let mut context = TurnContext {
        bundle,
        backend: Box::new(LaneBackend(None)),
        portfolio: portfolio.clone(),
        instruments,
        web: Box::new(crate::runtime::web_tools::Network),
    };
    let spawn_settings = settings.clone();
    let mut agent = Agent::new(Box::new(move || AgentClient::spawn(&spawn_settings, &[])));
    let clock = millis;
    let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
    let trigger = json!({"kind":"schedule","reason":"agent_turn","triggers":triggers});
    let turns = match lane {
        Lane::Decision => vec![runner.decision(trigger, brief, &mut context)?],
        Lane::Research if question.is_none() && !runner.requests.queued(now, 1)?.is_empty() => {
            runner.research_requests(&trigger, &brief, &mut context)?
        }
        Lane::Research | Lane::Gate => {
            vec![runner.turn(lane, trigger, json!(brief.render()?), &mut context)?]
        }
        Lane::Review => {
            if spent.calls >= lane_settings.daily_calls
                || spent.cost_usd >= lane_settings.daily_cost_usd
            {
                return Err("agent_budget_exhausted".into());
            }
            if !lanes::claim_review(&c)? {
                return Err("agent_review_already_ran".into());
            }
            vec![runner.review(trigger, &brief, &mut context)?]
        }
    };
    drop(runner);
    agent.shutdown()?;
    let ok = !turns.is_empty()
        && turns
            .iter()
            .all(|turn| turn.outcome.status == TurnStatus::Ok);
    Ok((
        ok,
        crate::runtime::transcript::redact(
            &json!({"lane":lane,"execution":"shadow_never_executed","turns":turns.iter().map(|turn| json!({
        "turn_id":turn.outcome.id,"status":turn.outcome.status,"error":turn.outcome.error,
        "model":turn.outcome.model,"provider":turn.outcome.provider,"usage":turn.outcome.usage,
        "cost_usd":turn.outcome.usage.cost_usd,"elapsed_ms":turn.outcome.elapsed_ms,
        "tool_calls":turn.outcome.tool_calls,"plan":turn.plan,"text":turn.outcome.text,
        "session_file":turn.outcome.session_file
    })).collect::<Vec<_>>()}),
        ),
    ))
}

pub(crate) fn run(args: Args) -> ExitCode {
    match turn(&args) {
        Ok((ok, report)) => {
            println!("{report}");
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!(
                "agent_turn_failed: {}",
                crate::runtime::harness::failure_reason(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
