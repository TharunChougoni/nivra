use super::*;
use nivra_core::market::MAX_FINANCIAL_AGE_MS;
use std::collections::VecDeque;

const RECENT_ACTIONS: usize = 256;

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(super) struct Counts {
    submitted: u64,
    pub(super) evaluated: u64,
    paired: u64,
    latency_us_total: u64,
    latency_us_max: u64,
    outcomes: BTreeMap<String, u64>,
    failures: BTreeMap<String, u64>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Ledger {
    pub(super) book: Portfolio,
    pub(super) counts: Counts,
    pub(super) actions: VecDeque<(i64, Vec<Value>)>,
}

pub(super) struct Flight {
    observed_at: i64,
    seq: u64,
    comparator: bool,
    started: std::time::Instant,
    quotes: BTreeMap<String, Quote>,
    instruments: Vec<Instrument>,
}

pub(super) struct Session<'a> {
    pub(super) id: i64,
    pub(super) plan: ShadowPlan,
    pub(super) ledger: Ledger,
    pub(super) cutoff: usize,
    pub(super) shared: &'a Shared,
    pub(super) quotes: BTreeMap<String, Quote>,
    pub(super) flight: Option<Flight>,
    pub(super) worker: Option<strategy::worker::Worker>,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        self.release_worker();
    }
}

