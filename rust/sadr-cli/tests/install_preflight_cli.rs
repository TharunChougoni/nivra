use std::{fs, process::Command};
#[test]
fn preflight_cli_is_read_only_redacted_and_confers_no_install_authority() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("paper.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY,body TEXT)")
        .unwrap();
    db.execute("INSERT INTO state VALUES(1,?)",[r#"{"cash":"50000","halted":false,"stopped":false,"closed":false,"units":0,"fills":[],"synthetic_private_field":"must-not-appear"}"#]).unwrap();
    drop(db);
    let original = fs::read(&path).unwrap();
    let run = |version: &str| {
        Command::new(env!("CARGO_BIN_EXE_sadr-rs"))
            .args(["paper-install-preflight", "--version", version, "--state"])
            .arg(&path)
            .output()
            .unwrap()
    };
    let good = run("v0.4.1");
    assert!(good.status.success());
    assert_eq!(
        String::from_utf8(good.stdout).unwrap().trim(),
        "pristine_original_book_verified_no_install_authority"
    );
    assert!(good.stderr.is_empty());
    let bad = run("v0.4.2");
    assert_eq!(bad.status.code(), Some(1));
    assert!(bad.stdout.is_empty());
    assert_eq!(
        String::from_utf8(bad.stderr).unwrap().trim(),
        "pristine_upgrade_refused"
    );
    assert_eq!(fs::read(&path).unwrap(), original);
}
