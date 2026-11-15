use sadr_core::{
    market::{AccountSource, Calendar, Instrument},
    paper_source::PaperSourceBinding,
};
use sadr_store::{archive::LegacyUpstoxCapture, paper_source::quote_receipt};
use serde_json::{Value, json};
#[test]
fn exact_original_quote_receipt_is_preserved_without_a_later_clock() {
    let vectors: Value =
        serde_json::from_str(include_str!("../../../fixtures/rust/archive_vectors.json")).unwrap();
    let case = &vectors["cases"][0];
    let capture = LegacyUpstoxCapture::parse(
        case["original_json"].as_str().unwrap().into(),
        case["original_hash"].as_str().unwrap().into(),
    )
    .unwrap();
    let instruments: Vec<Instrument> = serde_json::from_value(case["instruments"].clone()).unwrap();
    let identity: AccountSource = serde_json::from_value(case["account_source"].clone()).unwrap();
    let calendar: Calendar = serde_json::from_value(case["calendar"].clone()).unwrap();
    let binding=PaperSourceBinding::parse(&serde_json::to_vec(&json!({"source":"upstox-readonly-v1","instrument":instruments[0],"review_ref":"fixture"})).unwrap()).unwrap();
    let receipt = quote_receipt(&capture, &binding, &identity, &calendar).unwrap();
    assert_eq!(
        serde_json::to_value(&receipt).unwrap(),
        serde_json::to_value(
            capture
                .derive_receipt(&instruments, &identity, &calendar)
                .unwrap()
        )
        .unwrap()
    );
    let bound = receipt
        .snapshot()
        .bind_receipt(&receipt, &identity)
        .unwrap();
    assert_eq!(bound.received_at(), capture.received_at());
    assert_eq!(
        receipt.snapshot().quotes()[0].received_at(),
        capture.received_at()
    );
    let mut wrong = serde_json::to_value(&instruments[0]).unwrap();
    wrong["key"] = json!("OTHER");
    let binding = PaperSourceBinding::parse(
        &serde_json::to_vec(
            &json!({"source":"upstox-readonly-v1","instrument":wrong,"review_ref":"fixture"}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(quote_receipt(&capture, &binding, &identity, &calendar).is_err());
    let mut kotak = serde_json::to_value(&instruments[0]).unwrap();
    kotak["synthetic"] = json!(false);
    let binding=PaperSourceBinding::parse(&serde_json::to_vec(&json!({"source":"kotak-readonly-v1","instrument":kotak,"review_ref":"fixture","timestamp_reference":"fixture","native_mapping":{"broker":"kotak","canonical_key":instruments[0].key(),"instrument_version":instruments[0].version(),"symbol":instruments[0].symbol(),"segment":"nse_cm","token":"123","master_version":"fixture"}})).unwrap()).unwrap();
    assert!(quote_receipt(&capture, &binding, &identity, &calendar).is_err());
}
