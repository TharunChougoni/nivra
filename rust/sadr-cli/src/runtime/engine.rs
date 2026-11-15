use super::Result;
#[cfg(test)]
use super::minute;
use chrono::NaiveDate;
pub use sadr_core::execution::Quote;
use sadr_core::finance::Price;
use sadr_core::finance::{Amount, Money};
#[cfg(test)]
use sadr_core::finance::{FeeArithmetic, Side};
#[cfg(test)]
use sadr_core::market::MAX_FINANCIAL_AGE_MS;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Fill {
    pub at: i64,
    pub side: String,
    pub units: u64,
    pub price: Price,
    pub fee: Money,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Book {
    pub session: NaiveDate,
    pub cash: Amount,
    pub units: u64,
    pub entry: Option<Price>,
    pub high_water: Amount,
    pub entries: u64,
    pub halted: bool,
    pub stopped: bool,
    pub closed: bool,
    pub health: String,
    pub last_good: Option<i64>,
    pub last_decision: i64,
    pub history: Vec<Price>,
    pub fills: Vec<Fill>,
    pub fees: Amount,
    pub fee_policy: String,
}
fn a(s: &str) -> Amount {
    Amount::parse(s).expect("constant decimal")
}
#[cfg(test)]
fn fees() -> FeeArithmetic {
    serde_json::from_str(include_str!(
        "../../../../fixtures/test-legacy-paper-costs.json"
    ))
    .unwrap()
}
impl Book {
    #[cfg(test)]
    pub fn new(session: NaiveDate) -> Self {
        Self {session,cash:a("50000"),units:0,entry:None,high_water:a("50000"),entries:0,halted:false,stopped:false,closed:false,health:"waiting".into(),last_good:None,last_decision:0,history:vec![],fills:vec![],fees:Amount::ZERO,fee_policy:"SIMULATION_ONLY: INR25/order +10bps/side rounded up to paise; 5bps adverse slippage; NOT broker-certified".into()}
    }
    pub fn reconcile(&self) -> Result<()> {
        let mut cash = a("50000");
        let mut units = 0;
        let mut charges = Amount::ZERO;
        let mut entries = 0;
        for f in &self.fills {
            let n = f
                .price
                .amount()
                .checked_mul(Amount::from_unsigned(f.units))?;
            match f.side.as_str() {
                "buy" => {
                    if units != 0 || f.units == 0 {
                        return Err("invalid_fill_sequence".into());
                    }
                    units = f.units;
                    entries += 1;
                    cash = cash.checked_sub(n.checked_add(f.fee.amount())?)?;
                }
                "sell" => {
                    if units != f.units || units == 0 {
                        return Err("invalid_fill_sequence".into());
                    }
                    units = 0;
                    cash = cash.checked_add(n.checked_sub(f.fee.amount())?)?;
                }
                _ => return Err("invalid_fill_side".into()),
            }
            charges = charges.checked_add(f.fee.amount())?;
        }
        let last_entry = self
            .fills
            .iter()
            .rev()
            .find(|f| f.side == "buy")
            .map(|f| f.price);
        if cash != self.cash
            || units != self.units
            || charges != self.fees
            || entries != self.entries
            || self.entries > 10
            || self.history.len() > 20
            || self.fills.len() > 20
            || self.cash < Amount::ZERO
            || (self.units == 0) != self.entry.is_none()
            || (self.units > 0 && self.entry != last_entry)
        {
            return Err("book_reconciliation_failed".into());
        }
        Ok(())
    }
    #[cfg(test)]
    fn fill(&mut self, now: i64, side: Side, units: u64, raw: Price) -> Result<()> {
        let buying = matches!(side, Side::Buy);
        // Round adverse slippage to the verified INR0.01 ETF grid.
        let factor = if buying { a("1.0005") } else { a("0.9995") };
        let unrounded = raw.amount().checked_mul(factor)?;
        let pennies = unrounded.checked_floor_ratio(a("0.01"))?;
        let mut price = a("0.01").checked_mul(Amount::from_integer(i64::try_from(pennies)?))?;
        if buying && price < unrounded {
            price = price.checked_add(a("0.01"))?;
        }
        let price = Price::try_from(price)?;
        let notional = Money::try_from(price.amount().checked_mul(Amount::from_unsigned(units))?)?;
        let fee = fees().charge(side, notional)?;
        self.cash = if buying {
            self.cash
                .checked_sub(notional.amount().checked_add(fee.amount())?)?
        } else {
            self.cash
                .checked_add(notional.amount().checked_sub(fee.amount())?)?
        };
        self.fees = self.fees.checked_add(fee.amount())?;
        self.units = if buying { units } else { 0 };
        self.entry = if buying { Some(price) } else { None };
        if buying {
            self.entries += 1;
        }
        self.fills.push(Fill {
            at: now,
            side: if buying { "buy" } else { "sell" }.into(),
            units,
            price,
            fee,
        });
        Ok(())
    }
    #[cfg(test)]
    pub fn step(&mut self, now: i64, q: Option<&Quote>, strategy: &str) -> Result<String> {
        if self.halted || self.closed || self.stopped {
            return Ok("preserved_terminal_state".into());
        }
        if super::local(now).date_naive() != self.session || !(555..930).contains(&minute(now)) {
            return Ok("outside_session_no_financial_action".into());
        }
        let Some(q) = q.filter(|q| q.fresh(now, 8000)) else {
            // Data loss inhibits financial actions; it does not permanently halt the day.
            self.health = "data_unavailable_no_financial_action".into();
            return Ok(self.health.clone());
        };
        self.last_good = Some(q.receipt);
        self.health = "supervising".into();
        if !q.fresh(now, MAX_FINANCIAL_AGE_MS) {
            self.health = "supervision_grace_entries_blocked".into();
            return Ok(self.health.clone());
        }
        let equity = self.cash.checked_add(
            q.bid
                .amount()
                .checked_mul(Amount::from_unsigned(self.units))?,
        )?;
        self.high_water = self.high_water.max(equity);
        let loss = a("50000").checked_sub(equity)?;
        let drawdown = self.high_water.checked_sub(equity)?;
        let trip = loss >= a("500") || drawdown >= self.high_water.checked_mul(a("0.02"))?;
        if self.units > 0 {
            let entry = self.entry.ok_or("entry_missing")?.amount();
            if minute(now) >= 915
                || trip
                || q.bid.amount() <= entry.checked_mul(a("0.92"))?
                || q.bid.amount() >= entry.checked_mul(a("1.02"))?
            {
                if q.bid_units < self.units {
                    self.health = "exit_depth_unavailable".into();
                    return Ok(self.health.clone());
                }
                self.fill(now, Side::Sell, self.units, q.bid)?;
                if trip {
                    self.halted = true;
                    self.health = "risk_halted".into();
                }
                return Ok("paper_exit".into());
            }
        }
        if trip {
            self.halted = true;
            self.health = "risk_halted".into();
            return Ok(self.health.clone());
        }
        if now - self.last_decision < 30_000 {
            return Ok("supervised".into());
        }
        self.last_decision = now;
        let _ = strategy;
        // The legacy strategy hook has no prospects; that is a no-op, not a
        // failed decision (#221).
        Ok(super::planner::NO_DECISION_WORK.into())
    }
}
