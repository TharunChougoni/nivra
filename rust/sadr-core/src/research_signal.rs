//! Retrospective original-receipt daily close diagnostics. No returns or promotion authority.
//! Exact decimal support is bounded to scale <= 8 and a normalized coefficient <= 10^20;
//! positive values must also be < 10^16. These joint bounds keep 200/50 sums and
//! division within Python's 28-digit decimal context. Unsupported numbers fail
//! explicitly rather than rounding through binary floating point.
use chrono::{
    DateTime, Datelike, FixedOffset, NaiveDate, Offset, SecondsFormat, TimeZone, Timelike,
};
use rust_decimal::Decimal;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    NumericRange,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "signal_review_invalid",
            Self::NumericRange => "signal_review_numeric_range",
        })
    }
}
impl std::error::Error for Error {}
fn stamp(s: &str) -> Result<DateTime<FixedOffset>, Error> {
    let dt = DateTime::parse_from_rfc3339(s).map_err(|_| Error::Invalid)?;
    if !dt.year_ce().0 || dt.year_ce().1 > 9999 || dt.nanosecond() >= 1_000_000_000 {
        return Err(Error::Invalid);
    }
    dt.with_nanosecond(dt.nanosecond() / 1000 * 1000)
        .ok_or(Error::Invalid)
}
fn text(dt: DateTime<FixedOffset>, z: bool) -> String {
    dt.to_rfc3339_opts(
        if dt.nanosecond() == 0 {
            SecondsFormat::Secs
        } else {
            SecondsFormat::Micros
        },
        z,
    )
}
fn decimal(s: &str) -> Result<Decimal, Error> {
    let (base, exponent) = s
        .split_once(['e', 'E'])
        .map_or((s, 0), |(a, b)| (a, b.parse::<i32>().unwrap_or(i32::MAX)));
    if exponent.unsigned_abs() > 1000 || base.starts_with('-') || base.starts_with('+') {
        return Err(Error::NumericRange);
    }
    let (whole, fraction) = base.split_once('.').unwrap_or((base, ""));
    if whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|b| b.is_ascii_digit())
    {
        return Err(Error::NumericRange);
    }
    let mut digits = format!("{whole}{fraction}");
    let mut scale = i32::try_from(fraction.len())
        .map_err(|_| Error::NumericRange)?
        .checked_sub(exponent)
        .ok_or(Error::NumericRange)?;
    while scale > 0 && digits.ends_with('0') {
        digits.pop();
        scale -= 1;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() || digits.len() > 21 || scale > 8 {
        return Err(Error::NumericRange);
    }
    let mut coefficient = digits.parse::<i128>().map_err(|_| Error::NumericRange)?;
    while scale < 0 {
        coefficient = coefficient.checked_mul(10).ok_or(Error::NumericRange)?;
        scale += 1;
    }
    if coefficient > 100_000_000_000_000_000_000 {
        return Err(Error::NumericRange);
    }
    let d = Decimal::from_i128_with_scale(coefficient, scale as u32);
    if d >= Decimal::from(10_000_000_000_000_000_u64) {
        return Err(Error::NumericRange);
    }
    Ok(d)
}
/// This is a close-condition input, not an execution bar or historical availability claim.
/// Timestamps require RFC3339 strings. Direct close text accepts unsigned decimal
/// spelling with leading zeros, an optional empty fractional part, and integer
/// exponents (for example01,1.,1.e2), as Python Decimal does. Leading plus,
/// surrounding whitespace, underscores and Unicode digits are unsupported.
/// Captured bodies separately require valid original JSON grammar.
#[derive(Clone)]
pub struct TrendBar {
    timestamp: DateTime<FixedOffset>,
    available_at: DateTime<FixedOffset>,
    close: Decimal,
}
impl fmt::Debug for TrendBar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TrendBar(redacted)")
    }
}
impl TrendBar {
    pub fn new(timestamp: &str, available_at: &str, close: &str) -> Result<Self, Error> {
        let timestamp = stamp(timestamp)?;
        let available_at = stamp(available_at)?;
        if available_at <= timestamp {
            return Err(Error::Invalid);
        }
        Ok(Self {
            timestamp,
            available_at,
            close: decimal(close)?,
        })
    }
}
/// Chronology and collection availability are mandatory, including warmup rows.
pub fn trend(bars: &[TrendBar], asof: &str) -> Result<Vec<bool>, Error> {
    let asof = stamp(asof)?;
    if bars.iter().any(|b| b.available_at > asof)
        || bars.windows(2).any(|w| w[0].timestamp >= w[1].timestamp)
    {
        return Err(Error::Invalid);
    }
    let mut long = Decimal::ZERO;
    let mut short = Decimal::ZERO;
    let mut result = Vec::with_capacity(bars.len());
    for (i, b) in bars.iter().enumerate() {
        long = long.checked_add(b.close).ok_or(Error::NumericRange)?;
        short = short.checked_add(b.close).ok_or(Error::NumericRange)?;
        if i >= 200 {
            long = long
                .checked_sub(bars[i - 200].close)
                .ok_or(Error::NumericRange)?;
        }
        if i >= 50 {
            short = short
                .checked_sub(bars[i - 50].close)
                .ok_or(Error::NumericRange)?;
        }
        result.push(
            i >= 199
                && b.close
                    .checked_mul(Decimal::from(200))
                    .ok_or(Error::NumericRange)?
                    > long
                && b.close
                    .checked_mul(Decimal::from(50))
                    .ok_or(Error::NumericRange)?
                    > short,
        );
    }
    Ok(result)
}
// Preserve every numeric token before serde can convert it to f64. Prefix ALL strings,
// including keys, so an original string/object can never impersonate a numeric token.
fn exact_body(raw: &str) -> Result<Value, Error> {
    // Check original structure before quoting numeric values. serde_json's
    // IgnoredAny path uses ignore_integer/decimal/exponent lexical scanners,
    // with no f64 conversion (serde_json1.0.151/src/de.rs).
    let mut original = serde_json::Deserializer::from_str(raw);
    serde::de::IgnoredAny::deserialize(&mut original).map_err(|_| Error::Invalid)?;
    original.end().map_err(|_| Error::Invalid)?;
    let b = raw.as_bytes();
    let mut i = 0;
    let mut out = String::with_capacity(raw.len());
    while i < b.len() {
        if b[i] == b'"' {
            let start = i;
            i += 1;
            loop {
                if i >= b.len() {
                    return Err(Error::Invalid);
                }
                match b[i] {
                    b'\\' => {
                        i += 2;
                    }
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            let s: String = serde_json::from_str(raw.get(start..i).ok_or(Error::Invalid)?)
                .map_err(|_| Error::Invalid)?;
            out.push_str(&serde_json::to_string(&format!("s:{s}")).map_err(|_| Error::Invalid)?);
        } else if b[i] == b'-' || b[i].is_ascii_digit() {
            let start = i;
            i += 1;
            while i < b.len()
                && (b[i].is_ascii_digit() || matches!(b[i], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                i += 1;
            }
            let token = &raw[start..i];
            // Validate JSON grammar without accepting the parsed binary value.
            if !token.contains(['.', 'e', 'E']) && token.trim_start_matches('-').len() > 4300 {
                return Err(Error::NumericRange);
            }
            if !valid_number(token) {
                return Err(Error::Invalid);
            }
            out.push_str(
                &serde_json::to_string(&format!("n:{token}")).map_err(|_| Error::Invalid)?,
            );
        } else {
            let c = raw[i..].chars().next().ok_or(Error::Invalid)?;
            out.push(c);
            i += c.len_utf8();
        }
    }
    decode(out.as_bytes())
}
fn number(v: &Value) -> Result<&str, Error> {
    v.as_str()
        .and_then(|s| s.strip_prefix("n:"))
        .ok_or(Error::Invalid)
}
fn integer(v: &Value) -> Result<(), Error> {
    let s = number(v)?;
    let digits = s.strip_prefix('-').unwrap_or(s);
    if !digits.bytes().all(|c| c.is_ascii_digit())
        || (s.starts_with('-') && digits.bytes().any(|c| c != b'0'))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
fn source() -> String {
    "upstox-readonly-v1".into()
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    #[serde(default = "source")]
    source: String,
    path: String,
    #[serde(default)]
    params: BTreeMap<String, String>,
    requested_at: String,
    received_at: String,
    body: String,
}
pub struct DailyReview {
    original: Vec<u8>,
    canonical: Vec<u8>,
    capture_hash: String,
    report: Value,
}
impl fmt::Debug for DailyReview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DailySignalReview(retrospective, no authority)")
    }
}
impl DailyReview {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        // Accommodate worst-case JSON escaping of the complete 2 MB body.
        if raw.len() > 12_100_000 {
            return Err(Error::Invalid);
        }
        let f: Fields = serde_json::from_value(decode(raw)?).map_err(|_| Error::Invalid)?;
        if f.source != source()
            || !f.params.is_empty()
            || f.body.is_empty()
            || f.body.len() > 2_000_000
            || f.path.chars().count() > 2048
            || f.path.contains(['?', '#', '@', '\\'])
        {
            return Err(Error::Invalid);
        }
        let requested = stamp(&f.requested_at)?;
        let received = stamp(&f.received_at)?;
        if received < requested {
            return Err(Error::Invalid);
        }
        let parts: Vec<_> = f.path.split('/').collect();
        if parts.len() != 8
            || parts[..3] != ["", "v3", "historical-candle"]
            || parts[4..6] != ["days", "1"]
        {
            return Err(Error::Invalid);
        }
        Self::finish(raw, f, requested, received)
    }
    fn finish(
        raw: &[u8],
        mut f: Fields,
        requested: DateTime<FixedOffset>,
        received: DateTime<FixedOffset>,
    ) -> Result<Self, Error> {
        let parts: Vec<_> = f.path.split('/').collect();
        let key = percent_decode(parts[3])?;
        let valid_index = key.strip_prefix("NSE_INDEX|").is_some_and(|s| {
            !s.is_empty()
                && s.len() <= 80
                && s.as_bytes()[0].is_ascii_alphanumeric()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b" .&()-".contains(&b))
        });
        let valid_cash = key.strip_prefix("NSE_EQ|").is_some_and(|s| {
            s.len() == 12
                && s.bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        });
        if !valid_index && !valid_cash {
            return Err(Error::Invalid);
        }
        let end = exact_date(parts[6])?;
        let start = exact_date(parts[7])?;
        if parts[6] != end.to_string()
            || parts[7] != start.to_string()
            || start > end
            || end
                >= requested
                    .with_timezone(&chrono_tz::Asia::Kolkata)
                    .date_naive()
        {
            return Err(Error::Invalid);
        }
        let data = exact_body(&f.body)?;
        if data["s:status"] != "s:success" {
            return Err(Error::Invalid);
        }
        let rows = data["s:data"]["s:candles"]
            .as_array()
            .ok_or(Error::Invalid)?;
        if !(200..=5000).contains(&rows.len()) {
            return Err(Error::Invalid);
        }
        let mut ordered = BTreeMap::new();
        for row in rows {
            let r = row
                .as_array()
                .filter(|r| r.len() == 7)
                .ok_or(Error::Invalid)?;
            let label = r[0]
                .as_str()
                .and_then(|s| s.strip_prefix("s:"))
                .ok_or(Error::Invalid)?;
            let dt = stamp(label)?;
            let local_midnight = dt.date_naive().and_hms_opt(0, 0, 0).ok_or(Error::Invalid)?;
            let expected = chrono_tz::Asia::Kolkata
                .from_local_datetime(&local_midnight)
                .single()
                .ok_or(Error::Invalid)?
                .offset()
                .fix()
                .local_minus_utc();
            if dt.offset().local_minus_utc() != expected
                || dt.time() != chrono::NaiveTime::MIN
                || dt.date_naive() < start
                || dt.date_naive() > end
            {
                return Err(Error::Invalid);
            }
            let prices = r[1..5]
                .iter()
                .map(|v| decimal(number(v)?))
                .collect::<Result<Vec<_>, _>>()?;
            integer(&r[5])?;
            integer(&r[6])?;
            if prices[2] > prices[0].min(prices[3]) || prices[1] < prices[0].max(prices[3]) {
                return Err(Error::Invalid);
            }
            let bar = TrendBar::new(label, &f.received_at, &prices[3].to_string())?;
            if ordered.insert(dt.date_naive(), bar).is_some() {
                return Err(Error::Invalid);
            }
        }
        let bars: Vec<_> = ordered.into_values().collect();
        let signals = trend(&bars, &f.received_at)?;
        let transitions: Vec<_> = (200..bars.len())
            .filter(|&i| signals[i] && !signals[i - 1])
            .map(|i| text(bars[i].timestamp, false))
            .collect();
        f.requested_at = text(requested, true);
        f.received_at = text(received, true);
        let canonical = serde_json::to_vec(&f).map_err(|_| Error::Invalid)?;
        let capture_hash = Sha256::digest(&canonical)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let report = json!({"label":"SIGNAL_RESEARCH","status":"NOT_READY","strategy":"cash_trend_200_50_fixed_v1","diagnostic":"RETROSPECTIVE_CLOSE_CONDITION_ONLY","capture_id":capture_hash,"source":f.source,"instrument_key":key,"received_at":text(received,false),"availability":"ACTUAL_RECEIPT_ONLY_NOT_HISTORICAL_POINT_IN_TIME","rows":bars.len(),"observed_first_label":text(bars[0].timestamp,false),"observed_last_label":text(bars[bars.len()-1].timestamp,false),"warmup_rows":199,"eligible_close_rows":bars.len()-199,"condition_true_rows":signals.iter().filter(|&&b|b).count(),"rising_transitions_after_warmup":transitions.len(),"transition_labels":transitions,"last_condition":signals.last(),"calendar_coverage":"UNVERIFIED","historical_instrument_coverage":"UNVERIFIED","holdout":"NOT_OPENED","statistical_gate":"NOT_EVALUATED","execution":"NOT_SIMULATED","pnl_inr":null,"activation":"NOT_AUTHORIZED"});
        Ok(Self {
            original: raw.to_vec(),
            canonical,
            capture_hash,
            report,
        })
    }
    pub fn original_json(&self) -> &[u8] {
        &self.original
    }
    pub fn canonical_json(&self) -> &[u8] {
        &self.canonical
    }
    pub fn capture_hash(&self) -> &str {
        &self.capture_hash
    }
    pub fn report(&self) -> &Value {
        &self.report
    }
}
fn percent_decode(s: &str) -> Result<String, Error> {
    let mut out = Vec::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|s| u8::from_str_radix(s, 16).ok());
            if let Some(h) = h {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).map_err(|_| Error::Invalid)
}

