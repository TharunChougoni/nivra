//! The strategy executor (#42). On each scan the paper controller freezes one
//! observation and calls the library's active strategy on a worker thread,
//! outside the book transaction. Only applying the returned actions takes the
//! transaction, and it validates them again against the book at that moment:
//! the planner checks, then the shared execution gate with the original quote
//! receipts. A failed call produces no actions and never halts the book.
//! Paper only.
use super::*;
use crate::runtime::controller_events::Notify;
use crate::runtime::strategy::{
    self, ActionResult, Observation, Prospect, WorkerConfig, WorkerKey, host::StrategyHost,
};
use crate::runtime::strategy_library::{
    Library,
    choice::{self, Running},
};
use nivra_core::market::MAX_FINANCIAL_AGE_MS;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod latency;
use std::collections::BTreeMap;

/// How often the library is read again, so a pin, disable or new entry takes
/// effect without a restart.
const RESOLVE_MS: i64 = 30_000;
/// Actions, refusals and fills the dashboard lists.
const RECENT: usize = 20;
const ACTION_TABLE: &str = "CREATE TABLE IF NOT EXISTS strategy_action(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,observed_at INTEGER NOT NULL,strategy_id TEXT NOT NULL,idx INTEGER NOT NULL,instrument_key TEXT,side TEXT,units INTEGER NOT NULL,outcome TEXT NOT NULL);";

/// Active strategy supervision state for the agent lanes and execution gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SupervisionState {
    /// The run id of what the executor runs: an accepted strategy id or a
    /// `fallback:<name>` reference.
    pub active_id: Option<String>,
    /// Strategy execution is on: the decision lane supervises the choice at
    /// the open, before it and on regime changes, even with nothing running.
    pub supervised: bool,
}

impl SupervisionState {
    pub(crate) fn strategy_active(&self) -> bool {
        self.active_id.is_some()
    }
    /// An accepted library version runs, not a fallback.
    pub(crate) fn accepted(&self) -> bool {
        self.active_id
            .as_deref()
            .is_some_and(|id| !choice::is_fallback(id))
    }
}

/// What the executor would run now when `strategy_execution` is enabled.
pub(crate) fn supervision_state(c: &Config, cfg: &planner::PlannerConfig) -> SupervisionState {
    if !cfg.runs_strategies() {
        return SupervisionState::default();
    }
    let active_id = crate::runtime::strategy_library::root(c)
        .ok()
        .and_then(|root| choice::choose(&Library::at(root), &cfg.strategy_policy().ok()?).ok())
        .and_then(|choice| choice.running.map(|running| running.run_id()));
    SupervisionState {
        active_id,
        supervised: true,
    }
}

/// Today's live `strategy_action` outcome counts for `strategy_id`.
pub(crate) fn outcome_counts(db: &Connection, strategy_id: &str) -> BTreeMap<String, u64> {
    let Ok(true) = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='strategy_action')",
        [],
        |r| r.get::<_, bool>(0),
    ) else {
        return BTreeMap::new();
    };
    let Ok(mut stmt) = db.prepare(
        "SELECT outcome, COUNT(*) FROM strategy_action WHERE strategy_id=? GROUP BY outcome",
    ) else {
        return BTreeMap::new();
    };
    let Ok(rows) = stmt.query_map([strategy_id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    }) else {
        return BTreeMap::new();
    };
    rows.flatten()
        .filter_map(|(outcome, count)| u64::try_from(count).ok().map(|n| (outcome, n)))
        .collect()
}

struct Active {
    running: Running,
    run_id: String,
    source: Vec<u8>,
    top_n: usize,
}

/// One scan's state, frozen for a call: the ranked prospects, the depth
/// quotes still within two seconds of their original receipt, the book and
/// the tradable instruments.
pub(super) struct Frozen {
    pub at_ms: i64,
    pub prospects: Vec<Prospect>,
    pub quotes: BTreeMap<String, Quote>,
    pub portfolio: Portfolio,
    pub universe: BTreeMap<String, Instrument>,
}
impl Frozen {
    pub(super) fn capture(
        scan: &Scanner,
        now: i64,
        top_n: usize,
        p: &Portfolio,
        tradable: &[Instrument],
    ) -> Result<Self> {
        Ok(Self {
            at_ms: now,
            prospects: strategy::prospects(scan, now, top_n)?,
            quotes: scan
                .samples
                .iter()
                .filter_map(|(key, sample)| {
                    sample
                        .executable
                        .as_ref()
                        .filter(|q| q.fresh(now, MAX_FINANCIAL_AGE_MS))
                        .map(|q| (key.clone(), q.clone()))
                })
                .collect(),
            portfolio: p.clone(),
            universe: tradable
                .iter()
                .map(|i| (i.key.clone(), i.clone()))
                .collect(),
        })
    }
}

