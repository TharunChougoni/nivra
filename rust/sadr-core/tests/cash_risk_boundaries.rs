//! Fixed paper-profile arithmetic only; successful sizing is not preflight or authority.

use sadr_core::{
    finance::{Amount, FeeArithmetic, Money, Price},
    risk::{PaperCashPolicy, PaperCashProfile, Portfolio, paper_cash_lots},
};
use serde_json::{Value, json};

fn portfolio_json(capital: &str) -> Value {
    json!({"equity": capital, "session_start": capital, "high_water": capital, "cash": capital})
}

fn portfolio(capital: &str) -> Portfolio {
    serde_json::from_value(portfolio_json(capital)).unwrap()
}

fn fees(
    per_order: &str,
    buy: &str,
    sell: &str,
    slip: &str,
    increment: Option<&str>,
) -> FeeArithmetic {
    FeeArithmetic {
        per_order: Money::parse(per_order).unwrap(),
        buy_bps: Money::parse(buy).unwrap(),
        sell_bps: Money::parse(sell).unwrap(),
        slippage_bps: Money::parse(slip).unwrap(),
        rounding_increment: increment.map(|value| Price::parse(value).unwrap()),
    }
}

#[test]
fn profile_defaults_exact_ids_and_fixed_limits_cannot_be_overridden() {
    let default: PaperCashPolicy = serde_json::from_str("{}").unwrap();
    assert_eq!(default.profile(), PaperCashProfile::Cash5000);
    assert_eq!(default.capital(), Price::parse("5000").unwrap());
    for (profile, capital, per_entry, drawdown, entries) in [
        (PaperCashProfile::Cash5000, "5000", "0.4", "0.05", 3),
        (PaperCashProfile::Cash50000, "50000", "0.1", "0.02", 10),
    ] {
        let policy = PaperCashPolicy::new(profile);
        assert_eq!(policy.capital(), Price::parse(capital).unwrap());
        assert_eq!(
            serde_json::to_value(policy.limits()).unwrap(),
            json!({
                "per_entry": per_entry, "aggregate": per_entry, "daily_loss": "0.01",
                "drawdown": drawdown, "max_positions": 1, "max_entries": entries,
                "max_spread_fraction": "0.005", "max_quote_age": 2
            })
        );
        assert_eq!(
            serde_json::from_value::<PaperCashPolicy>(serde_json::to_value(policy).unwrap())
                .unwrap()
                .profile(),
            profile
        );
    }
    for value in [
        json!({"profile": "live"}),
        json!({"profile": "cash_5000_paper_v2"}),
        json!({"profile": true}),
        json!({"profile": null}),
        json!({"max_quote_age": 3}),
        json!({"max_entry": 5000}),
        json!({"profile": "cash_50000_paper_v1", "limits": {"max_quote_age": 60}}),
    ] {
        assert!(serde_json::from_value::<PaperCashPolicy>(value).is_err());
    }
}

