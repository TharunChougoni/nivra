//! Capital sleeves and book-aware order sizing.
//!
//! One book holds one cash balance. Each sleeve's share of it is derived from
//! the allocation ledger and the fills of that sleeve's instruments, so cash
//! keeps a single owner and `Portfolio::reconcile` can check the split.
//! `size` turns a sleeve's state and its configured policy into whole lots.
use crate::execution::Result;
use crate::finance::{Amount, FeeArithmetic, Money, Price, Side};
use crate::market::Segment;
use serde::{Deserialize, Serialize};

/// Where an instrument's capital comes from. `NSE_FO` keys trade from the
/// options sleeve; every other tradeable key is cash equity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sleeve {
    Cash,
    Options,
}

impl Sleeve {
    pub const ALL: [Sleeve; 2] = [Sleeve::Cash, Sleeve::Options];

    pub fn of_key(key: &str) -> Self {
        match Segment::from_key(key) {
            Some(Segment::NseFo) => Self::Options,
            _ => Self::Cash,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cash => "cash",
            Self::Options => "options",
        }
    }
}

/// One entry of the book's allocation ledger. Entries are only appended.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Allocation {
    pub at_ms: i64,
    pub sleeve: Sleeve,
    pub amount: Amount,
    pub reason: String,
}

/// One sleeve's state, derived from the book. `pnl` is marked equity minus
/// allocated capital.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SleeveView {
    pub sleeve: Sleeve,
    pub capital: Amount,
    pub cash: Amount,
    pub equity: Amount,
    pub pnl: Amount,
    pub open_positions: usize,
}

/// How one sleeve sizes a buy. Basis points are of the sleeve's equity.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SizingPolicy {
    /// Sleeve equity lost if the stop fills, round-trip costs included.
    pub risk_per_trade_bps: u32,
    /// Largest single position.
    pub max_position_bps: u32,
    /// Open positions the sleeve may hold.
    pub max_positions: u32,
    /// Below this notional the buy is refused rather than placed as dust.
    pub min_order_rupees: Amount,
}

impl SizingPolicy {
    /// The shipped cash-sleeve policy, for offline replays that name none.
    pub fn shipped_cash() -> Self {
        Self {
            risk_per_trade_bps: 50,
            max_position_bps: 4000,
            max_positions: 3,
            min_order_rupees: Amount::from_integer(200),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !(1..=10_000).contains(&self.risk_per_trade_bps)
            || !(1..=10_000).contains(&self.max_position_bps)
            || self.max_positions == 0
            || self.min_order_rupees < Amount::ZERO
        {
            return Err("invalid_sizing_policy".into());
        }
        Ok(())
    }
}

/// The runtime sizing policy of every sleeve, from the paper config.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SleevePolicies {
    pub cash: SizingPolicy,
    pub options: SizingPolicy,
}

impl SleevePolicies {
    pub fn get(&self, sleeve: Sleeve) -> &SizingPolicy {
        match sleeve {
            Sleeve::Cash => &self.cash,
            Sleeve::Options => &self.options,
        }
    }
}

/// One sleeve in the paper config: its capital and its sizing policy.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SleeveConfig {
    pub capital: Amount,
    pub risk_per_trade_bps: u32,
    pub max_position_bps: u32,
    pub max_positions: u32,
    pub min_order_rupees: Amount,
}

impl SleeveConfig {
    pub fn policy(&self) -> SizingPolicy {
        SizingPolicy {
            risk_per_trade_bps: self.risk_per_trade_bps,
            max_position_bps: self.max_position_bps,
            max_positions: self.max_positions,
            min_order_rupees: self.min_order_rupees,
        }
    }
}

/// The paper config's `sleeves`. Their capitals must add up to `capital`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SleeveConfigs {
    pub cash: SleeveConfig,
    pub options: SleeveConfig,
}

