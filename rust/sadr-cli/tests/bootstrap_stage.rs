use sadr_cli::bootstrap_stage::{Config, stage};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};
const ASSET: &str = "sadr-rs-x86_64-unknown-linux-musl";
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn elf() -> Vec<u8> {
    let mut bytes = vec![0_u8; 120];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16] = 2;
    bytes[18] = 62;
    bytes[20] = 1;
    bytes[32] = 64;
    bytes[52] = 64;
    bytes[54] = 56;
    bytes[56] = 1;
    bytes[64] = 1;
    bytes[68] = 5;
    bytes[96] = 120;
    bytes[104] = 120;
    bytes
}
fn set_release(config: &mut Config, bytes: &[u8]) {
    config.binary_sha256 = digest(bytes);
    fs::write(config.release_assets.join(ASSET), bytes).unwrap();
    fs::write(
        config.release_assets.join(format!("{ASSET}.sha256")),
        format!("{}  {ASSET}\n", config.binary_sha256),
    )
    .unwrap();
    fs::write(config.release_assets.join("release.json"), serde_json::to_vec(&json!({"tag":config.version,"source_commit":config.source_commit,"target":"x86_64-unknown-linux-musl","sha256":config.binary_sha256})).unwrap()).unwrap();
}
fn fixture() -> (tempfile::TempDir, Config) {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let assets = temp.path().join("assets");
    fs::create_dir(&assets).unwrap();
    let config_path = temp.path().join("paper.json");
    let unit_path = temp.path().join("paper.service");
    let paper = include_bytes!("../../../fixtures/rust/bootstrap-paper.example.json");
    let unit = include_bytes!("../../../packaging/rust/ind-sadr-rust-paper.service");
    fs::write(&config_path, paper).unwrap();
    fs::write(&unit_path, unit).unwrap();
    let mut config = Config {
        release_assets: assets,
        paper_config: config_path,
        service_unit: unit_path,
        output: temp.path().join("bundle"),
        install_home: "/home/nivra-example".into(),
        installer_artifact: temp.path().join("bundle"),
        version: "v0.4.2".into(),
        source_commit: "a".repeat(40),
        binary_sha256: String::new(),
        config_sha256: digest(paper),
        unit_sha256: digest(unit),
    };
    set_release(&mut config, &elf());
    (temp, config)
}
#[test]
fn stages_original_config_and_six_exact_checksum_bound_files_without_execution() {
    let (_temp, config) = fixture();
    let result = stage(&config).unwrap();
    assert!(!result.activation_performed);
    assert_eq!(result.artifact, config.output);
    assert_eq!(fs::read_dir(&config.output).unwrap().count(), 6);
    assert_eq!(fs::read(config.output.join("sadr-rs")).unwrap(), elf());
    assert_eq!(
        fs::read(config.output.join("VERSION")).unwrap(),
        b"v0.4.2\n"
    );
    assert_eq!(
        fs::read(config.output.join("SOURCE_COMMIT")).unwrap(),
        format!("{}\n", "a".repeat(40)).as_bytes()
    );
    assert_eq!(
        fs::read(config.output.join("paper-20261016.json")).unwrap(),
        fs::read(&config.paper_config).unwrap()
    );
    assert_eq!(
        fs::read(config.output.join("ind-sadr-rust-paper.service")).unwrap(),
        fs::read(&config.service_unit).unwrap()
    );
    let sums = fs::read_to_string(config.output.join("SHA256SUMS")).unwrap();
    for line in sums.lines() {
        let (sha, name) = line.split_once("  ").unwrap();
        assert_eq!(sha, digest(&fs::read(config.output.join(name)).unwrap()));
    }
    assert_eq!(sums.lines().count(), 5);
    assert_eq!(
        fs::metadata(&config.output).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(config.output.join("sadr-rs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    for name in [
        "VERSION",
        "SOURCE_COMMIT",
        "SHA256SUMS",
        "paper-20261016.json",
        "ind-sadr-rust-paper.service",
    ] {
        assert_eq!(
            fs::metadata(config.output.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert_eq!(
        stage(&config).unwrap_err(),
        "bootstrap_stage_immutable_output_required"
    );
    assert_eq!(fs::read(config.output.join("sadr-rs")).unwrap(), elf());

    // Optional NSE F&O session sections still bind through the strict paper
    // schema (staging only binds; runtime Config::validate owns semantics),
    // while a genuinely unknown key is still refused before any output.
    let (_temp, mut extended) = fixture();
    let mut paper: serde_json::Value =
        serde_json::from_slice(&fs::read(&extended.paper_config).unwrap()).unwrap();
    paper["quote_keys"] = json!({"prefixes": ["NSE_EQ|", "NSE_INDEX|"], "max_keys": 200});
    paper["paper_costs_by_segment"] = json!({
        "NSE_FO": {"per_order":"0.01","buy_bps":"0","sell_bps":"0","slippage_bps":"10","rounding_increment":"0.05"}
    });
    paper["index_feeds"] = json!({
        "enabled": true,
        "keys": ["NSE_INDEX|Nifty 50", "NSE_INDEX|Nifty Bank", "NSE_INDEX|India VIX"]
    });
    let bytes = serde_json::to_vec(&paper).unwrap();
    fs::write(&extended.paper_config, &bytes).unwrap();
    extended.config_sha256 = digest(&bytes);
    assert!(stage(&extended).is_ok());
    let (_temp, mut unknown) = fixture();
    let mut paper: serde_json::Value =
        serde_json::from_slice(&fs::read(&unknown.paper_config).unwrap()).unwrap();
    paper["unknown_section"] = json!({"x": 1});
    let bytes = serde_json::to_vec(&paper).unwrap();
    fs::write(&unknown.paper_config, &bytes).unwrap();
    unknown.config_sha256 = digest(&bytes);
    assert_eq!(
        stage(&unknown).unwrap_err(),
        "bootstrap_stage_invalid_paper_config"
    );
}
#[test]
fn unreviewed_or_mismatched_release_assets_refuse_before_writing() {
    for case in [
        "binary",
        "sidecar",
        "tag",
        "commit",
        "target",
        "manifest_digest",
        "duplicate",
        "unit",
        "paper",
        "missing",
    ] {
        let (_temp, config) = fixture();
        let manifest_path = config.release_assets.join("release.json");
        match case {
            "binary" => fs::write(config.release_assets.join(ASSET), b"unreviewed").unwrap(),
            "sidecar" => fs::write(
                config.release_assets.join(format!("{ASSET}.sha256")),
                format!("{}  ../sadr-rs\n", config.binary_sha256),
            )
            .unwrap(),
            "tag" | "commit" | "target" | "manifest_digest" => {
                let mut manifest: serde_json::Value =
                    serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
                let key = match case {
                    "tag" => "tag",
                    "commit" => "source_commit",
                    "target" => "target",
                    _ => "sha256",
                };
                manifest[key] = json!("changed");
                fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            }
            "duplicate" => {
                let old = fs::read_to_string(&manifest_path).unwrap();
                fs::write(&manifest_path, old.replacen('{', "{\"tag\":\"v0.4.2\",", 1)).unwrap();
            }
            "unit" => fs::write(&config.service_unit, b"unreviewed unit").unwrap(),
            "paper" => fs::write(&config.paper_config, b"{}").unwrap(),
            "missing" => fs::remove_file(&manifest_path).unwrap(),
            _ => unreachable!(),
        }
        assert!(stage(&config).is_err(), "{case}");
        assert!(!config.output.exists(), "{case}");
    }
}
#[test]
fn symlinks_hardlinks_public_parents_and_target_home_are_refused() {
    for case in [
        "asset_symlink",
        "config_symlink",
        "binary_hardlink",
        "output_symlink",
        "public_parent",
        "target_home",
    ] {
        let (temp, mut config) = fixture();
        match case {
            "asset_symlink" => {
                let link = temp.path().join("assets-link");
                std::os::unix::fs::symlink(&config.release_assets, &link).unwrap();
                config.release_assets = link;
            }
            "config_symlink" => {
                let link = temp.path().join("config-link");
                std::os::unix::fs::symlink(&config.paper_config, &link).unwrap();
                config.paper_config = link;
            }
            "binary_hardlink" => fs::hard_link(
                config.release_assets.join(ASSET),
                temp.path().join("binary-link"),
            )
            .unwrap(),
            "output_symlink" => {
                std::os::unix::fs::symlink(temp.path().join("outside"), &config.output).unwrap()
            }
            "public_parent" => {
                fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap()
            }
            "target_home" => config.output = Path::new("/home/nivra-example").join("bundle"),
            _ => unreachable!(),
        }
        assert!(stage(&config).is_err(), "{case}");
        assert!(!config.output.join("SHA256SUMS").exists());
    }
}

#[test]
fn staged_bundle_is_consumable_by_rust_installer_with_explicit_synthetic_binding() {
    use chrono::{DateTime, Utc};
    use sadr_cli::bootstrap::{self, Host, Inactivity};
    struct OfflineHost {
        reloads: usize,
    }
    impl Host for OfflineHost {
        fn trusted_now(&mut self) -> bootstrap::Result<DateTime<Utc>> {
            Ok("2026-10-16T11:00:00Z".parse().unwrap())
        }
        fn inactivity(&mut self) -> bootstrap::Result<Inactivity> {
            Ok(Inactivity::Inactive)
        }
        fn verify_binary_and_config(
            &mut self,
            binary: &Path,
            config: &Path,
            version: &str,
        ) -> bootstrap::Result<()> {
            assert_eq!(fs::read(binary).unwrap(), elf());
            assert!(config.is_file());
            assert_eq!(version, "v0.4.2");
            Ok(())
        }
        fn random_token(&mut self, bytes: &mut [u8; 32]) -> bootstrap::Result<()> {
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = index as u8;
            }
            Ok(())
        }
        fn daemon_reload(&mut self) -> bootstrap::Result<()> {
            self.reloads += 1;
            Ok(())
        }
    }
    let (temp, mut config) = fixture();
    let home = temp.path().join("explicit-synthetic-home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paper: serde_json::Value =
        serde_json::from_slice(&fs::read(&config.paper_config).unwrap()).unwrap();
    for (key, relative) in [
        ("directory", "ind-sadr-data/rust-paper-20261016"),
        ("token_file", ".config/ind-sadr/upstox_analytics.token"),
        (
            "dashboard_token_file",
            ".config/ind-sadr/rust-dashboard.token",
        ),
        ("read_journal", ".config/ind-sadr/upstox-shared-reads.db"),
    ] {
        paper[key] = json!(home.join(relative));
    }
    let bytes = serde_json::to_vec(&paper).unwrap();
    fs::write(&config.paper_config, &bytes).unwrap();
    config.config_sha256 = digest(&bytes);
    config.install_home = home.clone();
    let staged = stage(&config).unwrap();
    let installer = staged.installer_config();
    let mut host = OfflineHost { reloads: 0 };
    let installed = bootstrap::install(&installer, &mut host).unwrap();
    assert!(!installed.activation_performed);
    assert_eq!(host.reloads, 1);
    assert_eq!(
        fs::read(home.join("ind-sadr-rust-current/sadr-rs")).unwrap(),
        elf()
    );
    assert_eq!(
        fs::read(home.join(".config/ind-sadr/rust-paper.json")).unwrap(),
        bytes
    );
    assert!(
        !home
            .join(".config/ind-sadr/upstox_analytics.token")
            .exists()
    );
    assert!(
        !home
            .join(".config/ind-sadr/upstox-shared-reads.db")
            .exists()
    );
}
