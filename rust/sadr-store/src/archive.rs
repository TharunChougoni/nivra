//! Pure, bounded rederivation of legacy Upstox cash-quote captures.
//!
//! Exact capture bytes and Python data hashes establish consistency, not provider
//! authentication, original-file continuity, calendar readiness or trading authority.
//! Canonical capture timestamps support RFC3339 whole-minute offsets and at most
//! six fractional digits. Lone Unicode surrogates, oversized numeral tokens or
//! exponents, and timestamps outside the documented exact range fail closed.

use crate::StoreError;
use sadr_core::{
    finance::{Amount, Price},
    market::{AccountSource, ArchivedQuoteReceipt, Calendar, Instrument, Snapshot},
    time::Timestamp,
};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
};

const MAX_CAPTURE_BYTES: usize = 4_100_000;
const MAX_BODY_BYTES: usize = 2_000_000;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 64;
const MAX_NUMBER_BYTES: usize = 128;
const MAX_HASH_JSON_BYTES: usize = 16 * 1024 * 1024;
const QUOTE_PATH: &str = "/v2/market-quote/quotes";
const ARCHIVE_SOURCE: &str = "upstox-readonly-v1";
const QUOTE_SOURCE: &str = "upstox-full-quotes-v2";

#[derive(Debug, Clone)]
pub struct LegacyUpstoxCapture {
    original_json: String,
    original_hash: String,
    instrument_keys: String,
    requested_at: Timestamp,
    received_at: Timestamp,
    received_python_iso: String,
    body: String,
    data: Json,
}