impl SleeveConfigs {
    pub fn get(&self, sleeve: Sleeve) -> &SleeveConfig {
        match sleeve {
            Sleeve::Cash => &self.cash,
            Sleeve::Options => &self.options,
        }
    }

    pub fn validate(&self, capital: Amount) -> Result<()> {
        let mut total = Amount::ZERO;
        for sleeve in Sleeve::ALL {
            let config = self.get(sleeve);
            config.policy().validate()?;
            if config.capital < Amount::ZERO {
                return Err("sleeve_capital_invalid".into());
            }
            total = total.checked_add(config.capital)?;
        }
        if total != capital {
            return Err("sleeve_capital_sum_mismatch".into());
        }
        Ok(())
    }

    pub fn policies(&self) -> SleevePolicies {
        SleevePolicies {
            cash: self.cash.policy(),
            options: self.options.policy(),
        }
    }

    /// The opening allocation ledger of a fresh book. A sleeve with no
    /// capital gets no entry.
    pub fn allocations(&self, at_ms: i64) -> Vec<Allocation> {
        Sleeve::ALL
            .into_iter()
            .filter(|sleeve| self.get(*sleeve).capital > Amount::ZERO)
            .map(|sleeve| Allocation {
                at_ms,
                sleeve,
                amount: self.get(sleeve).capital,
                reason: "config_sleeves".into(),
            })
            .collect()
    }
}

/// The book state and order facts one sizing decision reads.
pub struct SizingInput<'a> {
    pub sleeve_equity: Amount,
    pub sleeve_cash: Amount,
    pub open_positions: u32,
    /// False when the buy adds to a position the sleeve already holds.
    pub opens_position: bool,
    /// Value of the units of this instrument already held, which count
    /// against the position cap.
    pub held_value: Amount,
    /// The worst fill price the order accepts.
    pub max_price: Price,
    pub stop: Price,
    pub ask_units: u64,
    pub lot: u64,
    pub costs: &'a FeeArithmetic,
    /// Scales the risk budget, in (0, 1].
    pub conviction: Amount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizingRefusal {
    SleevePositionsFull,
    StopNotBelowPrice,
    SizeBelowLot,
    SleeveCashInsufficient,
    SizeBelowMinOrder,
    PolicyExceeded,
}

impl SizingRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::SleevePositionsFull => "sleeve_positions_full",
            Self::StopNotBelowPrice => "size_stop_not_below_price",
            Self::SizeBelowLot => "size_below_lot",
            Self::SleeveCashInsufficient => "sleeve_cash_insufficient",
            Self::SizeBelowMinOrder => "size_below_min_order",
            Self::PolicyExceeded => "sizing_policy_exceeded",
        }
    }
}

fn bps(value: Amount, points: Amount) -> Result<Amount> {
    Ok(value
        .checked_mul(points)?
        .checked_mul(Amount::parse("0.0001")?)?)
}
fn whole(points: u32) -> Amount {
    Amount::from_unsigned(u64::from(points))
}

/// Which limits `units` keeps: `(cash, caps)`. Cash pays the notional at
/// `max_price` plus the buy fee. The caps are the position limit and the
/// risk budget: the loss from `max_price` to `stop`, both fees and the
/// sell-side slippage below the stop.
fn fits(input: &SizingInput, units: u64, risk_budget: Amount, cap: Amount) -> Result<(bool, bool)> {
    let n = Amount::from_unsigned(units);
    let notional = input.max_price.amount().checked_mul(n)?;
    let buy_fee = input
        .costs
        .charge(Side::Buy, Money::try_from(notional)?)?
        .amount();
    let exit = input.stop.amount().checked_mul(n)?;
    let sell_fee = input
        .costs
        .charge(Side::Sell, Money::try_from(exit)?)?
        .amount();
    let risk = notional
        .checked_sub(exit)?
        .checked_add(buy_fee)?
        .checked_add(sell_fee)?
        .checked_add(bps(exit, input.costs.slippage_bps.amount())?)?;
    Ok((
        notional.checked_add(buy_fee)? <= input.sleeve_cash,
        notional.checked_add(input.held_value)? <= cap && risk <= risk_budget,
    ))
}