#[derive(Clone)]
struct ActiveMeta {
    run_id: String,
    params: Value,
    top_n: usize,
    status_json: Value,
}

struct InFlight {
    worker_key: WorkerKey,
    activation_generation: u64,
    seq: u64,
    strategy_id: String,
    observed_at: i64,
    quotes: BTreeMap<String, Quote>,
    instruments: Vec<Instrument>,
    immediate_error: Option<&'static str>,
    timing: latency::Timing,
}

struct StrategyBook<'a> {
    db: &'a mut Connection,
    c: &'a Config,
    p: &'a mut Portfolio,
    scan: &'a mut Scanner,
}

pub(super) struct StrategyRun {
    library: Library,
    host: StrategyHost,
    /// Reference strategies to run, best first, while no accepted one can.
    policy: choice::ChoicePolicy,
    active: Option<Active>,
    known: BTreeMap<WorkerKey, ActiveMeta>,
    resolved_at: Option<i64>,
    /// Why no accepted version runs: no active entry, a build mismatch, a
    /// stand-down or a library read failure. Set beside a fallback too.
    blocked: Option<&'static str>,
    in_flight: Option<InFlight>,
    /// The failure last journaled, so a repeating one is written once.
    last_failure: Option<String>,
    calls: u64,
    failures: u64,
    skipped: u64,
    last_call_at_ms: Option<i64>,
}

impl StrategyRun {
    pub(super) fn new(
        library: Library,
        worker: WorkerConfig,
        policy: choice::ChoicePolicy,
        notify: Option<Notify>,
    ) -> Self {
        Self {
            library,
            host: StrategyHost::new(worker, notify),
            policy,
            active: None,
            known: BTreeMap::new(),
            resolved_at: None,
            blocked: None,
            in_flight: None,
            last_failure: None,
            calls: 0,
            failures: 0,
            skipped: 0,
            last_call_at_ms: None,
        }
    }

    fn worker_key(source: &[u8], params: &Value) -> Result<WorkerKey> {
        let params_bytes = serde_json::to_vec(params)?;
        Ok(WorkerKey {
            source_sha256: digest(source),
            params_sha256: digest(&params_bytes),
        })
    }

    fn status_json(running: &Running, run_id: &str) -> Value {
        match running {
            Running::Accepted { entry: e, .. } => json!({
                "strategy_id": e.strategy_id, "basis": "accepted", "author": e.author,
                "summary": e.summary, "tape_dates": e.tape_dates,
                "registered_at_ms": e.registered_at_ms,
            }),
            Running::Fallback(reference) => json!({
                "strategy_id": run_id, "basis": "fallback", "name": reference.name,
                "source_sha256": digest(reference.source),
            }),
        }
    }

    fn resolve(&self) -> Result<(Option<Active>, Option<&'static str>)> {
        let choice = choice::choose(&self.library, &self.policy)?;
        let active = match choice.running {
            Some(running) => Some(Active {
                top_n: running.top_n()?,
                run_id: running.run_id(),
                source: match &running {
                    Running::Accepted { entry, .. } => self.library.source(entry)?,
                    Running::Fallback(reference) => reference.source.to_vec(),
                },
                running,
            }),
            None => None,
        };
        Ok((active, choice.reason))
    }

    fn prune_known(&mut self) {
        let live = self.host.live_key().cloned();
        let desired = self.host.desired_key().cloned();
        self.known
            .retain(|key, _| live.as_ref() == Some(key) || desired.as_ref() == Some(key));
    }

