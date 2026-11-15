use sadr_core::market_capture::MarketCapture;
use sadr_store::{
    StoreError,
    market_archive::{Limits, MarketArchive},
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    original_json: String,
    sha256: String,
}

fn fixture() -> Case {
    let vectors: Vectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/market_archive_vectors.json"
    ))
    .unwrap();
    assert_eq!(vectors.cases.len(), 1);
    let case = vectors.cases.into_iter().next().unwrap();
    assert_eq!(case.id, "synthetic-canonical-upstox-capture");
    case
}

fn capture(original: &str) -> MarketCapture {
    MarketCapture::parse(original.as_bytes()).unwrap()
}

fn limits(records: u32, bytes: u64) -> Limits {
    Limits::new(records, bytes).unwrap()
}

#[test]
fn exact_capture_bytes_are_immutable_idempotent_and_durable() {
    let case = fixture();
    let parsed = capture(&case.original_json);
    assert_eq!(parsed.original_json(), case.original_json);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("market.db");
    let mut archive =
        MarketArchive::open(&path, limits(1, case.original_json.len() as u64)).unwrap();
    let id = archive.append(&parsed).unwrap();
    assert_eq!(id, case.sha256);
    assert_eq!(archive.append(&parsed).unwrap(), id);
    assert_eq!(
        archive.load(&id).unwrap().original_json(),
        case.original_json
    );

    let changed = case.original_json.replace("fixture\\\":1", "fixture\\\":2");
    assert_ne!(changed, case.original_json);
    assert!(matches!(
        archive.append(&capture(&changed)),
        Err(StoreError::Bound)
    ));
    archive.close().unwrap();

    let archive = MarketArchive::open(&path, limits(1, case.original_json.len() as u64)).unwrap();
    assert_eq!(
        archive.load(&id).unwrap().original_json(),
        case.original_json
    );
    archive.close().unwrap();
}
