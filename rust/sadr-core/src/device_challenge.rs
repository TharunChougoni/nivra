//! Offline validation only; no login, polling, credential material or provider access.
use serde::Deserialize;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("device_challenge_invalid")
    }
}
impl std::error::Error for Error {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    verification_uri: String,
    user_code: String,
    expires_in: u16,
}
/// A validated display challenge confers no inference or trading authority.
/// Display fields require deliberate access; Debug never reveals the code.
pub struct DeviceChallenge(Record);
impl fmt::Debug for DeviceChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceChallenge(redacted, no authority)")
    }
}
impl DeviceChallenge {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > 4096 {
            return Err(Error);
        }
        let value = crate::native::decode(raw).map_err(|_| Error)?;
        let record: Record = serde_json::from_value(value).map_err(|_| Error)?;
        let (scheme, target) = record.verification_uri.split_once("://").ok_or(Error)?;
        let (host, path) = target.split_once('/').ok_or(Error)?;
        // No URL normalization: exact public authority, literal bounded path only.
        if !scheme.eq_ignore_ascii_case("https")
            || ![
                "auth.x.ai",
                "accounts.x.ai",
                "grok.com",
                "x.ai",
                "auth.openai.com",
            ]
            .contains(&host)
            || path.len() > 160
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'/'))
            || !(4..=64).contains(&record.user_code.len())
            || !record
                .user_code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || !(1..=1800).contains(&record.expires_in)
        {
            return Err(Error);
        }
        Ok(Self(record))
    }
    pub fn verification_uri(&self) -> &str {
        &self.0.verification_uri
    }
    pub fn user_code(&self) -> &str {
        &self.0.user_code
    }
    pub fn expires_in_seconds(&self) -> u16 {
        self.0.expires_in
    }
}
