use super::*;
use nivra_core::intraday::{Clock, Features, Levels, Receipt};
use serde_json::json;

#[test]
fn a_nominee_is_covered_only_by_a_fresh_valid_full_touch() {
    let mut scan = Scanner::default();
    let mut inputs = Inputs::default();
    let at = 1_800_000_000_000;
    let instrument: Instrument = serde_json::from_value(
        json!({"key":"NSE_EQ|DEMO","symbol":"DEMO","series":"EQ","tick":"0.05"}),
    )
    .unwrap();
    let key = &instrument.key;
    let mut point = market_feed::Point {
        price: Price::parse("100").unwrap(),
        source: at,
        receipt: at,
        last_trade: at,
        prior_close: None,
        depth: None,
    };
    let settings = market_feed::Settings::default();
    assert!(
        !inputs
            .observe_prospect(&mut scan, &instrument, &point, &settings, at)
            .unwrap()
    );
    assert!(!inputs.covered(&scan, key, at, true));

    let depth = |ask: &str, receipt: i64| {
        Features::reduce(Receipt {
            clock: Clock {
                source: at,
                receipt,
            },
            touch: Some(Quote {
                source: at,
                receipt,
                bid: point.price,
                ask: Price::parse(ask).unwrap(),
                bid_units: 10,
                ask_units: 20,
            }),
            vwap: None,
            volume: None,
            buy_quantity: None,
            sell_quantity: None,
            levels: Levels::default(),
        })
    };
    point.depth = Some(depth("100.02", at + 1));
    point.receipt = at + 1;
    assert!(
        !inputs
            .observe_prospect(&mut scan, &instrument, &point, &settings, at + 1)
            .unwrap()
    );
    assert!(!inputs.covered(&scan, key, at + 1, true));

    point.depth = Some(depth("100.05", at + 2));
    point.receipt = at + 2;
    assert!(
        inputs
            .observe_prospect(&mut scan, &instrument, &point, &settings, at + 2)
            .unwrap()
    );
    assert!(inputs.covered(&scan, key, at + 2, true));
    assert!(!inputs.covered(&scan, key, at + 2, false));
    assert!(!inputs.covered(&scan, key, at + MAX_FINANCIAL_AGE_MS + 1, true));
}

#[test]
fn faster_ranking_does_not_accelerate_nominations() {
    let inputs = Inputs {
        refreshed_at: Some(10_000),
        ..Inputs::default()
    };
    assert!(!inputs.refresh_due(10_250, true, true));
    assert!(inputs.refresh_due(11_000, true, false));
    // With no active strategy, rank-only promotion keeps its own cadence.
    assert!(inputs.refresh_due(10_250, false, true));
    assert!(!inputs.refresh_due(11_000, false, false));
}
