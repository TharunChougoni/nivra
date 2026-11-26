//! Per-projection content revisions. Each hash covers the view fields that
//! projection's handler reads, minus the fields in [`CLOCKS`], so an idle loop
//! leaves every revision unchanged and clients skip the refetch.
use super::*;
use serde_json::{Value, json};

struct HashWriter(Sha256);
impl std::io::Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn short_hash(inputs: &[&Value]) -> String {
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, inputs).expect("in-memory JSON");
    writer.0.finalize()[..8]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fields that move with the loop clock or the feed tick, not with content,
/// as `(parent, key)`. A `None` parent matches the key at any depth. Any key
/// named `age_ms` or ending in `_age_ms` is a clock as well.
const CLOCKS: &[(Option<&str>, &str)] = &[
    (None, "as_of_ms"),
    (None, "captured_at_ms"),
    (None, "checked_at_ms"),
    (None, "observed_at_ms"),
    (None, "retained_window"),
    (None, "last_completed_at_ms"),
    (None, "expected_deadline_ms"),
    (None, "next_deadline_ms"),
    (None, "loop_generation"),
    (None, "process_memory"),
    (Some("controller_heartbeat"), "completed_at_ms"),
    (Some("market_feed"), "ticks_total"),
    (Some("market_feed"), "tick_rate_per_s"),
    (Some("market_feed"), "oldest_last_trade_ms"),
];

fn is_clock(parent: Option<&str>, key: &str) -> bool {
    key == "age_ms"
        || key.ends_with("_age_ms")
        || CLOCKS
            .iter()
            .any(|(at, clock)| *clock == key && at.is_none_or(|at| Some(at) == parent))
}

/// Removes every [`CLOCKS`] field, at any depth.
pub(super) fn strip_clocks(value: &mut Value) {
    fn strip(parent: Option<&str>, value: &mut Value) {
        match value {
            Value::Object(fields) => {
                fields.retain(|key, _| !is_clock(parent, key));
                for (key, child) in fields.iter_mut() {
                    strip(Some(key), child);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| strip(parent, item)),
            _ => {}
        }
    }
    strip(None, value);
}

/// Revision of the given lane journals, from file metadata only. A missing
/// file contributes a fixed `absent` marker.
fn agent_revision(root: Option<&Path>, files: &[&str]) -> String {
    let stats = files
        .iter()
        .map(|file| {
            let Some(root) = root else {
                return json!("absent");
            };
            match std::fs::symlink_metadata(root.join("model/agent").join(file)) {
                Ok(meta) => {
                    use std::os::unix::fs::MetadataExt;
                    json!([meta.len(), meta.ino(), meta.mtime(), meta.mtime_nsec()])
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!("absent"),
                Err(_) => json!("unavailable"),
            }
        })
        .collect::<Vec<_>>();
    short_hash(&stats.iter().collect::<Vec<_>>())
}

pub(crate) fn revisions(view: &Value, root: Option<&Path>) -> Value {
    let p = &view["portfolio"];
    let identity = [
        &view["live_enabled"],
        &view["paper_only"],
        &view["account"],
        &view["authorized_session"],
    ];
    let with_identity = |fields: &[&Value]| {
        let mut inputs = identity.to_vec();
        inputs.extend_from_slice(fields);
        short_hash(&inputs)
    };
    let positions = json!(p["positions"].as_object().map(|v| v.len()));
    let fills = json!(p["fills"].as_array().map(Vec::len));
    let mut alerts = json!(derived_alerts(view));
    strip_clocks(&mut alerts);
    // Liveness is judged at the loop's own completion, so only a malformed or
    // missing heartbeat changes its status here.
    let mut health = health_body(view, view["as_of_ms"].as_i64().unwrap_or(0));
    strip_clocks(&mut health);
    let discovery = &view["discovery"];
    let mut scanner = json!({
        "prospects":discovery["prospects"],
        "unavailable":discovery["unavailable"],
        "screen_reasons":discovery["screen_reasons"],
        "fresh_count":discovery["fresh_count"],
        "stale_count":discovery["stale_count"],
        "unseen_count":discovery["unseen_count"],
        "eligible_count":discovery["eligible_count"],
        "presented_count":discovery["presented_count"],
        "unavailable_count":discovery["unavailable_count"],
        "unavailable_truncated":discovery["unavailable_truncated"],
        "selection":discovery["selection"],
        "limitations":discovery["limitations"],
        "universe_size":view["universe_size"],
        "ranking":view["ranking"],
    });
    strip_clocks(&mut scanner);
    json!({
        "portfolio": with_identity(&[
            &view["capital"],
            p,
            &view["realised_net_pnl_by_fill"],
            &view["fee_policy"],
        ]),
        "fills": with_identity(&[&p["fills"]]),
        "plans": with_identity(&[&view["planner"]]),
        "scanner": with_identity(&[&scanner]),
        "sessions": with_identity(&[
            &view["status"],
            &view["capital"],
            &p["stopped"],
            &p["halted"],
            &p["closed"],
            &p["cash"],
            &positions,
            &fills,
        ]),
        "health": short_hash(&[&health]),
        "alerts": with_identity(&[&alerts]),
        "agent": agent_revision(root, &AGENT_FILES),
        "agent_turns": agent_revision(root, &AGENT_FILES[..1]),
    })
}
