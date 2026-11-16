use super::*;
/// #48: compare the default projection with spec evaluation and a frozen
/// golden ranking without opening a ledger or calling a provider.
#[test]
fn default_spec_matches_existing_warmed_scanner_ranking() {
    use sadr_core::{
        execution::{MarketFields, Sample},
        finance::{Amount, Price},
        strategy,
    };
    let now = 10_000;
    let mut scan = Scanner::default();
    let mut instruments = Vec::new();
    // Exact binary ratios make the tie intentional, rather than a rounded
    // display tie. Input order is deliberately opposite the key tie-break.
    for (key, price, volume, receipt) in [
        ("Z_up", "125", 100_000, now),
        ("A_down", "75", 100_000, now),
        ("quiet", "100", 100_000, now),
        ("small", "100.5", 100_000, now),
        ("illiquid", "150", 1, now),
        ("cheap", "5", 1_000_000, now),
        ("circuit", "199", 100_000, now),
        ("stale", "150", 100_000, now - 2001),
        ("future", "150", 100_000, now + 1),
    ] {
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
                seen: receipt,
                source: Some(receipt),
                last: receipt,
                current: Some(current),
                history: vec![Price::parse("100").unwrap(), current],
                reason: "eligible_no_signal".into(),
                market_fields: Some(MarketFields {
                    session_vwap: None,
                    best_bid: Some(99.0),
                    best_bid_qty: Some(10),
                    best_ask: Some(101.0),
                    best_ask_qty: Some(20),
                    total_buy_quantity: Some(100),
                    total_sell_quantity: Some(200),
                    lower_circuit_limit: Amount::parse("1").unwrap(),
                    upper_circuit_limit: Amount::parse("200").unwrap(),
                    volume,
                }),
                market_fields_receipt: Some(receipt),
                ..Sample::default()
            },
        );
    }
    let input = strategy::extract_features(&scan, now, 2000);
    let original = serde_json::to_value(&scan).unwrap();
    let universe: Vec<_> = instruments.iter().map(|i| i.key.clone()).collect();
    for limit in [1, 3, 10] {
        let settings: Settings = serde_json::from_value(json!({
            "max_scan_age_ms":2000,"max_prospects":limit
        }))
        .unwrap();
        let legacy = project(&scan, &instruments, &settings, now);
        let mut rejected = settings.clone();
        rejected.strategy = Some(
            serde_json::from_value(json!({
                "enabled":true,"champion":"missing","specs":[]
            }))
            .unwrap(),
        );
        assert_eq!(project(&scan, &instruments, &rejected, now), legacy);
        rejected.strategy.as_mut().unwrap().specs = vec![Default::default()];
        rejected.strategy.as_mut().unwrap().champion = "legacy_scanner".into();
        rejected
            .strategy
            .as_mut()
            .unwrap()
            .admission_bounds
            .max_weights = 0;
        assert_eq!(project(&scan, &instruments, &rejected, now), legacy);
        let spec = strategy::StrategySpec {
            shortlist_size: limit,
            ..Default::default()
        };
        // Exercise the actual serialized schema as an offline operator would.
        let spec = serde_json::from_value(serde_json::to_value(spec).unwrap()).unwrap();
        let evaluated = strategy::evaluate(&spec, &input).unwrap();
        let compatible =
            strategy::evaluate_default_scanner(&spec, &scan, &universe, now, 2000).unwrap();
        assert_eq!(
            compatible
                .rows
                .iter()
                .filter(|r| r.selected)
                .map(|r| (&r.key, r.score))
                .collect::<Vec<_>>(),
            evaluated
                .rows
                .iter()
                .filter(|r| r.selected)
                .map(|r| (&r.key, r.score))
                .collect::<Vec<_>>()
        );
        let selected: Vec<_> = evaluated
            .rows
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.key.as_str())
            .collect();
        let legacy_rows = legacy["prospects"].as_array().unwrap();
        let keys: Vec<_> = legacy_rows
            .iter()
            .map(|r| r["instrument_key"].as_str().unwrap())
            .collect();
        assert_eq!(selected, keys);
        let golden = ["A_down", "Z_up", "small", "quiet"];
        assert_eq!(selected, golden[..limit.min(golden.len())]);
        for (row, old) in evaluated
            .rows
            .iter()
            .filter(|r| r.selected)
            .zip(legacy_rows)
        {
            assert_eq!(
                row.score.unwrap(),
                old["sample_change_bps"].as_f64().unwrap().abs()
            );
            assert_eq!(row.trigger_eligible, Some(row.score.unwrap() >= 200.0));
        }
    }
    assert_eq!(serde_json::to_value(&scan).unwrap(), original);
}

