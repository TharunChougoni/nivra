use super::*;

#[cfg(test)]
fn exercise_plan(
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    claim: &redecision::Claim,
    instrument: &Instrument,
) -> Value {
    let input = json!({
        "allowlist":[instrument],
        "cash":p.cash,
        "fees":p.costs,
        "positions":p.positions,
    });
    json!({
        "decision_id":format!("{}-{}", c.session, claim.id),
        "attempt_id":claim.id,
        "session_binding":book_binding(c).unwrap(),
        "input_hash":digest(&serde_json::to_vec(&input).unwrap()),
        "input":input,
        "portfolio_revision":claim.portfolio_hash,
        "selection_provenance":{"source_hash":"a".repeat(64),"selection_reason":"live_ranking_consumed","live_ranking":true,"attribution_only":true,"activation_authority":false},
        "policy_sha256":digest(&planner::policy_bytes(cfg).unwrap()),
        "profile_sha256":cfg.profile_sha256().map(str::to_owned),
        "digest_id":"d".repeat(64),
        "owner_digest":{"version":1,"digest":{"complete":true,"as_of_ms":0,"coverage":{"universe":0,"scanned":0,"quoted":0,"featured":0,"experiments":0},"cash":{"available":"0"},"fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"}},"tool_results":[],"eligible":true,"facts_sha256":"0".repeat(64)},
        "luna_handoff_requested":false,
        "fake":false,
        "dry_run":false,
        "verdicts":[],
        "valid_until_ms":claim.deadline_ms
    })
}

/// One complete queued digest -> bound persistent turn -> committed paper plan.
#[cfg(test)]
#[allow(clippy::too_many_arguments)] // Fixture identity and each financial input are explicit.
fn exercise_round(
    db: &mut Connection,
    c: &Config,
    cfg: &planner::PlannerConfig,
    p: &Portfolio,
    instrument: &Instrument,
    raw: &str,
    event_id: &str,
    at: i64,
) -> Value {
    let event = json!({"id":event_id,"kind":"price_move",
        "instrument_key":instrument.key,"observed_ms":at,"evidence":{}});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params![event_id, at, event.to_string()],
    )
    .unwrap();
    let claim = redecision::reserve(db, c, cfg, p, at, true)
        .unwrap()
        .unwrap();
    let plan_started = std::time::Instant::now();
    let digest_plan = exercise_plan(c, cfg, p, &claim, instrument);
    let fixture_digest_build_us = plan_started.elapsed().as_micros();
    let enqueue_started = std::time::Instant::now();
    redecision::settle(db, c, cfg, p, &claim, Ok(digest_plan), at, true).unwrap();
    let enqueue_us = enqueue_started.elapsed().as_micros();
    let turn_started = std::time::Instant::now();
    let mut context = Context {
        db: &mut *db,
        portfolio: p,
        instruments: std::slice::from_ref(instrument),
    };
    let mut work = queued(&mut context, c, cfg, at).unwrap().unwrap();
    assert_eq!(
        work.facts["selection_provenance"]["source_hash"],
        "a".repeat(64)
    );
    assert_eq!(
        work.facts["selection_provenance"]["activation_authority"],
        false
    );
    work.claim.turn_id = format!("turn-{event_id}");
    work.claim.model_id = "fixture-supervisor".into();
    work.claim.started_ms = at;
    bind(&mut context, c, cfg, &mut work.claim, at).unwrap();
    let queue_to_bound_turn_us = turn_started.elapsed().as_micros();
    let commit_started = std::time::Instant::now();
    let receipt = commit(&mut context, c, cfg, &work.claim, raw, at + 1).unwrap();
    eprintln!(
        "fixture_owner_timing {}",
        json!({"provenance":"synthetic_fixture",
        "fixture_digest_build_us":fixture_digest_build_us,"settle_enqueue_us":enqueue_us,
        "queue_to_bound_turn_us":queue_to_bound_turn_us,
        "owner_commit_us":commit_started.elapsed().as_micros(),
        "scripted_owner_elapsed_ms":1,"provider_requests":0})
    );
    receipt
}