    pub(super) fn time_until_due(&self, now: i64) -> Duration {
        if self
            .in_flight
            .as_ref()
            .is_some_and(|flight| flight.immediate_error.is_some())
        {
            return Duration::ZERO;
        }
        let until_refresh = self
            .resolved_at
            .map(|at| at.saturating_add(RESOLVE_MS).saturating_sub(now).max(0) as u64)
            .map_or(Duration::ZERO, Duration::from_millis);
        self.host
            .time_until_due(now)
            .map_or(until_refresh, |d| d.min(until_refresh))
    }

    pub(super) fn has_call(&self) -> bool {
        self.in_flight.is_some()
    }

    pub(super) fn refresh_choice(
        &mut self,
        db: &Connection,
        c: &Config,
        now: i64,
        changed: bool,
    ) -> Result<()> {
        if changed {
            self.resolved_at = None;
        }
        if self.resolved_at.is_none_or(|at| now - at >= RESOLVE_MS) {
            self.resolved_at = Some(now);
            self.active = None;
            self.blocked = None;
            match self.resolve() {
                Ok((active, reason)) => {
                    let quiet = matches!(
                        reason,
                        None | Some("no_active_strategy" | "strategy_lane_stood_down")
                    );
                    self.blocked = reason;
                    if let (Some(reason), false) = (reason, quiet || active.is_some()) {
                        self.fail(db, c, now, None, reason)?;
                    }
                    self.active = active;
                }
                Err(error) => {
                    self.blocked = Some("strategy_library_unavailable");
                    let reason = failure_code(error.as_ref());
                    self.fail(db, c, now, None, &reason)?;
                }
            }
            match &self.active {
                Some(active) => {
                    let key = Self::worker_key(&active.source, active.running.params())?;
                    self.known.insert(
                        key,
                        ActiveMeta {
                            run_id: active.run_id.clone(),
                            params: active.running.params().clone(),
                            top_n: active.top_n,
                            status_json: Self::status_json(&active.running, &active.run_id),
                        },
                    );
                    if let Err(reason) = self
                        .host
                        .sync_desired(Some((&active.source, active.running.params())), now)
                    {
                        let run_id = active.run_id.clone();
                        self.blocked = Some(reason);
                        self.active = None;
                        self.prune_known();
                        self.fail(db, c, now, Some(&run_id), reason)?;
                        return Ok(());
                    }
                }
                None => self.retire_call(db, now)?,
            }
            self.prune_known();
        }
        self.host.advance_activation(now);
        if self
            .in_flight
            .as_ref()
            .is_some_and(|flight| flight.activation_generation != self.host.activation_generation())
        {
            self.record_revoked(db, now)?;
        }
        Ok(())
    }

    /// The prospect count for a call to start now, if the live worker is ready.
    pub(super) fn ready(&mut self, db: &Connection, c: &Config, now: i64) -> Result<Option<usize>> {
        self.refresh_choice(db, c, now, false)?;
        if self.active.is_none() {
            return Ok(None);
        }
        if self.in_flight.is_some() {
            self.skipped += 1;
            return Ok(None);
        }
        match self.host.live_ready(now) {
            Ok(true) => {
                self.prune_known();
                let top_n = self
                    .host
                    .live_key()
                    .and_then(|key| self.known.get(key))
                    .map(|meta| meta.top_n)
                    .or_else(|| self.active.as_ref().map(|a| a.top_n));
                Ok(top_n)
            }
            Ok(false) => Ok(None),
            Err(reason) => {
                let run_id = self
                    .host
                    .live_key()
                    .or_else(|| self.host.desired_key())
                    .and_then(|key| self.known.get(key))
                    .map(|meta| meta.run_id.clone())
                    .or_else(|| self.active.as_ref().map(|a| a.run_id.clone()));
                self.fail(db, c, now, run_id.as_deref(), reason)?;
                Ok(None)
            }
        }
    }

    #[cfg(test)]
    pub(super) fn settle_startup(
        &mut self,
        db: &Connection,
        c: &Config,
        now: i64,
    ) -> Result<Option<usize>> {
        let top_n = self.ready(db, c, now)?;
        if self.host.desired_starting() {
            let _ = self.host.wait_live_ready(now);
            self.prune_known();
            if top_n.is_none() {
                return self.ready(db, c, now);
            }
        }
        Ok(top_n)
    }

    #[cfg(test)]
    pub(super) fn host(&self) -> &StrategyHost {
        &self.host
    }

