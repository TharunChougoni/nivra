use super::*;

fn issue_137_planner_config(dir: &std::path::Path) -> (Config, planner::PlannerConfig) {
    let mut c = super::super::super::tests::config(dir);
    let cfg: planner::PlannerConfig = serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
    let file = dir.join("planner.json");
    fs::write(&file, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(file);
    (c, cfg)
}
#[test]
fn closed_flat_planner_halt_is_recovered_without_financial_change() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
    cfg.validate_decision_lane().unwrap();
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    p.closed = true;
    p.stopped = true;
    p.halted = true;
    p.health = "planner_authority_invalid_recovery_required".into();
    let cash = p.cash;
    let high_water = p.high_water;
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
    drop(db);

    assert_eq!(
        planner_recover(&c, false).unwrap_err().to_string(),
        "planner_recover_confirm_paper_review_required"
    );
    planner_recover(&c, true).unwrap();

    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.closed && recovered.stopped && !recovered.halted);
    assert_eq!(recovered.health, "closed");
    assert_eq!(recovered.cash, cash);
    assert_eq!(recovered.high_water, high_water);
    assert!(recovered.positions.is_empty() && recovered.fills.is_empty());
    let body: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='operator_planner_recovery_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["mode"], "closed_flat");
    assert_eq!(audit["financial_state_unchanged"], true);
    assert_eq!(audit["halted"], false);
    assert!(!redecision::blocked(&db, &c).unwrap());

    // A sealed flat book keeps a market-data integrity block; the halt clear
    // cannot silently retire it without the shared input proof.
    for block in [redecision::DATA_BLOCK, "planner_news_evidence_invalid"] {
        let d = tempfile::tempdir().unwrap();
        let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        redecision::block(&mut db, &c, block).unwrap();
        p.closed = true;
        p.stopped = true;
        p.halted = true;
        p.health = "planner_authority_invalid_recovery_required".into();
        let cash = p.cash;
        let high_water = p.high_water;
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
        drop(db);
        planner_recover(&c, true).unwrap();
        let mut db = open_book(&c).unwrap();
        let recovered = load_wide(&mut db, &c).unwrap();
        assert!(recovered.closed && recovered.stopped && !recovered.halted);
        assert_eq!(recovered.health, "closed");
        assert_eq!(recovered.cash, cash);
        assert_eq!(recovered.high_water, high_water);
        assert!(redecision::blocked(&db, &c).unwrap(), "{block}");
        let control: serde_json::Value = serde_json::from_str(
            &db.query_row(
                "SELECT body FROM planner_event_control WHERE id=1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(control["blocked"], block);
    }
}
#[test]
fn halted_model_led_book_enters_reviewed_protection_only_mode() {
    for corrected in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let (c, mut cfg) = super::super::super::decision::decision_fixture_config(d.path());
        cfg.validate_decision_lane().unwrap();
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        p.model_led = true;
        assert!(
            p.enter(start(), &instrument(1), &quote(start(), 100))
                .unwrap()
        );
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        if corrected {
            cfg.max_handoff_instruments -= 1;
            fs::write(
                c.planner_config.as_ref().unwrap(),
                serde_json::to_vec(&cfg).unwrap(),
            )
            .unwrap();
        }
        redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
        p.halted = true;
        p.health = "planner_authority_invalid_recovery_required".into();
        let cash = p.cash;
        let fills = p.fills.clone();
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
        drop(db);

        assert_eq!(
            planner_recover_protection_only(&c, false)
                .unwrap_err()
                .to_string(),
            "planner_recover_confirm_paper_review_required"
        );
        planner_recover_protection_only(&c, true).unwrap();

        let mut db = open_book(&c).unwrap();
        let recovered = load_wide(&mut db, &c).unwrap();
        assert!(recovered.halted);
        assert!(!recovered.stopped);
        assert_eq!(recovered.health, "risk_halted_protective_exits_only");
        assert_eq!(recovered.positions.len(), 1);
        assert_eq!(recovered.cash, cash);
        assert_eq!(recovered.fills, fills);
        assert!(redecision::config_current(&db, &cfg).unwrap());
        // The halt remains authoritative: no new planner entry can be reserved.
        let body: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='operator_planner_protection_only_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(audit["mode"], "protection_only");
        assert_eq!(audit["financial_state_unchanged"], true);
        assert_eq!(audit["entries_blocked"], true);
        assert_eq!(audit["halted"], true);
    }
}
#[test]
fn live_config_mismatch_suspends_planner_without_halting_the_book() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let mut current = Some(cfg.clone());
    let mut current_fallback = None;
    let mut current_reason = None;
    contain_event_error(
        Err("planner_event_configuration_changed".into()),
        &mut db,
        &c,
        &mut p,
        &mut current,
        &mut current_fallback,
        &mut current_reason,
        start(),
    )
    .unwrap();
    assert!(!p.halted);
    assert!(current.is_none());
    assert!(current_fallback.is_some());
    assert_eq!(current_reason, Some("planner_event_configuration_changed"));
    assert!(redecision::blocked(&db, &c).unwrap());
    let body: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='planner_configuration_changed_apply_required' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(audit["failed_check"].as_str().is_some());
    assert!(
        audit["recovery_command"]
            .as_str()
            .unwrap()
            .contains("config-apply")
    );
    persist(
        &mut db,
        &c,
        &p,
        start() + 1,
        None,
        None,
        "planner_configuration_changed_apply_required",
    )
    .unwrap();
    drop(db);

    // Repointing the policy in memory
    // has that exact policy applied by the reviewed transition.
    let mut switched = cfg.clone();
    switched.max_handoff_instruments -= 1;
    let out = d.path().join("planner.switched.json");
    fs::write(&out, serde_json::to_vec(&switched).unwrap()).unwrap();
    let mut applied = c.clone();
    applied.planner_config = Some(out.clone());
    config_apply(&applied, true).unwrap();
    let mut db = open_book(&c).unwrap();
    assert!(!redecision::blocked(&db, &c).unwrap());
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert_eq!(recovered.health, "operator_stopped");
    let applied_cfg = planner::PlannerConfig::load_for_session(&out, c.session).unwrap();
    assert!(redecision::config_current(&db, &applied_cfg).unwrap());
    let body: String = db
        .query_row(
            "SELECT body FROM ticks WHERE event='operator_config_apply_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let audit: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(audit["stopped"], true);
}
#[test]
fn planner_incident_never_halts_a_closed_book() {
    let d = tempfile::tempdir().unwrap();
    let (c, mut cfg) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    p.closed = true;
    p.stopped = true;
    p.health = "closed".into();
    persist(&mut db, &c, &p, start(), None, None, "closed").unwrap();
    // A restart after a config edit must not turn a closed book into a
    // planner-authority halt.
    cfg.max_handoff_instruments -= 1;
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    assert!(!redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap());
    assert!(!p.halted);
    assert_eq!(p.health, "closed");
    planner_incident(&mut db, &c, &mut p, start() + 1).unwrap();
    assert!(!p.halted);
    assert_eq!(p.health, "closed");
    let recorded: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='planner_incident_check')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(recorded);
}
#[test]
fn stopped_recovery_retires_interrupted_pending_claim_without_replay() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
    cfg.validate_decision_lane().unwrap();
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let claim = redecision::pending_fixture(&mut db, &c, &p, start());
    // A controller restart observes the reserved claim as interrupted.
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap();
    p.stopped = true;
    p.health = "operator_stopped".into();
    persist(&mut db, &c, &p, start() + 2, None, None, "operator_stopped").unwrap();
    let attempts: i64 = db
        .query_row("SELECT count(*) FROM planner_attempt_log", [], |r| r.get(0))
        .unwrap();
    drop(db);
    assert_eq!(
        redecision::reviewed_stopped_block(&open_book(&c).unwrap(), &c).unwrap(),
        redecision::ReviewedBlock::AttemptInterrupted
    );
    planner_recover_checked(&c, true, false, |_, _, _| Ok(())).unwrap();
    let mut db = open_book(&c).unwrap();
    let (attempt_state, result): (String, String) = db
        .query_row(
            "SELECT state,result FROM planner_attempt_log WHERE id=?",
            [&claim.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(attempt_state, "failed");
    let result: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(result["status"], "operator_review_interrupted_no_replay");
    assert_eq!(result["charges_preserved"], true);
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        attempts
    );
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert!(
            db.query_row(
                "SELECT EXISTS(SELECT 1 FROM ticks WHERE event='operator_pending_planner_attempt_retained')",
                [],
                |r| r.get::<_, bool>(0),
            )
            .unwrap()
        );
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert_eq!(recovered.health, "operator_stopped");
}
fn marker_raw(db: &Connection) -> Option<Vec<u8>> {
    let present: bool = db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_authority_owner')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    if !present {
        return None;
    }
    let value: Option<rusqlite::types::Value> = db
        .query_row(
            "SELECT body FROM planner_authority_owner WHERE id=1",
            [],
            |r| r.get(0),
        )
        .optional()
        .unwrap();
    value.map(|value| match value {
        rusqlite::types::Value::Text(text) => text.into_bytes(),
        rusqlite::types::Value::Blob(bytes) => bytes,
        _ => Vec::new(),
    })
}
fn marker_receipt(db: &Connection) -> Option<serde_json::Value> {
    marker_raw(db).map(|raw| serde_json::from_slice(&raw).unwrap())
}
fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn halted_owner_book(
    dir: &std::path::Path,
) -> (Config, planner::PlannerConfig, Connection, Portfolio) {
    let (c, cfg) = super::super::super::decision::decision_fixture_config(dir);
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    p.halted = true;
    p.stopped = true;
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
    (c, cfg, db, p)
}
/// A book whose authority predates the owner-only policy carries a stored
/// `legacy` owner marker. Initialization can no longer write one, so the
/// fixture rewrites the marker the way an older binary left it.
fn initialize_legacy_marked(
    db: &mut Connection,
    c: &Config,
    p: &Portfolio,
    legacy: &planner::PlannerConfig,
    at: i64,
) {
    redecision::initialize(db, c, p, Some(legacy), at).unwrap();
    let body: String = db
        .query_row(
            "SELECT body FROM planner_authority_owner WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut body: serde_json::Value = serde_json::from_str(&body).unwrap();
    body["owner"] = serde_json::json!("legacy");
    db.execute(
        "UPDATE planner_authority_owner SET body=? WHERE id=1",
        [body.to_string()],
    )
    .unwrap();
}
/// Startup must not destroy the original active head or a stronger sticky
/// block just because a claim is pending or the policy changed.
#[test]
fn startup_preserves_active_and_sticky_block() {
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    let claim = redecision::pending_fixture(&mut db, &c, &p, start());
    db.execute(
        "INSERT INTO planner_plan VALUES('legacy-plan',?)",
        [serde_json::json!({"decision_id":"legacy-plan"}).to_string()],
    )
    .unwrap();
    let raw: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut control: serde_json::Value = serde_json::from_str(&raw).unwrap();
    control["active"] = serde_json::json!("legacy-plan");
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [control.to_string()],
    )
    .unwrap();
    let plans: i64 = db
        .query_row("SELECT count(*) FROM planner_plan", [], |r| r.get(0))
        .unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap();
    let control: serde_json::Value = serde_json::from_str(
        &db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(control["active"], "legacy-plan");
    assert_eq!(control["blocked"], "attempt_interrupted_no_replay");
    assert_eq!(control["pending"]["id"], claim.id);
    assert_eq!(marker_receipt(&db).unwrap()["owner"], "pi");
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_plan", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        plans
    );
    // A pending claim must not replace a stronger sticky data block.
    let d = tempfile::tempdir().unwrap();
    let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    redecision::pending_fixture(&mut db, &c, &p, start());
    redecision::block(&mut db, &c, redecision::DATA_BLOCK).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start() + 1).unwrap();
    let control: serde_json::Value = serde_json::from_str(
        &db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(control["blocked"], redecision::DATA_BLOCK);
    assert!(control["pending"].is_object());

    // A config edit must not replace the same sticky block or clear the head.
    let mut corrected = cfg.clone();
    corrected.max_handoff_instruments -= 1;
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&corrected).unwrap(),
    )
    .unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&corrected), start() + 2).unwrap();
    let control: serde_json::Value = serde_json::from_str(
        &db.query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get::<_, String>(0),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(control["blocked"], redecision::DATA_BLOCK);
    assert_eq!(control["active"], serde_json::Value::Null);
}
/// Halted recovery must run the shared input proof before clearing a data or
/// news block; a configuration block is resolved by the exact policy rebind.
#[test]
fn halted_recovery_requires_block_proof() {
    for block in [redecision::DATA_BLOCK, "planner_news_evidence_invalid"] {
        let d = tempfile::tempdir().unwrap();
        let (c, _cfg, mut db, _p) = halted_owner_book(d.path());
        redecision::block(&mut db, &c, block).unwrap();
        let before = recovery_authority_snapshot(&db);
        drop(db);
        assert_eq!(
            planner_recover(&c, true).unwrap_err().to_string(),
            "planner_recover_input_unavailable",
            "{block}"
        );
        let db = open_book(&c).unwrap();
        assert_eq!(recovery_authority_snapshot(&db), before, "{block}");
    }

    // A configuration block is resolved by the exact policy rebind, not by a
    // no-op validator; the book still has to pass the ordinary halt checks.
    let d = tempfile::tempdir().unwrap();
    let (c, _cfg, mut db, p) = halted_owner_book(d.path());
    redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
    drop(db);
    planner_recover(&c, true).unwrap();
    let mut db = open_book(&c).unwrap();
    let recovered = load_wide(&mut db, &c).unwrap();
    assert!(recovered.stopped && !recovered.halted);
    assert!(!redecision::blocked(&db, &c).unwrap());
    assert_eq!(recovered.cash, p.cash);
    assert_eq!(recovered.positions, p.positions);
    assert_eq!(recovered.fills, p.fills);
}
/// Protection-only recovery keeps the halt and cannot clear a market-data
/// integrity block without the shared input proof.
#[test]
fn protection_only_retains_integrity_block() {
    for block in [redecision::DATA_BLOCK, "planner_news_evidence_invalid"] {
        let d = tempfile::tempdir().unwrap();
        let (c, cfg) = super::super::super::decision::decision_fixture_config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        p.model_led = true;
        assert!(
            p.enter(start(), &instrument(1), &quote(start(), 100))
                .unwrap()
        );
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        redecision::block(&mut db, &c, block).unwrap();
        p.halted = true;
        p.health = "planner_authority_invalid_recovery_required".into();
        let cash = p.cash;
        let fills = p.fills.clone();
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
        drop(db);
        planner_recover_protection_only(&c, true).unwrap();
        let mut db = open_book(&c).unwrap();
        let recovered = load_wide(&mut db, &c).unwrap();
        assert!(recovered.halted && !recovered.stopped);
        assert_eq!(recovered.health, "risk_halted_protective_exits_only");
        assert_eq!(recovered.cash, cash);
        assert_eq!(recovered.fills, fills);
        assert_eq!(recovered.positions.len(), 1);
        assert!(redecision::blocked(&db, &c).unwrap(), "{block}");
        let control: serde_json::Value = serde_json::from_str(
            &db.query_row(
                "SELECT body FROM planner_event_control WHERE id=1",
                [],
                |r| r.get::<_, String>(0),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(control["blocked"], block);
    }
}
fn recovery_authority_snapshot(db: &Connection) -> serde_json::Value {
    let portfolio: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let authority: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let ticks: i64 = db
        .query_row("SELECT count(*) FROM ticks", [], |r| r.get(0))
        .unwrap();
    let attempts: Vec<(String, String, Option<String>)> = db
        .prepare("SELECT id,state,result FROM planner_attempt_log ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    let receipt_exists: bool = db
        .query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='planner_authority_owner')", [], |r| r.get(0))
        .unwrap();
    let receipt = if receipt_exists {
        marker_raw(db)
            .map(|raw| serde_json::json!({"raw_hex":hex_bytes(&raw),"sha256":digest(&raw)}))
    } else {
        None
    };
    serde_json::json!({"portfolio":portfolio,"authority":authority,"ticks":ticks,
        "attempts":attempts,"receipt_exists":receipt_exists,"receipt":receipt})
}
#[test]
fn recovery_owner_receipt_compatibility() {
    for case in [
        "pre_marker",
        "corrected_owner",
        "invalid_version",
        "invalid_binding",
        "invalid_owner",
        "stale_hash",
        "missing_row",
    ] {
        let d = tempfile::tempdir().unwrap();
        let (c, mut cfg) = super::super::super::decision::decision_fixture_config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = load_wide(&mut db, &c).unwrap();
        redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
        p.halted = true;
        p.stopped = true;
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
        if case == "corrected_owner" {
            cfg.max_handoff_instruments -= 1;
            fs::write(
                c.planner_config.as_ref().unwrap(),
                serde_json::to_vec(&cfg).unwrap(),
            )
            .unwrap();
        }
        let expected_error = match case {
            // A journal that predates the owner marker has no proven owner.
            "pre_marker" => {
                db.execute_batch("DROP TABLE planner_authority_owner")
                    .unwrap();
                Some("planner_authority_owner_missing")
            }
            "invalid_version" | "invalid_binding" | "invalid_owner" | "stale_hash" => {
                let raw: String = db
                    .query_row(
                        "SELECT body FROM planner_authority_owner WHERE id=1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut receipt: serde_json::Value = serde_json::from_str(&raw).unwrap();
                match case {
                    "invalid_version" => receipt["version"] = serde_json::json!(2),
                    "invalid_binding" => receipt["binding"] = serde_json::json!("other-book"),
                    "invalid_owner" => receipt["owner"] = serde_json::json!("legacy"),
                    _ => receipt["config_hash"] = serde_json::json!("stale"),
                }
                db.execute(
                    "UPDATE planner_authority_owner SET body=? WHERE id=1",
                    [receipt.to_string()],
                )
                .unwrap();
                Some(if case == "invalid_owner" {
                    "planner_authority_owner_invalid"
                } else {
                    "planner_authority_owner_mismatch"
                })
            }
            "missing_row" => {
                db.execute("DELETE FROM planner_authority_owner WHERE id=1", [])
                    .unwrap();
                Some("planner_authority_owner_missing")
            }
            _ => None,
        };
        let before = recovery_authority_snapshot(&db);
        drop(db);
        if let Some(expected) = expected_error {
            assert_eq!(
                planner_recover(&c, true).unwrap_err().to_string(),
                expected,
                "{case}"
            );
            assert_eq!(
                recovery_authority_snapshot(&open_book(&c).unwrap()),
                before,
                "{case}"
            );
        } else {
            planner_recover(&c, true).unwrap();
            let mut db = open_book(&c).unwrap();
            let recovered = load_wide(&mut db, &c).unwrap();
            assert!(recovered.stopped && !recovered.halted);
            assert_eq!(recovered.cash, p.cash);
            assert_eq!(recovered.positions, p.positions);
            assert_eq!(recovered.fills, p.fills);
            assert!(redecision::config_current(&db, &cfg).unwrap());
            redecision::decision_startup_check(&db, &c, &cfg).unwrap();
            let raw: String = db
                .query_row(
                    "SELECT body FROM planner_authority_owner WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let receipt: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(receipt["owner"], "pi");
            assert_eq!(
                receipt["config_hash"],
                digest(&planner::policy_bytes(&cfg).unwrap())
            );
        }
    }
    // Capture the current-code dead ends before the repair path exists. These
    // stay as regression checks: repair is an explicit offline command, so the
    // controller still refuses a stale marker until that command runs.
    let d = tempfile::tempdir().unwrap();
    let (c, mut cfg) = super::super::super::decision::decision_fixture_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), start()).unwrap();
    p.halted = true;
    p.stopped = true;
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
    let prior = digest(&planner::policy_bytes(&cfg).unwrap());
    cfg.max_handoff_instruments -= 1;
    let current = digest(&planner::policy_bytes(&cfg).unwrap());
    fs::write(
        c.planner_config.as_ref().unwrap(),
        serde_json::to_vec(&cfg).unwrap(),
    )
    .unwrap();
    let raw: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut authority: serde_json::Value = serde_json::from_str(&raw).unwrap();
    authority["config_hash"] = serde_json::json!(current);
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [authority.to_string()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,?,'operator_planner_config_applied')",
        rusqlite::params![
            start(),
            serde_json::json!({"prior_config_hash":prior,"current_config_hash":current})
                .to_string()
        ],
    )
    .unwrap();
    drop(db);
    let error = planner_recover(&c, true).unwrap_err().to_string();
    eprintln!("authority_repair_red: stale_marker_recover_refused={error}");
    assert_eq!(error, "planner_authority_owner_mismatch");

    let d = tempfile::tempdir().unwrap();
    let (c, legacy) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    initialize_legacy_marked(&mut db, &c, &p, &legacy, start());
    p.halted = true;
    p.stopped = true;
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
    drop(db);
    let (owner_c, owner) = super::super::super::decision::decision_fixture_config(d.path());
    owner.validate_decision_lane().unwrap();
    let recover_error = planner_recover(&owner_c, true).unwrap_err().to_string();
    let apply_error = config_apply(&owner_c, true).unwrap_err().to_string();
    eprintln!(
        "authority_repair_red: halted_legacy_recover={recover_error} config_apply={apply_error}"
    );
    assert_eq!(recover_error, "planner_authority_owner_invalid");
    assert_eq!(apply_error, "config_apply_halted_requires_planner_recover");

    let d = tempfile::tempdir().unwrap();
    let (c, legacy) = issue_137_planner_config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = load_wide(&mut db, &c).unwrap();
    initialize_legacy_marked(&mut db, &c, &p, &legacy, start());
    mark_operator_stopped(&mut p);
    persist(&mut db, &c, &p, start(), None, None, "operator_stopped").unwrap();
    let before = recovery_authority_snapshot(&db);
    drop(db);
    let (owner_c, _) = super::super::super::decision::decision_fixture_config(d.path());
    assert_eq!(
        config_apply(&owner_c, true).unwrap_err().to_string(),
        "planner_authority_owner_invalid"
    );
    let db = open_book(&owner_c).unwrap();
    assert_eq!(recovery_authority_snapshot(&db), before);
}
