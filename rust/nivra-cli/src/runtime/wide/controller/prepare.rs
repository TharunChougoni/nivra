//! Pre-open universe, option and history preparation.
use super::*;
use std::sync::mpsc::{Receiver, TryRecvError, sync_channel};

pub(super) struct Preparation<T>(Receiver<std::result::Result<T, String>>);

impl<T: Send + 'static> Preparation<T> {
    fn spawn(
        c: &Config,
        notify: Notify,
        work: impl FnOnce(&data::Client, &Config) -> Result<T> + Send + 'static,
    ) -> Result<Self> {
        let c = c.clone();
        let (tx, rx) = sync_channel(1);
        thread::Builder::new()
            .name("instrument-preparation".into())
            .spawn(move || {
                let result = data::Client::new(&c.market_data_transport)
                    .and_then(|client| work(&client, &c))
                    .map_err(|error| failure_code(error.as_ref()));
                if tx.send(result).is_ok() {
                    notify.signal(ControllerEvent::Preparation);
                }
            })?;
        Ok(Self(rx))
    }

    fn take(pending: &mut Option<Self>) -> Option<std::result::Result<T, String>> {
        let result = match pending.as_ref()?.0.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => Err("instrument_preparation_exited".into()),
        };
        *pending = None;
        Some(result)
    }
}

