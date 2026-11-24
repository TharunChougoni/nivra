use super::*;
#[test]
fn shadow_inputs_use_receipt_minutes_and_skip_missing_references() {
    assert_eq!(
        super::controller::research_context_status(true)["reason"],
        "controller_session_end_unavailable"
    );
    assert_eq!(
        super::controller::research_context_status(true)["action"],
        "none"
    );
    assert_eq!(
        super::controller::research_context_status(false)["status"],
        "available"
    );
    let now = start();
    let mut scan = Scanner::default();
    let mut sample = nivra_core::execution::Sample {
        seen: now,
        current: Some(Price::parse("110").unwrap()),
        regime_references: Some((
            now,
            Price::parse("100").unwrap(),
            Price::parse("105").unwrap(),
        )),
        ..Default::default()
    };
    scan.samples.insert("available".into(), sample.clone());
    sample.regime_references = None;
    scan.samples.insert("missing_open".into(), sample);
    let inputs = shadow_regime_inputs(&scan, now, 2000, &[]);
    assert_eq!(inputs.instrument_count, 1);
    assert_eq!(inputs.minute_count, 0);
    assert!((inputs.opening_gap - 0.05).abs() < 1e-12);
    assert!((inputs.session_return - (110.0 / 105.0 - 1.0)).abs() < 1e-12);
    assert_eq!(
        nivra_core::regime::classify(&inputs, &Default::default()),
        nivra_core::regime::Regime::Unknown
    );
    assert_eq!(
        shadow_regime_inputs(&scan, now + 2001, 2000, &[]).instrument_count,
        0
    );
    let settings: super::discovery::Settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "feature_store":{"max_bars":3,"min_instruments":1}
    }))
    .unwrap();
    let mut store = FeatureStore::new(settings.feature_store.clone()).unwrap();
    store.reset_for_session(local(now).date_naive());
    let instrument = Instrument {
        key: "available".into(),
        symbol: "AVAILABLE".into(),
        tick: Price::parse("0.05").unwrap(),
        series: "EQ".into(),
    };
    // REST scan path uses receipt, even when processing occurs in a later minute.
    for (offset, price) in [(0, 100), (60_000, 110), (120_000, 121)] {
        let rows = serde_json::from_value(serde_json::json!({"row":{
            "instrument_token":"available", "symbol":"AVAILABLE",
            "last_price":price, "timestamp": now + offset
        }}))
        .unwrap();
        scan_rest_rows(
            &mut scan,
            std::slice::from_ref(&instrument),
            &rows,
            now + offset,
            now + offset + 60_000,
            120_000,
            false,
            Some(&mut store),
        )
        .unwrap();
    }
    let minutes = store.market_minute_returns(settings.feature_store.min_instruments);
    assert_eq!(
        shadow_regime_inputs(&scan, now + 120_000, 2000, &minutes).minute_count,
        1
    );
    assert_eq!(store.bars("available").unwrap()[0].start_minute, 555);
    assert_eq!(store.status()["bars_total"], 3);
    let points = BTreeMap::from([(
        instrument.key.clone(),
        super::super::market_feed::Point {
            price: Price::parse("130").unwrap(),
            source: now + 180_000,
            receipt: now + 180_000,
            last_trade: now + 180_000,
            prior_close: None,
        },
    )]);
    super::super::market_feed::observe_cache(
        &mut scan,
        std::slice::from_ref(&instrument),
        &points,
        &Default::default(),
        now + 240_000,
        Some(now + 240_000),
        Some(&mut store),
    )
    .unwrap();
    assert_eq!(
        store
            .bars("available")
            .unwrap()
            .back()
            .unwrap()
            .start_minute,
        558
    );
    assert_eq!(store.market_minute_returns(1).len(), 1); // cap retains three bars
    assert_eq!(store.status()["completed_minutes"], 1);
    let defaults: super::discovery::Settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,"feature_store":{}
    }))
    .unwrap();
    assert_eq!(defaults.feature_store, Default::default());
    for invalid in [
        serde_json::json!({"max_bars":1}),
        serde_json::json!({"min_instruments":0}),
    ] {
        let cfg: super::discovery::Settings = serde_json::from_value(serde_json::json!({
            "max_scan_age_ms":2000,"max_prospects":10,"feature_store":invalid
        }))
        .unwrap();
        assert!(cfg.validate().is_err());
    }
    store.reset_for_session(local(now).date_naive());
    assert_eq!(store.status()["instruments"], 0);
    assert!(store.market_minute_returns(1).is_empty());
    let settings =
        serde_json::from_value(serde_json::json!({"max_scan_age_ms":2000,"max_prospects":10}))
            .unwrap();
    let before = super::super::discovery::project(&scan, &[], &settings, now);
    scan.samples.get_mut("available").unwrap().regime_references = None;
    assert_eq!(
        super::super::discovery::project(&scan, &[], &settings, now),
        before
    );

    // Non-tradeable index context reads the same feed points; it never enters
    // the cash universe, scanner screens or paper execution.
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(dir.path());
    c.quote_keys.prefixes.push("NSE_INDEX|".into());
    c.index_feeds.enabled = true;
    c.index_feeds.regime = Some(nivra_core::regime::RegimeConfig {
        min_instruments: 3,
        min_minutes: 2,
        ..nivra_core::regime::RegimeConfig::default()
    });
    c.validate().unwrap();
    let index_at = now + 300_000;
    let mut index_store = FeatureStore::new(Default::default()).unwrap();
    index_store.reset_for_session(local(index_at).date_naive());
    let mut index_points = BTreeMap::new();
    for (n, key) in c.index_feeds.keys.iter().enumerate() {
        let prior = 24_000.0 + n as f64 * 1_000.0;
        let price = prior * 1.01;
        for minute in 555..=560 {
            index_store
                .observe(
                    key,
                    price + f64::from(minute - 555),
                    now + i64::from(minute - 555) * 60_000,
                )
                .unwrap();
        }
        index_points.insert(
            key.clone(),
            super::super::market_feed::Point {
                price: Price::parse(&format!("{price}")).unwrap(),
                source: index_at,
                receipt: index_at,
                last_trade: index_at,
                prior_close: Some(Price::parse(&format!("{prior}")).unwrap()),
            },
        );
    }
    let context = IndexContext::capture(
        &c,
        &index_points,
        &index_store,
        &Default::default(),
        index_at,
        Some(index_at),
    );
    let json = context.json();
    assert_eq!(json["paper_only"], true);
    assert_eq!(json["tradeable"], false);
    let observations = json["observations"].as_array().unwrap();
    assert_eq!(observations.len(), 3);
    let nifty = observations
        .iter()
        .find(|observation| observation["instrument_key"] == "NSE_INDEX|Nifty 50")
        .unwrap();
    assert!(
        (nifty["change_bps"].as_f64().unwrap() - 100.0).abs() < 1e-6,
        "index change is measured against the provider prior close"
    );
    assert_eq!(nifty["fresh"], true);
    assert_eq!(json["regime"]["advisory_only"], true);
    let label = json["regime"]["label"].as_str().unwrap();
    assert_ne!(label, "unknown");
    assert!(label.ends_with("gap"));
    // Missing points stay missing; no price or regime is invented.
    let empty = IndexContext::capture(
        &c,
        &BTreeMap::new(),
        &index_store,
        &Default::default(),
        index_at,
        Some(index_at),
    )
    .json();
    assert!(empty["observations"].as_array().unwrap().is_empty());
    assert!(empty["regime"].is_null());
}

#[test]
fn shadow_scorer_is_disabled_without_io_and_failure_preserves_live_projection() {
    let dir = tempfile::tempdir().unwrap();
    let scorer = dir.path().join("scorer.py");
    let params = dir.path().join("params.json");
    let mut settings: super::super::discovery::Settings =
        serde_json::from_value(serde_json::json!({
            "max_scan_age_ms":2000,"max_prospects":10,
            "shadow_scorer":{"scorer_path":scorer,"params_path":params},
            "scorer_sandbox":{"bwrap_path":"/nonexistent/nivra-bwrap"}
        }))
        .unwrap();
    settings.validate().unwrap();
    let cfg = settings.shadow_scorer.as_ref().unwrap();
    assert_eq!(cfg.evaluate_ms, 60000);
    assert_eq!(cfg.top_n, 10);
    assert!(!cfg.shortlist_only);
    let now = start();
    let instruments = vec![instrument(1)];
    let mut scan = Scanner::default();
    scan.samples.insert(
        instruments[0].key.clone(),
        nivra_core::execution::Sample {
            seen: now,
            source: Some(now),
            current: Some(Price::parse("110").unwrap()),
            history: vec![Price::parse("100").unwrap(), Price::parse("110").unwrap()],
            market_fields: Some(nivra_core::execution::MarketFields {
                lower_circuit_limit: a("50"),
                upper_circuit_limit: a("150"),
                volume: 100000,
                session_vwap: None,
                best_bid: None,
                best_ask: None,
                best_bid_qty: None,
                best_ask_qty: None,
                total_buy_quantity: None,
                total_sell_quantity: None,
            }),
            market_fields_receipt: Some(now),
            ..Default::default()
        },
    );

    scan.ranking
        .push((a("12"), instruments[0].key.clone(), now));
    let ranking = scan.ranking.clone();
    let projection = super::super::discovery::project(&scan, &instruments, &settings, now);
    assert!(!super::super::discovery::candidates(&projection).is_empty());
    let mut shadow = ShadowScorer {
        synchronous: true,
        ..Default::default()
    };
    shadow.evaluate(&settings, &scan, &instruments, now, None);
    let mut status = Some(projection.clone());
    shadow.attach(&mut status);
    assert_eq!(status.as_ref().unwrap(), &projection);
    assert!(status.unwrap().get("shadow_scorer").is_none());
    assert!(shadow.last_evaluation.is_none());
    assert!(!scorer.exists() && !params.exists());
    std::fs::write(&scorer, "print('{}')").unwrap();
    std::fs::write(&params, "{}").unwrap();
    settings.shadow_scorer.as_mut().unwrap().enabled = true;
    settings.validate().unwrap();
    shadow.evaluate(&settings, &scan, &instruments, now, None);
    assert_eq!(
        shadow.status.as_ref().unwrap()["fallback_reason"],
        "unavailable"
    );
    assert_eq!(shadow.status.as_ref().unwrap()["ineligible_count"], 1);
    let mut status = Some(projection.clone());
    shadow.attach(&mut status);
    assert!(
        status
            .as_mut()
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("shadow_scorer")
            .is_some()
    );
    assert_eq!(status.unwrap(), projection);
    assert_eq!(
        super::super::discovery::project(&scan, &instruments, &settings, now),
        projection
    );
    assert_eq!(scan.ranking, ranking);
    shadow.evaluate(&settings, &scan, &instruments, now + 59999, None);
    assert_eq!(shadow.last_evaluation, Some(now));
    shadow.evaluate(&settings, &scan, &instruments, now + 60000, None);
    assert_eq!(shadow.last_evaluation, Some(now + 60000));
    // An unfinished worker blocks launches even past the interval; polling is nonblocking.
    let (sender, receiver) = std::sync::mpsc::channel();
    shadow.pending = Some(receiver);
    shadow.poll();
    shadow.evaluate(&settings, &scan, &instruments, now + 120000, None);
    assert_eq!(shadow.last_evaluation, Some(now + 60000));
    let completed = serde_json::json!({"as_of_ms":now,"top":[],"ineligible_count":1});
    sender.send(completed.clone()).unwrap();
    shadow.poll();
    assert_eq!(shadow.status, Some(completed));
    assert!(shadow.pending.is_none());
    // Session reset drops the old receiver without waiting for its worker.
    let (sender, receiver) = std::sync::mpsc::channel();
    shadow.pending = Some(receiver);
    shadow = ShadowScorer::default();
    assert!(sender.send(serde_json::json!({"as_of_ms":now})).is_err());
    shadow.poll();
    assert!(shadow.status.is_none());
    // Restrict actual extracted sandbox rows only for this step's enabled prefilter.
    let mut two_instruments = instruments.clone();
    two_instruments.push(instrument(2));
    scan.samples.insert(
        two_instruments[1].key.clone(),
        scan.samples[&instruments[0].key].clone(),
    );
    settings.shadow_scorer.as_mut().unwrap().shortlist_only = true;
    settings.prefilter.enabled = true;
    let keys = BTreeSet::from([instruments[0].key.clone()]);
    let mut shadow = ShadowScorer {
        synchronous: true,
        ..Default::default()
    };
    shadow.evaluate(&settings, &scan, &two_instruments, now, Some(&keys));
    let status = shadow.status.as_ref().unwrap();
    assert_eq!(status["comparison"]["input_rows"], 1);
    assert_eq!(status["comparison"]["overlap_top_n"], 0);
    assert_eq!(
        status["comparison"]["default_top"],
        serde_json::json!([instruments[0].key, two_instruments[1].key])
    );
    assert_eq!(status["fallback_reason"], "unavailable");
    let all_features = nivra_sandbox::Features::from([
        ("A".into(), serde_json::json!({"move_bps":1})),
        ("B".into(), serde_json::json!({"move_bps":2})),
    ]);
    let mut features = all_features.clone();
    ShadowScorer::restrict_input(&mut features, Some(&BTreeSet::from(["B".into()])));
    assert_eq!(
        features,
        nivra_sandbox::Features::from([("B".into(), all_features["B"].clone())])
    );
    assert_eq!(
        ShadowScorer::comparison(
            &serde_json::json!([{"instrument_key":"B"},{"instrument_key":"C"}]),
            &["A".into(), "B".into()],
            2
        ),
        serde_json::json!({"overlap_top_n":1,"default_top":["A","B"],"input_rows":2})
    );
    for (enabled, shortlist, expected) in [
        (true, None, 2),
        (true, Some(BTreeSet::new()), 0),
        (false, Some(keys), 2),
    ] {
        settings.prefilter.enabled = enabled;
        shadow.last_evaluation = None;
        shadow.evaluate(&settings, &scan, &two_instruments, now, shortlist.as_ref());
        assert_eq!(
            shadow.status.as_ref().unwrap()["comparison"]["input_rows"],
            expected
        );
    }
    for invalid in [
        serde_json::json!({"enabled":true}),
        serde_json::json!({"evaluate_ms":0}),
        serde_json::json!({"top_n":0}),
    ] {
        let mut bad = serde_json::to_value(&settings).unwrap();
        bad["shadow_scorer"] = invalid;
        assert!(
            serde_json::from_value::<super::super::discovery::Settings>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn disabled_shadow_selection_does_not_open_library_or_add_status() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-exist.sqlite");
    let settings: super::super::discovery::Settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "strategy_selection":{"enabled":false,"library_path":path}
    }))
    .unwrap();
    settings.validate().unwrap();
    assert_eq!(
        settings.strategy_selection.as_ref().unwrap().evaluate_ms,
        60_000
    );
    let scan = Scanner::default();
    let mut shadow = ShadowSelection::default();
    shadow.evaluate(&settings, &scan, start(), false, &[]);
    let projection = super::super::discovery::project(&scan, &[], &settings, start());
    let mut status = Some(projection.clone());
    shadow.attach(&mut status);
    assert_eq!(status.as_ref().unwrap(), &projection);
    assert!(status.unwrap().get("strategy_selection").is_none());
    assert!(!path.exists());
    assert!(shadow.last_evaluation.is_none());
    for invalid in [
        serde_json::json!({"enabled":true}),
        serde_json::json!({"evaluate_ms":0}),
        serde_json::json!({"regime":{"min_minutes":0}}),
        serde_json::json!({"selector":{"min_trades":0}}),
    ] {
        let mut bad = serde_json::to_value(&settings).unwrap();
        bad["strategy_selection"] = invalid;
        assert!(
            serde_json::from_value::<super::super::discovery::Settings>(bad)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn shadow_library_failure_and_interval_are_diagnostic_only() {
    let dir = tempfile::tempdir().unwrap();
    let settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "strategy_selection":{"enabled":true,"library_path":dir.path(),"evaluate_ms":60000}
    }))
    .unwrap();
    let mut shadow = ShadowSelection::default();
    let scan = Scanner::default();
    let now = start();
    shadow.evaluate(&settings, &scan, now, false, &[]);
    assert_eq!(shadow.status.as_ref().unwrap()["reason"], "ScorerFailure");
    assert_eq!(shadow.status.as_ref().unwrap()["chosen_id"], "default");
    shadow.evaluate(&settings, &scan, now + 59999, true, &[]);
    assert_eq!(shadow.last_evaluation, Some(now));
    shadow.evaluate(&settings, &scan, now + 60000, true, &[]);
    assert_eq!(shadow.last_evaluation, Some(now + 60000));
    assert_eq!(shadow.status.as_ref().unwrap()["reason"], "StaleData");
    assert!(scan.samples.is_empty());
    assert!(scan.ranking.is_empty());
}

