//! Exact, bounded native historical-read declarations. This validates a request
//! shape; it does not certify the broker API or authenticate a returned capture.
use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize, de, ser, ser::SerializeMap};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use crate::{
    finance::Price,
    market::{Calendar, Instrument, Quote},
    paper_source::{NativeMapping, PaperSourceBinding, Source},
    time::{ExchangeDate, Timestamp},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("market_capture_invalid")
    }
}
impl std::error::Error for Error {}

const MAX_BODY_BYTES: usize = 2_000_000;
const MAX_MASTER_BYTES: usize = 20_000_000;
const KOTAK_HISTORY_PATH: &str = "/market-data/1.0/historical/details";
const KOTAK_MASTER_PATH: &str = "/script-details/1.0/masterscrip/file-paths";

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum CaptureSource {
    #[serde(rename = "upstox-readonly-v1")]
    Upstox,
    #[serde(rename = "kotak-readonly-v1")]
    Kotak,
    #[serde(rename = "kotak-master-csv-v1")]
    KotakMaster,
}

struct CaptureFields {
    source: CaptureSource,
    path: String,
    params: BTreeMap<String, String>,
    param_order: Vec<String>,
    requested_at: String,
    received_at: String,
    body: String,
}

#[derive(Default)]
struct OrderedParams(Vec<(String, String)>);

impl<'de> Deserialize<'de> for OrderedParams {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> de::Visitor<'de> for Visitor {
            type Value = OrderedParams;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a bounded string map")
            }

            fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = Vec::new();
                let mut keys = BTreeSet::new();
                while let Some((key, value)) = map.next_entry::<String, String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate parameter"));
                    }
                    entries.push((key, value));
                }
                Ok(OrderedParams(entries))
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureFieldsInput {
    source: CaptureSource,
    path: String,
    #[serde(default)]
    params: OrderedParams,
    requested_at: String,
    received_at: String,
    body: String,
}

impl<'de> Deserialize<'de> for CaptureFields {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = CaptureFieldsInput::deserialize(deserializer)?;
        let param_order = input.params.0.iter().map(|(key, _)| key.clone()).collect();
        let params = input.params.0.into_iter().collect();
        Ok(Self {
            source: input.source,
            path: input.path,
            params,
            param_order,
            requested_at: input.requested_at,
            received_at: input.received_at,
            body: input.body,
        })
    }
}

/// A structurally validated response capture. It retains exact body text but
/// makes no claim that the source or its timestamps are authenticated.
pub struct MarketCapture {
    fields: CaptureFields,
    requested_at: Timestamp,
    received_at: Timestamp,
    data: Value,
    original_json: String,
}

/// One completed interval-start bar rederived from an original Kotak capture.
/// This is a parsing result, not evidence that the declared source is authentic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinuteBar {
    pub instrument_key: String,
    pub timestamp: Timestamp,
    pub available_at: Timestamp,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub volume: u64,
    pub open_interest: Option<u64>,
}

pub type KotakMinuteBar = MinuteBar;

impl fmt::Debug for MarketCapture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MarketCapture(validated declaration, source unauthenticated)")
    }
}

