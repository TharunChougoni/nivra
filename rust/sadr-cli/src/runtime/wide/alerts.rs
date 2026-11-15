//! Pure scanner/feed alert predicates.

pub(super) fn scanner_fresh_drop(previous: u64, current: u64, fraction: f64) -> bool {
    previous > 0 && current < previous && (previous - current) as f64 / previous as f64 > fraction
}

pub(super) fn scanner_fresh_drop_feed_ready(health: &serde_json::Value) -> bool {
    // Only a connected streaming feed is eligible. This explicitly suppresses
    // disconnected, warm-up, and rest_fallback projections while retaining
    // alerts for drops during established streaming.
    health["connected"].as_bool() == Some(true) && health["state"].as_str() == Some("streaming")
}
