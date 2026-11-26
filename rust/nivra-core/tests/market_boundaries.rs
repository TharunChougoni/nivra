//! Market record boundaries: quote domains and the archived receipt-time binding.

use nivra_core::{market::Quote, time::Timestamp};
use serde_json::{Value, json};

const RECEIVED: &str = "2026-10-15T04:00:00Z";

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn instrument_json() -> Value {
    json!({
        "key": "NSE_EQ|FIXTURE", "symbol": "FIXTURE", "kind": "cash",
        "lot_size": 25, "tick_size": "0.05", "version": "fixture-v1",
        "cas_eligible": false, "security_type": "NORMAL"
    })
}

fn quote_json() -> Value {
    json!({
        "instrument": instrument_json(), "bid": "100", "ask": "100.05",
        "bid_units": 100, "ask_units": 100,
        "source_at": RECEIVED, "received_at": RECEIVED
    })
}

#[test]
fn quote_structural_validity_is_separate_from_execution_and_freshness() {
    let mut locked = quote_json();
    locked["ask"] = json!("100");
    locked["bid_units"] = json!(0);
    locked["ask_units"] = json!(0);
    let quote: Quote = serde_json::from_value(locked).unwrap();
    assert_eq!(quote.bid(), quote.ask());
    assert_eq!(quote.bid_units(), 0);
    assert_eq!(quote.ask_units(), 0);
    assert!(quote.fresh(timestamp(RECEIVED)));
    assert!(quote.fresh(timestamp("2026-10-15T04:00:02Z")));
    assert!(!quote.fresh(timestamp("2026-10-15T04:00:02.000001Z")));
    assert!(!quote.fresh(timestamp("2026-10-15T03:59:59.999999Z")));
    for (field, bad) in [
        ("bid", json!("100.10")),
        ("ask", json!("0")),
        ("ask_units", json!(true)),
        ("source_at", json!("2026-10-15T04:00:00.000001Z")),
    ] {
        let mut value = quote_json();
        value[field] = bad;
        assert!(serde_json::from_value::<Quote>(value).is_err(), "{field}");
    }
}
