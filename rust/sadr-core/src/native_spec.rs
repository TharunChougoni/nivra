//! Immutable native-only policy admission. No provider access or invocation authority.
use crate::{
    native_grant::GrantRecord,
    native_request::{self, Envelope, Limits},
};
use chrono::{DateTime, FixedOffset};
use serde::Deserialize;
use serde_json::Value;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Invalid,
    AuthFailed,
    ModelUnavailable,
    UnverifiedPricing,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Invalid => "native_spec_invalid",
            Self::AuthFailed => "auth_failed",
            Self::ModelUnavailable => "model_unavailable",
            Self::UnverifiedPricing => "pricing_unverified",
        })
    }
}
impl std::error::Error for Error {}
fn v1() -> String {
    "v1".into()
}
fn unverified() -> String {
    "unverified".into()
}
fn timeout() -> u32 {
    30
}
fn input() -> usize {
    24000
}
fn output() -> u32 {
    1200
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    provider: String,
    model: String,
    subscription_account_hash: String,
    #[serde(default = "v1")]
    prompt_version: String,
    #[serde(default = "v1")]
    schema_version: String,
    #[serde(default = "timeout")]
    timeout_seconds: u32,
    #[serde(default = "input")]
    max_input_chars: usize,
    #[serde(default = "output")]
    max_output_tokens: u32,
    #[serde(default)]
    max_cost_micro_usd: u64,
    #[serde(default = "unverified")]
    pricing_asof: String,
}
#[derive(Clone, PartialEq, Eq)]
pub struct NativeSpec(Raw);
impl fmt::Debug for NativeSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeModelSpec(redacted, no authority)")
    }
}
fn hash(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl NativeSpec {
    pub fn parse(raw: &[u8], bound_account: &str) -> Result<Self, Error> {
        if raw.len() > 32768 {
            return Err(Error::Invalid);
        }
        let value = crate::native::decode(raw).map_err(|_| Error::Invalid)?;
        let r: Raw = serde_json::from_value(value).map_err(|_| Error::Invalid)?;
        if r.provider != "xai_subscription"
            || r.max_cost_micro_usd != 0
            || !hash(bound_account)
            || r.subscription_account_hash != bound_account
            || r.model.is_empty()
            || r.model.len() > 120
            || !r
                .model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
            || !(1..=30).contains(&r.timeout_seconds)
            || !(1..=1000000).contains(&r.max_input_chars)
            || r.max_output_tokens == 0
        {
            return Err(Error::Invalid);
        }
        Ok(Self(r))
    }
    pub fn exact_model(&self) -> &str {
        &self.0.model
    }
    pub fn account_hash(&self) -> &str {
        &self.0.subscription_account_hash
    }
    pub fn timeout_seconds(&self) -> u32 {
        self.0.timeout_seconds
    }
    /// A configured marker is not proof of genuine prices, entitlement or provider quota.
    pub fn pricing_configured(&self) -> bool {
        self.0.pricing_asof != "unverified"
    }
    pub fn check_grant(
        &self,
        grant: &GrantRecord,
        grant_id: &str,
        now: DateTime<FixedOffset>,
    ) -> Result<(), Error> {
        if !grant.matches_binding(self.account_hash(), grant_id)
            || grant.status(now).state != "authenticated"
        {
            return Err(Error::AuthFailed);
        }
        Ok(())
    }
    pub fn check_catalog(&self, raw: &[u8]) -> Result<(), Error> {
        let ids = crate::native::catalog(raw).map_err(|_| Error::Invalid)?;
        if !ids.iter().any(|id| id == self.exact_model()) {
            return Err(Error::ModelUnavailable);
        }
        Ok(())
    }
    pub fn request(&self, prompt: &str, schema: &Value) -> Result<Envelope, Error> {
        if !self.pricing_configured() {
            return Err(Error::UnverifiedPricing);
        }
        native_request::build(
            self.exact_model(),
            prompt,
            schema,
            Limits {
                max_input_chars: self.0.max_input_chars,
                max_output_tokens: self.0.max_output_tokens,
            },
        )
        .map_err(|_| Error::Invalid)
    }
}
