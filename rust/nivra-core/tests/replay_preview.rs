use nivra_core::replay;
fn data() -> (Vec<u8>, Vec<u8>) {
    let v = replay::example();
    (
        serde_json::to_vec(&v["manifest"]).unwrap(),
        v["events"].as_str().unwrap().as_bytes().to_vec(),
    )
}
#[test]
fn offline_example_has_no_fallback_entries_and_retains_curve_and_bindings() {
    let (m, e) = data();
    let p = replay::evaluate(&m, &e, || true).unwrap();
    assert_eq!(p.report.events, 18);
    assert_eq!(p.equity_curve.len(), 18);
    assert_eq!(p.report.portfolio.fills.len(), 0);
    assert!(p.report.portfolio.positions.is_empty());
    assert!(p.report.fees == nivra_core::finance::Amount::ZERO);
    assert!(!p.report.promotion_authorized);
    assert!(!p.report.operationally_accepted);
    assert_eq!(p.data_hash, replay::digest(&e));
    p.report.portfolio.reconcile().unwrap();
    use nivra_core::daily_research::{Dataset, evaluate};
    use serde_json::json;
    let body = json!({"status":"success","data":{"candles":[["2026-10-15T00:00:00+05:30",100,102,99,101,1000,0],["2026-10-16T00:00:00+05:30",101,112,100,111,2000,0]]}}).to_string();
    let receipt = json!({"key":"SYNTHETIC|DAILY","unit":"days","start":"2026-10-15","end":"2026-10-16","requested_ms":1792195200000_i64,"received_ms":1792195200001_i64,"body":body,"body_sha256":replay::digest(body.as_bytes())});
    let mut dataset = Dataset {
        schema: 1,
        receipts: vec![receipt.clone()],
    };
    let summary = evaluate(&dataset).unwrap();
    assert_eq!((summary.instruments, summary.bars), (1, 2));
    assert!(
        (summary.ranked_close_returns[0]["unadjusted_close_return"]
            .as_f64()
            .unwrap()
            - 10.0 / 101.0)
            .abs()
            < 1e-12
    );
    dataset.receipts.push(receipt.clone());
    assert_eq!(evaluate(&dataset).unwrap().bars, 2);
    dataset.receipts[0]["body_sha256"] = json!("tampered");
    assert!(evaluate(&dataset).is_err());
    dataset.receipts = vec![receipt];
    dataset.receipts[0]["received_ms"] = json!(0);
    assert!(evaluate(&dataset).is_err());
}
