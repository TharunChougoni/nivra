//! Exact arithmetic/domain boundaries; synthetic values are not a dated fee policy.

use sadr_core::finance::{
    Amount, CashOrderGrid, FeeArithmetic, FeeSchedule, FinancialError, Money, Price, Side,
};

fn fees(
    per_order: &str,
    buy: &str,
    sell: &str,
    slippage: &str,
    tick: Option<&str>,
) -> FeeArithmetic {
    FeeArithmetic {
        per_order: Money::parse(per_order).unwrap(),
        buy_bps: Money::parse(buy).unwrap(),
        sell_bps: Money::parse(sell).unwrap(),
        slippage_bps: Money::parse(slippage).unwrap(),
        rounding_increment: tick.map(|value| Price::parse(value).unwrap()),
    }
}

#[test]
fn fee_increments_round_up_only_across_the_exact_boundary() {
    for (increment, rows) in [
        (
            "0.01",
            [("99.999", "0.01"), ("100", "0.01"), ("100.001", "0.02")],
        ),
        (
            "0.05",
            [("499.999", "0.05"), ("500", "0.05"), ("500.001", "0.1")],
        ),
    ] {
        let policy = fees("0", "1", "2", "0", Some(increment));
        assert_eq!(policy.charge(Side::Buy, Money::ZERO).unwrap(), Money::ZERO);
        for (turnover, expected) in rows {
            assert_eq!(
                policy
                    .charge(Side::Buy, Money::parse(turnover).unwrap())
                    .unwrap(),
                Money::parse(expected).unwrap()
            );
        }
    }
    // Fixed and variable fees are added before one rounding operation.
    let policy = fees("0.001", "1", "3", "10", Some("0.01"));
    let turnover = Money::parse("100").unwrap();
    assert_eq!(
        policy.charge(Side::Buy, turnover).unwrap(),
        Money::parse("0.02").unwrap()
    );
    assert_eq!(
        policy.charge(Side::Sell, turnover).unwrap(),
        Money::parse("0.04").unwrap()
    );
    assert_eq!(
        policy.cash_requirement(turnover, true, 2).unwrap(),
        Money::parse("100.26").unwrap()
    );
    // The fee increment does not independently round slippage.
    let policy = fees("0", "0", "0", "1", Some("1"));
    assert_eq!(
        policy
            .cash_requirement(Money::parse("1").unwrap(), true, 2)
            .unwrap(),
        Money::parse("1.0002").unwrap()
    );

    // Per-segment lookup: cash keeps its schedule; a non-cash segment needs its
    // own configured entry and never borrows the cash 5/7 bps policy.
    use sadr_core::market::{Instrument, Segment};
    let cash = fees("0.01", "5", "7", "5", Some("0.01"));
    let fo = fees("0.01", "0", "0", "10", Some("0.05"));
    let schedule = FeeSchedule::new(cash.clone()).with_segment(Segment::NseFo, fo.clone());
    schedule.validate().unwrap();
    assert_eq!(schedule.for_segment(Segment::NseEq), Some(&cash));
    assert_eq!(schedule.for_segment(Segment::NseFo), Some(&fo));
    assert_eq!(schedule.for_segment(Segment::NseIndex), None);
    let cash_instrument: Instrument = serde_json::from_value(serde_json::json!({
        "key": "NSE_EQ|INF204KB14I2", "symbol": "NIFTYBEES", "kind": "cash",
        "lot_size": 1, "tick_size": "0.01", "version": "fixture-v1"
    }))
    .unwrap();
    let fo_instrument: Instrument = serde_json::from_value(serde_json::json!({
        "key": "NSE_FO|NIFTY26NOVFUT", "symbol": "NIFTY26NOVFUT", "kind": "future",
        "lot_size": 50, "tick_size": "0.05", "version": "fixture-v1",
        "expiry": "2026-11-26", "underlying": "NIFTY", "segment": "NSE_FO"
    }))
    .unwrap();
    let index_instrument: Instrument = serde_json::from_value(serde_json::json!({
        "key": "NSE_INDEX|Nifty 50", "symbol": "NIFTY", "kind": "index",
        "lot_size": 1, "tick_size": "0.05", "version": "fixture-v1"
    }))
    .unwrap();
    assert_eq!(schedule.for_instrument(&cash_instrument).unwrap(), &cash);
    assert_eq!(schedule.for_instrument(&fo_instrument).unwrap(), &fo);
    assert_eq!(
        schedule.for_instrument(&index_instrument),
        Err(FinancialError::InvalidCostScope)
    );
    // Missing FO schedule fails closed instead of reusing cash bps.
    let cash_only = FeeSchedule::new(cash);
    assert_eq!(
        cash_only.for_instrument(&fo_instrument),
        Err(FinancialError::InvalidCostScope)
    );
    let encoded = serde_json::to_value(&schedule).unwrap();
    assert_eq!(encoded["by_segment"]["NSE_FO"]["buy_bps"], "0");
    assert_eq!(
        serde_json::from_value::<FeeSchedule>(encoded).unwrap(),
        schedule
    );
}

