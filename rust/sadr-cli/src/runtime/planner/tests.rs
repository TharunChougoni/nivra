fn enabled_scorer_requires_after_minute_from_either_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planner.json");
    let base = serde_json::json!({
        "enabled": true,
        "decide": {"enabled": true, "model_id": "fixture-model", "daily_call_budget": 4}
    });
    let mut missing = base.clone();
    missing["orchestrator"] = serde_json::json!({"enabled": true});
    std::fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
    let error = match super::PlannerConfig::load(&path) {
        Ok(_) => panic!("an enabled scorer must require a due minute"),
        Err(error) => error.to_string(),
    };
    assert_eq!(error, "planner_scorer_after_minute_required");

    let mut top_level = base.clone();
    top_level["orchestrator"] = serde_json::json!({"enabled": true, "scorer_after_minute": 931});
    std::fs::write(&path, serde_json::to_vec(&top_level).unwrap()).unwrap();
    assert!(super::PlannerConfig::load(&path).is_ok());

    let mut trigger = base.clone();
    trigger["orchestrator"] = serde_json::json!({
        "enabled": true,
        "triggers": {"scorer": {"prompt": "scorer.md", "after_minute": 932}}
    });
    std::fs::write(&path, serde_json::to_vec(&trigger).unwrap()).unwrap();
    assert!(super::PlannerConfig::load(&path).is_ok());

    // A disabled orchestrator never schedules the scorer, so it needs no due minute.
    let mut disabled = base;
    disabled["orchestrator"] = serde_json::json!({"enabled": false});
    std::fs::write(&path, serde_json::to_vec(&disabled).unwrap()).unwrap();
    assert!(super::PlannerConfig::load(&path).is_ok());
}
#[test]
fn legacy_policy_bytes_do_not_gain_an_orchestrator_block() {
    // Validate the current setup as a complete input set. The prompt files
    // are public package sources; application grant paths are never opened.
    let setup = tempfile::tempdir().unwrap();
    let prompts = setup.path().join("prompts");
    std::fs::create_dir(&prompts).unwrap();
    let packaged =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/rust/prompts");
    for name in [
        "objective.md",
        "data.md",
        "data_owner.md",
        "decision.md",
        "profit_under_uncertainty.md",
        "decision_evidence_digest.md",
        "supervisor.md",
        "orchestrator.md",
        "review.md",
        "scorer.md",
    ] {
        std::fs::copy(packaged.join(name), prompts.join(name)).unwrap();
    }
    let current = include_str!("../../../../../packaging/rust/planner-current.example.json")
        .replace("${CONFIG_DIR}", setup.path().to_str().unwrap());
    let path = setup.path().join("planner.json");
    std::fs::write(&path, current).unwrap();
    let baseline = super::PlannerConfig::load(&path).unwrap();
    baseline.runtime_decision_owner_gate().unwrap();
    assert!(baseline.file_prompts.is_some());
    assert!(baseline.orchestrator_owns_decisions());
    assert!(!baseline.decide.enabled);
    assert!(!super::policy_bytes(&baseline).unwrap().is_empty());
    let mut obsolete = baseline.clone();
    obsolete.handoff_gate = Some(super::super::discovery::GateSettings::default());
    assert_eq!(
        obsolete.validate_decision_owner().unwrap_err().to_string(),
        "decision_owner_legacy_handoff_gate_refused"
    );
    obsolete.decision_owner = None;
    obsolete.validate_decision_owner().unwrap();

    let cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "digest": {"enabled": true, "model_id": "gpt-6-luna",
            "transport": "openai_codex", "daily_call_budget": 30},
        "decide": {"enabled": true, "model_id": "grok-4.7", "daily_call_budget": 20}
    }))
    .unwrap();
    assert!(cfg.orchestrator.is_none());
    let value = serde_json::to_value(&cfg).unwrap();
    assert!(value.get("orchestrator").is_none(), "{value}");
    let bytes = super::policy_bytes(&cfg).unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains("orchestrator"));

    // An experiment profile can change prompts and tighten runtime tuning,
    // but never the frozen trading-policy bytes.
    let dir = tempfile::tempdir().unwrap();
    let profile_prompt = dir.path().join("digest.md");
    std::fs::write(&profile_prompt, "custom digest prompt").unwrap();
    let profile_path = dir.path().join("profile.json");
    let base = serde_json::json!({
        "version": 1,
        "prompts": {"digest": "digest.md"},
        "tools": {"scanner_limit": 10},
        "cadence": {"idle_digest_interval_ms": 900000},
        "roles": {"digest": {"max_turns": 3}}
    });
    std::fs::write(&profile_path, serde_json::to_vec(&base).unwrap()).unwrap();
    let mut configured: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "experiment_profile": profile_path,
        "discovery": {"max_scan_age_ms":180000, "max_prospects":10},
        "digest": {"enabled": true, "model_id": "gpt-6-luna",
            "transport": "openai_codex", "daily_call_budget": 30},
        "decide": {"enabled": true, "model_id": "grok-4.7", "daily_call_budget": 20}
    }))
    .unwrap();
    let policy = super::policy_bytes(&configured).unwrap();
    configured.apply_experiment_profile().unwrap();
    assert_eq!(super::policy_bytes(&configured).unwrap(), policy);
    assert!(configured.profile_sha256().is_some());
    assert_eq!(
        configured.prompt_set().and_then(|p| p.role("digest")),
        Some("custom digest prompt")
    );
    assert_eq!(configured.role(super::Trigger::Digest).max_turns, Some(3));
    assert_eq!(configured.effective_tools().scanner_limit, 10);
    assert_eq!(
        configured
            .discovery
            .as_ref()
            .and_then(|d| d.idle_digest_interval_ms),
        Some(900000)
    );
    let sha = configured.profile_sha256().unwrap().to_owned();
    let missing = dir.path().join("missing-profile.json");
    configured.experiment_profile = Some(missing);
    assert!(configured.apply_experiment_profile().is_err());
    assert_eq!(configured.profile_sha256(), Some(sha.as_str()));
    let planner_path = dir.path().join("planner.json");
    std::fs::write(
        &planner_path,
        serde_json::to_vec(&serde_json::json!({
            "enabled": true,
            "discovery": {"max_scan_age_ms":180000, "max_prospects":10},
            "digest": {"enabled": true, "model_id": "gpt-6-luna",
                "transport": "openai_codex", "daily_call_budget": 30},
            "decide": {"enabled": true, "model_id": "grok-4.7", "daily_call_budget": 20}
        }))
        .unwrap(),
    )
    .unwrap();
    let checked = super::super::experiment_profile::check(&planner_path, &profile_path).unwrap();
    assert_eq!(checked["status"], "experiment_profile_valid");
    assert!(checked["profile_sha256"].is_string());

    // An explicit profile digest prompt binds both ownership modes. A
    // profile without one uses the owner prompt for owner policy and the
    // legacy prompt otherwise, and the recorded prompt identity follows.
    let explicit_prompts = configured.prompt_hashes().unwrap();
    configured.decision_owner = Some(super::DecisionOwnerPolicy {
        version: 1,
        owner: super::DecisionOwner::Orchestrator,
    });
    assert_eq!(configured.prompt_hashes().unwrap(), explicit_prompts);
    let default_profile = dir.path().join("default-profile.json");
    std::fs::write(&default_profile, br#"{"version":1}"#).unwrap();
    configured.experiment_profile = Some(default_profile);
    configured.apply_experiment_profile().unwrap();
    let owner_prompts = configured.prompt_hashes().unwrap();
    configured.decision_owner = None;
    let legacy_prompts = configured.prompt_hashes().unwrap();
    assert_ne!(owner_prompts["digest"], legacy_prompts["digest"]);
    assert_eq!(owner_prompts["decide"], legacy_prompts["decide"]);

    // Default files bind content, not directory names, into sealed policy.
    let prompt_dir = dir.path().join("prompts");
    std::fs::create_dir(&prompt_dir).unwrap();
    for name in [
        "objective",
        "data",
        "data_owner",
        "decision",
        "profit_under_uncertainty",
        "decision_evidence_digest",
        "supervisor",
        "orchestrator",
        "review",
        "scorer",
    ] {
        std::fs::write(
            prompt_dir.join(format!("{name}.md")),
            format!("file {name}"),
        )
        .unwrap();
    }
    let files = super::PlannerConfig::load(&planner_path).unwrap();
    let sealed = super::policy_bytes(&files).unwrap();
    assert_eq!(
        files.prompt_set().unwrap().role_for("digest", true),
        Some("file data_owner")
    );
    assert_eq!(
        files.prompt_set().unwrap().role_for("supervisor", true),
        Some("file orchestrator")
    );
    let moved = tempfile::tempdir().unwrap();
    std::fs::create_dir(moved.path().join("prompts")).unwrap();
    for entry in std::fs::read_dir(&prompt_dir).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(
            entry.path(),
            moved.path().join("prompts").join(entry.file_name()),
        )
        .unwrap();
    }
    let moved_config = moved.path().join("planner.json");
    std::fs::copy(&planner_path, &moved_config).unwrap();
    assert_eq!(
        super::policy_bytes(&super::PlannerConfig::load(&moved_config).unwrap()).unwrap(),
        sealed
    );
    std::fs::write(prompt_dir.join("data.md"), "new digest").unwrap();
    assert_ne!(
        super::policy_bytes(&super::PlannerConfig::load(&planner_path).unwrap()).unwrap(),
        sealed
    );
    // A previously loaded set retains its bytes; bad reloads never use embedded fallback.
    for invalid in [Vec::new(), vec![b'x'; 65_537], vec![0xff]] {
        std::fs::write(prompt_dir.join("data.md"), invalid).unwrap();
        assert!(super::PlannerConfig::load(&planner_path).is_err());
        assert_eq!(super::policy_bytes(&files).unwrap(), sealed);
    }
    std::fs::remove_file(prompt_dir.join("data.md")).unwrap();
    assert!(super::PlannerConfig::load(&planner_path).is_err());
    std::fs::create_dir(prompt_dir.join("data.md")).unwrap();
    assert!(super::PlannerConfig::load(&planner_path).is_err());
}

fn policy_test_prompts(dir: &std::path::Path, prefix: &str, text: &str) -> serde_json::Value {
    use super::super::orchestrator_model::Trigger;
    let mut triggers = serde_json::Map::new();
    for trigger in Trigger::ALL {
        let path = dir.join(format!("{prefix}-{}.md", trigger.as_str()));
        std::fs::write(&path, text).unwrap();
        triggers.insert(
            trigger.as_str().into(),
            serde_json::json!({
                "prompt": path,
                "tools": [],
                "max_turns": 2,
                "daily_call_budget": 5
            }),
        );
    }
    serde_json::json!({
        "enabled": true,
        "model": {"model_id": "grok-4.7", "transport": "direct_oauth"},
        "daily_call_budget": 200,
        "triggers": triggers
    })
}

fn policy_test_config(orchestrator: serde_json::Value) -> super::PlannerConfig {
    serde_json::from_value(serde_json::json!({
        "enabled": true,
        "orchestrator": orchestrator
    }))
    .unwrap()
}

#[test]
fn policy_hash_tracks_prompt_text_not_paths_or_defaults() {
    use super::super::orchestrator_model::Trigger;
    let dir = tempfile::tempdir().unwrap();
    let cfg = policy_test_config(policy_test_prompts(dir.path(), "change", "first"));
    let before = super::policy_bytes(&cfg).unwrap();
    let prompt = cfg.orchestrator.as_ref().unwrap().triggers[&Trigger::Operations]
        .prompt
        .clone();
    std::fs::write(prompt, "second").unwrap();
    let after = super::policy_bytes(&cfg).unwrap();
    assert_ne!(before, after);
    {
        let dir = tempfile::tempdir().unwrap();
        let first = policy_test_config(policy_test_prompts(dir.path(), "first", "same text"));
        let second = policy_test_config(policy_test_prompts(dir.path(), "second", "same text"));
        assert_eq!(
            super::policy_bytes(&first).unwrap(),
            super::policy_bytes(&second).unwrap()
        );
    }
    {
        let dir = tempfile::tempdir().unwrap();
        let mut base = policy_test_prompts(dir.path(), "defaults", "same text");
        base.as_object_mut().unwrap().remove("sandbox");
        let omitted = policy_test_config(base.clone());
        base.as_object_mut()
            .unwrap()
            .insert("sandbox".into(), serde_json::json!({"network": false}));
        for trigger in base["triggers"].as_object_mut().unwrap().values_mut() {
            trigger["enabled"] = serde_json::json!(true);
        }
        let explicit = policy_test_config(base);
        assert_eq!(
            super::policy_bytes(&omitted).unwrap(),
            super::policy_bytes(&explicit).unwrap()
        );
    }
    {
        use super::super::orchestrator_model::Trigger;
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = policy_test_config(policy_test_prompts(dir.path(), "missing", "same text"));
        cfg.orchestrator
            .as_mut()
            .unwrap()
            .triggers
            .get_mut(&Trigger::Operations)
            .unwrap()
            .prompt = dir.path().join("does-not-exist.md");
        assert_eq!(
            super::policy_bytes(&cfg).unwrap_err().to_string(),
            "orchestrator_prompt_missing"
        );
    }
}

