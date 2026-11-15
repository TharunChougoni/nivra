//! Independent synthetic cases; no archive, mandate or execution authority.
use sadr_core::{
    finance::{FeeArithmetic, Money},
    risk::{PaperCashPolicy, Verdict, paper_cash_preflight},
};
use serde_json::{Value, json};

fn input() -> Value {
    let instrument = json!({
        "key":"TEST|CASH", "symbol":"CASH", "kind":"cash",
        "lot_size":1, "tick_size":"0.01", "version":"synthetic-v1"
    });
    json!({
        "candidate": {
            "candidate_id":"test-candidate", "strategy":"synthetic", "snapshot_id":"test-snapshot",
            "created_at":"2026-10-15T10:00:00+05:30", "valid_until":"2026-10-15T10:10:00+05:30",
            "legs":[{"instrument":instrument, "side":"buy", "limit_price":"100"}],
            "planned_stop_loss_inr":"8", "stress_loss_inr":"100"
        },
        "snapshot": {
            "snapshot_id":"test-snapshot", "captured_at":"2026-10-15T10:00:00+05:30",
            "payload_hash":"a".repeat(64), "source":"synthetic",
            "quotes":[{"instrument":instrument, "bid":"99.99", "ask":"100",
                "bid_units":100, "ask_units":100, "source_at":"2026-10-15T10:00:00+05:30",
                "received_at":"2026-10-15T10:00:00+05:30"}]
        },
        "portfolio":{"session_start":"5000", "equity":"5000", "high_water":"5000", "cash":"5000"},
        "fees":{"per_order":"0", "buy_bps":"1", "sell_bps":"2", "slippage_bps":"0", "rounding_increment":null},
        "calendar":{"source":"synthetic", "valid_from":"2026-10-15", "valid_to":"2026-10-15"},
        "now":"2026-10-15T10:00:00+05:30", "allow_refreshed_snapshot":false
    })
}

fn verdict(v: &Value) -> Verdict {
    paper_cash_preflight(
        &serde_json::from_value(v["candidate"].clone()).unwrap(),
        &serde_json::from_value(v["snapshot"].clone()).unwrap(),
        &serde_json::from_value(v["portfolio"].clone()).unwrap(),
        &serde_json::from_value::<FeeArithmetic>(v["fees"].clone()).unwrap(),
        &serde_json::from_value(v["calendar"].clone()).unwrap(),
        serde_json::from_value(v["now"].clone()).unwrap(),
        PaperCashPolicy::default(),
        v["allow_refreshed_snapshot"].as_bool().unwrap(),
    )
    .unwrap()
}

fn denied(v: &Value, reasons: &[&str]) {
    let result = verdict(v);
    assert!(!result.accepted);
    assert_eq!(result.reasons, reasons);
    assert_eq!(result.lots, 0);
    assert_eq!(result.reservation, Money::ZERO);
}

#[test]
fn baseline_reserves_full_notional_and_round_trip_fees() {
    let result = verdict(&input());
    assert!(result.accepted);
    assert!(result.reasons.is_empty());
    assert_eq!(result.lots, 3);
    assert_eq!(result.reservation, Money::parse("300.09").unwrap());
    spread_threshold_is_inclusive_and_compared_without_division_rounding();
}

#[test]
fn source_age_is_inclusive_at_two_seconds_and_rejects_next_microsecond() {
    let mut v = input();
    v["now"] = json!("2026-10-15T10:00:02+05:30");
    assert!(verdict(&v).accepted);
    v["now"] = json!("2026-10-15T10:00:02.000001+05:30");
    denied(&v, &["stale_or_missing_quote", "unaffordable_or_no_depth"]);
    v["now"] = json!("2026-10-15T09:59:59.999999+05:30");
    denied(
        &v,
        &[
            "expired_decision",
            "stale_or_missing_quote",
            "unaffordable_or_no_depth",
        ],
    );
    drawdown_uses_high_water_independently_of_daily_loss();
}

fn spread_threshold_is_inclusive_and_compared_without_division_rounding() {
    let mut v = input();
    v["snapshot"]["quotes"][0]["bid"] = json!("99.5");
    assert!(verdict(&v).accepted);
    v["snapshot"]["quotes"][0]["bid"] = json!("99.49");
    denied(&v, &["wide_spread"]);
}

fn drawdown_uses_high_water_independently_of_daily_loss() {
    let mut v = input();
    v["portfolio"]["high_water"] = json!("6000");
    v["portfolio"]["equity"] = json!("5700.01");
    assert!(verdict(&v).accepted);
    v["portfolio"]["equity"] = json!("5700");
    denied(&v, &["drawdown"]);
}