#[test]
fn default_spec_exact_legacy_edges() {
    use sadr_core::{
        execution::{MarketFields, Sample},
        finance::{Amount, Price},
        strategy,
    };
    let now = 10_000;
    let mut scan = Scanner::default();
    let mut instruments = Vec::new();
    // Differences smaller than one f64 ULP deliberately exercise exact decimal
    // admission rather than merely testing displayed/rounded boundaries.
    for (key, price, volume, lower, upper) in [
        ("price_at", "10", 200_000, "1", "200"),
        ("price_above", "10.000000000000000001", 200_000, "1", "200"),
        ("price_below", "9.999999999999999999", 200_000, "1", "200"),
        ("value_at", "100", 10_000, "1", "200"),
        ("value_above", "100.000000000000000001", 10_000, "1", "200"),
        ("value_below", "99.999999999999999999", 10_000, "1", "200"),
        ("lower_at", "100", 100_000, "99", "200"),
        (
            "lower_above",
            "100",
            100_000,
            "98.999999999999999999",
            "200",
        ),
        (
            "lower_below",
            "100",
            100_000,
            "99.000000000000000001",
            "200",
        ),
        ("upper_at", "100", 100_000, "1", "101"),
        ("upper_above", "100", 100_000, "1", "101.000000000000000001"),
        ("upper_below", "100", 100_000, "1", "100.999999999999999999"),
        ("empty", "100", 100_000, "1", "200"),
        ("one", "100", 100_000, "1", "200"),
        ("stale_metadata", "125", 100_000, "1", "200"),
        ("missing_metadata", "150", 100_000, "1", "200"),
        ("no_metadata", "150", 100_000, "1", "200"),
        ("diagnostic", "125", 100_000, "1", "200"),
        ("diagnostic_invalid", "150", 100_000, "1", "200"),
        ("unseen", "150", 100_000, "1", "200"),
        ("stale_reason", "150", 100_000, "1", "200"),
        ("future_source", "150", 100_000, "1", "200"),
        ("receipt_at", "100", 100_000, "1", "200"),
        ("receipt_beyond", "150", 100_000, "1", "200"),
        ("old_universe", "175", 100_000, "1", "200"),
    ] {
        if key != "old_universe" {
            instruments.push(Instrument {
                key: key.into(),
                symbol: key.into(),
                series: "EQ".into(),
                tick: Price::parse("0.05").unwrap(),
            });
        }
        let current = Price::parse(price).unwrap();
        let mut sample = Sample {
            seen: now,
            source: Some(now),
            last: now,
            current: Some(current),
            history: vec![current],
            reason: "eligible_no_signal".into(),
            market_fields: Some(MarketFields {
                session_vwap: None,
                best_bid: Some(99.0),
                best_bid_qty: Some(10),
                best_ask: Some(101.0),
                best_ask_qty: Some(20),
                total_buy_quantity: Some(100),
                total_sell_quantity: Some(200),
                volume,
                lower_circuit_limit: Amount::parse(lower).unwrap(),
                upper_circuit_limit: Amount::parse(upper).unwrap(),
            }),
            market_fields_receipt: Some(now),
            ..Default::default()
        };
        match key {
            "empty" => sample.history.clear(),
            "stale_metadata" => {
                sample.history.insert(0, Price::parse("100").unwrap());
                sample.market_fields_receipt = Some(1);
            }
            "missing_metadata" => sample.reason = "missing_market_fields".into(),
            "no_metadata" => sample.market_fields = None,
            "diagnostic" => {
                sample.diagnostic = Some("symbol_mismatch".into());
                sample.history.insert(0, Price::parse("100").unwrap());
            }
            "diagnostic_invalid" => {
                sample.diagnostic = Some("invalid".into());
                sample.reason = "invalid_quote".into();
            }
            "unseen" => sample.seen = 0,
            "stale_reason" => sample.reason = "stale".into(),
            "future_source" => sample.source = Some(now + 1),
            "receipt_at" => {
                sample.seen = now - 2000;
                sample.source = Some(now - 2000);
            }
            "receipt_beyond" => sample.seen = now - 2001,
            _ => (),
        }
        scan.samples.insert(key.into(), sample);
    }
    instruments.push(Instrument {
        key: "absent".into(),
        symbol: "absent".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    });
    let universe: Vec<_> = instruments.iter().map(|i| i.key.clone()).collect();
    let original = serde_json::to_vec(&scan).unwrap();
    let golden = [
        "diagnostic",
        "stale_metadata",
        "empty",
        "lower_above",
        "lower_at",
        "one",
        "price_above",
        "price_at",
        "receipt_at",
        "upper_above",
        "upper_at",
        "value_above",
        "value_at",
    ];
    for limit in [1, 3, 10, 200] {
        let settings: Settings =
            serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":limit})).unwrap();
        let legacy = project(&scan, &instruments, &settings, now);
        let mut rejected = settings.clone();
        rejected.strategy = Some(
            serde_json::from_value(json!({
                "enabled":true,"champion":"missing","specs":[]
            }))
            .unwrap(),
        );
        assert_eq!(project(&scan, &instruments, &rejected, now), legacy);
        rejected.strategy.as_mut().unwrap().specs = vec![Default::default()];
        rejected.strategy.as_mut().unwrap().champion = "legacy_scanner".into();
        rejected
            .strategy
            .as_mut()
            .unwrap()
            .admission_bounds
            .max_weights = 0;
        assert_eq!(project(&scan, &instruments, &rejected, now), legacy);
        let spec: strategy::StrategySpec = serde_json::from_value(
            serde_json::to_value(strategy::StrategySpec {
                shortlist_size: limit,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        let result =
            strategy::evaluate_default_scanner(&spec, &scan, &universe, now, 2000).unwrap();
        let selected: Vec<_> = result
            .rows
            .iter()
            .filter(|r| r.selected)
            .map(|r| r.key.as_str())
            .collect();
        let old_rows = legacy["prospects"].as_array().unwrap();
        let old_keys: Vec<_> = old_rows
            .iter()
            .map(|r| r["instrument_key"].as_str().unwrap())
            .collect();
        assert_eq!(selected, old_keys);
        assert_eq!(selected, golden[..limit.min(golden.len())]);
        assert_eq!(result.rows.len(), universe.len());
        assert!(!result.rows.iter().any(|r| r.key == "old_universe"));
        for (new, old) in result.rows.iter().filter(|r| r.selected).zip(old_rows) {
            assert_eq!(
                new.score,
                Some(old["sample_change_bps"].as_f64().unwrap_or(0.0).abs())
            );
            assert_eq!(new.trigger_eligible, Some(new.score.unwrap() >= 200.0));
        }
        for key in [
            "price_below",
            "value_below",
            "lower_below",
            "upper_below",
            "missing_metadata",
            "no_metadata",
            "diagnostic_invalid",
            "unseen",
            "stale_reason",
            "future_source",
            "receipt_beyond",
            "absent",
        ] {
            let row = result.rows.iter().find(|r| r.key == key).unwrap();
            assert!(!row.selected && row.score.is_none(), "{key}");
        }
    }
    assert_eq!(serde_json::to_vec(&scan).unwrap(), original);
    assert!(
        strategy::evaluate_default_scanner(
            &strategy::StrategySpec::default(),
            &scan,
            &["duplicate".into(), "duplicate".into()],
            now,
            2000
        )
        .is_err()
    );
    let mut bad = strategy::StrategySpec::default();
    bad.weights[0].weight = -1.0;
    assert!(strategy::evaluate_default_scanner(&bad, &scan, &universe, now, 2000).is_err());
}