#[test]
fn orchestrator_policy_excludes_budgets_but_includes_tools_and_sandbox() {
    use super::super::orchestrator_model::{ModelProfile, Trigger};
    let dir = tempfile::tempdir().unwrap();
    let cfg = policy_test_config(policy_test_prompts(dir.path(), "budget", "same text"));
    let original = super::policy_bytes(&cfg).unwrap();

    let mut budgets = cfg.clone();
    let settings = budgets.orchestrator.as_mut().unwrap();
    settings.daily_call_budget += 100;
    settings
        .triggers
        .get_mut(&Trigger::Operations)
        .unwrap()
        .daily_call_budget = Some(999);
    assert_eq!(super::policy_bytes(&budgets).unwrap(), original);

    // The admitted model scope is policy: adding or changing it changes the
    // authority hash, while an unconfigured profile list stays omitted.
    let mut profiles = cfg.clone();
    let settings = profiles.orchestrator.as_mut().unwrap();
    settings.model_profiles.insert(
        "grok".into(),
        ModelProfile {
            model_id: "grok-4.7".into(),
            transport: sadr_cli::native_transport::Transport::DirectOauth,
            model_config: dir.path().join("grok-model.json"),
        },
    );
    settings.admitted_models = vec!["grok".into()];
    assert_ne!(super::policy_bytes(&profiles).unwrap(), original);
    let mut unadmitted = profiles.clone();
    unadmitted.orchestrator.as_mut().unwrap().admitted_models = vec!["missing".into()];
    assert!(
        unadmitted
            .orchestrator
            .as_ref()
            .unwrap()
            .validate_models()
            .is_err()
    );
    // A limit fallback (#245) is policy too; absent, no `fallback` key is
    // serialized, so every existing policy hash is unchanged.
    assert!(
        !String::from_utf8(original.clone())
            .unwrap()
            .contains("fallback")
    );
    let deepseek = super::FallbackProfile {
        model_id: "deepseek-v4-1-flash-1017".into(),
        transport: sadr_cli::native_transport::FallbackTransport::MistralChat,
        model_config: dir.path().join("mistral-fallback.json"),
        output_cap: Some(32_000),
        reasoning_effort: Some("max".into()),
    };
    let mut fallback = cfg.clone();
    fallback.orchestrator.as_mut().unwrap().model.fallback = Some(deepseek.clone());
    fallback.digest.fallback = Some(deepseek.clone());
    assert_ne!(super::policy_bytes(&fallback).unwrap(), original);
    assert_eq!(
        fallback.role(Trigger::Operations).fallback.as_ref(),
        Some(&deepseek)
    );
    assert_eq!(
        fallback.role(Trigger::Digest).fallback,
        Some(deepseek.clone())
    );
    let mut broken = fallback.clone();
    broken.orchestrator.as_mut().unwrap().model.fallback = Some(super::FallbackProfile {
        model_config: "relative.json".into(),
        ..deepseek.clone()
    });
    assert_eq!(
        broken
            .orchestrator
            .as_ref()
            .unwrap()
            .validate_models()
            .unwrap_err()
            .to_string(),
        "orchestrator_model_fallback_invalid"
    );
    broken.digest.fallback = Some(super::FallbackProfile {
        reasoning_effort: Some("ultra".into()),
        ..deepseek
    });
    assert!(super::config::role_invalid(&broken.digest, 120_000));

    let mut tools = cfg.clone();
    tools
        .orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&Trigger::Operations)
        .unwrap()
        .tools = Some(vec!["web_search".into()]);
    assert_ne!(super::policy_bytes(&tools).unwrap(), original);

    let mut sandbox = cfg.clone();
    sandbox.orchestrator.as_mut().unwrap().sandbox.memory_mb += 1;
    assert_ne!(super::policy_bytes(&sandbox).unwrap(), original);

    // #222: the web-tool policy is sealed policy when present, and an absent
    // block keeps the original bytes and makes no outbound call.
    let mut web = cfg.clone();
    web.web_tools = Some(super::super::web_tools::WebToolsSettings {
        enabled: true,
        provider: super::super::web_tools::WebProvider::Searxng,
        search_url: Some("https://search.example.test/search?format=json".into()),
        ..Default::default()
    });
    assert_ne!(super::policy_bytes(&web).unwrap(), original);
    let mut disabled_web = cfg.clone();
    disabled_web.web_tools = Some(super::super::web_tools::WebToolsSettings::default());
    assert_ne!(super::policy_bytes(&disabled_web).unwrap(), original);
    assert!(
        serde_json::to_value(&cfg)
            .unwrap()
            .get("web_tools")
            .is_none()
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planner.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({
            "enabled": false,
            "web_tools": {"enabled": true}
        }))
        .unwrap(),
    )
    .unwrap();
    let error = match super::PlannerConfig::load(&path) {
        Ok(_) => panic!("an enabled web-tools block must require an endpoint"),
        Err(error) => error.to_string(),
    };
    assert_eq!(error, "web_tools_endpoint_required");
    // The checked-in configuration fragment must stay a valid, disabled block.
    let fragment: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../packaging/rust/web-tools.fragment.json"
    ))
    .unwrap();
    let fragment_settings: super::super::web_tools::WebToolsSettings =
        serde_json::from_value(fragment["web_tools"].clone()).unwrap();
    fragment_settings.validate().unwrap();
    assert!(!fragment_settings.enabled);
}

#[test]
fn load_accepts_orchestrator_beside_digest_and_decide() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planner.json");
    let legacy_digest = serde_json::json!({
        "enabled": true,
        "model_id": "grok-4.7",
        "tools": [],
        "timeout_ms": 20000,
        "output_cap": 4096,
        "daily_call_budget": 1
    });

    let enabled_only =
        serde_json::json!({"orchestrator": {"enabled": true, "scorer_after_minute": 931}});
    std::fs::write(&path, serde_json::to_vec(&enabled_only).unwrap()).unwrap();
    super::PlannerConfig::load(&path).unwrap();

    // Digest and decide stay outside the orchestrator and may coexist with it.
    let with_digest_decide = serde_json::json!({
        "digest": legacy_digest,
        "decide": legacy_digest,
        "orchestrator": {"enabled": true, "scorer_after_minute": 931}
    });
    std::fs::write(&path, serde_json::to_vec(&with_digest_decide).unwrap()).unwrap();
    super::PlannerConfig::load(&path).unwrap();

    let legacy_only = serde_json::json!({"digest": legacy_digest});
    std::fs::write(&path, serde_json::to_vec(&legacy_only).unwrap()).unwrap();
    super::PlannerConfig::load(&path).unwrap();

    enabled_scorer_requires_after_minute_from_either_setting();
}