#[cfg(test)]
pub(crate) fn owner_fixture_config(dir: &std::path::Path) -> (Config, planner::PlannerConfig) {
    let mut c = super::super::tests::config(dir);
    let mut cfg: planner::PlannerConfig = serde_json::from_value(json!({
        "enabled":true,
        "digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex",
            "tools":[],"timeout_ms":20000,"output_cap":4096,"daily_call_budget":1},
        "events":{"enabled":true,"cooldown_ms":0,"max_pending":10,
            "policy":{"price_move_bps":100,"volume_step_units":null,"gap_bps":null,
                "proximity_bps":null,"max_event_age_ms":60000,"max_news_age_ms":60000,
                "max_instruments":20,"max_identities":100}},
        "decision_owner":{"version":1,"owner":"orchestrator"},
        "orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7","transport":"direct_oauth"},"triggers":{"supervisor":{"enabled":true,"prompt":"prompts/supervisor.md"}}}
    }))
    .unwrap();
    cfg.decide.enabled = false;
    let supervisor_prompt = dir.join("owner-supervisor.md");
    fs::write(&supervisor_prompt, "owner decision fixture").unwrap();
    cfg.orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&super::super::orchestrator_model::Trigger::Supervisor)
        .unwrap()
        .prompt = supervisor_prompt;
    // The fresh-quote executor reloads the durable policy to refuse a stale
    // intent; point it at the exact same reviewed policy this fixture uses.
    let planner_path = dir.join("owner-planner.json");
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(planner_path);
    (c, cfg)
}

/// Binding state a restarted controller can find: the turn is durable and the
/// ledger row is `running`, but no model result was committed.
#[cfg(test)]
pub(crate) fn interruption_fixture(
    dir: &std::path::Path,
    now: i64,
) -> (Config, planner::PlannerConfig, Connection, Portfolio, Claim) {
    let (c, cfg) = owner_fixture_config(dir);
    interruption_fixture_for_config(c, cfg, now, "grok-4.7")
}

/// Model-switch variant with the selectable profile installed before the owner
/// policy and digest claim are bound.
#[cfg(test)]
pub(crate) fn interruption_switch_fixture(
    dir: &std::path::Path,
    now: i64,
    base_model_id: &str,
    base_transport: sadr_cli::native_transport::Transport,
    profile_name: &str,
    profile_model_id: &str,
    profile_transport: sadr_cli::native_transport::Transport,
) -> (Config, planner::PlannerConfig, Connection, Portfolio, Claim) {
    let (mut c, mut cfg) = owner_fixture_config(dir);
    let base_path = dir.join("switch-base-model.json");
    let profile_path = dir.join("switch-profile-model.json");
    fs::write(&base_path, b"{}").unwrap();
    fs::write(&profile_path, b"{}").unwrap();
    let orchestrator = cfg.orchestrator.as_mut().unwrap();
    orchestrator.model.model_id = base_model_id.to_owned();
    orchestrator.model.transport = base_transport;
    orchestrator.model.model_config = Some(base_path);
    orchestrator.model_profiles.insert(
        profile_name.to_owned(),
        super::super::orchestrator_model::ModelProfile {
            model_id: profile_model_id.to_owned(),
            transport: profile_transport,
            model_config: profile_path,
        },
    );
    orchestrator.admitted_models = vec![profile_name.to_owned()];
    let planner_path = c.planner_config.as_ref().unwrap().clone();
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(planner_path);
    interruption_fixture_for_config(c, cfg, now, base_model_id)
}

