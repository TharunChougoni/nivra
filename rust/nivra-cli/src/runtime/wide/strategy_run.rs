//! The strategy executor (#42). On each scan the paper controller freezes one
//! observation and calls the library's active strategy on a worker thread,
//! outside the book transaction. Only applying the returned actions takes the
//! transaction, and it validates them again against the book at that moment:
//! the planner checks, then the shared execution gate with the original quote
//! receipts. A failed call produces no actions and never halts the book.
//! Paper only.
use super::*;
use crate::runtime::strategy::{self, ActionResult, Observation, Prospect};
use crate::runtime::strategy_library::{Entry, Library};
use nivra_core::market::MAX_FINANCIAL_AGE_MS;
use nivra_sandbox::SandboxConfig;
use serde_json::{Value, json};
use std::thread::JoinHandle;

/// How often the library is read again, so a pin, disable or new entry takes
/// effect without a restart.
const RESOLVE_MS: i64 = 30_000;
/// Actions, refusals and fills the dashboard lists.
const RECENT: usize = 20;
const ACTION_TABLE: &str = "CREATE TABLE IF NOT EXISTS strategy_action(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,observed_at INTEGER NOT NULL,strategy_id TEXT NOT NULL,idx INTEGER NOT NULL,instrument_key TEXT,side TEXT,units INTEGER NOT NULL,outcome TEXT NOT NULL);";

/// The build-specific reason an accepted entry cannot run here: its report
/// was produced by a different engine or a different action adapter.
pub(crate) fn build_mismatch(report: &Value) -> Option<&'static str> {
    if report["engine_sha256"].as_str() != Some(&nivra_core::replay::engine_fingerprint()) {
        Some("strategy_engine_changed")
    } else if report["adapter_sha256"].as_str() != Some(&strategy::replay::adapter_sha256()) {
        Some("strategy_adapter_changed")
    } else {
        None
    }
}

/// Active strategy supervision state for the agent lanes and execution gate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SupervisionState {
    pub active_id: Option<String>,
    pub any_eligible: bool,
}

impl SupervisionState {
    pub(crate) fn strategy_active(&self) -> bool {
        self.active_id.is_some()
    }
}