#[test]
fn restart_metadata_uses_original_receipt_and_configured_age() {
    let db = Connection::open_in_memory().unwrap();
    let instrument = instrument(1);
    let receipt = start();
    let mut original = Scanner::default();
    let fields = nivra_core::execution::MarketFields {
        session_vwap: None,
        best_bid: None,
        best_bid_qty: None,
        best_ask: None,
        best_ask_qty: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        lower_circuit_limit: a("50"),
        upper_circuit_limit: a("150"),
        volume: 100000,
    };
    original
        .observe_fields(receipt, &instrument, &quote(receipt, 100), &fields)
        .unwrap();
    checkpoint_scanner(&db, &original).unwrap();
    let now = receipt + 600000;
    for (limit, usable) in [(900000, true), (599999, false), (0, false)] {
        let mut restored = load_scanner(&db, limit, now, &Default::default()).unwrap();
        assert_eq!(restored.samples[&instrument.key].metadata_preloaded, usable);
        let rows = serde_json::json!({"row": {"instrument_token": instrument.key, "symbol": instrument.symbol}});
        scan_rest_rows(
            &mut restored,
            std::slice::from_ref(&instrument),
            rows.as_object().unwrap(),
            now,
            now,
            2000,
            true,
            None,
        )
        .unwrap();
        let points = std::collections::BTreeMap::from([(
            instrument.key.clone(),
            super::super::market_feed::Point {
                price: Price::parse("100").unwrap(),
                source: now,
                receipt: now,
                last_trade: now,
                prior_close: None,
            },
        )]);
        super::super::market_feed::observe_cache(
            &mut restored,
            std::slice::from_ref(&instrument),
            &points,
            &super::super::market_feed::Settings {
                metadata_preload_max_age_ms: limit,
                ..Default::default()
            },
            now,
            Some(now),
            None,
        )
        .unwrap();
        let sample = &restored.samples[&instrument.key];
        assert_eq!(sample.market_fields_receipt, usable.then_some(receipt));
        assert_eq!(
            sample.reason,
            if usable {
                "eligible_no_signal"
            } else {
                "missing_market_fields"
            }
        );
    }
}

#[test]
fn dated_session_evidence_is_built_from_the_successor_policy() {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open_in_memory().unwrap();
    let instrument = instrument(1);
    let receipt = super::super::millis() - 600_000;
    let mut restored = Scanner::default();
    let fields = nivra_core::execution::MarketFields {
        session_vwap: None,
        best_bid: None,
        best_bid_qty: None,
        best_ask: None,
        best_ask_qty: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        lower_circuit_limit: a("50"),
        upper_circuit_limit: a("150"),
        volume: 100000,
    };
    restored
        .observe_fields(receipt, &instrument, &quote(receipt, 100), &fields)
        .unwrap();
    checkpoint_scanner(&db, &restored).unwrap();
    let policy = |metadata_preload_max_age_ms: i64| crate::runtime::planner::PlannerConfig {
        discovery: Some(
            serde_json::from_value(serde_json::json!({
                "max_scan_age_ms":2000,"max_prospects":10,
                "market_feed":{"metadata_preload_max_age_ms":metadata_preload_max_age_ms}
            }))
            .unwrap(),
        ),
        ..Default::default()
    };
    let mut next = super::super::tests::config(dir.path());
    next.session = next.session.succ_opt().unwrap();
    let evidence =
        super::controller::SessionEvidence::load(&db, &next, Some(&policy(900_000))).unwrap();
    // Each dated book owns its stores, and its scanner preload follows that
    // book's policy instead of the session it replaced.
    assert_eq!(evidence.feature_store.session(), Some(next.session));
    assert_eq!(evidence.index_features.session(), Some(next.session));
    assert!(evidence.scan.samples[&instrument.key].metadata_preloaded);
    assert_eq!(evidence.option_status["status"], "disabled");
    let tight = super::controller::SessionEvidence::load(&db, &next, Some(&policy(0))).unwrap();
    assert!(!tight.scan.samples[&instrument.key].metadata_preloaded);
    let mut with_options = next.clone();
    with_options.nifty_options.enabled = true;
    let option_evidence =
        super::controller::SessionEvidence::load(&db, &with_options, None).unwrap();
    assert_eq!(option_evidence.option_status["status"], "waiting_for_spot");
}

#[test]
fn fresh_drop_requires_strict_threshold_and_nonzero_previous() {
    assert!(!scanner_fresh_drop(100, 50, 0.5));
    assert!(scanner_fresh_drop(100, 49, 0.5));
    assert!(!scanner_fresh_drop(100, 51, 0.5));
    assert!(!scanner_fresh_drop(0, 0, 0.5));
    assert!(!scanner_fresh_drop(0, 10, 0.5));
    assert!(!scanner_fresh_drop(100, 101, 0.5));
    assert!(!scanner_fresh_drop(100, 100, 0.5));
    assert!(scanner_fresh_drop(100, 0, 0.5));
    assert!(!scanner_fresh_drop(100, 0, 1.0));
    let open_projection = serde_json::json!({"connected":false,"state":"rest_fallback"});
    assert!(!scanner_fresh_drop_feed_ready(&open_projection));
    assert!(!scanner_fresh_drop_feed_ready(
        &serde_json::json!({"connected":true,"state":"rest_fallback"})
    ));
    let streaming = serde_json::json!({"connected":true,"state":"streaming"});
    assert!(scanner_fresh_drop_feed_ready(&streaming));
    assert!(scanner_fresh_drop(2696, 200, 0.5));
    assert!(scanner_fresh_drop_feed_ready(&streaming) && scanner_fresh_drop(2696, 200, 0.5));
}

fn live_scan(now: i64) -> (Scanner, Vec<Instrument>) {
    use nivra_core::execution::{MarketFields, Sample};
    let mut scan = Scanner::default();
    let mut instruments = Vec::new();
    for (key, price) in [("A", "110"), ("B", "120"), ("C", "105")] {
        instruments.push(Instrument {
            key: key.into(),
            symbol: key.into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        });
        let current = Price::parse(price).unwrap();
        scan.samples.insert(
            key.into(),
            Sample {
                seen: now,
                source: Some(now),
                last: now,
                current: Some(current),
                history: vec![Price::parse("100").unwrap(), current],
                reason: "eligible_no_signal".into(),
                market_fields: Some(MarketFields {
                    session_vwap: Some(100.0),
                    best_bid: Some(99.0),
                    best_bid_qty: Some(10),
                    best_ask: Some(101.0),
                    best_ask_qty: Some(20),
                    total_buy_quantity: Some(100),
                    total_sell_quantity: Some(200),
                    volume: 100_000,
                    lower_circuit_limit: Amount::parse("1").unwrap(),
                    upper_circuit_limit: Amount::parse("200").unwrap(),
                }),
                market_fields_receipt: Some(now),
                regime_references: Some((
                    now,
                    Price::parse("100").unwrap(),
                    Price::parse("100").unwrap(),
                )),
                ..Default::default()
            },
        );
    }
    (scan, instruments)
}
fn live_label(scan: &Scanner, now: i64) -> String {
    let cfg = nivra_core::regime::RegimeConfig {
        min_instruments: 1,
        min_minutes: 2,
        ..Default::default()
    };
    nivra_core::regime::classify(
        &shadow_regime_inputs(scan, now, 2000, &[0.001, 0.001]),
        &cfg,
    )
    .label()
}
fn live_settings(path: &std::path::Path) -> super::discovery::Settings {
    serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "strategy_selection":{"enabled":true,"mode":"live","library_path":path,
            "regime":{"min_instruments":1,"min_minutes":2}}
    }))
    .unwrap()
}
fn add_live_version(
    path: &std::path::Path,
    source: &str,
    inputs: serde_json::Value,
    label: &str,
) -> nivra_sandbox::library::Version {
    use nivra_sandbox::library::{Library, NewVersion, RunResult};
    let mut db = Library::open(path).unwrap();
    let version = db
        .add_version(NewVersion {
            name: "live-candidate",
            file_body: source,
            author_model: "fixture-model",
            intent: "fixture live ranking",
            inputs: &inputs,
            created_at_ms: 1,
        })
        .unwrap();
    for kind in ["replay", "shadow"] {
        for day in 1..=5 {
            db.record_result(&RunResult {
                sha256: version.sha256.clone(),
                run_kind: kind.into(),
                session_date: format!("2026-10-{day:02}"),
                regime: Some(label.into()),
                metrics: serde_json::json!({"trades":6,"net_pnl_after_costs":1.0,
                        "expectancy":0.5,"max_drawdown":0.0}),
                recorded_at_ms: day,
            })
            .unwrap();
        }
    }
    version
}

#[test]
fn strategy_selection_mode_defaults_parses_and_rejects_unsafe_live() {
    let mut settings: super::discovery::Settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "strategy_selection":{"enabled":false}
    }))
    .unwrap();
    assert_eq!(
        settings.strategy_selection.as_ref().unwrap().mode,
        super::discovery::StrategySelectionMode::Shadow
    );
    settings.validate().unwrap();
    for invalid in [
        serde_json::json!({"enabled":false,"mode":"live"}),
        serde_json::json!({"enabled":true,"mode":"live"}),
    ] {
        let mut bad = serde_json::to_value(&settings).unwrap();
        bad["strategy_selection"] = invalid;
        let bad: super::discovery::Settings = serde_json::from_value(bad).unwrap();
        assert!(bad.validate().is_err());
    }
    let mut bad = serde_json::to_value(&settings).unwrap();
    bad["strategy_selection"]["mode"] = serde_json::json!("unexpected");
    assert!(serde_json::from_value::<super::discovery::Settings>(bad).is_err());
    // Shadow mode keeps the mode field out of serialized configuration; live
    // mode must serialize it so the policy hash can bind the behaviour change.
    let serialized = serde_json::to_value(&settings).unwrap();
    assert!(serialized["strategy_selection"].get("mode").is_none());
    settings.strategy_selection.as_mut().unwrap().mode =
        super::discovery::StrategySelectionMode::Live;
    settings.strategy_selection.as_mut().unwrap().enabled = true;
    settings.strategy_selection.as_mut().unwrap().library_path = "/tmp/library".into();
    let serialized = serde_json::to_value(&settings).unwrap();
    assert_eq!(serialized["strategy_selection"]["mode"], "live");
    settings.validate().unwrap();
    let mut champion = settings.clone();
    champion.strategy = Some(
        serde_json::from_value(serde_json::json!({
            "enabled":true,"champion":"c","specs":[]
        }))
        .unwrap(),
    );
    assert!(champion.validate().is_err());
}