impl MarketCapture {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > MAX_MASTER_BYTES {
            return Err(Error);
        }
        crate::native::decode_bounded(raw, MAX_MASTER_BYTES).map_err(|_| Error)?;
        let fields: CaptureFields = serde_json::from_slice(raw).map_err(|_| Error)?;
        let requested_at = Timestamp::parse(&fields.requested_at).map_err(|_| Error)?;
        let received_at = Timestamp::parse(&fields.received_at).map_err(|_| Error)?;
        let is_master = fields.source == CaptureSource::KotakMaster;
        if fields.path.is_empty()
            || fields.path.len() > 2048
            || fields
                .path
                .bytes()
                .any(|b| matches!(b, b'?' | b'#' | b'@' | b'\\'))
            || fields.body.is_empty()
            || fields.body.len()
                > if is_master {
                    MAX_MASTER_BYTES
                } else {
                    MAX_BODY_BYTES
                }
            || requested_at > received_at
            || fields
                .params
                .values()
                .any(|v| v.is_empty() || v.len() > 20_000)
            || !path_matches(fields.source, &fields.path)
        {
            return Err(Error);
        }
        let historical = fields.source == CaptureSource::Kotak && fields.path == KOTAK_HISTORY_PATH;
        if historical {
            let (first, last) = kotak_minute_window(&fields.params)?;
            if last < first {
                return Err(Error);
            }
        } else if (fields.source != CaptureSource::Upstox && !fields.params.is_empty())
            || (fields.source == CaptureSource::Upstox
                && fields
                    .params
                    .keys()
                    .any(|k| !matches!(k.as_str(), "instrument_key" | "expiry_date")))
        {
            return Err(Error);
        }
        let data = match fields.source {
            CaptureSource::Kotak if historical => kotak_minute_response(fields.body.as_bytes())?,
            CaptureSource::Kotak if fields.path == KOTAK_MASTER_PATH => {
                kotak_master_manifest(fields.body.as_bytes())?
            }
            CaptureSource::Kotak => {
                let rows = kotak_quote_response(fields.body.as_bytes())?;
                Value::Array(rows)
            }
            CaptureSource::Upstox => upstox_response(fields.body.as_bytes())?,
            CaptureSource::KotakMaster => kotak_csv_master(fields.body.as_bytes(), &fields.path)?,
        };
        Ok(Self {
            fields,
            requested_at,
            received_at,
            data,
            original_json: std::str::from_utf8(raw).map_err(|_| Error)?.to_owned(),
        })
    }

    pub fn source(&self) -> &'static str {
        match self.fields.source {
            CaptureSource::Upstox => "upstox-readonly-v1",
            CaptureSource::Kotak => "kotak-readonly-v1",
            CaptureSource::KotakMaster => "kotak-master-csv-v1",
        }
    }
    pub fn path(&self) -> &str {
        &self.fields.path
    }
    pub fn params(&self) -> &BTreeMap<String, String> {
        &self.fields.params
    }
    pub fn requested_at(&self) -> Timestamp {
        self.requested_at
    }
    pub fn received_at(&self) -> Timestamp {
        self.received_at
    }
    pub fn body(&self) -> &str {
        &self.fields.body
    }
    pub fn data(&self) -> &Value {
        &self.data
    }
    /// Exact validated declaration bytes, retained for immutable archive storage.
    pub fn original_json(&self) -> &str {
        &self.original_json
    }

    /// Compact model field order used by Python's capture digest. Parameter
    /// insertion order, source timestamps, and response body are retained.
    pub fn model_dump_json(&self) -> Result<String, Error> {
        struct OrderedMap<'a> {
            map: &'a BTreeMap<String, String>,
            order: &'a [String],
        }
        impl Serialize for OrderedMap<'_> {
            fn serialize<S: ser::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.order.len()))?;
                for key in self.order {
                    let value = self
                        .map
                        .get(key)
                        .ok_or_else(|| ser::Error::custom("capture parameter order mismatch"))?;
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
        #[derive(Serialize)]
        struct Dump<'a> {
            source: &'a str,
            path: &'a str,
            params: OrderedMap<'a>,
            requested_at: &'a str,
            received_at: &'a str,
            body: &'a str,
        }
        serde_json::to_string(&Dump {
            source: self.source(),
            path: &self.fields.path,
            params: OrderedMap {
                map: &self.fields.params,
                order: &self.fields.param_order,
            },
            requested_at: &self.fields.requested_at,
            received_at: &self.fields.received_at,
            body: &self.fields.body,
        })
        .map_err(|_| Error)
    }
}

/// Strict Kotak quote normalization over an already archived response. The
/// returned domain quotes carry no order or activation authority.
pub fn normalize_kotak_snapshot(
    capture: &MarketCapture,
    bindings: &[&PaperSourceBinding],
    calendar: &Calendar,
) -> Result<Vec<Quote>, Error> {
    if bindings.is_empty()
        || bindings.len() > 50
        || capture.source() != "kotak-readonly-v1"
        || !capture.params().is_empty()
        || calendar.synthetic()
        || !calendar.is_open(capture.received_at()).map_err(|_| Error)?
    {
        return Err(Error);
    }
    let mut expected = BTreeMap::<String, (&Instrument, &NativeMapping)>::new();
    let mut path_parts = Vec::with_capacity(bindings.len());
    for binding in bindings {
        if binding.source() != Source::Kotak {
            return Err(Error);
        }
        let instrument = binding.instrument();
        let mapping = binding.native_mapping().ok_or(Error)?;
        if instrument.synthetic()
            || instrument.validate_paper_execution().is_err()
            || calendar.require_instrument(instrument).is_err()
            || expected
                .insert(
                    format!("{}|{}", mapping.segment(), mapping.token()),
                    (instrument, mapping),
                )
                .is_some()
        {
            return Err(Error);
        }
        path_parts.push(format!("{}|{}", mapping.segment(), mapping.token()));
    }
    let expected_path = format!(
        "/script-details/1.0/quotes/neosymbol/{}/all",
        path_parts.join(",")
    );
    if capture.path() != expected_path {
        return Err(Error);
    }
    let rows = capture.data().as_array().ok_or(Error)?;
    if rows.len() != expected.len() {
        return Err(Error);
    }
    let mut returned = BTreeMap::new();
    for row in rows {
        let identity = format!(
            "{}|{}",
            row.get("exchange").and_then(Value::as_str).ok_or(Error)?,
            row.get("exchange_token")
                .and_then(Value::as_str)
                .ok_or(Error)?
        );
        if !expected.contains_key(&identity) || returned.insert(identity, row).is_some() {
            return Err(Error);
        }
    }
    if returned.len() != expected.len() {
        return Err(Error);
    }
    let mut quotes = Vec::with_capacity(expected.len());
    for (identity, (instrument, mapping)) in expected {
        let row = returned.get(&identity).ok_or(Error)?;
        if row.get("display_symbol").and_then(Value::as_str) != Some(mapping.symbol()) {
            return Err(Error);
        }
        let stamp = row.get("lstup_time").and_then(Value::as_str).ok_or(Error)?;
        if !digits(stamp, 1, 11) {
            return Err(Error);
        }
        let source_at = Timestamp::from_epoch_millis(
            stamp
                .parse::<i64>()
                .ok()
                .and_then(|seconds| seconds.checked_mul(1000))
                .ok_or(Error)?,
        )
        .map_err(|_| Error)?;
        if source_at > capture.received_at()
            || capture
                .received_at()
                .elapsed_micros_since(source_at)
                .map_err(|_| Error)?
                > crate::market::MAX_FINANCIAL_AGE_MICROS
            || !calendar.is_open(source_at).map_err(|_| Error)?
        {
            return Err(Error);
        }
        let depth = row.get("depth").and_then(Value::as_object).ok_or(Error)?;
        let bids = kotak_depth_side(depth.get("buy").ok_or(Error)?, instrument)?;
        let asks = kotak_depth_side(depth.get("sell").ok_or(Error)?, instrument)?;
        let (bid, bid_units) = bids
            .into_iter()
            .max_by_key(|(price, _)| *price)
            .ok_or(Error)?;
        let (ask, ask_units) = asks
            .into_iter()
            .min_by_key(|(price, _)| *price)
            .ok_or(Error)?;
        if bid > ask {
            return Err(Error);
        }
        quotes.push(
            Quote::from_parts(
                instrument.clone(),
                bid,
                ask,
                bid_units,
                ask_units,
                source_at,
                capture.received_at(),
            )
            .map_err(|_| Error)?,
        );
    }
    let earliest = quotes.iter().map(Quote::source_at).min().ok_or(Error)?;
    let latest = quotes.iter().map(Quote::source_at).max().ok_or(Error)?;
    if latest.elapsed_micros_since(earliest).map_err(|_| Error)? > 1_000_000 {
        return Err(Error);
    }
    Ok(quotes)
}

