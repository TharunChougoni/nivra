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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_drop_requires_strict_threshold_and_nonzero_previous() {
        assert!(!scanner_fresh_drop(100, 50, 0.5));
        assert!(scanner_fresh_drop(100, 49, 0.5));
        assert!(!scanner_fresh_drop(100, 51, 0.5));
        assert!(!scanner_fresh_drop(0, 0, 0.5));
        assert!(!scanner_fresh_drop(0, 10, 0.5));
        assert!(!scanner_fresh_drop(100, 101, 0.5));
        assert!(!scanner_fresh_drop(100, 100, 0.5));
        assert!(scanner_fresh_drop(100, 0, 0.5));
        assert!(!scanner_fresh_drop(100, 0, 1.0));
        let open_projection = serde_json::json!({"connected":false,"state":"rest_fallback"});
        assert!(!scanner_fresh_drop_feed_ready(&open_projection));
        assert!(!scanner_fresh_drop_feed_ready(
            &serde_json::json!({"connected":true,"state":"rest_fallback"})
        ));
        let streaming = serde_json::json!({"connected":true,"state":"streaming"});
        assert!(scanner_fresh_drop_feed_ready(&streaming));
        assert!(scanner_fresh_drop(2696, 200, 0.5));
        assert!(scanner_fresh_drop_feed_ready(&streaming) && scanner_fresh_drop(2696, 200, 0.5));
    }
}
