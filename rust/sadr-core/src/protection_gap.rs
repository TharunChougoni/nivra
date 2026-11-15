//! Supplied diagnostic measurements only. Completeness never establishes a cause,
//! repairs protection, relaxes the two-second gate or grants trading authority.
use serde::Serialize;
use serde_json::Value;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Protection gap cause unavailable")
    }
}
impl std::error::Error for Error {}
#[derive(Clone, Copy, Serialize)]
enum Evidence {
    #[serde(rename = "COMPLETE")]
    Complete,
    #[serde(rename = "INCOMPLETE")]
    Incomplete,
}
#[derive(Serialize)]
pub struct GapCause {
    version: &'static str,
    receipt_age_seconds: Option<f64>,
    scheduler_wait_seconds: Option<f64>,
    dropped_records: Option<u32>,
    evidence: Evidence,
    cause: &'static str,
    inferred: bool,
    freshness_limit_seconds: u8,
    limit_relaxed: bool,
    repaired: bool,
}
impl fmt::Debug for GapCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProtectionGapCause(diagnostic only, no authority)")
    }
}
/// Records measured values; there is no clock, inference, fallback or gate change.
pub fn protection_gap_cause(
    receipt_age_seconds: Option<f64>,
    scheduler_wait_seconds: Option<f64>,
    dropped_records: Option<u32>,
) -> Result<GapCause, Error> {
    if [receipt_age_seconds, scheduler_wait_seconds]
        .into_iter()
        .flatten()
        .any(|v| !v.is_finite() || !(0.0..=86400.0).contains(&v))
        || dropped_records.is_some_and(|n| n > 1_000_000)
    {
        return Err(Error);
    }
    let complete = receipt_age_seconds.is_some()
        && scheduler_wait_seconds.is_some()
        && dropped_records.is_some();
    Ok(GapCause {
        version: "protection-gap-cause-v1",
        receipt_age_seconds,
        scheduler_wait_seconds,
        dropped_records,
        evidence: if complete {
            Evidence::Complete
        } else {
            Evidence::Incomplete
        },
        cause: "UNKNOWN",
        inferred: false,
        freshness_limit_seconds: 2,
        limit_relaxed: false,
        repaired: false,
    })
}
fn numeric_text(text: &str) -> Result<String, Error> {
    let text = text.trim();
    if text.is_empty() || text.len() > 128 {
        return Err(Error);
    }
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'_'
            && (i == 0
                || i + 1 == bytes.len()
                || !bytes[i - 1].is_ascii_digit()
                || !bytes[i + 1].is_ascii_digit())
        {
            return Err(Error);
        }
    }
    Ok(text.replace('_', ""))
}
fn number(value: &Value) -> Result<f64, Error> {
    match value {
        Value::Number(n) => n.as_f64().ok_or(Error),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => numeric_text(s)?.parse().map_err(|_| Error),
        _ => Err(Error),
    }
}
fn integer(value: &Value) -> Result<u32, Error> {
    if let Value::String(text) = value {
        let text = numeric_text(text)?;
        let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
        if fraction.bytes().any(|b| b != b'0') || whole.contains(['e', 'E']) || whole.is_empty() {
            return Err(Error);
        }
        let n = whole.parse::<i64>().map_err(|_| Error)?;
        return u32::try_from(n).map_err(|_| Error);
    }
    let n = number(value)?;
    if !n.is_finite() || n.fract() != 0.0 || !(0.0..=1_000_000.0).contains(&n) {
        return Err(Error);
    }
    Ok(n as u32)
}
fn false_literal(value: &Value) -> bool {
    matches!(value, Value::Bool(false)) || value.as_f64() == Some(0.0)
}
impl GapCause {
    /// Explicit historical declaration validation. Duplicate/unknown keys and
    /// bodies over 8KiB refuse; every returned authority/repair field is fixed.
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 8192 {
            return Err(Error);
        }
        let value = crate::native::decode(raw).map_err(|_| Error)?;
        let fields = value.as_object().ok_or(Error)?;
        const KEYS: &[&str] = &[
            "version",
            "receipt_age_seconds",
            "scheduler_wait_seconds",
            "dropped_records",
            "evidence",
            "cause",
            "inferred",
            "freshness_limit_seconds",
            "limit_relaxed",
            "repaired",
        ];
        if fields.keys().any(|key| !KEYS.contains(&key.as_str())) {
            return Err(Error);
        }
        for (key, expected) in [("version", "protection-gap-cause-v1"), ("cause", "UNKNOWN")] {
            if fields
                .get(key)
                .is_some_and(|v| v.as_str() != Some(expected))
            {
                return Err(Error);
            }
        }
        for key in ["inferred", "limit_relaxed", "repaired"] {
            if fields.get(key).is_some_and(|v| !false_literal(v)) {
                return Err(Error);
            }
        }
        if fields
            .get("freshness_limit_seconds")
            .is_some_and(|v| v.as_f64() != Some(2.0))
        {
            return Err(Error);
        }
        fn optional<T>(
            value: Option<&Value>,
            parse: impl FnOnce(&Value) -> Result<T, Error>,
        ) -> Result<Option<T>, Error> {
            value.filter(|v| !v.is_null()).map(parse).transpose()
        }
        let receipt = optional(fields.get("receipt_age_seconds"), number)?;
        let wait = optional(fields.get("scheduler_wait_seconds"), number)?;
        let dropped = optional(fields.get("dropped_records"), integer)?;
        let result = protection_gap_cause(receipt, wait, dropped)?;
        let expected = match result.evidence {
            Evidence::Complete => "COMPLETE",
            Evidence::Incomplete => "INCOMPLETE",
        };
        if fields.get("evidence").and_then(Value::as_str) != Some(expected) {
            return Err(Error);
        }
        Ok(result)
    }
    pub fn evidence_complete(&self) -> bool {
        matches!(self.evidence, Evidence::Complete)
    }
    pub fn execution_authorized(&self) -> bool {
        false
    }
}
