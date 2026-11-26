//! After-cost P&L, turnover and drawdown from the paper book.

use nivra_core::finance::{Amount, FinancialError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
/// Realized and marked P&L after explicit fees, with benchmark slippage reported
/// separately as a diagnostic cost drag. Fill prices already include adverse
/// slippage, so subtracting it from P&L again would count that impact twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinancialAfterCost {
    pub realized_pnl: Amount,
    pub marked_pnl: Amount,
    pub gross_profit: Amount,
    pub total_fees: Amount,
    pub total_slippage: Amount,
    pub total_cost_drag: Amount,
    pub net_after_cost_pnl: Amount,
}

impl FinancialAfterCost {
    pub fn calculate(
        realized_pnl: Amount,
        marked_pnl: Amount,
        total_fees: Amount,
        total_slippage: Amount,
    ) -> Result<Self, FinancialError> {
        let gross_profit = realized_pnl.checked_add(marked_pnl)?;
        let total_cost_drag = total_fees.checked_add(total_slippage)?;
        let net_after_cost_pnl = gross_profit.checked_sub(total_fees)?;
        Ok(Self {
            realized_pnl,
            marked_pnl,
            gross_profit,
            total_fees,
            total_slippage,
            total_cost_drag,
            net_after_cost_pnl,
        })
    }
}

/// Turnover ratio and fee/slippage cost drag metrics in basis points.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnoverCostDrag {
    pub notional_traded: Amount,
    pub capital: Amount,
    pub turnover_ratio: f64,
    pub fee_drag_bps: f64,
    pub slippage_drag_bps: f64,
    pub total_cost_drag_bps: f64,
}

impl TurnoverCostDrag {
    pub fn calculate(
        notional_traded: Amount,
        capital: Amount,
        total_fees: Amount,
        total_slippage: Amount,
    ) -> Result<Self, FinancialError> {
        let notional_f64 = notional_traded.to_string().parse::<f64>().unwrap_or(0.0);
        let capital_f64 = capital.to_string().parse::<f64>().unwrap_or(1.0);
        let turnover_ratio = if capital_f64 > 0.0 {
            notional_f64 / capital_f64
        } else {
            0.0
        };

        let fees_f64 = total_fees.to_string().parse::<f64>().unwrap_or(0.0);
        let slippage_f64 = total_slippage.to_string().parse::<f64>().unwrap_or(0.0);

        let fee_drag_bps = if notional_f64 > 0.0 {
            (fees_f64 / notional_f64) * 10_000.0
        } else {
            0.0
        };
        let slippage_drag_bps = if notional_f64 > 0.0 {
            (slippage_f64 / notional_f64) * 10_000.0
        } else {
            0.0
        };
        let total_cost_drag_bps = fee_drag_bps + slippage_drag_bps;

        Ok(Self {
            notional_traded,
            capital,
            turnover_ratio,
            fee_drag_bps,
            slippage_drag_bps,
            total_cost_drag_bps,
        })
    }
}

/// Peak-to-trough drawdown calculation from equity time-series.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DrawdownTracker {
    pub high_water_mark: Amount,
    pub current_equity: Amount,
    pub current_drawdown: Amount,
    pub max_drawdown: Amount,
    pub max_drawdown_pct: f64,
    pub max_drawdown_duration_ms: u64,
}

impl DrawdownTracker {
    pub fn from_series(points: &[(u64, Amount)]) -> Result<Self, FinancialError> {
        if points.is_empty() {
            return Ok(Self {
                high_water_mark: Amount::ZERO,
                current_equity: Amount::ZERO,
                current_drawdown: Amount::ZERO,
                max_drawdown: Amount::ZERO,
                max_drawdown_pct: 0.0,
                max_drawdown_duration_ms: 0,
            });
        }

        let mut peak = points[0].1;
        let mut peak_time = points[0].0;
        let mut max_dd = Amount::ZERO;
        let mut max_dd_pct = 0.0;
        let mut max_dd_duration = 0u64;

        for &(time, equity) in points {
            if equity > peak {
                peak = equity;
                peak_time = time;
            } else {
                let dd = peak.checked_sub(equity)?;
                if dd > max_dd {
                    max_dd = dd;
                    let peak_f64 = peak.to_string().parse::<f64>().unwrap_or(1.0);
                    let dd_f64 = dd.to_string().parse::<f64>().unwrap_or(0.0);
                    max_dd_pct = if peak_f64 > 0.0 {
                        (dd_f64 / peak_f64) * 100.0
                    } else {
                        0.0
                    };
                    max_dd_duration = time.saturating_sub(peak_time);
                }
            }
        }

        let current_equity = points.last().map(|p| p.1).unwrap_or(Amount::ZERO);
        let current_drawdown = if peak > current_equity {
            peak.checked_sub(current_equity)?
        } else {
            Amount::ZERO
        };

        Ok(Self {
            high_water_mark: peak,
            current_equity,
            current_drawdown,
            max_drawdown: max_dd,
            max_drawdown_pct: max_dd_pct,
            max_drawdown_duration_ms: max_dd_duration,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityReport {
    pub session: String,
    pub financials: FinancialAfterCost,
    pub turnover: TurnoverCostDrag,
    pub drawdown: DrawdownTracker,
}

impl ActivityReport {
    pub fn to_json(&self) -> Value {
        json!({
            "session": self.session,
            "financials": self.financials,
            "turnover": self.turnover,
            "drawdown": self.drawdown,
            "drawdown_coverage": {
                "basis": "realized_fill_checkpoints_plus_current_marks",
                "complete": false,
                "reason": "historical position marks are not durably retained with this report",
            },
        })
    }
}
