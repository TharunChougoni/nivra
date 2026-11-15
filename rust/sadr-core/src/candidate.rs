//! Validated immutable Python-reference candidate contracts.
//!
//! Option spread validation preserves domain compatibility; it neither enables
//! derivative trading nor supplies eligibility, model or execution authority.

use crate::{
    finance::{Amount, FinancialError, Money, Price, Side},
    market::{Instrument, InstrumentKind},
    time::Timestamp,
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Leg {
    instrument: Instrument,
    side: Side,
    limit_price: Price,
}

impl Leg {
    pub fn instrument(&self) -> &Instrument {
        &self.instrument
    }
    pub fn side(&self) -> Side {
        self.side
    }
    pub fn limit_price(&self) -> Price {
        self.limit_price
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CandidateFields {
    candidate_id: String,
    strategy: String,
    snapshot_id: String,
    created_at: Timestamp,
    valid_until: Timestamp,
    legs: Vec<Leg>,
    planned_stop_loss_inr: Money,
    stress_loss_inr: Price,
    #[serde(default)]
    payoff_max_loss_inr: Option<Money>,
    #[serde(default)]
    max_gain_inr: Option<Money>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Candidate(CandidateFields);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateError {
    InvalidLifetimeOrStructure,
    FirstLegNotLong,
    InvalidSpreadContracts,
    InvalidDebitVertical,
    InconsistentPayoff,
    Arithmetic(FinancialError),
}

impl fmt::Display for CandidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLifetimeOrStructure => "invalid_candidate_lifetime_or_structure",
            Self::FirstLegNotLong => "candidate_first_leg_must_be_long",
            Self::InvalidSpreadContracts => "invalid_spread_contracts",
            Self::InvalidDebitVertical => "invalid_debit_vertical",
            Self::InconsistentPayoff => "inconsistent_spread_payoff",
            Self::Arithmetic(_) => "candidate_exact_arithmetic_or_order_grid_unavailable",
        })
    }
}

impl Error for CandidateError {}

impl From<FinancialError> for CandidateError {
    fn from(error: FinancialError) -> Self {
        Self::Arithmetic(error)
    }
}

impl TryFrom<CandidateFields> for Candidate {
    type Error = CandidateError;

    fn try_from(value: CandidateFields) -> Result<Self, Self::Error> {
        if value.valid_until <= value.created_at || !matches!(value.legs.len(), 1 | 2) {
            return Err(CandidateError::InvalidLifetimeOrStructure);
        }
        for leg in &value.legs {
            // The existing public order API has a signed 64-bit unit range.
            // Never wrap a larger validated instrument lot into that range.
            let units = i64::try_from(leg.instrument().lot_size())
                .map_err(|_| FinancialError::InvalidOrderGrid)?;
            leg.instrument().validate_order(units, leg.limit_price())?;
        }
        if value.legs[0].side() != Side::Buy {
            return Err(CandidateError::FirstLegNotLong);
        }
        if value.legs.len() == 2 {
            let long = &value.legs[0];
            let short = &value.legs[1];
            let a = long.instrument();
            let b = short.instrument();
            if short.side() != Side::Sell
                || !matches!(a.kind(), InstrumentKind::Call | InstrumentKind::Put)
                || a.kind() != b.kind()
                || a.expiry() != b.expiry()
                || a.underlying() != b.underlying()
                || a.lot_size() != b.lot_size()
            {
                return Err(CandidateError::InvalidSpreadContracts);
            }
            let a_strike = a
                .strike()
                .ok_or(CandidateError::InvalidSpreadContracts)?
                .amount();
            let b_strike = b
                .strike()
                .ok_or(CandidateError::InvalidSpreadContracts)?
                .amount();
            let width = if a.kind() == InstrumentKind::Call {
                b_strike.checked_sub(a_strike)?
            } else {
                a_strike.checked_sub(b_strike)?
            };
            let debit = long
                .limit_price()
                .amount()
                .checked_sub(short.limit_price().amount())?;
            if debit <= Amount::ZERO || debit >= width {
                return Err(CandidateError::InvalidDebitVertical);
            }
            let lot_size = Amount::from_unsigned(a.lot_size());
            let loss = Money::try_from(debit.checked_mul(lot_size)?)?;
            let gain = Money::try_from(width.checked_sub(debit)?.checked_mul(lot_size)?)?;
            if value.payoff_max_loss_inr != Some(loss) || value.max_gain_inr != Some(gain) {
                return Err(CandidateError::InconsistentPayoff);
            }
        }
        Ok(Self(value))
    }
}

impl<'de> Deserialize<'de> for Candidate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(CandidateFields::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

impl Candidate {
    pub fn candidate_id(&self) -> &str {
        &self.0.candidate_id
    }
    pub fn strategy(&self) -> &str {
        &self.0.strategy
    }
    pub fn snapshot_id(&self) -> &str {
        &self.0.snapshot_id
    }
    pub fn created_at(&self) -> Timestamp {
        self.0.created_at
    }
    pub fn valid_until(&self) -> Timestamp {
        self.0.valid_until
    }
    pub fn legs(&self) -> &[Leg] {
        &self.0.legs
    }
    pub fn planned_stop_loss_inr(&self) -> Money {
        self.0.planned_stop_loss_inr
    }
    pub fn stress_loss_inr(&self) -> Price {
        self.0.stress_loss_inr
    }
    pub fn payoff_max_loss_inr(&self) -> Option<Money> {
        self.0.payoff_max_loss_inr
    }
    pub fn max_gain_inr(&self) -> Option<Money> {
        self.0.max_gain_inr
    }
}
