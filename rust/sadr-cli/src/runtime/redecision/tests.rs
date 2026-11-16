use super::*;
use sadr_core::execution::Instrument;
use sadr_core::finance::Price;
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
#[test]
fn collapsed_recheck_cites_real_triggers() {
    let db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE planner_event_log(id TEXT PRIMARY KEY,at INTEGER,body TEXT,state TEXT)",
    )
    .unwrap();
    for (id, at, body) in [
        ("p1", 1, json!({"kind":"price_move","instrument_key":"A"})),
        ("p2", 2, json!({"kind":"price_move","instrument_key":"B"})),
        (
            "s1",
            3,
            json!({"kind":"recheck","reason":"scanner_prospect_move","trigger":{"keys":["C"]}}),
        ),
    ] {
        db.execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
            rusqlite::params![id, at, body.to_string()],
        )
        .unwrap();
    }
    let pending = logged_pending(&db).unwrap();
    let refs = pending.iter().collect::<Vec<_>>();
    insert_recheck(&db, "binding", 10, 3, &refs).unwrap();
    let body: String = db
        .query_row("SELECT body FROM planner_event_log WHERE at=10", [], |r| {
            r.get(0)
        })
        .unwrap();
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["reason"], "planner_event_backlog_collapsed");
    assert_eq!(v["cause"], "collapsed_price_move");
    assert_eq!(v["collapsed_kinds"]["scanner_prospect_move"], 1);
    assert_eq!(
        v["collapsed_triggers"][0],
        json!(["scanner_prospect_move", "C"])
    );
    assert_eq!(trigger_reason(&v), "collapsed_price_move");
    let old = json!({"kind":"recheck","reason":"planner_event_backlog_collapsed"});
    assert_eq!(trigger_reason(&old), "planner_event_backlog_collapsed");

    // A terminal research event is stable across driver retries and keeps
    // its original terminal receipt time instead of generating clock wakes.
    let root = tempfile::tempdir().unwrap();
    let (c, owner_cfg) = super::super::decision_owner::owner_fixture_config(root.path());
    let mut owner_db = open_book(&c).unwrap();
    let portfolio = crate::runtime::tests::test_portfolio();
    initialize(&mut owner_db, &c, &portfolio, Some(&owner_cfg), at()).unwrap();
    let task_id = "a".repeat(64);
    let result_sha256 = "b".repeat(64);
    enqueue_owner_research_event(
        &owner_db,
        &c,
        &task_id,
        2,
        Some(&result_sha256),
        "succeeded",
        at() + 100,
    )
    .unwrap();
    enqueue_owner_research_event(
        &owner_db,
        &c,
        &task_id,
        2,
        Some(&result_sha256),
        "succeeded",
        at() + 100,
    )
    .unwrap();
    assert_eq!(
        enqueue_owner_research_event(
            &owner_db,
            &c,
            &task_id,
            2,
            Some(&"c".repeat(64)),
            "succeeded",
            at() + 100,
        )
        .unwrap_err()
        .to_string(),
        "planner_event_identity_changed"
    );
    let rows: i64 = owner_db
        .query_row(
            "SELECT count(*) FROM planner_event_log WHERE body LIKE '%research_task_terminal%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
    let event_body: String = owner_db
        .query_row(
            "SELECT body FROM planner_event_log WHERE body LIKE '%research_task_terminal%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let event: Value = serde_json::from_str(&event_body).unwrap();
    assert_eq!(event["research_task"]["task_id"], task_id);
    assert_eq!(event["research_task"]["revision"], 2);
    assert_eq!(event["research_task"]["result_sha256"], result_sha256);
    assert_eq!(event["observed_ms"], at() + 100);
    assert_eq!(event["kind"], "recheck");
    assert_eq!(event["reason"], "research_task_terminal");
    let event_at: i64 = owner_db
        .query_row(
            "SELECT at FROM planner_event_log WHERE id=?",
            [event["id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(event_at, at() + 100);
    assert_eq!(
        enqueue_owner_research_event(
            &owner_db,
            &c,
            &task_id,
            2,
            Some(&result_sha256),
            "cancelled",
            at() + 100,
        )
        .unwrap_err()
        .to_string(),
        "planner_owner_research_event_invalid"
    );
    assert_eq!(
        enqueue_owner_research_event(&owner_db, &c, &task_id, 3, None, "succeeded", at() + 100,)
            .unwrap_err()
            .to_string(),
        "planner_owner_research_event_invalid"
    );
    let pending = logged_pending(&owner_db).unwrap();
    let research_ref = pending
        .iter()
        .find(|logged| logged.id == event["id"].as_str().unwrap())
        .unwrap();
    insert_recheck(
        &owner_db,
        event["session_binding"].as_str().unwrap(),
        at() + 101,
        1,
        &[research_ref],
    )
    .unwrap();
    let collapsed_body: String = owner_db
        .query_row(
            "SELECT body FROM planner_event_log WHERE at=?",
            [at() + 101],
            |row| row.get(0),
        )
        .unwrap();
    let collapsed: Value = serde_json::from_str(&collapsed_body).unwrap();
    assert_eq!(collapsed["collapsed_research_tasks"][0]["task_id"], task_id);
    assert_eq!(collapsed["collapsed_research_tasks"][0]["revision"], 2);
    assert_eq!(
        collapsed["collapsed_research_tasks"][0]["source_event_id"],
        event["id"]
    );
    assert_eq!(
        collapsed["collapsed_research_tasks"][0]["source_observed_ms"],
        at() + 100
    );
}
fn at() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn cfg() -> planner::PlannerConfig {
    serde_json::from_value(json!({"enabled":true,"digest":{"enabled":true,"model_id":"fixture-model","daily_call_budget":4},"orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7"},"triggers":{"supervisor":{"prompt":"supervisor.md"}}},"events":{"enabled":true,"cooldown_ms":0,"max_pending":10,"policy":{"price_move_bps":100,"volume_step_units":null,"gap_bps":null,"proximity_bps":null,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}}})).unwrap()
}
fn instrument() -> Instrument {
    Instrument {
        key: "NSE_EQ|FIXTURE".into(),
        symbol: "FIXTURE".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }
}
fn sample(now: i64, price: &str) -> MarketObservation {
    let price = Price::parse(price).unwrap();
    MarketObservation {
        instrument: instrument(),
        quote: Quote {
            bid: price,
            ask: price,
            bid_units: 100,
            ask_units: 100,
            source: now,
            receipt: now,
        },
        cumulative_volume: Some(100),
        prior_close: None,
        session_open: None,
        exit_intent: None,
    }
}
fn plan(c: &Config, claim: &Claim) -> Value {
    let input = json!({"allowlist":[instrument()],"trigger":claim.trigger});
    json!({"attempt_id":claim.id,"decision_id":format!("{}-{}",c.session,claim.id),"session_binding":book_binding(c).unwrap(),"input_hash":digest(&serde_json::to_vec(&input).unwrap()),"input":input,"fake":false,"dry_run":false,"verdicts":[],"valid_until_ms":claim.deadline_ms})
}
fn owner_plan(c: &Config, cfg: &planner::PlannerConfig, claim: &Claim) -> Value {
    let mut plan = plan(c, claim);
    plan["policy_sha256"] = json!(digest(&planner::policy_bytes(cfg).unwrap()));
    plan["profile_sha256"] = Value::Null;
    plan["portfolio_revision"] = json!(claim.portfolio_hash);
    plan["digest_id"] = json!("d".repeat(64));
    plan["luna_handoff_requested"] = json!(false);
    plan["owner_digest"] = json!({"version":1,"digest":{"complete":true},"tool_results":[],"eligible":true,"facts_sha256":"f".repeat(64)});
    plan
}
fn admitted_deadline_survives_old_backlog() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let mut cfg = cfg();
    cfg.events.as_mut().unwrap().decision_timeout_ms = Some(300_000);
    initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
    let first = reserve(&mut db, &c, &cfg, &p, at(), true).unwrap().unwrap();
    settle(
        &mut db,
        &c,
        &cfg,
        &p,
        &first,
        Ok(owner_plan(&c, &cfg, &first)),
        at(),
        true,
    )
    .unwrap();
    for n in 0..3 {
        let id = format!("aged-{n}");
        let body = json!({"id":id,"kind":"price_move","instrument_key":format!("NSE_EQ|T{n}"),"observed_ms":at()+1000,"session_binding":book_binding(&c).unwrap()});
        db.execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
            rusqlite::params![id, at() + 1000, body.to_string()],
        )
        .unwrap();
    }
    let admitted = at() + 9000;
    let claim = reserve(&mut db, &c, &cfg, &p, admitted, true)
        .unwrap()
        .unwrap();
    assert_eq!(claim.deadline_ms, admitted + 300_000);
    assert_eq!(pending_count(&db).unwrap(), 0);
    settle(
        &mut db,
        &c,
        &cfg,
        &p,
        &claim,
        Ok(owner_plan(&c, &cfg, &claim)),
        admitted + 200_000,
        true,
    )
    .unwrap();
    assert!(
        reserve(&mut db, &c, &cfg, &p, admitted + 200_001, true)
            .unwrap()
            .is_none()
    );
    assert!(!p.halted);
}
#[test]
fn owner_cadence_waits_for_the_initial_event_and_every_digest_has_a_reason() {
    // The owner cadence defaults to 60 s, waits for the initial event, and
    // never outranks a material event that is already due.
    let mut owner_cfg = crate::runtime::redecision::tests::cfg();
    owner_cfg.discovery =
        Some(serde_json::from_value(json!({"max_scan_age_ms":180000,"max_prospects":10})).unwrap());
    let owner_dir = tempfile::tempdir().unwrap();
    let owner_c = crate::runtime::tests::config(owner_dir.path());
    let owner_p = crate::runtime::tests::test_portfolio();
    let mut owner_db = open_book(&owner_c).unwrap();
    initialize(&mut owner_db, &owner_c, &owner_p, Some(&owner_cfg), at()).unwrap();
    let initial = reserve(&mut owner_db, &owner_c, &owner_cfg, &owner_p, at(), true)
        .unwrap()
        .unwrap();
    assert_eq!(trigger_reason(&initial.trigger), "startup");
    settle(
        &mut owner_db,
        &owner_c,
        &owner_cfg,
        &owner_p,
        &initial,
        Ok(owner_plan(&owner_c, &owner_cfg, &initial)),
        at(),
        true,
    )
    .unwrap();
    // The orchestrator consumes the initial digest before cadence runs.
    owner_db
        .execute("DELETE FROM owner_digest_work", [])
        .unwrap();
    assert!(
        reserve(
            &mut owner_db,
            &owner_c,
            &owner_cfg,
            &owner_p,
            at() + 59_999,
            true
        )
        .unwrap()
        .is_none(),
        "no cadence digest inside the interval"
    );
    let cadence = reserve(
        &mut owner_db,
        &owner_c,
        &owner_cfg,
        &owner_p,
        at() + 60_000,
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(trigger_reason(&cadence.trigger), "digest_cadence");
    assert_eq!(cadence.trigger["kind"], "recheck");
    settle(
        &mut owner_db,
        &owner_c,
        &owner_cfg,
        &owner_p,
        &cadence,
        Ok(owner_plan(&owner_c, &owner_cfg, &cadence)),
        at() + 60_000,
        true,
    )
    .unwrap();
    owner_db
        .execute("DELETE FROM owner_digest_work", [])
        .unwrap();
    let control: String = owner_db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let control: Value = serde_json::from_str(&control).unwrap();
    assert_eq!(control["last_cadence_facts_sha256"], json!("f".repeat(64)));
    let repeated = reserve(
        &mut owner_db,
        &owner_c,
        &owner_cfg,
        &owner_p,
        at() + 120_000,
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(trigger_reason(&repeated.trigger), "digest_cadence");
    settle(
        &mut owner_db,
        &owner_c,
        &owner_cfg,
        &owner_p,
        &repeated,
        Ok(owner_plan(&owner_c, &owner_cfg, &repeated)),
        at() + 120_000,
        true,
    )
    .unwrap();
    let queued: i64 = owner_db
        .query_row(
            "SELECT count(*) FROM owner_digest_work WHERE state='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, 0);
    let (id, body) = disk_event("price_move", "NSE_EQ|FIXTURE", at() + 120_001, true);
    insert_pending(&owner_db, &id, at() + 120_001, &body);
    let material = reserve(
        &mut owner_db,
        &owner_c,
        &owner_cfg,
        &owner_p,
        at() + 120_001,
        true,
    )
    .unwrap()
    .unwrap();
    assert_eq!(trigger_reason(&material.trigger), "price_move");
}
#[test]
fn market_relative_scanner_trigger_ignores_a_broad_selloff() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let mut cfg = cfg();
    cfg.discovery = Some(
            serde_json::from_value(json!({"max_scan_age_ms":180000,"max_prospects":10,"trigger_move_bps":300.0,"trigger_market_relative":true})).unwrap(),
        );
    initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
    let selloff = ["A", "B", "C", "D", "E"]
        .map(|k| (format!("NSE_EQ|{k}"), Some(-450.0)))
        .to_vec();
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 1_000,
            &selloff,
            Some(-450.0),
            selloff.len()
        )
        .unwrap()
        .is_none()
    );
    let mut outlier = selloff.clone();
    outlier.push(("NSE_EQ|F".into(), Some(-900.0)));
    let reason = scan_refreshed(
        &mut db,
        &c,
        &cfg,
        at() + 2_000,
        &outlier,
        Some(-450.0),
        outlier.len(),
    )
    .unwrap()
    .unwrap();
    assert!(reason.contains("NSE_EQ|F") && reason.starts_with("scanner_prospect_move:1 "));
    // A one-sided top shortlist cannot redefine the broad flat market.
    let extremes = vec![("NSE_EQ|G".into(), Some(-1000.0))];
    let reason = scan_refreshed(&mut db, &c, &cfg, at() + 303000, &extremes, Some(0.0), 101)
        .unwrap()
        .unwrap();
    assert!(reason.contains("market_samples=101"));
    let body: String = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE at=?",
            [at() + 303000],
            |r| r.get(0),
        )
        .unwrap();
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["trigger"]["market_baseline_bps"], 0.0);
    assert_eq!(body["trigger"]["market_sample_count"], 101);
    let before: i64 = db
        .query_row("SELECT count(*) FROM planner_event_log", [], |r| r.get(0))
        .unwrap();
    let i = instrument();
    let quote = sample(at() + 304000, "5").quote;
    let rows = serde_json::Map::from_iter([(
        "renamed".into(),
        json!({"instrument_token":i.key,"symbol":"RENAMED","volume":1000000,"lower_circuit_limit":1,"upper_circuit_limit":200}),
    )]);
    observe_quotes(
        &mut db,
        &c,
        &cfg,
        &p,
        std::slice::from_ref(&i),
        &std::collections::BTreeMap::from([(i.key.clone(), quote)]),
        &rows,
        at() + 304000,
    )
    .unwrap();
    let after: i64 = db
        .query_row("SELECT count(*) FROM planner_event_log", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        before, after,
        "screened quotes cannot create ordinary Luna market events either"
    );
}
#[test]
fn scanner_refresh_triggers_only_on_deterministic_prospect_moves() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let mut cfg = cfg();
    cfg.discovery = Some(
            serde_json::from_value(json!({"max_scan_age_ms":180000,"max_prospects":10,"trigger_move_bps":150.0,"trigger_cooldown_ms":600000,"min_trigger_gap_ms":30000})).unwrap(),
        );
    initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
    let scanner_events = |db: &Connection| -> i64 {
        db.query_row(
            "SELECT count(*) FROM planner_event_log WHERE body LIKE '%scanner_prospect_move%'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let quiet = vec![
        ("NSE_EQ|A".to_owned(), Some(40.0)),
        ("NSE_EQ|B".to_owned(), None),
    ];
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 1_000,
            &quiet,
            Some(0.0),
            quiet.len()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        scanner_events(&db),
        0,
        "a refresh without a qualifying move is not a trigger"
    );
    let moved = vec![
        ("NSE_EQ|A".to_owned(), Some(-220.0)),
        ("NSE_EQ|B".to_owned(), Some(90.0)),
    ];
    let reason = scan_refreshed(
        &mut db,
        &c,
        &cfg,
        at() + 2_000,
        &moved,
        Some(0.0),
        moved.len(),
    )
    .unwrap()
    .unwrap();
    assert!(
        reason.contains("NSE_EQ|A") && reason.contains("150"),
        "{reason}"
    );
    assert_eq!(scanner_events(&db), 1);
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 62_000,
            &moved,
            Some(0.0),
            moved.len()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        scanner_events(&db),
        1,
        "same key inside cooldown does not retrigger"
    );
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 700_000,
            &moved,
            Some(0.0),
            moved.len()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        scanner_events(&db),
        1,
        "still past threshold is not a new crossing"
    );
    let other = vec![("NSE_EQ|C".to_owned(), Some(300.0))];
    let calm = vec![("NSE_EQ|A".to_owned(), Some(10.0))];
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 710_000,
            &calm,
            Some(0.0),
            calm.len()
        )
        .unwrap()
        .is_none()
    );
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 750_000,
            &moved,
            Some(0.0),
            moved.len()
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(scanner_events(&db), 2, "a fresh crossing re-arms the key");
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 760_000,
            &other,
            Some(0.0),
            other.len()
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        scanner_events(&db),
        2,
        "global gap suppresses a different key"
    );
    assert!(
        scan_refreshed(
            &mut db,
            &c,
            &cfg,
            at() + 790_000,
            &other,
            Some(0.0),
            other.len()
        )
        .unwrap()
        .is_some()
    );
}
#[test]
fn event_claims_supersede_once_and_preserve_crashes_failures_and_original_usage() {
    // Related fault scenarios share this behavioral contract; each owns its fixtures.
    observe_quotes_uses_dated_previous_close_without_a_saved_plan();
    observe_quotes_uses_latest_close_before_session_and_retries_pretrade_open();
    full_recheck_backlog_collapses_on_restart_and_records_prior_closes();
    recoverable_planner_failure_rechecks_after_cooldown();
    admitted_deadline_survives_old_backlog();

    // A market-open profile cannot consume its initial attempt before quotes
    // can be executable. No event/attempt reservation is written before admission.
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let mut open_cfg = cfg();
    assert!(
        serde_json::to_value(open_cfg.events.as_ref().unwrap())
            .unwrap()
            .get("decision_start_minute")
            .is_none()
    );
    let settings = open_cfg.events.as_mut().unwrap();
    settings.decision_start_minute = Some(479);
    assert!(settings.validate().is_err());
    settings.decision_start_minute = Some(930);
    assert!(settings.validate().is_err());
    settings.decision_start_minute = Some(555);
    settings.validate().unwrap();
    initialize(&mut db, &c, &p, Some(&open_cfg), at() - 75 * 60000).unwrap();
    assert!(
        reserve(&mut db, &c, &open_cfg, &p, at() - 1, true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(
        reserve(&mut db, &c, &open_cfg, &p, at(), false)
            .unwrap()
            .is_none()
    );
    assert!(
        reserve(&mut db, &c, &open_cfg, &p, at(), true)
            .unwrap()
            .is_some()
    );
    for outcome in [
        "success",
        "failure",
        "crash",
        "late",
        "stop",
        "changed",
        "changed_failure",
        "blocked",
    ] {
        let d = tempfile::tempdir().unwrap();
        let c = super::super::tests::config(d.path());
        let mut db = open_book(&c).unwrap();
        let mut p = crate::runtime::tests::test_portfolio();
        let cfg = cfg();
        initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
        assert!(
            reserve(&mut db, &c, &cfg, &p, at(), false)
                .unwrap()
                .is_none()
        );
        let initial = reserve(&mut db, &c, &cfg, &p, at(), true).unwrap().unwrap();
        assert!(
            reserve(&mut db, &c, &cfg, &p, at(), true)
                .unwrap()
                .is_none()
        );
        let original = owner_plan(&c, &cfg, &initial);
        settle(
            &mut db,
            &c,
            &cfg,
            &p,
            &initial,
            Ok(original.clone()),
            at(),
            true,
        )
        .unwrap();
        // An accepted digest defers to the owner; it never becomes a plan head.
        assert!(planner::saved_plan(&db, &c).unwrap().is_none());
        assert!(
            settle(
                &mut db,
                &c,
                &cfg,
                &p,
                &initial,
                Ok(original.clone()),
                at(),
                true
            )
            .is_err()
        );
        observe(
            &mut db,
            &c,
            &cfg,
            &p,
            &[sample(at() + 1000, "100")],
            &[],
            at() + 1000,
        )
        .unwrap();
        let observation = sample(at() + 2000, "102");
        observe(
            &mut db,
            &c,
            &cfg,
            &p,
            std::slice::from_ref(&observation),
            &[],
            at() + 2000,
        )
        .unwrap();
        observe(
            &mut db,
            &c,
            &cfg,
            &p,
            std::slice::from_ref(&observation),
            &[],
            at() + 2000,
        )
        .unwrap();
        assert_eq!(status(&db, &c).unwrap().unwrap()["pending_events"], 1);
        let next = reserve(&mut db, &c, &cfg, &p, at() + 2000, true)
            .unwrap()
            .unwrap();
        assert_ne!(next.id, initial.id);
        assert!(next.predecessor.is_none());
        assert_eq!(next.deadline_ms, at() + 12000);
        // A compatibility file cannot revive the superseded decision.
        fs::create_dir_all(c.directory.join("model")).unwrap();
        rollover::atomic(&c.directory.join("model/session-plan.json"), &original).unwrap();
        assert!(planner::saved_plan(&db, &c).unwrap().is_none());
        if outcome == "crash" {
            drop(db);
            db = open_book(&c).unwrap();
            initialize(&mut db, &c, &p, Some(&cfg), at() + 2001).unwrap();
            assert!(
                settle(
                    &mut db,
                    &c,
                    &cfg,
                    &p,
                    &next,
                    Ok(owner_plan(&c, &cfg, &next)),
                    at() + 2001,
                    true
                )
                .is_err()
            );
        } else {
            let mut now = at() + 2001;
            let result = if outcome == "failure" || outcome == "changed_failure" {
                Err("synthetic-provider-secret-must-not-be-stored".into())
            } else {
                Ok(owner_plan(&c, &cfg, &next))
            };
            if outcome == "late" {
                now = next.deadline_ms + 1;
            }
            if outcome == "stop" {
                p.stopped = true;
            }
            if outcome == "changed" || outcome == "changed_failure" {
                let observation = sample(at() + 2000, "100");
                assert!(
                    p.execute_plan(
                        at() + 2000,
                        &observation.instrument,
                        &observation.quote,
                        true,
                        1,
                        Price::parse("101").unwrap(),
                        sadr_core::execution::ExitIntent::default(),
                        "fixture-fill"
                    )
                    .unwrap()
                );
            }
            if outcome == "blocked" {
                block(&mut db, &c, "fixture_observation_failure").unwrap();
            }
            settle(&mut db, &c, &cfg, &p, &next, result, now, true).unwrap();
            assert!(planner::saved_plan(&db, &c).unwrap().is_none());
        }
        assert!(
            reserve(&mut db, &c, &cfg, &p, at() + 3000, true)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM planner_attempt_log", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM planner_plan WHERE id=?",
                [original["decision_id"].as_str().unwrap()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let result: Option<String> = db
            .query_row(
                "SELECT result FROM planner_attempt_log WHERE id=?",
                [&next.id],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!result.unwrap_or_default().contains("secret"));
        assert_eq!(planner::usage(&c).unwrap()["digest"], 0); // Admission initializes the namespace without charging a call.
        if outcome == "success" {
            db.execute_batch("DELETE FROM planner_event_control;")
                .unwrap();
            assert!(planner::saved_plan(&db, &c).is_err());
            db.execute_batch("DROP TABLE planner_event_control;")
                .unwrap();
            assert!(planner::saved_plan(&db, &c).is_err());
        }
    }
    // Pending evidence ages from its authentic observation, not reservation time.
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let mut cfg = cfg();
    cfg.events.as_mut().unwrap().max_pending = 1;
    initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
    let claim = reserve(&mut db, &c, &cfg, &p, at(), true).unwrap().unwrap();
    settle(
        &mut db,
        &c,
        &cfg,
        &p,
        &claim,
        Ok(owner_plan(&c, &cfg, &claim)),
        at(),
        true,
    )
    .unwrap();
    observe(
        &mut db,
        &c,
        &cfg,
        &p,
        &[sample(at() + 1000, "100")],
        &[],
        at() + 1000,
    )
    .unwrap();
    observe(
        &mut db,
        &c,
        &cfg,
        &p,
        &[sample(at() + 2000, "102")],
        &[],
        at() + 2000,
    )
    .unwrap();
    observe(
        &mut db,
        &c,
        &cfg,
        &p,
        &[sample(at() + 13000, "104")],
        &[],
        at() + 13000,
    )
    .unwrap();
    assert!(!blocked(&db, &c).unwrap());
    let claim = reserve(&mut db, &c, &cfg, &p, at() + 14000, true)
        .unwrap()
        .unwrap();
    assert_eq!(claim.deadline_ms, at() + 24000);
    assert_eq!(status(&db, &c).unwrap().unwrap()["pending_events"], 0);
}
fn observe_quotes_uses_dated_previous_close_without_a_saved_plan() {
    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(d.path());
    let history = d.path().join("history");
    fs::create_dir_all(&history).unwrap();
    c.history_directory = Some(history.clone());
    let i = instrument();
    let day = c.previous_session;
    let ts = format!("{day}T00:00:00+05:30");
    let body = serde_json::json!({"status":"success","data":{"candles":[[ts, 100.0, 101.0, 97.0, 98.0, 1000, 0]]}})
            .to_string();
    let id = super::super::history_download::range_id(&i.key, "days", day, day);
    let receipt = json!({
        "key": i.key,
        "unit": "days",
        "start": day,
        "end": day,
        "requested_ms": 1,
        "received_ms": at(),
        "body": body,
        "body_sha256": digest(body.as_bytes()),
    });
    fs::write(
        history.join(format!("{id}.receipt.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let cfg = cfg();
    initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
    let q = sample(at(), "100").quote;
    let quotes = std::collections::BTreeMap::from([(i.key.clone(), q)]);
    let mut rows = serde_json::Map::new();
    rows.insert(
        "row".into(),
        json!({
            "instrument_token": i.key,
            "symbol": i.symbol,
            "volume": 100,
            "ohlc": {"open": 99.5, "high": 101.0, "low": 99.0, "close": 100.5}
        }),
    );
    observe_quotes(&mut db, &c, &cfg, &p, &[i], &quotes, &rows, at()).unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 0);
    assert_eq!(view["coverage"]["market_observations"], 1);
    let saved: Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/market-references.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["as_of"], c.previous_session.to_string());
    assert_eq!(saved["closes"]["NSE_EQ|FIXTURE"], "98");
    assert_eq!(saved["close_basis"], "latest_daily_before_session");
    assert_eq!(
        saved["close_dates"]["NSE_EQ|FIXTURE"],
        c.previous_session.to_string()
    );
}
fn write_receipt(
    dir: &std::path::Path,
    key: &str,
    unit: &str,
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
    candles: Value,
    received_ms: i64,
) {
    let body = json!({"status":"success","data":{"candles":candles}}).to_string();
    let id = super::super::history_download::range_id(key, unit, start, end);
    let receipt = json!({
        "key": key,
        "unit": unit,
        "start": start,
        "end": end,
        "requested_ms": 1,
        "received_ms": received_ms,
        "body": body,
        "body_sha256": digest(body.as_bytes()),
    });
    fs::write(
        dir.join(format!("{id}.receipt.json")),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}
fn observe_quotes_uses_latest_close_before_session_and_retries_pretrade_open() {
    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(d.path());
    c.session = "2026-10-21".parse().unwrap();
    c.previous_session = "2026-10-19".parse().unwrap();
    let history = d.path().join("history");
    fs::create_dir_all(&history).unwrap();
    c.history_directory = Some(history.clone());
    let i = instrument();
    let now = DateTime::parse_from_rfc3339("2026-10-21T09:20:00+05:30")
        .unwrap()
        .timestamp_millis();
    let oct16 = "2026-10-16".parse().unwrap();
    let oct19 = "2026-10-19".parse().unwrap();
    let day = |date: &str, close: i64| {
        json!([
            format!("{date}T00:00:00+05:30"),
            close,
            close + 1,
            close - 1,
            close,
            1000,
            0
        ])
    };
    // Archive shape: a daily range that ends before the previous session, a
    // same-day session bar, and a minute bar that must not become a close.
    write_receipt(
        &history,
        &i.key,
        "days",
        "2025-10-17".parse().unwrap(),
        oct16,
        json!([day("2026-10-16", 90)]),
        now,
    );
    write_receipt(
        &history,
        &i.key,
        "days",
        c.session,
        c.session,
        json!([day("2026-10-21", 77)]),
        now,
    );
    write_receipt(
        &history,
        &i.key,
        "minutes",
        oct19,
        oct19,
        json!([["2026-10-19T09:15:00+05:30", 50, 51, 49, 50, 10, 0]]),
        now,
    );
    fs::create_dir_all(c.directory.join("model")).unwrap();
    fs::write(
        c.directory.join("model/market-references.json"),
        br#"{"session":"2026-10-21","as_of":"2026-10-19","closes":{}}"#,
    )
    .unwrap();
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let mut planner = cfg();
    planner.events.as_mut().unwrap().policy.gap_bps = Some(100);
    initialize(&mut db, &c, &p, Some(&planner), now).unwrap();
    let mut quote = sample(now, "100").quote;
    quote.ask = Price::parse("100.05").unwrap();
    let mut quotes = std::collections::BTreeMap::from([(i.key.clone(), quote.clone())]);
    let mut rows = serde_json::Map::new();
    rows.insert(
        "row".into(),
        json!({
            "instrument_token": i.key,
            "symbol": i.symbol,
            "volume": 0,
            "ohlc": {"open": 91, "high": 91, "low": 91, "close": 100.5}
        }),
    );
    observe_quotes(
        &mut db,
        &c,
        &planner,
        &p,
        std::slice::from_ref(&i),
        &quotes,
        &rows,
        now,
    )
    .unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 0);
    assert_eq!(view["coverage"]["market_observations"], 1);
    assert_eq!(view["events"].as_array().unwrap().len(), 0);
    let saved: Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/market-references.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["close_basis"], "latest_daily_before_session");
    assert_eq!(saved["closes"][&i.key], "90");
    assert_eq!(saved["close_dates"][&i.key], "2026-10-16");
    assert_eq!(saved["as_of"], "2026-10-19");
    // A later 19 Oct bar must not replace a verified cache on the next quote.
    write_receipt(
        &history,
        &i.key,
        "days",
        oct19,
        oct19,
        json!([day("2026-10-19", 91)]),
        now,
    );
    quote.bid = Price::parse("110").unwrap();
    quote.ask = Price::parse("110.05").unwrap();
    quote.source = now + 1000;
    quote.receipt = now + 1000;
    quotes.insert(i.key.clone(), quote.clone());
    rows["row"]["ohlc"]["open"] = json!(92);
    observe_quotes(
        &mut db,
        &c,
        &planner,
        &p,
        std::slice::from_ref(&i),
        &quotes,
        &rows,
        now + 1000,
    )
    .unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["coverage"]["market_observations"], 2);
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 0);
    assert_eq!(view["events"][0]["kind"], "price_move");
    assert_eq!(view["events"].as_array().unwrap().len(), 1);
    let saved: Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/market-references.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["close_dates"][&i.key], "2026-10-16");
    block(&mut db, &c, "planner_event_observation_invalid").unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "planner_event_observation_invalid"
    );
    rows["row"]["volume"] = json!(10);
    rows["row"]["ohlc"]["open"] = json!(99);
    quote.source = now + 2000;
    quote.receipt = now + 2000;
    quotes.insert(i.key.clone(), quote.clone());
    observe_quotes(
        &mut db,
        &c,
        &planner,
        &p,
        std::slice::from_ref(&i),
        &quotes,
        &rows,
        now + 2000,
    )
    .unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(view["coverage"]["market_observations"], 3);
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 0);
    assert!(
        view["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "gap")
    );
    rows["row"]["volume"] = json!(11);
    rows["row"]["ohlc"]["open"] = json!(102);
    quote.source = now + 3000;
    quote.receipt = now + 3000;
    quotes.insert(i.key.clone(), quote);
    observe_quotes(
        &mut db,
        &c,
        &planner,
        &p,
        std::slice::from_ref(&i),
        &quotes,
        &rows,
        now + 3000,
    )
    .unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "planner_event_observation_invalid");
    assert_eq!(view["coverage"]["market_observations"], 3);
    assert_eq!(view["pending_events"], 2);
    observe(&mut db, &c, &planner, &p, &[], &[], now + 4000).unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(view["coverage"]["market_observations"], 3);
    assert_eq!(view["pending_events"], 2);

    let later = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(later.path());
    c.session = "2026-10-21".parse().unwrap();
    c.previous_session = "2026-10-19".parse().unwrap();
    let history = later.path().join("history");
    fs::create_dir_all(&history).unwrap();
    c.history_directory = Some(history.clone());
    write_receipt(
        &history,
        &i.key,
        "days",
        oct16,
        oct16,
        json!([day("2026-10-16", 90)]),
        now,
    );
    write_receipt(
        &history,
        &i.key,
        "days",
        oct19,
        oct19,
        json!([day("2026-10-19", 91)]),
        now,
    );
    let mut db = open_book(&c).unwrap();
    let mut planner = cfg();
    planner.events.as_mut().unwrap().policy.gap_bps = Some(100);
    initialize(&mut db, &c, &p, Some(&planner), now).unwrap();
    let quote = sample(now, "100").quote;
    let quotes = std::collections::BTreeMap::from([(i.key.clone(), quote)]);
    let mut rows = serde_json::Map::new();
    rows.insert(
        "row".into(),
        json!({
            "instrument_token": i.key,
            "symbol": i.symbol,
            "volume": 0,
            "ohlc": {"open": 91, "high": 91, "low": 91, "close": 100.5}
        }),
    );
    observe_quotes(&mut db, &c, &planner, &p, &[i], &quotes, &rows, now).unwrap();
    let saved: Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/market-references.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["closes"]["NSE_EQ|FIXTURE"], "91");
    assert_eq!(saved["close_dates"]["NSE_EQ|FIXTURE"], "2026-10-19");
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["coverage"]["missing_prior_close_observations"],
        0
    );

    let stuck = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(stuck.path());
    let mut db = open_book(&c).unwrap();
    let cfg = cfg();
    initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
    block(&mut db, &c, "operator_stopped_no_replay").unwrap();
    observe(&mut db, &c, &cfg, &p, &[sample(at(), "100")], &[], at()).unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "operator_stopped_no_replay");
    assert_eq!(view["coverage"]["market_observations"], 0);
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 0);
}
fn disk_event(kind: &str, key: &str, at: i64, anchor: bool) -> (String, String) {
    // On-disk planner_event_log shape from the 2026-10-21 paper book:
    // string prices, integer volume, null exit, anchor only on price moves.
    let id = digest(format!("disk:{kind}:{key}:{at}").as_bytes());
    let observation = json!({
        "instrument": {"key": key, "symbol": "FIXTURE", "series": "EQ", "tick": "0.05"},
        "quote": {
            "bid": "514.1",
            "ask": "518.7",
            "bid_units": 1,
            "ask_units": 1,
            "source": at.saturating_sub(10),
            "receipt": at
        },
        "cumulative_volume": 2104,
        "prior_close": "536.45",
        "session_open": "530.7",
        "exit_intent": null
    });
    let evidence = if anchor {
        json!({"anchor": "526.25", "observation": observation})
    } else {
        json!({"observation": observation})
    };
    let body = json!({
        "id": id,
        "kind": kind,
        "instrument_key": key,
        "observed_ms": at,
        "evidence": evidence
    })
    .to_string();
    (id, body)
}
fn insert_pending(db: &Connection, id: &str, at: i64, body: &str) {
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params![id, at, body],
    )
    .unwrap();
}
fn event_state(db: &Connection, id: &str) -> String {
    db.query_row(
        "SELECT state FROM planner_event_log WHERE id=?",
        [id],
        |row| row.get(0),
    )
    .unwrap()
}
fn full_recheck_backlog_collapses_on_restart_and_records_prior_closes() {
    let base = at();
    let duplicates = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(duplicates.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let duplicate_cfg = cfg();
    initialize(&mut db, &c, &p, Some(&duplicate_cfg), base).unwrap();
    let mut price_ids = Vec::new();
    for offset in [0_i64, 1, 2] {
        let (id, body) = disk_event("price_move", "NSE_EQ|FIXTURE", base + offset, true);
        insert_pending(&db, &id, base + offset, &body);
        price_ids.push(id);
    }
    let (gap_id, gap_body) = disk_event("gap", "NSE_EQ|FIXTURE", base, false);
    insert_pending(&db, &gap_id, base, &gap_body);
    let (older_volume, older_volume_body) = disk_event("volume_move", "NSE_EQ|FIXTURE", base, true);
    let (newer_volume, newer_volume_body) =
        disk_event("volume_move", "NSE_EQ|FIXTURE", base + 5, true);
    insert_pending(&db, &older_volume, base, &older_volume_body);
    insert_pending(&db, &newer_volume, base + 5, &newer_volume_body);
    initialize(&mut db, &c, &p, Some(&duplicate_cfg), base + 10).unwrap();
    assert!(!blocked(&db, &c).unwrap());
    assert_eq!(event_state(&db, &price_ids[0]), "collapsed");
    assert_eq!(event_state(&db, &price_ids[1]), "collapsed");
    assert_eq!(event_state(&db, &price_ids[2]), "pending");
    assert_eq!(event_state(&db, &older_volume), "collapsed");
    assert_eq!(event_state(&db, &newer_volume), "pending");
    assert_eq!(event_state(&db, &gap_id), "pending");
    let kept: String = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE id=?",
            [&price_ids[2]],
            |row| row.get(0),
        )
        .unwrap();
    assert!(kept.contains("\"anchor\":\"526.25\""));
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM planner_event_log WHERE state='collapsed'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        3
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM planner_event_log WHERE body LIKE '%\"kind\":\"recheck\"%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );

    let burst = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(burst.path());
    let mut db = open_book(&c).unwrap();
    let mut burst_cfg = cfg();
    burst_cfg.events.as_mut().unwrap().max_pending = 3;
    initialize(&mut db, &c, &p, Some(&burst_cfg), base).unwrap();
    let mut names = Vec::new();
    for index in 0..5 {
        let mut name = instrument();
        name.key = format!("NSE_EQ|BATCH{index}");
        names.push(name);
    }
    let batch = |when: i64, price: &str| {
        names
            .iter()
            .map(|name| {
                let mut observation = sample(when, price);
                observation.instrument = name.clone();
                observation
            })
            .collect::<Vec<_>>()
    };
    observe(&mut db, &c, &burst_cfg, &p, &batch(base, "100"), &[], base).unwrap();
    observe(
        &mut db,
        &c,
        &burst_cfg,
        &p,
        &batch(base + 1000, "110"),
        &[],
        base + 1000,
    )
    .unwrap();
    assert!(!blocked(&db, &c).unwrap());
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(view["pending_events"], 1);
    assert_eq!(view["events"][0]["kind"], "recheck");
    assert_eq!(
            db.query_row(
                "SELECT count(*) FROM planner_event_log WHERE state='collapsed' AND body LIKE '%\"kind\":\"price_move\"%'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            5
        );

    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(d.path());
    let history = d.path().join("history");
    fs::create_dir_all(&history).unwrap();
    c.history_directory = Some(history.clone());
    let quote_name = instrument();
    let restart = base + 60_000;
    let backlog_at = restart - 440_000;
    write_receipt(
        &history,
        &quote_name.key,
        "days",
        c.previous_session,
        c.previous_session,
        json!([[
            format!("{}T00:00:00+05:30", c.previous_session),
            100,
            101,
            97,
            98,
            1000,
            0
        ]]),
        restart,
    );
    let mut db = open_book(&c).unwrap();
    let mut planner = cfg();
    {
        let events = planner.events.as_mut().unwrap();
        events.max_pending = 40;
        events.cooldown_ms = 300_000;
        events.policy.max_event_age_ms = 300_000;
        events.policy.price_move_bps = Some(100);
    }
    assert_eq!(planner.digest.daily_call_budget, 4);
    initialize(&mut db, &c, &p, Some(&planner), restart).unwrap();
    let usage_path = planner::fixture_reservation(&c, &planner);
    let usage_bytes = fs::read(&usage_path).unwrap();
    let raw: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let mut control: Value = serde_json::from_str(&raw).unwrap();
    control["blocked"] = json!("planner_event_queue_full");
    control["initial_available"] = json!(false);
    control["market_observations"] = json!(2875);
    control["missing_prior_close"] = json!(2875);
    control["last_started_ms"] = json!(restart - 300_000);
    db.execute(
        "UPDATE planner_event_control SET body=? WHERE id=1",
        [control.to_string()],
    )
    .unwrap();
    let mut price_sample = String::new();
    let mut price_sample_id = String::new();
    let mut gap_sample = String::new();
    let mut gap_sample_id = String::new();
    for index in 0..121 {
        let key = format!("NSE_EQ|T{index:04}");
        let (id, body) = disk_event("price_move", &key, backlog_at, true);
        if index == 0 {
            price_sample = body.clone();
            price_sample_id = id.clone();
        }
        insert_pending(&db, &id, backlog_at, &body);
    }
    for index in 0..56 {
        let key = format!("NSE_EQ|T{index:04}");
        let (id, body) = disk_event("gap", &key, backlog_at, false);
        if index == 0 {
            gap_sample = body.clone();
            gap_sample_id = id.clone();
        }
        insert_pending(&db, &id, backlog_at, &body);
    }
    let fill_id = digest(b"fixture-fill-kept");
    let fill_at = restart + 60_000;
    let fill_body = json!({
        "id": fill_id,
        "kind": "fill",
        "instrument_key": "NSE_EQ|FIXTURE",
        "observed_ms": fill_at,
        "evidence": {"fill_id": "fixture-fill"}
    })
    .to_string();
    insert_pending(&db, &fill_id, fill_at, &fill_body);
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM planner_event_log WHERE state='pending'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        178
    );
    initialize(&mut db, &c, &p, Some(&planner), restart).unwrap();
    assert!(!blocked(&db, &c).unwrap());
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(view["pending_events"], 2);
    assert_eq!(view["coverage"]["market_observations"], 2875);
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 2875);
    assert_eq!(event_state(&db, &price_sample_id), "collapsed");
    assert_eq!(event_state(&db, &gap_sample_id), "collapsed");
    assert_eq!(event_state(&db, &fill_id), "pending");
    let stored_price: String = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE id=?",
            [&price_sample_id],
            |row| row.get(0),
        )
        .unwrap();
    let stored_gap: String = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE id=?",
            [&gap_sample_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_price, price_sample);
    assert_eq!(stored_gap, gap_sample);
    assert!(!stored_gap.contains("\"anchor\""));
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM planner_event_log WHERE state='collapsed'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        177
    );
    let recheck: String = db
            .query_row(
                "SELECT body FROM planner_event_log WHERE state='pending' AND body LIKE '%\"kind\":\"recheck\"%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
    let recheck: Value = serde_json::from_str(&recheck).unwrap();
    assert_eq!(recheck["observed_ms"], restart);
    assert_eq!(recheck["reason"], "planner_event_backlog_collapsed");
    assert_eq!(recheck["collapsed"], 177);
    assert_eq!(recheck["session_binding"], book_binding(&c).unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_event_log", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        179
    );
    let mut quote = sample(restart, "100").quote;
    let mut quotes = std::collections::BTreeMap::from([(quote_name.key.clone(), quote.clone())]);
    let mut rows = serde_json::Map::new();
    rows.insert(
        "row".into(),
        json!({
            "instrument_token": quote_name.key,
            "symbol": quote_name.symbol,
            "volume": 100,
            "ohlc": {"open": 99.5, "high": 101.0, "low": 99.0, "close": 100.5}
        }),
    );
    observe_quotes(
        &mut db,
        &c,
        &planner,
        &p,
        std::slice::from_ref(&quote_name),
        &quotes,
        &rows,
        restart,
    )
    .unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["coverage"]["market_observations"], 2876);
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 2875);
    assert!(!blocked(&db, &c).unwrap());
    let saved: Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/market-references.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["close_basis"], "latest_daily_before_session");
    assert_eq!(saved["closes"][&quote_name.key], "98");
    let claim = reserve(&mut db, &c, &planner, &p, restart, true)
        .unwrap()
        .unwrap();
    assert_eq!(claim.trigger["kind"], "recheck");
    assert_eq!(claim.trigger["observed_ms"], restart);
    assert_eq!(claim.deadline_ms, restart + 300_000);
    assert!(p.fills.is_empty());
    settle(
        &mut db,
        &c,
        &planner,
        &p,
        &claim,
        Ok(owner_plan(&c, &planner, &claim)),
        restart,
        true,
    )
    .unwrap();
    // An accepted digest defers to the persistent owner; it never becomes an
    // executable plan head and never fills.
    assert!(p.fills.is_empty());
    assert!(p.positions.is_empty());
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(fs::read(&usage_path).unwrap(), usage_bytes);
    quote.bid = Price::parse("110").unwrap();
    quote.ask = Price::parse("110").unwrap();
    quote.source = restart + 1000;
    quote.receipt = restart + 1000;
    quotes.insert(quote_name.key.clone(), quote);
    rows["row"]["volume"] = json!(110);
    observe_quotes(
        &mut db,
        &c,
        &planner,
        &p,
        std::slice::from_ref(&quote_name),
        &quotes,
        &rows,
        restart + 1000,
    )
    .unwrap();
    assert!(
        reserve(&mut db, &c, &planner, &p, restart + 1000, true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["coverage"]["market_observations"], 2877);
    assert_eq!(view["coverage"]["missing_prior_close_observations"], 2875);
    assert!(
        view["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "price_move" && event["status"] == "pending")
    );
    let seen = view["coverage"]["market_observations"].as_u64().unwrap();
    block(&mut db, &c, "operator_stopped_no_replay").unwrap();
    observe(&mut db, &c, &planner, &p, &[], &[], restart + 2000).unwrap();
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "operator_stopped_no_replay");
    assert_eq!(view["coverage"]["market_observations"], seen);
    assert_eq!(planner.digest.daily_call_budget, 4);
}
fn recoverable_planner_failure_rechecks_after_cooldown() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    {
        let mut cfg = cfg();
        {
            let events = cfg.events.as_mut().unwrap();
            events.cooldown_ms = 300_000;
            events.policy.max_event_age_ms = 400_000;
        }
        initialize(&mut db, &c, &p, Some(&cfg), at()).unwrap();
        let claim = reserve(&mut db, &c, &cfg, &p, at(), true).unwrap().unwrap();
        settle(
            &mut db,
            &c,
            &cfg,
            &p,
            &claim,
            Err("synthetic-provider-secret-must-not-be-stored".into()),
            at(),
            true,
        )
        .unwrap();
        // A failed digest is one bad attempt, not a decision block (#221).
        let view = status(&db, &c).unwrap().unwrap();
        assert_eq!(view["status"], "waiting_for_eligible_event");
        assert_eq!(view["last_digest_failure"], "planner_failed_no_replay");
        let stored: String = db
            .query_row(
                "SELECT result FROM planner_attempt_log WHERE id=?",
                [&claim.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(stored.contains("planner_failed_no_replay"));
        assert!(!stored.contains("secret"));
        let (id, body) = disk_event("price_move", "NSE_EQ|FIXTURE", at() + 1_000, true);
        insert_pending(&db, &id, at() + 1_000, &body);
        let usage_path = planner::fixture_reservation(&c, &cfg);
        let usage_bytes = fs::read(&usage_path).unwrap();
        assert!(
            reserve(&mut db, &c, &cfg, &p, at() + 1_000, true)
                .unwrap()
                .is_none()
        );
        assert!(!blocked(&db, &c).unwrap());
        observe(&mut db, &c, &cfg, &p, &[], &[], at() + 1_500).unwrap();
        let view = status(&db, &c).unwrap().unwrap();
        assert_eq!(view["status"], "waiting_for_eligible_event");
        assert!(view["pending_events"].as_i64().unwrap() >= 1);
        assert!(
            reserve(&mut db, &c, &cfg, &p, at() + 1_500, true)
                .unwrap()
                .is_none()
        );
        assert!(!blocked(&db, &c).unwrap());
        assert_eq!(
            db.query_row("SELECT count(*) FROM planner_attempt_log", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            1
        );
        let next = reserve(&mut db, &c, &cfg, &p, at() + 300_000, true)
            .unwrap()
            .expect("recheck after cooldown");
        assert_ne!(next.id, claim.id);
        assert_eq!(next.trigger["kind"], "price_move");
        assert_eq!(fs::read(&usage_path).unwrap(), usage_bytes);
        assert_eq!(cfg.digest.daily_call_budget, 4);
    }

    let held = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(held.path());
    let mut db = open_book(&c).unwrap();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    let claim = reserve(&mut db, &c, &planner, &p, at(), true)
        .unwrap()
        .unwrap();
    block(&mut db, &c, "planner_event_observation_invalid").unwrap();
    settle(
        &mut db,
        &c,
        &planner,
        &p,
        &claim,
        Ok(owner_plan(&c, &planner, &claim)),
        claim.started_ms,
        true,
    )
    .unwrap();
    assert!(!blocked(&db, &c).unwrap());
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "waiting_for_eligible_event"
    );

    let late = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(late.path());
    let mut db = open_book(&c).unwrap();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    let claim = reserve(&mut db, &c, &planner, &p, at(), true)
        .unwrap()
        .unwrap();
    let mut changed = planner.clone();
    changed.digest.daily_call_budget = 9;
    assert_eq!(
        config_hash(&planner).unwrap(),
        config_hash(&changed).unwrap()
    );
    // A control hash that no longer matches its owner marker refuses and is
    // left exactly as stored; no archived policy upgrades it.
    let migration = tempfile::tempdir().unwrap();
    let migration_c = super::super::tests::config(migration.path());
    let mut migration_db = open_book(&migration_c).unwrap();
    initialize(&mut migration_db, &migration_c, &p, Some(&planner), at()).unwrap();
    let mut legacy = load(&migration_db).unwrap().unwrap();
    legacy.config_hash = digest(&serde_json::to_vec(&planner).unwrap());
    save(&migration_db, &legacy).unwrap();
    assert_eq!(
        initialize(&mut migration_db, &migration_c, &p, Some(&changed), at())
            .unwrap_err()
            .to_string(),
        "planner_authority_owner_mismatch"
    );
    assert_eq!(
        serde_json::to_value(load(&migration_db).unwrap().unwrap()).unwrap(),
        serde_json::to_value(&legacy).unwrap()
    );

    changed.digest.output_cap += 1;
    settle(
        &mut db,
        &c,
        &changed,
        &p,
        &claim,
        Ok(plan(&c, &claim)),
        claim.deadline_ms + 1,
        true,
    )
    .unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "planner_event_configuration_changed"
    );
    observe(&mut db, &c, &planner, &p, &[], &[], at() + 1).unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "planner_event_configuration_changed"
    );

    let expired = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(expired.path());
    let mut db = open_book(&c).unwrap();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    let claim = reserve(&mut db, &c, &planner, &p, at(), true)
        .unwrap()
        .unwrap();
    settle(
        &mut db,
        &c,
        &planner,
        &p,
        &claim,
        Ok(plan(&c, &claim)),
        claim.deadline_ms + 1,
        true,
    )
    .unwrap();
    // A recoverable digest failure is recorded, never a decision block (#221).
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(
        view["last_digest_failure"],
        "planner_completion_expired_or_changed"
    );
    observe(&mut db, &c, &planner, &p, &[], &[], at() + 1).unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "waiting_for_eligible_event"
    );

    let invalid = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(invalid.path());
    let mut db = open_book(&c).unwrap();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    let claim = reserve(&mut db, &c, &planner, &p, at(), true)
        .unwrap()
        .unwrap();
    settle(
        &mut db,
        &c,
        &planner,
        &p,
        &claim,
        Err("planner_invalid_plan_no_entries".into()),
        at(),
        true,
    )
    .unwrap();
    // A recoverable digest failure is recorded, never a decision block (#221).
    let view = status(&db, &c).unwrap().unwrap();
    assert_eq!(view["status"], "waiting_for_eligible_event");
    assert_eq!(
        view["last_digest_failure"],
        "planner_invalid_plan_no_entries"
    );
    observe(&mut db, &c, &planner, &p, &[], &[], at() + 1).unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "waiting_for_eligible_event"
    );

    let stopped = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(stopped.path());
    let mut db = open_book(&c).unwrap();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    block(&mut db, &c, "operator_stopped_no_replay").unwrap();
    observe(&mut db, &c, &planner, &p, &[sample(at(), "100")], &[], at()).unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "operator_stopped_no_replay"
    );
    assert!(
        reserve(&mut db, &c, &planner, &p, at(), true)
            .unwrap()
            .is_none()
    );
    apply_config(&db, &c, &planner).unwrap();
    assert_ne!(
        status(&db, &c).unwrap().unwrap()["status"],
        "operator_stopped_no_replay"
    );

    let p = crate::runtime::tests::test_portfolio();
    let owner_dir = tempfile::tempdir().unwrap();
    let (owner_c, owner) = super::super::decision_owner::owner_fixture_config(owner_dir.path());
    let mut owner_db = open_book(&owner_c).unwrap();
    initialize(&mut owner_db, &owner_c, &p, Some(&owner), at()).unwrap();
    assert!(settings(&owner).is_some());
    recover(&owner_db, &owner_c, &owner, RecoveryClearance::Reviewed).unwrap();
    assert!(config_current(&owner_db, &owner).unwrap());

    // Startup preserves an integrity block that is not a known recoverable
    // failure instead of replacing it with a weaker pending/config signal.
    for reason in [
        "planner_event_fill_history_changed",
        "planner_usage_namespace_invalid",
    ] {
        let sticky = tempfile::tempdir().unwrap();
        let c = super::super::tests::config(sticky.path());
        let mut db = open_book(&c).unwrap();
        let planner = cfg();
        initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
        reserve(&mut db, &c, &planner, &p, at(), true)
            .unwrap()
            .expect("pending claim");
        block(&mut db, &c, reason).unwrap();
        let mut changed = planner.clone();
        changed.events.as_mut().unwrap().cooldown_ms += 1;
        initialize(&mut db, &c, &p, Some(&changed), at() + 1).unwrap();
        assert_eq!(
            status(&db, &c).unwrap().unwrap()["status"],
            reason,
            "startup must preserve {reason}"
        );
    }
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
fn transient_read_block_clears_only_after_settled_attempt() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let p = crate::runtime::tests::test_portfolio();
    let planner = cfg();
    initialize(&mut db, &c, &p, Some(&planner), at()).unwrap();
    let claim = reserve(&mut db, &c, &planner, &p, at(), true)
        .unwrap()
        .unwrap();
    block(&mut db, &c, READ_TRANSIENT_BLOCK).unwrap();
    assert!(!clear_transient_read_block(&mut db, &c).unwrap());
    settle(
        &mut db,
        &c,
        &planner,
        &p,
        &claim,
        Err("http_timeout".into()),
        at(),
        true,
    )
    .unwrap();
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        READ_TRANSIENT_BLOCK
    );
    assert!(clear_transient_read_block(&mut db, &c).unwrap());
    assert!(!blocked(&db, &c).unwrap());
    assert_eq!(
        status(&db, &c).unwrap().unwrap()["status"],
        "waiting_for_eligible_event"
    );
    assert!(!clear_transient_read_block(&mut db, &c).unwrap());
    assert_eq!(
        db.query_row("SELECT count(*) FROM planner_attempt_log", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        1
    );
    let stored: String = db
        .query_row(
            "SELECT result FROM planner_attempt_log WHERE id=?",
            [&claim.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(stored.contains(READ_TRANSIENT_BLOCK));
    let cleared: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='planner_read_block_cleared'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cleared, 1);
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
#[test]
fn strategy_trigger_uses_champion_cooldown_and_attribution() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut db = open_book(&c).unwrap();
    let mut cfg = cfg();
    cfg.discovery=Some(serde_json::from_value(json!({"max_scan_age_ms":2000,"max_prospects":10,"min_trigger_gap_ms":0,"trigger_market_relative":true,"trigger_move_bps":900,"trigger_cooldown_ms":0,"strategy":{"enabled":true,"champion":"m","specs":[{"id":"m","version":7,"filters":[],"weights":[{"feature":"move_bps","weight":1}],"shortlist_size":1,"trigger_score":10,"cooldown_ms":10000}]}})).unwrap());
    initialize(
        &mut db,
        &c,
        &crate::runtime::tests::test_portfolio(),
        Some(&cfg),
        at(),
    )
    .unwrap();
    let selected = vec![("NSE_EQ|A".to_string(), Some(1.0))];
    let unselected = vec![("NSE_EQ|A".to_string(), Some(0.0))];
    assert!(
        scan_refreshed(&mut db, &c, &cfg, at() + 1000, &selected, None, 1)
            .unwrap()
            .is_some()
    );
    let raw: String = db
        .query_row(
            "SELECT body FROM planner_event_log WHERE body LIKE '%scanner_prospect_move%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let body: Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(body["trigger"]["strategy_spec_id"], "m");
    assert_eq!(body["trigger"]["strategy_spec_version"], 7);
    assert_eq!(body["trigger"]["cooldown_ms"], 10000);
    scan_refreshed(&mut db, &c, &cfg, at() + 2000, &unselected, None, 1).unwrap();
    assert!(
        scan_refreshed(&mut db, &c, &cfg, at() + 3000, &selected, None, 1)
            .unwrap()
            .is_none()
    );
    assert!(
        scan_refreshed(&mut db, &c, &cfg, at() + 12000, &selected, None, 1)
            .unwrap()
            .is_some()
    );
}
