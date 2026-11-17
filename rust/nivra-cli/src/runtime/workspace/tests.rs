use super::super::planner::{self, PlannerConfig};
use super::*;
use serde_json::{Value, json};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn settings() -> WorkspaceSettings {
    serde_json::from_value(json!({"enabled": true})).unwrap()
}

fn enabled_book() -> (tempfile::TempDir, PathBuf, WorkspaceSettings) {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("paper.sqlite"), b"BOOK").unwrap();
    fs::write(dir.path().join("book-identity.json"), b"{}").unwrap();
    let settings = settings();
    let root = ensure(dir.path(), &settings).unwrap();
    (dir, root, settings)
}

#[test]
fn workspace_seeds_layout_without_touching_the_book() {
    for input in [json!("{\"limit\":2}"), json!({"limit":2}), Value::Null] {
        assert!(
            parse_tool(
                "workspace_run",
                json!({"script":"scanner/cost_aware_momentum.py","input":input,"input_path":null})
            )
            .is_ok()
        );
    }
    for input in [json!("[]"), json!("invalid"), json!("42")] {
        assert!(
            parse_tool(
                "workspace_run",
                json!({"script":"scanner/cost_aware_momentum.py","input":input,"input_path":null})
            )
            .is_err()
        );
    }
    let (dir, root, settings) = enabled_book();
    for name in [
        "README.md",
        "scanner/cost_aware_momentum.py",
        "scanner/cost_aware_momentum.params.json",
        "notes",
        "skills",
        "tools",
        "data",
        "reports",
        "journal",
    ] {
        assert!(root.join(name).exists(), "{name}");
    }
    assert_eq!(
        fs::read_to_string(root.join("scanner/cost_aware_momentum.py")).unwrap(),
        SCANNER_PY
    );
    fs::write(root.join("README.md"), "operator note").unwrap();
    ensure(dir.path(), &settings).unwrap();
    assert_eq!(
        fs::read_to_string(root.join("README.md")).unwrap(),
        "operator note"
    );
    assert_eq!(fs::read(dir.path().join("paper.sqlite")).unwrap(), b"BOOK");
    assert_eq!(
        fs::read(dir.path().join("book-identity.json")).unwrap(),
        b"{}"
    );
    let mut missing = PlannerConfig::default();
    assert!(missing.workspace.is_none());
    let before = planner::policy_bytes(&missing).unwrap();
    missing.workspace = Some(settings.clone());
    let after = planner::policy_bytes(&missing).unwrap();
    assert_ne!(before, after);

    // #222: outbound network is explicit opt-in and omitted from serialized
    // policy while disabled; the sandbox config follows the reviewed setting.
    assert!(!settings.network);
    assert!(
        serde_json::to_value(&settings)
            .unwrap()
            .get("network")
            .is_none()
    );
    assert!(!sandbox_config(&settings).network);
    let networked: WorkspaceSettings =
        serde_json::from_value(json!({"enabled": true, "network": true})).unwrap();
    assert!(networked.network);
    assert!(sandbox_config(&networked).network);
    let mut policy = missing.clone();
    policy.workspace = Some(networked);
    assert_ne!(planner::policy_bytes(&policy).unwrap(), after);
}