    /// Submit `frozen` non-blockingly to the live worker. Call [`Self::ready`] first.
    pub(super) fn start(&mut self, frozen: Frozen, clock: &dyn Fn() -> i64) {
        if self.in_flight.is_some() {
            return;
        }
        let Some(live_key) = self.host.live_key().cloned() else {
            return;
        };
        let Some(meta) = self.known.get(&live_key) else {
            return;
        };
        let strategy_id = meta.run_id.clone();
        let mut flight = InFlight {
            worker_key: live_key,
            activation_generation: self.host.activation_generation(),
            seq: 0,
            timing: latency::Timing::new(strategy_id.clone(), frozen.at_ms, &frozen.quotes),
            strategy_id,
            observed_at: frozen.at_ms,
            quotes: frozen.quotes,
            instruments: Vec::new(),
            immediate_error: None,
        };
        let outcome = frozen
            .portfolio
            .costs
            .as_ref()
            .ok_or("paper_costs_required")
            .and_then(|costs| {
                let observation = Observation::new(
                    frozen.at_ms,
                    &meta.params,
                    &frozen.prospects,
                    &flight.quotes,
                    &frozen.portfolio,
                    &frozen.universe,
                    costs,
                );
                flight.instruments = observation.instruments().cloned().collect();
                let bytes = serde_json::to_vec(&observation).map_err(|_| "strategy_input_limit")?;
                let now = clock();
                flight.timing.submitted_at_ms = Some(now);
                self.host.submit_live(&bytes, now)
            });
        match outcome {
            Ok((activation_generation, seq)) => {
                flight.activation_generation = activation_generation;
                flight.seq = seq;
            }
            Err(reason) => flight.immediate_error = Some(reason),
        }
        self.calls += 1;
        self.last_call_at_ms = Some(frozen.at_ms);
        self.in_flight = Some(flight);
    }

    #[cfg(test)]
    pub(super) fn wait(&mut self) {
        while let Some(flight) = &self.in_flight {
            if flight.immediate_error.is_some()
                || flight.activation_generation != self.host.activation_generation()
                || self.host.is_call_finished(&flight.worker_key, i64::MAX)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Apply a finished call. `clock` is sampled after the book transaction
    /// begins, so the gate's freshness check uses the time of the fill.
    pub(super) fn poll(
        &mut self,
        db: &mut Connection,
        c: &Config,
        p: &mut Portfolio,
        scan: &mut Scanner,
        clock: &dyn Fn() -> i64,
    ) -> Result<()> {
        let Some(flight) = &self.in_flight else {
            return Ok(());
        };
        if flight.activation_generation != self.host.activation_generation() {
            return self.record_revoked(db, clock());
        }
        let (received_at_ms, outcome) = if let Some(reason) = flight.immediate_error {
            (None, Err(reason))
        } else {
            let Some(reply) = self.host.poll_call(&flight.worker_key, flight.seq, clock()) else {
                return Ok(());
            };
            (reply.received_at_ms, reply.outcome)
        };
        let mut flight = self.in_flight.take().expect("in_flight checked");
        flight.timing.reply_received_at_ms = received_at_ms;
        let outcome = outcome
            .and_then(|bytes| strategy::parse_actions(&bytes, &flight.instruments))
            .map_err(str::to_owned);
        match outcome {
            Ok(actions) if actions.is_empty() => {
                flight.timing.outcome = "no_actions".into();
                flight.timing.record(db)?;
                self.last_failure = None;
            }
            Ok(actions) => {
                match self.apply_cycle(StrategyBook { db, c, p, scan }, &mut flight, actions, clock)
                {
                    Ok(()) => self.last_failure = None,
                    Err(error) => self.record_failed_call(
                        db,
                        c,
                        clock(),
                        flight,
                        &failure_code(error.as_ref()),
                    )?,
                }
            }
            Err(reason) => self.record_failed_call(db, c, clock(), flight, &reason)?,
        }
        Ok(())
    }

    fn record_failed_call(
        &mut self,
        db: &Connection,
        c: &Config,
        now: i64,
        mut flight: InFlight,
        reason: &str,
    ) -> Result<()> {
        flight.timing.outcome = reason.to_owned();
        flight.timing.record(db)?;
        self.fail(db, c, now, Some(&flight.strategy_id), reason)
    }

    pub(super) fn retire_call(&mut self, db: &Connection, now: i64) -> Result<()> {
        if self.has_call() {
            self.record_revoked(db, now)?;
        }
        self.host.sync_desired(None, now)?;
        self.active = None;
        self.known.clear();
        Ok(())
    }

    fn record_revoked(&mut self, db: &Connection, now: i64) -> Result<()> {
        let mut flight = self.in_flight.take().expect("in_flight checked");
        flight.timing.reply_received_at_ms = self
            .host
            .poll_call(&flight.worker_key, flight.seq, now)
            .and_then(|reply| reply.received_at_ms);
        flight.timing.outcome = "strategy_activation_stale".into();
        flight.timing.record(db)
    }

    /// Journal a failure and note it in the day's diagnostics, once per
    /// change of reason. The book is left as it was.
    pub(super) fn fail(
        &mut self,
        db: &Connection,
        c: &Config,
        now: i64,
        strategy_id: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        self.failures += 1;
        if self.last_failure.as_deref() == Some(reason) {
            return Ok(());
        }
        self.last_failure = Some(reason.to_owned());
        let reason = diagnostics::code(reason);
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'strategy_failure')",
            rusqlite::params![
                now,
                json!({"strategy_id":strategy_id,"reason":reason}).to_string()
            ],
        )?;
        let stderr_tail = self.host.stderr_tail(None);
        diagnostics::note(
            &c.directory,
            now,
            "failure",
            json!({"stage":"strategy","strategy_id":strategy_id,"reason":reason,"stderr_tail":stderr_tail}),
        );
        Ok(())
    }

