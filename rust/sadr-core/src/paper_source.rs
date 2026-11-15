//! Declared source and native-instrument contracts only. No clients or credentials.
use crate::market::Instrument;
use serde::Deserialize;
use std::{collections::BTreeMap, fmt};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("paper_source_invalid")
    }
}
impl std::error::Error for Error {}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum Source {
    #[serde(rename = "upstox-readonly-v1")]
    Upstox,
    #[serde(rename = "kotak-readonly-v1")]
    Kotak,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeMapping {
    broker: String,
    canonical_key: String,
    instrument_version: String,
    segment: String,
    token: String,
    symbol: String,
    master_version: String,
}
impl NativeMapping {
    pub fn broker(&self) -> &str {
        &self.broker
    }
    pub fn canonical_key(&self) -> &str {
        &self.canonical_key
    }
    pub fn instrument_version(&self) -> &str {
        &self.instrument_version
    }
    pub fn segment(&self) -> &str {
        &self.segment
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    pub fn symbol(&self) -> &str {
        &self.symbol
    }
    pub fn master_version(&self) -> &str {
        &self.master_version
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    source: Source,
    instrument: Instrument,
    #[serde(default)]
    native_mapping: Option<NativeMapping>,
    #[serde(default)]
    timestamp_reference: Option<String>,
    review_ref: String,
}
pub struct PaperSourceBinding(Fields);
impl fmt::Debug for PaperSourceBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PaperSourceBinding(declared, no source authentication)")
    }
}
fn text(s: &str) -> bool {
    (1..=256).contains(&s.chars().count())
        && !s
            .trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
            .is_empty()
}
impl PaperSourceBinding {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        let f: Fields = serde_json::from_value(crate::native::decode(raw).map_err(|_| Error)?)
            .map_err(|_| Error)?;
        if f.instrument.synthetic()
            || f.instrument.validate_paper_execution().is_err()
            || !text(&f.review_ref)
        {
            return Err(Error);
        }
        match f.source {
            Source::Upstox if f.native_mapping.is_some() || f.timestamp_reference.is_some() => {
                return Err(Error);
            }
            Source::Kotak => {
                let m = f.native_mapping.as_ref().ok_or(Error)?;
                if !f.timestamp_reference.as_ref().is_some_and(|r| text(r))
                    || m.broker != "kotak"
                    || m.canonical_key != f.instrument.key()
                    || m.instrument_version != f.instrument.version()
                    || m.symbol != f.instrument.symbol()
                    || m.symbol.is_empty()
                    || !matches!(m.segment.as_str(), "nse_cm" | "bse_cm")
                    || !(1..=20).contains(&m.token.len())
                    || !m.token.bytes().all(|b| b.is_ascii_digit())
                    || m.master_version.is_empty()
                {
                    return Err(Error);
                }
            }
            Source::Upstox => {}
        }
        Ok(Self(f))
    }
    pub fn source(&self) -> Source {
        self.0.source
    }
    pub fn instrument(&self) -> &Instrument {
        &self.0.instrument
    }
    pub fn native_mapping(&self) -> Option<&NativeMapping> {
        self.0.native_mapping.as_ref()
    }
    pub fn review_ref(&self) -> &str {
        &self.0.review_ref
    }
    pub fn timestamp_reference(&self) -> Option<&str> {
        self.0.timestamp_reference.as_deref()
    }
    pub fn quote_params(&self) -> BTreeMap<String, String> {
        if self.source() == Source::Kotak {
            BTreeMap::new()
        } else {
            BTreeMap::from([("instrument_key".into(), self.instrument().key().into())])
        }
    }
    /// Compatibility check on caller declarations, not authentication of a client.
    pub fn reader_matches(&self, reader: ReaderDeclaration<'_>, origin: &str) -> bool {
        match (self.source(), reader) {
            (Source::Kotak, ReaderDeclaration::Kotak { origin: actual }) => actual == origin,
            (
                Source::Upstox,
                ReaderDeclaration::Upstox {
                    base_url,
                    max_attempts,
                },
            ) => origin == "https://api.upstox.com" && base_url == origin && max_attempts == 1,
            _ => false,
        }
    }
}
pub enum ReaderDeclaration<'a> {
    Upstox {
        base_url: &'a str,
        max_attempts: u32,
    },
    Kotak {
        origin: &'a str,
    },
    Other,
}
