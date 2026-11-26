use super::*;

#[test]
fn resume_preflight_requires_flat_paper_operator_stop() {
    let mut p = crate::runtime::tests::test_portfolio();
    assert_eq!(resume_preflight(&p), Err("resume_not_operator_stopped"));
    p.stopped = true;
    assert_eq!(resume_preflight(&p), Ok(()));
    p.halted = true;
    assert_eq!(resume_preflight(&p), Err("resume_risk_halted"));
    p.halted = false;
    p.closed = true;
    assert_eq!(resume_preflight(&p), Err("resume_session_closed"));
    p.closed = false;
    p.stopped = false;
    assert!(
        p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    p.stopped = true;
    assert_eq!(resume_preflight(&p), Err("resume_open_positions"));
    p.model_led = true;
    assert_eq!(resume_preflight(&p), Ok(()));
}
#[test]
fn resume_clears_operator_stop_with_audit_when_flat() {
    // A fork before exec retains the same open file description, even with
    // CLOEXEC. Model sandbox tests can fork while this test owns a lock.
    // A duplicate models that inheritance without spawning a process.
    let lock_dir = tempfile::tempdir().unwrap();
    let lock_config = super::super::super::tests::config(lock_dir.path());
    let owner = rollover::controller_lock(&lock_config).unwrap();
    let inherited = owner.0.try_clone().unwrap();
    assert!(rollover::controller_lock(&lock_config).is_err());
    drop(owner);
    let next = rollover::controller_lock(&lock_config).unwrap();
    drop(next);
    drop(inherited);

    let mut sealed = crate::runtime::tests::test_portfolio();
    sealed.closed = true;
    sealed.health = "closed".into();
    let before = serde_json::to_vec(&sealed).unwrap();
    assert!(stopped_restart_warning(&sealed).is_none());
    mark_operator_stopped(&mut sealed);
    assert_eq!(serde_json::to_vec(&sealed).unwrap(), before);

    let d = tempfile::tempdir().unwrap();
    let c = super::super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    p.health = "operator_stopped".into();
    persist(&mut db, &c, &p, start(), None, None, "operator_stopped").unwrap();
    drop(db);
    assert!(
        stopped_restart_warning(&p)
            .unwrap()
            .contains("resume --confirm-paper-review")
    );
    assert!(
        stopped_restart_warning(&p)
            .unwrap()
            .contains("expected_not_failure")
    );
    let mut halted = p.clone();
    halted.halted = true;
    assert!(
        !stopped_restart_warning(&halted)
            .unwrap()
            .contains("expected_not_failure")
    );
    resume(&c, true).unwrap();
    let mut db = open_book(&c).unwrap();
    let loaded = load_wide(&mut db, &c).unwrap();
    assert!(!loaded.stopped);
    assert!(stopped_restart_warning(&loaded).is_none());
    assert!(loaded.model_led);
    assert_eq!(loaded.health, "waiting");
    let events: Vec<String> = db
        .prepare("SELECT event FROM ticks ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert!(
        events
            .iter()
            .any(|e| e == "operator_resume_confirmed_paper_review")
    );
    assert!(resume(&c, true).is_err());
    assert!(resume(&c, false).is_err());
    // A planner-only halt can be rebound, but recovery itself never resumes.
    // Recovery success is owner-only; the legacy Decide path is refused in
    // production and covered by the explicit gate cases.
    drop(db);
    let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    assert!(p.model_led);
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    p.halted = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut db,
        &c,
        &p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    let attempts_before: i64 = db
        .query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap();
    drop(db);
    assert!(planner_recover(&c, false).is_err());
    planner_recover(&c, true).unwrap();
    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        attempts_before
    );

    // A stopped data block has no automatic recovery, but explicit review
    // can recover after input validation; failure preserves state and charges.
    redecision::block(&mut db, &c, "planner_data_failed_no_replay").unwrap();
    let before: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    assert_eq!(
        planner_recover_checked(&c, true, false, |_, _, _| Err(
            "planner_recover_no_eligible_input".into()
        ))
        .unwrap_err()
        .to_string(),
        "planner_recover_no_eligible_input"
    );
    let db = open_book(&c).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        before
    );
    drop(db);
    let checked = std::cell::Cell::new(false);
    planner_recover_checked(&c, true, false, |_, _, _| {
        checked.set(true);
        Ok(())
    })
    .unwrap();
    assert!(checked.get());
    let mut db = open_book(&c).unwrap();
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap(),
        attempts_before
    );
    assert!(load_wide(&mut db, &c).unwrap().stopped);
    let clean_control: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    redecision::block(&mut db, &c, "attempt_interrupted_no_replay").unwrap();
    drop(db);
    assert_eq!(
        planner_recover_checked(&c, true, false, |_, _, _| panic!(
            "must not validate an ambiguous block"
        ))
        .unwrap_err()
        .to_string(),
        "planner_recover_stopped_block_not_eligible"
    );
    let mut db = open_book(&c).unwrap();
    // Restore only this injected ambiguous fixture before the next scenario.
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [&clean_control],
    )
    .unwrap();
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    drop(db);
    planner_recover_checked(&c, true, false, |_, _, _| Ok(())).unwrap();

    // A flat zero-fill book is recoverable only while its original planner
    // authority journal exists and is bound to this book.
    let missing_dir = tempfile::tempdir().unwrap();
    let mut missing = super::super::super::tests::config(missing_dir.path());
    let missing_config = missing_dir.path().join("planner.json");
    fs::write(&missing_config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    missing.planner_config = Some(missing_config);
    let mut missing_db = open_book(&missing).unwrap();
    let mut missing_p = load_wide(&mut missing_db, &missing).unwrap();
    redecision::initialize(&mut missing_db, &missing, &missing_p, Some(&cfg), start()).unwrap();
    redecision::block(
        &mut missing_db,
        &missing,
        "planner_event_configuration_changed",
    )
    .unwrap();
    missing_p.halted = true;
    missing_p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut missing_db,
        &missing,
        &missing_p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    drop(missing_db);
    let missing_db = Connection::open(missing.directory.join("paper.sqlite")).unwrap();
    missing_db
            .execute_batch(
                "DROP TABLE planner_event_control; DELETE FROM ticks WHERE event='event_planner_authority';",
            )
            .unwrap();
    let missing_state_before: String = missing_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    drop(missing_db);
    assert_eq!(
        planner_recover(&missing, true).unwrap_err().to_string(),
        "planner_recover_authority_missing"
    );
    let missing_db = Connection::open(missing.directory.join("paper.sqlite")).unwrap();
    let missing_state_after: String = missing_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let control_exists: bool = missing_db
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_event_control')",
                [],
                |r| r.get(0),
            )
            .unwrap();
    assert_eq!(missing_state_after, missing_state_before);
    assert!(!control_exists);
    drop(missing_db);

    // A present but mismatched control binding also refuses without
    // changing either financial state or the corrupt control evidence.
    let corrupt_dir = tempfile::tempdir().unwrap();
    let mut corrupt = super::super::super::tests::config(corrupt_dir.path());
    let corrupt_config = corrupt_dir.path().join("planner.json");
    fs::write(&corrupt_config, serde_json::to_vec(&cfg).unwrap()).unwrap();
    corrupt.planner_config = Some(corrupt_config);
    let mut corrupt_db = open_book(&corrupt).unwrap();
    let mut corrupt_p = load_wide(&mut corrupt_db, &corrupt).unwrap();
    redecision::initialize(&mut corrupt_db, &corrupt, &corrupt_p, Some(&cfg), start()).unwrap();
    redecision::block(
        &mut corrupt_db,
        &corrupt,
        "planner_event_configuration_changed",
    )
    .unwrap();
    corrupt_p.halted = true;
    corrupt_p.health = "planner_authority_invalid_recovery_required".into();
    persist(
        &mut corrupt_db,
        &corrupt,
        &corrupt_p,
        start(),
        None,
        None,
        "planner_authority_invalid",
    )
    .unwrap();
    let mut control: serde_json::Value = corrupt_db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| {
                let body: String = r.get(0)?;
                Ok(serde_json::from_str(&body).unwrap())
            },
        )
        .unwrap();
    control["binding"] = serde_json::json!("mismatched-book-binding");
    let corrupt_control_before = serde_json::to_string(&control).unwrap();
    corrupt_db
        .execute(
            "UPDATE planner_event_control SET body=? WHERE id=1",
            [&corrupt_control_before],
        )
        .unwrap();
    let corrupt_state_before: String = corrupt_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    drop(corrupt_db);
    assert_eq!(
        planner_recover(&corrupt, true).unwrap_err().to_string(),
        "planner_event_binding_changed"
    );
    let corrupt_db = Connection::open(corrupt.directory.join("paper.sqlite")).unwrap();
    let corrupt_state_after: String = corrupt_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let corrupt_control_after: String = corrupt_db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(corrupt_state_after, corrupt_state_before);
    assert_eq!(corrupt_control_after, corrupt_control_before);
    drop(corrupt_db);

    // A risk halt is never recovered by the planner path.
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    p.halted = true;
    p.health = "daily_loss_tripwire".into();
    persist(&mut db, &c, &p, start(), None, None, "daily_loss_tripwire").unwrap();
    drop(db);
    assert!(planner_recover(&c, true).is_err());
}
#[test]
fn resume_refuses_missing_book_open_position_and_halt() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::super::tests::config(d.path());
    let error = resume(&c, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_book_missing");
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    p.halted = true;
    persist(&mut db, &c, &p, start(), None, None, "halted").unwrap();
    drop(db);
    let error = resume(&c, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_risk_halted");
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    assert!(
        p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    p.stopped = true;
    persist(&mut db, &c, &p, start(), None, None, "open").unwrap();
    drop(db);
    let error = resume(&c, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_open_positions");
    let loaded = load_wide(&mut open_book(&c).unwrap(), &c).unwrap();
    assert!(loaded.stopped);
    assert_eq!(loaded.positions.len(), 1);
}
#[test]
fn resume_resolves_active_session_book_and_keeps_safety_checks() {
    let d = tempfile::tempdir().unwrap();
    let sessions = d.path().join("sessions");
    let legacy_dir = d.path().join("legacy");
    fs::create_dir_all(&sessions).unwrap();
    fs::create_dir_all(&legacy_dir).unwrap();
    let mut legacy = super::super::super::tests::config(&legacy_dir);
    legacy.sessions_root = Some(sessions.clone());
    let mut db = open_book(&legacy).unwrap();
    let mut predecessor = crate::runtime::tests::test_portfolio();
    predecessor.stopped = true;
    predecessor.closed = true;
    predecessor.health = "operator_stopped".into();
    persist(
        &mut db,
        &legacy,
        &predecessor,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    drop(db);
    let active_dir = sessions.join("2026-10-22");
    fs::create_dir_all(&active_dir).unwrap();
    let capital = serde_json::json!(legacy.allocation().unwrap());
    rollover::atomic(
        &active_dir.join("strategy-decision.json"),
        &serde_json::json!({
            "reviewer": "none_model_led",
            "live_enabled": false,
            "chosen": {
                "strategy_id": "model_led_v1",
                "execution_selector": legacy.strategy,
                "capital": capital,
            },
            "capital": capital,
        }),
    )
    .unwrap();
    rollover::atomic(
        &sessions.join("current.json"),
        &serde_json::json!({"account": legacy.account, "session": "2026-10-22"}),
    )
    .unwrap();
    let active = rollover::current(&legacy).unwrap();
    assert_eq!(active.session, "2026-10-22".parse().unwrap());
    assert_eq!(active.previous_session, "2026-10-21".parse().unwrap());
    assert_ne!(active.directory, legacy.directory);
    let mut db = open_book(&active).unwrap();
    let mut book = crate::runtime::tests::test_portfolio();
    book.stopped = true;
    book.health = "operator_stopped".into();
    persist(
        &mut db,
        &active,
        &book,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    let cfg: planner::PlannerConfig = serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
    redecision::initialize(&mut db, &active, &book, Some(&cfg), start()).unwrap();
    let authority: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    drop(db);
    resume(&legacy, true).unwrap();
    let mut db = open_book(&active).unwrap();
    let unchanged: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(authority, unchanged);
    let resumed = load_wide(&mut db, &active).unwrap();
    redecision::initialize(&mut db, &active, &resumed, Some(&cfg), start()).unwrap();
    assert!(!redecision::blocked(&db, &active).unwrap());
    let audits: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='operator_model_led_enabled_v1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audits, 1);
    drop(db);
    let resumed = load_wide(&mut open_book(&active).unwrap(), &active).unwrap();
    assert!(!resumed.stopped);
    assert!(resumed.model_led);
    assert!(!resumed.closed);
    assert!(!resumed.halted);
    assert!(resumed.positions.is_empty());
    assert_eq!(resumed.health, "waiting");
    let untouched = load_wide(&mut open_book(&legacy).unwrap(), &legacy).unwrap();
    assert!(untouched.stopped);
    assert!(untouched.closed);
    let db = open_book(&active).unwrap();
    // Sanctioned config changes preserve original usage and predecessor seals.
    let planner_path = d.path().join("planner.json");
    let mut edited = cfg.clone();
    edited.model_led = Some(true);
    edited.max_handoff_instruments -= 1;
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    legacy.planner_config = Some(planner_path.clone());
    assert!(config_apply(&legacy, false).is_err());
    drop(db);
    config_apply(&legacy, true).unwrap();
    let active_config = rollover::current(&legacy).unwrap();
    let mut db = open_book(&active_config).unwrap();
    let applied = load_wide(&mut db, &active_config).unwrap();
    assert!(applied.stopped && applied.model_led && !applied.halted);
    redecision::initialize(&mut db, &active_config, &applied, Some(&edited), start()).unwrap();
    assert!(!redecision::blocked(&db, &active_config).unwrap());
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM ticks WHERE event='operator_planner_config_applied'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    // A carried model-led book adopts a planner change and keeps its holdings.
    let mut held = applied.clone();
    held.stopped = false;
    assert!(
        held.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    held.stopped = true;
    persist(
        &mut db,
        &active_config,
        &held,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    drop(db);
    edited.max_handoff_instruments -= 1;
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert_eq!(
        failure_code(resume(&legacy, true).unwrap_err().as_ref()),
        "resume_planner_config_not_applied"
    );
    config_apply(&legacy, true).unwrap();
    let mut db = open_book(&active_config).unwrap();
    let carried = load_wide(&mut db, &active_config).unwrap();
    assert_eq!(carried.positions.len(), 1);
    assert_eq!(carried.cash, held.cash);
    drop(db);
    edited.model_led = Some(false);
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert_eq!(
        config_apply(&legacy, true).unwrap_err().to_string(),
        "config_apply_book_not_flat"
    );
    let mut db = open_book(&active_config).unwrap();
    persist(
        &mut db,
        &active_config,
        &applied,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    edited.model_led = Some(true);
    // Explicit false is an intentional no-call policy, not a tamper incident.
    drop(db);
    edited.model_led = Some(false);
    fs::write(&planner_path, serde_json::to_vec(&edited).unwrap()).unwrap();
    config_apply(&legacy, true).unwrap();
    resume(&legacy, true).unwrap();
    let mut db = open_book(&active_config).unwrap();
    let disabled = load_wide(&mut db, &active_config).unwrap();
    assert!(!disabled.model_led);
    redecision::initialize(&mut db, &active_config, &disabled, Some(&edited), start()).unwrap();
    assert!(!redecision::blocked(&db, &active_config).unwrap());
    let mut halted = applied.clone();
    halted.halted = true;
    halted.health = "risk_halt".into();
    persist(
        &mut db,
        &active_config,
        &halted,
        start(),
        None,
        None,
        "risk_halt",
    )
    .unwrap();
    drop(db);
    assert_eq!(
        config_apply(&legacy, true).unwrap_err().to_string(),
        "config_apply_halted_requires_planner_recover"
    );
    assert_eq!(
        planner_recover(&legacy, true).unwrap_err().to_string(),
        "planner_recover_halt_not_eligible_last_halt_not_planner_authority"
    );
    let mut db = open_book(&active).unwrap();
    let mut closed = resumed;
    closed.stopped = true;
    closed.closed = true;
    persist(
        &mut db,
        &active,
        &closed,
        start(),
        None,
        None,
        "operator_stopped",
    )
    .unwrap();
    drop(db);
    let error = resume(&legacy, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "resume_session_closed");
    let active_book = load_wide(&mut open_book(&active).unwrap(), &active).unwrap();
    assert!(active_book.stopped);
    assert!(active_book.closed);
    let predecessor = load_wide(&mut open_book(&legacy).unwrap(), &legacy).unwrap();
    assert!(predecessor.stopped);
    assert!(predecessor.closed);
}