/// Units for one buy: the most whole lots that the risk budget, the position
/// cap, the sleeve's cash and the displayed depth all allow. With `requested`
/// units, admit them only when they fit the same limits. `Ok(Err(_))` is a
/// refusal with a stable snake_case code.
pub fn size(
    policy: &SizingPolicy,
    input: &SizingInput,
    requested: Option<u64>,
) -> Result<std::result::Result<u64, SizingRefusal>> {
    use SizingRefusal::*;
    if input.lot == 0 {
        return Err("sizing_lot_invalid".into());
    }
    if input.opens_position && input.open_positions >= policy.max_positions {
        return Ok(Err(SleevePositionsFull));
    }
    if input.stop >= input.max_price {
        return Ok(Err(StopNotBelowPrice));
    }
    let equity = input.sleeve_equity.max(Amount::ZERO);
    let risk_budget =
        bps(equity, whole(policy.risk_per_trade_bps))?.checked_mul(input.conviction)?;
    let cap = bps(equity, whole(policy.max_position_bps))?;
    let units = if let Some(units) = requested {
        // Named units must fit the same limits; the execution gate checks
        // the lot grid and the displayed depth.
        match fits(input, units, risk_budget, cap)? {
            (false, _) => return Ok(Err(SleeveCashInsufficient)),
            (true, false) => return Ok(Err(PolicyExceeded)),
            (true, true) => units,
        }
    } else {
        let max_lots = input.ask_units / input.lot;
        if max_lots == 0 {
            return Ok(Err(SizeBelowLot));
        }
        match fits(input, input.lot, risk_budget, cap)? {
            (false, _) => return Ok(Err(SleeveCashInsufficient)),
            (true, false) => return Ok(Err(SizeBelowLot)),
            (true, true) => {}
        }
        let (mut low, mut high) = (1, max_lots);
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            let units = middle.checked_mul(input.lot).ok_or("sizing_units_range")?;
            if fits(input, units, risk_budget, cap)? == (true, true) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        low * input.lot
    };
    if input
        .max_price
        .amount()
        .checked_mul(Amount::from_unsigned(units))?
        < policy.min_order_rupees
    {
        return Ok(Err(SizeBelowMinOrder));
    }
    Ok(Ok(units))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Amount {
        Amount::parse(s).unwrap()
    }
    fn p(s: &str) -> Price {
        Price::parse(s).unwrap()
    }
    fn costs() -> FeeArithmetic {
        serde_json::from_value(serde_json::json!({"per_order":"0","buy_bps":"0",
            "sell_bps":"0","slippage_bps":"0","rounding_increment":null}))
        .unwrap()
    }
    fn policy() -> SizingPolicy {
        SizingPolicy {
            risk_per_trade_bps: 50,
            max_position_bps: 4000,
            max_positions: 3,
            min_order_rupees: a("200"),
        }
    }
    fn input<'a>(costs: &'a FeeArithmetic, stop: &str) -> SizingInput<'a> {
        SizingInput {
            sleeve_equity: a("5000"),
            sleeve_cash: a("5000"),
            open_positions: 0,
            opens_position: true,
            held_value: a("0"),
            max_price: p("100"),
            stop: p(stop),
            ask_units: 1000,
            lot: 1,
            costs,
            conviction: a("1"),
        }
    }

    #[test]
    fn a_wider_stop_buys_fewer_units() {
        let costs = costs();
        // Risk budget 25 rupees: a 1-rupee stop allows 25 units, capped by
        // the 2000-rupee position limit at 20; a 5-rupee stop allows 5.
        assert_eq!(size(&policy(), &input(&costs, "99"), None).unwrap(), Ok(20));
        assert_eq!(size(&policy(), &input(&costs, "95"), None).unwrap(), Ok(5));
        let half = SizingInput {
            conviction: a("0.5"),
            ..input(&costs, "95")
        };
        assert_eq!(size(&policy(), &half, None).unwrap(), Ok(2));
    }

    #[test]
    fn options_size_to_whole_lots_from_the_sleeve() {
        let costs = costs();
        let options = SizingPolicy {
            risk_per_trade_bps: 3000,
            max_position_bps: 10_000,
            max_positions: 1,
            min_order_rupees: a("200"),
        };
        let lot = |max_price: &str, stop: &str| SizingInput {
            max_price: p(max_price),
            stop: p(stop),
            lot: 75,
            ..input(&costs, stop)
        };
        // One lot at 60 is 4500 of the 5000 sleeve; two lots do not fit.
        assert_eq!(size(&options, &lot("60", "40"), None).unwrap(), Ok(75));
        assert_eq!(
            size(&options, &lot("70", "50"), None).unwrap(),
            Err(SizingRefusal::SleeveCashInsufficient)
        );
        assert_eq!(
            size(&options, &lot("60", "40"), Some(150)).unwrap(),
            Err(SizingRefusal::SleeveCashInsufficient)
        );
        let deep = SizingInput {
            sleeve_equity: a("20000"),
            sleeve_cash: a("20000"),
            ..lot("60", "30")
        };
        // The budget is 30% of 20000, 6000. Two lots risk 4500; four lots
        // fit the cash but risk 9000.
        assert_eq!(size(&options, &deep, Some(150)).unwrap(), Ok(150));
        assert_eq!(
            size(&options, &deep, Some(300)).unwrap(),
            Err(SizingRefusal::PolicyExceeded)
        );
        assert_eq!(size(&options, &lot("60", "40"), Some(75)).unwrap(), Ok(75));
    }

    #[test]
    fn refusals_name_the_limit() {
        let costs = costs();
        let full = SizingInput {
            open_positions: 3,
            ..input(&costs, "99")
        };
        assert_eq!(
            size(&policy(), &full, None).unwrap(),
            Err(SizingRefusal::SleevePositionsFull)
        );
        let adding = SizingInput {
            opens_position: false,
            ..full
        };
        assert!(size(&policy(), &adding, None).unwrap().is_ok());
        assert_eq!(
            size(&policy(), &input(&costs, "100"), None).unwrap(),
            Err(SizingRefusal::StopNotBelowPrice)
        );
        // A 30-rupee stop risks more than the 25-rupee budget on one unit.
        assert_eq!(
            size(&policy(), &input(&costs, "70"), None).unwrap(),
            Err(SizingRefusal::SizeBelowLot)
        );
        // Depth for one unit at 100 is below the 200-rupee floor.
        let thin = SizingInput {
            ask_units: 1,
            ..input(&costs, "99")
        };
        assert_eq!(
            size(&policy(), &thin, None).unwrap(),
            Err(SizingRefusal::SizeBelowMinOrder)
        );
    }

    #[test]
    fn fees_count_against_cash_and_risk() {
        let costs: FeeArithmetic = serde_json::from_value(serde_json::json!({
            "per_order":"20","buy_bps":"0","sell_bps":"0","slippage_bps":"0",
            "rounding_increment":null}))
        .unwrap();
        // 25 rupees of risk minus 40 rupees of brokerage leaves nothing.
        assert_eq!(
            size(&policy(), &input(&costs, "99"), None).unwrap(),
            Err(SizingRefusal::SizeBelowLot)
        );
        let tight = SizingInput {
            sleeve_cash: a("110"),
            ..input(&costs, "99")
        };
        let roomy = SizingPolicy {
            risk_per_trade_bps: 10_000,
            min_order_rupees: a("0"),
            ..policy()
        };
        assert_eq!(
            size(&roomy, &tight, None).unwrap(),
            Err(SizingRefusal::SleeveCashInsufficient)
        );
    }
}
