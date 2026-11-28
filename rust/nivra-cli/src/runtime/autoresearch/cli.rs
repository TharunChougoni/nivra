//! `nivra research-loop`: the off-hours entry point the systemd timer runs,
//! plus status and backup. It runs the same coordinator as the controller,
//! against the last sealed session, under the same lease.
use super::coordinator::{Step, StepReport, TurnEnd, TurnPlan, Turns, status, step};
use super::{QueueJobs, enabled, open};
use crate::runtime::agent::lanes::{LaneRunner, TurnContext};
use crate::runtime::agent::protocol::TurnStatus;
use crate::runtime::agent::service::LaneBackend;
use crate::runtime::agent::{Agent, AgentClient, Lane};
use crate::runtime::planner::PlannerConfig;
use crate::runtime::{Config, Result, millis, rollover};
use chrono::NaiveDate;
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;

/// Runs a step's turns on the pi agent through the lane runner.
pub(crate) struct RunnerTurns<'r, 'a> {
    pub runner: &'r mut LaneRunner<'a>,
    pub context: &'r mut TurnContext,
}

impl Turns for RunnerTurns<'_, '_> {
    fn run(&mut self, plan: &TurnPlan) -> Result<TurnEnd> {
        let turn = self.runner.autonomous_turn(plan, self.context)?;
        Ok(TurnEnd {
            ok: turn.outcome.status == TurnStatus::Ok,
            calls: turn.outcome.usage.calls,
            cost_usd: turn.outcome.usage.cost_usd,
            skill_verdicts: turn.skill_verdicts,
        })
    }
}

/// Where the loop stands against the exchange calendar: whether today has a
/// session, whether it is sealed and which session ends a new campaign.
pub(crate) fn calendar(c: &Config, now: i64) -> (bool, bool, NaiveDate) {
    let today = nivra_core::autoresearch::ist_day(now);
    let trading_today = c.is_trading_day(today).unwrap_or(false);
    let sealed = crate::runtime::agent::lanes::sealed_review_source(c, &c.directory, c.session);
    let sealed_today = sealed && c.session == today;
    let last_sealed = if sealed {
        c.session
    } else {
        c.previous_session
    };
    (trading_today, sealed_today, last_sealed)
}

/// Whether the archive's daily bars reach `day`.
pub(crate) fn archive_covers(c: &Config, day: NaiveDate) -> Result<bool> {
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let mut db = crate::runtime::market_archive::connect(archive)?;
    Ok(nivra_store::market_archive::last_daily_bar_day(&mut db)?.is_some_and(|last| last >= day))
}

/// The host is short of memory: the loop yields before it starts work.
pub(crate) fn host_pressure() -> bool {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("MemAvailable:"))
                .and_then(|rest| {
                    rest.trim()
                        .trim_end_matches("kB")
                        .trim()
                        .parse::<u64>()
                        .ok()
                })
        })
        .is_some_and(|kib| kib < 160 * 1024)
}

#[derive(clap::Args)]
pub(crate) struct Args {
    /// The paper runtime config the controller runs with.
    #[arg(long)]
    config: PathBuf,
    #[command(subcommand)]
    action: Action,
}

#[derive(clap::Subcommand)]
enum Action {
    /// Step the loop back to back until it waits or `--max-minutes` pass.
    Tick {
        #[arg(long, default_value_t = 25)]
        max_minutes: u64,
    },
    /// Print the loop state the dashboard shows.
    Status,
    /// Copy research.sqlite and every object it references to a new directory.
    Backup {
        #[arg(long)]
        to: PathBuf,
    },
    /// Check a backup or a research root: every referenced object matches its digest.
    Verify {
        #[arg(long)]
        root: PathBuf,
    },
}

fn load(args: &Args) -> Result<(Config, PlannerConfig)> {
    let bytes = crate::read_bounded(&args.config, 16_384).map_err(|_| "config_unreadable")?;
    let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
    c.validate()?;
    let c = rollover::current(&c)?;
    let cfg = PlannerConfig::load_for_session(
        c.planner_config
            .as_deref()
            .ok_or("planner_config_missing")?,
        c.session,
    )?;
    Ok((c, cfg))
}

fn tick(c: &Config, cfg: &PlannerConfig, max_minutes: u64) -> Result<Vec<StepReport>> {
    // The controller runs the loop itself while it is up.
    match rollover::controller_lock(c) {
        Ok(lock) => drop(lock),
        Err(_) => return Err("controller_running".into()),
    }
    let settings = cfg.agent.clone().ok_or("agent_settings_missing")?;
    settings.lane(Lane::Research)?;
    let db = crate::runtime::open_book_current_read_only(&c.directory.join("paper.sqlite"))?;
    let portfolio =
        crate::runtime::wide::stored_portfolio(&db, c)?.ok_or("agent_turn_book_unavailable")?;
    drop(db);
    let mut context = TurnContext {
        bundle: json!({}),
        backend: Box::new(LaneBackend(None)),
        portfolio,
        instruments: Vec::new(),
        web: Box::new(crate::runtime::web_tools::Network),
    };
    let spawn = settings.clone();
    let mut agent = Agent::new(Box::new(move || AgentClient::spawn(&spawn, &[])));
    let clock = millis;
    let mut jobs = QueueJobs::new(c, cfg)?;
    let owner = format!("timer:{}", std::process::id());
    let deadline = millis().saturating_add((max_minutes.clamp(1, 600) * 60_000) as i64);
    let mut reports = Vec::new();
    {
        let mut runner = LaneRunner::new(&mut agent, &settings, c, cfg, &clock);
        while millis() < deadline {
            let (trading_today, sealed_today, last_sealed) = calendar(c, millis());
            let mut tape = |day| archive_covers(c, day);
            let report = step(&mut Step {
                c,
                cfg,
                owner: &owner,
                clock: &clock,
                turns: &mut RunnerTurns {
                    runner: &mut runner,
                    context: &mut context,
                },
                jobs: &mut jobs,
                stop: &host_pressure,
                tape: &mut tape,
                trading_today,
                sealed_today,
                last_sealed,
            })?;
            let progressed = report.progressed();
            reports.push(report);
            if !progressed {
                break;
            }
        }
    }
    agent.shutdown()?;
    Ok(reports)
}

pub(crate) fn run(args: Args) -> ExitCode {
    let result = (|| -> Result<serde_json::Value> {
        let (c, cfg) = load(&args)?;
        Ok(match &args.action {
            Action::Tick { max_minutes } => {
                if enabled(&cfg).is_none() {
                    json!({"status":"disabled"})
                } else {
                    json!({"steps":tick(&c, &cfg, *max_minutes)?})
                }
            }
            Action::Status => status(&c, &cfg, millis()),
            Action::Backup { to } => json!({"backup":open(&c)?.backup(to)?}),
            Action::Verify { root } => {
                json!({"verified":nivra_store::research_memory::verify(root)?})
            }
        })
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "research_loop_failed: {}",
                crate::runtime::harness::failure_reason(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}
