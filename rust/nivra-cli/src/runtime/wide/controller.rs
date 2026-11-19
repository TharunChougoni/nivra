//! Dated paper loop and worker orchestration; financial helpers remain in wide.
use super::*;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    let mut controller = Controller::start(c, client)?;
    loop {
        if let Flow::Stopped { at } = controller.tick()? {
            return controller.stop(at);
        }
    }
}

const HISTORY_CANCEL_WAIT: Duration = Duration::from_secs(5);

enum Flow {
    Continue,
    /// A stop signal was observed in the pass sampled at `at`.
    Stopped {
        at: i64,
    },
}

enum Advance {
    Adopted,
    Idle(&'static str),
}

/// The clock sample one loop pass is decided against.
#[derive(Clone, Copy)]
struct Now {
    now: i64,
    day: NaiveDate,
    m: u32,
}

/// State the paper loop carries across passes. Fields are declared in reverse
/// of their acquisition order so they drop like the former loop locals: workers
/// and the alert sender first, then the book, then the session and root locks.
struct Controller<'a> {
    client: &'a data::Client,
    volume_history_loaded: bool,
    last_history_check: i64,
    last_readiness: i64,
    readiness_status: serde_json::Value,
    history_status: serde_json::Value,
    history_attempt: HistoryAttempts,
    history_worker: Option<preparation::PreparationWorker>,
    planner_status: serde_json::Value,
    read_failure: Option<String>,
    reported: bool,
    retention_status: serde_json::Value,
    last_plan_read: i64,
    last_protection_read: i64,
    last_depth: i64,
    last_rank: i64,
    prefilter_status: Option<serde_json::Value>,
    prefilter_previous: BTreeSet<String>,
    shadow_scorer: ShadowScorer,
    shadow_selection: ShadowSelection,
    feed_fallback_seen: bool,
    index_context: super::IndexContext,
    feed_health: serde_json::Value,
    scan_telemetry: serde_json::Value,
    previous_fresh_count: Option<u64>,
    feed_worker: Option<super::super::market_feed::Worker>,
    last_scan: i64,
    scan: Scanner,
    retry: i64,
    rollover_seen: bool,
    stale_seen: bool,
    stale_since: Option<i64>,
    stop_seen: bool,
    halt_seen: bool,
    session_started: bool,
    last_fresh_receipt: Option<i64>,
    alerts: Alerts,
    option_retry: i64,
    last_option_depth: i64,
    option_status: serde_json::Value,
    option_instruments: Vec<Instrument>,
    option_universe: Option<options::OptionUniverse>,
    tradable: Vec<Instrument>,
    /// Shared so one pass's session phases can read it beside `&mut self`.
    universe: Option<Rc<Universe>>,
    stopping: Arc<AtomicBool>,
    display_catalog_status: serde_json::Value,
    display_catalog_started: AtomicBool,
    instrument_catalog: Arc<Mutex<Option<crate::runtime::instrument_display::Catalog>>>,
    equity_series: Arc<Mutex<web::EquitySeries>>,
    index_features: Arc<Mutex<FeatureStore>>,
    feature_store: Arc<Mutex<FeatureStore>>,
    view: Arc<Mutex<serde_json::Value>>,
    loop_generation: u64,
    owner_research_status: serde_json::Value,
    owner_research_worker: harness::OwnerResearchRuntime,
    /// pi lanes, when the planner has an `agent` block.
    agent: Option<super::super::agent::service::LaneService>,
    /// The built-in ranking the last rank pass produced, best first.
    live_ranking: Vec<super::super::agent::Ranked>,
    event_config_reason: Option<&'static str>,
    event_config: Option<planner::PlannerConfig>,
    p: Portfolio,
    db: Connection,
    lock: AdvisoryLock,
    config: Config,
    _daily_lock: rollover::ControllerLock,
}

/// Session end as exchange-local epoch millis, or `None` when the configured
/// minute does not name a time on the session date.
fn session_end_ms(c: &Config) -> Option<i64> {
    chrono::NaiveTime::from_num_seconds_from_midnight_opt(
        c.execution.session_end_minute.saturating_mul(60),
        0,
    )
    .and_then(|time| {
        c.session
            .and_time(time)
            .and_local_timezone(chrono::FixedOffset::east_opt(19800)?)
            .single()
    })
    .map(|stamp| stamp.timestamp_millis())
}

