use sadr_core::backtest::{BacktestError, Bar, replay};
use sadr_core::finance::{FeeArithmetic, Money};
use sadr_core::time::Timestamp;

fn at(millis: i64) -> Timestamp {
    Timestamp::from_epoch_millis(millis).unwrap()
}

fn fee() -> FeeArithmetic {
    FeeArithmetic {
        per_order: Money::parse("1").unwrap(),
        buy_bps: Money::ZERO,
        sell_bps: Money::ZERO,
        slippage_bps: Money::ZERO,
        rounding_increment: None,
    }
}

#[test]
fn a_future_bar_is_leakage_and_a_past_bar_keeps_its_fee() {
    let bars = [Bar {
        at: at(2_000),
        close: Money::parse("10").unwrap(),
    }];
    assert_eq!(
        replay(&bars, at(1_000), &fee(), Money::parse("10").unwrap()),
        Err(BacktestError::Leakage)
    );
    let replayed = replay(&bars, at(2_000), &fee(), Money::parse("10").unwrap()).unwrap();
    assert_eq!(replayed.cost(), Money::parse("11").unwrap());
    assert_eq!(replayed.bars_used(), 1);
    assert!(!replayed.promotion_authorized());
    assert!(!replayed.trading_authorized());
}
