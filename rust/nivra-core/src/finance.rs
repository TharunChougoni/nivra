//! First R1 arithmetic slice, not a dated fee policy or a trading/risk decision.
//!
//! Financial inputs are strings or integers, never JSON floats/bools. Operations
//! use checked integer coefficients and reject unrepresentable exact results;
//! they do not silently use Decimal's rounding/saturating arithmetic. Python's
//! broader Decimal range and contextual rounding are not claimed compatible.

use crate::market::Segment;
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{collections::BTreeMap, error::Error, fmt, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinancialError {
    InvalidDecimal,
    DecimalRange,
    NegativeMoney,
    NonpositivePrice,
    InvalidCostScope,
    InvalidLotCount,
    InvalidOrderGrid,
    InvalidDivisor,
    /// Structural derivative identity refused by `PaperMandate::NiftyOptionLong`.
    InvalidOptionMandate,
    /// Option underlying is not the configured Nifty index.
    OptionUnderlyingMismatch,
    /// Option units are not a positive multiple of the contract lot.
    OptionQuantityGrid,
    /// A sell would open or exceed a short option position.
    OptionShortOpen,
    /// The order would hold an option past its expiry.
    OptionExpiry,
}

impl fmt::Display for FinancialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidDecimal => "invalid_decimal_input",
            Self::DecimalRange => "exact_decimal_range_exceeded",
            Self::NegativeMoney => "money_must_be_nonnegative",
            Self::NonpositivePrice => "price_must_be_positive",
            Self::InvalidCostScope => "invalid_slippage_legs",
            Self::InvalidLotCount => "invalid_maximum_lots",
            Self::InvalidOrderGrid => "invalid_lot_or_tick_quantity",
            Self::InvalidDivisor => "positive_decimal_divisor_required",
            Self::InvalidOptionMandate => "invalid_option_mandate",
            Self::OptionUnderlyingMismatch => "option_underlying_mismatch",
            Self::OptionQuantityGrid => "option_quantity_grid",
            Self::OptionShortOpen => "option_short_open",
            Self::OptionExpiry => "option_expiry_hold",
        })
    }
}

impl Error for FinancialError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(Decimal);

impl Amount {
    pub const ZERO: Self = Self(Decimal::ZERO);

    pub fn from_integer(value: i64) -> Self {
        Self(Decimal::from(value))
    }

    pub fn from_unsigned(value: u64) -> Self {
        Self(Decimal::from(value))
    }

