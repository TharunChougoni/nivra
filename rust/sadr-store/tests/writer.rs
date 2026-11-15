//! Synthetic paper-ledger writer effects. Books are temporary and carry no account authority.
use rusqlite::{Connection, OpenFlags};
use sadr_core::{
    finance::{Amount, FeeArithmetic, Money, Price},
    market::Snapshot,
    risk::Verdict,
    time::Timestamp,
};
use sadr_store::{
    archive::LegacyUpstoxCapture,
    legacy::{LegacyCandidate, inspect_connection},
    protection::ProtectionBinding,
    writer::{Effect, EntryEvidence, Fill, LedgerWriter, Reservation, WriteError},
};
use serde_json::Value;
use std::os::unix::process::ExitStatusExt;
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Barrier},
    time::Duration,
};

fn ledger_vectors() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/rust/ledger_vectors.json")).unwrap()
}

fn protection_vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/rust/protection_vectors.json"
    ))
    .unwrap()
}

fn exposure_vector() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/rust/writer_exposure.json")).unwrap()
}

fn valid_case() -> Value {
    protection_vectors()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "valid-fresh-observation")
        .unwrap()
        .clone()
}

struct Evidence {
    binding: ProtectionBinding,
    protected: LegacyUpstoxCapture,
    latest: LegacyUpstoxCapture,
    snapshot: Snapshot,
}

