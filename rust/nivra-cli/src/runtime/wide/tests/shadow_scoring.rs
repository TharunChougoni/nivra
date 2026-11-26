use super::*;

#[test]
fn shadow_inputs_use_receipt_minutes_and_skip_missing_references() {
    assert_eq!(
        super::super::controller::research_context_status(true)["reason"],
        "controller_session_end_unavailable"
    );
    assert_eq!(
        super::super::controller::research_context_status(true)["action"],
        "none"
    );
    assert_eq!(
        super::super::controller::research_context_status(false)["status"],
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
    let settings: super::super::discovery::Settings = serde_json::from_value(serde_json::json!({
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
    assert_eq!(store.bars("available").unwrap()[0].start_minute(), 555);
    assert_eq!(store.status()["bars_total"], 3);
    let points = BTreeMap::from([(
        instrument.key.clone(),
        super::super::super::market_feed::Point {
            price: Price::parse("130").unwrap(),
            source: now + 180_000,
            receipt: now + 180_000,
            last_trade: now + 180_000,
            prior_close: None,
        },
    )]);
    super::super::super::market_feed::observe_cache(
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
            .start_minute(),
        558
    );
    assert_eq!(store.market_minute_returns(1).len(), 1); // cap retains three bars
    assert_eq!(store.status()["completed_minutes"], 1);
    let defaults: super::super::discovery::Settings = serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,"feature_store":{}
    }))
    .unwrap();
    assert_eq!(defaults.feature_store, Default::default());
    for invalid in [
        serde_json::json!({"max_bars":1}),
        serde_json::json!({"min_instruments":0}),
    ] {
        let cfg: super::super::discovery::Settings = serde_json::from_value(serde_json::json!({
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
    let before = super::super::super::discovery::project(&scan, &[], &settings, now);
    scan.samples.get_mut("available").unwrap().regime_references = None;
    assert_eq!(
        super::super::super::discovery::project(&scan, &[], &settings, now),
        before
    );

    // Metadata-only LTPC scans still populate regime_references when strategy_selection is enabled.
    let mut meta_scan = Scanner::default();
    let meta_rows = serde_json::from_value(serde_json::json!({
        "row": {
            "instrument_token": instrument.key,
            "symbol": instrument.symbol,
            "lower_circuit_limit": 90.0,
            "upper_circuit_limit": 120.0,
            "volume": 10_000,
            "ohlc": {"open": 105.0, "close": 100.0}
        }
    }))
    .unwrap();
    scan_rest_rows(
        &mut meta_scan,
        std::slice::from_ref(&instrument),
        &meta_rows,
        now,
        now,
        2000,
        true,
        None,
    )
    .unwrap();
    let mut selector_policy: super::super::planner::PlannerConfig =
        serde_json::from_value(serde_json::json!({
            "enabled": true,
            "discovery": {
                "max_scan_age_ms": 2000,
                "max_prospects": 10,
                "strategy_selection": {"enabled": true, "library_path": "/fixture/library"}
            }
        }))
        .unwrap();
    update_regime_references(&mut meta_scan, &meta_rows, now, Some(&selector_policy));
    assert_eq!(
        meta_scan.samples[&instrument.key].regime_references,
        Some((
            now,
            Price::parse("100").unwrap(),
            Price::parse("105").unwrap()
        ))
    );
    meta_scan
        .samples
        .get_mut(&instrument.key)
        .unwrap()
        .regime_references = None;
    selector_policy
        .discovery
        .as_mut()
        .unwrap()
        .strategy_selection = None;
    update_regime_references(&mut meta_scan, &meta_rows, now, Some(&selector_policy));
    assert_eq!(meta_scan.samples[&instrument.key].regime_references, None);

    // Non-tradeable index context reads the same feed points; it never enters
    // the cash universe, scanner screens or paper execution.
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::super::tests::config(dir.path());
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
            super::super::super::market_feed::Point {
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
    let mut settings: super::super::super::discovery::Settings =
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
    let projection = super::super::super::discovery::project(&scan, &instruments, &settings, now);
    assert!(!super::super::super::discovery::candidates(&projection).is_empty());
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
        super::super::super::discovery::project(&scan, &instruments, &settings, now),
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
            serde_json::from_value::<super::super::super::discovery::Settings>(bad)
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
    let settings: super::super::super::discovery::Settings =
        serde_json::from_value(serde_json::json!({
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
    let projection = super::super::super::discovery::project(&scan, &[], &settings, start());
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
            serde_json::from_value::<super::super::super::discovery::Settings>(bad)
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