/// Bounded shortlist over the fresh cash rows; returns the dashboard status.
fn prefilter_shortlist(
    scan: &Scanner,
    instruments: &[Instrument],
    settings: &super::super::discovery::Settings,
    rank_now: i64,
    previous: &mut BTreeSet<String>,
) -> serde_json::Value {
    let mut cache = scan.clone();
    cache
        .samples
        .retain(|key, _| instruments.iter().any(|i| &i.key == key));
    let strategy = settings.strategy.clone().unwrap_or_default();
    let snapshot = nivra_core::strategy::extract_features_with_settings(
        &cache,
        rank_now,
        settings.max_scan_age_ms,
        settings
            .market_feed
            .as_ref()
            .map(|f| f.metadata_stale_ms)
            .unwrap_or(settings.max_scan_age_ms),
        nivra_core::strategy::default_lookback_samples(),
        &strategy.feature_settings,
    );
    // Shadow extraction failure cannot stop the paper loop.
    let rows = snapshot.map(|s| s.rows).unwrap_or_default();
    let shortlist = nivra_core::prefilter::prefilter(&rows, previous, &settings.prefilter);
    let status = serde_json::json!({
        "size": shortlist.keys.len(), "entered": shortlist.entered.len(),
        "exited": shortlist.exited.len(),
        "top10": shortlist.keys.iter().take(10).collect::<Vec<_>>()
    });
    *previous = shortlist.keys.into_iter().collect();
    status
}

