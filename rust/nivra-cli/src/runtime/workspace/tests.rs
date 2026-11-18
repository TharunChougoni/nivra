use super::super::planner::{self, PlannerConfig};
use super::*;
use serde_json::{Value, json};
use std::fs;

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