#[test]
fn live_selection_scores_reorder_filter_and_change_candidates() {
    if !std::path::Path::new("/usr/bin/bwrap").is_file() {
        eprintln!("skipping live ranking fixture: bwrap unavailable");
        return;
    }
    let now = start();
    let (scan, instruments) = live_scan(now);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(
        &path,
        "print('{\"A\": 1, \"C\": 9}')",
        serde_json::json!({}),
        &label,
    );
    let settings = live_settings(&path);
    settings.validate().unwrap();
    let default = super::super::discovery::project(&scan, &instruments, &settings, now);
    let default_keys = default["prospects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["instrument_key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(default_keys.contains(&"B"));
    let mut observer = ShadowSelection::default();
    observer.evaluate(&settings, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(observer.status.as_ref().unwrap()["reason"], "BestEvidence");
    assert_eq!(observer.state.active, version.sha256);
    let live = observer
        .live_ranking(&settings, &scan, &instruments, &default, now)
        .expect("live scores");
    assert_eq!(live.scores.len(), 2);
    let ranked = super::super::discovery::project_with_live(
        &scan,
        &instruments,
        &settings,
        now,
        Some(&live),
    );
    let keys = ranked["prospects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["instrument_key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(keys, ["C", "A"]);
    assert_eq!(ranked["eligible_count"], 2);
    assert_eq!(ranked["live_ranking"]["sha256"], version.sha256);
    assert_eq!(ranked["live_ranking"]["name"], "live-candidate");
    let default_candidates = super::super::discovery::candidates(&default);
    let live_candidates = super::super::discovery::candidates(&ranked);
    assert_ne!(default_candidates, live_candidates);
    assert_eq!(
        live_candidates
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["C", "A"]
    );
    let mut limited = settings.clone();
    limited.max_prospects = 1;
    let ranked =
        super::super::discovery::project_with_live(&scan, &instruments, &limited, now, Some(&live));
    assert_eq!(ranked["prospects"].as_array().unwrap().len(), 1);
    assert_eq!(ranked["prospects"][0]["instrument_key"], "C");
    let status = observer.status.as_ref().unwrap();
    assert_eq!(status["mode"], "live");
    assert_eq!(status["shadow_only"], false);
    assert_eq!(status["live_applied"], true);
    assert_eq!(status["live_scored_count"], 2);
    assert_eq!(status["chosen_name"], "live-candidate");
    assert_eq!(status["previous_active"], "default");
    assert!(status["recent"].as_array().unwrap().len() <= 8);
}

#[test]
fn reviewed_research_ranker_scores_from_the_registered_source() {
    use crate::runtime::workspace::{self, WorkspaceOp, WorkspaceScope};

    let dir = tempfile::tempdir().unwrap();
    let book = dir.path();
    let library = book.join("library.sqlite");
    let planner_path = book.join("planner.json");
    let now = start();
    let session = local(now).date_naive();
    let (scan, instruments) = live_scan(now);
    let mut discovery = live_settings(&library);
    let cfg = crate::runtime::planner::PlannerConfig {
        discovery: Some(discovery.clone()),
        workspace: Some(serde_json::from_value(serde_json::json!({"enabled":true})).unwrap()),
        ..Default::default()
    };
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let workspace = cfg.workspace.as_ref().unwrap();
    let script = "scripts/ranker.py";
    let write = |source: &str| {
        workspace::execute_scoped(
            book,
            workspace,
            WorkspaceScope::ResearchSandbox,
            WorkspaceOp::Write {
                path: script.into(),
                text: source.into(),
            },
        )
        .unwrap();
    };
    let source = "print('{\"A\": 1, \"C\": 9}')\n";
    write(source);
    let sha = digest(source.as_bytes());
    let register = |expected_sha256: String| {
        planner::entry(planner::Command::RegisterRanker {
            planner: planner_path.clone(),
            session,
            book: book.to_path_buf(),
            script: script.into(),
            expected_sha256,
            author_model: "fixture-model".into(),
        })
    };
    assert_eq!(register("0".repeat(64)), std::process::ExitCode::FAILURE);
    assert!(!library.exists());
    assert_eq!(register(sha.clone()), std::process::ExitCode::SUCCESS);

    // Registering evidence alone leaves the built-in ranking in control.
    let baseline = super::super::discovery::project(&scan, &instruments, &discovery, now);
    let mut observer = ShadowSelection::default();
    observer.evaluate(&discovery, &scan, now, false, &[0.001, 0.001]);
    assert!(
        observer
            .live_ranking(&discovery, &scan, &instruments, &baseline, now)
            .is_none()
    );

    // A subsequent workspace edit cannot change the reviewed source.
    write("print('{}')\n");
    discovery
        .strategy_selection
        .as_mut()
        .unwrap()
        .paper_trial_sha256 = Some(sha.clone());
    discovery.validate().unwrap();
    let mut observer = ShadowSelection::default();
    observer.evaluate(&discovery, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(
        observer.status.as_ref().unwrap()["reason"],
        "ReviewedPaperTrial"
    );
    if !Path::new("/usr/bin/bwrap").is_file() {
        return;
    }
    let live = observer
        .live_ranking(&discovery, &scan, &instruments, &baseline, now)
        .unwrap();
    let ranked = super::super::discovery::project_with_live(
        &scan,
        &instruments,
        &discovery,
        now,
        Some(&live),
    );
    let prospects = super::super::discovery::candidates(&ranked);
    assert_eq!(
        prospects
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["C", "A"]
    );
    assert_eq!(ranked["live_ranking"]["sha256"], sha);
}

#[test]
fn shadow_selection_never_scores_or_requests_live_ranking() {
    let now = start();
    let (scan, instruments) = live_scan(now);
    let mut settings: super::discovery::Settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "strategy_selection":{"enabled":true,"mode":"shadow","library_path":"/missing/library"}
    }))
    .unwrap();
    settings.validate().unwrap();
    let projection = super::super::discovery::project(&scan, &instruments, &settings, now);
    let mut observer = ShadowSelection {
        mode: super::discovery::StrategySelectionMode::Shadow,
        state: nivra_sandbox::selector::SelectorState {
            active: "ab".repeat(32),
            last_switch_ms: None,
        },
        chosen_name: Some("candidate".into()),
        status: Some(serde_json::json!({"regime":"range"})),
        ..Default::default()
    };
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    assert!(observer.live.is_none());
    assert!(observer.source.is_none());
    assert!(observer.fallback.is_none());
    assert!(observer.cached_live().is_none());
    settings.strategy_selection = None;
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    let plain = super::super::discovery::project(&scan, &instruments, &settings, now);
    assert_eq!(
        serde_json::to_value(super::super::discovery::project_with_live(
            &scan,
            &instruments,
            &settings,
            now,
            None
        ))
        .unwrap(),
        serde_json::to_value(&plain).unwrap()
    );
}

#[test]
fn live_fallbacks_cover_selection_sandbox_and_source_failures() {
    use nivra_sandbox::library::{Library, RunResult};
    let now = start();
    let (scan, instruments) = live_scan(now);
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent.sqlite");
    let mut observer = ShadowSelection::default();
    let settings = live_settings(&absent);
    let projection = super::super::discovery::project(&scan, &instruments, &settings, now);
    // Missing library is a selector/scorer failure, never an initialization write.
    observer.evaluate(&settings, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(observer.status.as_ref().unwrap()["reason"], "ScorerFailure");
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "library_unavailable"
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["reason"],
        "ScorerFailure"
    );
    assert!(!absent.exists());
    // Missing exact source by SHA.
    let empty = dir.path().join("empty.sqlite");
    Library::open(&empty).unwrap();
    let settings = live_settings(&empty);
    observer.status = Some(serde_json::json!({"regime":"range"}));
    observer.state.active = "ab".repeat(32);
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "missing_source"
    );
    // Sandbox setup failure is typed and never falls back to direct execution.
    let path = dir.path().join("sandbox-failure.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(&path, "print('{\"A\": 1}')", serde_json::json!({}), &label);
    let mut settings = live_settings(&path);
    settings.scorer_sandbox = Some(nivra_sandbox::ScorerSandboxConfig {
        bwrap_path: "/nonexistent/nivra-bwrap".into(),
        ..Default::default()
    });
    let mut observer = ShadowSelection {
        status: Some(serde_json::json!({"regime":label})),
        state: nivra_sandbox::selector::SelectorState {
            active: version.sha256,
            last_switch_ms: None,
        },
        ..Default::default()
    };
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "sandbox_unavailable"
    );
    assert_eq!(observer.status.as_ref().unwrap()["live_applied"], false);
    // Invalid and empty score maps fail closed. Real execution needs bwrap.
    if std::path::Path::new("/usr/bin/bwrap").is_file() {
        for (source, inputs, detail) in [
            (
                "print('{\"params\": 1}')",
                serde_json::json!({"x":1}),
                "invalid_scores",
            ),
            ("print('{}')", serde_json::json!({}), "no_scored_candidates"),
        ] {
            let path = dir.path().join(format!("{detail}.sqlite"));
            let label = live_label(&scan, now);
            let version = add_live_version(&path, source, inputs, &label);
            let settings = live_settings(&path);
            let mut observer = ShadowSelection {
                status: Some(serde_json::json!({"regime":label})),
                state: nivra_sandbox::selector::SelectorState {
                    active: version.sha256,
                    last_switch_ms: None,
                },
                ..Default::default()
            };
            assert!(
                observer
                    .live_ranking(&settings, &scan, &instruments, &projection, now)
                    .is_none()
            );
            let fallback = &observer.status.as_ref().unwrap()["fallback"];
            assert_eq!(fallback["detail"], detail);
            assert_eq!(observer.status.as_ref().unwrap()["live_applied"], false);
        }
        // Every typed sandbox failure and a score set with no eligible
        // prospect has a stable fallback reason.
        let label = live_label(&scan, now);
        let cases = [
            (
                "sandbox_config",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    max_keys: 0,
                    ..Default::default()
                },
            ),
            (
                "sandbox_input",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    max_keys: 1,
                    ..Default::default()
                },
            ),
            (
                "sandbox_setup",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    python_path: "/nonexistent/nivra-python".into(),
                    ..Default::default()
                },
            ),
            (
                "sandbox_exit",
                "raise SystemExit(2)",
                nivra_sandbox::ScorerSandboxConfig::default(),
            ),
            (
                "sandbox_wall_timeout",
                "import time; time.sleep(5)",
                nivra_sandbox::ScorerSandboxConfig {
                    wall_ms: 1,
                    ..Default::default()
                },
            ),
            (
                "sandbox_output_limit",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    max_output_bytes: 4,
                    ..Default::default()
                },
            ),
            (
                "sandbox_output_invalid",
                "print('[]')",
                nivra_sandbox::ScorerSandboxConfig::default(),
            ),
        ];
        for (detail, source, sandbox) in cases {
            let path = dir.path().join(format!("{detail}.sqlite"));
            let version = add_live_version(&path, source, serde_json::json!({}), &label);
            let mut settings = live_settings(&path);
            settings.scorer_sandbox = Some(sandbox);
            let mut observer = ShadowSelection {
                regime: Some(label.clone()),
                status: Some(serde_json::json!({"regime":label.clone()})),
                state: nivra_sandbox::selector::SelectorState {
                    active: version.sha256,
                    last_switch_ms: None,
                },
                ..Default::default()
            };
            assert!(
                observer
                    .live_ranking(&settings, &scan, &instruments, &projection, now)
                    .is_none()
            );
            assert_eq!(
                observer.status.as_ref().unwrap()["fallback"]["detail"],
                detail
            );
        }
        let path = dir.path().join("no-overlap.sqlite");
        let version = add_live_version(&path, "print('{\"A\": 1}')", serde_json::json!({}), &label);
        let settings = live_settings(&path);
        let restricted = serde_json::json!({"prospects":[{"instrument_key":"Z"}]});
        let mut observer = ShadowSelection {
            regime: Some(label.clone()),
            status: Some(serde_json::json!({"regime":label.clone()})),
            state: nivra_sandbox::selector::SelectorState {
                active: version.sha256,
                last_switch_ms: None,
            },
            ..Default::default()
        };
        assert!(
            observer
                .live_ranking(&settings, &scan, &instruments, &restricted, now)
                .is_none()
        );
        assert_eq!(
            observer.status.as_ref().unwrap()["fallback"]["detail"],
            "no_scored_candidates"
        );
    }
    // The exact-source path is still exercised without the hostile sandbox.
    let path = dir.path().join("valid.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(
        &path,
        "print('{\"A\": 1, \"C\": 9}')",
        serde_json::json!({}),
        &label,
    );
    let settings = live_settings(&path);
    let mut observer = ShadowSelection {
        regime: Some(label.clone()),
        status: Some(serde_json::json!({"regime":label})),
        state: nivra_sandbox::selector::SelectorState {
            active: version.sha256,
            last_switch_ms: None,
        },
        ..Default::default()
    };
    if std::path::Path::new("/usr/bin/bwrap").is_file() {
        assert!(
            observer
                .live_ranking(&settings, &scan, &instruments, &projection, now)
                .is_some()
        );
    }
    // Selector-level fallbacks keep their existing reasons plus live detail.
    for (stale, path, detail) in [
        (true, empty.clone(), "stale_data"),
        (false, empty.clone(), "no_eligible_candidates"),
        (false, absent.clone(), "library_unavailable"),
    ] {
        let settings = live_settings(&path);
        let mut observer = ShadowSelection::default();
        observer.evaluate(&settings, &scan, now, stale, &[0.001, 0.001]);
        assert_eq!(
            observer.status.as_ref().unwrap()["fallback"]["detail"],
            detail
        );
    }
    let mut invalid_settings = live_settings(&empty);
    invalid_settings
        .strategy_selection
        .as_mut()
        .unwrap()
        .selector
        .min_trades = 0;
    let mut invalid = ShadowSelection::default();
    invalid.evaluate(&invalid_settings, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(invalid.status.as_ref().unwrap()["reason"], "InvalidConfig");
    assert_eq!(
        invalid.status.as_ref().unwrap()["fallback"]["detail"],
        "invalid_config"
    );
    // Unknown regime on an empty cache.
    let mut observer = ShadowSelection::default();
    observer.evaluate(&live_settings(&empty), &Scanner::default(), now, false, &[]);
    assert_eq!(observer.status.as_ref().unwrap()["reason"], "UnknownRegime");
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "unknown_regime"
    );
    // Active paper floor breach falls back before new evidence is considered.
    let path = dir.path().join("floor.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(&path, "print('{}')", serde_json::json!({}), &label);
    {
        let db = Library::open(&path).unwrap();
        db.record_result(&RunResult {
            sha256: version.sha256.clone(),
            run_kind: "paper".into(),
            session_date: "2026-10-23".into(),
            regime: Some(label.clone()),
            metrics: serde_json::json!({"trades":1,"net_pnl_after_costs":-1.0,
                    "expectancy":-1.0,"max_drawdown":1.0}),
            recorded_at_ms: 1,
        })
        .unwrap();
    }
    let mut observer = ShadowSelection::default();
    observer.state.active = version.sha256;
    observer.evaluate(&live_settings(&path), &scan, now, false, &[0.001, 0.001]);
    assert_eq!(
        observer.status.as_ref().unwrap()["reason"],
        "PaperFloorBreach"
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "paper_floor_breach"
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["chosen_id"],
        nivra_sandbox::selector::DEFAULT_ID
    );
    // Every typed sandbox failure has a stable, non-secret detail.
    use nivra_sandbox::Failure::*;
    for (failure, detail) in [
        (Config, "sandbox_config"),
        (Input, "sandbox_input"),
        (Setup, "sandbox_setup"),
        (Unavailable, "sandbox_unavailable"),
        (Exit, "sandbox_exit"),
        (WallTimeout, "sandbox_wall_timeout"),
        (OutputLimit, "sandbox_output_limit"),
        (OutputInvalid, "sandbox_output_invalid"),
    ] {
        assert_eq!(sandbox_failure_detail(failure), detail);
    }
}

fn start() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn instrument(n: usize) -> Instrument {
    Instrument {
        key: format!("NSE_EQ|INE{n:09}"),
        symbol: format!("S{n}"),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }
}
fn quote(at: i64, price: i64) -> Quote {
    Quote {
        bid: Price::try_from(Amount::from_integer(price)).unwrap(),
        ask: Price::try_from(Amount::from_integer(price).checked_add(a("0.05")).unwrap()).unwrap(),
        bid_units: 10000,
        ask_units: 10000,
        source: at,
        receipt: at,
    }
}

#[test]
fn full_quote_documented_optional_features_and_receipt_minutes() {
    use nivra_core::strategy::{Feature, extract_features};
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../fixtures/upstox_full_quote_doc_example.json"
    ))
    .unwrap();
    let documented = fixture["data"]["NSE_EQ:NHPC"].clone();
    let mut i = instrument(0);
    i.key = documented["instrument_token"].as_str().unwrap().into();
    i.symbol = "NHPC".into();
    // Screening depth remains optional; zero on either part removes the whole level.
    for (bid, ask, buy, sell, expected_bid, expected_ask, spread, imbalance) in [
        (
            serde_json::json!({"price": 52.05, "quantity": 6917}),
            serde_json::json!({"price": 0, "quantity": 0}),
            Some(6917_u64),
            Some(0_u64),
            Some(52.05),
            None,
            None,
            Some(1.0),
        ),
        (
            serde_json::json!({"price": 100, "quantity": 10}),
            serde_json::json!({"price": 102, "quantity": 20}),
            Some(10),
            Some(30),
            Some(100.0),
            Some(102.0),
            Some(2.0 / 101.0),
            Some(0.25),
        ),
        (
            serde_json::json!({"price": 102, "quantity": 10}),
            serde_json::json!({"price": 100, "quantity": 20}),
            Some(0),
            Some(30),
            Some(102.0),
            Some(100.0),
            None,
            Some(0.0),
        ),
        (
            serde_json::json!({"price": 100, "quantity": 10}),
            serde_json::json!({"price": 100, "quantity": 20}),
            Some(u64::MAX),
            Some(u64::MAX),
            Some(100.0),
            Some(100.0),
            Some(0.0),
            Some(0.5),
        ),
        (
            serde_json::json!({"price": 0, "quantity": 10}),
            serde_json::json!({"price": 102, "quantity": 0}),
            Some(0),
            Some(0),
            None,
            None,
            None,
            None,
        ),
        (
            serde_json::json!({"price": 100, "quantity": 0}),
            serde_json::json!({"price": 0, "quantity": 20}),
            None,
            Some(30),
            None,
            None,
            None,
            None,
        ),
        (
            serde_json::Value::Null,
            serde_json::Value::Null,
            Some(10),
            None,
            None,
            None,
            None,
            None,
        ),
    ] {
        let mut row = documented.clone();
        row["timestamp"] = serde_json::json!(start());
        row["depth"] = serde_json::json!({"buy": [bid], "sell": [ask]});
        row["total_buy_quantity"] = serde_json::json!(buy);
        row["total_sell_quantity"] = serde_json::json!(sell);
        if buy.is_none() {
            row.as_object_mut().unwrap().remove("depth");
        }
        let rows = serde_json::Map::from_iter([("NSE_EQ:NHPC".into(), row)]);
        let mut scan = Scanner::default();
        scan_rest_rows(
            &mut scan,
            std::slice::from_ref(&i),
            &rows,
            start(),
            start(),
            2000,
            false,
            None,
        )
        .unwrap();
        let fields = scan.samples[&i.key].market_fields.as_ref().unwrap();
        assert_eq!(fields.best_bid, expected_bid);
        assert_eq!(fields.best_ask, expected_ask);
        assert_eq!(
            fields.best_bid_qty,
            expected_bid.map(|_| if expected_bid == Some(52.05) {
                6917
            } else {
                10
            })
        );
        assert_eq!(fields.best_ask_qty, expected_ask.map(|_| 20));
        assert_eq!(fields.total_buy_quantity, buy);
        assert_eq!(fields.total_sell_quantity, sell);
        let snapshot = extract_features(&scan, start(), 2000);
        let features = &snapshot.rows[0].features;
        for (feature, expected) in [
            (Feature::BestBid, expected_bid),
            (Feature::BestAsk, expected_ask),
            (Feature::BestBidQty, fields.best_bid_qty.map(|v| v as f64)),
            (Feature::BestAskQty, fields.best_ask_qty.map(|v| v as f64)),
            (Feature::SpreadFraction, spread),
            (Feature::BookImbalance, imbalance),
        ] {
            assert_eq!(features.get(&feature).copied(), expected, "{feature:?}");
        }
        let mut legacy = serde_json::to_value(fields).unwrap();
        for name in [
            "best_bid",
            "best_ask",
            "best_bid_qty",
            "best_ask_qty",
            "total_buy_quantity",
            "total_sell_quantity",
        ] {
            legacy.as_object_mut().unwrap().remove(name);
        }
        let restored: nivra_core::execution::MarketFields =
            serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), legacy);
        assert!(
            extract_features(&scan, start() + 2001, 2000)
                .rows
                .is_empty()
        );
    }
    // Use a synthetic fresh source clock; only the archived receipt sets elapsed time.
    for (offset, expected_minutes) in [(-60_000, None), (0, Some(0.0)), (60_000, Some(1.0))] {
        for (average, expected_vwap) in [
            (Some(serde_json::json!(52.56)), Some(52.56)),
            (None, None),
            (Some(serde_json::json!(0)), None),
            (Some(serde_json::json!(-1)), None),
            (Some(serde_json::json!("NaN")), None),
            (Some(serde_json::Value::Null), None),
        ] {
            let mut row = documented.clone();
            row["timestamp"] = serde_json::json!(start() + offset - 500);
            row.as_object_mut().unwrap().remove("average_price");
            if let Some(value) = average {
                row["average_price"] = value;
            }
            let rows = serde_json::Map::from_iter([("NSE_EQ:NHPC".into(), row)]);
            let receipt = start() + offset;
            let mut scan = Scanner::default();
            scan_rest_rows(
                &mut scan,
                std::slice::from_ref(&i),
                &rows,
                receipt,
                receipt,
                2000,
                false,
                None,
            )
            .unwrap();
            let fields = scan.samples[&i.key].market_fields.as_ref().unwrap();
            assert_eq!(fields.session_vwap, expected_vwap);
            let features = extract_features(&scan, receipt + 1000, 2000);
            assert_eq!(
                features.rows[0]
                    .features
                    .get(&Feature::SessionVwap)
                    .copied(),
                expected_vwap
            );
            assert_eq!(
                features.rows[0]
                    .features
                    .get(&Feature::MinutesSinceOpen)
                    .copied(),
                expected_minutes
            );
            let mut legacy = serde_json::to_value(fields).unwrap();
            legacy.as_object_mut().unwrap().remove("session_vwap");
            assert!(
                serde_json::from_value::<nivra_core::execution::MarketFields>(legacy)
                    .unwrap()
                    .session_vwap
                    .is_none()
            );
        }
    }
}

