use rusqlite::Connection;
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    process::Command,
};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_nivra"))
}

fn write_private(path: &Path, body: &str) {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(body.as_bytes()).unwrap();
}

fn write_config(
    dir: &Path,
    token: &Path,
    journal: &Path,
    device: u64,
    inode: u64,
) -> std::path::PathBuf {
    let path = dir.join("paper.json");
    let body = serde_json::json!({
        "account": "tharun-upstox",
        "session": "2026-10-16",
        "previous_session": "2026-10-15",
        "calendar_source": "NSE/CMTR/71775",
        "directory": dir.join("data"),
        "sessions_root": dir.join("sessions"),
        "token_file": token,
        "dashboard_token_file": dir.join("dashboard.token"),
        "read_journal": journal,
        "read_scope": "upstox-market-tharun-upstox",
        "read_journal_device": device,
        "read_journal_inode": inode,
        "strategy": "model_planner_v1",
        "paper_costs": {"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"5","rounding_increment":"0.01"},
    });
    fs::write(&path, serde_json::to_vec(&body).unwrap()).unwrap();
    path
}

fn journal(dir: &Path) -> (std::path::PathBuf, u64, u64) {
    let path = dir.join("reads.db");
    let db = Connection::open(&path).unwrap();
    db.execute_batch(
        "CREATE TABLE read_requests(scope TEXT NOT NULL,at REAL NOT NULL);CREATE INDEX read_window ON read_requests(scope,at);",
    )
    .unwrap();
    let meta = fs::metadata(&path).unwrap();
    (path, meta.dev(), meta.ino())
}

#[test]
fn probe_refuses_missing_paper_costs_before_reading_credentials_or_journal() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        &dir.path().join("missing-token"),
        &dir.path().join("missing-journal"),
        1,
        1,
    );
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("paper_costs");
    fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    let result = bin()
        .args(["probe", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["read_failure_reason"], "paper_costs_required");
}
#[test]
fn probe_reports_credential_format_for_raw_invalid_token_without_echo() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("upstox.token");
    write_private(
        &token,
        "UPSTOX_ANALYTICS_TOKEN=must-not-leak-secret-value\nsecond-line\n",
    );
    let (journal, device, inode) = journal(dir.path());
    let config = write_config(dir.path(), &token, &journal, device, inode);
    let output = bin()
        .args(["probe", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("must-not-leak-secret-value"));
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["read_only_http_ok"], false);
    assert_eq!(report["executable_depth"], false);
    assert_eq!(report["secrets_emitted"], false);
    assert_eq!(report["read_failure_reason"], "credential_format");
}

#[test]
fn probe_accepts_raw_token_file_then_fails_closed_without_http_if_unreadable_journal() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("upstox.token");
    write_private(&token, "eyJhbGciOiJub25lIn0.eyJzdWIiOiJmaXh0dXJlIn0.sig\n");
    let config = write_config(dir.path(), &token, &dir.path().join("missing.db"), 1, 1);
    let output = bin()
        .args(["probe", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["read_failure_reason"],
        "original_read_journal_changed"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("eyJ"));
}

#[test]
fn probe_reports_token_file_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let (journal, device, inode) = journal(dir.path());
    let config = write_config(
        dir.path(),
        &dir.path().join("missing.token"),
        &journal,
        device,
        inode,
    );
    let output = bin()
        .args(["probe", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["read_failure_reason"], "token_file_unreadable");
}

#[test]
fn resume_requires_confirm_and_refuses_missing_book() {
    let dir = tempfile::tempdir().unwrap();
    let token = dir.path().join("upstox.token");
    write_private(&token, "fixture-token\n");
    let (journal, device, inode) = journal(dir.path());
    let config = write_config(dir.path(), &token, &journal, device, inode);
    let output = bin()
        .args(["resume", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "resume_confirm_paper_review_required"
    );
    let output = bin()
        .args(["resume", "--config"])
        .arg(&config)
        .arg("--confirm-paper-review")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "resume_book_missing"
    );
}

#[test]
fn version_matches_package() {
    let output = bin().arg("--version").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        text.trim(),
        format!("nivra-cli {}", env!("CARGO_PKG_VERSION"))
    );
    for version in [
        env!("CARGO_PKG_VERSION").to_owned(),
        format!("v{}", env!("CARGO_PKG_VERSION")),
    ] {
        assert!(
            bin()
                .args(["self-check", "--expected-version", &version])
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    let mismatch = bin()
        .args(["self-check", "--expected-version", "v0.0.0"])
        .output()
        .unwrap();
    assert_eq!(mismatch.status.code(), Some(1));
    let reason = String::from_utf8(mismatch.stderr).unwrap();
    assert!(reason.contains("self_check_version_mismatch: expected=v0.0.0"));
    assert!(reason.contains(&format!("actual=v{}", env!("CARGO_PKG_VERSION"))));
    let missing = Path::new("/nonexistent-fixture-config");
    for (command, prefix) in [
        (
            "planner-recover",
            "planner_recover_refused: config_unreadable",
        ),
        ("config-apply", "config_apply_refused: config_unreadable"),
    ] {
        let refused = bin()
            .args([command, "--config"])
            .arg(missing)
            .output()
            .unwrap();
        assert_eq!(refused.status.code(), Some(1));
        assert_eq!(String::from_utf8(refused.stderr).unwrap().trim(), prefix);
    }
}
