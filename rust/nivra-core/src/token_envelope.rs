//! Supplied token-response validation only. Opaque values are discarded, never exported.
use chrono::{DateTime, Duration, FixedOffset};
use serde_json::Value;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("token_response_invalid")
    }
}
impl std::error::Error for Error {}
/// Validation metadata is not an access grant or inference authority.
#[derive(Debug, PartialEq, Eq)]
pub struct Metadata {
    pub expires_at: DateTime<FixedOffset>,
}
pub fn validate(raw: &[u8], now: DateTime<FixedOffset>) -> Result<Metadata, Error> {
    if raw.len() > 32768 {
        return Err(Error);
    }
    let body = crate::native::decode(raw).map_err(|_| Error)?;
    let body = body.as_object().ok_or(Error)?;
    if !body
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
    {
        return Err(Error);
    }
    for key in ["access_token", "refresh_token"] {
        let token = body.get(key).and_then(Value::as_str).ok_or(Error)?;
        if token.is_empty()
            || token.len() > 16384
            || !token.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(Error);
        }
    }
    let seconds = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .ok_or(Error)?;
    if !(1..=604800).contains(&seconds) {
        return Err(Error);
    }
    if let Some(scope) = body.get("scope") {
        let scope = scope.as_str().ok_or(Error)?;
        let actual: std::collections::BTreeSet<_> = scope.split_whitespace().collect();
        let expected: std::collections::BTreeSet<_> =
            "openid profile email offline_access grok-cli:access api:access"
                .split_whitespace()
                .collect();
        if actual != expected {
            return Err(Error);
        }
    }
    let expires_at = now
        .checked_add_signed(Duration::seconds(seconds as i64))
        .ok_or(Error)?;
    Ok(Metadata { expires_at })
}
/// Codex device exchange. A present scope may only contain the OpenAI login words.
/// Absence is accepted because the device request sends no scope parameter.
pub fn validate_codex(raw: &[u8], now: DateTime<FixedOffset>) -> Result<Metadata, Error> {
    if raw.len() > 32768 {
        return Err(Error);
    }
    let body = crate::native::decode(raw).map_err(|_| Error)?;
    let body = body.as_object().ok_or(Error)?;
    if !body
        .get("token_type")
        .and_then(Value::as_str)
        .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
    {
        return Err(Error);
    }
    for key in ["access_token", "refresh_token"] {
        let token = body.get(key).and_then(Value::as_str).ok_or(Error)?;
        if token.is_empty()
            || token.len() > 16384
            || !token.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(Error);
        }
    }
    if let Some(scope) = body.get("scope") {
        let scope = scope.as_str().ok_or(Error)?;
        let allowed = ["openid", "profile", "email", "offline_access"];
        let words: Vec<_> = scope.split_whitespace().collect();
        if words.is_empty() || words.iter().any(|word| !allowed.contains(word)) {
            return Err(Error);
        }
    }
    let seconds = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .ok_or(Error)?;
    // The 2026-10-18 Codex device exchange returned 864000 seconds (10 days).
    // The 7-day xAI bound rejects that response before a grant can be stored.
    if !(1..=864_000).contains(&seconds) {
        return Err(Error);
    }
    let expires_at = now
        .checked_add_signed(Duration::seconds(seconds as i64))
        .ok_or(Error)?;
    Ok(Metadata { expires_at })
}