#[test]
fn scanner_screens_key_matching_missing_keys_and_whole_market_baseline() {
    let now = start() + 60_000;
    let universe = (0..108).map(instrument).collect::<Vec<_>>();
    let mut scan = Scanner {
        screens: Some(nivra_core::execution::ScreenPolicy::recommended()),
        ..Scanner::default()
    };
    let mut rows = serde_json::Map::new();
    for (n, instrument) in universe.iter().enumerate().take(106) {
        let (price, volume, low, high) = match n {
            101 => (5.0, 1_000_000, 1.0, 10.0),
            102 => (100.0, 0, 80.0, 120.0),
            103 => (119.5, 20000, 80.0, 120.0),
            104 => (80.5, 20000, 80.0, 120.0),
            n if n < 40 => (90.0, 20000, 50.0, 150.0),
            _ => (100.0, 20000, 80.0, 120.0),
        };
        let fields = nivra_core::execution::MarketFields {
            session_vwap: None,
            best_bid: None,
            best_bid_qty: None,
            best_ask: None,
            best_ask_qty: None,
            total_buy_quantity: None,
            total_sell_quantity: None,
            lower_circuit_limit: a("50"),
            upper_circuit_limit: a("150"),
            volume: 20000,
        };
        scan.observe_price(
            now - 31_000,
            &instrument.key,
            Price::parse("100").unwrap(),
            now - 31_000,
            now - 31_000,
            180000,
            Some(&fields),
        )
        .unwrap();
        rows.insert(format!("renamed-provider-row-{n}"),serde_json::json!({"instrument_token":instrument.key,"symbol":if n==105 {"RENAMED"}else{&instrument.symbol},
                "timestamp":now,"last_price":price,"volume":volume,"lower_circuit_limit":low,"upper_circuit_limit":high,
                "depth":{"buy":[{"price":price,"quantity":100}],"sell":[{"price":price,"quantity":100}]}}));
    }
    // A previously seen key disappears; a new key never returns a row.
    let previous = scan.samples.entry(universe[106].key.clone()).or_default();
    previous.seen = now - 20_000;
    previous.reason = "eligible_no_signal".into();
    scan_rest_rows(&mut scan, &universe, &rows, now, now, 180000, false, None).unwrap();
    assert_eq!(scan.samples[&universe[101].key].reason, "below_min_price");
    assert_eq!(
        scan.samples[&universe[102].key].reason,
        "below_min_traded_value"
    );
    assert_eq!(
        scan.samples[&universe[103].key].reason,
        "near_upper_circuit"
    );
    assert_eq!(
        scan.samples[&universe[104].key].reason,
        "near_lower_circuit"
    );
    assert_eq!(
        scan.samples[&universe[105].key].diagnostic.as_deref(),
        Some("symbol_mismatch")
    );
    assert!(
        data::parse_row(
            rows.values()
                .find(|r| r["instrument_token"] == universe[105].key)
                .unwrap(),
            now,
            &universe[105].key,
            &universe[105].symbol,
            universe[105].tick
        )
        .is_ok()
    );
    assert_eq!(scan.samples[&universe[106].key].seen, now - 20_000);
    assert_eq!(scan.samples[&universe[107].key].seen, 0);
    let settings: super::super::discovery::Settings =
        serde_json::from_value(serde_json::json!({"max_scan_age_ms":180000,"max_prospects":40}))
            .unwrap();
    let projection = super::super::discovery::project(&scan, &universe, &settings, now);
    assert_eq!(
        projection["market_median_bps"], 0.0,
        "101-stock market is flat despite negative top-40"
    );
    assert_eq!(projection["market_sample_count"], 106);
    assert_eq!(projection["fresh_count"], 106);
    assert_eq!(projection["stale_count"], 1);
    assert_eq!(projection["unseen_count"], 1);
    assert_eq!(projection["oldest_fresh_age_ms"], 0);
    assert_eq!(projection["screen_reasons"]["no_quote_returned"], 2);
    assert_eq!(projection["symbol_mismatch_count"], 1);
    assert!(
        projection["prospects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["symbol"].as_str().unwrap()[1..].parse::<usize>().unwrap() < 40)
    );
    for item in &universe[101..=104] {
        assert!(!Scanner::market_allows_with_policy(
            now,
            item,
            &data::parse_row(
                rows.values()
                    .find(|r| r["instrument_token"] == item.key)
                    .unwrap(),
                now,
                &item.key,
                &item.symbol,
                item.tick
            )
            .unwrap(),
            rows.values()
                .find(|r| r["instrument_token"] == item.key)
                .unwrap(),
            &nivra_core::execution::ScreenPolicy::recommended()
        ));
    }
    let seen = scan.samples[&universe[0].key].seen;
    scan_rest_rows(
        &mut scan,
        &universe,
        &rows,
        now + 1000,
        now + 1000,
        180000,
        true,
        None,
    )
    .unwrap();
    assert_eq!(
        scan.samples[&universe[0].key].seen, seen,
        "metadata cannot renew screening prices"
    );
    assert_eq!(
        scan.samples[&universe[107].key].seen, 0,
        "REST metadata cannot hide missing feed keys"
    );
    // Missing/stale keys never inflate fresh scan age or stay eligible.
    let mut wrong = rows.values().next().unwrap().clone();
    wrong["instrument_token"] = serde_json::json!("NSE_EQ|OTHER");
    assert!(
        data::parse_row(
            &wrong,
            now,
            &universe[0].key,
            &universe[0].symbol,
            universe[0].tick
        )
        .is_err()
    );
}

#[test]
fn stored_paper_cost_policy_cannot_change_on_resume() {
    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = load_wide(&mut db, &c).unwrap();
    assert_eq!(p.costs, c.paper_costs);
    persist(&mut db, &c, &p, start(), None, None, "prepared").unwrap();
    let original: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    c.paper_costs = Some(
        serde_json::from_str(include_str!("../../../../../fixtures/paper-costs.json")).unwrap(),
    );
    assert_eq!(
        load_wide(&mut db, &c).unwrap_err().to_string(),
        "paper_cost_policy_changed"
    );
    assert_eq!(
        open_book(&c).err().unwrap().to_string(),
        "paper_cost_policy_changed"
    );
    assert_eq!(
        db.query_row("SELECT body FROM state WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        original
    );
    let d2 = tempfile::tempdir().unwrap();
    let mut fresh = super::super::tests::config(d2.path());
    fresh.paper_costs = c.paper_costs.clone();
    let mut new_db = open_book(&fresh).unwrap();
    assert_eq!(load_wide(&mut new_db, &fresh).unwrap().costs, c.paper_costs);
}
#[test]
fn durable_terminal_corruption_and_rollback() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    persist(&mut db, &c, &p, start(), None, None, "stopped").unwrap();
    let loaded = load_wide(&mut db, &c).unwrap();
    assert!(loaded.stopped);
    assert!(
        !p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    p.cash = a("1");
    assert!(persist(&mut db, &c, &p, start(), None, None, "bad").is_err());
    assert_eq!(load_wide(&mut db, &c).unwrap(), loaded);
    db.execute_batch("CREATE TRIGGER fail_state BEFORE UPDATE ON state BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let mut current = loaded.clone();
    assert!(
        commit_quotes(
            &mut db,
            &c,
            &mut current,
            &BTreeMap::new(),
            start(),
            "rollback",
            start
        )
        .is_err()
    );
    assert_eq!(current, loaded);
}
#[test]
fn after_lock_clock_and_eod_no_invented_exit() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let at = start();
    let i = instrument(1);
    let quotes = BTreeMap::from([(i.key.clone(), quote(at, 100))]);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at + 2001).unwrap();
    assert!(p.fills.is_empty());
    assert!(p.enter(at, &i, &quote(at, 100)).unwrap());
    let close = at + 360 * 60000;
    p.protect(close, &BTreeMap::new()).unwrap();
    assert_eq!(p.positions.len(), 1);
    assert!(!p.halted);
    p.protect(close, &BTreeMap::from([(i.key.clone(), quote(close, 100))]))
        .unwrap();
    assert!(p.positions.is_empty());
    p.reconcile().unwrap();
}

#[test]
fn failed_history_attempt_retries_with_delay_and_session_bound() {
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_failed_retry_ms = 100;
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    attempts
        .completed(
            &cfg,
            10,
            &serde_json::json!({"status":"FAILED","failure":"history_preparation_cancelled"}),
        )
        .unwrap();
    let mut attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(!attempts.due(&cfg, 109));
    assert!(attempts.due(&cfg, 110));
    // A recorded late failure retries after the same delay instead of losing the day.
    let late_minute = cfg.history_preparation_cutoff_minute + 30;
    assert!(!late_preparation_due(
        &cfg,
        cfg.session,
        late_minute,
        109,
        &attempts
    ));
    assert!(late_preparation_due(
        &cfg,
        cfg.session,
        late_minute,
        110,
        &attempts
    ));
    attempts.started(&cfg, 110).unwrap();
    attempts
        .completed(&cfg, 120, &serde_json::json!({"status":"FAILED"}))
        .unwrap();
    let attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(!attempts.due(&cfg, 1000));
    assert!(history_attempt_window(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute - 1
    ));
    assert!(!history_attempt_window(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute
    ));
    assert!(!history_attempt_window(
        &cfg,
        cfg.session,
        cfg.execution.session_start_minute
    ));
    assert!(!history_attempt_window(
        &cfg,
        cfg.session.succ_opt().unwrap(),
        cfg.history_preparation_start_minute
    ));
    // A session adopted after the cutoff prepares immediately until the
    // late-start cutoff; exhausted attempts rule out another one.
    let late = cfg.execution.session_start_minute + 75;
    let fresh = HistoryAttempts::default();
    assert!(late_preparation_due(&cfg, cfg.session, late, 0, &fresh));
    assert!(late_preparation_due(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute,
        0,
        &fresh
    ));
    assert!(!late_preparation_due(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute - 1,
        0,
        &fresh
    ));
    assert!(!late_preparation_due(&cfg, cfg.session, 900, 0, &fresh));
    assert!(!late_preparation_due(
        &cfg,
        cfg.session.succ_opt().unwrap(),
        late,
        0,
        &fresh
    ));
    assert!(!late_preparation_due(&cfg, cfg.session, late, 0, &attempts));
    let window = preparation::PreparationWindow::LateStart;
    let at = DateTime::parse_from_rfc3339("2026-10-16T10:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let length =
        i64::from(cfg.history_preparation_cutoff_minute - cfg.history_preparation_start_minute)
            * 60_000;
    assert_eq!(
        window.deadline_ms(&cfg, cfg.session, at).unwrap(),
        at + length
    );
    let near_close = DateTime::parse_from_rfc3339("2026-10-16T14:59:00+05:30")
        .unwrap()
        .timestamp_millis();
    let close = DateTime::parse_from_rfc3339("2026-10-16T15:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    assert_eq!(
        window.deadline_ms(&cfg, cfg.session, near_close).unwrap(),
        close
    );
    cfg.history_directory = Some(fixture.path().join("history"));
    let request = preparation_request(&cfg, &[instrument(1)], at, window).unwrap();
    assert_eq!(request.target_session, cfg.session);
    assert_eq!(request.deadline_ms, at + length);
    cfg.history_preparation_failed_retry_ms = 0;
    assert!(cfg.validate().is_err());
}

#[test]
fn history_attempts_survive_restarts_without_replay() {
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_retry_ms = 100;
    let mut attempts = HistoryAttempts::default();
    assert!(attempts.due(&cfg, 0));
    attempts.started(&cfg, 0).unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(attempts.count, 1);
    assert_eq!(attempts.last_report()["status"], "IN_PROGRESS");
    assert_ne!(attempts.last_report()["status"], "BLOCKED");
    assert!(attempts.last_report()["reason"].is_null());
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(HistoryAttempts::path(&cfg)).unwrap()).unwrap();
    assert_eq!(persisted["records"][0]["report"]["status"], "IN_PROGRESS");
    assert_ne!(persisted["records"][0]["report"]["status"], "BLOCKED");
    assert!(persisted["records"][0]["completed_ms"].is_null());
    assert!(!attempts.due(&cfg, 1000)); // running claim is not retried
    assert!(attempts.started(&cfg, 1000).is_err());
    attempts
        .completed(&cfg, 10, &serde_json::json!({"status":"PARTIAL"}))
        .unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(!attempts.due(&cfg, 109));
    assert!(attempts.due(&cfg, 110));
    attempts.started(&cfg, 110).unwrap();
    attempts
        .completed(&cfg, 120, &serde_json::json!({"status":"PARTIAL"}))
        .unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(
        attempts
            .completed(&cfg, 130, &serde_json::json!({"status":"READY"}))
            .is_err()
    );
    assert_eq!(attempts.records.len(), 2);
    assert_eq!(attempts.records[0].report["status"], "PARTIAL");
    assert!(!attempts.due(&cfg, 1000)); // configured bound
    for status in ["READY", "UNKNOWN", "BLOCKED"] {
        let mut attempts = HistoryAttempts::default();
        attempts.started(&cfg, 0).unwrap();
        attempts
            .completed(
                &cfg,
                0,
                &serde_json::json!({"status":status,"failure":"retained_diagnostic"}),
            )
            .unwrap();
        let attempts = HistoryAttempts::load(&cfg).unwrap();
        assert_eq!(attempts.last_report()["failure"], "retained_diagnostic");
        assert!(!attempts.due(&cfg, 1000));
    }
    let original = fs::read(HistoryAttempts::path(&cfg)).unwrap();
    let mut wrong = cfg.clone();
    wrong.account = "different-account".into();
    assert!(HistoryAttempts::load(&wrong).is_err());
    wrong = cfg.clone();
    wrong.session = wrong.session.succ_opt().unwrap();
    assert!(HistoryAttempts::load(&wrong).is_err());
    assert_eq!(fs::read(HistoryAttempts::path(&cfg)).unwrap(), original);
    // Invalid/truncated state is refused, preserved, and never reset.
    let corrupt = tempfile::tempdir().unwrap();
    let bad = super::super::tests::config(corrupt.path());
    rollover::atomic(
        &HistoryAttempts::path(&bad),
        &serde_json::json!({"count":1}),
    )
    .unwrap();
    let before = fs::read(HistoryAttempts::path(&bad)).unwrap();
    assert!(HistoryAttempts::load(&bad).is_err());
    assert_eq!(fs::read(HistoryAttempts::path(&bad)).unwrap(), before);
    let nonregular = tempfile::tempdir().unwrap();
    let nonregular_cfg = super::super::tests::config(nonregular.path());
    fs::create_dir(HistoryAttempts::path(&nonregular_cfg)).unwrap();
    assert!(HistoryAttempts::load(&nonregular_cfg).is_err());
    // A failed durable claim cannot authorize a worker.
    let missing = tempfile::tempdir().unwrap();
    let mut missing_cfg = cfg.clone();
    missing_cfg.directory = missing.path().join("absent");
    assert!(HistoryAttempts::default().started(&missing_cfg, 0).is_err());
    let failed_completion = tempfile::tempdir().unwrap();
    let completion_cfg = super::super::tests::config(failed_completion.path());
    let mut incomplete = HistoryAttempts::default();
    incomplete.started(&completion_cfg, 0).unwrap();
    // Fault the existing atomic writer before rename: the durable claim remains.
    fs::create_dir(HistoryAttempts::path(&completion_cfg).with_extension("tmp")).unwrap();
    assert!(
        incomplete
            .completed(
                &completion_cfg,
                10,
                &serde_json::json!({"status":"PARTIAL"})
            )
            .is_err()
    );
    let restored = HistoryAttempts::load(&completion_cfg).unwrap();
    assert!(!restored.due(&completion_cfg, 1000));
    assert_eq!(restored.last_report()["status"], "IN_PROGRESS");
    assert!(restored.last_report()["reason"].is_null());
    cfg.history_preparation_retry_ms = -1;
    assert!(cfg.validate().is_err());
    cfg.history_preparation_retry_ms = 0;
    cfg.history_preparation_check_interval_ms = 0;
    assert!(cfg.validate().is_err());
    cfg.history_preparation_check_interval_ms = 1;
    assert!(cfg.validate().is_ok());
    cfg.session = cfg.session.succ_opt().unwrap();
    assert!(attempts.due(&cfg, 1000)); // next session gets its own bounded passes
    attempts.started(&cfg, 0).unwrap();
    assert_eq!(attempts.count, 1);
    cfg.history_preparation_max_attempts = 0;
    assert!(!HistoryAttempts::default().due(&cfg, 0));
}

#[test]
fn interrupted_history_recovery_settles_no_replay_and_bounds_retry() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_failed_retry_ms = 100;
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    let untouched = [
        ("paper.sqlite", b"sealed-book".as_slice()),
        ("read-journal.json", b"shared-quota".as_slice()),
        (
            "history-quota-receipt.json",
            b"immutable-receipt".as_slice(),
        ),
    ];
    for (name, body) in untouched {
        fs::write(cfg.directory.join(name), body).unwrap();
    }
    let before: Vec<_> = untouched
        .iter()
        .map(|(name, _)| fs::read(cfg.directory.join(name)).unwrap())
        .collect();
    let output = history_recover(&cfg, 10, true).unwrap();
    assert_eq!(output["status"], "recovered");
    assert_eq!(output["attempt"], 1);
    assert_eq!(output["started_ms"], 0);
    assert_eq!(output["completed_ms"], 10);
    assert_eq!(output["retry_at"], 110);
    assert_eq!(output["audit_receipt_written"], true);
    assert_eq!(output["paper_sqlite_touched"], false);
    let stored = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(stored.count, 1);
    assert_eq!(stored.records.len(), 1);
    assert_eq!(stored.records[0].started_ms, 0);
    assert_eq!(stored.records[0].completed_ms, Some(10));
    assert_eq!(stored.records[0].report["status"], "FAILED");
    assert_eq!(
        stored.records[0].report["reason"],
        HISTORY_ATTEMPT_RECOVERED
    );
    assert_eq!(
        stored.records[0].report["original_report"]["status"],
        "IN_PROGRESS"
    );
    assert_eq!(stored.retry_at, Some(110));
    assert!(!stored.due(&cfg, 109));
    assert!(stored.due(&cfg, 110));
    assert!(history_attempt_window(
        &cfg,
        cfg.session,
        cfg.history_preparation_start_minute
    ));
    assert_eq!(
        stored.records[0].report["recovery"]["confirmed_paper_review"],
        true
    );
    assert_eq!(stored.records[0].report["recovery"]["auto_replay"], false);
    // The receipt is immutable, private and describes both states.
    let receipt_path = history_recovery_receipt_path(&cfg, 1, 0);
    let receipt_before = fs::read(&receipt_path).unwrap();
    assert_eq!(
        fs::metadata(&receipt_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let receipt: serde_json::Value = serde_json::from_slice(&receipt_before).unwrap();
    assert_eq!(receipt["kind"], HISTORY_RECOVERY_RECEIPT_KIND);
    assert_eq!(receipt["paper_only"], true);
    assert_eq!(receipt["account"], cfg.account);
    assert_eq!(receipt["session"], cfg.session.to_string());
    assert_eq!(receipt["attempt"]["sequence"], 1);
    assert_eq!(receipt["attempt"]["max_attempts"], 2);
    assert_eq!(receipt["attempt"]["started_ms"], 0);
    assert_eq!(
        receipt["attempt"]["previous_completed_ms"],
        serde_json::Value::Null
    );
    assert_eq!(
        receipt["attempt"]["previous_report"]["status"],
        "IN_PROGRESS"
    );
    assert_eq!(receipt["attempt"]["new_completed_ms"], 10);
    assert_eq!(receipt["attempt"]["new_status"], "FAILED");
    assert_eq!(receipt["attempt"]["new_reason"], HISTORY_ATTEMPT_RECOVERED);
    assert_eq!(receipt["attempt"]["retry_at"], 110);
    assert_eq!(receipt["operator"]["confirmed_paper_review"], true);
    assert_eq!(receipt["recorded_at_ms"], 10);
    let receipts: Vec<_> = fs::read_dir(&cfg.directory)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("history-recovery-")
        })
        .collect();
    assert_eq!(receipts.len(), 1);
    // A repeated command reports success without a second durable mutation.
    let state_before = fs::read(HistoryAttempts::path(&cfg)).unwrap();
    let repeated = history_recover(&cfg, 500, true).unwrap();
    assert_eq!(repeated["status"], "already_recovered");
    assert_eq!(repeated["completed_ms"], 10);
    assert_eq!(repeated["retry_at"], 110);
    assert_eq!(repeated["audit_receipt_written"], false);
    assert_eq!(fs::read(HistoryAttempts::path(&cfg)).unwrap(), state_before);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
    for ((name, _), previous) in untouched.iter().zip(&before) {
        assert_eq!(&fs::read(cfg.directory.join(name)).unwrap(), previous);
    }
    // A restart reloads the settled record and normal admission starts the
    // next bounded attempt only after the retry deadline.
    let mut restarted = HistoryAttempts::load(&cfg).unwrap();
    restarted.started(&cfg, 110).unwrap();
    assert_eq!(restarted.count, 2);
    assert_eq!(restarted.records.len(), 2);
    assert_eq!(restarted.records[0].started_ms, 0);
    assert_eq!(restarted.records[0].completed_ms, Some(10));
    assert_eq!(restarted.records[1].completed_ms, None);
    assert_eq!(restarted.records[1].report["status"], "IN_PROGRESS");
}