    pub fn checked_add(self, other: Self) -> Result<Self, FinancialError> {
        exact_add(self.0, other.0).map(Self)
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, FinancialError> {
        let (a, b, scale) = aligned_coefficients(self.0, other.0)?;
        exact_parts(a.checked_sub(b).ok_or(FinancialError::DecimalRange)?, scale).map(Self)
    }

    pub fn checked_mul(self, other: Self) -> Result<Self, FinancialError> {
        exact_mul(self.0, other.0).map(Self)
    }

    /// Exact decimal division for reporting-only ratios. Financial gates must
    /// not derive cash or fees from a rounded quotient.
    pub fn checked_div(self, other: Self) -> Result<Self, FinancialError> {
        if other == Self::ZERO {
            return Err(FinancialError::InvalidDivisor);
        }
        self.0
            .checked_div(other.0)
            .map(Self)
            .ok_or(FinancialError::DecimalRange)
    }

    /// Exact integer quotient for a positive divisor, truncating toward zero
    /// like Python Decimal `//`. This is floor division for nonnegative inputs;
    /// callers must clamp negative risk budgets before using the result as lots.
    pub fn checked_floor_ratio(self, divisor: Self) -> Result<i128, FinancialError> {
        if divisor <= Self::ZERO {
            return Err(FinancialError::InvalidDivisor);
        }
        let (numerator, denominator, _) = aligned_coefficients(self.0, divisor.0)?;
        Ok(numerator / denominator)
    }

    pub fn is_multiple_of(self, divisor: Self) -> Result<bool, FinancialError> {
        if divisor <= Self::ZERO {
            return Err(FinancialError::InvalidDivisor);
        }
        let (numerator, denominator, _) = aligned_coefficients(self.0, divisor.0)?;
        Ok(numerator % denominator == 0)
    }

    pub fn parse(text: &str) -> Result<Self, FinancialError> {
        // Bound string/exponent work and reject non-decimal notation/NaN/infinity.
        if text.is_empty() || text.len() > 128 || !text.is_ascii() {
            return Err(FinancialError::InvalidDecimal);
        }
        let (negative, unsigned) = match text.as_bytes()[0] {
            b'-' => (true, &text[1..]),
            b'+' => (false, &text[1..]),
            _ => (false, text),
        };
        let mut pieces = unsigned.split(['e', 'E']);
        let base = pieces.next().ok_or(FinancialError::InvalidDecimal)?;
        let exponent = match pieces.next() {
            Some(value) => value
                .parse::<i32>()
                .map_err(|_| FinancialError::InvalidDecimal)?,
            None => 0,
        };
        if pieces.next().is_some() || !(-128..=128).contains(&exponent) {
            return Err(FinancialError::InvalidDecimal);
        }
        let mut coefficient = 0_i128;
        let mut fraction = 0_i32;
        let mut point_seen = false;
        let mut digit_seen = false;
        for byte in base.bytes() {
            if byte == b'.' && !point_seen {
                point_seen = true;
            } else if byte.is_ascii_digit() {
                digit_seen = true;
                coefficient = coefficient
                    .checked_mul(10)
                    .and_then(|value| value.checked_add(i128::from(byte - b'0')))
                    .ok_or(FinancialError::DecimalRange)?;
                if point_seen {
                    fraction += 1;
                }
            } else {
                return Err(FinancialError::InvalidDecimal);
            }
        }
        if !digit_seen {
            return Err(FinancialError::InvalidDecimal);
        }
        let scale = fraction - exponent;
        if coefficient == 0 {
            return Ok(Self::ZERO);
        }
        if negative {
            coefficient = -coefficient;
        }
        let value = if scale < 0 {
            let factor = 10_i128
                .checked_pow(scale.unsigned_abs())
                .ok_or(FinancialError::DecimalRange)?;
            exact_parts(
                coefficient
                    .checked_mul(factor)
                    .ok_or(FinancialError::DecimalRange)?,
                0,
            )?
        } else {
            exact_parts(coefficient, scale as u32)?
        };
        Ok(Self(value))
    }
}

impl FromStr for Amount {
    type Err = FinancialError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.normalize().fmt(f)
    }
}

