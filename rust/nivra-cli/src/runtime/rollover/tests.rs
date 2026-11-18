use super::*;
fn cost_transition_fixture(dir: &Path, closed: bool) -> (Config, Config) {
    use nivra_core::{
        execution::{ExitIntent, Instrument, Quote},
        finance::Price,
    };
    let mut old = super::super::tests::config(dir);
    old.sessions_root = Some(dir.join("sessions"));
    old.directory = old
        .sessions_root
        .as_ref()
        .unwrap()
        .join(old.session.to_string());
    fs::create_dir_all(&old.directory).unwrap();
    fs::write(&old.read_journal, b"fixture").unwrap();
    let metadata = fs::metadata(&old.read_journal).unwrap();
    old.read_journal_device = metadata.dev();
    old.read_journal_inode = metadata.ino();
    let db = open_book(&old).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let at = DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis();
    let q = Quote {
        bid: Price::parse("100").unwrap(),
        ask: Price::parse("100.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    assert!(
        p.execute_plan(
            at,
            &i,
            &q,
            true,
            2,
            Price::parse("101").unwrap(),
            ExitIntent::default(),
            "offline-advance-fixture"
        )
        .unwrap()
    );
    p.closed = closed;
    p.reconcile().unwrap();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![
            book_binding(&old).unwrap(),
            serde_json::to_string(&p).unwrap()
        ],
    )
    .unwrap();
    fs::write(
        old.directory.join("report.json"),
        serde_json::to_vec(
            &json!({"paper_only":true,"account":old.account,"session":old.session,"portfolio":p}),
        )
        .unwrap(),
    )
    .unwrap();
    drop(db);
    let prior = dir.join("prior-paper.json");
    fs::write(&prior, serde_json::to_vec(&old).unwrap()).unwrap();
    let mut changed = old.clone();
    changed.paper_costs = Some(
        serde_json::from_str(include_str!("../../../../../fixtures/paper-costs.json")).unwrap(),
    );
    changed.paper_cost_transition = Some(super::super::PaperCostTransition {
        source_session: old.session,
        prior_config: prior,
    });
    (old, changed)
}
fn state_body(c: &Config) -> String {
    open_book(c)
        .unwrap()
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap()
}
fn stopped_carry_fixture(dir: &Path) -> (Config, i64) {
    let (c, _) = cost_transition_fixture(dir, true);
    let now = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut db = open_book(&c).unwrap();
    let mut p: Portfolio = serde_json::from_str(&state_body(&c)).unwrap();
    let cfg: planner::PlannerConfig = serde_json::from_value(json!({"enabled":true,
            "digest":{"enabled":true,"model_id":"fixture-model","daily_call_budget":8},"orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7"},"triggers":{"supervisor":{"prompt":concat!(env!("CARGO_MANIFEST_DIR"),"/../../packaging/rust/prompts/supervisor.md")}}},
            "events":{"enabled":true,"cooldown_ms":0,"max_pending":10,
                "policy":{"price_move_bps":100,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}}})).unwrap();
    // Authority is recorded while the book can still trade. A closed book
    // skips initialization so a later config restart cannot halt it.
    p.closed = false;
    redecision::initialize(&mut db, &c, &p, Some(&cfg), now).unwrap();
    redecision::block(&mut db, &c, "operator_stopped_no_replay").unwrap();
    p.closed = true;
    p.stopped = true;
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&p).unwrap()],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,'{}','operator_stopped')",
        [now - 1000],
    )
    .unwrap();
    fs::write(c.directory.join("report.json"), serde_json::to_vec(&json!({
            "paper_only":true,"live_enabled":false,"account":c.account,"session":c.session,"portfolio":p})).unwrap()).unwrap();
    record_review(&c).unwrap();
    (c, now)
}