impl LegacyUpstoxCapture {
    pub fn parse(original_json: String, original_hash: String) -> Result<Self, StoreError> {
        if original_json.len() > MAX_CAPTURE_BYTES {
            return Err(StoreError::Bound);
        }
        if !valid_hash(&original_hash) || sha256(&original_json) != original_hash {
            return Err(StoreError::ArchiveHash);
        }
        let outer = Parser::parse(&original_json)?;
        let fields = outer.object()?;
        if fields.len() != 6
            || string_field(fields, "source")? != ARCHIVE_SOURCE
            || string_field(fields, "path")? != QUOTE_PATH
        {
            return Err(StoreError::Archive);
        }
        let params = field(fields, "params")?.object()?;
        if params.len() != 1 {
            return Err(StoreError::Archive);
        }
        let instrument_keys = string_field(params, "instrument_key")?;
        if instrument_keys.is_empty() || instrument_keys.chars().count() > 20_000 {
            return Err(StoreError::Bound);
        }
        let requested_raw = string_field(fields, "requested_at")?;
        let received_raw = string_field(fields, "received_at")?;
        let (requested_at, requested_python_iso, requested_canonical) =
            capture_time(requested_raw)?;
        let (received_at, received_python_iso, received_canonical) = capture_time(received_raw)?;
        if requested_at > received_at {
            return Err(StoreError::Archive);
        }
        let body = string_field(fields, "body")?;
        if body.is_empty() || body.len() > MAX_BODY_BYTES {
            return Err(StoreError::Bound);
        }
        // Exact field order, compact UTF-8 strings and Pydantic datetime spelling.
        // Rehashing semantically equivalent but noncanonical input is insufficient.
        #[derive(Serialize)]
        struct CanonicalCapture<'a> {
            source: &'a str,
            path: &'a str,
            params: BTreeMap<&'a str, &'a str>,
            requested_at: &'a str,
            received_at: &'a str,
            body: &'a str,
        }
        let canonical = serde_json::to_string(&CanonicalCapture {
            source: ARCHIVE_SOURCE,
            path: QUOTE_PATH,
            params: BTreeMap::from([("instrument_key", instrument_keys)]),
            requested_at: &requested_canonical,
            received_at: &received_canonical,
            body,
        })
        .map_err(|_| StoreError::Archive)?;
        if canonical != original_json {
            return Err(StoreError::Archive);
        }
        let parsed_body = Parser::parse(body)?;
        let envelope = parsed_body.object()?;
        if string_field(envelope, "status")? != "success" {
            return Err(StoreError::Archive);
        }
        let data = field(envelope, "data")?.clone();
        let result = Self {
            original_json: original_json.clone(),
            original_hash,
            instrument_keys: instrument_keys.to_owned(),
            requested_at,
            received_at,
            received_python_iso,
            body: body.to_owned(),
            data,
        };
        // The requested offset is intentionally validated even though only the
        // received offset participates in the legacy snapshot identifier.
        drop(requested_python_iso);
        Ok(result)
    }

    pub fn original_json(&self) -> &str {
        &self.original_json
    }
    pub fn original_hash(&self) -> &str {
        &self.original_hash
    }
    pub fn requested_at(&self) -> Timestamp {
        self.requested_at
    }
    pub fn received_at(&self) -> Timestamp {
        self.received_at
    }
    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn derive_receipt(
        &self,
        instruments: &[Instrument],
        identity: &AccountSource,
        calendar: &Calendar,
    ) -> Result<ArchivedQuoteReceipt, StoreError> {
        if !(1..=500).contains(&instruments.len())
            || identity.broker() != "upstox"
            || identity.archive_source() != ARCHIVE_SOURCE
            || identity.quote_source() != QUOTE_SOURCE
        {
            return Err(StoreError::Quote);
        }
        let mut keys = BTreeSet::new();
        for instrument in instruments {
            if instrument.synthetic()
                || instrument.validate_paper_execution().is_err()
                || !keys.insert(instrument.key())
            {
                return Err(StoreError::Quote);
            }
            calendar
                .require_instrument(instrument)
                .map_err(|_| StoreError::Quote)?;
        }
        if instruments
            .iter()
            .map(Instrument::key)
            .collect::<Vec<_>>()
            .join(",")
            != self.instrument_keys
        {
            return Err(StoreError::Quote);
        }
        let data = self.data.object().map_err(|_| StoreError::Quote)?;
        let mut rows = BTreeMap::new();
        for row in data.values() {
            let row = row.object().map_err(|_| StoreError::Quote)?;
            let key = string_field(row, "instrument_token").map_err(|_| StoreError::Quote)?;
            if rows.insert(key, row).is_some() {
                return Err(StoreError::Quote);
            }
        }
        if rows.len() != instruments.len() || rows.keys().copied().collect::<BTreeSet<_>>() != keys
        {
            return Err(StoreError::Quote);
        }
        let mut quotes = Vec::with_capacity(instruments.len());
        for instrument in instruments {
            let row = rows.get(instrument.key()).ok_or(StoreError::Quote)?;
            let depth = field(row, "depth")?
                .object()
                .map_err(|_| StoreError::Quote)?;
            let bid = best_depth(field(depth, "buy")?, true)?;
            let ask = best_depth(field(depth, "sell")?, false)?;
            let source_at = source_time(field(row, "timestamp")?)?;
            // Validated core serde constructors enforce crossed/future-source
            // rejection while preserving all provided instrument metadata.
            quotes.push(json!({
                "instrument": instrument, "bid": bid.0.amount(), "ask": ask.0.amount(),
                "bid_units": bid.1, "ask_units": ask.1,
                "source_at": source_at, "received_at": self.received_at
            }));
        }
        let mut python_data = String::new();
        python_json(&self.data, &mut python_data)?;
        let payload_hash = sha256(&python_data);
        let snapshot_id = sha256(&(payload_hash.clone() + &self.received_python_iso));
        let snapshot: Snapshot = serde_json::from_value(json!({
            "snapshot_id": snapshot_id, "captured_at": self.received_at,
            "payload_hash": payload_hash, "quotes": quotes, "source": QUOTE_SOURCE
        }))
        .map_err(|_| StoreError::Quote)?;
        serde_json::from_value(json!({
            "capture_id": self.original_hash, "identity": identity,
            "requested_at": self.requested_at, "received_at": self.received_at,
            "normalized_snapshot": snapshot
        }))
        .map_err(|_| StoreError::Quote)
    }
}

fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn sha256(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A bounded tree retains numeric lexemes: Python parse_float=Decimal hashes
/// decimal JSON numbers as strings, unlike integer JSON numbers.
#[derive(Debug, Clone)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}
impl Json {
    fn object(&self) -> Result<&BTreeMap<String, Json>, StoreError> {
        match self {
            Self::Object(value) => Ok(value),
            _ => Err(StoreError::Archive),
        }
    }
    fn string(&self) -> Result<&str, StoreError> {
        match self {
            Self::String(value) => Ok(value),
            _ => Err(StoreError::Archive),
        }
    }
}
fn field<'a>(object: &'a BTreeMap<String, Json>, key: &str) -> Result<&'a Json, StoreError> {
    object.get(key).ok_or(StoreError::Archive)
}
fn string_field<'a>(object: &'a BTreeMap<String, Json>, key: &str) -> Result<&'a str, StoreError> {
    field(object, key)?.string()
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
    nodes: usize,
}
impl<'a> Parser<'a> {
    fn parse(input: &'a str) -> Result<Json, StoreError> {
        let mut parser = Self {
            bytes: input.as_bytes(),
            at: 0,
            nodes: 0,
        };
        let value = parser.value(0)?;
        parser.whitespace();
        if parser.at != parser.bytes.len() {
            return Err(StoreError::Archive);
        }
        Ok(value)
    }
    fn whitespace(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\n' | b'\r' | b'\t'))
        {
            self.at += 1;
        }
    }
    fn consume(&mut self, byte: u8) -> bool {
        self.whitespace();
        if self.bytes.get(self.at) == Some(&byte) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn value(&mut self, depth: usize) -> Result<Json, StoreError> {
        self.nodes += 1;
        if depth > MAX_DEPTH || self.nodes > MAX_NODES {
            return Err(StoreError::Bound);
        }
        self.whitespace();
        match self.bytes.get(self.at).copied() {
            Some(b'"') => Ok(Json::String(self.string()?)),
            Some(b'{') => {
                self.at += 1;
                let mut object = BTreeMap::new();
                if self.consume(b'}') {
                    return Ok(Json::Object(object));
                }
                loop {
                    self.whitespace();
                    let key = self.string()?;
                    if object.contains_key(&key) || !self.consume(b':') {
                        return Err(StoreError::Archive);
                    }
                    object.insert(key, self.value(depth + 1)?);
                    if self.consume(b'}') {
                        break;
                    }
                    if !self.consume(b',') {
                        return Err(StoreError::Archive);
                    }
                }
                Ok(Json::Object(object))
            }
            Some(b'[') => {
                self.at += 1;
                let mut array = Vec::new();
                if self.consume(b']') {
                    return Ok(Json::Array(array));
                }
                loop {
                    array.push(self.value(depth + 1)?);
                    if self.consume(b']') {
                        break;
                    }
                    if !self.consume(b',') {
                        return Err(StoreError::Archive);
                    }
                }
                Ok(Json::Array(array))
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(Json::Null)
            }
            Some(b'-' | b'0'..=b'9') => Ok(Json::Number(self.number()?)),
            _ => Err(StoreError::Archive),
        }
    }
    fn literal(&mut self, text: &[u8]) -> Result<(), StoreError> {
        if self.bytes.get(self.at..self.at + text.len()) != Some(text) {
            return Err(StoreError::Archive);
        }
        self.at += text.len();
        Ok(())
    }
    fn string(&mut self) -> Result<String, StoreError> {
        if self.bytes.get(self.at) != Some(&b'"') {
            return Err(StoreError::Archive);
        }
        let start = self.at;
        self.at += 1;
        while let Some(byte) = self.bytes.get(self.at).copied() {
            self.at += 1;
            match byte {
                b'"' => {
                    return serde_json::from_slice(&self.bytes[start..self.at])
                        .map_err(|_| StoreError::Archive);
                }
                b'\\' => {
                    if self.at >= self.bytes.len() {
                        return Err(StoreError::Archive);
                    }
                    self.at += 1;
                }
                0..=31 => return Err(StoreError::Archive),
                _ => {}
            }
        }
        Err(StoreError::Archive)
    }
    fn number(&mut self) -> Result<String, StoreError> {
        let start = self.at;
        if self.bytes.get(self.at) == Some(&b'-') {
            self.at += 1;
        }
        match self.bytes.get(self.at) {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.at += 1;
                self.digits();
            }
            _ => return Err(StoreError::Archive),
        }
        if self.bytes.get(self.at) == Some(&b'.') {
            self.at += 1;
            let before = self.at;
            self.digits();
            if self.at == before {
                return Err(StoreError::Archive);
            }
        }
        if self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b'e' | b'E'))
        {
            self.at += 1;
            if self
                .bytes
                .get(self.at)
                .is_some_and(|b| matches!(b, b'+' | b'-'))
            {
                self.at += 1;
            }
            let before = self.at;
            self.digits();
            if self.at == before {
                return Err(StoreError::Archive);
            }
        }
        if self.at - start > MAX_NUMBER_BYTES {
            return Err(StoreError::Bound);
        }
        let token = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| StoreError::Archive)?
            .to_owned();
        NumberParts::parse(&token)?;
        Ok(token)
    }
    fn digits(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
    }
}

