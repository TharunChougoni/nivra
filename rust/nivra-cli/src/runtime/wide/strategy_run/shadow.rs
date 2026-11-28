//! A shadow owns its worker, receipt depth, book and research-store connection.
//! The controller publishes inputs through a bounded channel and never awaits
//! a shadow reply or a database write.
use super::*;
use crate::runtime::controller_events::Event;
use nivra_core::{
    execution::ExecutionSettings,
    finance::FeeArithmetic,
    sleeve::{Sleeve, SleeveView},
};
use nivra_store::research_memory::{ShadowPlan, ShadowState, Store};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
    mpsc::{self, Receiver, SyncSender},
};

mod book;
use book::{Ledger, Session};

pub(super) struct LiveBaseline {
    strategy_id: String,
    initial: SleeveView,
    first_fill: usize,
    changed: bool,
}

fn cash_view(p: &Portfolio) -> Option<SleeveView> {
    p.sleeve_views()
        .ok()?
        .into_iter()
        .find(|v| v.sleeve == Sleeve::Cash)
}

impl StrategyRun {
    #[cfg(test)]
    pub(in crate::runtime::wide) fn host_shadow_fixture(&mut self, slot: shadow::ShadowSlot) {
        self.host.shadow = Some(slot);
    }

    pub(in crate::runtime::wide) fn start_shadow(&mut self, setup: shadow::Setup) -> Result<()> {
        let slot = ShadowSlot::start(setup)?;
        slot.permit_worker(true);
        self.host.shadow = Some(slot);
        Ok(())
    }

    pub(in crate::runtime::wide) fn shadow_slot(&self) -> Option<&shadow::ShadowSlot> {
        self.host
            .shadow
            .as_ref()
            .filter(|slot| slot.top_n().is_some())
    }

    pub(in crate::runtime::wide) fn shadow_held(&self) -> Vec<Instrument> {
        self.shadow_slot()
            .map(shadow::ShadowSlot::held)
            .unwrap_or_default()
    }

    pub(in crate::runtime::wide) fn finish_shadow(&mut self, db: &Connection, p: &Portfolio) {
        let Some(slot) = self
            .host
            .shadow
            .as_mut()
            .filter(|slot| slot.input.is_some())
        else {
            return;
        };
        let interrupted = p.halted || p.stopped;
        let live = self.shadow_baseline.as_ref().and_then(|baseline| {
            let cash = cash_view(p)?;
            let cash_fills = p.fills.get(baseline.first_fill..)?.iter()
                .filter(|fill| Sleeve::of_key(&fill.instrument.key) == Sleeve::Cash);
            let mut fills = cash_fills.clone().filter(|fill| {
                fill.decision_id.as_deref() == Some(baseline.strategy_id.as_str())
            });
            let count = fills.clone().count();
            let fees = fills.try_fold(Amount::ZERO, |total, fill| total.checked_add(fill.fee)).ok();
            Some(json!({
                "strategy_id":baseline.strategy_id,
                "scans":self.calls,
                "stable":self.failures == 0 && !baseline.changed && baseline.initial.open_positions == 0
                    && cash.open_positions == 0 && cash_fills.count() == count,
                "capital":baseline.initial.equity,
                "marked_pnl":cash.equity.checked_sub(baseline.initial.equity).ok(),
                "open_positions":cash.open_positions,
                "fills":count, "fees":fees,
                "outcomes":outcome_counts(db, &baseline.strategy_id),
            }))
        });
        slot.close(Close { live, interrupted });
    }

    pub(in crate::runtime::wide) fn capture_top_n(
        &mut self,
        db: &Connection,
        c: &Config,
        now: i64,
    ) -> Result<Option<Capture>> {
        let live = self.take_observation(db, c, now)?;
        let shadow = std::mem::take(&mut self.shadow_dirty)
            .then(|| self.shadow_slot().and_then(shadow::ShadowSlot::top_n))
            .flatten();
        Ok(live.or(shadow).map(|top_n| Capture {
            top_n: live.unwrap_or(top_n),
            live: live.is_some(),
            shadow: shadow.is_some(),
        }))
    }

