//! Exact, aware instants and explicit exchange-local dates/times. No clock reads.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, SecondsFormat, Timelike, Utc};
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

    pub fn elapsed_micros_since(self, earlier: Self) -> Result<i64, TimeError> {
        self.0
            .signed_duration_since(earlier.0)
            .num_microseconds()
            .ok_or(TimeError::PrecisionOrRange)
    }

    pub fn ist_date(self) -> ExchangeDate {
        ExchangeDate(self.0.with_timezone(&Kolkata).date_naive())
    }

    pub fn ist_time(self) -> LocalTime {
        LocalTime(self.0.with_timezone(&Kolkata).time())
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
