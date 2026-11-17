use super::*;
#[test]
fn credential_warning_dedupe_keeps_roles_and_failure_transitions_distinct() {
    let warning = |role, reason| {
        Payload::new(
            Event::ModelCredentialUnavailable,
            None,
            json!({"model":role,"reason":reason}),
        )
    };
    let digest = warning("digest", "grant_refresh_required_or_expiry_unverified");
    assert_eq!(digest.severity, "warning");
    assert_eq!(digest.event, "model_credential_unavailable");
    assert_eq!(
        digest.dedupe_key,
        warning("digest", "grant_refresh_required_or_expiry_unverified").dedupe_key
    );
    assert_ne!(
        digest.dedupe_key,
        warning("decide", "grant_refresh_required_or_expiry_unverified").dedupe_key
    );
    assert_ne!(
        digest.dedupe_key,
        warning("digest", "model_credential_validation_failed").dedupe_key
    );
    assert_eq!(
        Event::from_recorded_name("model_credential_unavailable").map(RecordedEvent::as_str),
        Some("model_credential_unavailable")
    );
    assert_eq!(
        Event::from_recorded_name("approval_renewed").map(RecordedEvent::as_str),
        Some("approval_renewed")
    );
    assert_eq!(
        Event::from_recorded_name("approval_renew_blocked").map(RecordedEvent::as_str),
        Some("approval_renew_blocked")
    );
    assert!(Event::from_recorded_name("not_an_event").is_none());
    assert!(Event::ALL.iter().all(
        |event| event.name() != "approval_renewed" && event.name() != "approval_renew_blocked"
    ));
    let historic = serde_json::from_str::<Payload>(
            r#"{"source":"nivra","event":"approval_renewed","severity":"info","session":null,"at":"2026-11-09T00:00:00Z","version":"0.9.16","summary":"historic","details":{},"dedupe_key":"k"}"#,
        )
        .unwrap();
    assert!(historic.valid());
    assert_eq!(historic.event, "approval_renewed");
}

