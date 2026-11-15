//! Existing opt-in paper cash-profile budgets and sizing, without activation.
//!
//! Pure cash preflight is not activation: authoritative archive rederivation,
//! mandates, fresh independent protection, account identity and atomic portfolio
//! reservations must still be checked by a runtime.

use crate::{
    candidate::Candidate,
    finance::{Amount, FeeArithmetic, FinancialError, Money, Price, Side},
    market::{Calendar, NiftyOptionMandate, OptionCatalog, OptionOrder, PaperMandate, Snapshot},
    time::{ExchangeDate, Timestamp},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
pub enum PaperCashProfile {
    #[default]
    #[serde(rename = "cash_5000_paper_v1")]
    Cash5000,
    #[serde(rename = "cash_50000_paper_v1")]
    Cash50000,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PaperCashPolicy {
    #[serde(default)]
    profile: PaperCashProfile,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Portfolio {
    pub equity: Money,
    pub session_start: Price,
    pub high_water: Price,
    pub cash: Money,
    #[serde(default)]
    pub reserved_risk: Money,
    #[serde(default)]
    pub open_positions: u64,
    #[serde(default)]
    pub session_entries: u64,
    #[serde(default = "default_true")]
    pub reconciled: bool,
    #[serde(default)]
    pub halted: bool,
}

fn default_true() -> bool {
    true
}

// Only the immutable policy produces these limits; caller JSON cannot widen them.
#[derive(Debug, Clone, Serialize)]
pub struct CashProfileLimits {
    per_entry: Price,
    aggregate: Price,
    daily_loss: Price,
    drawdown: Price,
    max_positions: u64,
    max_entries: u64,
    max_spread_fraction: Price,
    max_quote_age: u8,
}

impl PaperCashPolicy {
    pub fn new(profile: PaperCashProfile) -> Self {
        Self { profile }
    }
    pub fn profile(self) -> PaperCashProfile {
        self.profile
    }

    pub fn capital(self) -> Price {
        constant_price(match self.profile {
            PaperCashProfile::Cash5000 => "5000",
            PaperCashProfile::Cash50000 => "50000",
        })
    }

    pub fn limits(self) -> CashProfileLimits {
        let funded = self.profile == PaperCashProfile::Cash50000;
        CashProfileLimits {
            per_entry: constant_price(if funded { "0.1" } else { "0.4" }),
            aggregate: constant_price(if funded { "0.1" } else { "0.4" }),
            daily_loss: constant_price("0.01"),
            drawdown: constant_price(if funded { "0.02" } else { "0.05" }),
            max_positions: 1,
            max_entries: if funded { 10 } else { 3 },
            max_spread_fraction: constant_price("0.005"),
            max_quote_age: 2,
        }
    }

    pub fn cash_budget(self, portfolio: &Portfolio) -> Result<Amount, FinancialError> {
        let cap = Amount::from_integer(if self.profile == PaperCashProfile::Cash50000 {
            5000
        } else {
            2000
        });
        Ok(cap
            .min(cap.checked_sub(portfolio.reserved_risk.amount())?)
            .min(portfolio.cash.amount()))
    }

    pub fn planned_budget(self, portfolio: &Portfolio) -> Result<Money, FinancialError> {
        let loss = portfolio
            .session_start
            .amount()
            .checked_sub(portfolio.equity.amount())?
            .max(Amount::ZERO);
        let (entry, daily) = if self.profile == PaperCashProfile::Cash50000 {
            (100, 500)
        } else {
            (25, 50)
        };
        Money::try_from(
            Amount::from_integer(entry)
                .min(Amount::from_integer(daily).checked_sub(loss)?)
                .max(Amount::ZERO),
        )
    }
}

fn constant_price(value: &str) -> Price {
    Price::parse(value).expect("fixed reviewed paper-profile constant")
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CashSizing {
    pub lots: u64,
    pub reservation: Money,
}

/// Preserve the Python fixed 8% baseline planned stop plus round-trip costs.
/// Planned stop loss is not a guaranteed maximum loss or a certified strategy.
pub fn paper_cash_lots(
    price: Price,
    fees: &FeeArithmetic,
    cash_budget: Amount,
    planned_budget: Money,
    maximum: i64,
) -> Result<CashSizing, FinancialError> {
    let mut upper = fees.affordable_cash_lots(price, cash_budget, maximum, true, 2)?;
    let mut lower = 0;
    let planned_stop_fraction = constant_price("0.08").amount();
    while lower < upper {
        let middle = lower + (upper - lower).div_ceil(2);
        let notional = price.amount().checked_mul(Amount::from_unsigned(middle))?;
        let requirement = fees.cash_requirement(Money::try_from(notional)?, true, 2)?;
        let costs = requirement.amount().checked_sub(notional)?;
        if notional
            .checked_mul(planned_stop_fraction)?
            .checked_add(costs)?
            <= planned_budget.amount()
        {
            lower = middle;
        } else {
            upper = middle - 1;
        }
    }
    let reservation = if lower == 0 {
        Money::ZERO
    } else {
        fees.cash_requirement(
            Money::try_from(price.amount().checked_mul(Amount::from_unsigned(lower))?)?,
            true,
            2,
        )?
    };
    Ok(CashSizing {
        lots: lower,
        reservation,
    })
}

/// Deterministic arithmetic verdict only, never a mandate or execution receipt.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub accepted: bool,
    pub reasons: Vec<String>,
    pub lots: u64,
    pub reservation: Money,
}

impl Verdict {
    fn deny(reason: &str) -> Self {
        Self {
            accepted: false,
            reasons: vec![reason.to_owned()],
            lots: 0,
            reservation: Money::ZERO,
        }
    }
}

/// Port of the existing fixed-profile paper cash preflight. A passing verdict
/// still requires independent runtime authority, receipt binding and protection.
/// Unrepresentable exact arithmetic fails closed instead of changing quantities.
#[allow(clippy::too_many_arguments)]
pub fn paper_cash_preflight(
    candidate: &Candidate,
    snapshot: &Snapshot,
    portfolio: &Portfolio,
    fees: &FeeArithmetic,
    calendar: &Calendar,
    now: Timestamp,
    policy: PaperCashPolicy,
    allow_refreshed_snapshot: bool,
) -> Result<Verdict, FinancialError> {
    let legs = candidate.legs();
    if portfolio.session_start != policy.capital()
        || legs.len() != 1
        || legs[0].side() != Side::Buy
        || legs[0].instrument().validate_paper_execution().is_err()
    {
        return Ok(Verdict::deny("paper_cash_scope"));
    }
    let price = legs[0].limit_price();
    if candidate.planned_stop_loss_inr().amount()
        != price
            .amount()
            .checked_mul(constant_price("0.08").amount())?
        || candidate.stress_loss_inr() != price
    {
        return Ok(Verdict::deny("paper_cash_risk_mismatch"));
    }
    let base = cash_base_preflight(
        candidate,
        snapshot,
        portfolio,
        fees,
        calendar,
        now,
        &policy.limits(),
        allow_refreshed_snapshot,
    )?;
    if !base.accepted {
        return Ok(base);
    }
    // The sizing helper deliberately exposes a signed 64-bit maximum. Do not
    // wrap an extreme quote-depth-derived quantity into a smaller allowance.
    let maximum = i64::try_from(base.lots).map_err(|_| FinancialError::InvalidLotCount)?;
    let sizing = paper_cash_lots(
        price,
        fees,
        policy.cash_budget(portfolio)?,
        policy.planned_budget(portfolio)?,
        maximum,
    )?;
    Ok(Verdict {
        accepted: sizing.lots > 0,
        reasons: if sizing.lots == 0 {
            vec!["paper_cash_planned_loss_or_cost".to_owned()]
        } else {
            Vec::new()
        },
        lots: sizing.lots,
        reservation: sizing.reservation,
    })
}

/// Preserve reference reason ordering and conservative base sizing before the
/// additional planned-loss/round-trip-slippage search. The caller fixes limits.
#[allow(clippy::too_many_arguments)]
fn cash_base_preflight(
    candidate: &Candidate,
    snapshot: &Snapshot,
    portfolio: &Portfolio,
    fees: &FeeArithmetic,
    calendar: &Calendar,
    now: Timestamp,
    limits: &CashProfileLimits,
    allow_refreshed_snapshot: bool,
) -> Result<Verdict, FinancialError> {
    let mut reasons = Vec::new();
    if portfolio.halted || !portfolio.reconciled {
        reasons.push("halted_or_unreconciled".to_owned());
    }
    if !(candidate.created_at() <= now && now < candidate.valid_until()) {
        reasons.push("expired_decision".to_owned());
    }
    if snapshot.snapshot_id() != candidate.snapshot_id() && !allow_refreshed_snapshot {
        reasons.push("snapshot_mismatch".to_owned());
    }
    match calendar.is_open(now) {
        Ok(true) => {}
        Ok(false) => reasons.push("session_closed".to_owned()),
        Err(_) => reasons.push("calendar_missing".to_owned()),
    }
    if portfolio
        .session_start
        .amount()
        .checked_sub(portfolio.equity.amount())?
        >= portfolio
            .session_start
            .amount()
            .checked_mul(limits.daily_loss.amount())?
    {
        reasons.push("daily_loss".to_owned());
    }
    if portfolio
        .high_water
        .amount()
        .checked_sub(portfolio.equity.amount())?
        >= portfolio
            .high_water
            .amount()
            .checked_mul(limits.drawdown.amount())?
    {
        reasons.push("drawdown".to_owned());
    }
    if portfolio.open_positions >= limits.max_positions {
        reasons.push("position_limit".to_owned());
    }
    if portfolio.session_entries >= limits.max_entries {
        reasons.push("entry_limit".to_owned());
    }
    let mut capacities = Vec::new();
    let mut times = Vec::new();
    for leg in candidate.legs() {
        let quote = snapshot
            .quotes()
            .iter()
            .find(|quote| quote.instrument().key() == leg.instrument().key());
        let Some(quote) = quote else {
            reasons.push("stale_or_missing_quote".to_owned());
            continue;
        };
        // Full dated instrument equality, not only the canonical symbol/key.
        // Quote::fresh owns the unchanged inclusive two-second source-age gate.
        if quote.instrument() != leg.instrument() || !quote.fresh(now) {
            reasons.push("stale_or_missing_quote".to_owned());
            continue;
        }
        times.push(quote.source_at());
        // Positive ask permits this exact cross-product comparison without a
        // rounded division: (ask-bid)/ask > fixed spread fraction.
        if quote.ask().amount().checked_sub(quote.bid().amount())?
            > quote
                .ask()
                .amount()
                .checked_mul(limits.max_spread_fraction.amount())?
        {
            reasons.push("wide_spread".to_owned());
        }
        if (leg.side() == Side::Buy && quote.ask() > leg.limit_price())
            || (leg.side() == Side::Sell && quote.bid() < leg.limit_price())
        {
            reasons.push("price_changed".to_owned());
        }
        if leg
            .instrument()
            .expiry()
            .is_some_and(|expiry| expiry <= now.ist_date())
        {
            reasons.push("expiry_entry".to_owned());
        }
        let depth = if leg.side() == Side::Buy {
            quote.ask_units()
        } else {
            quote.bid_units()
        };
        capacities.push(depth / leg.instrument().lot_size());
    }
    if let (Some(first), Some(last)) = (times.iter().min(), times.iter().max()) {
        let skew = last
            .elapsed_micros_since(*first)
            .map_err(|_| FinancialError::DecimalRange)?;
        if skew > 1_000_000 {
            reasons.push("cross_leg_skew".to_owned());
        }
    }
    let long = &candidate.legs()[0];
    let standalone = long
        .limit_price()
        .amount()
        .checked_mul(Amount::from_unsigned(long.instrument().lot_size()))?;
    let mut costs = Amount::ZERO;
    for leg in candidate.legs() {
        let turnover = Money::try_from(
            leg.limit_price()
                .amount()
                .checked_mul(Amount::from_unsigned(leg.instrument().lot_size()))?,
        )?;
        let opposite = match leg.side() {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        };
        let leg_cost = fees
            .charge(leg.side(), turnover)?
            .amount()
            .checked_add(fees.charge(opposite, turnover)?.amount())?;
        costs = costs.checked_add(leg_cost)?;
    }
    let unit_risk = standalone
        .max(candidate.stress_loss_inr().amount())
        .checked_add(costs)?;
    let budget = portfolio
        .session_start
        .amount()
        .checked_mul(limits.per_entry.amount())?
        .min(
            portfolio
                .session_start
                .amount()
                .checked_mul(limits.aggregate.amount())?
                .checked_sub(portfolio.reserved_risk.amount())?,
        )
        .min(portfolio.cash.amount());
    let depth = capacities.into_iter().min().unwrap_or(0);
    let lots = if budget <= Amount::ZERO {
        0
    } else {
        let whole_lots = budget.checked_floor_ratio(unit_risk)?;
        u64::try_from(whole_lots.min(i128::from(depth)))
            .map_err(|_| FinancialError::InvalidLotCount)?
    };
    if lots == 0 {
        reasons.push("unaffordable_or_no_depth".to_owned());
    }
    if !reasons.is_empty() {
        return Ok(Verdict {
            accepted: false,
            reasons,
            lots: 0,
            reservation: Money::ZERO,
        });
    }
    Ok(Verdict {
        accepted: true,
        reasons,
        lots,
        reservation: Money::try_from(unit_risk.checked_mul(Amount::from_unsigned(lots))?)?,
    })
}

/// Configured option preflight identity: the underlying must match the
/// contract payload and the session date bounds the expiry hold.
#[derive(Debug, Clone, Copy)]
pub struct OptionPreflightSettings<'a> {
    pub underlying_key: &'a str,
    pub underlying_symbol: &'a str,
    pub today: ExchangeDate,
}

fn option_reason(error: FinancialError) -> &'static str {
    match error {
        FinancialError::InvalidOptionMandate => "paper_option_scope",
        FinancialError::OptionUnderlyingMismatch => "paper_option_underlying",
        FinancialError::OptionQuantityGrid => "paper_option_quantity",
        FinancialError::OptionShortOpen => "paper_option_short_open",
        FinancialError::OptionExpiry => "paper_option_expiry",
        _ => "paper_option_scope",
    }
}

/// Deterministic buy-only Nifty option preflight. Passing is not an execution
/// receipt: the runtime still revalidates the plan and fills on fresh depth.
///
/// Long premium spends `limit price x units` plus the configured F&O fee; it
/// never debits index notional and there is no margin ledger. Sells are only
/// allowed against same-key held units and settle at the bid.
#[allow(clippy::too_many_arguments)]
pub fn paper_option_preflight(
    candidate: &Candidate,
    snapshot: &Snapshot,
    portfolio: &Portfolio,
    holdings: &BTreeMap<String, u64>,
    catalog: &OptionCatalog,
    fees: &FeeArithmetic,
    calendar: &Calendar,
    now: Timestamp,
    settings: OptionPreflightSettings<'_>,
    allow_refreshed_snapshot: bool,
) -> Result<Verdict, FinancialError> {
    let legs = candidate.legs();
    if legs.len() != 1 {
        return Ok(Verdict::deny("paper_option_scope"));
    }
    let leg = &legs[0];
    let Some(contract) = catalog.contract(leg.instrument().key()) else {
        return Ok(Verdict::deny("paper_option_scope"));
    };
    if contract.instrument().ok().as_ref() != Some(leg.instrument()) {
        return Ok(Verdict::deny("paper_option_scope"));
    }
    let buy = leg.side() == Side::Buy;
    let held = holdings.get(leg.instrument().key()).copied().unwrap_or(0);
    let mut reasons: Vec<String> = Vec::new();
    if portfolio.halted || !portfolio.reconciled {
        reasons.push("halted_or_unreconciled".to_owned());
    }
    if !(candidate.created_at() <= now && now < candidate.valid_until()) {
        reasons.push("expired_decision".to_owned());
    }
    if snapshot.snapshot_id() != candidate.snapshot_id() && !allow_refreshed_snapshot {
        reasons.push("snapshot_mismatch".to_owned());
    }
    match calendar.is_open(now) {
        Ok(true) => {}
        Ok(false) => reasons.push("session_closed".to_owned()),
        Err(_) => reasons.push("calendar_missing".to_owned()),
    }
    let Some(quote) = snapshot
        .quotes()
        .iter()
        .find(|quote| quote.instrument().key() == leg.instrument().key())
    else {
        return Ok(Verdict::deny("stale_or_missing_quote"));
    };
    // Full dated instrument equality; `Quote::fresh` owns the inclusive
    // two-second original-receipt gate.
    if quote.instrument() != leg.instrument() || !quote.fresh(now) {
        return Ok(Verdict::deny("stale_or_missing_quote"));
    }
    if !leg.limit_price().is_multiple_of(contract.tick)? {
        reasons.push("paper_option_tick".to_owned());
    }
    if (buy && quote.ask() > leg.limit_price()) || (!buy && quote.bid() < leg.limit_price()) {
        reasons.push("price_changed".to_owned());
    }
    // Premium-based sizing: buys are bounded by available cash including the
    // F&O buy fee and by executable ask depth; sells by same-key holdings and
    // bid depth. No cash liquidity/circuit screens apply to premiums.
    let units = if buy {
        let per_lot = Price::try_from(
            leg.limit_price()
                .amount()
                .checked_mul(Amount::from_unsigned(contract.lot_size))?,
        )?;
        let affordable =
            fees.affordable_cash_lots(per_lot, portfolio.cash.amount(), i64::MAX, false, 0)?;
        affordable
            .min(quote.ask_units() / contract.lot_size)
            .checked_mul(contract.lot_size)
            .ok_or(FinancialError::InvalidLotCount)?
    } else {
        (held / contract.lot_size)
            .min(quote.bid_units() / contract.lot_size)
            .checked_mul(contract.lot_size)
            .ok_or(FinancialError::InvalidLotCount)?
    };
    if units == 0 {
        reasons.push(
            if buy {
                "paper_option_cash"
            } else {
                "paper_option_short_open"
            }
            .to_owned(),
        );
        return Ok(Verdict {
            accepted: false,
            reasons,
            lots: 0,
            reservation: Money::ZERO,
        });
    }
    let mandate = NiftyOptionMandate {
        underlying_key: settings.underlying_key,
        underlying_symbol: settings.underlying_symbol,
    };
    let order = OptionOrder {
        contract,
        buy,
        units,
        held_units: held,
        today: settings.today,
    };
    if let Err(error) = PaperMandate::NiftyOptionLong.admit_option(&order, &mandate) {
        reasons.push(option_reason(error).to_owned());
    }
    let turnover = Money::try_from(
        leg.limit_price()
            .amount()
            .checked_mul(Amount::from_unsigned(units))?,
    )?;
    let fee = fees.charge(if buy { Side::Buy } else { Side::Sell }, turnover)?;
    let reservation = if buy {
        Money::try_from(turnover.amount().checked_add(fee.amount())?)?
    } else {
        Money::ZERO
    };
    if buy && reservation.amount() > portfolio.cash.amount() {
        reasons.push("paper_option_cash".to_owned());
    }
    if !reasons.is_empty() {
        return Ok(Verdict {
            accepted: false,
            reasons,
            lots: 0,
            reservation: Money::ZERO,
        });
    }
    Ok(Verdict {
        accepted: true,
        reasons,
        lots: units / contract.lot_size,
        reservation,
    })
}
