//! Shared dated simulation engine for paper and chronological replay.
//! No data acquisition, storage, credentials, models or orders.
use crate::finance::{Amount, FeeArithmetic, FeeSchedule, Money, Price, Side};
use crate::market::{MAX_FINANCIAL_AGE_MS, OptionCatalog, Segment};
use chrono::{DateTime, FixedOffset, NaiveDate, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Instrument {
    pub key: String,
    pub symbol: String,
    pub series: String,
    pub tick: Price,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Quote {
    pub bid: Price,
    pub ask: Price,
    pub bid_units: u64,
    pub ask_units: u64,
    pub source: i64,
    pub receipt: i64,
}
impl Quote {
    pub fn fresh(&self, now: i64, limit: i64) -> bool {
        now.checked_sub(self.source)
            .is_some_and(|age| (0..=limit).contains(&age))
            && now
                .checked_sub(self.receipt)
                .is_some_and(|age| (0..=limit).contains(&age))
    }
}
pub(crate) fn local(now: i64) -> Result<DateTime<FixedOffset>> {
    Ok(DateTime::<Utc>::from_timestamp_millis(now)
        .ok_or("simulation_clock_range")?
        .with_timezone(&FixedOffset::east_opt(19800).ok_or("simulation_offset")?))
}
/// Runtime policy only; never serialized into financial or scanner state.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ExecutionSettings {
    pub history_sample_interval_ms: i64,
    pub history_max_samples: usize,
    pub legacy_entry_cutoff_minute: u32,
    pub legacy_liquidation_minute: u32,
    pub legacy_stop_multiplier: Amount,
    pub legacy_target_multiplier: Amount,
    pub session_start_minute: u32,
    pub session_end_minute: u32,
    /// Absent keeps the historical execute/liquidate behaviour byte-for-byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_policy: Option<ExitPolicy>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitMode {
    #[default]
    Review,
    Execute,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltMode {
    #[default]
    Freeze,
    Liquidate,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExitPolicy {
    pub target: ExitMode,
    pub time: ExitMode,
    pub stop: ExitMode,
    pub stop_backstop_bps: Option<u32>,
    pub on_halt: HaltMode,
}
impl Default for ExitPolicy {
    fn default() -> Self {
        Self {
            target: ExitMode::Review,
            time: ExitMode::Review,
            stop: ExitMode::Execute,
            stop_backstop_bps: None,
            on_halt: HaltMode::Freeze,
        }
    }
}
impl ExitPolicy {
    pub fn validate(&self) -> Result<()> {
        if self
            .stop_backstop_bps
            .is_some_and(|n| !(1..=10_000).contains(&n))
        {
            return Err("invalid_exit_policy".into());
        }
        Ok(())
    }
}
/// Default NSE cash session close (15:30 local), in minutes after midnight.
pub const DEFAULT_SESSION_END_MINUTE: u32 = 930;
impl Default for ExecutionSettings {
    fn default() -> Self {
        Self {
            history_sample_interval_ms: 30_000,
            history_max_samples: 20,
            legacy_entry_cutoff_minute: 915,
            legacy_liquidation_minute: 915,
            legacy_stop_multiplier: a("0.98"),
            legacy_target_multiplier: a("1.02"),
            session_start_minute: 555,
            session_end_minute: DEFAULT_SESSION_END_MINUTE,
            exit_policy: None,
        }
    }
}
impl ExecutionSettings {
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
    pub fn validate(&self) -> Result<()> {
        if self.history_max_samples == 0
            || self.history_sample_interval_ms < 1
            || self.session_start_minute >= self.session_end_minute
            || self.session_end_minute > 1440
            || self.legacy_entry_cutoff_minute >= 1440
            || self.legacy_liquidation_minute >= 1440
            || self.legacy_stop_multiplier <= Amount::ZERO
            || self.legacy_target_multiplier <= Amount::ZERO
        {
            return Err("invalid_execution_settings".into());
        }
        if let Some(policy) = &self.exit_policy {
            policy.validate()?;
        }
        Ok(())
    }
}
/// Minutes elapsed since the configured session open. The open minute comes
/// from `ExecutionSettings.session_start_minute`; there is no compiled constant.
pub fn minutes_since_open(receipt: i64, session_start_minute: u32) -> Option<f64> {
    let t = local(receipt).ok()?;
    let elapsed = t.num_seconds_from_midnight() as f64 / 60.0
        + t.nanosecond() as f64 / 60_000_000_000.0
        - f64::from(session_start_minute);
    (elapsed >= 0.0).then_some(elapsed)
}

pub fn session_vwap(row: &serde_json::Value) -> Option<f64> {
    row["average_price"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.0)
}

pub fn book_level(row: &serde_json::Value, side: &str) -> (Option<f64>, Option<u64>) {
    let level = &row["depth"][side][0];
    match (level["price"].as_f64(), level["quantity"].as_u64()) {
        (Some(price), Some(qty)) if price.is_finite() && price > 0.0 && qty > 0 => {
            (Some(price), Some(qty))
        }
        _ => (None, None),
    }
}

pub(crate) fn minute(now: i64) -> Result<u32> {
    let t = local(now)?;
    Ok(t.hour() * 60 + t.minute())
}
pub fn number(v: &serde_json::Value) -> Result<Amount> {
    if !v.is_number() {
        return Err("number_missing".into());
    }
    Ok(Amount::parse(&v.to_string())?)
}
// Keep the ledger/schema identity: v0.4.2 changes only authorized risk thresholds.
pub const POLICY: &str = "wide_cash_momentum_v1_20261016";
pub fn a(s: &str) -> Amount {
    Amount::parse(s).expect("fixed decimal")
}
/// Display cost basis rounded upward to 1e-8 rupees; cash always uses exact fills.
fn combined_entry(old_units: u64, old: Price, units: u64, price: Price) -> Result<(u64, Price)> {
    let total = old_units
        .checked_add(units)
        .ok_or("position_units_overflow")?;
    let cost = old
        .amount()
        .checked_mul(Amount::from_unsigned(old_units))?
        .checked_add(price.amount().checked_mul(Amount::from_unsigned(units))?)?;
    let increment = a("0.00000001");
    let divisor = increment.checked_mul(Amount::from_unsigned(total))?;
    let steps = cost.checked_floor_ratio(divisor)?;
    let mut average = increment.checked_mul(Amount::from_integer(i64::try_from(steps)?))?;
    if average.checked_mul(Amount::from_unsigned(total))? < cost {
        average = average.checked_add(increment)?;
    }
    Ok((total, Price::try_from(average)?))
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ExitIntent {
    pub target: Option<Price>,
    pub stop: Option<Price>,
    /// Absolute epoch milliseconds.
    pub time: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub instrument: Instrument,
    pub units: u64,
    pub entry: Price,
    pub mark: Price,
    pub marked_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_intent: Option<ExitIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Fill {
    pub at: i64,
    pub instrument: Instrument,
    pub units: u64,
    pub buy: bool,
    pub price: Price,
    pub fee: Amount,
    /// Original executable ask (buy) or bid (sell); absent in legacy fills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_price: Option<Price>,
    /// Adverse signed bps, including tick rounding, rounded to eight decimal places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slippage_bps: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    /// Exact policy used for this fill; missing means the historical default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs: Option<FeeArithmetic>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Portfolio {
    #[serde(skip)]
    pub settings: ExecutionSettings,
    /// Runtime-only per-segment fee lookup. Absent means the legacy single
    /// cash `costs` policy; never serialized, so persisted cash books are
    /// byte-identical.
    #[serde(skip)]
    pub fee_schedule: Option<FeeSchedule>,
    /// Runtime-only bounded option catalog for admission, fills and expiry.
    /// Never serialized into the cash book.
    #[serde(skip)]
    pub options: Option<OptionCatalog>,
    pub policy: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs: Option<FeeArithmetic>,
    #[serde(default)]
    pub model_led: bool,
    pub cash: Amount,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_allocation: Option<Amount>,
    pub high_water: Amount,
    pub halted: bool,
    pub stopped: bool,
    pub closed: bool,
    pub positions: BTreeMap<String, Position>,
    pub fills: Vec<Fill>,
    pub health: String,
    /// Set by `protect` for target/time/stop review events. Not financial state.
    #[serde(skip)]
    pub exit_reviews: Vec<ExitReview>,
    #[serde(skip)]
    pub exit_review_at: BTreeMap<String, i64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExitReview {
    pub instrument_key: String,
    pub reason: String,
    pub at_ms: i64,
}
impl Portfolio {
    pub fn new() -> Self {
        Self::with_capital(a("5000"))
    }
    pub fn with_capital(capital: Amount) -> Self {
        Self {
            settings: ExecutionSettings::default(),
            fee_schedule: None,
            options: None,
            policy: POLICY.into(),
            costs: None,
            model_led: false,
            cash: capital,
            initial_allocation: Some(capital),
            high_water: capital,
            halted: false,
            stopped: false,
            closed: false,
            positions: BTreeMap::new(),
            fills: vec![],
            health: "waiting".into(),
            exit_reviews: vec![],
            exit_review_at: BTreeMap::new(),
        }
    }
    pub fn phase(&self) -> crate::book_phase::BookPhase {
        crate::book_phase::BookPhase::from_flags(self.stopped, self.halted, self.closed)
    }
    /// FIFO net realised P&L per fill. Buy fees belong to their lot; partial
    /// allocations round to 1e-8 INR and the final exit consumes the residual.
    /// Reporting errors/unknown opening lots produce None, never a trading error.
    pub fn realised_net_pnl(&self) -> Vec<Option<Amount>> {
        use std::collections::VecDeque;
        let mut lots = BTreeMap::<String, VecDeque<(u64, Amount)>>::new();
        let mut unknown = std::collections::BTreeSet::new();
        self.fills
            .iter()
            .map(|f| {
                if unknown.contains(&f.instrument.key) {
                    return None;
                }
                let result = (|| -> Result<Option<Amount>> {
                    let n = f
                        .price
                        .amount()
                        .checked_mul(Amount::from_unsigned(f.units))?;
                    let queue = lots.entry(f.instrument.key.clone()).or_default();
                    if f.buy {
                        queue.push_back((f.units, n.checked_add(f.fee)?));
                        return Ok(None);
                    }
                    let mut remaining = f.units;
                    let mut cost = Amount::ZERO;
                    while remaining > 0 {
                        let (units, basis) = queue.front_mut().ok_or("unknown_opening_lot")?;
                        let taken = remaining.min(*units);
                        let allocated = if taken == *units {
                            *basis
                        } else {
                            let basis: rust_decimal::Decimal = basis.to_string().parse()?;
                            let value = basis
                                .checked_mul(taken.into())
                                .and_then(|v| v.checked_div((*units).into()))
                                .ok_or("pnl_range")?
                                .round_dp(8);
                            Amount::parse(&value.to_string())?
                        };
                        cost = cost.checked_add(allocated)?;
                        *basis = basis.checked_sub(allocated)?;
                        *units -= taken;
                        remaining -= taken;
                        if *units == 0 {
                            queue.pop_front();
                        }
                    }
                    Ok(Some(n.checked_sub(f.fee)?.checked_sub(cost)?))
                })();
                match result {
                    Ok(value) => value,
                    Err(_) => {
                        unknown.insert(f.instrument.key.clone());
                        None
                    }
                }
            })
            .collect()
    }
    pub fn allocation(&self) -> Result<Amount> {
        if let Some(capital) = self.initial_allocation {
            if capital <= Amount::ZERO {
                return Err("invalid_initial_allocation".into());
            }
            return Ok(capital);
        }
        // Legacy books did not store allocation. Reverse the recorded cash movements.
        self.fills.iter().try_fold(self.cash, |cash, f| {
            let n = f
                .price
                .amount()
                .checked_mul(Amount::from_unsigned(f.units))?;
            Ok(if f.buy {
                cash.checked_add(n.checked_add(f.fee)?)?
            } else {
                cash.checked_sub(n.checked_sub(f.fee)?)?
            })
        })
    }
    /// Exact fee policy for one instrument key. A runtime per-segment schedule
    /// selects `paper_costs` for `NSE_EQ` and the configured `NSE_FO` entry for
    /// options; absent a schedule the legacy single cash policy is used.
    pub fn fee_policy(&self, key: &str) -> Result<FeeArithmetic> {
        if let Some(schedule) = &self.fee_schedule {
            return match Segment::from_key(key) {
                Some(Segment::NseEq) => Ok(schedule.cash().clone()),
                Some(segment) => schedule
                    .for_segment(segment)
                    .cloned()
                    .ok_or_else(|| "paper_fee_schedule_missing".into()),
                None => Err("paper_fee_segment_unknown".into()),
            };
        }
        self.costs
            .clone()
            .ok_or_else(|| "paper_costs_required".into())
    }

    /// Runtime option contract for a key, when this book can trade options.
    pub fn option_contract(&self, key: &str) -> Option<&crate::market::OptionContract> {
        self.options
            .as_ref()
            .and_then(|catalog| catalog.contract(key))
    }

    pub fn reconcile(&self) -> Result<()> {
        if let Some(costs) = &self.costs {
            costs.validate()?;
        }
        if let Some(schedule) = &self.fee_schedule {
            schedule.validate()?;
        }
        if self.policy != POLICY {
            return Err("portfolio_policy_or_bounds".into());
        }
        let mut cash = self.allocation()?;
        let mut held = BTreeMap::<String, (u64, Price, Instrument)>::new();
        for f in &self.fills {
            if f.units == 0 {
                return Err("zero_fill".into());
            }
            let n = f
                .price
                .amount()
                .checked_mul(Amount::from_unsigned(f.units))?;
            if let Some(policy) = &f.costs {
                policy.validate()?;
                let expected = policy
                    .charge(
                        if f.buy { Side::Buy } else { Side::Sell },
                        Money::try_from(n)?,
                    )?
                    .amount();
                if f.fee != expected {
                    return Err("fee_reconciliation".into());
                }
            } else if f.fee < Amount::ZERO {
                return Err("fee_reconciliation".into());
            }
            if f.buy {
                let (units, entry) =
                    if let Some((units, entry, instrument)) = held.get(&f.instrument.key) {
                        if instrument != &f.instrument {
                            return Err("buy_identity_changed".into());
                        }
                        combined_entry(*units, *entry, f.units, f.price)?
                    } else {
                        (f.units, f.price)
                    };
                held.insert(
                    f.instrument.key.clone(),
                    (units, entry, f.instrument.clone()),
                );
                cash = cash.checked_sub(n.checked_add(f.fee)?)?;
                if cash < Amount::ZERO {
                    return Err("cash_overdrawn".into());
                }
            } else {
                let old = held
                    .remove(&f.instrument.key)
                    .ok_or("sell_without_position")?;
                if old.0 < f.units || old.2 != f.instrument {
                    return Err("sell_identity_or_units".into());
                }
                if old.0 > f.units {
                    held.insert(f.instrument.key.clone(), (old.0 - f.units, old.1, old.2));
                }
                cash = cash.checked_add(n.checked_sub(f.fee)?)?;
            }
        }
        if cash != self.cash || cash < Amount::ZERO || held.len() != self.positions.len() {
            return Err("cash_reconciliation".into());
        }
        for (key, p) in &self.positions {
            if held.get(key) != Some(&(p.units, p.entry, p.instrument.clone())) {
                return Err("position_reconciliation".into());
            }
        }
        Ok(())
    }
    pub fn equity(&self) -> Result<Amount> {
        self.positions.values().try_fold(self.cash, |v, p| {
            Ok(v.checked_add(
                p.mark
                    .amount()
                    .checked_mul(Amount::from_unsigned(p.units))?,
            )?)
        })
    }
    pub fn exposure(&self) -> Result<Amount> {
        self.positions.values().try_fold(Amount::ZERO, |v, p| {
            Ok(v.checked_add(
                p.entry
                    .amount()
                    .checked_mul(Amount::from_unsigned(p.units))?,
            )?)
        })
    }
    pub fn execution_with_costs(
        raw: Price,
        tick: Price,
        buy: bool,
        costs: &FeeArithmetic,
    ) -> Result<Price> {
        costs.validate()?;
        let rate = costs.slippage_bps.amount().checked_mul(a("0.0001"))?;
        let factor = if buy {
            a("1").checked_add(rate)?
        } else {
            a("1").checked_sub(rate)?
        };
        let value = raw.amount().checked_mul(factor)?;
        let count = value.checked_floor_ratio(tick.amount())?;
        let mut rounded = tick
            .amount()
            .checked_mul(Amount::from_integer(i64::try_from(count)?))?;
        if buy && rounded < value {
            rounded = rounded.checked_add(tick.amount())?;
        }
        Ok(Price::try_from(rounded)?)
    }
    fn fill(&mut self, now: i64, i: &Instrument, q: &Quote, buy: bool, units: u64) -> Result<()> {
        let expected_price = if buy { q.ask } else { q.bid };
        let policy = self.fee_policy(&i.key)?;
        let price = Self::execution_with_costs(expected_price, i.tick, buy, &policy)?;
        let slippage_bps = (|| -> Result<Amount> {
            // Reporting only: never feed rounded attribution back into cash or fills.
            let difference = if buy {
                price.amount().checked_sub(expected_price.amount())?
            } else {
                expected_price.amount().checked_sub(price.amount())?
            };
            let numerator: rust_decimal::Decimal = difference.to_string().parse()?;
            let denominator: rust_decimal::Decimal = expected_price.amount().to_string().parse()?;
            let slippage = numerator
                .checked_mul(rust_decimal::Decimal::from(10000))
                .and_then(|v| v.checked_div(denominator))
                .ok_or("slippage_attribution_range")?
                .round_dp(8);
            Ok(Amount::parse(&slippage.to_string())?)
        })()
        .ok();
        let n = price.amount().checked_mul(Amount::from_unsigned(units))?;
        let fee = policy
            .charge(
                if buy { Side::Buy } else { Side::Sell },
                Money::try_from(n)?,
            )?
            .amount();
        self.cash = if buy {
            self.cash.checked_sub(n.checked_add(fee)?)?
        } else {
            self.cash.checked_add(n.checked_sub(fee)?)?
        };
        if buy {
            let (units, price) = if let Some(p) = self.positions.get(&i.key) {
                if p.instrument != *i {
                    return Err("buy_identity_changed".into());
                }
                combined_entry(p.units, p.entry, units, price)?
            } else {
                (units, price)
            };
            self.positions.insert(
                i.key.clone(),
                Position {
                    instrument: i.clone(),
                    units,
                    entry: price,
                    mark: q.bid,
                    marked_at: q.source.min(q.receipt),
                    exit_intent: None,
                    decision_id: None,
                },
            );
        } else {
            if let Some(p) = self.positions.get_mut(&i.key) {
                p.units -= units;
                if p.units == 0 {
                    self.positions.remove(&i.key);
                }
            }
        }
        self.fills.push(Fill {
            at: now,
            instrument: i.clone(),
            units,
            buy,
            price,
            fee,
            expected_price: Some(expected_price),
            slippage_bps,
            // Record the exact policy charged (cash or per-segment F&O), so
            // reconciliation re-derives the same fee for option fills too.
            costs: Some(policy),
            decision_id: None,
        });
        Ok(())
    }
    pub fn protect(&mut self, now: i64, quotes: &BTreeMap<String, Quote>) -> Result<()> {
        if self.stopped || self.closed {
            return Ok(());
        }
        self.exit_reviews.clear();
        for (key, p) in &mut self.positions {
            if let Some(q) = quotes
                .get(key)
                .filter(|q| q.fresh(now, MAX_FINANCIAL_AGE_MS))
            {
                p.mark = q.bid;
                p.marked_at = q.source.min(q.receipt);
            }
        }
        let equity = self.equity()?;
        self.high_water = self.high_water.max(equity);
        let positions = self.positions.clone();
        let policy = self.settings.exit_policy.clone();
        for (key, p) in positions {
            let Some(q) = quotes
                .get(&key)
                .filter(|q| q.fresh(now, MAX_FINANCIAL_AGE_MS))
            else {
                self.health = "held_quote_stale_entries_blocked".into();
                continue;
            };
            let target_hit = p
                .exit_intent
                .as_ref()
                .and_then(|intent| intent.target)
                .is_some_and(|target| q.bid >= target);
            let stop_hit = p
                .exit_intent
                .as_ref()
                .and_then(|intent| intent.stop)
                .is_some_and(|stop| q.bid <= stop);
            let time_hit = p
                .exit_intent
                .as_ref()
                .and_then(|intent| intent.time)
                .is_some_and(|time| now >= time);
            let backstop_hit = policy.as_ref().and_then(|policy| {
                let applies = policy.stop == ExitMode::Review
                    || p.exit_intent
                        .as_ref()
                        .and_then(|intent| intent.stop)
                        .is_none();
                let bps = policy.stop_backstop_bps.filter(|_| applies)?;
                let factor = Amount::from_integer(10_000_i64.checked_sub(i64::from(bps))?)
                    .checked_div(Amount::from_integer(10_000))
                    .ok()?;
                let floor = p.entry.amount().checked_mul(factor).ok()?;
                Some(q.bid.amount() <= floor)
            });
            let execute = if let Some(policy) = &policy {
                if self.halted {
                    match policy.on_halt {
                        HaltMode::Liquidate => true,
                        HaltMode::Freeze => {
                            (policy.stop == ExitMode::Execute && stop_hit)
                                || backstop_hit.unwrap_or(false)
                        }
                    }
                } else {
                    (policy.stop == ExitMode::Execute && stop_hit)
                        || backstop_hit.unwrap_or(false)
                        || (policy.target == ExitMode::Execute && target_hit)
                        || (policy.time == ExitMode::Execute && time_hit)
                }
            } else {
                (self.halted
                    || if self.model_led {
                        target_hit || stop_hit || time_hit
                    } else {
                        minute(now)? >= self.settings.legacy_liquidation_minute
                            || q.bid.amount()
                                <= p.entry
                                    .amount()
                                    .checked_mul(self.settings.legacy_stop_multiplier)?
                            || q.bid.amount()
                                >= p.entry
                                    .amount()
                                    .checked_mul(self.settings.legacy_target_multiplier)?
                    })
                    && q.bid_units >= p.units
            };
            if execute && q.bid_units >= p.units {
                self.fill(now, &p.instrument, q, false, p.units)?;
                self.fills.last_mut().expect("fill").decision_id = p.decision_id;
                continue;
            }
            if let Some(policy) = &policy
                && !self.halted
            {
                let mut reasons = Vec::new();
                if policy.target == ExitMode::Review && target_hit {
                    reasons.push("target_reached");
                }
                if policy.time == ExitMode::Review && time_hit {
                    reasons.push("time_reached");
                }
                if policy.stop == ExitMode::Review && stop_hit {
                    reasons.push("stop_reached");
                }
                for reason in reasons {
                    let stamp = format!("{key}:{reason}");
                    if now.saturating_sub(
                        self.exit_review_at.get(&stamp).copied().unwrap_or(i64::MIN),
                    ) >= 60_000
                    {
                        self.exit_reviews.push(ExitReview {
                            instrument_key: key.clone(),
                            reason: reason.into(),
                            at_ms: now,
                        });
                        self.exit_review_at.insert(stamp, now);
                    }
                }
            }
        }
        Ok(())
    }
    /// Update a held position's exit intent without trading.
    pub fn revise_exit(
        &mut self,
        i: &Instrument,
        intent: &ExitIntent,
        decision: &str,
    ) -> Result<bool> {
        if self.stopped || self.closed || self.halted {
            return Ok(false);
        }
        let Some(position) = self.positions.get_mut(&i.key) else {
            return Ok(false);
        };
        if position.instrument != *i
            || intent
                .target
                .zip(intent.stop)
                .is_some_and(|(target, stop)| target <= stop)
            || intent.time.is_some_and(|time| time <= 0)
            || [intent.target, intent.stop]
                .into_iter()
                .flatten()
                .any(|price| {
                    !price
                        .amount()
                        .is_multiple_of(i.tick.amount())
                        .unwrap_or(false)
                })
        {
            return Ok(false);
        }
        position.exit_intent = Some(intent.clone());
        position.decision_id = Some(decision.into());
        Ok(true)
    }
    /// Deterministic execution gate; caller commits under its ledger lock.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_plan(
        &mut self,
        now: i64,
        i: &Instrument,
        q: &Quote,
        buy: bool,
        units: u64,
        max_price: Price,
        exit: ExitIntent,
        decision: &str,
    ) -> Result<bool> {
        if self.stopped
            || self.closed
            || (buy && (self.halted || exit.time.is_some_and(|time| time <= now)))
            || units == 0
            || !q.fresh(now, MAX_FINANCIAL_AGE_MS)
            || q.ask < q.bid
            || !(self.settings.session_start_minute..self.settings.session_end_minute)
                .contains(&minute(now)?)
        {
            return Ok(false);
        }
        // Option execution gate: a bounded catalog contract must exist, units
        // must be a lot multiple, and no order may hold past expiry. Unknown
        // NSE_FO keys are never treated as cash.
        if Segment::from_key(&i.key) == Some(Segment::NseFo) {
            let Some(catalog) = self.options.as_ref() else {
                return Ok(false);
            };
            let Some(contract) = catalog.contract(&i.key) else {
                return Ok(false);
            };
            if !units.is_multiple_of(contract.lot_size)
                || (buy && contract.expiry <= catalog.session)
                || (!buy && contract.expiry < catalog.session)
            {
                return Ok(false);
            }
        }
        let policy = self.fee_policy(&i.key)?;
        let price =
            Self::execution_with_costs(if buy { q.ask } else { q.bid }, i.tick, buy, &policy)?;
        if buy {
            if price > max_price
                || self
                    .positions
                    .get(&i.key)
                    .is_some_and(|p| p.instrument != *i)
                || units > q.ask_units
                || units > self.size_with_costs(i, q, self.cash)?
                || self
                    .positions
                    .values()
                    .any(|p| now < p.marked_at || now - p.marked_at > MAX_FINANCIAL_AGE_MS)
            {
                return Ok(false);
            }
        } else if units > q.bid_units
            || !self
                .positions
                .get(&i.key)
                .is_some_and(|p| p.instrument == *i && units <= p.units)
        {
            return Ok(false);
        }
        if !buy {
            let notional = price.amount().checked_mul(Amount::from_unsigned(units))?;
            let charge = policy
                .charge(Side::Sell, Money::try_from(notional)?)?
                .amount();
            if self.cash.checked_add(notional)?.checked_sub(charge)? < Amount::ZERO {
                return Ok(false);
            }
        }
        let mut next = self.clone();
        next.fill(now, i, q, buy, units)?;
        next.model_led = true;
        next.fills.last_mut().expect("fill").decision_id = Some(decision.into());
        if buy && let Some(p) = next.positions.get_mut(&i.key) {
            if exit != ExitIntent::default() || p.exit_intent.is_none() {
                p.exit_intent = Some(exit);
            }
            p.decision_id = Some(decision.into());
        }
        next.reconcile()?;
        *self = next;
        Ok(true)
    }
    fn size_with_costs(&self, i: &Instrument, q: &Quote, budget: Amount) -> Result<u64> {
        let policy = self.fee_policy(&i.key)?;
        let price = Self::execution_with_costs(q.ask, i.tick, true, &policy)?;
        Ok(policy.affordable_cash_lots(price, budget, i64::try_from(q.ask_units)?, false, 0)?)
    }
    pub fn enter(&mut self, now: i64, i: &Instrument, q: &Quote) -> Result<bool> {
        if self.halted
            || self.stopped
            || self.closed
            || minute(now)? >= self.settings.legacy_entry_cutoff_minute
            || self.positions.contains_key(&i.key)
            || q.ask < q.bid
            || !q.fresh(now, MAX_FINANCIAL_AGE_MS)
            || self
                .positions
                .values()
                .any(|p| now - p.marked_at > MAX_FINANCIAL_AGE_MS || now < p.marked_at)
        {
            return Ok(false);
        }
        let budget = self.cash;
        let units = self.size_with_costs(i, q, budget)?;
        if units == 0 {
            return Ok(false);
        }
        self.fill(now, i, q, true, units)?;
        Ok(true)
    }
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Sample {
    /// Average volume from validated prepared daily sessions only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_daily_volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_daily_volume_sessions: Option<u32>,
    /// Optional REST OHLC references for shadow regime diagnostics only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regime_references: Option<(i64, Price, Price)>,
    /// Screening-only connection activity; never an executable quote receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_activity: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_kind: Option<String>,
    /// Latest REST bid/ask/depth for a shortlisted key; never a screening price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<Quote>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_for_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_fields: Option<MarketFields>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_fields_receipt: Option<i64>,
    /// Restored screening metadata; original receipt remains authoritative.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub metadata_preloaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    pub history: Vec<Price>,
    pub last: i64,
    pub seen: i64,
    pub reason: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Scanner {
    #[serde(skip)]
    pub settings: ExecutionSettings,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screens: Option<ScreenPolicy>,
    pub samples: BTreeMap<String, Sample>,
    pub cursor: usize,
    pub ranking: Vec<(Amount, String, i64)>,
}
impl Scanner {
    pub fn observe(
        &mut self,
        now: i64,
        i: &Instrument,
        q: &Quote,
        row: &serde_json::Value,
    ) -> Result<()> {
        let sample = self.samples.entry(i.key.clone()).or_default();
        sample.seen = q.receipt;
        sample.reason = "warming".into();
        if !q.fresh(now, MAX_FINANCIAL_AGE_MS) {
            sample.reason = "stale".into();
            return Ok(());
        }
        let fields = MarketFields {
            session_vwap: session_vwap(row),
            best_bid: book_level(row, "buy").0,
            best_bid_qty: book_level(row, "buy").1,
            best_ask: book_level(row, "sell").0,
            best_ask_qty: book_level(row, "sell").1,
            total_buy_quantity: row["total_buy_quantity"].as_u64(),
            total_sell_quantity: row["total_sell_quantity"].as_u64(),
            lower_circuit_limit: number(&row["lower_circuit_limit"])?,
            upper_circuit_limit: number(&row["upper_circuit_limit"])?,
            volume: row["volume"].as_u64().ok_or("volume_missing")?,
        };
        self.observe_fields(now, i, q, &fields)
    }
    pub fn observe_fields(
        &mut self,
        now: i64,
        i: &Instrument,
        q: &Quote,
        fields: &MarketFields,
    ) -> Result<()> {
        let s = self.samples.entry(i.key.clone()).or_default();
        s.scan_activity = None;
        s.seen = q.receipt;
        s.price_kind = Some("bid".into());
        s.current = Some(q.bid);
        s.source = Some(q.source);
        s.fresh_for_ms = Some(MAX_FINANCIAL_AGE_MS);
        s.metadata_preloaded = false;
        s.market_fields = Some(fields.clone());
        s.market_fields_receipt = Some(q.receipt);
        s.reason = "warming".into();
        if !q.fresh(now, MAX_FINANCIAL_AGE_MS) {
            s.reason = "stale".into();
            return Ok(());
        }
        s.reason = self
            .screens
            .as_ref()
            .unwrap_or(&ScreenPolicy::default())
            .reason(q.bid.amount(), fields)?
            .into();
        self.ranking.retain(|(_, key, _)| key != &i.key);
        if now - s.last < self.settings.history_sample_interval_ms {
            return Ok(());
        }
        s.last = now;
        s.history.push(q.bid);
        if s.history.len() > self.settings.history_max_samples {
            s.history.remove(0);
        }
        Ok(())
    }
}

impl Default for Portfolio {
    fn default() -> Self {
        Self::new()
    }
}
impl Portfolio {
    /// Identical clock/data boundary for paper and historical adapters.
    pub fn apply_at(
        &mut self,
        session: NaiveDate,
        now: i64,
        quotes: &BTreeMap<String, Quote>,
        entries: &[Instrument],
    ) -> Result<()> {
        // Commit the in-memory result only after reconciliation; adapters own durability.
        let mut next = self.clone();
        if local(now)?.date_naive() == session
            && (self.settings.session_start_minute..self.settings.session_end_minute)
                .contains(&minute(now)?)
        {
            next.health = "scanning".into();
            next.protect(now, quotes)?;
            for instrument in entries {
                if let Some(quote) = quotes.get(&instrument.key) {
                    next.enter(now, instrument, quote)?;
                }
            }
        }
        next.reconcile()?;
        *self = next;
        Ok(())
    }
}

impl Scanner {
    pub fn ranked(
        &mut self,
        now: i64,
        portfolio: &Portfolio,
        universe: &[Instrument],
    ) -> Vec<Instrument> {
        self.ranking.retain(|(_, _, at)| {
            now.checked_sub(*at)
                .is_some_and(|age| (0..=60000).contains(&age))
        });
        self.ranking
            .sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        self.ranking
            .iter()
            .filter(|(_, key, _)| !portfolio.positions.contains_key(key))
            .take(5)
            .filter_map(|(_, key, _)| universe.iter().find(|i| &i.key == key).cloned())
            .collect()
    }
    pub fn market_allows(
        now: i64,
        instrument: &Instrument,
        quote: &Quote,
        row: &serde_json::Value,
    ) -> bool {
        let mut check = Self::default();
        check.observe(now, instrument, quote, row).is_ok()
            && check
                .samples
                .get(&instrument.key)
                .is_some_and(|sample| sample.reason == "eligible_no_signal")
    }
}

/// Prospect screens only. Held-position protection never uses these gates.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScreenPolicy {
    pub min_price: Amount,
    pub min_traded_value: Amount,
    pub min_circuit_distance_bps: Amount,
}
impl Default for ScreenPolicy {
    fn default() -> Self {
        Self {
            min_price: Amount::ZERO,
            min_traded_value: Amount::ZERO,
            min_circuit_distance_bps: Amount::ZERO,
        }
    }
}
impl ScreenPolicy {
    pub fn recommended() -> Self {
        Self {
            min_price: a("10"),
            min_traded_value: a("1000000"),
            min_circuit_distance_bps: a("100"),
        }
    }
    pub fn validate(&self) -> Result<()> {
        if self.min_price < Amount::ZERO
            || self.min_traded_value < Amount::ZERO
            || self.min_circuit_distance_bps < Amount::ZERO
            || self.min_circuit_distance_bps >= a("10000")
        {
            return Err("invalid_screen_policy".into());
        }
        Ok(())
    }
    pub fn reason(&self, price: Amount, fields: &MarketFields) -> Result<&'static str> {
        self.validate()?;
        if price < self.min_price {
            return Ok("below_min_price");
        }
        if fields.volume == 0
            || price.checked_mul(Amount::from_unsigned(fields.volume))? < self.min_traded_value
        {
            return Ok("below_min_traded_value");
        }
        if fields.lower_circuit_limit <= Amount::ZERO
            || fields.upper_circuit_limit <= fields.lower_circuit_limit
        {
            return Ok("invalid_circuit_limits");
        }
        let distance = price.checked_mul(self.min_circuit_distance_bps)?;
        if price <= fields.lower_circuit_limit
            || price
                .checked_sub(fields.lower_circuit_limit)?
                .checked_mul(a("10000"))?
                < distance
        {
            return Ok("near_lower_circuit");
        }
        if price >= fields.upper_circuit_limit
            || fields
                .upper_circuit_limit
                .checked_sub(price)?
                .checked_mul(a("10000"))?
                < distance
        {
            return Ok("near_upper_circuit");
        }
        Ok("eligible_no_signal")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_vwap: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_bid: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_bid_qty: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_ask: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_ask_qty: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_buy_quantity: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_sell_quantity: Option<u64>,
    pub lower_circuit_limit: Amount,
    pub upper_circuit_limit: Amount,
    pub volume: u64,
}
impl Scanner {
    pub fn market_allows_with_policy(
        now: i64,
        instrument: &Instrument,
        quote: &Quote,
        row: &serde_json::Value,
        policy: &ScreenPolicy,
    ) -> bool {
        let mut check = Self {
            screens: Some(policy.clone()),
            ..Self::default()
        };
        check.observe(now, instrument, quote, row).is_ok()
            && check
                .samples
                .get(&instrument.key)
                .is_some_and(|sample| sample.reason == "eligible_no_signal")
    }
    /// Record a failed observation without moving the last successful receipt.
    #[allow(clippy::too_many_arguments)] // Distinct source/receipt clocks and screening-only fields; never executable depth.
    pub fn observe_price(
        &mut self,
        now: i64,
        key: &str,
        price: Price,
        source: i64,
        receipt: i64,
        stale_ms: i64,
        fields: Option<&MarketFields>,
    ) -> Result<()> {
        let s = self.samples.entry(key.to_owned()).or_default();
        s.scan_activity = None;
        s.seen = receipt;
        s.price_kind = Some("last_price".into());
        s.current = Some(price);
        s.source = Some(source);
        s.fresh_for_ms = Some(stale_ms);
        s.reason = if ![source, receipt].into_iter().all(|at| {
            now.checked_sub(at)
                .is_some_and(|age| (0..=stale_ms).contains(&age))
        }) {
            "stale"
        } else if let Some(fields) = fields {
            self.screens
                .as_ref()
                .unwrap_or(&ScreenPolicy::default())
                .reason(price.amount(), fields)?
        } else {
            "missing_market_fields"
        }
        .into();
        if s.reason == "stale" {
            return Ok(());
        }
        if now - s.last >= self.settings.history_sample_interval_ms || s.history.is_empty() {
            s.last = now;
            s.history.push(price);
            if s.history.len() > self.settings.history_max_samples {
                s.history.remove(0);
            }
        }
        Ok(())
    }
    pub fn missing(&mut self, key: &str, reason: &str) {
        let sample = self.samples.entry(key.to_owned()).or_default();
        sample.reason = reason.into();
        self.ranking.retain(|(_, k, _)| k != key);
    }
    pub fn market_allows_fields(
        now: i64,
        instrument: &Instrument,
        quote: &Quote,
        fields: &MarketFields,
    ) -> bool {
        let mut check = Self::default();
        check.observe_fields(now, instrument, quote, fields).is_ok()
            && check
                .samples
                .get(&instrument.key)
                .is_some_and(|s| s.reason == "eligible_no_signal")
    }
}

#[cfg(test)]
mod execution_settings_tests {
    use super::*;
    #[test]
    fn defaults_and_state_bytes_preserve_original_policy() {
        let mut settings: ExecutionSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, ExecutionSettings::default());
        assert_eq!(
            (
                settings.history_sample_interval_ms,
                settings.history_max_samples
            ),
            (30_000, 20)
        );
        assert_eq!(
            (
                settings.legacy_entry_cutoff_minute,
                settings.legacy_liquidation_minute
            ),
            (915, 915)
        );
        assert_eq!(
            (
                settings.legacy_stop_multiplier,
                settings.legacy_target_multiplier
            ),
            (a("0.98"), a("1.02"))
        );
        assert_eq!(
            (settings.session_start_minute, settings.session_end_minute),
            (555, 930)
        );
        // Minutes since open follow the configured session start, not a
        // leftover compiled 555 constant.
        let open = 1_792_122_300_000_i64; // 2026-10-16T09:15:00+05:30
        assert_eq!(minutes_since_open(open, 555), Some(0.0));
        assert_eq!(minutes_since_open(open + 60_000, 555), Some(1.0));
        assert_eq!(minutes_since_open(open, 600), None);
        assert_eq!(minutes_since_open(open + 45 * 60_000, 600), Some(0.0));
        assert_eq!(minutes_since_open(open - 1, 555), None);
        settings.validate().unwrap();
        settings.session_start_minute = 930;
        assert!(settings.validate().is_err());
        settings.session_start_minute = 931;
        assert!(settings.validate().is_err());
        settings.session_start_minute = 555;
        settings.history_max_samples = 0;
        assert!(settings.validate().is_err());
        settings.history_max_samples = 1;
        settings.validate().unwrap();
        let mut portfolio = Portfolio::new();
        let bytes = serde_json::to_vec(&portfolio).unwrap();
        portfolio.settings = settings.clone();
        assert_eq!(serde_json::to_vec(&portfolio).unwrap(), bytes);
        let mut scanner = Scanner::default();
        let bytes = serde_json::to_vec(&scanner).unwrap();
        scanner.settings = settings;
        assert_eq!(serde_json::to_vec(&scanner).unwrap(), bytes);

        // Model-owned exits: a review target does not sell; a stop still does;
        // a halt freezes instead of liquidating. Absent policy keeps the old sell.
        let now = 1_700_000_000_000_i64;
        let instrument = Instrument {
            key: "NSE_EQ|A".into(),
            symbol: "A".into(),
            series: "EQ".into(),
            tick: Price::try_from(a("0.05")).unwrap(),
        };
        let quote = |bid: &str| Quote {
            bid: Price::try_from(a(bid)).unwrap(),
            ask: Price::try_from(a(bid)).unwrap(),
            bid_units: 10,
            ask_units: 10,
            source: now,
            receipt: now,
        };
        let mut held = Portfolio::new();
        held.model_led = true;
        held.costs = Some(
            serde_json::from_str(include_str!(
                "../../../fixtures/test-legacy-paper-costs.json"
            ))
            .unwrap(),
        );
        held.positions.insert(
            instrument.key.clone(),
            Position {
                instrument: instrument.clone(),
                units: 1,
                entry: Price::try_from(a("100")).unwrap(),
                mark: Price::try_from(a("100")).unwrap(),
                marked_at: now,
                exit_intent: Some(ExitIntent {
                    target: Some(Price::try_from(a("110")).unwrap()),
                    stop: Some(Price::try_from(a("90")).unwrap()),
                    time: None,
                }),
                decision_id: Some("d1".into()),
            },
        );
        let quotes = BTreeMap::from([(instrument.key.clone(), quote("120"))]);
        held.settings.exit_policy = Some(ExitPolicy::default());
        held.protect(now, &quotes).unwrap();
        assert_eq!(held.positions.len(), 1, "review target must not sell");
        assert_eq!(held.exit_reviews[0].reason, "target_reached");
        held.halted = true;
        held.protect(now, &quotes).unwrap();
        assert_eq!(held.positions.len(), 1, "freeze keeps the target unsold");
        held.halted = false;
        held.settings.exit_policy = None;
        held.protect(now, &quotes).unwrap();
        assert!(held.positions.is_empty(), "absent policy still executes");
        let revised = {
            let mut book = Portfolio::new();
            book.model_led = true;
            book.positions.insert(
                instrument.key.clone(),
                Position {
                    instrument: instrument.clone(),
                    units: 1,
                    entry: Price::try_from(a("100")).unwrap(),
                    mark: Price::try_from(a("100")).unwrap(),
                    marked_at: now,
                    exit_intent: Some(ExitIntent {
                        target: Some(Price::try_from(a("110")).unwrap()),
                        stop: Some(Price::try_from(a("90")).unwrap()),
                        time: None,
                    }),
                    decision_id: None,
                },
            );
            assert!(
                book.revise_exit(
                    &instrument,
                    &ExitIntent {
                        target: Some(Price::try_from(a("130")).unwrap()),
                        stop: Some(Price::try_from(a("95")).unwrap()),
                        time: None,
                    },
                    "d2",
                )
                .unwrap()
            );
            assert_eq!(book.fills.len(), 0);
            book.positions[&instrument.key]
                .exit_intent
                .as_ref()
                .unwrap()
                .target
        };
        assert_eq!(revised, Some(Price::try_from(a("130")).unwrap()));
    }
}