    pub(in crate::runtime::wide) fn start_shared(
        &mut self,
        frozen: Frozen,
        p: &Portfolio,
        capture: Capture,
        clock: &dyn Fn() -> i64,
    ) {
        let shadow_input = capture.shadow.then(|| frozen.clone());
        if capture.live {
            self.start(frozen, p, clock);
        }
        if let Some(frozen) = shadow_input {
            let comparator = self
                .in_flight
                .as_ref()
                .filter(|flight| capture.live && flight.immediate_error.is_none())
                .map(|flight| flight.strategy_id.clone());
            if let Some(id) = &comparator {
                if let Some(baseline) = &mut self.shadow_baseline {
                    baseline.changed |= baseline.strategy_id != *id;
                } else if let Some(initial) = cash_view(p) {
                    self.shadow_baseline = Some(LiveBaseline {
                        strategy_id: id.clone(),
                        initial,
                        first_fill: p.fills.len(),
                        changed: false,
                    });
                }
            }
            if let Some(slot) = self.shadow_slot() {
                slot.observe(frozen, comparator);
            }
        }
    }
}

const CHECKPOINT_MS: i64 = 30_000;
const ADMISSION_LEAD_MS: i64 = 30_000;
const REPORT_VERSION: u32 = 1;
const INPUT_CAPACITY: usize = 32;

#[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq)]
struct Policy {
    settings: ExecutionSettings,
    capital: Amount,
    sizing: nivra_core::sleeve::SleevePolicies,
    costs: FeeArithmetic,
    env_sha256: Option<String>,
    top_n: usize,
}

enum Input {
    Receipts(BTreeMap<String, Quote>),
    Observation(Frozen, Option<String>),
}

struct Close {
    live: Option<Value>,
    interrupted: bool,
}

struct Shared {
    observation_pending: AtomicBool,
    offered_scans: AtomicU64,
    missed_scans: AtomicU64,
    worker_ready: AtomicBool,
    missed_receipts: AtomicU64,
    permit: Mutex<Permit>,
    view: Mutex<View>,
    close: Mutex<Option<Close>>,
    notify: Option<Notify>,
}

#[derive(Default)]
struct Permit {
    allowed: bool,
    reserved: bool,
}

#[derive(Default)]
struct View {
    top_n: Option<usize>,
    held: Vec<Instrument>,
    status: Value,
}

pub(crate) struct ShadowSlot {
    input: Option<SyncSender<Input>>,
    shared: Arc<Shared>,
}

impl ShadowSlot {
    pub(super) fn start(setup: Setup) -> Result<Self> {
        Self::start_with_clock(setup, millis)
    }

