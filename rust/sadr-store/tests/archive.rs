//! Synthetic Python archive interoperability; no source or credential probes.
use sadr_core::{
    market::{AccountSource, ArchivedQuoteReceipt, Calendar, Instrument, Snapshot},
    time::Timestamp,
};
use sadr_store::{StoreError, archive::LegacyUpstoxCapture};
use serde_json::{Value, json};

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/rust/archive_vectors.json")).unwrap()
}
fn base() -> Value {
    vectors()["cases"][0].clone()
}
fn parse(case: &Value) -> Result<LegacyUpstoxCapture, StoreError> {
    LegacyUpstoxCapture::parse(
        case["original_json"].as_str().unwrap().into(),
        case["original_hash"].as_str().unwrap().into(),
    )
}
fn derive(case: &Value) -> Result<ArchivedQuoteReceipt, StoreError> {
    let instruments: Vec<Instrument> = serde_json::from_value(case["instruments"].clone()).unwrap();
    let identity: AccountSource = serde_json::from_value(case["account_source"].clone()).unwrap();
    let calendar: Calendar = serde_json::from_value(case["calendar"].clone()).unwrap();
    parse(case)?.derive_receipt(&instruments, &identity, &calendar)
}

#[test]
fn original_bodies_rederive_exact_python_hashes_snapshots_and_receipts() {
    let v = vectors();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["synthetic"], true);
    assert_eq!(v["reference"], "python-sadr");
    for key in [
        "reference_domain_sha256",
        "reference_market_archive_sha256",
        "reference_market_sha256",
        "reference_paper_source_sha256",
    ] {
        assert_eq!(v[key].as_str().unwrap().len(), 64);
    }
    assert_eq!(v["cases"].as_array().unwrap().len(), 50);
    let mut accepted = 0;
    let mut rejected = 0;
    for case in v["cases"].as_array().unwrap() {
        let result = derive(case);
        if case["expected_rejected"] == true {
            assert!(result.is_err(), "{}", case["id"]);
            rejected += 1;
        } else {
            let receipt = result.unwrap_or_else(|error| panic!("{}: {error}", case["id"]));
            let actual = serde_json::to_value(&receipt).unwrap();
            let actual_snapshot: Snapshot =
                serde_json::from_value(actual["normalized_snapshot"].clone()).unwrap();
            let expected: Snapshot =
                serde_json::from_value(case["expected_snapshot"].clone()).unwrap();
            assert_eq!(actual_snapshot, expected, "{}", case["id"]);
            assert_eq!(actual["capture_id"], case["original_hash"]);
            assert_eq!(actual["identity"], case["account_source"]);
            let capture = parse(case).unwrap();
            assert_eq!(
                capture.original_json(),
                case["original_json"].as_str().unwrap()
            );
            assert_eq!(
                capture.original_hash(),
                case["original_hash"].as_str().unwrap()
            );
            assert_eq!(actual_snapshot.captured_at(), capture.received_at());
            accepted += 1;
        }
    }
    assert_eq!((accepted, rejected), (25, 25));
}

#[test]
fn rederived_receipt_keeps_financial_age_and_full_projection_binding() {
    let case = base();
    let receipt = derive(&case).unwrap();
    let snapshot: Snapshot = serde_json::from_value(case["expected_snapshot"].clone()).unwrap();
    let identity: AccountSource = serde_json::from_value(case["account_source"].clone()).unwrap();
    let bound = snapshot.bind_receipt(&receipt, &identity).unwrap();
    assert!(
        bound
            .check_fresh(Timestamp::parse("2026-10-15T10:00:02+05:30").unwrap())
            .is_ok()
    );
    assert!(
        bound
            .check_fresh(Timestamp::parse("2026-10-15T10:00:02.000001+05:30").unwrap())
            .is_err()
    );
    let mut changed = case["expected_snapshot"].clone();
    changed["quotes"][0]["ask_units"] = json!(999);
    let changed: Snapshot = serde_json::from_value(changed).unwrap();
    assert!(changed.bind_receipt(&receipt, &identity).is_err());
}