impl Controller {
    /// A service stop waits briefly for the cancelled preparation so its attempt
    /// is recorded failed, and retryable in the window, instead of left
    /// interrupted for review. A worker that does not finish stays interrupted.
    pub(super) fn settle_cancelled_history(
        &mut self,
        mut worker: preparation::PreparationWorker,
    ) -> serde_json::Value {
        let deadline = std::time::Instant::now() + HISTORY_CANCEL_WAIT;
        loop {
            if let Some(report) = worker.poll() {
                let recorded = serde_json::to_value(report).is_ok_and(|report| {
                    self.history_attempt
                        .completed(&self.config, millis(), &report)
                        .is_ok()
                });
                return serde_json::json!({"cancelled":true,"attempt_completed":recorded});
            }
            if std::time::Instant::now() >= deadline {
                return serde_json::json!({"cancelled":true,"attempt_completed":false});
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub(super) fn prepare_universe(&mut self, t: Now) -> Result<()> {
        let Now { now, day, .. } = t;
        let c = &self.config;
        if let Some(result) = Preparation::take(&mut self.pending_universe) {
            self.retry = millis() + c.universe_prepare_retry_ms as i64;
            match result {
                Ok(prepared) => {
                    self.tradable = prepared.instruments.clone();
                    self.tradable
                        .extend(self.option_instruments.iter().cloned());
                    if let Ok(mut catalog) = self.instrument_catalog.lock() {
                        *catalog = Some(super::super::super::instrument_display::Catalog::new(
                            prepared.dashboard_projection(),
                        ));
                    }
                    super::super::super::instrument_display::start(
                        c,
                        &self.instrument_catalog,
                        &self.display_catalog_started,
                    );
                    self.universe = Some(Rc::new(prepared));
                    self.volume_history_loaded = false;
                }
                Err(reason) => eprintln!("universe_prepare_failed reason={reason}"),
            }
        }
        if self.universe.is_none()
            && self.pending_universe.is_none()
            && day <= c.session
            && c.is_trading_day(day)?
            && now >= self.retry
        {
            self.pending_universe = Some(Preparation::spawn(
                c,
                self.events.notify(),
                universe::prepare,
            )?);
        }
        Ok(())
    }

    pub(super) fn prepare_options(&mut self, t: Now) -> Result<()> {
        let Now { now, day, .. } = t;
        let c = &self.config;
        if let Some(result) = Preparation::take(&mut self.pending_options) {
            match result {
                Ok(prepared) => {
                    let catalog = prepared.catalog()?;
                    let instruments = prepared.instruments()?;
                    self.p.options = Some(catalog);
                    self.p.fee_schedule = Some(c.fee_schedule()?);
                    self.tradable.extend(instruments.iter().cloned());
                    self.option_instruments = instruments;
                    self.option_status = prepared.dashboard_projection();
                    self.option_universe = Some(prepared);
                    let session_date =
                        nivra_core::time::ExchangeDate::parse(&c.session.to_string())?;
                    if let Err(reason) = options::expiry::resume_guard(&self.p, session_date)
                        && !self.p.halted
                    {
                        self.p.halted = true;
                        self.p.health = reason.to_string();
                        persist(
                            &mut self.db,
                            c,
                            &self.p,
                            millis(),
                            None,
                            None,
                            "option_expiry_guard",
                        )?;
                    }
                }
                Err(reason) => {
                    self.option_status =
                        serde_json::json!({"status":"unavailable","reason":reason});
                }
            }
        }
        if c.nifty_options.enabled
            && self.option_universe.is_none()
            && self.pending_options.is_none()
            && self.universe.is_some()
            && day == c.session
            && now >= self.option_retry
        {
            self.option_retry = millis() + 15_000;
            if let Some(spot) = options::spot_quote(&self.index_context, c, now) {
                self.pending_options = Some(Preparation::spawn(
                    c,
                    self.events.notify(),
                    move |client, c| options::prepare(client, c, spot, now),
                )?);
            } else {
                self.option_status = serde_json::json!({
                    "status":"waiting_for_spot",
                    "reason":"option_spot_unavailable",
                });
            }
        }
        Ok(())
    }

    pub(super) fn prepare_history(&mut self, t: Now) -> Result<()> {
        let Now { now, day, m } = t;
        let c = &self.config;
        if let Some(worker) = self.history_worker.as_mut() {
            if now >= worker.deadline_ms() || day != c.session {
                worker.cancel();
            }
            if let Some(report) = worker.poll() {
                self.history_status = serde_json::to_value(report)?;
                self.history_attempt
                    .completed(c, now, &self.history_status)?;
                self.history_status["universe_session"] =
                    self.history_status["target_session"].clone();
                self.history_worker = None;
                self.volume_history_loaded = false;
            }
        }
        let window = if history_attempt_window(c, day, m) && self.history_attempt.due(c, now) {
            Some(preparation::PreparationWindow::PreOpen)
        } else if late_preparation_due(c, day, m, now, &self.history_attempt) {
            Some(preparation::PreparationWindow::LateStart)
        } else {
            None
        };
        if c.history_preparation_enabled
            && let Some(window) = window
            && self.history_worker.is_none()
            && let Some(u) = &self.universe
        {
            self.history_attempt.started(c, now)?;
            let request = preparation_request(c, &u.instruments, now, window);
            match request.and_then(preparation::PreparationWorker::spawn) {
                Ok(worker) => {
                    self.history_worker = Some(worker);
                    self.history_status =
                        serde_json::json!({"status":"PENDING","target_session":day});
                }
                Err(error) => {
                    self.history_status = serde_json::json!({"status":"BLOCKED","target_session":day,"reason":"history_preparation_admission_failed","failure":failure_code(error.as_ref())});
                    self.history_attempt
                        .completed(c, now, &self.history_status)?;
                }
            }
        }
        // Restart after the acquisition cutoff must revalidate original receipts.
        // Never restart downloads or infer readiness from a stale projection.
        if c.history_preparation_enabled
            && day == c.session
            && (c.history_preparation_cutoff_minute..c.execution.session_end_minute).contains(&m)
            && self.history_worker.is_none()
            && (self.last_history_check == 0
                || now - self.last_history_check >= c.history_preparation_check_interval_ms)
            && let Some(u) = &self.universe
        {
            self.history_status = recovered_history_status(c, &u.instruments, now);
            self.last_history_check = now;
        }
        if !self.volume_history_loaded
            && let Some(u) = &self.universe
        {
            load_daily_volumes(c, &u.instruments, now, &mut self.scan);
            self.volume_history_loaded = true;
        }
        self.history_status["last_attempt"] = self.history_attempt.last_report();
        self.history_status["attempts"] = serde_json::json!(self.history_attempt.count);
        Ok(())
    }

    pub(super) fn check_readiness(&mut self, t: Now) {
        let Now { now, day, m } = t;
        let c = &self.config;
        if now.saturating_sub(self.last_readiness) >= c.model_readiness_check_interval_ms
            || self.last_readiness == 0
        {
            self.readiness_status = readiness_snapshot(c, &self.p, now, &self.history_status);
            if day == c.session
                && (c.history_preparation_start_minute..c.execution.session_start_minute)
                    .contains(&m)
                && self.readiness_status["status"] != "ready"
            {
                self.alerts.emit(
                    Event::ReadinessBlocked,
                    c.session,
                    serde_json::json!({"ready":false}),
                );
            }
            self.last_readiness = now;
        }
    }

    pub(super) fn session_active(&self, t: Now) -> Result<bool> {
        let Now { now, day, m } = t;
        let c = &self.config;
        let active = c
            .calendar_at(day)?
            .is_open(nivra_core::time::Timestamp::from_epoch_millis(now)?)?
            && day == c.session
            && (c.execution.session_start_minute..c.execution.session_end_minute).contains(&m)
            && !self.p.closed
            && !self.p.stopped;
        Ok(active)
    }

    pub(super) fn track_feed_activity(&mut self, now: i64, active: bool) {
        let c = &self.config;
        if !active || self.p.halted {
            note_feed_health(&self.feature_store, now, false);
            self.feed_worker = None;
            self.feed_fallback_seen = false;
            self.feed_health =
                serde_json::json!({"mode":"rest","state":"inactive","fallback":false});
        }
        if active && !self.session_started {
            self.alerts.emit(
                Event::SessionStart,
                c.session,
                serde_json::json!({"resumed":!self.p.fills.is_empty()}),
            );
            self.session_started = true;
        }
    }

    pub(super) fn history_admitted(&self) -> bool {
        history_admitted(&self.config, &self.history_status)
    }
}
