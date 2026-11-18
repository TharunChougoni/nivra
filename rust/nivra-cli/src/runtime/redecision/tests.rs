use super::*;
fn at() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn cfg() -> planner::PlannerConfig {
    serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap()
}
#[test]
fn transient_read_classification_is_narrow() {
    for reason in [
        "http_timeout",
        "http_401_unauthorized",
        "http_429_rate_limited",
        "http_503",
        "read_only_http_failed",
        "read_quota_wait_per_second",
        "read_quota_wait_per_minute",
        "read_quota_wait_per_half_hour",
        "response_too_large",
        "response_not_utf8",
    ] {
        assert!(transient_read_failure(reason), "{reason}");
    }
    for reason in [
        "invalid_quote_json",
        "invalid_quote_status",
        "invalid_quote_data",
        "duplicate_quote",
        "quote_keys",
        "private_file_unreadable",
        "read_journal_unreadable",
        "original_read_journal_changed",
        "read_clock_backwards",
    ] {
        assert!(!transient_read_failure(reason), "{reason}");
    }
}
#[test]
fn transient_read_block_never_masks_sticky_blocks() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    block(&mut db, &c, READ_TRANSIENT_BLOCK).unwrap();
    block(&mut db, &c, DATA_BLOCK).unwrap();
    assert_eq!(status(&db, &c).unwrap().unwrap()["status"], DATA_BLOCK);
    assert!(!clear_transient_read_block(&mut db, &c).unwrap());

    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    block(&mut db, &c, READ_TRANSIENT_BLOCK).unwrap();
    block(&mut db, &c, "planner_cancelled_no_replay").unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        READ_TRANSIENT_BLOCK
    );
    assert!(clear_transient_read_block(&mut db, &c).unwrap());

    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    block(&mut db, &c, READ_TRANSIENT_BLOCK).unwrap();
    block(&mut db, &c, "operator_stopped_no_replay").unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "operator_stopped_no_replay"
    );
    assert!(!clear_transient_read_block(&mut db, &c).unwrap());
}
