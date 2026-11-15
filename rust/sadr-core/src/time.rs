//! Exact, aware instants and explicit exchange-local dates/times. No clock reads.

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, Offset,
    SecondsFormat, Timelike, Utc,
};
use chrono_tz::Asia::Kolkata;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeError {
    InvalidAwareTimestamp,
    InvalidDate,
    InvalidLocalTime,
    PrecisionOrRange,
}

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidAwareTimestamp => "invalid_aware_timestamp",
            Self::InvalidDate => "invalid_exchange_date",
            Self::InvalidLocalTime => "invalid_local_time",
            Self::PrecisionOrRange => "timestamp_precision_or_range_exceeded",
        })
    }
}

impl Error for TimeError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    /// Exact integer epoch milliseconds; no wall clock or float conversion.
    pub fn from_epoch_millis(value: i64) -> Result<Self, TimeError> {
        let instant =
            DateTime::<Utc>::from_timestamp_millis(value).ok_or(TimeError::PrecisionOrRange)?;
        if !(1..=9999).contains(&instant.year()) {
            return Err(TimeError::PrecisionOrRange);
        }
        Ok(Self(instant))
    }

    pub fn parse(text: &str) -> Result<Self, TimeError> {
        if text.len() > 64 || !text.is_ascii() {
            return Err(TimeError::InvalidAwareTimestamp);
        }
        // Chrono can truncate fractional digits beyond nanoseconds. Inspect the
        // wire precision first so even a very late nonzero digit cannot disappear.
        if text.split_once('.').is_some_and(|(_, fraction)| {
            fraction.bytes().take_while(u8::is_ascii_digit).count() > 6
        }) {
            return Err(TimeError::PrecisionOrRange);
        }
        let value =
            DateTime::parse_from_rfc3339(text).map_err(|_| TimeError::InvalidAwareTimestamp)?;
        let utc = value.with_timezone(&Utc);
        // Python's domain has microsecond precision and no leap-second representation.
        if !(1..=9999).contains(&value.year())
            || !(1..=9999).contains(&utc.year())
            || utc.timestamp_subsec_nanos() >= 1_000_000_000
            || !utc.timestamp_subsec_nanos().is_multiple_of(1000)
        {
            return Err(TimeError::PrecisionOrRange);
        }
        Ok(Self(utc))
    }

    /// Diagnostic-only Python isoformat compatibility for historical offsets
    /// containing seconds. Financial Timestamp::parse remains strict RFC3339.
    pub fn parse_isoformat_offset_seconds(text: &str) -> Result<Self, TimeError> {
        if let Ok(instant) = Self::parse(text) {
            return Ok(instant);
        }
        if text.len() > 64 || !text.is_ascii() || text.len() < 28 {
            return Err(TimeError::InvalidAwareTimestamp);
        }
        let split = text.len() - 9;
        let (local, offset) = text.split_at(split);
        let local_bytes = local.as_bytes();
        if local_bytes.len() < 19
            || local_bytes[4] != b'-'
            || local_bytes[7] != b'-'
            || local_bytes[10] != b'T'
            || local_bytes[13] != b':'
            || local_bytes[16] != b':'
            || local_bytes[..19]
                .iter()
                .enumerate()
                .any(|(i, c)| !matches!(i, 4 | 7 | 10 | 13 | 16) && !c.is_ascii_digit())
            || (local_bytes.len() > 19
                && (local_bytes[19] != b'.'
                    || local_bytes.len() == 20
                    || !local_bytes[20..].iter().all(u8::is_ascii_digit)))
        {
            return Err(TimeError::InvalidAwareTimestamp);
        }
        let b = offset.as_bytes();
        if !matches!(b[0], b'+' | b'-')
            || b[3] != b':'
            || b[6] != b':'
            || b.iter()
                .enumerate()
                .any(|(i, c)| !matches!(i, 0 | 3 | 6) && !c.is_ascii_digit())
        {
            return Err(TimeError::InvalidAwareTimestamp);
        }
        if local.split_once('.').is_some_and(|(_, fraction)| {
            fraction.bytes().take_while(u8::is_ascii_digit).count() > 6
        }) {
            return Err(TimeError::PrecisionOrRange);
        }
        let hours = (b[1] - b'0') as i32 * 10 + (b[2] - b'0') as i32;
        let minutes = (b[4] - b'0') as i32 * 10 + (b[5] - b'0') as i32;
        let seconds = (b[7] - b'0') as i32 * 10 + (b[8] - b'0') as i32;
        if hours > 23 || minutes > 59 || seconds > 59 {
            return Err(TimeError::InvalidAwareTimestamp);
        }
        let offset = (hours * 3600 + minutes * 60 + seconds) * if b[0] == b'-' { -1 } else { 1 };
        let zone = FixedOffset::east_opt(offset).ok_or(TimeError::InvalidAwareTimestamp)?;
        let naive = NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M:%S%.f")
            .map_err(|_| TimeError::InvalidAwareTimestamp)?;
        let value = naive
            .and_local_timezone(zone)
            .single()
            .ok_or(TimeError::InvalidAwareTimestamp)?;
        let utc = value.with_timezone(&Utc);
        if !(1..=9999).contains(&value.year())
            || !(1..=9999).contains(&utc.year())
            || utc.timestamp_subsec_nanos() >= 1_000_000_000
            || !utc.timestamp_subsec_nanos().is_multiple_of(1000)
        {
            return Err(TimeError::PrecisionOrRange);
        }
        Ok(Self(utc))
    }

    pub fn elapsed_micros_since(self, earlier: Self) -> Result<i64, TimeError> {
        self.0
            .signed_duration_since(earlier.0)
            .num_microseconds()
            .ok_or(TimeError::PrecisionOrRange)
    }

    pub fn checked_add_micros(self, micros: i64) -> Result<Self, TimeError> {
        let instant = self
            .0
            .checked_add_signed(Duration::microseconds(micros))
            .ok_or(TimeError::PrecisionOrRange)?;
        if !(1..=9999).contains(&instant.year()) {
            return Err(TimeError::PrecisionOrRange);
        }
        Ok(Self(instant))
    }

    pub fn ist_date(self) -> ExchangeDate {
        ExchangeDate(self.0.with_timezone(&Kolkata).date_naive())
    }

    pub fn ist_time(self) -> LocalTime {
        LocalTime(self.0.with_timezone(&Kolkata).time())
    }

    /// Python `datetime.isoformat()` for this instant in Asia/Kolkata.
    /// Whole seconds omit the fraction; other instants keep six digits.
    pub fn to_ist_isoformat(self) -> String {
        let local = self.0.with_timezone(&Kolkata);
        let fraction = if local.timestamp_subsec_micros() == 0 {
            String::new()
        } else {
            format!(".{:06}", local.timestamp_subsec_micros())
        };
        let offset = if local.offset().fix().local_minus_utc() % 60 == 0 {
            "%:z"
        } else {
            "%::z"
        };
        format!(
            "{}{}{}",
            local.format("%Y-%m-%dT%H:%M:%S"),
            fraction,
            local.format(offset)
        )
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0.to_rfc3339_opts(SecondsFormat::AutoSi, true))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExchangeDate(NaiveDate);

