use super::*;
use std::collections::BTreeMap;

const ASSET: &str = "nivra-x86_64-unknown-linux-musl";

struct Home {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    stage: PathBuf,
    paper: PathBuf,
}

fn at(text: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(text).unwrap()
}

fn evening() -> DateTime<FixedOffset> {
    at("2026-11-16T18:00:00+05:30")
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn fixture() -> Home {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let home = root.join("home");
    let config = home.join(CONFIG_DIR);
    let sessions = home.join("ind-sadr-data/current/sessions");
    let session = sessions.join("2026-10-16");
    fs::create_dir_all(&session).unwrap();
    fs::create_dir_all(config.join("prompts")).unwrap();
    for name in [
        "objective.md",
        "data_owner.md",
        "supervisor.md",
        "orchestrator.md",
        "scorer.md",
    ] {
        fs::write(config.join("prompts").join(name), "fixture prompt").unwrap();
    }
    fs::write(config.join("alert-webhook.json"), "{}").unwrap();
    let paper = config.join("rust-paper.json");
    let planner = config.join("planner.json");

    let mut c = super::super::tests::config(&session);
    c.sessions_root = Some(sessions.clone());
    c.token_file = config.join("upstox-token");
    c.dashboard_token_file = config.join("dashboard-token");
    c.read_journal = home.join("ind-sadr-history/read.sqlite");
    c.planner_config = Some(planner.clone());
    let db = super::super::open_book(&c).unwrap();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![
            super::super::book_binding(&c).unwrap(),
            serde_json::to_string(&nivra_core::execution::Portfolio::new()).unwrap()
        ],
    )
    .unwrap();
    drop(db);
    let mut value = serde_json::to_value(&c).unwrap();
    value["planner_config"] = json!(planner);
    value["sessions_root"] = json!(sessions);
    value["history_directory"] = json!(home.join("ind-sadr-history"));
    value["allow_legacy_model_led_default"] = json!(false);
    write_json(&paper, &value);

    write_json(
        &planner,
        &json!({
            "enabled": true,
            "decision_owner": {"version": 1, "owner": "orchestrator"},
            "decide": {"enabled": false, "model_id": "grok-4.7", "transport": "direct_oauth"},
            "digest": {"enabled": true, "model_id": "gpt-6-luna", "transport": "openai_codex",
                "model_config": config.join("openai-model.json"), "tools": [], "timeout_ms": 20000,
                "output_cap": 4096, "daily_call_budget": 1},
            "events": {"enabled": true, "cooldown_ms": 0, "max_pending": 10,
                "policy": {"price_move_bps": 100, "max_event_age_ms": 60000, "max_news_age_ms": 60000,
                    "max_instruments": 20, "max_identities": 100}},
            "orchestrator": {"enabled": true,
                "model": {"model_id": "grok-4.7", "transport": "direct_oauth",
                    "model_config": config.join("grok-model.json")},
                "triggers": {"supervisor": {"enabled": true, "after_decide": true,
                    "prompt": config.join("prompts/supervisor.md")}},
                "trigger_prompts": {"post_decide": "legacy"}}
        }),
    );
    write_json(
        &config.join("openai-model.json"),
        &json!({"transport": "openai_codex", "grant_file": config.join("openai-grant")}),
    );
    write_json(
        &config.join("grok-model.json"),
        &json!({"transport": "direct_oauth"}),
    );
    write_json(
        &config.join("updater.json"),
        &json!({"remote": "github-ind-sadr:TharunChougoni/nivra.git", "paper_config": paper}),
    );
    write_json(
        &config.join("updater-nivra.json"),
        &json!({"remote": "github-nivra:TharunChougoni/nivra.git", "paper_config": paper,
            "distribution_migration": {"from": "ind-sadr"}}),
    );
    fs::create_dir_all(home.join("ind-sadr-deploy")).unwrap();
    fs::write(home.join("ind-sadr-deploy/.lock"), "").unwrap();
    fs::create_dir_all(home.join(".local/state/ind-sadr")).unwrap();
    fs::write(
        home.join(".local/state/ind-sadr/update.json"),
        r#"{"status":"already_current"}"#,
    )
    .unwrap();
    let old = home.join(format!("ind-sadr-rust-releases/v0.9.33-{}", "b".repeat(40)));
    fs::create_dir_all(&old).unwrap();
    fs::write(old.join("sadr-rs"), "old binary").unwrap();
    std::os::unix::fs::symlink(&old, home.join(OLD_CURRENT)).unwrap();

    let stage = root.join("stage");
    fs::create_dir_all(&stage).unwrap();
    let binary = b"nivra release binary";
    let sha = super::super::digest(binary);
    fs::write(stage.join(ASSET), binary).unwrap();
    fs::write(
        stage.join(format!("{ASSET}.sha256")),
        format!("{sha}  {ASSET}\n"),
    )
    .unwrap();
    write_json(
        &stage.join("release.json"),
        &json!({"tag": format!("v{}", env!("CARGO_PKG_VERSION")), "source_commit": "a".repeat(40),
            "target": "x86_64-unknown-linux-musl", "sha256": sha}),
    );
    Home {
        _dir: dir,
        root,
        home,
        stage,
        paper,
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            let content = if meta.file_type().is_symlink() {
                fs::read_link(&path)
                    .unwrap()
                    .into_os_string()
                    .into_encoded_bytes()
            } else if meta.is_dir() {
                pending.push(path.clone());
                b"dir".to_vec()
            } else {
                fs::read(&path).unwrap()
            };
            out.insert(path, content);
        }
    }
    out
}