fn kotak_depth_side(value: &Value, instrument: &Instrument) -> Result<Vec<(Price, u64)>, Error> {
    let levels = value
        .as_array()
        .filter(|items| (1..=5).contains(&items.len()))
        .ok_or(Error)?;
    let mut prices = BTreeSet::new();
    let mut result = Vec::with_capacity(levels.len());
    for level in levels {
        let price_text = level.get("price").and_then(Value::as_str).ok_or(Error)?;
        let quantity_text = level.get("quantity").and_then(Value::as_str).ok_or(Error)?;
        let orders_text = level.get("orders").and_then(Value::as_str).ok_or(Error)?;
        let mut price_parts = price_text.split('.');
        let whole = price_parts.next().unwrap_or_default();
        let fraction = price_parts.next();
        if !digits(whole, 1, 12)
            || price_parts.next().is_some()
            || fraction.is_some_and(|part| !digits(part, 1, 8))
            || !quantity_text.bytes().all(|byte| byte.is_ascii_digit())
            || quantity_text.is_empty()
            || quantity_text.len() > 12
            || !orders_text.bytes().all(|byte| byte.is_ascii_digit())
            || orders_text.is_empty()
            || orders_text.len() > 12
        {
            return Err(Error);
        }
        let quantity = quantity_text.parse::<u64>().map_err(|_| Error)?;
        let orders = orders_text.parse::<u64>().map_err(|_| Error)?;
        let price = match Price::parse(price_text) {
            Ok(price) => price,
            Err(_) if price_text == "0" && quantity == 0 && orders == 0 => continue,
            Err(_) => return Err(Error),
        };
        if quantity == 0
            || orders == 0
            || !price
                .is_multiple_of(instrument.tick_size())
                .map_err(|_| Error)?
            || !prices.insert(price)
        {
            return Err(Error);
        }
        result.push((price, quantity));
    }
    if result.is_empty() {
        return Err(Error);
    }
    Ok(result)
}

fn digits(value: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn accept_upstox_minute_declaration(
    capture: &MarketCapture,
    instrument_key: &str,
    calendar: &Calendar,
) -> Result<(), Error> {
    if capture.source() != "upstox-readonly-v1"
        || !capture.params().is_empty()
        || instrument_key.is_empty()
        || instrument_key.len() > 256
        || calendar.synthetic()
        || calendar.require_key(instrument_key).is_err()
    {
        Err(Error)
    } else {
        Ok(())
    }
}

/// Reparse one exact archived Upstox current-session minute response. Returned
/// bars are interval-start observations and keep the original receipt as their
/// availability floor; no clock or provider call occurs here.
pub fn normalize_upstox_intraday_capture(
    capture: &MarketCapture,
    instrument_key: &str,
    calendar: &Calendar,
) -> Result<Vec<MinuteBar>, Error> {
    accept_upstox_minute_declaration(capture, instrument_key, calendar)?;
    let encoded = encode_path_component(instrument_key);
    let expected_path = format!("/v3/historical-candle/intraday/{encoded}/minutes/1");
    let day = capture.requested_at().ist_date();
    if capture.path() != expected_path || capture.received_at().ist_date() != day {
        return Err(Error);
    }
    let exchange_day = ExchangeDate::parse(&day.to_string()).map_err(|_| Error)?;
    let (opening, closing) = calendar
        .hours(exchange_day)
        .map_err(|_| Error)?
        .ok_or(Error)?;
    let opening_at = local_timestamp(exchange_day, &opening.to_string())?;
    let closing_at = local_timestamp(exchange_day, &closing.to_string())?;
    let session_micros = closing_at
        .elapsed_micros_since(opening_at)
        .map_err(|_| Error)?;
    // Python rejects any session that is not an exact number of one-minute slots.
    if capture.requested_at() < opening_at
        || session_micros <= 0
        || session_micros % 60_000_000 != 0
    {
        return Err(Error);
    }
    let rows = capture
        .data()
        .get("candles")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= 1000)
        .ok_or(Error)?;
    let day_naive = NaiveDate::parse_from_str(&day.to_string(), "%Y-%m-%d").map_err(|_| Error)?;
    upstox_completed_minutes(
        rows,
        instrument_key,
        calendar,
        day_naive,
        day_naive,
        capture.received_at(),
    )
}

