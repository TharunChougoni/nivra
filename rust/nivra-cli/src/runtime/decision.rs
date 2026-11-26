//! Shared pi decision-turn context, plan supersession and after-cost activity figures.
use super::*;
use nivra_core::execution::{Instrument, Portfolio};
use nivra_core::finance::{Amount, Price};
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) struct Context<'a> {
    pub db: &'a mut Connection,
    pub portfolio: &'a Portfolio,
    pub instruments: &'a [Instrument],
}

pub(crate) fn in_window(c: &Config, now: i64) -> bool {
    local(now).date_naive() == c.session
        && (c.execution.session_start_minute..c.execution.session_end_minute).contains(&minute(now))
}

pub(crate) fn supersede_plan_actions(
    db: &Connection,
    c: &Config,
    id: &str,
    reason: &str,
) -> Result<()> {
    let raw: String = db.query_row("SELECT body FROM planner_plan WHERE id=?", [id], |r| {
        r.get(0)
    })?;
    let plan: Value = serde_json::from_str(&raw)?;
    planner::validate_saved(c, &plan)?;
    if plan["decision_id"] != id {
        return Err("owner_plan_identity_invalid".into());
    }
    let verdicts: Vec<planner::Verdict> = serde_json::from_value(plan["verdicts"].clone())?;
    for verdict in verdicts.into_iter().filter(|v| v.action.is_some()) {
        db.execute(
            "INSERT OR IGNORE INTO planner_action VALUES(?,?,?)",
            rusqlite::params![id, verdict.index as i64, reason],
        )?;
    }
    Ok(())
}

/// After-cost figures from the book's own fills. Conversion, abstention,
/// latency and experiment attribution came from the retired orchestrator
/// ledger and stay at their zero values.
pub(crate) fn activity_analytics(c: &Config, portfolio: &Portfolio) -> Result<Value> {
    let session = c.session.to_string();
    struct OpenLot {
        units: u64,
        price: Price,
    }

    let fills_executed = portfolio.fills.len() as u64;
    let conversion = activity_attribution::CandidateConversion::new(0, 0, 0, 0, fills_executed);

    let mut lots = BTreeMap::<String, std::collections::VecDeque<OpenLot>>::new();
    let mut gross_realized = Amount::ZERO;
    let mut total_notional = Amount::ZERO;
    let mut total_fees = Amount::ZERO;
    let mut total_slippage = Amount::ZERO;

    let capital = portfolio.allocation().unwrap_or(portfolio.cash);
    let mut dd_points = Vec::new();
    dd_points.push((0u64, capital));
    let mut current_realized_cum = capital;

    for fill in &portfolio.fills {
        let notional = fill
            .price
            .amount()
            .checked_mul(Amount::from_unsigned(fill.units))
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        total_notional = total_notional
            .checked_add(notional)
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        total_fees = total_fees
            .checked_add(fill.fee)
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;

        if let Some(expected) = fill.expected_price {
            let adverse = if fill.buy {
                if fill.price.amount() > expected.amount() {
                    fill.price
                        .amount()
                        .checked_sub(expected.amount())
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?
                } else {
                    Amount::ZERO
                }
            } else if expected.amount() > fill.price.amount() {
                expected
                    .amount()
                    .checked_sub(fill.price.amount())
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?
            } else {
                Amount::ZERO
            };
            let slip_notional = adverse
                .checked_mul(Amount::from_unsigned(fill.units))
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            total_slippage = total_slippage
                .checked_add(slip_notional)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        }

        let mut fill_realized_pnl = Amount::ZERO;
        let queue = lots.entry(fill.instrument.key.clone()).or_default();
        if fill.buy {
            queue.push_back(OpenLot {
                units: fill.units,
                price: fill.price,
            });
        } else {
            let mut remaining = fill.units;
            let mut gross_buy_notional = Amount::ZERO;
            while remaining > 0 && !queue.is_empty() {
                let lot = queue.front_mut().unwrap();
                let taken = remaining.min(lot.units);
                let lot_notional = lot
                    .price
                    .amount()
                    .checked_mul(Amount::from_unsigned(taken))
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                gross_buy_notional = gross_buy_notional
                    .checked_add(lot_notional)
                    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                lot.units -= taken;
                remaining -= taken;
                if lot.units == 0 {
                    queue.pop_front();
                }
            }
            let sale_pnl = notional
                .checked_sub(gross_buy_notional)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            gross_realized = gross_realized
                .checked_add(sale_pnl)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            fill_realized_pnl = sale_pnl;
        }
        current_realized_cum = current_realized_cum
            .checked_add(fill_realized_pnl)
            .and_then(|equity| equity.checked_sub(fill.fee))
            .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        dd_points.push((fill.at.max(0) as u64, current_realized_cum));
    }

    let mut marked_pnl = Amount::ZERO;
    let mut accounted_units: BTreeMap<String, u64> = BTreeMap::new();

    for (inst_key, queue) in lots {
        if let Some(pos) = portfolio.positions.get(&inst_key) {
            for lot in queue {
                if lot.units > 0 {
                    *accounted_units.entry(inst_key.clone()).or_insert(0) += lot.units;
                    let entry_val = lot
                        .price
                        .amount()
                        .checked_mul(Amount::from_unsigned(lot.units))
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    let mark_val = pos
                        .mark
                        .amount()
                        .checked_mul(Amount::from_unsigned(lot.units))
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    let lot_pnl = mark_val
                        .checked_sub(entry_val)
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                    marked_pnl = marked_pnl
                        .checked_add(lot_pnl)
                        .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
                }
            }
        }
    }

    for (key, pos) in &portfolio.positions {
        let accounted = accounted_units.get(key).copied().unwrap_or(0);
        if pos.units > accounted {
            let remaining_units = pos.units - accounted;
            let entry_val = pos
                .entry
                .amount()
                .checked_mul(Amount::from_unsigned(remaining_units))
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            let mark_val = pos
                .mark
                .amount()
                .checked_mul(Amount::from_unsigned(remaining_units))
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            let pos_pnl = mark_val
                .checked_sub(entry_val)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
            marked_pnl = marked_pnl
                .checked_add(pos_pnl)
                .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;
        }
    }

    if let Ok(current_eq) = portfolio.equity() {
        let last_time = portfolio
            .positions
            .values()
            .map(|position| position.marked_at)
            .filter(|at| *at > 0)
            .max()
            .or_else(|| portfolio.fills.last().map(|fill| fill.at))
            .unwrap_or(0)
            .max(0) as u64;
        dd_points.push((last_time, current_eq));
    }

    let financials = activity_attribution::FinancialAfterCost::calculate(
        gross_realized,
        marked_pnl,
        total_fees,
        total_slippage,
    )
    .map_err(|e| Fail::new(format!("financial_calc_error: {e:?}")))?;

    let turnover = activity_attribution::TurnoverCostDrag::calculate(
        total_notional,
        capital,
        total_fees,
        total_slippage,
    )
    .map_err(|e| Fail::new(format!("turnover_calc_error: {e:?}")))?;

    let drawdown = activity_attribution::DrawdownTracker::from_series(&dd_points)
        .map_err(|e| Fail::new(format!("drawdown_calc_error: {e:?}")))?;

    let latency = activity_attribution::DecisionLatencyStats::from_latencies(&[]);

    Ok(activity_attribution::ActivityReport {
        session,
        conversion,
        abstentions: activity_attribution::AbstentionTaxonomy::default(),
        financials,
        turnover,
        drawdown,
        latency,
    }
    .to_json())
}

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::decision_fixture_config;