fn valid_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    if b.get(i) == Some(&b'0') {
        i += 1;
    } else {
        if !b.get(i).is_some_and(|c| matches!(c, b'1'..=b'9')) {
            return false;
        }
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if start == i {
            return false;
        }
    }
    if b.get(i).is_some_and(|c| matches!(c, b'e' | b'E')) {
        i += 1;
        if b.get(i).is_some_and(|c| matches!(c, b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if start == i {
            return false;
        }
    }
    i == b.len()
}

// All body numbers are losslessly tagged strings before this decoder. Outer
// capture numbers cannot satisfy any field. Duplicate objects are always refused.
struct Strict(Value);
impl<'de> Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Strict;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("exact bounded JSON")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Strict, E> {
                Ok(Strict(Value::Bool(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Strict, E> {
                Ok(Strict(Value::Null))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Strict, E> {
                Ok(Strict(Value::String(v.into())))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Strict, E> {
                Ok(Strict(Value::String(v)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                let mut v = Vec::new();
                while let Some(Strict(x)) = a.next_element()? {
                    v.push(x);
                }
                Ok(Strict(Value::Array(v)))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Strict, A::Error> {
                let mut v = serde_json::Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if v.contains_key(&k) {
                        return Err(de::Error::custom("duplicate_json_key"));
                    }
                    let Strict(x) = a.next_value()?;
                    v.insert(k, x);
                }
                Ok(Strict(Value::Object(v)))
            }
        }
        d.deserialize_any(V)
    }
}
fn decode(raw: &[u8]) -> Result<Value, Error> {
    let Strict(v) = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    if !v.is_object() {
        return Err(Error::Invalid);
    }
    Ok(v)
}

fn exact_date(raw: &str) -> Result<NaiveDate, Error> {
    let b = raw.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| !matches!(i, 4 | 7) && !c.is_ascii_digit())
    {
        return Err(Error::Invalid);
    }
    let d = NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| Error::Invalid)?;
    if !(1..=9999).contains(&d.year()) {
        return Err(Error::Invalid);
    }
    Ok(d)
}