fn fresh_drop_alert(
    c: &Config,
    alerts: &Alerts,
    feed_health: &serde_json::Value,
    previous: u64,
    projection: &serde_json::Value,
    fresh_count: u64,
) {
    let mut reasons = projection["screen_reasons"]
        .as_object()
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    reasons.sort_by(|(ak, av), (bk, bv)| bv.as_u64().cmp(&av.as_u64()).then(ak.cmp(bk)));
    let top_reasons = reasons
        .into_iter()
        .take(5)
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

/// The private dashboard bearer token: at least 32 ASCII alphanumerics.
fn dashboard_token(c: &Config) -> Result<String> {
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
    Ok(token)
}

fn load_pi_policy(c: &Config) -> Result<planner::PlannerConfig> {
    let path = c
        .planner_config
        .as_deref()
        .ok_or("pi_paper_planner_required")?;
    let cfg = planner::PlannerConfig::load_for_session(path, c.session)?;
    if !cfg.uses_pi_paper() {
        return Err("pi_paper_planner_required".into());
    }
    Ok(cfg)
}

fn attach_agent(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    policy: Result<planner::PlannerConfig>,
) -> Result<
    Option<(
        planner::PlannerConfig,
        super::super::agent::service::LaneService,
    )>,
> {
    let attach = (|| -> Result<_> {
        let cfg = policy?;
        super::super::agent::paper::initialize(db, c, &cfg, p, millis())?;
        let agent = super::super::agent::service::LaneService::start(&cfg)
            .ok_or("pi_paper_service_unavailable")?;
        Ok((cfg, agent))
    })();
    match attach {
        Ok((cfg, agent)) => {
            p.settings.exit_policy = cfg.exit_policy.clone();
            Ok(Some((cfg, agent)))
        }
        Err(error) => {
            planner_incident_cause(db, c, p, millis(), Some(&failure_code(error.as_ref())))?;
            Ok(None)
        }
    }
}

impl<'a> Controller<'a> {
    fn start(c: &Config, client: &'a data::Client) -> Result<Self> {
        let _daily_lock = rollover::controller_lock(c)?;
        let config = c.clone();
        let policy = load_pi_policy(c);
        let policy = if c.directory.join("paper.sqlite").is_file() {
            policy
        } else {
            Ok(policy?)
        };
        fs::create_dir_all(&c.directory)?;
        fs::set_permissions(&c.directory, fs::Permissions::from_mode(0o700))?;
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(c.directory.join("controller.lock"))?;
        lock.try_lock_exclusive()?;
        let lock = AdvisoryLock::new(lock);
        let mut db = open_book(c)?;
        let action_fault = ensure_actions(&db).is_err();
        let mut p = load_wide(&mut db, c)?;
        if let Some(warning) = stopped_restart_warning(&p) {
            eprintln!("{warning}");
        }
        if action_fault {
            planner_incident(&mut db, c, &mut p, millis())?;
        }
        let (event_config, agent) = match attach_agent(&mut db, c, &mut p, policy)? {
            Some((cfg, agent)) => (Some(cfg), Some(agent)),
            None => (None, None),
        };
        let event_config_reason = agent.is_none().then_some("pi_paper_authority_unavailable");
        if c.prior_config.is_none() || c.legacy_directory.is_some() {
            persist(&mut db, c, &p, millis(), None, None, "wide_reconciled")?;
        }
        let token = dashboard_token(c)?;
        let view = Arc::new(Mutex::new(serde_json::json!({"status":"starting"})));
        let feature_store = Arc::new(Mutex::new(session_features(c, event_config.as_ref())?));
        // Separate store: index minute returns must never enter the cash market median.
        let index_features = Arc::new(Mutex::new(session_features(c, None)?));
        let equity_series = Arc::new(Mutex::new(web::EquitySeries::new(
            c.dashboard.equity_sample_interval_ms,
            c.dashboard.equity_max_points,
        )));
        let instrument_catalog = Arc::new(Mutex::new(None));
        let display_catalog_started = AtomicBool::new(false);
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
        let stopping = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&stopping))?;
        signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&stopping))?;
        let universe = if c.directory.join("wide-universe.json").exists() {
            Some(Rc::new(universe::cached(c)?))
        } else {
            None
        };
        // Cash universe plus the bounded option shortlist; the option rows are
        // subscribed and read but never enter the cash scanner projection.
        let tradable = universe
            .as_ref()
            .map(|u| u.instruments.clone())
            .unwrap_or_default();
        let mut option_status = serde_json::json!({"status":"disabled"});
        if c.nifty_options.enabled {
            // The runtime-only per-segment schedule is never serialized into the
            // cash book; `paper_costs` stays the cash policy.
            p.fee_schedule = Some(c.fee_schedule()?);
            option_status =
                serde_json::json!({"status":"waiting_for_spot","reason":"option_spot_unavailable"});
        }
        if let Ok(mut catalog) = instrument_catalog.lock() {
            *catalog = universe
                .as_deref()
                .map(|u| super::super::instrument_display::Catalog::new(u.dashboard_projection()));
        }
        super::super::instrument_display::start(c, &instrument_catalog, &display_catalog_started);
        let alerts = Alerts::start(c.alert_webhook_file.as_deref(), true);
        let scan = load_scanner(
            &db,
            event_config
                .as_ref()
                .and_then(|cfg| cfg.discovery.as_ref())
                .and_then(|d| d.market_feed.as_ref())
                .map_or(0, |feed| feed.metadata_preload_max_age_ms),
            millis(),
            &c.execution,
        )?;
        let reported = p.closed && c.directory.join("report.json").is_file();
        let history_attempt = HistoryAttempts::load(&config)?;
        Ok(Self {
            client,
            volume_history_loaded: false,
            last_history_check: 0,
            last_readiness: 0,
            readiness_status: serde_json::json!({"status":"unknown","reason":"readiness_not_checked"}),
            history_status: serde_json::json!({"status":"UNKNOWN","reason":"preparation_not_started"}),
            history_attempt,
            history_worker: None,
            planner_status: serde_json::json!({"status":"disabled_no_plan"}),
            read_failure: None,
            reported,
            retention_status: serde_json::json!({"status":"not_run"}),
            last_plan_read: 0,
            last_protection_read: 0,
            last_depth: 0,
            last_rank: 0,
            prefilter_status: None,
            prefilter_previous: BTreeSet::new(),
            shadow_scorer: ShadowScorer::default(),
            shadow_selection: ShadowSelection::default(),
            feed_fallback_seen: false,
            index_context: super::IndexContext::default(),
            feed_health: serde_json::json!({"mode":"rest","fallback":false}),
            scan_telemetry: serde_json::json!({}),
            previous_fresh_count: None,
            feed_worker: None,
            last_scan: 0,
            scan,
            retry: 0,
            rollover_seen: false,
            stale_seen: false,
            stale_since: None,
            stop_seen: false,
            halt_seen: false,
            session_started: false,
            last_fresh_receipt: None,
            alerts,
            option_retry: 0,
            last_option_depth: 0,
            option_status,
            option_instruments: Vec::new(),
            option_universe: None,
            tradable,
            universe,
            stopping,
            display_catalog_status: serde_json::json!({"status":"not_prepared"}),
            display_catalog_started,
            instrument_catalog,
            equity_series,
            index_features,
            feature_store,
            view,
            loop_generation: 0,
            owner_research_status: serde_json::json!({"status":"disabled"}),
            owner_research_worker: harness::OwnerResearchRuntime::default(),
            // The research lane's own slot (#237): never shared with trading turns.
            agent,
            live_ranking: Vec::new(),
            event_config_reason,
            event_config,
            p,
            db,
            lock,
            config,
            _daily_lock,
        })
    }

    /// One loop pass. Phase order is the journal and clock order.
    fn tick(&mut self) -> Result<Flow> {
        self.shadow_scorer.poll();
        let now = millis();
        let t = Now {
            now,
            day: local(now).date_naive(),
            m: minute(now),
        };
        let session_end_ms = session_end_ms(&self.config);
        let research_context_unavailable = self.event_config.is_some() && session_end_ms.is_none();
        if research_context_unavailable {
            self.owner_research_status = serde_json::json!({
                "status":"unavailable",
                "reason":"controller_session_end_unavailable",
                "action":"none"
            });
        }
        if self.stopping.load(Ordering::Relaxed) {
            return Ok(Flow::Stopped { at: now });
        }
        self.prepare_universe(t)?;
        self.prepare_options(t)?;
        self.prepare_history(t)?;
        self.check_readiness(t);
        let active = self.session_active(t)?;
        let mut status = if self.universe.is_some() {
            "sleeping_until_configured_session"
        } else {
            "sleeping_waiting_for_d1_universe"
        }
        .to_owned();
        self.track_feed_activity(now, active);
        let history_admitted = self.history_admitted();
        if active {
            status = self.trade_session(t, history_admitted)?.to_owned();
        }
        if let Some(closed) = self.close_session(t)? {
            status = closed;
        }
        self.owner_research(now, research_context_unavailable);
        let agent_now = millis();
        self.agent_tick(
            Now {
                now: agent_now,
                day: local(agent_now).date_naive(),
                m: minute(agent_now),
            },
            active && history_admitted && self.universe.is_some() && self.read_failure.is_none(),
        );
        if t.day > self.config.session {
            match self.rollover(t)? {
                Advance::Adopted => return Ok(Flow::Continue),
                Advance::Idle(idle) => status = idle.to_owned(),
            }
        }
        if self.p.stopped {
            status = "stopped_operator_review_required".to_owned();
        }
        self.emit_transitions(t, active)?;
        let interval_ms = self.publish(
            t,
            status,
            active,
            history_admitted,
            research_context_unavailable,
        )?;
        thread::sleep(Duration::from_millis(interval_ms));
        Ok(Flow::Continue)
    }

    /// Feed this pass to the pi lanes. A lane failure is reported and never
    /// stops the controller: protection and fills stay deterministic.
    fn agent_tick(&mut self, t: Now, decision_ready: bool) {
        let (Some(agent), Some(cfg)) = (self.agent.as_mut(), self.event_config.as_ref()) else {
            return;
        };
        let Ok(store) = self.feature_store.lock() else {
            return;
        };
        let rank_max_age = cfg
            .discovery
            .as_ref()
            .map_or(0, |settings| settings.max_scan_age_ms);
        let ranking = if t.now.saturating_sub(self.last_rank).max(0) <= rank_max_age {
            self.live_ranking.as_slice()
        } else {
            &[]
        };
        let cycle = super::super::agent::service::Cycle {
            c: &self.config,
            cfg,
            now: t.now,
            minute: t.m,
            sealed: self.reported && t.day >= self.config.session,
            portfolio: &self.p,
            instruments: &self.tradable,
            ranking,
            ranked_at: self.last_rank,
            decision_ready,
            scanner: &self.scan,
            store: &store,
            index: self
                .config
                .index_feeds
                .enabled
                .then(|| self.index_context.json()),
        };
        if let Err(error) = agent.tick(&cycle, &mut self.db) {
            eprintln!("agent_lanes_failed reason={}", failure_code(error.as_ref()));
        }
    }

    /// Cancel workers, then settle the book for an operator or service stop.
    fn stop(mut self, now: i64) -> Result<()> {
        let mut intent = shutdown::intent(&self.config, now);
        if let Some(worker) = self.history_worker.take() {
            worker.cancel();
            if intent.kind == shutdown::ShutdownKind::Service {
                intent.history = self.settle_cancelled_history(worker);
            }
        }
        let in_flight = self.agent.take().and_then(|agent| {
            let claim = agent.pending_claim().cloned();
            agent.stop();
            claim
        });
        let details = shutdown::settle(
            &mut self.db,
            &self.config,
            &mut self.p,
            &intent,
            in_flight.as_ref(),
            now,
        )?;
        eprintln!("controller_shutdown kind={}", intent.kind.as_str());
        self.alerts.emit(Event::Stop, self.config.session, details);
        self.alerts.finish();
        Ok(())
    }

    /// A service stop waits briefly for the cancelled preparation so its attempt
    /// is recorded failed, and retryable in the window, instead of left
    /// interrupted for review. A worker that does not finish stays interrupted.
    fn settle_cancelled_history(
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

    fn prepare_universe(&mut self, t: Now) -> Result<()> {
        let Now { now, day, .. } = t;
        let c = &self.config;
        if self.universe.is_none()
            && day <= c.session
            && c.is_trading_day(day)?
            && now >= self.retry
        {
            self.universe = universe::prepare(self.client, c).ok().map(Rc::new);
            if let Some(prepared) = &self.universe {
                self.tradable = prepared.instruments.clone();
                self.tradable
                    .extend(self.option_instruments.iter().cloned());
            }
            if let Ok(mut catalog) = self.instrument_catalog.lock() {
                *catalog = self.universe.as_deref().map(|u| {
                    super::super::instrument_display::Catalog::new(u.dashboard_projection())
                });
            }
            super::super::instrument_display::start(
                c,
                &self.instrument_catalog,
                &self.display_catalog_started,
            );
            self.volume_history_loaded = false;
            self.retry = millis() + c.universe_prepare_retry_ms as i64;
        }
        Ok(())
    }

    fn prepare_options(&mut self, t: Now) -> Result<()> {
        let Now { now, day, .. } = t;
        let c = &self.config;
        if c.nifty_options.enabled
            && self.option_universe.is_none()
            && self.universe.is_some()
            && day == c.session
            && now >= self.option_retry
        {
            self.option_retry = millis() + 15_000;
            match options::spot_quote(&self.scan, c, now) {
                Some(spot) => match options::prepare(self.client, c, spot, now) {
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
                    Err(error) => {
                        self.option_status = serde_json::json!({
                            "status":"unavailable",
                            "reason":failure_code(error.as_ref()),
                        });
                    }
                },
                None => {
                    self.option_status = serde_json::json!({
                        "status":"waiting_for_spot",
                        "reason":"option_spot_unavailable",
                    });
                }
            }
        }
        Ok(())
    }

    fn prepare_history(&mut self, t: Now) -> Result<()> {
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

    fn check_readiness(&mut self, t: Now) {
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

    fn session_active(&self, t: Now) -> Result<bool> {
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

    fn track_feed_activity(&mut self, now: i64, active: bool) {
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

    fn history_admitted(&self) -> bool {
        let c = &self.config;
        !c.history_preparation_enabled
            || (self.history_status["target_session"] == c.session.to_string()
                && self.history_status["ready_instruments"]
                    .as_u64()
                    .is_some_and(|n| n > 0)
                && matches!(
                    self.history_status["status"].as_str(),
                    Some("READY" | "PARTIAL")
                ))
    }

    /// Market reads, scanning, ranking and the owner tick inside the session.
    fn trade_session(&mut self, t: Now, history_admitted: bool) -> Result<&'static str> {
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
            .filter(|s| s.mode == super::super::market_feed::Mode::Ltpc);
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
                    .unwrap_or(super::super::discovery::DEFAULT_SCAN_BATCH_SIZE)
            });
        let ranking_window = discovery
            .and_then(|d| d.ranking_window_ms)
            .unwrap_or(super::super::discovery::DEFAULT_RANKING_WINDOW_MS);
        let freshness = discovery
            .and_then(|d| d.quote_freshness_ms)
            .unwrap_or(super::super::discovery::MAX_QUOTE_FRESHNESS_MS);
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
        // Rank the entire dated universe before truncating prospects.
        let rank_interval = feed_settings
            .as_ref()
            .map(|s| s.tick_ms)
            .unwrap_or(scan_interval);
        if !self.p.halted && now - self.last_rank >= rank_interval {
            self.rank(&u, now, feed_settings.as_ref(), feed_fallback, &held)?;
            self.last_rank = now;
        }
        if self.p.halted {
            status = "risk_halted_protective_exits_only";
        }
        self.clear_read_block(freshness)?;
        Ok(status)
    }

    /// Returns `(fallback, fresh)` for this pass's market feed.
    fn poll_feed(
        &mut self,
        u: &Universe,
        now: i64,
        feed_settings: Option<&super::super::market_feed::Settings>,
    ) -> Result<(bool, bool)> {
        let c = &self.config;
        let mut feed_fallback = true;
        let mut feed_fresh = false;
        if let Some(settings) = feed_settings {
            if self.feed_worker.is_none() && !self.p.halted {
                match super::super::market_feed::Worker::start(c, &u.instruments, settings.clone())
                {
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
                    super::super::market_feed::observe_cache(
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
                                super::observe_feature(
                                    &mut index_store,
                                    key,
                                    point.price,
                                    point.receipt,
                                );
                            }
                        }
                        self.index_context = super::IndexContext::capture(
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
    fn note_feed_transition(&mut self, feed_fallback: bool) -> Result<()> {
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

    fn protection_read(&mut self, held: &[Instrument], now: i64) -> Result<()> {
        let c = &self.config;
        if !held.is_empty() && now - self.last_protection_read >= 2000 {
            self.last_protection_read = now;
            match read_apply(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                held,
                &[],
                None,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                None,
                false,
                None,
                &self.feature_store,
                true,
            )? {
                Some(reason) => self.read_failure = Some(reason),
                None => self.read_failure = None,
            }
        }
        Ok(())
    }

    fn plan_read(&mut self, history_admitted: bool, now: i64) -> Result<()> {
        let c = &self.config;
        if history_admitted
            && self.read_failure.is_none()
            && !self.p.halted
            && planner::saved_plan(&self.db, c)?.is_some()
            && now - self.last_plan_read >= 2000
        {
            let plan = planner::saved_plan(&self.db, c)?.ok_or("planner_saved_plan_invalid")?;
            let mut actions: Vec<planner::Verdict> =
                serde_json::from_value(plan["verdicts"].clone())?;
            let mut pending = Vec::new();
            for a in actions.drain(..) {
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
                self.read_failure = read_apply(
                    self.client,
                    c,
                    &mut self.db,
                    &mut self.p,
                    &planned,
                    &[],
                    None,
                    &mut self.last_fresh_receipt,
                    self.event_config.as_ref(),
                    None,
                    false,
                    None,
                    &self.feature_store,
                    false,
                )?;
            }
        }
        Ok(())
    }

    fn scan_batch(
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
        match read_apply(
            self.client,
            c,
            &mut self.db,
            &mut self.p,
            batch,
            &[],
            Some(&mut self.scan),
            &mut self.last_fresh_receipt,
            self.event_config.as_ref(),
            None,
            feed_live,
            None,
            &self.feature_store,
            false,
        )? {
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
    fn option_depth_read(&mut self, now: i64) -> Result<()> {
        let c = &self.config;
        if self.option_universe.is_some()
            && !self.option_instruments.is_empty()
            && !self.p.halted
            && now - self.last_option_depth >= 2000
        {
            self.last_option_depth = now;
            self.read_failure = read_apply(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                &self.option_instruments,
                &[],
                None,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                None,
                false,
                Some(&mut self.scan),
                &self.feature_store,
                false,
            )?;
        }
        Ok(())
    }

    /// Rank the entire dated universe before truncating prospects.
    fn rank(
        &mut self,
        u: &Universe,
        now: i64,
        feed_settings: Option<&super::super::market_feed::Settings>,
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
            let projection =
                super::super::discovery::project(&self.scan, &u.instruments, settings, rank_now);
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
            super::super::strategy_results::record_results(
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
                Some(live) => super::super::discovery::project_with_live(
                    &self.scan,
                    &u.instruments,
                    settings,
                    rank_now,
                    Some(live),
                ),
                None => projection,
            };
            if self.agent.is_some() {
                self.live_ranking = super::super::agent::service::ranking(&projection);
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
            let candidates = super::super::discovery::candidates(&projection);
            checkpoint_scanner(&self.db, &self.scan)?;
            if let Some(settings) = feed_settings
                && now - self.last_depth >= settings.depth_poll_ms
                && !feed_fallback
            {
                self.depth_read(u, now, settings, held, &candidates)?;
            }
        }
        Ok(())
    }

    /// Order-book depth for the leading candidates the book does not hold.
    fn depth_read(
        &mut self,
        u: &Universe,
        now: i64,
        settings: &super::super::market_feed::Settings,
        held: &[Instrument],
        candidates: &[(String, Option<f64>)],
    ) -> Result<()> {
        let c = &self.config;
        let held_keys = held.iter().map(|i| i.key.as_str()).collect::<BTreeSet<_>>();
        let shortlist = candidates
            .iter()
            .take(settings.shortlist_size)
            .filter_map(|(key, _)| {
                u.instruments
                    .iter()
                    .find(|i| &i.key == key && !held_keys.contains(i.key.as_str()))
                    .cloned()
            })
            .collect::<Vec<_>>();
        if !shortlist.is_empty() {
            self.last_depth = now;
            self.read_failure = read_apply(
                self.client,
                c,
                &mut self.db,
                &mut self.p,
                &shortlist,
                &[],
                None,
                &mut self.last_fresh_receipt,
                self.event_config.as_ref(),
                None,
                false,
                Some(&mut self.scan),
                &self.feature_store,
                false,
            )?;
        }
        Ok(())
    }

    fn clear_read_block(&mut self, freshness: i64) -> Result<()> {
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

    /// Session close: settle the owner attempt, close the book and report once.
    /// Returns the closed book's health as the pass status.
    fn close_session(&mut self, t: Now) -> Result<Option<String>> {
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
            self.retention_status = match super::super::retention::run(c, day, true) {
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

    fn write_report(&mut self, now: i64) -> Result<()> {
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

    fn owner_research(&mut self, now: i64, research_context_unavailable: bool) {
        let c = &self.config;
        if let Some(cfg) = self.event_config.as_ref() {
            let owner_blocked = self
                .agent
                .as_ref()
                .is_some_and(|agent| agent.pending_claim().is_some());
            let launch = harness::ResearchLaunch::for_book(harness::LaunchBook {
                halted: self.p.halted,
                stopped: self.p.stopped,
                owner_decision_pending: owner_blocked,
                context_unavailable: research_context_unavailable,
            });
            let mut status = match harness::research_tick_owner_tasks(
                &mut self.db,
                c,
                cfg,
                launch,
                &mut self.owner_research_worker,
                now,
            ) {
                Ok(status) => status,
                Err(_) => serde_json::json!({"status":"unavailable"}),
            };
            status["observation"] = harness::research_observation_status(c, cfg, now);
            if research_context_unavailable {
                status["context"] = research_context_status(true);
            }
            self.owner_research_status = status;
        }
    }

    fn rollover(&mut self, t: Now) -> Result<Advance> {
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
    fn adopt(&mut self, next: Config, now: i64) -> Result<()> {
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
        self.config = next;
        self.history_attempt = HistoryAttempts::load(&self.config)?;
        self.last_history_check = 0;
        self.history_status =
            serde_json::json!({"status":"UNKNOWN","reason":"preparation_not_started"});
        self.universe = None;
        if let Ok(mut catalog) = self.instrument_catalog.lock() {
            *catalog = None;
        }
        self.display_catalog_started
            .store(false, std::sync::atomic::Ordering::Release);
        self.display_catalog_status = serde_json::json!({"status":"not_prepared"});
        self.volume_history_loaded = false;
        self.scan = load_scanner(
            &self.db,
            self.event_config
                .as_ref()
                .and_then(|cfg| cfg.discovery.as_ref())
                .and_then(|d| d.market_feed.as_ref())
                .map_or(0, |feed| feed.metadata_preload_max_age_ms),
            millis(),
            &self.config.execution,
        )?;
        self.feed_worker = None;
        *self
            .feature_store
            .lock()
            .map_err(|_| Fail::new("feature_store_lock"))? =
            session_features(&self.config, self.event_config.as_ref())?;
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
        self.owner_research_status = if self.config.planner_config.is_some() {
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
        match attach_agent(&mut self.db, &self.config, &mut self.p, policy)? {
            Some((cfg, agent)) => {
                self.event_config = Some(cfg);
                self.event_config_reason = None;
                self.agent = Some(agent);
            }
            None => {
                self.event_config = None;
                self.event_config_reason = Some("pi_paper_authority_unavailable");
            }
        }
        Ok(())
    }

    fn emit_transitions(&mut self, t: Now, active: bool) -> Result<()> {
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

    fn refresh_planner_status(&mut self) -> Result<()> {
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
            serde_json::to_value(super::super::agent::read_spend(c).ok())?;
        Ok(())
    }

    fn entry_status(&self) -> &'static str {
        let c = &self.config;
        if self.p.halted {
            "risk_halted_protective_exits_only"
        } else if self.p.stopped {
            "operator_stopped"
        } else if self.planner_status["pending_attempt"].is_string() {
            "planning_no_new_entries"
        } else if planner::saved_plan(&self.db, c).ok().flatten().is_some() {
            "model_plan_ready"
        } else if self.planner_status["status"].as_str() == Some(planner::NO_DECISION_WORK) {
            planner::NO_DECISION_WORK
        } else {
            "model_plan_unavailable_no_trades"
        }
    }

    fn discovery_status(&self) -> Option<serde_json::Value> {
        let cached_live = self.shadow_selection.cached_live().cloned();
        let mut discovery_status = self
            .event_config
            .as_ref()
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|settings| {
                self.universe.as_ref().map(|u| {
                    super::super::discovery::project_with_live(
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
    fn publish(
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
            super::super::decision_owner::activity_analytics(&self.db, c, &self.p).ok()
        } else {
            None
        };
        if let Some(status) = super::super::instrument_display::status(&self.instrument_catalog) {
            self.display_catalog_status = status;
        }
        let (research_library, experiment_lifecycle) = self.research_views(now);
        // One completion sample owns as_of_ms, the heartbeat and both research
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
        let mut v_json = serde_json::json!({"research_library":research_library,"experiment_lifecycle":experiment_lifecycle,"instrument_display":self.display_catalog_status,"paper_only":true,"live_enabled":LIVE_ENABLED,"account":c.account,"authorized_session":c.session,"next_session_config":c.next_sessions(day)?,"policy":self.p.policy,"model_led":self.p.model_led,"strategy_decision":rollover::published_decision(c),"status":status,"as_of_ms":completed_at_ms,"universe_size":self.universe.as_ref().map(|u|u.instruments.len()),"universe_exclusions":self.universe.as_ref().map(|u|&u.exclusions),"security_ready":self.universe.is_some(),"scanned_count":scanned_count,"pending_count":self.universe.as_ref().map(|u|u.instruments.len().saturating_sub(scanned_count as usize)),"market_feed":self.feed_health,"oldest_scan_age_ms":oldest,"screen_reasons":reasons,"discovery":discovery_status,"ranking":self.scan.ranking.iter().take(10).collect::<Vec<_>>(),"capital":self.p.allocation()?,"planner":self.planner_status,"nifty_options":self.option_status,"research_tasks":self.owner_research_status,"activity_attribution":owner_activity,"retention":self.retention_status,"readiness":self.readiness_status,"history_preparation":self.history_status,"model_initialization":initialization,"model_status":"model-led","entry_status":entry_status,"realised_net_pnl_by_fill":self.p.realised_net_pnl(),"portfolio":self.p,"read_failure_reason":self.read_failure,"fee_policy":format!("SIMULATION_ONLY: {}", serde_json::to_string(&self.p.costs.clone().ok_or("paper_costs_required")?)?),"updater":updater_status});
        if let Some(agent) = &self.agent {
            v_json["agent"] = agent.status();
        }
        v_json["research_context"] = research_context_status;
        v_json["controller_heartbeat"] = super::super::operational_health::heartbeat(
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
        let prepared_summary = super::super::web::update_prepared_summary(&v_json);
        nivra_cli::event_feed::summary_changed(&prepared_summary.etag);
        Ok(interval_ms)
    }

    /// Research library and experiment lifecycle projections.
    fn research_views(&self, now: i64) -> (Option<serde_json::Value>, serde_json::Value) {
        let c = &self.config;
        let research_library = self.event_config.as_ref().map(|cfg|
            super::super::experiment_ledger::research::library(c, cfg)
                .unwrap_or_else(|_| serde_json::json!({"status":"unavailable","reason":"original_research_library_unavailable","items":[],"activation_authorized":false}))
        );
        let experiment_lifecycle = self.event_config
            .as_ref()
            .and_then(|cfg| {
                super::super::harness::research::checked_lifecycle_library(c, cfg, now).ok()
            })
            .unwrap_or_else(|| serde_json::json!({"status":"unavailable","reason":"experiment_lifecycle_unavailable","items":[],"activation_authorized":false}));
        let research_library = super::super::experiment_ledger::lifecycle::merge_into_library(
            research_library,
            &experiment_lifecycle,
        );
        (research_library, experiment_lifecycle)
    }

    fn operational_health(
        &self,
        day: NaiveDate,
        completed_at_ms: i64,
    ) -> Result<serde_json::Value> {
        let c = &self.config;
        let operational_health = super::super::operational_health::capture(
            completed_at_ms,
            c.calendar_coverage_at(day)?,
            crate::alert::transport_status(&c.account, &c.session.to_string(), completed_at_ms),
        );
        Ok(serde_json::to_value(operational_health)?)
    }
}
