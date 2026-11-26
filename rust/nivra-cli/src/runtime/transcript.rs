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
    redact_with_findings(value).0
}

/// Most findings one redaction keeps; `count` still counts all of them.
const MAX_FINDINGS: usize = 16;
/// Substrings that redact a whole string, with the class each one reports.
/// Class names avoid the markers themselves, so a diagnostics line that
/// names a class survives its own redaction.
const SECRET_MARKERS: [(&str, &str); 5] = [
    ("Bearer ", "bearer_prefix"),
    ("sk-", "sk_prefix"),
    ("access_token", "oauth_access_word"),
    ("refresh_token", "oauth_refresh_word"),
    ("-----BEGIN PRIVATE KEY", "private_key_block"),
];

/// Where the secret guard matched, so a false positive can be diagnosed
/// without the secret: the pattern class, the field path, the string's
/// length and the match offset in bytes. Never the matched text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct Finding {
    pub class: &'static str,
    pub field: String,
    pub length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
}

#[derive(Debug, Default, Serialize)]
pub(super) struct Findings {
    pub count: usize,
    pub matches: Vec<Finding>,
}

impl Findings {
    fn push(&mut self, finding: Finding) {
        self.count += 1;
        if self.matches.len() < MAX_FINDINGS {
            self.matches.push(finding);
        }
    }
}

/// `redact`, plus where each pattern matched.
pub(super) fn redact_with_findings(value: &Value) -> (Value, Findings) {
    let mut value = value.clone();
    let mut findings = Findings::default();
    redact_in_place(&mut value, "$", &mut findings);
    (value, findings)
}

/// A field name as a finding reports it: model-authored keys that are not
/// short plain identifiers are replaced, so a path never carries free text.
fn segment(key: &str) -> &str {
    if !key.is_empty()
        && key.len() <= 40
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        key
    } else {
        "?"
    }
}

/// Credential assignments also occur in prose, environment-style text and
/// headers. Inspect the key immediately before each separator; mentioning a
/// credential key without an assignment is not itself a secret. Returns the
/// separator's byte offset.
fn credential_assignment(text: &str) -> Option<usize> {
    let mut end = 0;
    for segment in text.split_inclusive(['=', ':']) {
        end += segment.len();
        let Some(prefix) = segment.strip_suffix(['=', ':']) else {
            continue;
        };
        let prefix = prefix.trim_end().trim_end_matches(['\'', '"', '`']);
        let key = prefix
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .next()
            .unwrap_or_default()
            .trim_start_matches('-');
        if super::model_history::secret_key(key) {
            return Some(end - 1);
        }
    }
    None
}
fn redact_in_place(value: &mut Value, path: &str, findings: &mut Findings) {
    match value {
        Value::Object(fields) => {
            fields.retain(|key, value| {
                let secret = super::model_history::secret_key(key);
                if secret {
                    findings.push(Finding {
                        class: "secret_key_field",
                        field: path.to_owned(),
                        length: value
                            .as_str()
                            .map_or_else(|| value.to_string().len(), str::len),
                        offset: None,
                    });
                }
                !secret
            });
            for (key, value) in fields.iter_mut() {
                redact_in_place(value, &format!("{path}.{}", segment(key)), findings);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter_mut().enumerate() {
                redact_in_place(value, &format!("{path}[{index}]"), findings);
            }
        }
        Value::String(text)
            if SECRET_MARKERS
                .iter()
                .any(|(marker, _)| text.contains(marker)) =>
        {
            for (marker, class) in SECRET_MARKERS {
                if let Some(offset) = text.find(marker) {
                    findings.push(Finding {
                        class,
                        field: path.to_owned(),
                        length: text.len(),
                        offset: Some(offset),
                    });
                }
            }
            *text = "[redacted]".into()
        }
        Value::String(text) if text.trim_start().starts_with(['{', '[']) => {
            if let Ok(mut nested) = serde_json::from_str::<Value>(text) {
                let original = nested.clone();
                redact_in_place(&mut nested, &format!("{path}#json"), findings);
                if nested != original {
                    *text = nested.to_string();
                }
            } else if let Some(offset) = credential_assignment(text) {
                findings.push(Finding {
                    class: "credential_assignment",
                    field: path.to_owned(),
                    length: text.len(),
                    offset: Some(offset),
                });
                *text = "[redacted]".into();
            }
        }
        Value::String(text) => {
            if let Some(offset) = credential_assignment(text) {
                findings.push(Finding {
                    class: "credential_assignment",
                    field: path.to_owned(),
                    length: text.len(),
                    offset: Some(offset),
                });
                *text = "[redacted]".into()
            }
        }
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
