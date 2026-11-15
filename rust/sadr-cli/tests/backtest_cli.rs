use std::{fs, process::Command};
#[test]
fn offline_cli_produces_reconciled_report_and_retains_exact_bindings() {
    let t = tempfile::tempdir().unwrap();
    let example = sadr_core::replay::example();
    let m = t.path().join("manifest.json");
    let e = t.path().join("events.ndjson");
    fs::write(&m, example["manifest"].to_string()).unwrap();
    fs::write(&e, example["events"].as_str().unwrap()).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_sadr-rs"))
        .args(["backtest", "--manifest"])
        .arg(&m)
        .arg("--events")
        .arg(&e)
        .output()
        .unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        report["engine_hash"],
        sadr_core::replay::engine_fingerprint()
    );
    assert_eq!(report["report"]["promotion_authorized"], false);
    assert_eq!(
        report["report"]["portfolio"]["fills"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