#[cfg(test)]
fn interruption_fixture_for_config(
    c: Config,
    cfg: planner::PlannerConfig,
    now: i64,
    model_id: &str,
) -> (Config, planner::PlannerConfig, Connection, Portfolio, Claim) {
    let mut p = super::super::tests::test_portfolio();
    p.model_led = true;
    let instrument = exercise_instrument();
    let mut db = super::super::open_book(&c).unwrap();
    super::super::wide::ensure_actions(&db).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), now).unwrap();
    let task_id = "c".repeat(64);
    let result_sha256 = "d".repeat(64);
    let event = json!({"id":"owner-interrupted-event","kind":"recheck",
        "reason":"research_task_terminal","instrument_key":instrument.key,"observed_ms":now,
        "research_task":{"task_id":task_id,"revision":3,"status":"succeeded",
            "result_sha256":result_sha256},"evidence":{}});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params!["owner-interrupted-event", now, event.to_string()],
    )
    .unwrap();
    let claim = redecision::reserve(&mut db, &c, &cfg, &p, now, true)
        .unwrap()
        .unwrap();
    // Startup still wins the reservation. The receipt is folded onto that claim;
    // these checks exercise the validator directly rather than assuming the
    // research row itself was selected.
    let mut missing_time = claim.clone();
    missing_time.event_id = "owner-interrupted-event".into();
    missing_time.trigger["research_task"] = json!({"task_id":task_id,"revision":3,
        "status":"succeeded","result_sha256":result_sha256});
    missing_time.trigger["observed_ms"] = Value::Null;
    assert_eq!(
        research_task_refs(&missing_time).unwrap_err().to_string(),
        "owner_research_task_receipt_invalid"
    );
    let mut over_cap = claim.clone();
    over_cap.event_id = "owner-interrupted-event".into();
    over_cap.trigger["observed_ms"] = json!(now);
    over_cap.trigger["research_task"] = json!({"task_id":task_id,"revision":3,
        "status":"succeeded","result_sha256":result_sha256});
    over_cap.trigger["collapsed_research_tasks"] = json!(
        (0..8)
            .map(
                |index| json!({"task_id":format!("{:064x}", index + 1),"revision":1,
            "status":"cancelled","result_sha256":null,"source_event_id":format!("event-{index}"),
            "source_observed_ms":now})
            )
            .collect::<Vec<_>>()
    );
    let (bounded_refs, truncated) = research_task_refs(&over_cap).unwrap();
    assert_eq!(bounded_refs.len(), 8);
    assert!(truncated);
    redecision::settle(
        &mut db,
        &c,
        &cfg,
        &p,
        &claim,
        Ok(exercise_plan(&c, &cfg, &p, &claim, &instrument)),
        now,
        true,
    )
    .unwrap();
    let mut context = Context {
        db: &mut db,
        portfolio: &p,
        instruments: std::slice::from_ref(&instrument),
    };
    let mut work = queued(&mut context, &c, &cfg, now).unwrap().unwrap();
    assert_eq!(work.facts["research_tasks"][0]["task_id"], task_id);
    assert_eq!(work.facts["research_tasks"][0]["revision"], 3);
    assert_eq!(work.facts["research_tasks"][0]["status"], "succeeded");
    assert_eq!(
        work.facts["research_tasks"][0]["result_sha256"],
        result_sha256
    );
    assert_eq!(
        work.facts["research_tasks"][0]["source_event_id"],
        "owner-interrupted-event"
    );
    assert_eq!(work.facts["research_tasks"][0]["source_observed_ms"], now);
    work.claim.turn_id = "turn-owner-interrupted".into();
    work.claim.model_id = model_id.into();
    work.claim.started_ms = now;
    bind(&mut context, &c, &cfg, &mut work.claim, now).unwrap();
    (c, cfg, db, p, work.claim)
}

#[cfg(test)]
pub(crate) fn exercise_instrument() -> Instrument {
    use sadr_core::finance::Price;
    Instrument {
        key: "NSE_EQ|OWNER_FIXTURE".into(),
        symbol: "OWNER_FIXTURE".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }
}

