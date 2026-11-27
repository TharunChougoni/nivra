use super::*;
pub(super) fn test_costs() -> nivra_core::finance::FeeArithmetic {
    serde_json::from_str(include_str!(
        "../../../../fixtures/test-legacy-paper-costs.json"
    ))
    .unwrap()
}
pub(super) fn test_portfolio() -> nivra_core::execution::Portfolio {
    let mut p = nivra_core::execution::Portfolio::new();
    p.costs = Some(test_costs());
    p.sizing = Some(test_sleeves("5000", "0").policies());
    p
}
pub(super) fn config(dir: &Path) -> Config {
    Config {
        exit_backtest: exit_backtest::Settings::default(),
        history_download: HistoryDownloadSettings::default(),
        universe_prepare_retry_ms: default_universe_prepare_retry_ms(),
        idle_poll_interval_ms: default_idle_poll_interval_ms(),
        market_data_transport: MarketDataTransportSettings::default(),
        market_archive: None,
        storage: StorageSettings::default(),
        dashboard: DashboardSettings::default(),
        paper_costs: Some(test_costs()),
        paper_costs_by_segment: std::collections::BTreeMap::new(),
        quote_keys: QuoteKeySettings::default(),
        index_feeds: IndexFeedSettings::default(),
        nifty_options: NiftyOptionSettings::default(),
        paper_cost_transition: None,
        alert_webhook_file: None,
        alert_stale_minutes: 5,
        account: "tharun-upstox".into(),
        session: "2026-10-16".parse().unwrap(),
        previous_session: "2026-10-15".parse().unwrap(),
        calendar_source: "NSE/CMTR/71775".into(),
        directory: dir.to_owned(),
        sessions_root: Some(dir.join("sessions")),
        daily_history_sessions: 21,
        execution: Default::default(),
        history_preparation_start_minute: 480,
        history_preparation_cutoff_minute: 550,
        history_preparation_enabled: false,
        history_skip_unavailable: false,
        history_preparation_max_fetch_failures: 25,
        history_preparation_max_attempts: 3,
        history_avg_daily_volume_min_sessions: 10,
        history_preparation_retry_ms: 60_000,
        history_preparation_failed_retry_ms: 60_000,
        history_preparation_check_interval_ms: 60_000,
        model_readiness_check_interval_ms: default_model_readiness_check_interval_ms(),
        planner_config: None,
        token_file: dir.join("not-read"),
        dashboard_token_file: dir.join("not-read-dashboard"),
        dashboard_public_host: None,
        read_journal: dir.join("not-read-journal"),
        read_scope: "upstox-market-tharun-upstox".into(),
        read_journal_device: 0,
        read_journal_inode: 0,
        strategy: "model_planner_v1".into(),
        capital: Some(nivra_core::execution::a("5000")),
        sleeves: test_sleeves("5000", "0"),
    }
}
/// Sleeves whose capitals are `cash` and `options`. Their policies let cash
/// and depth alone bound a buy, so tests that are not about sizing keep the
/// units they name.
pub(crate) fn test_sleeves(cash: &str, options: &str) -> nivra_core::sleeve::SleeveConfigs {
    let roomy = |capital: &str| {
        serde_json::json!({"capital": capital, "risk_per_trade_bps": 10000,
            "max_position_bps": 10000, "max_positions": 20, "min_order_rupees": "0"})
    };
    serde_json::from_value(serde_json::json!({"cash": roomy(cash), "options": roomy(options)}))
        .unwrap()
}
#[test]
fn execution_preparation_defaults_and_validation() {
    super::setup::exercise();
    // A template is intentionally non-runnable until original bindings are
    // supplied. Validate only schema/mandate using known fixture identities.
    let mut template: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../packaging/rust/paper-current.example.json"
    ))
    .unwrap();
    template["session"] = serde_json::json!("2026-10-16");
    template["previous_session"] = serde_json::json!("2026-10-15");
    template["read_journal_device"] = serde_json::json!(0);
    template["read_journal_inode"] = serde_json::json!(0);
    template["market_archive"] = serde_json::json!({
        "url": "postgresql://nivra@localhost/nivra_market",
        "password_file": "/private/archive-password",
    });
    let baseline: Config = serde_json::from_value(template).unwrap();
    baseline.validate().unwrap();
    let original = serde_json::to_value(config(Path::new("fixture"))).unwrap();
    let mut c: Config = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(c.daily_history_sessions, 21);
    assert_eq!(c.history_preparation_start_minute, 480);
    assert_eq!(c.history_preparation_cutoff_minute, 550);
    assert!(c.execution.is_default());
    assert_eq!(serde_json::to_value(&c).unwrap(), original);
    let mut archive = MarketArchiveSettings {
        url: "postgresql://nivra@localhost/nivra_market".into(),
        password_file: "/private/archive-password".into(),
    };
    assert!(archive.validate().is_ok());
    let binding = book_binding(&c).unwrap();
    c.market_archive = Some(archive.clone());
    assert_eq!(book_binding(&c).unwrap(), binding);
    archive.url = "postgresql://nivra:example@localhost/nivra_market".into();
    assert!(archive.validate().is_err());
    archive.url = "postgresql://nivra@localhost/nivra_market".into();
    archive.password_file = "relative-password".into();
    c.market_archive = Some(archive);
    assert!(c.validate().is_err());
    c.market_archive = None;
    c.history_preparation_start_minute = 550;
    assert!(c.validate().is_err());
    c.history_preparation_start_minute = 480;
    c.history_preparation_cutoff_minute = 555;
    assert!(c.validate().is_err());
    c.history_preparation_cutoff_minute = 550;
    c.daily_history_sessions = 0;
    assert!(c.validate().is_err());
    c.daily_history_sessions = 21;
    c.validate().unwrap();
    // Quote-key policy defaults to the NSE cash prefix and stays out of the
    // serialized config until an operator configures it.
    assert_eq!(c.quote_keys, QuoteKeySettings::default());
    assert_eq!(c.quote_keys.prefixes, vec!["NSE_EQ|".to_string()]);
    assert_eq!(c.quote_keys.max_keys, 200);
    assert!(original.get("quote_keys").is_none());
    for (prefixes, max_keys) in [
        (vec!["NSE_EQ|".to_string()], 0),
        (vec!["NSE_EQ|".to_string()], 201),
        (Vec::new(), 200),
        (vec!["BSE_EQ|".to_string()], 200),
        (vec!["NSE_EQ".to_string()], 200),
    ] {
        c.quote_keys = QuoteKeySettings { prefixes, max_keys };
        assert!(c.validate().is_err());
    }
    c.quote_keys = QuoteKeySettings {
        prefixes: vec!["NSE_EQ|".into(), "NSE_INDEX|".into()],
        max_keys: 200,
    };
    c.validate().unwrap();
    let configured = serde_json::to_value(&c).unwrap();
    assert_eq!(configured["quote_keys"]["max_keys"], 200);
    let roundtrip: Config = serde_json::from_value(configured).unwrap();
    assert_eq!(roundtrip.quote_keys, c.quote_keys);
    c.quote_keys = QuoteKeySettings::default();
    c.validate().unwrap();
    // Index feeds are off by default with the three documented keys, and the
    // serialized default config never sprouts the new section.
    assert_eq!(c.index_feeds, IndexFeedSettings::default());
    assert!(!c.index_feeds.enabled);
    assert_eq!(c.index_feeds.keys.len(), 3);
    assert_eq!(c.index_feeds.keys[0], "NSE_INDEX|Nifty 50");
    assert_eq!(c.index_feeds.keys[1], "NSE_INDEX|Nifty Bank");
    assert_eq!(c.index_feeds.keys[2], "NSE_INDEX|India VIX");
    assert!(original.get("index_feeds").is_none());
    // Enabling fails closed unless the operator also allows the prefix.
    c.index_feeds.enabled = true;
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "index_feeds_prefix_not_allowed"
    );
    c.quote_keys.prefixes.push("NSE_INDEX|".into());
    c.validate().unwrap();
    let enabled = serde_json::to_value(&c).unwrap();
    assert_eq!(enabled["index_feeds"]["enabled"], true);
    assert_eq!(enabled["index_feeds"]["keys"].as_array().unwrap().len(), 3);
    let roundtrip: Config = serde_json::from_value(enabled).unwrap();
    assert_eq!(roundtrip.index_feeds, c.index_feeds);
    for keys in [
        vec!["NSE_EQ|INF204KB14I2".to_string()],
        vec![
            "NSE_INDEX|Nifty 50".to_string(),
            "NSE_INDEX|Nifty 50".to_string(),
        ],
        Vec::new(),
    ] {
        c.index_feeds.keys = keys;
        assert!(c.validate().is_err());
    }
    c.index_feeds.keys = default_index_feed_keys();
    // Advisory regime thresholds must describe the configured index set.
    c.index_feeds.regime = Some(nivra_core::regime::RegimeConfig {
        min_instruments: 4,
        ..nivra_core::regime::RegimeConfig::default()
    });
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "index_feeds_regime_invalid"
    );
    c.index_feeds.regime = Some(nivra_core::regime::RegimeConfig {
        min_instruments: 3,
        min_minutes: 2,
        ..nivra_core::regime::RegimeConfig::default()
    });
    c.validate().unwrap();
    c.index_feeds = IndexFeedSettings::default();
    c.quote_keys = QuoteKeySettings::default();
    // The daily-history lookback still requires the 21-session schema minimum.
    c.daily_history_sessions = 20;
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "invalid_daily_history_sessions"
    );
    c.daily_history_sessions = 21;
    c.validate().unwrap();
    // Buy-only Nifty options default off and stay out of the serialized config
    // until an operator enables them; enabling requires an F&O fee schedule and
    // an admitted NSE_FO quote-key prefix.
    assert_eq!(c.nifty_options, NiftyOptionSettings::default());
    assert_eq!(c.nifty_options.underlying_key, "NSE_INDEX|Nifty 50");
    assert_eq!(c.nifty_options.underlying_symbol, "NIFTY");
    assert_eq!(c.nifty_options.expected_lot_size, Some(65));
    assert_eq!(c.nifty_options.strike_window, 20);
    assert!(c.nifty_options.buy_only);
    assert!(
        serde_json::to_value(config(Path::new("fixture")))
            .unwrap()
            .get("nifty_options")
            .is_none()
    );
    c.nifty_options.enabled = true;
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "nifty_options_fee_schedule_required"
    );
    c.paper_costs_by_segment.insert(
        nivra_core::market::Segment::NseFo,
        serde_json::from_value(serde_json::json!({
            "per_order":"0.01","buy_bps":"0","sell_bps":"0",
            "slippage_bps":"10","rounding_increment":"0.05"
        }))
        .unwrap(),
    );
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "nifty_options_quote_keys_required"
    );
    c.quote_keys.prefixes.push("NSE_FO|".into());
    c.validate().unwrap();
    for (field, value) in [
        ("underlying_key", serde_json::json!("NSE_EQ|NIFTYBEES")),
        ("underlying_key", serde_json::json!("NSE_INDEX|")),
        ("underlying_symbol", serde_json::json!("")),
        ("expected_lot_size", serde_json::json!(0)),
        ("strike_window", serde_json::json!(0)),
        ("strike_window", serde_json::json!(101)),
        ("expiries", serde_json::json!([])),
        ("expiries", serde_json::json!(["current_month"])),
        ("buy_only", serde_json::json!(false)),
    ] {
        let mut changed: serde_json::Value = serde_json::to_value(&c).unwrap();
        changed["nifty_options"][field] = value;
        let changed: Config = serde_json::from_value(changed).unwrap();
        assert_eq!(
            changed.validate().unwrap_err().to_string(),
            "nifty_options_settings_invalid",
            "{field}"
        );
    }
    let configured = serde_json::to_value(&c).unwrap();
    assert_eq!(configured["nifty_options"]["enabled"], true);
    assert_eq!(
        serde_json::from_value::<Config>(configured)
            .unwrap()
            .nifty_options,
        c.nifty_options
    );
}
#[test]
fn paper_costs_are_required_and_validated() {
    let mut c = config(Path::new("fixture"));
    c.paper_costs = None;
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "paper_costs_required"
    );
    c.paper_costs =
        Some(serde_json::from_str(include_str!("../../../../fixtures/paper-costs.json")).unwrap());
    c.validate().unwrap();
    // The cash schedule is unchanged; an optional NSE_FO schedule is looked up
    // per segment and never reuses the cash 5/7 bps policy.
    let cash = c.paper_costs.clone().unwrap();
    let fo: nivra_core::finance::FeeArithmetic = serde_json::from_value(serde_json::json!({
        "per_order": "0.01", "buy_bps": "0", "sell_bps": "0",
        "slippage_bps": "10", "rounding_increment": "0.05"
    }))
    .unwrap();
    assert!(
        serde_json::to_value(config(Path::new("fixture")))
            .unwrap()
            .get("paper_costs_by_segment")
            .is_none()
    );
    c.paper_costs_by_segment
        .insert(nivra_core::market::Segment::NseFo, fo.clone());
    c.validate().unwrap();
    let schedule = c.fee_schedule().unwrap();
    assert_eq!(
        schedule.for_segment(nivra_core::market::Segment::NseEq),
        Some(&cash)
    );
    assert_eq!(
        schedule.for_segment(nivra_core::market::Segment::NseFo),
        Some(&fo)
    );
    assert_eq!(
        schedule.for_segment(nivra_core::market::Segment::NseIndex),
        None
    );
    let encoded = serde_json::to_value(&schedule).unwrap();
    assert_eq!(encoded["by_segment"]["NSE_FO"]["slippage_bps"], "10");
    assert_eq!(
        serde_json::from_value::<nivra_core::finance::FeeSchedule>(encoded).unwrap(),
        schedule
    );
    // NSE_EQ belongs to `paper_costs`; a duplicate segment key is refused.
    c.paper_costs_by_segment
        .insert(nivra_core::market::Segment::NseEq, fo);
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "paper_costs_by_segment_invalid"
    );
    // A cost-only transition needs an absolute prior config and a non-future
    // source session; both are validated, not inferred.
    let mut c = config(Path::new("fixture"));
    c.paper_cost_transition = Some(PaperCostTransition {
        source_session: c.session,
        prior_config: PathBuf::from("/tmp/prior-paper.json"),
    });
    c.validate().unwrap();
    c.paper_cost_transition.as_mut().unwrap().source_session = c.session.succ_opt().unwrap();
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "paper_cost_transition_invalid"
    );
    c.paper_cost_transition.as_mut().unwrap().source_session = c.session;
    c.paper_cost_transition.as_mut().unwrap().prior_config = PathBuf::from("prior-paper.json");
    assert_eq!(
        c.validate().unwrap_err().to_string(),
        "paper_cost_transition_invalid"
    );
}
#[test]
fn runtime_timing_defaults_preserve_binding_and_reject_invalid_bounds() {
    let original = serde_json::to_value(config(Path::new("fixture"))).unwrap();
    let c: Config = serde_json::from_value(original.clone()).unwrap();
    c.validate().unwrap();
    assert_eq!(c.universe_prepare_retry_ms, 300_000);
    assert_eq!(c.idle_poll_interval_ms, 15_000);
    assert_eq!(c.market_data_transport.http_timeout_ms, 7_000);
    assert_eq!(c.storage.read_journal_busy_timeout_ms, 1_000);
    assert_eq!(c.dashboard.refresh_interval_ms, 5_000);
    for (group, key, default, maximum) in [
        (None, "universe_prepare_retry_ms", 300_000, 86_400_000),
        (None, "idle_poll_interval_ms", 15_000, 300_000),
        (
            Some("market_data_transport"),
            "http_timeout_ms",
            7_000,
            120_000,
        ),
        (
            Some("storage"),
            "read_journal_busy_timeout_ms",
            1_000,
            60_000,
        ),
        (Some("dashboard"), "refresh_interval_ms", 5_000, 300_000),
    ] {
        assert!(original.get(group.unwrap_or(key)).is_none());
        for value in [0, maximum + 1, -1, 1, maximum, default] {
            let mut input = original.clone();
            if let Some(group) = group {
                input[group] = serde_json::json!({key: value});
            } else {
                input[key] = value.into();
            }
            let decoded = serde_json::from_value::<Config>(input);
            if value < 1 || value > maximum {
                assert!(
                    decoded.is_err() || decoded.unwrap().validate().is_err(),
                    "{key}={value}"
                );
            } else {
                let decoded = decoded.unwrap();
                decoded.validate().unwrap();
                let serialized = serde_json::to_value(&decoded).unwrap();
                if value == default {
                    assert_eq!(serialized, original);
                    assert_eq!(book_binding(&decoded).unwrap(), book_binding(&c).unwrap());
                } else {
                    assert_ne!(serialized, original);
                    let roundtrip: Config = serde_json::from_value(serialized).unwrap();
                    assert_eq!(
                        book_binding(&decoded).unwrap(),
                        book_binding(&roundtrip).unwrap()
                    );
                    assert_ne!(book_binding(&decoded).unwrap(), book_binding(&c).unwrap());
                }
            }
        }
    }
}

