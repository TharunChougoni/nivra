//! Per-projection content revisions. Each hash covers the view fields that
//! projection's handler reads, minus per-loop clocks (`as_of_ms`, the
//! controller heartbeat, `captured_at_ms`), so an idle loop leaves every
//! revision unchanged and clients skip the refetch.
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

/// Revision of the lane journals `/api/v1/agent` reads, from file metadata
/// only. A missing file contributes a fixed `absent` marker.
fn agent_revision(root: Option<&Path>) -> String {
    let stats = AGENT_FILES
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

/// Alerts carry the loop clock as `observed_at_ms`.
fn strip_alert_clocks(alerts: &mut Value) {
    for alert in alerts.as_array_mut().into_iter().flatten() {
        if let Some(fields) = alert.as_object_mut() {
            fields.shift_remove("observed_at_ms");
        }
    }
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
    strip_alert_clocks(&mut alerts);
    let mut health = health_body(view, 0);
    if let Some(fields) = health.as_object_mut() {
        for clock in ["as_of_ms", "controller_liveness", "controller_heartbeat"] {
            fields.shift_remove(clock);
        }
    }
    if let Some(Value::Object(fields)) = health.get_mut("operational_health") {
        fields.shift_remove("captured_at_ms");
    }
    strip_alert_clocks(&mut health["alerts"]);
    json!({
        "portfolio": with_identity(&[
            &view["capital"],
            p,
            &view["realised_net_pnl_by_fill"],
            &view["fee_policy"],
        ]),
        "fills": with_identity(&[&p["fills"]]),
        "plans": with_identity(&[&view["planner"]]),
        "scanner": with_identity(&[&view["discovery"], &view["universe_size"], &view["ranking"]]),
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
        "agent": agent_revision(root),
    })
}
