//! Exact plain-document receipts and bounded lookup. No collector, network or source authentication.
use chrono::{DateTime, FixedOffset, SecondsFormat, Timelike};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("research_evidence_invalid")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, PartialEq, Eq)]
struct Stamp(DateTime<FixedOffset>);
impl Stamp {
    fn text(&self, use_z: bool) -> String {
        self.0.to_rfc3339_opts(
            if self.0.nanosecond() == 0 {
                SecondsFormat::Secs
            } else {
                SecondsFormat::Micros
            },
            use_z,
        )
    }
}
impl<'de> Deserialize<'de> for Stamp {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        let dt = DateTime::parse_from_rfc3339(&s)
            .map_err(|_| de::Error::custom("aware_timestamp_required"))?;
        if !dt.year_ce().0 || dt.year_ce().1 > 9999 || dt.nanosecond() >= 1_000_000_000 {
            return Err(de::Error::custom("timestamp_range"));
        }
        Ok(Self(
            dt.with_nanosecond(dt.nanosecond() / 1000 * 1000)
                .ok_or_else(|| de::Error::custom("timestamp_range"))?,
        ))
    }
}
use chrono::Datelike;
impl Serialize for Stamp {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text(true))
    }
}
fn media() -> String {
    "text/plain; charset=utf-8".into()
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CaptureFields {
    source_uri: String,
    #[serde(default = "media")]
    media_type: String,
    body: String,
    requested_at: Stamp,
    received_at: Stamp,
    #[serde(default)]
    published_at: Option<Stamp>,
}
fn digest(raw: &[u8]) -> String {
    Sha256::digest(raw)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl CaptureFields {
    fn valid(&self) -> bool {
        let Ok(uri) = url::Url::parse(&self.source_uri) else {
            return false;
        };
        (1..=2048).contains(&self.source_uri.chars().count())
            && self.source_uri.starts_with("https://")
            && !self.source_uri[8..]
                .split('/')
                .next()
                .unwrap_or("")
                .is_empty()
            && !self.source_uri.contains('\\')
            && !self.source_uri.chars().any(|c| c as u32 <= 32)
            && uri.scheme() == "https"
            && uri.host_str().is_some()
            && uri.username().is_empty()
            && uri.password().is_none()
            && uri.query().is_none_or(str::is_empty)
            && uri.fragment().is_none_or(str::is_empty)
            && self.media_type == media()
            && (1..=12000).contains(&self.body.chars().count())
            && self.body.len() <= 48000
            && self.requested_at.0 <= self.received_at.0
            && self
                .published_at
                .as_ref()
                .is_none_or(|p| p.0 <= self.received_at.0)
    }
    fn canonical(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|_| Error)
    }
}
pub struct Capture {
    received_at_micros: i64,
    original: Vec<u8>,
    canonical: Vec<u8>,
    hash: String,
    document_hash: String,
}
impl fmt::Debug for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchEvidenceCapture(redacted, no authority)")
    }
}
impl Capture {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 256 * 1024 {
            return Err(Error);
        }
        let fields: CaptureFields =
            serde_json::from_value(crate::native::decode(raw).map_err(|_| Error)?)
                .map_err(|_| Error)?;
        if !fields.valid() {
            return Err(Error);
        }
        let canonical = fields.canonical()?;
        #[derive(Serialize)]
        struct Document<'a> {
            source_uri: &'a str,
            media_type: &'a str,
            body: &'a str,
            published_at: &'a Option<Stamp>,
        }
        let doc = Document {
            source_uri: &fields.source_uri,
            media_type: &fields.media_type,
            body: &fields.body,
            published_at: &fields.published_at,
        };
        let document_hash = digest(&serde_json::to_vec(&doc).map_err(|_| Error)?);
        Ok(Self {
            received_at_micros: fields.received_at.0.timestamp_micros(),
            original: raw.to_vec(),
            hash: digest(&canonical),
            canonical,
            document_hash,
        })
    }
    pub fn received_at_micros(&self) -> i64 {
        self.received_at_micros
    }
    pub fn original_json(&self) -> &[u8] {
        &self.original
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn hash(&self) -> &str {
        &self.hash
    }
    pub fn document_hash(&self) -> &str {
        &self.document_hash
    }
    pub fn source_authenticated(&self) -> bool {
        false
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceFields {
    source_id: String,
    text: String,
    available_at: Stamp,
    #[serde(default)]
    capture: Option<CaptureFields>,
}
/// Historical evidence declaration. Uncaptured text is structurally valid but
/// does not authenticate a source or satisfy a captured-evidence runtime gate.
#[derive(Deserialize)]
#[serde(try_from = "EvidenceFields")]
pub struct Evidence(EvidenceFields);
impl TryFrom<EvidenceFields> for Evidence {
    type Error = Error;
    fn try_from(fields: EvidenceFields) -> Result<Self, Error> {
        if fields.text.chars().count() > 12000 {
            return Err(Error);
        }
        if let Some(c) = &fields.capture
            && (!c.valid()
                || digest(&c.canonical()?) != fields.source_id
                || c.body != fields.text
                || c.received_at != fields.available_at)
        {
            return Err(Error);
        }
        Ok(Self(fields))
    }
}
impl fmt::Debug for Evidence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Evidence(redacted, no source authentication)")
    }
}
impl Evidence {
    pub fn source_id(&self) -> &str {
        &self.0.source_id
    }
    pub fn text(&self) -> &str {
        &self.0.text
    }
    pub fn captured(&self) -> bool {
        self.0.capture.is_some()
    }
    pub fn source_authenticated(&self) -> bool {
        false
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolFields {
    evidence: Vec<EvidenceFields>,
}
pub struct EvidenceTools {
    evidence: Vec<EvidenceFields>,
}
impl fmt::Debug for EvidenceTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchEvidenceTools(redacted, no authority)")
    }
}
impl EvidenceTools {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 256 * 1024 {
            return Err(Error);
        }
        let f: ToolFields = serde_json::from_value(crate::native::decode(raw).map_err(|_| Error)?)
            .map_err(|_| Error)?;
        if !(1..=8).contains(&f.evidence.len())
            || f.evidence
                .iter()
                .map(|e| &e.source_id)
                .collect::<BTreeSet<_>>()
                .len()
                != f.evidence.len()
            || f.evidence
                .iter()
                .map(|e| e.text.chars().count())
                .sum::<usize>()
                > 16000
        {
            return Err(Error);
        }
        for e in &f.evidence {
            let c = e.capture.as_ref().ok_or(Error)?;
            if !c.valid()
                || digest(&c.canonical()?) != e.source_id
                || c.body != e.text
                || c.received_at != e.available_at
            {
                return Err(Error);
            }
        }
        Ok(Self {
            evidence: f.evidence,
        })
    }
    pub fn available(&self, now: &str) -> Result<(), Error> {
        let now: Stamp = serde_json::from_value(json!(now)).map_err(|_| Error)?;
        if self.evidence.iter().any(|e| e.available_at.0 > now.0) {
            return Err(Error);
        }
        Ok(())
    }
    pub fn catalog(&self) -> Value {
        json!(self.evidence.iter().map(|e|json!({"source_id":e.source_id,"characters":e.text.chars().count(),"available_at":e.available_at.text(false)})).collect::<Vec<_>>())
    }
    pub fn read(&self, ids: &[&str]) -> Result<Value, Error> {
        if ids.len() > 3 || ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err(Error);
        }
        let mut result = Vec::new();
        for id in ids {
            let e = self
                .evidence
                .iter()
                .find(|e| e.source_id == *id)
                .ok_or(Error)?;
            result.push(
                json!({"source_id":id,"text":e.text,"available_at":e.available_at.text(false)}),
            );
        }
        Ok(json!(result))
    }
}
#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Action {
    Read,
    Abstain,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestFields {
    action: Action,
    source_ids: Vec<String>,
}
pub struct EvidenceRequest(RequestFields);
impl fmt::Debug for EvidenceRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResearchEvidenceRequest(no authority)")
    }
}
impl EvidenceRequest {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 8192 {
            return Err(Error);
        }
        let f: RequestFields =
            serde_json::from_value(crate::native::decode(raw).map_err(|_| Error)?)
                .map_err(|_| Error)?;
        if f.source_ids.len() > 3
            || f.source_ids.iter().collect::<BTreeSet<_>>().len() != f.source_ids.len()
            || (f.action == Action::Read) == f.source_ids.is_empty()
        {
            return Err(Error);
        }
        Ok(Self(f))
    }
    pub fn source_ids(&self) -> &[String] {
        &self.0.source_ids
    }
}
