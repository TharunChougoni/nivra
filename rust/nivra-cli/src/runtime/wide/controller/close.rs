//! Session close, sealed report, rollover and successor adoption.
use super::*;

impl Controller<'_> {
    /// Session close: settle the owner attempt, close the book and report once.
    /// Returns the closed book's health as the pass status.
    pub(super) fn close_session(&mut self, t: Now) -> Result<Option<String>> {
        let Now { now, day, m } = t;
        let c = &self.config;
        if !(day > c.session || (day == c.session && m >= c.execution.session_end_minute)) {
            return Ok(None);
        }
        if let Some(agent) = &mut self.agent
            && let Err(error) = agent.cancel_decision(&mut self.db, c)
        {
            planner_incident_cause(
                &mut self.db,
                c,
                &mut self.p,
                now,
                Some(&failure_code(error.as_ref())),
            )?;
        }
        if close_portfolio(&mut self.p) {
            persist(&mut self.db, c, &self.p, now, None, None, "post_close")?;
            self.retention_status = match super::super::super::retention::run(c, day, true) {
                Ok(summary) => {
                    let summary = serde_json::to_value(summary).unwrap_or(serde_json::Value::Null);
                    if c.dashboard.retention.pack_after_days.is_some()
                        || c.dashboard.retention.delete_after_days.is_some()
                    {
                        eprintln!("dashboard_retention {summary}");
                    }
                    summary
                }
                Err(_) => {
                    eprintln!("dashboard_retention_failed");
                    serde_json::json!({"status":"unavailable","reason":"retention_failed"})
                }
            };
        }
        if !self.reported {
            self.write_report(now)?;
        }
        Ok(Some(self.p.health.clone()))
    }

    pub(super) fn write_report(&mut self, now: i64) -> Result<()> {
        let c = &self.config;
        let tmp = c.directory.join("wide-report.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(
                &serde_json::json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":c.account,"session":c.session,"as_of_ms":now,"realised_net_pnl_by_fill":self.p.realised_net_pnl(),"portfolio":self.p,"universe":self.universe.as_deref(),"nifty_options":self.option_status,"scan_count":self.scan.samples.len(),"carried_positions":if self.p.model_led {self.p.positions.len()}else{0}}),
            )?,
        )?;
        fs::File::open(&tmp)?.sync_all()?;
        fs::rename(tmp, c.directory.join("report.json"))?;
        rollover::record_review(c)?;
        self.reported = true;
        let fees = self
            .p
            .fills
            .iter()
            .try_fold(Amount::ZERO, |total, f| total.checked_add(f.fee))
            .ok();
        let net = if self.p.positions.is_empty() {
            self.p.cash.checked_sub(self.p.allocation()?).ok()
        } else {
            None
        };
        let hash = fs::read(c.directory.join("report.json"))
            .ok()
            .map(|b| digest(&b));
        self.alerts.emit(Event::SessionEnd, c.session, serde_json::json!({"net_pnl":net,"fills":self.p.fills.len(),"fees":fees,"open_unresolved":if self.p.model_led {0}else{self.p.positions.len()},"carried_positions":if self.p.model_led {self.p.positions.len()}else{0},"report_hash":hash}));
        Ok(())
    }

    pub(super) fn owner_research(&mut self, now: i64, research_context_unavailable: bool) {
        let c = &self.config;
        if let Some(cfg) = self.event_config.as_ref() {
            let owner_blocked = self
                .agent
                .as_ref()
                .is_some_and(|agent| agent.pending_claim().is_some());
            let launch = harness::ResearchLaunch::for_book(harness::LaunchBook {
                halted: self.p.halted,
                stopped: self.p.stopped,
                decision_pending: owner_blocked,
                context_unavailable: research_context_unavailable,
            });
            let mut status = match harness::research_tick_tasks(
                &mut self.db,
                c,
                cfg,
                launch,
                &mut self.research_worker,
                now,
            ) {
                Ok(status) => status,
                Err(_) => serde_json::json!({"status":"unavailable"}),
            };
            status["observation"] = if c.market_archive.is_some() && cfg.research_tools.is_some() {
                self.research_worker.archive_observation().cloned().unwrap_or_else(|| {
                    serde_json::json!({"status":"unavailable","reason":"archive_research_view_pending"})
                })
            } else {
                harness::research_observation_status(c, cfg, now)
            };
            if research_context_unavailable {
                status["context"] = research_context_status(true);
            }
            self.research_tasks_status = status;
        }
    }

    pub(super) fn rollover(&mut self, t: Now) -> Result<Advance> {
        if self.research_worker.archive_maturing() {
            return Ok(Advance::Idle("research_maturation_in_progress"));
        }
        let Now { now, day, .. } = t;
        let c = &self.config;
        match rollover::advance_pi(c, now) {
            Ok(Some(next)) => {
                self.adopt(next, now)?;
                Ok(Advance::Adopted)
            }
            Ok(None) => Ok(Advance::Idle(
                rollover::idle_status(c, day, now)
                    .unwrap_or("rollover_blocked_inspect_review_human_required"),
            )),
            Err(_) => Ok(Advance::Idle(
                "rollover_failed_original_book_preserved_human_required",
            )),
        }
    }

    /// Move the controller onto the next dated session's book and reset the
    /// per-session loop state.
    pub(super) fn adopt(&mut self, next: Config, now: i64) -> Result<()> {
        self.stop_archive()?;
        fs2::FileExt::unlock(&*self.lock)?;
        self.lock = AdvisoryLock::new(
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(next.directory.join("controller.lock"))?,
        );
        self.lock.try_lock_exclusive()?;
        self.db = open_book(&next)?;
        let action_fault = ensure_actions(&self.db).is_err();
        self.p = load_wide(&mut self.db, &next)?;
        if action_fault {
            planner_incident(&mut self.db, &next, &mut self.p, millis())?;
        }
        persist(
            &mut self.db,
            &next,
            &self.p,
            now,
            None,
            None,
            "daily_session_created",
        )?;
        self.last_fresh_receipt = None;
        self.session_started = false;
        self.halt_seen = false;
        self.stop_seen = false;
        self.stale_since = None;
        self.stale_seen = false;
        self.rollover_seen = false;
        self.archive = Some(start_archive(&next)?);
        self.research_worker.reset_archive_evidence();
        self.config = next;
        self.history_attempt = HistoryAttempts::load(&self.config)?;
        self.last_history_check = 0;
        self.history_status =
            serde_json::json!({"status":"UNKNOWN","reason":"preparation_not_started"});
        self.universe = None;
        self.tradable.clear();
        if let Ok(mut catalog) = self.instrument_catalog.lock() {
            *catalog = None;
        }
        self.display_catalog_started
            .store(false, std::sync::atomic::Ordering::Release);
        self.display_catalog_status = serde_json::json!({"status":"not_prepared"});
        self.volume_history_loaded = false;
        self.feed_worker = None;
        if let Ok(mut series) = self.equity_series.lock() {
            series.reset();
        }
        self.shadow_selection = ShadowSelection::default();
        self.shadow_scorer = ShadowScorer::default();
        self.prefilter_previous.clear();
        self.prefilter_status = None;
        self.last_rank = 0;
        self.last_depth = 0;
        self.retry = 0;
        self.last_scan = 0;
        self.reported = false;
        self.read_failure = None;
        self.planner_status = serde_json::json!({"status":"disabled_no_plan"});
        self.research_tasks_status = if self.config.planner_config.is_some() {
            serde_json::json!({
                "status":"unavailable",
                "reason":"planner_config_unavailable",
                "action":"none"
            })
        } else {
            serde_json::json!({"status":"disabled"})
        };
        if let Some(agent) = self.agent.take() {
            agent.stop();
        }
        self.live_ranking.clear();
        let policy = load_pi_policy(&self.config);
        let (event_config, agent, event_config_reason) =
            attach_policy(&mut self.db, &self.config, &mut self.p, policy)?;
        self.event_config = event_config;
        self.agent = agent;
        // A call still running for the previous book is dropped with it.
        self.strategy = strategy_runner(&self.config, self.event_config.as_ref())?;
        self.event_config_reason = event_config_reason;
        let SessionEvidence {
            scan,
            feature_store,
            index_features,
            option_status,
        } = SessionEvidence::load(&self.db, &self.config, self.event_config.as_ref())?;
        self.scan = scan;
        *self
            .feature_store
            .lock()
            .map_err(|_| Fail::new("feature_store_lock"))? = feature_store;
        *self
            .index_features
            .lock()
            .map_err(|_| Fail::new("feature_store_lock"))? = index_features;
        self.option_status = option_status;
        self.option_instruments.clear();
        self.option_universe = None;
        self.option_retry = 0;
        self.last_option_depth = 0;
        self.index_context = super::super::IndexContext::default();
        self.note_session("session_adopted");
        Ok(())
    }
}