    /// The dashboard's view: the active entry, the counters, and the latest
    /// actions, gate refusals and fills.
    pub(super) fn status(&self, db: &Connection, p: &Portfolio) -> Value {
        let live_meta = self.host.live_key().and_then(|key| self.known.get(key));
        let state = if self.in_flight.is_some() {
            "calling"
        } else if live_meta.is_some() || self.active.is_some() {
            "ready"
        } else {
            "blocked"
        };
        let live_id = live_meta
            .map(|m| m.run_id.as_str())
            .or_else(|| self.active.as_ref().map(|a| a.run_id.as_str()));
        let fills: Vec<_> = live_id
            .map(|id| {
                p.fills
                    .iter()
                    .rev()
                    .filter(|f| f.decision_id.as_deref() == Some(id))
                    .take(RECENT)
                    .collect()
            })
            .unwrap_or_default();
        let strategy = live_meta.map(|m| m.status_json.clone()).or_else(|| {
            self.active
                .as_ref()
                .map(|a| Self::status_json(&a.running, &a.run_id))
        });
        json!({
            "enabled": true,
            "state": state,
            "reason": self.blocked,
            "strategy": strategy,
            "fallback": self.policy.fallback,
            "activation_generation": self.host.activation_generation(),
            "pool_size": self.host.pool_len(),
            "stderr_tail": self.host.stderr_tail(None),
            "calls": self.calls, "failures": self.failures, "skipped": self.skipped,
            "last_call_at_ms": self.last_call_at_ms, "last_failure": self.last_failure,
            "actions": recent_actions(db).unwrap_or_default(),
            "latency": latency::summary(db).unwrap_or_else(|_| json!({"error":"strategy_latency_unavailable"})),
            "refusals": super::recent_refusals(db, 50)
                .map(|rows| rows.into_iter()
                    .filter(|r| r["source"]["kind"] == "strategy")
                    .take(RECENT)
                    .collect::<Vec<_>>())
                .unwrap_or_default(),
            "fills": fills,
        })
    }