/// Inspect the library when `strategy_execution` is enabled: returns the
/// runnable active strategy id (matching this build's engine and adapter) and
/// whether any eligible strategies exist for supervision.
pub(crate) fn supervision_state(c: &Config, cfg: &planner::PlannerConfig) -> SupervisionState {
    if !cfg.runs_strategies() {
        return SupervisionState::default();
    }
    let Ok(root) = crate::runtime::strategy_library::root(c) else {
        return SupervisionState::default();
    };
    let library = Library::at(root);
    let Ok(resolved) = library.resolve() else {
        return SupervisionState::default();
    };
    let any_eligible = resolved.eligible().next().is_some();
    let active_id = resolved.active.as_deref().and_then(|id| {
        let entry = resolved.entries.iter().find(|e| e.strategy_id == id)?;
        let report = library.report(entry).ok()?;
        build_mismatch(&report).is_none().then(|| id.to_owned())
    });
    SupervisionState {
        active_id,
        any_eligible,
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
    entry: Entry,
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

type Called = (
    std::result::Result<Vec<Value>, &'static str>,
    Vec<Instrument>,
);

struct InFlight {
    strategy_id: String,
    observed_at: i64,
    quotes: BTreeMap<String, Quote>,
    handle: JoinHandle<Called>,
}

pub(super) struct StrategyRun {
    library: Library,
    sandbox: SandboxConfig,
    active: Option<Active>,
    resolved_at: Option<i64>,
    /// Why no call starts: no active entry, a build mismatch or a library
    /// read failure.
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
    pub(super) fn new(library: Library, sandbox: SandboxConfig) -> Self {
        Self {
            library,
            sandbox,
            active: None,
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

    fn resolve(&self) -> Result<std::result::Result<Active, &'static str>> {
        let resolved = self.library.resolve()?;
        let Some(id) = resolved.active.as_deref() else {
            return Ok(Err("no_active_strategy"));
        };
        let entry = resolved
            .entries
            .iter()
            .find(|e| e.strategy_id == id)
            .ok_or("strategy_not_found")?
            .clone();
        let report = self.library.report(&entry)?;
        if let Some(reason) = build_mismatch(&report) {
            return Ok(Err(reason));
        }
        let top_n = report["parameters"]["top_n"]
            .as_u64()
            .map_or(Ok(20), usize::try_from)?;
        Ok(Ok(Active {
            source: self.library.source(&entry)?,
            entry,
            top_n,
        }))
    }

    /// The prospect count for a call to start now, or `None` when none may:
    /// no runnable entry, or the previous call is still running. A running
    /// call skips this scan; nothing queues.
    pub(super) fn ready(&mut self, db: &Connection, c: &Config, now: i64) -> Result<Option<usize>> {
        if self.resolved_at.is_none_or(|at| now - at >= RESOLVE_MS) {
            self.resolved_at = Some(now);
            self.active = None;
            self.blocked = None;
            match self.resolve() {
                Ok(Ok(active)) => self.active = Some(active),
                Ok(Err("no_active_strategy")) => self.blocked = Some("no_active_strategy"),
                Ok(Err(reason)) => {
                    self.blocked = Some(reason);
                    self.fail(db, c, now, None, reason)?;
                }
                Err(error) => {
                    self.blocked = Some("strategy_library_unavailable");
                    let reason = failure_code(error.as_ref());
                    self.fail(db, c, now, None, &reason)?;
                }
            }
        }
        let Some(active) = &self.active else {
            return Ok(None);
        };
        if self.in_flight.is_some() {
            self.skipped += 1;
            return Ok(None);
        }
        Ok(Some(active.top_n))
    }

    /// Start one sandboxed call on `frozen`. Call [`Self::ready`] first.
    pub(super) fn start(&mut self, frozen: Frozen) {
        let Some(active) = &self.active else { return };
        if self.in_flight.is_some() {
            return;
        }
        let sandbox = self.sandbox.clone();
        let source = active.source.clone();
        let params = active.entry.params.clone();
        let quotes = frozen.quotes.clone();
        let handle = std::thread::spawn(move || {
            let costs = frozen.portfolio.costs.clone();
            let Some(costs) = costs else {
                return (Err("paper_costs_required"), Vec::new());
            };
            let observation = Observation::new(
                frozen.at_ms,
                &params,
                &frozen.prospects,
                &frozen.quotes,
                &frozen.portfolio,
                &frozen.universe,
                &costs,
            );
            let called = strategy::call(&sandbox, &source, &observation);
            let instruments = observation.instruments().cloned().collect();
            (called, instruments)
        });
        self.calls += 1;
        self.last_call_at_ms = Some(frozen.at_ms);
        self.in_flight = Some(InFlight {
            strategy_id: active.entry.strategy_id.clone(),
            observed_at: frozen.at_ms,
            quotes,
            handle,
        });
    }

    #[cfg(test)]
    pub(super) fn wait(&self) {
        while self
            .in_flight
            .as_ref()
            .is_some_and(|f| !f.handle.is_finished())
        {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// Apply a finished call. `clock` is sampled after the book transaction
    /// begins, so the gate's freshness check uses the time of the fill.
    pub(super) fn poll(
        &mut self,
        db: &mut Connection,
        c: &Config,
        p: &mut Portfolio,
        clock: &dyn Fn() -> i64,
    ) -> Result<()> {
        if !self
            .in_flight
            .as_ref()
            .is_some_and(|f| f.handle.is_finished())
        {
            return Ok(());
        }
        let Some(flight) = self.in_flight.take() else {
            return Ok(());
        };
        let (strategy_id, observed_at) = (flight.strategy_id.clone(), flight.observed_at);
        let (called, instruments) = flight
            .handle
            .join()
            .unwrap_or((Err("strategy_call_panicked"), Vec::new()));
        let outcome = match called {
            Err(reason) => Err(reason.to_owned()),
            Ok(actions) => apply_cycle(
                db,
                c,
                p,
                &strategy_id,
                observed_at,
                flight.quotes,
                &instruments,
                actions,
                clock,
            )
            .map_err(|error| failure_code(error.as_ref())),
        };
        match outcome {
            Ok(()) => self.last_failure = None,
            Err(reason) => self.fail(db, c, clock(), Some(&strategy_id), &reason)?,
        }
        Ok(())
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
        diagnostics::note(
            &c.directory,
            now,
            "failure",
            json!({"stage":"strategy","strategy_id":strategy_id,"reason":reason}),
        );
        Ok(())
    }

    /// The dashboard's view: the active entry, the counters, and the latest
    /// actions, gate refusals and fills.
    pub(super) fn status(&self, db: &Connection, p: &Portfolio) -> Value {
        let active = self.active.as_ref().map(|a| &a.entry);
        let state = if self.in_flight.is_some() {
            "calling"
        } else if active.is_some() {
            "ready"
        } else {
            "blocked"
        };
        let fills: Vec<_> = active
            .map(|e| {
                p.fills
                    .iter()
                    .rev()
                    .filter(|f| f.decision_id.as_deref() == Some(e.strategy_id.as_str()))
                    .take(RECENT)
                    .collect()
            })
            .unwrap_or_default();
        json!({
            "enabled": true,
            "state": state,
            "reason": self.blocked,
            "strategy": active.map(|e| json!({
                "strategy_id": e.strategy_id, "author": e.author, "summary": e.summary,
                "tape_dates": e.tape_dates, "registered_at_ms": e.registered_at_ms,
            })),
            "calls": self.calls, "failures": self.failures, "skipped": self.skipped,
            "last_call_at_ms": self.last_call_at_ms, "last_failure": self.last_failure,
            "actions": recent_actions(db).unwrap_or_default(),
            "refusals": super::recent_refusals(db, 50)
                .map(|rows| rows.into_iter()
                    .filter(|r| r["source"]["kind"] == "strategy")
                    .take(RECENT)
                    .collect::<Vec<_>>())
                .unwrap_or_default(),
            "fills": fills,
        })
    }
}

#[allow(clippy::too_many_arguments)] // One call's frozen inputs and the post-lock clock.
fn apply_cycle(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    strategy_id: &str,
    observed_at: i64,
    mut quotes: BTreeMap<String, Quote>,
    instruments: &[Instrument],
    actions: Vec<Value>,
    clock: &dyn Fn() -> i64,
) -> Result<()> {
    if actions.is_empty() {
        return Ok(());
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let now = clock();
    ensure_actions(&tx)?;
    tx.execute_batch(ACTION_TABLE)?;
    let mut next = p.clone();
    let results = strategy::apply(
        &mut next,
        now,
        &mut quotes,
        instruments,
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
    } in &results
    {
        tx.execute(
            "INSERT INTO strategy_action(at,observed_at,strategy_id,idx,instrument_key,side,units,outcome) VALUES(?,?,?,?,?,?,?,?)",
            rusqlite::params![now, observed_at, strategy_id, *index as i64, key, side, *units as i64, outcome],
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
            record_refusal(&tx, now, source, *index, &action, refused)?;
        }
    }
    next.reconcile()?;
    tx.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'strategy_actions')",
        rusqlite::params![
            now,
            json!({"strategy_id":strategy_id,"observed_at_ms":observed_at,
                "outcomes":results.iter().map(|r| r.outcome.as_str()).collect::<Vec<_>>()})
            .to_string()
        ],
    )?;
    tx.execute(
        "INSERT INTO state VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
        rusqlite::params![book_binding(c)?, serde_json::to_string(&next)?],
    )?;
    tx.commit()?;
    *p = next;
    Ok(())
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