impl ExchangeDate {
    pub fn parse(text: &str) -> Result<Self, TimeError> {
        if text.len() != 10 || !text.is_ascii() {
            return Err(TimeError::InvalidDate);
        }
        if text.bytes().enumerate().any(|(index, byte)| {
            if matches!(index, 4 | 7) {
                byte != b'-'
            } else {
                !byte.is_ascii_digit()
            }
        }) {
            return Err(TimeError::InvalidDate);
        }
        let date =
            NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| TimeError::InvalidDate)?;
        if !(1..=9999).contains(&date.year()) {
            return Err(TimeError::InvalidDate);
        }
        Ok(Self(date))
    }

    pub fn is_weekend(self) -> bool {
        self.0.weekday().number_from_monday() >= 6
    }

    pub fn previous_day(self) -> Option<Self> {
        self.0.pred_opt().map(Self)
    }
}

impl fmt::Display for ExchangeDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct LocalTime(NaiveTime);

impl LocalTime {
    pub fn parse(text: &str) -> Result<Self, TimeError> {
        if text.len() > 15 || !text.is_ascii() {
            return Err(TimeError::InvalidLocalTime);
        }
        let time = NaiveTime::parse_from_str(text, "%H:%M:%S%.f")
            .or_else(|_| NaiveTime::parse_from_str(text, "%H:%M"))
            .map_err(|_| TimeError::InvalidLocalTime)?;
        if time.nanosecond() >= 1_000_000_000 || !time.nanosecond().is_multiple_of(1000) {
            return Err(TimeError::PrecisionOrRange);
        }
        Ok(Self(time))
    }

    pub fn is_whole_minute(self) -> bool {
        self.0.second() == 0 && self.0.nanosecond() == 0
    }
}

impl fmt::Display for LocalTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

macro_rules! string_serde {
    ($type:ty) => {
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.to_string())
            }
        }
        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let text = String::deserialize(deserializer)?;
                Self::parse(&text).map_err(de::Error::custom)
            }
        }
    };
}

string_serde!(Timestamp);
string_serde!(ExchangeDate);
string_serde!(LocalTime);

pub const IANA_TZDB_VERSION: &str = chrono_tz::IANA_TZDB_VERSION;
