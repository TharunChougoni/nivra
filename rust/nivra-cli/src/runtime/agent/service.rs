//! The controller's side of the pi lanes. Each scan cycle feeds the trigger
//! engine; a due lane turn runs on one worker thread that owns the agent
//! process. The controller settles its claim before a paper plan can execute.
use super::brief::{self, Brief, Facts, RegimeFacts, SessionFacts};
use super::client::{Agent, AgentClient};
use super::lanes::{self, LaneRunner, TurnContext};
use super::paper;
use super::protocol::{TurnStatus, Usage};
use super::requests::Requests;
use super::tools::{PlanAttempt, Prospect, Verdict};
use super::triggers::{
    FoObservation, Observation, Phase, Ranked, Trigger, TriggerEngine, TriggerKind, ranker_registry,
};
use super::{AgentMode, AgentSettings, Lane};
use crate::runtime::options::{SpotQuote, chain};
use crate::runtime::planner::{self, Backend, PlannerConfig};
use crate::runtime::wide::strategy_run::{self, SupervisionState};
use crate::runtime::{
    Config, Connection, Result, autoresearch, decision, decision_evidence, redecision,
};
use nivra_core::execution::{Instrument, Portfolio, Scanner};
use nivra_core::feature_store::FeatureStore;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, channel, sync_channel};
use std::thread::JoinHandle;

const CONTEXT_RETRY_MS: i64 = 5_000;

/// What the after-close research turn is for. The review, when it left text,
/// rides along in the same task.
const AFTER_CLOSE_TASK: &str = "The session is closed and speed does not matter. Read today's review when present. Use archive_window over at least three recent NSE sessions to check intraday fills and strategy acceptance. Use archive_bars over a longer training and test period to study performance across regimes. Queue bar jobs with backtest and read the verified reports when they finish. Keep a short note per strategy under notes/ with its source hash, costs, and net result.";

/// What the pre-open decision turn is for.
const PRE_OPEN_TASK: &str = "The market opens soon. Choose today's action strategy before the first scan. Read strategies in the brief: select an eligible accepted version that suits the regime and the last sessions, else the best configured fallback (fallback:<name>), with strategy_select. If nothing eligible or configured suits the expected session, post a research request asking for a strategy suited to it, naming the regime you expect. Submit no plan: no quote is executable before the open.";

/// What research does when no accepted strategy can run today.
const STRATEGY_SEARCH_TASK: &str = "No accepted action strategy can run on this build today. Follow the strategy-library skill: replay the reference action strategies under reference/strategies/ with backtest on the recent_archive dataset, with each one's shipped params and the selection window below, then your own variants of the best. Every replay that passes acceptance registers itself in the library. Keep a note per strategy under notes/ with its source hash, params and net result, and say which passed and which you would run today.";

fn strategy_search_task(c: &Config, cfg: &PlannerConfig, now: i64) -> Value {
    let window = cfg
        .research_tools
        .as_ref()
        .and_then(|r| r.candidate_compute.as_ref())
        .and_then(|candidate| candidate.strategy.as_ref())
        .map(|settings| {
            crate::runtime::harness::research::RecentArchiveWindow::resolve(c, settings, now)
                .map_or_else(
                    |reason| json!({"unavailable":reason}),
                    |window| json!(window),
                )
        });
    json!({"kind":"strategy_search","session":c.session.to_string(),
        "instructions":STRATEGY_SEARCH_TASK,"recent_archive":window})
}

/// Serves intraday context from the controller's frozen evidence snapshot.
/// It never calls a model: pi owns every model call.
pub(crate) struct LaneBackend(pub Option<decision_evidence::Snapshot>);

impl Backend for LaneBackend {
    fn intraday_context(&mut self, keys: &[String], at: i64) -> Option<Result<Value>> {
        self.0.as_ref().map(|snapshot| snapshot.read(keys, at))
    }
}

/// The live ranking from a discovery projection: its prospects, best first,
/// scored by the selected strategy or else the sampled move in bps.
pub(crate) fn ranking(projection: &Value) -> Vec<Ranked> {
    projection["prospects"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            Some(Ranked {
                instrument_key: row["instrument_key"].as_str()?.to_owned(),
                score: row["strategy_score"]
                    .as_f64()
                    .or_else(|| row["sample_change_bps"].as_f64())
                    .filter(|score| score.is_finite())?,
            })
        })
        .collect()
}

/// Everything one scan cycle offers the lanes.
pub(crate) struct Cycle<'a> {
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    pub now: i64,
    pub minute: u32,
    pub sealed: bool,
    pub portfolio: &'a Portfolio,
    pub instruments: &'a [Instrument],
    pub ranking: &'a [Ranked],
    pub ranked_at: i64,
    pub decision_ready: bool,
    /// History, universe and book are ready for today's session, before or
    /// during it: the pre-open strategy choice may run.
    pub prepared: bool,
    pub scanner: &'a Scanner,
    pub store: &'a FeatureStore,
    pub index: Option<Value>,
    pub regime: Option<RegimeFacts>,
    /// Fresh Nifty spot when the option universe is enabled.
    pub spot: Option<SpotQuote>,
}

/// F&O triggers the gate escalated, with its validated prospects; the next
/// dispatch runs them on the decision lane without screening them again.
#[derive(Clone, Debug, PartialEq)]
struct Escalation {
    triggers: Vec<Trigger>,
    prospects: Vec<Prospect>,
}

/// A lane turn ready to hand the worker.
struct Prepared {
    job: Job,
    claim: Option<redecision::Claim>,
    consumes_agenda: bool,
}

struct Job {
    c: Config,
    cfg: PlannerConfig,
    work: Work,
}

struct Work {
    lane: Lane,
    triggers: Vec<Trigger>,
    brief: Brief,
    context: TurnContext,
    paper_turn_id: Option<String>,
    /// One research-loop step instead of a lane turn: today's calendar facts.
    research_loop: Option<(bool, bool, chrono::NaiveDate)>,
}

/// One finished job, as the controller settles it.
struct Done {
    lane: Lane,
    triggers: Vec<Trigger>,
    turns: Vec<Value>,
    plan: Option<(String, PlanAttempt)>,
    paper_input: Option<Value>,
    review: Option<String>,
    /// The gate's answer; `None` when the gate turn ended without one.
    verdict: Option<Verdict>,
    error: Option<String>,
    /// The agent is restarting; the batch may run again from this time.
    retry_at_ms: Option<i64>,
    /// What a research-loop step did.
    loop_report: Option<autoresearch::StepReport>,
}

/// One research-loop step on the lane worker, which owns the agent.
fn loop_step(
    runner: &mut LaneRunner<'_>,
    job: &mut Work,
    (trading_today, sealed_today, last_sealed): (bool, bool, chrono::NaiveDate),
) -> Result<autoresearch::StepReport> {
    let (c, cfg) = (runner.c, runner.cfg);
    let mut jobs = autoresearch::QueueJobs::new(c, cfg)?;
    let preempt = runner.preempt.clone();
    let stop = move || preempt.load(Ordering::Acquire) || autoresearch::cli::host_pressure();
    let owner = format!("controller:{}", std::process::id());
    let mut tape = |day| autoresearch::cli::archive_covers(c, day);
    let clock = crate::runtime::millis;
    autoresearch::step(&mut autoresearch::Step {
        c,
        cfg,
        owner: &owner,
        clock: &clock,
        turns: &mut autoresearch::cli::RunnerTurns {
            runner,
            context: &mut job.context,
        },
        jobs: &mut jobs,
        stop: &stop,
        tape: &mut tape,
        trading_today,
        sealed_today,
        last_sealed,
    })
}

