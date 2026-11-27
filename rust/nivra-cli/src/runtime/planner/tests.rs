/// Sealed dispositions bind the planner policy hash; upgrading the binary must not
/// add default-valued discovery blocks to the serialized policy (#66).
#[test]
fn default_discovery_blocks_do_not_change_policy_bytes() {
    let cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "discovery": {"max_scan_age_ms": 180000, "max_prospects": 40,
            "market_feed": {"mode": "ltpc", "token_check_ms": 1000}}
    }))
    .unwrap();
    let bytes = String::from_utf8(super::policy_bytes(&cfg).unwrap()).unwrap();
    assert!(!bytes.contains("prefilter"), "{bytes}");
    assert!(!bytes.contains("feature_store"), "{bytes}");
    for added in [
        "connect_timeout_ms",
        "fresh_drop_alert_fraction",
        "handshake_io_timeout_ms",
        "max_connect_addresses",
        "metadata_preload_max_age_ms",
        "read_poll_timeout_ms",
        "scan_activity_timeout_ms",
        "telemetry_window_ms",
        "worker_idle_sleep_ms",
    ] {
        assert!(!bytes.contains(added), "{added}: {bytes}");
    }
    assert!(
        bytes.contains("token_check_ms"),
        "pre-existing fields keep serializing"
    );
    let mut changed = cfg.clone();
    changed.discovery.as_mut().unwrap().prefilter.enabled = true;
    changed.discovery.as_mut().unwrap().feature_store.max_bars = 10;
    let bytes = String::from_utf8(super::policy_bytes(&changed).unwrap()).unwrap();
    assert!(bytes.contains("prefilter") && bytes.contains("feature_store"));
}
#[test]
fn shadow_selector_does_not_change_trading_policy() {
    let mut cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "discovery":{"max_scan_age_ms":180000,"max_prospects":40}
    }))
    .unwrap();
    let original = super::policy_bytes(&cfg).unwrap();
    cfg.discovery.as_mut().unwrap().strategy_selection = Some(
        serde_json::from_value(
            serde_json::json!({"enabled":true,"library_path":"/shadow/library"}),
        )
        .unwrap(),
    );
    assert_eq!(super::policy_bytes(&cfg).unwrap(), original);
    cfg.discovery.as_mut().unwrap().max_prospects += 1;
    assert_ne!(super::policy_bytes(&cfg).unwrap(), original);
}
#[test]
fn live_selector_mode_is_bound_by_the_trading_policy_hash() {
    let mut cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "discovery":{"max_scan_age_ms":180000,"max_prospects":40}
    }))
    .unwrap();
    let absent = super::policy_bytes(&cfg).unwrap();
    cfg.discovery.as_mut().unwrap().strategy_selection = Some(
        serde_json::from_value(serde_json::json!({"enabled":true,"library_path":"/live/library"}))
            .unwrap(),
    );
    // An old config that omits `mode` is still shadow and keeps the legacy hash.
    assert_eq!(super::policy_bytes(&cfg).unwrap(), absent);
    cfg.discovery
        .as_mut()
        .unwrap()
        .strategy_selection
        .as_mut()
        .unwrap()
        .mode = super::discovery::StrategySelectionMode::Live;
    let live = super::policy_bytes(&cfg).unwrap();
    assert_ne!(live, absent);
    let text = String::from_utf8(live).unwrap();
    assert!(text.contains("\"strategy_selection\""));
    assert!(text.contains("\"mode\":\"live\""));
    // Flipping back must restore byte-identical legacy policy.
    cfg.discovery
        .as_mut()
        .unwrap()
        .strategy_selection
        .as_mut()
        .unwrap()
        .mode = super::discovery::StrategySelectionMode::Shadow;
    assert_eq!(super::policy_bytes(&cfg).unwrap(), absent);
}
use super::*;
use nivra_core::execution::ExitIntent;