/// Reparse one exact archived Upstox historical minute window. The window must
/// be a past span of fewer than 28 days, and every completed session minute in
/// that span must be present. Availability stays at the original receipt.
pub fn normalize_upstox_historical_capture(
    capture: &MarketCapture,
    instrument_key: &str,
    calendar: &Calendar,
) -> Result<Vec<MinuteBar>, Error> {
    accept_upstox_minute_declaration(capture, instrument_key, calendar)?;
    let encoded = encode_path_component(instrument_key);
    let prefix = format!("/v3/historical-candle/{encoded}/minutes/1/");
    let Some(rest) = capture.path().strip_prefix(&prefix) else {
        return Err(Error);
    };
    let Some((last_text, first_text)) = rest.split_once('/') else {
        return Err(Error);
    };
    if first_text.contains('/') {
        return Err(Error);
    }
    let first = strict_iso_date(first_text)?;
    let last = strict_iso_date(last_text)?;
    let requested_day = capture.requested_at().ist_date().to_string();
    if first < NaiveDate::from_ymd_opt(2022, 1, 1).ok_or(Error)?
        || last < first
        || (last - first).num_days() >= 28
        || last.to_string() >= requested_day
    {
        return Err(Error);
    }
    let rows = capture
        .data()
        .get("candles")
        .and_then(Value::as_array)
        .filter(|rows| rows.len() <= 50_000)
        .ok_or(Error)?;
    upstox_completed_minutes(
        rows,
        instrument_key,
        calendar,
        first,
        last,
        capture.received_at(),
    )
}

fn upstox_completed_minutes(
    rows: &[Value],
    instrument_key: &str,
    calendar: &Calendar,
    first: NaiveDate,
    last: NaiveDate,
    received: Timestamp,
) -> Result<Vec<MinuteBar>, Error> {
    let mut seen = BTreeMap::<Timestamp, MinuteBar>::new();
    let mut complete = BTreeMap::<Timestamp, MinuteBar>::new();
    for row in rows {
        let values = row
            .as_array()
            .filter(|values| values.len() == 7)
            .ok_or(Error)?;
        let timestamp = Timestamp::parse(values[0].as_str().ok_or(Error)?).map_err(|_| Error)?;
        let day_text = timestamp.ist_date().to_string();
        if day_text < first.to_string() || day_text > last.to_string() {
            return Err(Error);
        }
        let exchange_day = ExchangeDate::parse(&day_text).map_err(|_| Error)?;
        let (opening, closing) = calendar
            .hours(exchange_day)
            .map_err(|_| Error)?
            .ok_or(Error)?;
        let opening_at = local_timestamp(exchange_day, &opening.to_string())?;
        let closing_at = local_timestamp(exchange_day, &closing.to_string())?;
        let offset = timestamp
            .elapsed_micros_since(opening_at)
            .map_err(|_| Error)?;
        if timestamp < opening_at
            || timestamp >= closing_at
            || timestamp > received
            || offset < 0
            || offset % 60_000_000 != 0
        {
            return Err(Error);
        }
        let open = number_price(&values[1])?;
        let high = number_price(&values[2])?;
        let low = number_price(&values[3])?;
        let close = number_price(&values[4])?;
        let volume = values[5].as_u64().ok_or(Error)?;
        let open_interest = values[6].as_u64().ok_or(Error)?;
        if high < open.max(close) || low > open.min(close) || low > high {
            return Err(Error);
        }
        let bar = MinuteBar {
            instrument_key: instrument_key.to_owned(),
            timestamp,
            available_at: received.max(
                timestamp
                    .checked_add_micros(60_000_000)
                    .map_err(|_| Error)?,
            ),
            open,
            high,
            low,
            close,
            volume,
            open_interest: Some(open_interest),
        };
        if seen.get(&timestamp).is_some_and(|prior| prior != &bar) {
            return Err(Error);
        }
        seen.insert(timestamp, bar.clone());
        if timestamp
            .checked_add_micros(60_000_000)
            .is_ok_and(|end| end <= received)
        {
            complete.insert(timestamp, bar);
        }
    }
    let mut expected = BTreeSet::new();
    let mut day = first;
    loop {
        let exchange_day = ExchangeDate::parse(&day.to_string()).map_err(|_| Error)?;
        if let Some((opening, closing)) = calendar.hours(exchange_day).map_err(|_| Error)? {
            let opening_at = local_timestamp(exchange_day, &opening.to_string())?;
            let closing_at = local_timestamp(exchange_day, &closing.to_string())?;
            let session_micros = closing_at
                .elapsed_micros_since(opening_at)
                .map_err(|_| Error)?;
            if session_micros <= 0 || session_micros % 60_000_000 != 0 {
                return Err(Error);
            }
            let cutoff = received.min(closing_at);
            let elapsed = cutoff.elapsed_micros_since(opening_at).map_err(|_| Error)?;
            let expected_count = if elapsed < 0 {
                0
            } else {
                (elapsed / 60_000_000).clamp(0, session_micros / 60_000_000)
            };
            for minute in 0..expected_count {
                expected.insert(
                    opening_at
                        .checked_add_micros(minute * 60_000_000)
                        .map_err(|_| Error)?,
                );
            }
        }
        if day == last {
            break;
        }
        day = day.checked_add_signed(Duration::days(1)).ok_or(Error)?;
    }
    if complete.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(Error);
    }
    Ok(complete.into_values().collect())
}