fn stray_paths(value: &Value, home: &Path, out: &mut Vec<String>) {
    match value {
        Value::String(text)
            if text.contains("ind-sadr")
                && ![DATA_DIR, CONFIG_DIR, "ind-sadr-history"]
                    .iter()
                    .any(|kept| Path::new(text).starts_with(home.join(kept))) =>
        {
            out.push(text.clone());
        }
        Value::Array(items) => items.iter().for_each(|v| stray_paths(v, home, out)),
        Value::Object(map) => map.values().for_each(|v| stray_paths(v, home, out)),
        _ => {}
    }
}

#[test]
fn dry_run_changes_nothing_apply_converges_and_rerun_is_a_no_op() {
    let h = fixture();
    let config = h.home.join(CONFIG_DIR);
    let live = plan(&h.home, &h.stage).unwrap().paper;
    let book = live.directory.join("paper.sqlite");
    let db = rusqlite::Connection::open(&book).unwrap();
    let original: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    drop(db);
    for (stopped, halted, closed, next) in [
        (true, false, false, "preserve_stopped_or_halted_book"),
        (false, true, false, "preserve_stopped_or_halted_book"),
        (true, true, true, "preserve_stopped_or_halted_book"),
        (
            false,
            false,
            true,
            "start_service_successor_binds_new_policy",
        ),
        (false, false, false, "config_apply_then_resume"),
    ] {
        let mut body = parse(original.as_bytes()).unwrap();
        body["stopped"] = json!(stopped);
        body["halted"] = json!(halted);
        body["closed"] = json!(closed);
        let db = rusqlite::Connection::open(&book).unwrap();
        db.execute("UPDATE state SET body=? WHERE id=1", [body.to_string()])
            .unwrap();
        drop(db);
        let saved = snapshot(&h.root);
        let preview = run(&h.home, &h.stage, false, false, evening()).unwrap();
        assert_eq!(
            preview["operator_next"], next,
            "stopped={stopped}, halted={halted}, closed={closed}"
        );
        assert_eq!(preview["book"]["stopped"], stopped);
        assert_eq!(preview["book"]["halted"], halted);
        assert_eq!(snapshot(&h.root), saved, "readiness preserves the book");
    }
    let db = rusqlite::Connection::open(&book).unwrap();
    db.execute(
        "UPDATE state SET body=? WHERE id=1",
        ["invalid retained body"],
    )
    .unwrap();
    drop(db);
    let unreadable = snapshot(&h.root);
    let preview = run(&h.home, &h.stage, false, false, evening()).unwrap();
    assert_eq!(preview["book"]["state"], "unreadable");
    assert_eq!(preview["operator_next"], "inspect_book_readiness");
    assert_eq!(snapshot(&h.root), unreadable);
    let mut stopped = parse(original.as_bytes()).unwrap();
    stopped["stopped"] = json!(true);
    let db = rusqlite::Connection::open(&book).unwrap();
    db.execute("UPDATE state SET body=? WHERE id=1", [stopped.to_string()])
        .unwrap();
    drop(db);
    let before = snapshot(&h.root);

    let preview = run(&h.home, &h.stage, false, false, evening()).unwrap();
    assert_eq!(preview["status"], "dry_run");
    assert_eq!(preview["book"]["binding"], "matches");
    assert_eq!(snapshot(&h.root), before, "dry run writes nothing");
    let planned = preview["steps"].to_string();
    for removed in [
        "decision_owner",
        "decide",
        "orchestrator.triggers.supervisor.after_decide",
        "orchestrator.trigger_prompts.post_decide",
        "allow_legacy_model_led_default",
    ] {
        assert!(planned.contains(&format!("\"{removed}\"")), "{removed}");
    }

    let applied = run(&h.home, &h.stage, true, false, evening()).unwrap();
    assert_eq!(applied["status"], "applied");
    assert_eq!(applied["operator_next"], "preserve_stopped_or_halted_book");
    assert_eq!(fs::read(&book).unwrap(), before[&book]);
    let anchor = live.directory.join("book-identity.json");
    assert_eq!(fs::read(&anchor).unwrap(), before[&anchor]);

    let paper: Config = serde_json::from_slice(&fs::read(&h.paper).unwrap()).unwrap();
    let planner = planner::PlannerConfig::load(&config.join("planner.json")).unwrap();
    assert!(planner.orchestrator.unwrap().trigger_prompts.is_empty());
    for model in ["openai-model.json", "grok-model.json"] {
        crate::model::parse_config(&fs::read(config.join(model)).unwrap()).unwrap();
    }
    update::validate_config(&fs::read(config.join("updater.json")).unwrap()).unwrap();
    let updater = parse(&fs::read(config.join("updater.json")).unwrap()).unwrap();
    assert_eq!(updater["remote"], update::CANONICAL_REMOTE);
    let grok = parse(&fs::read(config.join("grok-model.json")).unwrap()).unwrap();
    assert_eq!(grok["grant_file"], json!(config.join("xai-grant")));
    assert_eq!(
        paper.alert_webhook_file,
        Some(config.join("alert-webhook.json"))
    );
    drop(super::super::open_book(&paper).unwrap());
    let mut stray = Vec::new();
    for name in [
        "rust-paper.json",
        "planner.json",
        "openai-model.json",
        "grok-model.json",
        "updater.json",
    ] {
        let path = config.join(name);
        stray_paths(
            &parse(&fs::read(&path).unwrap()).unwrap(),
            &h.home,
            &mut stray,
        );
        assert!(
            fs::symlink_metadata(sibling(&path, BACKUP_SUFFIX)).is_ok(),
            "{name} original kept"
        );
    }
    assert!(stray.is_empty(), "{stray:?}");
    assert_eq!(
        fs::read(sibling(&h.paper, BACKUP_SUFFIX)).unwrap(),
        before[&h.paper]
    );

    assert!(h.home.join("nivra-deploy/.lock").is_file());
    assert!(h.home.join(".local/state/nivra/update.json").is_file());
    assert!(!h.home.join("ind-sadr-deploy").exists());
    assert_eq!(
        fs::read(h.home.join("nivra-current/nivra")).unwrap(),
        b"nivra release binary"
    );
    assert_eq!(
        fs::canonicalize(h.home.join("nivra-previous")).unwrap(),
        fs::canonicalize(h.home.join(OLD_CURRENT)).unwrap()
    );
    let unit = fs::read_to_string(h.home.join(".config/systemd/user/nivra-paper.service")).unwrap();
    assert!(unit.contains(
        "ExecStart=%h/nivra-current/nivra paper --config %h/.config/ind-sadr/rust-paper.json"
    ));
    assert!(
        unit.contains("ReadWritePaths=%h/ind-sadr-data %h/.config/ind-sadr %h/.local/state/nivra")
    );

    let migrated = snapshot(&h.root);
    let again = run(&h.home, &h.stage, true, false, evening()).unwrap();
    assert_eq!(again["status"], "already_migrated");
    assert_eq!(again["steps"], json!([]));
    assert_eq!(snapshot(&h.root), migrated, "rerun is a no-op");
}

