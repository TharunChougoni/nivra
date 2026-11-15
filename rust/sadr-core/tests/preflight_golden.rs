//! Actual Python reference outputs; synthetic data grants no runtime authority.
use sadr_core::{
    candidate::Candidate,
    finance::{FeeArithmetic, Money, Price},
    market::{Calendar, Snapshot},
    risk::{PaperCashPolicy, Portfolio, paper_cash_preflight},
    time::{ExchangeDate, Timestamp},
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vectors {
    schema_version: u32,
    reference: String,
    reference_domain_sha256: String,
    reference_risk_sha256: String,
    synthetic: bool,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fees {
    model_version: String,
    as_of: ExchangeDate,
    synthetic: bool,
    per_order: Money,
    buy_bps: Money,
    sell_bps: Money,
    slippage_bps: Money,
    rounding_increment: Option<Price>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    candidate: Candidate,
    snapshot: Snapshot,
    portfolio: Portfolio,
    fees: Fees,
    calendar: Calendar,
    now: Timestamp,
    policy: PaperCashPolicy,
    allow_refreshed_snapshot: bool,
    expected: Expected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    accepted: bool,
    reasons: Vec<String>,
    lots: u64,
    reservation: Money,
}

#[test]
fn complete_cash_preflight_matches_actual_python_verdicts() {
    let vectors: Vectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/preflight_vectors.json"
    ))
    .unwrap();
    assert_eq!(vectors.schema_version, 1);
    assert_eq!(vectors.reference, "python-sadr");
    assert_eq!(vectors.reference_domain_sha256.len(), 64);
    assert_eq!(vectors.reference_risk_sha256.len(), 64);
    assert!(vectors.synthetic);
    assert_eq!(vectors.cases.len(), 51);
    for case in vectors.cases {
        assert_eq!(case.fees.model_version, "synthetic-rust-preflight-v1");
        assert_eq!(case.fees.as_of, ExchangeDate::parse("2026-10-15").unwrap());
        assert!(case.fees.synthetic);
        let fees = FeeArithmetic {
            per_order: case.fees.per_order,
            buy_bps: case.fees.buy_bps,
            sell_bps: case.fees.sell_bps,
            slippage_bps: case.fees.slippage_bps,
            rounding_increment: case.fees.rounding_increment,
        };
        let actual = paper_cash_preflight(
            &case.candidate,
            &case.snapshot,
            &case.portfolio,
            &fees,
            &case.calendar,
            case.now,
            case.policy,
            case.allow_refreshed_snapshot,
        )
        .unwrap();
        assert_eq!(actual.accepted, case.expected.accepted, "{}", case.id);
        assert_eq!(actual.reasons, case.expected.reasons, "{}", case.id);
        assert_eq!(actual.lots, case.expected.lots, "{}", case.id);
        assert_eq!(actual.reservation, case.expected.reservation, "{}", case.id);
    }
}