fn halted_carry_fixture(dir: &Path) -> (Config, i64, planner::PlannerConfig) {
    use nivra_core::{
        execution::{ExitIntent, Instrument, Quote},
        finance::Price,
    };
    let mut c = super::super::tests::config(dir);
    c.sessions_root = Some(dir.join("sessions"));
    c.directory = c
        .sessions_root
        .as_ref()
        .unwrap()
        .join(c.session.to_string());
    fs::create_dir_all(&c.directory).unwrap();
    fs::write(&c.read_journal, b"fixture").unwrap();
    let metadata = fs::metadata(&c.read_journal).unwrap();
    c.read_journal_device = metadata.dev();
    c.read_journal_inode = metadata.ino();
    let cfg: planner::PlannerConfig = serde_json::from_value(json!({"enabled":true,
            "digest":{"enabled":true,"model_id":"fixture-model","daily_call_budget":8},"orchestrator":{"enabled":true,"model":{"model_id":"grok-4.7"},"triggers":{"supervisor":{"prompt":concat!(env!("CARGO_MANIFEST_DIR"),"/../../packaging/rust/prompts/supervisor.md")}}},
            "events":{"enabled":true,"cooldown_ms":0,"max_pending":10,
                "policy":{"price_move_bps":100,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}}})).unwrap();
    let config_path = dir.join("planner.json");
    fs::write(&config_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(config_path);
    let at = DateTime::parse_from_rfc3339("2026-10-16T15:29:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let q = Quote {
        bid: Price::parse("100").unwrap(),
        ask: Price::parse("100.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    assert!(
        p.execute_plan(
            at,
            &i,
            &q,
            true,
            2,
            Price::parse("101").unwrap(),
            ExitIntent::default(),
            "halted-carry-fixture"
        )
        .unwrap()
    );
    p.model_led = true;
    // Authority exists while the book can still trade; a later restart of a
    // closed or halted book never initializes a fresh authority.
    redecision::initialize(&mut db, &c, &p, Some(&cfg), at).unwrap();
    p.halted = true;
    p.closed = true;
    p.health = "risk_halted".into();
    p.reconcile().unwrap();
    wide::persist(&mut db, &c, &p, at, None, None, "daily_loss_tripwire").unwrap();
    drop(db);
    fs::write(
            c.directory.join("report.json"),
            serde_json::to_vec(&json!({"paper_only":true,"live_enabled":false,"account":c.account,"session":c.session,"portfolio":p})).unwrap(),
        )
        .unwrap();
    record_review(&c).unwrap();
    (c, at, cfg)
}
fn carried_closed_fixture(dir: &Path) -> (Config, PathBuf) {
    use nivra_core::{
        execution::{ExitIntent, Instrument, Quote},
        finance::Price,
    };
    let mut c = super::super::tests::config(dir);
    c.sessions_root = Some(dir.join("sessions"));
    c.directory = c
        .sessions_root
        .as_ref()
        .unwrap()
        .join(c.session.to_string());
    fs::create_dir_all(&c.directory).unwrap();
    fs::write(&c.read_journal, b"fixture").unwrap();
    let metadata = fs::metadata(&c.read_journal).unwrap();
    c.read_journal_device = metadata.dev();
    c.read_journal_inode = metadata.ino();
    let at = DateTime::parse_from_rfc3339("2026-10-16T14:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let q = Quote {
        bid: Price::parse("100").unwrap(),
        ask: Price::parse("100.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    assert!(
        p.execute_plan(
            at,
            &i,
            &q,
            true,
            2,
            Price::parse("101").unwrap(),
            ExitIntent::default(),
            "carried-closed-fixture"
        )
        .unwrap()
    );
    p.model_led = true;
    p.closed = true;
    p.health = "closed_with_carried_positions".into();
    p.reconcile().unwrap();
    wide::persist(&mut db, &c, &p, at, None, None, "post_close").unwrap();
    drop(db);
    let path = dir.join("paper.json");
    fs::write(&path, serde_json::to_vec(&c).unwrap()).unwrap();
    (c, path)
}
fn rewrite_book(c: &Config, p: &Portfolio) {
    open_book(c)
        .unwrap()
        .execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(p).unwrap()],
        )
        .unwrap();
}

#[test]
fn halted_carry_reaches_protection_only_successor_and_clears_only_after_review() {
    use nivra_core::{execution::Quote, finance::Price};
    let d = tempfile::tempdir().unwrap();
    let (c, at, cfg) = halted_carry_fixture(d.path());
    let source = state_body(&c);
    let source_state = fs::read(c.directory.join("paper.sqlite")).unwrap();
    let source_report = fs::read(c.directory.join("report.json")).unwrap();
    let source_review = fs::read(c.directory.join("review.json")).unwrap();
    let monday = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let blocked = preview(&c, monday).unwrap();
    assert_eq!(blocked["status"], "rollover_blocked");
    assert_eq!(blocked["review"]["halted"], true);
    assert_eq!(blocked["review"]["carry_admitted"], false);
    assert!(accept_halted_carry(&c, false, at).is_err());
    let receipt = accept_halted_carry(&c, true, at).unwrap();
    assert_eq!(accept_halted_carry(&c, true, at + 1).unwrap(), receipt);
    assert_eq!(receipt["evidence"]["halt_reason"], "risk_halted");
    assert_eq!(receipt["evidence"]["health"], "risk_halted");
    assert_eq!(receipt["evidence"]["open_positions"], 1);
    let receipt_bytes = fs::read(c.directory.join(HALTED_CARRY_FILE)).unwrap();
    let plan = preview(&c, monday).unwrap();
    assert_eq!(plan["status"], "rollover_ready");
    assert_eq!(
        crate::runtime::phases::PreviewStatus::parse(plan["status"].as_str().unwrap()).unwrap(),
        crate::runtime::phases::PreviewStatus::RolloverReady
    );
    assert_eq!(
        crate::runtime::phases::SealVerdict::parse("eligible_to_seal").unwrap(),
        crate::runtime::phases::SealVerdict::EligibleToSeal
    );
    assert_eq!(
        crate::runtime::phases::SealVerdict::parse("stop_or_halt_human_required").unwrap(),
        crate::runtime::phases::SealVerdict::StopOrHaltHumanRequired
    );
    assert_eq!(
        crate::runtime::phases::SealVerdict::parse("unresolved_positions_human_required").unwrap(),
        crate::runtime::phases::SealVerdict::UnresolvedPositionsHumanRequired
    );
    assert_eq!(
        crate::runtime::phases::IdleStatus::RolloverBlockedInspectReviewHumanRequired.as_str(),
        "rollover_blocked_inspect_review_human_required"
    );
    assert_eq!(
        crate::runtime::phases::PlannerLogPhase::parse("pending")
            .unwrap()
            .as_str(),
        "pending"
    );
    assert_eq!(
        crate::runtime::phases::PlannerLogPhase::parse("reserved")
            .unwrap()
            .as_str(),
        "reserved"
    );
    assert!(nivra_core::book_phase::BookPhase::from_flags(true, true, false).carry_blocked());
    assert!(!nivra_core::book_phase::BookPhase::from_flags(false, true, true).carry_blocked());
    assert_eq!(
        nivra_core::book_phase::BookPhase::from_flags(false, false, false),
        nivra_core::book_phase::BookPhase::Live
    );
    assert_eq!(plan["review"]["halted"], true);
    assert_eq!(plan["review"]["carry_admitted"], true);
    assert_eq!(plan["review"]["halted_carry_admitted"], true);
    assert_eq!(plan["review"]["reason"], HALTED_CARRY_REASON);
    assert_eq!(plan["review"]["carried_positions"], 1);
    assert_eq!(plan["strategy_decision"]["carry_required"], true);
    // An eligible review still starts the session late, at 10:30 IST. Only
    // after the 15:00 late-start cutoff is it a normal next-session wait, not
    // a human-review blocker (#159).
    let late = monday + 2 * 60 * 60_000;
    assert_eq!(preview(&c, late).unwrap()["status"], "rollover_ready");
    let missed = monday + (6 * 60 + 35) * 60_000;
    assert_eq!(
        preview(&c, missed).unwrap()["status"],
        "missed_preopen_idle"
    );
    assert_eq!(
        idle_status(&c, local(missed).date_naive(), missed).unwrap(),
        "missed_preopen_idle"
    );
    assert!(advance(&c, missed).unwrap().is_none());
    assert!(
        !c.sessions_root
            .as_ref()
            .unwrap()
            .join("current.json")
            .exists()
    );
    record_review(&c).unwrap();
    // The honest blocked review seal is preserved, never rewritten.
    assert_eq!(
        fs::read(c.directory.join("review.json")).unwrap(),
        source_review
    );
    assert_eq!(
        advance_offline(&c, monday, true).unwrap()["status"],
        "advanced"
    );
    assert_eq!(
        advance_offline(&c, monday, true).unwrap()["status"],
        "already_advanced"
    );
    let next = current(&c).unwrap();
    let carry: Value =
        serde_json::from_slice(&fs::read(next.directory.join("carry-over.json")).unwrap()).unwrap();
    assert_eq!(carry["halted_carry"]["halt_reason"], "risk_halted");
    assert_eq!(carry["halted_carry"]["health"], PROTECTIVE_HEALTH);
    assert_eq!(carry["halted_carry"]["entries_blocked"], true);
    verify_carry(&next, &carry).unwrap();
    let mut changed = carry.clone();
    changed["halted_carry"]["halt_reason"] = json!("other_halt");
    assert!(verify_carry(&next, &changed).is_err());
    let mut db = open_book(&next).unwrap();
    let mut carried = wide::load_wide(&mut db, &next).unwrap();
    assert!(carried.halted && !carried.stopped && !carried.closed);
    assert_eq!(carried.health, PROTECTIVE_HEALTH);
    let source_book: Portfolio = serde_json::from_str(&source).unwrap();
    assert_eq!(carried.positions, source_book.positions);
    assert_eq!(carried.cash, source_book.cash);
    assert_eq!(carried.fills, source_book.fills);
    // Entries stay blocked while the carried halt is set: resume and the
    // reviewed config transition both refuse, and no model claim can run.
    assert_eq!(
        failure_code(wide::resume(&next, true).unwrap_err().as_ref()),
        "resume_not_operator_stopped"
    );
    // Controller startup records the adopted halt and fresh authority.
    redecision::initialize(&mut db, &next, &carried, Some(&cfg), monday).unwrap();
    let marker: String = db
        .query_row(
            "SELECT body FROM planner_authority_owner WHERE id=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let marker: serde_json::Value = serde_json::from_str(&marker).unwrap();
    assert_eq!(marker["owner"], "orchestrator");
    assert_eq!(marker["binding"], book_binding(&next).unwrap());
    assert!(
        redecision::reserve(&mut db, &next, &cfg, &carried, monday, true)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        failure_code(wide::config_apply(&next, true).unwrap_err().as_ref()),
        "config_apply_halted_requires_planner_recover"
    );
    wide::persist(
        &mut db,
        &next,
        &carried,
        monday,
        None,
        None,
        "daily_session_created",
    )
    .unwrap();
    assert_eq!(
        failure_code(wide::planner_recover(&next, true).unwrap_err().as_ref()),
        "planner_recover_book_not_flat"
    );
    // Protection-only exit still runs the frozen exit policy while halted.
    let mut quotes = std::collections::BTreeMap::new();
    quotes.insert(
        "NSE_EQ|SYNTHETIC".to_string(),
        Quote {
            bid: Price::parse("100").unwrap(),
            ask: Price::parse("100.05").unwrap(),
            bid_units: 100,
            ask_units: 100,
            source: monday,
            receipt: monday,
        },
    );
    carried.protect(monday, &quotes).unwrap();
    assert!(carried.positions.is_empty() && carried.halted);
    wide::persist(
        &mut db,
        &next,
        &carried,
        monday + 1_000,
        None,
        None,
        "paper_exit",
    )
    .unwrap();
    drop(db);
    wide::planner_recover(&next, true).unwrap();
    let flat: Portfolio = serde_json::from_str(&state_body(&next)).unwrap();
    assert!(!flat.halted && flat.stopped && flat.positions.is_empty());
    assert_eq!(flat.health, "operator_stopped");
    let db = open_book(&next).unwrap();
    let audit: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='operator_planner_recovery_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    drop(db);
    let audit: Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["mode"], "flat_carried_protective");
    assert_eq!(audit["entries_blocked_before"], true);
    wide::resume(&next, true).unwrap();
    // The original source book, report, review and acceptance are untouched.
    assert_eq!(state_body(&c), source);
    assert_eq!(
        fs::read(c.directory.join("paper.sqlite")).unwrap(),
        source_state
    );
    assert_eq!(
        fs::read(c.directory.join("report.json")).unwrap(),
        source_report
    );
    assert_eq!(
        fs::read(c.directory.join("review.json")).unwrap(),
        source_review
    );
    assert_eq!(
        fs::read(c.directory.join(HALTED_CARRY_FILE)).unwrap(),
        receipt_bytes
    );
    assert_eq!(current(&c).unwrap().session, next.session);
    let second = tempfile::tempdir().unwrap();
    halted_carry_closed_flat_clear(second.path());
}

/// The carried protective halt can also reach the close flat before the
/// operator clears it; the reviewed recovery still clears only the halt.
fn halted_carry_closed_flat_clear(dir: &Path) {
    use nivra_core::{execution::Quote, finance::Price};
    let (c, at, cfg) = halted_carry_fixture(dir);
    let next_day = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    accept_halted_carry(&c, true, at).unwrap();
    advance_offline(&c, next_day, true).unwrap();
    let next = current(&c).unwrap();
    let mut db = open_book(&next).unwrap();
    let mut p = wide::load_wide(&mut db, &next).unwrap();
    redecision::initialize(&mut db, &next, &p, Some(&cfg), next_day).unwrap();
    wide::persist(
        &mut db,
        &next,
        &p,
        next_day,
        None,
        None,
        "daily_session_created",
    )
    .unwrap();
    let mut quotes = std::collections::BTreeMap::new();
    quotes.insert(
        "NSE_EQ|SYNTHETIC".to_string(),
        Quote {
            bid: Price::parse("100").unwrap(),
            ask: Price::parse("100.05").unwrap(),
            bid_units: 100,
            ask_units: 100,
            source: next_day,
            receipt: next_day,
        },
    );
    p.protect(next_day, &quotes).unwrap();
    assert!(p.positions.is_empty() && p.halted);
    p.closed = true;
    wide::persist(
        &mut db,
        &next,
        &p,
        next_day + 1_000,
        None,
        None,
        "post_close",
    )
    .unwrap();
    drop(db);
    // A closed flat book is safe to deploy with the controller left stopped,
    // even while a planner halt remains (#158).
    let config_path = dir.join("closed-flat-next-config.json");
    fs::write(&config_path, serde_json::to_vec(&next).unwrap()).unwrap();
    assert!(stopped_book_safe(&config_path).unwrap());
    wide::planner_recover(&next, true).unwrap();
    let closed: Portfolio = serde_json::from_str(&state_body(&next)).unwrap();
    assert!(closed.closed && !closed.halted && closed.positions.is_empty());
    assert_eq!(closed.health, "closed");
    let db = open_book(&next).unwrap();
    let audit: String = db
            .query_row(
                "SELECT body FROM ticks WHERE event='operator_planner_recovery_confirmed_paper_review' ORDER BY id DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
    drop(db);
    let audit: Value = serde_json::from_str(&audit).unwrap();
    assert_eq!(audit["mode"], "closed_flat");
    assert_eq!(audit["carried_protective"], true);
    assert_eq!(audit["halted"], false);
    updater_adopts_closed_carried_book_only_outside_market_hours();
}

fn updater_adopts_closed_carried_book_only_outside_market_hours() {
    let d = tempfile::tempdir().unwrap();
    let (c, path) = carried_closed_fixture(d.path());
    let body = state_body(&c);
    let book: Portfolio = serde_json::from_str(&body).unwrap();
    let close = DateTime::parse_from_rfc3339("2026-10-16T16:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let session = DateTime::parse_from_rfc3339("2026-10-16T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let preopen = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    // The carried closed book is adopted read-only outside market hours.
    assert!(update_safe(&path, close).unwrap());
    assert!(update_safe(&path, preopen).unwrap());
    // In-session activation keeps the flat-book requirement.
    assert!(!update_safe(&path, session).unwrap());
    let mut p = book.clone();
    p.halted = true;
    rewrite_book(&c, &p);
    assert!(!update_safe(&path, close).unwrap());
    p = book.clone();
    p.closed = false;
    rewrite_book(&c, &p);
    assert!(!update_safe(&path, close).unwrap());
    p = book.clone();
    p.stopped = true;
    rewrite_book(&c, &p);
    assert!(!update_safe(&path, close).unwrap());
    p = book.clone();
    p.model_led = false;
    rewrite_book(&c, &p);
    assert!(!update_safe(&path, close).unwrap());
    p = book.clone();
    p.cash = p.cash.checked_add(nivra_core::execution::a("1")).unwrap();
    rewrite_book(&c, &p);
    assert!(!update_safe(&path, close).unwrap_or(false));
    // A flat closed book keeps the original in-session rule.
    let mut flat = crate::runtime::tests::test_portfolio();
    flat.model_led = true;
    flat.closed = true;
    rewrite_book(&c, &flat);
    assert!(update_safe(&path, session).unwrap());
    // An unsettled attempt defers even outside market hours.
    rewrite_book(&c, &book);
    let db = open_book(&c).unwrap();
    db.execute_batch("CREATE TABLE IF NOT EXISTS planner_attempt_log(id TEXT PRIMARY KEY,event_id TEXT NOT NULL UNIQUE,body TEXT NOT NULL,state TEXT NOT NULL,result TEXT);").unwrap();
    db.execute(
        "INSERT INTO planner_attempt_log VALUES('pending','event','{}','reserved',NULL)",
        [],
    )
    .unwrap();
    drop(db);
    assert!(!update_safe(&path, close).unwrap());
    // The probe is read-only: the bound state and identity are preserved.
    assert_eq!(state_body(&c), body);
    assert!(c.directory.join("book-identity.json").is_file());
}
#[test]
fn stopped_carry_acceptance_preserves_source_and_carries_once() {
    let d = tempfile::tempdir().unwrap();
    let (c, now) = stopped_carry_fixture(d.path());
    let preserved: Vec<_> = [
        "paper.sqlite",
        "report.json",
        "review.json",
        "book-identity.json",
    ]
    .iter()
    .map(|name| (*name, fs::read(c.directory.join(name)).unwrap()))
    .collect();
    let blocked = preview(&c, now).unwrap();
    assert_eq!(blocked["status"], "rollover_blocked");
    assert_eq!(blocked["review"]["model_led"], true);
    assert_eq!(blocked["review"]["carry_admitted"], false);
    assert!(accept_stopped_carry(&c, false, now).is_err());
    let receipt = accept_stopped_carry(&c, true, now).unwrap();
    assert_eq!(accept_stopped_carry(&c, true, now + 1).unwrap(), receipt);
    let receipt_bytes = fs::read(c.directory.join(STOPPED_CARRY_FILE)).unwrap();
    let plan = preview(&c, now).unwrap();
    assert_eq!(plan["status"], "rollover_ready");
    assert_eq!(plan["review"]["carried_positions"], 1);
    assert_eq!(plan["review"]["carry_admitted"], true);
    assert_eq!(plan["strategy_decision"]["carry_required"], true);
    record_review(&c).unwrap();
    assert_eq!(
        advance_offline(&c, now, true).unwrap()["status"],
        "advanced"
    );
    assert_eq!(
        advance_offline(&c, now, true).unwrap()["status"],
        "already_advanced"
    );
    let next = current(&c).unwrap();
    let carry: Value =
        serde_json::from_slice(&fs::read(next.directory.join("carry-over.json")).unwrap()).unwrap();
    verify_carry(&next, &carry).unwrap();
    let source: Portfolio = serde_json::from_str(&state_body(&c)).unwrap();
    let carried = wide::load_wide(&mut open_book(&next).unwrap(), &next).unwrap();
    assert_eq!(source.positions, carried.positions);
    assert_eq!(source.fills, carried.fills);
    assert_eq!(source.cash, carried.cash);
    assert!(!carried.stopped && !carried.halted && !carried.closed);
    for (name, bytes) in preserved {
        assert_eq!(fs::read(c.directory.join(name)).unwrap(), bytes, "{name}");
    }
    assert_eq!(
        fs::read(c.directory.join(STOPPED_CARRY_FILE)).unwrap(),
        receipt_bytes
    );
}

#[test]
fn stopped_flat_book_keeps_acknowledgment_and_legacy_review_reading() {
    let d = tempfile::tempdir().unwrap();
    let (c, now) = stopped_carry_fixture(d.path());
    let mut p = crate::runtime::tests::test_portfolio();
    p.stopped = true;
    p.closed = true;
    let db = open_book(&c).unwrap();
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&p).unwrap()],
    )
    .unwrap();
    fs::write(c.directory.join("report.json"), serde_json::to_vec(&json!({
            "paper_only":true,"live_enabled":false,"account":c.account,"session":c.session,"portfolio":p})).unwrap()).unwrap();
    record_review(&c).unwrap();
    assert!(accept_stopped_carry(&c, true, now).is_err());
    acknowledge(&c, true, now).unwrap();
    assert_eq!(preview(&c, now).unwrap()["status"], "rollover_ready");
    assert_eq!(
        decision(&c, &json!({"model_led_account":true}), c.session).unwrap()["carry_required"],
        true
    );
    assert_eq!(
        decision(
            &c,
            &json!({"model_led_account":true,"carry_admitted":false}),
            c.session
        )
        .unwrap()["carry_required"],
        false
    );
    offline_advance_refuses_unready_preview_without_mutation();
}