    fn apply_cycle(
        &self,
        book: StrategyBook<'_>,
        flight: &mut InFlight,
        actions: Vec<Value>,
        clock: &dyn Fn() -> i64,
    ) -> Result<()> {
        let StrategyBook { db, c, p, scan } = book;
        let strategy_id = &flight.strategy_id;
        let observed_at = flight.observed_at;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        flight.timing.book_locked_at_ms = Some(clock());
        if flight.activation_generation != self.host.activation_generation() {
            return Err("strategy_activation_stale".into());
        }
        let authoritative = choice::choose(&self.library, &self.policy)?;
        if authoritative
            .running
            .as_ref()
            .map(Running::run_id)
            .as_deref()
            != Some(strategy_id.as_str())
        {
            return Err("strategy_activation_stale".into());
        }
        flight.quotes.retain(|key, frozen| {
            let Some(current) = scan
                .samples
                .get(key)
                .and_then(|sample| sample.executable.as_ref())
            else {
                return false;
            };
            if (current.receipt, current.source) != (frozen.receipt, frozen.source) {
                return false;
            }
            // Use only the units left on this receipt after earlier actions.
            frozen.bid_units = frozen.bid_units.min(current.bid_units);
            frozen.ask_units = frozen.ask_units.min(current.ask_units);
            true
        });
        let now = clock();
        ensure_actions(&tx)?;
        tx.execute_batch(ACTION_TABLE)?;
        let mut next = p.clone();
        let results = strategy::apply(
            &mut next,
            clock,
            &mut flight.quotes,
            &flight.instruments,
            actions,
            strategy_id,
        )?;
        for ActionResult {
            index,
            key,
            side,
            units,
            outcome,
            max_price,
            refused,
            gate_at_ms,
        } in &results
        {
            if let Some((quote, decided_at_ms)) = key
                .as_ref()
                .and_then(|key| flight.quotes.get(key))
                .zip(*gate_at_ms)
            {
                flight.timing.gates.push(latency::GateTime {
                    index: *index,
                    quote: quote.into(),
                    decided_at_ms,
                });
            }
            let at = gate_at_ms.unwrap_or(now);
            tx.execute(
            "INSERT INTO strategy_action(at,observed_at,strategy_id,idx,instrument_key,side,units,outcome) VALUES(?,?,?,?,?,?,?,?)",
            rusqlite::params![at, observed_at, strategy_id, *index as i64, key, side, *units as i64, outcome],
        )?;
            if let (Some(refused), Some(key), Some(side), Some(max_price)) =
                (refused, key, side, max_price)
            {
                let action = RefusedAction {
                    side,
                    instrument_key: key,
                    units: *units,
                    max_price: max_price.amount().to_string(),
                };
                let source = RefusalSource::Strategy {
                    id: strategy_id,
                    observed_at,
                };
                record_refusal(&tx, at, source, *index, &action, refused)?;
            }
        }
        next.reconcile()?;
        tx.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'strategy_actions')",
            rusqlite::params![
                now,
                json!({"strategy_id":strategy_id,"observed_at_ms":observed_at,
                "basis":if choice::is_fallback(strategy_id) { "fallback" } else { "accepted" },
                "outcomes":results.iter().map(|r| r.outcome.as_str()).collect::<Vec<_>>()})
                .to_string()
            ],
        )?;
        tx.execute(
            "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            rusqlite::params![book_binding(c)?, serde_json::to_string(&next)?],
        )?;
        flight.timing.outcome = "actions_evaluated".into();
        flight.timing.record(&tx)?;
        tx.commit()?;
        for (key, quote) in flight.quotes.iter() {
            if let Some(current) = scan
                .samples
                .get_mut(key)
                .and_then(|sample| sample.executable.as_mut())
            {
                current.bid_units = quote.bid_units;
                current.ask_units = quote.ask_units;
            }
        }
        *p = next;
        Ok(())
    }
}

/// The latest strategy actions with their outcomes, newest first. A book
/// that never ran a strategy has none.
pub(super) fn recent_actions(db: &Connection) -> Result<Vec<Value>> {
    let present: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='strategy_action')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        return Ok(Vec::new());
    }
    let mut rows = db.prepare(
        "SELECT at,observed_at,strategy_id,idx,instrument_key,side,units,outcome FROM strategy_action ORDER BY id DESC LIMIT ?",
    )?;
    let rows = rows
        .query_map([RECENT as i64], |r| {
            Ok(json!({
                "at_ms": r.get::<_, i64>(0)?, "observed_at_ms": r.get::<_, i64>(1)?,
                "strategy_id": r.get::<_, String>(2)?, "index": r.get::<_, i64>(3)?,
                "instrument_key": r.get::<_, Option<String>>(4)?,
                "side": r.get::<_, Option<String>>(5)?, "units": r.get::<_, i64>(6)?,
                "outcome": r.get::<_, String>(7)?,
            }))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
