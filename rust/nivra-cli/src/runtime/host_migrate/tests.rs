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

/// Monday after the close.
fn evening() -> DateTime<FixedOffset> {
    at("2026-11-16T18:00:00+05:30")
}

fn write_json(path: &Path, value: &Value) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

/// A home shaped like the installed v0.9.33 host: kept data/config dirs, an
/// owner planner still carrying the removed legacy keys, and a bound live book.
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
    drop(super::super::open_book(&c).unwrap());
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

/// Every file's bytes and every link's target under the fixture root.
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

/// String leaves naming an `ind-sadr` location outside the kept roots.
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
    let before = snapshot(&h.root);

    let preview = run(&h.home, &h.stage, false, evening()).unwrap();
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

    let applied = run(&h.home, &h.stage, true, evening()).unwrap();
    assert_eq!(applied["status"], "applied");

    // Every rewritten config parses with the renamed binary's own types.
    let paper: Config = serde_json::from_slice(&fs::read(&h.paper).unwrap()).unwrap();
    let planner = planner::PlannerConfig::load(&config.join("planner.json")).unwrap();
    assert!(planner.orchestrator.unwrap().trigger_prompts.is_empty());
    for model in ["openai-model.json", "grok-model.json"] {
        crate::model::parse_config(&fs::read(config.join(model)).unwrap()).unwrap();
    }
    update::validate_config(&fs::read(config.join("updater.json")).unwrap()).unwrap();
    let updater = parse(&fs::read(config.join("updater.json")).unwrap()).unwrap();
    assert_eq!(updater["remote"], update::CANONICAL_REMOTE);
    // Defaults the old binary derived from ind-sadr paths are now explicit.
    let grok = parse(&fs::read(config.join("grok-model.json")).unwrap()).unwrap();
    assert_eq!(grok["grant_file"], json!(config.join("xai-grant")));
    assert_eq!(
        paper.alert_webhook_file,
        Some(config.join("alert-webhook.json"))
    );
    // The live book still opens: its binding hashes the unchanged kept paths.
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

    // Hard-coded locations moved; the old release pointer stays for rollback.
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
    let again = run(&h.home, &h.stage, true, evening()).unwrap();
    assert_eq!(again["status"], "already_migrated");
    assert_eq!(again["steps"], json!([]));
    assert_eq!(snapshot(&h.root), migrated, "rerun is a no-op");
}

#[test]
fn refuses_blackout_held_lock_open_update_and_binding_change() {
    let h = fixture();
    let before = snapshot(&h.root);
    let refusal = |now| run(&h.home, &h.stage, true, now).unwrap_err();

    assert_eq!(
        refusal(at("2026-11-16T10:00:00+05:30")),
        "host_migrate_activation_blackout"
    );

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

    // A bound path that moved would orphan the live book; refuse instead.
    let mut paper = parse(&fs::read(&h.paper).unwrap()).unwrap();
    paper["dashboard_token_file"] = json!(h.home.join(".config/nivra/dashboard-token"));
    write_json(&h.paper, &paper);
    assert_eq!(refusal(evening()), "host_migrate_book_binding_mismatch");
    fs::write(&h.paper, &before[&h.paper]).unwrap();

    assert_eq!(snapshot(&h.root), before, "refusals write nothing");
}
