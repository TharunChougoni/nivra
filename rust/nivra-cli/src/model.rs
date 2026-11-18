//! Configuration compatibility for archived planner policies and host migration.
#[cfg(test)]
fn now() -> chrono::DateTime<chrono::FixedOffset> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    chrono::DateTime::from_timestamp(duration.as_secs() as i64, duration.subsec_nanos())
        .expect("clock range")
        .fixed_offset()
}

use nivra_cli::native_transport::Transport;
#[cfg(test)]
use nivra_core::native_grant::SCOPE;
#[cfg(test)]
use nivra_core::{
    native,
    native_grant::{CLIENT, GrantRecord},
};
use serde::Deserialize;
#[cfg(test)]
pub(crate) const LUNA: &str = "gpt-6-luna";
#[cfg(test)]
pub(crate) const FAST_VARIANT: &str = "grok-4.7-build-fast";
#[cfg(test)]
use serde_json::{Value, json};
#[cfg(test)]
use sha2::{Digest, Sha256};
use std::path::PathBuf;
#[cfg(test)]
use std::{
    path::Path,
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default, rename = "alert_webhook_file")]
    _alert_webhook_file: Option<PathBuf>,
    #[serde(default)]
    transport: Transport,
    #[serde(default, rename = "direct_oauth_approval_file")]
    _direct_oauth_approval_file: Option<PathBuf>,
    #[serde(default, rename = "proxy_client_version")]
    _proxy_client_version: Option<String>,
    #[serde(default = "default_grant")]
    grant_file: PathBuf,
    #[serde(default, rename = "model_id")]
    _model_id: String,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    #[serde(default, rename = "enabled")]
    _enabled: bool,
}
fn default_timeout() -> u64 {
    20000
}
fn default_grant() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config/nivra/xai-grant")
}
#[cfg(test)]
fn config(path: &Path) -> Result<Config> {
    let bytes = crate::read_bounded(path, 16384).map_err(|_| "model_config_unreadable")?;
    parse_config(&bytes)
}
/// Validate model credential config bytes without reading the grant itself.
pub(crate) fn parse_config(bytes: &[u8]) -> Result<Config> {
    let c: Config = serde_json::from_slice(bytes).map_err(|_| "model_config_invalid")?;
    if !(1..=120000).contains(&c.timeout_ms) {
        return Err("model_timeout_invalid".into());
    }
    if c.transport == Transport::OpenaiCodex && c.grant_file == default_grant() {
        return Err("openai_grant_path_required".into());
    }
    nivra_store::native_grant::file_path_ok(&c.grant_file).map_err(|_| "grant_path_invalid")?;
    Ok(c)
}
#[cfg(test)]
pub fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod catalog;
mod fallback;
mod grant;
#[cfg(test)]
mod probe;
#[cfg(test)]
pub(crate) use catalog::*;
pub(crate) use fallback::*;
pub(crate) use grant::*;
#[cfg(test)]
pub(crate) use probe::*;

// Redact before truncating: neither a clipped credential nor terminal escapes may leak.
#[cfg(test)]
fn safe_text(raw: &str, token: &str, limit: usize) -> String {
    let raw = if token.is_empty() {
        raw.to_owned()
    } else {
        raw.replace(token, "[redacted]")
    };
    let mut hide_next = false;
    let mut words = Vec::new();
    for word in raw.split_whitespace() {
        let lower = word.to_ascii_lowercase();
        let label = [
            "token",
            "bearer",
            "authorization",
            "secret",
            "api_key",
            "api-key",
            "password",
            "credential",
            "grant",
        ]
        .iter()
        .any(|s| lower.contains(s));
        let suspicious = word.len() >= 24
            || word.contains("eyJ")
            || lower.contains("xai-")
            || lower.contains("sk-")
            || word.contains('=')
            || word.contains('@');
        words.push(if hide_next || label || suspicious {
            "[redacted]".to_owned()
        } else {
            word.chars()
                .filter(|c| {
                    !c.is_control()
                        && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
                })
                .collect()
        });
        hide_next = label;
    }
    words.join(" ").chars().take(limit).collect()
}
#[cfg(test)]
fn check_http(status: u16, bytes: &[u8], token: &str) -> Result<()> {
    check_http_secrets(status, bytes, &[token])
}
#[cfg(test)]
fn check_http_secrets(status: u16, bytes: &[u8], secrets: &[&str]) -> Result<()> {
    if (200..300).contains(&status) {
        return Ok(());
    }
    Err(http_error(status, bytes, secrets))
}
#[cfg(test)]
fn http_error(status: u16, bytes: &[u8], secrets: &[&str]) -> String {
    // Only provider error fields, never the raw body, headers, account or request.
    let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    let error = &value["error"];
    let fields = [
        error.get("code"),
        error.get("type"),
        error.get("message"),
        error.as_str().map(|_| error),
        value.get("code"),
        value.get("message"),
        value.get("error_description"),
    ];
    let detail = fields
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    let detail = secrets
        .iter()
        .filter(|s| !s.is_empty())
        .fold(detail, |text, secret| text.replace(secret, "[redacted]"));
    let detail = safe_text(&detail, "", 300);
    if detail.is_empty() {
        format!("model_http_{status}")
    } else {
        format!("model_http_{status} {detail}")
    }
}

#[cfg(test)]
mod fallback_tests;
#[cfg(test)]
mod verification_tests;

#[cfg(test)]
mod refresh_readiness_tests;