#[test]
fn plan_validation_reserves_cash_and_holdings() {
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let mut p = crate::runtime::tests::test_portfolio();
    let at = DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis();
    let action = |key: &str, side: &str, units: u64| {
        let exit = if side == "buy" {
            json!({"stop":"90"})
        } else {
            json!({})
        };
        json!({"instrument_key":key,"side":side,"units":units,"max_price":"100","rationale":"fixture","exit_intent":exit})
    };
    let plan = json!({"actions":[action("absent","buy",1),action(&i.key,"buy",50),action(&i.key,"sell",1),action(&i.key,"buy",2)],"stance":"test","notes":""});
    let v = validate(&plan.to_string(), &p, std::slice::from_ref(&i), at).unwrap();
    assert_eq!(
        v.iter().map(|v| v.reason.as_str()).collect::<Vec<_>>(),
        vec![
            "action_allowlist",
            "sleeve_cash_insufficient",
            "action_sell_unheld",
            "accepted"
        ]
    );
    assert!(validate("not JSON", &p, std::slice::from_ref(&i), at).is_err());
    let abstain = json!({"actions":[],"stance":"abstain","notes":"no trade"});
    assert!(
        validate(&abstain.to_string(), &p, std::slice::from_ref(&i), at)
            .unwrap()
            .is_empty()
    );
    let with_reasons = json!({"actions":[],"stance":"abstain","notes":"wait",
            "candidate_reasons":[{"instrument_key":i.key,"reason":"spread too wide"}]});
    assert!(
        validate(&with_reasons.to_string(), &p, std::slice::from_ref(&i), at)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        super::plan::plan_document(&with_reasons.to_string()).unwrap()["candidate_reasons"][0]["reason"],
        "spread too wide"
    );
    let mut oversized = with_reasons;
    oversized["candidate_reasons"][0]["reason"] = "x".repeat(2049).into();
    assert_eq!(
        validate(&oversized.to_string(), &p, std::slice::from_ref(&i), at)
            .err()
            .unwrap()
            .to_string(),
        "planner_plan_bounds"
    );
    for (time, reason) in [
        (580_000, "action_time_exit_expired"),
        (at, "action_time_exit_expired"),
        (at + 1, "accepted"),
    ] {
        let mut buy = action(&i.key, "buy", 2);
        buy["exit_intent"]["time"] = time.into();
        let plan = json!({"actions":[buy],"stance":"test","notes":""});
        let verdicts = validate(&plan.to_string(), &p, std::slice::from_ref(&i), at).unwrap();
        assert_eq!(verdicts[0].reason, reason);
    }
    let q = Quote {
        bid: Price::parse("99").unwrap(),
        ask: Price::parse("99.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    assert!(
        p.execute_plan(
            at,
            &i,
            &q,
            true,
            3,
            Price::parse("100").unwrap(),
            ExitIntent::default(),
            "fixture"
        )
        .unwrap()
        .is_ok()
    );
    let plan = json!({"actions":[action(&i.key,"sell",4),action(&i.key,"sell",2),action(&i.key,"sell",2)],"stance":"hold","notes":""});
    let v = validate(&plan.to_string(), &p, std::slice::from_ref(&i), at).unwrap();
    assert_eq!(
        v.iter().map(|v| v.reason.as_str()).collect::<Vec<_>>(),
        vec!["action_sell_units", "accepted", "action_sell_units"]
    );
}

#[test]
fn buys_are_sized_from_the_sleeve_and_its_policy() {
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let mut p = crate::runtime::tests::test_portfolio();
    p.costs = Some(
        serde_json::from_value(json!({"per_order":"0","buy_bps":"0","sell_bps":"0",
            "slippage_bps":"0","rounding_increment":null}))
        .unwrap(),
    );
    // Risk 50 bps of 5000 (25 rupees), at most 2000 in one name.
    p.sizing = Some(
        serde_json::from_value(json!({
            "cash":{"risk_per_trade_bps":50,"max_position_bps":4000,"max_positions":3,"min_order_rupees":"200"},
            "options":{"risk_per_trade_bps":3000,"max_position_bps":10000,"max_positions":1,"min_order_rupees":"200"}}))
        .unwrap(),
    );
    let at = DateTime::parse_from_rfc3339("2026-10-16T10:15:00+05:30")
        .unwrap()
        .timestamp_millis();
    let buy = |units: Value, stop: Value| {
        json!({"instrument_key":i.key,"side":"buy","units":units,
        "max_price":"100","rationale":"fixture","exit_intent":{"stop":stop}})
    };
    let verdicts = |actions: Vec<Value>| {
        let plan = json!({"actions":actions,"stance":"test","notes":""});
        validate(&plan.to_string(), &p, std::slice::from_ref(&i), at)
            .unwrap()
            .into_iter()
            .map(|v| (v.reason, v.units))
            .collect::<Vec<_>>()
    };
    let r = |reason: &str, units: u64| (reason.to_owned(), units);
    // A 5-rupee stop risks 25 on 5 units. A 1-rupee stop would allow 25, but
    // the 2000-rupee position cap already holds the first 500.
    assert_eq!(
        verdicts(vec![
            buy(Value::Null, json!("95")),
            buy(Value::Null, json!("99"))
        ]),
        vec![r("accepted", 5), r("accepted", 15)]
    );
    assert_eq!(
        verdicts(vec![
            buy(json!(6), json!("95")),
            buy(json!(5), json!("95")),
            buy(Value::Null, Value::Null),
            buy(Value::Null, json!("100")),
        ]),
        vec![
            r("sizing_policy_exceeded", 0),
            r("accepted", 5),
            r("action_stop_required", 0),
            r("size_stop_not_below_price", 0),
        ]
    );
    // A strategy never names units, and its buys size against displayed depth.
    let quotes = BTreeMap::from([(
        i.key.clone(),
        serde_json::from_value::<Quote>(json!({"bid":"99.95","ask":"100","bid_units":3,
            "ask_units":3,"source":at,"receipt":at}))
        .unwrap(),
    )]);
    let strategy = validate_actions(
        vec![buy(json!(1), json!("95")), buy(Value::Null, json!("99"))],
        &p,
        std::slice::from_ref(&i),
        at,
        &quotes,
    )
    .unwrap();
    assert_eq!(strategy[0].reason, "action_strategy_buy_units");
    // Three units at 100 is above the 200-rupee floor.
    assert_eq!(
        (strategy[1].reason.as_str(), strategy[1].units),
        ("accepted", 3)
    );
}