struct NumberParts {
    negative: bool,
    digits: String,
    exponent: i32,
    decimal: bool,
}
impl NumberParts {
    fn parse(raw: &str) -> Result<Self, StoreError> {
        if raw.len() > MAX_NUMBER_BYTES {
            return Err(StoreError::Bound);
        }
        let negative = raw.starts_with('-');
        let unsigned = raw.strip_prefix('-').unwrap_or(raw);
        let (base, explicit) = if let Some(index) = unsigned.find(['e', 'E']) {
            let exponent = unsigned[index + 1..]
                .parse::<i32>()
                .map_err(|_| StoreError::Bound)?;
            if !(-128..=128).contains(&exponent) {
                return Err(StoreError::Bound);
            }
            (&unsigned[..index], exponent)
        } else {
            (unsigned, 0)
        };
        let fraction = base
            .split_once('.')
            .map_or(0, |(_, fraction)| fraction.len());
        let raw_digits: String = base.chars().filter(|c| *c != '.').collect();
        let digits = raw_digits.trim_start_matches('0');
        Ok(Self {
            negative,
            digits: if digits.is_empty() {
                "0".into()
            } else {
                digits.into()
            },
            exponent: explicit - fraction as i32,
            decimal: raw.contains(['.', 'e', 'E']),
        })
    }
    fn python_string(&self) -> String {
        if !self.decimal {
            return format!(
                "{}{}",
                if self.negative && self.digits != "0" {
                    "-"
                } else {
                    ""
                },
                self.digits
            );
        }
        let adjusted = self.exponent + self.digits.len() as i32 - 1;
        let sign = if self.negative { "-" } else { "" };
        if self.exponent > 0 || adjusted < -6 {
            let significand = if self.digits.len() == 1 {
                self.digits.clone()
            } else {
                format!("{}.{}", &self.digits[..1], &self.digits[1..])
            };
            format!("{sign}{significand}E{adjusted:+}")
        } else if self.exponent == 0 {
            format!("{sign}{}", self.digits)
        } else {
            let point = self.digits.len() as i32 + self.exponent;
            if point <= 0 {
                format!("{sign}0.{}{}", "0".repeat((-point) as usize), self.digits)
            } else {
                format!(
                    "{sign}{}.{}",
                    &self.digits[..point as usize],
                    &self.digits[point as usize..]
                )
            }
        }
    }
    fn integer_text(&self) -> Result<String, StoreError> {
        if self.digits == "0" {
            return Ok("0".to_owned());
        }
        let point = self.digits.len() as i32 + self.exponent;
        let digits = if point <= 0 {
            "0".to_owned()
        } else if point < self.digits.len() as i32 {
            self.digits[..point as usize].to_owned()
        } else {
            if point > 19 {
                return Err(StoreError::Quote);
            }
            format!(
                "{}{}",
                self.digits,
                "0".repeat((point as usize) - self.digits.len())
            )
        };
        Ok(format!(
            "{}{}",
            if self.negative && digits != "0" {
                "-"
            } else {
                ""
            },
            digits
        ))
    }
}

