//! OpenAI Codex subscription grant. Separate from the xAI grant schema.
//! No file discovery, refresh, credential export or trading authority.
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::fmt;
pub const MAX_GRANT_BYTES: usize = 32768;
const ISSUER: &str = "https://auth.openai.com";
/// Public Codex device client. This is not an API key.
pub const CLIENT: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Application label for the device login. The device request sends no OAuth scope.
const SCOPE: &str = "openai-codex-device";
const VERSION: &str = "ind-sadr-openai-grant-v1";
fn issuer() -> String {
    ISSUER.into()
}
fn client() -> String {
    CLIENT.into()
}
fn scope() -> String {
    SCOPE.into()
}
fn version() -> String {
    VERSION.into()
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("independent_grant_invalid")
    }
}
impl std::error::Error for Error {}
struct Secret;
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        if text.is_empty() || text.len() > 16384 || !text.bytes().all(|b| (33..=126).contains(&b)) {
            return Err(de::Error::custom("invalid_grant_secret"));
        }
        Ok(Self)
    }
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Pending,
    Authenticated,
    Refreshing,
    ReauthRequired,
    LoggedOut,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(default = "version")]
    version: String,
    #[serde(default = "issuer")]
    issuer: String,
    #[serde(default = "client")]
    client_id: String,
    #[serde(default = "scope")]
    scope: String,
    state: State,
    grant_id: String,
    revision: u64,
    login_attempts: u64,
    #[serde(default)]
    refresh_attempts: u64,
    #[serde(default)]
    logout_count: u64,
    updated_at: DateTime<FixedOffset>,
    expires_at: Option<DateTime<FixedOffset>>,
    account_hash: Option<String>,
    access_token: Option<Secret>,
    refresh_token: Option<Secret>,
}
pub struct GrantRecord(Record);
impl fmt::Debug for GrantRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GrantRecord(redacted, no inference authority)")
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Metadata {
    pub grant_id: String,
    pub account_hash: Option<String>,
    pub expires_at: Option<String>,
    pub revision: u64,
    pub login_attempts: u64,
    pub refresh_attempts: u64,
    pub logout_count: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    pub provider: &'static str,
    pub state: &'static str,
    pub entitlement: &'static str,
    pub live_enabled: bool,
    pub inference_enabled: bool,
    #[serde(flatten)]
    pub metadata: Option<Metadata>,
}
fn hex(s: &str, length: usize) -> bool {
    s.len() == length
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl GrantRecord {
    pub fn parse(raw: &[u8]) -> Result<Self, Error> {
        if raw.len() > MAX_GRANT_BYTES {
            return Err(Error);
        }
        let value = crate::native::decode(raw).map_err(|_| Error)?;
        let r: Record = serde_json::from_value(value).map_err(|_| Error)?;
        if r.version != VERSION
            || r.issuer != ISSUER
            || r.client_id != CLIENT
            || r.scope != SCOPE
            || !hex(&r.grant_id, 32)
            || r.revision == 0
            || r.login_attempts == 0
            || r.account_hash.as_deref().is_some_and(|s| !hex(s, 64))
        {
            return Err(Error);
        }
        if matches!(r.state, State::Authenticated | State::Refreshing)
            && (r.access_token.is_none()
                || r.refresh_token.is_none()
                || r.expires_at.is_none()
                || r.account_hash.is_none())
        {
            return Err(Error);
        }
        if matches!(r.state, State::Pending | State::LoggedOut)
            && (r.access_token.is_some() || r.refresh_token.is_some())
        {
            return Err(Error);
        }
        Ok(Self(r))
    }
    pub fn updated_at(&self) -> DateTime<FixedOffset> {
        self.0.updated_at
    }
    pub fn status(&self, now: DateTime<FixedOffset>) -> Status {
        let r = &self.0;
        let state = match r.state {
            State::Authenticated if r.expires_at.is_some_and(|expiry| expiry <= now) => "expired",
            State::Authenticated => "authenticated",
            State::Refreshing => "refreshing",
            State::Pending => "pending",
            State::ReauthRequired => "reauth_required",
            State::LoggedOut => "logged_out",
        };
        Status {
            provider: "openai_codex",
            state,
            entitlement: "UNVERIFIED",
            live_enabled: false,
            inference_enabled: false,
            metadata: Some(Metadata {
                grant_id: r.grant_id.clone(),
                account_hash: r.account_hash.clone(),
                expires_at: r.expires_at.map(|at| at.to_rfc3339()),
                revision: r.revision,
                login_attempts: r.login_attempts,
                refresh_attempts: r.refresh_attempts,
                logout_count: r.logout_count,
            }),
        }
    }
}