impl Serialize for Amount {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = Amount;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an exact decimal string or integer, never a float/bool")
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Amount, E> {
                Amount::parse(value).map_err(E::custom)
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Amount, E> {
                Ok(Amount(Decimal::from(value)))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Amount, E> {
                Ok(Amount(Decimal::from(value)))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Money(Amount);

impl Default for Money {
    fn default() -> Self {
        Self::ZERO
    }
}

impl Money {
    pub const ZERO: Self = Self(Amount::ZERO);

    pub fn parse(text: &str) -> Result<Self, FinancialError> {
        Self::try_from(Amount::parse(text)?)
    }

    pub fn amount(self) -> Amount {
        self.0
    }
}

impl TryFrom<Amount> for Money {
    type Error = FinancialError;

    fn try_from(amount: Amount) -> Result<Self, FinancialError> {
        if amount < Amount::ZERO {
            Err(FinancialError::NegativeMoney)
        } else {
            Ok(Self(amount))
        }
    }
}

impl<'de> Deserialize<'de> for Money {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(Amount::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Price(Amount);

impl Price {
    pub fn parse(text: &str) -> Result<Self, FinancialError> {
        Self::try_from(Amount::parse(text)?)
    }

    pub fn amount(self) -> Amount {
        self.0
    }

    pub fn is_multiple_of(self, tick: Self) -> Result<bool, FinancialError> {
        self.0.is_multiple_of(tick.0)
    }
}

impl TryFrom<Amount> for Price {
    type Error = FinancialError;

    fn try_from(amount: Amount) -> Result<Self, FinancialError> {
        if amount <= Amount::ZERO {
            Err(FinancialError::NonpositivePrice)
        } else {
            Ok(Self(amount))
        }
    }
}

impl<'de> Deserialize<'de> for Price {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(Amount::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

/// Arithmetic fields only. Source, dated fees, entitlement and strategy approval
/// must be bound separately before a future runtime can consume this policy.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeeArithmetic {
    pub per_order: Money,
    pub buy_bps: Money,
    pub sell_bps: Money,
    pub slippage_bps: Money,
    pub rounding_increment: Option<Price>,
}

impl FeeArithmetic {
    pub fn validate(&self) -> Result<(), FinancialError> {
        if self.buy_bps.amount() > Amount::from_integer(10000)
            || self.sell_bps.amount() > Amount::from_integer(10000)
            || self.slippage_bps.amount() >= Amount::from_integer(10000)
        {
            return Err(FinancialError::InvalidCostScope);
        }
        self.cash_requirement(Money::ZERO, true, 2)?;
        Ok(())
    }

    pub fn charge(&self, side: Side, turnover: Money) -> Result<Money, FinancialError> {
        let rate = match side {
            Side::Buy => self.buy_bps,
            Side::Sell => self.sell_bps,
        };
        let amount = exact_add(self.per_order.0.0, basis_points(turnover.0.0, rate.0.0)?)?;
        let amount = match self.rounding_increment {
            Some(increment) => round_up(amount, increment.0.0)?,
            None => amount,
        };
        Money::try_from(Amount(amount))
    }

    pub fn cash_requirement(
        &self,
        turnover: Money,
        round_trip: bool,
        slippage_legs: u8,
    ) -> Result<Money, FinancialError> {
        if slippage_legs > 2 {
            return Err(FinancialError::InvalidCostScope);
        }
        let buy = self.charge(Side::Buy, turnover)?;
        let sell = if round_trip {
            self.charge(Side::Sell, turnover)?
        } else {
            Money::ZERO
        };
        let slippage = if slippage_legs == 0 {
            Decimal::ZERO
        } else {
            exact_mul(
                basis_points(turnover.0.0, self.slippage_bps.0.0)?,
                Decimal::from(slippage_legs),
            )?
        };
        let cost = exact_add(
            exact_add(exact_add(turnover.0.0, buy.0.0)?, sell.0.0)?,
            slippage,
        )?;
        Money::try_from(Amount(cost))
    }

    pub fn affordable_cash_lots(
        &self,
        turnover_per_lot: Price,
        budget: Amount,
        maximum_lots: i64,
        round_trip: bool,
        slippage_legs: u8,
    ) -> Result<u64, FinancialError> {
        let mut upper = u64::try_from(maximum_lots).map_err(|_| FinancialError::InvalidLotCount)?;
        // Validate scope even if no lot is affordable.
        self.cash_requirement(Money::ZERO, round_trip, slippage_legs)?;
        if budget <= Amount::ZERO {
            return Ok(0);
        }
        let (budget_coefficient, unit_coefficient, _) =
            aligned_coefficients(budget.0, turnover_per_lot.0.0)?;
        upper = (budget_coefficient / unit_coefficient).min(i128::from(upper)) as u64;
        let mut lower = 0;
        while lower < upper {
            let middle = lower + (upper - lower).div_ceil(2);
            let turnover = Money::try_from(Amount(exact_mul(
                turnover_per_lot.0.0,
                Decimal::from(middle),
            )?))?;
            let cost = self.cash_requirement(turnover, round_trip, slippage_legs)?;
            if cost.amount() <= budget {
                lower = middle;
            } else {
                upper = middle - 1;
            }
        }
        Ok(lower)
    }
}

/// Per-segment fee lookup. `cash` is the unchanged session `paper_costs`
/// schedule; `by_segment` holds optional non-cash schedules (for example
/// `NSE_FO`). A non-cash instrument never borrows the cash bps as a stand-in.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeeSchedule {
    cash: FeeArithmetic,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    by_segment: BTreeMap<Segment, FeeArithmetic>,
}

impl FeeSchedule {
    pub fn new(cash: FeeArithmetic) -> Self {
        Self {
            cash,
            by_segment: BTreeMap::new(),
        }
    }

    pub fn with_segment(mut self, segment: Segment, fees: FeeArithmetic) -> Self {
        self.by_segment.insert(segment, fees);
        self
    }

    pub fn cash(&self) -> &FeeArithmetic {
        &self.cash
    }

    /// Configured non-cash schedules, in segment order.
    pub fn segments(&self) -> impl Iterator<Item = (Segment, &FeeArithmetic)> {
        self.by_segment
            .iter()
            .map(|(segment, fees)| (*segment, fees))
    }

    /// Cash equity uses `paper_costs`; other segments need a configured entry.
    pub fn for_segment(&self, segment: Segment) -> Option<&FeeArithmetic> {
        match segment {
            Segment::NseEq => Some(&self.cash),
            segment => self.by_segment.get(&segment),
        }
    }

    pub fn validate(&self) -> Result<(), FinancialError> {
        self.cash.validate()?;
        for fees in self.by_segment.values() {
            fees.validate()?;
        }
        Ok(())
    }
}

/// Tick/lot arithmetic only. This cannot establish instrument/session eligibility.
#[derive(Debug, Clone, Copy)]
pub struct CashOrderGrid {
    lot_size: u64,
    tick_size: Price,
}

impl CashOrderGrid {
    pub fn new(lot_size: u64, tick_size: Price) -> Result<Self, FinancialError> {
        if lot_size == 0 {
            return Err(FinancialError::InvalidOrderGrid);
        }
        Ok(Self {
            lot_size,
            tick_size,
        })
    }

    pub fn validate(&self, units: i64, price: Price) -> Result<(), FinancialError> {
        let units = u64::try_from(units).map_err(|_| FinancialError::InvalidOrderGrid)?;
        if units == 0 || !units.is_multiple_of(self.lot_size) {
            return Err(FinancialError::InvalidOrderGrid);
        }
        let (price, tick, _) = aligned_coefficients(price.0.0, self.tick_size.0.0)?;
        if price % tick != 0 {
            return Err(FinancialError::InvalidOrderGrid);
        }
        Ok(())
    }
}

fn exact_parts(mut coefficient: i128, mut scale: u32) -> Result<Decimal, FinancialError> {
    if coefficient == 0 {
        return Ok(Decimal::ZERO);
    }
    while scale > 0 && coefficient % 10 == 0 {
        coefficient /= 10;
        scale -= 1;
    }
    if scale > Decimal::MAX_SCALE {
        return Err(FinancialError::DecimalRange);
    }
    Decimal::try_from_i128_with_scale(coefficient, scale).map_err(|_| FinancialError::DecimalRange)
}

fn aligned_coefficients(a: Decimal, b: Decimal) -> Result<(i128, i128, u32), FinancialError> {
    let scale = a.scale().max(b.scale());
    let align = |value: Decimal| {
        let factor = 10_i128
            .checked_pow(scale - value.scale())
            .ok_or(FinancialError::DecimalRange)?;
        value
            .mantissa()
            .checked_mul(factor)
            .ok_or(FinancialError::DecimalRange)
    };
    Ok((align(a)?, align(b)?, scale))
}

fn exact_add(a: Decimal, b: Decimal) -> Result<Decimal, FinancialError> {
    let (a, b, scale) = aligned_coefficients(a, b)?;
    exact_parts(a.checked_add(b).ok_or(FinancialError::DecimalRange)?, scale)
}

fn exact_mul(a: Decimal, b: Decimal) -> Result<Decimal, FinancialError> {
    exact_parts(
        a.mantissa()
            .checked_mul(b.mantissa())
            .ok_or(FinancialError::DecimalRange)?,
        a.scale() + b.scale(),
    )
}

fn basis_points(turnover: Decimal, rate: Decimal) -> Result<Decimal, FinancialError> {
    exact_parts(
        turnover
            .mantissa()
            .checked_mul(rate.mantissa())
            .ok_or(FinancialError::DecimalRange)?,
        turnover.scale() + rate.scale() + 4,
    )
}

fn round_up(amount: Decimal, increment: Decimal) -> Result<Decimal, FinancialError> {
    let (numerator, denominator, _) = aligned_coefficients(amount, increment)?;
    let steps = (numerator / denominator)
        .checked_add(i128::from(numerator % denominator != 0))
        .ok_or(FinancialError::DecimalRange)?;
    exact_parts(
        steps
            .checked_mul(increment.mantissa())
            .ok_or(FinancialError::DecimalRange)?,
        increment.scale(),
    )
}
