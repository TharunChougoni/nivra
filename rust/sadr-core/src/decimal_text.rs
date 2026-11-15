//! Exact decimal text for declared scientific metadata, preserving Python Decimal spelling.
//! This does not certify fees, accept nonfinite values or authorize execution.
use rust_decimal::Decimal;
use serde::{Serialize, Serializer};
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("exact_declared_decimal_invalid")
    }
}
impl std::error::Error for Error {}
pub struct ExactDecimal {
    value: Decimal,
    text: String,
}
impl ExactDecimal {
    pub fn parse(input: &str) -> Result<Self, Error> {
        if input.len() > 32768 {
            return Err(Error);
        }
        // Python Decimal permits underscores in its string constructor. No evaluation.
        let input = input.trim().replace('_', "");
        let (negative, unsigned) = match input.as_bytes().first() {
            Some(b'-') => (true, &input[1..]),
            Some(b'+') => (false, &input[1..]),
            _ => (false, input.as_str()),
        };
        let mut parts = unsigned.split(['e', 'E']);
        let base = parts.next().ok_or(Error)?;
        let exponent = if let Some(e) = parts.next() {
            e.parse::<i32>().map_err(|_| Error)?
        } else {
            0
        };
        if parts.next().is_some() || exponent.unsigned_abs() > 32768 {
            return Err(Error);
        }
        let mut parts = base.split('.');
        let whole = parts.next().ok_or(Error)?;
        let fraction = parts.next().unwrap_or("");
        if parts.next().is_some()
            || (whole.is_empty() && fraction.is_empty())
            || !whole
                .bytes()
                .chain(fraction.bytes())
                .all(|b| b.is_ascii_digit())
        {
            return Err(Error);
        }
        let coefficient = format!("{whole}{fraction}");
        let coefficient = coefficient.trim_start_matches('0');
        let digits = if coefficient.is_empty() {
            "0"
        } else {
            coefficient
        };
        let exponent = exponent
            .checked_sub(i32::try_from(fraction.len()).map_err(|_| Error)?)
            .ok_or(Error)?;
        let adjusted = exponent
            .checked_add(i32::try_from(digits.len()).map_err(|_| Error)? - 1)
            .ok_or(Error)?;
        let sign = if negative { "-" } else { "" };
        let text = if exponent > 0 || adjusted < -6 {
            let mantissa = if digits.len() == 1 {
                digits.to_owned()
            } else {
                format!("{}.{}", &digits[..1], &digits[1..])
            };
            format!("{sign}{mantissa}E{adjusted:+}")
        } else if exponent == 0 {
            format!("{sign}{digits}")
        } else {
            let place = i32::try_from(digits.len())
                .map_err(|_| Error)?
                .checked_add(exponent)
                .ok_or(Error)?;
            if place <= 0 {
                format!("{sign}0.{}{digits}", "0".repeat((-place) as usize))
            } else {
                let place = place as usize;
                format!("{sign}{}.{}", &digits[..place], &digits[place..])
            }
        };
        let value = if digits == "0" {
            Decimal::ZERO
        } else {
            let trimmed = digits.trim_end_matches('0');
            let trailing = digits.len() - trimmed.len();
            let exponent = exponent
                .checked_add(i32::try_from(trailing).map_err(|_| Error)?)
                .ok_or(Error)?;
            let mut coefficient = trimmed.parse::<i128>().map_err(|_| Error)?;
            let scale = if exponent >= 0 {
                coefficient = coefficient
                    .checked_mul(10_i128.checked_pow(exponent as u32).ok_or(Error)?)
                    .ok_or(Error)?;
                0
            } else {
                exponent.checked_neg().ok_or(Error)? as u32
            };
            if negative {
                coefficient = coefficient.checked_neg().ok_or(Error)?;
            }
            Decimal::try_from_i128_with_scale(coefficient, scale).map_err(|_| Error)?
        };
        Ok(Self { value, text })
    }
    pub fn value(&self) -> Decimal {
        self.value
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}
impl fmt::Debug for ExactDecimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ExactDeclaredDecimal(redacted)")
    }
}
impl Serialize for ExactDecimal {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.text)
    }
}
