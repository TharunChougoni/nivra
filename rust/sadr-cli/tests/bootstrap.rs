use chrono::{DateTime, Utc};
use sadr_cli::bootstrap::{Config, Host, Inactivity, Result, install};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

struct Fake {
    now: DateTime<Utc>,
    state: Inactivity,
    reload: usize,
    checks: usize,
    entropy: bool,
    fail_check: bool,
    fail_reload: bool,
    become_active: bool,
    probes: usize,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            now: "2026-10-16T11:00:00Z".parse().unwrap(),
            state: Inactivity::Inactive,
            reload: 0,
            checks: 0,
            entropy: true,
            fail_check: false,
            fail_reload: false,
            become_active: false,
            probes: 0,
        }
    }
}
impl Host for Fake {
    fn trusted_now(&mut self) -> Result<DateTime<Utc>> {
        Ok(self.now)
    }
    fn inactivity(&mut self) -> Result<Inactivity> {
        self.probes += 1;
        Ok(if self.become_active && self.probes > 2 {
            Inactivity::Active
        } else {
            self.state
        })
    }
    fn verify_binary_and_config(
        &mut self,
        binary: &Path,
        config: &Path,
        version: &str,
    ) -> Result<()> {
        assert!(binary.to_str().unwrap().starts_with("/proc/self/fd/"));
        assert!(binary.is_file() && config.is_file());
        assert_eq!(version, "v0.4.2");
        self.checks += 1;
        if self.fail_check {
            Err("fake_check_failed")
        } else {
            Ok(())
        }
    }
    fn random_token(&mut self, bytes: &mut [u8; 32]) -> Result<()> {
        if self.entropy {
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = index as u8;
            }
        }
        Ok(())
    }
    fn daemon_reload(&mut self) -> Result<()> {
        self.reload += 1;
        if self.fail_reload {
            Err("fake_reload_failed")
        } else {
            Ok(())
        }
    }
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn fixture() -> (tempfile::TempDir, Config) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let artifact = tmp.path().join("artifact");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&artifact).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let config = serde_json::json!({"session":"2026-10-16", "directory":home.join("ind-sadr-data/rust-paper-20261016"), "dashboard_token_file":home.join(".config/ind-sadr/rust-dashboard.token")});
    let files = [
        ("sadr-rs", b"synthetic binary never executed".to_vec()),
        ("VERSION", b"v0.4.2\n".to_vec()),
        (
            "SOURCE_COMMIT",
            format!("{}\n", "a".repeat(40)).into_bytes(),
        ),
        ("paper-20261016.json", serde_json::to_vec(&config).unwrap()),
        (
            "ind-sadr-rust-paper.service",
            include_bytes!("../../../packaging/rust/ind-sadr-rust-paper.service").to_vec(),
        ),
    ];
    let mut sums = String::new();
    for (name, bytes) in &files {
        fs::write(artifact.join(name), bytes).unwrap();
        sums.push_str(&format!("{}  {name}\n", digest(bytes)));
    }
    fs::write(artifact.join("SHA256SUMS"), sums).unwrap();
    let cfg = Config {
        home,
        artifact,
        version: "v0.4.2".into(),
        source_commit: "a".repeat(40),
        binary_sha256: digest(&files[0].1),
        config_sha256: digest(&files[3].1),
        unit_sha256: digest(&files[4].1),
    };
    (tmp, cfg)
}
#[test]
fn installs_real_files_without_activation_and_refuses_retry() {
    let (_tmp, config) = fixture();
    let mut host = Fake::default();
    let installed = install(&config, &mut host).unwrap();
    assert!(!installed.activation_performed);
    assert_eq!(host.reload, 1);
    assert_eq!(host.checks, 1);
    assert_eq!(
        fs::read(config.home.join("ind-sadr-rust-current/sadr-rs")).unwrap(),
        b"synthetic binary never executed"
    );
    let token = config.home.join(".config/ind-sadr/rust-dashboard.token");
    assert_eq!(
        fs::metadata(&token).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(&token).unwrap().len(), 64);
    assert!(
        !config
            .home
            .join("ind-sadr-deploy/bootstrap-pending.json")
            .exists()
    );
    assert_eq!(
        install(&config, &mut host).unwrap_err(),
        "bootstrap_existing_or_interrupted_install"
    );
    assert!(installed.release.exists());
}
#[test]
fn frozen_shell_time_decisions() {
    let vectors: serde_json::Value = serde_json::from_str(include_str!(
        "../../../fixtures/rust/bootstrap_vectors.json"
    ))
    .unwrap();
    for row in vectors["time_vectors"].as_array().unwrap() {
        let (_tmp, config) = fixture();
        let mut host = Fake {
            now: row["utc"].as_str().unwrap().parse().unwrap(),
            ..Fake::default()
        };
        assert_eq!(
            install(&config, &mut host).is_ok(),
            row["allowed"].as_bool().unwrap()
        );
    }
}
#[test]
fn guards_refuse_hold_active_unknown_and_tampering_without_release() {
    for case in [
        "hold",
        "active",
        "unknown",
        "tamper",
        "version",
        "revision",
        "checksum_path",
        "missing_checksum",
    ] {
        let (_tmp, mut config) = fixture();
        let mut host = Fake::default();
        match case {
            "hold" => {
                fs::create_dir(config.home.join("ind-sadr-deploy")).unwrap();
                fs::write(config.home.join("ind-sadr-deploy/HOLD"), b"").unwrap();
            }
            "active" => host.state = Inactivity::Active,
            "unknown" => host.state = Inactivity::Unknown,
            "tamper" => fs::write(config.artifact.join("sadr-rs"), b"changed").unwrap(),
            "version" => config.version = "v00.4.2".into(),
            "revision" => config.source_commit = "../bad".into(),
            "checksum_path" => fs::write(
                config.artifact.join("SHA256SUMS"),
                format!("{}  ../other\n", "a".repeat(64)),
            )
            .unwrap(),
            "missing_checksum" => fs::write(config.artifact.join("SHA256SUMS"), "").unwrap(),
            _ => unreachable!(),
        }
        assert!(install(&config, &mut host).is_err(), "{case}");
        assert!(!config.home.join("ind-sadr-rust-current").exists());
        assert_eq!(host.reload, 0);
    }
}
#[test]
fn original_configuration_and_existing_token_are_preserved() {
    let (_tmp, config) = fixture();
    for relative in [".config", ".config/ind-sadr"] {
        fs::create_dir(config.home.join(relative)).unwrap();
        fs::set_permissions(
            config.home.join(relative),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let token = config.home.join(".config/ind-sadr/rust-dashboard.token");
    fs::write(&token, b"existing-private-test-token").unwrap();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o600)).unwrap();
    let path = config.home.join(".config/ind-sadr/rust-paper.json");
    fs::write(&path, b"original binding").unwrap();
    assert_eq!(
        install(&config, &mut Fake::default()).unwrap_err(),
        "bootstrap_original_binding_mismatch"
    );
    assert_eq!(fs::read(&path).unwrap(), b"original binding");
    fs::remove_file(&path).unwrap();
    install(&config, &mut Fake::default()).unwrap();
    assert_eq!(fs::read(&token).unwrap(), b"existing-private-test-token");

    // The strict bootstrap binding accepts the optional NSE F&O session
    // sections (bootstrap only binds; runtime Config::validate owns semantics)
    // and still refuses a genuinely unknown key.
    let private = config.home.join(".config/ind-sadr");
    let paper = serde_json::json!({
        "account": "tharun-upstox",
        "session": "2026-10-16",
        "previous_session": "2026-10-15",
        "calendar_source": "NSE/CMTR/71775",
        "directory": config.home.join("ind-sadr-data/rust-paper-20261016"),
        "token_file": private.join("upstox_analytics.token"),
        "dashboard_token_file": private.join("rust-dashboard.token"),
        "read_journal": private.join("upstox-shared-reads.db"),
        "read_scope": "upstox-market-tharun-upstox",
        "strategy": "model_planner_v1",
        "capital": "5000",
        "read_journal_device": 66305,
        "read_journal_inode": 1787490,
        "paper_costs": {"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"5","rounding_increment":"0.01"},
        "quote_keys": {"prefixes": ["NSE_EQ|", "NSE_INDEX|"], "max_keys": 200},
        "paper_costs_by_segment": {"NSE_FO": {"per_order":"0.01","buy_bps":"0","sell_bps":"0","slippage_bps":"10","rounding_increment":"0.05"}},
        "index_feeds": {"enabled": true, "keys": ["NSE_INDEX|Nifty 50", "NSE_INDEX|Nifty Bank", "NSE_INDEX|India VIX"]}
    });
    fs::write(&path, serde_json::to_vec(&paper).unwrap()).unwrap();
    sadr_cli::bootstrap::paper_config_binding(&path, &config.home).unwrap();
    let mut unknown = paper;
    unknown["unknown_section"] = serde_json::json!({"x": 1});
    fs::write(&path, serde_json::to_vec(&unknown).unwrap()).unwrap();
    assert_eq!(
        sadr_cli::bootstrap::paper_config_binding(&path, &config.home).unwrap_err(),
        "bootstrap_invalid_paper_config"
    );
}
#[test]
fn failures_preserve_immutable_release_and_pending_evidence() {
    for case in ["self_check", "entropy", "reload", "late_active"] {
        let (_tmp, config) = fixture();
        let mut host = Fake {
            fail_check: case == "self_check",
            entropy: case != "entropy",
            fail_reload: case == "reload",
            become_active: case == "late_active",
            ..Fake::default()
        };
        assert!(install(&config, &mut host).is_err());
        assert!(
            config
                .home
                .join("ind-sadr-deploy/bootstrap-pending.json")
                .exists()
        );
        assert_eq!(
            fs::read_dir(config.home.join("ind-sadr-rust-releases"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(
            install(&config, &mut Fake::default()).unwrap_err(),
            "bootstrap_existing_or_interrupted_install"
        );
    }
}
#[test]
fn symlinks_existing_books_and_public_roots_refuse() {
    for case in [
        "artifact_symlink",
        "home_public",
        "book",
        "release",
        "unit_symlink",
    ] {
        let (_tmp, mut config) = fixture();
        match case {
            "artifact_symlink" => {
                let path = config.artifact.with_file_name("link");
                std::os::unix::fs::symlink(&config.artifact, &path).unwrap();
                config.artifact = path;
            }
            "home_public" => {
                fs::set_permissions(&config.home, fs::Permissions::from_mode(0o755)).unwrap()
            }
            "book" => {
                fs::create_dir_all(config.home.join("ind-sadr-data/rust-paper-20261016")).unwrap()
            }
            "release" => {
                let root = config.home.join("ind-sadr-rust-releases");
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                fs::create_dir(root.join(format!("{}-{}", config.version, config.source_commit)))
                    .unwrap();
            }
            "unit_symlink" => {
                for relative in [".config", ".config/systemd", ".config/systemd/user"] {
                    fs::create_dir(config.home.join(relative)).unwrap();
                    fs::set_permissions(
                        config.home.join(relative),
                        fs::Permissions::from_mode(0o700),
                    )
                    .unwrap();
                }
                std::os::unix::fs::symlink(
                    config.artifact.join("ind-sadr-rust-paper.service"),
                    config
                        .home
                        .join(".config/systemd/user/ind-sadr-rust-paper.service"),
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(install(&config, &mut Fake::default()).is_err(), "{case}");
        assert!(!config.home.join("ind-sadr-rust-current").exists());
    }
}

#[test]
fn shared_deployment_lock_is_nonblocking() {
    use fs2::FileExt;
    let (_tmp, config) = fixture();
    let deploy = config.home.join("ind-sadr-deploy");
    fs::create_dir(&deploy).unwrap();
    fs::set_permissions(&deploy, fs::Permissions::from_mode(0o700)).unwrap();
    let lock = fs::File::create(deploy.join(".lock")).unwrap();
    lock.lock_exclusive().unwrap();
    assert_eq!(
        install(&config, &mut Fake::default()).unwrap_err(),
        "bootstrap_lock_busy"
    );
    assert!(!config.home.join("ind-sadr-rust-current").exists());
}

#[test]
fn callback_cannot_redirect_namespace_writes_or_publish_modified_release() {
    struct AttackHost {
        fake: Fake,
        home: std::path::PathBuf,
        outside: std::path::PathBuf,
        attack: &'static str,
    }
    impl Host for AttackHost {
        fn trusted_now(&mut self) -> Result<DateTime<Utc>> {
            self.fake.trusted_now()
        }
        fn inactivity(&mut self) -> Result<Inactivity> {
            self.fake.inactivity()
        }
        fn verify_binary_and_config(
            &mut self,
            binary: &Path,
            config: &Path,
            version: &str,
        ) -> Result<()> {
            self.fake
                .verify_binary_and_config(binary, config, version)?;
            match self.attack {
                "late_config_delete" => {}
                "existing_token_delete" => {
                    fs::remove_file(self.home.join(".config/ind-sadr/rust-dashboard.token"))
                        .unwrap();
                }
                "new_token" => {
                    fs::create_dir_all(self.home.join(".config/ind-sadr")).unwrap();
                    fs::write(
                        self.home.join(".config/ind-sadr/rust-dashboard.token"),
                        b"raced synthetic token",
                    )
                    .unwrap();
                }
                "binary" => fs::write(binary, b"replaced after check").unwrap(),
                "config" => fs::write(config, b"{}").unwrap(),
                "config_namespace" | "release_namespace" => {
                    let name = if self.attack == "config_namespace" {
                        ".config"
                    } else {
                        "ind-sadr-rust-releases"
                    };
                    fs::rename(
                        self.home.join(name),
                        self.home.join(format!("{name}-original")),
                    )
                    .unwrap();
                    std::os::unix::fs::symlink(&self.outside, self.home.join(name)).unwrap();
                }
                _ => unreachable!(),
            }
            Ok(())
        }
        fn random_token(&mut self, bytes: &mut [u8; 32]) -> Result<()> {
            if self.attack == "late_config_delete" {
                fs::remove_file(self.home.join(".config/ind-sadr/rust-paper.json")).unwrap();
            }
            self.fake.random_token(bytes)
        }
        fn daemon_reload(&mut self) -> Result<()> {
            self.fake.daemon_reload()
        }
    }
    for attack in [
        "binary",
        "config",
        "config_namespace",
        "release_namespace",
        "late_config_delete",
        "existing_token_delete",
        "new_token",
    ] {
        let (tmp, config) = fixture();
        if attack == "existing_token_delete" {
            let dir = config.home.join(".config/ind-sadr");
            fs::create_dir_all(&dir).unwrap();
            for path in [config.home.join(".config"), dir.clone()] {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
            }
            fs::write(
                dir.join("rust-dashboard.token"),
                b"original synthetic token",
            )
            .unwrap();
            fs::set_permissions(
                dir.join("rust-dashboard.token"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        let outside = tmp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        let mut host = AttackHost {
            fake: Fake::default(),
            home: config.home.clone(),
            outside: outside.clone(),
            attack,
        };
        assert!(install(&config, &mut host).is_err(), "{attack}");
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0, "{attack}");
        assert!(!config.home.join("ind-sadr-rust-current").exists());
        assert!(
            config
                .home
                .join("ind-sadr-deploy/bootstrap-pending.json")
                .exists()
        );
        assert_eq!(host.fake.reload, 0);
    }
}

#[test]
fn reload_callback_cannot_complete_a_corrupt_install() {
    struct ReloadAttack {
        fake: Fake,
        home: std::path::PathBuf,
        attack: &'static str,
    }
    impl Host for ReloadAttack {
        fn trusted_now(&mut self) -> Result<DateTime<Utc>> {
            self.fake.trusted_now()
        }
        fn inactivity(&mut self) -> Result<Inactivity> {
            self.fake.inactivity()
        }
        fn verify_binary_and_config(
            &mut self,
            binary: &Path,
            config: &Path,
            version: &str,
        ) -> Result<()> {
            self.fake.verify_binary_and_config(binary, config, version)
        }
        fn random_token(&mut self, bytes: &mut [u8; 32]) -> Result<()> {
            self.fake.random_token(bytes)
        }
        fn daemon_reload(&mut self) -> Result<()> {
            match self.attack {
                "config" => {
                    fs::write(self.home.join(".config/ind-sadr/rust-paper.json"), b"{}").unwrap()
                }
                "unit" => fs::write(
                    self.home
                        .join(".config/systemd/user/ind-sadr-rust-paper.service"),
                    b"changed",
                )
                .unwrap(),
                "token" => {
                    let path = self.home.join(".config/ind-sadr/rust-dashboard.token");
                    fs::rename(&path, path.with_extension("original")).unwrap();
                    fs::write(&path, b"replacement").unwrap();
                }
                "data" => {
                    let path = self.home.join("ind-sadr-data/rust-paper-20261016");
                    fs::rename(&path, path.with_extension("original")).unwrap();
                    fs::create_dir(&path).unwrap();
                }
                _ => unreachable!(),
            }
            self.fake.daemon_reload()
        }
    }
    for attack in ["config", "unit", "token", "data"] {
        let (_tmp, config) = fixture();
        let mut host = ReloadAttack {
            fake: Fake::default(),
            home: config.home.clone(),
            attack,
        };
        assert!(install(&config, &mut host).is_err(), "{attack}");
        assert!(
            config
                .home
                .join("ind-sadr-deploy/bootstrap-pending.json")
                .exists()
        );
        assert!(
            config.home.join("ind-sadr-rust-current").exists(),
            "failed publication evidence stays visible"
        );
        assert_eq!(host.fake.reload, 1);
    }
}

#[test]
fn dangling_enablement_and_shared_credentials_refuse() {
    for attack in ["wants", "alias", "token-hardlink", "lock-hardlink"] {
        let (_tmp, config) = fixture();
        let units = config.home.join(".config/systemd/user");
        fs::create_dir_all(&units).unwrap();
        for path in [
            config.home.join(".config"),
            config.home.join(".config/systemd"),
            units.clone(),
        ] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        match attack {
            "wants" => {
                let wants = units.join("default.target.wants");
                fs::create_dir(&wants).unwrap();
                std::os::unix::fs::symlink(
                    "../ind-sadr-rust-paper.service",
                    wants.join("ind-sadr-rust-paper.service"),
                )
                .unwrap();
            }
            "alias" => std::os::unix::fs::symlink(
                "ind-sadr-rust-paper.service",
                units.join("alias.service"),
            )
            .unwrap(),
            "token-hardlink" | "lock-hardlink" => {
                let (dir, filename) = if attack == "token-hardlink" {
                    (config.home.join(".config/ind-sadr"), "rust-dashboard.token")
                } else {
                    (config.home.join("ind-sadr-deploy"), ".lock")
                };
                fs::create_dir(&dir).unwrap();
                fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
                fs::write(dir.join(filename), b"synthetic retained bytes").unwrap();
                fs::set_permissions(dir.join(filename), fs::Permissions::from_mode(0o600)).unwrap();
                fs::hard_link(dir.join(filename), config.home.join("shared-original")).unwrap();
            }
            _ => unreachable!(),
        }
        let mut host = Fake::default();
        assert!(install(&config, &mut host).is_err(), "{attack}");
        assert_eq!(host.reload, 0);
        assert!(!config.home.join("ind-sadr-rust-current").exists());
    }
}