#[test]
fn recovered_clears_stale_failure_marker() {
    let dir = tempfile::tempdir().unwrap();
    failure(dir.path());
    assert!(dir.path().join("last-failure").exists());
    recovered(dir.path());
    assert!(!dir.path().join("last-failure").exists());
    recovered(dir.path());
}
#[test]
fn delivery_policy_and_secret_boundary() {
    let state = tempfile::tempdir().unwrap();
    let session = Some("2026-10-16".parse().unwrap());
    for rollback in [false, true] {
        let failure = Payload::new(
            Event::UpdaterFailure,
            session,
            json!({"rollback":rollback,"reason":"update_preflight_failed"}),
        );
        assert_eq!(failure.event, "updater_failure");
        assert_eq!(failure.severity, "critical");
        assert!(failure.summary.contains("update_preflight_failed"));
    }
    assert_ne!(
        Payload::new(
            Event::UpdaterFailure,
            session,
            json!({"reason":"update_preflight_failed"})
        )
        .dedupe_key,
        Payload::new(
            Event::UpdaterFailure,
            session,
            json!({"reason":"update_rollback_restart_failed"})
        )
        .dedupe_key
    );
    for event in [Event::Stop, Event::Halt] {
        let closed = Payload::new(
            event,
            session,
            json!({"closed":true,"open_unresolved":0,"reason":"operator_acknowledged_closed_stopped_book"}),
        );
        assert_eq!(closed.severity, "info");
        assert!(!closed.summary.contains("review required"));
        assert_eq!(
            Payload::new(event, session, json!({"closed":false})).severity,
            "critical"
        );
    }
    let feed_down = Payload::new(
        Event::MarketFeedFallback,
        session,
        json!({"feed_generation":1}),
    );
    let next_down = Payload::new(
        Event::MarketFeedFallback,
        session,
        json!({"feed_generation":2}),
    );
    assert_eq!(feed_down.severity, "warning");
    assert_ne!(feed_down.dedupe_key, next_down.dedupe_key);
    assert_eq!(
        Payload::new(
            Event::MarketFeedRecovered,
            session,
            json!({"feed_generation":1})
        )
        .severity,
        "info"
    );
    let planned = Payload::new(
        Event::Stop,
        session,
        json!({"closed":false,"operator_stopped":true,"halted":false,"open_unresolved":1}),
    );
    assert_eq!(planned.severity, "info");
    assert_eq!(planned.details["classification"], "expected_not_failure");
    assert!(planned.summary.contains("expected_not_failure"));
    assert!(planned.summary.contains("explicit resume required"));
    for event in [Event::Stop, Event::Halt] {
        assert_eq!(
            Payload::new(
                event,
                session,
                json!({"closed":false,"operator_stopped":true,"halted":true})
            )
            .severity,
            "critical"
        );
    }
    assert_eq!(
        Payload::new(
            Event::Stop,
            session,
            json!({"closed":false,"operator_stopped":true})
        )
        .severity,
        "critical"
    );
    plan_restart(state.path(), "v0.5.13", 1000).unwrap();
    let marker = || planned_restart(state.path(), 1001);
    let stop = Payload::new(Event::Stop, session, json!({"closed":true})).with_restart(marker());
    let helper = Payload::new(Event::RestartLoop, session, json!({})).with_restart(marker());
    let startup = Payload::new(Event::PlannedRestart, None, json!({})).with_restart(marker());
    assert_eq!(stop.event, "planned_restart");
    assert_eq!(stop.severity, "info");
    assert_eq!(stop.dedupe_key, helper.dedupe_key);
    assert_eq!(stop.dedupe_key, startup.dedupe_key);
    assert_eq!(
        Payload::new(Event::Halt, session, json!({"closed":false}))
            .with_restart(marker())
            .severity,
        "critical"
    );
    assert_eq!(
        Payload::new(Event::Stop, session, json!({"closed":false}))
            .with_restart(marker())
            .severity,
        "critical"
    );
    plan_restart(state.path(), "v0.5.12", 1002).unwrap(); // rollback shares the same notice
    assert_eq!(marker().unwrap().version, "v0.5.13");
    assert!(planned_restart(state.path(), 999).is_none());
    assert!(planned_restart(state.path(), 1600).is_none());
    assert_eq!(
        Payload::new(Event::RestartLoop, session, json!({}))
            .with_restart(planned_restart(state.path(), 1600))
            .severity,
        "critical"
    );
    fs::write(state.path().join("planned-restart.json"), b"malformed").unwrap();
    assert!(marker().is_none());
    plan_restart(state.path(), "v0.5.13", 1000).unwrap();
    fs::set_permissions(
        state.path().join("planned-restart.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(marker().is_none());
    struct Fake(usize, Option<Value>);
    impl Transport for Fake {
        fn send(&mut self, _: &Endpoint, p: &Payload) -> Result<u16> {
            let body = serde_json::to_string(p).unwrap();
            assert!(!body.contains("secret"));
            self.0 += 1;
            self.1 = Some(p.details.clone());
            Ok(204)
        }
    }
    let dir = tempfile::tempdir_in("/var/tmp").unwrap();
    let path = dir.path().join("endpoint.json");
    fs::write(
        &path,
        br#"{"url":"http://example.invalid/?secret=query","header_value":"secret-header"}"#,
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(endpoint(&path).is_err());
    fs::write(
        &path,
        br#"{"url":"https://example.invalid/?secret=query","header_value":"secret-header"}"#,
    )
    .unwrap();
    let e = endpoint(&path).unwrap();
    let mut db = database(dir.path()).unwrap();
    let mut t = Fake(0, None);
    assert_eq!(
        deliver(&mut db, &e, stop, 1000, &mut t, false).unwrap(),
        204
    );
    assert_eq!(t.1.as_ref().unwrap()["suppressed_count"], 0);
    assert_eq!(
        deliver(&mut db, &e, helper, 1001, &mut t, false).unwrap(),
        0
    );
    assert_eq!(
        deliver(&mut db, &e, startup, 1002, &mut t, false).unwrap(),
        0
    );
    assert_eq!(t.0, 1);
    // Isolate the existing admission scenarios from the planned notice above.
    db.execute_batch("DELETE FROM delivery; DELETE FROM attempts; DELETE FROM suppressed_keys;")
        .unwrap();
    t.0 = 0;
    t.1 = None;
    for i in 0..25 {
        let mut p = Payload::new(Event::Test, None, json!({}));
        p.dedupe_key = format!("test:{i}");
        let status = deliver(&mut db, &e, p, 10000, &mut t, false).unwrap();
        assert_eq!(status, if i < 20 { 204 } else { 0 });
    }
    assert_eq!(t.0, 20);
    assert_eq!(
        db.query_row(
            "SELECT COALESCE(SUM(count),0) FROM suppressed_keys",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        5
    );
    drop(db);
    let mut db = database(dir.path()).unwrap(); // Admission survives sender/process restart.
    let mut p = Payload::new(Event::Test, None, json!({}));
    p.dedupe_key = "test:0".into();
    assert_eq!(deliver(&mut db, &e, p, 10001, &mut t, false).unwrap(), 0);
    // Dedupe independently of the global rate bound.
    db.execute("DELETE FROM delivery", []).unwrap();
    let mut p = Payload::new(Event::Test, None, json!({}));
    p.dedupe_key = "test:0".into();
    assert_eq!(deliver(&mut db, &e, p, 10002, &mut t, false).unwrap(), 0);
    assert_eq!(t.0, 20);
    let suppressed = |db: &Connection, key: &str| {
        db.query_row(
            "SELECT COALESCE((SELECT count FROM suppressed_keys WHERE key=?),0)",
            [key],
            |r| r.get::<_, i64>(0),
        )
        .unwrap()
    };
    assert_eq!(suppressed(&db, "test:0"), 2);
    assert_eq!(suppressed(&db, "test:20"), 1);
    // Another event type keeps its own count instead of inheriting test:0's.
    assert_eq!(
        deliver(
            &mut db,
            &e,
            Payload::new(Event::Halt, None, json!({})),
            10003,
            &mut t,
            false
        )
        .unwrap(),
        204
    );
    assert_eq!(t.1.as_ref().unwrap()["suppressed_count"], 0);
    // A later delivery of test:0 reports its own two suppressions and clears
    // only that key; unrelated history remains separate.
    let mut p = Payload::new(Event::Test, None, json!({}));
    p.dedupe_key = "test:0".into();
    assert_eq!(deliver(&mut db, &e, p, 14000, &mut t, false).unwrap(), 204);
    assert_eq!(t.1.as_ref().unwrap()["suppressed_count"], 2);
    assert_eq!(suppressed(&db, "test:0"), 0);
    assert_eq!(suppressed(&db, "test:20"), 1);
    struct Unavailable;
    impl Transport for Unavailable {
        fn send(&mut self, _: &Endpoint, _: &Payload) -> Result<u16> {
            Err(())
        }
    }
    struct ServerError;
    impl Transport for ServerError {
        fn send(&mut self, _: &Endpoint, _: &Payload) -> Result<u16> {
            Ok(503)
        }
    }
    struct ClientError;
    impl Transport for ClientError {
        fn send(&mut self, _: &Endpoint, _: &Payload) -> Result<u16> {
            Ok(400)
        }
    }
    assert!(
        deliver(
            &mut db,
            &e,
            Payload::new(Event::Halt, None, json!({})),
            14001,
            &mut Unavailable,
            false
        )
        .is_err()
    );

    // A failed alert remains in the bounded SQLite outbox across a sender
    // restart. Its admitted key bypasses fresh-event dedupe on retry.
    let outbox_dir = tempfile::tempdir().unwrap();
    let missing = transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 20_000_000);
    assert_eq!(missing.status, AlertStoreStatus::Missing);
    assert!(missing.counts.is_none());
    assert!(missing.retained_window.is_none());
    assert!(!missing.delivery_guarantee);
    assert_eq!(
        missing.ack_meaning,
        "sender_http_2xx_receipt_not_human_read"
    );
    assert_eq!(
        missing.last_accepted_scope,
        "retained_window_only_not_lifetime"
    );
    assert_eq!(
        missing.last_failure_scope,
        "retained_window_only_not_lifetime"
    );
    assert_eq!(
        missing.failure_marker_definition,
        "marker_presence_not_attempt_outcome"
    );
    assert_eq!(
        missing.timestamp_normalization,
        "delivery_at_unix_seconds_checked_mul_1000_unknown_on_overflow"
    );
    // A missing store stays missing: no lazy database or schema creation.
    assert!(!outbox_dir.path().join("delivery.sqlite").exists());
    {
        let _empty = database(outbox_dir.path()).unwrap();
    }
    let empty = transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 20_000_000);
    assert_eq!(empty.status, AlertStoreStatus::Available);
    let empty_counts = empty.counts.as_ref().unwrap();
    assert_eq!(empty_counts.queued, 0);
    assert_eq!(empty_counts.retrying, 0);
    assert_eq!(empty_counts.transport_accepted, 0);
    assert_eq!(
        empty_counts.transport_accepted_scope,
        "retained_window_only_not_lifetime"
    );
    assert_eq!(empty_counts.dead, 0);
    assert_eq!(empty_counts.dead_definition, "quarantined_outbox_rows");
    assert!(empty_counts.dropped.is_none());
    assert_eq!(empty_counts.in_flight_or_transport_error, 0);
    assert_eq!(
        empty_counts.in_flight_or_transport_error_definition,
        "status_zero_or_null_in_flight_or_transport_error_not_confirmed_failure"
    );
    // Current outbox rows are a separate scope from the retained delivery
    // window: the outbox cap is 64, while delivery retains at most 20 rows
    // inside the one-hour window.
    assert_eq!(
        empty_counts.outbox_scope,
        "current_host_outbox_not_delivery_window"
    );
    assert_eq!(empty_counts.outbox_max_rows, 64);
    assert_eq!(
        empty_counts.queued_scope,
        "current_outbox_rows_not_retained_delivery_window"
    );
    assert_eq!(
        empty_counts.retrying_scope,
        "current_outbox_rows_not_retained_delivery_window"
    );
    assert_eq!(
        empty_counts.dead_scope,
        "current_outbox_quarantined_rows_not_delivery_window"
    );
    assert_eq!(
        empty_counts.in_flight_or_transport_error_scope,
        "retained_window_only_not_lifetime"
    );
    let empty_json = serde_json::to_value(&empty).unwrap();
    assert_eq!(
        empty_json["counts"]["outbox_scope"],
        "current_host_outbox_not_delivery_window"
    );
    assert_eq!(empty_json["counts"]["outbox_max_rows"], 64);
    assert_eq!(
        empty_json["counts"]["queued_scope"],
        "current_outbox_rows_not_retained_delivery_window"
    );
    assert_eq!(
        empty_json["counts"]["dead_scope"],
        "current_outbox_quarantined_rows_not_delivery_window"
    );
    assert_eq!(empty.counts_scope, "host_alert_outbox_not_account_filtered");
    assert_eq!(empty.scope.account, "tharun-upstox");
    assert_eq!(empty.scope.session, "2026-10-16");
    assert_eq!(empty.scope.store_scope, "host_alert_outbox");
    assert!(!empty.failure_marker_present);
    let empty_window = empty.retained_window.as_ref().unwrap();
    assert_eq!(empty_window.start_ms, 20_000_000 - 3_600_000);
    assert_eq!(empty_window.end_ms, 20_000_000);
    assert_eq!(
        empty_window.scope,
        "retained_rows_within_window_lower_bound_not_lifetime"
    );
    assert_eq!(empty_window.max_retained_rows, 20);
    assert_eq!(empty_window.retained_delivery_rows, 0);
    assert_eq!(empty_window.rows_in_window, 0);
    assert_eq!(empty_window.truncation, "unknown_cap_or_lazy_pruning");
    assert!(!empty_window.clock_anomaly);
    let queued = Payload::new(Event::UpdaterFailure, None, json!({"reason":"test"}));
    let mut outbox_db = database(outbox_dir.path()).unwrap();
    enqueue(&mut outbox_db, &queued, 20_000).unwrap();
    let queued_status =
        transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 20_000_000);
    assert_eq!(queued_status.counts.as_ref().unwrap().queued, 1);
    assert_eq!(queued_status.counts.as_ref().unwrap().retrying, 0);
    struct Available(usize, Option<String>);
    impl Transport for Available {
        fn send(&mut self, _: &Endpoint, p: &Payload) -> Result<u16> {
            self.0 += 1;
            self.1 = Some(serde_json::to_string(p).unwrap());
            Ok(204)
        }
    }
    assert!(
        process_one(
            outbox_dir.path(),
            &mut outbox_db,
            &e,
            20_000,
            &mut ServerError,
            &AtomicU64::new(0),
            false,
        )
        .unwrap()
    );
    assert_eq!(
        outbox_db
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        outbox_db
            .query_row("SELECT admitted FROM outbox", [], |r| r.get::<_, bool>(0))
            .unwrap()
    );
    assert_eq!(
        outbox_db
            .query_row("SELECT status FROM delivery", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        503
    );
    let failed_status =
        transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 20_000_000);
    let failed_counts = failed_status.counts.as_ref().unwrap();
    assert_eq!(failed_counts.queued, 0);
    assert_eq!(failed_counts.retrying, 1);
    assert_eq!(failed_status.last_failure_at_ms, Some(20_000_000));
    assert_eq!(failed_status.last_failure_category, Some("receiver_error"));
    assert_eq!(
        failed_status.last_failure_scope,
        "retained_window_only_not_lifetime"
    );
    assert!(failed_status.failure_marker_present);
    drop(outbox_db);
    let mut outbox_db = database(outbox_dir.path()).unwrap();
    let mut available = Available(0, None);
    assert!(
        !process_one(
            outbox_dir.path(),
            &mut outbox_db,
            &e,
            20_004,
            &mut available,
            &AtomicU64::new(0),
            false,
        )
        .unwrap()
    );
    assert!(
        process_one(
            outbox_dir.path(),
            &mut outbox_db,
            &e,
            20_005,
            &mut available,
            &AtomicU64::new(0),
            false,
        )
        .unwrap()
    );
    assert_eq!(available.0, 1);
    assert!(available.1.unwrap().contains("updater_failure"));
    assert_eq!(
        outbox_db
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        outbox_db
            .query_row(
                "SELECT status FROM delivery ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        204
    );
    let accepted_status =
        transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 20_005_000);
    let accepted_counts = accepted_status.counts.as_ref().unwrap();
    assert_eq!(accepted_counts.queued, 0);
    assert_eq!(accepted_counts.retrying, 0);
    assert_eq!(accepted_counts.transport_accepted, 1);
    assert_eq!(accepted_status.last_accepted_at_ms, Some(20_005_000));
    assert_eq!(
        accepted_status.last_accepted_scope,
        "retained_window_only_not_lifetime"
    );
    enqueue(
        &mut outbox_db,
        &Payload::new(Event::Test, None, json!({"terminal":"4xx"})),
        21_000,
    )
    .unwrap();
    assert!(
        process_one(
            outbox_dir.path(),
            &mut outbox_db,
            &e,
            21_000,
            &mut ClientError,
            &AtomicU64::new(0),
            false,
        )
        .unwrap()
    );
    assert_eq!(
        outbox_db
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        outbox_db
            .query_row(
                "SELECT status FROM delivery ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        400
    );
    let rejected_status =
        transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 21_000_000);
    assert_eq!(rejected_status.last_failure_at_ms, Some(21_000_000));
    assert_eq!(
        rejected_status.last_failure_category,
        Some("receiver_rejected")
    );
    // The retained window is a lower bound, not lifetime delivery history.
    // Reading two hours after the last accepted receipt without a new send
    // must not claim "never delivered": the old rows are simply out of scope.
    let aged = transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 27_205_000);
    let aged_counts = aged.counts.as_ref().unwrap();
    assert_eq!(aged_counts.transport_accepted, 0);
    assert!(aged.last_accepted_at_ms.is_none());
    assert!(aged.last_failure_at_ms.is_none());
    assert!(aged.last_failure_category.is_none());
    assert_eq!(
        aged.last_accepted_scope,
        "retained_window_only_not_lifetime"
    );
    let aged_window = aged.retained_window.as_ref().unwrap();
    assert_eq!(aged_window.start_ms, 27_205_000 - 3_600_000);
    assert_eq!(aged_window.end_ms, 27_205_000);
    assert_eq!(
        aged_window.scope,
        "retained_rows_within_window_lower_bound_not_lifetime"
    );
    assert!(aged_window.retained_delivery_rows >= 3);
    assert_eq!(aged_window.rows_in_window, 0);
    assert_eq!(aged_window.truncation, "unknown_cap_or_lazy_pruning");
    assert!(!aged_window.clock_anomaly);
    // The table caps retained delivery rows at 20; the projection discloses
    // that cap and does not pretend the retained rows are complete history.
    let cap_dir = tempfile::tempdir().unwrap();
    let cap_db = database(cap_dir.path()).unwrap();
    for i in 0..20 {
        cap_db
            .execute(
                "INSERT INTO delivery(at,status) VALUES(?,?)",
                params![20_000 + i, 204],
            )
            .unwrap();
    }
    drop(cap_db);
    let cap = transport_status_at(cap_dir.path(), "tharun-upstox", "2026-10-16", 20_020_000);
    assert_eq!(cap.counts.as_ref().unwrap().transport_accepted, 20);
    let cap_window = cap.retained_window.as_ref().unwrap();
    assert_eq!(cap_window.max_retained_rows, 20);
    assert_eq!(cap_window.retained_delivery_rows, 20);
    assert_eq!(cap_window.rows_in_window, 20);
    assert_eq!(cap_window.truncation, "unknown_cap_or_lazy_pruning");
    assert!(!cap_window.clock_anomaly);
    // A status-0 row is an in-flight or transport-error attempt, not a
    // confirmed failure. An older last-failure marker does not prove this
    // attempt failed, so it stays out of last_failure and is exposed alone.
    let ambiguous_dir = tempfile::tempdir().unwrap();
    let ambiguous_db = database(ambiguous_dir.path()).unwrap();
    ambiguous_db
        .execute("INSERT INTO delivery(at,status) VALUES(?,0)", [20_000])
        .unwrap();
    drop(ambiguous_db);
    fs::write(
        ambiguous_dir.path().join("last-failure"),
        b"older_attempt\n",
    )
    .unwrap();
    let ambiguous = transport_status_at(
        ambiguous_dir.path(),
        "tharun-upstox",
        "2026-10-16",
        20_000_000,
    );
    let ambiguous_counts = ambiguous.counts.as_ref().unwrap();
    assert_eq!(ambiguous_counts.in_flight_or_transport_error, 1);
    assert_eq!(ambiguous_counts.transport_accepted, 0);
    assert!(ambiguous.last_failure_at_ms.is_none());
    assert!(ambiguous.last_failure_category.is_none());
    assert!(ambiguous.failure_marker_present);
    assert_eq!(
        ambiguous.failure_marker_definition,
        "marker_presence_not_attempt_outcome"
    );
    let ambiguous_db = Connection::open(ambiguous_dir.path().join("delivery.sqlite")).unwrap();
    ambiguous_db
        .execute("UPDATE delivery SET status=503 WHERE status=0", [])
        .unwrap();
    drop(ambiguous_db);
    let finalized = transport_status_at(
        ambiguous_dir.path(),
        "tharun-upstox",
        "2026-10-16",
        20_000_000,
    );
    assert_eq!(
        finalized
            .counts
            .as_ref()
            .unwrap()
            .in_flight_or_transport_error,
        0
    );
    assert_eq!(finalized.last_failure_at_ms, Some(20_000_000));
    assert_eq!(finalized.last_failure_category, Some("receiver_error"));
    // A seconds source that overflows milliseconds is unknown, never a
    // clamped near-i64::MAX timestamp. The retained window ends at
    // floor(now_ms/1000), so an in-window row cannot overflow; the checked
    // conversion is asserted directly and the boundary row keeps its exact
    // millisecond value instead of being clamped.
    assert_eq!(checked_seconds_to_ms(i64::MAX / 1000 + 1), None);
    let overflow_dir = tempfile::tempdir().unwrap();
    let overflow_db = database(overflow_dir.path()).unwrap();
    let huge_seconds = i64::MAX / 1000;
    overflow_db
        .execute(
            "INSERT INTO delivery(at,status) VALUES(?,?)",
            params![huge_seconds, 204],
        )
        .unwrap();
    overflow_db
        .execute(
            "INSERT INTO delivery(at,status) VALUES(?,?)",
            params![huge_seconds, 503],
        )
        .unwrap();
    drop(overflow_db);
    let overflow =
        transport_status_at(overflow_dir.path(), "tharun-upstox", "2026-10-16", i64::MAX);
    let huge_ms = huge_seconds.checked_mul(1000).unwrap();
    assert_eq!(overflow.counts.as_ref().unwrap().transport_accepted, 1);
    assert_eq!(overflow.last_accepted_at_ms, Some(huge_ms));
    assert_eq!(overflow.last_failure_at_ms, Some(huge_ms));
    assert_eq!(overflow.last_failure_category, Some("receiver_error"));
    assert!(!overflow.retained_window.as_ref().unwrap().clock_anomaly);
    // A null seconds source is unknown, not a zero timestamp.
    let invalid_dir = tempfile::tempdir().unwrap();
    let invalid_db = database(invalid_dir.path()).unwrap();
    invalid_db
        .execute("INSERT INTO delivery(at,status) VALUES(NULL,204)", [])
        .unwrap();
    drop(invalid_db);
    let invalid = transport_status_at(
        invalid_dir.path(),
        "tharun-upstox",
        "2026-10-16",
        20_000_000,
    );
    assert_eq!(invalid.counts.as_ref().unwrap().transport_accepted, 0);
    assert!(invalid.last_accepted_at_ms.is_none());
    // A future receipt is excluded from the window and flagged, not counted
    // as a current acceptance.
    let future_dir = tempfile::tempdir().unwrap();
    let future_db = database(future_dir.path()).unwrap();
    future_db
        .execute(
            "INSERT INTO delivery(at,status) VALUES(?,?)",
            params![20_001, 204],
        )
        .unwrap();
    drop(future_db);
    let future = transport_status_at(future_dir.path(), "tharun-upstox", "2026-10-16", 20_000_000);
    assert_eq!(future.counts.as_ref().unwrap().transport_accepted, 0);
    assert!(future.last_accepted_at_ms.is_none());
    assert!(future.retained_window.as_ref().unwrap().clock_anomaly);
    assert!(
        !fs::read(outbox_dir.path().join("delivery.sqlite"))
            .unwrap()
            .windows(6)
            .any(|b| b == b"secret")
    );
    for i in 0..OUTBOX_LIMIT {
        enqueue(
            &mut outbox_db,
            &Payload::new(Event::Test, None, json!({"sequence":i})),
            30_000 + i,
        )
        .unwrap();
    }
    assert!(
        enqueue(
            &mut outbox_db,
            &Payload::new(Event::Test, None, json!({})),
            31_000,
        )
        .is_err()
    );
    let full_status =
        transport_status_at(outbox_dir.path(), "tharun-upstox", "2026-10-16", 31_000_000);
    assert_eq!(full_status.counts.as_ref().unwrap().queued, OUTBOX_LIMIT);
    let malformed_dir = tempfile::tempdir().unwrap();
    let malformed_path = malformed_dir.path().join("delivery.sqlite");
    let malformed_db = Connection::open(&malformed_path).unwrap();
    malformed_db
        .execute_batch("CREATE TABLE unrelated(x INTEGER)")
        .unwrap();
    drop(malformed_db);
    let malformed = transport_status_at(
        malformed_dir.path(),
        "tharun-upstox",
        "2026-10-16",
        20_000_000,
    );
    assert_eq!(malformed.status, AlertStoreStatus::Malformed);
    assert!(malformed.counts.is_none());
    assert!(malformed.retained_window.is_none());
    let privacy_dir = tempfile::tempdir().unwrap();
    let mut privacy_db = database(privacy_dir.path()).unwrap();
    enqueue(
        &mut privacy_db,
        &Payload::new(Event::Test, None, json!({"secret":"must-not-leak"})),
        1,
    )
    .unwrap();
    drop(privacy_db);
    let privacy = transport_status_at(
        privacy_dir.path(),
        "tharun-upstox",
        "2026-10-16",
        20_000_000,
    );
    let encoded = serde_json::to_string(&privacy).unwrap();
    assert!(!encoded.contains("must-not-leak"));
    assert!(!encoded.contains("payload"));
    assert!(!encoded.contains("webhook"));
    assert!(!encoded.contains("Authorization"));

    assert!(
        !fs::read(dir.path().join("delivery.sqlite"))
            .unwrap()
            .windows(6)
            .any(|b| b == b"secret")
    );
    failure(dir.path());
    assert_eq!(
        fs::read_to_string(dir.path().join("last-failure")).unwrap(),
        "alert_failed_redacted\n"
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(endpoint(&path).is_err());
}