#[test]
fn affordable_lots_obey_exact_cost_and_maximum_boundaries() {
    let policy = fees("1", "0", "0", "0", None);
    let unit = Price::parse("10").unwrap();
    for (budget, round_trip, expected) in [
        ("100", false, 9),
        ("101", false, 10),
        ("101", true, 9),
        ("102", true, 10),
    ] {
        assert_eq!(
            policy
                .affordable_cash_lots(unit, Amount::parse(budget).unwrap(), 20, round_trip, 0)
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        policy
            .affordable_cash_lots(unit, Amount::parse("1000").unwrap(), 3, false, 0)
            .unwrap(),
        3
    );
    assert_eq!(
        policy
            .affordable_cash_lots(unit, Amount::parse("1000").unwrap(), 0, false, 0)
            .unwrap(),
        0
    );
    for budget in ["-1", "0", "0.1", "10.99"] {
        assert_eq!(
            policy
                .affordable_cash_lots(unit, Amount::parse(budget).unwrap(), 100, false, 0)
                .unwrap(),
            0
        );
    }
    // Impossible oversized lots must not be multiplied just to discover zero is affordable.
    assert_eq!(
        policy
            .affordable_cash_lots(
                Price::parse("1e28").unwrap(),
                Amount::parse("1").unwrap(),
                100,
                false,
                0
            )
            .unwrap(),
        0
    );
}

#[test]
fn order_grid_rejects_invalid_units_and_off_tick_prices_exactly() {
    assert_eq!(
        CashOrderGrid::new(0, Price::parse("0.05").unwrap()).unwrap_err(),
        FinancialError::InvalidOrderGrid
    );
    let grid = CashOrderGrid::new(25, Price::parse("0.05").unwrap()).unwrap();
    for units in [25, 50, 100] {
        for price in ["0.05", "1", "1.05", "105e-2"] {
            assert_eq!(grid.validate(units, Price::parse(price).unwrap()), Ok(()));
        }
    }
    for units in [i64::MIN, -25, 0, 1, 24, 26] {
        assert_eq!(
            grid.validate(units, Price::parse("1.05").unwrap())
                .unwrap_err(),
            FinancialError::InvalidOrderGrid
        );
    }
    for price in [
        "0.01",
        "1.0499999999999999999999999999",
        "1.0500000000000000000000000001",
    ] {
        assert_eq!(
            grid.validate(25, Price::parse(price).unwrap()).unwrap_err(),
            FinancialError::InvalidOrderGrid
        );
    }
    let larger_than_signed_units =
        CashOrderGrid::new(u64::MAX, Price::parse("1").unwrap()).unwrap();
    assert_eq!(
        larger_than_signed_units
            .validate(i64::MAX, Price::parse("1").unwrap())
            .unwrap_err(),
        FinancialError::InvalidOrderGrid
    );
    assert_eq!(
        CashOrderGrid::new(1, Price::parse("1e-28").unwrap())
            .unwrap()
            .validate(1, Price::parse("2e-28").unwrap()),
        Ok(())
    );
}

#[test]
fn model_led_cash_has_no_archived_entry_caps_and_keeps_freshness_and_cash_integrity() {
    use sadr_core::execution::{Instrument, Portfolio, Quote, a};
    use std::collections::BTreeMap;
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-19T09:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let i = |n| Instrument {
        key: format!("SYNTHETIC|{n}"),
        symbol: format!("S{n}"),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let mut q = Quote {
        bid: Price::parse("100").unwrap(),
        ask: Price::parse("100").unwrap(),
        bid_units: 1,
        ask_units: 1,
        source: at,
        receipt: at,
    };
    let mut p = Portfolio::with_capital(a("5000"));
    p.costs = Some(
        serde_json::from_str(include_str!(
            "../../../fixtures/test-legacy-paper-costs.json"
        ))
        .unwrap(),
    );
    for n in 0..8 {
        assert!(p.enter(at, &i(n), &q).unwrap());
    }
    assert_eq!(p.positions.len(), 8);
    p.reconcile().unwrap();
    q.receipt = at - 2001;
    assert!(!p.enter(at, &i(9), &q).unwrap());
    q.receipt = at;
    let mut expensive = q.clone();
    expensive.bid = Price::parse("6000").unwrap();
    expensive.ask = expensive.bid;
    assert!(!p.enter(at, &i(9), &expensive).unwrap());
    assert!(p.cash >= a("0"));
    let mut loss = q.clone();
    loss.bid = Price::parse("1").unwrap();
    loss.ask = loss.bid;
    let quotes = (0..8)
        .map(|n| (i(n).key, loss.clone()))
        .collect::<BTreeMap<_, _>>();
    p.protect(at, &quotes).unwrap();
    assert!(!p.halted); // Loss and drawdown no longer impose a runtime risk limit.
    assert!(p.positions.is_empty());
    p.reconcile().unwrap();
    let mut many = Portfolio::with_capital(a("5000"));
    many.costs = p.costs.clone();
    let mut target = q.clone();
    target.bid = Price::parse("103").unwrap();
    target.ask = target.bid;
    for n in 0..51 {
        assert!(many.enter(at, &i(n), &q).unwrap());
        many.protect(at, &BTreeMap::from([(i(n).key, target.clone())]))
            .unwrap();
    }
    assert_eq!(many.fills.len(), 102);
    many.reconcile().unwrap();
    // Large entries spend available cash rather than an old per-position/planned-loss cap.
    let mut large = Portfolio::with_capital(a("20000"));
    large.costs = p.costs.clone();
    q.ask_units = 10000;
    assert!(large.enter(at, &i(99), &q).unwrap());
    assert!(large.exposure().unwrap() > a("10000"));
    large.reconcile().unwrap();
}
