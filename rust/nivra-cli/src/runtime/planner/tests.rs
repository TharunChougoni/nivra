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
    let action = |key: &str, side: &str, units: u64| json!({"instrument_key":key,"side":side,"units":units,"max_price":"100","rationale":"fixture","exit_intent":{}});
    let plan = json!({"actions":[action("absent","buy",1),action(&i.key,"buy",50),action(&i.key,"sell",1),action(&i.key,"buy",2)],"stance":"test","notes":""});
    let v = validate(&plan.to_string(), &p, std::slice::from_ref(&i), at).unwrap();
    assert_eq!(
        v.iter().map(|v| v.reason.as_str()).collect::<Vec<_>>(),
        vec![
            "action_allowlist",
            "action_cash",
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
    );
    let plan = json!({"actions":[action(&i.key,"sell",4),action(&i.key,"sell",2),action(&i.key,"sell",2)],"stance":"hold","notes":""});
    let v = validate(&plan.to_string(), &p, std::slice::from_ref(&i), at).unwrap();
    assert_eq!(
        v.iter().map(|v| v.reason.as_str()).collect::<Vec<_>>(),
        vec!["action_sell_units", "accepted", "action_sell_units"]
    );
}
