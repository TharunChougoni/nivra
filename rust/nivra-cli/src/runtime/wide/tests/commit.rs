use super::*;

#[test]
fn stored_paper_cost_policy_cannot_change_on_resume() {
    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = load_wide(&mut db, &c).unwrap();
    assert_eq!(p.costs, c.paper_costs);
    persist(&mut db, &c, &p, start(), None, None, "prepared").unwrap();
    let original: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    c.paper_costs = Some(
        serde_json::from_str(include_str!("../../../../../../fixtures/paper-costs.json")).unwrap(),
    );
    assert_eq!(
        load_wide(&mut db, &c).unwrap_err().to_string(),
        "paper_cost_policy_changed"
    );
    assert_eq!(
        open_book(&c).err().unwrap().to_string(),
        "paper_cost_policy_changed"
    );
    assert_eq!(
        db.query_row("SELECT body FROM state WHERE id=1", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        original
    );
    let d2 = tempfile::tempdir().unwrap();
    let mut fresh = super::super::super::tests::config(d2.path());
    fresh.paper_costs = c.paper_costs.clone();
    let mut new_db = open_book(&fresh).unwrap();
    assert_eq!(load_wide(&mut new_db, &fresh).unwrap().costs, c.paper_costs);
}
#[test]
fn durable_terminal_corruption_and_rollback() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    persist(&mut db, &c, &p, start(), None, None, "stopped").unwrap();
    let loaded = load_wide(&mut db, &c).unwrap();
    assert!(loaded.stopped);
    assert!(
        !p.enter(start(), &instrument(1), &quote(start(), 100))
            .unwrap()
    );
    p.cash = a("1");
    assert!(persist(&mut db, &c, &p, start(), None, None, "bad").is_err());
    assert_eq!(load_wide(&mut db, &c).unwrap(), loaded);
    db.execute_batch("CREATE TRIGGER fail_state BEFORE UPDATE ON state BEGIN SELECT RAISE(ABORT,'fixture'); END;").unwrap();
    let mut current = loaded.clone();
    assert!(
        commit_quotes(
            &mut db,
            &c,
            &mut current,
            &BTreeMap::new(),
            start(),
            "rollback",
            start
        )
        .is_err()
    );
    assert_eq!(current, loaded);
}
#[test]
fn after_lock_clock_and_eod_no_invented_exit() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let at = start();
    let i = instrument(1);
    let quotes = BTreeMap::from([(i.key.clone(), quote(at, 100))]);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at + 2001).unwrap();
    assert!(p.fills.is_empty());
    assert!(p.enter(at, &i, &quote(at, 100)).unwrap());
    let close = at + 360 * 60000;
    p.protect(close, &BTreeMap::new()).unwrap();
    assert_eq!(p.positions.len(), 1);
    assert!(!p.halted);
    p.protect(close, &BTreeMap::from([(i.key.clone(), quote(close, 100))]))
        .unwrap();
    assert!(p.positions.is_empty());
    p.reconcile().unwrap();
}
/// Event authority with the given plan as its active head, as the owner path
/// leaves it after an accepted decision.
fn activate_fixture_plan(db: &mut Connection, c: &Config, p: &Portfolio, plan: &serde_json::Value) {
    if !redecision::has_authority(db).unwrap() {
        let cfg: planner::PlannerConfig = serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
        ensure_actions(db).unwrap();
        redecision::initialize(db, c, p, Some(&cfg), start()).unwrap();
    }
    let mut plan = plan.clone();
    let id = plan["decision_id"].as_str().unwrap().to_owned();
    let attempt = format!("fixture-attempt-{id}");
    plan["decision_owner"] = serde_json::json!("pi");
    plan["attempt_id"] = serde_json::json!(attempt);
    let body = plan.to_string();
    db.execute(
        "INSERT INTO planner_plan VALUES(?,?)",
        rusqlite::params![id, body],
    )
    .unwrap();
    db.execute(
        "INSERT INTO planner_attempt_log VALUES(?,?,'{}','planned',?)",
        rusqlite::params![
            attempt,
            format!("fixture-event-{id}"),
            serde_json::json!({"decision_id":id,"plan_sha256":digest(body.as_bytes())}).to_string()
        ],
    )
    .unwrap();
    let control: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let mut control: serde_json::Value = serde_json::from_str(&control).unwrap();
    control["active"] = plan["decision_id"].clone();
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [control.to_string()],
    )
    .unwrap();
}
#[test]
fn planned_fills_commit_freshness_decision_and_exit_intents_atomically() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let i = instrument(1);
    let at = start();
    let input = serde_json::json!({"allowlist":[i]});
    let raw=serde_json::json!({"actions":[{"instrument_key":i.key,"side":"buy","units":2,"max_price":"101","rationale":"fixture","exit_intent":{"target":"110","stop":"90"}}],"stance":"long","notes":""}).to_string();
    let verdicts = planner::validate(&raw, &p, std::slice::from_ref(&i), at).unwrap();
    fs::create_dir_all(c.directory.join("model/fixture-stale")).unwrap();
    let mut plan = serde_json::json!({"decision_id":"fixture-stale","session_binding":book_binding(&c).unwrap(),"input_hash":digest(&serde_json::to_vec(&input).unwrap()),"input":input,"fake":false,"dry_run":false,"verdicts":verdicts});
    activate_fixture_plan(&mut db, &c, &p, &plan);
    let quotes = BTreeMap::from([(i.key.clone(), quote(at, 100))]);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at + 2001).unwrap();
    assert!(p.fills.is_empty());
    // The refusal reason is stored beside the verdict, in the audit file and in
    // the dashboard projection; a fill carries none.
    let refused = execution_projection(&db).unwrap();
    assert_eq!(refused[0]["verdict"], "execution_gate_rejected");
    assert_eq!(refused[0]["reason"], "stale_quote");
    let audit: serde_json::Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/fixture-stale/execution.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(audit[0]["reason"], "stale_quote");
    let facts = serde_json::json!({"quote_age_ms":2001,"receipt_age_ms":2001,"max_age_ms":2000});
    assert_eq!(audit[0]["facts"], facts);
    // The next decision brief restates the refusal by source, with the
    // action, the reason in plain words and the numbers the check used.
    assert_eq!(
        recent_refusals(&db, 5).unwrap(),
        vec![serde_json::json!({
            "at_ms":at + 2001,"source":{"kind":"decision","id":"fixture-stale","index":0},
            "action":{"side":"buy","instrument_key":i.key,"units":2,"max_price":"101"},
            "reason":"stale_quote","reason_text":"the quote was older than the freshness bound",
            "facts":facts,
        })]
    );
    let mut expired = plan.clone();
    expired["decision_id"] = serde_json::json!("fixture-time-expired");
    expired["verdicts"][0]["action"]["exit_intent"]["time"] = serde_json::json!(at + 1);
    fs::create_dir_all(c.directory.join("model/fixture-time-expired")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &expired);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at + 1).unwrap();
    assert!(p.fills.is_empty());
    assert!(p.positions.is_empty());

    plan["decision_id"] = serde_json::json!("fixture-screened");
    fs::create_dir_all(c.directory.join("model/fixture-screened")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes_screened(
        &mut db,
        &c,
        &mut p,
        &quotes,
        at,
        "fixture",
        || at,
        Some(&BTreeSet::new()),
        false,
    )
    .unwrap();
    assert!(p.fills.is_empty() && !p.halted);
    let screened: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='fixture-screened'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(screened, "screen_rejected");
    // A1: an intent bound to a different book revision is refused without a
    // fill; a missing revision remains the legacy compatible behavior.
    plan["decision_id"] = serde_json::json!("fixture-stale-portfolio");
    plan["portfolio_revision"] = serde_json::json!("0".repeat(64));
    fs::create_dir_all(c.directory.join("model/fixture-stale-portfolio")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert!(p.fills.is_empty());
    let stale: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='fixture-stale-portfolio'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stale, "portfolio_changed");
    // A policy hash that cannot be revalidated is refused explicitly.
    plan["decision_id"] = serde_json::json!("fixture-policy-changed");
    plan["portfolio_revision"] = serde_json::json!(redecision::portfolio_hash(&p).unwrap());
    plan["policy_sha256"] = serde_json::json!("a".repeat(64));
    fs::create_dir_all(c.directory.join("model/fixture-policy-changed")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert!(p.fills.is_empty());
    let changed: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='fixture-policy-changed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(changed, "policy_changed");
    plan.as_object_mut().unwrap().remove("policy_sha256");
    plan["portfolio_revision"] = serde_json::json!(redecision::portfolio_hash(&p).unwrap());
    plan["decision_id"] = serde_json::json!("fixture-fresh");
    fs::create_dir_all(c.directory.join("model/fixture-fresh")).unwrap();
    activate_fixture_plan(&mut db, &c, &p, &plan);
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert_eq!(p.fills.len(), 1);
    assert_eq!(p.fills[0].decision_id.as_deref(), Some("fixture-fresh"));
    commit_quotes(&mut db, &c, &mut p, &quotes, at, "fixture", || at).unwrap();
    assert_eq!(p.fills.len(), 1);
    let close = at + 360 * 60000;
    let q = BTreeMap::from([(i.key.clone(), quote(close, 103))]);
    commit_quotes(&mut db, &c, &mut p, &q, close, "fixture", || close).unwrap();
    assert_eq!(p.positions.len(), 1); // No legacy +2% or 15:15 flatten.
    let later = close + 60000;
    let q = BTreeMap::from([(i.key.clone(), quote(later, 111))]);
    // A stock excluded from new buys still receives its protective exit.
    commit_quotes_screened(
        &mut db,
        &c,
        &mut p,
        &q,
        later,
        "fixture",
        || later,
        Some(&BTreeSet::new()),
        false,
    )
    .unwrap();
    assert!(p.positions.is_empty());
    assert_eq!(p.fills[1].decision_id.as_deref(), Some("fixture-fresh"));
    p.reconcile().unwrap();
    assert!(
        c.directory
            .join("model/fixture-fresh/execution.json")
            .exists()
    );
}
#[test]
fn event_planning_suspends_old_actions_without_interrupting_protection() {
    for fault in [
        "none",
        "missing_control",
        "malformed_verdict",
        "malformed_allowlist",
        "audit_projection",
        "missing_actions",
        "observer",
    ] {
        let d = tempfile::tempdir().unwrap();
        let c = super::super::super::tests::config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = crate::runtime::tests::test_portfolio();
        let at = start();
        let i = instrument(1);
        let second = instrument(2);
        let raw=serde_json::json!({"actions":[{"instrument_key":i.key,"side":"buy","units":1,"max_price":"101","rationale":"fixture","exit_intent":{"target":"110","stop":"90"}},{"instrument_key":second.key,"side":"buy","units":1,"max_price":"101","rationale":"fixture","exit_intent":{"stop":"90"}}],"stance":"long","notes":""}).to_string();
        let input = serde_json::json!({"allowlist":[i,second]});
        let plan = serde_json::json!({"decision_id":"fixture-predecessor","session_binding":book_binding(&c).unwrap(),"input_hash":digest(&serde_json::to_vec(&input).unwrap()),"input":input,"fake":false,"dry_run":false,"verdicts":planner::validate(&raw,&p,&[i.clone(),second.clone()],at).unwrap()});
        fs::create_dir_all(c.directory.join("model/fixture-predecessor")).unwrap();
        activate_fixture_plan(&mut db, &c, &p, &plan);
        commit_quotes(
            &mut db,
            &c,
            &mut p,
            &BTreeMap::from([(i.key.clone(), quote(at, 100))]),
            at,
            "fixture",
            || at,
        )
        .unwrap();
        assert_eq!(p.positions.len(), 1);
        assert_eq!(p.fills.len(), 1);
        let cfg:planner::PlannerConfig=serde_json::from_value(serde_json::json!({"enabled":true,"agent":{"mode":"paper","node":"/usr/bin/node","entry":"/opt/nivra/agent/dist/src/main.js","agent_dir":"/var/lib/nivra/pi","lanes":{"decision":{"model":{"provider":"openai-codex","id":"gpt-6.1-sol"},"daily_calls":10,"daily_cost_usd":10.0,"turn_timeout_ms":10000,"max_tool_calls":40}}}})).unwrap();
        let claim = redecision::pending_fixture(&mut db, &c, &p, at + 1000);
        assert_eq!(claim.predecessor.as_deref(), Some("fixture-predecessor"));
        if fault == "missing_control" {
            db.execute_batch("DELETE FROM planner_event_control;")
                .unwrap();
        }
        if [
            "malformed_verdict",
            "malformed_allowlist",
            "audit_projection",
        ]
        .contains(&fault)
        {
            // A failed digest settles the claim without changing the head.
            let mut current = plan.clone();
            current["decision_id"] = serde_json::json!("fixture-successor");
            if fault == "audit_projection" {
                current["verdicts"] = serde_json::json!([]);
            }
            redecision::fail_pending_fixture(&mut db, &claim);
            // A newer accepted plan is the active head; its damaged evidence
            // is read on the next tick.
            db.execute(
                "INSERT INTO planner_plan VALUES(?,?)",
                rusqlite::params!["fixture-successor", current.to_string()],
            )
            .unwrap();
            let control: String = db
                .query_row(
                    "SELECT body FROM planner_event_control WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let mut control: serde_json::Value = serde_json::from_str(&control).unwrap();
            control["active"] = serde_json::json!("fixture-successor");
            db.execute(
                "UPDATE planner_event_control SET body=? WHERE id=1",
                [control.to_string()],
            )
            .unwrap();
            if fault == "malformed_verdict" {
                current["verdicts"] = serde_json::json!([{}]);
            }
            if fault == "malformed_allowlist" {
                current["input"]["allowlist"][0]["tick"] = serde_json::json!("not a price");
                current["input_hash"] =
                    serde_json::json!(digest(&serde_json::to_vec(&current["input"]).unwrap()));
            }
            db.execute(
                "UPDATE planner_plan SET body=? WHERE id=?",
                rusqlite::params![
                    current.to_string(),
                    current["decision_id"].as_str().unwrap()
                ],
            )
            .unwrap();
            if fault == "audit_projection" {
                let path = c
                    .directory
                    .join("model")
                    .join(current["decision_id"].as_str().unwrap());
                fs::create_dir_all(path.join("execution.tmp")).unwrap();
            }
        }
        if fault == "missing_actions" {
            db.execute_batch("DROP TABLE planner_action;").unwrap();
        }
        if fault == "observer" {
            let mut settings = Some(cfg.clone());
            let mut settings_fallback = None;
            let mut settings_reason = None;
            contain_event_error(
                Err("fixture-invalid-event-state".into()),
                &mut db,
                &c,
                &mut p,
                &mut settings,
                &mut settings_fallback,
                &mut settings_reason,
                at + 1200,
            )
            .unwrap();
            assert!(settings.is_none());
            assert!(settings_fallback.is_some());
            assert_eq!(settings_reason, Some("planner_integrity_failure"));
        }
        let later = at + 1500;
        let quotes = BTreeMap::from([
            (i.key.clone(), quote(later, 111)),
            (second.key.clone(), quote(later, 100)),
        ]);
        commit_quotes(&mut db, &c, &mut p, &quotes, later, "fixture", || later).unwrap();
        assert!(p.positions.is_empty(), "{fault}");
        assert_eq!(p.fills.len(), 2);
        assert!(!p.fills[1].buy);
        assert_eq!(p.halted, fault != "none");
        if fault == "missing_actions" {
            assert!(
                !db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='planner_action')",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .unwrap()
            );
        } else {
            let rows:i64=db.query_row("SELECT count(*) FROM planner_action WHERE decision='fixture-predecessor' AND idx=1",[],|r|r.get(0)).unwrap();
            assert_eq!(rows, 0); // Its second action never executes while suspended or damaged.
        }
        p.reconcile().unwrap();
        let saved = load_wide(&mut db, &c).unwrap();
        assert_eq!(p, saved);
    }
}
