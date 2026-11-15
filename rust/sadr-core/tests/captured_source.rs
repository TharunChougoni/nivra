use sadr_core::captured_source::CapturedMarketSource;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Vectors {
    base: Value,
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    id: String,
    mutation: String,
    accepted: bool,
}

#[test]
fn original_quote_and_candle_declarations_keep_exact_source_linkage() {
    let vectors: Vectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/captured_source_vectors.json"
    ))
    .unwrap();
    assert_eq!(vectors.cases.len(), 8);
    for case in vectors.cases {
        let mut source = vectors.base.clone();
        match case.mutation.as_str() {
            "none" => {}
            "quote-param" => source["quotes"]["params"]["instrument_key"] = "OTHER".into(),
            "future-receipt" => source["candles"][0]["received_at"] = "2026-10-01T04:31:00Z".into(),
            "duplicate-candle" => {
                let candle = source["candles"][0].clone();
                source["candles"].as_array_mut().unwrap().push(candle);
            }
            "mixed-source" => source["candles"][0]["source"] = "kotak-readonly-v1".into(),
            "wrong-route" => source["candles"][0]["path"] = "/v2/order/place".into(),
            "synthetic-instrument" => source["instrument"]["synthetic"] = true.into(),
            "oversized" => source["candles"][0]["body"] = "x".repeat(600_001).into(),
            _ => panic!("unknown captured source vector"),
        }
        let result = serde_json::to_vec(&source)
            .map_err(|_| ())
            .and_then(|raw| CapturedMarketSource::parse(&raw).map_err(|_| ()));
        assert_eq!(result.is_ok(), case.accepted, "{}", case.id);
        if let Ok(source) = result {
            assert_eq!(source.instrument().key(), "NSE_EQ|FIXTURE");
            assert!(source.quotes().received_at() > source.candles()[0].received_at());
            assert_eq!(source.candles().len(), 1);
        }
    }
}