#[test]
fn history_recovery_accepts_legacy_interrupted_prewrite() {
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_failed_retry_ms = 100;
    rollover::atomic(
        &HistoryAttempts::path(&cfg),
        &serde_json::json!({
            "account":cfg.account,
            "session":cfg.session,
            "count":1,
            "records":[{"started_ms":0,"completed_ms":null,
                "report":{"status":"BLOCKED","reason":HISTORY_ATTEMPT_INTERRUPTED}}],
            "retry_at":null
        }),
    )
    .unwrap();
    let output = history_recover(&cfg, 10, true).unwrap();
    assert_eq!(output["status"], "recovered");
    let stored = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(stored.records[0].report["status"], "FAILED");
    assert_eq!(
        stored.records[0].report["original_report"]["reason"],
        HISTORY_ATTEMPT_INTERRUPTED
    );
    assert_eq!(stored.retry_at, Some(110));
}

#[test]
fn history_recovery_refuses_unsafe_or_malformed_state() {
    fn refused(cfg: &Config, code: &str) {
        let path = HistoryAttempts::path(cfg);
        let before = fs::read(&path).ok();
        let error = history_recover(cfg, 100, true).unwrap_err();
        assert_eq!(failure_code(error.as_ref()), code);
        assert_eq!(fs::read(&path).ok(), before);
    }
    let fixture = tempfile::tempdir().unwrap();
    let cfg = super::super::tests::config(fixture.path());
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    // A live/stopped-lock owner prevents recovery of an in-flight claim.
    let busy = AdvisoryLock::new(
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cfg.directory.join("controller.lock"))
            .unwrap(),
    );
    busy.try_lock_exclusive().unwrap();
    refused(&cfg, "history_recover_controller_busy");
    drop(busy);
    fs::create_dir_all(cfg.sessions_root.as_ref().unwrap()).unwrap();
    let root_busy = AdvisoryLock::new(
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cfg.sessions_root.as_ref().unwrap().join("controller.lock"))
            .unwrap(),
    );
    root_busy.try_lock_exclusive().unwrap();
    refused(&cfg, "history_recover_controller_busy");
    drop(root_busy);
    // A completed attempt is not an interrupted claim, even when recovered.
    attempts
        .completed(&cfg, 5, &serde_json::json!({"status":"PARTIAL"}))
        .unwrap();
    refused(&cfg, "history_recover_attempt_completed");
    // Incomplete state with any other reason is an in-flight/unknown attempt.
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 10).unwrap();
    attempts.records[0].report =
        serde_json::json!({"status":"BLOCKED","reason":"history_preparation_admission_failed"});
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_attempt_not_interrupted");
    // The original count still owns the configured budget.
    let mut exhausted = super::super::tests::config(fixture.path());
    exhausted.history_preparation_max_attempts = 1;
    let mut attempts = HistoryAttempts::default();
    attempts.started(&exhausted, 20).unwrap();
    refused(&exhausted, "history_recover_attempt_budget_exhausted");
    // Identity and sequence checks never reset or repair foreign state.
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 30).unwrap();
    attempts.account = "different-account".into();
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_account_mismatch");
    attempts.account = cfg.account.clone();
    attempts.session = cfg.session.succ_opt();
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_session_mismatch");
    attempts.session = Some(cfg.session);
    attempts.count = 0;
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_state_invalid");
    rollover::atomic(
        &HistoryAttempts::path(&cfg),
        &serde_json::json!({
            "account":cfg.account,
            "session":cfg.session,
            "count":2,
            "records":[{"started_ms":40,"completed_ms":null,
                "report":{"status":"BLOCKED","reason":HISTORY_ATTEMPT_INTERRUPTED}}],
            "retry_at":null
        }),
    )
    .unwrap();
    refused(&cfg, "history_recover_state_invalid");
    fs::write(HistoryAttempts::path(&cfg), b"{not-json").unwrap();
    refused(&cfg, "history_recover_state_invalid");
    fs::remove_file(HistoryAttempts::path(&cfg)).unwrap();
    refused(&cfg, "history_recover_state_missing");
    fs::create_dir(HistoryAttempts::path(&cfg)).unwrap();
    refused(&cfg, "history_recover_state_not_regular");
    assert!(
        history_recover(&cfg, 100, false)
            .unwrap_err()
            .to_string()
            .contains("confirm_paper_review")
    );
}

#[test]
fn prepared_daily_volume_requires_minimum_genuine_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(dir.path());
    c.history_directory = Some(dir.path().join("history"));
    fs::create_dir(c.history_directory.as_ref().unwrap()).unwrap();
    let instruments = vec![instrument(1), instrument(2), instrument(3)];
    let request = preparation_request(
        &c,
        &instruments,
        start(),
        preparation::PreparationWindow::PreOpen,
    )
    .unwrap();
    assert_eq!(c.history_avg_daily_volume_min_sessions, 10);
    for (instrument, count) in instruments.iter().zip([21, 9]) {
        let rows: Vec<_> = request
            .required_sessions
            .iter()
            .take(count)
            .enumerate()
            .map(|(n, day)| {
                serde_json::json!([
                    format!("{day}T00:00:00+05:30"),
                    100,
                    102,
                    99,
                    101,
                    (n + 1) * 100,
                    0
                ])
            })
            .collect();
        let body = serde_json::json!({"status":"success","data":{"candles":rows}}).to_string();
        let receipt = serde_json::json!({"key":instrument.key,"unit":"days",
                "start":request.required_sessions[0],"end":c.previous_session,
                "requested_ms":start(),"received_ms":start(),"body":body,
                "body_sha256":digest(body.as_bytes())});
        let path = request.history_root.join(format!(
            "{}.receipt.json",
            history_download::range_id(
                &instrument.key,
                "days",
                request.required_sessions[0],
                c.previous_session
            )
        ));
        fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    }
    let mut scan = Scanner::default();
    for instrument in &instruments {
        let sample = scan.samples.entry(instrument.key.clone()).or_default();
        sample.avg_daily_volume = Some(999.0);
        sample.avg_daily_volume_sessions = Some(21);
    }
    load_daily_volumes(&c, &instruments, start(), &mut scan);
    let full = &scan.samples[&instruments[0].key];
    assert_eq!(full.avg_daily_volume, Some(1100.0));
    assert_eq!(full.avg_daily_volume_sessions, Some(21));
    for instrument in &instruments[1..] {
        let sample = &scan.samples[&instrument.key];
        assert_eq!(sample.avg_daily_volume, None);
        assert_eq!(sample.avg_daily_volume_sessions, None);
        let value = serde_json::to_value(sample).unwrap();
        assert!(value.get("avg_daily_volume").is_none());
        assert!(value.get("avg_daily_volume_sessions").is_none());
    }
    c.history_avg_daily_volume_min_sessions = 9;
    load_daily_volumes(&c, &instruments, start(), &mut scan);
    assert_eq!(
        scan.samples[&instruments[1].key].avg_daily_volume,
        Some(500.0)
    );
    assert_eq!(
        scan.samples[&instruments[1].key].avg_daily_volume_sessions,
        Some(9)
    );
}

