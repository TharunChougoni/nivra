//! Synthetic replay. A later bar cannot decide an earlier point, and fees stay on.

use crate::{
    finance::{FeeArithmetic, FinancialError, Money, Side},
    time::Timestamp,
};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub at: Timestamp,
    pub close: Money,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Replay {
    cost: Money,
    bars_used: usize,
}

impl Replay {
    pub fn cost(&self) -> Money {
        self.cost
    }

    pub fn bars_used(&self) -> usize {
        self.bars_used
    }

    pub fn promotion_authorized(&self) -> bool {
        false
    }

    pub fn trading_authorized(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BacktestError {
    Leakage,
    Empty,
    Cost,
}

impl fmt::Display for BacktestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Leakage => "backtest_leakage_rejected",
            Self::Empty => "backtest_empty",
            Self::Cost => "backtest_cost_rejected",
        })
    }
}

impl Error for BacktestError {}

pub fn replay(
    bars: &[Bar],
    decision_at: Timestamp,
    fee: &FeeArithmetic,
    turnover: Money,
) -> Result<Replay, BacktestError> {
    if bars.is_empty() {
        return Err(BacktestError::Empty);
    }
    if bars.iter().any(|bar| bar.at > decision_at) {
        return Err(BacktestError::Leakage);
    }
    let charge = fee.charge(Side::Buy, turnover).map_err(cost_error)?;
    if charge <= Money::ZERO {
        return Err(BacktestError::Cost);
    }
    let total = turnover
        .amount()
        .checked_add(charge.amount())
        .map_err(cost_error)?;
    let cost = Money::try_from(total).map_err(cost_error)?;
    Ok(Replay {
        cost,
        bars_used: bars.len(),
    })
}

fn cost_error(error: FinancialError) -> BacktestError {
    let _ = error;
    BacktestError::Cost
}
