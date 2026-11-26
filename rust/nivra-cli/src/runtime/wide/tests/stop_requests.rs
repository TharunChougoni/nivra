use super::*;

fn tick_count(db: &Connection, event: &str) -> i64 {
    db.query_row("SELECT count(*) FROM ticks WHERE event=?", [event], |r| {
        r.get(0)
    })
    .unwrap()
}
fn archived_requests(c: &Config, label: &str) -> usize {
    fs::read_dir(c.sessions_root.as_ref().unwrap().join("stop-requests"))
        .map(|dir| {
            dir.filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(&format!("-{label}.json"))
            })
            .count()
        })
        .unwrap_or(0)
}
#[test]
fn service_shutdown_continues_and_only_a_request_stops_the_book() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let claim = redecision::pending_fixture(&mut db, &c, &p, start());

    // A plain SIGTERM (restart, updater, host stop) is a service shutdown: the
    // cancelled claim is retired failed/no-replay, nothing sticks, the book stays open.
    let intent = shutdown::intent(&c, start() + 1);
    assert_eq!(intent.kind, shutdown::ShutdownKind::Service);
    let details =
        shutdown::settle(&mut db, &c, &mut p, &intent, Some(&claim), start() + 1).unwrap();
    assert_eq!(details["operator_stopped"], false);
    assert_eq!(details["shutdown"], "service");
    assert!(!p.stopped && !p.halted);
    let (state, result): (String, String) = db
        .query_row(
            "SELECT state,result FROM planner_attempt_log WHERE id=?",
            [&claim.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(state, "failed");
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "service_shutdown_interrupted_no_replay");
    assert_eq!(result["charges_preserved"], true);
    assert_eq!(tick_count(&db, "service_shutdown"), 1);
    assert_eq!(tick_count(&db, "operator_stopped"), 0);
    drop(db);
    // The restarted controller continues the same dated book without review.
    let mut db = open_book(&c).unwrap();
    let restarted = load_wide(&mut db, &c).unwrap();
    assert!(!restarted.stopped && stopped_restart_warning(&restarted).is_none());
    assert_eq!(rollover::current(&c).unwrap().session, c.session);
    redecision::initialize(&mut db, &c, &restarted, Some(&cfg), start() + 2).unwrap();
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert!(!redecision::pending_attempt(&db).unwrap());

    // Retirement that cannot be proven exact fails closed behind the reviewed block.
    let failed = tempfile::tempdir().unwrap();
    let (fc, fcfg) = super::super::super::decision::decision_fixture_config(failed.path());
    let mut fdb = open_book(&fc).unwrap();
    let mut fp = load_wide(&mut fdb, &fc).unwrap();
    redecision::initialize(&mut fdb, &fc, &fp, Some(&fcfg), start()).unwrap();
    let mut foreign = redecision::pending_fixture(&mut fdb, &fc, &fp, start());
    foreign.id = "not-the-pending-claim".into();
    let intent = shutdown::intent(&fc, start() + 4);
    let details =
        shutdown::settle(&mut fdb, &fc, &mut fp, &intent, Some(&foreign), start() + 4).unwrap();
    assert_eq!(details["operator_stopped"], false);
    assert!(!fp.stopped && !fp.halted);
    assert_eq!(
        redecision::recovery_block(&fdb, &fc).unwrap(),
        redecision::RecoveryBlock::Reviewed(redecision::ReviewedBlock::AttemptInterrupted)
    );
    let body: String = fdb
        .query_row(
            "SELECT body FROM ticks WHERE event='service_shutdown' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["planner"]["retired"], false);
    assert_eq!(body["planner"]["fallback_block_recorded"], true);

    // Stale, foreign and non-private requests are ignored and archived, never deleted.
    let mut foreign_session = c.clone();
    foreign_session.session = c.session.succ_opt().unwrap();
    shutdown::write_request(&foreign_session, start()).unwrap();
    let ignored = shutdown::intent(&c, start() + 5);
    assert_eq!(ignored.kind, shutdown::ShutdownKind::Service);
    assert_eq!(ignored.request["reason"], "stop_request_foreign_session");
    shutdown::write_request(&c, start()).unwrap();
    let stale = shutdown::intent(&c, start() + shutdown::STOP_REQUEST_TTL_MS + 1);
    assert_eq!(stale.kind, shutdown::ShutdownKind::Service);
    assert_eq!(stale.request["reason"], "stop_request_stale");
    let path = shutdown::request_path(&c).unwrap();
    fs::write(&path, b"{}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        shutdown::intent(&c, start() + 6).request["reason"],
        "stop_request_invalid"
    );
    assert_eq!(archived_requests(&c, "ignored"), 3);
    assert!(!path.exists());

    // `nivra stop` while a controller holds the root lock records a request the
    // controller consumes on SIGTERM; the book then stops exactly as before.
    assert_eq!(
        failure_code(stop(&c, false, start()).unwrap_err().as_ref()),
        "stop_confirm_paper_review_required"
    );
    let running = rollover::controller_lock(&c).unwrap();
    let requested = stop(&c, true, start() + 7).unwrap();
    assert_eq!(requested["stop_requested"], true);
    assert_eq!(
        requested["next_step"],
        "systemctl --user stop nivra-paper.service"
    );
    drop(running);
    let mut p = restarted.clone();
    let intent = shutdown::intent(&c, start() + 8);
    assert_eq!(intent.kind, shutdown::ShutdownKind::Operator);
    let details = shutdown::settle(&mut db, &c, &mut p, &intent, None, start() + 8).unwrap();
    assert_eq!(details["operator_stopped"], true);
    assert!(p.stopped);
    assert_eq!(p.health, "operator_stopped");
    assert_eq!(tick_count(&db, "operator_stopped"), 1);
    assert_eq!(archived_requests(&c, "consumed"), 1);
    assert_eq!(
        shutdown::intent(&c, start() + 9).kind,
        shutdown::ShutdownKind::Service
    );
    drop(db);

    // With no controller running, `nivra stop` stops the active book directly.
    let d = tempfile::tempdir().unwrap();
    let c = super::super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    persist(&mut db, &c, &p, start(), None, None, "open").unwrap();
    drop(db);
    let direct = stop(&c, true, start() + 1).unwrap();
    assert_eq!(direct["controller_running"], false);
    assert_eq!(direct["already_stopped"], false);
    let stopped = load_wide(&mut open_book(&c).unwrap(), &c).unwrap();
    assert!(stopped.stopped && !stopped.halted);
    assert_eq!(
        stop(&c, true, start() + 2).unwrap()["already_stopped"],
        true
    );
    assert!(!shutdown::request_path(&c).unwrap().exists());
}
