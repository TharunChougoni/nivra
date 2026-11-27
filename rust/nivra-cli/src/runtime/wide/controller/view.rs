//! Alerts, planner status and the published dashboard view.
use super::*;

impl Controller {
    pub(super) fn emit_transitions(&mut self, t: Now, active: bool) -> Result<()> {
        let Now { now, day, m } = t;
        let c = &self.config;
        if self.p.halted && !self.halt_seen {
            self.alerts.emit(
                Event::Halt,
                c.session,
                serde_json::json!({"closed":self.p.closed,"open_unresolved":self.p.positions.len()}),
            );
        }
        self.halt_seen = self.p.halted;
        if self.p.stopped && !self.stop_seen {
            self.alerts.emit(
                Event::Stop,
                c.session,
                serde_json::json!({"closed":self.p.closed,"open_unresolved":self.p.positions.len(),"operator_stopped":true,"halted":self.p.halted}),
            );
        }
        self.stop_seen = self.p.stopped;
        if day > c.session && c.is_trading_day(day)? && !self.rollover_seen {
            let missed = rollover::preview(c, now)
                .ok()
                .is_some_and(|v| v["status"] == "missed_preopen_idle");
            self.alerts.emit(
                if missed {
                    Event::MissedPreopen
                } else {
                    Event::RolloverBlocked
                },
                day,
                serde_json::json!({"previous_session":c.session}),
            );
            self.rollover_seen = true;
        }
        if active {
            let receipt = self.last_fresh_receipt;
            // Start counting at today's configured open (or the first active pass
            // after a late start or restart), never count overnight.
            let open = now - i64::from(m.saturating_sub(c.execution.session_start_minute)) * 60_000;
            let since = self.stale_since.get_or_insert(now);
            if let Some(receipt) = receipt
                .filter(|r| *r >= open && *r + nivra_core::market::MAX_FINANCIAL_AGE_MS > *since)
            {
                *since = receipt + nivra_core::market::MAX_FINANCIAL_AGE_MS;
                self.stale_seen = false;
            } else if now - *since > i64::from(c.alert_stale_minutes) * 60_000 && !self.stale_seen {
                self.alerts.emit(
                    Event::StaleFeed,
                    c.session,
                    serde_json::json!({"stale_minutes":c.alert_stale_minutes}),
                );
                self.stale_seen = true;
            }
        } else {
            self.stale_since = None;
            self.stale_seen = false;
        }
        Ok(())
    }

    pub(super) fn refresh_planner_status(&mut self) -> Result<()> {
        let c = &self.config;
        let execution = match execution_projection(&self.db) {
            Ok(rows) => rows,
            Err(_) => {
                planner_incident(&mut self.db, c, &mut self.p, millis())?;
                Vec::new()
            }
        };
        if let Some(events) = match redecision::status(&self.db, c) {
            Ok(status) => status,
            Err(_) => {
                self.event_config = None;
                self.event_config_reason = Some("planner_authority_invalid");
                planner_incident(&mut self.db, c, &mut self.p, millis())?;
                Some(serde_json::json!({"status":"planner_authority_invalid_recovery_required"}))
            }
        } {
            self.planner_status = events;
        }
        self.planner_status["execution"] = serde_json::json!(execution);
        self.planner_status["nifty_options"] = self.option_status.clone();
        self.planner_status["authority"] = serde_json::json!("pi");
        self.planner_status["calls_used"] =
            serde_json::to_value(super::super::super::agent::read_spend(c).ok())?;
        Ok(())
    }

