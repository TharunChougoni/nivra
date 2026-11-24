use super::*;

#[test]
fn real_updater_keeps_the_controller_stopped_when_requested() {
    let mut host = host::RealHost {
        agent: ureq::Agent::new_with_defaults(),
        current: PathBuf::new(),
        token_file: PathBuf::new(),
        remote: String::new(),
        cache: PathBuf::new(),
        paper: PathBuf::new(),
        now: None,
        operator_stopped: false,
    };
    assert!(!host.keep_stopped());
    host.operator_stopped = true;
    assert!(host.keep_stopped());
}

struct Fake {
    home: PathBuf,
    now: chrono::DateTime<chrono::FixedOffset>,
    session_safe: bool,
    corrupt: bool,
    smoke_fail: bool,
    after_smoke: Option<chrono::DateTime<chrono::FixedOffset>>,
    restarts: Vec<PathBuf>,
    health_checks: Vec<PathBuf>,
    fail_next_health: bool,
    stops: usize,
    fail_post_stop_preflight: bool,
    keep_stopped: bool,
    assets_pending: bool,
    rollback_preflight_fail: bool,
    rollback_preflights: Vec<PathBuf>,
    open_positions: u64,
    pending_orders: u64,
    halted: bool,
    fail_stop: bool,
    swap_failures: usize,
    swap_before_failure: bool,
    fail_restart: bool,
    installed_version: Option<String>,
}
impl Fake {
    fn new(home: &Path) -> Self {
        Self {
            home: home.into(),
            now: "2026-10-17T12:00:00+05:30".parse().unwrap(),
            session_safe: true,
            corrupt: false,
            smoke_fail: false,
            after_smoke: None,
            restarts: vec![],
            health_checks: vec![],
            fail_next_health: false,
            stops: 0,
            fail_post_stop_preflight: false,
            keep_stopped: false,
            assets_pending: false,
            rollback_preflight_fail: false,
            rollback_preflights: vec![],
            open_positions: 0,
            pending_orders: 0,
            halted: false,
            fail_stop: false,
            swap_failures: 0,
            swap_before_failure: false,
            fail_restart: false,
            installed_version: None,
        }
    }
}
impl Host for Fake {
    fn keep_stopped(&self) -> bool {
        self.keep_stopped
    }
    fn fetch(&mut self, name: &str, _: usize) -> Result<Vec<u8>> {
        if self.assets_pending && name.ends_with("release.json") {
            return Err("release_assets_pending");
        }
        let sha = "a".repeat(40);
        let sum = digest(b"candidate");
        Ok(if name == "tags" {
            format!(
                "{}\trefs/tags/v0.6.0\n{sha}\trefs/tags/v0.6.0^{{}}\n{}\trefs/tags/v99.0.0-rc1\n",
                "b".repeat(40),
                "c".repeat(40)
            )
            .into_bytes()
        } else if name.ends_with("release.json") {
            serde_json::json!({"tag":"v0.6.0","source_commit":sha,"target":TARGET,"sha256":sum})
                .to_string()
                .into_bytes()
        } else if name.ends_with(".sha256") {
            format!("{sum}  {ASSET}\n").into_bytes()
        } else if self.corrupt {
            b"bad".to_vec()
        } else {
            b"candidate".to_vec()
        })
    }
    fn version(&mut self, binary: &Path) -> Result<String> {
        if let Some(version) = &self.installed_version {
            return Ok(version.clone());
        }
        Ok(if fs::read(binary).unwrap() == b"candidate" {
            "v0.6.0"
        } else {
            "v0.5.46"
        }
        .into())
    }
    fn stop(&mut self) -> Result<()> {
        self.stops += 1;
        if self.fail_stop {
            return Err("update_stop_failed");
        }
        Ok(())
    }
    fn guard(&mut self, force: bool) -> Result<Admission> {
        if !self.session_safe {
            return Ok(Admission::Deferred("deferred_book_not_flat"));
        }
        let market = !window(self.now, true);
        let open = self.open_positions > 0;
        let pending = self.pending_orders > 0;
        if (open || pending) && self.halted {
            return Ok(Admission::Deferred("deferred_halted_book"));
        }
        if open || pending {
            let exposure = if open {
                "open_positions"
            } else {
                "pending_orders"
            };
            if force {
                return Ok(Admission::Forced(if market {
                    if open {
                        "market_open_open_positions"
                    } else {
                        "market_open_pending_orders"
                    }
                } else {
                    exposure
                }));
            }
            return Ok(Admission::Deferred(match (market, exposure) {
                (true, "open_positions") => "deferred_market_open_open_positions",
                (true, _) => "deferred_market_open_pending_orders",
                (false, "open_positions") => "deferred_open_positions",
                (false, _) => "deferred_pending_orders",
            }));
        }
        Ok(if market {
            if force {
                Admission::Forced("market_open_flat")
            } else {
                Admission::Deferred("deferred_market_open")
            }
        } else {
            Admission::Safe
        })
    }
    fn smoke(&mut self, _: &Path, _: &str) -> Result<()> {
        if self.smoke_fail {
            return Err("fake_smoke_failed");
        }
        if self.fail_post_stop_preflight && self.stops > 0 {
            return Err("update_preflight_failed");
        }
        if let Some(now) = self.after_smoke {
            self.now = now;
        }
        Ok(())
    }
    fn rollback_preflight(&mut self, binary: &Path) -> Result<()> {
        self.rollback_preflights.push(binary.into());
        if self.rollback_preflight_fail {
            return Err("update_rollback_config_incompatible");
        }
        Ok(())
    }
    fn restart(&mut self) -> Result<()> {
        self.restarts
            .push(fs::read_link(self.home.join("nivra-current")).unwrap());
        if self.fail_restart {
            return Err("update_restart_failed");
        }
        Ok(())
    }
    fn swap(&mut self, current: &Path, target: &Path) -> Result<()> {
        if self.swap_failures > 0 {
            self.swap_failures -= 1;
            if self.swap_before_failure {
                swap(current, target)?;
            }
            return Err("update_swap_failed");
        }
        swap(current, target)
    }
    fn healthy(&mut self) -> Result<()> {
        self.health_checks
            .push(fs::read_link(self.home.join("nivra-current")).unwrap());
        if self.fail_next_health {
            // The restarted controller reports its open book operator-stopped.
            self.fail_next_health = false;
            paper_health_at(
                br#"{"paper_only":true,"live_enabled":false,"status":"stopped_operator_review_required","as_of_ms":1000000,"book":{"stopped":true,"closed":false}}"#,
                "v0.6.0",
                1_000_000,
            )
        } else {
            paper_health_at(
                br#"{"paper_only":true,"live_enabled":false,"status":"scanning","as_of_ms":1000000,"portfolio":{},"account":{}}"#,
                "v0.6.0",
                1_000_000,
            )
        }
    }
}
fn fixture() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".local/state/nivra")).unwrap();
    for (name, bytes) in [
        ("old", b"active binary".as_slice()),
        ("previous", b"previous binary".as_slice()),
    ] {
        let release = h.join("nivra-releases").join(name);
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("nivra"), bytes).unwrap();
    }
    std::os::unix::fs::symlink(h.join("nivra-releases/old"), h.join("nivra-current")).unwrap();
    std::os::unix::fs::symlink(h.join("nivra-releases/previous"), h.join("nivra-previous"))
        .unwrap();
    home
}
fn retained(h: &Path) {
    assert_eq!(
        fs::read(h.join("nivra-releases/old/nivra")).unwrap(),
        b"active binary"
    );
    assert_eq!(
        fs::read(h.join("nivra-releases/previous/nivra")).unwrap(),
        b"previous binary"
    );
}
fn unchanged(h: &Path, host: &Fake) {
    assert_eq!(
        fs::read_link(h.join("nivra-current")).unwrap(),
        h.join("nivra-releases/old")
    );
    assert_eq!(
        fs::read_link(h.join("nivra-previous")).unwrap(),
        h.join("nivra-releases/previous")
    );
    assert!(host.restarts.is_empty());
    assert!(host.health_checks.is_empty());
    assert!(!h.join(".local/state/nivra/pending.json").exists());
    retained(h);
}
#[test]
fn update_decision_verifies_and_defers_then_activates_or_rolls_back() {
    use clap::Parser;
    for version in ["0.6.0", "v0.6.0"] {
        let args = crate::Args::try_parse_from([
            "nivra",
            "update",
            "--config",
            "config.json",
            "--tag",
            version,
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            crate::Command::Update { tag: Some(_), .. }
        ));
        let home = fixture();
        let h = home.path();
        let mut host = Fake::new(h);
        assert_eq!(
            apply_selected(
                h,
                &mut host,
                false,
                false,
                false,
                &Selection::new(Some(version), None).unwrap()
            ),
            Ok("updated")
        );
    }
    let force = crate::Args::try_parse_from([
        "nivra",
        "update",
        "--config",
        "config.json",
        "--force-unwatched-window",
    ])
    .unwrap();
    assert!(matches!(
        force.command,
        crate::Command::Update {
            force_unwatched_window: true,
            ..
        }
    ));
    assert!(
        crate::Args::try_parse_from([
            "nivra",
            "update",
            "--config",
            "config.json",
            "--operator-stopped",
            "--confirm-paper-review",
            "--force-unwatched-window",
        ])
        .is_err()
    );
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    assert_eq!(apply(h, &mut host, false, false), Ok("updated"));
    let stage = fs::canonicalize(h.join("nivra-current")).unwrap();
    let receipt = fs::read(stage.join("installed-update.json")).unwrap();
    swap(&h.join("nivra-current"), &h.join("nivra-releases/old")).unwrap();
    assert_eq!(apply(h, &mut host, false, false), Ok("updated"));
    assert_eq!(
        fs::read(stage.join("installed-update.json")).unwrap(),
        receipt
    );
    assert_eq!(
        fs::read_dir(&stage)
            .unwrap()
            .filter(|entry| entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("installed-update"))
            .count(),
        2
    );
    assert!(
        crate::Args::try_parse_from([
            "nivra",
            "update",
            "--config",
            "config.json",
            "--tag",
            "0.6.0",
            "--sha",
            "aaaaaaa"
        ])
        .is_err()
    );
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    assert_eq!(
        apply_selected(
            h,
            &mut host,
            false,
            false,
            false,
            &Selection::new(None, Some(&"a".repeat(40))).unwrap()
        ),
        Ok("updated")
    );
    assert!(Selection::new(None, Some("not-a-commit")).is_err());
    // An out-of-band install has an actual manifest/binary but no updater metadata.
    let home = fixture();
    let h = home.path();
    let current = h.join("nivra-releases/old");
    let mut host = Fake::new(h);
    fs::write(current.join("nivra"), b"candidate").unwrap();
    fs::write(
        current.join("release.json"),
        host.fetch("v0.6.0/release.json", 16384).unwrap(),
    )
    .unwrap();
    fs::write(current.join("VERSION"), b"v0.6.0\n").unwrap();
    fs::write(
        current.join("SOURCE_COMMIT"),
        format!("{}\n", "a".repeat(40)),
    )
    .unwrap();
    fs::write(
        current.join("SHA256SUMS"),
        format!(
            "{}  nivra\n{}  paper.json\n",
            digest(b"candidate"),
            "f".repeat(64)
        ),
    )
    .unwrap();
    fs::set_permissions(&current, fs::Permissions::from_mode(0o755)).unwrap();
    for target in [
        Selection::default(),
        Selection::new(Some("0.6.0"), None).unwrap(),
    ] {
        assert_eq!(
            apply_selected(h, &mut host, false, false, false, &target),
            Ok("already_current")
        );
        assert!(host.restarts.is_empty() && host.stops == 0);
    }
    assert_eq!(fs::read(current.join("VERSION")).unwrap(), b"v0.6.0\n");
    assert_eq!(
        fs::metadata(&current).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for (name, mode) in [
        ("nivra", 0o755),
        ("release.json", 0o600),
        ("VERSION", 0o600),
    ] {
        assert_eq!(
            fs::metadata(current.join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            mode
        );
    }
    fs::remove_file(current.join("release.json")).unwrap();
    fs::remove_file(current.join("VERSION")).unwrap();
    fs::remove_file(current.join("SOURCE_COMMIT")).unwrap();
    for target in [
        Selection::default(),
        Selection::new(Some("v0.6.0"), None).unwrap(),
        Selection::new(None, Some("aaaaaaa")).unwrap(),
    ] {
        assert_eq!(
            apply_selected(h, &mut host, false, false, false, &target),
            Ok("already_current")
        );
    }
    // An installed release the configured distribution does not publish under
    // its recorded identity refuses before any effect.
    let home = fixture();
    let h = home.path();
    let dir = h.join("nivra-releases/old");
    fs::write(dir.join("nivra"), b"legacy").unwrap();
    fs::write(
        dir.join("release.json"),
        serde_json::json!({
            "tag": "v0.5.46",
            "source_commit": "e".repeat(40),
            "target": TARGET,
            "sha256": digest(b"legacy"),
        })
        .to_string(),
    )
    .unwrap();
    let target = Selection::new(Some("v0.6.0"), None).unwrap();
    let mut host = Fake::new(h);
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Err("installed_release_identity_mismatch")
    );
    assert!(host.restarts.is_empty() && host.stops == 0);
    assert!(valid_remote("github-nivra:TharunChougoni/nivra.git"));
    assert!(!valid_remote("github-ind-sadr:TharunChougoni/nivra.git"));
    assert!(!valid_remote("https://github.com/TharunChougoni/nivra.git"));
    assert!(!valid_remote("github-nivra:Tharun Chougoni/nivra.git"));
    // A tag whose CI assets are not yet published: the timer defers cleanly.
    {
        let home = fixture();
        let h = home.path();
        let mut host = Fake::new(h);
        host.assets_pending = true;
        assert_eq!(
            apply_selected(h, &mut host, false, false, false, &Selection::default()),
            Ok("deferred_release_assets_pending")
        );
        let target = Selection::new(Some("v0.6.0"), None).unwrap();
        assert_eq!(
            apply_selected(h, &mut host, false, false, false, &target),
            Err("release_assets_pending")
        );
        assert!(host.restarts.is_empty() && host.stops == 0);
    }
    // Operator-stopped deploy: swap and smoke, never restart or health-probe.
    {
        let home = fixture();
        let h = home.path();
        let mut host = Fake::new(h);
        host.keep_stopped = true;
        let target = Selection::new(Some("v0.6.0"), None).unwrap();
        assert!(apply_selected(h, &mut host, false, false, false, &target).is_ok());
        assert!(host.restarts.is_empty() && host.health_checks.is_empty());
        let live = h.join("nivra-current");
        assert_eq!(fs::read(live.join("nivra")).unwrap(), b"candidate");
    }
    // Preflight fails after flip: stop, restore old pointer/start, retain failure.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.fail_post_stop_preflight = true;
    assert_eq!(
        apply_selected(
            h,
            &mut host,
            false,
            false,
            false,
            &Selection::new(Some("v0.6.0"), None).unwrap()
        ),
        Err("update_rolled_back")
    );
    assert_eq!(
        fs::read_link(h.join("nivra-current")).unwrap(),
        h.join("nivra-releases/old")
    );
    assert_eq!(host.restarts, vec![h.join("nivra-releases/old")]);
    assert!(
        h.join(format!(
            "nivra-releases/v0.6.0-{}/failed-update.json",
            "a".repeat(40)
        ))
        .exists()
    );
    assert!(!h.join(".local/state/nivra/pending.json").exists());
    // Stable cwd and removed checkout overrides are properties of every Git command.
    let command = git_command(
        &["ls-remote", "--tags", "github-nivra:repo"],
        Path::new("/private/stable-state"),
    );
    assert_eq!(
        command.get_current_dir(),
        Some(Path::new("/private/stable-state"))
    );
    let safe=safe_git_stderr(b"fatal: permission denied Bearer shortsecret https://user:password@host private-key SECRET eyJabcdefghijklmnopqrstuvwxyz");
    for secret in ["shortsecret", "user:password", "SECRET", "eyJabc"] {
        assert!(!safe.contains(secret));
    }
    assert!(safe.contains("permission denied"));
    let home = fixture();
    let h = home.path();
    let state = h.join(".local/state/nivra");
    let deploy = h.join("nivra-deploy");
    fs::create_dir_all(&deploy).unwrap();
    for path in [state.join("updater.lock"), deploy.join(".lock")] {
        let owner = AdvisoryLock::new(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap(),
        );
        owner.try_lock_exclusive().unwrap();
        // A sibling test may fork while this lock is held, and the child
        // shares the open file description until exec closes it. Keep a
        // duplicate alive across the release: the guard must unlock
        // explicitly, or the next with_update_locks call sees a busy lock (#77).
        let inherited = owner.try_clone().unwrap();
        assert_eq!(
            with_update_locks(h, &state, false, false, || {
                let mut host = Fake::new(h);
                let result = apply_selected(
                    h,
                    &mut host,
                    false,
                    false,
                    false,
                    &Selection::new(Some("v0.6.0"), None).unwrap(),
                );
                panic!("manual apply reached a held timer lock: {result:?}");
            }),
            Ok("skipped_lock_busy")
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(state.join("update.json")).unwrap()).unwrap();
        assert_eq!(record["status"], "skipped_lock_busy");
        assert_eq!(record["check"], false);
        assert_eq!(record["dry_run"], false);
        assert!(!record["at"].as_str().unwrap().is_empty());
        unchanged(h, &Fake::new(h));
        drop(owner);
        assert_eq!(
            with_update_locks(h, &state, true, false, || Ok("checked")),
            Ok("checked")
        );
        drop(inherited);
    }
    // Real failures still reach entry's critical-alert / nonzero-exit branch.
    for failure in ["update_rolled_back", "release_checksum_mismatch"] {
        assert_eq!(
            with_update_locks(h, &state, false, false, || Err(failure)),
            Err(failure)
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(state.join("update.json")).unwrap()).unwrap();
        assert_eq!(record["status"], failure);
    }
    fs::remove_file(deploy.join(".lock")).unwrap();
    std::os::unix::fs::symlink(state.join("updater.lock"), deploy.join(".lock")).unwrap();
    assert_eq!(
        with_update_locks(h, &state, false, false, || panic!("invalid lock")),
        Err("update_lock_unavailable")
    );
    let now = 1_000_000;
    let body =
        br#"{"v":1,"paper_only":true,"live_enabled":false,"status":"idle","as_of_ms":1000000}"#;
    assert!(paper_health_at(body, "v0.6.0", now).is_ok());
    assert!(paper_health_at(body, "v0.6.0", now + 360_000).is_ok());
    assert!(paper_health_at(body, "v0.6.0", now + 360_001).is_err());
    assert!(paper_health_at(body, "v0.6.0", now - 1).is_err());
    let mut snapshot: serde_json::Value = serde_json::from_slice(body).unwrap();
    snapshot["version"] = serde_json::json!("0.6.0");
    snapshot["controller_heartbeat"] = serde_json::json!({
        "version":1,"completed_at_ms":now,"next_deadline_ms":now+15_000,"loop_generation":7
    });
    snapshot["operational_health"] = serde_json::json!({
        "version":1,"captured_at_ms":now,
        "supervisor":{"status":"unknown","state":"missing"},
        "calendar":{"covered_to":"2026-12-31"},
        "alert_transport":{"status":"missing","counts":null}
    });
    let checked = |value: &serde_json::Value, at| {
        paper_health_at(&serde_json::to_vec(value).unwrap(), "v0.6.0", at)
    };
    assert!(checked(&snapshot, now + 75_000).is_ok());
    assert!(checked(&snapshot, now + 75_001).is_err());
    // An API response time or a cached healthy projection cannot renew a tick.
    snapshot["response_at_ms"] = serde_json::json!(now + 360_001);
    snapshot["controller_liveness"] = serde_json::json!({"status":"healthy"});
    assert!(checked(&snapshot, now + 360_001).is_err());
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("completed_at_ms", serde_json::json!(now + 1)),
        ("next_deadline_ms", serde_json::json!(now - 1)),
        ("next_deadline_ms", serde_json::json!(now + 300_001)),
        ("next_deadline_ms", serde_json::json!("soon")),
        ("loop_generation", serde_json::json!(-1)),
        ("loop_generation", serde_json::Value::Null),
    ] {
        let mut malformed = snapshot.clone();
        malformed["controller_heartbeat"][field] = value;
        assert!(checked(&malformed, now).is_err(), "{field}");
    }
    snapshot["controller_heartbeat"] = serde_json::Value::Null;
    assert!(checked(&snapshot, now).is_err());
    snapshot
        .as_object_mut()
        .unwrap()
        .remove("controller_heartbeat");
    for at in [
        serde_json::Value::Null,
        serde_json::json!("1000000"),
        serde_json::json!(0),
        serde_json::json!(-1),
        serde_json::json!(1.5),
    ] {
        snapshot["as_of_ms"] = at;
        assert!(checked(&snapshot, now).is_err());
    }
    snapshot.as_object_mut().unwrap().remove("as_of_ms");
    assert!(checked(&snapshot, now).is_err());
    let mut current: serde_json::Value = serde_json::from_slice(body).unwrap();
    current["as_of_ms"] = serde_json::json!(now_utc().timestamp_millis());
    assert!(paper_health(&serde_json::to_vec(&current).unwrap(), "v0.6.0").is_ok());
    let stalled =
        br#"{"v":1,"paper_only":true,"live_enabled":false,"status":"scanning","as_of_ms":1}"#;
    assert!(paper_health(stalled, "v0.6.0").is_err());
    for bad in [
        br#"{"sessions":[]}"#.as_slice(),
        br#"{"paper_only":true,"live_enabled":true,"status":"idle","as_of_ms":1000000}"#,
        br#"{"paper_only":true,"live_enabled":false,"status":"idle","as_of_ms":1000000,"version":"0.5.9"}"#,
        br#"{"paper_only":true,"live_enabled":false,"status":"idle"}"#,
        br#"{"paper_only":true,"live_enabled":false,"status":null,"as_of_ms":1000000}"#,
        br#"{"paper_only":false,"live_enabled":false,"status":"idle","as_of_ms":1000000}"#,
        &vec![b' '; 65537],
    ] {
        assert!(paper_health_at(bad, "v0.6.0", now).is_err());
    }
    let stopped = br#"{"paper_only":true,"live_enabled":false,"status":"stopped_operator_review_required","as_of_ms":1000000,"book":{"stopped":true,"closed":false}}"#;
    assert_eq!(
        paper_health_at(stopped, "v0.6.0", now),
        Err("update_health_book_operator_stopped")
    );
    // A closed book stopped before its seal rolls over normally; it is not refused.
    let sealed = br#"{"paper_only":true,"live_enabled":false,"status":"idle","as_of_ms":1000000,"book":{"stopped":true,"closed":true}}"#;
    assert!(paper_health_at(sealed, "v0.6.0", now).is_ok());
    let mut probes = 0;
    assert_eq!(
        poll_health(
            Duration::from_secs(60),
            || Ok(0),
            || {
                probes += 1;
                paper_health_at(stopped, "v0.6.0", now)
            },
            || Duration::ZERO,
            |_| {},
        ),
        Err("update_health_book_operator_stopped")
    );
    assert_eq!(probes, 1);
    for ready_after in [3, 30, 31] {
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let mut attempts = 0;
        let result = poll_health(
            Duration::from_secs(60),
            || Ok(0),
            || {
                attempts += 1;
                if attempts > ready_after {
                    paper_health_at(body, "v0.6.0", now)
                } else {
                    Err("update_dashboard_health_failed")
                }
            },
            || elapsed.get(),
            |delay| elapsed.set(elapsed.get() + delay),
        );
        assert_eq!(result.is_ok(), ready_after <= 30);
        assert!(elapsed.get() <= Duration::from_secs(60));
    }
    // A slow but healthy restart is admitted inside the longer bounded wait;
    // the failure budget still refuses an unresponsive dashboard.
    let elapsed = std::cell::Cell::new(Duration::ZERO);
    let mut attempts = 0;
    assert!(
        poll_health(
            HEALTH_WAIT,
            || Ok(0),
            || {
                attempts += 1;
                if attempts > 100 {
                    paper_health_at(body, "v0.6.0", now)
                } else {
                    Err("update_dashboard_health_failed")
                }
            },
            || elapsed.get(),
            |delay| elapsed.set(elapsed.get() + delay),
        )
        .is_ok()
    );
    assert!(elapsed.get() <= HEALTH_WAIT);
    let mut checks = 0;
    assert_eq!(
        poll_health(
            Duration::from_secs(60),
            || {
                checks += 1;
                Ok(u64::from(checks > 2))
            },
            || Err("update_dashboard_health_failed"),
            || Duration::ZERO,
            |_| {}
        ),
        Err("update_service_unhealthy")
    );
    assert_eq!(
        poll_health(
            Duration::from_secs(60),
            || Err("update_service_unhealthy"),
            || panic!("inactive service must not be probed"),
            || Duration::ZERO,
            |_| {}
        ),
        Err("update_service_unhealthy")
    );
    assert!(Version::parse("v0.10.0").unwrap() > Version::parse("v0.9.0").unwrap());
    assert_eq!(select("abc\trefs/tags/v99.0.0\n", "v0.5.8"), Ok(None));
    // Inject the clock and session verdict into the same window used by RealHost,
    // then exercise the complete activation path, not only the predicate.
    for (time, safe, expected) in [
        ("2026-10-19T08:59:59+05:30", true, true),
        ("2026-10-19T09:00:00+05:30", true, false),
        ("2026-10-19T12:00:00+05:30", true, false),
        ("2026-10-19T15:45:00+05:30", true, false),
        ("2026-10-19T15:45:59+05:30", true, false),
        ("2026-10-19T15:46:00+05:30", true, true),
        ("2026-10-17T12:00:00+05:30", true, true),
        ("2026-10-17T12:00:00+05:30", false, false),
        ("2026-10-19T19:00:00+05:30", false, false),
    ] {
        let home = fixture();
        let h = home.path();
        let mut host = Fake::new(h);
        host.now = time.parse().unwrap();
        host.session_safe = safe;
        assert_eq!(
            apply(h, &mut host, false, false),
            Ok(if expected {
                "updated"
            } else if !safe {
                "deferred_book_not_flat"
            } else {
                "deferred_market_open"
            })
        );
        if expected {
            let current = fs::read_link(h.join("nivra-current")).unwrap();
            assert_eq!(fs::read(current.join("nivra")).unwrap(), b"candidate");
            assert_eq!(host.restarts, vec![current.clone()]);
            assert_eq!(host.health_checks, vec![current]);
            assert_eq!(
                fs::read_link(h.join("nivra-previous")).unwrap(),
                h.join("nivra-releases/old")
            );
        } else {
            unchanged(h, &host);
        }
        retained(h);
    }
    for fail in [false, true] {
        let home = fixture();
        let h = home.path();
        let old = h.join("nivra-releases/old");
        let stage = h.join(format!("nivra-releases/v0.6.0-{}", "a".repeat(40)));
        let mut host = Fake::new(h);
        host.fail_next_health = fail;
        host.corrupt = true;
        assert_eq!(
            apply(h, &mut host, false, false),
            Err("release_checksum_mismatch")
        );
        unchanged(h, &host);
        assert!(!stage.exists());
        host.corrupt = false;
        host.smoke_fail = true;
        assert_eq!(apply(h, &mut host, false, false), Err("fake_smoke_failed"));
        unchanged(h, &host);
        assert_eq!(fs::read(stage.join("nivra")).unwrap(), b"candidate");
        host.smoke_fail = false;
        // Preparation crosses into Monday's blackout; a late guard must defer.
        host.now = "2026-10-19T08:59:59+05:30".parse().unwrap();
        host.after_smoke = Some("2026-10-19T09:00:00+05:30".parse().unwrap());
        assert_eq!(
            apply(h, &mut host, false, false),
            Ok("deferred_market_open")
        );
        unchanged(h, &host);
        host.after_smoke = None;
        host.now = "2026-10-19T19:00:00+05:30".parse().unwrap();
        assert_eq!(
            apply(h, &mut host, false, false),
            if fail {
                Err("update_rolled_back")
            } else {
                Ok("updated")
            }
        );
        let mut targets = vec![stage.clone()];
        if fail {
            targets.push(old.clone());
        }
        assert_eq!(host.restarts, targets);
        assert_eq!(host.health_checks, targets);
        assert_eq!(
            fs::read_link(h.join("nivra-current")).unwrap(),
            if fail { old.clone() } else { stage.clone() }
        );
        assert_eq!(
            fs::read_link(h.join("nivra-previous")).unwrap(),
            if fail {
                h.join("nivra-releases/previous")
            } else {
                old.clone()
            }
        );
        assert!(!h.join(".local/state/nivra/pending.json").exists());
        let marker = if fail {
            "failed-update.json"
        } else {
            "installed-update.json"
        };
        let evidence_bytes = fs::read(stage.join(marker)).unwrap();
        let evidence: serde_json::Value = serde_json::from_slice(&evidence_bytes).unwrap();
        if fail {
            assert_eq!(
                evidence["recovery"]["cause"],
                "update_health_book_operator_stopped"
            );
        }
        assert_eq!(evidence["previous"], old.to_str().unwrap());
        assert_eq!(evidence["new"], stage.to_str().unwrap());
        let artifacts: Vec<_> = [
            "nivra",
            "release.json",
            "SOURCE_COMMIT",
            "VERSION",
            "SHA256SUMS",
        ]
        .into_iter()
        .map(|name| (stage.join(name), fs::read(stage.join(name)).unwrap()))
        .collect();
        assert_eq!(fs::read(stage.join("nivra")).unwrap(), b"candidate");
        retained(h);
        if fail {
            assert_eq!(
                apply(h, &mut host, false, false),
                Err("failed_release_requires_review")
            );
            assert_eq!(host.restarts.len(), 2);
            assert_eq!(host.health_checks.len(), 2);
            assert_eq!(fs::read(stage.join(marker)).unwrap(), evidence_bytes);
            for (path, bytes) in artifacts {
                assert_eq!(fs::read(path).unwrap(), bytes);
            }
            assert_eq!(fs::read_link(h.join("nivra-current")).unwrap(), old);
            retained(h);
        }
    }
    rollback_preflight_refusal_leaves_running_update_untouched();
}
fn rollback_preflight_refusal_leaves_running_update_untouched() {
    let home = fixture();
    let h = home.path();
    let config = h.join("paper.json");
    let config_bytes =
        br#"{"paper_costs":{},"paper_cost_transition":{"source_session":"2026-10-16"}}"#;
    fs::write(&config, config_bytes).unwrap();
    let mut host = Fake::new(h);
    host.rollback_preflight_fail = true;
    assert_eq!(
        apply_selected(
            h,
            &mut host,
            false,
            false,
            false,
            &Selection::new(Some("v0.6.0"), None).unwrap()
        ),
        Err("update_rollback_config_incompatible")
    );
    assert_eq!(
        host.rollback_preflights,
        vec![h.join("nivra-releases/old/nivra")]
    );
    assert_eq!(host.stops, 0);
    unchanged(h, &host);
    assert_eq!(fs::read(&config).unwrap(), config_bytes);
    let stage = h.join(format!("nivra-releases/v0.6.0-{}", "a".repeat(40)));
    assert!(!stage.join("failed-update.json").exists());
    assert!(!stage.join("installed-update.json").exists());
}
#[test]
fn rollback_preflight_passes_then_updates_and_gates_keep_stopped() {
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    // Previews never run the old binary or touch the running controller.
    assert_eq!(apply(h, &mut host, true, false), Ok("available"));
    assert_eq!(apply(h, &mut host, false, true), Ok("would_update"));
    assert!(host.rollback_preflights.is_empty() && host.stops == 0);
    let target = Selection::new(Some("v0.6.0"), None).unwrap();
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("updated")
    );
    assert_eq!(
        host.rollback_preflights,
        vec![h.join("nivra-releases/old/nivra")]
    );
    assert_eq!(host.stops, 1);
    let current = fs::read_link(h.join("nivra-current")).unwrap();
    assert_eq!(fs::read(current.join("nivra")).unwrap(), b"candidate");
    assert_eq!(host.restarts, vec![current.clone()]);
    assert_eq!(host.health_checks, vec![current]);

    // Operator-stopped deploys use the same pre-flip compatibility check.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.keep_stopped = true;
    host.rollback_preflight_fail = true;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Err("update_rollback_config_incompatible")
    );
    assert_eq!(host.stops, 0);
    unchanged(h, &host);
    host.rollback_preflight_fail = false;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("updated")
    );
    assert_eq!(host.stops, 1);
    assert_eq!(host.rollback_preflights.len(), 2);
    assert!(host.restarts.is_empty() && host.health_checks.is_empty());
    assert_eq!(
        fs::read(h.join("nivra-current/nivra")).unwrap(),
        b"candidate"
    );

    // #152 deploy guard: open exposure in-session defers with an explicit
    // reason, and a forced activation records the bounded unwatched window.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.now = "2026-10-19T12:00:00+05:30".parse().unwrap();
    host.open_positions = 1;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("deferred_market_open_open_positions")
    );
    assert_eq!(host.stops, 0);
    assert!(host.restarts.is_empty());
    unchanged(h, &host);
    host.open_positions = 0;
    host.pending_orders = 1;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("deferred_market_open_pending_orders")
    );
    assert_eq!(host.stops, 0);
    // A risk halt still refuses the forced path.
    host.pending_orders = 0;
    host.open_positions = 1;
    host.halted = true;
    assert_eq!(
        apply_selected(h, &mut host, false, false, true, &target),
        Ok("deferred_halted_book")
    );
    assert_eq!(host.stops, 0);
    host.halted = false;
    assert_eq!(
        apply_selected(h, &mut host, false, false, true, &target),
        Ok("updated")
    );
    let stage = fs::canonicalize(h.join("nivra-current")).unwrap();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(stage.join("installed-update.json")).unwrap()).unwrap();
    assert_eq!(receipt["unwatched"]["forced"], true);
    assert_eq!(receipt["unwatched"]["reason"], "market_open_open_positions");
    assert!(receipt["unwatched"]["started_at"].as_str().is_some());
    assert!(receipt["unwatched"]["stopped_at"].as_str().is_some());
    assert!(
        receipt["unwatched"]["duration_ms"]
            .as_i64()
            .is_some_and(|ms| ms >= 0)
    );
    // A flat in-session book may be forced with an explicit bounded window; the
    // halt/stopped guards above still refuse it.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.now = "2026-10-19T12:00:00+05:30".parse().unwrap();
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("deferred_market_open")
    );
    assert_eq!(host.stops, 0);
    assert_eq!(
        apply_selected(h, &mut host, false, false, true, &target),
        Ok("updated")
    );
    let stage = fs::canonicalize(h.join("nivra-current")).unwrap();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(stage.join("installed-update.json")).unwrap()).unwrap();
    assert_eq!(receipt["unwatched"]["forced"], true);
    assert_eq!(receipt["unwatched"]["reason"], "market_open_flat");
    // A flat book outside the session needs no forced window and is not recorded.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.now = "2026-10-17T12:00:00+05:30".parse().unwrap();
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("updated")
    );
    let stage = fs::canonicalize(h.join("nivra-current")).unwrap();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(stage.join("installed-update.json")).unwrap()).unwrap();
    assert!(receipt.get("unwatched").is_none());
    // Open exposure outside the session also defers unless forced.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.now = "2026-10-17T12:00:00+05:30".parse().unwrap();
    host.open_positions = 1;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Ok("deferred_open_positions")
    );
    assert_eq!(host.stops, 0);
    assert_eq!(
        apply_selected(h, &mut host, false, false, true, &target),
        Ok("updated")
    );
    let stage = fs::canonicalize(h.join("nivra-current")).unwrap();
    let receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(stage.join("installed-update.json")).unwrap()).unwrap();
    assert_eq!(receipt["unwatched"]["reason"], "open_positions");
}
#[test]
fn candidate_smoke_failure_skips_rollback_preflight_without_flip() {
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.smoke_fail = true;
    assert_eq!(
        apply_selected(
            h,
            &mut host,
            false,
            false,
            false,
            &Selection::new(Some("v0.6.0"), None).unwrap()
        ),
        Err("fake_smoke_failed")
    );
    assert!(host.rollback_preflights.is_empty());
    assert_eq!(host.stops, 0);
    unchanged(h, &host);
    assert!(
        h.join(format!("nivra-releases/v0.6.0-{}/nivra", "a".repeat(40)))
            .exists()
    );
    // #149: a preflight failure leaves a bounded reason+time record; raw stderr
    // with paths or tokens is never retained. A later success clears it.
    let state = h.join(".local/state/nivra");
    assert!(preflight_status(&state).is_none());
    record_preflight_failure(
            &state,
            "candidate",
            Some("v0.8.20"),
            b"fatal: /home/operator/.config/nivra/paper.json Authorization: Bearer secret-token account=ABC\n",
        );
    let record = preflight_status(&state).unwrap();
    assert_eq!(record["reason"], "preflight_failed_unspecified");
    assert_eq!(record["phase"], "candidate");
    assert_eq!(record["version"], "v0.8.20");
    assert!(record["since"].as_str().is_some_and(|at| at.contains('T')));
    assert_eq!(
        fs::metadata(state.join("preflight-failure.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        preflight_reason(b"research_runtime_unreadable\n"),
        "research_runtime_unreadable"
    );
    assert_eq!(
        preflight_reason(b"Bearer abcdefghijklmnop"),
        "preflight_failed_unspecified"
    );
    record_preflight_failure(
        &state,
        "candidate",
        Some("v0.8.20"),
        b"research_runtime_unreadable\n",
    );
    let details = updater_failure_details(Some(&state), "update_preflight_failed");
    assert_eq!(details["reason"], "update_preflight_failed");
    assert_eq!(
        details["preflight_failure"]["reason"],
        "research_runtime_unreadable"
    );
    assert_eq!(details["preflight_failure"]["version"], "v0.8.20");
    let text = details.to_string();
    for secret in ["/home/operator", "Authorization", "secret-token", "ABC"] {
        assert!(!text.contains(secret), "{text}");
    }
    clear_preflight_failure(&state);
    assert!(preflight_status(&state).is_none());
    assert!(
        updater_failure_details(Some(&state), "update_preflight_failed")
            .get("preflight_failure")
            .is_none()
    );
}
#[test]
fn failed_stop_swap_and_rollback_restart_are_recovered_with_evidence() {
    let target = Selection::new(Some("v0.6.0"), None).unwrap();
    let old = |h: &Path| h.join("nivra-releases/old");
    let current = |h: &Path| h.join("nivra-current");
    let stage = |h: &Path| h.join(format!("nivra-releases/v0.6.0-{}", "a".repeat(40)));
    let state = |h: &Path| h.join(".local/state/nivra");
    let receipt = |h: &Path| -> serde_json::Value {
        serde_json::from_slice(&fs::read(stage(h).join("failed-update.json")).unwrap()).unwrap()
    };
    // A stop failure restores the previous controller rather than leaving it down.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.fail_stop = true;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Err("update_stop_failed")
    );
    assert_eq!(host.restarts, vec![old(h)]);
    assert_eq!(host.health_checks, vec![old(h)]);
    assert_eq!(fs::canonicalize(current(h)).unwrap(), old(h));
    assert!(!state(h).join("pending.json").exists());
    assert_eq!(receipt(h)["recovery"]["cause"], "update_stop_failed");
    assert_eq!(receipt(h)["recovery"]["restarted"], true);
    assert!(!state(h).join("update-recovery.json").exists());

    // A swap that fails before the rename restarts the untouched previous release.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.swap_failures = 1;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Err("update_swap_failed")
    );
    assert_eq!(host.restarts, vec![old(h)]);
    assert_eq!(fs::canonicalize(current(h)).unwrap(), old(h));
    assert_eq!(receipt(h)["recovery"]["swapped"], false);
    assert_eq!(receipt(h)["recovery"]["pointer_restored"], true);

    // A swap that fails after the rename still restores the pointer, then restarts.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.swap_failures = 1;
    host.swap_before_failure = true;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Err("update_swap_failed")
    );
    assert_eq!(fs::canonicalize(current(h)).unwrap(), old(h));
    assert_eq!(host.restarts, vec![old(h)]);
    assert_eq!(receipt(h)["recovery"]["swapped"], true);
    assert_eq!(receipt(h)["recovery"]["pointer_restored"], true);

    // A rollback that cannot restart the previous release is loud and distinct.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    host.fail_post_stop_preflight = true;
    host.fail_restart = true;
    assert_eq!(
        apply_selected(h, &mut host, false, false, false, &target),
        Err("update_rollback_restart_failed")
    );
    assert_eq!(fs::canonicalize(current(h)).unwrap(), old(h));
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(state(h).join("update-recovery.json")).unwrap()).unwrap();
    assert_eq!(record["kind"], "rollback_restart_failed");
    assert_eq!(record["cause"], "update_preflight_failed");
    assert_eq!(record["pointer_restored"], true);
    assert_eq!(record["restarted"], false);
    assert_eq!(receipt(h)["recovery"]["restarted"], false);
    let details = updater_failure_details(Some(&state(h)), "update_rollback_restart_failed");
    assert_eq!(details["rollback"], true);
    assert_eq!(details["rollback_restart_failed"], true);
    assert_eq!(details["reason"], "update_rollback_restart_failed");
    assert_eq!(details["recovery"]["kind"], "rollback_restart_failed");
    assert!(!details.to_string().contains('/'));

    // A stale pre-flip transaction is preserved as evidence, then a fresh run proceeds.
    let home = fixture();
    let h = home.path();
    let mut host = Fake::new(h);
    fs::write(
        state(h).join("pending.json"),
        serde_json::to_vec(&serde_json::json!({
            "previous":old(h),
            "new":stage(h),
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(apply(h, &mut host, false, false), Ok("updated"));
    assert!(!state(h).join("pending.json").exists());
    let preserved: Vec<_> = fs::read_dir(state(h))
        .unwrap()
        .filter_map(|entry| {
            let name = entry.unwrap().file_name().to_string_lossy().into_owned();
            name.starts_with("interrupted-update.").then_some(name)
        })
        .collect();
    assert_eq!(preserved.len(), 1, "{preserved:?}");
    // A pending whose pointer already reached the new release is ambiguous:
    // report it with detail instead of replaying or deleting evidence.
    let live = fs::canonicalize(current(h)).unwrap();
    fs::write(
        state(h).join("pending.json"),
        serde_json::to_vec(&serde_json::json!({"previous":old(h),"new":live})).unwrap(),
    )
    .unwrap();
    let stops = host.stops;
    assert_eq!(
        apply(h, &mut host, false, false),
        Err("interrupted_update_new_release_active_requires_review")
    );
    assert_eq!(host.stops, stops);
    assert!(state(h).join("pending.json").exists());
    let review: serde_json::Value =
        serde_json::from_slice(&fs::read(state(h).join("pending-review.json")).unwrap()).unwrap();
    assert_eq!(
        review["reason"],
        "interrupted_update_new_release_active_requires_review"
    );
    assert!(review["current"].as_str().is_some());
    // Invalid evidence is preserved and never guessed at.
    fs::write(state(h).join("pending.json"), b"{not-json").unwrap();
    assert_eq!(
        apply(h, &mut host, false, false),
        Err("interrupted_update_evidence_invalid_requires_review")
    );
    assert!(state(h).join("pending.json").exists());
}