pub(crate) struct LaneService {
    mode: AgentMode,
    settings: AgentSettings,
    engine: TriggerEngine,
    jobs: Option<SyncSender<Job>>,
    done: Receiver<Done>,
    worker: Option<JoinHandle<()>>,
    preempt: Arc<AtomicBool>,
    busy: Option<Lane>,
    pending_claim: Option<redecision::Claim>,
    hold_until_ms: i64,
    agenda: Option<String>,
    escalated: Option<Escalation>,
    /// This scan's near-the-money option chain, for F&O triggers and briefs.
    chain: Option<chain::Chain>,
    /// Why the chain could not be built, journalled once per change.
    chain_failure: Option<String>,
    /// This process claimed the after-close marker, so a requeued batch may
    /// still run its turn.
    after_close_claimed: bool,
    /// When the research loop may step next.
    loop_next_ms: i64,
    /// The loop state the dashboard reads, refreshed every few seconds.
    loop_status: (i64, Value),
    status: Value,
}

/// The trigger queue a lane's batch returns to: gate batches are decision
/// triggers that have not been screened yet.
fn queue(lane: Lane) -> Lane {
    if lane == Lane::Gate {
        Lane::Decision
    } else {
        lane
    }
}

fn append(c: &Config, file: &str, line: &Value) -> Result<()> {
    let root = lanes::root(c);
    lanes::private_dir(&root)?;
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(root.join(file))?;
    writeln!(out, "{line}")?;
    Ok(out.sync_all()?)
}

fn primary(triggers: &[Trigger]) -> Value {
    let first = triggers.first();
    json!({"kind":first.map(|t| t.kind),"reason":first.map(|t| t.reason.as_str()),"triggers":triggers})
}

fn turn_summary(turn: &lanes::LaneTurn) -> Value {
    json!({"turn_id":turn.outcome.id,"lane":turn.outcome.lane,"status":turn.outcome.status,
        "error":turn.outcome.error,"usage":turn.outcome.usage,"model":turn.outcome.model,
        "provider":turn.outcome.provider,"fallback":turn.outcome.fallback,"tool_calls":turn.outcome.tool_calls.len(),
        "elapsed_ms":turn.outcome.elapsed_ms})
}