    pub(super) fn entry_status(&self) -> &'static str {
        let c = &self.config;
        if self.p.halted {
            "risk_halted_protective_exits_only"
        } else if self.p.stopped {
            "operator_stopped"
        } else if self.planner_status["pending_attempt"].is_string() {
            "planning_no_new_entries"
        } else if redecision::active_plan(&self.db, c)
            .ok()
            .flatten()
            .is_some()
        {
            "model_plan_ready"
        } else if self.planner_status["status"].as_str() == Some(planner::NO_DECISION_WORK) {
            planner::NO_DECISION_WORK
        } else {
            "model_plan_unavailable_no_trades"
        }
    }

    pub(super) fn discovery_status(&self) -> Option<serde_json::Value> {
        let cached_live = self.shadow_selection.cached_live().cloned();
        let mut discovery_status = self
            .event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|settings| {
                self.universe.as_ref().map(|u| {
                    super::super::super::discovery::project_with_live(
                        &self.scan,
                        &u.instruments,
                        settings,
                        millis(),
                        cached_live.as_ref(),
                    )
                })
            });
        if let Some(serde_json::Value::Object(health)) = discovery_status.as_mut()
            && let Some(telemetry) = self.scan_telemetry.as_object()
        {
            health.extend(telemetry.clone());
        }
        if let Some(status) = discovery_status.as_mut()
            && let Ok(store) = self.feature_store.lock()
        {
            status["feature_store"] = store.status();
        }
        if let (Some(status), Some(prefilter)) = (&mut discovery_status, &self.prefilter_status) {
            status["prefilter"] = prefilter.clone();
        }
        self.shadow_selection.attach(&mut discovery_status);
        self.shadow_scorer.attach(&mut discovery_status);
        discovery_status
    }

    /// Publish the dashboard view; returns the sleep before the next pass.
    pub(super) fn publish(
        &mut self,
        t: Now,
        mut status: String,
        active: bool,
        history_admitted: bool,
        research_context_unavailable: bool,
    ) -> Result<u64> {
        let Now { now, day, .. } = t;
        self.refresh_planner_status()?;
        if self.p.halted {
            status = "risk_halted_protective_exits_only".into();
        }
        let entry_status = self.entry_status();
        if active && !self.p.halted && !self.p.stopped {
            status = entry_status.to_owned();
        }
        let discovery_status = self.discovery_status();
        let c = &self.config;
        let oldest = discovery_status
            .as_ref()
            .and_then(|d| d["oldest_fresh_age_ms"].as_i64());
        let reasons = discovery_status
            .as_ref()
            .map(|d| d["screen_reasons"].clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let scanned_count = discovery_status
            .as_ref()
            .and_then(|d| d["fresh_count"].as_u64())
            .unwrap_or(0);
        if let Ok(equity) = self.p.equity()
            && let Ok(mut series) = self.equity_series.lock()
        {
            series.observe(now, &self.p.cash.to_string(), &equity.to_string());
        }
        let initialization = serde_json::json!({"stage":if self.p.halted {"halted_requires_review"} else if !history_admitted {"waiting_for_history"} else if self.planner_status["pending_attempt"].is_string() {"pi_decision_in_progress"} else if entry_status=="model_plan_ready" {"model_initialized"} else {"waiting_for_initial_or_next_model_plan"},"paper_only":true,"financial_execution_requires_accepted_plan":true});
        let updater_status = crate::update::dashboard_status();
        let owner_activity = if self.event_config.is_some() {
            super::super::super::decision::activity_analytics(c, &self.p).ok()
        } else {
            None
        };
        if let Some(status) =
            super::super::super::instrument_display::status(&self.instrument_catalog)
        {
            self.display_catalog_status = status;
        }
        let research_library = self.research_library();
        // One completion sample owns as_of_ms, the heartbeat and research
        // projections. Request time in the dashboard API never renews it.
        let completed_at_ms = millis();
        self.loop_generation = self
            .loop_generation
            .checked_add(1)
            .ok_or_else(|| Fail::new("controller_loop_generation_exhausted"))?;
        let active_tick_ms = self
            .event_config
            .as_ref()
            .and_then(|c| c.discovery.as_ref())
            .and_then(|d| d.market_feed.as_ref())
            .map(|s| s.tick_ms.min(2000) as u64)
            .unwrap_or(2000);
        let interval_ms = if active {
            active_tick_ms
        } else {
            c.idle_poll_interval_ms
        };
        let research_context_status = research_context_status(research_context_unavailable);
        let mut v_json = serde_json::json!({"research_library":research_library,"instrument_display":self.display_catalog_status,"paper_only":true,"live_enabled":LIVE_ENABLED,"account":c.account,"authorized_session":c.session,"next_session_config":c.next_sessions(day)?,"policy":self.p.policy,"model_led":self.p.model_led,"strategy_decision":rollover::published_decision(c),"status":status,"as_of_ms":completed_at_ms,"universe_size":self.universe.as_ref().map(|u|u.instruments.len()),"universe_exclusions":self.universe.as_ref().map(|u|&u.exclusions),"security_ready":self.universe.is_some(),"scanned_count":scanned_count,"pending_count":self.universe.as_ref().map(|u|u.instruments.len().saturating_sub(scanned_count as usize)),"market_feed":self.feed_health,"oldest_scan_age_ms":oldest,"screen_reasons":reasons,"discovery":discovery_status,"ranking":self.scan.ranking.iter().take(10).collect::<Vec<_>>(),"capital":self.p.allocation()?,"planner":self.planner_status,"nifty_options":self.option_status,"research_tasks":self.research_tasks_status,"activity_attribution":owner_activity,"retention":self.retention_status,"readiness":self.readiness_status,"history_preparation":self.history_status,"model_initialization":initialization,"model_status":"model-led","entry_status":entry_status,"realised_net_pnl_by_fill":self.p.realised_net_pnl(),"portfolio":self.p,"read_failure_reason":self.read_failure,"fee_policy":format!("SIMULATION_ONLY: {}", serde_json::to_string(&self.p.costs.clone().ok_or("paper_costs_required")?)?),"updater":updater_status});
        if let Some(agent) = &self.agent {
            v_json["agent"] = agent.status();
        }
        v_json["strategy_execution"] = match &self.strategy {
            Some(run) => run.status(&self.db, &self.p),
            None => serde_json::json!({"enabled":false}),
        };
        v_json["research_context"] = research_context_status;
        v_json["controller_heartbeat"] = super::super::super::operational_health::heartbeat(
            completed_at_ms,
            interval_ms,
            self.loop_generation,
        );
        v_json["operational_health"] = self.operational_health(day, completed_at_ms)?;
        if c.index_feeds.enabled {
            // Additive, non-tradeable index context; cash fields stay unchanged.
            v_json["index_feeds"] = self.index_context.json();
        }
        *self.view.lock().map_err(|_| Fail::new("dashboard_lock"))? = v_json.clone();
        super::super::super::web::publish_summary(&v_json, Some(&c.directory));
        Ok(interval_ms)
    }

    fn research_library(&self) -> Option<serde_json::Value> {
        self.event_config.as_ref().map(|cfg|
            super::super::super::experiment_ledger::research::library(&self.config, cfg)
                .unwrap_or_else(|_| serde_json::json!({"status":"unavailable","reason":"original_research_library_unavailable","items":[],"activation_authorized":false}))
        )
    }

    pub(super) fn operational_health(
        &self,
        day: NaiveDate,
        completed_at_ms: i64,
    ) -> Result<serde_json::Value> {
        let c = &self.config;
        let operational_health = super::super::super::operational_health::capture(
            completed_at_ms,
            c.calendar_coverage_at(day)?,
            crate::alert::transport_status(&c.account, &c.session.to_string(), completed_at_ms),
        );
        Ok(serde_json::to_value(operational_health)?)
    }
}