/// Sealed dispositions bind the planner policy hash; upgrading the binary must not
/// add default-valued discovery blocks to the serialized policy (#66).
#[test]
fn default_discovery_blocks_do_not_change_policy_bytes() {
    let cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "discovery": {"max_scan_age_ms": 180000, "max_prospects": 40,
            "market_feed": {"mode": "ltpc", "token_check_ms": 1000}}
    }))
    .unwrap();
    let bytes = String::from_utf8(super::policy_bytes(&cfg).unwrap()).unwrap();
    assert!(!bytes.contains("prefilter"), "{bytes}");
    assert!(!bytes.contains("feature_store"), "{bytes}");
    for added in [
        "connect_timeout_ms",
        "fresh_drop_alert_fraction",
        "handshake_io_timeout_ms",
        "max_connect_addresses",
        "metadata_preload_max_age_ms",
        "read_poll_timeout_ms",
        "scan_activity_timeout_ms",
        "telemetry_window_ms",
        "worker_idle_sleep_ms",
    ] {
        assert!(!bytes.contains(added), "{added}: {bytes}");
    }
    assert!(
        bytes.contains("token_check_ms"),
        "pre-existing fields keep serializing"
    );
    let mut changed = cfg.clone();
    changed.discovery.as_mut().unwrap().prefilter.enabled = true;
    changed.discovery.as_mut().unwrap().feature_store.max_bars = 10;
    let bytes = String::from_utf8(super::policy_bytes(&changed).unwrap()).unwrap();
    assert!(bytes.contains("prefilter") && bytes.contains("feature_store"));
}
#[test]
fn shadow_selector_does_not_change_trading_policy() {
    let mut cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "discovery":{"max_scan_age_ms":180000,"max_prospects":40}
    }))
    .unwrap();
    let original = super::policy_bytes(&cfg).unwrap();
    cfg.discovery.as_mut().unwrap().strategy_selection = Some(
        serde_json::from_value(
            serde_json::json!({"enabled":true,"library_path":"/shadow/library"}),
        )
        .unwrap(),
    );
    assert_eq!(super::policy_bytes(&cfg).unwrap(), original);
    assert_ne!(super::legacy_policy_bytes(&cfg).unwrap(), original);
    cfg.discovery.as_mut().unwrap().max_prospects += 1;
    assert_ne!(super::policy_bytes(&cfg).unwrap(), original);
}
#[test]
fn live_selector_mode_is_bound_by_the_trading_policy_hash() {
    let mut cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "discovery":{"max_scan_age_ms":180000,"max_prospects":40}
    }))
    .unwrap();
    let absent = super::policy_bytes(&cfg).unwrap();
    cfg.discovery.as_mut().unwrap().strategy_selection = Some(
        serde_json::from_value(serde_json::json!({"enabled":true,"library_path":"/live/library"}))
            .unwrap(),
    );
    // An old config that omits `mode` is still shadow and keeps the legacy hash.
    assert_eq!(super::policy_bytes(&cfg).unwrap(), absent);
    cfg.discovery
        .as_mut()
        .unwrap()
        .strategy_selection
        .as_mut()
        .unwrap()
        .mode = super::discovery::StrategySelectionMode::Live;
    let live = super::policy_bytes(&cfg).unwrap();
    assert_ne!(live, absent);
    let text = String::from_utf8(live).unwrap();
    assert!(text.contains("\"strategy_selection\""));
    assert!(text.contains("\"mode\":\"live\""));
    // Flipping back must restore byte-identical legacy policy.
    cfg.discovery
        .as_mut()
        .unwrap()
        .strategy_selection
        .as_mut()
        .unwrap()
        .mode = super::discovery::StrategySelectionMode::Shadow;
    assert_eq!(super::policy_bytes(&cfg).unwrap(), absent);
}
#[test]
fn compact_projection_ranks_rounds_and_drops_allowlist() {
    let row = |k: &str, liq: Value| {
        json!([
            k,
            k,
            "2026-10-22",
            null,
            "10",
            0.123456789,
            0,
            0,
            0,
            0,
            0,
            liq
        ])
    };
    let mut p = json!({
        "positions": {"H": {"mark": "10"}},
        "discovery": {"prospects": [{"instrument_key": "P", "sample_change_bps": 12.3456789}]},
        "universe_features": [row("A", json!(5.0)), row("B", json!("900")), row("H", json!(1.0)), row("P", Value::Null), row("C", json!(50.0))],
        "allowlist": [{"key": "A"}, {"key": "B"}]
    });
    let settings = ContextSettings {
        universe_rows: 3,
        include_allowlist: false,
        decimals: Some(4),
        mode: ContextMode::Projection,
    };
    compact_projection(&mut p, &settings);
    let keys: Vec<&str> = p["universe_features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[0].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["H", "P", "B"]);
    assert_eq!(p["universe_features"][0][5], json!(0.1235));
    assert_eq!(
        p["discovery"]["prospects"][0]["sample_change_bps"],
        json!(12.3457)
    );
    assert_eq!(p["universe_features_presented"]["of"], 5);
    assert!(p.get("allowlist").is_none());
    assert_eq!(p["allowlist_reference"]["count"], 2);
    let mut kept = json!({"allowlist": [1], "universe_features": []});
    compact_projection(
        &mut kept,
        &ContextSettings {
            universe_rows: 1,
            include_allowlist: true,
            decimals: None,
            mode: ContextMode::Projection,
        },
    );
    assert_eq!(kept["allowlist"], json!([1]));
}
#[test]
fn directive_and_supervisor_contracts() {
    let mut chains = vec![("directives", super::super::directives::exercise as fn())];
    chains.extend(super::super::supervisor::exercise_chains());
    super::super::supervisor::run_named(&chains);
}
use super::*;
use sadr_core::execution::ExitIntent;

fn quota_burst_and_review_roles() {
    struct CountingBackend(u32);
    impl Backend for CountingBackend {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            self.0 += 1;
            Ok("bounded synthetic response".into())
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let cfg = PlannerConfig::default();
    // Legacy usage remains trigger-keyed and read-only before pinning.
    let legacy = c.directory.join("model/usage");
    fs::create_dir_all(&legacy).unwrap();
    let attempt = legacy.join("operations-0001.attempt.json");
    fs::write(&attempt, b"{}").unwrap();
    assert_eq!(usage(&c).unwrap()["operations"], 1);
    assert_eq!(fs::read(&attempt).unwrap(), b"{}");
    assert!(!c.directory.join("model/usage-identity.json").exists());
    fs::remove_file(attempt).unwrap();
    assert_eq!(usage(&c).unwrap()["operations"], 0);
    let fixed_clock = || 1_792_000_000_000_i64;
    let guard = CallGuard {
        cancel: None,
        deadline: None,
        not_before: None,
        configuration: None,
        journal_required: std::cell::Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: None,
        context_hash: None,
        clock: &fixed_clock,
        duration: None,
        started_wall_ms: None,
    };
    let roles = [
        ("digest", 50),
        ("decide", 10),
        ("operations", 8),
        ("strategy", 8),
    ];
    let mut backend = CountingBackend(0);
    let mut digest = ModelRole {
        model_id: crate::model::LUNA.into(),
        daily_call_budget: 50,
        ..Default::default()
    };
    for index in 0..50 {
        call_guarded(
            &c,
            &cfg,
            "digest",
            &digest,
            &format!("digest-{index}"),
            &mut backend,
            &guard,
        )
        .unwrap();
    }
    assert_eq!(backend.0, 50);
    assert_eq!(usage(&c).unwrap()["digest"], 50);
    assert_eq!(
        call_guarded(
            &c,
            &cfg,
            "digest",
            &digest,
            "over-cap",
            &mut backend,
            &guard,
        )
        .unwrap_err()
        .to_string(),
        "planner_budget_exhausted"
    );
    assert_eq!(backend.0, 50, "refusal must precede the fake provider call");

    for (role_name, budget) in roles.into_iter().skip(1) {
        let role = ModelRole {
            model_id: format!("fixture-{role_name}"),
            daily_call_budget: budget,
            ..Default::default()
        };
        for index in 0..budget {
            call_guarded(
                &c,
                &cfg,
                role_name,
                &role,
                &format!("{role_name}-{index}"),
                &mut backend,
                &guard,
            )
            .unwrap();
        }
    }
    assert_eq!(backend.0, 76);
    let counts = usage(&c).unwrap();
    assert_eq!(counts["digest"], 50);
    assert_eq!(counts["decide"], 10);
    assert_eq!(counts["operations"], 8);
    assert_eq!(counts["strategy"], 8);

    let identity_path = c.directory.join("model/usage-identity.json");
    let manifest_path = c.directory.join("model/usage-reservations.json");
    let identity_before = fs::read(&identity_path).unwrap();
    let manifest_before = fs::read(&manifest_path).unwrap();
    digest.daily_call_budget = 49;
    assert_eq!(
        call_guarded(
            &c,
            &cfg,
            "digest",
            &digest,
            "lowered-cap",
            &mut backend,
            &guard,
        )
        .unwrap_err()
        .to_string(),
        "planner_budget_exhausted"
    );
    assert_eq!(backend.0, 76);
    assert_eq!(fs::read(&identity_path).unwrap(), identity_before);
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(usage(&c).unwrap(), counts);
}

fn dated_discovery_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let old_path = dir.path().join("old.json");
    let future_path = dir.path().join("future.json");
    let old:PlannerConfig=serde_json::from_value(json!({"enabled":true,"digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex","daily_call_budget":30},"decide":{"enabled":true,"model_id":"grok-4.7","daily_call_budget":20}})).unwrap();
    fs::write(&old_path, serde_json::to_vec(&old).unwrap()).unwrap();
    let mut future = old.clone();
    future.effective_from = Some(NaiveDate::from_ymd_opt(2026, 10, 23).unwrap());
    future.previous_config = Some(old_path.clone());
    future.full_universe_features = Some(true);
    future.discovery=Some(serde_json::from_value(json!({"max_scan_age_ms":180000,"max_prospects":10000,"scan_batch_size":200,"scan_interval_ms":8000})).unwrap());
    future.digest.daily_call_budget = 5000;
    future.decide.daily_call_budget = 500;
    fs::write(&future_path, serde_json::to_vec(&future).unwrap()).unwrap();
    let today = PlannerConfig::load_for_session(
        &future_path,
        NaiveDate::from_ymd_opt(2026, 10, 22).unwrap(),
    )
    .unwrap();
    assert_eq!(policy_bytes(&today).unwrap(), policy_bytes(&old).unwrap());
    assert!(today.discovery.is_none());
    let next = PlannerConfig::load_for_session(
        &future_path,
        NaiveDate::from_ymd_opt(2026, 10, 23).unwrap(),
    )
    .unwrap();
    assert!(next.discovery.is_some());
    assert_eq!(next.digest.daily_call_budget, 5000);
    assert_eq!(next.decide.daily_call_budget, 500);
    future.previous_config = Some(future_path.clone());
    fs::write(&future_path, serde_json::to_vec(&future).unwrap()).unwrap();
    assert_eq!(
        PlannerConfig::load_for_session(
            &future_path,
            NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
        )
        .err()
        .unwrap()
        .to_string(),
        "planner_config_schedule_cycle"
    );
}
fn large_configured_usage_journal() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(dir.path());
    let cfg:PlannerConfig=serde_json::from_value(json!({"digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex","daily_call_budget":5000,"budget_unit":"event","max_turns":64},"decide":{"daily_call_budget":500}})).unwrap();
    let path = dir.path().join("planner.json");
    fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    c.planner_config = Some(path);
    let journal = usage_directory(&c, false).unwrap();
    let mut records = BTreeMap::new();
    for n in 0..10001 {
        let name = format!("digest-{n:04}.attempt.json");
        let file = journal.join(&name);
        fs::write(
            &file,
            json!({"model":"gpt-6-luna","budget_event":format!("event-{}",n/3)}).to_string(),
        )
        .unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        records.insert(name, reservation_stamp(&file).unwrap());
    }
    rollover::atomic(
        &c.directory.join("model/usage-reservations.json"),
        &serde_json::to_value(UsageManifest {
            session_binding: book_binding(&c).unwrap(),
            reservations: records,
        })
        .unwrap(),
    )
    .unwrap();
    assert!(
        fs::metadata(c.directory.join("model/usage-reservations.json"))
            .unwrap()
            .len()
            > 1_000_000
    );
    assert_eq!(usage(&c).unwrap()["digest"], 3334);
    assert!(journal.join("digest-10000.attempt.json").exists());

    // Admission contention is waited out briefly, then reported as an
    // explicit timeout; it never aborts on the first EWOULDBLOCK (#161).
    let lock_path = c.directory.join("model/usage.lock");
    drop(usage_lock_with(&c, false, 0).unwrap());
    let path = lock_path.clone();
    let (ready, acquired) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::lock_exclusive(&file).unwrap();
        ready.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(120));
        fs2::FileExt::unlock(&file).unwrap();
    });
    acquired.recv().unwrap();
    drop(usage_lock_with(&c, true, 1_000).unwrap());
    holder.join().unwrap();

    let (ready, acquired) = std::sync::mpsc::channel();
    let path = lock_path.clone();
    let holder = std::thread::spawn(move || {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        fs2::FileExt::lock_exclusive(&file).unwrap();
        ready.send(()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        fs2::FileExt::unlock(&file).unwrap();
    });
    acquired.recv().unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        usage_lock_with(&c, true, 40).unwrap_err().to_string(),
        "planner_usage_lock_timeout"
    );
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
    holder.join().unwrap();
}
fn logical_event_budget_preserves_provider_turns() {
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let mut cfg = PlannerConfig::default();
    cfg.decide.daily_call_budget = 1;
    cfg.decide.budget_unit = Some("event".into());
    struct Count(usize);
    impl Backend for Count {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            self.0 += 1;
            Ok("{}".into())
        }
    }
    let mut backend = Count(0);
    let mut guard = preview_guard();
    guard.invocation_id = Some("same-durable-event");
    for _ in 0..2 {
        call_guarded(
            &c,
            &cfg,
            "decide",
            &cfg.decide,
            "fixture",
            &mut backend,
            &guard,
        )
        .unwrap();
    }
    assert_eq!(usage(&c).unwrap()["decide"], 1);
    assert_eq!(backend.0, 2);
    assert_eq!(
        fs::read_dir(c.directory.join("model/usage"))
            .unwrap()
            .flatten()
            .filter(|f| f.file_name().to_string_lossy().ends_with(".attempt.json"))
            .count(),
        2
    );
    guard.invocation_id = Some("next-event");
    assert_eq!(
        call_guarded(
            &c,
            &cfg,
            "decide",
            &cfg.decide,
            "fixture",
            &mut backend,
            &guard
        )
        .unwrap_err()
        .to_string(),
        "planner_budget_exhausted"
    );
    assert_eq!(backend.0, 2);
    assert_eq!(usage(&c).unwrap()["decide"], 1);
}
#[test]
fn serialized_provider_envelope_allows_encrypted_reasoning_overhead() {
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let cfg = PlannerConfig::default();
    let role = ModelRole {
        enabled: true,
        model_id: "grok-4.7".into(),
        transport: sadr_cli::native_transport::Transport::DirectOauth,
        output_cap: 8192,
        daily_call_budget: 8,
        timeout_ms: 20_000,
        ..Default::default()
    };
    let old_cap = (role.output_cap as usize).saturating_mul(16);
    let raised_cap = (role.output_cap as usize).saturating_mul(64);
    assert_eq!(OUTPUT_BYTE_CAP_MULTIPLIER, 64);
    assert_eq!(role.output_byte_limit(), raised_cap);
    struct Payload(String);
    impl Backend for Payload {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            Ok(self.0.clone())
        }
    }
    let guard = preview_guard();
    // 2026-10-29 paper: compact grok decide turns reached ~127 KiB; ×16 was 128 KiB.
    let just_over_old = "x".repeat(old_cap + 1);
    assert!(just_over_old.len() > old_cap && just_over_old.len() <= raised_cap);
    assert_eq!(role.provider_envelope_code(&just_over_old), None);
    assert_eq!(role.provider_envelope_code(""), Some("planner_output_cap"));
    let at_raised = "x".repeat(raised_cap);
    assert_eq!(role.provider_envelope_code(&at_raised), None);
    let runaway = "x".repeat(raised_cap + 1);
    assert_eq!(
        role.provider_envelope_code(&runaway),
        Some("planner_output_cap")
    );
    call_guarded(
        &c,
        &cfg,
        "decide",
        &role,
        "fixture",
        &mut Payload(just_over_old),
        &guard,
    )
    .unwrap();
    call_guarded(
        &c,
        &cfg,
        "decide",
        &role,
        "fixture",
        &mut Payload(at_raised),
        &guard,
    )
    .unwrap();
    assert_eq!(
        call_guarded(
            &c,
            &cfg,
            "decide",
            &role,
            "fixture",
            &mut Payload(String::new()),
            &guard,
        )
        .unwrap_err()
        .to_string(),
        "planner_output_or_timeout_cap"
    );
    assert_eq!(
        call_guarded(
            &c,
            &cfg,
            "decide",
            &role,
            "fixture",
            &mut Payload(runaway),
            &guard,
        )
        .unwrap_err()
        .to_string(),
        "planner_output_or_timeout_cap"
    );
}