fn best_depth(value: &Json, buy: bool) -> Result<(Price, u64), StoreError> {
    let Json::Array(levels) = value else {
        return Err(StoreError::Quote);
    };
    let mut best: Option<(Price, u64)> = None;
    for level in levels {
        let level = level.object().map_err(|_| StoreError::Quote)?;
        let Json::Number(price_token) = field(level, "price")? else {
            return Err(StoreError::Quote);
        };
        let price = Amount::parse(price_token).map_err(|_| StoreError::Arithmetic)?;
        if price < Amount::ZERO {
            return Err(StoreError::Quote);
        }
        let Json::Number(quantity) = field(level, "quantity")? else {
            return Err(StoreError::Quote);
        };
        let parts = NumberParts::parse(quantity)?;
        if parts.decimal || (parts.negative && parts.digits != "0") {
            return Err(StoreError::Quote);
        }
        let units = parts.digits.parse::<u64>().map_err(|_| StoreError::Quote)?;
        if price == Amount::ZERO || units == 0 {
            continue;
        }
        let price = Price::try_from(price).map_err(|_| StoreError::Quote)?;
        if best.is_none_or(|(old, _)| if buy { price > old } else { price < old }) {
            best = Some((price, units));
        }
    }
    best.ok_or(StoreError::Quote)
}

fn source_time(value: &Json) -> Result<Timestamp, StoreError> {
    let millis = match value {
        Json::String(value) if value.contains('T') => {
            return Timestamp::parse(value).map_err(|_| StoreError::Quote);
        }
        Json::String(value) => {
            let value = value.trim_matches(|c: char| c.is_ascii_whitespace());
            let unsigned = value.strip_prefix(['+', '-']).unwrap_or(value);
            if unsigned.is_empty() || !unsigned.bytes().all(|b| b.is_ascii_digit()) {
                return Err(StoreError::Quote);
            }
            value.parse::<i64>().map_err(|_| StoreError::Quote)?
        }
        Json::Number(value) => NumberParts::parse(value)?
            .integer_text()?
            .parse::<i64>()
            .map_err(|_| StoreError::Quote)?,
        _ => return Err(StoreError::Quote),
    };
    // Below 2^32 seconds, Python int(millis)/1000 binary64 conversion rounds
    // back to the same datetime microseconds as this exact integer conversion.
    const LIMIT: i64 = 4_294_967_296_000;
    if millis <= -LIMIT || millis >= LIMIT {
        return Err(StoreError::Quote);
    }
    Timestamp::from_epoch_millis(millis).map_err(|_| StoreError::Quote)
}

fn capture_time(raw: &str) -> Result<(Timestamp, String, String), StoreError> {
    let stamp = Timestamp::parse(raw).map_err(|_| StoreError::Archive)?;
    // Keep offset wall time for Python's snapshot hash. Timestamp itself stores UTC.
    let (base, offset) = if let Some(base) = raw.strip_suffix('Z') {
        (base, "+00:00")
    } else if raw.len() >= 25 {
        let split = raw.len() - 6;
        let offset = &raw[split..];
        if !matches!(offset.as_bytes()[0], b'+' | b'-') || offset.as_bytes()[3] != b':' {
            return Err(StoreError::Archive);
        }
        (
            &raw[..split],
            if offset == "-00:00" { "+00:00" } else { offset },
        )
    } else {
        return Err(StoreError::Archive);
    };
    if base.len() < 19 || base.as_bytes()[10] != b'T' {
        return Err(StoreError::Archive);
    }
    let (whole, fraction) = match base.split_once('.') {
        Some(pair) => pair,
        None => (base, ""),
    };
    if whole.len() != 19 || fraction.len() > 6 || !fraction.bytes().all(|b| b.is_ascii_digit()) {
        return Err(StoreError::Archive);
    }
    let fraction = if fraction.bytes().all(|b| b == b'0') {
        String::new()
    } else {
        format!(".{fraction:0<6}")
    };
    let python = format!("{whole}{fraction}{offset}");
    let canonical = if offset == "+00:00" {
        format!("{whole}{fraction}Z")
    } else {
        python.clone()
    };
    Ok((stamp, python, canonical))
}