#[test]
fn workspace_write_read_list_and_path_policy() {
    let (dir, _root, settings) = enabled_book();
    let book = dir.path();
    let written = execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "notes/day.md".into(),
            text: "persist this".into(),
        },
    )
    .unwrap();
    assert_eq!(written["path"], "notes/day.md");
    assert_eq!(written["execution_authorized"], false);
    let read = execute(book, &settings, WorkspaceOp::Read("notes/day.md".into())).unwrap();
    assert_eq!(read["text"], "persist this");
    let listed = execute(book, &settings, WorkspaceOp::List(Some("notes".into()))).unwrap();
    assert_eq!(listed["entries"][0]["name"], "day.md");
    for path in [
        "../paper.sqlite",
        "/etc/passwd",
        "scanner/cost_aware_momentum.py",
        "tools/sneak.py",
        "notes/../scanner/x.py",
    ] {
        assert!(
            execute(
                book,
                &settings,
                WorkspaceOp::Write {
                    path: path.into(),
                    text: "no".into(),
                },
            )
            .is_err(),
            "{path}"
        );
    }
    assert_eq!(fs::read(book.join("paper.sqlite")).unwrap(), b"BOOK");
    assert!(execute(book, &settings, WorkspaceOp::Read("../paper.sqlite".into())).is_err());
    assert!(
        serde_json::from_value::<WorkspaceSettings>(json!({
            "enabled": true,
            "root": "../outside"
        }))
        .unwrap()
        .validate()
        .is_err()
    );

    // Research scope writes only notes/skills and never runs or installs.
    execute_scoped(
        book,
        &settings,
        WorkspaceScope::Research,
        WorkspaceOp::Write {
            path: "notes/research.md".into(),
            text: "research note".into(),
        },
    )
    .unwrap();
    execute_scoped(
        book,
        &settings,
        WorkspaceScope::Research,
        WorkspaceOp::Write {
            path: "notes/sub/deep.md".into(),
            text: "nested note".into(),
        },
    )
    .unwrap();
    for path in ["reports/research.md", "data/research.json", "journal/x.md"] {
        assert!(
            execute_scoped(
                book,
                &settings,
                WorkspaceScope::Research,
                WorkspaceOp::Write {
                    path: path.into(),
                    text: "no".into(),
                },
            )
            .is_err(),
            "{path}"
        );
    }
    assert!(
        execute_scoped(
            book,
            &settings,
            WorkspaceScope::Research,
            WorkspaceOp::Run {
                script: "scanner/cost_aware_momentum.py".into(),
                input: None,
                input_path: None,
            },
        )
        .is_err()
    );
    assert!(
        execute_scoped(
            book,
            &settings,
            WorkspaceScope::Research,
            WorkspaceOp::Install {
                name: "extra.py".into(),
                text: "print(1)\n".into(),
            },
        )
        .is_err()
    );

    // Pinned refs are bounded to notes/skills, verified on the original path,
    // and never copied. Changed, missing and symlinked sources fail explicitly.
    let state = seed_state(book, &settings, "2026-10-15", 123);
    assert_eq!(state["available"], true);
    assert_eq!(state["version"], 1);
    let refs = state["refs"].as_array().unwrap();
    assert!(!refs.is_empty());
    assert!(refs.iter().all(|reference| {
        matches!(reference["kind"].as_str(), Some("notes" | "skills"))
            && reference["source_session"] == "2026-10-15"
            && reference["captured_at_ms"] == 123
    }));
    assert!(
        refs.iter()
            .any(|reference| reference["path"] == "notes/sub/deep.md")
    );
    let reference: WorkspaceRefV1 = serde_json::from_value(refs[0].clone()).unwrap();
    let pinned = read_pinned(book, &settings, &reference).unwrap();
    assert_eq!(pinned["sha256"], reference.sha256);
    assert_eq!(pinned["untrusted"], true);
    assert_eq!(pinned["execution_authorized"], false);
    let original = fs::read(book.join("workspace").join(&reference.path)).unwrap();
    fs::write(book.join("workspace").join(&reference.path), b"changed").unwrap();
    assert_eq!(
        read_pinned(book, &settings, &reference)
            .unwrap_err()
            .to_string(),
        "workspace_ref_changed"
    );
    fs::write(book.join("workspace").join(&reference.path), &original).unwrap();
    fs::remove_file(book.join("workspace").join(&reference.path)).unwrap();
    assert_eq!(
        read_pinned(book, &settings, &reference)
            .unwrap_err()
            .to_string(),
        "workspace_ref_missing"
    );
    std::os::unix::fs::symlink(
        book.join("paper.sqlite"),
        book.join("workspace/notes/link.md"),
    )
    .unwrap();
    let link = WorkspaceRefV1 {
        version: 1,
        source_session: "2026-10-16".into(),
        path: "notes/link.md".into(),
        sha256: "0".repeat(64),
        captured_at_ms: 1,
        kind: WorkspaceRefKind::Notes,
    };
    assert!(matches!(
        read_pinned(book, &settings, &link)
            .unwrap_err()
            .to_string()
            .as_str(),
        "workspace_ref_symlink" | "workspace_symlink"
    ));
    let escaped = WorkspaceRefV1 {
        path: "reports/escape.md".into(),
        ..link.clone()
    };
    assert!(read_pinned(book, &settings, &escaped).is_err());

    // Old seeds without workspace_state stay byte-identical; a new seed adds
    // the field only when the reviewed workspace policy supplies refs.
    let mut context = session::SeedContext {
        source_turn_id: "turn-old".into(),
        standing_instructions: "standing".into(),
        positions: json!([]),
        directives: json!([]),
        alerts: json!([]),
        research_state: json!({"available":true,"tasks":[]}),
        unresolved_work_refs: json!({"available":true,"refs":[]}),
        review_state: Value::Null,
        workspace_state: Value::Null,
    };
    let old = session::build_seed(book, "2026-10-15", "2026-10-16", &context);
    assert!(old.get("workspace_state").is_none());
    assert_eq!(
        serde_json::to_vec(&old).unwrap(),
        serde_json::to_vec(&session::build_seed(
            book,
            "2026-10-15",
            "2026-10-16",
            &context
        ))
        .unwrap()
    );
    session::validate_seed(&old, "2026-10-16").unwrap();
    context.workspace_state = state;
    let new = session::build_seed(book, "2026-10-15", "2026-10-16", &context);
    assert!(new.get("workspace_state").is_some());
    session::validate_seed(&new, "2026-10-16").unwrap();
    let mut unknown = new.clone();
    unknown["workspace_state"]["extra"] = json!(true);
    assert!(session::validate_seed(&unknown, "2026-10-16").is_err());

    // Issue 8: a valid component containing two dots is not a parent escape.
    execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "notes/v1..2.md".into(),
            text: "dots".into(),
        },
    )
    .unwrap();
    let dotted_state = seed_state(book, &settings, "2026-10-15", 77);
    let dotted_ref = dotted_state["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|reference| reference["path"] == "notes/v1..2.md")
        .cloned()
        .unwrap();
    let dotted: WorkspaceRefV1 = serde_json::from_value(dotted_ref).unwrap();
    let dotted_context = session::SeedContext {
        source_turn_id: "turn-dotted".into(),
        standing_instructions: "standing".into(),
        positions: json!([]),
        directives: json!([]),
        alerts: json!([]),
        research_state: json!({"available":true,"tasks":[]}),
        unresolved_work_refs: json!({"available":true,"refs":[]}),
        review_state: Value::Null,
        workspace_state: dotted_state.clone(),
    };
    let dotted_seed = session::build_seed(book, "2026-10-15", "2026-10-16", &dotted_context);
    session::validate_seed(&dotted_seed, "2026-10-16").unwrap();
    session::write_seed(book, "2026-10-16", &dotted_seed).unwrap();
    assert_eq!(
        read_pinned(book, &settings, &dotted).unwrap()["text"],
        "dots"
    );
    for bad in [
        "../escape.md",
        "/absolute.md",
        "notes/../escape.md",
        "notes/..",
    ] {
        let mut bad_seed = dotted_seed.clone();
        bad_seed["workspace_state"]["refs"][0]["path"] = json!(bad);
        assert!(
            session::validate_seed(&bad_seed, "2026-10-16").is_err(),
            "{bad}"
        );
    }

    // Issue 9: pinning uses the reviewed file bound and the same bounded UTF-8
    // reader as a pinned read; an unavailable file never becomes a readable ref.
    let mut small = settings.clone();
    small.max_file_bytes = 4096;
    fs::write(book.join("workspace/notes/big.md"), vec![b'x'; 10 * 1024]).unwrap();
    fs::write(book.join("workspace/notes/binary.md"), [0xff, 0xfe, 0x00]).unwrap();
    fs::write(book.join("workspace/notes/ok.md"), b"ok").unwrap();
    let small_state = seed_state(book, &small, "2026-10-15", 88);
    let small_paths = small_state["refs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|reference| reference["path"].as_str())
        .collect::<Vec<_>>();
    assert!(!small_paths.contains(&"notes/big.md"));
    assert!(!small_paths.contains(&"notes/binary.md"));
    assert!(small_paths.contains(&"notes/ok.md"));
    let big_ref = WorkspaceRefV1 {
        version: 1,
        source_session: "2026-10-15".into(),
        path: "notes/big.md".into(),
        sha256: digest(&fs::read(book.join("workspace/notes/big.md")).unwrap()),
        captured_at_ms: 1,
        kind: WorkspaceRefKind::Notes,
    };
    assert_eq!(
        read_pinned(book, &small, &big_ref).unwrap_err().to_string(),
        "workspace_ref_too_large"
    );

    // Issue 10: an existing non-private file is replaced by a new private inode
    // atomically; a reader holding the old inode still sees the old bytes.
    let legacy_path = _root.join("notes/legacy.md");
    fs::write(&legacy_path, b"old").unwrap();
    fs::set_permissions(&legacy_path, fs::Permissions::from_mode(0o644)).unwrap();
    let mut old_reader = fs::File::open(&legacy_path).unwrap();
    execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "notes/legacy.md".into(),
            text: "new".into(),
        },
    )
    .unwrap();
    assert_eq!(fs::metadata(&legacy_path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(fs::read_to_string(&legacy_path).unwrap(), "new");
    let mut old_text = String::new();
    old_reader.read_to_string(&mut old_text).unwrap();
    assert_eq!(old_text, "old");
    let leftovers = fs::read_dir(_root.join("notes"))
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    let outside_write = book.join("outside-write.txt");
    fs::write(&outside_write, b"outside").unwrap();
    fs::set_permissions(&outside_write, fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(&outside_write, _root.join("notes/link-write.md")).unwrap();
    assert!(
        execute(
            book,
            &settings,
            WorkspaceOp::Write {
                path: "notes/link-write.md".into(),
                text: "no".into(),
            },
        )
        .is_err()
    );
    assert_eq!(fs::read(&outside_write).unwrap(), b"outside");
    assert_eq!(fs::metadata(&outside_write).unwrap().mode() & 0o777, 0o644);
    let outside_hard = book.join("outside-hard.txt");
    fs::write(&outside_hard, b"outside").unwrap();
    fs::set_permissions(&outside_hard, fs::Permissions::from_mode(0o644)).unwrap();
    fs::hard_link(&outside_hard, _root.join("notes/hard.md")).unwrap();
    execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "notes/hard.md".into(),
            text: "new".into(),
        },
    )
    .unwrap();
    assert_eq!(fs::read(&outside_hard).unwrap(), b"outside");
    assert_eq!(fs::metadata(&outside_hard).unwrap().mode() & 0o777, 0o644);
    assert_eq!(
        fs::read_to_string(_root.join("notes/hard.md")).unwrap(),
        "new"
    );
    assert_eq!(
        fs::metadata(_root.join("notes/hard.md")).unwrap().mode() & 0o777,
        0o600
    );
    let readonly = _root.join("notes/readonly");
    fs::create_dir(&readonly).unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(readonly.join("old.md"), b"old").unwrap();
    fs::set_permissions(readonly.join("old.md"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        execute(
            book,
            &settings,
            WorkspaceOp::Write {
                path: "notes/readonly/new.md".into(),
                text: "no".into(),
            },
        )
        .is_err()
    );
    assert_eq!(fs::read(readonly.join("old.md")).unwrap(), b"old");
    fs::set_permissions(&readonly, fs::Permissions::from_mode(0o700)).unwrap();
    let failed_leftovers = fs::read_dir(&readonly)
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with(".tmp"))
        .collect::<Vec<_>>();
    assert!(failed_leftovers.is_empty(), "{failed_leftovers:?}");

    // Issue 11: every current workspace component is owner-only, not only the
    // leaf that happened to be created last.
    execute(
        book,
        &settings,
        WorkspaceOp::Write {
            path: "notes/2026/11/day.md".into(),
            text: "nested".into(),
        },
    )
    .unwrap();
    for relative in ["notes", "notes/2026", "notes/2026/11"] {
        assert_eq!(
            fs::metadata(_root.join(relative)).unwrap().mode() & 0o777,
            0o700,
            "{relative}"
        );
    }
    assert_eq!(
        fs::metadata(_root.join("notes/2026/11/day.md"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    fs::set_permissions(_root.join("notes/2026"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        execute(
            book,
            &settings,
            WorkspaceOp::Write {
                path: "notes/2026/11/other.md".into(),
                text: "no".into(),
            },
        )
        .is_err()
    );
    fs::set_permissions(_root.join("notes/2026"), fs::Permissions::from_mode(0o700)).unwrap();
    let outside_notes = book.join("outside-notes");
    fs::create_dir_all(&outside_notes).unwrap();
    fs::write(outside_notes.join("canary"), b"outside").unwrap();
    fs::set_permissions(&outside_notes, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(_root.join("notes/2026/11")).unwrap();
    std::os::unix::fs::symlink(&outside_notes, _root.join("notes/2026/11")).unwrap();
    assert!(
        execute(
            book,
            &settings,
            WorkspaceOp::Write {
                path: "notes/2026/11/day2.md".into(),
                text: "no".into(),
            },
        )
        .is_err()
    );
    assert_eq!(fs::read(outside_notes.join("canary")).unwrap(), b"outside");
    assert!(fs::symlink_metadata(outside_notes.join("day2.md")).is_err());
}

#[test]
fn workspace_run_allowlist_and_install_gate() {
    let (dir, _root, settings) = enabled_book();
    let book = dir.path();
    assert!(
        execute(
            book,
            &settings,
            WorkspaceOp::Run {
                script: "notes/day.md".into(),
                input: None,
                input_path: None,
            },
        )
        .is_err()
    );
    let params: Value = serde_json::from_str(SCANNER_PARAMS).unwrap();
    let base = json!({
        "price": 500.0, "move_bps": 150.0, "market_relative_bps": 180.0,
        "traded_value": 50_000_000.0, "circuit_distance_bps": 800.0,
        "volatility_bps": 120.0, "range_position": 0.9,
        "best_bid": 499.9, "best_ask": 500.0,
        "best_bid_qty": 500.0, "best_ask_qty": 600.0,
        "spread_fraction": 0.0002, "book_imbalance": 0.2,
        "session_vwap": 495.0, "rel_volume_tod": 2.5,
        "minutes_since_open": 120.0
    });
    let mut hot = base.clone();
    hot["move_bps"] = 300.0.into();
    hot["market_relative_bps"] = 330.0.into();
    hot["session_vwap"] = 490.0.into();
    hot["rel_volume_tod"] = 3.0.into();
    let mut down = base.clone();
    down["move_bps"] = (-150.0).into();
    down["market_relative_bps"] = (-120.0).into();
    down["session_vwap"] = 505.0.into();
    let input = json!({
        "params": params,
        "GOOD": base,
        "HOT": hot,
        "DOWN": down
    });
    let scored = execute(
        book,
        &settings,
        parse_tool("workspace_run", json!({"script":"scanner/cost_aware_momentum.py", "input":input.to_string(), "input_path":null})).unwrap(),
    )
    .unwrap();
    fs::write(_root.join("data/input.json"), input.to_string()).unwrap();
    let path_scored = execute(book, &settings, parse_tool("workspace_run", json!({
        "script":"scanner/cost_aware_momentum.py", "input":null, "input_path":"data/input.json"
    })).unwrap()).unwrap();
    assert_eq!(path_scored["scores"], scored["scores"]);
    assert!(execute(book, &settings, parse_tool("workspace_run", json!({
        "script":"scanner/cost_aware_momentum.py", "input":input.to_string(), "input_path":"data/input.json"
    })).unwrap()).is_err(), "inline and path inputs must not be combined");
    assert_eq!(scored["execution_authorized"], false);
    assert_eq!(scored["authority"], "advisory_scores_not_orders");
    let scores = scored["scores"].as_object().unwrap();
    assert!(scores["HOT"].as_f64().unwrap() > scores["GOOD"].as_f64().unwrap());
    assert!(!scores.contains_key("DOWN"));
    assert!(
        execute(
            book,
            &settings,
            WorkspaceOp::Install {
                name: "extra.py".into(),
                text: "print(1)\n".into(),
            },
        )
        .is_err()
    );
    let text = "print(1)\n";
    let mut allowed = settings.clone();
    allowed.install_allowlist = vec![InstallAllow {
        name: "extra.py".into(),
        sha256: digest(text.as_bytes()),
    }];
    let installed = execute(
        book,
        &allowed,
        WorkspaceOp::Install {
            name: "extra.py".into(),
            text: text.into(),
        },
    )
    .unwrap();
    assert_eq!(installed["path"], "tools/extra.py");
    allowed.install_allowlist[0].sha256 = "0".repeat(64);
    assert!(
        execute(
            book,
            &allowed,
            WorkspaceOp::Install {
                name: "extra.py".into(),
                text: text.into(),
            },
        )
        .is_err()
    );
}
