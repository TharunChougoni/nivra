//! Copy caller-supplied timing measurements into the existing bounded journal
//! record contract. No clocks, trading result, financial freshness or health
//! authority. Canonical receipt offsets are retained, including negative ages.
use crate::timing_journal::{Error, Record};
use nivra_core::time::Timestamp;

/// Measured inputs only. Timestamp text must be RFC3339 or a Python ISO offset with seconds, at microsecond precision;
/// Python's broader datetime text coercions are not inferred here. Span order is
/// retained, with duplicate names refused rather than constructing an ambiguous map.
pub struct Measurements<'a> {
    pub phase: &'a str,
    pub status: &'a str,
    pub started_at: &'a str,
    pub elapsed_ms: f64,
    pub spans_ms: &'a [(&'a str, f64)],
    pub observation_age_seconds: Option<f64>,
    pub predicate: Option<&'a str>,
    pub capture_received_at: Option<&'a str>,
    pub completed_at: Option<&'a str>,
}
impl std::fmt::Debug for Measurements<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectionTimingMeasurements(redacted, no authority)")
    }
}
pub(crate) struct Fields {
    pub phase: String,
    pub status: String,
    pub started_at: String,
    pub started_ist: String,
    pub elapsed_ms: f64,
    pub spans_ms: Vec<(String, f64)>,
    pub observation_age_seconds: Option<f64>,
    pub predicate: Option<String>,
    pub capture_received_at: Option<String>,
    pub completed_at: Option<String>,
    pub receipt_age_seconds: Option<f64>,
}
/// The builder owns copies only; every diagnostic invariant is enforced by the
/// existing Record validator. Partial receipt measurements continue to refuse.
pub fn build(input: Measurements<'_>) -> Result<Record, Error> {
    if !Record::measurement_labels_valid(&input) {
        return Err(Error);
    }
    let start = Timestamp::parse_isoformat_offset_seconds(input.started_at).map_err(|_| Error)?;
    let receipt = input.capture_received_at.map(canonical_stamp).transpose()?;
    let completed = input.completed_at.map(canonical_stamp).transpose()?;
    let receipt_age_seconds = match (receipt.as_ref(), completed.as_ref()) {
        (Some((r, _)), Some((c, _))) => Some(micros_to_seconds(
            c.elapsed_micros_since(*r).map_err(|_| Error)?,
        )),
        _ => None,
    };
    Record::from_measurement(Fields {
        phase: input.phase.into(),
        status: input.status.into(),
        started_at: utc_isoformat(start),
        started_ist: start.to_ist_isoformat(),
        elapsed_ms: input.elapsed_ms,
        spans_ms: input
            .spans_ms
            .iter()
            .map(|(k, v)| ((*k).into(), *v))
            .collect(),
        observation_age_seconds: input.observation_age_seconds,
        predicate: input.predicate.map(str::to_owned),
        capture_received_at: receipt.map(|(_, s)| s),
        completed_at: completed.map(|(_, s)| s),
        receipt_age_seconds,
    })
}
fn utc_isoformat(stamp: Timestamp) -> String {
    // Timestamp Display is UTC with Z and minimal millisecond/microsecond
    // precision; Python datetime.isoformat uses +00:00 and six nonzero digits.
    let text = stamp.to_string();
    let without_z = text.strip_suffix('Z').unwrap_or(&text);
    format!("{}+00:00", six_digits(without_z))
}
fn six_digits(s: &str) -> String {
    if let Some((whole, fraction)) = s.split_once('.') {
        if fraction.bytes().all(|b| b == b'0') {
            whole.into()
        } else {
            format!("{whole}.{fraction:0<6}")
        }
    } else {
        s.into()
    }
}
fn canonical_stamp(s: &str) -> Result<(Timestamp, String), Error> {
    let instant = Timestamp::parse_isoformat_offset_seconds(s).map_err(|_| Error)?;
    let (time, offset) = if let Some(time) = s.strip_suffix('Z').or_else(|| s.strip_suffix('z')) {
        (time, "+00:00".to_owned())
    } else {
        let seconds = s.len() >= 9
            && s.as_bytes()[s.len() - 3] == b':'
            && matches!(s.as_bytes()[s.len() - 9], b'+' | b'-');
        let split = s.len() - if seconds { 9 } else { 6 };
        let (t, o) = s.split_at(split);
        let o = if seconds && &o[6..] == ":00" {
            &o[..6]
        } else {
            o
        };
        (t, if o == "-00:00" { "+00:00" } else { o }.to_owned())
    };
    let mut time = six_digits(time);
    if time.len() > 10 {
        time.replace_range(10..11, "T");
    }
    Ok((instant, format!("{time}{offset}")))
}
/// Correctly rounded integer microseconds / 1,000,000, matching Python
/// timedelta.total_seconds. Casting the integer to f64 first can double-round
/// long intervals; retain integer precision through the final rounding step.
pub(crate) fn micros_to_seconds(micros: i64) -> f64 {
    if micros == 0 {
        return 0.0;
    }
    let n = micros.unsigned_abs() as u128;
    let denominator = 1_000_000_u128;
    let mut exponent = n.ilog2() as i32 - denominator.ilog2() as i32;
    let below = if exponent >= 0 {
        n < (denominator << exponent)
    } else {
        (n << (-exponent)) < denominator
    };
    if below {
        exponent -= 1;
    }
    let shift = 52 - exponent;
    let scaled = if shift >= 0 { n << shift } else { n };
    let divisor = if shift >= 0 {
        denominator
    } else {
        denominator << (-shift)
    };
    let mut significand = scaled / divisor;
    let remainder = scaled % divisor;
    if remainder * 2 > divisor || (remainder * 2 == divisor && significand & 1 == 1) {
        significand += 1;
    }
    if significand == 1_u128 << 53 {
        significand >>= 1;
        exponent += 1;
    }
    let bits = ((exponent + 1023) as u64) << 52
        | (significand as u64 - (1_u64 << 52))
        | if micros < 0 { 1_u64 << 63 } else { 0 };
    f64::from_bits(bits)
}