    pub(in crate::runtime::wide) fn start_with_clock(
        setup: Setup,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> Result<Self> {
        let (send, incoming) = mpsc::sync_channel(INPUT_CAPACITY);
        let shared = Arc::new(Shared {
            observation_pending: AtomicBool::new(false),
            offered_scans: AtomicU64::new(0),
            missed_scans: AtomicU64::new(0),
            worker_ready: AtomicBool::new(false),
            missed_receipts: AtomicU64::new(0),
            permit: Mutex::default(),
            view: Mutex::default(),
            close: Mutex::default(),
            notify: setup.notify.clone(),
        });
        let state = shared.clone();
        std::thread::Builder::new()
            .name("strategy-shadow".into())
            .spawn(move || {
                let result = run(&setup, &state, incoming, &clock);
                if let Err(error) = result {
                    let mut view = state.view.lock().unwrap_or_else(|e| e.into_inner());
                    view.top_n = None;
                    view.held.clear();
                    view.status =
                        json!({"state":"unavailable","reason":failure_code(error.as_ref())});
                }
                if let Some(notify) = &setup.notify {
                    notify.signal(Event::Strategy);
                }
            })?;
        Ok(Self {
            input: Some(send),
            shared,
        })
    }

    pub(crate) fn permit_worker(&self, allowed: bool) -> bool {
        let mut permit = self.shared.permit.lock().unwrap_or_else(|e| e.into_inner());
        permit.allowed = allowed;
        permit.reserved
    }

    pub(crate) fn worker_reserved(&self) -> bool {
        self.shared
            .permit
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserved
    }

    pub(crate) fn top_n(&self) -> Option<usize> {
        self.shared.view.lock().ok()?.top_n
    }

    pub(super) fn held(&self) -> Vec<Instrument> {
        self.shared
            .view
            .lock()
            .map(|view| view.held.clone())
            .unwrap_or_default()
    }

    pub(in crate::runtime::wide) fn holds_any(&self, instruments: &[Instrument]) -> bool {
        self.shared.view.lock().is_ok_and(|view| {
            view.held
                .iter()
                .any(|held| instruments.iter().any(|i| i.key == held.key))
        })
    }

    pub(super) fn status(&self) -> Value {
        self.shared
            .view
            .lock()
            .map(|view| view.status.clone())
            .unwrap_or_default()
    }

    pub(in crate::runtime::wide) fn receipt_gap(&self) {
        self.shared.missed_receipts.fetch_add(1, Relaxed);
    }

    pub(in crate::runtime::wide) fn receipts(&self, quotes: BTreeMap<String, Quote>) {
        if let Some(send) = &self.input
            && !quotes.is_empty()
            && send.try_send(Input::Receipts(quotes)).is_err()
        {
            self.receipt_gap();
        }
    }

    pub(super) fn observe(&self, frozen: Frozen, comparator: Option<String>) {
        if let Some(send) = &self.input {
            self.shared.offered_scans.fetch_add(1, Relaxed);
            if !self.shared.observation_pending.swap(true, Relaxed) {
                if send
                    .try_send(Input::Observation(frozen, comparator))
                    .is_ok()
                {
                    return;
                }
                self.shared.observation_pending.store(false, Relaxed);
            }
            self.shared.missed_scans.fetch_add(1, Relaxed);
        }
    }

    #[cfg(test)]
    pub(in crate::runtime::wide) fn ready(&self) -> bool {
        self.shared.worker_ready.load(Relaxed)
    }

    #[cfg(test)]
    pub(in crate::runtime::wide) fn busy(&self) -> bool {
        self.shared.observation_pending.load(Relaxed)
    }

    fn close(&mut self, close: Close) {
        if self.input.is_some() {
            *self.shared.close.lock().unwrap_or_else(|e| e.into_inner()) = Some(close);
            self.input.take();
        }
    }
}

#[derive(Clone)]
pub(in crate::runtime::wide) struct Setup {
    pub config: Config,
    pub choice: choice::ChoicePolicy,
    pub worker: WorkerConfig,
    pub settings: ExecutionSettings,
    pub sessions: u32,
    pub open_ms: i64,
    pub end_ms: i64,
    pub notify: Option<Notify>,
}

fn pinned(setup: &Setup, store: &Store, library: &Library) -> Result<Option<ShadowPlan>> {
    let selected = choice::choose(library, &setup.choice)?;
    let comparator_id = selected.running.as_ref().map(Running::run_id);
    for entry in selected.resolved.eligible().rev() {
        if Some(&entry.strategy_id) == comparator_id.as_ref()
            || entry.registered_at_ms >= setup.open_ms
            || store.shadow_window(Some(&entry.strategy_id))?.is_some()
        {
            continue;
        }
        let report = library.report(entry)?;
        if crate::runtime::strategy_library::build_mismatch(&report).is_some()
            || library
                .entry_review_mismatch(
                    entry,
                    &setup.worker.python_path,
                    setup.worker.env_dir.is_some(),
                )?
                .is_some()
        {
            continue;
        }
        let top_n = match &selected.running {
            Some(comparator) => comparator.top_n()?,
            None => Running::Accepted {
                entry: Box::new(entry.clone()),
                report,
            }
            .top_n()?,
        };
        let mut sessions = vec![setup.config.session];
        while sessions.len() < setup.sessions as usize {
            let next = sessions
                .last()
                .and_then(|date| date.succ_opt())
                .ok_or("session_date_range")?;
            sessions.push(setup.config.next_sessions(next)?.0);
        }
        let delta = sessions
            .last()
            .ok_or("session_date_range")?
            .signed_duration_since(setup.config.session)
            .num_milliseconds();
        let inputs = Policy {
            settings: setup.settings.clone(),
            capital: setup.config.sleeves.cash.capital,
            sizing: setup.config.sleeves.policies(),
            costs: setup
                .config
                .paper_costs
                .clone()
                .ok_or("paper_costs_required")?,
            env_sha256: setup.worker.env_sha256()?,
            top_n,
        };
        return Ok(Some(ShadowPlan {
            version: REPORT_VERSION,
            strategy_id: entry.strategy_id.clone(),
            comparator_id,
            sessions,
            end_ms: setup.end_ms.saturating_add(delta),
            source_blob: store.put_object(&library.source(entry)?)?,
            params: entry.params.clone(),
            runner_sha256: strategy::runner_sha256(),
            engine_sha256: nivra_core::replay::engine_fingerprint(),
            adapter_sha256: strategy::replay::adapter_sha256(),
            review_sha256: entry.review.policy_sha256.clone(),
            replay_sha256: entry.report_sha256.clone(),
            inputs: serde_json::to_value(inputs)?,
        }));
    }
    Ok(None)
}

fn run(
    setup: &Setup,
    shared: &Shared,
    incoming: Receiver<Input>,
    clock: &dyn Fn() -> i64,
) -> Result<()> {
    let c = &setup.config;
    let mut store = crate::runtime::autoresearch::open(c)?;
    let library = Library::at(crate::runtime::strategy_library::root(c)?);
    let prior = store.shadow_window(None)?;
    if !prior
        .as_ref()
        .is_some_and(|window| window.state == ShadowState::Running)
        && c.is_trading_day(c.session)?
    {
        let wait = setup
            .open_ms
            .saturating_sub(ADMISSION_LEAD_MS)
            .saturating_sub(clock());
        if wait > 0
            && incoming
                .recv_timeout(Duration::from_millis(wait as u64))
                .is_err_and(|error| error == mpsc::RecvTimeoutError::Disconnected)
        {
            return Ok(());
        }
    }
    let now = clock();
    let (id, plan, mut ledger) = match prior {
        Some(window) if window.state == ShadowState::Running => {
            store.interrupt_shadow(window.id, "strategy_shadow_restarted")?;
            publish_stored(&store, shared)?;
            return Ok(());
        }
        Some(window) if window.state == ShadowState::BetweenSessions => {
            let session = window
                .plan
                .sessions
                .iter()
                .position(|day| *day == c.session);
            let Some(previous) = session
                .and_then(|idx| idx.checked_sub(1))
                .map(|idx| window.plan.sessions[idx])
            else {
                if window
                    .plan
                    .sessions
                    .last()
                    .is_some_and(|last| *last < c.session)
                {
                    store.interrupt_shadow(window.id, "strategy_shadow_session_missed")?;
                }
                return publish_stored(&store, shared);
            };
            let book = store
                .shadow_ledger(window.id, previous)?
                .ok_or("strategy_shadow_session_missing")?;
            if now >= setup.open_ms {
                store.interrupt_shadow(window.id, "strategy_shadow_session_missed")?;
                return publish_stored(&store, shared);
            }
            let previous: Ledger = serde_json::from_slice(&book)?;
            store.continue_shadow(window.id, c.session)?;
            (
                window.id,
                window.plan,
                Ledger {
                    book: previous.book,
                    counts: Default::default(),
                    actions: Default::default(),
                },
            )
        }
        _ => {
            if now >= setup.open_ms || !c.is_trading_day(c.session)? {
                return publish_stored(&store, shared);
            }
            let Some(plan) = pinned(setup, &store, &library)? else {
                return publish_stored(&store, shared);
            };
            let policy: Policy = serde_json::from_value(plan.inputs.clone())?;
            let Some(id) = store.start_shadow(&plan, c.session)? else {
                return Ok(());
            };
            let mut book = Portfolio::with_capital(policy.capital);
            book.model_led = true;
            (
                id,
                plan,
                Ledger {
                    book,
                    counts: Default::default(),
                    actions: Default::default(),
                },
            )
        }
    };
    let policy: Policy = serde_json::from_value(plan.inputs.clone())?;
    if plan.version != REPORT_VERSION
        || plan.runner_sha256 != strategy::runner_sha256()
        || plan.engine_sha256 != nivra_core::replay::engine_fingerprint()
        || plan.adapter_sha256 != strategy::replay::adapter_sha256()
        || plan.review_sha256 != crate::runtime::strategy_library::review::review_policy_sha256()
        || policy.env_sha256 != setup.worker.env_sha256()?
    {
        store.interrupt_shadow(id, "strategy_shadow_build_changed")?;
        return publish_stored(&store, shared);
    }

    // On Linux niceness is per thread. The Python child and its I/O threads
    // inherit this priority, while the paper controller retains its priority.
    if nivra_sandbox::hardening::lower_worker_priority().is_err() {
        store.interrupt_shadow(id, "strategy_shadow_priority_failed")?;
        return publish_stored(&store, shared);
    }

    let cutoff = policy.top_n;
    ledger.book.settings = policy.settings;
    ledger.book.sizing = Some(policy.sizing);
    ledger.book.costs = Some(policy.costs);
    let source = store.object(&plan.source_blob)?;
    let mut runner = Session {
        id,
        plan,
        ledger,
        cutoff,
        shared,
        quotes: BTreeMap::new(),
        flight: None,
        worker: None,
    };
    runner.publish(ShadowState::Running, None, None)?;
    if let Some(notify) = &setup.notify {
        notify.signal(Event::Strategy);
    }
    let mut next_checkpoint = now.saturating_add(CHECKPOINT_MS);
    let mut revision = None;
    let reason = loop {
        let now = clock();
        let Ok(current) = library.choice_revision() else {
            break Some("strategy_library_unavailable");
        };
        if revision.as_ref() != Some(&current) {
            match library.resolve_for(&setup.choice.policy_sha256) {
                Ok(resolved) if resolved.disabled.contains(&runner.plan.strategy_id) => {
                    break Some("strategy_shadow_disabled");
                }
                Err(_) => break Some("strategy_library_unavailable"),
                Ok(_) => (),
            }
            revision = Some(current);
        }
        runner.worker_permit(&source, &setup.worker, now);
        if runner.flight.is_none() {
            let ready = runner
                .worker
                .as_mut()
                .is_some_and(|worker| worker.ensure_ready(now) == Ok(true));
            shared.worker_ready.store(ready, Relaxed);
        }
        let timeout = if runner.flight.is_some() { 5 } else { 50 };
        match incoming.recv_timeout(Duration::from_millis(timeout)) {
            Ok(Input::Receipts(quotes)) => runner.protect(c.session, clock(), quotes),
            Ok(Input::Observation(frozen, comparator)) => {
                runner.submit(frozen, comparator.as_deref(), clock());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => (),
            Err(mpsc::RecvTimeoutError::Disconnected) => break None,
        }
        if clock() < setup.end_ms {
            runner.poll(clock);
        }
        if now >= next_checkpoint {
            if runner
                .save(&mut store, c.session, ShadowState::Running, None, None)
                .is_err()
            {
                break Some("strategy_shadow_checkpoint_failed");
            }
            next_checkpoint = now.saturating_add(CHECKPOINT_MS);
        }
    };
    runner.release_worker();
    let close = shared
        .close
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let reason = reason.or_else(|| {
        if close.as_ref().is_none_or(|close| close.interrupted) {
            Some("strategy_shadow_stopped")
        } else if runner.ledger.counts.evaluated == 0 {
            Some("strategy_shadow_no_coverage")
        } else if shared.missed_receipts.load(Relaxed) != 0 {
            Some("strategy_shadow_receipt_gap")
        } else {
            None
        }
    });
    let status = if reason.is_some() {
        ShadowState::Inconclusive
    } else if runner.plan.sessions.last() == Some(&c.session) {
        ShadowState::Completed
    } else {
        ShadowState::BetweenSessions
    };
    let result = runner.save(
        &mut store,
        c.session,
        status,
        reason,
        close.as_ref().and_then(|close| close.live.as_ref()),
    );
    if result.is_err() {
        store.interrupt_shadow(id, "strategy_shadow_report_failed")?;
    }
    result
}

fn publish_stored(store: &Store, shared: &Shared) -> Result<()> {
    if let Some(window) = store.shadow_window(None)? {
        shared.view.lock().unwrap_or_else(|e| e.into_inner()).status = public_status(&window);
    }
    Ok(())
}

/// Public evidence omits the pinned source, parameters and runtime policy.
fn public_status(window: &nivra_store::research_memory::ShadowWindow) -> Value {
    json!({
        "version":window.plan.version, "strategy_id":window.plan.strategy_id, "comparator_id":window.plan.comparator_id,
        "sessions":window.plan.sessions, "scheduled_end_ms":window.plan.end_ms,
        "state":window.state, "reason":window.reason, "summary":window.summary,
    })
}

pub(in crate::runtime::wide) fn disable(root: &std::path::Path) -> Result<()> {
    let store = Store::open(root)?;
    if let Some(window) = store.shadow_window(None)? {
        store.interrupt_shadow(window.id, "strategy_shadow_disabled")?;
    }
    Ok(())
}

pub(crate) fn status(c: &Config, candidate: Option<&str>) -> Result<Value> {
    let root = crate::runtime::autoresearch::research_root(c)?;
    if !root.join("research.sqlite").is_file() {
        return Ok(json!({"state":"idle"}));
    }
    Ok(Store::open(&root)?
        .shadow_window(candidate)?
        .as_ref()
        .map_or_else(|| json!({"state":"idle"}), public_status))
}
