//! Bounded, expiring, audited orchestrator directives.
//!
//! A directive is context for the next Luna or decide envelope. It never creates,
//! sizes or blocks an order. The execution path does not read this module.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) const STORE: &str = "model/directives.ndjson";
/// The one decision owner's explicitly requested operator context.
pub(crate) const OWNER_TARGET: &str = "owner";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    pub max_active: usize,
    pub max_text_chars: usize,
    pub max_keys: usize,
    pub max_ttl_ms: i64,
    pub targets: Vec<&'static str>,
    pub require_ack: bool,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_active: 3,
            max_text_chars: 240,
            max_keys: 5,
            max_ttl_ms: 1_800_000,
            // `owner` is reserved for the local operator CLI. A model response
            // can never nominate itself as the owner instruction channel.
            targets: vec!["luna", "decide", "both"],
            require_ack: false,
        }
    }
}

impl Limits {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Directive {
    pub id: String,
    pub text: String,
    pub keys: Vec<String>,
    pub target: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// Operator request identity for an `owner` instruction. Model-authored
    /// directives leave these absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authorization_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Record {
    Issued { directive: Directive },
    Superseded { id: String, at_ms: i64 },
    Expired { id: String, at_ms: i64 },
    Acknowledged { id: String, at_ms: i64, by: String },
}

fn path(root: &Path) -> PathBuf {
    root.join(STORE)
}

pub(crate) fn active(root: &Path, now: i64, target: &str) -> Vec<Directive> {
    let Ok(raw) = fs::read_to_string(path(root)) else {
        return Vec::new();
    };
    let mut live: Vec<Directive> = Vec::new();
    let mut dead = std::collections::BTreeSet::new();
    for line in raw.lines() {
        let Ok(record) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        match record {
            Record::Issued { directive } => live.push(directive),
            Record::Superseded { id, .. } | Record::Expired { id, .. } => {
                dead.insert(id);
            }
            Record::Acknowledged { .. } => {}
        }
    }
    live.retain(|directive| {
        !dead.contains(&directive.id)
            && directive.expires_at_ms > now
            && (directive.target == target
                || directive.target == "both"
                || target == "any"
                // An operator instruction targets the one decision owner. In
                // the legacy policy that owner is the `decide` role; the
                // two-role orchestrator receives it through the owner-turn
                // `any` summary instead.
                || (directive.target == OWNER_TARGET && target == "decide"))
    });
    live.sort_by_key(|directive| std::cmp::Reverse(directive.issued_at_ms));
    live.truncate(Limits::default().max_active);
    live
}

pub(crate) fn envelope(root: &Path, now: i64, target: &str, max_active: usize) -> Value {
    let mut items = active(root, now, target);
    items.truncate(max_active);
    json!(
        items
            .into_iter()
            .map(|directive| json!({
                "id": directive.id,
                "text": directive.text,
                "keys": directive.keys,
                "target": directive.target,
                "authorization_scope": directive.authorization_scope,
                "expires_at_ms": directive.expires_at_ms,
            }))
            .collect::<Vec<_>>()
    )
}

/// Same check for the directives delivered to a specific role. Acknowledgements
/// are checked only against those directives, so unknown IDs are ignored.
pub(crate) fn unacknowledged_for(
    root: &Path,
    digest: &Value,
    now: i64,
    target: &str,
) -> Vec<String> {
    let acked = digest["directive_acknowledgements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    active(root, now, target)
        .into_iter()
        .filter(|directive| !acked.contains(directive.id.as_str()))
        .map(|directive| directive.id)
        .collect()
}
