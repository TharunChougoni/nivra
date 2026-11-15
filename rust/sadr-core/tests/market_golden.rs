//! Synthetic expected outputs from actual Python domain validation/calendar code.

use sadr_core::{
    market::{Instrument, Quote, Snapshot},
    time::Timestamp,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vectors {
    schema_version: u32,
    reference: String,
    reference_domain_sha256: String,
    synthetic: bool,
    instrument_cases: Vec<ValidationCase>,
    quote_cases: Vec<QuoteCase>,
    snapshot_cases: Vec<ValidationCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidationCase {
    id: String,
    input: Value,
    expected_valid: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuoteCase {
    id: String,
    input: Value,
    now: String,
    expected_valid: bool,
    expected_fresh: Option<bool>,
}

fn vectors() -> Vectors {
    let vectors: Vectors =
        serde_json::from_str(include_str!("../../../fixtures/rust/market_vectors.json")).unwrap();
    assert_eq!(vectors.schema_version, 1);
    assert_eq!(vectors.reference, "python-sadr");
    assert_eq!(vectors.reference_domain_sha256.len(), 64);
    assert!(vectors.synthetic);
    vectors
}

#[test]
fn instrument_validation_matches_python() {
    let vectors = vectors();
    assert_eq!(vectors.instrument_cases.len(), 29);
    for case in vectors.instrument_cases {
        assert_eq!(
            serde_json::from_value::<Instrument>(case.input).is_ok(),
            case.expected_valid,
            "{}",
            case.id
        );
    }
    snapshot_structural_validation_matches_python();
}

#[test]
fn quote_validation_and_microsecond_freshness_match_python() {
    let vectors = vectors();
    assert_eq!(vectors.quote_cases.len(), 11);
    for case in vectors.quote_cases {
        let quote = serde_json::from_value::<Quote>(case.input);
        assert_eq!(quote.is_ok(), case.expected_valid, "{}", case.id);
        if let Ok(quote) = quote {
            assert_eq!(
                Some(quote.fresh(Timestamp::parse(&case.now).unwrap())),
                case.expected_fresh,
                "{}",
                case.id
            );
        }
    }
}

fn snapshot_structural_validation_matches_python() {
    let vectors = vectors();
    assert_eq!(vectors.snapshot_cases.len(), 8);
    for case in vectors.snapshot_cases {
        assert_eq!(
            serde_json::from_value::<Snapshot>(case.input).is_ok(),
            case.expected_valid,
            "{}",
            case.id
        );
    }
}