fn strict_iso_date(text: &str) -> Result<NaiveDate, Error> {
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| Error)?;
    if date.to_string() == text {
        Ok(date)
    } else {
        Err(Error)
    }
}

fn number_price(value: &Value) -> Result<Price, Error> {
    Price::parse(value.as_number().ok_or(Error)?.to_string().as_str()).map_err(|_| Error)
}

fn encode_path_component(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            result.push(char::from(byte));
        } else {
            result.push('%');
            result.push(char::from(b"0123456789ABCDEF"[(byte >> 4) as usize]));
            result.push(char::from(b"0123456789ABCDEF"[(byte & 0x0f) as usize]));
        }
    }
    result
}

fn upstox_response(raw: &[u8]) -> Result<Value, Error> {
    let parsed = crate::native::decode_bounded(raw, MAX_BODY_BYTES).map_err(|_| Error)?;
    let object = parsed.as_object().ok_or(Error)?;
    if object.get("status").and_then(Value::as_str) != Some("success") {
        return Err(Error);
    }
    object.get("data").cloned().ok_or(Error)
}

/// Parse the bounded Kotak CSV master without converting provider cells to
/// numbers. Only the two observed padded header spellings are normalized.
pub fn kotak_csv_master(raw: &[u8], path: &str) -> Result<Value, Error> {
    const MAX_ROWS: usize = 50_000;
    const REQUIRED: [&str; 7] = [
        "pSymbol",
        "pExchSeg",
        "pTrdSymbol",
        "pISIN",
        "pSegment",
        "dTickSize",
        "lLotSize",
    ];
    if raw.len() > MAX_MASTER_BYTES {
        return Err(Error);
    }
    let segment = path
        .rsplit('/')
        .next()
        .and_then(|name| name.strip_suffix("-v1.csv"))
        .filter(|segment| matches!(*segment, "nse_cm" | "bse_cm"))
        .ok_or(Error)?;
    let text = std::str::from_utf8(raw).map_err(|_| Error)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = csv::ReaderBuilder::new()
        .flexible(false)
        .has_headers(false)
        .from_reader(text.as_bytes());
    let mut records = reader.records();
    let header = records.next().ok_or(Error)?.map_err(|_| Error)?;
    if header.is_empty() || header.len() > 256 {
        return Err(Error);
    }
    let headers = header
        .iter()
        .map(|name| match name {
            "dTickSize " => "dTickSize",
            "lExpiryDate " => "lExpiryDate",
            other => other,
        })
        .collect::<Vec<_>>();
    if headers
        .iter()
        .any(|name| name.is_empty() || name.len() > 128 || !name.is_ascii())
        || headers.iter().collect::<BTreeSet<_>>().len() != headers.len()
        || !REQUIRED.iter().all(|name| headers.contains(name))
    {
        return Err(Error);
    }
    let token_index = headers
        .iter()
        .position(|name| *name == "pSymbol")
        .ok_or(Error)?;
    let segment_index = headers
        .iter()
        .position(|name| *name == "pExchSeg")
        .ok_or(Error)?;
    let mut tokens = BTreeSet::new();
    let mut rows = Vec::new();
    for record in records {
        let record = record.map_err(|_| Error)?;
        if rows.len() >= MAX_ROWS || record.len() != headers.len() {
            return Err(Error);
        }
        if record
            .iter()
            .any(|value| value.chars().count() > 4096 || value.contains('\0'))
        {
            return Err(Error);
        }
        let token = record.get(token_index).ok_or(Error)?;
        if token.is_empty()
            || token.len() > 20
            || !token.bytes().all(|byte| byte.is_ascii_digit())
            || !tokens.insert(token.to_owned())
            || record.get(segment_index) != Some(segment)
        {
            return Err(Error);
        }
        let row = headers
            .iter()
            .zip(record.iter())
            .map(|(key, value)| ((*key).to_owned(), Value::String(value.to_owned())))
            .collect();
        rows.push(Value::Object(row));
    }
    if rows.is_empty() {
        return Err(Error);
    }
    Ok(Value::Array(rows))
}

