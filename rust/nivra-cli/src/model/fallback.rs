//! Archived fallback configuration and offline response fixtures.
use super::*;
use nivra_cli::native_transport::FallbackTransport;

/// Application-owned fallback credential config, referenced by the profile's
/// `model_config`. It names a private key file; it never holds the key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FallbackConfig {
    #[serde(rename = "transport")]
    _transport: FallbackTransport,
    api_key_file: PathBuf,
}

pub(crate) fn parse_fallback_config(bytes: &[u8]) -> Result<FallbackConfig> {
    let c: FallbackConfig = serde_json::from_slice(bytes).map_err(|_| "fallback_config_invalid")?;
    nivra_store::native_grant::file_path_ok(&c.api_key_file)
        .map_err(|_| "fallback_key_path_invalid")?;
    Ok(c)
}

/// Every reason code that may switch one call, in operator-facing order.
/// Readiness publishes this set so the reviewed policy is visible without
/// reading the source; each switch records exactly one of these static codes
/// in evidence, logs and the alert, never provider text.
#[cfg(test)]
pub(crate) const FALLBACK_REASONS: [&str; 5] = [
    "usage_limit_reached",
    "rate_limited",
    "quota_exceeded",
    "entitlement_auth_failed",
    "response_validation_rejected",
];

/// The bounded class of a primary failure that may switch one call to the
/// reviewed fallback (#245, #21), or `None` when the failure is anything else.
/// `primary` is the primary call's transport evidence and bounds the two
/// provider-shape classes by stage:
///
/// - `usage_limit_reached`, `rate_limited`, `quota_exceeded`: HTTP 429 after
///   the bounded retries or a provider limit error.
/// - `entitlement_auth_failed`: HTTP 401/403 from the catalog or inference
///   request (provider stage `catalog`, `inference` or `http_response`), for
///   example an expired subscription or a revoked grant.
/// - `response_validation_rejected`: an opaque HTTP 200 whose body is a
///   provider error object or stream failure at response validation
///   (`model_http_200`), for example a provider that rejects the supervisor
///   prompt.
///
/// Timeouts, HTTP 5xx, cancellation, malformed output from a real response, a
/// grant-refresh auth failure (`grant_owner`) and local refusals never switch.
#[cfg(test)]
pub(crate) fn fallback_reason(error: &str, primary: &Value) -> Option<&'static str> {
    let code = error.split_whitespace().next()?;
    if !code.starts_with("model_http_") {
        return None;
    }
    let lower = error.to_ascii_lowercase();
    if lower.contains("usage_limit") || lower.contains("usage limit") {
        Some("usage_limit_reached")
    } else if lower.contains("quota") {
        Some("quota_exceeded")
    } else if code == "model_http_429"
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
    {
        Some("rate_limited")
    } else if matches!(code, "model_http_401" | "model_http_403")
        && matches!(
            primary["stage"].as_str(),
            Some("catalog" | "inference" | "http_response")
        )
    {
        Some("entitlement_auth_failed")
    } else if code == "model_http_200" && primary["stage"] == "response_validation" {
        Some("response_validation_rejected")
    } else {
        None
    }
}