#[test]
fn resume_preflight_requires_flat_paper_operator_stop() {
    // A market-open restart needs no preparation worker or provider request.
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(dir.path());
    c.history_directory = Some(dir.path().join("history"));
    fs::create_dir(c.history_directory.as_ref().unwrap()).unwrap();
    let instruments = vec![instrument(1)];
    let request = preparation_request(
        &c,
        &instruments,
        start(),
        preparation::PreparationWindow::PreOpen,
    )
    .unwrap();
    let rows: Vec<_> = request
        .required_sessions
        .iter()
        .map(|day| serde_json::json!([format!("{day}T00:00:00+05:30"), 100, 102, 99, 101, 1000, 0]))
        .collect();
    let body = serde_json::json!({"status":"success","data":{"candles":rows}}).to_string();
    let receipt = serde_json::json!({"key":instruments[0].key,"unit":"days","start":request.required_sessions[0],"end":c.previous_session,"requested_ms":start(),"received_ms":start(),"body":body,"body_sha256":digest(body.as_bytes())});
    let path = request.history_root.join(format!(
        "{}.receipt.json",
        history_download::range_id(
            &instruments[0].key,
            "days",
            request.required_sessions[0],
            c.previous_session
        )
    ));
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let before = fs::read(&path).unwrap();
    let restored = recovered_history_status(&c, &instruments, start());
    assert_eq!(restored["status"], "READY", "{restored}");
    assert_eq!(restored["ready_instruments"], 1);
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::write(&path, b"invalid").unwrap();
    assert_eq!(
        recovered_history_status(&c, &instruments, start())["status"],
        "UNKNOWN"
    );
    let mut p = crate::runtime::tests::test_portfolio();
    assert_eq!(resume_preflight(&p), Err("resume_not_operator_stopped"));
    p.stopped = true;
    assert_eq!(resume_preflight(&p), Ok(()));
    p.halted = true;
    assert_eq!(resume_preflight(&p), Err("resume_risk_halted"));
    p.halted = false;
    p.closed = true;
    assert_eq!(resume_preflight(&p), Err("resume_session_closed"));
    p.closed = false;
    p.stopped = false;
    assert!(
        p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    p.stopped = true;
    assert_eq!(resume_preflight(&p), Err("resume_open_positions"));
    assert_eq!(pending_orders(&p), 0);
    p.model_led = true;
    assert_eq!(resume_preflight(&p), Ok(()));
}
fn issue_137_planner_config(dir: &std::path::Path) -> (Config, planner::PlannerConfig) {
    let mut c = super::super::tests::config(dir);
    let cfg: planner::PlannerConfig = serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
    let file = dir.join("planner.json");
    fs::write(&file, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(file);
    (c, cfg)
}
fn closed_flat_planner_halt_is_recovered_without_financial_change() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
    cfg.validate_decision_lane().unwrap();
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    p.closed = true;
    p.stopped = true;
    p.halted = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    let cash = p.cash;
    let high_water = p.high_water;
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    drop(db);

    assert_eq!(
        planner_recover(&c, false).unwrap_err().to_string(),
        "planner_recover_confirm_paper_review_required"
    );
    planner_recover(&c, true).unwrap();

    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.closed && recovered.stopped && !recovered.halted);
    assert_eq!(recovered.health, "closed");
    assert_eq!(recovered.cash, cash);
    assert_eq!(recovered.high_water, high_water);
    assert!(recovered.positions.is_empty() && recovered.fills.is_empty());
    let body: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='operator_planner_recovery_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["mode"], "closed_flat");
    assert_eq!(audit["financial_state_unchanged"], true);
    assert_eq!(audit["halted"], false);
    assert!(!redecision::blocked(&db, &c).unwrap());

    // A sealed flat book keeps a market-data integrity block; the halt clear
    // cannot silently retire it without the shared input proof.
    for block in [redecision::DATA_BLOCK, "planner_news_evidence_invalid"] {
        let d = tempfile::tempdir().unwrap();
        let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        redecision::block(&mut db, &c, block).unwrap();
        p.closed = true;
        p.stopped = true;
        p.halted = true;
        p.health = "planner_authority_invalid_recovery_required".into();
        let cash = p.cash;
        let high_water = p.high_water;
        persist(
            &mut db,
            &c,
            &p,
            start(),
            None,
            None,
            "planner_authority_invalid",
        )
        .unwrap();
        drop(db);
        planner_recover(&c, true).unwrap();
        let mut db = open_book(&c).unwrap();
        let recovered = load_wide(&mut db, &c).unwrap();
        assert!(recovered.closed && recovered.stopped && !recovered.halted);
        assert_eq!(recovered.health, "closed");
        assert_eq!(recovered.cash, cash);
        assert_eq!(recovered.high_water, high_water);
        assert!(redecision::blocked(&db, &c).unwrap(), "{block}");
        let control: serde_json::Value = serde_json::from_str(
            &db.query_row(
                "SELECT body FROM planner_event_control WHERE id=1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(control["blocked"], block);
    }
}
fn halted_model_led_book_enters_reviewed_protection_only_mode(corrected: bool) {
    let d = tempfile::tempdir().unwrap();
    let (c, mut cfg) = super::super::decision::decision_fixture_config(d.path());
    cfg.validate_decision_lane().unwrap();
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    p.model_led = true;
    assert!(
        p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    if corrected {
        cfg.max_handoff_instruments -= 1;
        fs::write(
            c.planner_config.as_ref().unwrap(),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
    }
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    p.halted = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    let cash = p.cash;
    let fills = p.fills.clone();
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    drop(db);

    assert_eq!(
        planner_recover_protection_only(&c, false)
            .unwrap_err()
            .to_string(),
        "planner_recover_confirm_paper_review_required"
    );
    planner_recover_protection_only(&c, true).unwrap();

    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.halted);
    assert!(!recovered.stopped);
    assert_eq!(recovered.health, "risk_halted_protective_exits_only");
    assert_eq!(recovered.positions.len(), 1);
    assert_eq!(recovered.cash, cash);
    assert_eq!(recovered.fills, fills);
    assert!(redecision::config_current(&db, &cfg).unwrap());
    // The halt remains authoritative: no new planner entry can be reserved.
    let body: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='operator_planner_protection_only_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["mode"], "protection_only");
    assert_eq!(audit["financial_state_unchanged"], true);
    assert_eq!(audit["entries_blocked"], true);
    assert_eq!(audit["halted"], true);
}
fn live_config_mismatch_suspends_planner_without_halting_the_book() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let mut current = Some(cfg.clone());
    let mut current_fallback = None;
    let mut current_reason = None;
    contain_event_error(
        Err("planner_event_configuration_changed".into()),
        &mut db,
        &c,
        &mut p,
        &mut current,
        &mut current_fallback,
        &mut current_reason,
        start(),
    )
    .unwrap();
    assert!(!p.halted);
    assert!(current.is_none());
    assert!(current_fallback.is_some());
    assert_eq!(current_reason, Some("planner_event_configuration_changed"));
    assert!(redecision::blocked(&db, &c).unwrap());
    let body: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='planner_configuration_changed_apply_required' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(audit["failed_check"].as_str().is_some());
    assert!(
        audit["recovery_command"]
            .as_str()
            .unwrap()
            .contains("config-apply")
    );
    persist(
        &mut db,
        &c,
        &p,
        start() + 1,
        None,
        None,
        "planner_configuration_changed_apply_required",
    )
    .unwrap();
    drop(db);

    // Repointing the policy in memory
    // has that exact policy applied by the reviewed transition.
    let mut switched = cfg.clone();
    switched.max_handoff_instruments -= 1;
    let out = d.path().join("planner.switched.json");
    fs::write(&out, serde_json::to_vec(&switched).unwrap()).unwrap();
    let mut applied = c.clone();
    applied.planner_config = Some(out.clone());
    config_apply(&applied, true).unwrap();
    let mut db = open_book(&c).unwrap();
    assert!(!redecision::blocked(&db, &c).unwrap());
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert_eq!(recovered.health, "operator_stopped");
    let applied_cfg = planner::PlannerConfig::load_for_session(&out, c.session).unwrap();
    assert!(redecision::config_current(&db, &applied_cfg).unwrap());
    let body: String = db
        .query_row(
            "SELECT body FROM ticks WHERE event='operator_config_apply_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["stopped"], true);
}
fn planner_incident_never_halts_a_closed_book() {
    let d = tempfile::tempdir().unwrap();
    let (c, mut cfg) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    p.closed = true;
    p.stopped = true;
    p.health = "closed".into();
    persist(&mut db, &c, &p, start(), None, None, "closed").unwrap();
    // A restart after a config edit must not turn a closed book into a
    // planner-authority halt.
    cfg.max_handoff_instruments -= 1;
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    assert!(!redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap());
    assert!(!p.halted);
    assert_eq!(p.health, "closed");
    planner_incident(&mut db, &c, &mut p, start() + 1).unwrap();
    assert!(!p.halted);
    assert_eq!(p.health, "closed");
    let recorded: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='planner_incident_check')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(recorded);
}
fn stopped_recovery_retires_interrupted_pending_claim_without_replay() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
    cfg.validate_decision_lane().unwrap();
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let claim = redecision::pending_fixture(&mut db, &c, &p, start());
    // A controller restart observes the reserved claim as interrupted.
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap();
    p.stopped = true;
    p.health = "operator_stopped".into();
    persist(&mut db, &c, &p, start() + 2, None, None, "operator_stopped").unwrap();
    let attempts: i64 = db
        .query_row("SELECT count(*) FROM planner_attempt_log", [], |r| r.get(0))
        .unwrap();
    drop(db);
    assert_eq!(
        redecision::reviewed_stopped_block(&open_book(&c).unwrap(), &c).unwrap(),
        redecision::ReviewedBlock::AttemptInterrupted
    );
    planner_recover_checked(&c, true, false, |_, _, _| Ok(())).unwrap();
    let mut db = open_book(&c).unwrap();
    let (attempt_state, result): (String, String) = db
        .query_row(
            "SELECT state,result FROM planner_attempt_log WHERE id=?",
            [&claim.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(attempt_state, "failed");
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "operator_review_interrupted_no_replay");
    assert_eq!(result["charges_preserved"], true);
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        attempts
    );
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='operator_pending_planner_attempt_retained')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        );
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert_eq!(recovered.health, "operator_stopped");
}
fn tick_count(db: &Connection, event: &str) -> i64 {
    db.query_row("SELECT count(*) FROM ticks WHERE event=?", [event], |r| {
        r.get(0)
    })
    .unwrap()
}
fn archived_requests(c: &Config, label: &str) -> usize {
    fs::read_dir(c.sessions_root.as_ref().unwrap().join("stop-requests"))
        .map(|dir| {
            dir.filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("-{label}.json"))
            })
            .count()
        })
        .unwrap_or(0)
}
fn service_shutdown_continues_and_only_a_request_stops_the_book() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let claim = redecision::pending_fixture(&mut db, &c, &p, start());

    // A plain SIGTERM (restart, updater, host stop) is a service shutdown: the
    // cancelled claim is retired failed/no-replay, nothing sticks, the book stays open.
    let intent = shutdown::intent(&c, start() + 1);
    assert_eq!(intent.kind, shutdown::ShutdownKind::Service);
    let details =
        shutdown::settle(&mut db, &c, &mut p, &intent, Some(&claim), start() + 1).unwrap();
    assert_eq!(details["operator_stopped"], false);
    assert_eq!(details["shutdown"], "service");
    assert!(!p.stopped && !p.halted);
    let (state, result): (String, String) = db
        .query_row(
            "SELECT state,result FROM planner_attempt_log WHERE id=?",
            [&claim.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "failed");
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "service_shutdown_interrupted_no_replay");
    assert_eq!(result["charges_preserved"], true);
    assert_eq!(tick_count(&db, "service_shutdown"), 1);
    assert_eq!(tick_count(&db, "operator_stopped"), 0);
    drop(db);
    // The restarted controller continues the same dated book without review.
    let mut db = open_book(&c).unwrap();
    let restarted = load_wide(&mut db, &c).unwrap();
    assert!(!restarted.stopped && stopped_restart_warning(&restarted).is_none());
    assert_eq!(rollover::current(&c).unwrap().session, c.session);
    redecision::initialize(&mut db, &c, &restarted, Some(&cfg), start() + 2).unwrap();
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert!(!redecision::pending_attempt(&db).unwrap());

    // Retirement that cannot be proven exact fails closed behind the reviewed block.
    let failed = tempfile::tempdir().unwrap();
    let (fc, fcfg) = super::super::decision::decision_fixture_config(failed.path());
    let mut fdb = open_book(&fc).unwrap();
    let mut fp = load_wide(&mut fdb, &fc).unwrap();
    redecision::initialize(&mut fdb, &fc, &fp, Some(&fcfg), start()).unwrap();
    let mut foreign = redecision::pending_fixture(&mut fdb, &fc, &fp, start());
    foreign.id = "not-the-pending-claim".into();
    let intent = shutdown::intent(&fc, start() + 4);
    let details =
        shutdown::settle(&mut fdb, &fc, &mut fp, &intent, Some(&foreign), start() + 4).unwrap();
    assert_eq!(details["operator_stopped"], false);
    assert!(!fp.stopped && !fp.halted);
    assert_eq!(
        redecision::recovery_block(&fdb, &fc).unwrap(),
        redecision::RecoveryBlock::Reviewed(redecision::ReviewedBlock::AttemptInterrupted)
    );
    let body: String = fdb
        .query_row(
            "SELECT body FROM ticks WHERE event='service_shutdown' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["planner"]["retired"], false);
    assert_eq!(body["planner"]["fallback_block_recorded"], true);

    // Stale, foreign and non-private requests are ignored and archived, never deleted.
    let mut foreign_session = c.clone();
    foreign_session.session = c.session.succ_opt().unwrap();
    shutdown::write_request(&foreign_session, start()).unwrap();
    let ignored = shutdown::intent(&c, start() + 5);
    assert_eq!(ignored.kind, shutdown::ShutdownKind::Service);
    assert_eq!(ignored.request["reason"], "stop_request_foreign_session");
    shutdown::write_request(&c, start()).unwrap();
    let stale = shutdown::intent(&c, start() + shutdown::STOP_REQUEST_TTL_MS + 1);
    assert_eq!(stale.kind, shutdown::ShutdownKind::Service);
    assert_eq!(stale.request["reason"], "stop_request_stale");
    let path = shutdown::request_path(&c).unwrap();
    fs::write(&path, b"{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        shutdown::intent(&c, start() + 6).request["reason"],
        "stop_request_invalid"
    );
    assert_eq!(archived_requests(&c, "ignored"), 3);
    assert!(!path.exists());

    // `nivra stop` while a controller holds the root lock records a request the
    // controller consumes on SIGTERM; the book then stops exactly as before.
    assert_eq!(
        failure_code(stop(&c, false, start()).unwrap_err().as_ref()),
        "stop_confirm_paper_review_required"
    );
    let running = rollover::controller_lock(&c).unwrap();
    let requested = stop(&c, true, start() + 7).unwrap();
    assert_eq!(requested["stop_requested"], true);
    assert_eq!(
        requested["next_step"],
        "systemctl --user stop nivra-paper.service"
    );
    drop(running);
    let mut p = restarted.clone();
    let intent = shutdown::intent(&c, start() + 8);
    assert_eq!(intent.kind, shutdown::ShutdownKind::Operator);
    let details = shutdown::settle(&mut db, &c, &mut p, &intent, None, start() + 8).unwrap();
    assert_eq!(details["operator_stopped"], true);
    assert!(p.stopped);
    assert_eq!(p.health, "operator_stopped");
    assert_eq!(tick_count(&db, "operator_stopped"), 1);
    assert_eq!(archived_requests(&c, "consumed"), 1);
    assert_eq!(
        shutdown::intent(&c, start() + 9).kind,
        shutdown::ShutdownKind::Service
    );
    drop(db);

    // With no controller running, `nivra stop` stops the active book directly.
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    persist(&mut db, &c, &p, start(), None, None, "open").unwrap();
    drop(db);
    let direct = stop(&c, true, start() + 1).unwrap();
    assert_eq!(direct["controller_running"], false);
    assert_eq!(direct["already_stopped"], false);
    let stopped = load_wide(&mut open_book(&c).unwrap(), &c).unwrap();
    assert!(stopped.stopped && !stopped.halted);
    assert_eq!(
        stop(&c, true, start() + 2).unwrap()["already_stopped"],
        true
    );
    assert!(!shutdown::request_path(&c).unwrap().exists());
}
fn marker_raw(db: &Connection) -> Option<Vec<u8>> {
    let present: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_authority_owner')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if !present {
        return None;
    }
    let value: Option<rusqlite::types::Value> = db
        .query_row(
            "SELECT body FROM planner_authority_owner WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    value.map(|value| match value {
        rusqlite::types::Value::Text(text) => text.into_bytes(),
        rusqlite::types::Value::Blob(bytes) => bytes,
        _ => Vec::new(),
    })
}
fn marker_receipt(db: &Connection) -> Option<serde_json::Value> {
    marker_raw(db).map(|raw| serde_json::from_slice(&raw).unwrap())
}
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn halted_owner_book(
    dir: &std::path::Path,
) -> (Config, planner::PlannerConfig, Connection, Portfolio) {
    let (c, cfg) = super::super::decision::decision_fixture_config(dir);
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    p.halted = true;
    p.stopped = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    (c, cfg, db, p)
}
/// A book whose authority predates the owner-only policy carries a stored
/// `legacy` owner marker. Initialization can no longer write one, so the
/// fixture rewrites the marker the way an older binary left it.
fn initialize_legacy_marked(
    db: &mut Connection,
    c: &Config,
    p: &Portfolio,
    legacy: &planner::PlannerConfig,
    at: i64,
) {
    redecision::initialize(db, c, p, Some(legacy), at).unwrap();
    let body: String = db
        .query_row(
            "SELECT body FROM planner_authority_owner WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut body: serde_json::Value = serde_json::from_str(&body).unwrap();
    body["owner"] = serde_json::json!("legacy");
    db.execute(
        "UPDATE planner_authority_owner SET body=? WHERE id=1",
        [body.to_string()],
    )
    .unwrap();
}
/// Startup must not destroy the original active head or a stronger sticky
/// block just because a claim is pending or the policy changed.
fn startup_preserves_active_and_sticky_block() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let claim = redecision::pending_fixture(&mut db, &c, &p, start());
    db.execute(
        "INSERT INTO planner_plan VALUES('legacy-plan',?)",
        [serde_json::json!({"decision_id":"legacy-plan"}).to_string()],
    )
    .unwrap();
    let raw: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut control: serde_json::Value = serde_json::from_str(&raw).unwrap();
    control["active"] = serde_json::json!("legacy-plan");
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [control.to_string()],
    )
    .unwrap();
    let plans: i64 = db
        .query_row("SELECT count(*) FROM planner_plan", [], |r| r.get(0))
        .unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap();
    let control: serde_json::Value = serde_json::from_str(
        &db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(control["active"], "legacy-plan");
    assert_eq!(control["blocked"], "attempt_interrupted_no_replay");
    assert_eq!(control["pending"]["id"], claim.id);
    assert_eq!(marker_receipt(&db).unwrap()["owner"], "pi");
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_plan", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        plans
    );
    // A pending claim must not replace a stronger sticky data block.
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    redecision::pending_fixture(&mut db, &c, &p, start());
    redecision::block(&mut db, &c, redecision::DATA_BLOCK).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap();
    let control: serde_json::Value = serde_json::from_str(
        &db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(control["blocked"], redecision::DATA_BLOCK);
    assert!(control["pending"].is_object());

    // A config edit must not replace the same sticky block or clear the head.
    let mut corrected = cfg.clone();
    corrected.max_handoff_instruments -= 1;
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&corrected).unwrap(),
    )
    .unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&corrected), start() + 2).unwrap();
    let control: serde_json::Value = serde_json::from_str(
        &db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(control["blocked"], redecision::DATA_BLOCK);
    assert_eq!(control["active"], serde_json::Value::Null);
}

/// Halted recovery must run the shared input proof before clearing a data or
/// news block; a configuration block is resolved by the exact policy rebind.
fn halted_recovery_requires_block_proof() {
    for block in [redecision::DATA_BLOCK, "planner_news_evidence_invalid"] {
        let d = tempfile::tempdir().unwrap();
        let (c, _cfg, mut db, _p) = halted_owner_book(d.path());
        redecision::block(&mut db, &c, block).unwrap();
        let before = recovery_authority_snapshot(&db);
        drop(db);
        assert_eq!(
            planner_recover(&c, true).unwrap_err().to_string(),
            "planner_recover_input_unavailable",
            "{block}"
        );
        let db = open_book(&c).unwrap();
        assert_eq!(recovery_authority_snapshot(&db), before, "{block}");
    }

    // A configuration block is resolved by the exact policy rebind, not by a
    // no-op validator; the book still has to pass the ordinary halt checks.
    let d = tempfile::tempdir().unwrap();
    let (c, _cfg, mut db, p) = halted_owner_book(d.path());
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    drop(db);
    planner_recover(&c, true).unwrap();
    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert_eq!(recovered.cash, p.cash);
    assert_eq!(recovered.positions, p.positions);
    assert_eq!(recovered.fills, p.fills);
}