fn evidence() -> Evidence {
    let case = valid_case();
    let binding = serde_json::from_value(case["binding"].clone()).unwrap();
    let protected = LegacyUpstoxCapture::parse(
        case["protected_capture"]["original_json"]
            .as_str()
            .unwrap()
            .to_owned(),
        case["protected_capture"]["original_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap();
    let latest = LegacyUpstoxCapture::parse(
        case["latest_entry_capture"]["original_json"]
            .as_str()
            .unwrap()
            .to_owned(),
        case["latest_entry_capture"]["original_hash"]
            .as_str()
            .unwrap()
            .to_owned(),
    )
    .unwrap();
    let snapshot = serde_json::from_value(case["entry_snapshot"].clone()).unwrap();
    Evidence {
        binding,
        protected,
        latest,
        snapshot,
    }
}

fn evidence_ref(evidence: &Evidence) -> EntryEvidence<'_> {
    EntryEvidence {
        binding: &evidence.binding,
        protected: &evidence.protected,
        latest_entry: &evidence.latest,
        snapshot: Some(&evidence.snapshot),
    }
}

fn now_stamp() -> Timestamp {
    Timestamp::parse("2026-10-15T10:00:00+05:30").unwrap()
}

fn candidate_from(value: &Value) -> LegacyCandidate {
    LegacyCandidate::parse(
        value["model_json"].as_str().unwrap().to_owned(),
        value["hash"].as_str().unwrap().to_owned(),
    )
    .unwrap()
}

fn verdict_from(text: &str) -> Verdict {
    let parsed: Value = serde_json::from_str(text).unwrap();
    Verdict {
        accepted: parsed["accepted"].as_bool().unwrap(),
        reasons: parsed["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reason| reason.as_str().unwrap().to_owned())
            .collect(),
        lots: parsed["lots"].as_u64().unwrap(),
        reservation: Money::parse(parsed["reservation"].as_str().unwrap()).unwrap(),
    }
}

fn fees_from(value: &Value) -> FeeArithmetic {
    FeeArithmetic {
        per_order: Money::parse(value["per_order"].as_str().unwrap()).unwrap(),
        buy_bps: Money::parse(value["buy_bps"].as_str().unwrap()).unwrap(),
        sell_bps: Money::parse(value["sell_bps"].as_str().unwrap()).unwrap(),
        slippage_bps: Money::parse(value["slippage_bps"].as_str().unwrap()).unwrap(),
        rounding_increment: Some(
            Price::parse(value["rounding_increment"].as_str().unwrap()).unwrap(),
        ),
    }
}

fn trace_by_id(id: &str) -> Value {
    ledger_vectors()["traces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|trace| trace["id"] == id)
        .unwrap()
        .clone()
}

fn ledger_fees() -> FeeArithmetic {
    fees_from(&trace_by_id("reserve")["fees"])
}

fn apply_ledger_schema(db: &Connection) {
    for sql in ledger_vectors()["schema_sql"].as_array().unwrap() {
        db.execute_batch(sql.as_str().unwrap()).unwrap();
    }
}

fn seed_protection(db: &Connection) {
    let protection = protection_vectors();
    for sql in protection["schema_sql"]["ledger_and_protection"]
        .as_array()
        .unwrap()
    {
        let sql = sql.as_str().unwrap();
        if sql.contains("CREATE TABLE paper_protection_profile")
            || sql.contains("CREATE TABLE paper_protection_jobs")
        {
            db.execute_batch(sql).unwrap();
        }
    }
    let case = valid_case();
    let profile = &case["profile"];
    db.execute(
        "INSERT INTO paper_protection_profile(id,value,observations,last_at) VALUES(1,?,?,?)",
        (
            profile["value"].as_str().unwrap(),
            profile["observations"].as_i64().unwrap(),
            profile["last_at"].as_str().unwrap(),
        ),
    )
    .unwrap();
    for job in case["jobs"].as_array().unwrap() {
        db.execute(
            "INSERT INTO paper_protection_jobs(seq,state,capture,result,result_hash) VALUES(?,?,?,?,?)",
            (
                job["seq"].as_i64().unwrap(),
                job["state"].as_str().unwrap(),
                job["capture"].as_str().unwrap(),
                job["result"].as_str().unwrap(),
                job["result_hash"].as_str().unwrap(),
            ),
        )
        .unwrap();
    }
}

fn insert_account(db: &Connection, capital: &str, halted: i64) {
    db.execute(
        "INSERT INTO account(id,cash,initial,halted) VALUES(1,?,?,?)",
        (capital, capital, halted),
    )
    .unwrap();
}

fn open_book(capital: &str, halted: i64, protected: bool, clock: Timestamp) -> LedgerWriter {
    let db = Connection::open_in_memory().unwrap();
    apply_ledger_schema(&db);
    insert_account(&db, capital, halted);
    if protected {
        seed_protection(&db);
    }
    LedgerWriter::open(db, move || Some(clock)).unwrap()
}

struct ObservedFill {
    id: String,
    intent: String,
    leg: i64,
    side: String,
    units: i64,
    price: String,
    fee: String,
    stamp: String,
}

fn assert_amount(actual: &str, expected: &str) {
    assert_eq!(
        Amount::parse(actual).unwrap(),
        Amount::parse(expected).unwrap(),
        "{actual} != {expected}"
    );
}

fn assert_book(db: &Connection, state: &Value) {
    let account = &state["account"][0];
    let (cash, initial, halted): (String, String, i64) = db
        .query_row(
            "SELECT cash,initial,halted FROM account WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_amount(&cash, account["cash"].as_str().unwrap());
    assert_amount(&initial, account["initial"].as_str().unwrap());
    assert_eq!(halted, account["halted"].as_i64().unwrap());

    let mut statement = db
        .prepare("SELECT id,hash,candidate,lots,reserved,state,created FROM intents ORDER BY id")
        .unwrap();
    let intents: Vec<(String, String, String, i64, String, String, String)> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let expected_intents = state["intents"].as_array().unwrap();
    assert_eq!(intents.len(), expected_intents.len());
    for (actual, expected) in intents.iter().zip(expected_intents) {
        assert_eq!(actual.0, expected["id"].as_str().unwrap());
        assert_eq!(actual.1, expected["hash"].as_str().unwrap());
        assert_eq!(actual.2, expected["candidate"].as_str().unwrap());
        assert_eq!(actual.3, expected["lots"].as_i64().unwrap());
        assert_amount(&actual.4, expected["reserved"].as_str().unwrap());
        assert_eq!(actual.5, expected["state"].as_str().unwrap());
        assert_eq!(actual.6, expected["created"].as_str().unwrap());
    }

    let mut statement = db
        .prepare("SELECT id,intent,leg,side,units,price,fee,stamp FROM fills ORDER BY id")
        .unwrap();
    let fills: Vec<ObservedFill> = statement
        .query_map([], |row| {
            Ok(ObservedFill {
                id: row.get(0)?,
                intent: row.get(1)?,
                leg: row.get(2)?,
                side: row.get(3)?,
                units: row.get(4)?,
                price: row.get(5)?,
                fee: row.get(6)?,
                stamp: row.get(7)?,
            })
        })
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    let expected_fills = state["fills"].as_array().unwrap();
    assert_eq!(fills.len(), expected_fills.len());
    for (actual, expected) in fills.iter().zip(expected_fills) {
        assert_eq!(actual.id, expected["id"].as_str().unwrap());
        assert_eq!(actual.intent, expected["intent"].as_str().unwrap());
        assert_eq!(actual.leg, expected["leg"].as_i64().unwrap());
        assert_eq!(actual.side, expected["side"].as_str().unwrap());
        assert_eq!(actual.units, expected["units"].as_i64().unwrap());
        assert_amount(&actual.price, expected["price"].as_str().unwrap());
        assert_amount(&actual.fee, expected["fee"].as_str().unwrap());
        assert_eq!(actual.stamp, expected["stamp"].as_str().unwrap());
    }

    let kinds: Vec<String> = db
        .prepare("SELECT kind FROM events ORDER BY seq")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|kind| kind.unwrap())
        .collect();
    let expected_kinds: Vec<&str> = state["event_kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kind| kind.as_str().unwrap())
        .collect();
    assert_eq!(kinds, expected_kinds);

    for holding in state["holdings"].as_array().unwrap() {
        let units: i64 = db
            .query_row(
                "SELECT COALESCE(SUM(CASE WHEN side='buy' THEN units ELSE -units END),0) \
                 FROM fills WHERE intent=?1 AND leg=?2",
                (
                    holding["intent"].as_str().unwrap(),
                    holding["leg"].as_i64().unwrap(),
                ),
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(units, holding["units"].as_i64().unwrap());
    }
    let counts = &state["row_counts"];
    for (table, column) in [
        ("account", "account"),
        ("intents", "intents"),
        ("fills", "fills"),
        ("events", "events"),
        ("metadata", "metadata"),
    ] {
        let count: i64 = db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, counts[column].as_i64().unwrap(), "{table}");
    }
}

fn replay(trace: &Value, writer: &mut LedgerWriter, evidence: &Evidence) {
    let candidate = candidate_from(&trace["candidate"]);
    let collision = trace.get("collision_candidate").map(candidate_from);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let fees = fees_from(&trace["fees"]);
    let cap = Money::parse(trace["cap"].as_str().unwrap()).unwrap();
    let now = Timestamp::parse(trace["now"].as_str().unwrap()).unwrap();
    let intent = trace["id"].as_str().unwrap();
    for operation in trace["operations"].as_array().unwrap() {
        let action = operation["action"].as_str().unwrap();
        match action {
            "reserve" | "reserve_expired" => {
                let result = writer.reserve(reserve_for(
                    intent, &candidate, &verdict, cap, now, evidence,
                ));
                expect_bool_or_error(operation, result);
            }
            "reserve_twice" => {
                let flags = operation["result"].as_array().unwrap();
                for flag in flags {
                    let result = writer.reserve(reserve_for(
                        intent, &candidate, &verdict, cap, now, evidence,
                    ));
                    assert_eq!(result.unwrap(), effect_of(flag.as_bool().unwrap()));
                }
            }
            "reserve_then_collision" => {
                let result = writer.reserve(reserve_for(
                    intent, &candidate, &verdict, cap, now, evidence,
                ));
                assert_eq!(result.unwrap(), Effect::Applied);
                let result = writer.reserve(reserve_for(
                    intent,
                    collision.as_ref().unwrap(),
                    &verdict,
                    cap,
                    now,
                    evidence,
                ));
                assert_eq!(result.unwrap_err(), WriteError::Collision);
                assert!(operation.get("error").is_some());
            }
            "fill_one" | "fill" | "fill_race" => {
                let result = writer.fill(fill_for(fill_spec(
                    "fill-1", intent, "100", false, &fees, now, evidence,
                )));
                expect_bool_or_error(operation, result);
            }
            "fill_two" => {
                let result = writer.fill(fill_for(fill_spec(
                    "fill-2", intent, "100", false, &fees, now, evidence,
                )));
                expect_bool_or_error(operation, result);
            }
            "duplicate_fill" => {
                let result = writer.fill(fill_for(fill_spec(
                    "fill-1", intent, "100", false, &fees, now, evidence,
                )));
                assert_eq!(result.unwrap(), Effect::Unchanged);
            }
            "fill_id_collision" => {
                let result = writer.fill(fill_for(fill_spec(
                    "fill-1", intent, "99.95", false, &fees, now, evidence,
                )));
                assert_eq!(result.unwrap_err(), WriteError::Collision);
            }
            "request_cancel" => {
                writer.request_cancel(intent).unwrap();
            }
            "confirm_cancel" => {
                assert_eq!(writer.cancel(intent).unwrap(), Effect::Applied);
            }
            "halt" => {
                assert_eq!(writer.halt("synthetic_test_halt").unwrap(), Effect::Applied);
            }
            "exit" => {
                let result = writer.fill(fill_for(fill_spec(
                    "exit-1", intent, "99", true, &fees, now, evidence,
                )));
                assert_eq!(result.unwrap(), Effect::Applied);
            }
            other => panic!("unexpected ledger action {other}"),
        }
        assert_book(writer.connection(), &operation["state"]);
    }
}

fn reserve_for<'a>(
    intent: &'a str,
    candidate: &'a LegacyCandidate,
    verdict: &'a Verdict,
    cap: Money,
    now: Timestamp,
    evidence: &'a Evidence,
) -> Reservation<'a> {
    Reservation {
        intent_id: intent,
        candidate,
        verdict,
        cap,
        requested: now,
        evidence: Some(evidence_ref(evidence)),
    }
}

struct FillSpec<'a> {
    fill_id: &'a str,
    intent: &'a str,
    units: i64,
    price: &'a str,
    exiting: bool,
    fees: &'a FeeArithmetic,
    now: Timestamp,
    evidence: &'a Evidence,
}

fn fill_spec<'a>(
    fill_id: &'a str,
    intent: &'a str,
    price: &'a str,
    exiting: bool,
    fees: &'a FeeArithmetic,
    now: Timestamp,
    evidence: &'a Evidence,
) -> FillSpec<'a> {
    FillSpec {
        fill_id,
        intent,
        units: 1,
        price,
        exiting,
        fees,
        now,
        evidence,
    }
}

