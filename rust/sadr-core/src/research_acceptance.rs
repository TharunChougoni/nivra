//! Inert novel-strategy evidence declarations and the original bounded checklist.
//! Hashes and `sealed` are caller assertions, never authenticated provenance.
//! No holdout, generated code, file, provider or trading capability is accessed.
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::Value;
use std::{collections::BTreeSet, fmt};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("novel_evaluation_unavailable")
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Preregistration,
    OutOfSample,
    CostStress,
    WalkForward,
    Holdout,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Preregistration => "preregistration",
            Self::OutOfSample => "out_of_sample",
            Self::CostStress => "cost_stress",
            Self::WalkForward => "walk_forward",
            Self::Holdout => "holdout",
        }
    }
}
const KINDS: [Kind; 5] = [
    Kind::Preregistration,
    Kind::OutOfSample,
    Kind::CostStress,
    Kind::WalkForward,
    Kind::Holdout,
];
// Preserve the original non-strict Pydantic boolean declaration coercions. This
// does not authenticate the asserted seal or grant access to holdout material.
#[derive(Clone, Copy, Serialize)]
#[serde(transparent)]
struct DeclaredBool(bool);
impl<'de> Deserialize<'de> for DeclaredBool {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        let b = match v {
            Value::Bool(b) => Some(b),
            Value::Number(n) => match n.as_f64() {
                Some(0.0) => Some(false),
                Some(1.0) => Some(true),
                _ => None,
            },
            Value::String(s) => {
                if ["0", "off", "f", "false", "n", "no"]
                    .iter()
                    .any(|v| s.eq_ignore_ascii_case(v))
                {
                    Some(false)
                } else if ["1", "on", "t", "true", "y", "yes"]
                    .iter()
                    .any(|v| s.eq_ignore_ascii_case(v))
                {
                    Some(true)
                } else {
                    None
                }
            }
            _ => None,
        }
        .ok_or_else(|| de::Error::custom("boolean_declaration_required"))?;
        Ok(Self(b))
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceFields {
    kind: Kind,
    sha256: String,
    sealed: DeclaredBool,
}
#[derive(Clone)]
pub struct EvidenceRef {
    fields: EvidenceFields,
    canonical: Vec<u8>,
}
impl fmt::Debug for EvidenceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NovelEvidenceRef(untrusted declaration, no authority)")
    }
}
impl EvidenceRef {
    /// Duplicate JSON keys and an 8 KiB raw-input cap are explicit stricter
    /// boundaries. A valid hash is a declaration, not source authentication.
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 8192 {
            return Err(Error);
        }
        crate::native::decode(raw).map_err(|_| Error)?;
        let fields: EvidenceFields = serde_json::from_slice(raw).map_err(|_| Error)?;
        if fields.sha256.len() != 64
            || !fields
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error);
        }
        let canonical = serde_json::to_vec(&fields).map_err(|_| Error)?;
        Ok(Self { fields, canonical })
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn kind(&self) -> Kind {
        self.fields.kind
    }
    pub fn sealed_declared(&self) -> bool {
        self.fields.sealed.0
    }
    pub fn source_authenticated(&self) -> bool {
        false
    }
}
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Review {
    #[default]
    Unreviewed,
    Accepted,
    Rejected,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceStatus {
    Complete,
    Incomplete,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Holdout {
    Sealed,
    Opened,
    Missing,
}
#[derive(Clone, Copy, Default)]
struct NoLive;
impl Serialize for NoLive {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bool(false)
    }
}
impl<'de> Deserialize<'de> for NoLive {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        if v == Value::Bool(false) || matches!(v,Value::Number(ref n) if n.as_f64()==Some(0.0)) {
            Ok(Self)
        } else {
            Err(de::Error::custom("live_disabled_required"))
        }
    }
}
fn version() -> String {
    "novel-evaluation-v1".into()
}
fn certification() -> String {
    "NOT_CERTIFIED".into()
}
fn unauthorized() -> String {
    "NOT_AUTHORIZED".into()
}
fn not_executed() -> String {
    "NOT_EXECUTED".into()
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EvaluationFields {
    #[serde(default = "version")]
    version: String,
    strategy_id: String,
    evidence_status: EvidenceStatus,
    missing: Vec<String>,
    holdout: Holdout,
    evidence_review: Review,
    #[serde(default = "certification")]
    certification: String,
    #[serde(default = "unauthorized")]
    promotion: String,
    #[serde(default = "unauthorized")]
    execution: String,
    #[serde(default = "not_executed")]
    generated_code: String,
    #[serde(default)]
    live_enabled: NoLive,
}
pub struct Evaluation {
    fields: EvaluationFields,
    canonical: Vec<u8>,
}
impl fmt::Debug for Evaluation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NovelEvaluationChecklist(redacted, not certified, no authority)")
    }
}
fn strategy(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:-".contains(&b))
}
impl Evaluation {
    /// Untrusted schema declaration. Its original coherence rules do not prove
    /// that evidence was checked; use evaluate_novel to derive the checklist.
    /// Duplicate JSON keys and a 32 KiB input cap are explicit stricter boundaries.
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 32768 {
            return Err(Error);
        }
        crate::native::decode(raw).map_err(|_| Error)?;
        Self::checked(serde_json::from_slice(raw).map_err(|_| Error)?)
    }
    fn checked(fields: EvaluationFields) -> Result<Self, Error> {
        if fields.version != version()
            || !strategy(&fields.strategy_id)
            || fields.missing.len() > 8
            || (fields.evidence_status == EvidenceStatus::Complete) != fields.missing.is_empty()
            || fields.certification != certification()
            || fields.promotion != unauthorized()
            || fields.execution != unauthorized()
            || fields.generated_code != not_executed()
        {
            return Err(Error);
        }
        let canonical = serde_json::to_vec(&fields).map_err(|_| Error)?;
        Ok(Self { fields, canonical })
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn evidence_status(&self) -> EvidenceStatus {
        self.fields.evidence_status
    }
    pub fn missing(&self) -> &[String] {
        &self.fields.missing
    }
    pub fn holdout(&self) -> Holdout {
        self.fields.holdout
    }
    pub fn evidence_review(&self) -> Review {
        self.fields.evidence_review
    }
    pub fn certified(&self) -> bool {
        false
    }
    pub fn promotion_authorized(&self) -> bool {
        false
    }
    pub fn execution_authorized(&self) -> bool {
        false
    }
    pub fn live_enabled(&self) -> bool {
        false
    }
}
/// Presence/sealed-declaration checklist only. An ACCEPTED review still cannot
/// certify, promote, execute, enable live trading, or authenticate any evidence.
pub fn evaluate_novel(
    strategy_id: &str,
    evidence: &[EvidenceRef],
    review: Review,
) -> Result<Evaluation, Error> {
    if !strategy(strategy_id) || evidence.len() > 8 {
        return Err(Error);
    }
    let present = evidence
        .iter()
        .map(EvidenceRef::kind)
        .collect::<BTreeSet<_>>();
    if present.len() != evidence.len() {
        return Err(Error);
    }
    let mut missing = KINDS
        .iter()
        .filter(|k| !present.contains(k))
        .map(|k| k.name().to_owned())
        .collect::<Vec<_>>();
    let holdout = match evidence.iter().find(|e| e.kind() == Kind::Holdout) {
        None => Holdout::Missing,
        Some(e) if e.sealed_declared() => Holdout::Sealed,
        Some(_) => {
            missing.push("holdout_sealed".into());
            Holdout::Opened
        }
    };
    let review =
        if review != Review::Unreviewed && (!missing.is_empty() || holdout != Holdout::Sealed) {
            Review::Rejected
        } else {
            review
        };
    Evaluation::checked(EvaluationFields {
        version: version(),
        strategy_id: strategy_id.into(),
        evidence_status: if missing.is_empty() {
            EvidenceStatus::Complete
        } else {
            EvidenceStatus::Incomplete
        },
        missing,
        holdout,
        evidence_review: review,
        certification: certification(),
        promotion: unauthorized(),
        execution: unauthorized(),
        generated_code: not_executed(),
        live_enabled: NoLive,
    })
}