#[test]
fn validation_reserves_cash_and_holdings_and_failure_never_creates_entries() {
    // Related fault scenarios share this behavioral contract; each owns its fixtures.
    super::super::paper_replay::exercise();
    dated_discovery_configuration();
    large_configured_usage_journal();
    quota_burst_and_review_roles();
    logical_event_budget_preserves_provider_turns();
    measured_digest_without_owner_requires_migration();
    plan_text_and_decide_budget_keep_the_config_hash();

    // Restart reads the original successful digest without reserving another call.
    let restart_dir = tempfile::tempdir().unwrap();
    let restart = super::super::tests::config(restart_dir.path());
    let digest_role = ModelRole {
        model_id: crate::model::LUNA.into(),
        transport: sadr_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 8,
        ..Default::default()
    };
    struct DigestFixture;
    impl Backend for DigestFixture {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            Ok(json!({"summary":"synthetic summary","evidence_ids":["bundle"],"missing_data":[],"uncertainties":[]}).to_string())
        }
    }
    let guard = preview_guard();
    let cfg = PlannerConfig::default();
    let raw = call_guarded(
        &restart,
        &cfg,
        "digest",
        &digest_role,
        "fixture",
        &mut DigestFixture,
        &guard,
    )
    .unwrap();
    for _ in 0..3 {
        assert_eq!(
            existing_digest(&restart, &digest_role, &preview_guard()).unwrap(),
            Some(raw.clone())
        );
    }
    assert_eq!(usage(&restart).unwrap()["digest"], 1);
    for _ in 1..8 {
        call_guarded(
            &restart,
            &cfg,
            "digest",
            &digest_role,
            "fixture",
            &mut DigestFixture,
            &preview_guard(),
        )
        .unwrap();
    }
    assert!(
        call_guarded(
            &restart,
            &cfg,
            "digest",
            &digest_role,
            "fixture",
            &mut DigestFixture,
            &preview_guard()
        )
        .is_err()
    );
    let mut raised = digest_role.clone();
    raised.daily_call_budget = 30;
    call_guarded(
        &restart,
        &cfg,
        "digest",
        &raised,
        "fixture",
        &mut DigestFixture,
        &preview_guard(),
    )
    .unwrap();
    assert_eq!(usage(&restart).unwrap()["digest"], 9);
    let mut other = restart.clone();
    other.session = other.previous_session;
    assert!(existing_digest(&other, &digest_role, &preview_guard()).is_err());
    let role = ModelRole::default();
    assert_eq!(role.run_budget_ms(), role.timeout_ms);
    assert!(
        serde_json::to_value(&role)
            .unwrap()
            .get("run_timeout_ms")
            .is_none()
    );
    let mut settings = postclose::Settings::default();
    settings.operations.timeout_ms = 120000;
    settings.operations.run_timeout_ms = Some(600000);
    settings.validate().unwrap();
    settings.operations.run_timeout_ms = Some(1800001);
    assert!(settings.validate().is_err());
    settings.operations.run_timeout_ms = Some(119999);
    assert!(settings.validate().is_err());
    live::tests::freshness_and_cash();
    super::super::harness::check_boundaries();
    let d = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(d.path());
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let mut p = crate::runtime::tests::test_portfolio();
    let action = |key: &str, side: &str, units: u64| json!({"instrument_key":key,"side":side,"units":units,"max_price":"100","rationale":"fixture","exit_intent":{}});
    let plan = json!({"actions":[action("absent","buy",1),action(&i.key,"buy",50),action(&i.key,"sell",1),action(&i.key,"buy",2)],"stance":"test","notes":""});
    let v = validate(&plan.to_string(), &p, std::slice::from_ref(&i)).unwrap();
    assert_eq!(
        v.iter().map(|v| v.reason.as_str()).collect::<Vec<_>>(),
        vec![
            "action_allowlist",
            "action_cash",
            "action_sell_unheld",
            "accepted"
        ]
    );
    assert!(validate("not JSON", &p, std::slice::from_ref(&i)).is_err());
    let abstain = json!({"actions":[],"stance":"abstain","notes":"no trade"});
    assert!(
        validate(&abstain.to_string(), &p, std::slice::from_ref(&i))
            .unwrap()
            .is_empty()
    );
    let with_reasons = json!({"actions":[],"stance":"abstain","notes":"wait",
            "candidate_reasons":[{"instrument_key":i.key,"reason":"spread too wide"}]});
    assert!(
        validate(&with_reasons.to_string(), &p, std::slice::from_ref(&i))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        plan_document(&with_reasons.to_string()).unwrap()["candidate_reasons"][0]["reason"],
        "spread too wide"
    );
    let mut oversized = with_reasons;
    oversized["candidate_reasons"][0]["reason"] = "x".repeat(2049).into();
    assert_eq!(
        validate(&oversized.to_string(), &p, std::slice::from_ref(&i))
            .err()
            .unwrap()
            .to_string(),
        "planner_plan_bounds"
    );
    let at = DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis();
    let q = Quote {
        bid: Price::parse("99").unwrap(),
        ask: Price::parse("99.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    assert!(
        p.execute_plan(
            at,
            &i,
            &q,
            true,
            3,
            Price::parse("100").unwrap(),
            ExitIntent::default(),
            "fixture"
        )
        .unwrap()
    );
    let plan = json!({"actions":[action(&i.key,"sell",4),action(&i.key,"sell",2),action(&i.key,"sell",2)],"stance":"hold","notes":""});
    let v = validate(&plan.to_string(), &p, std::slice::from_ref(&i)).unwrap();
    assert_eq!(
        v.iter().map(|v| v.reason.as_str()).collect::<Vec<_>>(),
        vec!["action_sell_units", "accepted", "action_sell_units"]
    );
    struct Failure(usize);
    impl Backend for Failure {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            self.0 += 1;
            Err("synthetic_failure".into())
        }
    }
    let cfg = PlannerConfig::default();
    let role = ModelRole {
        daily_call_budget: 1,
        ..Default::default()
    };
    let mut backend = Failure(0);
    let before = p.clone();
    assert!(call(&c, &cfg, "decide", &role, "fixture", &mut backend).is_err());
    assert!(call(&c, &cfg, "decide", &role, "fixture", &mut backend).is_err());
    assert_eq!(backend.0, 2);
    assert_eq!(p, before);
    assert!(!cfg.enabled && !cfg.digest.enabled && !cfg.decide.enabled);
    assert!(produce(&c, &cfg, &p, std::slice::from_ref(&i), false).is_err());
    assert!(!c.directory.join("model/session-plan.json").exists());
    assert_eq!(p, before);
    let cancel = std::sync::atomic::AtomicBool::new(false);
    struct Cancelling<'a>(&'a std::sync::atomic::AtomicBool, usize);
    impl Backend for Cancelling<'_> {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            self.1 += 1;
            self.0.store(true, std::sync::atomic::Ordering::Release);
            Ok("fixture digest".into())
        }
    }
    let mut cancelling = Cancelling(&cancel, 0);
    let guard = CallGuard {
        cancel: Some(&cancel),
        deadline: None,
        not_before: None,
        configuration: None,
        journal_required: std::cell::Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: None,
        context_hash: None,
        clock: &|| at,
        duration: None,
        started_wall_ms: None,
    };
    assert!(
        call_guarded(
            &c,
            &cfg,
            "digest",
            &role,
            "fixture",
            &mut cancelling,
            &guard
        )
        .is_ok()
    );
    assert!(
        call_guarded(
            &c,
            &cfg,
            "decide",
            &role,
            "fixture",
            &mut cancelling,
            &guard
        )
        .is_err()
    );
    assert_eq!(cancelling.1, 1);
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":1,"decide":0,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
    );
    let clock = std::cell::Cell::new(0i64);
    struct Advancing<'a>(&'a std::cell::Cell<i64>, usize);
    impl Backend for Advancing<'_> {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            self.1 += 1;
            self.0.set(2);
            Ok("fixture digest".into())
        }
    }
    let guard = CallGuard {
        cancel: None,
        deadline: Some(1),
        not_before: None,
        configuration: None,
        journal_required: std::cell::Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: None,
        context_hash: None,
        clock: &|| clock.get(),
        duration: None,
        started_wall_ms: None,
    };
    let two = ModelRole {
        daily_call_budget: 2,
        ..role.clone()
    };
    let mut advancing = Advancing(&clock, 0);
    assert!(call_guarded(&c, &cfg, "digest", &two, "fixture", &mut advancing, &guard).is_ok());
    assert!(call_guarded(&c, &cfg, "decide", &two, "fixture", &mut advancing, &guard).is_err());
    assert_eq!(advancing.1, 1);
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":2,"decide":0,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
    );
    let enough = ModelRole {
        daily_call_budget: 10,
        ..role.clone()
    };
    let guard = CallGuard {
        cancel: None,
        deadline: None,
        not_before: None,
        configuration: None,
        journal_required: std::cell::Cell::new(true),
        minimum_prior_calls: 2,
        invocation_id: None,
        context_hash: None,
        clock: &|| at,
        duration: None,
        started_wall_ms: None,
    };
    assert!(
        call_guarded(
            &c,
            &cfg,
            "decide",
            &enough,
            "fixture",
            &mut advancing,
            &guard
        )
        .is_err()
    );
    assert_eq!(advancing.1, 1);
    struct DryRuns(usize);
    impl Backend for DryRuns {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            self.0 += 1;
            Ok("{}".into())
        }
    }
    let mut previews = DryRuns(0);
    // #238: the research lane charges its own `research` bucket. Its reservation
    // must stay a valid journal record, while unknown role prefixes still refuse.
    let journal_dir = tempfile::tempdir().unwrap();
    let write_reservation = |name: &str| {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let path = journal_dir.path().join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        file.write_all(br#"{"reserved_ms":0}"#).unwrap();
        file.sync_all().unwrap();
    };
    write_reservation("research-0000.attempt.json");
    write_reservation("supervisor-0000.attempt.json");
    assert!(super::usage::reservations(journal_dir.path(), 16).is_ok());
    write_reservation("bogus-0000.attempt.json");
    assert!(super::usage::reservations(journal_dir.path(), 16).is_err());
    assert!(
        call(
            &c,
            &cfg,
            "decide",
            &enough,
            "fixture preview 1",
            &mut previews
        )
        .is_ok()
    );
    assert!(
        call(
            &c,
            &cfg,
            "decide",
            &enough,
            "fixture preview 2",
            &mut previews
        )
        .is_ok()
    );
    let latest = c.directory.join("model/usage/decide-0002.attempt.json");
    let preserved = c.directory.join("model/preserved-decide-0002.json");
    fs::rename(&latest, &preserved).unwrap();
    assert!(call(&c, &cfg, "decide", &enough, "fixture event", &mut previews).is_err());
    assert!(usage(&c).is_err());
    assert_eq!(previews.0, 2); // A missing preview reservation cannot refund a call.
    fs::rename(&preserved, &latest).unwrap();
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":2,"decide":2,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
    );
    let original_bytes = fs::read(&latest).unwrap();
    fs::write(&latest, b"{\"changed\":\"synthetic reservation\"}").unwrap();
    assert!(call(&c, &cfg, "decide", &enough, "fixture event", &mut previews).is_err());
    assert_eq!(previews.0, 2);
    fs::copy(
        &latest,
        c.directory.join("model/changed-reservation-evidence.json"),
    )
    .unwrap();
    fs::write(&latest, &original_bytes).unwrap();
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":2,"decide":2,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
    );
    let mut augmented = p.clone();
    assert!(
        augmented
            .execute_plan(
                at,
                &i,
                &q,
                true,
                2,
                Price::parse("100").unwrap(),
                ExitIntent::default(),
                "add-fixture"
            )
            .unwrap()
    );
    assert_eq!(augmented.positions[&i.key].units, 5);
    assert!(
        augmented
            .execute_plan(
                at,
                &i,
                &q,
                false,
                1,
                Price::parse("100").unwrap(),
                ExitIntent::default(),
                "partial-sell-fixture"
            )
            .unwrap()
    );
    assert_eq!(augmented.positions[&i.key].units, 4);
    augmented.reconcile().unwrap();
    let history = d.path().join("daily");
    fs::create_dir(&history).unwrap();
    let mut with_history = c.clone();
    with_history.history_directory = Some(history.clone());
    let first = c
        .previous_session
        .checked_sub_days(chrono::Days::new(20))
        .unwrap();
    let mut rows = Vec::new();
    for offset in 0..21 {
        let day = first.checked_add_days(chrono::Days::new(offset)).unwrap();
        let close = 100 + offset;
        rows.push(json!([
            format!("{day}T00:00:00+05:30"),
            close,
            close + 1,
            close - 1,
            close,
            1000 + offset,
            0
        ]));
    }
    rows.push(json!([
        format!("{}T00:00:00+05:30", c.session),
        999,
        1000,
        998,
        999,
        9999,
        0
    ]));
    let body = json!({"status":"success","data":{"candles":rows}}).to_string();
    let receipt = json!({"key":i.key,"unit":"days","start":first,"end":c.session,"requested_ms":at,"received_ms":at,"body":body,"body_sha256":digest(body.as_bytes())});
    let path = history.join(format!(
        "{}.receipt.json",
        history_download::range_id(&i.key, "days", first, c.session)
    ));
    fs::write(&path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    let bundle = bundle(&with_history, &p, std::slice::from_ref(&i), 40).unwrap();
    let feature = &bundle["shortlist"][0];
    assert_eq!(bundle["eligible_feature_count"], 1);
    assert_eq!(bundle["feature_coverage_partial"], false);
    assert_eq!(feature["last_close"], 120.0);
    assert!((feature["return_20d"].as_f64().unwrap() - 0.2).abs() < 1e-12);
    assert_eq!(feature["gap_as_of"], c.previous_session.to_string());
    assert!(feature["volatility_20d"].as_f64().unwrap() > 0.0);
    let (empty, references) =
        bundle_with_references(&with_history, &p, std::slice::from_ref(&i), 0).unwrap();
    assert!(empty["shortlist"].as_array().unwrap().is_empty());
    assert_eq!(empty["eligible_feature_count"], 1);
    assert_eq!(empty["feature_coverage_partial"], true);
    assert_eq!(references["closes"][&i.key], "120");
    assert_eq!(references["as_of"], c.previous_session.to_string());
    assert_eq!(references["close_basis"], "latest_daily_before_session");
    assert_eq!(
        references["close_dates"][&i.key],
        c.previous_session.to_string()
    );
    let mut changed = receipt;
    changed["body_sha256"] = json!("invalid");
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(super::bundle(&with_history, &p, std::slice::from_ref(&i), 40).is_err());
    assert_eq!(p, before);
    // A local provider failure still retains the attempted direct origin/model.
    let missing_model_config = d.path().join("no-provider-config.json");
    let mut direct_backend = Subscription::new(&missing_model_config);
    let direct_role = ModelRole {
        enabled: true,
        model_config: Some(missing_model_config.clone()),
        model_id: "grok-4.7".into(),
        transport: sadr_cli::native_transport::Transport::DirectOauth,
        daily_call_budget: 20,
        ..Default::default()
    };
    let direct_policy = PlannerConfig {
        digest: direct_role.clone(),
        ..Default::default()
    };
    assert!(
        call(
            &c,
            &direct_policy,
            "digest",
            &direct_role,
            "fixture data",
            &mut direct_backend
        )
        .is_err()
    );
    let failed: Value = serde_json::from_slice(
        &fs::read(c.directory.join("model/usage/digest-0002.result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(failed["failed"], true);
    assert_eq!(
        failed["transport_evidence"]["origin"],
        sadr_cli::native_transport::DIRECT_RESPONSES
    );
    assert_eq!(failed["transport_evidence"]["model"], "grok-4.7");
    let mut direct_cfg = PlannerConfig {
        digest: direct_role.clone(),
        ..Default::default()
    };
    let config_file = d.path().join("direct-digest.json");
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.decide = ModelRole {
        model_id: "grok-4.7".into(),
        transport: sadr_cli::native_transport::Transport::DirectOauth,
        ..Default::default()
    };
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.decide.tools = vec!["x_search".into(), "web_search".into()];
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.decide.tools.clear();
    direct_cfg.digest.transport = sadr_cli::native_transport::Transport::OpenaiCodex;
    direct_cfg.digest.model_id = crate::model::LUNA.into();
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.digest.model_id = "gpt-6.1-sol".into();
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.digest.model_id = crate::model::LUNA.into();
    direct_cfg.decide.transport = sadr_cli::native_transport::Transport::OpenaiCodex;
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    // Luna may decide over its own grant, but never with native search tools.
    direct_cfg.decide.model_id = crate::model::LUNA.into();
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.decide.tools = vec!["web_search".into()];
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    assert!(PlannerConfig::load(&config_file).is_err());
    direct_cfg.decide.tools.clear();
    direct_cfg.decide.transport = sadr_cli::native_transport::Transport::DirectOauth;
    direct_cfg.decide.model_id = crate::model::FAST_VARIANT.into();
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    let journal = c.directory.join("model/usage");
    fs::rename(&journal, c.directory.join("model/preserved-usage")).unwrap();
    fs::create_dir(&journal).unwrap();
    assert!(usage(&c).is_err());
    assert!(call(&c, &cfg, "decide", &enough, "fixture", &mut advancing).is_err());
    assert_eq!(advancing.1, 1);
    postclose::behavioral_assertions();
}

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};

struct Slow<'a> {
    clock: &'a std::cell::Cell<i64>,
    responses: VecDeque<Value>,
    latencies: VecDeque<i64>,
    requests: Vec<Value>,
    cancel_at: Option<usize>,
    cancel: Option<&'a AtomicBool>,
}
impl Backend for Slow<'_> {
    fn call(&mut self, _: &ModelRole, prompt: &str) -> Result<String> {
        let index = self.requests.len();
        self.requests.push(serde_json::from_str(prompt)?);
        let step = self
            .latencies
            .pop_front()
            .ok_or("fixture_latency_missing")?;
        self.clock.set(self.clock.get() + step);
        if self.cancel_at == Some(index)
            && let Some(flag) = self.cancel
        {
            flag.store(true, Ordering::Release);
        }
        let value = self.responses.pop_front().ok_or("fixture_exhausted")?;
        if value["fixture_timeout"] == true {
            return Err("planner_call_timeout".into());
        }
        Ok(value.to_string())
    }
}

