//! Capital sleeves: each sleeve spends only its own share of the book's cash.

use nivra_core::execution::{ExitIntent, Instrument, Portfolio, Quote, Refusal};
use nivra_core::finance::{Amount, FeeArithmetic, Price};
use nivra_core::sleeve::{Allocation, Sleeve};

const AT: i64 = 1_795_066_200_000; // 2026-11-19 11:00 IST

fn a(s: &str) -> Amount {
    Amount::parse(s).unwrap()
}

fn book() -> Portfolio {
    let allocation = |sleeve, amount| Allocation {
        at_ms: 0,
        sleeve,
        amount: a(amount),
        reason: "fixture".into(),
    };
    let mut p = Portfolio::with_allocations(vec![
        allocation(Sleeve::Cash, "5000"),
        allocation(Sleeve::Options, "5000"),
    ]);
    p.costs = Some(
        serde_json::from_value::<FeeArithmetic>(serde_json::json!({"per_order":"0",
            "buy_bps":"0","sell_bps":"0","slippage_bps":"0","rounding_increment":null}))
        .unwrap(),
    );
    p
}

fn stock() -> (Instrument, Quote) {
    let price = Price::parse("100").unwrap();
    (
        Instrument {
            key: "NSE_EQ|INE000000001".into(),
            symbol: "FIXTURE".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        },
        Quote {
            bid: price,
            ask: price,
            bid_units: 1000,
            ask_units: 1000,
            source: AT - 100,
            receipt: AT - 100,
        },
    )
}

#[test]
fn a_cash_buy_cannot_spend_the_options_sleeve() {
    let mut p = book();
    let (i, q) = stock();
    let max = Price::parse("100").unwrap();
    let mut buy = |units| {
        p.execute_plan(AT, &i, &q, true, units, max, ExitIntent::default(), "d")
            .unwrap()
            .err()
    };
    // 60 units cost 6000: within the 10000 book but over the 5000 cash sleeve.
    let refused = buy(60).unwrap();
    assert_eq!(refused.reason, Refusal::InsufficientCash);
    assert_eq!(refused.facts.cash.as_deref(), Some("5000"));
    assert_eq!(buy(50), None);
    assert_eq!(p.sleeve_cash(Sleeve::Cash).unwrap(), a("0"));
    assert_eq!(p.sleeve_cash(Sleeve::Options).unwrap(), a("5000"));
    assert_eq!(p.sleeve_equity(Sleeve::Cash).unwrap(), a("5000"));
    assert_eq!(p.allocation().unwrap(), a("10000"));
    p.reconcile().unwrap();
}

#[test]
fn reconcile_refuses_a_fill_that_overdraws_its_sleeve() {
    let mut p = book();
    let (i, q) = stock();
    let max = Price::parse("100").unwrap();
    p.execute_plan(AT, &i, &q, true, 50, max, ExitIntent::default(), "d")
        .unwrap()
        .unwrap();
    // The same book stored with a 4000 cash sleeve spent 5000 from it.
    let mut tampered = p.clone();
    tampered.allocations[0].amount = a("4000");
    tampered.allocations[1].amount = a("6000");
    assert_eq!(
        tampered.reconcile().unwrap_err().to_string(),
        "sleeve_cash_overdrawn"
    );
}

#[test]
fn session_strategy_pnl_marks_its_own_lots_and_prorates_the_partial_exit_fee() {
    use nivra_core::sleeve::session_strategy_pnl;

    let mut p = book();
    p.costs = Some(
        serde_json::from_value::<FeeArithmetic>(serde_json::json!({
            "per_order": "1", "buy_bps": "0", "sell_bps": "0",
            "slippage_bps": "0", "rounding_increment": null
        }))
        .unwrap(),
    );
    let (instrument, quote) = stock();
    let mut trade = |at, buy, units, price, strategy| {
        let price = Price::parse(price).unwrap();
        let quote = Quote {
            bid: price,
            ask: price,
            source: at,
            receipt: at,
            ..quote.clone()
        };
        p.execute_plan(
            at,
            &instrument,
            &quote,
            buy,
            units,
            price,
            ExitIntent::default(),
            strategy,
        )
        .unwrap()
        .unwrap();
    };
    trade(AT - 1, true, 1, "100", "strategy");
    trade(AT, true, 4, "100", "strategy");
    trade(AT + 1, false, 3, "110", "protection");
    let mark = Price::parse("110").unwrap();
    p.protect(
        AT + 1,
        &std::collections::BTreeMap::from([(
            instrument.key.clone(),
            Quote {
                bid: mark,
                ask: mark,
                source: AT + 1,
                receipt: AT + 1,
                ..quote
            },
        )]),
    )
    .unwrap();

    // One of the three sold units belonged to the earlier entry. Two of
    // today's units sold and two remain marked at 110.
    assert_eq!(
        session_strategy_pnl(&p, AT, "strategy").unwrap(),
        Some(a("38.33333333"))
    );
    assert_eq!(session_strategy_pnl(&p, AT, "protection").unwrap(), None);
}
