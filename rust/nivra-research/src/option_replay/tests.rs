use super::*;
use nivra_core::{execution::Quote, finance::Price, market::InstrumentKind};
use nivra_store::market_archive::options::GateCandidate;
use serde_json::json;

fn fixture(ask: &str, age: i64, expiry: &str) -> (Policy, Cohort) {
    let session = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    let at = nivra_store::market_archive::ist_midnight(session) + 10 * 3_600_000;
    let price = |value: &str| Price::parse(value).unwrap();
    let contract = OptionContract {
        key: "NSE_FO|fixture".into(),
        symbol: "NIFTY fixture".into(),
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        underlying_symbol: "NIFTY".into(),
        option: InstrumentKind::Call,
        expiry: ExchangeDate::parse(expiry).unwrap(),
        strike: price("26000"),
        tick: price("0.05"),
        lot_size: 65,
        freeze_quantity: None,
        weekly: true,
    };
    let key = contract.key.clone();
    let quote = |bid, ask, receipt| Quote {
        bid: price(bid),
        ask: price(ask),
        bid_units: 65,
        ask_units: 65,
        source: receipt,
        receipt,
    };
    let gate = GateObservation {
        spot: Some(OptionSpot {
            key: contract.underlying_key.clone(),
            price: price("26000"),
            source_ms: at,
            receipt_ms: at,
        }),
        candidates: vec![GateCandidate {
            instrument_key: key.clone(),
            quote: quote("49.95", ask, at - age),
            offset: 0,
            spread_ticks: 1,
            depth_lots: 1,
            round_trip_cost_bps: 10.0,
            tradable: true,
        }],
        input: json!({}),
        verdict: Some(json!({"escalate":true,"prospects":[{"instrument_key":key}]})),
    };
    let opportunity = Opportunity {
        at,
        gate,
        contracts: [(key.clone(), contract)].into(),
        exits: [(key, Some(vec![quote("55", "55.05", at + HOLD_MS)]))].into(),
    };
    let policy = Policy {
        fees: serde_json::from_value(json!({
            "per_order":"20","buy_bps":"0","sell_bps":"0",
            "slippage_bps":"0","rounding_increment":null,
        }))
        .unwrap(),
        sizing: SizingPolicy {
            risk_per_trade_bps: 3000,
            max_position_bps: 10000,
            max_positions: 1,
            min_order_rupees: Amount::from_integer(200),
        },
        capital: Amount::from_integer(5_000),
        session_start_minute: 555,
        session_end_minute: 930,
    };
    (
        policy,
        Cohort {
            session,
            observations: vec![opportunity],
        },
    )
}

#[test]
fn option_replay_requires_two_sided_evidence_and_accounts_in_whole_lots() -> Result<()> {
    let (policy, mut cohort) = fixture("50", 2_000, "2026-10-20");
    let report = compare(&policy, &cohort, Ranking::Gate)?;
    assert_eq!(report.proposed, report.frozen_gate);
    assert_eq!(report.proposed.net_pnl, Some(Amount::from_integer(285)));
    assert_eq!(report.proposed.trades, 1);
    assert_eq!(report.proposed.equity_sleeve_cash, Amount::ZERO);
    assert_eq!(report.proposed.outcomes[0].units, 65);
    cohort.observations[0]
        .exits
        .values_mut()
        .for_each(|q| *q = None);
    let report = compare(&policy, &cohort, Ranking::Gate)?;
    assert_eq!(report.reason, Some(TAPE_UNAVAILABLE));
    assert_eq!(report.proposed.net_pnl, None);
    cohort.observations.clear();
    assert_eq!(
        compare(&policy, &cohort, Ranking::Gate)?.reason,
        Some(TAPE_UNAVAILABLE)
    );
    Ok(())
}

#[test]
fn option_replay_refuses_unaffordable_stale_expired_and_shallow_quotes() -> Result<()> {
    for (ask, age, expiry, code) in [
        ("77", 0, "2026-10-20", "sleeve_cash_insufficient"),
        ("50", 2_001, "2026-10-20", "stale_quote"),
        ("50", 0, "2026-10-16", "option_contract_invalid"),
        ("50", 0, "2026-10-15", "option_contract_invalid"),
    ] {
        let (policy, cohort) = fixture(ask, age, expiry);
        let report = compare(&policy, &cohort, Ranking::Gate)?;
        assert_eq!(report.proposed.outcomes[0].reason, code);
        assert_eq!(report.proposed.trades, 0);
    }
    let (policy, mut cohort) = fixture("50", 0, "2026-10-20");
    cohort.observations[0]
        .exits
        .values_mut()
        .for_each(|q| q.as_mut().unwrap()[0].bid_units = 64);
    let report = compare(&policy, &cohort, Ranking::Gate)?;
    assert_eq!(report.proposed.outcomes[0].reason, "insufficient_depth");
    assert_eq!(report.proposed.net_pnl, None);
    Ok(())
}