#[test]
fn next_session_monday_and_weekend_idle_calendar() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(dir.path());
    c.validate().unwrap();
    let mut monday = c.clone();
    monday.session = "2026-10-19".parse().unwrap();
    monday.previous_session = "2026-10-16".parse().unwrap();
    monday.validate().unwrap();
    assert!(!c.is_trading_day("2026-10-20".parse().unwrap()).unwrap());
    assert_eq!(
        c.next_sessions("2026-10-21".parse().unwrap()).unwrap(),
        ("2026-10-21".parse().unwrap(), "2026-10-19".parse().unwrap())
    );
    let outside = c.calendar_at("2027-01-01".parse().unwrap()).unwrap_err();
    assert_eq!(
        outside.downcast_ref::<nivra_core::market::MarketError>(),
        Some(&nivra_core::market::MarketError::CalendarCoverageMissing)
    );
    let coverage = c
        .calendar_coverage_at("2026-11-14".parse().unwrap())
        .unwrap();
    assert_eq!(coverage.source, "NSE/CMTR/71775");
    assert_eq!(coverage.covered_from, "2026-01-01");
    assert_eq!(coverage.covered_to, "2026-12-31");
    assert_eq!(coverage.current_operational_date, "2026-11-14");
    assert!(coverage.covered);
    assert_eq!(coverage.days_remaining, 47);
    assert!(!coverage.expiring);
    assert!(!coverage.expired);
    assert_eq!(coverage.expiry_warning_days, 30);
    assert_eq!(
        coverage.expiry_warning_basis,
        "operator_chosen_default_not_exchange_guarantee"
    );
    assert_eq!(
        coverage.update_prerequisite,
        "verified_exchange_calendar_update_required"
    );
    assert_eq!(coverage.requested_session, c.session.to_string());
    assert!(coverage.requested_session_covered);
    assert_eq!(coverage.historical_session, c.previous_session.to_string());
    assert!(coverage.historical_session_covered);
    // Current-date coverage and the historical book session are separate facts:
    // an out-of-coverage current date must not erase a covered book session.
    let mut historical = c.clone();
    historical.previous_session = "2025-12-31".parse().unwrap();
    let historical_coverage = historical
        .calendar_coverage_at("2026-11-14".parse().unwrap())
        .unwrap();
    assert!(historical_coverage.covered);
    assert!(historical_coverage.requested_session_covered);
    assert!(!historical_coverage.historical_session_covered);
    assert_eq!(
        historical_coverage.historical_session,
        "2025-12-31".to_string()
    );
    let warning = c
        .calendar_coverage_at("2026-12-02".parse().unwrap())
        .unwrap();
    assert_eq!(warning.days_remaining, 29);
    assert!(warning.expiring);
    assert!(!warning.expired);
    let end = c
        .calendar_coverage_at("2026-12-31".parse().unwrap())
        .unwrap();
    assert_eq!(end.days_remaining, 0);
    assert!(end.expiring);
    assert!(!end.expired);
    assert!(end.covered);
    let outside_coverage = c
        .calendar_coverage_at("2027-01-01".parse().unwrap())
        .unwrap();
    assert!(!outside_coverage.covered);
    assert_eq!(outside_coverage.days_remaining, -1);
    assert!(outside_coverage.expired);
    assert!(!outside_coverage.expiring);
    assert_eq!(outside_coverage.covered_to, "2026-12-31");
    assert!(outside_coverage.requested_session_covered);
    assert!(outside_coverage.historical_session_covered);
    for today in ["2026-10-17", "2026-10-18", "2026-10-19"] {
        assert_eq!(
            c.next_sessions(today.parse().unwrap()).unwrap(),
            ("2026-10-19".parse().unwrap(), "2026-10-16".parse().unwrap())
        );
        let at = DateTime::parse_from_rfc3339(&format!("{today}T10:00:00+05:30"))
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            c.calendar_at(local(at).date_naive())
                .unwrap()
                .is_open(nivra_core::time::Timestamp::from_epoch_millis(at).unwrap())
                .unwrap(),
            today == "2026-10-19"
        );
    }
}

#[test]
fn missing_original_book_is_never_replaced() {
    let d = tempfile::tempdir().unwrap();
    let c = config(d.path());
    drop(open_book(&c).unwrap());
    fs::rename(
        c.directory.join("paper.sqlite"),
        c.directory.join("original-preserved.sqlite"),
    )
    .unwrap();
    assert!(open_book(&c).is_err());
    assert!(!c.directory.join("paper.sqlite").exists());
}
