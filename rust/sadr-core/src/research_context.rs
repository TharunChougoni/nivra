//! Bounded untrusted scientific declarations, never provenance, fees or promotion acceptance.
use crate::decimal_text::ExactDecimal;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use std::{collections::BTreeSet, fmt};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("research_context_invalid")
    }
}
impl std::error::Error for Error {}
struct Exact(ExactDecimal);
impl<'de> Deserialize<'de> for Exact {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Box::<serde_json::value::RawValue>::deserialize(d)?;
        let text = if value.get().starts_with('"') {
            serde_json::from_str::<String>(value.get())
                .map_err(|_| de::Error::custom("decimal_required"))?
        } else {
            value.get().to_owned()
        };
        ExactDecimal::parse(&text)
            .map(Self)
            .map_err(|_| de::Error::custom("bounded_exact_decimal_required"))
    }
}
impl Serialize for Exact {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(s)
    }
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Window {
    first: String,
    last: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Parameter {
    name: String,
    value: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Criterion {
    metric: String,
    comparison: String,
    threshold: Exact,
}
fn assumption() -> String {
    "ASSUMPTION_NOT_CERTIFIED".into()
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Costs {
    reference: String,
    fee_inr: Exact,
    slippage_bps: Exact,
    #[serde(default = "assumption")]
    label: String,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    registration_hash: String,
    universe: Vec<String>,
    timeframe: String,
    variant_id: String,
    parameters: Vec<Parameter>,
    development: Window,
    validation: Window,
    costs: Costs,
    criteria: Vec<Criterion>,
}
pub struct RegisteredContext {
    fields: Fields,
    original: Vec<u8>,
    declaration: Value,
    canonical: Vec<u8>,
}
impl fmt::Debug for RegisteredContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RegisteredResearchContext(untrusted declaration, no authority)")
    }
}
fn name(s: &str) -> bool {
    (1..=80).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn bounded(s: &str, max: usize) -> bool {
    (1..=max).contains(&s.chars().count())
}
fn decimal(d: &Exact) -> bool {
    let n = d.0.value().normalize();
    let scale = n.scale() as usize;
    let digits = n.mantissa().unsigned_abs().to_string().len();
    scale <= 8 && digits.saturating_sub(scale) <= 10
}
fn date(s: &str) -> Result<NaiveDate, Error> {
    if s.len() != 10
        || !s.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 4 | 7) {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
        || &s[..4] == "0000"
    {
        return Err(Error);
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| Error)
}
fn window(w: &Window) -> Result<(NaiveDate, NaiveDate), Error> {
    let first = date(&w.first)?;
    let last = date(&w.last)?;
    if first > last {
        return Err(Error);
    }
    Ok((first, last))
}
impl RegisteredContext {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 32768 {
            return Err(Error);
        }
        // Strict duplicate-key/shape validation, then parse original decimal lexemes.
        crate::native::decode(raw).map_err(|_| Error)?;
        let f: Fields = serde_json::from_slice(raw).map_err(|_| Error)?;
        let development = window(&f.development)?;
        let validation = window(&f.validation)?;
        if f.registration_hash.len() != 64
            || !f
                .registration_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || f.timeframe != "daily"
            || !name(&f.variant_id)
            || development.1 >= validation.0
            || !(1..=20).contains(&f.universe.len())
            || f.universe.iter().any(|s| !bounded(s, 128))
            || f.universe.iter().collect::<BTreeSet<_>>().len() != f.universe.len()
            || !(1..=20).contains(&f.parameters.len())
            || f.parameters
                .iter()
                .any(|p| !name(&p.name) || !bounded(&p.value, 256))
            || f.parameters
                .iter()
                .map(|p| &p.name)
                .collect::<BTreeSet<_>>()
                .len()
                != f.parameters.len()
            || !(1..=20).contains(&f.criteria.len())
            || f.criteria.iter().any(|c| {
                !name(&c.metric)
                    || !matches!(c.comparison.as_str(), "ge" | "le")
                    || !decimal(&c.threshold)
            })
            || f.criteria
                .iter()
                .map(|c| &c.metric)
                .collect::<BTreeSet<_>>()
                .len()
                != f.criteria.len()
            || !bounded(&f.costs.reference, 256)
            || f.costs.label != "ASSUMPTION_NOT_CERTIFIED"
            || !decimal(&f.costs.fee_inr)
            || f.costs.fee_inr.0.value() < Decimal::ZERO
            || !decimal(&f.costs.slippage_bps)
            || f.costs.slippage_bps.0.value() < Decimal::ZERO
            || f.costs.slippage_bps.0.value() > Decimal::from(10000)
        {
            return Err(Error);
        }
        let canonical = serde_json::to_vec(&f).map_err(|_| Error)?;
        if canonical.len() > 32768 {
            return Err(Error);
        }
        let declaration = serde_json::from_slice(&canonical).map_err(|_| Error)?;
        Ok(Self {
            canonical,
            fields: f,
            original: raw.to_vec(),
            declaration,
        })
    }
    /// Original bytes preserve historical identity; never regenerate an old registration hash.
    pub fn original_json(&self) -> &[u8] {
        &self.original
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn declaration(&self) -> &Value {
        &self.declaration
    }
    pub fn registration_hash(&self) -> &str {
        &self.fields.registration_hash
    }
    pub fn cost_label(&self) -> &str {
        &self.fields.costs.label
    }
    pub fn promotion_authorized(&self) -> bool {
        false
    }
}
