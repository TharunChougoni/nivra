//! The controller's side of the pi lanes. Each scan cycle feeds the trigger
//! engine; a due lane turn runs on one worker thread that owns the agent
//! process. The controller settles its claim before a paper plan can execute.
use super::brief::{self, Brief, Facts, RegimeFacts, SessionFacts};
use super::client::{Agent, AgentClient};
use super::lanes::{self, LaneRunner, TurnContext};
use super::paper;
use super::protocol::{TurnStatus, Usage};
use super::requests::Requests;
use super::tools::{PlanAttempt, Verdict};
use super::triggers::{
    Observation, Phase, Ranked, Trigger, TriggerEngine, TriggerKind, ranker_registry,
};
use super::{AgentMode, AgentSettings, Lane};
use crate::runtime::planner::{self, Backend, PlannerConfig};
use crate::runtime::wide::strategy_run::{self, SupervisionState};
use crate::runtime::{Config, Connection, Result, decision, decision_evidence, redecision};
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
const AFTER_CLOSE_TASK: &str = "The session is closed and speed does not matter. Read today's review when present. From today's recorded quote tape, write or revise strategies in the workspace, backtest each one after costs on today's session and on earlier datasets, and keep a short note per strategy under notes/ with its source hash, the costs assumed and the net result.";

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
    pub scanner: &'a Scanner,
    pub store: &'a FeatureStore,
    pub index: Option<Value>,
    pub regime: Option<RegimeFacts>,
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
    /// Entry triggers the gate escalated; the next dispatch runs them on the
    /// decision lane without screening them again.
    escalated: Option<Vec<Trigger>>,
    /// This process claimed the after-close marker, so a requeued batch may
    /// still run its turn.
    after_close_claimed: bool,
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
    };
    let result = (|| -> Result<()> {
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
            after_close_claimed: false,
            status: json!({"status":"idle"}),
        })
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
                    .is_some_and(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
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
            strategy_supervision: supervision.any_eligible,
            strategy_fills,
            regime: cycle.regime.as_ref().map(|r| r.label.as_str()),
        });
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
            && phase.trading()
            && self.fraction(Lane::Decision, &spent) < 1.0
            && self.engine.decision_waiting()
            && (!cycle.ranking.is_empty()
                || !held.is_empty()
                || fired.iter().any(|t| t.kind == TriggerKind::Fill)
                || supervision.any_eligible)
        {
            self.preempt.store(true, Ordering::Release);
        }
        if self.busy.is_none() && cycle.now >= self.hold_until_ms {
            self.dispatch(cycle, db, &held, phase, &spent, &supervision)?;
        }
        Ok(())
    }

    fn skip(&self, c: &Config, lane: Lane, triggers: &[Trigger], reason: &str) -> Result<()> {
        append(
            c,
            "triggers.ndjson",
            &json!({"event":"skipped","lane":lane,"reason":reason,"triggers":triggers}),
        )
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
        let strategy_active = supervision.strategy_active();
        let (lane, mut triggers) = match self.escalated.take() {
            Some(triggers) => (Lane::Decision, triggers),
            None => {
                let Some((lane, triggers)) = self.engine.next() else {
                    return Ok(());
                };
                (
                    self.screen(c, lane, &triggers, spent, strategy_active)?,
                    triggers,
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
                return Ok(());
            }
            triggers = rest;
        }
        let Ok(settings) = self.settings.lane(lane) else {
            return self.skip(c, lane, &triggers, "agent_lane_not_configured");
        };
        let p = cycle.portfolio;
        if matches!(lane, Lane::Decision | Lane::Gate) {
            if !phase.trading() || p.halted || p.stopped || p.closed {
                return self.skip(c, lane, &triggers, "agent_book_not_trading");
            }
            if cycle.ranking.is_empty()
                && held.is_empty()
                && !triggers
                    .iter()
                    .any(|trigger| trigger.kind == TriggerKind::Fill)
                && !(supervision.any_eligible
                    && triggers
                        .iter()
                        .any(|trigger| trigger.kind == TriggerKind::Schedule))
            {
                return self.skip(c, lane, &triggers, "agent_no_eligible_evidence");
            }
        }
        if self.fraction(lane, spent) >= 1.0
            || (lane == Lane::Gate && self.fraction(Lane::Decision, spent) >= 1.0)
        {
            return self.skip(c, lane, &triggers, "agent_budget_exhausted");
        }
        let Some(jobs) = self.jobs.as_ref() else {
            return self.skip(c, lane, &triggers, "agent_worker_unavailable");
        };
        let context = match self.context(cycle) {
            Ok(context) => context,
            Err(error) => {
                self.hold_until_ms = cycle.now.saturating_add(CONTEXT_RETRY_MS);
                let entry = json!({"event":"deferred","lane":lane,"at_ms":cycle.now,
                    "reason":crate::runtime::harness::failure_reason(error.as_ref()),
                    "retry_at_ms":self.hold_until_ms,"triggers":triggers});
                self.engine.requeue(queue(lane), triggers);
                return append(c, "triggers.ndjson", &entry);
            }
        };
        if lane == Lane::Review && !lanes::claim(c, lanes::REVIEW_MARKER)? {
            return self.skip(c, lane, &triggers, "agent_review_already_ran");
        }
        let after_close = |t: &Trigger| t.kind == TriggerKind::AfterClose;
        if !self.after_close_claimed && triggers.iter().any(after_close) {
            if lanes::claim(c, lanes::AFTER_CLOSE_MARKER)? {
                self.after_close_claimed = true;
            } else {
                let (ran, rest): (Vec<_>, Vec<_>) = triggers.into_iter().partition(after_close);
                self.skip(c, lane, &ran, "agent_after_close_already_ran")?;
                if rest.is_empty() {
                    return Ok(());
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
        let budget = brief::budget(
            lane,
            &used,
            settings.daily_calls,
            settings.daily_cost_usd,
            self.engine.entry_bar(self.fraction(Lane::Decision, spent)),
        );
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
            Lane::Research => json!({"kind":triggers[0].kind}),
            Lane::Review => json!({"kind":"review","session":c.session.to_string()}),
            Lane::Gate => json!({"kind":"gate"}),
            Lane::Decision => Value::Null,
        };
        let mut brief = Brief::build(&facts, budget, task, &triggers);
        if lane == Lane::Decision {
            brief.strategies = brief::strategy_facts(db, c, cycle.cfg);
            brief.execution_refusals =
                crate::runtime::wide::recent_refusals(db, brief::MAX_REFUSALS)?;
        }
        let claim = if lane == Lane::Decision && self.mode == AgentMode::Paper {
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
                return self.skip(c, lane, &triggers, "agent_paper_authority_unavailable");
            };
            Some(claim)
        } else {
            None
        };
        let job = Job {
            c: c.clone(),
            cfg: cycle.cfg.clone(),
            work: Work {
                lane,
                triggers: triggers.clone(),
                brief,
                context,
                paper_turn_id: claim.as_ref().map(paper::turn_id),
            },
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

    /// The lane a decision batch runs on first: the gate when one is
    /// configured with budget left and every trigger is an entry candidate,
    /// unless an active strategy owns cash entries. Fills, position risk and
    /// scheduled turns always reach the decision lane directly. An exhausted
    /// gate passes its batches through.
    fn screen(
        &self,
        c: &Config,
        lane: Lane,
        triggers: &[Trigger],
        spent: &BTreeMap<Lane, Usage>,
        strategy_active: bool,
    ) -> Result<Lane> {
        if strategy_active
            || lane != Lane::Decision
            || !self.settings.lanes.contains_key(&Lane::Gate)
            || !triggers
                .iter()
                .all(|trigger| trigger.kind == TriggerKind::EntryCandidate)
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
        let paper_decision = done.lane == Lane::Decision && self.mode == AgentMode::Paper;
        if let Some(retry_at) = done.retry_at_ms {
            self.hold_until_ms = retry_at;
            if !paper_decision {
                self.engine.requeue(queue(done.lane), done.triggers.clone());
            }
        } else if done.lane == Lane::Gate {
            // A gate that failed or gave no verdict escalates: a broken screen
            // must not silence entry triggers. The journal says which it was.
            let (escalate, reason) = match &done.verdict {
                Some(verdict) => (verdict.escalate, verdict.reason.as_str()),
                None => (
                    true,
                    done.error.as_deref().unwrap_or("agent_gate_no_verdict"),
                ),
            };
            append(
                c,
                "triggers.ndjson",
                &json!({"event":"gated","at_ms":cycle.now,"escalate":escalate,
                    "verdict":done.verdict.is_some(),"reason":reason,"triggers":done.triggers}),
            )?;
            if escalate {
                self.escalated = Some(done.triggers.clone());
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
            paper_decision.then(|| "agent_paper_claim_unavailable".to_owned())
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
            scanner: &scanner,
            store: &store,
            index: None,
            regime: None,
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
    fn the_gate_screens_only_entry_candidates_and_escalates_unless_it_declines() {
        let dir = tempfile::tempdir().unwrap();
        let (c, mut cfg) = crate::runtime::decision::decision_fixture_config(dir.path());
        let agent = cfg.agent.as_mut().unwrap();
        let mut gate = agent.lanes[&Lane::Decision].clone();
        gate.daily_calls = 2;
        agent.lanes.insert(Lane::Gate, gate);
        let mut service = LaneService::start(&cfg).unwrap();
        let entry = || {
            vec![Trigger::new(
                TriggerKind::EntryCandidate,
                "entered_top_k",
                1,
            )]
        };
        let mut spent = BTreeMap::new();

        assert_eq!(
            service
                .screen(&c, Lane::Decision, &entry(), &spent, false)
                .unwrap(),
            Lane::Gate
        );
        assert_eq!(
            service
                .screen(&c, Lane::Decision, &entry(), &spent, true)
                .unwrap(),
            Lane::Decision
        );
        let mut mixed = entry();
        mixed.push(Trigger::new(TriggerKind::Fill, "fill_batch", 1));
        assert_eq!(
            service
                .screen(&c, Lane::Decision, &mixed, &spent, false)
                .unwrap(),
            Lane::Decision
        );
        spent.insert(
            Lane::Gate,
            Usage {
                calls: 2,
                ..Usage::default()
            },
        );
        assert_eq!(
            service
                .screen(&c, Lane::Decision, &entry(), &spent, false)
                .unwrap(),
            Lane::Decision
        );

        let portfolio = crate::runtime::tests::test_portfolio();
        let scanner = Scanner::default();
        let store = FeatureStore::new(Default::default()).unwrap();
        let cycle = Cycle {
            c: &c,
            cfg: &cfg,
            now: 20_000,
            minute: 600,
            sealed: false,
            portfolio: &portfolio,
            instruments: &[],
            ranking: &[],
            ranked_at: 0,
            decision_ready: true,
            scanner: &scanner,
            store: &store,
            index: None,
            regime: None,
        };
        let mut db = Connection::open_in_memory().unwrap();
        let done = |verdict: Option<Verdict>, error: Option<&str>| Done {
            lane: Lane::Gate,
            triggers: entry(),
            turns: Vec::new(),
            plan: None,
            paper_input: None,
            review: None,
            verdict,
            error: error.map(str::to_owned),
            retry_at_ms: None,
        };
        let verdict = |escalate| {
            Some(Verdict {
                escalate,
                reason: "depth".into(),
            })
        };
        service
            .settle(&cycle, &mut db, done(verdict(false), None))
            .unwrap();
        assert!(service.escalated.is_none());
        service
            .settle(&cycle, &mut db, done(verdict(true), None))
            .unwrap();
        assert_eq!(service.escalated.take(), Some(entry()));
        // A failed screen must not silence the triggers.
        service
            .settle(&cycle, &mut db, done(None, Some("agent_turn_timeout")))
            .unwrap();
        assert_eq!(service.escalated.take(), Some(entry()));

        let journal = std::fs::read_to_string(lanes::root(&c).join("triggers.ndjson")).unwrap();
        let events: Vec<Value> = journal
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let kinds: Vec<(&str, Option<bool>)> = events
            .iter()
            .map(|e| (e["event"].as_str().unwrap(), e["escalate"].as_bool()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("gate_bypassed", None),
                ("gated", Some(false)),
                ("gated", Some(true)),
                ("gated", Some(true))
            ]
        );
        assert_eq!(events[3]["reason"], "agent_turn_timeout");
        service.stop();
    }
}
