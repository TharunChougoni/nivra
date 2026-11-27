//! In-session market reads, scanning, ranking and depth reads.
use super::*;

impl Controller<'_> {
    /// Market reads, scanning, ranking and the owner tick inside the session.
    pub(super) fn trade_session(&mut self, t: Now, history_admitted: bool) -> Result<&'static str> {
        let Now { now, m, .. } = t;
        let Some(u) = self.universe.clone() else {
            return Ok("waiting_for_d1_universe");
        };
        // No plan yet with nothing to decide is a no-op, not a failure
        // (#221). The planner status names the reason.
        let mut status =
            if self.planner_status["status"].as_str() == Some(planner::NO_DECISION_WORK) {
                planner::NO_DECISION_WORK
            } else {
                "model_plan_unavailable_no_trades"
            };
        let feed_settings = self
            .event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|d| d.market_feed.clone())
            .filter(|s| s.mode == super::super::super::market_feed::Mode::Ltpc);
        self.scan.screens = Some(
            self.event_config
                .as_ref()
                .and_then(|cfg| cfg.discovery.as_ref())
                .and_then(|d| d.screens.clone())
                .unwrap_or_else(nivra_core::execution::ScreenPolicy::recommended),
        );
        let (feed_fallback, _) = self.poll_feed(&u, now, feed_settings.as_ref())?;
        // Held instruments always read and committed before any scanner request.
        let held: Vec<_> = self
            .p
            .positions
            .values()
            .map(|p| p.instrument.clone())
            .collect();
        self.protection_read(&held, now)?;
        self.plan_read(history_admitted, now)?;
        let discovery = self
            .event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref());
        let scan_interval = feed_settings
            .as_ref()
            .map(|s| s.rest_interval_ms(feed_fallback))
            .unwrap_or_else(|| discovery.and_then(|d| d.scan_interval_ms).unwrap_or(8000));
        let scan_batch = feed_settings
            .as_ref()
            .map(|s| s.rest_batch_size)
            .unwrap_or_else(|| {
                discovery
                    .and_then(|d| d.scan_batch_size)
                    .unwrap_or(super::super::super::discovery::DEFAULT_SCAN_BATCH_SIZE)
            });
        let ranking_window = discovery
            .and_then(|d| d.ranking_window_ms)
            .unwrap_or(super::super::super::discovery::DEFAULT_RANKING_WINDOW_MS);
        let freshness = discovery
            .and_then(|d| d.quote_freshness_ms)
            .unwrap_or(super::super::super::discovery::MAX_QUOTE_FRESHNESS_MS);
        if !self.p.halted
            && now - self.last_scan >= scan_interval
            && m < self.config.execution.session_end_minute
        {
            self.scan_batch(
                &u,
                now,
                scan_batch,
                ranking_window,
                feed_settings.is_some() && !feed_fallback,
            )?;
        }
        self.option_depth_read(now)?;
        let rank_interval = feed_settings
            .as_ref()
            .map(|s| s.tick_ms)
            .unwrap_or(scan_interval);
        let ranked = !self.p.halted && now - self.last_rank >= rank_interval;
        if ranked {
            self.rank(&u, now, feed_settings.as_ref(), feed_fallback, &held)?;
            self.last_rank = now;
        }
        self.strategy_cycle(ranked)?;
        if self.p.halted {
            status = "risk_halted_protective_exits_only";
        }
        self.clear_read_block(freshness)?;
        Ok(status)
    }

    /// Returns `(fallback, fresh)` for this pass's market feed.
    pub(super) fn poll_feed(
        &mut self,
        u: &Universe,
        now: i64,
        feed_settings: Option<&super::super::super::market_feed::Settings>,
    ) -> Result<(bool, bool)> {
        let c = &self.config;
        let mut feed_fallback = true;
        let mut feed_fresh = false;
        if let Some(settings) = feed_settings {
            if self.feed_worker.is_none() && !self.p.halted {
                match super::super::super::market_feed::Worker::start(
                    c,
                    &u.instruments,
                    settings.clone(),
                ) {
                    Ok(worker) => self.feed_worker = Some(worker),
                    Err(_) => {
                        note_feed_health(&self.feature_store, now, false);
                        self.feed_health = serde_json::json!({"mode":"ltpc","fallback":true,"reason":"feed_setup_blocked"})
                    }
                }
            }
            if let Some(worker) = &self.feed_worker {
                let (points, health, fallback, feed_now, activity) = worker.snapshot()?;
                note_feed_health(&self.feature_store, feed_now, !fallback);
                self.feed_health = health;
                feed_fallback = fallback;
                if now - self.last_rank >= settings.tick_ms {
                    let mut store = self
                        .feature_store
                        .lock()
                        .map_err(|_| Fail::new("feature_store_lock"))?;
                    super::super::super::market_feed::observe_cache(
                        &mut self.scan,
                        &u.instruments,
                        &points,
                        settings,
                        feed_now,
                        activity,
                        Some(&mut store),
                    )?;
                    feed_fresh = !fallback;
                    if c.index_feeds.enabled {
                        // Non-tradeable index context: separate store so
                        // cash market medians and screens stay unchanged.
                        let mut index_store = self
                            .index_features
                            .lock()
                            .map_err(|_| Fail::new("feature_store_lock"))?;
                        for key in &c.index_feeds.keys {
                            if let Some(point) = points.get(key) {
                                super::super::observe_feature(
                                    &mut index_store,
                                    key,
                                    point.price,
                                    point.receipt,
                                );
                            }
                        }
                        self.index_context = super::super::IndexContext::capture(
                            c,
                            &points,
                            &index_store,
                            settings,
                            feed_now,
                            activity,
                        );
                    }
                }
            }
            self.note_feed_transition(feed_fallback)?;
        } else {
            if let Ok(mut store) = self.feature_store.lock() {
                store.record_feed_health(now, false);
            }
            self.feed_worker = None;
            self.feed_health = serde_json::json!({"mode":"rest","fallback":false});
        }
        Ok((feed_fallback, feed_fresh))
    }

    /// Journal and alert a feed fallback or recovery once per transition.
    pub(super) fn note_feed_transition(&mut self, feed_fallback: bool) -> Result<()> {
        let c = &self.config;
        if feed_fallback && !self.feed_fallback_seen && !self.feed_health["reason"].is_null() {
            eprintln!(
                "market_feed_fallback reason={} fallback=rate_limited_rest",
                self.feed_health["reason"]
                    .as_str()
                    .unwrap_or("feed_unavailable")
            );
            persist(
                &mut self.db,
                c,
                &self.p,
                millis(),
                None,
                Some(&self.feed_health.to_string()),
                "market_feed_fallback",
            )?;
            self.alerts.emit(Event::MarketFeedFallback,c.session,serde_json::json!({"reason":self.feed_health["reason"],"feed_generation":self.feed_health["reconnects"],"fallback":"rate_limited_rest"}));
            self.feed_fallback_seen = true;
        } else if !feed_fallback && self.feed_fallback_seen {
            eprintln!("market_feed_recovered mode=ltpc");
            persist(
                &mut self.db,
                c,
                &self.p,
                millis(),
                None,
                Some(&self.feed_health.to_string()),
                "market_feed_recovered",
            )?;
            self.alerts.emit(
                Event::MarketFeedRecovered,
                c.session,
                serde_json::json!({"feed_generation":self.feed_health["reconnects"]}),
            );
            self.feed_fallback_seen = false;
        }
        Ok(())
    }

    pub(super) fn protection_read(&mut self, held: &[Instrument], now: i64) -> Result<()> {
        let c = &self.config;
        if !held.is_empty() && now - self.last_protection_read >= 2000 {
            self.last_protection_read = now;
            // A strategy observes held depth too, so it can sell what it holds.
            let depth_sink = self.strategy.is_some().then_some(&mut self.scan);
            match read_executable_quotes(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                held,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                &self.feature_store,
                QuoteReadTarget::Held { depth_sink },
                self.archive.as_ref(),
            )? {
                Some(reason) => self.read_failure = Some(reason),
                None => self.read_failure = None,
            }
        }
        Ok(())
    }

    pub(super) fn plan_read(&mut self, history_admitted: bool, now: i64) -> Result<()> {
        let c = &self.config;
        if history_admitted
            && self.read_failure.is_none()
            && !self.p.halted
            && let Some(plan) = redecision::active_plan(&self.db, c)?
            && now - self.last_plan_read >= 2000
        {
            let actions: Vec<planner::Verdict> = serde_json::from_value(plan["verdicts"].clone())?;
            let mut pending = Vec::new();
            for a in actions {
                let done: bool = self.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM planner_action WHERE decision=? AND idx=?)",
                    rusqlite::params![
                        plan["decision_id"]
                            .as_str()
                            .ok_or("planner_saved_plan_invalid")?,
                        a.index as i64
                    ],
                    |r| r.get(0),
                )?;
                if !done {
                    pending.push(a);
                }
            }
            let actions = pending;
            let planned: Vec<_> = self
                .tradable
                .iter()
                .filter(|i| {
                    actions
                        .iter()
                        .any(|v| v.action.as_ref().is_some_and(|a| a.instrument_key == i.key))
                })
                .cloned()
                .collect();
            if !planned.is_empty() {
                self.last_plan_read = now;
                self.read_failure = read_executable_quotes(
                    self.client,
                    c,
                    &mut self.db,
                    &mut self.p,
                    &planned,
                    &mut self.last_fresh_receipt,
                    self.event_config.as_ref(),
                    &self.feature_store,
                    QuoteReadTarget::Plan,
                    self.archive.as_ref(),
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn scan_batch(
        &mut self,
        u: &Universe,
        now: i64,
        scan_batch: usize,
        ranking_window: i64,
        feed_live: bool,
    ) -> Result<()> {
        let c = &self.config;
        if self.scan.cursor >= u.instruments.len() {
            return Err(Fail::new("scanner_cursor_identity").into());
        }
        let end = self
            .scan
            .cursor
            .saturating_add(scan_batch)
            .min(u.instruments.len());
        let batch = &u.instruments[self.scan.cursor..end];
        self.scan
            .ranking
            .retain(|(_, _, at)| now - *at <= ranking_window);
        let outcome = if feed_live {
            read_scanner_metadata(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                batch,
                &mut self.scan,
                self.event_config.as_ref(),
                self.archive.as_ref(),
            )?
        } else {
            read_executable_quotes(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                batch,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                &self.feature_store,
                QuoteReadTarget::Scan(&mut self.scan),
                self.archive.as_ref(),
            )?
        };
        match outcome {
            Some(reason) => {
                self.read_failure = Some(reason);
                self.last_scan = millis();
            }
            None => {
                self.read_failure = None;
                self.scan
                    .ranking
                    .sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
                self.scan.cursor = if end >= u.instruments.len() { 0 } else { end };
                checkpoint_scanner(&self.db, &self.scan)?;
                self.last_scan = millis();
            }
        }
        Ok(())
    }

    /// Bounded option shortlist REST depth: the same executable read path cash
    /// uses, on option keys only. Cash liquidity/circuit screens never apply to
    /// premiums.
    pub(super) fn option_depth_read(&mut self, now: i64) -> Result<()> {
        let c = &self.config;
        if self.option_universe.is_some()
            && !self.option_instruments.is_empty()
            && !self.p.halted
            && now - self.last_option_depth >= 2000
        {
            self.last_option_depth = now;
            self.read_failure = read_executable_quotes(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                &self.option_instruments,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                &self.feature_store,
                QuoteReadTarget::Depth {
                    scan: &mut self.scan,
                },
                self.archive.as_ref(),
            )?;
        }
        Ok(())
    }

    /// Rank the entire dated universe before truncating prospects.
    pub(super) fn rank(
        &mut self,
        u: &Universe,
        now: i64,
        feed_settings: Option<&super::super::super::market_feed::Settings>,
        feed_fallback: bool,
        held: &[Instrument],
    ) -> Result<()> {
        let c = &self.config;
        let discovery = self
            .event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref());
        let scan_started = std::time::Instant::now();
        if let Some(settings) = discovery {
            let rank_now = millis();
            let projection = super::super::super::discovery::project(
                &self.scan,
                &u.instruments,
                settings,
                rank_now,
            );
            let mut rank_shortlist = None;
            if settings.prefilter.enabled {
                self.prefilter_status = Some(prefilter_shortlist(
                    &self.scan,
                    &u.instruments,
                    settings,
                    rank_now,
                    &mut self.prefilter_previous,
                ));
                rank_shortlist = Some(&self.prefilter_previous);
            } else {
                self.prefilter_previous.clear();
                self.prefilter_status = None;
            }
            self.shadow_scorer.evaluate(
                settings,
                &self.scan,
                &u.instruments,
                rank_now,
                rank_shortlist,
            );
            super::super::super::strategy_results::record_results(
                settings,
                &projection,
                &self.scan,
                &book_binding(c)?,
            )?;
            let minute_returns = self
                .feature_store
                .lock()
                .map_err(|_| Fail::new("feature_store_lock"))?
                .market_minute_returns(settings.feature_store.min_instruments);
            self.shadow_selection.evaluate(
                settings,
                &self.scan,
                now,
                projection["fresh_count"].as_u64().unwrap_or(0) == 0,
                &minute_returns,
            );
            let live_ranking = self.shadow_selection.live_ranking(
                settings,
                &self.scan,
                &u.instruments,
                &projection,
                rank_now,
            );
            let projection = match live_ranking.as_ref() {
                Some(live) => super::super::super::discovery::project_with_live(
                    &self.scan,
                    &u.instruments,
                    settings,
                    rank_now,
                    Some(live),
                ),
                None => projection,
            };
            if let Some(agent) = self.agent.as_ref() {
                self.live_ranking = super::super::super::agent::service::ranking(&projection);
                self.tape.observe(now, &self.live_ranking, agent.top_k());
            }
            let fresh_count = projection["fresh_count"].as_u64().unwrap_or(0);
            let fraction = settings
                .market_feed
                .as_ref()
                .map(|s| s.fresh_drop_alert_fraction)
                .unwrap_or_else(|| market_feed::Settings::default().fresh_drop_alert_fraction);
            if scanner_fresh_drop_feed_ready(&self.feed_health)
                && let Some(previous) = self.previous_fresh_count
                && scanner_fresh_drop(previous, fresh_count, fraction)
            {
                fresh_drop_alert(
                    c,
                    &self.alerts,
                    &self.feed_health,
                    previous,
                    &projection,
                    fresh_count,
                );
            }
            // A fallback projection must not seed the next live baseline.
            // The first established stream projection only seeds it;
            // drops compare consecutive live projections.
            self.previous_fresh_count =
                scanner_fresh_drop_feed_ready(&self.feed_health).then_some(fresh_count);
            self.scan_telemetry = serde_json::json!({
                "scan_latency_ms":scan_started.elapsed().as_secs_f64() * 1000.0,
                "instruments_scanned":u.instruments.len(),
                "eligible":projection["eligible_count"],
                "stale":projection["stale_count"]
            });
            let candidates = super::super::super::discovery::candidates(&projection);
            checkpoint_scanner(&self.db, &self.scan)?;
            if let Some(settings) = feed_settings
                && now - self.last_depth >= settings.depth_poll_ms
            {
                if !feed_fallback {
                    self.depth_read(u, now, settings, held, &candidates)?;
                } else if !self.tape.is_empty() {
                    // The fallback scan rotates through the universe; the
                    // watched names lose their depth cadence until recovery.
                    self.last_depth = now;
                    self.tape_gap(now, "feed_fallback", 0)?;
                }
            }
        }
        Ok(())
    }

    /// Apply a finished strategy call, then start one on this scan when the
    /// rank pass ran. The call runs on its own thread; protection and fills
    /// never wait for it.
    pub(super) fn strategy_cycle(&mut self, ranked: bool) -> Result<()> {
        let Some(run) = self.strategy.as_mut() else {
            return Ok(());
        };
        if !self
            .event_config
            .as_ref()
            .is_some_and(planner::PlannerConfig::runs_strategies)
        {
            return Ok(());
        }
        run.poll(&mut self.db, &self.config, &mut self.p, &millis)?;
        if !ranked || self.p.halted || self.p.stopped || self.p.closed {
            return Ok(());
        }
        let now = millis();
        let Some(top_n) = run.ready(&self.db, &self.config, now)? else {
            return Ok(());
        };
        match super::super::strategy_run::Frozen::capture(
            &self.scan,
            now,
            top_n,
            &self.p,
            &self.tradable,
        ) {
            Ok(frozen) => run.start(frozen, &millis),
            Err(error) => run.fail(
                &self.db,
                &self.config,
                now,
                None,
                &failure_code(error.as_ref()),
            )?,
        }
        Ok(())
    }

    /// Order-book depth for the leading candidates the book does not hold.
    pub(super) fn depth_read(
        &mut self,
        u: &Universe,
        now: i64,
        settings: &super::super::super::market_feed::Settings,
        held: &[Instrument],
        candidates: &[(String, Option<f64>)],
    ) -> Result<()> {
        let held_keys = held.iter().map(|i| i.key.as_str()).collect::<BTreeSet<_>>();
        let shortlist_keys = candidates
            .iter()
            .take(settings.shortlist_size)
            .map(|(key, _)| key.as_str())
            .filter(|key| !held_keys.contains(key))
            .collect::<Vec<_>>();
        // The tape shares this read: one request whatever its key count, so
        // only the batch and request key limits bound it, never the shortlist.
        let cap = settings
            .rest_batch_size
            .min(self.config.quote_keys.max_keys)
            .max(shortlist_keys.len());
        let (keys, dropped) = self.tape.batch(&shortlist_keys, &held_keys, cap);
        let shortlist = keys
            .iter()
            .filter_map(|key| u.instruments.iter().find(|i| i.key == *key).cloned())
            .collect::<Vec<_>>();
        if dropped > 0 {
            self.tape_gap(now, "depth_batch_full", dropped)?;
        }
        let c = &self.config;
        if !shortlist.is_empty() {
            self.last_depth = now;
            self.read_failure = read_executable_quotes(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                &shortlist,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                &self.feature_store,
                QuoteReadTarget::Depth {
                    scan: &mut self.scan,
                },
                self.archive.as_ref(),
            )?;
        }
        Ok(())
    }

    /// Journal a stretch the research tape could not cover.
    pub(super) fn tape_gap(&mut self, now: i64, reason: &str, dropped: usize) -> Result<()> {
        let body = serde_json::json!({"reason":reason,"watched":self.tape.len(),"dropped":dropped});
        persist(
            &mut self.db,
            &self.config,
            &self.p,
            now,
            None,
            Some(&body.to_string()),
            QUOTE_TAPE_GAP_EVENT,
        )
    }

    pub(super) fn clear_read_block(&mut self, freshness: i64) -> Result<()> {
        let now = millis();
        let fresh = self.last_fresh_receipt.is_some_and(|at| {
            now.checked_sub(at)
                .is_some_and(|age| (0..=freshness).contains(&age))
        });
        if fresh
            && self.read_failure.is_none()
            && !self.p.stopped
            && !self.p.halted
            && !self.p.closed
        {
            redecision::clear_transient_read_block(&mut self.db, &self.config)?;
        }
        Ok(())
    }
}
