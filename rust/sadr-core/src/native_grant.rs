//! Independent application's grant schema and redacted status only.
//! No file discovery, provider access, refresh, credential export or authority.
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::fmt;
pub const MAX_GRANT_BYTES: usize = 32768;
const ISSUER: &str = "https://auth.x.ai";
// Shared public OAuth client identifier from the existing application protocol.
pub const CLIENT: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
const VERSION: &str = "ind-sadr-xai-grant-v1";
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
    #[serde(default, rename = "refresh_verified_at")]
    _refresh_verified_at: Option<DateTime<FixedOffset>>,
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
    /// Exact authenticated identity only; expiry, lease and invocation gates remain external.
    pub fn matches_binding(&self, account_hash: &str, grant_id: &str) -> bool {
        matches!(self.0.state, State::Authenticated)
            && self.0.account_hash.as_deref() == Some(account_hash)
            && self.0.grant_id == grant_id
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
            provider: "xai_subscription",
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
pub fn missing_status() -> Status {
    Status {
        provider: "xai_subscription",
        state: "not_logged_in",
        entitlement: "UNVERIFIED",
        live_enabled: false,
        inference_enabled: false,
        metadata: None,
    }
}

/// Metadata-only auth projections. Opaque credentials remain discarded; no writer/transport.
pub enum RefreshPlan {
    Cached(GrantRecord),
    Attempt(RefreshAttempt),
}
pub struct RefreshAttempt(GrantRecord);
pub struct Rotation(GrantRecord);
pub struct Confirmation {
    pub record: GrantRecord,
    pub account_matches: bool,
}
impl fmt::Debug for RefreshPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshPlan(redacted, no authority)")
    }
}
impl fmt::Debug for RefreshAttempt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RefreshAttempt(redacted, no authority)")
    }
}
impl fmt::Debug for Rotation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Rotation(redacted, no authority)")
    }
}
impl GrantRecord {
    fn advance(&mut self, now: DateTime<FixedOffset>) -> Result<(), Error> {
        if now < self.0.updated_at {
            return Err(Error);
        }
        let revision = self.0.revision.checked_add(1).ok_or(Error)?;
        self.0.revision = revision;
        self.0.updated_at = now;
        Ok(())
    }
    pub fn auth_projection(&self) -> serde_json::Value {
        let r = &self.0;
        serde_json::json!({"state":match r.state{State::Pending=>"pending",State::Authenticated=>"authenticated",State::Refreshing=>"refreshing",State::ReauthRequired=>"reauth_required",State::LoggedOut=>"logged_out"},"grant_id":r.grant_id,"account_hash":r.account_hash,"revision":r.revision,"login_attempts":r.login_attempts,"refresh_attempts":r.refresh_attempts,"logout_count":r.logout_count,"updated_at":r.updated_at.to_rfc3339(),"expires_at":r.expires_at.map(|x|x.to_rfc3339()),"access_present":r.access_token.is_some(),"refresh_present":r.refresh_token.is_some()})
    }
    pub fn begin_refresh(mut self, now: DateTime<FixedOffset>) -> Result<RefreshPlan, Error> {
        if !matches!(self.0.state, State::Authenticated) || now < self.0.updated_at {
            return Err(Error);
        }
        let threshold = now
            .checked_add_signed(chrono::Duration::seconds(60))
            .ok_or(Error)?;
        if self.0.expires_at.ok_or(Error)? > threshold {
            return Ok(RefreshPlan::Cached(self));
        }
        let attempts = self.0.refresh_attempts.checked_add(1).ok_or(Error)?;
        self.advance(now)?;
        self.0.refresh_attempts = attempts;
        self.0.state = State::Refreshing;
        Ok(RefreshPlan::Attempt(RefreshAttempt(self)))
    }
    pub fn logout_projection(mut self, now: DateTime<FixedOffset>) -> Result<Self, Error> {
        let count = self.0.logout_count.checked_add(1).ok_or(Error)?;
        self.advance(now)?;
        self.0.state = State::LoggedOut;
        self.0.logout_count = count;
        self.0.access_token = None;
        self.0.refresh_token = None;
        self.0.expires_at = None;
        Ok(self)
    }
    /// Only a token-free logged-out tombstone may be serialized by this API.
    pub fn logout_document(&self) -> Result<serde_json::Value, Error> {
        let r = &self.0;
        if !matches!(r.state, State::LoggedOut)
            || r.access_token.is_some()
            || r.refresh_token.is_some()
            || r.expires_at.is_some()
        {
            return Err(Error);
        }
        Ok(
            serde_json::json!({"version":r.version,"issuer":r.issuer,"client_id":r.client_id,"scope":r.scope,"state":"logged_out","grant_id":r.grant_id,"account_hash":r.account_hash,"revision":r.revision,"login_attempts":r.login_attempts,"refresh_attempts":r.refresh_attempts,"logout_count":r.logout_count,"updated_at":r.updated_at.to_rfc3339(),"expires_at":null,"access_token":null,"refresh_token":null}),
        )
    }
}
impl RefreshAttempt {
    pub fn projection(&self) -> serde_json::Value {
        self.0.auth_projection()
    }
    pub fn interrupt(mut self, now: DateTime<FixedOffset>) -> Result<GrantRecord, Error> {
        self.0.advance(now)?;
        self.0.0.state = State::ReauthRequired;
        Ok(self.0)
    }
    pub fn stage_rotation(
        mut self,
        expiry: DateTime<FixedOffset>,
        now: DateTime<FixedOffset>,
    ) -> Result<Rotation, Error> {
        if expiry <= now {
            return Err(Error);
        }
        self.0.advance(now)?;
        self.0.0.state = State::ReauthRequired;
        self.0.0.expires_at = Some(expiry);
        Ok(Rotation(self.0))
    }
}
impl Rotation {
    pub fn projection(&self) -> serde_json::Value {
        self.0.auth_projection()
    }
    pub fn confirm(
        mut self,
        observed_account: &str,
        now: DateTime<FixedOffset>,
    ) -> Result<Confirmation, Error> {
        if !hex(observed_account, 64) {
            return Err(Error);
        }
        self.0.advance(now)?;
        let matches = self.0.0.account_hash.as_deref() == Some(observed_account);
        self.0.0.state = if matches {
            State::Authenticated
        } else {
            State::ReauthRequired
        };
        Ok(Confirmation {
            record: self.0,
            account_matches: matches,
        })
    }
    pub fn interrupt(mut self, now: DateTime<FixedOffset>) -> Result<GrantRecord, Error> {
        self.0.advance(now)?;
        Ok(self.0)
    }
}