#[test]
fn refuses_blackout_held_lock_open_update_and_binding_change() {
    for changed in [
        "stopped",
        "halted",
        "pending_old",
        "pending_new",
        "backup",
        "next",
        "unchanged_model",
        "staged_updater",
        "new_staged_updater",
    ] {
        let h = fixture();
        let model = h.home.join(CONFIG_DIR).join("openai-model.json");
        if changed == "unchanged_model" {
            let mut value = parse(&read(&model).unwrap()).unwrap();
            value["alert_webhook_file"] = json!(h.home.join(CONFIG_DIR).join("alert-webhook.json"));
            write_json(&model, &value);
        }
        if changed == "new_staged_updater" {
            fs::remove_file(h.home.join(CONFIG_DIR).join("updater-nivra.json")).unwrap();
        }
        let preview = plan(&h.home, &h.stage).unwrap();
        let live = rollover::current(&preview.paper).unwrap();
        for path in lock_paths(&h.home, &preview.paper, &live).unwrap() {
            if path.parent().is_some_and(Path::exists) {
                drop(hold(&path).unwrap());
            }
        }
        let expected = match changed {
            "stopped" | "halted" => {
                let db = rusqlite::Connection::open(live.directory.join("paper.sqlite")).unwrap();
                let body: String = db
                    .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
                    .unwrap();
                let mut body = parse(body.as_bytes()).unwrap();
                body[changed] = json!(true);
                db.execute("UPDATE state SET body=? WHERE id=1", [body.to_string()])
                    .unwrap();
                "host_migrate_book_state_changed"
            }
            "pending_old" | "pending_new" => {
                let state = if changed == "pending_old" {
                    RENAMES[1].0
                } else {
                    RENAMES[1].1
                };
                write_json(&h.home.join(state).join("pending.json"), &json!({}));
                "host_migrate_update_transaction_requires_review"
            }
            "unchanged_model" => {
                assert!(
                    !preview
                        .steps
                        .iter()
                        .any(|step| matches!(step, Step::Rewrite { path, .. } if path == &model))
                );
                let mut value = parse(&read(&model).unwrap()).unwrap();
                value["grant_file"] = json!(h.home.join(CONFIG_DIR).join("changed-grant"));
                write_json(&model, &value);
                "host_migrate_config_changed"
            }
            "staged_updater" | "new_staged_updater" => {
                write_json(
                    &h.home.join(CONFIG_DIR).join("updater-nivra.json"),
                    &json!({"paper_config": "changed.json"}),
                );
                "host_migrate_config_changed"
            }
            "backup" | "next" => {
                let (before, after) = preview
                    .steps
                    .iter()
                    .find_map(|step| match step {
                        Step::Rewrite {
                            path,
                            before,
                            after,
                            ..
                        } if path == &h.paper => Some((before, after)),
                        _ => None,
                    })
                    .unwrap();
                let (suffix, bytes, code) = if changed == "backup" {
                    (BACKUP_SUFFIX, before, "host_migrate_backup_conflict")
                } else {
                    (NEXT_SUFFIX, after, "host_migrate_next_conflict")
                };
                let path = sibling(&h.paper, suffix);
                fs::write(&path, bytes).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
                assert_eq!(
                    plan(&h.home, &h.stage).err(),
                    Some(code),
                    "{changed} preview"
                );
                code
            }
            _ => unreachable!(),
        };
        let before = snapshot(&h.root);
        assert_eq!(
            apply(&h.home, &preview).err(),
            Some(expected),
            "{changed} apply"
        );
        assert_eq!(
            snapshot(&h.root),
            before,
            "{changed} refusal writes nothing"
        );
        if changed == "backup" || changed == "next" {
            let suffix = if changed == "backup" {
                BACKUP_SUFFIX
            } else {
                NEXT_SUFFIX
            };
            assert_eq!(
                fs::metadata(sibling(&h.paper, suffix)).unwrap().mode() & 0o7777,
                0o644
            );
        }
    }

    let h = fixture();
    let refusal = |now| run(&h.home, &h.stage, true, false, now).unwrap_err();

    let live = plan(&h.home, &h.stage).unwrap().paper;
    let book = live.directory.join("paper.sqlite");
    let preserved = live.directory.join("paper.sqlite.original");
    fs::rename(&book, &preserved).unwrap();
    fs::copy(&preserved, &book).unwrap();
    let replaced = snapshot(&h.root);
    assert_eq!(refusal(evening()), "host_migrate_book_identity_mismatch");
    assert_eq!(
        snapshot(&h.root),
        replaced,
        "replacement refuses before writes"
    );
    fs::remove_file(&book).unwrap();
    fs::rename(&preserved, &book).unwrap();
    let db = rusqlite::Connection::open(&book).unwrap();
    let binding: String = db
        .query_row("SELECT binding FROM state WHERE id=1", [], |r| r.get(0))
        .unwrap();
    db.execute(
        "UPDATE state SET binding=? WHERE id=1",
        ["different binding"],
    )
    .unwrap();
    drop(db);
    let rebound = snapshot(&h.root);
    assert_eq!(refusal(evening()), "host_migrate_book_binding_mismatch");
    assert_eq!(snapshot(&h.root), rebound);
    let db = rusqlite::Connection::open(&book).unwrap();
    db.execute("UPDATE state SET binding=? WHERE id=1", [&binding])
        .unwrap();
    drop(db);

    let release = StagedRelease::read(&h.stage).unwrap();
    let releases = h.home.join("nivra-releases");
    fs::create_dir(&releases).unwrap();
    let installed = release.install(&releases).unwrap();
    for metadata in ["VERSION", "SOURCE_COMMIT", "SHA256SUMS"] {
        let path = installed.join(metadata);
        let kept = installed.join(format!("{metadata}.preserved"));
        fs::rename(&path, &kept).unwrap();
        let incomplete = snapshot(&h.root);
        assert!(
            !release.installed_at(&installed),
            "missing {metadata} is incomplete"
        );
        assert!(release.install(&releases).is_err());
        assert_eq!(
            refusal(evening()),
            "host_migrate_release_directory_conflict"
        );
        assert_eq!(
            snapshot(&h.root),
            incomplete,
            "incomplete metadata remains untouched"
        );
        fs::rename(kept, path).unwrap();
    }
    for name in ["VERSION", "SOURCE_COMMIT", "SHA256SUMS"] {
        let path = installed.join(name);
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"invalid retained metadata").unwrap();
        let corrupt = snapshot(&h.root);
        assert!(
            !release.installed_at(&installed),
            "invalid {name} is incomplete"
        );
        assert!(release.install(&releases).is_err());
        assert_eq!(snapshot(&h.root), corrupt);
        fs::write(&path, original).unwrap();
    }
    let version = fs::read_to_string(installed.join("VERSION")).unwrap();
    fs::write(installed.join("VERSION"), format!("{version}\n")).unwrap();
    let checksums = fs::read_to_string(installed.join("SHA256SUMS")).unwrap();
    fs::write(
        installed.join("SHA256SUMS"),
        format!("{checksums}{}  release.json\n", "b".repeat(64)),
    )
    .unwrap();
    let compatible = snapshot(&h.root);
    assert!(release.installed_at(&installed));
    assert_eq!(snapshot(&h.root), compatible, "completeness is read only");
    fs::remove_dir_all(&installed).unwrap();
    let manifest_bytes = fs::read(h.stage.join("release.json")).unwrap();
    let manifest: update::Manifest = serde_json::from_slice(&manifest_bytes).unwrap();
    assert!(
        update::stage_release(
            &releases,
            &manifest,
            &manifest_bytes,
            b"interrupted artifact"
        )
        .is_err()
    );
    assert!(
        !installed.exists(),
        "failed release never occupies canonical path"
    );
    let retained_release = fs::read_dir(&releases)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read(retained_release.join("nivra")).unwrap(),
        b"interrupted artifact"
    );
    release.install(&releases).unwrap();
    assert!(release.installed_at(&installed));
    assert_eq!(
        fs::read(retained_release.join("nivra")).unwrap(),
        b"interrupted artifact"
    );
    fs::remove_dir_all(&releases).unwrap();

    for (path, expected) in [
        (
            sibling(&h.paper, BACKUP_SUFFIX),
            "host_migrate_backup_conflict",
        ),
        (sibling(&h.paper, NEXT_SUFFIX), "host_migrate_next_conflict"),
        (
            h.home.join(".config/systemd/user/nivra-paper.service"),
            "host_migrate_unit_conflict",
        ),
    ] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"retained conflicting bytes").unwrap();
        let conflict = snapshot(&h.root);
        assert_eq!(refusal(evening()), expected);
        assert_eq!(
            snapshot(&h.root),
            conflict,
            "conflicting artifacts remain untouched"
        );
        fs::remove_file(&path).unwrap();
    }
    fs::remove_dir(h.home.join(".config/systemd/user")).unwrap();
    fs::remove_dir(h.home.join(".config/systemd")).unwrap();

    let artifact = h.root.join("immutable-artifact");
    fs::write(&artifact, b"original").unwrap();
    assert!(write_private(&artifact, b"replacement", 0o600, true).is_err());
    assert_eq!(fs::read(&artifact).unwrap(), b"original");
    let retained: Vec<_> = fs::read_dir(&h.root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("immutable-artifact.pending.")
        })
        .collect();
    assert_eq!(
        retained.len(),
        1,
        "failed private publication retains staging"
    );
    assert_eq!(fs::read(&retained[0]).unwrap(), b"replacement");
    fs::remove_file(&artifact).unwrap();
    write_private(&artifact, b"retry", 0o600, true).unwrap();
    assert_eq!(fs::read(&artifact).unwrap(), b"retry");
    assert_eq!(fs::read(&retained[0]).unwrap(), b"replacement");
    fs::remove_file(&artifact).unwrap();
    fs::remove_file(&retained[0]).unwrap();

    let before = snapshot(&h.root);
    assert_eq!(
        refusal(at("2026-11-16T10:00:00+05:30")),
        "host_migrate_activation_blackout"
    );
    let market_hours = run(
        &h.home,
        &h.stage,
        false,
        true,
        at("2026-11-16T10:00:00+05:30"),
    )
    .unwrap();
    assert_eq!(market_hours["status"], "dry_run");
    assert_eq!(market_hours["during_market_hours"], true);

    let lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(
            h.home
                .join("ind-sadr-data/current/sessions/controller.lock"),
        )
        .unwrap();
    lock.lock_exclusive().unwrap();
    assert_eq!(refusal(evening()), "host_migrate_lock_held");
    drop(lock);
    fs::remove_file(
        h.home
            .join("ind-sadr-data/current/sessions/controller.lock"),
    )
    .unwrap();

    let pending = h.home.join(".local/state/ind-sadr/pending.json");
    fs::write(&pending, "{}").unwrap();
    assert_eq!(
        refusal(evening()),
        "host_migrate_update_transaction_requires_review"
    );
    fs::remove_file(&pending).unwrap();

    let mut paper = parse(&fs::read(&h.paper).unwrap()).unwrap();
    paper["dashboard_token_file"] = json!(h.home.join(".config/nivra/dashboard-token"));
    write_json(&h.paper, &paper);
    assert_eq!(refusal(evening()), "host_migrate_book_binding_mismatch");
    fs::write(&h.paper, &before[&h.paper]).unwrap();

    assert_eq!(snapshot(&h.root), before, "refusals write nothing");
}