impl Session<'_> {
    pub(super) fn release_worker(&mut self) {
        self.shared.worker_ready.store(false, Relaxed);
        if self.flight.take().is_some() {
            self.failure("strategy_shadow_call_revoked");
            self.shared.observation_pending.store(false, Relaxed);
        }
        if self.worker.take().is_some() {
            self.shared
                .permit
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .reserved = false;
        }
    }

    pub(super) fn worker_permit(&mut self, source: &[u8], config: &WorkerConfig, now: i64) {
        let allowed = {
            let mut permit = self.shared.permit.lock().unwrap_or_else(|e| e.into_inner());
            if permit.allowed && self.worker.is_none() {
                permit.reserved = true;
            }
            permit.allowed
        };
        if !allowed {
            self.release_worker();
        } else if self.worker.is_none() {
            let key = WorkerKey {
                source_sha256: self.plan.source_blob.clone(),
                params_sha256: digest(self.plan.params.to_string().as_bytes()),
            };
            match strategy::worker::Worker::start(
                key,
                source.to_vec(),
                self.plan.params.clone(),
                config.clone(),
                now,
                None,
            ) {
                Ok(worker) => self.worker = Some(worker),
                Err(code) => {
                    self.shared
                        .permit
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .reserved = false;
                    self.failure(code);
                }
            }
        }
    }

    fn failure(&mut self, code: &str) {
        *self.ledger.counts.failures.entry(code.into()).or_default() += 1;
    }

    pub(super) fn protect(
        &mut self,
        session: chrono::NaiveDate,
        now: i64,
        quotes: BTreeMap<String, Quote>,
    ) {
        let mut updated = BTreeMap::new();
        for (key, q) in quotes {
            if self
                .quotes
                .get(&key)
                .is_none_or(|old| q.receipt > old.receipt && q.source >= old.source)
            {
                updated.insert(key.clone(), q.clone());
                self.quotes.insert(key, q);
            }
        }
        self.quotes
            .retain(|_, quote| quote.fresh(now, MAX_FINANCIAL_AGE_MS));
        if !self
            .ledger
            .book
            .positions
            .keys()
            .any(|key| updated.contains_key(key))
        {
            return;
        }
        let before = self.ledger.book.fills.len();
        let held_quotes = self
            .ledger
            .book
            .positions
            .keys()
            .filter_map(|key| {
                self.quotes
                    .get(key)
                    .map(|quote| (key.clone(), quote.clone()))
            })
            .collect();
        if let Err(error) = self.ledger.book.apply_at(session, now, &held_quotes, &[]) {
            self.failure(&failure_code(error.as_ref()));
            return;
        }
        for fill in &self.ledger.book.fills[before..] {
            if let Some(q) = self.quotes.get_mut(&fill.instrument.key) {
                q.bid_units = q.bid_units.saturating_sub(fill.units);
            }
        }
        self.publish_held();
    }

    pub(super) fn submit(&mut self, frozen: Frozen, comparator: Option<&str>, now: i64) {
        let result = self.start_call(frozen, comparator, now);
        if result.is_err() {
            self.shared.observation_pending.store(false, Relaxed);
            if let Err(code) = result {
                self.failure(code);
            }
        }
    }

    fn start_call(
        &mut self,
        frozen: Frozen,
        comparator: Option<&str>,
        now: i64,
    ) -> std::result::Result<(), &'static str> {
        if frozen.cutoff != self.cutoff {
            return Err("strategy_shadow_cutoff_changed");
        }
        let worker = self
            .worker
            .as_mut()
            .ok_or("strategy_shadow_worker_unavailable")?;
        if !worker.ensure_ready(now)? {
            return Err("strategy_shadow_worker_warming");
        }
        let mut quotes = BTreeMap::new();
        for (key, original) in frozen.quotes {
            if let Some(raw) = self
                .quotes
                .get(&key)
                .filter(|q| (q.receipt, q.source) == (original.receipt, original.source))
            {
                quotes.insert(key, raw.clone());
            }
        }
        for key in self.ledger.book.positions.keys() {
            if let Some(q) = self
                .quotes
                .get(key)
                .filter(|q| q.fresh(frozen.at_ms, MAX_FINANCIAL_AGE_MS))
            {
                quotes.insert(key.clone(), q.clone());
            }
        }
        let costs = self
            .ledger
            .book
            .costs
            .as_ref()
            .ok_or("paper_costs_required")?;
        let observation = Observation::new(
            frozen.at_ms,
            &self.plan.params,
            &frozen.prospects,
            &quotes,
            &self.ledger.book,
            &frozen.universe,
            costs,
        )?;
        let instruments = observation.instruments().cloned().collect();
        let bytes = serde_json::to_vec(&observation).map_err(|_| "strategy_input_limit")?;
        let started = std::time::Instant::now();
        let seq = worker.submit(&bytes, now)?;
        self.shared.worker_ready.store(false, Relaxed);
        self.ledger.counts.submitted += 1;
        self.flight = Some(Flight {
            observed_at: frozen.at_ms,
            seq,
            comparator: comparator.is_some() && comparator == self.plan.comparator_id.as_deref(),
            started,
            quotes,
            instruments,
        });
        Ok(())
    }

    pub(super) fn poll(&mut self, clock: &dyn Fn() -> i64) {
        let Some(worker) = self.worker.as_mut() else {
            return;
        };
        let Some(reply) = worker.poll(clock()) else {
            return;
        };
        if let Some(flight) = self.flight.take() {
            self.apply_reply(flight, reply, clock);
            self.shared.observation_pending.store(false, Relaxed);
        }
    }

    fn apply_reply(
        &mut self,
        mut flight: Flight,
        reply: strategy::worker::WorkerReply,
        clock: &dyn Fn() -> i64,
    ) {
        let elapsed = u64::try_from(flight.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let counts = &mut self.ledger.counts;
        counts.latency_us_total = counts.latency_us_total.saturating_add(elapsed);
        counts.latency_us_max = counts.latency_us_max.max(elapsed);
        if flight.seq != reply.seq {
            self.failure("strategy_output_invalid");
            return;
        }
        let actions = match reply
            .outcome
            .and_then(|bytes| strategy::parse_actions(&bytes, &flight.instruments))
        {
            Ok(actions) => actions,
            Err(code) => {
                self.failure(code);
                return;
            }
        };
        flight.quotes.retain(|key, frozen| {
            if let Some(latest) = self.quotes.get(key)
                && (latest.receipt, latest.source) == (frozen.receipt, frozen.source)
            {
                frozen.bid_units = frozen.bid_units.min(latest.bid_units);
                frozen.ask_units = frozen.ask_units.min(latest.ask_units);
                true
            } else {
                false
            }
        });
        let mut next = self.ledger.book.clone();
        let results = match strategy::apply(
            &mut next,
            clock,
            &mut flight.quotes,
            &flight.instruments,
            actions,
            &self.plan.strategy_id,
        ) {
            Ok(results) => results,
            Err(error) => {
                self.failure(&failure_code(error.as_ref()));
                return;
            }
        };
        self.quotes.extend(flight.quotes);
        self.ledger.book = next;
        self.ledger.counts.evaluated += 1;
        self.ledger.counts.paired += u64::from(flight.comparator);
        let mut rows = Vec::with_capacity(results.len());
        for action in results {
            *self
                .ledger
                .counts
                .outcomes
                .entry(action.outcome.clone())
                .or_default() += 1;
            rows.push(json!(action));
        }
        if !rows.is_empty() {
            if self.ledger.actions.len() == RECENT_ACTIONS {
                self.ledger.actions.pop_front();
            }
            self.ledger.actions.push_back((flight.observed_at, rows));
        }
        self.publish_held();
    }

    fn publish_held(&self) {
        let held = self
            .ledger
            .book
            .positions
            .values()
            .map(|p| p.instrument.clone())
            .collect::<Vec<_>>();
        let changed = {
            let mut view = self.shared.view.lock().unwrap_or_else(|e| e.into_inner());
            if view.held == held {
                false
            } else {
                view.held = held;
                true
            }
        };
        if changed && let Some(notify) = &self.shared.notify {
            notify.signal(Event::Strategy);
        }
    }

    fn summary(&self, status: ShadowState, comparator: Option<&Value>) -> Result<Value> {
        let counts = &self.ledger.counts;
        let skipped = self.shared.missed_scans.load(Relaxed);
        let eligible = self.shared.offered_scans.load(Relaxed);
        let coverage = counts
            .evaluated
            .saturating_mul(10_000)
            .checked_div(eligible)
            .unwrap_or(0);
        let closed = matches!(
            status,
            ShadowState::Completed | ShadowState::BetweenSessions
        );
        let paper = &self.ledger.book;
        let fees = paper
            .fills
            .iter()
            .try_fold(Amount::ZERO, |total, fill| total.checked_add(fill.fee))?;
        let comparison = comparator
            .filter(|live| live["strategy_id"].as_str() == self.plan.comparator_id.as_deref());
        Ok(json!({
            "eligible_scans":eligible, "evaluated_scans":counts.evaluated,
            "submitted_scans":counts.submitted, "skipped_scans":skipped, "coverage_bps":coverage,
            "paired_scans":counts.paired, "missed_receipts":self.shared.missed_receipts.load(Relaxed),
            "latency_us_max":counts.latency_us_max,
            "latency_us_mean":counts.latency_us_total.checked_div(counts.submitted).unwrap_or(0),
            "failures":counts.failures,
            "shadow":closed.then(|| json!({
                "fills":paper.fills.len(), "open_positions":paper.positions.len(),
                "capital":paper.allocation().ok(), "fees":fees, "outcomes":counts.outcomes,
                "marked_pnl":paper.equity().and_then(|equity| Ok(equity.checked_sub(paper.allocation()?)?)).ok(),
            })),
            "live":comparison.filter(|_| closed),
            "comparable":status == ShadowState::Completed && self.plan.sessions.len() == 1
                && paper.positions.is_empty() && coverage >= 9_500
                && comparison.is_some_and(|live| live["stable"] == true && live["capital"] == json!(paper.allocation().ok())
                    && live["scans"].as_u64() == Some(counts.paired))
                && counts.evaluated > 0 && counts.paired == counts.evaluated,
        }))
    }

    pub(super) fn publish(
        &self,
        status: ShadowState,
        reason: Option<&str>,
        comparator: Option<&Value>,
    ) -> Result<()> {
        let summary = self.summary(status, comparator)?;
        let mut view = self.shared.view.lock().unwrap_or_else(|e| e.into_inner());
        view.top_n = if status == ShadowState::Running {
            Some(self.cutoff)
        } else {
            None
        };
        if status != ShadowState::Running {
            view.held.clear();
        }
        view.status = json!({
            "version":self.plan.version, "strategy_id":self.plan.strategy_id, "comparator_id":self.plan.comparator_id,
            "sessions":self.plan.sessions, "scheduled_end_ms":self.plan.end_ms,
            "state":status, "reason":reason, "summary":summary,
        });
        Ok(())
    }

    pub(super) fn save(
        &self,
        store: &mut Store,
        session: chrono::NaiveDate,
        status: ShadowState,
        reason: Option<&str>,
        comparator: Option<&Value>,
    ) -> Result<()> {
        store.save_shadow(
            self.id,
            session,
            &serde_json::to_vec(&self.ledger)?,
            &self.summary(status, comparator)?,
            (status, reason),
        )?;
        self.publish(status, reason, comparator)
    }
}