fn path_matches(source: CaptureSource, path: &str) -> bool {
    match source {
        CaptureSource::Upstox => {
            path == "/v2/option/chain"
                || path == "/v2/market-quote/quotes"
                || valid_upstox_history_path(path)
        }
        CaptureSource::Kotak => {
            path == KOTAK_HISTORY_PATH || path == KOTAK_MASTER_PATH || valid_kotak_quote_path(path)
        }
        CaptureSource::KotakMaster => valid_kotak_master_path(path),
    }
}

/// Validate the read-only master discovery response and return its data object.
pub fn kotak_master_manifest(raw: &[u8]) -> Result<Value, Error> {
    let parsed = crate::native::decode_any_bounded(raw, MAX_BODY_BYTES).map_err(|_| Error)?;
    let object = parsed.as_object().ok_or(Error)?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "data" | "stat" | "stCode"))
        || object
            .get("stat")
            .is_some_and(|value| value.as_str() != Some("Ok"))
        || object
            .get("stCode")
            .is_some_and(|value| value.as_i64() != Some(200))
    {
        return Err(Error);
    }
    object
        .get("data")
        .filter(|value| value.is_object())
        .cloned()
        .ok_or(Error)
}

/// Resolve exactly one dated CSV path from a captured original manifest.
/// This computes a path for later callers; it performs no download.
pub fn kotak_master_url(
    capture: &MarketCapture,
    segment: &str,
    master_date: NaiveDate,
) -> Result<String, Error> {
    const BASE: &str = "https://lapi.kotaksecurities.com/wso2-scripmaster/v1/prod";
    if capture.source() != "kotak-readonly-v1"
        || capture.path() != KOTAK_MASTER_PATH
        || !matches!(segment, "nse_cm" | "bse_cm")
        || master_date.to_string() > capture.received_at().ist_date().to_string()
    {
        return Err(Error);
    }
    let data = capture.data().as_object().ok_or(Error)?;
    if data.get("baseFolder").and_then(Value::as_str) != Some(BASE) {
        return Err(Error);
    }
    let paths = data
        .get("filesPaths")
        .and_then(Value::as_array)
        .filter(|paths| (1..=20).contains(&paths.len()))
        .ok_or(Error)?;
    let mut matches = Vec::new();
    for value in paths {
        let url = value.as_str().ok_or(Error)?;
        let rest = url.strip_prefix(&format!("{BASE}/")).ok_or(Error)?;
        let (date, file) = rest.split_once('/').ok_or(Error)?;
        let parsed_date = NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| Error)?;
        if parsed_date.to_string() != date {
            return Err(Error);
        }
        match file.strip_prefix("transformed-v1/") {
            Some(file) if matches!(file, "nse_cm-v1.csv" | "bse_cm-v1.csv") => {
                if date == master_date.to_string() && file == format!("{segment}-v1.csv") {
                    matches.push(url.to_owned());
                }
            }
            _ => {
                let file = file.strip_prefix("transformed/").ok_or(Error)?;
                let file = file.strip_suffix(".csv").ok_or(Error)?;
                if file.is_empty()
                    || file.len() > 128
                    || !file
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
                {
                    return Err(Error);
                }
            }
        }
    }
    if matches.len() != 1 {
        return Err(Error);
    }
    Ok(matches.remove(0))
}

fn valid_upstox_history_path(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    let offset = if parts.get(3) == Some(&"intraday") {
        1
    } else {
        0
    };
    parts.first() == Some(&"")
        && parts.get(1) == Some(&"v3")
        && parts.get(2) == Some(&"historical-candle")
        && parts.len() >= 6 + offset
        && parts.len() <= 8 + offset
        && !parts[3 + offset].is_empty()
        && matches!(
            parts[4 + offset],
            "minutes" | "hours" | "days" | "weeks" | "months"
        )
        && !parts[5 + offset].is_empty()
        && parts[5 + offset].bytes().all(|b| b.is_ascii_digit())
        && parts[6 + offset..].iter().all(|p| {
            p.len() == 10
                && NaiveDate::parse_from_str(p, "%Y-%m-%d").is_ok_and(|d| d.to_string() == *p)
        })
}

fn valid_kotak_quote_path(path: &str) -> bool {
    let prefix = "/script-details/1.0/quotes/neosymbol/";
    let Some(values) = path
        .strip_prefix(prefix)
        .and_then(|p| p.strip_suffix("/all"))
    else {
        return false;
    };
    let symbols: Vec<_> = values.split(',').collect();
    (1..=50).contains(&symbols.len()) && symbols.iter().all(|s| valid_native_symbol(s))
}

fn valid_kotak_master_path(path: &str) -> bool {
    let Some(rest) = path.strip_prefix("/wso2-scripmaster/v1/prod/") else {
        return false;
    };
    let mut parts = rest.split('/');
    let date = parts.next().unwrap_or_default();
    let transformed = parts.next() == Some("transformed-v1");
    let file = parts.next().unwrap_or_default();
    transformed
        && parts.next().is_none()
        && date.len() == 10
        && NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok_and(|d| d.to_string() == date)
        && matches!(file, "nse_cm-v1.csv" | "bse_cm-v1.csv")
}

