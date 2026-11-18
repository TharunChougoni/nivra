//! Bounded display evidence; original run artifacts remain authoritative.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
thread_local! { static BLOCK_NEXT_START: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RunIndexLine {
    pub v: u8,
    pub run: String,
    pub role: String,
    pub model: String,
    pub trigger_kind: Option<String>,
    pub started_ms: i64,
    pub duration_ms: i64,
    pub outcome: String,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub cost_estimate_inr: Option<String>,
    pub tool_calls: usize,
    pub tool_failures: usize,
    pub instruments: Vec<String>,
    pub bytes: u64,
    pub mode: String,
    #[serde(default)]
    pub profile_sha256: Option<String>,
}

// Unlike the legacy history projection, transcripts retain non-secret account
// and input fields. Credential keys share the same denylist at write/read time.
pub(super) fn redact(value: &Value) -> Value {
    let mut value = value.clone();
    redact_in_place(&mut value);
    value
}

/// Credential assignments also occur in prose, environment-style text and
/// headers. Inspect the key immediately before each separator; mentioning a
/// credential key without an assignment is not itself a secret.
fn credential_assignment(text: &str) -> bool {
    text.split_inclusive(['=', ':']).any(|segment| {
        let Some(prefix) = segment.strip_suffix(['=', ':']) else {
            return false;
        };
        let prefix = prefix.trim_end().trim_end_matches(['\'', '"', '`']);
        let key = prefix
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .next()
            .unwrap_or_default()
            .trim_start_matches('-');
        super::model_history::secret_key(key)
    })
}
fn redact_in_place(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|key, _| !super::model_history::secret_key(key));
            for value in fields.values_mut() {
                redact_in_place(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_in_place(value);
            }
        }
        Value::String(text)
            if text.contains("Bearer ")
                || text.contains("sk-")
                || text.contains("access_token")
                || text.contains("refresh_token")
                || text.contains("-----BEGIN PRIVATE KEY") =>
        {
            *text = "[redacted]".into()
        }
        Value::String(text) if text.trim_start().starts_with(['{', '[']) => {
            if let Ok(mut nested) = serde_json::from_str::<Value>(text) {
                let original = nested.clone();
                redact_in_place(&mut nested);
                if nested != original {
                    *text = nested.to_string();
                }
            } else if credential_assignment(text) {
                *text = "[redacted]".into();
            }
        }
        Value::String(text) if credential_assignment(text) => *text = "[redacted]".into(),
        _ => {}
    }
}

#[cfg(test)]
mod tests {}

#[cfg(test)]
mod index_tests {}

#[cfg(test)]
thread_local! { static RUN_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod list_tests {}
