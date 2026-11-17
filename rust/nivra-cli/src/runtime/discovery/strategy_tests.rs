use super::*;
use nivra_core::{execution::Sample, finance::Price};
#[test]
fn champion_requires_exact_qualified_owner_approval() {
    use nivra_store::strategy_results::{
        Evidence, EvidenceKind, Proposal, Qualification, ResultsDb, proposal_hash,
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("results.sqlite");
    let mut settings: Settings = serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":10,"strategy":{"enabled":true,"champion":"m","specs":[{"id":"m","version":1,"filters":[],"weights":[{"feature":"move_bps","weight":1}],"shortlist_size":1,"trigger_score":0,"cooldown_ms":0}]}})).unwrap();
    assert!(settings.validate().is_err());
    settings.strategy_results_database = Some(path.clone());
    let db = ResultsDb::open(&path).unwrap();
    let spec = settings.strategy.as_ref().unwrap().specs[0].clone();
    let proposal = Proposal {
        id: "reviewed".into(),
        parent_id: None,
        source_hash: proposal_hash(&spec).unwrap(),
        rationale: "recorded hypothesis".into(),
        spec,
        scorer: None,
    };
    db.propose(&proposal).unwrap();
    assert!(settings.validate().is_err());
    db.record_qualification("reviewed", true, 4).unwrap();
    db.record(&Evidence {
        id: "replay-approved".into(),
        kind: EvidenceKind::Replay,
        observed_ms: 3,
        spec_id: format!("{}:{}", proposal.spec.id, proposal.spec.version),
        scan_id: None,
        digest_id: None,
        decide_id: None,
        body: json!({"spec_hash": proposal.source_hash, "failed_ticks": 0}),
    })
    .unwrap();
    db.record_qualification_evidence(
        "reviewed",
        &Qualification {
            id: "qualification-approved".into(),
            replay_id: "replay-approved".into(),
            champion: proposal.spec.clone(),
            sample_count: 100,
            out_of_sample_count: 30,
            training_end_ms: 1,
            holdout_start_ms: 2,
            holdout_end_ms: 3,
            candidate_return_bps: 10.0,
            champion_return_bps: 0.0,
        },
    )
    .unwrap();
    db.approve("reviewed", "Tharun", "explicit-owner-record", true, 4)
        .unwrap();
    assert!(settings.validate().is_err());
    db.record_qualification("reviewed", true, 5).unwrap();
    settings.validate().unwrap();
    let mut shadow = settings.strategy.as_ref().unwrap().specs[0].clone();
    shadow.id = "unpromoted_shadow".into();
    shadow.weights[0].weight = -1.0;
    settings.strategy.as_mut().unwrap().specs.push(shadow);
    settings.validate().unwrap();
    settings.strategy.as_mut().unwrap().specs[0].weights[0].weight = 2.0;
    assert!(settings.validate().is_err());
    settings.strategy.as_mut().unwrap().enabled = false;
    settings.validate().unwrap();
}
#[test]
fn champion_replaces_fixed_screen_and_shadow_cannot_trigger() {
    let mut scan = Scanner::default();
    scan.samples.insert(
        "A".into(),
        Sample {
            seen: 1000,
            source: Some(1000),
            last: 1000,
            current: Some(Price::parse("110").unwrap()),
            history: vec![Price::parse("100").unwrap(), Price::parse("110").unwrap()],
            reason: "low_volume".into(),
            ..Sample::default()
        },
    );
    let instruments = vec![Instrument {
        key: "A".into(),
        symbol: "A".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }];
    let mut settings:Settings=serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":10,"strategy":{"enabled":true,"champion":"champion","specs":[{"id":"champion","version":1,"filters":[],"weights":[{"feature":"move_bps","weight":1}],"shortlist_size":1,"trigger_score":50,"cooldown_ms":1234},{"id":"shadow","version":2,"filters":[],"weights":[{"feature":"move_bps","weight":-1}],"shortlist_size":1,"trigger_score":0,"cooldown_ms":9999}]}})).unwrap();
    settings.strategy.as_ref().unwrap().validate().unwrap();
    let result = project(&scan, &instruments, &settings, 1001);
    assert_eq!(result["prospects"][0]["strategy_spec_id"], "champion");
    let traced = attribution(&result);
    assert_eq!(traced["spec_version"], 1);
    assert_eq!(
        traced["scan_id"],
        result["strategy_snapshots"][0]["scan_id"]
    );
    assert_eq!(
        traced["scan_id"],
        result["prospects"][0]["strategy_scan_id"]
    );
    assert_eq!(traced["spec_hash"].as_str().unwrap().len(), 64);
    assert!(attribution(&json!({})).is_null());
    assert_eq!(result["strategy_snapshots"].as_array().unwrap().len(), 2);
    assert_eq!(candidates(&result), vec![("A".into(), Some(1.0))]);
    settings.strategy.as_mut().unwrap().champion = "shadow".into();
    let result = project(&scan, &instruments, &settings, 1001);
    assert_eq!(result["prospects"].as_array().unwrap().len(), 0);
    assert_eq!(candidates(&result), vec![("A".into(), Some(0.0))]);
    settings.strategy.as_mut().unwrap().enabled = false;
    assert_eq!(
        project(&scan, &instruments, &settings, 1001)["prospects"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    settings.strategy.as_mut().unwrap().enabled = true;
    assert_eq!(
        candidates(&project(&scan, &instruments, &settings, 4001)),
        vec![("A".into(), None)]
    );
    assert_eq!(
        project(&scan, &instruments, &settings, 4001)["prospects"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // The recommended spec reproduces the legacy two-sided shortlist and
    // trigger set, including a quiet prospect that stays on the shortlist.
    let instruments: Vec<_> = ["A", "B", "C"]
        .into_iter()
        .map(|key| Instrument {
            key: key.into(),
            symbol: key.into(),
            ..instruments[0].clone()
        })
        .collect();
    let mut legacy_scan = Scanner::default();
    for (key, price) in [("A", "103"), ("B", "96"), ("C", "100.5")] {
        legacy_scan.samples.insert(
            key.into(),
            Sample {
                seen: 1000,
                source: Some(1000),
                last: 1000,
                current: Some(Price::parse(price).unwrap()),
                history: vec![Price::parse("100").unwrap(), Price::parse(price).unwrap()],
                reason: "eligible_no_signal".into(),
                market_fields: Some(nivra_core::execution::MarketFields {
                    session_vwap: None,
                    best_bid: None,
                    best_bid_qty: None,
                    best_ask: None,
                    best_ask_qty: None,
                    total_buy_quantity: None,
                    total_sell_quantity: None,
                    lower_circuit_limit: nivra_core::finance::Amount::parse("80").unwrap(),
                    upper_circuit_limit: nivra_core::finance::Amount::parse("120").unwrap(),
                    volume: 100_000,
                }),
                market_fields_receipt: Some(1000),
                ..Sample::default()
            },
        );
    }
    settings.strategy = None;
    let old = project(&legacy_scan, &instruments, &settings, 1001);
    let spec = nivra_core::strategy::StrategySpec::default();
    settings.strategy = Some(nivra_core::strategy::StrategySettings {
        enabled: true,
        champion: spec.id.clone(),
        specs: vec![spec],
        ..Default::default()
    });
    let new = project(&legacy_scan, &instruments, &settings, 1001);
    let keys = |projection: &Value| {
        projection["prospects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["instrument_key"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&old), ["B", "A", "C"]);
    assert_eq!(keys(&old), keys(&new));
    assert_eq!(
        candidates(&new),
        vec![
            ("B".into(), Some(1.0)),
            ("A".into(), Some(1.0)),
            ("C".into(), Some(0.0))
        ]
    );
    // A removed cache key cannot affect ranks or market-relative features.
    let mut outside = legacy_scan.samples["A"].clone();
    outside.current = Some(Price::parse("999").unwrap());
    legacy_scan
        .samples
        .insert("outside_universe".into(), outside);
    assert_eq!(
        new["strategy_snapshots"],
        project(&legacy_scan, &instruments, &settings, 1001)["strategy_snapshots"]
    );
    let mut recent = settings.strategy.as_ref().unwrap().specs[0].clone();
    recent.id = "recent".into();
    recent.lookback_samples = 1;
    settings.strategy.as_mut().unwrap().specs.push(recent);
    let windows = project(&legacy_scan, &instruments, &settings, 1001);
    assert_eq!(
        windows["strategy_snapshots"][0],
        new["strategy_snapshots"][0]
    );
    assert!(
        windows["strategy_snapshots"][1]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["features"]["move_bps"] == 0.0)
    );
    assert_eq!(legacy_scan.samples["A"].history.len(), 2);
}
