//! Real SQLite databases reconstructed exclusively from synthetic Python traces.
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use sadr_core::finance::{Amount, Money, Price};
use sadr_store::{
    StoreError,
    legacy::{IntentState, LegacyCandidate, inspect_connection},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/rust/ledger_vectors.json")).unwrap()
}

fn populate(db: &Connection, state: &Value) {
    for sql in vectors()["schema_sql"].as_array().unwrap() {
        db.execute_batch(sql.as_str().unwrap()).unwrap();
    }
    for (table, columns) in [
        ("account", vec!["id", "cash", "initial", "halted"]),
        (
            "intents",
            vec![
                "id",
                "hash",
                "candidate",
                "lots",
                "reserved",
                "state",
                "created",
            ],
        ),
        (
            "fills",
            vec![
                "id", "intent", "leg", "side", "units", "price", "fee", "stamp",
            ],
        ),
        ("metadata", vec!["key", "value"]),
    ] {
        for row in state[table].as_array().unwrap() {
            let values: Vec<SqlValue> = columns
                .iter()
                .map(|key| match &row[key] {
                    Value::String(value) => SqlValue::Text(value.clone()),
                    Value::Number(value) => SqlValue::Integer(value.as_i64().unwrap()),
                    Value::Null => SqlValue::Null,
                    _ => panic!("fixture has unsupported SQLite value"),
                })
                .collect();
            let placeholders = vec!["?"; columns.len()].join(",");
            db.execute(
                &format!(
                    "INSERT INTO {table} ({}) VALUES ({placeholders})",
                    columns.join(",")
                ),
                params_from_iter(values),
            )
            .unwrap();
        }
    }
    for (index, kind) in state["event_kinds"].as_array().unwrap().iter().enumerate() {
        db.execute(
            "INSERT INTO events(seq,kind,payload) VALUES(?,?,?)",
            (index as i64 + 1, kind.as_str().unwrap(), "{}"),
        )
        .unwrap();
    }
}

fn database(state: &Value) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    populate(&db, state);
    db
}