/// Run one job on the worker's agent. Research batches answer every queued
/// request; any other research trigger gets one turn about its task.
fn run(runner: &mut LaneRunner<'_>, job: &mut Work) -> Done {
    let trigger = primary(&job.triggers);
    let mut done = Done {
        lane: job.lane,
        triggers: job.triggers.clone(),
        turns: Vec::new(),
        plan: None,
        paper_input: None,
        review: None,
        verdict: None,
        error: None,
        retry_at_ms: None,
        loop_report: None,
    };
    let result = (|| -> Result<()> {
        if let Some(calendar) = job.research_loop {
            done.loop_report = Some(loop_step(runner, job, calendar)?);
            return Ok(());
        }
        match job.lane {
            Lane::Decision => {
                let brief = job.brief.clone();
                let turn = if let Some(id) = job.paper_turn_id.take() {
                    let turn = runner.decision_as(id, trigger, brief, &mut job.context)?;
                    if turn.outcome.status == TurnStatus::Ok && turn.outcome.error.is_none() {
                        done.paper_input = Some(std::mem::take(&mut job.context.bundle));
                    }
                    turn
                } else {
                    runner.decision(trigger, brief, &mut job.context)?
                };
                done.turns.push(turn_summary(&turn));
                if let Some(plan) = turn.plan {
                    done.plan = Some((turn.outcome.id.clone(), plan));
                }
            }
            Lane::Research => {
                if job.triggers.iter().any(|t| t.kind == TriggerKind::Request) {
                    for turn in runner.research_requests(&trigger, &job.brief, &mut job.context)? {
                        done.turns.push(turn_summary(&turn));
                    }
                }
                if job.triggers.iter().any(|t| t.kind != TriggerKind::Request)
                    && !runner.preempt.load(Ordering::Acquire)
                {
                    let turn = runner.turn(
                        Lane::Research,
                        trigger.clone(),
                        json!(job.brief.render()?),
                        &mut job.context,
                    )?;
                    done.turns.push(turn_summary(&turn));
                }
            }
            Lane::Gate => {
                let turn = runner.turn(
                    Lane::Gate,
                    trigger,
                    json!(job.brief.render()?),
                    &mut job.context,
                )?;
                done.turns.push(turn_summary(&turn));
                if turn.outcome.status == TurnStatus::Ok {
                    done.verdict = turn.verdict;
                }
            }
            Lane::Review => {
                let turn = runner.review(trigger, &job.brief, &mut job.context)?;
                done.turns.push(turn_summary(&turn));
                if turn.outcome.status == TurnStatus::Ok {
                    let text = crate::runtime::transcript::redact(&json!(turn.outcome.text));
                    done.review = text.as_str().map(str::to_owned);
                }
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        let reason = error.to_string();
        if reason == "agent_restart_backoff" {
            done.retry_at_ms = runner.agent.retry_at_ms();
        }
        done.error = Some(reason);
    }
    done
}

fn work(
    settings: AgentSettings,
    preempt: Arc<AtomicBool>,
    jobs: Receiver<Job>,
    out: std::sync::mpsc::Sender<Done>,
) {
    let spawn_settings = settings.clone();
    let mut agent = Agent::new(Box::new(move || AgentClient::spawn(&spawn_settings, &[])));
    let clock = crate::runtime::millis;
    for job in jobs {
        let Job { c, cfg, mut work } = job;
        let mut runner = LaneRunner::new(&mut agent, &settings, &c, &cfg, &clock);
        runner.preempt = preempt.clone();
        let done = run(&mut runner, &mut work);
        drop(runner);
        if out.send(done).is_err() {
            break;
        }
    }
    let _ = agent.shutdown();
}

impl LaneService {
    /// `None` without a `planner.agent` block: no agent process, no triggers.
    pub(crate) fn start(cfg: &PlannerConfig) -> Option<Self> {
        let settings = cfg.agent.clone()?;
        let (jobs, inbox) = sync_channel(1);
        let (out, done) = channel();
        let preempt = Arc::new(AtomicBool::new(false));
        let worker_settings = settings.clone();
        let worker_preempt = preempt.clone();
        let worker = std::thread::Builder::new()
            .name("nivra-agent-lanes".into())
            .spawn(move || work(worker_settings, worker_preempt, inbox, out))
            .ok()?;
        Some(Self {
            mode: settings.mode,
            engine: TriggerEngine::new(settings.triggers()),
            settings,
            jobs: Some(jobs),
            done,
            worker: Some(worker),
            preempt,
            busy: None,
            pending_claim: None,
            hold_until_ms: 0,
            agenda: None,
            escalated: None,
            chain: None,
            chain_failure: None,
            after_close_claimed: false,
            loop_next_ms: 0,
            loop_status: (i64::MIN, Value::Null),
            status: json!({"status":"idle"}),
        })
    }

    /// The research loop: its last step, budget left and latest verdict.
    pub(crate) fn research_loop_status(&self) -> Value {
        self.loop_status.1.clone()
    }

    pub(crate) fn status(&self) -> Value {
        self.status.clone()
    }

    /// Ranks of the live ranking that count as the top K.
    pub(crate) fn top_k(&self) -> usize {
        self.engine.settings().decision.top_k
    }

    pub(crate) fn pending_claim(&self) -> Option<&redecision::Claim> {
        self.pending_claim.as_ref()
    }

    pub(crate) fn cancel_decision(&mut self, db: &mut Connection, c: &Config) -> Result<()> {
        if let Some(claim) = self.pending_claim.as_ref() {
            paper::refuse(db, c, claim, "session_closed_no_replay")?;
            self.pending_claim = None;
            self.preempt.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn spent(&self, c: &Config) -> BTreeMap<Lane, Usage> {
        lanes::spend(c)
    }

    fn fraction(&self, lane: Lane, spent: &BTreeMap<Lane, Usage>) -> f64 {
        let Ok(settings) = self.settings.lane(lane) else {
            return 1.0;
        };
        let used = spent.get(&lane).cloned().unwrap_or_default();
        (used.calls as f64 / settings.daily_calls as f64)
            .max(used.cost_usd / settings.daily_cost_usd)
            .clamp(0.0, 1.0)
    }

    /// One scan cycle: settle a finished turn, evaluate triggers and start the
    /// next due turn. Lane failures are journalled; they never stop trading.
    pub(crate) fn tick(&mut self, cycle: &Cycle<'_>, db: &mut Connection) -> Result<()> {
        let c = cycle.c;
        match self.done.try_recv() {
            Ok(done) => self.settle(cycle, db, done)?,
            Err(TryRecvError::Disconnected) if self.busy.is_some() => {
                if let Some(claim) = self.pending_claim.as_ref() {
                    paper::refuse(db, c, claim, "agent_worker_exited")?;
                    self.pending_claim = None;
                }
                self.busy = None;
                self.status = json!({"status":"unavailable","reason":"agent_worker_exited"});
            }
            Err(_) => {}
        }
        if cycle.now.saturating_sub(self.loop_status.0) >= 15_000 {
            self.loop_status = (cycle.now, autoresearch::status(c, cycle.cfg, cycle.now));
        }
        let spent = self.spent(c);
        let mut held = brief::held(cycle.portfolio);
        held.retain(|position| {
            cycle
                .portfolio
                .positions
                .get(&position.instrument_key)
                .is_some_and(|position| {
                    cycle
                        .now
                        .checked_sub(position.marked_at)
                        .is_some_and(|age| {
                            (0..=nivra_core::market::MAX_FINANCIAL_AGE_MS).contains(&age)
                        })
                })
        });
        let in_session = crate::runtime::local(cycle.now).date_naive() == c.session;
        let phase = if (in_session && cycle.decision_ready) || cycle.sealed {
            Phase::at(
                cycle.minute,
                c.execution.session_start_minute,
                self.engine.settings().decision.pre_close_minute,
                c.execution.session_end_minute,
                cycle.sealed,
            )
        } else if in_session
            && cycle.prepared
            && cycle.minute < c.execution.session_start_minute
            && !(cycle.portfolio.closed || cycle.portfolio.stopped)
        {
            Phase::PreOpen
        } else {
            Phase::Closed
        };
        let research = self.settings.lanes.contains_key(&Lane::Research);
        let requests = if research {
            Requests::new(c, &self.engine.settings().research)
                .queued(cycle.now, 1)?
                .len()
        } else {
            0
        };
        let supervision = strategy_run::supervision_state(c, cycle.cfg);
        let strategy_fills = cycle
            .portfolio
            .fills
            .iter()
            .filter(|f| {
                f.decision_id
                    .as_deref()
                    .is_some_and(crate::runtime::strategy_library::choice::is_strategy_run)
            })
            .count();
        let rankers = ranker_registry();
        let fired = self.engine.observe(&Observation {
            now: cycle.now,
            minute: cycle.minute,
            open_minute: c.execution.session_start_minute,
            phase,
            held: &held,
            fills: cycle.portfolio.fills.len(),
            ranking: cycle.ranking,
            ranked_at: cycle.ranked_at,
            decision_spent: self.fraction(Lane::Decision, &spent),
            research_budget_left: research && self.fraction(Lane::Research, &spent) < 1.0,
            requests_queued: requests,
            rankers: &rankers,
            after_close_due: research
                && phase == Phase::Sealed
                && !lanes::root(c).join(lanes::AFTER_CLOSE_MARKER).exists(),
            review_running: self.busy == Some(Lane::Review),
            strategy_active: supervision.strategy_active(),
            strategy_supervision: supervision.supervised,
            strategy_accepted: supervision.accepted(),
            strategy_fills,
            regime: cycle.regime.as_ref().map(|r| r.label.as_str()),
        });
        let mut fired = fired;
        self.refresh_chain(cycle)?;
        if let Some(chain) = &self.chain {
            fired.extend(self.engine.observe_fo(&FoObservation {
                now: cycle.now,
                minute: cycle.minute,
                phase,
                decision_spent: self.fraction(Lane::Decision, &spent),
                chain,
            }));
        }
        for trigger in &fired {
            append(
                c,
                "triggers.ndjson",
                &json!({"event":"fired","lane":trigger.kind.lane(),"trigger":trigger}),
            )?;
        }
        if self
            .busy
            .is_some_and(|lane| !matches!(lane, Lane::Decision | Lane::Gate))
            && (phase.trading() || phase == Phase::PreOpen)
            && self.fraction(Lane::Decision, &spent) < 1.0
            && self.engine.decision_waiting()
            && (!cycle.ranking.is_empty()
                || !held.is_empty()
                || fired
                    .iter()
                    .any(|t| matches!(t.kind, TriggerKind::Fill | TriggerKind::FoSignal))
                || supervision.supervised)
        {
            self.preempt.store(true, Ordering::Release);
        }
        if self.busy.is_none() && cycle.now >= self.hold_until_ms {
            self.dispatch(cycle, db, &held, phase, &spent, &supervision)?;
        }
        // Lane turns go first; the loop takes the worker only when it is idle
        // and the book is not trading.
        if self.busy.is_none()
            && cycle.now >= self.hold_until_ms.max(self.loop_next_ms)
            && !phase.trading()
            && phase != Phase::PreOpen
            && self.settings.lanes.contains_key(&Lane::Research)
            && crate::runtime::autoresearch::enabled(cycle.cfg).is_some()
        {
            self.dispatch_loop(cycle, phase)?;
        }
        Ok(())
    }

    /// Hand the worker one research-loop step.
    fn dispatch_loop(&mut self, cycle: &Cycle<'_>, phase: Phase) -> Result<()> {
        let c = cycle.c;
        let context = match self.context(cycle) {
            Ok(context) => context,
            Err(_) => {
                self.loop_next_ms = cycle.now.saturating_add(CONTEXT_RETRY_MS);
                return Ok(());
            }
        };
        let facts = Facts {
            session: SessionFacts {
                date: c.session.to_string(),
                phase,
                minute: cycle.minute,
                now_ms: cycle.now,
            },
            portfolio: cycle.portfolio,
            ranking: cycle.ranking,
            memory: None,
            execution: "shadow_never_executed",
            regime: cycle.regime.as_ref(),
        };
        let settings = self.settings.lane(Lane::Research)?;
        let budget = brief::budget(
            Lane::Research,
            &Usage::default(),
            settings.daily_calls,
            settings.daily_cost_usd,
            (0.0, 0),
        );
        let job = Job {
            c: c.clone(),
            cfg: cycle.cfg.clone(),
            work: Work {
                lane: Lane::Research,
                triggers: Vec::new(),
                brief: Brief::build(&facts, budget, Value::Null, &[]),
                context,
                paper_turn_id: None,
                research_loop: Some(crate::runtime::autoresearch::cli::calendar(c, cycle.now)),
            },
        };
        let Some(jobs) = self.jobs.as_ref() else {
            return Ok(());
        };
        self.preempt.store(false, Ordering::Release);
        if jobs.try_send(job).is_err() {
            self.loop_next_ms = cycle.now.saturating_add(CONTEXT_RETRY_MS);
            return Ok(());
        }
        self.busy = Some(Lane::Research);
        self.status = json!({"status":"running","lane":Lane::Research,"since_ms":cycle.now,"research_loop":true,"mode":self.mode});
        Ok(())
    }

    fn skip(&self, c: &Config, lane: Lane, triggers: &[Trigger], reason: &str) -> Result<()> {
        append(
            c,
            "triggers.ndjson",
            &json!({"event":"skipped","lane":lane,"reason":reason,"triggers":triggers}),
        )
    }

    /// The near-the-money chain for this scan, when options are enabled and
    /// the universe and a fresh spot exist.
    fn refresh_chain(&mut self, cycle: &Cycle<'_>) -> Result<()> {
        let c = cycle.c;
        let (Some(catalog), Some(spot)) = (cycle.portfolio.options.as_ref(), &cycle.spot) else {
            self.chain = None;
            return Ok(());
        };
        let built = chain::near_money(
            catalog,
            cycle.scanner,
            cycle.portfolio,
            spot,
            c.session,
            cycle.now,
            self.engine.settings().fo.strikes_near,
        );
        let failure = built
            .as_ref()
            .err()
            .map(|error| crate::runtime::harness::failure_reason(error.as_ref()));
        if failure.is_some() && failure != self.chain_failure {
            append(
                c,
                "triggers.ndjson",
                &json!({"event":"fo_chain_unavailable","at_ms":cycle.now,"reason":failure}),
            )?;
        }
        self.chain_failure = failure;
        self.chain = built.ok();
        Ok(())
    }

    fn dispatch(
        &mut self,
        cycle: &Cycle<'_>,
        db: &mut Connection,
        held: &[super::triggers::Held],
        phase: Phase,
        spent: &BTreeMap<Lane, Usage>,
        supervision: &SupervisionState,
    ) -> Result<()> {
        let c = cycle.c;
        let Some(Prepared {
            job,
            claim,
            consumes_agenda,
        }) = self.prepare(cycle, db, held, phase, spent, supervision)?
        else {
            return Ok(());
        };
        let (lane, triggers) = (job.work.lane, job.work.triggers.clone());
        let Some(jobs) = self.jobs.as_ref() else {
            return self.skip(c, lane, &triggers, "agent_worker_unavailable");
        };
        self.preempt.store(false, Ordering::Release);
        match jobs.try_send(job) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                if let Some(claim) = &claim {
                    paper::refuse(db, c, claim, "agent_worker_busy")?;
                } else {
                    self.engine.requeue(queue(lane), triggers);
                }
                return Ok(());
            }
            Err(TrySendError::Disconnected(_)) => {
                if let Some(claim) = &claim {
                    paper::refuse(db, c, claim, "agent_worker_unavailable")?;
                }
                self.jobs = None;
                return self.skip(c, lane, &triggers, "agent_worker_unavailable");
            }
        }
        self.pending_claim = claim;
        if consumes_agenda {
            self.agenda = None;
        }
        if lane == Lane::Decision {
            self.engine.decision_started(cycle.now, held, cycle.ranking);
        }
        self.busy = Some(lane);
        self.status = json!({"status":"running","lane":lane,"since_ms":cycle.now,"triggers":triggers.len(),"mode":self.mode});
        append(
            c,
            "triggers.ndjson",
            &json!({"event":"dispatched","lane":lane,"at_ms":cycle.now,"triggers":triggers}),
        )
    }

    /// The next due turn, or `None` when it was skipped, deferred or nothing
    /// is due. Every skip is journalled.
    fn prepare(
        &mut self,
        cycle: &Cycle<'_>,
        db: &mut Connection,
        held: &[super::triggers::Held],
        phase: Phase,
        spent: &BTreeMap<Lane, Usage>,
        supervision: &SupervisionState,
    ) -> Result<Option<Prepared>> {
        let c = cycle.c;
        let strategy_active = supervision.strategy_active();
        let (lane, mut triggers, prospects) = match self.escalated.take() {
            Some(escalation) => (Lane::Decision, escalation.triggers, escalation.prospects),
            None => {
                let Some((lane, triggers)) = self.engine.next() else {
                    return Ok(None);
                };
                (
                    self.screen(c, lane, &triggers, spent)?,
                    triggers,
                    Vec::new(),
                )
            }
        };
        if strategy_active && matches!(lane, Lane::Decision | Lane::Gate) {
            let (entry, rest): (Vec<_>, Vec<_>) = triggers
                .into_iter()
                .partition(|t| t.kind == TriggerKind::EntryCandidate);
            if !entry.is_empty() {
                self.skip(c, lane, &entry, "strategy_owns_cash_entries")?;
            }
            if rest.is_empty() {
                return Ok(None);
            }
            triggers = rest;
        }
        let Ok(settings) = self.settings.lane(lane) else {
            return self
                .skip(c, lane, &triggers, "agent_lane_not_configured")
                .map(|()| None);
        };
        let p = cycle.portfolio;
        let pre_open = lane == Lane::Decision
            && phase == Phase::PreOpen
            && triggers.iter().all(|t| t.kind == TriggerKind::PreOpen);
        if matches!(lane, Lane::Decision | Lane::Gate) {
            if !(phase.trading() || pre_open) || p.halted || p.stopped || p.closed {
                return self
                    .skip(c, lane, &triggers, "agent_book_not_trading")
                    .map(|()| None);
            }
            if cycle.ranking.is_empty()
                && held.is_empty()
                && !triggers.iter().any(|trigger| {
                    matches!(trigger.kind, TriggerKind::Fill | TriggerKind::FoSignal)
                })
                && !(supervision.supervised
                    && triggers.iter().any(|trigger| {
                        matches!(trigger.kind, TriggerKind::Schedule | TriggerKind::PreOpen)
                    }))
            {
                return self
                    .skip(c, lane, &triggers, "agent_no_eligible_evidence")
                    .map(|()| None);
            }
        }
        if self.fraction(lane, spent) >= 1.0
            || (lane == Lane::Gate && self.fraction(Lane::Decision, spent) >= 1.0)
        {
            return self
                .skip(c, lane, &triggers, "agent_budget_exhausted")
                .map(|()| None);
        }
        if self.jobs.is_none() {
            return self
                .skip(c, lane, &triggers, "agent_worker_unavailable")
                .map(|()| None);
        }
        let context = match self.context(cycle) {
            Ok(context) => context,
            Err(error) => {
                self.hold_until_ms = cycle.now.saturating_add(CONTEXT_RETRY_MS);
                let entry = json!({"event":"deferred","lane":lane,"at_ms":cycle.now,
                    "reason":crate::runtime::harness::failure_reason(error.as_ref()),
                    "retry_at_ms":self.hold_until_ms,"triggers":triggers});
                self.engine.requeue(queue(lane), triggers);
                return append(c, "triggers.ndjson", &entry).map(|()| None);
            }
        };
        if lane == Lane::Review && !lanes::claim(c, lanes::REVIEW_MARKER)? {
            return self
                .skip(c, lane, &triggers, "agent_review_already_ran")
                .map(|()| None);
        }
        let after_close = |t: &Trigger| t.kind == TriggerKind::AfterClose;
        if !self.after_close_claimed && triggers.iter().any(after_close) {
            if lanes::claim(c, lanes::AFTER_CLOSE_MARKER)? {
                self.after_close_claimed = true;
            } else {
                let (ran, rest): (Vec<_>, Vec<_>) = triggers.into_iter().partition(after_close);
                self.skip(c, lane, &ran, "agent_after_close_already_ran")?;
                if rest.is_empty() {
                    return Ok(None);
                }
                triggers = rest;
            }
        }
        let memory = lanes::memory(c, lane);
        let facts = Facts {
            session: SessionFacts {
                date: c.session.to_string(),
                phase,
                minute: cycle.minute,
                now_ms: cycle.now,
            },
            portfolio: p,
            ranking: cycle.ranking,
            memory: memory.as_ref(),
            execution: if self.mode == AgentMode::Paper {
                "paper_plan_pending_host_validation"
            } else {
                "shadow_never_executed"
            },
            regime: cycle.regime.as_ref(),
        };
        let used = spent.get(&lane).cloned().unwrap_or_default();
        let decision_spent = self.fraction(Lane::Decision, spent);
        let mut budget = brief::budget(
            lane,
            &used,
            settings.daily_calls,
            settings.daily_cost_usd,
            self.engine.entry_bar(decision_spent),
        );
        if lane == Lane::Gate {
            budget.fo_bar = Some(self.engine.settings().fo.bar(decision_spent));
        }
        let consumes_agenda = lane == Lane::Research
            && triggers
                .iter()
                .any(|trigger| trigger.kind == TriggerKind::Agenda);
        let agenda = consumes_agenda.then_some(self.agenda.as_deref()).flatten();
        let task = match lane {
            Lane::Research if triggers.iter().any(after_close) => {
                json!({"kind":"after_close","session":c.session.to_string(),
                    "instructions":AFTER_CLOSE_TASK,"review":agenda})
            }
            Lane::Research if consumes_agenda => {
                json!({"kind":"agenda","review":self.agenda})
            }
            Lane::Research
                if triggers
                    .iter()
                    .any(|t| t.kind == TriggerKind::StrategySearch) =>
            {
                strategy_search_task(c, cycle.cfg, cycle.now)
            }
            Lane::Research => json!({"kind":triggers[0].kind}),
            Lane::Review => json!({"kind":"review","session":c.session.to_string()}),
            Lane::Gate => json!({"kind":"gate"}),
            Lane::Decision if pre_open => {
                json!({"kind":"pre_open","session":c.session.to_string(),"instructions":PRE_OPEN_TASK})
            }
            Lane::Decision => Value::Null,
        };
        let mut brief = Brief::build(&facts, budget, task, &triggers);
        if matches!(lane, Lane::Decision | Lane::Gate) {
            brief.fo = self.chain.clone();
        }
        if lane == Lane::Decision {
            brief.gate_prospects = prospects;
            brief.strategies = brief::strategy_facts(db, c, cycle.cfg);
            brief.execution_refusals =
                crate::runtime::wide::recent_refusals(db, brief::MAX_REFUSALS)?;
        }
        // The pre-open turn chooses a strategy; it never holds a paper plan.
        let claim = if lane == Lane::Decision && self.mode == AgentMode::Paper && !pre_open {
            let claim = paper::reserve(
                db,
                c,
                cycle.cfg,
                p,
                cycle.now,
                &context.bundle,
                primary(&triggers),
            )?;
            let Some(claim) = claim else {
                return self
                    .skip(c, lane, &triggers, "agent_paper_authority_unavailable")
                    .map(|()| None);
            };
            Some(claim)
        } else {
            None
        };
        Ok(Some(Prepared {
            job: Job {
                c: c.clone(),
                cfg: cycle.cfg.clone(),
                work: Work {
                    lane,
                    triggers,
                    brief,
                    context,
                    paper_turn_id: claim.as_ref().map(paper::turn_id),
                    research_loop: None,
                },
            },
            claim,
            consumes_agenda,
        }))
    }

    /// The lane a decision batch runs on first: the gate when one is
    /// configured with budget left and every trigger is an F&O signal. Cash
    /// entry candidates, fills, position risk and scheduled turns reach the
    /// decision lane directly. An exhausted gate passes its batches through.
    fn screen(
        &self,
        c: &Config,
        lane: Lane,
        triggers: &[Trigger],
        spent: &BTreeMap<Lane, Usage>,
    ) -> Result<Lane> {
        if lane != Lane::Decision
            || !self.settings.lanes.contains_key(&Lane::Gate)
            || !triggers
                .iter()
                .all(|trigger| trigger.kind == TriggerKind::FoSignal)
        {
            return Ok(lane);
        }
        if self.fraction(Lane::Gate, spent) >= 1.0 {
            append(
                c,
                "triggers.ndjson",
                &json!({"event":"gate_bypassed","reason":"agent_budget_exhausted","triggers":triggers}),
            )?;
            return Ok(Lane::Decision);
        }
        Ok(Lane::Gate)
    }

    fn context(&self, cycle: &Cycle<'_>) -> Result<TurnContext> {
        let (bundle, _) = planner::call_time_input(
            cycle.c,
            cycle.cfg,
            cycle.portfolio,
            cycle.instruments,
            &|| cycle.now,
        )?;
        let snapshot = cycle
            .cfg
            .decision_evidence
            .as_ref()
            .map(|settings| {
                decision_evidence::Snapshot::capture(
                    settings,
                    cycle.c.session,
                    cycle.now,
                    cycle.instruments,
                    cycle.scanner,
                    cycle.store,
                    cycle.index.clone(),
                )
            })
            .transpose()?;
        Ok(TurnContext {
            bundle,
            backend: Box::new(LaneBackend(snapshot)),
            portfolio: cycle.portfolio.clone(),
            instruments: cycle.instruments.to_vec(),
            web: Box::new(crate::runtime::web_tools::Network),
        })
    }

    fn settle(&mut self, cycle: &Cycle<'_>, db: &mut Connection, done: Done) -> Result<()> {
        let c = cycle.c;
        self.busy = None;
        self.preempt.store(false, Ordering::Release);
        if done.loop_report.is_some() || (done.triggers.is_empty() && done.error.is_some()) {
            // Back to back while it progresses; otherwise wait as it asked.
            let next = done
                .loop_report
                .as_ref()
                .map_or(cycle.now.saturating_add(CONTEXT_RETRY_MS), |report| {
                    report.next_wake_ms
                });
            self.loop_next_ms = next.max(cycle.now.saturating_add(1_000));
            self.status = json!({"status":"idle","last":{"lane":done.lane,"research_loop":done.loop_report,
                "error":done.error,"settled_ms":cycle.now},"mode":self.mode});
            return Ok(());
        }
        let paper_decision = done.lane == Lane::Decision && self.mode == AgentMode::Paper;
        if let Some(retry_at) = done.retry_at_ms {
            self.hold_until_ms = retry_at;
            if !paper_decision {
                self.engine.requeue(queue(done.lane), done.triggers.clone());
            }
        } else if done.lane == Lane::Gate {
            // A gate that failed or gave no verdict escalates: a broken screen
            // must not silence F&O triggers. The journal says which it was.
            let (escalate, reason, prospects) = match &done.verdict {
                Some(verdict) => (
                    verdict.escalate,
                    verdict.reason.as_str(),
                    verdict.prospects.clone(),
                ),
                None => (
                    true,
                    done.error.as_deref().unwrap_or("agent_gate_no_verdict"),
                    Vec::new(),
                ),
            };
            append(
                c,
                "triggers.ndjson",
                &json!({"event":"gated","at_ms":cycle.now,"escalate":escalate,
                    "verdict":done.verdict.is_some(),"reason":reason,"prospects":prospects,
                    "triggers":done.triggers}),
            )?;
            if escalate {
                self.escalated = Some(Escalation {
                    triggers: done.triggers.clone(),
                    prospects,
                });
            }
        }
        let paper_status = if let Some(claim) = self.pending_claim.as_ref() {
            let result = commit_done(cycle, db, claim, &done);
            let status = match result {
                Ok(_) => "committed_for_paper_execution".to_owned(),
                Err(error) => {
                    let reason = paper::failure_reason(error.as_ref());
                    paper::refuse(db, c, claim, &reason)?;
                    reason
                }
            };
            self.pending_claim = None;
            Some(status)
        } else {
            let pre_open = done.triggers.iter().all(|t| t.kind == TriggerKind::PreOpen);
            (paper_decision && !pre_open).then(|| "agent_paper_claim_unavailable".to_owned())
        };
        if let Some(text) = &done.review {
            self.agenda = Some(text.chars().take(4_000).collect());
            self.engine.push(Trigger::new(
                TriggerKind::Agenda,
                "review_agenda",
                cycle.now,
            ));
        }
        let plan = done.plan.as_ref().map(|(turn_id, attempt)| {
            let status = paper_status.as_deref().unwrap_or(match attempt {
                PlanAttempt::Validated { .. } => "shadow_not_executed",
                PlanAttempt::Refused { .. } => "refused_by_validation",
            });
            crate::runtime::transcript::redact(&json!({"status":status,"record":{
                "turn_id":turn_id,"mode":self.mode,"at_ms":cycle.now,"attempt":attempt
            }}))
        });
        self.status = json!({"status":"idle","last":{"lane":done.lane,"turns":done.turns,
            "error":done.error,"plan":plan,"paper_status":paper_status,"settled_ms":cycle.now},"mode":self.mode});
        if let Some(plan) = plan {
            append(c, "plans.ndjson", &plan)?;
        }
        Ok(())
    }

    /// Abort a running turn and wait for the worker to shut down its agent.
    pub(crate) fn stop(mut self) {
        self.jobs = None;
        self.preempt.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn commit_done(
    cycle: &Cycle<'_>,
    db: &mut Connection,
    claim: &redecision::Claim,
    done: &Done,
) -> Result<Value> {
    if done.lane != Lane::Decision || !cycle.decision_ready || cycle.sealed {
        return Err("agent_paper_book_not_ready".into());
    }
    let (Some(input), Some((turn_id, PlanAttempt::Validated { plan, .. })), [turn]) =
        (&done.paper_input, &done.plan, done.turns.as_slice())
    else {
        return Err("agent_paper_no_validated_decision".into());
    };
    let mut context = decision::Context {
        db,
        portfolio: cycle.portfolio,
        instruments: cycle.instruments,
    };
    paper::commit(
        &mut context,
        cycle.c,
        cycle.cfg,
        claim,
        paper::Completed {
            turn_id,
            provider: turn["provider"]
                .as_str()
                .ok_or("agent_paper_turn_binding_invalid")?,
            model: turn["model"]
                .as_str()
                .ok_or("agent_paper_turn_binding_invalid")?,
            proposal: plan,
            input,
        },
        crate::runtime::millis(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_review_waits_for_context_before_claiming_the_seal() {
        let dir = tempfile::tempdir().unwrap();
        let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
        cfg.decision_evidence = Some(decision_evidence::Settings {
            max_age_ms: 0,
            ..Default::default()
        });
        let mut service = LaneService::start(&cfg).unwrap();
        let portfolio = crate::runtime::tests::test_portfolio();
        let scanner = Scanner::default();
        let store = FeatureStore::new(Default::default()).unwrap();
        let cycle = Cycle {
            c: &c,
            cfg: &cfg,
            now: 20_000,
            minute: 930,
            sealed: true,
            portfolio: &portfolio,
            instruments: &[],
            ranking: &[],
            ranked_at: 0,
            decision_ready: false,
            prepared: false,
            scanner: &scanner,
            store: &store,
            index: None,
            regime: None,
            spot: None,
        };

        let mut db = Connection::open_in_memory().unwrap();
        service.tick(&cycle, &mut db).unwrap();

        assert!(service.busy.is_none());
        assert!(!lanes::root(&c).join("review-claimed.json").exists());
        assert_eq!(service.engine.next().unwrap().0, Lane::Review);
        let journal = std::fs::read_to_string(lanes::root(&c).join("triggers.ndjson")).unwrap();
        let deferred: Value = serde_json::from_str(journal.lines().last().unwrap()).unwrap();
        assert_eq!(deferred["event"], "deferred");
        assert_eq!(deferred["retry_at_ms"], cycle.now + CONTEXT_RETRY_MS);
        service.stop();
    }

    #[test]
    fn a_prepared_book_chooses_its_strategy_at_nine_without_a_paper_claim() {
        let dir = tempfile::tempdir().unwrap();
        let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
        cfg.strategy_execution = Some(planner::StrategyExecution {
            enabled: true,
            fallback: vec!["vwap_trend".into()],
            ..Default::default()
        });
        let mut service = LaneService::start(&cfg).unwrap();
        let portfolio = crate::runtime::tests::test_portfolio();
        let scanner = Scanner::default();
        let store = FeatureStore::new(Default::default()).unwrap();
        let nine = chrono::DateTime::parse_from_rfc3339(&format!("{}T09:00:00+05:30", c.session))
            .unwrap()
            .timestamp_millis();
        let cycle = |now: i64, minute, prepared| Cycle {
            c: &c,
            cfg: &cfg,
            now,
            minute,
            sealed: false,
            portfolio: &portfolio,
            instruments: &[],
            ranking: &[],
            ranked_at: 0,
            decision_ready: false,
            prepared,
            scanner: &scanner,
            store: &store,
            index: None,
            regime: None,
            spot: None,
        };
        let mut db = crate::runtime::open_book(&c).unwrap();
        // History not yet prepared: nothing fires, even at nine.
        service.tick(&cycle(nine, 540, false), &mut db).unwrap();
        assert!(service.busy.is_none());
        service
            .tick(&cycle(nine - 60_000, 539, true), &mut db)
            .unwrap();
        assert!(service.busy.is_none());
        service.tick(&cycle(nine, 540, true), &mut db).unwrap();
        assert_eq!(service.busy, Some(Lane::Decision));
        assert!(service.pending_claim.is_none());
        let journal = std::fs::read_to_string(lanes::root(&c).join("triggers.ndjson")).unwrap();
        let events: Vec<Value> = journal
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let seen: Vec<(&str, &str)> = events
            .iter()
            .map(|e| {
                (
                    e["event"].as_str().unwrap(),
                    e["trigger"]["kind"]
                        .as_str()
                        .or(e["lane"].as_str())
                        .unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("fired", "pre_open"),
                ("fired", "strategy_search"),
                ("dispatched", "decision"),
            ]
        );
        // The research search waits its turn behind the decision.
        assert_eq!(service.engine.next().unwrap().0, Lane::Research);
        service.stop();
    }

    #[test]
    fn the_gate_screens_fo_signals_and_its_valid_prospects_reach_the_decision_brief() {
        use crate::runtime::options::chain::tests::{catalog, depth, spot};
        let dir = tempfile::tempdir().unwrap();
        let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
        let agent = cfg.agent.as_mut().unwrap();
        agent.mode = AgentMode::Shadow;
        let mut gate = agent.lanes[&Lane::Decision].clone();
        gate.daily_calls = 2;
        agent.lanes.insert(Lane::Gate, gate);
        let settings = agent.clone();
        let mut service = LaneService::start(&cfg).unwrap();
        let signal = || vec![Trigger::new(TriggerKind::FoSignal, "premium_move", 1)];
        let mut spent = BTreeMap::new();
        let route = |service: &LaneService, triggers: &[Trigger], spent: &BTreeMap<_, _>| {
            service.screen(&c, Lane::Decision, triggers, spent).unwrap()
        };

        // Only F&O batches meet the gate; cash entry candidates go straight
        // to the decision lane, and so does any batch with a fill.
        assert_eq!(route(&service, &signal(), &spent), Lane::Gate);
        let cash = [Trigger::new(
            TriggerKind::EntryCandidate,
            "entered_top_k",
            1,
        )];
        assert_eq!(route(&service, &cash, &spent), Lane::Decision);
        let mut mixed = signal();
        mixed.push(Trigger::new(TriggerKind::Fill, "fill_batch", 1));
        assert_eq!(route(&service, &mixed, &spent), Lane::Decision);
        spent.insert(
            Lane::Gate,
            Usage {
                calls: 2,
                ..Usage::default()
            },
        );
        assert_eq!(route(&service, &signal(), &spent), Lane::Decision);
        spent.clear();

        let now = c
            .session
            .and_hms_opt(4, 30, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let mut portfolio = crate::runtime::tests::test_portfolio();
        portfolio.cash = nivra_core::finance::Amount::parse("15000").unwrap();
        portfolio.options = Some(catalog("26040"));
        let mut scanner = Scanner::default();
        depth(&mut scanner, "NSE_FO|26000CE", "100", "100.20", now - 100);
        let store = FeatureStore::new(Default::default()).unwrap();
        let cycle = Cycle {
            c: &c,
            cfg: &cfg,
            now,
            minute: 600,
            sealed: false,
            portfolio: &portfolio,
            instruments: &[],
            ranking: &[],
            ranked_at: 0,
            decision_ready: true,
            prepared: true,
            scanner: &scanner,
            store: &store,
            index: None,
            regime: None,
            spot: Some(spot("26040", now - 50)),
        };
        let mut db = Connection::open_in_memory().unwrap();
        let supervision = SupervisionState::default();
        service.refresh_chain(&cycle).unwrap();
        let prepare = |service: &mut LaneService, db: &mut Connection| {
            service
                .prepare(&cycle, db, &[], Phase::Open, &spent, &supervision)
                .unwrap()
                .unwrap()
                .job
                .work
        };

        // The gate's brief carries the chain and the F&O thresholds.
        service.engine.push(signal().remove(0));
        let mut work = prepare(&mut service, &mut db);
        assert_eq!(work.lane, Lane::Gate);
        let chain = work.brief.fo.as_ref().unwrap();
        assert_eq!(chain.rows[0].instrument_key, "NSE_FO|26000CE");
        assert_eq!(work.brief.budget.fo_bar.unwrap().spot_move_bps, 30.0);

        // A scripted gate escalates four prospects; the host keeps the one
        // that names a contract in the universe with a matching idea.
        let prospect = |key: &str, idea: &str| json!({"instrument_key":key,"idea":idea,"evidence":"premium +20%, 4 ticks, 4 lots","why":"momentum at the money"});
        let (client, peer) = super::super::tests::connect(move |peer| {
            let (id, _) = peer.turn();
            let reply = peer.call(&id, "verdict", json!({"escalate":true,"reason":"ATM call premium jumped",
                "prospects":[prospect("NSE_FO|26000CE","buy_call"),prospect("NSE_FO|99999CE","buy_call"),
                    prospect("NSE_FO|26000PE","buy_call"),prospect("NSE_FO|26000CE","buy_call")]}));
            peer.finish(&id, "ok", "", 1, 0.0);
            reply
        });
        let mut fixture = super::super::tests::fixture_agent(client);
        let clock = move || now;
        let mut runner = LaneRunner::new(&mut fixture, &settings, &c, &cfg, &clock);
        let done = run(&mut runner, &mut work);
        drop(runner);
        let reply = peer.join().unwrap();
        assert_eq!(reply["output"]["prospects"], json!(["NSE_FO|26000CE"]));
        let dropped: Vec<&str> = reply["output"]["dropped"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["reason"].as_str().unwrap())
            .collect();
        assert_eq!(
            dropped,
            [
                "prospect_not_in_option_universe",
                "prospect_idea_mismatch",
                "prospect_duplicate"
            ]
        );
        service.settle(&cycle, &mut db, done).unwrap();

        // The next decision turn runs the escalated batch with the prospect,
        // its terms taken from the option universe.
        let work = prepare(&mut service, &mut db);
        assert_eq!(work.lane, Lane::Decision);
        assert_eq!(work.triggers[0].kind, TriggerKind::FoSignal);
        let prospects = &work.brief.gate_prospects;
        assert_eq!(prospects.len(), 1);
        let p = &prospects[0];
        assert_eq!((p.rank, p.instrument_key.as_str()), (1, "NSE_FO|26000CE"));
        assert_eq!(
            (p.strike.as_str(), p.expiry.as_str(), p.lot_size),
            ("26000", "2026-10-20", 65)
        );
        assert_eq!(p.idea, super::super::tools::Idea::BuyCall);
        let brief: Value = serde_json::from_str(&work.brief.render().unwrap()).unwrap();
        assert_eq!(brief["gate_prospects"][0]["why"], "momentum at the money");
        assert!(brief["fo"]["rows"].is_array());

        // A declined screen ends the batch; a failed one escalates without prospects.
        let gated = |verdict: Option<Verdict>, error: Option<&str>| Done {
            lane: Lane::Gate,
            triggers: signal(),
            turns: Vec::new(),
            plan: None,
            paper_input: None,
            review: None,
            verdict,
            error: error.map(str::to_owned),
            retry_at_ms: None,
            loop_report: None,
        };
        let declined = Verdict {
            escalate: false,
            reason: "wide".into(),
            prospects: Vec::new(),
        };
        service
            .settle(&cycle, &mut db, gated(Some(declined), None))
            .unwrap();
        assert!(service.escalated.is_none());
        service
            .settle(&cycle, &mut db, gated(None, Some("agent_turn_timeout")))
            .unwrap();
        assert_eq!(
            service.escalated.take(),
            Some(Escalation {
                triggers: signal(),
                prospects: Vec::new()
            })
        );
        let journal = std::fs::read_to_string(lanes::root(&c).join("triggers.ndjson")).unwrap();
        let gated: Vec<Value> = journal
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|e| e["event"] == "gated")
            .collect();
        assert_eq!(gated[0]["prospects"][0]["instrument_key"], "NSE_FO|26000CE");
        assert_eq!(gated[2]["reason"], "agent_turn_timeout");
        service.stop();
    }

    #[test]
    fn the_pre_open_choice_and_a_gated_fo_escalation_share_one_session() {
        use crate::runtime::options::chain::tests::{catalog, depth, spot};
        let dir = tempfile::tempdir().unwrap();
        let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
        cfg.strategy_execution = Some(planner::StrategyExecution {
            enabled: true,
            fallback: vec!["vwap_trend".into()],
            ..Default::default()
        });
        let agent = cfg.agent.as_mut().unwrap();
        agent.mode = AgentMode::Shadow;
        let mut gate = agent.lanes[&Lane::Decision].clone();
        gate.daily_calls = 2;
        agent.lanes.insert(Lane::Gate, gate);
        let mut service = LaneService::start(&cfg).unwrap();
        // Hold the worker's channels so each turn is settled by hand.
        let (jobs, inbox) = sync_channel(1);
        let (out, done) = channel();
        service.jobs = Some(jobs);
        service.done = done;
        let mut portfolio = crate::runtime::tests::test_portfolio();
        portfolio.cash = nivra_core::finance::Amount::parse("15000").unwrap();
        portfolio.options = Some(catalog("26040"));
        let store = FeatureStore::new(Default::default()).unwrap();
        let at = |hh, mm| {
            chrono::DateTime::parse_from_rfc3339(&format!("{}T{hh:02}:{mm:02}:00+05:30", c.session))
                .unwrap()
                .timestamp_millis()
        };
        let mut db = crate::runtime::open_book(&c).unwrap();
        // One scan: a fresh at-the-money call quote and a fresh spot.
        let mut tick =
            |service: &mut LaneService, now: i64, minute: u32, bid: &str, index: &str| {
                let mut scanner = Scanner::default();
                let ask = format!("{bid}.20");
                depth(&mut scanner, "NSE_FO|26000CE", bid, &ask, now - 100);
                let cycle = Cycle {
                    c: &c,
                    cfg: &cfg,
                    now,
                    minute,
                    sealed: false,
                    portfolio: &portfolio,
                    instruments: &[],
                    ranking: &[],
                    ranked_at: 0,
                    decision_ready: minute >= c.execution.session_start_minute,
                    prepared: true,
                    scanner: &scanner,
                    store: &store,
                    index: None,
                    regime: None,
                    spot: Some(spot(index, now - 50)),
                };
                service.tick(&cycle, &mut db).unwrap();
                inbox.try_recv().ok().map(|job| job.work)
            };
        let finish = |work: &Work, verdict: Option<Verdict>| {
            out.send(Done {
                lane: work.lane,
                triggers: work.triggers.clone(),
                turns: Vec::new(),
                plan: None,
                paper_input: None,
                review: None,
                verdict,
                error: None,
                retry_at_ms: None,
                loop_report: None,
            })
            .unwrap();
        };

        // 09:00: the pre-open turn chooses the strategy. The chain is visible
        // but raises nothing before the open.
        let pre_open = tick(&mut service, at(9, 0), 540, "100", "26040").unwrap();
        assert_eq!(pre_open.lane, Lane::Decision);
        assert!(
            pre_open
                .triggers
                .iter()
                .all(|t| t.kind == TriggerKind::PreOpen)
        );
        assert_eq!(pre_open.brief.task["kind"], "pre_open");
        assert!(pre_open.brief.fo.is_some());
        assert!(pre_open.brief.gate_prospects.is_empty());
        finish(&pre_open, None);

        // 10:00: the research search that fired with it runs next.
        let search = tick(&mut service, at(10, 0), 600, "100", "26040").unwrap();
        assert_eq!(search.lane, Lane::Research);
        assert_eq!(search.brief.task["kind"], "strategy_search");
        finish(&search, None);

        // 10:01: spot +40 bps and the call premium +20 %. The pre-open turn
        // did not swallow the move: the F&O batch meets the gate.
        let screen = tick(&mut service, at(10, 1), 601, "120", "26144").unwrap();
        assert_eq!(screen.lane, Lane::Gate);
        assert!(
            screen
                .triggers
                .iter()
                .all(|t| t.kind == TriggerKind::FoSignal)
        );
        assert_eq!(screen.brief.budget.fo_bar.unwrap().spot_move_bps, 30.0);
        let (prospects, dropped) = super::super::tools::validate_prospects(
            &[json!({"instrument_key":"NSE_FO|26000CE","idea":"buy_call",
                "evidence":"premium +20%","why":"momentum at the money"})],
            portfolio.options.as_ref(),
        );
        assert!(dropped.is_empty());
        finish(
            &screen,
            Some(Verdict {
                escalate: true,
                reason: "ATM call premium jumped".into(),
                prospects,
            }),
        );

        // 10:02: the escalated batch reaches the decision lane as an ordinary
        // turn with the gate's prospect, not as a pre-open choice.
        let decision = tick(&mut service, at(10, 2), 602, "120", "26144").unwrap();
        assert_eq!(decision.lane, Lane::Decision);
        assert_eq!(decision.triggers, screen.triggers);
        assert!(decision.brief.task.is_null());
        assert_eq!(
            decision.brief.gate_prospects[0].instrument_key,
            "NSE_FO|26000CE"
        );
        let journal = std::fs::read_to_string(lanes::root(&c).join("triggers.ndjson")).unwrap();
        let events: Vec<String> = journal
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .map(|e| {
                let what = e["trigger"]["kind"].as_str().or(e["lane"].as_str());
                format!("{} {}", e["event"].as_str().unwrap(), what.unwrap_or(""))
            })
            .collect();
        assert_eq!(
            events,
            [
                "fired pre_open",
                "fired strategy_search",
                "dispatched decision",
                "dispatched research",
                "fired fo_signal",
                "fired fo_signal",
                "dispatched gate",
                "gated ",
                "dispatched decision",
            ]
        );
        service.stop();
    }
}