#[test]
fn cash_budget_preserves_signed_overreservation_and_independent_cash_cap() {
    for (profile, capital, cap) in [
        (PaperCashProfile::Cash5000, "5000", "2000"),
        (PaperCashProfile::Cash50000, "50000", "5000"),
    ] {
        let policy = PaperCashPolicy::new(profile);
        let mut p = portfolio(capital);
        assert_eq!(policy.cash_budget(&p).unwrap(), Amount::parse(cap).unwrap());
        p.cash = Money::parse("12.34").unwrap();
        assert_eq!(
            policy.cash_budget(&p).unwrap(),
            Amount::parse("12.34").unwrap()
        );
        p.cash = Money::parse(capital).unwrap();
        p.reserved_risk = Money::parse(cap).unwrap();
        assert_eq!(policy.cash_budget(&p).unwrap(), Amount::ZERO);
        p.reserved_risk = Money::try_from(
            Amount::parse(cap)
                .unwrap()
                .checked_add(Amount::parse("0.01").unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            policy.cash_budget(&p).unwrap(),
            Amount::parse("-0.01").unwrap()
        );
        p.cash = Money::ZERO;
        assert_eq!(
            policy.cash_budget(&p).unwrap(),
            Amount::parse("-0.01").unwrap()
        );
    }
}

#[test]
fn planned_budget_has_exact_remaining_daily_loss_boundaries() {
    for (profile, capital, rows) in [
        (
            PaperCashProfile::Cash5000,
            "5000",
            [
                ("5000", "25"),
                ("4975", "25"),
                ("4974.99", "24.99"),
                ("4950.01", "0.01"),
                ("4950", "0"),
                ("4949.99", "0"),
            ],
        ),
        (
            PaperCashProfile::Cash50000,
            "50000",
            [
                ("50000", "100"),
                ("49600", "100"),
                ("49599.99", "99.99"),
                ("49500.01", "0.01"),
                ("49500", "0"),
                ("49499.99", "0"),
            ],
        ),
    ] {
        let policy = PaperCashPolicy::new(profile);
        let mut p = portfolio(capital);
        for (equity, expected) in rows {
            p.equity = Money::parse(equity).unwrap();
            assert_eq!(
                policy.planned_budget(&p).unwrap(),
                Money::parse(expected).unwrap(),
                "{equity}"
            );
        }
    }
}

#[test]
fn exact_stop_and_full_cash_thresholds_are_both_binding() {
    let policy = fees("1", "0", "0", "0", None);
    let price = Price::parse("100").unwrap();
    for (cash, planned, lots, reservation) in [
        ("102", "10", 1, "102"),
        ("101.999999", "10", 0, "0"),
        ("102", "9.999999", 0, "0"),
        ("202", "18", 2, "202"),
        ("202", "17.999999", 1, "102"),
        ("201.999999", "18", 1, "102"),
    ] {
        let actual = paper_cash_lots(
            price,
            &policy,
            Amount::parse(cash).unwrap(),
            Money::parse(planned).unwrap(),
            100,
        )
        .unwrap();
        assert_eq!(actual.lots, lots, "cash={cash}, planned={planned}");
        assert_eq!(actual.reservation, Money::parse(reservation).unwrap());
    }
    let mixed_costs = fees("2.5", "1", "2", "3", None);
    // 8 stop + 2.51 buy + 2.52 sell + .06 two-leg slippage = 13.09.
    for (planned, lots) in [("13.09", 1), ("13.089999", 0)] {
        let actual = paper_cash_lots(
            price,
            &mixed_costs,
            Amount::parse("200").unwrap(),
            Money::parse(planned).unwrap(),
            1,
        )
        .unwrap();
        assert_eq!(actual.lots, lots);
        assert_eq!(
            actual.reservation,
            Money::parse(if lots == 1 { "105.09" } else { "0" }).unwrap()
        );
    }
}

#[test]
fn rounded_fees_apply_to_total_notional_once_per_side_and_preserve_steps() {
    let per_side_rounding = fees("0", "1", "1", "0", Some("1"));
    let actual = paper_cash_lots(
        Price::parse("1").unwrap(),
        &per_side_rounding,
        Amount::parse("12").unwrap(),
        Money::parse("2.8").unwrap(),
        100,
    )
    .unwrap();
    assert_eq!(actual.lots, 10);
    assert_eq!(actual.reservation, Money::parse("12").unwrap());
    let stepped = fees("0", "100", "100", "0", Some("1"));
    for (planned, lots, reservation) in [("12.799999", 10, "102"), ("12.8", 11, "114")] {
        let actual = paper_cash_lots(
            Price::parse("10").unwrap(),
            &stepped,
            Amount::parse("500").unwrap(),
            Money::parse(planned).unwrap(),
            20,
        )
        .unwrap();
        assert_eq!(actual.lots, lots);
        assert_eq!(actual.reservation, Money::parse(reservation).unwrap());
    }
}

#[test]
fn option_preflight_spends_premium_not_index_notional_and_keeps_cash_binding() {
    use sadr_core::{
        candidate::Candidate,
        market::{Calendar, InstrumentKind, OptionCatalog, OptionContract, Snapshot},
        risk::{OptionPreflightSettings, paper_option_preflight},
        time::{ExchangeDate, Timestamp},
    };
    use std::collections::BTreeMap;
    let now = Timestamp::parse("2026-10-16T04:30:00Z").unwrap();
    let now_ms = chrono::DateTime::parse_from_rfc3339("2026-10-16T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let contract = OptionContract {
        key: "NSE_FO|26000CE".into(),
        symbol: "NIFTY 26000 CE 20 OCT 26".into(),
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        underlying_symbol: "NIFTY".into(),
        expiry: ExchangeDate::parse("2026-10-20").unwrap(),
        strike: Price::parse("26000").unwrap(),
        option: InstrumentKind::Call,
        lot_size: 65,
        tick: Price::parse("0.05").unwrap(),
        freeze_quantity: Some(1800),
        weekly: true,
    };
    let instrument = contract.instrument().unwrap();
    let catalog = OptionCatalog::try_new(
        ExchangeDate::parse("2026-10-16").unwrap(),
        "NSE_INDEX|Nifty 50",
        "NIFTY",
        contract.expiry,
        Price::parse("26000").unwrap(),
        [contract.clone()],
    )
    .unwrap();
    let fo_fees = fees("0.01", "0", "0", "10", Some("0.05"));
    let calendar: Calendar = serde_json::from_value(json!({
        "source": "fixture-calendar", "valid_from": "2026-10-15", "valid_to": "2026-10-31"
    }))
    .unwrap();
    let settings = OptionPreflightSettings {
        underlying_key: "NSE_INDEX|Nifty 50",
        underlying_symbol: "NIFTY",
        today: ExchangeDate::parse("2026-10-16").unwrap(),
    };
    let candidate = |limit: &str, snapshot_id: &str| -> Candidate {
        serde_json::from_value(json!({
            "candidate_id": "option-fixture", "strategy": "fixture", "snapshot_id": snapshot_id,
            "created_at": "2026-10-16T04:29:00Z", "valid_until": "2026-10-16T04:35:00Z",
            "legs": [{"instrument": instrument, "side": "buy", "limit_price": limit}],
            "planned_stop_loss_inr": "0", "stress_loss_inr": limit
        }))
        .unwrap()
    };
    let stamp = |millis: i64| {
        chrono::DateTime::<chrono::Utc>::from_timestamp_millis(millis)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    };
    let snapshot = |bid: &str, ask: &str, ask_units: u64, receipt: i64| -> Snapshot {
        serde_json::from_value(json!({
            "snapshot_id": "option-snapshot", "captured_at": stamp(now_ms),
            "payload_hash": "a".repeat(64), "source": "upstox-full-quotes-v2",
            "quotes": [{"instrument": instrument, "bid": bid, "ask": ask,
                "bid_units": 650, "ask_units": ask_units,
                "source_at": stamp(receipt), "received_at": stamp(receipt)}]
        }))
        .unwrap()
    };
    let book = portfolio("5000");
    let holdings = BTreeMap::new();
    let run = |candidate: &Candidate,
               snapshot: &Snapshot,
               portfolio: &Portfolio,
               settings: OptionPreflightSettings<'_>| {
        paper_option_preflight(
            candidate, snapshot, portfolio, &holdings, &catalog, &fo_fees, &calendar, now,
            settings, true,
        )
        .unwrap()
    };
    // 65 units x 50 premium + the configured FO fee, never the index notional.
    let verdict = run(
        &candidate("50", "option-snapshot"),
        &snapshot("49.95", "50", 650, now_ms),
        &book,
        settings,
    );
    assert!(verdict.accepted);
    assert_eq!(verdict.lots, 1);
    assert_eq!(verdict.reservation, Money::parse("3250.05").unwrap());
    assert!(verdict.reservation.amount() < Amount::from_integer(1_690_000));
    // Available cash including fees stays binding.
    let small = portfolio("3000");
    let verdict = run(
        &candidate("50", "option-snapshot"),
        &snapshot("49.95", "50", 650, now_ms),
        &small,
        settings,
    );
    assert!(!verdict.accepted);
    assert!(verdict.reasons.contains(&"paper_option_cash".to_string()));
    // No cash liquidity/circuit screens apply to premiums: a cheap premium
    // below the cash minimum price is still a valid option order.
    let verdict = run(
        &candidate("5", "option-snapshot"),
        &snapshot("4.95", "5", 65, now_ms),
        &book,
        settings,
    );
    assert!(verdict.accepted);
    assert_eq!(verdict.lots, 1);
    assert_eq!(verdict.reservation, Money::parse("325.05").unwrap());
    // A stale original receipt fails closed even with a matching snapshot.
    let verdict = run(
        &candidate("50", "option-snapshot"),
        &snapshot("49.95", "50", 650, now_ms - 2_001),
        &book,
        settings,
    );
    assert!(!verdict.accepted);
    assert!(
        verdict
            .reasons
            .contains(&"stale_or_missing_quote".to_string())
    );
    // A future or another-underlying contract never enters this preflight.
    let futures: Candidate = serde_json::from_value(json!({
        "candidate_id": "future-fixture", "strategy": "fixture", "snapshot_id": "option-snapshot",
        "created_at": "2026-10-16T04:29:00Z", "valid_until": "2026-10-16T04:35:00Z",
        "legs": [{"instrument": {"key":"NSE_FO|NIFTY26OCTFUT","symbol":"NIFTY26OCTFUT",
            "kind":"future","lot_size":50,"tick_size":"0.05","version":"fixture-v1",
            "expiry":"2026-10-20","underlying":"NIFTY","segment":"NSE_FO"},
            "side":"buy","limit_price":"50"}],
        "planned_stop_loss_inr": "0", "stress_loss_inr": "50"
    }))
    .unwrap();
    let verdict = run(
        &futures,
        &snapshot("49.95", "50", 650, now_ms),
        &book,
        settings,
    );
    assert!(!verdict.accepted);
    assert!(verdict.reasons.contains(&"paper_option_scope".to_string()));
    // Expiry-day opening is refused; the position would be held past expiry.
    let verdict = run(
        &candidate("50", "option-snapshot"),
        &snapshot("49.95", "50", 650, now_ms),
        &book,
        OptionPreflightSettings {
            today: ExchangeDate::parse("2026-10-20").unwrap(),
            ..settings
        },
    );
    assert!(!verdict.accepted);
    assert!(verdict.reasons.contains(&"paper_option_expiry".to_string()));
}