#[test]
fn stopped_carry_refuses_ineligible_or_changed_evidence() {
    for case in [
        "halted",
        "open",
        "not_stopped",
        "not_model_led",
        "strategy",
        "flat",
        "unreconciled",
        "pending_claim",
        "integrity_block",
        "authority_binding",
        "partial_receipt",
        "pending_attempt",
        "no_operator_stop",
        "live",
        "not_paper",
        "report_state",
        "report_hash",
        "state_hash",
        "accepted_report_changed",
        "accepted_state_changed",
        "accepted_attempt_changed",
        "busy",
    ] {
        let d = tempfile::tempdir().unwrap();
        let (mut c, now) = stopped_carry_fixture(d.path());
        if case.starts_with("accepted_") {
            accept_stopped_carry(&c, true, now).unwrap();
        }
        let db = open_book(&c).unwrap();
        let mut p: Portfolio = serde_json::from_str(&state_body(&c)).unwrap();
        let mut report: Value =
            serde_json::from_slice(&fs::read(c.directory.join("report.json")).unwrap()).unwrap();
        let mut seal: Value =
            serde_json::from_slice(&fs::read(c.directory.join("review.json")).unwrap()).unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(c.directory.join("controller.lock"))
            .ok()
            .map(AdvisoryLock::new);
        match case {
            "halted" => p.halted = true,
            "open" => p.closed = false,
            "not_stopped" => p.stopped = false,
            "not_model_led" => p.model_led = false,
            "strategy" => c.strategy = "model_abstain".into(),
            "flat" => {
                p = crate::runtime::tests::test_portfolio();
                p.stopped = true;
                p.closed = true;
            }
            "unreconciled" => p.positions.clear(),
            "pending_claim" => {
                let raw: String = db
                    .query_row(
                        "SELECT body FROM planner_event_control WHERE id=1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut control: Value = serde_json::from_str(&raw).unwrap();
                control["pending"] = json!({"id":"pending","event_id":"event","trigger":{},"binding":"fixture","portfolio_hash":"fixture","started_ms":now,"deadline_ms":now+1000,"predecessor":null,"previous_attempts":0});
                db.execute(
                    "UPDATE planner_event_control SET body=? WHERE id=1",
                    [control.to_string()],
                )
                .unwrap();
            }
            "authority_binding" => {
                let raw: String = db
                    .query_row(
                        "SELECT body FROM planner_event_control WHERE id=1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut control: Value = serde_json::from_str(&raw).unwrap();
                control["binding"] = json!("different-book");
                db.execute(
                    "UPDATE planner_event_control SET body=? WHERE id=1",
                    [control.to_string()],
                )
                .unwrap();
            }
            "partial_receipt" => {
                fs::write(c.directory.join(STOPPED_CARRY_FILE), b"{").unwrap();
                fs::set_permissions(
                    c.directory.join(STOPPED_CARRY_FILE),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            "integrity_block" => {
                let raw: String = db
                    .query_row(
                        "SELECT body FROM planner_event_control WHERE id=1",
                        [],
                        |r| r.get(0),
                    )
                    .unwrap();
                let mut control: Value = serde_json::from_str(&raw).unwrap();
                control["blocked"] = json!("planner_data_failed_no_replay");
                db.execute(
                    "UPDATE planner_event_control SET body=? WHERE id=1",
                    [control.to_string()],
                )
                .unwrap();
            }
            "pending_attempt" => {
                db.execute("INSERT INTO planner_attempt_log VALUES('pending','event','{}','reserved',NULL)", []).unwrap();
            }
            "accepted_attempt_changed" => {
                db.execute(
                    "INSERT INTO planner_attempt_log VALUES('settled','event','{}','failed','{}')",
                    [],
                )
                .unwrap();
            }
            "no_operator_stop" => {
                db.execute("DELETE FROM ticks WHERE event='operator_stopped'", [])
                    .unwrap();
            }
            "live" => report["live_enabled"] = json!(true),
            "not_paper" => report["paper_only"] = json!(false),
            "report_state" => report["portfolio"]["stopped"] = json!(false),
            "report_hash" => seal["report_hash"] = json!("stale"),
            "state_hash" => seal["state_hash"] = json!("stale"),
            "accepted_report_changed" => report["extra"] = json!(true),
            "accepted_state_changed" => p.health = "changed".into(),
            "busy" => {
                lock.as_ref().unwrap().try_lock_exclusive().unwrap();
            }
            _ => unreachable!(),
        }
        db.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
        fs::write(
            c.directory.join("report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        fs::write(
            c.directory.join("review.json"),
            serde_json::to_vec(&seal).unwrap(),
        )
        .unwrap();
        let before = fs::read(c.directory.join("paper.sqlite")).unwrap();
        assert!(accept_stopped_carry(&c, true, now).is_err(), "{case}");
        if case.starts_with("accepted_") {
            assert!(preview(&c, now).is_err(), "{case}");
        } else if case == "partial_receipt" {
            assert_eq!(
                fs::read(c.directory.join(STOPPED_CARRY_FILE)).unwrap(),
                b"{"
            );
        } else {
            assert!(!c.directory.join(STOPPED_CARRY_FILE).exists(), "{case}");
        }
        assert_eq!(
            fs::read(c.directory.join("paper.sqlite")).unwrap(),
            before,
            "{case}"
        );
    }
    // The halted carry has its own reviewed receipt; the same ineligible and
    // changed-evidence shapes refuse and never create the acceptance file.
    for case in [
        "not_model_led",
        "not_closed",
        "not_halted",
        "stopped",
        "flat",
        "strategy",
        "pending_attempt",
        "partial_receipt",
        "state_hash",
        "report_state",
        "accepted_state_changed",
        "accepted_attempt_changed",
        "busy",
    ] {
        let d = tempfile::tempdir().unwrap();
        let (mut c, at, _cfg) = halted_carry_fixture(d.path());
        if case.starts_with("accepted_") {
            accept_halted_carry(&c, true, at).unwrap();
        }
        let db = open_book(&c).unwrap();
        let mut p: Portfolio = serde_json::from_str(&state_body(&c)).unwrap();
        let mut report: Value =
            serde_json::from_slice(&fs::read(c.directory.join("report.json")).unwrap()).unwrap();
        let mut seal: Value =
            serde_json::from_slice(&fs::read(c.directory.join("review.json")).unwrap()).unwrap();
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(c.directory.join("controller.lock"))
            .ok()
            .map(AdvisoryLock::new);
        match case {
            "not_model_led" => p.model_led = false,
            "not_closed" => p.closed = false,
            "not_halted" => p.halted = false,
            "stopped" => p.stopped = true,
            "flat" => {
                p = crate::runtime::tests::test_portfolio();
                p.model_led = true;
                p.halted = true;
                p.closed = true;
                p.health = "risk_halted".into();
            }
            "strategy" => c.strategy = "model_abstain".into(),
            "pending_attempt" => {
                db.execute("INSERT INTO planner_attempt_log VALUES('pending','event','{}','reserved',NULL)", []).unwrap();
            }
            "partial_receipt" => {
                fs::write(c.directory.join(HALTED_CARRY_FILE), b"{").unwrap();
                fs::set_permissions(
                    c.directory.join(HALTED_CARRY_FILE),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            "state_hash" => seal["state_hash"] = json!("stale"),
            "report_state" => report["portfolio"]["stopped"] = json!(true),
            "accepted_state_changed" => p.health = "changed".into(),
            "accepted_attempt_changed" => {
                db.execute(
                    "INSERT INTO planner_attempt_log VALUES('settled','event','{}','failed','{}')",
                    [],
                )
                .unwrap();
            }
            "busy" => {
                lock.as_ref().unwrap().try_lock_exclusive().unwrap();
            }
            _ => unreachable!(),
        }
        db.execute(
            "UPDATE state SET body=? WHERE id=1",
            [serde_json::to_string(&p).unwrap()],
        )
        .unwrap();
        fs::write(
            c.directory.join("report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        fs::write(
            c.directory.join("review.json"),
            serde_json::to_vec(&seal).unwrap(),
        )
        .unwrap();
        let before = fs::read(c.directory.join("paper.sqlite")).unwrap();
        assert!(accept_halted_carry(&c, true, at).is_err(), "{case}");
        if case.starts_with("accepted_") {
            let next_day = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
                .unwrap()
                .timestamp_millis();
            assert!(preview(&c, next_day).is_err(), "{case}");
        } else if case == "partial_receipt" {
            assert_eq!(fs::read(c.directory.join(HALTED_CARRY_FILE)).unwrap(), b"{");
        } else {
            assert!(!c.directory.join(HALTED_CARRY_FILE).exists(), "{case}");
        }
        assert_eq!(
            fs::read(c.directory.join("paper.sqlite")).unwrap(),
            before,
            "{case}"
        );
        drop(db);
    }
}
#[test]
fn offline_advance_requires_explicit_confirmation() {
    let d = tempfile::tempdir().unwrap();
    let (old, changed) = cost_transition_fixture(d.path(), true);
    let now = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let source = fs::read(old.directory.join("paper.sqlite")).unwrap();
    let error = advance_offline(&changed, now, false).unwrap_err();
    assert_eq!(
        failure_code(error.as_ref()),
        "rollover_advance_confirm_paper_review_required"
    );
    assert!(!root(&changed).unwrap().join("current.json").exists());
    assert_eq!(
        fs::read(old.directory.join("paper.sqlite")).unwrap(),
        source
    );
}
fn offline_advance_refuses_unready_preview_without_mutation() {
    let d = tempfile::tempdir().unwrap();
    let (old, _changed) = cost_transition_fixture(d.path(), false);
    let now = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let source = fs::read(old.directory.join("paper.sqlite")).unwrap();
    let anchor = fs::read(old.directory.join("book-identity.json")).unwrap();
    let report = fs::read(old.directory.join("report.json")).unwrap();
    assert_eq!(preview(&old, now).unwrap()["status"], "rollover_blocked");
    let error = advance_offline(&old, now, true).unwrap_err();
    assert_eq!(failure_code(error.as_ref()), "rollover_advance_not_ready");
    assert!(!root(&old).unwrap().join("current.json").exists());
    assert!(!root(&old).unwrap().join("2026-10-19").exists());
    assert!(!old.directory.join("review.json").exists());
    assert_eq!(
        fs::read(old.directory.join("paper.sqlite")).unwrap(),
        source
    );
    assert_eq!(
        fs::read(old.directory.join("book-identity.json")).unwrap(),
        anchor
    );
    assert_eq!(fs::read(old.directory.join("report.json")).unwrap(), report);
}
#[test]
fn offline_advance_binds_successor_and_preserves_source_evidence() {
    let d = tempfile::tempdir().unwrap();
    let (old, changed) = cost_transition_fixture(d.path(), true);
    let now = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let source_binding = book_binding(&old).unwrap();
    let source_body = state_body(&old);
    let source_fills = serde_json::from_str::<Portfolio>(&source_body)
        .unwrap()
        .fills;
    let source = fs::read(old.directory.join("paper.sqlite")).unwrap();
    let anchor = fs::read(old.directory.join("book-identity.json")).unwrap();
    let out = advance_offline(&changed, now, true).unwrap();
    assert_eq!(out["status"], "advanced");
    assert_eq!(out["preview_status"], "rollover_ready");
    assert_eq!(out["source_session"], "2026-10-16");
    assert_eq!(out["target_session"], "2026-10-19");
    assert_eq!(out["migration"]["kind"], "explicit_paper_cost_transition");
    assert_eq!(out["migration_kind"], "explicit_paper_cost_transition");
    assert_eq!(out["live_enabled"], false);
    assert_eq!(out["source_binding"], source_binding);
    let next = current(&changed).unwrap();
    assert_eq!(next.session.to_string(), "2026-10-19");
    assert_eq!(out["target_binding"], book_binding(&next).unwrap());
    let mut next_db = open_book(&next).unwrap();
    let carried = wide::load_wide(&mut next_db, &next).unwrap();
    assert_eq!(carried.costs, changed.paper_costs);
    assert_eq!(carried.fills, source_fills);
    assert_ne!(book_binding(&next).unwrap(), source_binding);
    assert_eq!(book_binding(&changed).unwrap(), source_binding);
    assert_eq!(state_body(&old), source_body);
    assert_eq!(
        fs::read(old.directory.join("paper.sqlite")).unwrap(),
        source
    );
    assert_eq!(
        fs::read(old.directory.join("book-identity.json")).unwrap(),
        anchor
    );
}
#[test]
fn offline_advance_resolves_pointer_source_and_stays_idempotent() {
    let d = tempfile::tempdir().unwrap();
    let (old, changed) = cost_transition_fixture(d.path(), true);
    let day_one = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let day_two = DateTime::parse_from_rfc3339("2026-10-21T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    let source_body = state_body(&old);
    let source = fs::read(old.directory.join("paper.sqlite")).unwrap();
    let out = advance_offline(&changed, day_one, true).unwrap();
    assert_eq!(out["status"], "advanced");
    assert_eq!(out["source_session"], "2026-10-16");
    assert_eq!(out["target_session"], "2026-10-19");
    let pointer = fs::read(root(&changed).unwrap().join("current.json")).unwrap();
    let successor = current(&changed).unwrap();
    assert_eq!(successor.session.to_string(), "2026-10-19");
    assert_eq!(changed.session.to_string(), "2026-10-16");

    // The config still names the previous session while the pointer is at
    // the next day. That requested target is already committed, so the
    // command must not replay the stale config transition.
    let repeated = advance_offline(&changed, day_one, true).unwrap();
    assert_eq!(repeated["status"], "already_advanced");
    assert_eq!(repeated["source_session"], "2026-10-16");
    assert_eq!(repeated["target_session"], "2026-10-19");
    assert_eq!(repeated["migration_kind"], "explicit_paper_cost_transition");
    assert_eq!(
        fs::read(root(&changed).unwrap().join("current.json")).unwrap(),
        pointer
    );
    assert!(!root(&changed).unwrap().join("2026-10-20").exists());

    // Close and seal the successor, then advance again. The source is the
    // pointer session, not the stale config session.
    let mut successor_db = open_book(&successor).unwrap();
    let mut p = wide::load_wide(&mut successor_db, &successor).unwrap();
    p.closed = true;
    p.health = "closed".into();
    wide::persist(
        &mut successor_db,
        &successor,
        &p,
        day_one + 6 * 3_600_000,
        None,
        None,
        "session_closed",
    )
    .unwrap();
    fs::write(
        successor.directory.join("report.json"),
        serde_json::to_vec(&json!({
            "paper_only":true,
            "live_enabled":false,
            "account":successor.account,
            "session":successor.session,
            "portfolio":p,
        }))
        .unwrap(),
    )
    .unwrap();
    drop(successor_db);
    record_review(&successor).unwrap();
    let successor_state = fs::read(successor.directory.join("paper.sqlite")).unwrap();

    let out = advance_offline(&changed, day_two, true).unwrap();
    assert_eq!(out["status"], "advanced");
    assert_eq!(out["source_session"], "2026-10-19");
    assert_eq!(out["target_session"], "2026-10-21");
    assert_eq!(out["source_binding"], book_binding(&successor).unwrap());
    let next = current(&changed).unwrap();
    assert_eq!(next.session.to_string(), "2026-10-21");
    assert_eq!(out["target_binding"], book_binding(&next).unwrap());
    assert_eq!(
        fs::read(successor.directory.join("paper.sqlite")).unwrap(),
        successor_state
    );

    let pointer = fs::read(root(&changed).unwrap().join("current.json")).unwrap();
    let repeated = advance_offline(&changed, day_two, true).unwrap();
    assert_eq!(repeated["status"], "already_advanced");
    assert_eq!(repeated["target_session"], "2026-10-21");
    assert_eq!(
        fs::read(root(&changed).unwrap().join("current.json")).unwrap(),
        pointer
    );
    assert!(!root(&changed).unwrap().join("2026-10-22").exists());
    assert_eq!(state_body(&old), source_body);
    assert_eq!(
        fs::read(old.directory.join("paper.sqlite")).unwrap(),
        source
    );
}
#[test]
fn explicit_cost_transition_carries_closed_book_without_rebinding_source() {
    use nivra_core::{
        execution::{ExitIntent, Instrument, Quote},
        finance::Price,
    };
    for legacy in [false, true] {
        let d = tempfile::tempdir().unwrap();
        let mut old = super::super::tests::config(d.path());
        old.sessions_root = Some(d.path().join("sessions"));
        old.directory = old
            .sessions_root
            .as_ref()
            .unwrap()
            .join(old.session.to_string());
        fs::create_dir_all(&old.directory).unwrap();
        fs::write(&old.read_journal, b"fixture").unwrap();
        let metadata = fs::metadata(&old.read_journal).unwrap();
        old.read_journal_device = metadata.dev();
        old.read_journal_inode = metadata.ino();
        if legacy {
            old.paper_costs = None;
        }
        let db = open_book(&old).unwrap();
        let mut p = crate::runtime::tests::test_portfolio();
        let i = Instrument {
            key: "NSE_EQ|SYNTHETIC".into(),
            symbol: "SYNTHETIC".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        };
        let at = DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
            .unwrap()
            .timestamp_millis();
        let q = Quote {
            bid: Price::parse("100").unwrap(),
            ask: Price::parse("100.05").unwrap(),
            bid_units: 100,
            ask_units: 100,
            source: at,
            receipt: at,
        };
        assert!(
            p.execute_plan(
                at,
                &i,
                &q,
                true,
                2,
                Price::parse("101").unwrap(),
                ExitIntent::default(),
                "fixture"
            )
            .unwrap()
        );
        if legacy {
            p.costs = None;
            for fill in &mut p.fills {
                fill.costs = None;
            }
        }
        p.closed = true;
        p.reconcile().unwrap();
        db.execute(
            "INSERT INTO state VALUES(1,?,?)",
            rusqlite::params![
                book_binding(&old).unwrap(),
                serde_json::to_string(&p).unwrap()
            ],
        )
        .unwrap();
        fs::write(
            old.directory.join("report.json"),
            serde_json::to_vec(&json!({"session":old.session,"portfolio":p})).unwrap(),
        )
        .unwrap();
        drop(db);
        let prior = d.path().join("prior-paper.json");
        fs::write(&prior, serde_json::to_vec(&old).unwrap()).unwrap();
        let original = fs::read(old.directory.join("paper.sqlite")).unwrap();
        let anchor = fs::read(old.directory.join("book-identity.json")).unwrap();
        let mut changed = old.clone();
        changed.paper_costs = Some(
            serde_json::from_str(include_str!("../../../../../fixtures/paper-costs.json")).unwrap(),
        );
        changed.paper_cost_transition = Some(super::super::PaperCostTransition {
            source_session: old.session,
            prior_config: prior,
        });
        let monday = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            wide::load_wide(&mut open_book(&changed).unwrap(), &changed)
                .unwrap_err()
                .to_string(),
            "paper_cost_policy_changed"
        );
        let plan = preview(&changed, monday).unwrap();
        assert_eq!(plan["status"], "rollover_ready");
        assert_eq!(
            plan["strategy_decision"]["migration"]["kind"],
            "explicit_paper_cost_transition"
        );
        assert_eq!(
            plan["strategy_decision"]["migration"]["original_state_preserved"],
            true
        );
        let mut unrelated = changed.clone();
        unrelated.capital = Some(nivra_core::execution::a("7000"));
        assert_eq!(
            original_config(&unrelated).err().unwrap().to_string(),
            "paper_cost_transition_not_cost_only"
        );
        let next = advance(&changed, monday).unwrap().unwrap();
        let carried = wide::load_wide(&mut open_book(&next).unwrap(), &next).unwrap();
        assert_eq!(carried.costs, changed.paper_costs);
        assert_eq!(carried.cash, p.cash);
        assert_eq!(carried.positions, p.positions);
        assert_eq!(carried.fills, p.fills);
        assert_eq!(
            fs::read(old.directory.join("paper.sqlite")).unwrap(),
            original
        );
        assert_eq!(
            fs::read(old.directory.join("book-identity.json")).unwrap(),
            anchor
        );
        assert!(current(&changed).unwrap().validate().is_ok());
    }
}
#[test]
fn overnight_rollover_carries_account_and_preserves_original_and_terminal_books() {
    closed_incident::check_boundaries();
    use nivra_core::{
        execution::{ExitIntent, Instrument, Quote},
        finance::Price,
    };
    let d = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(d.path());
    c.sessions_root = Some(d.path().join("sessions"));
    c.directory = c
        .sessions_root
        .as_ref()
        .unwrap()
        .join(c.session.to_string());
    fs::create_dir_all(&c.directory).unwrap();
    let db = open_book(&c).unwrap();
    let mut p = crate::runtime::tests::test_portfolio();
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let at = DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis();
    let q = Quote {
        bid: Price::parse("100").unwrap(),
        ask: Price::parse("100.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    assert!(
        p.execute_plan(
            at,
            &i,
            &q,
            true,
            2,
            Price::parse("101").unwrap(),
            ExitIntent::default(),
            "overnight-fixture"
        )
        .unwrap()
    );
    p.closed = true;
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![
            book_binding(&c).unwrap(),
            serde_json::to_string(&p).unwrap()
        ],
    )
    .unwrap();
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&json!({"session":c.session,"portfolio":p})).unwrap(),
    )
    .unwrap();
    drop(db);
    let original = fs::read(c.directory.join("paper.sqlite")).unwrap();
    // A controller first up at 10:30 IST still adopts the day's session and
    // carries its holdings under protection.
    let monday = DateTime::parse_from_rfc3339("2026-10-19T10:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    assert_eq!(review(&c).unwrap()["carried_positions"], 1);
    let next = advance(&c, monday).unwrap().unwrap();
    assert_eq!(next.session, "2026-10-19".parse().unwrap());
    // In-process rollover used to leave the successor without a scanner
    // journal; the first open-market checkpoint then crashed the controller.
    let next_db = open_book(&next).unwrap();
    wide::checkpoint_scanner(&next_db, &nivra_core::execution::Scanner::default()).unwrap();
    let scanner_present: bool = next_db
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scanner')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(scanner_present);
    drop(next_db);
    let carried = wide::load_wide(&mut open_book(&next).unwrap(), &next).unwrap();
    assert_eq!(carried.cash, p.cash);
    assert_eq!(carried.positions, p.positions);
    assert_eq!(carried.fills, p.fills);
    assert!(!carried.closed);
    let carry_path = next.directory.join("carry-over.json");
    let carry_bytes = fs::read(&carry_path).unwrap();
    fs::remove_file(&carry_path).unwrap();
    assert!(wide::load_wide(&mut open_book(&next).unwrap(), &next).is_err());
    fs::write(&carry_path, carry_bytes).unwrap();

    assert_eq!(
        fs::read(c.directory.join("paper.sqlite")).unwrap(),
        original
    );
    let db = open_book(&c).unwrap();
    p.stopped = true;
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&p).unwrap()],
    )
    .unwrap();
    assert_eq!(
        review(&c).unwrap()["status"],
        "unresolved_positions_human_required"
    );
    assert!(
        verify_carry(
            &next,
            &serde_json::from_slice(&fs::read(next.directory.join("carry-over.json")).unwrap())
                .unwrap()
        )
        .is_err()
    );
}
