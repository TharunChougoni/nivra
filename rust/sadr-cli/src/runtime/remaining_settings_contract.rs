use super::*;
use std::os::unix::fs::FileTypeExt;
#[test]
fn history_download_settings_defaults_and_bounds() {
    let mut c = tests::config(Path::new("/tmp/unused-history-settings"));
    let defaults: HistoryDownloadSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(
        (
            defaults.quota_wait_max_attempts,
            defaults.quota_wait_poll_ms
        ),
        (1800, 1000)
    );
    assert!(defaults.validate().is_ok());
    assert_eq!(defaults.reserve_half_hour_reads, 300);
    assert_eq!(defaults.reserve_second_reads, 2);
    assert_eq!(defaults.reserve_minute_reads, 30);
    for (second, minute) in [(5, 30), (2, 120)] {
        let mut invalid = defaults.clone();
        invalid.reserve_second_reads = second;
        invalid.reserve_minute_reads = minute;
        assert!(invalid.validate().is_err());
    }
    let mut invalid = defaults.clone();
    invalid.reserve_half_hour_reads = 1500;
    assert!(invalid.validate().is_err());
    assert!(
        serde_json::to_value(&c)
            .unwrap()
            .get("history_download")
            .is_none()
    );
    for (attempts, poll) in [(1801, 1000), (1800, 0), (1800, 60001)] {
        c.history_download = HistoryDownloadSettings {
            quota_wait_max_attempts: attempts,
            quota_wait_poll_ms: poll,
            ..HistoryDownloadSettings::default()
        };
        assert!(c.validate().is_err());
    }
    c.history_download = HistoryDownloadSettings {
        quota_wait_max_attempts: 0,
        quota_wait_poll_ms: 1,
        ..HistoryDownloadSettings::default()
    };
    assert!(c.validate().is_ok());
}
#[test]
fn dashboard_settings_defaults_and_bounds() {
    let mut c = tests::config(Path::new("/tmp/unused-dashboard-settings"));
    let defaults: DashboardSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(
        (defaults.backtest_timeout_ms, defaults.history_max_files),
        (3000, 256)
    );
    assert!(defaults.validate().is_ok());
    assert!(serde_json::to_value(&c).unwrap().get("dashboard").is_none());
    // Changing an existing key must still omit new default keys from sealed bytes.
    c.dashboard.refresh_interval_ms = 1;
    let encoded = serde_json::to_value(&c).unwrap();
    assert_eq!(
        encoded["dashboard"],
        serde_json::json!({"refresh_interval_ms":1})
    );
    for (timeout, files) in [(0, 256), (3001, 256), (3000, 0), (3000, 257)] {
        c.dashboard.backtest_timeout_ms = timeout;
        c.dashboard.history_max_files = files;
        assert!(c.validate().is_err());
    }
    c.dashboard.backtest_timeout_ms = 1;
    c.dashboard.history_max_files = 1;
    assert!(c.validate().is_ok());
}
#[test]
fn dashboard_phase1_settings_defaults_bounds_and_roundtrip() {
    let mut c = tests::config(Path::new("/tmp/unused-dashboard-phase1"));
    let defaults: DashboardSettings = serde_json::from_str("{}").unwrap();
    assert_eq!(
        (defaults.workers, defaults.queue, defaults.static_max_age_s),
        (4, 16, 86_400)
    );
    assert_eq!(
        (
            defaults.gzip_min_bytes,
            defaults.history_read_max_bytes,
            defaults.history_list_cache,
            defaults.sse_max_streams
        ),
        (1024, 8_000_000, true, 3)
    );
    assert_eq!(
        (
            defaults.sse_heartbeat_ms,
            defaults.longpoll_hold_ms,
            defaults.transcript_max_bytes
        ),
        (15_000, 25_000, 2_097_152)
    );
    assert_eq!(
        (
            defaults.transcript_text_max_bytes,
            defaults.transcript_args_max_bytes,
            defaults.blob_slice_max_bytes
        ),
        (16_384, 4096, 262_144)
    );
    assert_eq!(
        (
            defaults.series_max_points,
            defaults.series_max_range_ms,
            defaults.equity_max_points,
            defaults.equity_sample_interval_ms,
            defaults.price_batch_instruments,
            defaults.price_batch_interval_ms,
        ),
        (2_000, 86_400_000, 4_096, 15_000, 24, 2_000)
    );
    assert_eq!(defaults.retention, RetentionSettings::default());
    assert!(defaults.retention.pack_after_days.is_none());
    assert!(defaults.retention.delete_after_days.is_none());
    assert!(defaults.model_prices.is_empty());
    assert!(defaults.validate().is_ok());
    // Default settings keep existing serialized config bytes: no dashboard
    // keys appear.
    assert!(serde_json::to_value(&c).unwrap().get("dashboard").is_none());
    // Changing one key must still omit new default keys from sealed bytes.
    c.dashboard.refresh_interval_ms = 1;
    c.dashboard.workers = 8;
    assert_eq!(
        serde_json::to_value(&c).unwrap()["dashboard"],
        serde_json::json!({"refresh_interval_ms":1,"workers":8})
    );
    let check = |fragment: &str, valid: bool| {
        let decoded: std::result::Result<DashboardSettings, _> =
            serde_json::from_str(&format!("{{{fragment}}}"));
        match decoded {
            Err(_) => assert!(!valid, "{fragment}"),
            Ok(settings) => assert_eq!(settings.validate().is_ok(), valid, "{fragment}"),
        }
    };
    for (fragment, valid) in [
        ("\"workers\": 0", false),
        ("\"workers\": 17", false),
        ("\"workers\": 16", true),
        ("\"queue\": 0", false),
        ("\"queue\": 257", false),
        ("\"queue\": 256", true),
        ("\"static_max_age_s\": 31536001", false),
        ("\"static_max_age_s\": 0", true),
        ("\"gzip_min_bytes\": 1048577", false),
        ("\"gzip_min_bytes\": 0", true),
        ("\"history_read_max_bytes\": 999999", false),
        ("\"history_read_max_bytes\": 64000001", false),
        ("\"history_read_max_bytes\": 1000000", true),
        ("\"sse_write_timeout_ms\": 0", false),
        ("\"sse_write_timeout_ms\": 60001", false),
        ("\"sse_write_timeout_ms\": 1000", true),
        ("\"sse_max_streams\": 17", false),
        ("\"sse_max_streams\": 0", true),
        ("\"sse_heartbeat_ms\": 999", false),
        ("\"sse_heartbeat_ms\": 60001", false),
        ("\"sse_heartbeat_ms\": 1000", true),
        ("\"longpoll_hold_ms\": 999", false),
        ("\"longpoll_hold_ms\": 60001", false),
        ("\"longpoll_hold_ms\": 1000", true),
        ("\"transcript_max_bytes\": 65535", false),
        ("\"transcript_max_bytes\": 16777217", false),
        ("\"transcript_text_max_bytes\": 1023", false),
        ("\"transcript_text_max_bytes\": 262145", false),
        ("\"transcript_args_max_bytes\": 255", false),
        ("\"transcript_args_max_bytes\": 65537", false),
        ("\"blob_slice_max_bytes\": 4095", false),
        ("\"blob_slice_max_bytes\": 4194305", false),
        ("\"series_max_points\": 63", false),
        ("\"series_max_points\": 10001", false),
        ("\"series_max_points\": 1000", true),
        ("\"series_max_range_ms\": 59999", false),
        ("\"series_max_range_ms\": 604800001", false),
        ("\"series_max_range_ms\": 3600000", true),
        ("\"equity_max_points\": 63", false),
        ("\"equity_max_points\": 65537", false),
        ("\"equity_max_points\": 1000", true),
        ("\"equity_sample_interval_ms\": 999", false),
        ("\"equity_sample_interval_ms\": 300001", false),
        ("\"equity_sample_interval_ms\": 15000", true),
        ("\"price_batch_instruments\": 0", false),
        ("\"price_batch_instruments\": 257", false),
        ("\"price_batch_instruments\": 24", true),
        ("\"price_batch_interval_ms\": 499", false),
        ("\"price_batch_interval_ms\": 60001", false),
        ("\"price_batch_interval_ms\": 2000", true),
        ("\"retention\": {\"pack_after_days\": 0}", false),
        ("\"retention\": {\"delete_after_days\": 0}", false),
        ("\"retention\": {\"pack_after_days\": 366}", false),
        ("\"retention\": {\"delete_after_days\": 366}", false),
        (
            "\"retention\": {\"pack_after_days\": 7, \"delete_after_days\": 7}",
            false,
        ),
        (
            "\"retention\": {\"pack_after_days\": 7, \"delete_after_days\": 6}",
            false,
        ),
        (
            "\"retention\": {\"pack_after_days\": 7, \"delete_after_days\": 30}",
            true,
        ),
        ("\"retention\": {\"delete_after_days\": 30}", true),
        ("\"retention\": {\"pack_after_days\": 7}", true),
        ("\"retention\": {\"keep_full_quote_bodies\": 0}", false),
        ("\"retention\": {\"max_quote_body_bytes\": 100}", false),
        ("\"retention\": {\"max_model_file_bytes\": 10}", false),
        ("\"retention\": {\"keep_full_quote_bodies\": 32}", true),
        (
            "\"model_prices\": {\"m\": {\"input_inr_per_mtok\": \"x\", \"output_inr_per_mtok\": \"2\"}}",
            false,
        ),
        (
            "\"model_prices\": {\"m\": {\"input_inr_per_mtok\": \"0.5\", \"output_inr_per_mtok\": \"bad\"}}",
            false,
        ),
        (
            "\"model_prices\": {\"m\": {\"input_inr_per_mtok\": \"0.5\", \"output_inr_per_mtok\": \"2\", \"cached_inr_per_mtok\": \"bad\"}}",
            false,
        ),
        (
            "\"model_prices\": {\"m\": {\"input_inr_per_mtok\": \"0.5\", \"output_inr_per_mtok\": \"2\", \"cached_inr_per_mtok\": \"0.25\"}}",
            true,
        ),
        (
            "\"model_prices\": {\"m\": {\"input_inr_per_mtok\": \"0.5\", \"output_inr_per_mtok\": \"2\"}}",
            true,
        ),
        ("\"failed_auth_per_minute\": 0", false),
        ("\"failed_auth_per_minute\": 601", false),
        ("\"failed_auth_per_minute\": 1", true),
        ("\"failed_auth_per_minute\": 600", true),
        ("\"failed_auth_per_minute\": 30", true),
    ] {
        check(fragment, valid);
    }
    assert!(serde_json::from_str::<DashboardSettings>("{\"unknown_key\": 1}").is_err());
    assert!(
        serde_json::from_str::<DashboardSettings>("{\"retention\": {\"unknown\": 1}}").is_err()
    );
    let full = DashboardSettings {
        // Retired shell keys must still round-trip nondefaults unchanged:
        // dropping them would alter deployed configuration hashes.
        model_feed_unavailable_grace_ms: 7000,
        workers: 8,
        queue: 32,
        static_max_age_s: 3600,
        gzip_min_bytes: 512,
        history_read_max_bytes: 16_000_000,
        history_list_cache: false,
        sse_max_streams: 1,
        sse_heartbeat_ms: 2000,
        longpoll_hold_ms: 30_000,
        transcript_max_bytes: 4_194_304,
        transcript_text_max_bytes: 32_768,
        transcript_args_max_bytes: 8192,
        blob_slice_max_bytes: 131_072,
        failed_auth_per_minute: 45,
        retention: RetentionSettings {
            pack_after_days: Some(7),
            delete_after_days: Some(30),
            ..RetentionSettings::default()
        },
        model_prices: std::collections::BTreeMap::from([(
            "model-x".into(),
            ModelPrice {
                input_inr_per_mtok: "12.5".into(),
                output_inr_per_mtok: "50".into(),
                cached_inr_per_mtok: Some("1.25".into()),
            },
        )]),
        ..DashboardSettings::default()
    };
    assert!(full.validate().is_ok());
    let roundtrip: DashboardSettings =
        serde_json::from_str(&serde_json::to_string(&full).unwrap()).unwrap();
    assert_eq!(roundtrip, full);

    // Verify dashboard-token rotate CLI behavior and 0600 mode.
    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("test-dashboard.token");
    let cfg_path = dir.path().join("test-config.json");
    let mut test_c = c.clone();
    test_c.dashboard_token_file = token_path.clone();
    fs::write(&cfg_path, serde_json::to_string(&test_c).unwrap()).unwrap();

    // Pre-create token file with 0644 permissions to ensure rotation strictly enforces 0600
    fs::write(&token_path, "pre_existing_unrotated").unwrap();
    fs::set_permissions(&token_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
        0o644
    );

    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, Some("too_short")),
        ExitCode::FAILURE
    );
    assert_eq!(
        dashboard_token_rotate_entry(
            &cfg_path,
            Some("invalid_symbol_character_token_12345678901234567890!"),
        ),
        ExitCode::FAILURE
    );
    let valid_token = "explicitToken1234567890abcdef0123456789";
    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, Some(valid_token)),
        ExitCode::SUCCESS
    );
    let contents = fs::read_to_string(&token_path).unwrap();
    assert_eq!(contents.trim(), valid_token);
    let meta = fs::metadata(&token_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);

    // Auto-generated 32-byte hex entropy token
    let nested_token_path = dir.path().join("new-parent/more/dashboard.token");
    test_c.dashboard_token_file = nested_token_path.clone();
    fs::write(&cfg_path, serde_json::to_string(&test_c).unwrap()).unwrap();
    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, None),
        ExitCode::SUCCESS
    );
    let generated = fs::read_to_string(&nested_token_path).unwrap();
    assert_eq!(generated.trim().len(), 64);
    assert!(generated.trim().bytes().all(|b| b.is_ascii_alphanumeric()));
    let meta = fs::metadata(&nested_token_path).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    for parent in [
        nested_token_path.parent().unwrap(),
        nested_token_path.parent().unwrap().parent().unwrap(),
    ] {
        assert_eq!(
            fs::metadata(parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    test_c.dashboard_token_file = token_path.clone();
    fs::write(&cfg_path, serde_json::to_string(&test_c).unwrap()).unwrap();

    let target = dir.path().join("unrelated-target");
    fs::write(&target, "preserve permissions").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    fs::remove_file(&token_path).unwrap();
    std::os::unix::fs::symlink(&target, &token_path).unwrap();
    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, Some(valid_token)),
        ExitCode::FAILURE
    );
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o644
    );

    let hard_link_target = dir.path().join("hard-link-target");
    fs::write(&hard_link_target, "preserve linked contents").unwrap();
    fs::set_permissions(&hard_link_target, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(&token_path).unwrap();
    fs::hard_link(&hard_link_target, &token_path).unwrap();
    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, Some(valid_token)),
        ExitCode::FAILURE
    );
    assert_eq!(
        fs::read_to_string(&hard_link_target).unwrap(),
        "preserve linked contents"
    );

    fs::remove_file(&token_path).unwrap();
    let fifo_dir = fs::File::open(dir.path()).unwrap();
    rustix::fs::mkfifoat(
        &fifo_dir,
        "token-fifo",
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let fifo_path = dir.path().join("token-fifo");
    test_c.dashboard_token_file = fifo_path.clone();
    fs::write(&cfg_path, serde_json::to_string(&test_c).unwrap()).unwrap();
    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, Some(valid_token)),
        ExitCode::FAILURE
    );
    assert!(
        fs::symlink_metadata(&fifo_path)
            .unwrap()
            .file_type()
            .is_fifo()
    );

    let real_parent = dir.path().join("real-parent");
    let linked_parent = dir.path().join("linked-parent");
    fs::create_dir(&real_parent).unwrap();
    std::os::unix::fs::symlink(&real_parent, &linked_parent).unwrap();
    test_c.dashboard_token_file = linked_parent.join("dashboard.token");
    fs::write(&cfg_path, serde_json::to_string(&test_c).unwrap()).unwrap();
    assert_eq!(
        dashboard_token_rotate_entry(&cfg_path, Some(valid_token)),
        ExitCode::FAILURE
    );
    assert!(!real_parent.join("dashboard.token").exists());

    // Verify activity attribution metrics
    use super::activity_attribution::*;
    let conv = CandidateConversion::new(100, 50, 20, 15, 10);
    assert_eq!(conv.candidate_to_decision_rate, 0.5);
    assert_eq!(conv.decision_to_action_rate, 0.4);
    assert_eq!(conv.action_to_fill_rate, 0.5);
    assert_eq!(conv.overall_conversion_rate, 0.1);

    let mut abs = AbstentionTaxonomy::default();
    abs.record("model_abstain_wait");
    abs.record("insufficient_cash_paper_cash_scope");
    abs.record("stale_feed_freshness_expired");
    abs.record("spread_too_wide_cost");
    abs.record("market_halted_session");
    abs.record("universe_exclusion_filter");
    abs.record("low_confidence_score");
    abs.record("duplicate_order_pending");
    abs.record("unknown_custom_reason");
    assert_eq!(abs.total_abstentions, 9);
    assert_eq!(abs.model_abstained, 1);
    assert_eq!(abs.risk_preflight_blocked, 1);
    assert_eq!(abs.freshness_expired, 1);
    assert_eq!(abs.spread_cost_excessive, 1);
    assert_eq!(abs.market_closed_or_halted, 1);
    assert_eq!(abs.universe_filter_excluded, 1);
    assert_eq!(abs.candidate_confidence_low, 1);
    assert_eq!(abs.duplicate_or_in_flight, 1);
    assert_eq!(abs.other, 1);

    let fin = FinancialAfterCost::calculate(
        sadr_core::finance::Amount::parse("150.50").unwrap(),
        sadr_core::finance::Amount::parse("50.25").unwrap(),
        sadr_core::finance::Amount::parse("10.00").unwrap(),
        sadr_core::finance::Amount::parse("5.75").unwrap(),
    )
    .unwrap();
    assert_eq!(
        fin.gross_profit,
        sadr_core::finance::Amount::parse("200.75").unwrap()
    );
    assert_eq!(
        fin.total_cost_drag,
        sadr_core::finance::Amount::parse("15.75").unwrap()
    );
    assert_eq!(
        fin.net_after_cost_pnl,
        sadr_core::finance::Amount::parse("190.75").unwrap()
    );

    let turnover = TurnoverCostDrag::calculate(
        sadr_core::finance::Amount::parse("50000").unwrap(),
        sadr_core::finance::Amount::parse("10000").unwrap(),
        sadr_core::finance::Amount::parse("25").unwrap(),
        sadr_core::finance::Amount::parse("25").unwrap(),
    )
    .unwrap();
    assert_eq!(turnover.turnover_ratio, 5.0);
    assert_eq!(turnover.fee_drag_bps, 5.0);
    assert_eq!(turnover.slippage_drag_bps, 5.0);
    assert_eq!(turnover.total_cost_drag_bps, 10.0);

    let dd_points = vec![
        (1000, sadr_core::finance::Amount::parse("5000").unwrap()),
        (2000, sadr_core::finance::Amount::parse("5200").unwrap()),
        (3000, sadr_core::finance::Amount::parse("4940").unwrap()),
        (4000, sadr_core::finance::Amount::parse("5100").unwrap()),
    ];
    let dd = DrawdownTracker::from_series(&dd_points).unwrap();
    assert_eq!(
        dd.high_water_mark,
        sadr_core::finance::Amount::parse("5200").unwrap()
    );
    assert_eq!(
        dd.max_drawdown,
        sadr_core::finance::Amount::parse("260").unwrap()
    );
    assert_eq!(dd.max_drawdown_duration_ms, 1000);
    assert!((dd.max_drawdown_pct - 5.0).abs() < 1e-6);

    let latency = DecisionLatencyStats::from_latencies(&[200, 450, 750, 1500, 3200, 6000, 12000]);
    assert_eq!(latency.count, 7);
    assert_eq!(latency.min_ms, 200);
    assert_eq!(latency.max_ms, 12000);
    assert_eq!(latency.buckets.get("<500ms"), Some(&2));

    let report = ActivityReport {
        session: "2026-10-28".into(),
        conversion: conv,
        abstentions: abs,
        financials: fin,
        turnover,
        drawdown: dd,
        latency,
        experiment_attributions: vec![
            ExperimentAttribution::new(
                "hash123".into(),
                Some("spec1".into()),
                Some("top_score".into()),
                5,
                2,
                sadr_core::finance::Amount::parse("10000").unwrap(),
                sadr_core::finance::Amount::parse("100").unwrap(),
                sadr_core::finance::Amount::ZERO,
                sadr_core::finance::Amount::parse("10").unwrap(),
                Some("promoted".into()),
            )
            .unwrap(),
        ],
    };
    let rep_json = report.to_json();
    assert_eq!(rep_json["session"], "2026-10-28");
    assert_eq!(
        rep_json["experiment_attributions"][0]["source_hash"],
        "hash123"
    );
}
