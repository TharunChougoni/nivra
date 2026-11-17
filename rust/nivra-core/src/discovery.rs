//! Validate supplied discovery evidence only. No discovery fetch or auth authority.
use serde_json::Value;
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("discovery_changed")
    }
}
impl std::error::Error for Error {}
/// A valid document establishes protocol compatibility, never entitlement or readiness.
pub fn validate(status: u16, raw: &[u8]) -> Result<(), Error> {
    if status != 200 || raw.len() > 32768 {
        return Err(Error);
    }
    let value = crate::native::decode(raw).map_err(|_| Error)?;
    let object = value.as_object().ok_or(Error)?;
    for (key, expected) in [
        ("issuer", "https://auth.x.ai"),
        (
            "device_authorization_endpoint",
            "https://auth.x.ai/oauth2/device/code",
        ),
        ("token_endpoint", "https://auth.x.ai/oauth2/token"),
        ("userinfo_endpoint", "https://auth.x.ai/oauth2/userinfo"),
    ] {
        if object.get(key).and_then(Value::as_str) != Some(expected) {
            return Err(Error);
        }
    }
    for (key, required) in [
        (
            "grant_types_supported",
            &[
                "urn:ietf:params:oauth:grant-type:device_code",
                "refresh_token",
            ][..],
        ),
        ("token_endpoint_auth_methods_supported", &["none"][..]),
        (
            "scopes_supported",
            &[
                "openid",
                "profile",
                "email",
                "offline_access",
                "grok-cli:access",
                "api:access",
            ][..],
        ),
    ] {
        let items = object.get(key).and_then(Value::as_array).ok_or(Error)?;
        if items.len() > 256
            || items
                .iter()
                .any(|v| v.as_str().is_none_or(|s| s.len() > 256))
            || required
                .iter()
                .any(|r| !items.iter().any(|v| v.as_str() == Some(r)))
        {
            return Err(Error);
        }
    }
    Ok(())
}
