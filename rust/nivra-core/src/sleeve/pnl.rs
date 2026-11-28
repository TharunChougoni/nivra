use super::*;
use crate::execution::Portfolio;
use std::collections::{BTreeMap, VecDeque};

/// Net marked P&L of a strategy's cash entries since the session opened.
/// FIFO attributes each exit to its entry, including protective and manual
/// exits. Earlier inventory and entries made by other strategies keep their
/// place in the queue. A strategy that opened nothing has no result yet.
pub fn session_strategy_pnl(
    book: &Portfolio,
    session_open_ms: i64,
    strategy_id: &str,
) -> Result<Option<Amount>> {
    let owned_entry = |fill: &crate::execution::Fill| {
        fill.buy
            && fill.at >= session_open_ms
            && fill.decision_id.as_deref() == Some(strategy_id)
            && Sleeve::of_key(&fill.instrument.key) == Sleeve::Cash
    };
    let mut lots: BTreeMap<&str, VecDeque<(u64, bool)>> = book
        .fills
        .iter()
        .filter(|fill| owned_entry(fill))
        .map(|fill| (fill.instrument.key.as_str(), VecDeque::new()))
        .collect();
    if lots.is_empty() {
        return Ok(None);
    }
    let mut pnl = Amount::ZERO;
    for fill in &book.fills {
        let key = fill.instrument.key.as_str();
        let Some(queue) = lots.get_mut(key) else {
            continue;
        };
        if fill.buy {
            let owned = owned_entry(fill);
            queue.push_back((fill.units, owned));
            if owned {
                let cost = fill
                    .price
                    .amount()
                    .checked_mul(Amount::from_unsigned(fill.units))?;
                pnl = pnl.checked_sub(cost.checked_add(fill.fee)?)?;
            }
            continue;
        }

        let mut remaining = fill.units;
        let mut owned_units = 0;
        while remaining > 0 {
            let (units, owned) = queue.front_mut().ok_or("unknown_opening_lot")?;
            let taken = remaining.min(*units);
            remaining -= taken;
            *units -= taken;
            if *owned {
                owned_units += taken;
            }
            if *units == 0 {
                queue.pop_front();
            }
        }
        if owned_units > 0 {
            let units = Amount::from_unsigned(owned_units);
            let proceeds = fill.price.amount().checked_mul(units)?;
            let fee = if owned_units == fill.units {
                fill.fee
            } else {
                let share = fill
                    .fee
                    .checked_mul(units)?
                    .checked_div(Amount::from_unsigned(fill.units))?;
                let decimal: rust_decimal::Decimal = share.to_string().parse()?;
                Amount::parse(&decimal.round_dp(8).to_string())?
            };
            pnl = pnl.checked_add(proceeds.checked_sub(fee)?)?;
        }
    }

    for (key, queue) in lots {
        let units: u64 = queue
            .into_iter()
            .filter_map(|(units, owned)| owned.then_some(units))
            .sum();
        if units > 0 {
            let position = book.positions.get(key).ok_or("missing_position_mark")?;
            pnl = pnl.checked_add(
                position
                    .mark
                    .amount()
                    .checked_mul(Amount::from_unsigned(units))?,
            )?;
        }
    }
    Ok(Some(pnl))
}