fn session_ms() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn listed() -> Instrument {
    Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }
}
fn paper_cfg(timeout_ms: u64, run_timeout_ms: Option<u64>) -> PlannerConfig {
    let mut cfg: PlannerConfig = serde_json::from_value(json!({
        "enabled": true,
        "provider": "xai_subscription",
        "shortlist_n": 40,
        "digest": {
            "enabled": false,
            "model_id": "",
            "tools": [],
            "timeout_ms": 60000,
            "output_cap": 4096,
            "daily_call_budget": 0
        },
        "decide": {
            "enabled": true,
            "model_id": "grok-4.7",
            "transport": "direct_oauth",
            "tools": ["web_search", "x_search"],
            "timeout_ms": timeout_ms,
            "output_cap": 8192,
            "daily_call_budget": 8
        },
        "events": {
            "enabled": true,
            "cooldown_ms": 0,
            "max_pending": 10,
            "policy": {
                "price_move_bps": 100,
                "volume_step_units": null,
                "gap_bps": null,
                "proximity_bps": null,
                "max_event_age_ms": 300000,
                "max_news_age_ms": 60000,
                "max_instruments": 20,
                "max_identities": 100
            }
        }
    }))
    .unwrap();
    cfg.decide.run_timeout_ms = run_timeout_ms;
    cfg
}
fn seed_history(c: &mut Config, key: &str, at: i64) {
    let history = c.directory.join("daily-history");
    fs::create_dir(&history).unwrap();
    c.history_directory = Some(history.clone());
    let first = c
        .previous_session
        .checked_sub_days(chrono::Days::new(20))
        .unwrap();
    let mut rows = Vec::new();
    for offset in 0..21 {
        let day = first.checked_add_days(chrono::Days::new(offset)).unwrap();
        let close = 100 + offset;
        rows.push(json!([
            format!("{day}T00:00:00+05:30"),
            close,
            close + 1,
            close - 1,
            close,
            1000 + offset,
            0
        ]));
    }
    rows.push(json!([
        format!("{}T00:00:00+05:30", c.session),
        999,
        1000,
        998,
        999,
        9999,
        0
    ]));
    let body = json!({"status":"success","data":{"candles":rows}}).to_string();
    let receipt = json!({
        "key": key,
        "unit": "days",
        "start": first,
        "end": c.session,
        "requested_ms": at,
        "received_ms": at,
        "body": body,
        "body_sha256": digest(body.as_bytes())
    });
    fs::write(
        history.join(format!(
            "{}.receipt.json",
            history_download::range_id(key, "days", first, c.session)
        )),
        serde_json::to_vec(&receipt).unwrap(),
    )
    .unwrap();
}
struct Outcome {
    c: Config,
    #[allow(dead_code)]
    db: rusqlite::Connection,
    #[allow(dead_code)]
    p: Portfolio,
    cfg: PlannerConfig,
    claim: redecision::Claim,
    #[allow(dead_code)]
    i: Instrument,
    #[allow(dead_code)]
    at: i64,
    #[allow(dead_code)]
    now: i64,
    #[allow(dead_code)]
    produce_wall_us: u128,
    requests: Vec<Value>,
    plan: std::result::Result<Value, String>,
    /// Keeps the session directory alive for the assertions.
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}
fn drive(
    cfg: &PlannerConfig,
    responses: Vec<Value>,
    latencies: Vec<i64>,
    cancel_at: Option<usize>,
) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(dir.path());
    let i = listed();
    let at = if cfg.digest.enabled {
        // VM-shaped archived predecessor and a fresh, already-published rollover.
        let original = c.clone();
        let db = super::super::open_book(&original).unwrap();
        let mut archived = crate::runtime::tests::test_portfolio();
        archived.closed = true;
        archived.stopped = true;
        let bytes = serde_json::to_string(&archived).unwrap();
        db.execute(
            "INSERT INTO state VALUES(1,?,?)",
            rusqlite::params![book_binding(&original).unwrap(), bytes],
        )
        .unwrap();
        drop(db);
        let sessions = dir.path().join("sessions");
        let target = sessions.join("2026-10-22");
        fs::create_dir_all(&target).unwrap();
        c.sessions_root = Some(sessions.clone());
        // Config is a new paper mandate; original bytes are never rebound.
        rollover::atomic(
            &sessions.join("current.json"),
            &json!({"account":c.account,"session":"2026-10-22"}),
        )
        .unwrap();
        rollover::atomic(&target.join("strategy-decision.json"), &json!({"reviewer":"none_model_led","live_enabled":false,"chosen":{"strategy_id":"model_led_v1","execution_selector":c.strategy,"capital":c.allocation().unwrap()},"capital":c.allocation().unwrap()})).unwrap();
        c = rollover::current(&c).unwrap();
        let preserved: String = rusqlite::Connection::open(original.directory.join("paper.sqlite"))
            .unwrap()
            .query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(preserved, bytes);
        DateTime::parse_from_rfc3339("2026-10-22T09:15:00+05:30")
            .unwrap()
            .timestamp_millis()
    } else {
        session_ms()
    };
    seed_history(&mut c, &i.key, at);
    let mut db = super::super::open_book(&c).unwrap();
    let mut p = super::super::wide::load_wide(&mut db, &c).unwrap();
    assert!(p.model_led);
    assert!(saved_plan(&db, &c).unwrap().is_none());
    assert!(has_eligible_input(&c, &p, std::slice::from_ref(&i)).unwrap());
    let quote = Quote {
        bid: Price::parse("100").unwrap(),
        ask: Price::parse("100.05").unwrap(),
        bid_units: 100,
        ask_units: 100,
        source: at,
        receipt: at,
    };
    super::super::wide::commit_quotes(
        &mut db,
        &c,
        &mut p,
        &std::collections::BTreeMap::from([(i.key.clone(), quote)]),
        &[],
        at,
        "fixture",
        || at,
    )
    .unwrap();
    if cfg.discovery.is_some() {
        let mut scan = sadr_core::execution::Scanner::default();
        scan.samples.insert(
            i.key.clone(),
            sadr_core::execution::Sample {
                history: vec![Price::parse("100").unwrap()],
                current: Some(Price::parse("101").unwrap()),
                seen: at,
                source: Some(at),
                last: at,
                reason: "move".into(),
                market_fields: Some(sadr_core::execution::MarketFields {
                    session_vwap: None,
                    best_bid: Some(100.0),
                    best_bid_qty: Some(100),
                    best_ask: Some(100.05),
                    best_ask_qty: Some(100),
                    total_buy_quantity: Some(1000),
                    total_sell_quantity: Some(1000),
                    lower_circuit_limit: sadr_core::execution::a("80"),
                    upper_circuit_limit: sadr_core::execution::a("120"),
                    volume: 1_000_000,
                }),
                ..Default::default()
            },
        );
        db.execute(
                "CREATE TABLE IF NOT EXISTS scanner(id INTEGER PRIMARY KEY CHECK(id=1),body TEXT NOT NULL)",
                [],
            )
            .unwrap();
        db.execute(
            "INSERT INTO scanner VALUES(1,?) ON CONFLICT(id) DO UPDATE SET body=excluded.body",
            [serde_json::to_string(&scan).unwrap()],
        )
        .unwrap();
    }
    assert!(
        !p.halted && p.fills.is_empty(),
        "priming commit must not trade"
    );
    redecision::initialize(&mut db, &c, &p, Some(cfg), at).unwrap();
    assert!(c.directory.join("model/usage-identity.json").is_file());
    if cfg.orchestrator_owns_decisions() {
        rollover::atomic(
            &c.directory.join("model/luna-latest.json"),
            &json!({
                "session_binding": book_binding(&c).unwrap(),
                "at_ms": at,
                "digest": {"summary":"old Luna context must not be replayed"}
            }),
        )
        .unwrap();
    }
    if cfg.digest.enabled {
        // Reproduce v0.5.37: seven pre-inference failures on a rolled book,
        // before coverage became eligible and before a namespace existed.
        fs::remove_file(c.directory.join("model/usage-identity.json")).unwrap();
        fs::remove_dir(c.directory.join("model/usage")).unwrap();
        for n in 0..7 {
            db.execute(
                "INSERT INTO planner_attempt_log VALUES(?,?,?,'failed',?)",
                rusqlite::params![
                    format!("old-{n}"),
                    format!("old-event-{n}"),
                    "{}",
                    json!({"status":"model_plan_unavailable_no_trades"}).to_string()
                ],
            )
            .unwrap();
        }
        // Ambiguous evidence must never get a replacement allowance.
        db.execute(
            "UPDATE planner_attempt_log SET state='interrupted' WHERE id='old-0'",
            [],
        )
        .unwrap();
        assert!(redecision::initialize(&mut db, &c, &p, Some(cfg), at).is_err());
        assert!(!c.directory.join("model/usage-identity.json").exists());
        db.execute(
            "UPDATE planner_attempt_log SET state='failed' WHERE id='old-0'",
            [],
        )
        .unwrap();
        redecision::initialize(&mut db, &c, &p, Some(cfg), at).unwrap();
        assert_eq!(
            usage(&c).unwrap(),
            json!({"digest":0,"decide":0,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
        );
    }
    let claim = redecision::reserve(&mut db, &c, cfg, &p, at, true)
        .unwrap()
        .expect("initial claim");
    let clock = std::cell::Cell::new(claim.started_ms);
    let flag = AtomicBool::new(false);
    let mut backend = Slow {
        clock: &clock,
        responses: VecDeque::from(responses),
        latencies: VecDeque::from(latencies),
        requests: Vec::new(),
        cancel_at,
        cancel: Some(&flag),
    };
    let measurement_started = std::time::Instant::now();
    let plan = produce_with_backend(
        &c,
        cfg,
        &p,
        std::slice::from_ref(&i),
        &claim,
        Some(&flag),
        &mut backend,
        &|| clock.get(),
    );
    Outcome {
        c,
        db,
        p,
        cfg: cfg.clone(),
        claim,
        i,
        at,
        now: clock.get(),
        produce_wall_us: measurement_started.elapsed().as_micros(),
        requests: backend.requests,
        plan: plan.map_err(|error| error.to_string()),
        dir,
    }
}
#[test]
fn tools_only_digest_sees_only_the_trigger_envelope() {
    let mut cfg = paper_cfg(120_000, None);
    cfg.digest = ModelRole {
        enabled: true,
        model_id: "gpt-6-luna".into(),
        transport: sadr_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 30,
        ..Default::default()
    };
    cfg.require_digest = Some(true);
    cfg.discovery =
        Some(serde_json::from_value(json!({"max_scan_age_ms":5000,"max_prospects":20})).unwrap());
    cfg.context = Some(ContextSettings {
        universe_rows: 150,
        include_allowlist: true,
        decimals: None,
        mode: ContextMode::ToolsOnly,
    });
    cfg.full_universe_features = Some(true);
    let digest = json!({
        "summary":"one fresh scanner prospect",
        "evidence_ids":["bundle"],
        "missing_data":[],
        "uncertainties":[],
        "handoff":{"decision":"wait","instrument_keys":[],"reason":"no actionable setup"}
    })
    .to_string();
    let out = drive(
        &cfg,
        vec![
            json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":digest}]}]}),
        ],
        vec![100],
        None,
    );
    assert_eq!(out.requests.len(), 1, "wait must not call Grok");
    let prompt = out.requests[0]["input"].to_string();
    assert!(
        prompt.len() <= 1024,
        "trigger envelope is {} bytes",
        prompt.len()
    );
    for forbidden in [
        "universe_features",
        "shortlist",
        "allowlist_reference",
        "historical_features",
        "\"discovery\":",
        "\"prospects\":",
        "\"cash\":",
        "\"positions\":",
        "\"fees\":",
        "\"previous_",
    ] {
        assert!(!prompt.contains(forbidden), "Luna input leaked {forbidden}");
    }
    assert!(prompt.contains("maximize the continuing paper account"));
    assert_eq!(
        out.plan.as_ref().unwrap_err().to_string(),
        "decision_owner_migration_required"
    );

    // A v1 orchestrator owner records Luna's valid wait digest as evidence,
    // without parsing its handoff, reserving a decide call or carrying old
    // Luna/Grok advisory context into this fresh digest request.
    let mut owner_cfg = cfg.clone();
    owner_cfg.require_digest = None;
    owner_cfg.decide.enabled = false;
    owner_cfg.decision_owner = Some(DecisionOwnerPolicy {
        version: 1,
        owner: DecisionOwner::Orchestrator,
    });
    let mut orchestrator = super::super::orchestrator_model::OrchestratorModelSettings {
        enabled: true,
        ..Default::default()
    };
    orchestrator.model.model_id = "grok-4.7".into();
    for trigger in [Trigger::Operations, Trigger::Strategy] {
        orchestrator.triggers.get_mut(&trigger).unwrap().enabled = false;
    }
    // Policy hashing loads every configured trigger's prompt; use durable
    // fixture files instead of the checkout's relative default paths.
    let prompt_dir = tempfile::tempdir().unwrap();
    for trigger in Trigger::ALL {
        let path = prompt_dir.path().join(format!("{}.md", trigger.as_str()));
        fs::write(&path, "owner test prompt").unwrap();
        if let Some(settings) = orchestrator.triggers.get_mut(&trigger) {
            settings.prompt = path;
        }
    }
    orchestrator.triggers.insert(
        Trigger::Supervisor,
        super::super::orchestrator_model::TriggerSettings {
            enabled: true,
            prompt: prompt_dir.path().join("supervisor.md"),
            ..Default::default()
        },
    );
    owner_cfg.orchestrator = Some(orchestrator);
    assert!(owner_cfg.decision_owner_requirements().is_empty());
    let owner_digest = json!({
        "complete":true,
        "as_of_ms":0,
        "coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},
        "gaps":[],
        "changed_prospects":[],
        "active_rankings":[],
        "candidate_rankings":[],
        "bar_context":[],
        "spread_depth":[],
        "holdings":[],
        "cash":{"available":"0"},
        "fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},
        "recent_actions":[],
        "experiments":[],
        "evidence_ids":["bundle"]
    })
    .to_string();
    let owner_out = drive(
        &owner_cfg,
        vec![
            json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":owner_digest}]}]}),
        ],
        vec![100],
        None,
    );
    assert_eq!(owner_out.requests.len(), 1, "owner mode calls Luna only");
    assert!(owner_out.plan.is_ok(), "{:?}", owner_out.plan);
    let owner_plan = owner_out.plan.as_ref().unwrap();
    assert_eq!(owner_plan["parsed_plan"]["actions"], json!([]));
    assert_eq!(
        owner_plan["parsed_plan"]["notes"],
        "orchestrator_digest_ready"
    );
    assert_eq!(owner_plan["model_id"], owner_cfg.digest.model_id);
    assert_eq!(
        owner_plan["budgets"],
        json!({
            "digest": owner_cfg.digest.daily_call_budget,
            "supervisor": owner_cfg.role(Trigger::Supervisor).daily_call_budget,
        })
    );
    assert_eq!(owner_plan["luna_handoff_requested"], false);
    assert_eq!(owner_plan["owner_digest"]["version"], 1);
    assert_eq!(owner_plan["owner_digest"]["digest"]["complete"], true);
    assert_eq!(owner_plan["owner_digest"]["eligible"], true);
    assert_eq!(
        owner_plan["owner_digest"]["facts_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        owner_plan["digest_id"],
        owner_plan["digest_attribution"]["digest_id"]
    );
    assert_eq!(owner_plan["digest_id"].as_str().unwrap().len(), 64);
    assert_eq!(
        owner_plan["owner_digest"]["tool_results"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    for key in [
        "previous_luna",
        "previous_grok_decision",
        "decision_history",
        "advisory_memory",
    ] {
        assert!(owner_plan["input"].get(key).is_none(), "unexpected {key}");
    }
    assert_eq!(usage(&owner_out.c).unwrap()["digest"], 1);
    assert_eq!(usage(&owner_out.c).unwrap()["decide"], 0);
}

#[test]
fn measured_digest_without_owner_requires_migration() {
    const {
        assert!(!super::super::LIVE_ENABLED);
    }
    let mut pipeline = paper_cfg(120_000, None);
    pipeline.digest = ModelRole {
        enabled: true,
        model_id: "gpt-6-luna".into(),
        transport: sadr_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 30,
        ..Default::default()
    };
    pipeline.decide.daily_call_budget = 20;
    pipeline.full_universe_features = Some(true);
    pipeline.context = Some(ContextSettings {
        universe_rows: 150,
        include_allowlist: false,
        decimals: None,
        mode: ContextMode::Projection,
    });
    pipeline.require_digest = Some(true);
    pipeline.operator_context = Some("Synthetic startup mandate context".into());
    let digest = json!({"summary":"Synthetic dated market bundle", "evidence_ids":["bundle"], "missing_data":[], "uncertainties":[]}).to_string();
    let initial = drive(
        &pipeline,
        vec![
            json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":digest}]}]}),
        ],
        vec![100],
        None,
    );
    assert_eq!(initial.claim.previous_attempts, 7);
    assert_eq!(initial.cfg.digest.model_id, "gpt-6-luna");
    assert_eq!(initial.cfg.decide.model_id, "grok-4.7");
    assert!(
        initial.requests[0]["input"]
            .to_string()
            .contains("universe_features")
    );
    assert!(
        initial.requests[0]["input"]
            .to_string()
            .contains("Synthetic startup mandate context")
    );
    assert_eq!(
        initial.requests.len(),
        1,
        "owner-less digest must not call decide"
    );
    assert_eq!(
        initial.plan.as_ref().unwrap_err().to_string(),
        "decision_owner_migration_required"
    );
    assert_eq!(usage(&initial.c).unwrap()["digest"], 1);
    assert_eq!(usage(&initial.c).unwrap()["decide"], 0);
}

#[test]
fn provider_failures_retain_bounded_reason_and_stage_without_raw_text() {
    for code in [
        "planner_call_timeout",
        "planner_output_or_timeout_cap",
        "planner_output_invalid",
        "planner_output_cap",
        "planner_response_model_mismatch",
        "model_body_invalid",
        "model_response_incomplete",
        "model_response_invalid",
        "model_response_model_mismatch",
        "model_stream_invalid",
        "model_stream_incomplete",
        "model_transport_failed",
        "model_origin_refused",
        "model_request_invalid",
        "harness_output_invalid",
    ] {
        assert_eq!(
            provider_failure(code)
                .as_ref()
                .map(|detail| detail.reason.as_str()),
            Some(code)
        );
        let err: Box<dyn std::error::Error> = code.into();
        if code == "planner_call_timeout" {
            assert_eq!(reason(err.as_ref()), "planner_failed_no_new_entries");
        } else {
            assert_eq!(reason(err.as_ref()), "planner_provider_failed");
        }
    }
    assert_eq!(
        provider_failure("model_timeout")
            .as_ref()
            .map(|detail| detail.reason.as_str()),
        Some("planner_call_timeout")
    );
    assert_eq!(
        provider_failure("model_http_502 upstream body text")
            .as_ref()
            .map(|detail| detail.reason.as_str()),
        Some("model_http_502")
    );
    assert_eq!(
        provider_failure("model_http_502 upstream body text")
            .as_ref()
            .and_then(|detail| detail.http_status),
        Some(502)
    );
    assert_eq!(provider_failure("model_http_9990"), None);
    assert_eq!(
        provider_failure("raw provider body access_token=secret"),
        None
    );
    assert_eq!(
        provider_failure("planner_response_model_mismatch")
            .as_ref()
            .map(|detail| detail.reason.as_str()),
        Some("planner_response_model_mismatch")
    );

    for code in [
        "harness_tool_definitions_changed",
        "harness_tool_invalid",
        "harness_request_invalid",
        "harness_final_tools_invalid",
    ] {
        let err: Box<dyn std::error::Error> = format!("{code} secret-provider-text").into();
        let record = failure_record_with_provider(
            reason(err.as_ref()),
            err.as_ref(),
            7,
            &json!({"stage":"tool_definitions"}),
        );
        assert_eq!(record["reason"], code);
        assert_eq!(record["provider_reason"], code);
        assert_eq!(record["provider_stage"], "tool_definitions");
        assert!(!record.to_string().contains("secret-provider-text"));
    }
    let stage = json!({"stage":"response_validation","transport":"direct_oauth"});
    let err: Box<dyn std::error::Error> = "model_stream_incomplete".into();
    let record = failure_record_with_provider(reason(err.as_ref()), err.as_ref(), 7, &stage);
    assert_eq!(record["status"], "planner_provider_failed");
    assert_eq!(record["reason"], "model_stream_incomplete");
    assert_eq!(record["provider_reason"], "model_stream_incomplete");
    assert_eq!(record["provider_stage"], "response_validation");
    assert!(record.get("http_status").is_none());

    // An incomplete response retains its allowlisted cause and the measured
    // request/token sizes in bounded failure evidence (#224).
    let incomplete = json!({
        "stage":"response_validation",
        "http_status":200,
        "incomplete_reason":"max_output_tokens",
        "request_bytes":1_105_225_u64,
        "prompt_bytes":1_005_366_u64,
        "max_output_tokens":4096_u64,
        "tokens_in":1_105_225_u64,
        "tokens_out":0_u64,
    });
    let err: Box<dyn std::error::Error> = "model_response_incomplete".into();
    let record = failure_record_with_provider(reason(err.as_ref()), err.as_ref(), 7, &incomplete);
    assert_eq!(record["status"], "planner_provider_failed");
    assert_eq!(record["reason"], "model_response_incomplete");
    assert_eq!(record["provider_reason"], "model_response_incomplete");
    assert_eq!(record["provider_stage"], "response_validation");
    assert_eq!(record["http_status"], 200);
    assert_eq!(record["incomplete_reason"], "max_output_tokens");
    assert_eq!(record["request_bytes"], 1_105_225);
    assert_eq!(record["prompt_bytes"], 1_005_366);
    assert_eq!(record["max_output_tokens"], 4096);
    assert_eq!(record["tokens_in"], 1_105_225);
    assert_eq!(record["tokens_out"], 0);
    // Provider text in the cause field is never echoed; only allowlisted
    // causes and numeric sizes pass through.
    let unknown = json!({"stage":"response_validation",
        "incomplete_reason":"provider secret detail","request_bytes":"not-a-number"});
    let record = failure_record_with_provider(reason(err.as_ref()), err.as_ref(), 7, &unknown);
    assert!(record.get("incomplete_reason").is_none());
    assert!(record.get("request_bytes").is_none());
    assert!(!record.to_string().contains("provider secret detail"));

    let raw = "model_http_401 bearer synthetic-secret-token {\"account\":\"synthetic\"}";
    let err: Box<dyn std::error::Error> = raw.into();
    let record = failure_record_with_provider("planner_provider_failed", err.as_ref(), 7, &stage);
    assert_eq!(record["reason"], "model_http_401");
    assert_eq!(record["provider_reason"], "model_http_401");
    assert_eq!(record["http_status"], 401);
    let text = record.to_string();
    assert!(!text.contains("synthetic-secret-token"));
    assert!(!text.contains("synthetic\""));
    assert!(!text.contains("account"));

    let err: Box<dyn std::error::Error> = "raw body access_token=synthetic-secret".into();
    let record = failure_record_with_provider("planner_provider_failed", err.as_ref(), 7, &stage);
    assert_eq!(record["reason"], "planner_provider_failed");
    assert!(!record.to_string().contains("synthetic-secret"));
    assert!(!record.to_string().contains("access_token"));

    let err: Box<dyn std::error::Error> = "planner_call_timeout".into();
    let record = failure_record_with_provider(
        reason(err.as_ref()),
        err.as_ref(),
        7,
        &json!({"stage":"transport"}),
    );
    assert_eq!(record["reason"], "planner_call_timeout");
    assert_eq!(record["provider_reason"], "planner_call_timeout");
    assert_eq!(record["provider_stage"], "transport");

    // A local admission/budget error after an earlier provider turn must not
    // inherit that turn's stage or HTTP status (#162/A0).
    for code in [
        "planner_usage_busy_no_retry",
        "planner_usage_lock_timeout",
        "planner_budget_exhausted",
    ] {
        let err: Box<dyn std::error::Error> = code.into();
        let record = failure_record_with_provider(
            reason(err.as_ref()),
            err.as_ref(),
            7,
            &json!({"stage":"output_validation","http_status":200}),
        );
        assert_eq!(record["reason"], code);
        assert!(
            record.get("provider_stage").is_none(),
            "{code} inherited a provider stage"
        );
        assert!(
            record.get("http_status").is_none(),
            "{code} inherited an HTTP status"
        );
    }
}
fn plan_text_and_decide_budget_keep_the_config_hash() {
    let abstain = json!({"actions":[],"stance":"abstain","notes":"wait"});
    let raw = format!("Prefer {{cash}} until the open.\n{abstain}");
    assert_eq!(plan_document(&raw).unwrap()["notes"], "wait");
    assert!(
        validate(&raw, &crate::runtime::tests::test_portfolio(), &[])
            .unwrap()
            .is_empty()
    );
    assert!(plan_document("not JSON").is_err());
    assert!(validate("not JSON", &crate::runtime::tests::test_portfolio(), &[]).is_err());
    let first = json!({"actions":[],"stance":"abstain","notes":"first"});
    let second = json!({"actions":[],"stance":"abstain","notes":"second"});
    assert_eq!(
        plan_document(&format!("{first}\ntrailing\n{second}")).unwrap()["notes"],
        "second"
    );
    assert_eq!(
        plan_document(&format!("{first} {{\"status\":\"ignored\"}}")).unwrap()["notes"],
        "first"
    );
    let code = |text: &str| -> &'static str {
        let err: Box<dyn std::error::Error> = text.into();
        reason(err.as_ref())
    };
    assert_eq!(
        code("planner_event_configuration_changed"),
        "planner_event_configuration_changed"
    );
    assert_eq!(
        code("planner_cancelled_no_replay"),
        "planner_cancelled_no_replay"
    );
    assert_eq!(
        code("planner_event_deadline_expired"),
        "planner_event_deadline_expired"
    );
    assert_eq!(code("harness_deadline_expired"), "planner_budget_exhausted");
    assert_eq!(
        code("harness_turn_budget_exhausted"),
        "planner_budget_exhausted"
    );
    assert_eq!(
        code("planner_json_invalid"),
        "planner_invalid_plan_no_entries"
    );
    assert_eq!(code("anything-else"), "planner_failed_no_new_entries");
    assert_eq!(
        code("harness_digest_invalid"),
        "planner_invalid_plan_no_entries"
    );
    for specific in [
        "harness_digest_json_invalid",
        "harness_digest_schema_invalid",
        "harness_digest_bounds_invalid",
    ] {
        assert_eq!(code(specific), "planner_invalid_plan_no_entries");
        let err: Box<dyn std::error::Error> = specific.into();
        assert_eq!(
            failure_record(code(specific), err.as_ref(), 7)["reason"],
            specific
        );
    }
    let err: Box<dyn std::error::Error> = "harness_digest_invalid".into();
    let record = failure_record("planner_invalid_plan_no_entries", err.as_ref(), 7);
    assert_eq!(record["reason"], "harness_digest_invalid");
    assert_eq!(record["status"], "planner_invalid_plan_no_entries");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planner.json");
    let defaults: PlannerConfig = serde_json::from_value(json!({"digest":{},"decide":{}})).unwrap();
    assert_eq!(defaults.digest.daily_call_budget, 0);
    assert_eq!(defaults.decide.daily_call_budget, 0);
    assert_eq!(defaults.decide.timeout_ms, 150_000);
    let mut explicit = defaults.clone();
    explicit.enabled = true;
    explicit.decide.enabled = true;
    explicit.decide.model_id = "fixture-model".into();
    fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
    assert_eq!(
        PlannerConfig::load(&path).err().unwrap().to_string(),
        "planner_role_invalid"
    );
    explicit.decide.daily_call_budget = 17;
    explicit.model_led = Some(true);
    explicit.usage_namespace = Some("custom-usage".into());
    fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
    assert_eq!(
        PlannerConfig::load(&path).unwrap().decide.daily_call_budget,
        17
    );
    let mut c = super::super::tests::config(dir.path());
    c.planner_config = Some(path.clone());
    prepare_review_usage(&c).unwrap();
    let pin = fs::read(c.directory.join("model/usage-identity.json")).unwrap();
    assert!(c.directory.join("model/custom-usage").is_dir());
    explicit.usage_namespace = Some("changed-usage".into());
    fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
    assert!(prepare_review_usage(&c).is_err());
    assert_eq!(
        fs::read(c.directory.join("model/usage-identity.json")).unwrap(),
        pin
    );
    for invalid in ["../usage", "/tmp/usage", "..", ""] {
        explicit.usage_namespace = Some(invalid.into());
        fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
        assert!(PlannerConfig::load(&path).is_err());
    }
    let mut raised = defaults.clone();
    raised.digest.daily_call_budget = 500;
    raised.decide.daily_call_budget = 100;
    assert_eq!(
        policy_bytes(&defaults).unwrap(),
        policy_bytes(&raised).unwrap()
    );
    raised.decide.output_cap += 1;
    assert_ne!(
        policy_bytes(&defaults).unwrap(),
        policy_bytes(&raised).unwrap()
    );
    let cfg = paper_cfg(120_000, None);
    fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let loaded = PlannerConfig::load(&path).unwrap();
    assert!(
        serde_json::to_value(&loaded).unwrap()["decide"]
            .get("run_timeout_ms")
            .is_none()
    );
    assert_eq!(loaded.decide.run_budget_ms(), 120_000);
    assert_eq!(loaded.decide.invocation_budget_ms("decide"), DEFAULT_RUN_MS);
    assert_eq!(
        loaded.digest.invocation_budget_ms("digest"),
        loaded.digest.timeout_ms
    );
    let hash = digest(&serde_json::to_vec(&loaded).unwrap());
    assert_eq!(
        hash,
        digest(&serde_json::to_vec(&PlannerConfig::load(&path).unwrap()).unwrap())
    );
    let mut bounded = paper_cfg(120_000, Some(MAX_RUN_MS));
    fs::write(&path, serde_json::to_vec(&bounded).unwrap()).unwrap();
    let loaded = PlannerConfig::load(&path).unwrap();
    assert_eq!(loaded.decide.invocation_budget_ms("decide"), MAX_RUN_MS);
    assert_ne!(digest(&serde_json::to_vec(&loaded).unwrap()), hash);
    bounded.decide.run_timeout_ms = Some(MAX_RUN_MS + 1);
    fs::write(&path, serde_json::to_vec(&bounded).unwrap()).unwrap();
    assert!(PlannerConfig::load(&path).is_err());
    bounded.decide.run_timeout_ms = Some(119_999);
    fs::write(&path, serde_json::to_vec(&bounded).unwrap()).unwrap();
    assert!(PlannerConfig::load(&path).is_err());
    bounded.decide.timeout_ms = 150_001;
    bounded.decide.run_timeout_ms = Some(150_001);
    fs::write(&path, serde_json::to_vec(&bounded).unwrap()).unwrap();
    assert!(PlannerConfig::load(&path).is_err());
    bounded.decide.timeout_ms = 60_000;
    bounded.decide.run_timeout_ms = Some(60_000);
    fs::write(&path, serde_json::to_vec(&bounded).unwrap()).unwrap();
    assert_eq!(
        PlannerConfig::load(&path)
            .unwrap()
            .decide
            .invocation_budget_ms("decide"),
        60_000
    );
    let mut digest_cfg = paper_cfg(120_000, None);
    digest_cfg.digest.timeout_ms = 60_000;
    digest_cfg.digest.run_timeout_ms = Some(120_000);
    fs::write(&path, serde_json::to_vec(&digest_cfg).unwrap()).unwrap();
    PlannerConfig::load(&path).unwrap();
    digest_cfg.digest.run_timeout_ms = Some(120_001);
    fs::write(&path, serde_json::to_vec(&digest_cfg).unwrap()).unwrap();
    assert!(PlannerConfig::load(&path).is_err());

    // Phase 2 deterministic validation: NSE_FO intents use the bounded option
    // catalog and the configured F&O fee schedule, never cash bps or index
    // notional. Buy-to-open only; sells need same-key held units.
    use sadr_core::finance::FeeSchedule;
    use sadr_core::market::{InstrumentKind, OptionCatalog, OptionContract, Segment};
    use sadr_core::time::ExchangeDate;
    let contract = OptionContract {
        key: "NSE_FO|26000CE".into(),
        symbol: "NIFTY 26000 CE 20 OCT 26".into(),
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        underlying_symbol: "NIFTY".into(),
        expiry: ExchangeDate::parse("2026-10-20").unwrap(),
        strike: sadr_core::finance::Price::parse("26000").unwrap(),
        option: InstrumentKind::Call,
        lot_size: 65,
        tick: sadr_core::finance::Price::parse("0.05").unwrap(),
        freeze_quantity: Some(1800),
        weekly: true,
    };
    let option_row = sadr_core::execution::Instrument {
        key: contract.key.clone(),
        symbol: contract.symbol.clone(),
        series: "CE".into(),
        tick: contract.tick,
    };
    let cash_row = sadr_core::execution::Instrument {
        key: "NSE_EQ|INE000000001".into(),
        symbol: "FIXTURE".into(),
        series: "EQ".into(),
        tick: sadr_core::finance::Price::parse("0.05").unwrap(),
    };
    let fo_fees: sadr_core::finance::FeeArithmetic = serde_json::from_value(json!({
        "per_order":"0.01","buy_bps":"0","sell_bps":"0",
        "slippage_bps":"10","rounding_increment":"0.05"
    }))
    .unwrap();
    let mut option_book = crate::runtime::tests::test_portfolio();
    option_book.cash = sadr_core::execution::a("3250.05");
    option_book.fee_schedule = Some(
        FeeSchedule::new(option_book.costs.clone().unwrap()).with_segment(Segment::NseFo, fo_fees),
    );
    option_book.options = Some(
        OptionCatalog::try_new(
            ExchangeDate::parse("2026-10-16").unwrap(),
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            contract.expiry,
            sadr_core::finance::Price::parse("26000").unwrap(),
            [contract.clone()],
        )
        .unwrap(),
    );
    let instruments = [cash_row.clone(), option_row.clone()];
    let action = |key: &str, side: &str, units: Option<u64>, max_price: &str| {
        json!({"instrument_key":key,"side":side,"units":units,"max_price":max_price,
            "rationale":"fixture","exit_intent":{}})
    };
    let verdicts = validate(
        &json!({"actions":[action("NSE_FO|26000CE","buy",Some(65),"50")],"stance":"long","notes":""}).to_string(),
        &option_book,
        &instruments,
    )
    .unwrap();
    assert_eq!(verdicts[0].reason, "accepted");
    assert_eq!(verdicts[0].units, 65);
    // The cash book would charge 5 bps (1.635); the F&O schedule charges
    // 0.05, so the exact premium + FO fee fits the available cash.
    let verdicts = validate(
        &json!({"actions":[action("NSE_FO|26000CE","buy",Some(130),"50")],"stance":"long","notes":""}).to_string(),
        &option_book,
        &instruments,
    )
    .unwrap();
    assert_eq!(verdicts[0].reason, "action_cash");
    // Lot grid, expiry hold, unknown derivative and index keys are refused.
    for (key, units, reason) in [
        ("NSE_FO|26000CE", 64u64, "action_option_quantity"),
        ("NSE_FO|26000PE", 65, "action_option_scope"),
        ("NSE_EQ|INE000000001", 1, "accepted"),
    ] {
        let mut rows = instruments.to_vec();
        if key == "NSE_FO|26000PE" {
            // An NSE_FO key that is not in the bounded catalog is refused,
            // never treated as cash or invented as a contract.
            rows.push(sadr_core::execution::Instrument {
                key: key.into(),
                symbol: "NIFTY 26000 PE".into(),
                series: "PE".into(),
                tick: sadr_core::finance::Price::parse("0.05").unwrap(),
            });
        }
        let verdicts = validate(
            &json!({"actions":[action(key,"buy",Some(units),"50")],"stance":"long","notes":""})
                .to_string(),
            &option_book,
            &rows,
        )
        .unwrap();
        assert_eq!(verdicts[0].reason, reason, "{key}");
    }
    let mut index_rows = instruments.to_vec();
    index_rows.push(sadr_core::execution::Instrument {
        key: "NSE_INDEX|Nifty 50".into(),
        symbol: "NIFTY".into(),
        series: "INDEX".into(),
        tick: sadr_core::finance::Price::parse("0.05").unwrap(),
    });
    let verdicts = validate(
        &json!({"actions":[action("NSE_INDEX|Nifty 50","buy",Some(1),"26000")],"stance":"long","notes":""}).to_string(),
        &option_book,
        &index_rows,
    )
    .unwrap();
    assert_eq!(verdicts[0].reason, "action_index_untradeable");
    // Expiry-day opening is refused; a same-day sell against the holding is
    // the explicit flatten path.
    let mut expiry_book = option_book.clone();
    if let Some(catalog) = expiry_book.options.as_mut() {
        catalog.session = ExchangeDate::parse("2026-10-20").unwrap();
    }
    let verdicts = validate(
        &json!({"actions":[action("NSE_FO|26000CE","buy",Some(65),"50")],"stance":"long","notes":""}).to_string(),
        &expiry_book,
        &instruments,
    )
    .unwrap();
    assert_eq!(verdicts[0].reason, "action_option_expiry");
    // Sells are only against same-key held units.
    let verdicts = validate(
        &json!({"actions":[action("NSE_FO|26000CE","sell",Some(65),"49.95")],"stance":"flat","notes":""}).to_string(),
        &option_book,
        &instruments,
    )
    .unwrap();
    assert_eq!(verdicts[0].reason, "action_option_short_open");
    let mut held_book = option_book.clone();
    held_book.positions.insert(
        option_row.key.clone(),
        sadr_core::execution::Position {
            instrument: option_row.clone(),
            units: 65,
            entry: sadr_core::finance::Price::parse("50").unwrap(),
            mark: sadr_core::finance::Price::parse("50").unwrap(),
            marked_at: 0,
            exit_intent: None,
            decision_id: None,
        },
    );
    let verdicts = validate(
        &json!({"actions":[action("NSE_FO|26000CE","sell",Some(65),"49.95")],"stance":"flat","notes":""}).to_string(),
        &held_book,
        &instruments,
    )
    .unwrap();
    assert_eq!(verdicts[0].reason, "accepted");
    assert_eq!(verdicts[0].units, 65);
}

#[test]
fn legacy_role_accessor_returns_the_stored_blocks() {
    use super::super::orchestrator_model::Trigger;
    let cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "digest": {"enabled": true, "model_id": "gpt-6-luna",
            "transport": "openai_codex", "tools": [], "timeout_ms": 20000,
            "output_cap": 4096, "daily_call_budget": 30},
        "decide": {"enabled": true, "model_id": "grok-4.7",
            "transport": "direct_oauth", "tools": ["web_search"],
            "timeout_ms": 150000, "output_cap": 8192, "daily_call_budget": 20}
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(cfg.role(Trigger::Digest)).unwrap(),
        serde_json::to_value(&cfg.digest).unwrap()
    );
    assert_eq!(
        serde_json::to_value(cfg.role(Trigger::Decide)).unwrap(),
        serde_json::to_value(&cfg.decide).unwrap()
    );
    {
        use super::super::orchestrator_model::Trigger;
        let cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
            "enabled": true,
            "post_close": {
                "enabled": true,
                "operations": {"enabled": true, "model_id": "grok-4.7",
                    "transport": "direct_oauth", "tools": [],
                    "timeout_ms": 20000, "output_cap": 4096,
                    "daily_call_budget": 3},
                "strategy": {"enabled": true, "model_id": "grok-4.7",
                    "transport": "direct_oauth", "tools": ["web_search", "x_search"],
                    "timeout_ms": 150000, "output_cap": 8192,
                    "daily_call_budget": 2}
            }
        }))
        .unwrap();
        let review = cfg.post_close.as_ref().unwrap();
        assert_eq!(
            serde_json::to_value(cfg.role(Trigger::Operations)).unwrap(),
            serde_json::to_value(&review.operations).unwrap()
        );
        assert_eq!(
            serde_json::to_value(cfg.role(Trigger::Strategy)).unwrap(),
            serde_json::to_value(&review.strategy).unwrap()
        );
    }
}