/// Validate the read-only Kotak one-minute response envelope. The original body
/// remains the source of identity; returned JSON is only a parsed projection.
pub fn kotak_minute_response(raw: &[u8]) -> Result<Value, Error> {
    let parsed = crate::native::decode_bounded(raw, MAX_BODY_BYTES).map_err(|_| Error)?;
    let object = parsed.as_object().ok_or(Error)?;
    if object.len() > 3
        || object.get("status").and_then(Value::as_str) != Some("success")
        || object.get("interval").and_then(Value::as_str) != Some("1min")
    {
        return Err(Error);
    }
    let data = object.get("data").and_then(Value::as_object).ok_or(Error)?;
    if data.len() != 1
        || data
            .get("candles")
            .and_then(Value::as_array)
            .is_none_or(|rows| rows.len() > 50_000)
    {
        return Err(Error);
    }
    Ok(Value::Object(data.clone()))
}

/// Normalize completed Kotak one-minute rows only when the original capture,
/// native mapping, canonical instrument, and reviewed session calendar agree.
/// The calendar and capture remain declarations; this does not authenticate either.
pub fn normalize_kotak_minute_capture(
    capture: &MarketCapture,
    binding: &PaperSourceBinding,
    calendar: &Calendar,
) -> Result<Vec<KotakMinuteBar>, Error> {
    let mapping = binding.native_mapping().ok_or(Error)?;
    let instrument = binding.instrument();
    if binding.source() != Source::Kotak
        || instrument.synthetic()
        || calendar.synthetic()
        || calendar.require_instrument(instrument).is_err()
        || capture.source() != "kotak-readonly-v1"
        || capture.path() != KOTAK_HISTORY_PATH
        || binding.timestamp_reference().is_none()
        || capture.params().get("neosymbol")
            != Some(&format!("{}|{}", mapping.segment(), mapping.token()))
    {
        return Err(Error);
    }
    let (first, last) = kotak_minute_window(capture.params())?;
    let requested_day = capture.requested_at().ist_date().to_string();
    let last_day = last.to_string();
    let received_day = capture.received_at().ist_date().to_string();
    if last_day > requested_day
        || (last_day == requested_day
            && (received_day != requested_day
                || calendar
                    .hours(ExchangeDate::parse(&last_day).map_err(|_| Error)?)
                    .map_err(|_| Error)?
                    .is_none_or(|(opening, _)| capture.requested_at().ist_time() < opening)))
    {
        return Err(Error);
    }

    let rows = capture
        .data()
        .get("candles")
        .and_then(Value::as_array)
        .ok_or(Error)?;
    if rows.len() > 50_000 {
        return Err(Error);
    }
    let mut seen = BTreeMap::<Timestamp, KotakMinuteBar>::new();
    let mut completed = BTreeMap::<Timestamp, KotakMinuteBar>::new();
    for row in rows {
        let values = row.as_array().ok_or(Error)?;
        if !(6..=7).contains(&values.len()) {
            return Err(Error);
        }
        let stamp_text = values[0].as_str().ok_or(Error)?;
        let timestamp = Timestamp::parse(stamp_text).map_err(|_| Error)?;
        let day = timestamp.ist_date();
        let day_text = day.to_string();
        if day_text < first.to_string() || day_text > last.to_string() {
            return Err(Error);
        }
        let (opening, closing) = calendar.hours(day).map_err(|_| Error)?.ok_or(Error)?;
        let opening_at = local_timestamp(day, &opening.to_string())?;
        let closing_at = local_timestamp(day, &closing.to_string())?;
        let elapsed = timestamp
            .elapsed_micros_since(opening_at)
            .map_err(|_| Error)?;
        if timestamp < opening_at
            || timestamp >= closing_at
            || timestamp > capture.received_at()
            || elapsed < 0
            || elapsed % 60_000_000 != 0
        {
            return Err(Error);
        }
        let open = row_price(&values[1], instrument)?;
        let high = row_price(&values[2], instrument)?;
        let low = row_price(&values[3], instrument)?;
        let close = row_price(&values[4], instrument)?;
        let volume = values[5].as_u64().ok_or(Error)?;
        let open_interest = match values.get(6) {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or(Error)?),
        };
        if high < open.max(close) || low > open.min(close) || low > high {
            return Err(Error);
        }
        let available_at = capture.received_at().max(
            timestamp
                .checked_add_micros(60_000_000)
                .map_err(|_| Error)?,
        );
        let bar = KotakMinuteBar {
            instrument_key: instrument.key().to_owned(),
            timestamp,
            available_at,
            open,
            high,
            low,
            close,
            volume,
            open_interest,
        };
        if seen.get(&timestamp).is_some_and(|prior| prior != &bar) {
            return Err(Error);
        }
        seen.insert(timestamp, bar.clone());
        if timestamp
            .checked_add_micros(60_000_000)
            .is_ok_and(|end| end <= capture.received_at())
        {
            completed.insert(timestamp, bar);
        }
    }

    let mut expected = BTreeSet::new();
    let mut day = first;
    loop {
        let exchange_day = ExchangeDate::parse(&day.to_string()).map_err(|_| Error)?;
        if let Some((opening, closing)) = calendar.hours(exchange_day).map_err(|_| Error)? {
            let open_at = local_timestamp(exchange_day, &opening.to_string())?;
            let close_at = local_timestamp(exchange_day, &closing.to_string())?;
            let session_micros = close_at.elapsed_micros_since(open_at).map_err(|_| Error)?;
            if session_micros < 0 || session_micros % 60_000_000 != 0 {
                return Err(Error);
            }
            let cutoff = capture.received_at().min(close_at);
            let elapsed = cutoff.elapsed_micros_since(open_at).map_err(|_| Error)?;
            let count = (elapsed / 60_000_000).clamp(0, session_micros / 60_000_000);
            for minute in 0..count {
                expected.insert(
                    open_at
                        .checked_add_micros(minute * 60_000_000)
                        .map_err(|_| Error)?,
                );
            }
        }
        if day == last {
            break;
        }
        day = day.checked_add_signed(Duration::days(1)).ok_or(Error)?;
    }
    if completed.keys().copied().collect::<BTreeSet<_>>() != expected {
        return Err(Error);
    }
    Ok(completed.into_values().collect())
}