/// A dated planner schedule whose predecessor is legacy-shaped, with the paper
/// config's session before `effective_from`: the observed migration failure.
fn dated_chain(h: &Home, previous_shortlist: usize, active_shortlist: usize) -> (PathBuf, PathBuf) {
    let config = h.home.join(CONFIG_DIR);
    let active = config.join("planner.json");
    let previous = config.join("planner-previous.json");
    let mut legacy = parse(&fs::read(&active).unwrap()).unwrap();
    legacy["shortlist_n"] = json!(previous_shortlist);
    write_json(&previous, &legacy);
    let mut value = legacy.clone();
    value["shortlist_n"] = json!(active_shortlist);
    value["effective_from"] = json!("2026-11-16");
    value["previous_config"] = json!(previous);
    write_json(&active, &value);
    (active, previous)
}

#[test]
fn migrates_the_previous_config_planner_chain_and_refuses_bad_chains() {
    let h = fixture();
    let config = h.home.join(CONFIG_DIR);
    let (active, previous) = dated_chain(&h, 111, 222);
    let book_session: chrono::NaiveDate = parse(&fs::read(&h.paper).unwrap()).unwrap()["session"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let effective: chrono::NaiveDate = "2026-11-16".parse().unwrap();
    assert!(book_session < effective);
    // The active policy hash, computed from the migrated active shape alone.
    let mut expected = parse(&fs::read(&active).unwrap()).unwrap();
    for key in removed_keys(Kind::Planner) {
        drop_key(&mut expected, key, "", &mut Vec::new());
    }
    let expected_path = config.join("planner-policy-expected.json");
    write_json(&expected_path, &expected);
    // The observed bug: migration rewrites only the active planner, and
    // preflight resolves the book session through the legacy predecessor.
    assert!(
        planner::PlannerConfig::load_for_session(&expected_path, book_session).is_err(),
        "legacy predecessor must refuse before migration"
    );
    let active_policy =
        planner::policy_bytes(&planner::PlannerConfig::load(&expected_path).unwrap()).unwrap();
    let legacy_previous = fs::read(&previous).unwrap();

    let before = snapshot(&h.root);
    let preview = run(&h.home, &h.stage, false, false, evening()).unwrap();
    assert_eq!(preview["status"], "dry_run");
    assert_eq!(snapshot(&h.root), before, "dry run writes nothing");
    let planned = preview["steps"].to_string();
    for path in [&active, &previous] {
        assert!(
            planned.contains(&format!("\"path\":\"{}\"", path.display())),
            "chain planner {} is in the report",
            path.display()
        );
    }

    let applied = run(&h.home, &h.stage, true, false, evening()).unwrap();
    assert_eq!(applied["status"], "applied");
    // Preflight's resolution step: the book session uses the rewritten predecessor.
    let predecessor = planner::PlannerConfig::load_for_session(&active, book_session).unwrap();
    assert_eq!(predecessor.shortlist_n, 111);
    let current = planner::PlannerConfig::load_for_session(&active, effective).unwrap();
    assert_eq!(current.shortlist_n, 222);
    // The predecessor is preserved as evidence and validated under the new types.
    assert_eq!(
        fs::read(sibling(&previous, BACKUP_SUFFIX)).unwrap(),
        legacy_previous
    );
    let rewritten = parse(&fs::read(&previous).unwrap()).unwrap();
    assert!(rewritten.get("decision_owner").is_none());
    assert!(rewritten.get("decide").is_none());
    planner::PlannerConfig::load(&previous).unwrap();
    // The active policy hash is unchanged by the predecessor entry.
    assert_eq!(
        planner::policy_bytes(&planner::PlannerConfig::load(&active).unwrap()).unwrap(),
        active_policy
    );
    assert_eq!(
        planner::policy_bytes(&current).unwrap(),
        active_policy,
        "resolving the active planner does not depend on the predecessor"
    );
    assert_eq!(
        parse(&fs::read(&active).unwrap()).unwrap()["previous_config"],
        json!(previous)
    );
    let migrated = snapshot(&h.root);
    let again = run(&h.home, &h.stage, true, false, evening()).unwrap();
    assert_eq!(again["status"], "already_migrated");
    assert_eq!(snapshot(&h.root), migrated, "rerun is a no-op");

    // A cycle in the predecessor chain refuses before writing.
    let h = fixture();
    let (_, previous) = dated_chain(&h, 111, 222);
    let mut value = parse(&fs::read(&previous).unwrap()).unwrap();
    value["effective_from"] = json!("2026-10-01");
    value["previous_config"] = json!(previous);
    write_json(&previous, &value);
    let before = snapshot(&h.root);
    assert_eq!(
        plan(&h.home, &h.stage).err(),
        Some("host_migrate_planner_chain_cycle")
    );
    assert_eq!(snapshot(&h.root), before, "cycle refusal writes nothing");

    // A chain deeper than the resolver bound refuses with an explicit reason.
    let h = fixture();
    let config = h.home.join(CONFIG_DIR);
    let (active, previous) = dated_chain(&h, 111, 222);
    let base = {
        let mut value = parse(&fs::read(&active).unwrap()).unwrap();
        let map = value.as_object_mut().unwrap();
        map.remove("effective_from");
        map.remove("previous_config");
        for key in removed_keys(Kind::Planner) {
            drop_key(&mut value, key, "", &mut Vec::new());
        }
        value
    };
    let mut path = config.join("planner-chain-30.json");
    write_json(&path, &base);
    for i in (0..30).rev() {
        let next = config.join(format!("planner-chain-{i}.json"));
        let mut value = base.clone();
        value["effective_from"] = json!("2026-11-16");
        value["previous_config"] = json!(path);
        write_json(&next, &value);
        path = next;
    }
    let mut value = parse(&fs::read(&previous).unwrap()).unwrap();
    for key in removed_keys(Kind::Planner) {
        drop_key(&mut value, key, "", &mut Vec::new());
    }
    value["effective_from"] = json!("2026-11-16");
    value["previous_config"] = json!(path);
    write_json(&previous, &value);
    let before = snapshot(&h.root);
    assert_eq!(
        plan(&h.home, &h.stage).err(),
        Some("host_migrate_planner_chain_too_deep")
    );
    assert_eq!(snapshot(&h.root), before, "too-deep refusal writes nothing");
}
