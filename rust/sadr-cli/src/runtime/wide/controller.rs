//! Dated paper loop and worker orchestration; financial helpers remain in wide.
use super::*;

/// Explicit research-context availability, independent of the research worker
/// status. A missing session end is reported as unavailable, never replaced by
/// a fabricated cutoff or by an idle research tick.
pub(crate) fn research_context_status(unavailable: bool) -> serde_json::Value {
    if unavailable {
        serde_json::json!({
            "status":"unavailable",
            "reason":"controller_session_end_unavailable",
            "action":"none"
        })
    } else {
        serde_json::json!({"status":"available"})
    }
}

pub(in crate::runtime) fn run(c: &Config, client: &data::Client) -> Result<()> {
    let _daily_lock = rollover::controller_lock(c)?;
    let mut config = c.clone();
    fs::create_dir_all(&c.directory)?;
    fs::set_permissions(&c.directory, fs::Permissions::from_mode(0o700))?;
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()?;
    let mut lock = AdvisoryLock::new(lock);
    // The controller refuses a policy that does not meet the decision-owner
    // prerequisites before it opens the book.
    if let Some(path) = &c.planner_config
        && let Ok(planner_config) = planner::PlannerConfig::load_for_session(path, c.session)
    {
        planner_config.validate_decision_owner()?;
    }
    let mut db = open_book(c)?;
    let action_fault = ensure_actions(&db).is_err();
    let mut p = load_wide(&mut db, c)?;
    if let Some(warning) = stopped_restart_warning(&p) {
        eprintln!("{warning}");
    }
    if action_fault {
        planner_incident(&mut db, c, &mut p, millis())?;
    }
    let planner_config_path = c
        .planner_config
        .as_deref()
        .filter(|_| c.prior_config.is_none() || c.legacy_directory.is_some());
    let mut event_config = planner_config_path
        .and_then(|path| planner::PlannerConfig::load_for_session(path, c.session).ok());
    // A configured path that failed to load is unavailable, not unconfigured.
    let mut event_config_reason = if planner_config_path.is_some() && event_config.is_none() {
        Some("planner_config_unavailable")
    } else {
        None
    };
    // The last loaded policy is retained only for static health caps after a
    // usage/authority failure; it is never used to resume planner work.
    let mut supervisor_config_fallback: Option<planner::PlannerConfig> = None;
    if let Some(cfg) = &event_config {
        p.settings.exit_policy = cfg.exit_policy.clone();
    }
    let mut event_mode = if closed_incident::closed_sealed(c, &p) {
        false
    } else {
        match redecision::initialize(&mut db, c, &p, event_config.as_ref(), millis()) {
            Ok(mode) => mode,
            Err(error) => {
                let cause = error.to_string();
                supervisor_config_fallback = event_config.take().or(supervisor_config_fallback);
                event_config_reason = Some("planner_authority_invalid");
                planner_incident_cause(&mut db, c, &mut p, millis(), Some(&cause))?;
                true
            }
        }
    };
    // An owner policy may only run against authority initialized by that exact
    // policy. A legacy or interrupted authority needs the reviewed config-apply
    // transition; stop planner admissions and keep host protection running.
    if event_mode
        && let Some(cfg) = event_config.as_ref()
        && let Err(error) = redecision::owner_startup_check(&db, c, cfg)
    {
        let cause = error.to_string();
        supervisor_config_fallback = event_config.take().or(supervisor_config_fallback);
        event_config_reason = Some("planner_authority_invalid");
        planner_incident_cause(&mut db, c, &mut p, millis(), Some(&cause))?;
    }
    let mut event_worker: Option<redecision::Worker> = None;
    let mut supervisor_worker: Option<super::super::supervisor::Worker> = None;
    let mut aside_worker: Option<super::super::supervisor::Worker> = None;
    // The research lane's own slot (#237): never shared with trading turns.
    let mut research_lane = super::super::supervisor::research::Lane::default();
    let mut scorer_worker: Option<scorer_trigger::Worker> = None;
    let mut scorer_status = serde_json::json!({"status":"awaiting_close"});
    let mut owner_research_worker = harness::OwnerResearchRuntime::default();
    let mut owner_research_status = serde_json::json!({"status":"disabled"});
    let mut research_context_unavailable;
    let mut loop_generation = 0u64;
    if c.prior_config.is_none() || c.legacy_directory.is_some() {
        persist(&mut db, c, &p, millis(), None, None, "wide_reconciled")?;
    }
    let token = match private(&c.dashboard_token_file) {
        Ok(raw) => raw.trim().to_owned(),
        Err(error) => {
            let code = failure_code(error.as_ref());
            if code == "private_file_required" {
                return Err(Fail::new("private_file_required").into());
            }
            return Err(Fail::new("dashboard_token_unreadable").into());
        }
    };
    if token.len() < 32 || !token.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Fail::new("dashboard_token_invalid").into());
    }
    let view = Arc::new(Mutex::new(serde_json::json!({"status":"starting"})));
    let feature_store = Arc::new(Mutex::new(session_features(c, event_config.as_ref())?));
    // Separate store: index minute returns must never enter the cash market median.
    let index_features = Arc::new(Mutex::new(session_features(c, None)?));
    let equity_series = Arc::new(Mutex::new(web::EquitySeries::new(
        c.dashboard.equity_sample_interval_ms,
        c.dashboard.equity_max_points,
    )));
    let instrument_catalog = Arc::new(Mutex::new(None));
    let display_catalog_started = std::sync::atomic::AtomicBool::new(false);
    let mut display_catalog_status = serde_json::json!({"status":"not_prepared"});
    web::start(
        token,
        c.dashboard_public_host.clone(),
        Arc::clone(&view),
        c.directory.join("model/live-stream.json"),
        c.dashboard.clone(),
        Some(Arc::clone(&feature_store)),
        Some(Arc::clone(&equity_series)),
        Some(Arc::clone(&instrument_catalog)),
    )?;
    let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stopping))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stopping))?;
    let mut universe: Option<Universe> = if c.directory.join("wide-universe.json").exists() {
        Some(universe::cached(c)?)
    } else {
        None
    };
    // Cash universe plus the bounded option shortlist; the option rows are
    // subscribed and read but never enter the cash scanner projection.
    let mut tradable: Vec<Instrument> = universe
        .as_ref()
        .map(|u| u.instruments.clone())
        .unwrap_or_default();
    let mut option_universe: Option<options::OptionUniverse> = None;
    let mut option_instruments: Vec<Instrument> = Vec::new();
    let mut option_status = serde_json::json!({"status":"disabled"});
    let mut last_option_depth = 0;
    let mut option_retry = 0;
    if c.nifty_options.enabled {
        // The runtime-only per-segment schedule is never serialized into the
        // cash book; `paper_costs` stays the cash policy.
        p.fee_schedule = Some(c.fee_schedule()?);
        option_status =
            serde_json::json!({"status":"waiting_for_spot","reason":"option_spot_unavailable"});
    }
    if let Ok(mut catalog) = instrument_catalog.lock() {
        *catalog = universe.as_ref().map(Universe::dashboard_projection);
    }
    super::super::instrument_display::start(c, &instrument_catalog, &display_catalog_started);
    let alerts = Alerts::start(c.alert_webhook_file.as_deref(), true);
    let mut last_fresh_receipt = None;
    let mut session_started = false;
    let mut halt_seen = false;
    let mut stop_seen = false;
    let mut stale_since: Option<i64> = None;
    let mut stale_seen = false;
    let mut rollover_seen = false;
    let mut retry = 0;
    let mut scan = load_scanner(
        &db,
        event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|d| d.market_feed.as_ref())
            .map_or(0, |feed| feed.metadata_preload_max_age_ms),
        millis(),
        &c.execution,
    )?;
    let mut last_scan = 0;

    let mut feed_worker: Option<super::super::market_feed::Worker> = None;
    let mut previous_fresh_count = None;
    let mut scan_telemetry = serde_json::json!({});
    let mut feed_health = serde_json::json!({"mode":"rest","fallback":false});
    let mut index_context = super::IndexContext::default();
    let mut feed_fallback_seen = false;
    let mut shadow_selection = ShadowSelection::default();
    let mut shadow_scorer = ShadowScorer::default();
    let mut prefilter_previous = std::collections::BTreeSet::new();
    let mut prefilter_status = None;
    let mut last_rank = 0;
    let mut last_depth = 0;
    let mut last_protection_read = 0;
    let mut last_plan_read = 0;
    let mut retention_status = serde_json::json!({"status":"not_run"});
    let mut reported = p.closed && c.directory.join("report.json").is_file();
    let mut read_failure: Option<String> = None;
    let mut planner_status = serde_json::json!({"status":"disabled_no_plan"});
    let mut history_worker: Option<preparation::PreparationWorker> = None;
    let mut history_attempt = HistoryAttempts::load(&config)?;
    let mut history_status =
        serde_json::json!({"status":"UNKNOWN","reason":"preparation_not_started"});
    let mut readiness_status =
        serde_json::json!({"status":"unknown","reason":"readiness_not_checked"});
    let mut last_readiness = 0;
    let mut last_history_check = 0;
    let mut volume_history_loaded = false;
    loop {
        shadow_scorer.poll();
        let c = &config;
        let now = millis();
        let day = local(now).date_naive();
        let m = minute(now);
        if event_worker.is_none()
            && let Some(cfg) = event_config.as_mut()
        {
            let _ = refresh_experiment_profile(&mut db, c, cfg, now, &alerts);
        }
        let session_end_ms = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
            c.execution.session_end_minute.saturating_mul(60),
            0,
        )
        .and_then(|time| {
            c.session
                .and_time(time)
                .and_local_timezone(chrono::FixedOffset::east_opt(19800)?)
                .single()
        })
        .map(|stamp| stamp.timestamp_millis());
        research_context_unavailable = event_config.is_some() && session_end_ms.is_none();
        if research_context_unavailable {
            owner_research_status = serde_json::json!({
                "status":"unavailable",
                "reason":"controller_session_end_unavailable",
                "action":"none"
            });
        }
        if let (Some(cfg), Some(session_end_ms)) = (event_config.as_ref(), session_end_ms) {
            let allowlist = tradable.iter().map(|i| i.key.clone()).collect::<Vec<_>>();
            let tick = super::super::supervisor::Tick {
                now,
                minute: m,
                in_session: day == c.session
                    && (c.execution.session_start_minute..c.execution.session_end_minute)
                        .contains(&m),
                fill_count: p.fills.len(),
                allowlist: &allowlist,
                session_end_ms,
                positions: serde_json::to_value(&p.positions).unwrap_or(serde_json::Value::Null),
            };
            let mut context = super::super::decision_owner::Context {
                db: &mut db,
                portfolio: &p,
                instruments: &tradable,
            };
            super::super::supervisor::consider_owner(
                c,
                cfg,
                &mut supervisor_worker,
                &mut aside_worker,
                &tick,
                Some(&alerts),
                &mut context,
            );
            // Research ticks after trading and never gates it.
            super::super::supervisor::research::tick_lane(
                c,
                cfg,
                &mut research_lane,
                now,
                session_end_ms,
                !p.halted && !p.stopped,
                p.fills.iter().map(|fill| fill.at).max(),
            );
        }
        if stopping.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(worker) = history_worker.take() {
                worker.cancel();
            }
            drop(supervisor_worker.take());
            drop(aside_worker.take());
            research_lane.stop();
            if let Some(worker) = event_worker.take() {
                worker
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Release);
                if redecision::block(&mut db, c, "operator_stopped_no_replay").is_err() {
                    p.halted = true;
                }
            }
            mark_operator_stopped(&mut p);
            persist(&mut db, c, &p, now, None, None, "operator_stopped")?;
            alerts.emit(
                Event::Stop,
                c.session,
                serde_json::json!({"closed":p.closed,"open_unresolved":p.positions.len(),"operator_stopped":true,"halted":p.halted}),
            );
            alerts.finish();
            return Ok(());
        }
        if universe.is_none() && day <= c.session && c.is_trading_day(day)? && now >= retry {
            universe = universe::prepare(client, c).ok();
            if let Some(prepared) = &universe {
                tradable = prepared.instruments.clone();
                tradable.extend(option_instruments.iter().cloned());
            }
            if let Ok(mut catalog) = instrument_catalog.lock() {
                *catalog = universe.as_ref().map(Universe::dashboard_projection);
            }
            super::super::instrument_display::start(
                c,
                &instrument_catalog,
                &display_catalog_started,
            );
            volume_history_loaded = false;
            retry = millis() + c.universe_prepare_retry_ms as i64;
        }
        // Bounded option universe: spot from the existing quote map, then the
        // current-week contract API and BOD join. Missing or stale spot fails
        // closed with an explicit reason and is retried, never inferred.
        if c.nifty_options.enabled
            && option_universe.is_none()
            && universe.is_some()
            && day == c.session
            && now >= option_retry
        {
            option_retry = millis() + 15_000;
            match options::spot_quote(&scan, c, now) {
                Some(spot) => match options::prepare(client, c, spot, now) {
                    Ok(prepared) => {
                        let catalog = prepared.catalog()?;
                        let instruments = prepared.instruments()?;
                        p.options = Some(catalog);
                        p.fee_schedule = Some(c.fee_schedule()?);
                        tradable.extend(instruments.iter().cloned());
                        option_instruments = instruments;
                        option_status = prepared.dashboard_projection();
                        option_universe = Some(prepared);
                        let session_date =
                            sadr_core::time::ExchangeDate::parse(&c.session.to_string())?;
                        if let Err(reason) = options::expiry::resume_guard(&p, session_date)
                            && !p.halted
                        {
                            p.halted = true;
                            p.health = reason.to_string();
                            persist(&mut db, c, &p, millis(), None, None, "option_expiry_guard")?;
                        }
                    }
                    Err(error) => {
                        option_status = serde_json::json!({
                            "status":"unavailable",
                            "reason":failure_code(error.as_ref()),
                        });
                    }
                },
                None => {
                    option_status = serde_json::json!({
                        "status":"waiting_for_spot",
                        "reason":"option_spot_unavailable",
                    });
                }
            }
        }
        if let Some(worker) = history_worker.as_mut() {
            if m >= c.history_preparation_cutoff_minute || day != c.session {
                worker.cancel();
            }
            if let Some(report) = worker.poll() {
                history_status = serde_json::to_value(report)?;
                history_attempt.completed(c, now, &history_status)?;
                history_status["universe_session"] = history_status["target_session"].clone();
                history_worker = None;
                volume_history_loaded = false;
            }
        }
        if c.history_preparation_enabled
            && history_attempt_window(c, day, m)
            && history_attempt.due(c, now)
            && history_worker.is_none()
            && let Some(u) = &universe
        {
            history_attempt.started(c, now)?;
            let request = preparation_request(c, &u.instruments, now);
            match request.and_then(preparation::PreparationWorker::spawn) {
                Ok(worker) => {
                    history_worker = Some(worker);
                    history_status = serde_json::json!({"status":"PENDING","target_session":day});
                }
                Err(error) => {
                    history_status = serde_json::json!({"status":"BLOCKED","target_session":day,"reason":"history_preparation_admission_failed","failure":failure_code(error.as_ref())});
                    history_attempt.completed(c, now, &history_status)?;
                }
            }
        }
        // Restart after the acquisition cutoff must revalidate original receipts.
        // Never restart downloads or infer readiness from a stale projection.
        if c.history_preparation_enabled
            && day == c.session
            && (c.history_preparation_cutoff_minute..c.execution.session_end_minute).contains(&m)
            && history_worker.is_none()
            && (last_history_check == 0
                || now - last_history_check >= c.history_preparation_check_interval_ms)
            && let Some(u) = &universe
        {
            history_status = recovered_history_status(c, &u.instruments, now);
            last_history_check = now;
        }
        if !volume_history_loaded && let Some(u) = &universe {
            load_daily_volumes(c, &u.instruments, now, &mut scan);
            volume_history_loaded = true;
        }
        history_status["last_attempt"] = history_attempt.last_report();
        history_status["attempts"] = serde_json::json!(history_attempt.count);
        if now.saturating_sub(last_readiness) >= c.model_readiness_check_interval_ms
            || last_readiness == 0
        {
            readiness_status = readiness_snapshot(c, &p, now, &history_status);
            if day == c.session
                && (c.history_preparation_start_minute..c.execution.session_start_minute)
                    .contains(&m)
                && readiness_status["status"] != "ready"
            {
                alerts.emit(
                    Event::ReadinessBlocked,
                    c.session,
                    serde_json::json!({"ready":false}),
                );
            }
            if day == c.session {
                for warning in readiness::credential_warnings(&readiness_status["models"]) {
                    alerts.emit(Event::ModelCredentialUnavailable, c.session, warning);
                }
            }
            last_readiness = now;
        }
        let active = c
            .calendar_at(day)?
            .is_open(sadr_core::time::Timestamp::from_epoch_millis(now)?)?
            && day == c.session
            && (c.execution.session_start_minute..c.execution.session_end_minute).contains(&m)
            && !p.closed
            && !p.stopped;
        let mut status = if universe.is_some() {
            "sleeping_until_configured_session"
        } else {
            "sleeping_waiting_for_d1_universe"
        };
        if !active || p.halted {
            note_feed_health(&feature_store, now, false);
            feed_worker = None;
            feed_fallback_seen = false;
            feed_health = serde_json::json!({"mode":"rest","state":"inactive","fallback":false});
        }
        if active && !session_started {
            alerts.emit(
                Event::SessionStart,
                c.session,
                serde_json::json!({"resumed":!p.fills.is_empty()}),
            );
            session_started = true;
        }
        let history_admitted = !c.history_preparation_enabled
            || (history_status["target_session"] == c.session.to_string()
                && history_status["ready_instruments"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && matches!(history_status["status"].as_str(), Some("READY" | "PARTIAL")));
        if history_admitted
            && day == c.session
            && (c.history_preparation_start_minute..c.execution.session_end_minute).contains(&m)
            && !p.closed
            && !p.halted
            && !p.stopped
            && universe.is_some()
            && event_mode
            && !active
            && p.positions.is_empty()
            && !super::super::supervisor::holding_luna(supervisor_worker.as_ref(), now)
            && let Some(cfg) = &event_config
        {
            let result = {
                let store = feature_store
                    .lock()
                    .map_err(|_| Fail::new("feature_store_lock"))?;
                event_tick(
                    &mut db,
                    c,
                    cfg,
                    &p,
                    &tradable,
                    &mut event_worker,
                    &scan,
                    &store,
                    None,
                    now,
                    true,
                    true,
                )
            };
            contain_event_error(
                result,
                &mut db,
                c,
                &mut p,
                &mut event_worker,
                &mut event_config,
                &mut supervisor_config_fallback,
                &mut event_config_reason,
                millis(),
            )?;
        }
        if active {
            status = "waiting_for_d1_universe";
            if let Some(u) = &universe {
                // No plan yet with nothing to decide is a no-op, not a failure
                // (#221). The planner status names the reason.
                status = if let Some(reason) = planner_status["status"]
                    .as_str()
                    .filter(|status| *status == planner::NO_DECISION_WORK)
                {
                    reason
                } else {
                    "model_plan_unavailable_no_trades"
                };
                let feed_settings = event_config
                    .as_ref()
                    .and_then(|cfg| cfg.discovery.as_ref())
                    .and_then(|d| d.market_feed.clone())
                    .filter(|s| s.mode == super::super::market_feed::Mode::Ltpc);
                scan.screens = Some(
                    event_config
                        .as_ref()
                        .and_then(|cfg| cfg.discovery.as_ref())
                        .and_then(|d| d.screens.clone())
                        .unwrap_or_else(sadr_core::execution::ScreenPolicy::recommended),
                );
                let mut feed_fallback = true;
                let mut feed_fresh = false;
                if let Some(settings) = &feed_settings {
                    if feed_worker.is_none() && !p.halted {
                        match super::super::market_feed::Worker::start(
                            c,
                            &u.instruments,
                            settings.clone(),
                        ) {
                            Ok(worker) => feed_worker = Some(worker),
                            Err(_) => {
                                note_feed_health(&feature_store, now, false);
                                feed_health = serde_json::json!({"mode":"ltpc","fallback":true,"reason":"feed_setup_blocked"})
                            }
                        }
                    }
                    if let Some(worker) = &feed_worker {
                        let (points, health, fallback, feed_now, activity) = worker.snapshot()?;
                        note_feed_health(&feature_store, feed_now, !fallback);
                        feed_health = health;
                        feed_fallback = fallback;
                        if now - last_rank >= settings.tick_ms {
                            let mut store = feature_store
                                .lock()
                                .map_err(|_| Fail::new("feature_store_lock"))?;
                            super::super::market_feed::observe_cache(
                                &mut scan,
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
                                let mut index_store = index_features
                                    .lock()
                                    .map_err(|_| Fail::new("feature_store_lock"))?;
                                for key in &c.index_feeds.keys {
                                    if let Some(point) = points.get(key) {
                                        super::observe_feature(
                                            &mut index_store,
                                            key,
                                            point.price,
                                            point.receipt,
                                        );
                                    }
                                }
                                index_context = super::IndexContext::capture(
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
                    if feed_fallback && !feed_fallback_seen && !feed_health["reason"].is_null() {
                        eprintln!(
                            "market_feed_fallback reason={} fallback=rate_limited_rest",
                            feed_health["reason"].as_str().unwrap_or("feed_unavailable")
                        );
                        persist(
                            &mut db,
                            c,
                            &p,
                            millis(),
                            None,
                            Some(&feed_health.to_string()),
                            "market_feed_fallback",
                        )?;
                        alerts.emit(Event::MarketFeedFallback,c.session,serde_json::json!({"reason":feed_health["reason"],"feed_generation":feed_health["reconnects"],"fallback":"rate_limited_rest"}));
                        feed_fallback_seen = true;
                    } else if !feed_fallback && feed_fallback_seen {
                        eprintln!("market_feed_recovered mode=ltpc");
                        persist(
                            &mut db,
                            c,
                            &p,
                            millis(),
                            None,
                            Some(&feed_health.to_string()),
                            "market_feed_recovered",
                        )?;
                        alerts.emit(
                            Event::MarketFeedRecovered,
                            c.session,
                            serde_json::json!({"feed_generation":feed_health["reconnects"]}),
                        );
                        feed_fallback_seen = false;
                    }
                } else {
                    if let Ok(mut store) = feature_store.lock() {
                        store.record_feed_health(now, false);
                    }
                    feed_worker = None;
                    feed_health = serde_json::json!({"mode":"rest","fallback":false});
                }
                // Held instruments always read and committed before any scanner request.
                let held: Vec<_> = p.positions.values().map(|p| p.instrument.clone()).collect();
                if !held.is_empty() && now - last_protection_read >= 2000 {
                    last_protection_read = now;
                    match read_apply(
                        client,
                        c,
                        &mut db,
                        &mut p,
                        &held,
                        &[],
                        None,
                        &mut last_fresh_receipt,
                        event_config.as_ref().filter(|_| event_mode),
                        event_worker.as_ref().map(|w| w.cancel.as_ref()),
                        false,
                        None,
                        &feature_store,
                        true,
                    )? {
                        Some(reason) => read_failure = Some(reason),
                        None => read_failure = None,
                    }
                }
                if history_admitted
                    && read_failure.is_none()
                    && !p.halted
                    && planner::saved_plan(&db, c)?.is_some()
                    && now - last_plan_read >= 2000
                {
                    let plan = planner::saved_plan(&db, c)?.ok_or("planner_saved_plan_invalid")?;
                    let mut actions: Vec<planner::Verdict> =
                        serde_json::from_value(plan["verdicts"].clone())?;
                    let mut pending = Vec::new();
                    for a in actions.drain(..) {
                        let done:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM planner_action WHERE decision=? AND idx=?)",rusqlite::params![plan["decision_id"].as_str().ok_or("planner_saved_plan_invalid")?,a.index as i64],|r|r.get(0))?;
                        if !done {
                            pending.push(a);
                        }
                    }
                    let actions = pending;
                    let planned: Vec<_> = tradable
                        .iter()
                        .filter(|i| {
                            actions.iter().any(|v| {
                                v.action.as_ref().is_some_and(|a| a.instrument_key == i.key)
                            })
                        })
                        .cloned()
                        .collect();
                    if !planned.is_empty() {
                        last_plan_read = now;
                        read_failure = read_apply(
                            client,
                            c,
                            &mut db,
                            &mut p,
                            &planned,
                            &[],
                            None,
                            &mut last_fresh_receipt,
                            event_config.as_ref().filter(|_| event_mode),
                            event_worker.as_ref().map(|w| w.cancel.as_ref()),
                            false,
                            None,
                            &feature_store,
                            false,
                        )?;
                    }
                }
                let discovery = event_config.as_ref().and_then(|cfg| cfg.discovery.as_ref());
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
                            .unwrap_or(super::super::discovery::DEFAULT_SCAN_BATCH_SIZE)
                    });
                let ranking_window = discovery
                    .and_then(|d| d.ranking_window_ms)
                    .unwrap_or(super::super::discovery::DEFAULT_RANKING_WINDOW_MS);
                let freshness = discovery
                    .and_then(|d| d.quote_freshness_ms)
                    .unwrap_or(super::super::discovery::MAX_QUOTE_FRESHNESS_MS);
                if !p.halted
                    && now - last_scan >= scan_interval
                    && m < c.execution.session_end_minute
                {
                    if scan.cursor >= u.instruments.len() {
                        return Err(Fail::new("scanner_cursor_identity").into());
                    }
                    let end = scan
                        .cursor
                        .saturating_add(scan_batch)
                        .min(u.instruments.len());
                    let batch = &u.instruments[scan.cursor..end];
                    scan.ranking
                        .retain(|(_, _, at)| now - *at <= ranking_window);
                    match read_apply(
                        client,
                        c,
                        &mut db,
                        &mut p,
                        batch,
                        &[],
                        Some(&mut scan),
                        &mut last_fresh_receipt,
                        event_config.as_ref().filter(|_| event_mode),
                        event_worker.as_ref().map(|w| w.cancel.as_ref()),
                        feed_settings.is_some() && !feed_fallback,
                        None,
                        &feature_store,
                        false,
                    )? {
                        Some(reason) => {
                            read_failure = Some(reason);
                            last_scan = millis();
                        }
                        None => {
                            read_failure = None;
                            scan.ranking
                                .sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
                            scan.cursor = if end >= u.instruments.len() { 0 } else { end };
                            checkpoint_scanner(&db, &scan)?;
                            last_scan = millis();
                        }
                    }
                }
                // Bounded option shortlist REST depth: the same executable read
                // path cash uses, on option keys only. Cash liquidity/circuit
                // screens never apply to premiums.
                if option_universe.is_some()
                    && !option_instruments.is_empty()
                    && !p.halted
                    && now - last_option_depth >= 2000
                {
                    last_option_depth = now;
                    read_failure = read_apply(
                        client,
                        c,
                        &mut db,
                        &mut p,
                        &option_instruments,
                        &[],
                        None,
                        &mut last_fresh_receipt,
                        event_config.as_ref().filter(|_| event_mode),
                        event_worker.as_ref().map(|w| w.cancel.as_ref()),
                        false,
                        Some(&mut scan),
                        &feature_store,
                        false,
                    )?;
                }
                // Rank the entire dated universe before truncating prospects.
                let rank_interval = feed_settings
                    .as_ref()
                    .map(|s| s.tick_ms)
                    .unwrap_or(scan_interval);
                if !p.halted && now - last_rank >= rank_interval {
                    let scan_started = std::time::Instant::now();
                    if let (Some(cfg), Some(settings)) = (&event_config, discovery) {
                        let rank_now = millis();
                        let projection = super::super::discovery::project(
                            &scan,
                            &u.instruments,
                            settings,
                            rank_now,
                        );
                        let mut rank_shortlist = None;
                        if settings.prefilter.enabled {
                            let mut cache = scan.clone();
                            cache
                                .samples
                                .retain(|key, _| u.instruments.iter().any(|i| &i.key == key));
                            let strategy = settings.strategy.clone().unwrap_or_default();
                            let snapshot = sadr_core::strategy::extract_features_with_settings(
                                &cache,
                                rank_now,
                                settings.max_scan_age_ms,
                                settings
                                    .market_feed
                                    .as_ref()
                                    .map(|f| f.metadata_stale_ms)
                                    .unwrap_or(settings.max_scan_age_ms),
                                sadr_core::strategy::default_lookback_samples(),
                                &strategy.feature_settings,
                            );
                            // Shadow extraction failure cannot stop the paper loop.
                            let rows = snapshot.map(|s| s.rows).unwrap_or_default();
                            let shortlist = sadr_core::prefilter::prefilter(
                                &rows,
                                &prefilter_previous,
                                &settings.prefilter,
                            );
                            prefilter_status = Some(serde_json::json!({
                                "size": shortlist.keys.len(), "entered": shortlist.entered.len(),
                                "exited": shortlist.exited.len(),
                                "top10": shortlist.keys.iter().take(10).collect::<Vec<_>>()
                            }));
                            prefilter_previous = shortlist.keys.into_iter().collect();
                            rank_shortlist = Some(&prefilter_previous);
                        } else {
                            prefilter_previous.clear();
                            prefilter_status = None;
                        }
                        shadow_scorer.evaluate(
                            settings,
                            &scan,
                            &u.instruments,
                            rank_now,
                            rank_shortlist,
                        );
                        super::super::strategy_results::record_results(
                            settings,
                            &projection,
                            &scan,
                            &book_binding(c)?,
                        )?;
                        let minute_returns = feature_store
                            .lock()
                            .map_err(|_| Fail::new("feature_store_lock"))?
                            .market_minute_returns(settings.feature_store.min_instruments);
                        shadow_selection.evaluate(
                            settings,
                            &scan,
                            now,
                            projection["fresh_count"].as_u64().unwrap_or(0) == 0,
                            &minute_returns,
                        );
                        let live_ranking = shadow_selection.live_ranking(
                            settings,
                            &scan,
                            &u.instruments,
                            &projection,
                            rank_now,
                        );
                        let projection = match live_ranking.as_ref() {
                            Some(live) => super::super::discovery::project_with_live(
                                &scan,
                                &u.instruments,
                                settings,
                                rank_now,
                                Some(live),
                            ),
                            None => projection,
                        };
                        let fresh_count = projection["fresh_count"].as_u64().unwrap_or(0);
                        let fraction = settings
                            .market_feed
                            .as_ref()
                            .map(|s| s.fresh_drop_alert_fraction)
                            .unwrap_or_else(|| {
                                market_feed::Settings::default().fresh_drop_alert_fraction
                            });
                        if scanner_fresh_drop_feed_ready(&feed_health)
                            && let Some(previous) = previous_fresh_count
                            && scanner_fresh_drop(previous, fresh_count, fraction)
                        {
                            let mut reasons = projection["screen_reasons"]
                                .as_object()
                                .into_iter()
                                .flatten()
                                .collect::<Vec<_>>();
                            reasons.sort_by(|(ak, av), (bk, bv)| {
                                bv.as_u64().cmp(&av.as_u64()).then(ak.cmp(bk))
                            });
                            let top_reasons = reasons.into_iter().take(5)
                                .map(|(reason, count)| serde_json::json!({"reason":reason,"count":count}))
                                .collect::<Vec<_>>();
                            let details = serde_json::json!({
                                "previous_fresh_count":previous,
                                "fresh_count":fresh_count,
                                "stale_count":projection["stale_count"],
                                "unseen_count":projection["unseen_count"],
                                "top_sample_reasons":top_reasons,
                                "state":feed_health["state"],
                                "reason":feed_health["reason"],
                                "connected":feed_health["connected"],
                                "last_heartbeat_age_ms":feed_health["last_heartbeat_age_ms"]
                            });
                            eprintln!("scanner_fresh_drop {details}");
                            alerts.emit(Event::ScannerFreshDrop, c.session, details);
                        }
                        // A fallback projection must not seed the next live baseline.
                        // The first established stream projection only seeds it;
                        // drops compare consecutive live projections.
                        previous_fresh_count =
                            scanner_fresh_drop_feed_ready(&feed_health).then_some(fresh_count);
                        scan_telemetry = serde_json::json!({
                            "scan_latency_ms":scan_started.elapsed().as_secs_f64() * 1000.0,
                            "instruments_scanned":u.instruments.len(),
                            "eligible":projection["eligible_count"],
                            "stale":projection["stale_count"]
                        });
                        let candidates = super::super::discovery::candidates(&projection);
                        if let Some(reason) = redecision::scan_refreshed(
                            &mut db,
                            c,
                            cfg,
                            millis(),
                            &candidates,
                            projection["market_median_bps"].as_f64(),
                            projection["market_sample_count"].as_u64().unwrap_or(0) as usize,
                        )? {
                            eprintln!("scanner_trigger_queued {reason}");
                        }
                        checkpoint_scanner(&db, &scan)?;
                        if let Some(settings) = &feed_settings
                            && now - last_depth >= settings.depth_poll_ms
                            && !feed_fallback
                        {
                            let held_keys =
                                held.iter().map(|i| i.key.as_str()).collect::<BTreeSet<_>>();
                            let shortlist = candidates
                                .iter()
                                .take(settings.shortlist_size)
                                .filter_map(|(key, _)| {
                                    u.instruments
                                        .iter()
                                        .find(|i| {
                                            &i.key == key && !held_keys.contains(i.key.as_str())
                                        })
                                        .cloned()
                                })
                                .collect::<Vec<_>>();
                            if !shortlist.is_empty() {
                                last_depth = now;
                                read_failure = read_apply(
                                    client,
                                    c,
                                    &mut db,
                                    &mut p,
                                    &shortlist,
                                    &[],
                                    None,
                                    &mut last_fresh_receipt,
                                    event_config.as_ref().filter(|_| event_mode),
                                    event_worker.as_ref().map(|w| w.cancel.as_ref()),
                                    false,
                                    Some(&mut scan),
                                    &feature_store,
                                    false,
                                )?;
                            }
                        }
                    }
                    last_rank = now;
                }
                if p.halted {
                    status = "risk_halted_protective_exits_only";
                }
                if event_mode
                    && !super::super::supervisor::holding_luna(supervisor_worker.as_ref(), millis())
                    && let Some(cfg) = &event_config
                {
                    let rest_fresh = last_fresh_receipt.is_some_and(|at| {
                        millis()
                            .checked_sub(at)
                            .is_some_and(|age| (0..=freshness).contains(&age))
                    });
                    let fresh = feed_fresh || rest_fresh;
                    // A transport/quota read block clears only after a validated
                    // REST quote and with no attempt in flight. The attempt itself
                    // is settled as failed/no-replay, never replayed.
                    if event_worker.is_none()
                        && read_failure.is_none()
                        && rest_fresh
                        && !p.stopped
                        && !p.halted
                        && !p.closed
                    {
                        redecision::clear_transient_read_block(&mut db, c)?;
                    }
                    let result = {
                        let store = feature_store
                            .lock()
                            .map_err(|_| Fail::new("feature_store_lock"))?;
                        event_tick(
                            &mut db,
                            c,
                            cfg,
                            &p,
                            &tradable,
                            &mut event_worker,
                            &scan,
                            &store,
                            c.index_feeds.enabled.then_some(&index_context),
                            millis(),
                            history_admitted
                                && read_failure.is_none()
                                && p.positions.values().all(|position| {
                                    millis()
                                        .checked_sub(position.marked_at)
                                        .is_some_and(|age| (0..=freshness).contains(&age))
                                }),
                            fresh,
                        )
                    };
                    contain_event_error(
                        result,
                        &mut db,
                        c,
                        &mut p,
                        &mut event_worker,
                        &mut event_config,
                        &mut supervisor_config_fallback,
                        &mut event_config_reason,
                        millis(),
                    )?;
                }
            }
        }
        if day > c.session || (day == c.session && m >= c.execution.session_end_minute) {
            if let Some(worker) = event_worker.take() {
                worker
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Release);
                if redecision::block(&mut db, c, "session_closed_no_replay").is_err() {
                    p.halted = true;
                }
            }
            if close_portfolio(&mut p) {
                persist(&mut db, c, &p, now, None, None, "post_close")?;
                retention_status = match super::super::retention::run(c, day, true) {
                    Ok(summary) => {
                        let summary =
                            serde_json::to_value(summary).unwrap_or(serde_json::Value::Null);
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
            if !reported {
                let tmp = c.directory.join("wide-report.tmp");
                fs::write(
                    &tmp,
                    serde_json::to_vec_pretty(
                        &serde_json::json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":c.account,"session":c.session,"as_of_ms":now,"realised_net_pnl_by_fill":p.realised_net_pnl(),"portfolio":p,"universe":universe,"nifty_options":option_status,"scan_count":scan.samples.len(),"carried_positions":if p.model_led {p.positions.len()}else{0}}),
                    )?,
                )?;
                fs::File::open(&tmp)?.sync_all()?;
                fs::rename(tmp, c.directory.join("report.json"))?;
                rollover::record_review(c)?;
                reported = true;
                let fees = p
                    .fills
                    .iter()
                    .try_fold(Amount::ZERO, |total, f| total.checked_add(f.fee))
                    .ok();
                let net = if p.positions.is_empty() {
                    p.cash.checked_sub(p.allocation()?).ok()
                } else {
                    None
                };
                let hash = fs::read(c.directory.join("report.json"))
                    .ok()
                    .map(|b| digest(&b));
                alerts.emit(Event::SessionEnd, c.session, serde_json::json!({"net_pnl":net,"fills":p.fills.len(),"fees":fees,"open_unresolved":if p.model_led {0}else{p.positions.len()},"carried_positions":if p.model_led {p.positions.len()}else{0},"report_hash":hash}));
            }
            status = &p.health;
        }
        if reported {
            scorer_status = match scorer_trigger::tick(
                &mut db,
                c,
                event_config.as_ref(),
                &p,
                now,
                &mut scorer_worker,
            ) {
                Ok(status) => status,
                Err(_) => {
                    drop(scorer_worker.take());
                    serde_json::json!({"status":"scorer_unavailable_original_evidence_preserved"})
                }
            };
        }
        if let Some(cfg) = event_config.as_ref() {
            let owner_blocked =
                super::super::decision_owner::research_priority_blocked(&db, c).unwrap_or(true);
            let launch = harness::ResearchLaunch::for_book(harness::LaunchBook {
                halted: p.halted,
                stopped: p.stopped,
                owner_decision_pending: owner_blocked,
                context_unavailable: research_context_unavailable,
            });
            let capture = harness::research_capture_observation(&mut db, c, cfg, &p, client, now);
            let mut status = match harness::research_tick_owner_tasks(
                &mut db,
                c,
                cfg,
                launch,
                &mut owner_research_worker,
                now,
            ) {
                Ok(status) => status,
                Err(_) => serde_json::json!({"status":"unavailable"}),
            };
            status["observation_capture"] = capture;
            status["observation"] = harness::research_observation_status(c, cfg, now);
            if research_context_unavailable {
                status["context"] = research_context_status(true);
            }
            owner_research_status = status;
        }
        if day > c.session {
            match rollover::advance(c, now) {
                Ok(Some(next)) => {
                    fs2::FileExt::unlock(&*lock)?;
                    lock = AdvisoryLock::new(
                        fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .mode(0o600)
                            .open(next.directory.join("controller.lock"))?,
                    );
                    lock.try_lock_exclusive()?;
                    db = open_book(&next)?;
                    let action_fault = ensure_actions(&db).is_err();
                    p = load_wide(&mut db, &next)?;
                    if action_fault {
                        planner_incident(&mut db, &next, &mut p, millis())?;
                    }
                    persist(&mut db, &next, &p, now, None, None, "daily_session_created")?;
                    last_fresh_receipt = None;
                    session_started = false;
                    halt_seen = false;
                    stop_seen = false;
                    stale_since = None;
                    stale_seen = false;
                    rollover_seen = false;
                    config = next;
                    history_attempt = HistoryAttempts::load(&config)?;
                    last_history_check = 0;
                    history_status =
                        serde_json::json!({"status":"UNKNOWN","reason":"preparation_not_started"});
                    universe = None;
                    if let Ok(mut catalog) = instrument_catalog.lock() {
                        *catalog = None;
                    }
                    display_catalog_started.store(false, std::sync::atomic::Ordering::Release);
                    display_catalog_status = serde_json::json!({"status":"not_prepared"});
                    volume_history_loaded = false;
                    scan = load_scanner(
                        &db,
                        event_config
                            .as_ref()
                            .and_then(|cfg| cfg.discovery.as_ref())
                            .and_then(|d| d.market_feed.as_ref())
                            .map_or(0, |feed| feed.metadata_preload_max_age_ms),
                        millis(),
                        &config.execution,
                    )?;
                    feed_worker = None;
                    *feature_store
                        .lock()
                        .map_err(|_| Fail::new("feature_store_lock"))? =
                        session_features(&config, event_config.as_ref())?;
                    if let Ok(mut series) = equity_series.lock() {
                        series.reset();
                    }
                    shadow_selection = ShadowSelection::default();
                    shadow_scorer = ShadowScorer::default();
                    prefilter_previous.clear();
                    prefilter_status = None;
                    last_rank = 0;
                    last_depth = 0;
                    retry = 0;
                    last_scan = 0;
                    reported = false;
                    read_failure = None;
                    planner_status = serde_json::json!({"status":"disabled_no_plan"});
                    event_worker = None;
                    drop(scorer_worker.take());
                    scorer_status = serde_json::json!({"status":"awaiting_close"});
                    owner_research_status = if config.planner_config.is_some() {
                        serde_json::json!({
                            "status":"unavailable",
                            "reason":"planner_config_unavailable",
                            "action":"none"
                        })
                    } else {
                        serde_json::json!({"status":"disabled"})
                    };
                    event_config = config.planner_config.as_deref().and_then(|path| {
                        planner::PlannerConfig::load_for_session(path, config.session).ok()
                    });
                    event_config_reason =
                        if config.planner_config.is_some() && event_config.is_none() {
                            Some("planner_config_unavailable")
                        } else {
                            None
                        };
                    supervisor_config_fallback = None;
                    p.settings.exit_policy = event_config
                        .as_ref()
                        .and_then(|cfg| cfg.exit_policy.clone());
                    event_mode = match redecision::initialize(
                        &mut db,
                        &config,
                        &p,
                        event_config.as_ref(),
                        millis(),
                    ) {
                        Ok(mode) => mode,
                        Err(error) => {
                            let cause = error.to_string();
                            supervisor_config_fallback =
                                event_config.take().or(supervisor_config_fallback);
                            event_config_reason = Some("planner_authority_invalid");
                            planner_incident_cause(
                                &mut db,
                                &config,
                                &mut p,
                                millis(),
                                Some(&cause),
                            )?;
                            true
                        }
                    };
                    continue;
                }
                Ok(None) => {
                    status = rollover::idle_status(c, day, now)
                        .unwrap_or("rollover_blocked_inspect_review_human_required");
                }
                Err(_) => status = "rollover_failed_original_book_preserved_human_required",
            }
        }
        if p.stopped {
            status = "stopped_operator_review_required";
        }
        if p.halted && !halt_seen {
            alerts.emit(
                Event::Halt,
                c.session,
                serde_json::json!({"closed":p.closed,"open_unresolved":p.positions.len()}),
            );
        }
        halt_seen = p.halted;
        if p.stopped && !stop_seen {
            alerts.emit(
                Event::Stop,
                c.session,
                serde_json::json!({"closed":p.closed,"open_unresolved":p.positions.len(),"operator_stopped":true,"halted":p.halted}),
            );
        }
        stop_seen = p.stopped;
        if day > c.session && c.is_trading_day(day)? && !rollover_seen {
            let missed = rollover::preview(c, now)
                .ok()
                .is_some_and(|v| v["status"] == "missed_preopen_idle");
            alerts.emit(
                if missed {
                    Event::MissedPreopen
                } else {
                    Event::RolloverBlocked
                },
                day,
                serde_json::json!({"previous_session":c.session}),
            );
            rollover_seen = true;
        }
        if active {
            let receipt = last_fresh_receipt;
            // Start counting at today's open (or controller start), never count overnight.
            let open = now - i64::from(m - 555) * 60_000;
            let since = stale_since.get_or_insert(now);
            if let Some(receipt) = receipt
                .filter(|r| *r >= open && *r + sadr_core::market::MAX_FINANCIAL_AGE_MS > *since)
            {
                *since = receipt + sadr_core::market::MAX_FINANCIAL_AGE_MS;
                stale_seen = false;
            } else if now - *since > i64::from(c.alert_stale_minutes) * 60_000 && !stale_seen {
                alerts.emit(
                    Event::StaleFeed,
                    c.session,
                    serde_json::json!({"stale_minutes":c.alert_stale_minutes}),
                );
                stale_seen = true;
            }
        } else {
            stale_since = None;
            stale_seen = false;
        }
        let mut status = status.to_owned();
        let execution = match execution_projection(&db) {
            Ok(rows) => rows,
            Err(_) => {
                planner_incident(&mut db, c, &mut p, millis())?;
                Vec::new()
            }
        };
        if let Some(events) = match redecision::status(&db, c) {
            Ok(status) => status,
            Err(_) => {
                event_worker = None;
                supervisor_config_fallback = event_config.take().or(supervisor_config_fallback);
                event_config_reason = Some("planner_authority_invalid");
                planner_incident(&mut db, c, &mut p, millis())?;
                Some(serde_json::json!({"status":"planner_authority_invalid_recovery_required"}))
            }
        } {
            planner_status = events;
            if let Some(cfg) = &event_config {
                planner_status["coverage"]["news"] = serde_json::json!(if cfg
                    .events
                    .as_ref()
                    .and_then(|s| s.news_file.as_ref())
                    .is_some()
                {
                    "explicit_archived_feed"
                } else {
                    "not_configured"
                });
            }
        }
        planner_status["execution"] = serde_json::json!(execution);
        planner_status["nifty_options"] = option_status.clone();
        let usage = usage_projection(&mut db, c, &mut p, millis())?;
        let supervisor_charged_calls = usage["supervisor"].as_u64();
        if usage.is_null() {
            event_worker = None;
            supervisor_config_fallback = event_config.take().or(supervisor_config_fallback);
            event_config_reason = Some("planner_usage_unavailable_recovery_required");
            planner_status["status"] =
                serde_json::json!("planner_usage_unavailable_recovery_required");
        }
        planner_status["budget_used"] = usage["digest"]
            .as_u64()
            .map_or(serde_json::Value::Null, |digest| serde_json::json!(digest));
        planner_status["calls_used"] = usage;
        if p.halted {
            status = "risk_halted_protective_exits_only".into();
        }
        let entry_status = if p.halted {
            "risk_halted_protective_exits_only"
        } else if p.stopped {
            "operator_stopped"
        } else if planner_status["pending_attempt"].is_string() {
            "planning_no_new_entries"
        } else if planner_status["last_review"]["handoff_requested"] == false {
            "luna_waiting_for_next_scan"
        } else if planner::saved_plan(&db, c).ok().flatten().is_some() {
            "model_plan_ready"
        } else if planner_status["status"].as_str() == Some(planner::NO_DECISION_WORK) {
            planner::NO_DECISION_WORK
        } else {
            "model_plan_unavailable_no_trades"
        };
        if active && !p.halted && !p.stopped {
            status = entry_status.to_owned();
        }
        let cached_live = shadow_selection.cached_live().cloned();
        let mut discovery_status = event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|settings| {
                universe.as_ref().map(|u| {
                    super::super::discovery::project_with_live(
                        &scan,
                        &u.instruments,
                        settings,
                        millis(),
                        cached_live.as_ref(),
                    )
                })
            });
        if let Some(serde_json::Value::Object(health)) = discovery_status.as_mut()
            && let Some(telemetry) = scan_telemetry.as_object()
        {
            health.extend(telemetry.clone());
        }
        if let Some(status) = discovery_status.as_mut()
            && let Ok(store) = feature_store.lock()
        {
            status["feature_store"] = store.status();
        }
        if let (Some(status), Some(prefilter)) = (&mut discovery_status, &prefilter_status) {
            status["prefilter"] = prefilter.clone();
        }
        shadow_selection.attach(&mut discovery_status);
        shadow_scorer.attach(&mut discovery_status);
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
        if let Ok(equity) = p.equity()
            && let Ok(mut series) = equity_series.lock()
        {
            series.observe(now, &p.cash.to_string(), &equity.to_string());
        }
        let initialization = serde_json::json!({"stage":if p.halted {"halted_requires_review"} else if !history_admitted {"waiting_for_history"} else if planner_status["pending_attempt"].is_string() {"digest_and_decide_in_progress"} else if entry_status=="model_plan_ready" {"model_initialized"} else {"waiting_for_initial_or_next_model_plan"},"paper_only":true,"financial_execution_requires_accepted_plan":true});
        let updater_status = crate::update::dashboard_status();
        let owner_activity = if event_config.is_some() {
            super::super::decision_owner::activity_analytics(&db, c, &p).ok()
        } else {
            None
        };
        if let Some(status) = super::super::instrument_display::status(&instrument_catalog) {
            display_catalog_status = status;
        }
        let research_library = event_config.as_ref().map(|cfg|
            super::super::experiment_ledger::research::library(c, cfg)
                .unwrap_or_else(|_| serde_json::json!({"status":"unavailable","reason":"original_research_library_unavailable","items":[],"activation_authorized":false}))
        );
        let experiment_lifecycle = event_config
            .as_ref()
            .and_then(|cfg| {
                super::super::harness::research::checked_lifecycle_library(c, cfg, now).ok()
            })
            .unwrap_or_else(|| serde_json::json!({"status":"unavailable","reason":"experiment_lifecycle_unavailable","items":[],"activation_authorized":false}));
        let research_library = super::super::experiment_ledger::lifecycle::merge_into_library(
            research_library,
            &experiment_lifecycle,
        );
        let owner_review_status = event_config
            .as_ref()
            .map(|cfg| match session_end_ms {
                Some(session_end_ms) => {
                    super::super::supervisor::review_status(c, cfg, &p, now, session_end_ms)
                }
                None => serde_json::json!({
                    "version":1,
                    "status":"unavailable",
                    "reason":"controller_session_end_unavailable",
                    "activation_authorized":false
                }),
            })
            .unwrap_or_else(|| serde_json::json!({"version":1,"status":"disabled"}));
        // One completion sample owns as_of_ms, the heartbeat and both research
        // projections. Request time in the dashboard API never renews it.
        let completed_at_ms = millis();
        let (owner_research, research_operational) = super::super::supervisor::research_projection(
            c,
            event_config.as_ref(),
            event_config_reason,
            &p,
            completed_at_ms,
            session_end_ms,
        );
        loop_generation = loop_generation
            .checked_add(1)
            .ok_or_else(|| Fail::new("controller_loop_generation_exhausted"))?;
        let active_tick_ms = event_config
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
        let mut v_json = serde_json::json!({"research_library":research_library,"experiment_lifecycle":experiment_lifecycle,"instrument_display":display_catalog_status,"paper_only":true,"live_enabled":LIVE_ENABLED,"account":c.account,"authorized_session":c.session,"next_session_config":c.next_sessions(day)?,"policy":p.policy,"model_led":p.model_led,"strategy_decision":rollover::published_decision(c),"status":status,"as_of_ms":completed_at_ms,"universe_size":universe.as_ref().map(|u|u.instruments.len()),"universe_exclusions":universe.as_ref().map(|u|&u.exclusions),"security_ready":universe.is_some(),"scanned_count":scanned_count,"pending_count":universe.as_ref().map(|u|u.instruments.len().saturating_sub(scanned_count as usize)),"market_feed":feed_health,"oldest_scan_age_ms":oldest,"screen_reasons":reasons,"discovery":discovery_status,"ranking":scan.ranking.iter().take(10).collect::<Vec<_>>(),"capital":p.allocation()?,"planner":planner_status,"nifty_options":option_status,"scorer_trigger":scorer_status,"research_tasks":owner_research_status,"activity_attribution":owner_activity,"retention":retention_status,"readiness":readiness_status,"history_preparation":history_status,"model_initialization":initialization,"model_status":"model-led","entry_status":entry_status,"realised_net_pnl_by_fill":p.realised_net_pnl(),"portfolio":p,"read_failure_reason":read_failure,"fee_policy":format!("SIMULATION_ONLY: {}", serde_json::to_string(&p.costs.clone().ok_or("paper_costs_required")?)?),"updater":updater_status});
        v_json["owner_review"] = owner_review_status;
        v_json["owner_research"] = owner_research;
        v_json["research_context"] = research_context_status;
        v_json["controller_heartbeat"] = super::super::operational_health::heartbeat(
            completed_at_ms,
            interval_ms,
            loop_generation,
        );
        let current_selection = event_config
            .as_ref()
            .and_then(|cfg| super::super::supervisor::current_selection_binding(c, cfg));
        let operational_health = super::super::operational_health::capture(
            completed_at_ms,
            super::super::supervisor::operational_status_with_research(
                &c.directory,
                &c.session.to_string(),
                completed_at_ms,
                super::super::supervisor::supervisor_config(
                    event_config.as_ref(),
                    supervisor_config_fallback.as_ref(),
                    event_config_reason,
                ),
                supervisor_charged_calls,
                Some(research_operational),
                current_selection.as_deref(),
            ),
            c.calendar_coverage_at(day)?,
            crate::alert::transport_status(&c.account, &c.session.to_string(), completed_at_ms),
        );
        v_json["operational_health"] = serde_json::to_value(operational_health)?;
        if c.index_feeds.enabled {
            // Additive, non-tradeable index context; cash fields stay unchanged.
            v_json["index_feeds"] = index_context.json();
        }
        *view.lock().map_err(|_| Fail::new("dashboard_lock"))? = v_json.clone();
        let prepared_summary = super::super::web::update_prepared_summary(&v_json);
        sadr_cli::model_stream::summary_changed(&prepared_summary.etag);
        thread::sleep(Duration::from_millis(interval_ms));
    }
}