fn append(output: &mut String, value: &str) -> Result<(), StoreError> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|len| len > MAX_HASH_JSON_BYTES)
    {
        return Err(StoreError::Bound);
    }
    output.push_str(value);
    Ok(())
}
fn python_string(value: &str, output: &mut String) -> Result<(), StoreError> {
    append(output, "\"")?;
    for character in value.chars() {
        match character {
            '"' => append(output, "\\\"")?,
            '\\' => append(output, "\\\\")?,
            '\u{8}' => append(output, "\\b")?,
            '\u{c}' => append(output, "\\f")?,
            '\n' => append(output, "\\n")?,
            '\r' => append(output, "\\r")?,
            '\t' => append(output, "\\t")?,
            ' '..='~' => output.push(character),
            _ => {
                let code = u32::from(character);
                let mut escaped = String::new();
                if code <= 0xffff {
                    write!(&mut escaped, "\\u{code:04x}").map_err(|_| StoreError::Archive)?;
                } else {
                    let code = code - 0x10000;
                    write!(
                        &mut escaped,
                        "\\u{:04x}\\u{:04x}",
                        0xd800 + (code >> 10),
                        0xdc00 + (code & 0x3ff)
                    )
                    .map_err(|_| StoreError::Archive)?;
                }
                append(output, &escaped)?;
            }
        }
        if output.len() > MAX_HASH_JSON_BYTES {
            return Err(StoreError::Bound);
        }
    }
    append(output, "\"")
}
fn python_json(value: &Json, output: &mut String) -> Result<(), StoreError> {
    match value {
        Json::Null => append(output, "null"),
        Json::Bool(true) => append(output, "true"),
        Json::Bool(false) => append(output, "false"),
        Json::String(value) => python_string(value, output),
        Json::Number(value) => {
            let parts = NumberParts::parse(value)?;
            if parts.decimal {
                python_string(&parts.python_string(), output)
            } else {
                append(output, &parts.python_string())
            }
        }
        Json::Array(values) => {
            append(output, "[")?;
            for (index, value) in values.iter().enumerate() {
                if index != 0 {
                    append(output, ", ")?;
                }
                python_json(value, output)?;
            }
            append(output, "]")
        }
        Json::Object(values) => {
            append(output, "{")?;
            for (index, (key, value)) in values.iter().enumerate() {
                if index != 0 {
                    append(output, ", ")?;
                }
                python_string(key, output)?;
                append(output, ": ")?;
                python_json(value, output)?;
            }
            append(output, "}")
        }
    }
}

/// Canonical Python plain-JSON encoding for persisted reports. Decimal/float
/// lexemes are deliberately unsupported, including in otherwise ignored extras.
pub(crate) fn canonical_plain_json(raw: &str) -> Result<String, StoreError> {
    if raw.len() > MAX_BODY_BYTES {
        return Err(StoreError::Bound);
    }
    fn require_plain(value: &Json) -> Result<(), StoreError> {
        match value {
            Json::Number(token) if NumberParts::parse(token)?.decimal => Err(StoreError::Archive),
            Json::Array(values) => values.iter().try_for_each(require_plain),
            Json::Object(values) => values.values().try_for_each(require_plain),
            _ => Ok(()),
        }
    }
    let parsed = Parser::parse(raw)?;
    require_plain(&parsed)?;
    let mut canonical = String::new();
    python_json(&parsed, &mut canonical)?;
    Ok(canonical)
}
