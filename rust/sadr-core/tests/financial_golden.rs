//! Independent golden outputs exported from actual Python domain methods.

use sadr_core::finance::{Amount, CashOrderGrid, FeeArithmetic, Money, Price, Side};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vectors {
    schema_version: u32,
    reference: String,
    reference_domain_sha256: String,
    reference_decimal_precision: u32,
    synthetic: bool,
    fee_cases: Vec<FeeCase>,
    cash_cases: Vec<CashCase>,
    lot_cases: Vec<LotCase>,
    order_cases: Vec<OrderCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeeCase {
    id: String,
    policy: FeeArithmetic,
    side: Side,
    turnover: Money,
    expected_charge: Money,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CashCase {
    id: String,
    policy: FeeArithmetic,
    turnover: Money,
    round_trip: bool,
    slippage_legs: u8,
    expected_requirement: Money,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LotCase {
    id: String,
    policy: FeeArithmetic,
    turnover_per_lot: Price,
    budget: Amount,
    maximum_lots: i64,
    round_trip: bool,
    slippage_legs: u8,
    expected_lots: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderCase {
    id: String,
    lot_size: u64,
    tick_size: Price,
    units: i64,
    // Price validation is itself part of these cases, including negative/zero.
    price: Amount,
    expected_valid: bool,
}

fn vectors() -> Vectors {
    let vectors: Vectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/financial_vectors.json"
    ))
    .unwrap();
    assert_eq!(vectors.schema_version, 1);
    assert_eq!(vectors.reference, "python-sadr");
    assert_eq!(vectors.reference_decimal_precision, 28);
    assert_eq!(vectors.reference_domain_sha256.len(), 64);
    assert!(
        vectors
            .reference_domain_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert!(vectors.synthetic);
    vectors
}

#[test]
fn fee_charge_matches_python_vectors() {
    let vectors = vectors();
    assert_eq!(vectors.fee_cases.len(), 48);
    for case in vectors.fee_cases {
        assert_eq!(
            case.policy.charge(case.side, case.turnover).unwrap(),
            case.expected_charge,
            "{}",
            case.id
        );
    }
    cash_requirement_matches_python_vectors();
}

fn cash_requirement_matches_python_vectors() {
    let vectors = vectors();
    assert_eq!(vectors.cash_cases.len(), 96);
    for case in vectors.cash_cases {
        assert_eq!(
            case.policy
                .cash_requirement(case.turnover, case.round_trip, case.slippage_legs)
                .unwrap(),
            case.expected_requirement,
            "{}",
            case.id
        );
    }
}

#[test]
fn affordable_cash_lots_match_python_vectors() {
    let vectors = vectors();
    assert_eq!(vectors.lot_cases.len(), 32);
    for case in vectors.lot_cases {
        assert_eq!(
            case.policy
                .affordable_cash_lots(
                    case.turnover_per_lot,
                    case.budget,
                    case.maximum_lots,
                    case.round_trip,
                    case.slippage_legs
                )
                .unwrap(),
            case.expected_lots,
            "{}",
            case.id
        );
    }
    cash_order_grid_matches_python_vectors();
}

fn cash_order_grid_matches_python_vectors() {
    let vectors = vectors();
    assert_eq!(vectors.order_cases.len(), 12);
    for case in vectors.order_cases {
        let grid = CashOrderGrid::new(case.lot_size, case.tick_size).unwrap();
        let valid = Price::try_from(case.price)
            .and_then(|price| grid.validate(case.units, price))
            .is_ok();
        assert_eq!(valid, case.expected_valid, "{}", case.id);
    }
}
