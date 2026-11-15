//! Explicit future-session disposition. The original halted book is never rewritten.
use super::orchestrator_model::Trigger;
use super::phases::SealVerdict;
use super::*;
use sadr_cli::lock::AdvisoryLock;
use sadr_core::execution::Portfolio;
use serde_json::{Value, json};
use std::io::Write;

pub(super) const REASON: &str = "operator_disposed_closed_zero_fill_planner_incident";
const FILE: &str = "closed-planner-disposition.json";

fn next_day(c: &Config) -> Result<NaiveDate> {
    Ok(
        c.next_sessions(c.session.succ_opt().ok_or("disposition_date_range")?)?
            .0,
    )
}
// The report timestamp is the durable close boundary, authenticated by review.json.
fn sealed(c: &Config, p: &Portfolio) -> Result<(Value, Value, Vec<u8>, Vec<u8>)> {
    let report = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| "disposition_report_unreadable")?;
    let review = crate::read_bounded(&c.directory.join("review.json"), 1_048_576)
        .map_err(|_| "disposition_review_unreadable")?;
    let r: Value = sadr_core::native::decode(&report)?;
    let seal: Value = sadr_core::native::decode(&review)?;
    let reported: Portfolio = serde_json::from_value(r["portfolio"].clone())?;
    reported.reconcile()?;
    if LIVE_ENABLED
        || r["account"] != c.account
        || r["session"] != c.session.to_string()
        || r["paper_only"] != true
        || r["live_enabled"] != false
        || !reported.closed
        || seal["session"] != c.session.to_string()
        || seal["closed"] != true
        || seal["halted"] != reported.halted
        || seal["open_unresolved"] != 0
        || seal["report_hash"] != digest(&report)
        || seal["state_hash"] != digest(serde_json::to_string(&reported)?.as_bytes())
    {
        return Err("disposition_report_seal_mismatch".into());
    }
    let mut comparable = p.clone();
    comparable.health.clone_from(&reported.health);
    comparable.stopped = reported.stopped;
    comparable.halted = reported.halted;
    if comparable != reported || !reported.positions.is_empty() {
        return Err("disposition_financial_state_changed".into());
    }
    Ok((r, seal, report, review))
}
pub(super) fn closed_sealed(c: &Config, p: &Portfolio) -> bool {
    p.closed && sealed(c, p).is_ok()
}
fn evidence(c: &Config, db: &Connection, body: &str) -> Result<Value> {
    let p: Portfolio = serde_json::from_str(body)?;
    p.reconcile()?;
    // Disposition authorizes only a successor; the halted source remains immutable.
    if LIVE_ENABLED
        || c.strategy != "model_planner_v1"
        || !p.closed
        || !p.halted
        || !p.positions.is_empty()
    {
        return Err("disposition_requires_closed_flat_zero_fill_planner_book".into());
    }
    let mut rows = db.prepare("SELECT event FROM ticks WHERE event LIKE '%halt%' OR event LIKE '%tripwire%' OR event LIKE '%corrupt%' OR event LIKE '%authority_invalid%'")?;
    let events = rows
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !events.iter().any(|v| v == "planner_authority_invalid")
        || events.iter().any(|v| v != "planner_authority_invalid")
    {
        return Err("disposition_non_planner_incident".into());
    }
    let (r, _, report, review) = sealed(c, &p)?;
    let reported: Portfolio = serde_json::from_value(r["portfolio"].clone())?;
    let planner = if reported.halted {
        // Preserve the original, narrowly scoped legacy disposition contract.
        if !p.stopped || !p.fills.is_empty() || p.cash != p.allocation()? || p.high_water != p.cash
        {
            return Err("disposition_requires_closed_flat_zero_fill_planner_book".into());
        }
        redecision::closed_incident_evidence(db, c)?
    } else {
        let at = r["as_of_ms"]
            .as_i64()
            .ok_or("disposition_seal_time_missing")?;
        if local(at).date_naive() != c.session {
            return Err("disposition_seal_date_invalid".into());
        }
        let invalid: i64 = db.query_row("SELECT count(*) FROM ticks WHERE (event LIKE '%halt%' OR event LIKE '%tripwire%' OR event LIKE '%corrupt%' OR event LIKE '%authority_invalid%') AND (event != 'planner_authority_invalid' OR at <= ?)", [at], |r| r.get(0))?;
        let history: Vec<(i64, String, String)> = db
            .prepare("SELECT at,reason,source FROM paper_halt_history ORDER BY id")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        if invalid != 0
            || history.is_empty()
            || history.iter().any(|(time, reason, source)| {
                *time <= at
                    || reason != "planner_authority_invalid_recovery_required"
                    || source != "state_transition"
            })
        {
            return Err("disposition_non_post_seal_planner_incident".into());
        }
        redecision::post_seal_incident_evidence(db)?
    };
    let next_policy = planner::PlannerConfig::load(
        c.planner_config
            .as_deref()
            .ok_or("disposition_next_planner_required")?,
    )?;
    let decide_role = next_policy.role(Trigger::Decide);
    if !next_policy.enabled || !decide_role.enabled {
        return Err("disposition_next_planner_disabled".into());
    }
    let usage = planner::usage(c)?; // Validates the original reservation namespace and bytes.
    Ok(
        json!({"version":"closed-zero-fill-planner-disposition-v1","binding":book_binding(c)?,
        "source_session":c.session,"target_session":next_day(c)?,"next_planner_policy_sha256":digest(&planner::policy_bytes(&next_policy)?),"state_sha256":digest(body.as_bytes()),
        "identity_sha256":digest(&crate::read_bounded(&c.directory.join("book-identity.json"),16384).map_err(|_| "disposition_identity_unreadable")?),
        "report_sha256":digest(&report),"review_sha256":digest(&review),"planner":planner,
        "usage_sha256":digest(&serde_json::to_vec(&usage)?),
        "usage_identity_sha256":digest(&crate::read_bounded(&c.directory.join("model/usage-identity.json"),16384).map_err(|_| "disposition_usage_identity_missing")?),
        "usage_manifest_sha256":digest(&crate::read_bounded(&c.directory.join("model/usage-reservations.json"),1_000_000).map_err(|_| "disposition_usage_manifest_missing")?),"usage":usage,"live_enabled":false}),
    )
}
/// None means no mandate. An invalid existing mandate fails closed rather than disappearing.
pub(super) fn approved(c: &Config, db: &Connection, body: &str) -> Result<Option<Value>> {
    let path = c.directory.join(FILE);
    if !path.try_exists()? {
        return Ok(None);
    }
    let raw = private(&path)?;
    let value: Value = sadr_core::native::decode(raw.as_bytes())?;
    let expected = evidence(c, db, body)?;
    // Accept a pre-upgrade seal only when its policy equals the exact current
    // configuration under the old serialization; never rewrite the seal.
    let mut legacy_expected = expected.clone();
    let next_policy = planner::PlannerConfig::load(
        c.planner_config
            .as_deref()
            .ok_or("disposition_next_planner_required")?,
    )?;
    legacy_expected["next_planner_policy_sha256"] =
        json!(digest(&planner::legacy_policy_bytes(&next_policy)?));
    if value["reason"] != REASON
        || (value["evidence"] != expected && value["evidence"] != legacy_expected)
        || value["approved_at_ms"].as_i64().is_none()
        || value["approved_at_ms"].as_i64().is_some_and(|at| {
            let day = local(at).date_naive();
            day < c.session
                || day > next_day(c).unwrap_or(c.session)
                || (day == c.session && minute(at) <= 945)
                || (day == next_day(c).unwrap_or(c.session) && minute(at) >= 555)
        })
    {
        return Err("disposition_binding_changed".into());
    }
    Ok(Some(value))
}
pub(super) fn accepted_review(c: &Config, db: &Connection, body: &str) -> Result<Option<Value>> {
    let Some(value) = approved(c, db, body)? else {
        return Ok(None);
    };
    Ok(Some(
        json!({"status":SealVerdict::EligibleToSeal.as_str(),"reason":REASON,"session":c.session,
        "target_session":value["evidence"]["target_session"],"state_hash":digest(body.as_bytes()),
        "report_hash":value["evidence"]["report_sha256"],"disposition_hash":digest(&serde_json::to_vec(&value)?),
        "closed":true,"halted":true,"stopped":serde_json::from_str::<Portfolio>(body)?.stopped,"open_unresolved":0,"carried_positions":0,
        "model_led":serde_json::from_str::<Portfolio>(body)?.model_led,"carry_admitted":true,"original_incident_preserved":true,"live_enabled":false}),
    ))
}
pub(super) fn successor(
    c: &Config,
    db: &Connection,
    body: &str,
    target: NaiveDate,
) -> Result<Portfolio> {
    let value = approved(c, db, body)?.ok_or("disposition_confirmation_missing")?;
    if value["evidence"]["target_session"] != target.to_string() {
        return Err("disposition_target_session_mismatch".into());
    }
    let mut p: Portfolio = serde_json::from_str(body)?;
    // Clear flags only on a copy for the explicitly authorized future book.
    p.closed = false;
    p.halted = false;
    p.stopped = false;
    p.model_led = true;
    p.health = "carried_waiting_for_fresh_quotes".into();
    Ok(p)
}
fn confirm(c: &Config, target: NaiveDate, confirmed: bool, now: i64) -> Result<Value> {
    if !confirmed {
        return Err("disposition_confirm_paper_review_required".into());
    }
    c.validate()?;
    let _root_lock = rollover::controller_lock(c)?;
    let c = rollover::current(c)?;
    if target != next_day(&c)?
        || local(now).date_naive() < c.session
        || local(now).date_naive() > target
        || (local(now).date_naive() == c.session && minute(now) <= 945)
        || (local(now).date_naive() == target && minute(now) >= 555)
    {
        return Err("disposition_target_or_maintenance_window_invalid".into());
    }
    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(c.directory.join("controller.lock"))?;
    lock.try_lock_exclusive()
        .map_err(|_| "disposition_controller_busy")?;
    let _lock = AdvisoryLock::new(lock);
    let (db, body) = rollover::bound_state(&c)?;
    if let Some(prior) = approved(&c, &db, &body)? {
        return Ok(prior);
    }
    let value = json!({"reason":REASON,"approved_at_ms":now,"evidence":evidence(&c,&db,&body)?});
    let path = c.directory.join(FILE);
    // A partial/failed receipt is evidence, never overwritten or auto-retried.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    f.write_all(&serde_json::to_vec_pretty(&value)?)?;
    f.sync_all()?;
    fs::File::open(&c.directory)?.sync_all()?;
    Ok(value)
}
// Only application reason codes may reach stderr; provider/OS errors can contain paths.
fn refusal_code(code: &str) -> String {
    if code.len() <= 128
        && (code.starts_with("disposition_") || code.starts_with("planner_"))
        && code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        code.into()
    } else {
        "disposition_evidence_unreadable_or_invalid".into()
    }
}
pub fn entry(path: &Path, target: NaiveDate, confirmed: bool) -> ExitCode {
    let result = (|| -> Result<Value> {
        let c: Config = serde_json::from_slice(&fs::read(path)?)?;
        confirm(&c, target, confirmed, millis())
    })();
    match result {
        Ok(_) => {
            println!(
                "{}",
                json!({"status":"closed_incident_disposition_recorded","target_session":target,"original_halt_preserved":true,"live_enabled":false})
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("closed_incident_disposition_refused_original_book_preserved");
            eprintln!("{}", refusal_code(&error.to_string()));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
pub(super) fn check_boundaries() {
    use sadr_core::finance::Amount;
    assert_eq!(
        refusal_code("disposition_pending_attempt"),
        "disposition_pending_attempt"
    );
    assert_eq!(
        refusal_code("/private/fixture: secret"),
        "disposition_evidence_unreadable_or_invalid"
    );
    struct Failed;
    impl planner::Backend for Failed {
        fn call(&mut self, _: &planner::ModelRole, _: &str) -> Result<String> {
            Err("synthetic_consumed_failure".into())
        }
    }
    fn fixture() -> (tempfile::TempDir, Config, i64, NaiveDate) {
        let d = tempfile::tempdir().unwrap();
        let mut c = super::tests::config(d.path());
        c.sessions_root = Some(d.path().join("sessions"));
        let cfg: planner::PlannerConfig = serde_json::from_value(json!({"enabled":true,"decide":{"enabled":true,"model_id":"fixture-model","daily_call_budget":8},"events":{"enabled":true,"cooldown_ms":0,"max_pending":10,"policy":{"price_move_bps":100,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}}})).unwrap();
        let config_path = d.path().join("planner.json");
        fs::write(&config_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        c.planner_config = Some(config_path);
        let now = DateTime::parse_from_rfc3339("2026-10-16T16:00:00+05:30")
            .unwrap()
            .timestamp_millis();
        let mut db = open_book(&c).unwrap();
        let mut p = Portfolio::with_capital(c.allocation().unwrap());
        redecision::initialize(&mut db, &c, &p, Some(&cfg), now - 60_000).unwrap();
        redecision::block(&mut db, &c, "planner_event_configuration_changed").unwrap();
        assert!(
            planner::call_guarded(
                &c,
                &cfg,
                "decide",
                &cfg.decide,
                "fixture",
                &mut Failed,
                &planner::preview_guard()
            )
            .is_err()
        );
        p.closed = true;
        p.stopped = true;
        p.halted = true;
        p.health = "closed".into();
        db.execute(
            "INSERT INTO state VALUES(1,?,?)",
            rusqlite::params![
                book_binding(&c).unwrap(),
                serde_json::to_string(&p).unwrap()
            ],
        )
        .unwrap();
        db.execute("INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,NULL,'planner_authority_invalid')",[now]).unwrap();
        fs::write(c.directory.join("report.json"),serde_json::to_vec(&json!({"paper_only":true,"live_enabled":false,"account":c.account,"session":c.session,"portfolio":p})).unwrap()).unwrap();
        drop(db);
        rollover::record_review(&c).unwrap();
        let target = next_day(&c).unwrap();
        (d, c, now, target)
    }
    // A legacy shadow-policy receipt stays readable only for the exact current
    // configuration, and checking it never rewrites the original receipt.
    {
        let (_d, c, now, target) = fixture();
        let path = c.planner_config.as_ref().unwrap();
        let mut cfg = planner::PlannerConfig::load(path).unwrap();
        cfg.digest.enabled = true;
        cfg.digest.model_id = "fixture-model".into();
        cfg.digest.daily_call_budget = 8;
        cfg.full_universe_features = Some(true);
        cfg.discovery = Some(serde_json::from_value(json!({"max_scan_age_ms":180000,"max_prospects":40,"strategy_selection":{"enabled":true,"library_path":"/shadow/library"}})).unwrap());
        fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        let mut receipt = confirm(&c, target, true, now).unwrap();
        receipt["evidence"]["next_planner_policy_sha256"] =
            json!(digest(&planner::legacy_policy_bytes(&cfg).unwrap()));
        let saved = serde_json::to_vec(&receipt).unwrap();
        fs::write(c.directory.join(FILE), &saved).unwrap();
        let (db, body) = rollover::bound_state(&c).unwrap();
        assert!(approved(&c, &db, &body).unwrap().is_some());
        assert_eq!(fs::read(c.directory.join(FILE)).unwrap(), saved);
        cfg.discovery.as_mut().unwrap().max_prospects += 1;
        fs::write(path, serde_json::to_vec(&cfg).unwrap()).unwrap();
        assert!(approved(&c, &db, &body).is_err());
    }
    // Post-seal disposition accepts a reconciled loss without rewriting any source bytes.
    for with_fills in [false, true] {
        for refusal in 0..15 {
            let (_d, c, now, target) = fixture();
            let db = Connection::open(c.directory.join("paper.sqlite")).unwrap();
            let mut p = Portfolio::with_capital(c.allocation().unwrap());
            if with_fills {
                use sadr_core::{
                    execution::{Fill, Instrument},
                    finance::{Money, Price, Side},
                };
                let instrument = Instrument {
                    key: "fixture".into(),
                    symbol: "FIXTURE".into(),
                    series: "EQ".into(),
                    tick: Price::parse("0.05").unwrap(),
                };
                for (buy, price) in [(true, "100"), (false, "90")] {
                    let price = Price::parse(price).unwrap();
                    let fee = super::tests::test_costs()
                        .charge(
                            if buy { Side::Buy } else { Side::Sell },
                            Money::try_from(price.amount()).unwrap(),
                        )
                        .unwrap()
                        .amount();
                    p.cash = if buy {
                        p.cash
                            .checked_sub(price.amount().checked_add(fee).unwrap())
                            .unwrap()
                    } else {
                        p.cash
                            .checked_add(price.amount().checked_sub(fee).unwrap())
                            .unwrap()
                    };
                    p.fills.push(Fill {
                        at: now - 120_000,
                        instrument: instrument.clone(),
                        units: 1,
                        buy,
                        price,
                        fee,
                        expected_price: None,
                        slippage_bps: None,
                        decision_id: None,
                        costs: None,
                    });
                }
                assert!(p.cash < p.allocation().unwrap());
            }
            p.closed = true;
            p.health = "closed".into();
            p.reconcile().unwrap();
            let sealed_body = serde_json::to_string(&p).unwrap();
            let report = serde_json::to_vec(&json!({"paper_only":true,"live_enabled":false,"account":c.account,"session":c.session,"as_of_ms":now-60_000,"portfolio":p})).unwrap();
            fs::write(c.directory.join("report.json"), &report).unwrap();
            fs::write(c.directory.join("review.json"), serde_json::to_vec(&json!({"session":c.session,"closed":true,"halted":false,"open_unresolved":0,"state_hash":digest(sealed_body.as_bytes()),"report_hash":digest(&report)})).unwrap()).unwrap();
            assert!(closed_sealed(&c, &p));
            let cfg = planner::PlannerConfig::load(c.planner_config.as_ref().unwrap()).unwrap();
            let mut init_db = Connection::open(c.directory.join("paper.sqlite")).unwrap();
            assert!(!redecision::initialize(&mut init_db, &c, &p, Some(&cfg), now).unwrap());
            // Real incident shape: deployed policy changed the binding; close left
            // a stale active id, no block and no pending claim. History exceeds
            // the legacy 16 MB limit, with the observed 23/30/21 state counts.
            let raw: String = db
                .query_row(
                    "SELECT body FROM planner_event_control WHERE id=1",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            let mut control: Value = serde_json::from_str(&raw).unwrap();
            control["binding"] = json!("old-book-binding");
            control["blocked"] = Value::Null;
            control["active"] = json!("2026-10-26-stale-event");
            db.execute(
                "UPDATE planner_event_control SET body=? WHERE id=1",
                [control.to_string()],
            )
            .unwrap();
            // One large row proves streaming history does not retain legacy per-row limits.
            for i in 0..74 {
                let state = if i < 23 {
                    "planned"
                } else if i < 53 {
                    "failed"
                } else {
                    "deferred"
                };
                let body = if i == 0 && refusal == 0 {
                    json!({"fixture": "x".repeat(16_000_001)}).to_string()
                } else {
                    "{}".into()
                };
                db.execute(
                    "INSERT INTO planner_attempt_log VALUES(?,?,?,?,'{}')",
                    rusqlite::params![
                        format!("attempt-{i:02}"),
                        format!("event-{i:02}"),
                        body,
                        state
                    ],
                )
                .unwrap();
            }
            if refusal == 13 {
                control["pending"] = json!({"id":"pending","event_id":"event","trigger":{},"binding":"old-book-binding","portfolio_hash":"fixture","started_ms":now,"deadline_ms":now+1000,"predecessor":null,"previous_attempts":0});
                db.execute(
                    "UPDATE planner_event_control SET body=? WHERE id=1",
                    [control.to_string()],
                )
                .unwrap();
            }
            if refusal == 14 {
                db.execute(
                    "UPDATE planner_attempt_log SET state='reserved' WHERE id='attempt-00'",
                    [],
                )
                .unwrap();
            }
            // Neither initialization nor incident handling may mutate this sealed book.
            let mut controller_db = Connection::open(c.directory.join("paper.sqlite")).unwrap();
            assert!(!redecision::initialize(&mut controller_db, &c, &p, Some(&cfg), now).unwrap());
            wide::check_closed_incident_guard(&mut controller_db, &c, &mut p, now);
            p.halted = true;
            p.health = "planner_authority_invalid_recovery_required".into();
            wide::check_closed_incident_guard(&mut controller_db, &c, &mut p, now);
            db.execute_batch("CREATE TABLE paper_halt_history(id INTEGER PRIMARY KEY,at INTEGER NOT NULL,reason TEXT NOT NULL,source TEXT NOT NULL)").unwrap();
            db.execute("INSERT INTO paper_halt_history VALUES(1,?,'planner_authority_invalid_recovery_required','state_transition')", [now]).unwrap();
            match refusal {
                1 => {
                    db.execute("UPDATE paper_halt_history SET reason='other_halt'", [])
                        .unwrap();
                }
                2 => {
                    db.execute("UPDATE paper_halt_history SET at=?", [now - 60_000])
                        .unwrap();
                }
                3 => {
                    db.execute(
                        "UPDATE ticks SET at=? WHERE event='planner_authority_invalid'",
                        [now - 60_000],
                    )
                    .unwrap();
                }
                4 => {
                    p.closed = false;
                }
                5 => {
                    p.cash = Amount::parse("1").unwrap();
                }
                6 => {
                    p.high_water = Amount::parse("6000").unwrap();
                }
                7 => {
                    fs::write(c.directory.join("report.json"), b"{}").unwrap();
                }
                8 => {
                    fs::write(c.directory.join("review.json"), b"{}").unwrap();
                }
                9 => {
                    db.execute("INSERT INTO ticks(at,event) VALUES(?,'other_halt')", [now])
                        .unwrap();
                }
                10 => {
                    p.positions.insert("fixture".into(), serde_json::from_value(json!({"instrument":{"key":"fixture","symbol":"FIXTURE","series":"EQ","tick":"0.05"},"units":1,"entry":"100","mark":"100","marked_at":now})).unwrap());
                }
                11 => {
                    let mut r: Value = serde_json::from_slice(&report).unwrap();
                    r["live_enabled"] = json!(true);
                    fs::write(
                        c.directory.join("report.json"),
                        serde_json::to_vec(&r).unwrap(),
                    )
                    .unwrap();
                }
                12 => {
                    let mut r: Value = serde_json::from_slice(&report).unwrap();
                    r["session"] = json!("2026-10-15");
                    fs::write(
                        c.directory.join("report.json"),
                        serde_json::to_vec(&r).unwrap(),
                    )
                    .unwrap();
                }
                _ => {}
            }
            db.execute(
                "UPDATE state SET body=? WHERE id=1",
                [serde_json::to_string(&p).unwrap()],
            )
            .unwrap();
            drop(db);
            let original = fs::read(c.directory.join("paper.sqlite")).unwrap();
            if refusal == 0 {
                assert!(confirm(&c, target, false, now).is_err());
                let lock = AdvisoryLock::new(
                    fs::OpenOptions::new()
                        .write(true)
                        .create(true)
                        .truncate(false)
                        .open(c.directory.join("controller.lock"))
                        .unwrap(),
                );
                lock.try_lock_exclusive().unwrap();
                assert!(confirm(&c, target, true, now).is_err());
                drop(lock);
                let receipt = confirm(&c, target, true, now).unwrap();
                assert_eq!(receipt["evidence"]["planner"]["attempt_count"], 74);
                assert!(
                    receipt["evidence"]["planner"]["attempt_body_result_bytes"]
                        .as_u64()
                        .unwrap()
                        > 16_000_000
                );
                let (check_db, check_body) = rollover::bound_state(&c).unwrap();
                assert!(approved(&c, &check_db, &check_body).unwrap().is_some());
                assert!(
                    accepted_review(&c, &check_db, &check_body)
                        .unwrap()
                        .is_some()
                );
                assert_eq!(
                    rollover::preview(
                        &c,
                        DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
                            .unwrap()
                            .timestamp_millis()
                    )
                    .unwrap()["status"],
                    "rollover_ready"
                );
                let path = c.directory.join("config.json");
                let mut serialized = serde_json::to_value(&c).unwrap();
                serialized["planner_config"] = json!(c.planner_config);
                fs::write(&path, serde_json::to_vec(&serialized).unwrap()).unwrap();
                assert!(rollover::update_safe(&path, now).unwrap());
                assert!(rollover::stopped_book_safe(&path).unwrap());
                assert_eq!(
                    fs::read(c.directory.join("paper.sqlite")).unwrap(),
                    original
                );
                assert_eq!(fs::read(c.directory.join("report.json")).unwrap(), report);
                let tamper_db = Connection::open(c.directory.join("paper.sqlite")).unwrap();
                tamper_db
                    .execute(
                        "UPDATE planner_attempt_log SET result='changed' WHERE id='attempt-00'",
                        [],
                    )
                    .unwrap();
                assert!(approved(&c, &check_db, &check_body).is_err());
                tamper_db
                    .execute(
                        "UPDATE planner_attempt_log SET result='{}' WHERE id='attempt-00'",
                        [],
                    )
                    .unwrap();
                assert!(approved(&c, &check_db, &check_body).unwrap().is_some());
                tamper_db
                    .execute("UPDATE planner_event_control SET body=body || ' '", [])
                    .unwrap();
                assert!(approved(&c, &check_db, &check_body).is_err());
            } else {
                assert!(confirm(&c, target, true, now).is_err(), "refusal {refusal}");
                assert!(!c.directory.join(FILE).exists());
            }
        }
    }
    // A holiday/weekend between source close and the named next open is a
    // valid maintenance window; receipt verification must agree with creation.
    let (_weekend_dir, weekend, _, weekend_target) = fixture();
    let weekend_now = DateTime::parse_from_rfc3339("2026-10-18T16:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    confirm(&weekend, weekend_target, true, weekend_now).unwrap();
    let (weekend_db, weekend_body) = rollover::bound_state(&weekend).unwrap();
    assert!(
        approved(&weekend, &weekend_db, &weekend_body)
            .unwrap()
            .is_some()
    );
    let (_d, c, now, target) = fixture();
    let before = [
        "paper.sqlite",
        "book-identity.json",
        "report.json",
        "review.json",
        "model/usage-identity.json",
        "model/usage-reservations.json",
    ]
    .map(|n| fs::read(c.directory.join(n)).unwrap());
    assert!(confirm(&c, target, false, now).is_err());
    assert!(confirm(&c, target.succ_opt().unwrap(), true, now).is_err());
    let mut wrong = c.clone();
    wrong.account = "wrong-account".into();
    assert!(confirm(&wrong, target, true, now).is_err());
    assert!(!c.directory.join(FILE).exists());
    let approval = confirm(&c, target, true, now).unwrap();
    assert_eq!(approval, confirm(&c, target, true, now + 1_000).unwrap());
    let at = DateTime::parse_from_rfc3339("2026-10-19T08:30:00+05:30")
        .unwrap()
        .timestamp_millis();
    assert_eq!(
        rollover::preview(&c, at).unwrap()["status"],
        "rollover_ready"
    );
    let mut cfg_path = c.directory.join("paper-config.json");
    let mut config_value = serde_json::to_value(&c).unwrap();
    config_value["planner_config"] = json!(c.planner_config);
    fs::write(&cfg_path, serde_json::to_vec(&config_value).unwrap()).unwrap();
    assert!(rollover::update_safe(&cfg_path, at).unwrap());
    fs::remove_file(&cfg_path).unwrap();
    cfg_path.clear();
    let next = rollover::advance(&c, at).unwrap().unwrap();
    let carry_bytes = fs::read(next.directory.join("carry-over.json")).unwrap();
    let carry: Value = serde_json::from_slice(&carry_bytes).unwrap();
    rollover::verify_carry(&next, &carry).unwrap();
    // The carry destination binding must be authenticated during verification.
    let mut wrong_destination = carry.clone();
    wrong_destination["destination_binding"] = json!("different-destination");
    let mut wrong_session = next.clone();
    wrong_session.session = next.session.succ_opt().unwrap();
    wrong_session.previous_session = next.session;
    assert!(rollover::verify_carry(&wrong_session, &carry).is_err());
    assert!(rollover::verify_carry(&next, &wrong_destination).is_err());
    let copied: Portfolio = serde_json::from_value(carry["portfolio"].clone()).unwrap();
    assert_eq!(copied.cash, Amount::parse("5000").unwrap());
    assert!(!copied.halted && !copied.stopped && !copied.closed && copied.model_led);
    // Re-enter after a crash between staged files and the current-pointer commit.
    fs::remove_file(c.sessions_root.as_ref().unwrap().join("current.json")).unwrap();
    let retried = rollover::advance(&c, at).unwrap().unwrap();
    assert_eq!(retried.session, next.session);
    assert_eq!(
        carry_bytes,
        fs::read(next.directory.join("carry-over.json")).unwrap()
    );
    for (name, original) in [
        "paper.sqlite",
        "book-identity.json",
        "report.json",
        "review.json",
        "model/usage-identity.json",
        "model/usage-reservations.json",
    ]
    .into_iter()
    .zip(before)
    {
        assert_eq!(
            fs::read(c.directory.join(name)).unwrap(),
            original,
            "{name}"
        );
    }
    // Neither partial receipts nor post-approval changes can authorize a successor.
    let (_d, changed, changed_now, changed_target) = fixture();
    fs::write(changed.directory.join(FILE), b"{partial").unwrap();
    assert!(confirm(&changed, changed_target, true, changed_now).is_err());
    assert_eq!(fs::read(changed.directory.join(FILE)).unwrap(), b"{partial");
    let (_d, changed, changed_now, changed_target) = fixture();
    confirm(&changed, changed_target, true, changed_now).unwrap();
    fs::write(changed.directory.join("report.json"), b"changed").unwrap();
    assert!(rollover::preview(&changed, at).is_err());
    let (_d, pending, pending_now, pending_target) = fixture();
    let db = Connection::open(pending.directory.join("paper.sqlite")).unwrap();
    db.execute(
        "INSERT INTO planner_attempt_log VALUES('pending','event','{}','reserved',NULL)",
        [],
    )
    .unwrap();
    drop(db);
    assert!(confirm(&pending, pending_target, true, pending_now).is_err());
    let (_d, malformed, malformed_now, malformed_target) = fixture();
    let db = Connection::open(malformed.directory.join("paper.sqlite")).unwrap();
    db.execute(
        "INSERT INTO planner_attempt_log VALUES('bad','event','{}','failed','{}')",
        [],
    )
    .unwrap();
    drop(db);
    assert!(confirm(&malformed, malformed_target, true, malformed_now).is_err());
    let (_d, loss, loss_now, loss_target) = fixture();
    let db = Connection::open(loss.directory.join("paper.sqlite")).unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?,NULL,NULL,'daily_loss_tripwire')",
        [loss_now],
    )
    .unwrap();
    drop(db);
    assert!(confirm(&loss, loss_target, true, loss_now).is_err());
    let (_d, exposure, exposure_now, exposure_target) = fixture();
    let db = Connection::open(exposure.directory.join("paper.sqlite")).unwrap();
    let body: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    let mut p: Portfolio = serde_json::from_str(&body).unwrap();
    p.cash = Amount::parse("4999").unwrap();
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        [serde_json::to_string(&p).unwrap()],
    )
    .unwrap();
    drop(db);
    assert!(confirm(&exposure, exposure_target, true, exposure_now).is_err());
}