fn local_timestamp(day: ExchangeDate, time: &str) -> Result<Timestamp, Error> {
    Timestamp::parse(&format!("{day}T{time}+05:30")).map_err(|_| Error)
}

fn row_price(value: &Value, instrument: &Instrument) -> Result<Price, Error> {
    let number = value.as_number().ok_or(Error)?.to_string();
    let price = Price::parse(&number).map_err(|_| Error)?;
    if !price
        .is_multiple_of(instrument.tick_size())
        .map_err(|_| Error)?
    {
        return Err(Error);
    }
    Ok(price)
}

/// Validate a successful Kotak quote-array envelope before native mapping.
pub fn kotak_quote_response(raw: &[u8]) -> Result<Vec<Value>, Error> {
    let parsed = crate::native::decode_any_bounded(raw, MAX_BODY_BYTES).map_err(|_| Error)?;
    let rows = parsed
        .as_array()
        .filter(|rows| (1..=50).contains(&rows.len()))
        .ok_or(Error)?;
    Ok(rows.clone())
}

/// Bind one raw Kotak quote row to an explicitly selected native identity.
/// This intentionally does not validate depth, price, freshness, or usability.
pub fn validate_kotak_quote_review(
    capture: &MarketCapture,
    segment: &str,
    token: &str,
    expected_symbol: &str,
) -> Result<(), Error> {
    if !matches!(segment, "nse_cm" | "bse_cm")
        || !valid_native_symbol(&format!("{segment}|{token}"))
        || !(1..=100).contains(&expected_symbol.len())
        || !expected_symbol.is_ascii()
        || !expected_symbol.bytes().all(|byte| {
            byte.is_ascii_uppercase() || byte.is_ascii_digit() || b"&_.-".contains(&byte)
        })
        || capture.source() != "kotak-readonly-v1"
        || capture.path() != format!("/script-details/1.0/quotes/neosymbol/{segment}|{token}/all")
    {
        return Err(Error);
    }
    let rows = capture.data().as_array().ok_or(Error)?;
    if rows.len() != 1 {
        return Err(Error);
    }
    let row = rows[0].as_object().ok_or(Error)?;
    if row.get("exchange").and_then(Value::as_str) != Some(segment)
        || row.get("exchange_token").and_then(Value::as_str) != Some(token)
        || row.get("display_symbol").and_then(Value::as_str) != Some(expected_symbol)
    {
        return Err(Error);
    }
    Ok(())
}

/// Validate the Python Kotak minute-window contract while preserving strict
/// ISO dates and the documented conservative 28-day maximum.
pub fn kotak_minute_window(
    params: &BTreeMap<String, String>,
) -> Result<(NaiveDate, NaiveDate), Error> {
    if params.len() != 4
        || params.get("interval").map(String::as_str) != Some("1min")
        || params
            .get("neosymbol")
            .is_none_or(|s| !valid_native_symbol(s))
    {
        return Err(Error);
    }
    let first_text = params.get("fromdate").ok_or(Error)?;
    let last_text = params.get("todate").ok_or(Error)?;
    let first = NaiveDate::parse_from_str(first_text, "%Y-%m-%d").map_err(|_| Error)?;
    let last = NaiveDate::parse_from_str(last_text, "%Y-%m-%d").map_err(|_| Error)?;
    if first.to_string() != *first_text
        || last.to_string() != *last_text
        || (last - first).num_days() < 0
        || (last - first).num_days() >= 28
    {
        return Err(Error);
    }
    Ok((first, last))
}

fn valid_native_symbol(value: &str) -> bool {
    let Some((segment, token)) = value.split_once('|') else {
        return false;
    };
    matches!(segment, "nse_cm" | "bse_cm")
        && !token.is_empty()
        && token.len() <= 20
        && token.bytes().all(|b| b.is_ascii_digit())
}