#[cfg(test)]
pub(crate) fn exercise() {
    use sadr_core::finance::Price;

    fn timestamp() -> i64 {
        chrono::DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
            .unwrap()
            .timestamp_millis()
    }
    // Live ranking is retained as decision provenance. It does not activate a
    // scorer or authorize a fill.
    let live = serde_json::json!({
        "strategy_champion": "unused-champion",
        "selection": "selected strategy scorer scores, descending",
        "live_ranking": {"sha256": "a".repeat(64), "name": "live-candidate", "scored_count": 3, "paper_only": true},
    });
    let live_attr = super::super::discovery::attribution(&live);
    assert_eq!(live_attr["spec_hash"], "a".repeat(64));
    assert_eq!(live_attr["live_ranking"], true);
    assert_eq!(
        live_attr["selection_reason"],
        "selected strategy scorer scores, descending"
    );
    let provenance = selected_version(&serde_json::json!({"selection_provenance": {
        "source_hash": live_attr["spec_hash"],
        "selection_reason": live_attr["selection_reason"],
        "live_ranking": true,
    }}));
    assert_eq!(provenance["activation_authority"], false);
    assert_eq!(provenance["attribution_only"], true);
    let dir = tempfile::tempdir().unwrap();
    let (c, cfg) = owner_fixture_config(dir.path());
    let mut p = super::super::tests::test_portfolio();
    p.model_led = true;
    let instrument = exercise_instrument();
    let now = timestamp();
    let mut db = super::super::open_book(&c).unwrap();
    super::super::wide::ensure_actions(&db).unwrap();
    redecision::initialize(&mut db, &c, &p, Some(&cfg), now).unwrap();

    let first = redecision::reserve(&mut db, &c, &cfg, &p, now, true)
        .unwrap()
        .unwrap();
    // A complete-universe durable input is larger than a provider response.
    // Carry it through settlement, owner admission, binding and atomic commit.
    let mut first_plan = exercise_plan(&c, &cfg, &p, &first, &instrument);
    let bars: Vec<_> = (0..21)
        .map(|offset| json!({"offset":offset,"close":"100.00","volume":10000}))
        .collect();
    first_plan["input"]["historical_features"] = json!(
        (0..2733)
            .map(|index| {
                json!({"instrument_key":format!("fixture-universe-{index}"),"bars":bars})
            })
            .collect::<Vec<_>>()
    );
    first_plan["input_hash"] = json!(digest(&serde_json::to_vec(&first_plan["input"]).unwrap()));
    let large = serde_json::to_vec(&first_plan).unwrap();
    assert!(large.len() > 1_000_000 && large.len() < planner::SAVED_PLAN_MAX_BYTES);
    assert!(sadr_core::native::decode(&large).is_err());
    assert!(
        redecision::settle(&mut db, &c, &cfg, &p, &first, Ok(first_plan), now, true,)
            .unwrap()
            .is_none()
    );
    let mut context = Context {
        db: &mut db,
        portfolio: &p,
        instruments: std::slice::from_ref(&instrument),
    };
    let mut work = queued(&mut context, &c, &cfg, now).unwrap().unwrap();
    assert_eq!(work.claim.id, first.id);
    let original: String = context
        .db
        .query_row(
            "SELECT result FROM planner_attempt_log WHERE id=?",
            [&first.id],
            |r| r.get(0),
        )
        .unwrap();
    for (replacement, expected) in [
        (
            " ".repeat(planner::SAVED_PLAN_MAX_BYTES + 1),
            "owner_digest_evidence_changed",
        ),
        (
            "{\"duplicate\":1,\"duplicate\":2}".into(),
            "owner_digest_evidence_invalid",
        ),
        (
            {
                let mut invalid: Value = serde_json::from_str(&original).unwrap();
                invalid["attempt_id"] = json!("different-attempt");
                invalid.to_string()
            },
            "owner_digest_evidence_invalid",
        ),
    ] {
        let mut invalid = work.claim.clone();
        invalid.base_sha256 = digest(replacement.as_bytes());
        context
            .db
            .execute(
                "UPDATE planner_attempt_log SET result=? WHERE id=?",
                rusqlite::params![replacement, first.id],
            )
            .unwrap();
        assert_eq!(
            base_plan(context.db, &c, &invalid).unwrap_err().to_string(),
            expected
        );
    }
    context
        .db
        .execute(
            "UPDATE planner_attempt_log SET result=? WHERE id=?",
            rusqlite::params![original, first.id],
        )
        .unwrap();
    work.claim.turn_id = "turn-owner-fixture-1".into();
    work.claim.model_id = "fixture-supervisor".into();
    work.claim.started_ms = now;
    bind(&mut context, &c, &cfg, &mut work.claim, now).unwrap();

    // A newer digest coalesces queued work, while an already-bound turn stays
    // running and prevents a second decision from being admitted.
    let event = json!({"id":"owner-second-event","kind":"price_move",
        "instrument_key":instrument.key,"observed_ms":now+1,"evidence":{}});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params!["owner-second-event", now + 1, event.to_string()],
    )
    .unwrap();
    let second = redecision::reserve(&mut db, &c, &cfg, &p, now + 1, true)
        .unwrap()
        .unwrap();
    redecision::settle(
        &mut db,
        &c,
        &cfg,
        &p,
        &second,
        Ok(exercise_plan(&c, &cfg, &p, &second, &instrument)),
        now + 1,
        true,
    )
    .unwrap();
    let second_state: String = db
        .query_row(
            "SELECT state FROM owner_digest_work WHERE id=?",
            [&second.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(second_state, "pending");
    let running_count: i64 = db
        .query_row(
            "SELECT count(*) FROM owner_digest_work WHERE state='running'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(running_count, 1);

    let concurrent_event = json!({"id":"owner-concurrent-digest","kind":"price_move",
        "instrument_key":instrument.key,"observed_ms":now+2,"evidence":{}});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params![
            "owner-concurrent-digest",
            now + 2,
            concurrent_event.to_string()
        ],
    )
    .unwrap();
    let concurrent = redecision::reserve(&mut db, &c, &cfg, &p, now + 2, true)
        .unwrap()
        .unwrap();
    assert!(
        redecision::owner_ready(&db, &c, &cfg).unwrap(),
        "a factual digest is not a second decision owner"
    );
    let control: String = db
        .query_row(
            "SELECT body FROM planner_event_control WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let raw = json!({"actions":[],"stance":"hold","notes":"fixture"}).to_string();
    let mut context = Context {
        db: &mut db,
        portfolio: &p,
        instruments: std::slice::from_ref(&instrument),
    };
    for reason in [
        redecision::DATA_BLOCK,
        "operator_stopped_no_replay",
        "planner_event_configuration_changed",
    ] {
        let mut blocked: Value = serde_json::from_str(&control).unwrap();
        blocked["blocked"] = json!(reason);
        context
            .db
            .execute(
                "UPDATE planner_event_control SET body=? WHERE id=1",
                [blocked.to_string()],
            )
            .unwrap();
        assert_eq!(
            commit(&mut context, &c, &cfg, &work.claim, &raw, now + 2)
                .unwrap_err()
                .to_string(),
            "planner_decision_owner_not_ready"
        );
    }
    context
        .db
        .execute(
            "UPDATE planner_event_control SET body=? WHERE id=1",
            [&control],
        )
        .unwrap();
    // A transient quote suspension suspends execution only: it is not a sticky
    // block, so the owner decision still commits while it is set (#221).
    redecision::block(context.db, &c, redecision::READ_TRANSIENT_BLOCK).unwrap();
    let committed_plan = commit(&mut context, &c, &cfg, &work.claim, &raw, now + 2).unwrap();
    let suspended: Value = serde_json::from_str(
        &context
            .db
            .query_row::<String, _, _>(
                "SELECT body FROM planner_event_control WHERE id=1",
                [],
                |r| r.get(0),
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(suspended["pending"]["id"], concurrent.id);
    assert_eq!(suspended["blocked"], redecision::READ_TRANSIENT_BLOCK);
    assert!(
        redecision::active_plan(context.db, &c)
            .unwrap()
            .flatten()
            .is_none(),
        "quote suspension still blocks execution"
    );
    assert_eq!(committed_plan["decision_owner"], "orchestrator");
    assert!(commit(&mut context, &c, &cfg, &work.claim, &raw, now + 2).is_err());
    redecision::settle(
        context.db,
        &c,
        &cfg,
        &p,
        &concurrent,
        Err("read_quota_wait_per_second".into()),
        now + 3,
        true,
    )
    .unwrap();
    // Only a validated fresh quote clears the suspension, and the committed
    // owner plan then becomes executable.
    assert!(redecision::clear_transient_read_block(context.db, &c).unwrap());
    assert_eq!(
        redecision::active_plan(context.db, &c).unwrap().flatten(),
        Some(committed_plan.clone())
    );
    let next_event = json!({"id":"owner-next-facts","kind":"price_move",
        "instrument_key":instrument.key,"observed_ms":now+3,"evidence":{}});
    context
        .db
        .execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
            rusqlite::params!["owner-next-facts", now + 3, next_event.to_string()],
        )
        .unwrap();
    let next_facts = redecision::reserve(context.db, &c, &cfg, &p, now + 3, true)
        .unwrap()
        .unwrap();
    assert_eq!(
        redecision::active_plan(context.db, &c).unwrap().flatten(),
        Some(committed_plan.clone()),
        "clearing the read block keeps the committed owner plan"
    );
    let mut empty_facts = exercise_plan(&c, &cfg, &p, &next_facts, &instrument);
    empty_facts["owner_digest"]["eligible"] = json!(false);
    redecision::settle(
        context.db,
        &c,
        &cfg,
        &p,
        &next_facts,
        Ok(empty_facts),
        now + 3,
        true,
    )
    .unwrap();
    assert_eq!(
        redecision::active_plan(context.db, &c).unwrap().flatten(),
        Some(committed_plan.clone())
    );
    // A newer digest that fails recoverably between the bind and the commit is
    // one bad attempt, not a decision block (#221): it keeps its reason on the
    // attempt and does not refuse the owner.
    let failing_event = json!({"id":"owner-failing-digest","kind":"price_move",
        "instrument_key":instrument.key,"observed_ms":now+4,"evidence":{}});
    context
        .db
        .execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
            rusqlite::params!["owner-failing-digest", now + 4, failing_event.to_string()],
        )
        .unwrap();
    let failing = redecision::reserve(context.db, &c, &cfg, &p, now + 4, true)
        .unwrap()
        .unwrap();
    redecision::settle(
        context.db,
        &c,
        &cfg,
        &p,
        &failing,
        Err("planner_provider_failed".into()),
        now + 4,
        true,
    )
    .unwrap();
    assert!(
        redecision::owner_ready(context.db, &c, &cfg).unwrap(),
        "a recoverable digest failure does not refuse the owner"
    );
    assert_eq!(
        redecision::status(context.db, &c).unwrap().unwrap()["last_digest_failure"],
        "planner_provider_failed"
    );
    assert_eq!(
        redecision::active_plan(context.db, &c).unwrap().flatten(),
        Some(committed_plan.clone()),
        "a failed digest keeps the committed owner plan"
    );
    assert_eq!(
        committed(&db, &c, &work.claim).unwrap(),
        Some(committed_plan.clone())
    );

    // Recovery is tied to the exact original book and validates every receipt
    // identity field before returning the committed plan.
    let other_dir = tempfile::tempdir().unwrap();
    let other_book = super::super::tests::config(other_dir.path());
    assert!(committed(&db, &other_book, &work.claim).unwrap().is_none());
    let prior_receipt: String = db
        .query_row(
            "SELECT result FROM owner_digest_work WHERE id=?",
            [&first.id],
            |r| r.get(0),
        )
        .unwrap();
    let mut receipt: Value = serde_json::from_str(&prior_receipt).unwrap();
    receipt["digest_id"] = json!("0".repeat(64));
    db.execute(
        "UPDATE owner_digest_work SET result=? WHERE id=?",
        rusqlite::params![receipt.to_string(), first.id],
    )
    .unwrap();
    assert!(committed(&db, &c, &work.claim).is_err());
    db.execute(
        "UPDATE owner_digest_work SET result=? WHERE id=?",
        rusqlite::params![prior_receipt, first.id],
    )
    .unwrap();

    // Halted books refuse queued work without replacing the active plan.
    let mut halted = p.clone();
    halted.halted = true;
    {
        let mut context = Context {
            db: &mut db,
            portfolio: &halted,
            instruments: std::slice::from_ref(&instrument),
        };
        assert!(matches!(queued(&mut context, &c, &cfg, now + 3),
                Err(ref error) if error.to_string() == "owner_work_refused_owner_decision_outside_session"));
    }
    let state: String = db
        .query_row(
            "SELECT state FROM owner_digest_work WHERE id=?",
            [&second.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "refused");
    assert_eq!(
        redecision::active_plan(&db, &c).unwrap().flatten(),
        Some(committed_plan.clone())
    );

    // Altering the source attempt bytes after admission fails the pinned digest
    // check; no owner plan is activated from reconstructed or substituted data.
    let third_event = json!({"id":"owner-third-event","kind":"price_move",
        "instrument_key":instrument.key,"observed_ms":now+4,"evidence":{}});
    db.execute(
        "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
        rusqlite::params!["owner-third-event", now + 4, third_event.to_string()],
    )
    .unwrap();
    let third = redecision::reserve(&mut db, &c, &cfg, &p, now + 4, true)
        .unwrap()
        .unwrap();
    redecision::settle(
        &mut db,
        &c,
        &cfg,
        &p,
        &third,
        Ok(exercise_plan(&c, &cfg, &p, &third, &instrument)),
        now + 4,
        true,
    )
    .unwrap();
    let original: String = db
        .query_row(
            "SELECT result FROM planner_attempt_log WHERE id=?",
            [&third.id],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE planner_attempt_log SET result=? WHERE id=?",
        rusqlite::params![format!("{original} "), third.id],
    )
    .unwrap();
    let mut context = Context {
        db: &mut db,
        portfolio: &p,
        instruments: std::slice::from_ref(&instrument),
    };
    assert!(
        matches!(queued(&mut context, &c, &cfg, now + 4), Err(ref error)
            if error.to_string() == "owner_digest_evidence_changed")
    );

    // Separate halted, stopped and expired queued turns all fail closed and
    // leave no active plan. The current work is always the newest pending row.
    for (event_id, delta) in [
        ("owner-halted-event", 5_i64),
        ("owner-stopped-event", 6_i64),
        ("owner-expired-event", 7_i64),
    ] {
        let event = json!({"id":event_id,"kind":"price_move",
            "instrument_key":instrument.key,"observed_ms":now+delta,"evidence":{}});
        db.execute(
            "INSERT INTO planner_event_log VALUES(?,?,?,'pending')",
            rusqlite::params![event_id, now + delta, event.to_string()],
        )
        .unwrap();
        let next = redecision::reserve(&mut db, &c, &cfg, &p, now + delta, true)
            .unwrap()
            .unwrap();
        redecision::settle(
            &mut db,
            &c,
            &cfg,
            &p,
            &next,
            Ok(exercise_plan(&c, &cfg, &p, &next, &instrument)),
            now + delta,
            true,
        )
        .unwrap();
        let when = if event_id == "owner-expired-event" {
            next.deadline_ms + 1
        } else {
            now + delta + 1
        };
        let mut invalid = p.clone();
        invalid.halted = event_id == "owner-halted-event";
        invalid.stopped = event_id == "owner-stopped-event";
        let mut context = Context {
            db: &mut db,
            portfolio: &invalid,
            instruments: std::slice::from_ref(&instrument),
        };
        let expected = if event_id == "owner-expired-event" {
            "owner_work_refused_owner_decision_expired"
        } else {
            "owner_work_refused_owner_decision_outside_session"
        };
        assert!(matches!(queued(&mut context, &c, &cfg, when),
                Err(ref error) if error.to_string() == expected));
        let work_state: String = db
            .query_row(
                "SELECT state FROM owner_digest_work WHERE id=?",
                [&next.id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(work_state, "refused");
        // Refusing queued work never erases the committed owner plan or its
        // action receipts; only a replacement validated owner decision or an
        // explicit stop/halt/recover transition may supersede them.
        assert_eq!(
            redecision::active_plan(&db, &c).unwrap().flatten(),
            Some(committed_plan.clone())
        );
    }

    // A committed owner intent uses the unchanged fresh-quote paper executor.
    // Buy, hold, exit revision and sell each hydrate from the owner plan head
    // and record their own action receipt without a second model decision.
    let quote = |at: i64| sadr_core::execution::Quote {
        bid: Price::try_from(sadr_core::finance::Amount::from_integer(100)).unwrap(),
        ask: Price::try_from(
            sadr_core::finance::Amount::from_integer(100)
                .checked_add(sadr_core::execution::a("0.05"))
                .unwrap(),
        )
        .unwrap(),
        bid_units: 10_000,
        ask_units: 10_000,
        source: at,
        receipt: at,
    };
    let buy_raw = json!({"actions":[{"instrument_key":instrument.key,"side":"buy","units":2,
        "max_price":"101","rationale":"owner buy",
        "exit_intent":{"target":"110","stop":"90"}}],"stance":"long","notes":""})
    .to_string();
    let buy_plan = exercise_round(
        &mut db,
        &c,
        &cfg,
        &p,
        &instrument,
        &buy_raw,
        "owner-exec-buy",
        now + 10,
    );
    // The decision turn owns its execution window; it never inherits the
    // digest attempt's earlier deadline.
    assert!(
        buy_plan["valid_until_ms"].as_i64().unwrap() > first.deadline_ms,
        "owner plan inherited the digest deadline"
    );
    // A model switch may not promote while the accepted intent still has
    // unmatched action receipts.
    assert!(!actions_settled(&mut db, &c, now + 10).unwrap());
    let mut quotes = std::collections::BTreeMap::new();
    quotes.insert(instrument.key.clone(), quote(now + 10));
    super::super::wide::commit_quotes(
        &mut db,
        &c,
        &mut p,
        &quotes,
        &[],
        now + 10,
        "owner-exec-buy",
        || now + 10,
    )
    .unwrap();
    assert_eq!(p.fills.len(), 1);
    assert_eq!(p.positions[&instrument.key].units, 2);
    assert!(actions_settled(&mut db, &c, now + 10).unwrap());
    assert_eq!(
        p.fills[0].decision_id.as_deref(),
        Some("owner-turn-owner-exec-buy")
    );

    let hold_raw = json!({"actions":[],"stance":"hold","notes":"owner hold"}).to_string();
    exercise_round(
        &mut db,
        &c,
        &cfg,
        &p,
        &instrument,
        &hold_raw,
        "owner-exec-hold",
        now + 15,
    );
    super::super::wide::commit_quotes(
        &mut db,
        &c,
        &mut p,
        &quotes,
        &[],
        now + 15,
        "owner-exec-hold",
        || now + 15,
    )
    .unwrap();
    assert_eq!(p.fills.len(), 1);

    let revise_raw = json!({"actions":[{"instrument_key":instrument.key,"side":"revise_exit",
        "rupees":null,"units":null,"max_price":null,"rationale":"raise the stop",
        "exit_intent":{"target":"120","stop":"95"}}],"stance":"hold","notes":""})
    .to_string();
    exercise_round(
        &mut db,
        &c,
        &cfg,
        &p,
        &instrument,
        &revise_raw,
        "owner-exec-revise",
        now + 20,
    );
    super::super::wide::commit_quotes(
        &mut db,
        &c,
        &mut p,
        &quotes,
        &[],
        now + 20,
        "owner-exec-revise",
        || now + 20,
    )
    .unwrap();
    assert_eq!(p.fills.len(), 1);
    assert_eq!(
        p.positions[&instrument.key]
            .exit_intent
            .as_ref()
            .and_then(|exit| exit.stop),
        Some(Price::parse("95").unwrap())
    );
    let revised: String = db
        .query_row(
            "SELECT verdict FROM planner_action WHERE decision='owner-turn-owner-exec-revise'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(revised, "exit_revised");

    let sell_raw = json!({"actions":[{"instrument_key":instrument.key,"side":"sell","units":1,
        "max_price":"100","rationale":"owner trim",
        "exit_intent":{"target":null,"stop":null,"time":null}}],"stance":"trim","notes":""})
    .to_string();
    exercise_round(
        &mut db,
        &c,
        &cfg,
        &p,
        &instrument,
        &sell_raw,
        "owner-exec-sell",
        now + 25,
    );
    super::super::wide::commit_quotes(
        &mut db,
        &c,
        &mut p,
        &quotes,
        &[],
        now + 25,
        "owner-exec-sell",
        || now + 25,
    )
    .unwrap();
    assert_eq!(p.fills.len(), 2);
    assert!(!p.fills[1].buy);
    assert_eq!(p.positions[&instrument.key].units, 1);
    assert_eq!(
        p.fills[1].decision_id.as_deref(),
        Some("owner-turn-owner-exec-sell")
    );

    let report = activity_report(&db, &c, &p).unwrap();
    let report_json = report.to_json();
    assert_eq!(report.session, c.session.to_string());
    assert!(report.conversion.decisions_considered >= 2);
    assert_eq!(report.conversion.fills_executed, 2);
    assert_eq!(
        report.financials.realized_pnl,
        Amount::parse("-0.20").unwrap()
    );
    assert_eq!(
        report.financials.marked_pnl,
        Amount::parse("-0.15").unwrap()
    );
    assert_eq!(
        report.financials.gross_profit,
        Amount::parse("-0.35").unwrap()
    );
    assert_eq!(
        report.financials.total_slippage,
        Amount::parse("0.25").unwrap()
    );
    assert_eq!(
        report.financials.net_after_cost_pnl,
        report
            .financials
            .gross_profit
            .checked_sub(report.financials.total_fees)
            .unwrap()
    );
    assert_eq!(
        report.financials.total_cost_drag,
        report
            .financials
            .total_fees
            .checked_add(report.financials.total_slippage)
            .unwrap()
    );
    assert_eq!(report_json["drawdown_coverage"]["complete"], false);
    assert_eq!(report.experiment_attributions.len(), 1);
    assert_eq!(
        report.experiment_attributions[0].source_hash,
        "a".repeat(64)
    );
    assert_eq!(report.experiment_attributions[0].fills_count, 2);
    assert_eq!(
        report.experiment_attributions[0].realized_pnl,
        Amount::parse("-0.20").unwrap()
    );
    assert_eq!(
        report.experiment_attributions[0].marked_pnl,
        Amount::parse("-0.15").unwrap()
    );
    assert_eq!(
        report.experiment_attributions[0].net_after_cost_pnl,
        Amount::parse("-0.35")
            .unwrap()
            .checked_sub(report.experiment_attributions[0].fees_incurred)
            .unwrap()
    );
    assert_eq!(report_json["session"], c.session.to_string());
    assert_eq!(activity_analytics(&db, &c, &p).unwrap(), report_json);

    // Without the owner-work schema, retain exact portfolio financials while
    // leaving decision and experiment attribution empty.
    let no_owner_db = rusqlite::Connection::open_in_memory().unwrap();
    let no_owner_report = activity_report(&no_owner_db, &c, &p).unwrap();
    assert_eq!(no_owner_report.conversion.decisions_considered, 0);
    assert_eq!(no_owner_report.financials, report.financials);
    assert!(no_owner_report.experiment_attributions.is_empty());

    // Reviewed authority migration: a legacy-initialized book refuses the owner
    // policy until config-apply transitions the exact policy. The financial
    // state bytes and active plan stay untouched, and rollback is the same
    // reviewed transition back to the predecessor policy.
    let migration_dir = tempfile::tempdir().unwrap();
    let migration_c = super::super::tests::config(migration_dir.path());
    let mut migration_db = super::super::open_book(&migration_c).unwrap();
    super::super::wide::persist(
        &mut migration_db,
        &migration_c,
        &p,
        now,
        None,
        None,
        "fixture_book",
    )
    .unwrap();
    let mut legacy_cfg = cfg.clone();
    legacy_cfg.decision_owner = None;
    legacy_cfg.decide.enabled = true;
    redecision::initialize(&mut migration_db, &migration_c, &p, Some(&legacy_cfg), now).unwrap();
    let book_before: String = migration_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        redecision::owner_startup_check(&migration_db, &migration_c, &cfg)
            .unwrap_err()
            .to_string(),
        "decision_owner_migration_required"
    );
    redecision::apply_config(&migration_db, &migration_c, &cfg).unwrap();
    redecision::owner_startup_check(&migration_db, &migration_c, &cfg).unwrap();
    let book_after: String = migration_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(book_before, book_after, "migration changed the paper book");
    redecision::apply_config(&migration_db, &migration_c, &legacy_cfg).unwrap();
    redecision::owner_startup_check(&migration_db, &migration_c, &legacy_cfg).unwrap();
    let book_rolled_back: String = migration_db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        book_before, book_rolled_back,
        "rollback changed the paper book"
    );
}
