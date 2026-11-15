use sadr_cli::{
    bootstrap_cli::{install_entry, stage_entry},
    bootstrap_stage::Config,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
};
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn write_config(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}
fn fixture() -> (tempfile::TempDir, Config) {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let assets = temp.path().join("assets");
    fs::create_dir(&assets).unwrap();
    let mut binary = vec![0_u8; 120];
    binary[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (offset, value) in [
        (16, 2),
        (18, 62),
        (20, 1),
        (32, 64),
        (52, 64),
        (54, 56),
        (56, 1),
        (64, 1),
        (68, 5),
        (96, 120),
        (104, 120),
    ] {
        binary[offset] = value;
    }
    let binary_sha256 = digest(&binary);
    fs::write(assets.join("sadr-rs-x86_64-unknown-linux-musl"), binary).unwrap();
    fs::write(
        assets.join("sadr-rs-x86_64-unknown-linux-musl.sha256"),
        format!("{binary_sha256}  sadr-rs-x86_64-unknown-linux-musl\n"),
    )
    .unwrap();
    fs::write(assets.join("release.json"), serde_json::to_vec(&serde_json::json!({"tag":"v0.4.2","source_commit":"a".repeat(40),"target":"x86_64-unknown-linux-musl","sha256":binary_sha256})).unwrap()).unwrap();
    let paper = include_bytes!("../../../fixtures/rust/bootstrap-paper.example.json");
    let unit = include_bytes!("../../../packaging/rust/ind-sadr-rust-paper.service");
    let paper_config = write_config(temp.path(), "paper.json", paper);
    let service_unit = write_config(temp.path(), "unit.service", unit);
    let config = Config {
        release_assets: assets,
        paper_config,
        service_unit,
        output: temp.path().join("bundle"),
        install_home: "/home/nivra-example".into(),
        installer_artifact: "/home/nivra-example/reviewed-incoming/bundle".into(),
        version: "v0.4.2".into(),
        source_commit: "a".repeat(40),
        binary_sha256,
        config_sha256: digest(paper),
        unit_sha256: digest(unit),
    };
    (temp, config)
}
#[test]
fn valid_stage_command_only_stages_a_verified_bundle() {
    let (temp, config) = fixture();
    let path = write_config(
        temp.path(),
        "stage.json",
        &serde_json::to_vec(&config).unwrap(),
    );
    assert_eq!(stage_entry(&path), ExitCode::SUCCESS);
    assert_eq!(fs::read_dir(&config.output).unwrap().count(), 6);
    assert_eq!(
        fs::read(config.output.join("paper-20261016.json")).unwrap(),
        fs::read(&config.paper_config).unwrap()
    );
}
#[test]
fn malformed_or_duplicate_stage_inputs_refuse_before_staging() {
    for case in [
        "malformed",
        "unknown",
        "duplicate",
        "oversized",
        "symlink",
        "parent_symlink",
        "hardlink",
        "writable",
    ] {
        let (temp, config) = fixture();
        let mut raw = serde_json::to_vec(&config).unwrap();
        match case {
            "malformed" => raw = b"{private-untrusted-parser-content".to_vec(),
            "unknown" => raw = b"{\"secret_field\":\"SHOULD_NOT_APPEAR_IN_ERROR\"}".to_vec(),
            "duplicate" => {
                raw = String::from_utf8(raw)
                    .unwrap()
                    .replacen('{', "{\"version\":\"v0.4.2\",", 1)
                    .into_bytes()
            }
            "oversized" => raw = vec![b' '; 64 * 1024 + 1],
            _ => {}
        }
        let mut path = write_config(temp.path(), "stage.json", &raw);
        match case {
            "symlink" => {
                let link = temp.path().join("link.json");
                std::os::unix::fs::symlink(&path, &link).unwrap();
                path = link;
            }
            "parent_symlink" => {
                let directory = temp.path().join("parent");
                fs::create_dir(&directory).unwrap();
                fs::rename(&path, directory.join("stage.json")).unwrap();
                let link = temp.path().join("parent-link");
                std::os::unix::fs::symlink(directory, &link).unwrap();
                path = link.join("stage.json");
            }
            "hardlink" => fs::hard_link(&path, temp.path().join("other.json")).unwrap(),
            "writable" => fs::set_permissions(&path, fs::Permissions::from_mode(0o622)).unwrap(),
            _ => {}
        }
        assert_eq!(stage_entry(&path), ExitCode::FAILURE, "{case}");
        assert!(!config.output.exists());
    }
}
#[test]
fn invalid_install_dates_or_schema_refuse_before_any_local_host_adapter() {
    let temp = tempfile::tempdir().unwrap();
    let malformed = write_config(
        temp.path(),
        "invalid.json",
        b"{\"unrecognized\":\"PRIVATE_DO_NOT_ECHO\"}",
    );
    // Every configuration here is rejected before install_local can run.
    for text in [
        "",
        "invalid",
        "2026-10-16",
        "2026-10-16T11:00:00+05:30",
        "2026-10-16T11:00:00-00:00",
        "2026-10-16T11:00:60Z",
        "2026-10-16T11:00:00.1234567891Z",
        "0000-01-01T00:00:00Z",
    ] {
        assert_eq!(install_entry(&malformed, text), ExitCode::FAILURE);
    }
    assert_eq!(
        install_entry(&malformed, "2026-10-16T11:00:00Z"),
        ExitCode::FAILURE
    );
    let config = sadr_cli::bootstrap::Config {
        home: "/home/reviewed".into(),
        artifact: "/reviewed/bundle".into(),
        version: "v0.4.2".into(),
        source_commit: "a".repeat(40),
        binary_sha256: "b".repeat(64),
        config_sha256: "c".repeat(64),
        unit_sha256: "d".repeat(64),
    };
    let duplicate =
        serde_json::to_string(&config)
            .unwrap()
            .replacen('{', "{\"version\":\"v0.4.2\",", 1);
    let path = write_config(temp.path(), "duplicate.json", duplicate.as_bytes());
    assert_eq!(
        install_entry(&path, "2026-10-16T11:00:00Z"),
        ExitCode::FAILURE
    );
    assert_eq!(stage_entry(Path::new("relative.json")), ExitCode::FAILURE);
    assert_eq!(
        install_entry(Path::new("relative.json"), "2026-10-16T11:00:00Z"),
        ExitCode::FAILURE
    );
}