/// Protection-only recovery keeps the halt and cannot clear a market-data
/// integrity block without the shared input proof.
fn protection_only_retains_integrity_block() {
    for block in [redecision::DATA_BLOCK, "planner_news_evidence_invalid"] {
        let d = tempfile::tempdir().unwrap();
        let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        p.model_led = true;
        assert!(
            p.enter(start(), &instrument(1), &quote(start(), 100))
                .unwrap()
        );
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        redecision::block(&mut db, &c, block).unwrap();
        p.halted = true;
        p.health = "planner_authority_invalid_recovery_required".into();
        let cash = p.cash;
        let fills = p.fills.clone();
        persist(
            &mut db,
            &c,
            &p,
            start(),
            None,
            None,
            "planner_authority_invalid",
        )
        .unwrap();
        drop(db);
        planner_recover_protection_only(&c, true).unwrap();
        let mut db = open_book(&c).unwrap();
        let recovered = load_wide(&mut db, &c).unwrap();
        assert!(recovered.halted && !recovered.stopped);
        assert_eq!(recovered.health, "risk_halted_protective_exits_only");
        assert_eq!(recovered.cash, cash);
        assert_eq!(recovered.fills, fills);
        assert_eq!(recovered.positions.len(), 1);
        assert!(redecision::blocked(&db, &c).unwrap(), "{block}");
        let control: serde_json::Value = serde_json::from_str(
            &db.query_row(
                "SELECT body FROM planner_event_control WHERE id=1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(control["blocked"], block);
    }
}

fn recovery_authority_snapshot(db: &Connection) -> serde_json::Value {
    let portfolio: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let authority: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let ticks: i64 = db
        .query_row("SELECT count(*) FROM ticks", [], |r| r.get(0))
        .unwrap();
    let attempts: Vec<(String, String, Option<String>)> = db
        .prepare("SELECT id,state,result FROM planner_attempt_log ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let receipt_exists: bool = db
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_authority_owner')", [], |r| r.get(0))
        .unwrap();
    let receipt = if receipt_exists {
        marker_raw(db)
            .map(|raw| serde_json::json!({"raw_hex":hex_bytes(&raw),"sha256":digest(&raw)}))
    } else {
        None
    };
    serde_json::json!({"portfolio":portfolio,"authority":authority,"ticks":ticks,
        "attempts":attempts,"receipt_exists":receipt_exists,"receipt":receipt})
}
fn recovery_owner_receipt_compatibility() {
    for case in [
        "pre_marker",
        "corrected_owner",
        "invalid_version",
        "invalid_binding",
        "invalid_owner",
        "stale_hash",
        "missing_row",
    ] {
        let d = tempfile::tempdir().unwrap();
        let (c, mut cfg) = super::super::decision::decision_fixture_config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        p.halted = true;
        p.stopped = true;
        p.health = "planner_authority_invalid_recovery_required".into();
        persist(
            &mut db,
            &c,
            &p,
            start(),
            None,
            None,
            "planner_authority_invalid",
        )
        .unwrap();
        if case == "corrected_owner" {
            cfg.max_handoff_instruments -= 1;
            fs::write(
                c.planner_config.as_ref().unwrap(),
                serde_json::to_vec(&cfg).unwrap(),
            )
            .unwrap();
        }
        let expected_error = match case {
            // A journal that predates the owner marker has no proven owner.
            "pre_marker" => {
                db.execute_batch("DROP TABLE planner_authority_owner")
                    .unwrap();
                Some("planner_authority_owner_missing")
            }
            "invalid_version" | "invalid_binding" | "invalid_owner" | "stale_hash" => {
                let raw: String = db
                    .query_row(
                        "SELECT body FROM planner_authority_owner WHERE id=1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut receipt: serde_json::Value = serde_json::from_str(&raw).unwrap();
                match case {
                    "invalid_version" => receipt["version"] = serde_json::json!(2),
                    "invalid_binding" => receipt["binding"] = serde_json::json!("other-book"),
                    "invalid_owner" => receipt["owner"] = serde_json::json!("legacy"),
                    _ => receipt["config_hash"] = serde_json::json!("stale"),
                }
                db.execute(
                    "UPDATE planner_authority_owner SET body=? WHERE id=1",
                    [receipt.to_string()],
                )
                .unwrap();
                Some(if case == "invalid_owner" {
                    "planner_authority_owner_invalid"
                } else {
                    "planner_authority_owner_mismatch"
                })
            }
            "missing_row" => {
                db.execute("DELETE FROM planner_authority_owner WHERE id=1", [])
                    .unwrap();
                Some("planner_authority_owner_missing")
            }
            _ => None,
        };
        let before = recovery_authority_snapshot(&db);
        drop(db);
        if let Some(expected) = expected_error {
            assert_eq!(
                planner_recover(&c, true).unwrap_err().to_string(),
                expected,
                "{case}"
            );
            assert_eq!(
                recovery_authority_snapshot(&open_book(&c).unwrap()),
                before,
                "{case}"
            );
        } else {
            planner_recover(&c, true).unwrap();
            let mut db = open_book(&c).unwrap();
            let recovered = load_wide(&mut db, &c).unwrap();
            assert!(recovered.stopped && !recovered.halted);
            assert_eq!(recovered.cash, p.cash);
            assert_eq!(recovered.positions, p.positions);
            assert_eq!(recovered.fills, p.fills);
            assert!(redecision::config_current(&db, &cfg).unwrap());
            redecision::decision_startup_check(&db, &c, &cfg).unwrap();
            let raw: String = db
                .query_row(
                    "SELECT body FROM planner_authority_owner WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let receipt: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(receipt["owner"], "pi");
            assert_eq!(
                receipt["config_hash"],
                digest(&planner::policy_bytes(&cfg).unwrap())
            );
        }
    }
    // Capture the current-code dead ends before the repair path exists. These
    // stay as regression checks: repair is an explicit offline command, so the
    // controller still refuses a stale marker until that command runs.
    let d = tempfile::tempdir().unwrap();
    let (c, mut cfg) = super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    p.halted = true;
    p.stopped = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    let prior = digest(&planner::policy_bytes(&cfg).unwrap());
    cfg.max_handoff_instruments -= 1;
    let current = digest(&planner::policy_bytes(&cfg).unwrap());
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    let raw: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut authority: serde_json::Value = serde_json::from_str(&raw).unwrap();
    authority["config_hash"] = serde_json::json!(current);
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [authority.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_config_applied')",
        rusqlite::params![
            start(),
            serde_json::json!({"prior_config_hash":prior,"current_config_hash":current})
                .to_string()
        ],
    )
    .unwrap();
    drop(db);
    let error = planner_recover(&c, true).unwrap_err().to_string();
    eprintln!("authority_repair_red: stale_marker_recover_refused={error}");
    assert_eq!(error, "planner_authority_owner_mismatch");

    let d = tempfile::tempdir().unwrap();
    let (c, legacy) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    initialize_legacy_marked(&mut db, &c, &p, &legacy, start());
    p.halted = true;
    p.stopped = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    drop(db);
    let (owner_c, owner) = super::super::decision::decision_fixture_config(d.path());
    owner.validate_decision_lane().unwrap();
    let recover_error = planner_recover(&owner_c, true).unwrap_err().to_string();
    let apply_error = config_apply(&owner_c, true).unwrap_err().to_string();
    eprintln!(
        "authority_repair_red: halted_legacy_recover={recover_error} config_apply={apply_error}"
    );
    assert_eq!(recover_error, "planner_authority_owner_invalid");
    assert_eq!(apply_error, "config_apply_halted_requires_planner_recover");

    let d = tempfile::tempdir().unwrap();
    let (c, legacy) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    initialize_legacy_marked(&mut db, &c, &p, &legacy, start());
    mark_operator_stopped(&mut p);
    persist(&mut db, &c, &p, start(), None, None, "operator_stopped").unwrap();
    let before = recovery_authority_snapshot(&db);
    drop(db);
    let (owner_c, _) = super::super::decision::decision_fixture_config(d.path());
    assert_eq!(
        config_apply(&owner_c, true).unwrap_err().to_string(),
        "planner_authority_owner_invalid"
    );
    let db = open_book(&owner_c).unwrap();
    assert_eq!(recovery_authority_snapshot(&db), before);
}
#[test]
fn resume_clears_operator_stop_with_audit_when_flat() {
    // A fork before exec retains the same open file description, even with
    // CLOEXEC. Model sandbox tests can fork while this test owns a lock.
    // A duplicate models that inheritance without spawning a process.
    let lock_dir = tempfile::tempdir().unwrap();
    let lock_config = super::super::tests::config(lock_dir.path());
    let owner = rollover::controller_lock(&lock_config).unwrap();
    let inherited = owner.0.try_clone().unwrap();
    assert!(rollover::controller_lock(&lock_config).is_err());
    drop(owner);
    let next = rollover::controller_lock(&lock_config).unwrap();
    drop(next);
    drop(inherited);

    // Related fault scenarios share this behavioral contract; each owns its fixtures.
    startup_preserves_active_and_sticky_block();
    halted_recovery_requires_block_proof();
    protection_only_retains_integrity_block();
    resume_resolves_active_session_book_and_keeps_safety_checks();
    recovery_owner_receipt_compatibility();
    // Recovery success is owner-only.
    closed_flat_planner_halt_is_recovered_without_financial_change();
    halted_model_led_book_enters_reviewed_protection_only_mode(false);
    stopped_recovery_retires_interrupted_pending_claim_without_replay();
    service_shutdown_continues_and_only_a_request_stops_the_book();
    halted_model_led_book_enters_reviewed_protection_only_mode(true);
    live_config_mismatch_suspends_planner_without_halting_the_book();
    planner_incident_never_halts_a_closed_book();
    let mut sealed = crate::runtime::tests::test_portfolio();
    sealed.closed = true;
    sealed.health = "closed".into();
    let before = serde_json::to_vec(&sealed).unwrap();
    assert!(stopped_restart_warning(&sealed).is_none());
    mark_operator_stopped(&mut sealed);
    assert_eq!(serde_json::to_vec(&sealed).unwrap(), before);

    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    p.health = "operator_stopped".into();
    persist(&mut db, &c, &p, start(), None, None, "operator_stopped").unwrap();
    drop(db);
    assert!(
        stopped_restart_warning(&p)
            .unwrap()
            .contains("resume --confirm-paper-review")
    );
    assert!(
        stopped_restart_warning(&p)
            .unwrap()
            .contains("expected_not_failure")
    );
    let mut halted = p.clone();
    halted.halted = true;
    assert!(
        !stopped_restart_warning(&halted)
            .unwrap()
            .contains("expected_not_failure")
    );
    resume(&c, true).unwrap();
    let mut db = open_book(&c).unwrap();
    let loaded = load_wide(&mut db, &c).unwrap();
    assert!(!loaded.stopped);
    assert!(stopped_restart_warning(&loaded).is_none());
    assert!(loaded.model_led);
    assert_eq!(loaded.health, "waiting");
    let events: Vec<String> = db
        .prepare("SELECT event FROM ticks ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e == "operator_resume_confirmed_paper_review")
    );
    assert!(resume(&c, true).is_err());
    assert!(resume(&c, false).is_err());
    // A planner-only halt can be rebound, but recovery itself never resumes.
    // Recovery success is owner-only; the legacy Decide path is refused in
    // production and covered by the explicit gate cases.
    drop(db);
    let (c, cfg) = super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    assert!(p.model_led);
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    p.halted = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    let attempts_before: i64 = db
        .query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    drop(db);
    assert!(planner_recover(&c, false).is_err());
    planner_recover(&c, true).unwrap();
    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        attempts_before
    );

    // A stopped data block has no automatic recovery, but explicit review
    // can recover after input validation; failure preserves state and charges.
    redecision::block(&mut db, &c, "planner_data_failed_no_replay").unwrap();
    let before: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    assert_eq!(
        planner_recover_checked(&c, true, false, |_, _, _| Err(
            "planner_recover_no_eligible_input".into()
        ))
        .unwrap_err()
        .to_string(),
        "planner_recover_no_eligible_input"
    );
    let db = open_book(&c).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        before
    );
    drop(db);
    let checked = std::cell::Cell::new(false);
    planner_recover_checked(&c, true, false, |_, _, _| {
        checked.set(true);
        Ok(())
    })
    .unwrap();
    assert!(checked.get());
    let mut db = open_book(&c).unwrap();
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        attempts_before
    );
    assert!(load_wide(&mut db, &c).unwrap().stopped);
    let clean_control: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    redecision::block(&mut db, &c, "attempt_interrupted_no_replay").unwrap();
    drop(db);
    assert_eq!(
        planner_recover_checked(&c, true, false, |_, _, _| panic!(
            "must not validate an ambiguous block"
        ))
        .unwrap_err()
        .to_string(),
        "planner_recover_stopped_block_not_eligible"
    );
    let mut db = open_book(&c).unwrap();
    // Restore only this injected ambiguous fixture before the next scenario.
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [&clean_control],
    )
    .unwrap();
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    drop(db);
    planner_recover_checked(&c, true, false, |_, _, _| Ok(())).unwrap();

    // A flat zero-fill book is recoverable only while its original planner
    // authority journal exists and is bound to this book.
    let missing_dir = tempfile::tempdir().unwrap();
    let mut missing = super::super::tests::config(missing_dir.path());
    let missing_config = missing_dir.path().join("planner.json");
    fs::write(&missing_config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    missing.planner_config = Some(missing_config);
    let mut missing_db = open_book(&missing).unwrap();
    let mut missing_p = load_wide(&mut missing_db, &missing).unwrap();
    redecision::initialize(&mut missing_db, &missing, &missing_p, Some(&cfg), start()).unwrap();
    redecision::block(
        &mut missing_db,
        &missing,
        "planner_event_configuration_changed",
    )
    .unwrap();
    missing_p.halted = true;
    missing_p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut missing_db,
        &missing,
        &missing_p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    drop(missing_db);
    let missing_db = Connection::open(missing.directory.join("paper.sqlite")).unwrap();
    missing_db
            .execute_batch(
                "DROP TABLE planner_event_control; DELETE FROM ticks WHERE event='event_planner_authority';",
            )
            .unwrap();
    let missing_state_before: String = missing_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    drop(missing_db);
    assert_eq!(
        planner_recover(&missing, true).unwrap_err().to_string(),
        "planner_recover_authority_missing"
    );
    let missing_db = Connection::open(missing.directory.join("paper.sqlite")).unwrap();
    let missing_state_after: String = missing_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let control_exists: bool = missing_db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_event_control')",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(missing_state_after, missing_state_before);
    assert!(!control_exists);
    drop(missing_db);

    // A present but mismatched control binding also refuses without
    // changing either financial state or the corrupt control evidence.
    let corrupt_dir = tempfile::tempdir().unwrap();
    let mut corrupt = super::super::tests::config(corrupt_dir.path());
    let corrupt_config = corrupt_dir.path().join("planner.json");
    fs::write(&corrupt_config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    corrupt.planner_config = Some(corrupt_config);
    let mut corrupt_db = open_book(&corrupt).unwrap();
    let mut corrupt_p = load_wide(&mut corrupt_db, &corrupt).unwrap();
    redecision::initialize(&mut corrupt_db, &corrupt, &corrupt_p, Some(&cfg), start()).unwrap();
    redecision::block(
        &mut corrupt_db,
        &corrupt,
        "planner_event_configuration_changed",
    )
    .unwrap();
    corrupt_p.halted = true;
    corrupt_p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut corrupt_db,
        &corrupt,
        &corrupt_p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    let mut control: serde_json::Value = corrupt_db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| {
                let body: String = r.get(0)?;
                Ok(serde_json::from_str(&body).unwrap())
            },
        )
        .unwrap();
    control["binding"] = serde_json::json!("mismatched-book-binding");
    let corrupt_control_before = serde_json::to_string(&control).unwrap();
    corrupt_db
        .execute(
            "UPDATE planner_event_control SET body=? WHERE id=1",
            [&corrupt_control_before],
        )
        .unwrap();
    let corrupt_state_before: String = corrupt_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    drop(corrupt_db);
    assert_eq!(
        planner_recover(&corrupt, true).unwrap_err().to_string(),
        "planner_event_binding_changed"
    );
    let corrupt_db = Connection::open(corrupt.directory.join("paper.sqlite")).unwrap();
    let corrupt_state_after: String = corrupt_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let corrupt_control_after: String = corrupt_db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(corrupt_state_after, corrupt_state_before);
    assert_eq!(corrupt_control_after, corrupt_control_before);
    drop(corrupt_db);

    // A risk halt is never recovered by the planner path.
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    p.halted = true;
    p.health = "daily_loss_tripwire".into();
    persist(&mut db, &c, &p, start(), None, None, "daily_loss_tripwire").unwrap();
    drop(db);
    assert!(planner_recover(&c, true).is_err());
}
#[test]
fn resume_refuses_missing_book_open_position_and_halt() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let error = resume(&c, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_book_missing");
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    p.halted = true;
    persist(&mut db, &c, &p, start(), None, None, "halted").unwrap();
    drop(db);
    let error = resume(&c, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_risk_halted");
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    assert!(
        p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    p.stopped = true;
    persist(&mut db, &c, &p, start(), None, None, "open").unwrap();
    drop(db);
    let error = resume(&c, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_open_positions");
    let loaded = load_wide(&mut open_book(&c).unwrap(), &c).unwrap();
    assert!(loaded.stopped);
    assert_eq!(loaded.positions.len(), 1);
}
fn resume_resolves_active_session_book_and_keeps_safety_checks() {
    let d = tempfile::tempdir().unwrap();
    let sessions = d.path().join("sessions");
    let legacy_dir = d.path().join("legacy");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&legacy_dir).unwrap();
    let mut legacy = super::super::tests::config(&legacy_dir);
    legacy.sessions_root = Some(sessions.clone());
    let mut db = open_book(&legacy).unwrap();
    let mut predecessor = crate::runtime::tests::test_portfolio();
    predecessor.stopped = true;
    predecessor.closed = true;
    predecessor.health = "operator_stopped".into();
    persist(
        &mut db,
        &legacy,
        &predecessor,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    drop(db);
    let active_dir = sessions.join("2026-10-22");
    fs::create_dir_all(&active_dir).unwrap();
    let capital = serde_json::json!(legacy.allocation().unwrap());
    rollover::atomic(
        &active_dir.join("strategy-decision.json"),
        &serde_json::json!({
            "reviewer": "none_model_led",
            "live_enabled": false,
            "chosen": {
                "strategy_id": "model_led_v1",
                "execution_selector": legacy.strategy,
                "capital": capital,
            },
            "capital": capital,
        }),
    )
    .unwrap();
    rollover::atomic(
        &sessions.join("current.json"),
        &serde_json::json!({"account": legacy.account, "session": "2026-10-22"}),
    )
    .unwrap();
    let active = rollover::current(&legacy).unwrap();
    assert_eq!(active.session, "2026-10-22".parse().unwrap());
    assert_eq!(active.previous_session, "2026-10-21".parse().unwrap());
    assert_ne!(active.directory, legacy.directory);
    let mut db = open_book(&active).unwrap();
    let mut book = crate::runtime::tests::test_portfolio();
    book.stopped = true;
    book.health = "operator_stopped".into();
    persist(
        &mut db,
        &active,
        &book,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    let cfg: planner::PlannerConfig = serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
    redecision::initialize(&mut db, &active, &book, Some(&cfg), start()).unwrap();
    let authority: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    resume(&legacy, true).unwrap();
    let mut db = open_book(&active).unwrap();
    let unchanged: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(authority, unchanged);
    let resumed = load_wide(&mut db, &active).unwrap();
    redecision::initialize(&mut db, &active, &resumed, Some(&cfg), start()).unwrap();
    assert!(!redecision::blocked(&db, &active).unwrap());
    let audits: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='operator_model_led_enabled_v1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audits, 1);
    drop(db);
    let resumed = load_wide(&mut open_book(&active).unwrap(), &active).unwrap();
    assert!(!resumed.stopped);
    assert!(resumed.model_led);
    assert!(!resumed.closed);
    assert!(!resumed.halted);
    assert!(resumed.positions.is_empty());
    assert_eq!(resumed.health, "waiting");
    let untouched = load_wide(&mut open_book(&legacy).unwrap(), &legacy).unwrap();
    assert!(untouched.stopped);
    assert!(untouched.closed);
    let db = open_book(&active).unwrap();
    // Sanctioned config changes preserve original usage and predecessor seals.
    let planner_path = d.path().join("planner.json");
    let mut edited = cfg.clone();
    edited.model_led = Some(true);
    edited.max_handoff_instruments -= 1;
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    legacy.planner_config = Some(planner_path.clone());
    assert!(config_apply(&legacy, false).is_err());
    drop(db);
    config_apply(&legacy, true).unwrap();
    let active_config = rollover::current(&legacy).unwrap();
    let mut db = open_book(&active_config).unwrap();
    let applied = load_wide(&mut db, &active_config).unwrap();
    assert!(applied.stopped && applied.model_led && !applied.halted);
    redecision::initialize(&mut db, &active_config, &applied, Some(&edited), start()).unwrap();
    assert!(!redecision::blocked(&db, &active_config).unwrap());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM ticks WHERE event='operator_planner_config_applied'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    // A carried model-led book adopts a planner change and keeps its holdings.
    let mut held = applied.clone();
    held.stopped = false;
    assert!(
        held.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    held.stopped = true;
    persist(
        &mut db,
        &active_config,
        &held,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    drop(db);
    edited.max_handoff_instruments -= 1;
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert_eq!(
        failure_code(resume(&legacy, true).unwrap_err().as_ref()),
        "resume_planner_config_not_applied"
    );
    config_apply(&legacy, true).unwrap();
    let mut db = open_book(&active_config).unwrap();
    let carried = load_wide(&mut db, &active_config).unwrap();
    assert_eq!(carried.positions.len(), 1);
    assert_eq!(carried.cash, held.cash);
    drop(db);
    edited.model_led = Some(false);
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert_eq!(
        config_apply(&legacy, true).unwrap_err().to_string(),
        "config_apply_book_not_flat"
    );
    let mut db = open_book(&active_config).unwrap();
    persist(
        &mut db,
        &active_config,
        &applied,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    edited.model_led = Some(true);
    // Explicit false is an intentional no-call policy, not a tamper incident.
    drop(db);
    edited.model_led = Some(false);
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    config_apply(&legacy, true).unwrap();
    resume(&legacy, true).unwrap();
    let mut db = open_book(&active_config).unwrap();
    let disabled = load_wide(&mut db, &active_config).unwrap();
    assert!(!disabled.model_led);
    redecision::initialize(&mut db, &active_config, &disabled, Some(&edited), start()).unwrap();
    assert!(!redecision::blocked(&db, &active_config).unwrap());
    let mut halted = applied.clone();
    halted.halted = true;
    halted.health = "risk_halt".into();
    persist(
        &mut db,
        &active_config,
        &halted,
        start(),
        None,
        None,
        "risk_halt",
    )
    .unwrap();
    drop(db);
    assert_eq!(
        config_apply(&legacy, true).unwrap_err().to_string(),
        "config_apply_halted_requires_planner_recover"
    );
    assert_eq!(
        planner_recover(&legacy, true).unwrap_err().to_string(),
        "planner_recover_halt_not_eligible_last_halt_not_planner_authority"
    );
    let mut db = open_book(&active).unwrap();
    let mut closed = resumed;
    closed.stopped = true;
    closed.closed = true;
    persist(
        &mut db,
        &active,
        &closed,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    drop(db);
    let error = resume(&legacy, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_session_closed");
    let active_book = load_wide(&mut open_book(&active).unwrap(), &active).unwrap();
    assert!(active_book.stopped);
    assert!(active_book.closed);
    let predecessor = load_wide(&mut open_book(&legacy).unwrap(), &legacy).unwrap();
    assert!(predecessor.stopped);
    assert!(predecessor.closed);
}
/// Event authority with the given plan as its active head, as the owner path
/// leaves it after an accepted decision.
fn activate_fixture_plan(db: &mut Connection, c: &Config, p: &Portfolio, plan: &serde_json::Value) {
    if !redecision::has_authority(db).unwrap() {
        let cfg: planner::PlannerConfig = serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
        ensure_actions(db).unwrap();
        redecision::initialize(db, c, p, Some(&cfg), start()).unwrap();
    }
    let mut plan = plan.clone();
    let id = plan["decision_id"].as_str().unwrap().to_owned();
    let attempt = format!("fixture-attempt-{id}");
    plan["decision_owner"] = serde_json::json!("pi");
    plan["attempt_id"] = serde_json::json!(attempt);
    let body = plan.to_string();
    db.execute(
        "INSERT INTO planner_plan VALUES(?,?)",
        rusqlite::params![id, body],
    )
    .unwrap();
    db.execute(
        "INSERT INTO planner_attempt_log VALUES(?,?,'{}','planned',?)",
        rusqlite::params![
            attempt,
            format!("fixture-event-{id}"),
            serde_json::json!({"decision_id":id,"plan_sha256":digest(body.as_bytes())}).to_string()
        ],
    )
    .unwrap();
    let control: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut control: serde_json::Value = serde_json::from_str(&control).unwrap();
    control["active"] = plan["decision_id"].clone();
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [control.to_string()],
    )
    .unwrap();
}
#[test]
fn planned_fills_commit_freshness_decision_and_exit_intents_atomically() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let i = instrument(1);
    let at = start();
    let input = serde_json::json!({"allowlist":[i]});
    let raw=serde_json::json!({"actions":[{"instrument_key":i.key,"side":"buy","units":2,"max_price":"101","rationale":"fixture","exit_intent":{"target":"110","stop":"90"}}],"stance":"long","notes":""}).to_string();
    let verdicts = planner::validate(&raw, &p, std::slice::from_ref(&i), at).unwrap();
    fs::create_dir_all(c.directory.join("model/fixture-stale")).unwrap();
    let mut plan = serde_json::json!({"decision_id":"fixture-stale","session_binding":book_binding(&c).unwrap(),"input_hash":digest(&serde_json::to_vec(&input).unwrap()),"input":input,"fake":false,"dry_run":false,"verdicts":verdicts});
    activate_fixture_plan(&mut db, &c, &p, &plan);
    let quotes = BTreeMap::from([(i.key.clone(), quote(at, 100))]);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at + 2001).unwrap();
    assert!(p.fills.is_empty());
    let mut expired = plan.clone();
    expired["decision_id"] = serde_json::json!("fixture-time-expired");
    expired["verdicts"][0]["action"]["exit_intent"]["time"] = serde_json::json!(at + 1);
    fs::create_dir_all(c.directory.join("model/fixture-time-expired")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &expired);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at + 1).unwrap();
    assert!(p.fills.is_empty());
    assert!(p.positions.is_empty());

    plan["decision_id"] = serde_json::json!("fixture-screened");
    fs::create_dir_all(c.directory.join("model/fixture-screened")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes_screened(
        &mut db,
        &c,
        &mut p,
        &quotes,
        at,
        "fixture",
        || at,
        Some(&BTreeSet::new()),
        false,
    )
    .unwrap();
    assert!(p.fills.is_empty() && !p.halted);
    let screened: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='fixture-screened'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(screened, "screen_rejected");
    // A1: an intent bound to a different book revision is refused without a
    // fill; a missing revision remains the legacy compatible behavior.
    plan["decision_id"] = serde_json::json!("fixture-stale-portfolio");
    plan["portfolio_revision"] = serde_json::json!("0".repeat(64));
    fs::create_dir_all(c.directory.join("model/fixture-stale-portfolio")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert!(p.fills.is_empty());
    let stale: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='fixture-stale-portfolio'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, "portfolio_changed");
    // A policy hash that cannot be revalidated is refused explicitly.
    plan["decision_id"] = serde_json::json!("fixture-policy-changed");
    plan["portfolio_revision"] = serde_json::json!(redecision::portfolio_hash(&p).unwrap());
    plan["policy_sha256"] = serde_json::json!("a".repeat(64));
    fs::create_dir_all(c.directory.join("model/fixture-policy-changed")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert!(p.fills.is_empty());
    let changed: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='fixture-policy-changed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(changed, "policy_changed");
    plan.as_object_mut().unwrap().remove("policy_sha256");
    plan["portfolio_revision"] = serde_json::json!(redecision::portfolio_hash(&p).unwrap());
    plan["decision_id"] = serde_json::json!("fixture-fresh");
    fs::create_dir_all(c.directory.join("model/fixture-fresh")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert_eq!(p.fills.len(), 1);
    assert_eq!(p.fills[0].decision_id.as_deref(), Some("fixture-fresh"));
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert_eq!(p.fills.len(), 1);
    let close = at + 360 * 60000;
    let q = BTreeMap::from([(i.key.clone(), quote(close, 103))]);
    commit_quotes(&mut db, &c, &mut p, &q, close, "fixture", || close).unwrap();
    assert_eq!(p.positions.len(), 1); // No legacy +2% or 15:15 flatten.
    let later = close + 60000;
    let q = BTreeMap::from([(i.key.clone(), quote(later, 111))]);
    // A stock excluded from new buys still receives its protective exit.
    commit_quotes_screened(
        &mut db,
        &c,
        &mut p,
        &q,
        later,
        "fixture",
        || later,
        Some(&BTreeSet::new()),
        false,
    )
    .unwrap();
    assert!(p.positions.is_empty());
    assert_eq!(p.fills[1].decision_id.as_deref(), Some("fixture-fresh"));
    p.reconcile().unwrap();
    assert!(
        c.directory
            .join("model/fixture-fresh/execution.json")
            .exists()
    );
}
#[test]
fn event_planning_suspends_old_actions_without_interrupting_protection() {
    for fault in [
        "none",
        "missing_control",
        "malformed_verdict",
        "malformed_allowlist",
        "audit_projection",
        "missing_actions",
        "observer",
    ] {
        let d = tempfile::tempdir().unwrap();
        let c = super::super::tests::config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = crate::runtime::tests::test_portfolio();
        let at = start();
        let i = instrument(1);
        let second = instrument(2);
        let raw=serde_json::json!({"actions":[{"instrument_key":i.key,"side":"buy","units":1,"max_price":"101","rationale":"fixture","exit_intent":{"target":"110"}},{"instrument_key":second.key,"side":"buy","units":1,"max_price":"101","rationale":"fixture","exit_intent":{}}],"stance":"long","notes":""}).to_string();
        let input = serde_json::json!({"allowlist":[i,second]});
        let plan = serde_json::json!({"decision_id":"fixture-predecessor","session_binding":book_binding(&c).unwrap(),"input_hash":digest(&serde_json::to_vec(&input).unwrap()),"input":input,"fake":false,"dry_run":false,"verdicts":planner::validate(&raw,&p,&[i.clone(),second.clone()],at).unwrap()});
        fs::create_dir_all(c.directory.join("model/fixture-predecessor")).unwrap();
        activate_fixture_plan(&mut db, &c, &p, &plan);
        commit_quotes(
            &mut db,
            &c,
            &mut p,
            &BTreeMap::from([(i.key.clone(), quote(at, 100))]),
            at,
            "fixture",
            || at,
        )
        .unwrap();
        assert_eq!(p.positions.len(), 1);
        assert_eq!(p.fills.len(), 1);
        let cfg:planner::PlannerConfig=serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
        let claim = redecision::pending_fixture(&mut db, &c, &p, at + 1000);
        assert_eq!(claim.predecessor.as_deref(), Some("fixture-predecessor"));
        if fault == "missing_control" {
            db.execute_batch("DELETE FROM planner_event_control;")
                .unwrap();
        }
        if [
            "malformed_verdict",
            "malformed_allowlist",
            "audit_projection",
        ]
        .contains(&fault)
        {
            // A failed digest settles the claim without changing the head.
            let mut current = plan.clone();
            current["decision_id"] = serde_json::json!("fixture-successor");
            if fault == "audit_projection" {
                current["verdicts"] = serde_json::json!([]);
            }
            redecision::fail_pending_fixture(&mut db, &claim);
            // A newer accepted plan is the active head; its damaged evidence
            // is read on the next tick.
            db.execute(
                "INSERT INTO planner_plan VALUES(?,?)",
                rusqlite::params!["fixture-successor", current.to_string()],
            )
            .unwrap();
            let control: String = db
                .query_row(
                    "SELECT body FROM planner_event_control WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let mut control: serde_json::Value = serde_json::from_str(&control).unwrap();
            control["active"] = serde_json::json!("fixture-successor");
            db.execute(
                "UPDATE planner_event_control SET body=? WHERE id=1",
                [control.to_string()],
            )
            .unwrap();
            if fault == "malformed_verdict" {
                current["verdicts"] = serde_json::json!([{}]);
            }
            if fault == "malformed_allowlist" {
                current["input"]["allowlist"][0]["tick"] = serde_json::json!("not a price");
                current["input_hash"] =
                    serde_json::json!(digest(&serde_json::to_vec(&current["input"]).unwrap()));
            }
            db.execute(
                "UPDATE planner_plan SET body=? WHERE id=?",
                rusqlite::params![
                    current.to_string(),
                    current["decision_id"].as_str().unwrap()
                ],
            )
            .unwrap();
            if fault == "audit_projection" {
                let path = c
                    .directory
                    .join("model")
                    .join(current["decision_id"].as_str().unwrap());
                fs::create_dir_all(path.join("execution.tmp")).unwrap();
            }
        }
        if fault == "missing_actions" {
            db.execute_batch("DROP TABLE planner_action;").unwrap();
        }
        if fault == "observer" {
            let mut settings = Some(cfg.clone());
            let mut settings_fallback = None;
            let mut settings_reason = None;
            contain_event_error(
                Err("fixture-invalid-event-state".into()),
                &mut db,
                &c,
                &mut p,
                &mut settings,
                &mut settings_fallback,
                &mut settings_reason,
                at + 1200,
            )
            .unwrap();
            assert!(settings.is_none());
            assert!(settings_fallback.is_some());
            assert_eq!(settings_reason, Some("planner_integrity_failure"));
        }
        let later = at + 1500;
        let quotes = BTreeMap::from([
            (i.key.clone(), quote(later, 111)),
            (second.key.clone(), quote(later, 100)),
        ]);
        commit_quotes(&mut db, &c, &mut p, &quotes, later, "fixture", || later).unwrap();
        assert!(p.positions.is_empty(), "{fault}");
        assert_eq!(p.fills.len(), 2);
        assert!(!p.fills[1].buy);
        assert_eq!(p.halted, fault != "none");
        if fault == "missing_actions" {
            assert!(
                !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='planner_action')",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
            );
        } else {
            let rows:i64=db.query_row("SELECT count(*) FROM planner_action WHERE decision='fixture-predecessor' AND idx=1",[],|r|r.get(0)).unwrap();
            assert_eq!(rows, 0); // Its second action never executes while suspended or damaged.
        }
        p.reconcile().unwrap();
        let saved = load_wide(&mut db, &c).unwrap();
        assert_eq!(p, saved);
    }
}
