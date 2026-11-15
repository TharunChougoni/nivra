use super::*;
use sadr_core::{
    execution::{Quote, Sample},
    finance::Price,
};
#[test]
fn shortlisted_prospect_carries_bid_ask_and_depth() {
    let mut scan = Scanner::default();
    let sample = |executable| Sample {
        price_kind: Some("ltpc".into()),
        seen: 1000,
        source: Some(1000),
        last: 1000,
        current: Some(Price::parse("110").unwrap()),
        history: vec![Price::parse("100").unwrap(), Price::parse("110").unwrap()],
        reason: "eligible_no_signal".into(),
        market_fields: Some(sadr_core::execution::MarketFields {
            session_vwap: None,
            best_bid: None,
            best_bid_qty: None,
            best_ask: None,
            best_ask_qty: None,
            total_buy_quantity: None,
            total_sell_quantity: None,
            lower_circuit_limit: Price::parse("90").unwrap().amount(),
            upper_circuit_limit: Price::parse("130").unwrap().amount(),
            volume: 1_000_000,
        }),
        executable,
        ..Sample::default()
    };
    scan.samples.insert(
        "A".into(),
        sample(Some(Quote {
            bid: Price::parse("109.95").unwrap(),
            ask: Price::parse("110.05").unwrap(),
            bid_units: 40,
            ask_units: 25,
            source: 990,
            receipt: 995,
        })),
    );
    scan.samples.insert("B".into(), sample(None));
    let instrument = |k: &str| Instrument {
        key: k.into(),
        symbol: k.into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let settings: Settings =
        serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":10})).unwrap();
    let result = project(&scan, &[instrument("A"), instrument("B")], &settings, 1001);
    let rows = result["prospects"].as_array().unwrap();
    let a = rows.iter().find(|r| r["instrument_key"] == "A").unwrap();
    assert_eq!(a["bid"], "109.95");
    assert_eq!(a["ask"], "110.05");
    assert_eq!(a["ask_units"], 25);
    assert_eq!(a["quote_age_ms"], 6);
    let b = rows.iter().find(|r| r["instrument_key"] == "B").unwrap();
    assert!(b["bid"].is_null());
}
#[test]
fn tunables_validate_and_freshness_cannot_exceed_two_seconds() {
    let ok: Settings = serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":10,"ranking_window_ms":30000,"quote_freshness_ms":1500,"digest_cadence_ms":60000})).unwrap();
    ok.validate().unwrap();
    assert_eq!(
        serde_json::to_value(&ok).unwrap()["digest_cadence_ms"],
        60000
    );
    for cadence in [999, 3_600_001] {
        let bad: Settings = serde_json::from_value(
            json!({"max_scan_age_ms":2000,"max_prospects":10,"digest_cadence_ms":cadence}),
        )
        .unwrap();
        assert!(bad.validate().is_err(), "cadence {cadence} must be refused");
    }
    let loose: Settings = serde_json::from_value(
        json!({"max_scan_age_ms":2000,"max_prospects":10,"quote_freshness_ms":5000}),
    )
    .unwrap();
    assert!(loose.validate().is_err());
}
