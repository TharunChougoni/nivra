//! Bounded composition of original quote and candle captures. This is a source
//! linkage declaration, not provider authentication or normalized market input.
use crate::{market::Instrument, market_capture::MarketCapture, paper_source::NativeMapping};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("captured_market_source_invalid")
    }
}
impl std::error::Error for Error {}

pub struct CapturedMarketSource {
    instrument: Instrument,
    quotes: MarketCapture,
    candles: Vec<MarketCapture>,
    native_mapping: Option<NativeMapping>,
    timestamp_reference: Option<String>,
}

impl fmt::Debug for CapturedMarketSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CapturedMarketSource(bounded declarations, provider unauthenticated)")
    }
}

impl CapturedMarketSource {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 600_000 {
            return Err(Error);
        }
        let value = crate::native::decode_bounded(raw, 600_000).map_err(|_| Error)?;
        let object = value.as_object().ok_or(Error)?;
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "instrument" | "quotes" | "candles" | "native_mapping" | "timestamp_reference"
            )
        }) {
            return Err(Error);
        }
        let instrument: Instrument =
            serde_json::from_value(object.get("instrument").cloned().ok_or(Error)?)
                .map_err(|_| Error)?;
        let quotes = parse_capture(object.get("quotes").ok_or(Error)?)?;
        let candle_values = object
            .get("candles")
            .and_then(Value::as_array)
            .filter(|items| (1..=16).contains(&items.len()))
            .ok_or(Error)?;
        let candles = candle_values
            .iter()
            .map(parse_capture)
            .collect::<Result<Vec<_>, _>>()?;
        let native_mapping = object
            .get("native_mapping")
            .filter(|value| !value.is_null())
            .map(|value| serde_json::from_value::<NativeMapping>(value.clone()).map_err(|_| Error))
            .transpose()?;
        let timestamp_reference = object
            .get("timestamp_reference")
            .filter(|value| !value.is_null())
            .and_then(Value::as_str)
            .map(str::to_owned);
        if instrument.synthetic()
            || instrument.validate_paper_execution().is_err()
            || candles
                .iter()
                .any(|capture| capture.source() != quotes.source())
            || !capture_set_unique(&candles)?
            || candles
                .iter()
                .any(|capture| capture.received_at() > quotes.received_at())
            || candles
                .iter()
                .map(|capture| capture.body().len())
                .sum::<usize>()
                + quotes.body().len()
                > 600_000
        {
            return Err(Error);
        }
        match quotes.source() {
            "upstox-readonly-v1" => {
                if native_mapping.is_some()
                    || timestamp_reference.is_some()
                    || quotes.path() != "/v2/market-quote/quotes"
                    || quotes.params().len() != 1
                    || quotes.params().get("instrument_key").map(String::as_str)
                        != Some(instrument.key())
                {
                    return Err(Error);
                }
            }
            "kotak-readonly-v1" => {
                let mapping = native_mapping.as_ref().ok_or(Error)?;
                let reference = timestamp_reference.as_deref().ok_or(Error)?;
                if mapping.broker() != "kotak"
                    || mapping.canonical_key() != instrument.key()
                    || mapping.instrument_version() != instrument.version()
                    || mapping.symbol() != instrument.symbol()
                    || mapping.master_version().trim().is_empty()
                    || !matches!(mapping.segment(), "nse_cm" | "bse_cm")
                    || !digits(mapping.token(), 1, 20)
                    || reference.trim().is_empty()
                    || reference.chars().count() > 256
                    || quotes.path()
                        != format!(
                            "/script-details/1.0/quotes/neosymbol/{}|{}/all",
                            mapping.segment(),
                            mapping.token()
                        )
                    || !quotes.params().is_empty()
                {
                    return Err(Error);
                }
            }
            _ => return Err(Error),
        }
        Ok(Self {
            instrument,
            quotes,
            candles,
            native_mapping,
            timestamp_reference,
        })
    }

    pub fn instrument(&self) -> &Instrument {
        &self.instrument
    }
    pub fn quotes(&self) -> &MarketCapture {
        &self.quotes
    }
    pub fn candles(&self) -> &[MarketCapture] {
        &self.candles
    }
    pub fn native_mapping(&self) -> Option<&NativeMapping> {
        self.native_mapping.as_ref()
    }
    pub fn timestamp_reference(&self) -> Option<&str> {
        self.timestamp_reference.as_deref()
    }
}

fn parse_capture(value: &Value) -> Result<MarketCapture, Error> {
    MarketCapture::parse(&serde_json::to_vec(value).map_err(|_| Error)?).map_err(|_| Error)
}

fn capture_set_unique(captures: &[MarketCapture]) -> Result<bool, Error> {
    let mut ids = BTreeSet::new();
    for capture in captures {
        let bytes = capture.model_dump_json().map_err(|_| Error)?;
        let id = Sha256::digest(bytes.as_bytes());
        if !ids.insert(id.to_vec()) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn digits(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}
