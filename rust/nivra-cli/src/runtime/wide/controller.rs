//! Dated paper loop and worker orchestration; financial helpers remain in wide.
use super::*;
use crate::runtime::{
    controller_events::{Event as ControllerEvent, Events, Inbox, Notify},
    market_archive::capture,
};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

mod choice_watch;
mod close;
mod dispatch;
mod prepare;
mod quotes;
mod strategy_inputs;
mod trading;
mod view;

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

pub(in crate::runtime) fn run(c: &Config) -> Result<()> {
    let mut controller = Controller::start(c)?;
    controller.note_session("startup");
    let mut events = Events::default();
    loop {
        match controller.tick(&events) {
            Ok(Flow::Continue) => {}
            Ok(Flow::Stopped { at }) => return controller.stop(at),
            Err(error) => {
                controller.stop_archive()?;
                return Err(error);
            }
        }
        events = controller.events.wait(controller.time_until_due());
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
struct Controller {
    pending_universe: Option<prepare::Preparation<Universe>>,
    pending_options: Option<prepare::Preparation<options::OptionUniverse>>,
    events: Inbox,
    next_maintenance: i64,
    active: bool,
    _choice_watch: Option<choice_watch::ChoiceWatch>,
    quote_reads: quotes::QuoteReads,
    strategy_inputs: strategy_inputs::Inputs,
    depth_candidates: Vec<(String, Option<f64>)>,
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
    checked_feed_retry: i64,
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
    /// The last health snapshot written to `diagnostics.ndjson`.
    diagnosed_health: serde_json::Value,
    /// The pi lane failure last written to `diagnostics.ndjson`.
    agent_failure: Option<String>,
    session_started: bool,
    last_fresh_receipt: Option<i64>,
    archive: Option<capture::Writer>,
    alerts: Alerts,
    option_retry: i64,
    last_option_depth: i64,
    option_status: serde_json::Value,
    option_instruments: Vec<Instrument>,
    option_universe: Option<options::OptionUniverse>,
    tradable: Vec<Instrument>,
    tradable_by_key: BTreeMap<String, usize>,
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
    research_tasks_status: serde_json::Value,
    research_worker: harness::ResearchRuntime,
    /// pi lanes, when the planner has an `agent` block.
    agent: Option<super::super::agent::service::LaneService>,
    /// The strategy executor, when the planner enables `strategy_execution`.
    strategy: Option<super::strategy_run::StrategyRun>,
    /// The built-in ranking the last rank pass produced, best first.
    live_ranking: Vec<super::super::agent::Ranked>,
    /// Top-K names the depth read keeps on the research quote tape.
    tape: super::tape::TapeWatch,
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
    session_minute_ms(c, c.execution.session_end_minute)
}

fn session_minute_ms(c: &Config, minute: u32) -> Option<i64> {
    chrono::NaiveTime::from_num_seconds_from_midnight_opt(minute.saturating_mul(60), 0)
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

/// The executor for a policy that enables `strategy_execution`; none otherwise.
fn strategy_runner(
    c: &Config,
    cfg: Option<&planner::PlannerConfig>,
    notify: Notify,
) -> Result<Option<super::strategy_run::StrategyRun>> {
    if !cfg.is_some_and(planner::PlannerConfig::runs_strategies) {
        return Ok(None);
    }
    let worker = cfg
        .map(planner::PlannerConfig::effective_strategy_worker)
        .unwrap_or_default();
    Ok(Some(super::strategy_run::StrategyRun::new(
        super::super::strategy_library::Library::at(super::super::strategy_library::root(c)?),
        worker,
        cfg.map(planner::PlannerConfig::strategy_policy)
            .transpose()?
            .unwrap_or_default(),
        Some(notify),
    )))
}

fn watch_strategy_choices(
    c: &Config,
    strategy: &Option<super::strategy_run::StrategyRun>,
    notify: Notify,
) -> Option<choice_watch::ChoiceWatch> {
    strategy.as_ref()?;
    let watch = super::super::strategy_library::root(c)
        .and_then(|root| choice_watch::ChoiceWatch::start(&root, notify));
    match watch {
        Ok(watch) => Some(watch),
        Err(_) => {
            eprintln!("strategy_choice_watch_unavailable");
            None
        }
    }
}

fn instrument_index(tradable: &[Instrument]) -> BTreeMap<String, usize> {
    tradable
        .iter()
        .enumerate()
        .map(|(index, i)| (i.key.clone(), index))
        .collect()
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

/// Attach the pi lanes for one dated book. A failed attach records its incident
/// and an explicit reason; it never aborts the paper loop.
fn attach_policy(
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    policy: Result<planner::PlannerConfig>,
) -> Result<(
    Option<planner::PlannerConfig>,
    Option<super::super::agent::service::LaneService>,
    Option<&'static str>,
)> {
    Ok(match attach_agent(db, c, p, policy)? {
        Some((cfg, agent)) => (Some(cfg), Some(agent), None),
        None => (None, None, Some("pi_paper_authority_unavailable")),
    })
}

/// Per-dated-book scanner and feature evidence. Cold start and in-process
/// rollover both build it from the successor policy, so a dated book never
/// inherits another session's scanner preload, feature stores or option state.
pub(super) struct SessionEvidence {
    pub(super) scan: Scanner,
    pub(super) feature_store: FeatureStore,
    /// Separate store: index minute returns must never enter the cash market median.
    pub(super) index_features: FeatureStore,
    pub(super) option_status: serde_json::Value,
}

impl SessionEvidence {
    pub(super) fn load(
        db: &Connection,
        c: &Config,
        policy: Option<&planner::PlannerConfig>,
    ) -> Result<Self> {
        let preload_max_age = policy
            .and_then(|cfg| cfg.discovery.as_ref())
            .and_then(|d| d.market_feed.as_ref())
            .map_or(0, |feed| feed.metadata_preload_max_age_ms);
        Ok(Self {
            scan: load_scanner(db, preload_max_age, millis(), &c.execution)?,
            feature_store: session_features(c, policy)?,
            index_features: session_features(c, None)?,
            option_status: if c.nifty_options.enabled {
                serde_json::json!({"status":"waiting_for_spot","reason":"option_spot_unavailable"})
            } else {
                serde_json::json!({"status":"disabled"})
            },
        })
    }
}

fn start_archive(c: &Config) -> Result<capture::Writer> {
    capture::Writer::start(
        &c.directory,
        &rollover::root(c)?,
        c.archive_settings()?.clone(),
    )
}

impl Controller {
    fn start(c: &Config) -> Result<Self> {
        c.archive_settings()?;
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
        let (event_config, agent, event_config_reason) = attach_policy(&mut db, c, &mut p, policy)?;
        let events = Inbox::new();
        let strategy = strategy_runner(c, event_config.as_ref(), events.notify())?;
        let choice_watch = watch_strategy_choices(c, &strategy, events.notify());
        let quote_reads = quotes::QuoteReads::start(c, events.notify())?;
        persist(&mut db, c, &p, millis(), None, None, "wide_reconciled")?;
        let token = dashboard_token(c)?;
        let view = Arc::new(Mutex::new(serde_json::json!({"status":"starting"})));
        let SessionEvidence {
            scan,
            feature_store,
            index_features,
            option_status,
        } = SessionEvidence::load(&db, c, event_config.as_ref())?;
        let feature_store = Arc::new(Mutex::new(feature_store));
        let index_features = Arc::new(Mutex::new(index_features));
        let equity_series = Arc::new(Mutex::new(web::EquitySeries::new(
            c.dashboard.equity_sample_interval_ms,
            c.dashboard.equity_max_points,
        )));
        let instrument_catalog = Arc::new(Mutex::new(None));
        let display_catalog_started = AtomicBool::new(false);
        web::start(
            web::Ctx::new(
                token,
                c.dashboard_public_host.clone(),
                Arc::clone(&view),
                c.dashboard.clone(),
            )
            .with_history(c.directory.join("model/live-stream.json"))
            .with_series(
                Arc::clone(&feature_store),
                Arc::clone(&equity_series),
                Arc::clone(&instrument_catalog),
            )
            .with_archive(c.market_archive.clone()),
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
        if c.nifty_options.enabled {
            // The runtime-only per-segment schedule is never serialized into the
            // cash book; `paper_costs` stays the cash policy.
            p.fee_schedule = Some(c.fee_schedule()?);
        }
        if let Ok(mut catalog) = instrument_catalog.lock() {
            *catalog = universe
                .as_deref()
                .map(|u| super::super::instrument_display::Catalog::new(u.dashboard_projection()));
        }
        super::super::instrument_display::start(c, &instrument_catalog, &display_catalog_started);
        let alerts = Alerts::start(c.alert_webhook_file.as_deref(), true);
        let reported = p.closed && c.directory.join("report.json").is_file();
        let history_attempt = HistoryAttempts::load(&config)?;
        let archive = Some(start_archive(c)?);
        Ok(Self {
            pending_universe: None,
            pending_options: None,
            events,
            next_maintenance: 0,
            active: false,
            _choice_watch: choice_watch,
            quote_reads,
            strategy_inputs: strategy_inputs::Inputs::default(),
            depth_candidates: Vec::new(),
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
            checked_feed_retry: 0,
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
            diagnosed_health: serde_json::Value::Null,
            agent_failure: None,
            session_started: false,
            last_fresh_receipt: None,
            archive,
            alerts,
            option_retry: 0,
            last_option_depth: 0,
            option_status,
            option_instruments: Vec::new(),
            option_universe: None,
            tradable_by_key: instrument_index(&tradable),
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
            research_tasks_status: serde_json::json!({"status":"disabled"}),
            research_worker: harness::ResearchRuntime::default(),
            // The research lane's own slot (#237): never shared with trading turns.
            agent,
            strategy,
            live_ranking: Vec::new(),
            tape: super::tape::TapeWatch::default(),
            event_config_reason,
            event_config,
            p,
            db,
            lock,
            config,
            _daily_lock,
        })
    }

    /// Feed this pass to the pi lanes. A lane failure is reported and never
    /// stops the controller: protection and fills stay deterministic.
    fn agent_tick(&mut self, t: Now, decision_ready: bool, prepared: bool) {
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
        let regime = self.index_context.regime_facts().or_else(|| {
            let regime_cfg = cfg
                .discovery
                .as_ref()
                .and_then(|d| d.strategy_selection.as_ref())
                .map(|s| s.regime.clone())
                .unwrap_or_default();
            let max_age = cfg
                .discovery
                .as_ref()
                .map_or(super::super::discovery::MAX_QUOTE_FRESHNESS_MS, |d| {
                    d.max_scan_age_ms
                });
            let minute_returns = store.market_minute_returns(regime_cfg.min_instruments);
            let inputs = shadow::shadow_regime_inputs(&self.scan, t.now, max_age, &minute_returns);
            if inputs.instrument_count == 0
                || inputs.minute_count == 0
                || !inputs.session_return.is_finite()
                || !inputs.opening_gap.is_finite()
                || !inputs.realised_volatility.is_finite()
                || !inputs.breadth.is_finite()
            {
                return None;
            }
            let label = nivra_core::regime::classify(&inputs, &regime_cfg).label();
            Some(super::super::agent::RegimeFacts {
                source: "market",
                label,
                session_return: inputs.session_return,
                opening_gap: inputs.opening_gap,
                realised_volatility: inputs.realised_volatility,
                breadth: inputs.breadth,
                instrument_count: inputs.instrument_count,
                minute_count: inputs.minute_count,
            })
        });
        let strategy_health = self.strategy.as_mut().and_then(|run| {
            let open = session_minute_ms(&self.config, self.config.execution.session_start_minute)?;
            let close = session_end_ms(&self.config)?;
            Some(run.health(&self.p, t.now, open..close))
        });
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
            prepared,
            scanner: &self.scan,
            store: &store,
            index: self
                .config
                .index_feeds
                .enabled
                .then(|| self.index_context.json()),
            regime,
            strategy_health: strategy_health.as_ref(),
            spot: super::super::options::spot_quote(&self.index_context, &self.config, t.now),
        };
        match agent.tick(&cycle, &mut self.db) {
            Ok(()) => self.agent_failure = None,
            Err(error) => {
                let reason = failure_code(error.as_ref());
                eprintln!("agent_lanes_failed reason={reason}");
                // A failure repeated every pass is written once.
                if self.agent_failure.as_deref() != Some(reason.as_str()) {
                    diagnostics::note(
                        &self.config.directory,
                        t.now,
                        "failure",
                        serde_json::json!({"stage":"agent_lanes","reason":diagnostics::code(&reason)}),
                    );
                    self.agent_failure = Some(reason);
                }
            }
        }
    }

    /// The session's startup evidence: release, book flags and lanes. No
    /// account, path or credential.
    fn note_session(&mut self, event: &str) {
        let p = &self.p;
        diagnostics::note(
            &self.config.directory,
            millis(),
            event,
            serde_json::json!({
                "version":env!("CARGO_PKG_VERSION"),"session":self.config.session,
                "pid":std::process::id(),"halted":p.halted,"stopped":p.stopped,
                "closed":p.closed,"health":diagnostics::code(&p.health),
                "positions":p.positions.len(),"fills":p.fills.len(),
                "agent_lanes":self.agent.is_some(),
                "planner_reason":self.event_config_reason,
            }),
        );
        self.diagnosed_health = serde_json::Value::Null;
    }

    /// Write the book's health flags, read failure and feed mode when they
    /// change, so a closed day's transitions survive host journal retention.
    fn note_health(&mut self, now: i64) {
        let p = &self.p;
        let health = serde_json::json!({
            "health":diagnostics::code(&p.health),"halted":p.halted,"stopped":p.stopped,
            "closed":p.closed,
            "read_failure":self.read_failure.as_deref().map(diagnostics::code),
            "feed_mode":self.feed_health["mode"],"feed_fallback":self.feed_health["fallback"],
        });
        if health != self.diagnosed_health {
            diagnostics::note(
                &self.config.directory,
                now,
                "health_transition",
                health.clone(),
            );
            self.diagnosed_health = health;
        }
    }

    fn record_archive_gaps(&self) -> Result<()> {
        let Some(writer) = &self.archive else {
            return Ok(());
        };
        let dropped = writer.take_dropped();
        if dropped > 0 {
            if let Err(error) = capture::record_gap(&self.db, dropped) {
                writer.restore_dropped(dropped);
                return Err(error);
            }
            self.alerts.emit(
                Event::ArchiveGap,
                self.config.session,
                serde_json::json!({"dropped_batches":dropped}),
            );
        }
        for session in writer.prior_gaps() {
            self.alerts.emit(
                Event::ArchiveGap,
                session,
                serde_json::json!({"dropped_batches":1}),
            );
        }
        Ok(())
    }

    fn stop_archive(&mut self) -> Result<()> {
        if let Some(writer) = &mut self.archive {
            writer.stop();
        }
        self.record_archive_gaps()?;
        self.archive = None;
        Ok(())
    }

    /// Cancel workers, then settle the book for an operator or service stop.
    fn stop(mut self, now: i64) -> Result<()> {
        self.stop_archive()?;
        if let Some(run) = &mut self.strategy {
            run.retire_call(&self.db, now)?;
        }
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
        diagnostics::note(
            &self.config.directory,
            now,
            "shutdown",
            serde_json::json!({"kind":intent.kind.as_str(),"halted":self.p.halted,
                "stopped":self.p.stopped,"closed":self.p.closed,
                "in_flight_decision":in_flight.is_some()}),
        );
        self.alerts.emit(Event::Stop, self.config.session, details);
        self.alerts.finish();
        Ok(())
    }
}
