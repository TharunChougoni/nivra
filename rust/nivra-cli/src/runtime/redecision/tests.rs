use super::*;
#[test]
fn configuration_check_names_the_failed_prompt_or_config_hash() {
    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(d.path());
    let prompt = d.path().join("scorer.md");
    fs::write(&prompt, "scorer v1").unwrap();
    let mut planner: planner::PlannerConfig = serde_json::from_value(json!({
            "enabled":true,
            "digest":{"enabled":true,"model_id":"fixture-model","daily_call_budget":4},
            "events":{"enabled":true,"cooldown_ms":0,"max_pending":10,"policy":{"price_move_bps":100,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}},
            "orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7"},"triggers":{"scorer":{"prompt":prompt,"after_minute":931},"supervisor":{"prompt":prompt}}}
        }))
        .unwrap();
    let path = d.path().join("planner.json");
    fs::write(&path, serde_json::to_vec(&planner).unwrap()).unwrap();
    c.planner_config = Some(path.clone());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();

    fs::write(&prompt, "scorer v2").unwrap();
    let check = configuration_check(&db, &c);
    assert_eq!(check["failed_check"], "prompt_sha256");
    assert_ne!(
        check["prior_prompt_sha256"]["scorer"],
        check["current_prompt_sha256"]["scorer"]
    );

    // Restoring the prompt and changing any other policy input is a config
    // hash failure, not a prompt failure.
    fs::write(&prompt, "scorer v1").unwrap();
    planner.events.as_mut().unwrap().cooldown_ms = 1;
    fs::write(&path, serde_json::to_vec(&planner).unwrap()).unwrap();
    let check = configuration_check(&db, &c);
    assert_eq!(check["failed_check"], "config_hash");
}
fn at() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn cfg() -> planner::PlannerConfig {
    serde_json::from_value(json!({"enabled":true,"digest":{"enabled":true,"model_id":"fixture-model","daily_call_budget":4},"orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7"},"triggers":{"supervisor":{"prompt":"supervisor.md"}}},"events":{"enabled":true,"cooldown_ms":0,"max_pending":10,"policy":{"price_move_bps":100,"volume_step_units":null,"gap_bps":null,"proximity_bps":null,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}}})).unwrap()
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
