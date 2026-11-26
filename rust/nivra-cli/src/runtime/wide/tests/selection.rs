use super::*;

fn live_scan(now: i64) -> (Scanner, Vec<Instrument>) {
    use nivra_core::execution::{MarketFields, Sample};
    let mut scan = Scanner::default();
    let mut instruments = Vec::new();
    for (key, price) in [("A", "110"), ("B", "120"), ("C", "105")] {
        instruments.push(Instrument {
            key: key.into(),
            symbol: key.into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        });
        let current = Price::parse(price).unwrap();
        scan.samples.insert(
            key.into(),
            Sample {
                seen: now,
                source: Some(now),
                last: now,
                current: Some(current),
                history: vec![Price::parse("100").unwrap(), current],
                reason: "eligible_no_signal".into(),
                market_fields: Some(MarketFields {
                    session_vwap: Some(100.0),
                    best_bid: Some(99.0),
                    best_bid_qty: Some(10),
                    best_ask: Some(101.0),
                    best_ask_qty: Some(20),
                    total_buy_quantity: Some(100),
                    total_sell_quantity: Some(200),
                    volume: 100_000,
                    lower_circuit_limit: Amount::parse("1").unwrap(),
                    upper_circuit_limit: Amount::parse("200").unwrap(),
                }),
                market_fields_receipt: Some(now),
                regime_references: Some((
                    now,
                    Price::parse("100").unwrap(),
                    Price::parse("100").unwrap(),
                )),
                ..Default::default()
            },
        );
    }
    (scan, instruments)
}
fn live_label(scan: &Scanner, now: i64) -> String {
    let cfg = nivra_core::regime::RegimeConfig {
        min_instruments: 1,
        min_minutes: 2,
        ..Default::default()
    };
    nivra_core::regime::classify(
        &shadow_regime_inputs(scan, now, 2000, &[0.001, 0.001]),
        &cfg,
    )
    .label()
}
fn live_settings(path: &std::path::Path) -> super::super::discovery::Settings {
    serde_json::from_value(serde_json::json!({
        "max_scan_age_ms":2000,"max_prospects":10,
        "strategy_selection":{"enabled":true,"mode":"live","library_path":path,
            "regime":{"min_instruments":1,"min_minutes":2}}
    }))
    .unwrap()
}
fn add_live_version(
    path: &std::path::Path,
    source: &str,
    inputs: serde_json::Value,
    label: &str,
) -> nivra_sandbox::library::Version {
    use nivra_sandbox::library::{Library, NewVersion, RunResult};
    let mut db = Library::open(path).unwrap();
    let version = db
        .add_version(NewVersion {
            name: "live-candidate",
            file_body: source,
            author_model: "fixture-model",
            intent: "fixture live ranking",
            inputs: &inputs,
            created_at_ms: 1,
        })
        .unwrap();
    for kind in ["replay", "shadow"] {
        for day in 1..=5 {
            db.record_result(&RunResult {
                sha256: version.sha256.clone(),
                run_kind: kind.into(),
                session_date: format!("2026-10-{day:02}"),
                regime: Some(label.into()),
                metrics: serde_json::json!({"trades":6,"net_pnl_after_costs":1.0,
                        "expectancy":0.5,"max_drawdown":0.0}),
                recorded_at_ms: day,
            })
            .unwrap();
        }
    }
    version
}
#[test]
fn strategy_selection_mode_defaults_parses_and_rejects_unsafe_live() {
    let mut settings: super::super::discovery::Settings =
        serde_json::from_value(serde_json::json!({
            "max_scan_age_ms":2000,"max_prospects":10,
            "strategy_selection":{"enabled":false}
        }))
        .unwrap();
    assert_eq!(
        settings.strategy_selection.as_ref().unwrap().mode,
        super::super::discovery::StrategySelectionMode::Shadow
    );
    settings.validate().unwrap();
    for invalid in [
        serde_json::json!({"enabled":false,"mode":"live"}),
        serde_json::json!({"enabled":true,"mode":"live"}),
    ] {
        let mut bad = serde_json::to_value(&settings).unwrap();
        bad["strategy_selection"] = invalid;
        let bad: super::super::discovery::Settings = serde_json::from_value(bad).unwrap();
        assert!(bad.validate().is_err());
    }
    let mut bad = serde_json::to_value(&settings).unwrap();
    bad["strategy_selection"]["mode"] = serde_json::json!("unexpected");
    assert!(serde_json::from_value::<super::super::discovery::Settings>(bad).is_err());
    // Shadow mode keeps the mode field out of serialized configuration; live
    // mode must serialize it so the policy hash can bind the behaviour change.
    let serialized = serde_json::to_value(&settings).unwrap();
    assert!(serialized["strategy_selection"].get("mode").is_none());
    settings.strategy_selection.as_mut().unwrap().mode =
        super::super::discovery::StrategySelectionMode::Live;
    settings.strategy_selection.as_mut().unwrap().enabled = true;
    settings.strategy_selection.as_mut().unwrap().library_path = "/tmp/library".into();
    let serialized = serde_json::to_value(&settings).unwrap();
    assert_eq!(serialized["strategy_selection"]["mode"], "live");
    settings.validate().unwrap();
    let mut champion = settings.clone();
    champion.strategy = Some(
        serde_json::from_value(serde_json::json!({
            "enabled":true,"champion":"c","specs":[]
        }))
        .unwrap(),
    );
    assert!(champion.validate().is_err());
}
#[test]
fn live_selection_scores_reorder_filter_and_change_candidates() {
    if !std::path::Path::new("/usr/bin/bwrap").is_file() {
        eprintln!("skipping live ranking fixture: bwrap unavailable");
        return;
    }
    let now = start();
    let (scan, instruments) = live_scan(now);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("library.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(
        &path,
        "print('{\"A\": 1, \"C\": 9}')",
        serde_json::json!({}),
        &label,
    );
    let settings = live_settings(&path);
    settings.validate().unwrap();
    let default = super::super::super::discovery::project(&scan, &instruments, &settings, now);
    let default_keys = default["prospects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["instrument_key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(default_keys.contains(&"B"));
    let mut observer = ShadowSelection::default();
    observer.evaluate(&settings, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(observer.status.as_ref().unwrap()["reason"], "BestEvidence");
    assert_eq!(observer.state.active, version.sha256);
    let live = observer
        .live_ranking(&settings, &scan, &instruments, &default, now)
        .expect("live scores");
    assert_eq!(live.scores.len(), 2);
    let ranked = super::super::super::discovery::project_with_live(
        &scan,
        &instruments,
        &settings,
        now,
        Some(&live),
    );
    let keys = ranked["prospects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["instrument_key"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(keys, ["C", "A"]);
    assert_eq!(ranked["eligible_count"], 2);
    assert_eq!(ranked["live_ranking"]["sha256"], version.sha256);
    assert_eq!(ranked["live_ranking"]["name"], "live-candidate");
    let default_candidates = super::super::super::discovery::candidates(&default);
    let live_candidates = super::super::super::discovery::candidates(&ranked);
    assert_ne!(default_candidates, live_candidates);
    assert_eq!(
        live_candidates
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["C", "A"]
    );
    let mut limited = settings.clone();
    limited.max_prospects = 1;
    let ranked = super::super::super::discovery::project_with_live(
        &scan,
        &instruments,
        &limited,
        now,
        Some(&live),
    );
    assert_eq!(ranked["prospects"].as_array().unwrap().len(), 1);
    assert_eq!(ranked["prospects"][0]["instrument_key"], "C");
    let status = observer.status.as_ref().unwrap();
    assert_eq!(status["mode"], "live");
    assert_eq!(status["shadow_only"], false);
    assert_eq!(status["live_applied"], true);
    assert_eq!(status["live_scored_count"], 2);
    assert_eq!(status["chosen_name"], "live-candidate");
    assert_eq!(status["previous_active"], "default");
    assert!(status["recent"].as_array().unwrap().len() <= 8);
}
#[test]
fn reviewed_research_ranker_scores_from_the_registered_source() {
    use crate::runtime::workspace::{self, WorkspaceOp, WorkspaceScope};

    let dir = tempfile::tempdir().unwrap();
    let book = dir.path();
    let library = book.join("library.sqlite");
    let planner_path = book.join("planner.json");
    let now = start();
    let session = local(now).date_naive();
    let (scan, instruments) = live_scan(now);
    let mut discovery = live_settings(&library);
    let cfg = crate::runtime::planner::PlannerConfig {
        discovery: Some(discovery.clone()),
        workspace: Some(serde_json::from_value(serde_json::json!({"enabled":true})).unwrap()),
        ..Default::default()
    };
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let workspace = cfg.workspace.as_ref().unwrap();
    let script = "scripts/ranker.py";
    let write = |source: &str| {
        workspace::execute_scoped(
            book,
            workspace,
            WorkspaceScope::ResearchSandbox,
            WorkspaceOp::Write {
                path: script.into(),
                text: source.into(),
            },
        )
        .unwrap();
    };
    let source = "print('{\"A\": 1, \"C\": 9}')\n";
    write(source);
    let sha = digest(source.as_bytes());
    let register = |expected_sha256: String| {
        planner::entry(planner::Command::RegisterRanker {
            planner: planner_path.clone(),
            session,
            book: book.to_path_buf(),
            script: script.into(),
            expected_sha256,
            author_model: "fixture-model".into(),
        })
    };
    assert_eq!(register("0".repeat(64)), std::process::ExitCode::FAILURE);
    assert!(!library.exists());
    assert_eq!(register(sha.clone()), std::process::ExitCode::SUCCESS);

    // Registering evidence alone leaves the built-in ranking in control.
    let baseline = super::super::super::discovery::project(&scan, &instruments, &discovery, now);
    let mut observer = ShadowSelection::default();
    observer.evaluate(&discovery, &scan, now, false, &[0.001, 0.001]);
    assert!(
        observer
            .live_ranking(&discovery, &scan, &instruments, &baseline, now)
            .is_none()
    );

    // A subsequent workspace edit cannot change the reviewed source.
    write("print('{}')\n");
    discovery
        .strategy_selection
        .as_mut()
        .unwrap()
        .paper_trial_sha256 = Some(sha.clone());
    discovery.validate().unwrap();
    let mut observer = ShadowSelection::default();
    observer.evaluate(&discovery, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(
        observer.status.as_ref().unwrap()["reason"],
        "ReviewedPaperTrial"
    );
    if !Path::new("/usr/bin/bwrap").is_file() {
        return;
    }
    let live = observer
        .live_ranking(&discovery, &scan, &instruments, &baseline, now)
        .unwrap();
    let ranked = super::super::super::discovery::project_with_live(
        &scan,
        &instruments,
        &discovery,
        now,
        Some(&live),
    );
    let prospects = super::super::super::discovery::candidates(&ranked);
    assert_eq!(
        prospects
            .iter()
            .map(|(key, _)| key.as_str())
            .collect::<Vec<_>>(),
        ["C", "A"]
    );
    assert_eq!(ranked["live_ranking"]["sha256"], sha);
}
#[test]
fn shadow_selection_never_scores_or_requests_live_ranking() {
    let now = start();
    let (scan, instruments) = live_scan(now);
    let mut settings: super::super::discovery::Settings =
        serde_json::from_value(serde_json::json!({
            "max_scan_age_ms":2000,"max_prospects":10,
            "strategy_selection":{"enabled":true,"mode":"shadow","library_path":"/missing/library"}
        }))
        .unwrap();
    settings.validate().unwrap();
    let projection = super::super::super::discovery::project(&scan, &instruments, &settings, now);
    let mut observer = ShadowSelection {
        mode: super::super::discovery::StrategySelectionMode::Shadow,
        state: nivra_sandbox::selector::SelectorState {
            active: "ab".repeat(32),
            last_switch_ms: None,
        },
        chosen_name: Some("candidate".into()),
        status: Some(serde_json::json!({"regime":"range"})),
        ..Default::default()
    };
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    assert!(observer.live.is_none());
    assert!(observer.source.is_none());
    assert!(observer.fallback.is_none());
    assert!(observer.cached_live().is_none());
    settings.strategy_selection = None;
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    let plain = super::super::super::discovery::project(&scan, &instruments, &settings, now);
    assert_eq!(
        serde_json::to_value(super::super::super::discovery::project_with_live(
            &scan,
            &instruments,
            &settings,
            now,
            None
        ))
        .unwrap(),
        serde_json::to_value(&plain).unwrap()
    );
}
#[test]
fn live_fallbacks_cover_selection_sandbox_and_source_failures() {
    use nivra_sandbox::library::{Library, RunResult};
    let now = start();
    let (scan, instruments) = live_scan(now);
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent.sqlite");
    let mut observer = ShadowSelection::default();
    let settings = live_settings(&absent);
    let projection = super::super::super::discovery::project(&scan, &instruments, &settings, now);
    // Missing library is a selector/scorer failure, never an initialization write.
    observer.evaluate(&settings, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(observer.status.as_ref().unwrap()["reason"], "ScorerFailure");
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "library_unavailable"
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["reason"],
        "ScorerFailure"
    );
    assert!(!absent.exists());
    // Missing exact source by SHA.
    let empty = dir.path().join("empty.sqlite");
    Library::open(&empty).unwrap();
    let settings = live_settings(&empty);
    observer.status = Some(serde_json::json!({"regime":"range"}));
    observer.state.active = "ab".repeat(32);
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "missing_source"
    );
    // Sandbox setup failure is typed and never falls back to direct execution.
    let path = dir.path().join("sandbox-failure.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(&path, "print('{\"A\": 1}')", serde_json::json!({}), &label);
    let mut settings = live_settings(&path);
    settings.scorer_sandbox = Some(nivra_sandbox::ScorerSandboxConfig {
        bwrap_path: "/nonexistent/nivra-bwrap".into(),
        ..Default::default()
    });
    let mut observer = ShadowSelection {
        status: Some(serde_json::json!({"regime":label})),
        state: nivra_sandbox::selector::SelectorState {
            active: version.sha256,
            last_switch_ms: None,
        },
        ..Default::default()
    };
    assert!(
        observer
            .live_ranking(&settings, &scan, &instruments, &projection, now)
            .is_none()
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "sandbox_unavailable"
    );
    assert_eq!(observer.status.as_ref().unwrap()["live_applied"], false);
    // Invalid and empty score maps fail closed. Real execution needs bwrap.
    if std::path::Path::new("/usr/bin/bwrap").is_file() {
        for (source, inputs, detail) in [
            (
                "print('{\"params\": 1}')",
                serde_json::json!({"x":1}),
                "invalid_scores",
            ),
            ("print('{}')", serde_json::json!({}), "no_scored_candidates"),
        ] {
            let path = dir.path().join(format!("{detail}.sqlite"));
            let label = live_label(&scan, now);
            let version = add_live_version(&path, source, inputs, &label);
            let settings = live_settings(&path);
            let mut observer = ShadowSelection {
                status: Some(serde_json::json!({"regime":label})),
                state: nivra_sandbox::selector::SelectorState {
                    active: version.sha256,
                    last_switch_ms: None,
                },
                ..Default::default()
            };
            assert!(
                observer
                    .live_ranking(&settings, &scan, &instruments, &projection, now)
                    .is_none()
            );
            let fallback = &observer.status.as_ref().unwrap()["fallback"];
            assert_eq!(fallback["detail"], detail);
            assert_eq!(observer.status.as_ref().unwrap()["live_applied"], false);
        }
        // Every typed sandbox failure and a score set with no eligible
        // prospect has a stable fallback reason.
        let label = live_label(&scan, now);
        let cases = [
            (
                "sandbox_config",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    max_keys: 0,
                    ..Default::default()
                },
            ),
            (
                "sandbox_input",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    max_keys: 1,
                    ..Default::default()
                },
            ),
            (
                "sandbox_setup",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    python_path: "/nonexistent/nivra-python".into(),
                    ..Default::default()
                },
            ),
            (
                "sandbox_exit",
                "raise SystemExit(2)",
                nivra_sandbox::ScorerSandboxConfig::default(),
            ),
            (
                "sandbox_wall_timeout",
                "import time; time.sleep(5)",
                nivra_sandbox::ScorerSandboxConfig {
                    wall_ms: 1,
                    ..Default::default()
                },
            ),
            (
                "sandbox_output_limit",
                "print('{\"A\":1}')",
                nivra_sandbox::ScorerSandboxConfig {
                    max_output_bytes: 4,
                    ..Default::default()
                },
            ),
            (
                "sandbox_output_invalid",
                "print('[]')",
                nivra_sandbox::ScorerSandboxConfig::default(),
            ),
        ];
        for (detail, source, sandbox) in cases {
            let path = dir.path().join(format!("{detail}.sqlite"));
            let version = add_live_version(&path, source, serde_json::json!({}), &label);
            let mut settings = live_settings(&path);
            settings.scorer_sandbox = Some(sandbox);
            let mut observer = ShadowSelection {
                regime: Some(label.clone()),
                status: Some(serde_json::json!({"regime":label.clone()})),
                state: nivra_sandbox::selector::SelectorState {
                    active: version.sha256,
                    last_switch_ms: None,
                },
                ..Default::default()
            };
            assert!(
                observer
                    .live_ranking(&settings, &scan, &instruments, &projection, now)
                    .is_none()
            );
            assert_eq!(
                observer.status.as_ref().unwrap()["fallback"]["detail"],
                detail
            );
        }
        let path = dir.path().join("no-overlap.sqlite");
        let version = add_live_version(&path, "print('{\"A\": 1}')", serde_json::json!({}), &label);
        let settings = live_settings(&path);
        let restricted = serde_json::json!({"prospects":[{"instrument_key":"Z"}]});
        let mut observer = ShadowSelection {
            regime: Some(label.clone()),
            status: Some(serde_json::json!({"regime":label.clone()})),
            state: nivra_sandbox::selector::SelectorState {
                active: version.sha256,
                last_switch_ms: None,
            },
            ..Default::default()
        };
        assert!(
            observer
                .live_ranking(&settings, &scan, &instruments, &restricted, now)
                .is_none()
        );
        assert_eq!(
            observer.status.as_ref().unwrap()["fallback"]["detail"],
            "no_scored_candidates"
        );
    }
    // The exact-source path is still exercised without the hostile sandbox.
    let path = dir.path().join("valid.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(
        &path,
        "print('{\"A\": 1, \"C\": 9}')",
        serde_json::json!({}),
        &label,
    );
    let settings = live_settings(&path);
    let mut observer = ShadowSelection {
        regime: Some(label.clone()),
        status: Some(serde_json::json!({"regime":label})),
        state: nivra_sandbox::selector::SelectorState {
            active: version.sha256,
            last_switch_ms: None,
        },
        ..Default::default()
    };
    if std::path::Path::new("/usr/bin/bwrap").is_file() {
        assert!(
            observer
                .live_ranking(&settings, &scan, &instruments, &projection, now)
                .is_some()
        );
    }
    // Selector-level fallbacks keep their existing reasons plus live detail.
    for (stale, path, detail) in [
        (true, empty.clone(), "stale_data"),
        (false, empty.clone(), "no_eligible_candidates"),
        (false, absent.clone(), "library_unavailable"),
    ] {
        let settings = live_settings(&path);
        let mut observer = ShadowSelection::default();
        observer.evaluate(&settings, &scan, now, stale, &[0.001, 0.001]);
        assert_eq!(
            observer.status.as_ref().unwrap()["fallback"]["detail"],
            detail
        );
    }
    let mut invalid_settings = live_settings(&empty);
    invalid_settings
        .strategy_selection
        .as_mut()
        .unwrap()
        .selector
        .min_trades = 0;
    let mut invalid = ShadowSelection::default();
    invalid.evaluate(&invalid_settings, &scan, now, false, &[0.001, 0.001]);
    assert_eq!(invalid.status.as_ref().unwrap()["reason"], "InvalidConfig");
    assert_eq!(
        invalid.status.as_ref().unwrap()["fallback"]["detail"],
        "invalid_config"
    );
    // Unknown regime on an empty cache.
    let mut observer = ShadowSelection::default();
    observer.evaluate(&live_settings(&empty), &Scanner::default(), now, false, &[]);
    assert_eq!(observer.status.as_ref().unwrap()["reason"], "UnknownRegime");
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "unknown_regime"
    );
    // Active paper floor breach falls back before new evidence is considered.
    let path = dir.path().join("floor.sqlite");
    let label = live_label(&scan, now);
    let version = add_live_version(&path, "print('{}')", serde_json::json!({}), &label);
    {
        let db = Library::open(&path).unwrap();
        db.record_result(&RunResult {
            sha256: version.sha256.clone(),
            run_kind: "paper".into(),
            session_date: "2026-10-23".into(),
            regime: Some(label.clone()),
            metrics: serde_json::json!({"trades":1,"net_pnl_after_costs":-1.0,
                    "expectancy":-1.0,"max_drawdown":1.0}),
            recorded_at_ms: 1,
        })
        .unwrap();
    }
    let mut observer = ShadowSelection::default();
    observer.state.active = version.sha256;
    observer.evaluate(&live_settings(&path), &scan, now, false, &[0.001, 0.001]);
    assert_eq!(
        observer.status.as_ref().unwrap()["reason"],
        "PaperFloorBreach"
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["fallback"]["detail"],
        "paper_floor_breach"
    );
    assert_eq!(
        observer.status.as_ref().unwrap()["chosen_id"],
        nivra_sandbox::selector::DEFAULT_ID
    );
    // Every typed sandbox failure has a stable, non-secret detail.
    use nivra_sandbox::Failure::*;
    for (failure, detail) in [
        (Config, "sandbox_config"),
        (Input, "sandbox_input"),
        (Setup, "sandbox_setup"),
        (Unavailable, "sandbox_unavailable"),
        (Exit, "sandbox_exit"),
        (WallTimeout, "sandbox_wall_timeout"),
        (OutputLimit, "sandbox_output_limit"),
        (OutputInvalid, "sandbox_output_invalid"),
    ] {
        assert_eq!(sandbox_failure_detail(failure), detail);
    }
}