#[test]
fn unconfigured_orchestrator_trigger_is_disabled() {
    use super::super::orchestrator_model::{OrchestratorModelSettings, Trigger};
    let mut settings = OrchestratorModelSettings {
        enabled: true,
        ..OrchestratorModelSettings::default()
    };
    settings.triggers.remove(&Trigger::Strategy);
    let cfg = super::PlannerConfig {
        orchestrator: Some(settings),
        digest: super::ModelRole {
            enabled: true,
            model_id: "luna".into(),
            ..super::ModelRole::default()
        },
        ..super::PlannerConfig::default()
    };
    assert!(!cfg.role(Trigger::Strategy).enabled);
    assert_eq!(cfg.role(Trigger::Digest).model_id, "luna");
    assert!(!cfg.role(Trigger::Decide).enabled);
    let mut cfg = cfg;
    cfg.decide.enabled = true;
    cfg.decide.model_id = "independent-decide".into();
    cfg.decide.model_config = Some("decide-credentials.json".into());
    cfg.decide.daily_call_budget = 17;
    let decide = cfg.role(Trigger::Decide);
    assert!(decide.enabled);
    assert_eq!(decide.model_id, "independent-decide");
    assert_eq!(decide.model_config, cfg.decide.model_config);
    assert_eq!(decide.daily_call_budget, 17);
    cfg.orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&Trigger::Operations)
        .unwrap()
        .enabled = false;
    assert!(!cfg.role(Trigger::Operations).enabled);
    assert!(cfg.role(Trigger::Decide).enabled);
}

#[test]
fn subscription_preserves_local_tool_preflight_failure() {
    let role = ModelRole {
        model_id: crate::model::LUNA.into(),
        ..Default::default()
    };
    let frame = super::super::harness::Frame {
        role: "supervisor".into(),
        input: vec![],
        tools: vec![json!({"type":"function","name":"unexpected_tool"})],
        owner_tools: Default::default(),
        citation_ids: vec![],
        final_answer_only: false,
        review_only: false,
        decision_evidence: false,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let mut backend = Subscription::new(Path::new("/nonexistent-ind-sadr-model-config"));
    let error = backend
        .call(&role, &serde_json::to_string(&frame).unwrap())
        .unwrap_err();
    assert_eq!(error.to_string(), "harness_tool_definitions_changed");
    let record = failure_record_with_provider(
        reason(error.as_ref()),
        error.as_ref(),
        1,
        &backend.provenance(),
    );
    assert_eq!(record["reason"], "harness_tool_definitions_changed");
    assert_eq!(
        record["provider_reason"],
        "harness_tool_definitions_changed"
    );
    assert_eq!(record["provider_stage"], "tool_definitions");
}
