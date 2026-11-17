use std::{fs, path::Path, process::Command};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nivra"))
}

fn write(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
}

fn compare(dir: &Path, paper_costs: &Path, params: &str) -> std::process::Output {
    let snapshots = dir.join("snapshots.json");
    let candidate = dir.join("candidate.py");
    let params_path = dir.join("params.json");
    let out = dir.join("report.json");
    write(&snapshots, "[]");
    write(&candidate, "def score(data):\n return {}\n");
    write(&params_path, params);
    bin()
        .arg("scorer-compare")
        .arg("--snapshots")
        .arg(&snapshots)
        .arg("--candidate")
        .arg(&candidate)
        .arg("--params")
        .arg(&params_path)
        .arg("--paper-costs")
        .arg(paper_costs)
        .arg("--out")
        .arg(&out)
        .output()
        .unwrap()
}

fn failure_code(output: &std::process::Output) -> String {
    assert!(!output.status.success());
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn scorer_compare_surfaces_stable_paper_cost_codes() {
    let fixture = include_str!("../../../fixtures/paper-costs.json");
    let dir = tempfile::tempdir().unwrap();
    let missing = compare(dir.path(), &dir.path().join("absent.json"), "{}");
    assert!(failure_code(&missing).contains("scorer_compare_paper_costs_missing"));
    let costs = dir.path().join("paper-costs.json");
    write(
        &costs,
        r#"{"per_order":"-1","buy_bps":"5","sell_bps":"7","slippage_bps":"5"}"#,
    );
    let invalid = compare(dir.path(), &costs, "{}");
    assert!(failure_code(&invalid).contains("scorer_compare_paper_costs_invalid"));
    write(&costs, fixture);
    let conflict = compare(dir.path(), &costs, r#"{"per_order_fee":25.0}"#);
    assert!(failure_code(&conflict).contains("scorer_compare_paper_costs_conflict"));
    let reserved = compare(dir.path(), &costs, r#"{"per_order":"0.01"}"#);
    assert!(failure_code(&reserved).contains("scorer_compare_paper_costs_reserved"));
}