fn hash(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn raw_candidate_bytes_and_hash_survive_without_rust_reserialization() {
    let v = vectors();
    for trace in v["traces"].as_array().unwrap() {
        let raw = trace["candidate"]["model_json"].as_str().unwrap();
        let hash = trace["candidate"]["hash"].as_str().unwrap();
        let candidate = LegacyCandidate::parse(raw.into(), hash.into()).unwrap();
        assert_eq!(candidate.original_json(), raw);
        assert_eq!(candidate.original_hash(), hash);
        assert_ne!(serde_json::to_string(candidate.candidate()).unwrap(), raw);
        assert_eq!(
            LegacyCandidate::parse(format!("{raw} "), hash.into()).unwrap_err(),
            StoreError::CandidateHash
        );
        assert_eq!(
            LegacyCandidate::parse(raw.into(), hash.to_uppercase()).unwrap_err(),
            StoreError::CandidateHash
        );
    }
    let invalid = "{}";
    assert_eq!(
        LegacyCandidate::parse(invalid.into(), hash(invalid)).unwrap_err(),
        StoreError::Candidate
    );
    assert_eq!(
        LegacyCandidate::parse("x".repeat(65_537), "a".repeat(64)).unwrap_err(),
        StoreError::Bound
    );
}

#[test]
fn all_actual_python_trace_checkpoints_reconstruct_the_same_ledger_projection() {
    let v = vectors();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["reference"], "python-sadr");
    assert_eq!(v["synthetic"], true);
    assert_eq!(v["reference_domain_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(v["reference_ledger_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(v["traces"].as_array().unwrap().len(), 11);
    let mut checkpoints = 0;
    for trace in v["traces"].as_array().unwrap() {
        let mut states = vec![&trace["initial_state"], &trace["final_state"]];
        states.extend(
            trace["operations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|op| &op["state"]),
        );
        for state in states {
            let result = inspect_connection(database(state)).unwrap();
            let account = &state["account"][0];
            assert_eq!(
                result.cash(),
                Money::parse(account["cash"].as_str().unwrap()).unwrap()
            );
            assert_eq!(
                result.initial(),
                Price::parse(account["initial"].as_str().unwrap()).unwrap()
            );
            assert_eq!(result.halted(), account["halted"] == 1);
            assert_eq!(
                result.intents().len(),
                state["row_counts"]["intents"].as_u64().unwrap() as usize
            );
            assert_eq!(
                result.fill_count(),
                state["row_counts"]["fills"].as_u64().unwrap() as usize
            );
            assert_eq!(
                result.event_kinds(),
                state["event_kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| k.as_str().unwrap())
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                result.metadata().len(),
                state["row_counts"]["metadata"].as_u64().unwrap() as usize
            );
            let mut reserved = Amount::ZERO;
            for row in state["intents"].as_array().unwrap() {
                let intent = &result.intents()[row["id"].as_str().unwrap()];
                assert_eq!(intent.id(), row["id"].as_str().unwrap());
                assert_eq!(
                    intent.candidate().original_json(),
                    row["candidate"].as_str().unwrap()
                );
                assert_eq!(
                    intent.candidate().original_hash(),
                    row["hash"].as_str().unwrap()
                );
                assert_eq!(intent.lots(), row["lots"].as_u64().unwrap());
                assert_eq!(
                    intent.created(),
                    sadr_core::time::Timestamp::parse(row["created"].as_str().unwrap()).unwrap()
                );
                assert_eq!(
                    intent.state(),
                    match row["state"].as_str().unwrap() {
                        "RESERVED" => IntentState::Reserved,
                        "PART_FILLED" => IntentState::PartFilled,
                        "FILLED" => IntentState::Filled,
                        "CANCEL_PENDING" => IntentState::CancelPending,
                        "CANCELLED" => IntentState::Cancelled,
                        "CLOSED" => IntentState::Closed,
                        other => panic!("unexpected fixture state {other}"),
                    }
                );
                assert_eq!(
                    intent.reserved(),
                    Money::parse(row["reserved"].as_str().unwrap()).unwrap()
                );
                reserved = reserved.checked_add(intent.reserved().amount()).unwrap();
            }
            assert_eq!(result.reserved().amount(), reserved);
            for holding in state["holdings"].as_array().unwrap() {
                assert_eq!(
                    result.holdings(
                        holding["intent"].as_str().unwrap(),
                        holding["leg"].as_u64().unwrap() as usize
                    ),
                    holding["units"].as_i64().unwrap()
                );
            }
            checkpoints += 1;
        }
    }
    assert_eq!(checkpoints, 47);
}

#[test]
fn cancelled_exposure_and_halted_books_remain_visible_without_reactivation() {
    let v = vectors();
    let trace = v["traces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| {
            t["final_state"]["intents"][0]["state"] == "CANCELLED"
                && t["final_state"]["fills"]
                    .as_array()
                    .is_some_and(|f| !f.is_empty())
        })
        .unwrap();
    let mut state = trace["final_state"].clone();
    state["account"][0]["halted"] = json!(1);
    let result = inspect_connection(database(&state)).unwrap();
    assert!(result.halted());
    assert!(result.reserved() > Money::ZERO);
    assert_eq!(
        result.intents().values().next().unwrap().state(),
        IntentState::Cancelled
    );
    assert!(result.holdings(result.intents().keys().next().unwrap(), 0) > 0);
    state["intents"][0]["reserved"] = json!("5000");
    let result = inspect_connection(database(&state)).unwrap();
    assert!(result.reserved() > result.cash());
    state["intents"][0]["reserved"] = json!("0");
    assert_eq!(
        inspect_connection(database(&state)).unwrap_err(),
        StoreError::Rows
    );
}