fn fill_for(spec: FillSpec<'_>) -> Fill<'_> {
    Fill {
        fill_id: spec.fill_id,
        intent_id: spec.intent,
        leg_index: 0,
        units: spec.units,
        price: Price::parse(spec.price).unwrap(),
        fees: spec.fees,
        requested: spec.now,
        exiting: spec.exiting,
        evidence: Some(evidence_ref(spec.evidence)),
    }
}

fn effect_of(applied: bool) -> Effect {
    if applied {
        Effect::Applied
    } else {
        Effect::Unchanged
    }
}

fn expect_bool_or_error(operation: &Value, result: Result<Effect, WriteError>) {
    if operation.get("error").is_some() {
        assert_eq!(result.unwrap_err(), WriteError::Rejected);
        return;
    }
    if let Some(applied) = operation["result"].as_bool() {
        assert_eq!(result.unwrap(), effect_of(applied));
    } else {
        result.unwrap();
    }
}

fn alert_reason(db: &Connection) -> String {
    let payload: String = db
        .query_row(
            "SELECT payload FROM events WHERE kind='operator_alert' ORDER BY seq DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let parsed: Value = serde_json::from_str(&payload).unwrap();
    assert!(parsed.get("recorded_at").is_some());
    parsed["reason"].as_str().unwrap().to_owned()
}

#[test]
fn python_ledger_traces_commit_the_same_rows() {
    let evidence = evidence();
    for trace in ledger_vectors()["traces"].as_array().unwrap() {
        let mut writer = open_book(
            trace["initial_capital"].as_str().unwrap(),
            0,
            true,
            now_stamp(),
        );
        replay(trace, &mut writer, &evidence);
        assert_book(writer.connection(), &trace["final_state"]);
    }
}

#[test]
fn reserved_exposure_remains_after_cash_falls_below_it() {
    let vector = exposure_vector();
    assert_eq!(vector["synthetic"], true);
    assert_eq!(vector["reference"], "python-sadr");
    let evidence = evidence();
    let candidate = candidate_from(&vector["candidate"]);
    let verdict = verdict_from(vector["verdict_json"].as_str().unwrap());
    let fees = ledger_fees();
    let now = Timestamp::parse(vector["now"].as_str().unwrap()).unwrap();
    let cap = Money::parse(vector["cap"].as_str().unwrap()).unwrap();
    let mut writer = open_book(vector["initial_capital"].as_str().unwrap(), 0, true, now);
    assert_eq!(
        writer
            .reserve(reserve_for(
                "reserved-above-cash",
                &candidate,
                &verdict,
                cap,
                now,
                &evidence
            ))
            .unwrap(),
        Effect::Applied
    );
    assert_eq!(
        writer
            .fill(fill_for(fill_spec(
                "fill-1",
                "reserved-above-cash",
                vector["price"].as_str().unwrap(),
                false,
                &fees,
                now,
                &evidence,
            )))
            .unwrap(),
        Effect::Applied
    );
    let (cash, reserved, state, halted): (String, String, String, i64) = writer
        .connection()
        .query_row(
            "SELECT account.cash, intents.reserved, intents.state, account.halted \
             FROM account, intents WHERE account.id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_amount(&cash, vector["cash_after_fill"].as_str().unwrap());
    assert_amount(&reserved, vector["reserved_after_fill"].as_str().unwrap());
    assert!(Amount::parse(&reserved).unwrap() > Amount::parse(&cash).unwrap());
    assert_eq!(state, vector["state_after_fill"].as_str().unwrap());
    assert_eq!(halted, 0);
    let fee: String = writer
        .connection()
        .query_row("SELECT fee FROM fills", [], |row| row.get(0))
        .unwrap();
    assert_amount(&fee, vector["fee"].as_str().unwrap());
}

#[test]
fn ordinary_rejection_rolls_back_to_the_prior_book() {
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let mut verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    verdict.reservation = Money::parse("6000").unwrap();
    let now = now_stamp();
    let mut writer = open_book("5000", 0, true, now);
    let before = writer
        .connection()
        .query_row("SELECT cash,halted FROM account", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        });
    let rejected = writer.reserve(reserve_for(
        "reserve",
        &candidate,
        &verdict,
        Money::parse("5000").unwrap(),
        now,
        &evidence,
    ));
    assert_eq!(rejected.unwrap_err(), WriteError::Rejected);
    let after = writer
        .connection()
        .query_row("SELECT cash,halted FROM account", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap();
    assert_eq!(after, before.unwrap());
    let intents: i64 = writer
        .connection()
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    let events: i64 = writer
        .connection()
        .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
        .unwrap();
    assert_eq!((intents, events), (0, 0));

    verdict.reservation = Money::parse("110").unwrap();
    writer
        .reserve(reserve_for(
            "reserve",
            &candidate,
            &verdict,
            Money::parse("5000").unwrap(),
            now,
            &evidence,
        ))
        .unwrap();
    let fees = ledger_fees();
    let mut overfill_spec = fill_spec("fill-1", "reserve", "100", false, &fees, now, &evidence);
    overfill_spec.units = 2;
    let overfill = writer.fill(fill_for(overfill_spec));
    assert_eq!(overfill.unwrap_err(), WriteError::Rejected);
    let (cash, state, fills): (String, String, i64) = writer
        .connection()
        .query_row(
            "SELECT account.cash, intents.state, (SELECT COUNT(*) FROM fills) FROM account, intents",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_amount(&cash, "5000");
    assert_eq!(state, "RESERVED");
    assert_eq!(fills, 0);
}

#[test]
fn barrier_failure_rolls_back_the_uncommitted_reservation() {
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let now = now_stamp();
    let mut writer = open_book("5000", 0, true, now);
    let result = writer.reserve_with_barrier(
        reserve_for(
            "reserve",
            &candidate,
            &verdict,
            Money::parse("5000").unwrap(),
            now,
            &evidence,
        ),
        |_| Err(WriteError::Rejected),
    );
    assert_eq!(result.unwrap_err(), WriteError::Rejected);
    let intents: i64 = writer
        .connection()
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(intents, 0);
}

#[test]
fn missing_protection_commits_halt_and_alert_without_an_entry() {
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let now = now_stamp();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("baseline.sqlite3");
    {
        let db = Connection::open(&path).unwrap();
        apply_ledger_schema(&db);
        insert_account(&db, "5000", 0);
        let mut writer = LedgerWriter::open(db, move || Some(now)).unwrap();
        let effect = writer
            .reserve(reserve_for(
                "reserve",
                &candidate,
                &verdict,
                Money::parse("5000").unwrap(),
                now,
                &evidence,
            ))
            .unwrap();
        assert_eq!(effect, Effect::ProtectionDenied);
        assert_eq!(
            alert_reason(writer.connection()),
            "paper_entry_protection_unavailable"
        );
        let intents: i64 = writer
            .connection()
            .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
            .unwrap();
        assert_eq!(intents, 0);
    }
    let readable = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let snapshot = inspect_connection(readable).unwrap();
    assert!(snapshot.halted());
    assert_eq!(snapshot.cash(), Money::parse("5000").unwrap());
    assert!(snapshot.intents().is_empty());
    assert_eq!(snapshot.event_kinds(), ["operator_alert"]);
}

#[test]
fn a_pure_verdict_cannot_skip_the_post_lock_protection_check() {
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let now = now_stamp();
    let mut writer = open_book("5000", 0, true, now);
    let mut reservation = reserve_for(
        "reserve",
        &candidate,
        &verdict,
        Money::parse("5000").unwrap(),
        now,
        &evidence,
    );
    reservation.evidence = Some(EntryEvidence {
        snapshot: None,
        ..evidence_ref(&evidence)
    });
    assert_eq!(
        writer.reserve(reservation).unwrap(),
        Effect::ProtectionDenied
    );
    let intents: i64 = writer
        .connection()
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    let halted: i64 = writer
        .connection()
        .query_row("SELECT halted FROM account", [], |row| row.get(0))
        .unwrap();
    assert_eq!((intents, halted), (0, 1));
    assert_eq!(
        alert_reason(writer.connection()),
        "paper_entry_protection_unavailable"
    );

    let mut writer = open_book("5000", 0, true, now);
    let mut reservation = reserve_for(
        "reserve",
        &candidate,
        &verdict,
        Money::parse("5000").unwrap(),
        now,
        &evidence,
    );
    reservation.evidence = None;
    assert_eq!(
        writer.reserve(reservation).unwrap(),
        Effect::ProtectionDenied
    );
}

#[test]
fn stale_protection_after_the_lock_denies_entry_and_an_early_clock_rolls_back() {
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let requested = now_stamp();
    let stale = Timestamp::parse("2026-10-15T11:00:00+05:30").unwrap();
    let mut writer = open_book("5000", 0, true, stale);
    let effect = writer
        .reserve(reserve_for(
            "reserve",
            &candidate,
            &verdict,
            Money::parse("5000").unwrap(),
            requested,
            &evidence,
        ))
        .unwrap();
    assert_eq!(effect, Effect::ProtectionDenied);
    let intents: i64 = writer
        .connection()
        .query_row("SELECT COUNT(*) FROM intents", [], |row| row.get(0))
        .unwrap();
    assert_eq!(intents, 0);

    let early = Timestamp::parse("2026-10-15T09:59:00+05:30").unwrap();
    let mut writer = open_book("5000", 0, true, early);
    let result = writer.reserve(reserve_for(
        "reserve",
        &candidate,
        &verdict,
        Money::parse("5000").unwrap(),
        requested,
        &evidence,
    ));
    assert_eq!(result.unwrap_err(), WriteError::Rejected);
    let (halted, events): (i64, i64) = writer
        .connection()
        .query_row(
            "SELECT halted, (SELECT COUNT(*) FROM events) FROM account",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((halted, events), (0, 0));
}

#[test]
fn halted_state_stays_halted_and_still_allows_no_new_entry() {
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let now = now_stamp();
    let mut writer = open_book("5000", 1, true, now);
    assert_eq!(
        writer
            .reserve(reserve_for(
                "reserve",
                &candidate,
                &verdict,
                Money::parse("5000").unwrap(),
                now,
                &evidence
            ))
            .unwrap(),
        Effect::ProtectionDenied
    );
    let (halted, intents): (i64, i64) = writer
        .connection()
        .query_row(
            "SELECT halted, (SELECT COUNT(*) FROM intents) FROM account",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((halted, intents), (1, 0));
}

fn book_file(capital: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.sqlite3");
    let db = Connection::open(&path).unwrap();
    apply_ledger_schema(&db);
    insert_account(&db, capital, 0);
    seed_protection(&db);
    drop(db);
    (dir, path)
}

fn reserve_file(path: &Path, trace_id: &str, alternate: bool) -> Result<Effect, WriteError> {
    let now = now_stamp();
    let db = Connection::open(path).map_err(|_| WriteError::Unavailable)?;
    let mut writer = LedgerWriter::open(db, move || Some(now))?;
    let evidence = evidence();
    let trace = trace_by_id(trace_id);
    let candidate = if alternate {
        candidate_from(&trace["collision_candidate"])
    } else {
        candidate_from(&trace["candidate"])
    };
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    writer.reserve(reserve_for(
        trace["id"].as_str().unwrap(),
        &candidate,
        &verdict,
        Money::parse(trace["cap"].as_str().unwrap()).unwrap(),
        now,
        &evidence,
    ))
}

fn fill_file(path: &Path, price: &str) -> Result<Effect, WriteError> {
    let now = now_stamp();
    let db = Connection::open(path).map_err(|_| WriteError::Unavailable)?;
    let mut writer = LedgerWriter::open(db, move || Some(now))?;
    let evidence = evidence();
    let fees = ledger_fees();
    writer.fill(fill_for(fill_spec(
        "fill-1", "reserve", price, false, &fees, now, &evidence,
    )))
}

fn commit_reserve(path: &Path) {
    assert_eq!(
        reserve_file(path, "reserve", false).unwrap(),
        Effect::Applied
    );
}

fn race_two<T: Send>(left: impl FnOnce() -> T + Send, right: impl FnOnce() -> T + Send) -> (T, T) {
    let barrier = Arc::new(Barrier::new(2));
    let left_barrier = Arc::clone(&barrier);
    let right_barrier = barrier;
    std::thread::scope(|scope| {
        let left = scope.spawn(move || {
            left_barrier.wait();
            left()
        });
        let right = scope.spawn(move || {
            right_barrier.wait();
            right()
        });
        (left.join().unwrap(), right.join().unwrap())
    })
}

fn count_table(path: &Path, table: &str) -> i64 {
    let db = Connection::open(path).unwrap();
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

/// Two connections can make SQLite return `database is locked` at once, before
/// the writer's busy timeout runs. Retry that unavailable result only. The
/// shipped writer is unchanged, and a settled race still has one effect.
fn settle_race<T>(operation: impl Fn() -> Result<T, WriteError>) -> Result<T, WriteError> {
    for attempt in 0..40 {
        match operation() {
            Err(WriteError::Unavailable) => {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.subsec_nanos())
                    .unwrap_or(attempt);
                std::thread::sleep(Duration::from_micros(u64::from(nanos % 5_000) + 500));
            }
            other => return other,
        }
    }
    operation()
}

#[test]
fn two_connections_leave_exactly_one_reservation_or_fill() {
    let (_dir, path) = book_file("5000");
    let left_path = path.clone();
    let right_path = path.clone();
    let (left, right) = race_two(
        move || settle_race(|| reserve_file(&left_path, "reserve", false)),
        move || settle_race(|| reserve_file(&right_path, "reserve", false)),
    );
    let mut applied = 0;
    let mut unchanged = 0;
    for result in [left, right] {
        match result.unwrap() {
            Effect::Applied => applied += 1,
            Effect::Unchanged => unchanged += 1,
            Effect::ProtectionDenied => panic!("protection denied a fresh reservation race"),
        }
    }
    assert_eq!((applied, unchanged), (1, 1));
    assert_eq!(count_table(&path, "intents"), 1);
    assert_eq!(count_table(&path, "events"), 1);

    let (_collision_dir, collision_path) = book_file("5000");
    let left_path = collision_path.clone();
    let right_path = collision_path.clone();
    let (left, right) = race_two(
        move || settle_race(|| reserve_file(&left_path, "intent-id-collision", false)),
        move || settle_race(|| reserve_file(&right_path, "intent-id-collision", true)),
    );
    let mut applied = 0;
    let mut collisions = 0;
    for result in [left, right] {
        match result {
            Ok(Effect::Applied) => applied += 1,
            Err(WriteError::Collision) => collisions += 1,
            other => panic!("unexpected collision race: {other:?}"),
        }
    }
    assert_eq!((applied, collisions), (1, 1));
    assert_eq!(count_table(&collision_path, "intents"), 1);
    let stored_hash: String = Connection::open(&collision_path)
        .unwrap()
        .query_row("SELECT hash FROM intents", [], |row| row.get(0))
        .unwrap();
    let trace = trace_by_id("intent-id-collision");
    assert!(
        stored_hash == trace["candidate"]["hash"].as_str().unwrap()
            || stored_hash == trace["collision_candidate"]["hash"].as_str().unwrap()
    );

    let (_fill_dir, fill_path) = book_file("5000");
    commit_reserve(&fill_path);
    let left_path = fill_path.clone();
    let right_path = fill_path.clone();
    let (left, right) = race_two(
        move || settle_race(|| fill_file(&left_path, "100")),
        move || settle_race(|| fill_file(&right_path, "100")),
    );
    let mut applied = 0;
    let mut unchanged = 0;
    for result in [left, right] {
        match result.unwrap() {
            Effect::Applied => applied += 1,
            Effect::Unchanged => unchanged += 1,
            Effect::ProtectionDenied => panic!("protection denied a fresh fill race"),
        }
    }
    assert_eq!((applied, unchanged), (1, 1));
    assert_eq!(count_table(&fill_path, "fills"), 1);
    let cash: String = Connection::open(&fill_path)
        .unwrap()
        .query_row("SELECT cash FROM account", [], |row| row.get(0))
        .unwrap();
    assert_amount(&cash, "4899.45");

    let (_price_dir, price_path) = book_file("5000");
    commit_reserve(&price_path);
    let left_path = price_path.clone();
    let right_path = price_path.clone();
    let (left, right) = race_two(
        move || settle_race(|| fill_file(&left_path, "100")),
        move || settle_race(|| fill_file(&right_path, "99.95")),
    );
    let mut applied = 0;
    let mut collisions = 0;
    for result in [left, right] {
        match result {
            Ok(Effect::Applied) => applied += 1,
            Err(WriteError::Collision) => collisions += 1,
            other => panic!("unexpected fill collision race: {other:?}"),
        }
    }
    assert_eq!((applied, collisions), (1, 1));
    assert_eq!(count_table(&price_path, "fills"), 1);
    let (fill_id, price): (String, String) = Connection::open(&price_path)
        .unwrap()
        .query_row("SELECT id, price FROM fills", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(fill_id, "fill-1");
    assert!(price == "100" || price == "99.95");
}

fn pause_for_kill(ready: &str) -> Result<(), WriteError> {
    std::fs::write(ready, "READY").map_err(|_| WriteError::Unavailable)?;
    std::thread::sleep(Duration::from_secs(20));
    Ok(())
}

fn crash_child(role: &str) -> Result<(), String> {
    let db_path = std::env::var("SADR_CRASH_DB").map_err(|error| error.to_string())?;
    let ready = std::env::var("SADR_CRASH_READY").map_err(|error| error.to_string())?;
    let now = now_stamp();
    let db = Connection::open(&db_path).map_err(|error| error.to_string())?;
    let mut writer =
        LedgerWriter::open(db, move || Some(now)).map_err(|error| error.to_string())?;
    let evidence = evidence();
    let trace = trace_by_id("reserve");
    let candidate = candidate_from(&trace["candidate"]);
    let verdict = verdict_from(trace["verdict_json"].as_str().unwrap());
    let reservation = reserve_for(
        "reserve",
        &candidate,
        &verdict,
        Money::parse("5000").unwrap(),
        now,
        &evidence,
    );
    match role {
        "reserve-before" => {
            writer
                .reserve_with_barrier(reservation, |_| pause_for_kill(&ready))
                .map_err(|error| error.to_string())?;
        }
        "reserve-after" => {
            writer
                .reserve(reservation)
                .map_err(|error| error.to_string())?;
            std::fs::write(&ready, "READY").map_err(|error| error.to_string())?;
            std::thread::sleep(Duration::from_secs(20));
        }
        "fill-before" => {
            writer
                .reserve(reservation)
                .map_err(|error| error.to_string())?;
            let fees = ledger_fees();
            writer
                .fill_with_barrier(
                    fill_for(fill_spec(
                        "fill-1", "reserve", "100", false, &fees, now, &evidence,
                    )),
                    |_| pause_for_kill(&ready),
                )
                .map_err(|error| error.to_string())?;
        }
        other => return Err(format!("unknown crash role {other}")),
    }
    Ok(())
}

fn observe_crash(role: &str, db: &Path) {
    let ready = db.with_extension("ready");
    let _ = std::fs::remove_file(&ready);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("process_crash_before_and_after_commit")
        .arg("--exact")
        .arg("--test-threads=1")
        .env("SADR_CRASH_ROLE", role)
        .env("SADR_CRASH_DB", db)
        .env("SADR_CRASH_READY", &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let message = loop {
        if let Some(status) = child.try_wait().unwrap() {
            let note = std::fs::read_to_string(&ready).unwrap_or_default();
            panic!("crash child exited before the kill for {role}: {status} {note}");
        }
        if let Ok(text) = std::fs::read_to_string(&ready)
            && !text.is_empty()
        {
            break text;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("crash child was not ready for {role}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(message, "READY", "{role} ready file was {message}");
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert_eq!(
        status.signal(),
        Some(9),
        "{role} was not stopped with SIGKILL: {status}"
    );
}

struct CrashBook {
    cash: String,
    halted: i64,
    intents: i64,
    fills: i64,
    events: i64,
}

fn crash_book(path: &Path) -> CrashBook {
    let db = Connection::open(path).unwrap();
    let (cash, halted) = db
        .query_row("SELECT cash, halted FROM account WHERE id=1", [], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .unwrap();
    CrashBook {
        cash,
        halted,
        intents: count_table(path, "intents"),
        fills: count_table(path, "fills"),
        events: count_table(path, "events"),
    }
}

#[test]
fn process_crash_before_and_after_commit() {
    if let Ok(role) = std::env::var("SADR_CRASH_ROLE") {
        if let Err(message) = crash_child(&role) {
            if let Ok(ready) = std::env::var("SADR_CRASH_READY") {
                let _ = std::fs::write(ready, format!("FAIL {message}"));
            }
            eprintln!("{message}");
            std::process::exit(2);
        }
        if let Ok(ready) = std::env::var("SADR_CRASH_READY") {
            let _ = std::fs::write(ready, "FAIL child finished without being killed");
        }
        std::process::exit(2);
    }

    let (_before_dir, before_path) = book_file("5000");
    observe_crash("reserve-before", &before_path);
    let before = crash_book(&before_path);
    assert_amount(&before.cash, "5000");
    assert_eq!(before.halted, 0);
    assert_eq!((before.intents, before.fills, before.events), (0, 0, 0));

    let (_fill_dir, fill_path) = book_file("5000");
    observe_crash("fill-before", &fill_path);
    let fill = crash_book(&fill_path);
    assert_amount(&fill.cash, "5000");
    assert_eq!(fill.halted, 0);
    assert_eq!((fill.intents, fill.fills, fill.events), (1, 0, 1));
    let (state, reserved, kind): (String, String, String) = Connection::open(&fill_path)
        .unwrap()
        .query_row(
            "SELECT intents.state, intents.reserved, events.kind FROM intents, events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(state, "RESERVED");
    assert_amount(&reserved, "110");
    assert_eq!(kind, "reserved");

    let (_after_dir, after_path) = book_file("5000");
    observe_crash("reserve-after", &after_path);
    let after = crash_book(&after_path);
    assert_amount(&after.cash, "5000");
    assert_eq!(after.halted, 0);
    assert_eq!((after.intents, after.fills, after.events), (1, 0, 1));
    let (stored_json, stored_hash, kind): (String, String, String) = Connection::open(&after_path)
        .unwrap()
        .query_row(
            "SELECT intents.candidate, intents.hash, events.kind FROM intents, events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let trace = trace_by_id("reserve");
    assert_eq!(
        stored_json,
        trace["candidate"]["model_json"].as_str().unwrap()
    );
    assert_eq!(stored_hash, trace["candidate"]["hash"].as_str().unwrap());
    assert_eq!(kind, "reserved");
}
