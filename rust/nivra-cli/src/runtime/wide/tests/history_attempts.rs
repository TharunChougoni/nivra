use super::*;

#[test]
fn failed_history_attempt_retries_with_delay_and_session_bound() {
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_failed_retry_ms = 100;
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    attempts
        .completed(
            &cfg,
            10,
            &serde_json::json!({"status":"FAILED","failure":"history_preparation_cancelled"}),
        )
        .unwrap();
    let mut attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(!attempts.due(&cfg, 109));
    assert!(attempts.due(&cfg, 110));
    // A recorded late failure retries after the same delay instead of losing the day.
    let late_minute = cfg.history_preparation_cutoff_minute + 30;
    assert!(!late_preparation_due(
        &cfg,
        cfg.session,
        late_minute,
        109,
        &attempts
    ));
    assert!(late_preparation_due(
        &cfg,
        cfg.session,
        late_minute,
        110,
        &attempts
    ));
    attempts.started(&cfg, 110).unwrap();
    attempts
        .completed(&cfg, 120, &serde_json::json!({"status":"FAILED"}))
        .unwrap();
    let attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(!attempts.due(&cfg, 1000));
    assert!(history_attempt_window(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute - 1
    ));
    assert!(!history_attempt_window(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute
    ));
    assert!(!history_attempt_window(
        &cfg,
        cfg.session,
        cfg.execution.session_start_minute
    ));
    assert!(!history_attempt_window(
        &cfg,
        cfg.session.succ_opt().unwrap(),
        cfg.history_preparation_start_minute
    ));
    // A session adopted after the cutoff prepares immediately until the
    // late-start cutoff; exhausted attempts rule out another one.
    let late = cfg.execution.session_start_minute + 75;
    let fresh = HistoryAttempts::default();
    assert!(late_preparation_due(&cfg, cfg.session, late, 0, &fresh));
    assert!(late_preparation_due(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute,
        0,
        &fresh
    ));
    assert!(!late_preparation_due(
        &cfg,
        cfg.session,
        cfg.history_preparation_cutoff_minute - 1,
        0,
        &fresh
    ));
    assert!(!late_preparation_due(&cfg, cfg.session, 900, 0, &fresh));
    assert!(!late_preparation_due(
        &cfg,
        cfg.session.succ_opt().unwrap(),
        late,
        0,
        &fresh
    ));
    assert!(!late_preparation_due(&cfg, cfg.session, late, 0, &attempts));
    let window = preparation::PreparationWindow::LateStart;
    let at = DateTime::parse_from_rfc3339("2026-10-16T10:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let length =
        i64::from(cfg.history_preparation_cutoff_minute - cfg.history_preparation_start_minute)
            * 60_000;
    assert_eq!(
        window.deadline_ms(&cfg, cfg.session, at).unwrap(),
        at + length
    );
    let near_close = DateTime::parse_from_rfc3339("2026-10-16T14:59:00+05:30")
        .unwrap()
        .timestamp_millis();
    let close = DateTime::parse_from_rfc3339("2026-10-16T15:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    assert_eq!(
        window.deadline_ms(&cfg, cfg.session, near_close).unwrap(),
        close
    );
    cfg.market_archive = Some(super::super::super::config::MarketArchiveSettings {
        url: "postgresql://test@localhost/archive".into(),
        password_file: fixture.path().join("archive-password"),
    });
    let request = preparation_request(&cfg, &[instrument(1)], at, window).unwrap();
    assert_eq!(request.target_session, cfg.session);
    assert_eq!(request.deadline_ms, at + length);
    cfg.history_preparation_failed_retry_ms = 0;
    assert!(cfg.validate().is_err());
}
#[test]
fn history_attempts_survive_restarts_without_replay() {
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_retry_ms = 100;
    let mut attempts = HistoryAttempts::default();
    assert!(attempts.due(&cfg, 0));
    attempts.started(&cfg, 0).unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(attempts.count, 1);
    assert_eq!(attempts.last_report()["status"], "IN_PROGRESS");
    assert_ne!(attempts.last_report()["status"], "BLOCKED");
    assert!(attempts.last_report()["reason"].is_null());
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(HistoryAttempts::path(&cfg)).unwrap()).unwrap();
    assert_eq!(persisted["records"][0]["report"]["status"], "IN_PROGRESS");
    assert_ne!(persisted["records"][0]["report"]["status"], "BLOCKED");
    assert!(persisted["records"][0]["completed_ms"].is_null());
    assert!(!attempts.due(&cfg, 1000)); // running claim is not retried
    assert!(attempts.started(&cfg, 1000).is_err());
    attempts
        .completed(
            &cfg,
            10,
            &serde_json::json!({"status":"PARTIAL","retryable_missing_bars":0}),
        )
        .unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(attempts.retry_at, None);
    assert!(!attempts.due(&cfg, 1000));
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    attempts
        .completed(&cfg, 10, &serde_json::json!({"status":"PARTIAL"}))
        .unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(!attempts.due(&cfg, 109));
    assert!(attempts.due(&cfg, 110));
    attempts.started(&cfg, 110).unwrap();
    attempts
        .completed(&cfg, 120, &serde_json::json!({"status":"PARTIAL"}))
        .unwrap();
    attempts = HistoryAttempts::load(&cfg).unwrap();
    assert!(
        attempts
            .completed(&cfg, 130, &serde_json::json!({"status":"READY"}))
            .is_err()
    );
    assert_eq!(attempts.records.len(), 2);
    assert_eq!(attempts.records[0].report["status"], "PARTIAL");
    assert!(!attempts.due(&cfg, 1000)); // configured bound
    for status in ["READY", "UNKNOWN", "BLOCKED"] {
        let mut attempts = HistoryAttempts::default();
        attempts.started(&cfg, 0).unwrap();
        attempts
            .completed(
                &cfg,
                0,
                &serde_json::json!({"status":status,"failure":"retained_diagnostic"}),
            )
            .unwrap();
        let attempts = HistoryAttempts::load(&cfg).unwrap();
        assert_eq!(attempts.last_report()["failure"], "retained_diagnostic");
        assert!(!attempts.due(&cfg, 1000));
    }
    let original = fs::read(HistoryAttempts::path(&cfg)).unwrap();
    let mut wrong = cfg.clone();
    wrong.account = "different-account".into();
    assert!(HistoryAttempts::load(&wrong).is_err());
    wrong = cfg.clone();
    wrong.session = wrong.session.succ_opt().unwrap();
    assert!(HistoryAttempts::load(&wrong).is_err());
    assert_eq!(fs::read(HistoryAttempts::path(&cfg)).unwrap(), original);
    // Invalid/truncated state is refused, preserved, and never reset.
    let corrupt = tempfile::tempdir().unwrap();
    let bad = super::super::super::tests::config(corrupt.path());
    rollover::atomic(
        &HistoryAttempts::path(&bad),
        &serde_json::json!({"count":1}),
    )
    .unwrap();
    let before = fs::read(HistoryAttempts::path(&bad)).unwrap();
    assert!(HistoryAttempts::load(&bad).is_err());
    assert_eq!(fs::read(HistoryAttempts::path(&bad)).unwrap(), before);
    let nonregular = tempfile::tempdir().unwrap();
    let nonregular_cfg = super::super::super::tests::config(nonregular.path());
    fs::create_dir(HistoryAttempts::path(&nonregular_cfg)).unwrap();
    assert!(HistoryAttempts::load(&nonregular_cfg).is_err());
    // A failed durable claim cannot authorize a worker.
    let missing = tempfile::tempdir().unwrap();
    let mut missing_cfg = cfg.clone();
    missing_cfg.directory = missing.path().join("absent");
    assert!(HistoryAttempts::default().started(&missing_cfg, 0).is_err());
    let failed_completion = tempfile::tempdir().unwrap();
    let completion_cfg = super::super::super::tests::config(failed_completion.path());
    let mut incomplete = HistoryAttempts::default();
    incomplete.started(&completion_cfg, 0).unwrap();
    // Fault the existing atomic writer before rename: the durable claim remains.
    fs::create_dir(HistoryAttempts::path(&completion_cfg).with_extension("tmp")).unwrap();
    assert!(
        incomplete
            .completed(
                &completion_cfg,
                10,
                &serde_json::json!({"status":"PARTIAL"})
            )
            .is_err()
    );
    let restored = HistoryAttempts::load(&completion_cfg).unwrap();
    assert!(!restored.due(&completion_cfg, 1000));
    assert_eq!(restored.last_report()["status"], "IN_PROGRESS");
    assert!(restored.last_report()["reason"].is_null());
    cfg.history_preparation_retry_ms = -1;
    assert!(cfg.validate().is_err());
    cfg.history_preparation_retry_ms = 0;
    cfg.history_preparation_check_interval_ms = 0;
    assert!(cfg.validate().is_err());
    cfg.history_preparation_check_interval_ms = 1;
    assert!(cfg.validate().is_ok());
    cfg.session = cfg.session.succ_opt().unwrap();
    assert!(attempts.due(&cfg, 1000)); // next session gets its own bounded passes
    attempts.started(&cfg, 0).unwrap();
    assert_eq!(attempts.count, 1);
    cfg.history_preparation_max_attempts = 0;
    assert!(!HistoryAttempts::default().due(&cfg, 0));
}
#[test]
fn interrupted_history_recovery_settles_no_replay_and_bounds_retry() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_failed_retry_ms = 100;
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    let untouched = [
        ("paper.sqlite", b"sealed-book".as_slice()),
        ("read-journal.json", b"shared-quota".as_slice()),
        (
            "history-quota-receipt.json",
            b"immutable-receipt".as_slice(),
        ),
    ];
    for (name, body) in untouched {
        fs::write(cfg.directory.join(name), body).unwrap();
    }
    let before: Vec<_> = untouched
        .iter()
        .map(|(name, _)| fs::read(cfg.directory.join(name)).unwrap())
        .collect();
    let output = history_recover(&cfg, 10, true).unwrap();
    assert_eq!(output["status"], "recovered");
    assert_eq!(output["attempt"], 1);
    assert_eq!(output["started_ms"], 0);
    assert_eq!(output["completed_ms"], 10);
    assert_eq!(output["retry_at"], 110);
    assert_eq!(output["audit_receipt_written"], true);
    assert_eq!(output["paper_sqlite_touched"], false);
    let stored = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(stored.count, 1);
    assert_eq!(stored.records.len(), 1);
    assert_eq!(stored.records[0].started_ms, 0);
    assert_eq!(stored.records[0].completed_ms, Some(10));
    assert_eq!(stored.records[0].report["status"], "FAILED");
    assert_eq!(
        stored.records[0].report["reason"],
        HISTORY_ATTEMPT_RECOVERED
    );
    assert_eq!(
        stored.records[0].report["original_report"]["status"],
        "IN_PROGRESS"
    );
    assert_eq!(stored.retry_at, Some(110));
    assert!(!stored.due(&cfg, 109));
    assert!(stored.due(&cfg, 110));
    assert!(history_attempt_window(
        &cfg,
        cfg.session,
        cfg.history_preparation_start_minute
    ));
    assert_eq!(
        stored.records[0].report["recovery"]["confirmed_paper_review"],
        true
    );
    assert_eq!(stored.records[0].report["recovery"]["auto_replay"], false);
    // The receipt is immutable, private and describes both states.
    let receipt_path = history_recovery_receipt_path(&cfg, 1, 0);
    let receipt_before = fs::read(&receipt_path).unwrap();
    assert_eq!(
        fs::metadata(&receipt_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let receipt: serde_json::Value = serde_json::from_slice(&receipt_before).unwrap();
    assert_eq!(receipt["kind"], HISTORY_RECOVERY_RECEIPT_KIND);
    assert_eq!(receipt["paper_only"], true);
    assert_eq!(receipt["account"], cfg.account);
    assert_eq!(receipt["session"], cfg.session.to_string());
    assert_eq!(receipt["attempt"]["sequence"], 1);
    assert_eq!(receipt["attempt"]["max_attempts"], 2);
    assert_eq!(receipt["attempt"]["started_ms"], 0);
    assert_eq!(
        receipt["attempt"]["previous_completed_ms"],
        serde_json::Value::Null
    );
    assert_eq!(
        receipt["attempt"]["previous_report"]["status"],
        "IN_PROGRESS"
    );
    assert_eq!(receipt["attempt"]["new_completed_ms"], 10);
    assert_eq!(receipt["attempt"]["new_status"], "FAILED");
    assert_eq!(receipt["attempt"]["new_reason"], HISTORY_ATTEMPT_RECOVERED);
    assert_eq!(receipt["attempt"]["retry_at"], 110);
    assert_eq!(receipt["operator"]["confirmed_paper_review"], true);
    assert_eq!(receipt["recorded_at_ms"], 10);
    let receipts: Vec<_> = fs::read_dir(&cfg.directory)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("history-recovery-")
        })
        .collect();
    assert_eq!(receipts.len(), 1);
    // A repeated command reports success without a second durable mutation.
    let state_before = fs::read(HistoryAttempts::path(&cfg)).unwrap();
    let repeated = history_recover(&cfg, 500, true).unwrap();
    assert_eq!(repeated["status"], "already_recovered");
    assert_eq!(repeated["completed_ms"], 10);
    assert_eq!(repeated["retry_at"], 110);
    assert_eq!(repeated["audit_receipt_written"], false);
    assert_eq!(fs::read(HistoryAttempts::path(&cfg)).unwrap(), state_before);
    assert_eq!(fs::read(&receipt_path).unwrap(), receipt_before);
    for ((name, _), previous) in untouched.iter().zip(&before) {
        assert_eq!(&fs::read(cfg.directory.join(name)).unwrap(), previous);
    }
    // A restart reloads the settled record and normal admission starts the
    // next bounded attempt only after the retry deadline.
    let mut restarted = HistoryAttempts::load(&cfg).unwrap();
    restarted.started(&cfg, 110).unwrap();
    assert_eq!(restarted.count, 2);
    assert_eq!(restarted.records.len(), 2);
    assert_eq!(restarted.records[0].started_ms, 0);
    assert_eq!(restarted.records[0].completed_ms, Some(10));
    assert_eq!(restarted.records[1].completed_ms, None);
    assert_eq!(restarted.records[1].report["status"], "IN_PROGRESS");
}
#[test]
fn history_recovery_accepts_legacy_interrupted_prewrite() {
    let fixture = tempfile::tempdir().unwrap();
    let mut cfg = super::super::super::tests::config(fixture.path());
    cfg.history_preparation_max_attempts = 2;
    cfg.history_preparation_failed_retry_ms = 100;
    rollover::atomic(
        &HistoryAttempts::path(&cfg),
        &serde_json::json!({
            "account":cfg.account,
            "session":cfg.session,
            "count":1,
            "records":[{"started_ms":0,"completed_ms":null,
                "report":{"status":"BLOCKED","reason":HISTORY_ATTEMPT_INTERRUPTED}}],
            "retry_at":null
        }),
    )
    .unwrap();
    let output = history_recover(&cfg, 10, true).unwrap();
    assert_eq!(output["status"], "recovered");
    let stored = HistoryAttempts::load(&cfg).unwrap();
    assert_eq!(stored.records[0].report["status"], "FAILED");
    assert_eq!(
        stored.records[0].report["original_report"]["reason"],
        HISTORY_ATTEMPT_INTERRUPTED
    );
    assert_eq!(stored.retry_at, Some(110));
}
#[test]
fn history_recovery_refuses_unsafe_or_malformed_state() {
    fn refused(cfg: &Config, code: &str) {
        let path = HistoryAttempts::path(cfg);
        let before = fs::read(&path).ok();
        let error = history_recover(cfg, 100, true).unwrap_err();
        assert_eq!(failure_code(error.as_ref()), code);
        assert_eq!(fs::read(&path).ok(), before);
    }
    let fixture = tempfile::tempdir().unwrap();
    let cfg = super::super::super::tests::config(fixture.path());
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 0).unwrap();
    // A live/stopped-lock owner prevents recovery of an in-flight claim.
    let busy = AdvisoryLock::new(
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cfg.directory.join("controller.lock"))
            .unwrap(),
    );
    busy.try_lock_exclusive().unwrap();
    refused(&cfg, "history_recover_controller_busy");
    drop(busy);
    fs::create_dir_all(cfg.sessions_root.as_ref().unwrap()).unwrap();
    let root_busy = AdvisoryLock::new(
        fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(cfg.sessions_root.as_ref().unwrap().join("controller.lock"))
            .unwrap(),
    );
    root_busy.try_lock_exclusive().unwrap();
    refused(&cfg, "history_recover_controller_busy");
    drop(root_busy);
    // A completed attempt is not an interrupted claim, even when recovered.
    attempts
        .completed(&cfg, 5, &serde_json::json!({"status":"PARTIAL"}))
        .unwrap();
    refused(&cfg, "history_recover_attempt_completed");
    // Incomplete state with any other reason is an in-flight/unknown attempt.
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 10).unwrap();
    attempts.records[0].report =
        serde_json::json!({"status":"BLOCKED","reason":"history_preparation_admission_failed"});
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_attempt_not_interrupted");
    // The original count still owns the configured budget.
    let mut exhausted = super::super::super::tests::config(fixture.path());
    exhausted.history_preparation_max_attempts = 1;
    let mut attempts = HistoryAttempts::default();
    attempts.started(&exhausted, 20).unwrap();
    refused(&exhausted, "history_recover_attempt_budget_exhausted");
    // Identity and sequence checks never reset or repair foreign state.
    let mut attempts = HistoryAttempts::default();
    attempts.started(&cfg, 30).unwrap();
    attempts.account = "different-account".into();
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_account_mismatch");
    attempts.account = cfg.account.clone();
    attempts.session = cfg.session.succ_opt();
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_session_mismatch");
    attempts.session = Some(cfg.session);
    attempts.count = 0;
    attempts.save(&cfg).unwrap();
    refused(&cfg, "history_recover_state_invalid");
    rollover::atomic(
        &HistoryAttempts::path(&cfg),
        &serde_json::json!({
            "account":cfg.account,
            "session":cfg.session,
            "count":2,
            "records":[{"started_ms":40,"completed_ms":null,
                "report":{"status":"BLOCKED","reason":HISTORY_ATTEMPT_INTERRUPTED}}],
            "retry_at":null
        }),
    )
    .unwrap();
    refused(&cfg, "history_recover_state_invalid");
    fs::write(HistoryAttempts::path(&cfg), b"{not-json").unwrap();
    refused(&cfg, "history_recover_state_invalid");
    fs::remove_file(HistoryAttempts::path(&cfg)).unwrap();
    refused(&cfg, "history_recover_state_missing");
    fs::create_dir(HistoryAttempts::path(&cfg)).unwrap();
    refused(&cfg, "history_recover_state_not_regular");
    assert!(
        history_recover(&cfg, 100, false)
            .unwrap_err()
            .to_string()
            .contains("confirm_paper_review")
    );
}
