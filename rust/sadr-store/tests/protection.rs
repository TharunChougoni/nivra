//! Stubbed Python paper-entry guard comparisons. No files, owners, or credentials.
use rusqlite::Connection;
use sadr_core::{market::Snapshot, time::Timestamp};
use sadr_store::{
    StoreError,
    archive::LegacyUpstoxCapture,
    archive_storage::{ArchiveLimits, CaptureSelector, load_upstox_capture},
    protection::{ProtectionBinding, ProtectionObservation, validate_observation},
    protection_storage::inspect_protection_connection,
};
use serde_json::{Value, json};

fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/rust/protection_vectors.json"
    ))
    .unwrap()
}

fn limits() -> ArchiveLimits {
    ArchiveLimits::new(100, 10_000_000).unwrap()
}

fn execute_schema(db: &Connection, group: &str) {
    for sql in vectors()["schema_sql"][group].as_array().unwrap() {
        db.execute_batch(sql.as_str().unwrap()).unwrap();
    }
}

fn protection_database(case: &Value) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    execute_schema(&db, "ledger_and_protection");
    let halted = i64::from(case["halted"] == true);
    db.execute(
        "INSERT INTO account(id,cash,initial,halted) VALUES(1,'5000','5000',?)",
        [halted],
    )
    .unwrap();
    let profile = &case["profile"];
    db.execute(
        "INSERT INTO paper_protection_profile(id,value,observations,last_at) VALUES(1,?,?,?)",
        (
            profile["value"].as_str().unwrap(),
            profile["observations"].as_i64().unwrap(),
            profile["last_at"].as_str(),
        ),
    )
    .unwrap();
    for job in case["jobs"].as_array().unwrap() {
        db.execute(
            "INSERT INTO paper_protection_jobs(seq,state,capture,result,result_hash) VALUES(?,?,?,?,?)",
            (
                job["seq"].as_i64().unwrap(),
                job["state"].as_str().unwrap(),
                job["capture"].as_str(),
                job["result"].as_str(),
                job["result_hash"].as_str(),
            ),
        )
        .unwrap();
    }
    db
}

fn archive_database(rows: &[(&str, &str)]) -> Connection {
    let db = Connection::open_in_memory().unwrap();
    execute_schema(&db, "market_archive");
    for (id, payload) in rows {
        db.execute(
            "INSERT INTO market_captures(id,payload) VALUES(?,?)",
            (id, payload),
        )
        .unwrap();
    }
    db
}

fn load_capture(
    rows: &[(&str, &str)],
    selector: CaptureSelector<'_>,
) -> Result<LegacyUpstoxCapture, StoreError> {
    load_upstox_capture(archive_database(rows), selector, limits())
}

fn absent(case: &Value, flag: &str) -> bool {
    case.get(flag) == Some(&Value::Bool(false))
}

fn entry_guard(case: &Value) -> Result<ProtectionObservation, StoreError> {
    let binding: ProtectionBinding =
        serde_json::from_value(case["binding"].clone()).map_err(|_| StoreError::Protection)?;
    let now =
        Timestamp::parse(case["now"].as_str().unwrap()).map_err(|_| StoreError::Protection)?;
    let rows = inspect_protection_connection(protection_database(case), 100_000)?;
    let protected_record = &case["protected_capture"];
    let protected_id = protected_record["original_hash"].as_str().unwrap();
    let protected_json = protected_record["original_json"].as_str().unwrap();
    let protected = if absent(case, "protected_archive_capture_present") {
        load_capture(&[], CaptureSelector::Latest)?
    } else if case.get("protected_archive_payload_tampered") == Some(&Value::Bool(true)) {
        let tampered = format!("{protected_json} ");
        load_capture(
            &[(protected_id, tampered.as_str())],
            CaptureSelector::Id(protected_id),
        )?
    } else {
        load_capture(
            &[(protected_id, protected_json)],
            CaptureSelector::Id(protected_id),
        )?
    };
    let latest_record = &case["latest_entry_capture"];
    let latest_id = latest_record["original_hash"].as_str().unwrap();
    let latest_json = latest_record["original_json"].as_str().unwrap();
    let latest = if absent(case, "latest_entry_archive_capture_present") {
        load_capture(&[], CaptureSelector::Latest)?
    } else {
        load_capture(&[(latest_id, latest_json)], CaptureSelector::Latest)?
    };
    let snapshot = match &case["entry_snapshot"] {
        Value::Null => None,
        value => Some(
            serde_json::from_value::<Snapshot>(value.clone())
                .map_err(|_| StoreError::Protection)?,
        ),
    };
    validate_observation(
        &binding,
        rows.profile(),
        rows.jobs(),
        rows.halted(),
        now,
        &protected,
        &latest,
        snapshot.as_ref(),
    )
}

#[test]
fn stubbed_python_entry_guard_matches_forty_five_financial_cases() {
    let v = vectors();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["synthetic"], true);
    assert_eq!(v["reference"], "python-sadr");
    assert_eq!(
        v["projection_scope"],
        "stubbed paper_entry.protection_allows_entry(financial=True)"
    );
    for key in ["paper_entry", "services", "domain"] {
        assert_eq!(v["source_sha256"][key].as_str().unwrap().len(), 64);
    }
    let cases = v["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 45);
    let mut allowed = 0;
    for case in cases {
        let result = entry_guard(case);
        let expected = case["expected_allowed"] == true;
        assert_eq!(result.is_ok(), expected, "{}", case["id"]);
        if expected {
            allowed += 1;
            let observation = result.unwrap();
            let latest = case["jobs"]
                .as_array()
                .unwrap()
                .iter()
                .max_by_key(|job| job["seq"].as_u64().unwrap())
                .unwrap();
            assert_eq!(observation.sequence(), latest["seq"].as_u64().unwrap());
            assert_eq!(
                observation.original_result_hash(),
                latest["result_hash"].as_str().unwrap()
            );
            assert_eq!(
                observation.original_result(),
                latest["result"].as_str().unwrap()
            );
            assert_eq!(
                observation.protected().capture_id(),
                latest["capture"].as_str().unwrap()
            );
        }
    }
    assert_eq!(allowed, 4);
}

#[test]
fn protection_binding_rejects_a_mismatched_python_instrument_identity() {
    let mut binding = vectors()["cases"][0]["binding"].clone();
    binding["identity"]["instrument_key"] = json!("NSE_EQ|OTHER");
    assert!(serde_json::from_value::<ProtectionBinding>(binding).is_err());
}
