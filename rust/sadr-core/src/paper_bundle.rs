//! Historical typed paper inputs. Parsing is not activation, provenance, eligibility
//! or current-session authority. Original bytes remain the identity material.
use crate::{
    candidate::Candidate,
    finance::{FeeArithmetic, Money, Price},
    market::{Calendar, Snapshot},
    research_evidence::Evidence,
    risk::PaperCashPolicy,
    time::{ExchangeDate, Timestamp},
};
use serde::Deserialize;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("paper_bundle_invalid")
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DecisionPolicy {
    #[default]
    CodeOnly,
    Hybrid,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MandateFields {
    mandate_id: String,
    strategy: String,
    instruments: Vec<String>,
    capital: Price,
    valid_from: Timestamp,
    valid_until: Timestamp,
    approved_by: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    decision_policy: DecisionPolicy,
    #[serde(default)]
    cash_policy: Option<PaperCashPolicy>,
}
#[derive(Deserialize)]
#[serde(try_from = "MandateFields")]
pub struct PaperMandate(MandateFields);
impl TryFrom<MandateFields> for PaperMandate {
    type Error = Error;
    fn try_from(f: MandateFields) -> Result<Self, Error> {
        if f.mandate_id.is_empty()
            || f.approved_by.is_empty()
            || f.instruments.is_empty()
            || f.valid_until <= f.valid_from
            || f.cash_policy.is_some_and(|p| p.capital() != f.capital)
        {
            return Err(Error);
        }
        Ok(Self(f))
    }
}
impl PaperMandate {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        decode(raw)
    }
    pub fn mandate_id(&self) -> &str {
        &self.0.mandate_id
    }
    pub fn strategy(&self) -> &str {
        &self.0.strategy
    }
    pub fn instruments(&self) -> &[String] {
        &self.0.instruments
    }
    pub fn capital(&self) -> Price {
        self.0.capital
    }
    pub fn valid_from(&self) -> Timestamp {
        self.0.valid_from
    }
    pub fn valid_until(&self) -> Timestamp {
        self.0.valid_until
    }
    pub fn approved_by(&self) -> &str {
        &self.0.approved_by
    }
    pub fn declared_active(&self) -> bool {
        self.0.active
    }
    /// Historical default only: no runtime model fallback is introduced here.
    pub fn decision_policy(&self) -> DecisionPolicy {
        self.0.decision_policy
    }
    pub fn cash_policy(&self) -> Option<PaperCashPolicy> {
        self.0.cash_policy
    }
    pub fn activation_authorized(&self) -> bool {
        false
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FeeFields {
    model_version: String,
    as_of: ExchangeDate,
    synthetic: bool,
    per_order: Money,
    buy_bps: Money,
    sell_bps: Money,
    slippage_bps: Money,
    #[serde(default)]
    rounding_increment: Option<Price>,
}
#[derive(Deserialize)]
#[serde(try_from = "FeeFields")]
pub struct DatedFees(FeeFields);
impl TryFrom<FeeFields> for DatedFees {
    type Error = Error;
    fn try_from(f: FeeFields) -> Result<Self, Error> {
        if f.model_version.is_empty() {
            return Err(Error);
        }
        Ok(Self(f))
    }
}
impl DatedFees {
    pub fn model_version(&self) -> &str {
        &self.0.model_version
    }
    pub fn as_of(&self) -> ExchangeDate {
        self.0.as_of
    }
    pub fn synthetic(&self) -> bool {
        self.0.synthetic
    }
    pub fn arithmetic(&self) -> FeeArithmetic {
        FeeArithmetic {
            per_order: self.0.per_order,
            buy_bps: self.0.buy_bps,
            sell_bps: self.0.sell_bps,
            slippage_bps: self.0.slippage_bps,
            rounding_increment: self.0.rounding_increment,
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleFields {
    candidate: Candidate,
    snapshot: Snapshot,
    calendar: Calendar,
    fees: DatedFees,
    mandate: PaperMandate,
    bar_close: Timestamp,
    #[serde(default)]
    evidence: Vec<Evidence>,
}
pub struct PaperBundle {
    fields: BundleFields,
    original: Vec<u8>,
}
impl fmt::Debug for PaperBundle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PaperBundle(redacted, no activation authority)")
    }
}
impl PaperBundle {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        let fields: BundleFields = decode(raw)?;
        if fields.candidate.snapshot_id() != fields.snapshot.snapshot_id() {
            return Err(Error);
        }
        Ok(Self {
            fields,
            original: raw.to_vec(),
        })
    }
    pub fn original_json(&self) -> &[u8] {
        &self.original
    }
    pub fn candidate(&self) -> &Candidate {
        &self.fields.candidate
    }
    pub fn snapshot(&self) -> &Snapshot {
        &self.fields.snapshot
    }
    pub fn calendar(&self) -> &Calendar {
        &self.fields.calendar
    }
    pub fn fees(&self) -> &DatedFees {
        &self.fields.fees
    }
    pub fn mandate(&self) -> &PaperMandate {
        &self.fields.mandate
    }
    pub fn bar_close(&self) -> Timestamp {
        self.fields.bar_close
    }
    pub fn evidence(&self) -> &[Evidence] {
        &self.fields.evidence
    }
    pub fn activation_authorized(&self) -> bool {
        false
    }
}
fn decode<T: serde::de::DeserializeOwned>(raw: &[u8]) -> Result<T, Error> {
    // Bounded, duplicate-rejecting admission; no file or environment access.
    serde_json::from_value(crate::native::decode(raw).map_err(|_| Error)?).map_err(|_| Error)
}
