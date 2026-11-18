fn enabled_scorer_requires_after_minute_from_either_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planner.json");
    let base = serde_json::json!({"enabled": false});
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
fn packaged_owner_policy_binds_prompt_content_and_refuses_removed_keys() {
    // Validate the current setup as a complete input set. The prompt files
    // are public package sources; application grant paths are never opened.
    let setup = tempfile::tempdir().unwrap();
    let prompts = setup.path().join("prompts");
    std::fs::create_dir(&prompts).unwrap();
    let packaged =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/rust/prompts");
    for name in [
        "objective.md",
        "data_owner.md",
        "supervisor.md",
        "orchestrator.md",
        "scorer.md",
    ] {
        std::fs::copy(packaged.join(name), prompts.join(name)).unwrap();
    }
    let current = include_str!("../../../../../packaging/rust/planner-current.example.json")
        .replace("${CONFIG_DIR}", setup.path().to_str().unwrap());
    let path = setup.path().join("planner.json");
    std::fs::write(&path, &current).unwrap();
    let baseline = super::PlannerConfig::load(&path).unwrap();
    baseline.validate_decision_owner().unwrap();
    assert!(baseline.file_prompts.is_some());
    assert!(!super::policy_bytes(&baseline).unwrap().is_empty());
    // The owner is implicit. A config that still carries a removed legacy key
    // is refused until the reviewed migration rewrites it.
    for (key, value) in [
        ("decide", serde_json::json!({"enabled": false})),
        (
            "decision_owner",
            serde_json::json!({"version": 1, "owner": "orchestrator"}),
        ),
        ("handoff_gate", serde_json::json!({})),
        ("post_close", serde_json::json!({"enabled": false})),
    ] {
        let mut old: serde_json::Value = serde_json::from_str(&current).unwrap();
        old[key] = value;
        std::fs::write(&path, serde_json::to_vec(&old).unwrap()).unwrap();
        assert_eq!(
            super::PlannerConfig::load(&path).err().unwrap().to_string(),
            "planner_config_invalid",
            "{key}"
        );
    }

    let cfg: super::PlannerConfig = serde_json::from_value(serde_json::json!({
        "enabled": true,
        "digest": {"enabled": true, "model_id": "gpt-6-luna",
            "transport": "openai_codex", "daily_call_budget": 30}
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
            "transport": "openai_codex", "daily_call_budget": 30}
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
            "enabled": false,
            "discovery": {"max_scan_age_ms":180000, "max_prospects":10},
            "digest": {"enabled": true, "model_id": "gpt-6-luna",
                "transport": "openai_codex", "daily_call_budget": 30}
        }))
        .unwrap(),
    )
    .unwrap();
    let checked = super::super::experiment_profile::check(&planner_path, &profile_path).unwrap();
    assert_eq!(checked["status"], "experiment_profile_valid");
    assert!(checked["profile_sha256"].is_string());

    // An explicit profile digest prompt is the recorded digest identity; a
    // profile without one records the code-owned owner digest prompt.
    let explicit_prompts = configured.prompt_hashes().unwrap();
    let default_profile = dir.path().join("default-profile.json");
    std::fs::write(&default_profile, br#"{"version":1}"#).unwrap();
    configured.experiment_profile = Some(default_profile);
    configured.apply_experiment_profile().unwrap();
    let owner_prompts = configured.prompt_hashes().unwrap();
    assert_ne!(owner_prompts["digest"], explicit_prompts["digest"]);
    assert_eq!(
        owner_prompts["digest"],
        super::super::digest(super::super::harness::DATA_OWNER_PROMPT.as_bytes())
    );
    assert!(owner_prompts.get("decide").is_none());

    // Default files bind content, not directory names, into sealed policy.
    let prompt_dir = dir.path().join("prompts");
    std::fs::create_dir(&prompt_dir).unwrap();
    for name in [
        "objective",
        "data_owner",
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
        files.prompt_set().unwrap().role_for("digest"),
        Some("file data_owner")
    );
    assert_eq!(
        files.prompt_set().unwrap().role_for("supervisor"),
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
    std::fs::write(prompt_dir.join("data_owner.md"), "new digest").unwrap();
    assert_ne!(
        super::policy_bytes(&super::PlannerConfig::load(&planner_path).unwrap()).unwrap(),
        sealed
    );
    // A previously loaded set retains its bytes; bad reloads never use embedded fallback.
    for invalid in [Vec::new(), vec![b'x'; 65_537], vec![0xff]] {
        std::fs::write(prompt_dir.join("data_owner.md"), invalid).unwrap();
        assert!(super::PlannerConfig::load(&planner_path).is_err());
        assert_eq!(super::policy_bytes(&files).unwrap(), sealed);
    }
    std::fs::remove_file(prompt_dir.join("data_owner.md")).unwrap();
    assert!(super::PlannerConfig::load(&planner_path).is_err());
    std::fs::create_dir(prompt_dir.join("data_owner.md")).unwrap();
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
    let prompt = cfg.orchestrator.as_ref().unwrap().triggers[&Trigger::Scorer]
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
            .get_mut(&Trigger::Scorer)
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
        .get_mut(&Trigger::Scorer)
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
            transport: nivra_cli::native_transport::Transport::DirectOauth,
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
        transport: nivra_cli::native_transport::FallbackTransport::MistralChat,
        model_config: dir.path().join("mistral-fallback.json"),
        output_cap: Some(32_000),
        reasoning_effort: Some("max".into()),
    };
    let mut fallback = cfg.clone();
    fallback.orchestrator.as_mut().unwrap().model.fallback = Some(deepseek.clone());
    fallback.digest.fallback = Some(deepseek.clone());
    assert_ne!(super::policy_bytes(&fallback).unwrap(), original);
    assert_eq!(
        fallback.role(Trigger::Scorer).fallback.as_ref(),
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
        .get_mut(&Trigger::Scorer)
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
fn load_accepts_orchestrator_beside_digest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("planner.json");
    let digest_role = serde_json::json!({
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

    // The digest stays outside the orchestrator and coexists with it.
    let with_digest = serde_json::json!({
        "digest": digest_role,
        "orchestrator": {"enabled": true, "scorer_after_minute": 931}
    });
    std::fs::write(&path, serde_json::to_vec(&with_digest).unwrap()).unwrap();
    super::PlannerConfig::load(&path).unwrap();

    let legacy_only = serde_json::json!({"digest": digest_role});
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
use nivra_core::execution::ExitIntent;

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
    let roles = [("digest", 50), ("supervisor", 10), ("scorer", 16)];
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
    assert_eq!(counts["supervisor"], 10);
    assert_eq!(counts["scorer"], 16);

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
    let old:PlannerConfig=serde_json::from_value(json!({"enabled":false,"digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex","daily_call_budget":30}})).unwrap();
    fs::write(&old_path, serde_json::to_vec(&old).unwrap()).unwrap();
    let mut future = old.clone();
    future.effective_from = Some(NaiveDate::from_ymd_opt(2026, 10, 23).unwrap());
    future.previous_config = Some(old_path.clone());
    future.full_universe_features = Some(true);
    future.discovery=Some(serde_json::from_value(json!({"max_scan_age_ms":180000,"max_prospects":10000,"scan_batch_size":200,"scan_interval_ms":8000})).unwrap());
    future.digest.daily_call_budget = 5000;
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
    let cfg:PlannerConfig=serde_json::from_value(json!({"digest":{"enabled":true,"model_id":"gpt-6-luna","transport":"openai_codex","daily_call_budget":5000,"budget_unit":"event","max_turns":64}})).unwrap();
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
    cfg.digest.daily_call_budget = 1;
    cfg.digest.budget_unit = Some("event".into());
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
            "digest",
            &cfg.digest,
            "fixture",
            &mut backend,
            &guard,
        )
        .unwrap();
    }
    assert_eq!(usage(&c).unwrap()["digest"], 1);
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
            "digest",
            &cfg.digest,
            "fixture",
            &mut backend,
            &guard
        )
        .unwrap_err()
        .to_string(),
        "planner_budget_exhausted"
    );
    assert_eq!(backend.0, 2);
    assert_eq!(usage(&c).unwrap()["digest"], 1);
}
#[test]
fn serialized_provider_envelope_allows_encrypted_reasoning_overhead() {
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let cfg = PlannerConfig::default();
    let role = ModelRole {
        enabled: true,
        model_id: "grok-4.7".into(),
        transport: nivra_cli::native_transport::Transport::DirectOauth,
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
        "supervisor",
        &role,
        "fixture",
        &mut Payload(just_over_old),
        &guard,
    )
    .unwrap();
    call_guarded(
        &c,
        &cfg,
        "supervisor",
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
            "supervisor",
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
            "supervisor",
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
    plan_text_and_digest_budget_keep_the_config_hash();

    // Digest reservations count against the configured allowance and a raised
    // allowance admits the next call.
    let restart_dir = tempfile::tempdir().unwrap();
    let restart = super::super::tests::config(restart_dir.path());
    let digest_role = ModelRole {
        model_id: crate::model::LUNA.into(),
        transport: nivra_cli::native_transport::Transport::OpenaiCodex,
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
    assert!(!raw.is_empty());
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
    let role = ModelRole::default();
    assert_eq!(role.run_budget_ms(), role.timeout_ms);
    assert!(
        serde_json::to_value(&role)
            .unwrap()
            .get("run_timeout_ms")
            .is_none()
    );
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
    assert!(call(&c, &cfg, "digest", &role, "fixture", &mut backend).is_err());
    assert!(call(&c, &cfg, "digest", &role, "fixture", &mut backend).is_err());
    assert_eq!(backend.0, 2);
    assert_eq!(p, before);
    assert!(!cfg.enabled && !cfg.digest.enabled);
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
            "supervisor",
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
        json!({"digest":1,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
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
    assert!(
        call_guarded(
            &c,
            &cfg,
            "supervisor",
            &two,
            "fixture",
            &mut advancing,
            &guard
        )
        .is_err()
    );
    assert_eq!(advancing.1, 1);
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":2,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
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
            "supervisor",
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
    // Journals written before the owner-only policy may hold decide charges;
    // they stay valid, readable records.
    write_reservation("decide-0000.attempt.json");
    assert!(super::usage::reservations(journal_dir.path(), 16).is_ok());
    write_reservation("bogus-0000.attempt.json");
    assert!(super::usage::reservations(journal_dir.path(), 16).is_err());
    assert!(
        call(
            &c,
            &cfg,
            "supervisor",
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
            "supervisor",
            &enough,
            "fixture preview 2",
            &mut previews
        )
        .is_ok()
    );
    let latest = fs::read_dir(c.directory.join("model/usage"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("supervisor-")
                && path.to_string_lossy().ends_with(".attempt.json")
        })
        .max()
        .unwrap();
    let preserved = c.directory.join("model/preserved-supervisor-latest.json");
    fs::rename(&latest, &preserved).unwrap();
    assert!(
        call(
            &c,
            &cfg,
            "supervisor",
            &enough,
            "fixture event",
            &mut previews
        )
        .is_err()
    );
    assert!(usage(&c).is_err());
    assert_eq!(previews.0, 2); // A missing preview reservation cannot refund a call.
    fs::rename(&preserved, &latest).unwrap();
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":2,"operations":0,"strategy":0,"scorer":0,"supervisor":2,"research":0})
    );
    let original_bytes = fs::read(&latest).unwrap();
    fs::write(&latest, b"{\"changed\":\"synthetic reservation\"}").unwrap();
    assert!(
        call(
            &c,
            &cfg,
            "supervisor",
            &enough,
            "fixture event",
            &mut previews
        )
        .is_err()
    );
    assert_eq!(previews.0, 2);
    fs::copy(
        &latest,
        c.directory.join("model/changed-reservation-evidence.json"),
    )
    .unwrap();
    fs::write(&latest, &original_bytes).unwrap();
    assert_eq!(
        usage(&c).unwrap(),
        json!({"digest":2,"operations":0,"strategy":0,"scorer":0,"supervisor":2,"research":0})
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
        transport: nivra_cli::native_transport::Transport::DirectOauth,
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
    let latest_digest = fs::read_dir(c.directory.join("model/usage"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            name.starts_with("digest-") && name.ends_with(".result.json")
        })
        .max()
        .unwrap();
    let failed: Value = serde_json::from_slice(&fs::read(latest_digest).unwrap()).unwrap();
    assert_eq!(failed["failed"], true);
    assert_eq!(
        failed["transport_evidence"]["origin"],
        nivra_cli::native_transport::DIRECT_RESPONSES
    );
    assert_eq!(failed["transport_evidence"]["model"], "grok-4.7");
    let mut direct_cfg = PlannerConfig {
        digest: direct_role.clone(),
        ..Default::default()
    };
    let config_file = d.path().join("direct-digest.json");
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.digest.transport = nivra_cli::native_transport::Transport::OpenaiCodex;
    direct_cfg.digest.model_id = crate::model::LUNA.into();
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    direct_cfg.digest.model_id = "gpt-6.1-sol".into();
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    PlannerConfig::load(&config_file).unwrap();
    // The digest never runs native search tools.
    direct_cfg.digest.model_id = crate::model::LUNA.into();
    direct_cfg.digest.tools = vec!["web_search".into()];
    fs::write(&config_file, serde_json::to_vec(&direct_cfg).unwrap()).unwrap();
    assert!(PlannerConfig::load(&config_file).is_err());
    let journal = c.directory.join("model/usage");
    fs::rename(&journal, c.directory.join("model/preserved-usage")).unwrap();
    fs::create_dir(&journal).unwrap();
    assert!(usage(&c).is_err());
    assert!(call(&c, &cfg, "digest", &enough, "fixture", &mut advancing).is_err());
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
    provenance: Value,
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
        if let Some(stage) = value["fixture_provider_stage"].as_str() {
            self.provenance = json!({
                "stage": stage,
                "http_status": value["fixture_http_status"].as_u64().unwrap_or(401),
            });
        }
        if let Some(failure) = value["fixture_failure"].as_str() {
            return Err(failure.into());
        }
        Ok(value.to_string())
    }
    fn provenance(&self) -> Value {
        self.provenance.clone()
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
fn paper_cfg() -> PlannerConfig {
    serde_json::from_value(json!({
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
    .unwrap()
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
        let mut scan = nivra_core::execution::Scanner::default();
        scan.samples.insert(
            i.key.clone(),
            nivra_core::execution::Sample {
                history: vec![Price::parse("100").unwrap()],
                current: Some(Price::parse("101").unwrap()),
                seen: at,
                source: Some(at),
                last: at,
                reason: "move".into(),
                market_fields: Some(nivra_core::execution::MarketFields {
                    session_vwap: None,
                    best_bid: Some(100.0),
                    best_bid_qty: Some(100),
                    best_ask: Some(100.05),
                    best_ask_qty: Some(100),
                    total_buy_quantity: Some(1000),
                    total_sell_quantity: Some(1000),
                    lower_circuit_limit: nivra_core::execution::a("80"),
                    upper_circuit_limit: nivra_core::execution::a("120"),
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
    rollover::atomic(
        &c.directory.join("model/luna-latest.json"),
        &json!({
            "session_binding": book_binding(&c).unwrap(),
            "at_ms": at,
            "digest": {"summary":"old Luna context must not be replayed"}
        }),
    )
    .unwrap();
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
            json!({"digest":0,"operations":0,"strategy":0,"scorer":0,"supervisor":0,"research":0})
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
        provenance: Value::Null,
    };
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
        requests: backend.requests,
        plan: plan.map_err(|error| error.to_string()),
        dir,
    }
}
#[test]
fn tools_only_digest_sees_only_the_trigger_envelope() {
    let mut cfg = paper_cfg();
    cfg.digest = ModelRole {
        enabled: true,
        model_id: "gpt-6-luna".into(),
        transport: nivra_cli::native_transport::Transport::OpenaiCodex,
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
    // The orchestrator owner records Luna's valid factual digest as evidence
    // without carrying old Luna/Grok advisory context into this fresh request.
    let mut owner_cfg = cfg.clone();
    let mut orchestrator = super::super::orchestrator_model::OrchestratorModelSettings {
        enabled: true,
        ..Default::default()
    };
    orchestrator.model.model_id = "grok-4.7".into();
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
    let prompt = owner_out.requests[0]["input"].to_string();
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
    assert!(usage(&owner_out.c).unwrap().get("decide").is_none());
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

    // A pre-provider grant failure keeps its allowlisted local cause, and an
    // opaque HTTP 200 keeps an identifier-shaped provider error code (#14).
    // Free text in either field is dropped.
    let grant = json!({"stage":"grant_owner","local_cause":"grant_busy_or_unavailable",
        "provider_error_code":"context_length_exceeded"});
    let err: Box<dyn std::error::Error> = "planner_provider_failed".into();
    let record = failure_record_with_provider(reason(err.as_ref()), err.as_ref(), 7, &grant);
    assert_eq!(record["provider_stage"], "grant_owner");
    assert_eq!(record["local_cause"], "grant_busy_or_unavailable");
    assert_eq!(record["provider_error_code"], "context_length_exceeded");
    let free = json!({"stage":"grant_owner","local_cause":"lock held by pid 7",
        "provider_error_code":"Your prompt was rejected"});
    let record = failure_record_with_provider(reason(err.as_ref()), err.as_ref(), 7, &free);
    assert!(record.get("local_cause").is_none());
    assert!(record.get("provider_error_code").is_none());

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
fn plan_text_and_digest_budget_keep_the_config_hash() {
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
    let defaults: PlannerConfig = serde_json::from_value(json!({"digest":{}})).unwrap();
    assert_eq!(defaults.digest.daily_call_budget, 0);
    assert_eq!(defaults.digest.timeout_ms, 20_000);
    let mut explicit = defaults.clone();
    explicit.digest.enabled = true;
    explicit.digest.model_id = "fixture-model".into();
    fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
    assert_eq!(
        PlannerConfig::load(&path).err().unwrap().to_string(),
        "planner_role_invalid"
    );
    explicit.digest.daily_call_budget = 17;
    explicit.model_led = Some(true);
    explicit.usage_namespace = Some("custom-usage".into());
    fs::write(&path, serde_json::to_vec(&explicit).unwrap()).unwrap();
    assert_eq!(
        PlannerConfig::load(&path).unwrap().digest.daily_call_budget,
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
    assert_eq!(
        policy_bytes(&defaults).unwrap(),
        policy_bytes(&raised).unwrap()
    );
    raised.digest.output_cap += 1;
    assert_ne!(
        policy_bytes(&defaults).unwrap(),
        policy_bytes(&raised).unwrap()
    );
    let mut cfg = paper_cfg();
    cfg.enabled = false;
    fs::write(&path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let loaded = PlannerConfig::load(&path).unwrap();
    assert!(
        serde_json::to_value(&loaded).unwrap()["digest"]
            .get("run_timeout_ms")
            .is_none()
    );
    assert_eq!(loaded.digest.run_budget_ms(), loaded.digest.timeout_ms);
    let hash = digest(&serde_json::to_vec(&loaded).unwrap());
    assert_eq!(
        hash,
        digest(&serde_json::to_vec(&PlannerConfig::load(&path).unwrap()).unwrap())
    );
    let mut digest_cfg = paper_cfg();
    digest_cfg.enabled = false;
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
    use nivra_core::finance::FeeSchedule;
    use nivra_core::market::{InstrumentKind, OptionCatalog, OptionContract, Segment};
    use nivra_core::time::ExchangeDate;
    let contract = OptionContract {
        key: "NSE_FO|26000CE".into(),
        symbol: "NIFTY 26000 CE 20 OCT 26".into(),
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        underlying_symbol: "NIFTY".into(),
        expiry: ExchangeDate::parse("2026-10-20").unwrap(),
        strike: nivra_core::finance::Price::parse("26000").unwrap(),
        option: InstrumentKind::Call,
        lot_size: 65,
        tick: nivra_core::finance::Price::parse("0.05").unwrap(),
        freeze_quantity: Some(1800),
        weekly: true,
    };
    let option_row = nivra_core::execution::Instrument {
        key: contract.key.clone(),
        symbol: contract.symbol.clone(),
        series: "CE".into(),
        tick: contract.tick,
    };
    let cash_row = nivra_core::execution::Instrument {
        key: "NSE_EQ|INE000000001".into(),
        symbol: "FIXTURE".into(),
        series: "EQ".into(),
        tick: nivra_core::finance::Price::parse("0.05").unwrap(),
    };
    let fo_fees: nivra_core::finance::FeeArithmetic = serde_json::from_value(json!({
        "per_order":"0.01","buy_bps":"0","sell_bps":"0",
        "slippage_bps":"10","rounding_increment":"0.05"
    }))
    .unwrap();
    let mut option_book = crate::runtime::tests::test_portfolio();
    option_book.cash = nivra_core::execution::a("3250.05");
    option_book.fee_schedule = Some(
        FeeSchedule::new(option_book.costs.clone().unwrap()).with_segment(Segment::NseFo, fo_fees),
    );
    option_book.options = Some(
        OptionCatalog::try_new(
            ExchangeDate::parse("2026-10-16").unwrap(),
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            contract.expiry,
            nivra_core::finance::Price::parse("26000").unwrap(),
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
            rows.push(nivra_core::execution::Instrument {
                key: key.into(),
                symbol: "NIFTY 26000 PE".into(),
                series: "PE".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
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
    index_rows.push(nivra_core::execution::Instrument {
        key: "NSE_INDEX|Nifty 50".into(),
        symbol: "NIFTY".into(),
        series: "INDEX".into(),
        tick: nivra_core::finance::Price::parse("0.05").unwrap(),
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
        nivra_core::execution::Position {
            instrument: option_row.clone(),
            units: 65,
            entry: nivra_core::finance::Price::parse("50").unwrap(),
            mark: nivra_core::finance::Price::parse("50").unwrap(),
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
fn unconfigured_orchestrator_trigger_is_disabled() {
    use super::super::orchestrator_model::{OrchestratorModelSettings, Trigger};
    let mut settings = OrchestratorModelSettings {
        enabled: true,
        ..OrchestratorModelSettings::default()
    };
    settings.triggers.remove(&Trigger::Scorer);
    let cfg = super::PlannerConfig {
        orchestrator: Some(settings),
        digest: super::ModelRole {
            enabled: true,
            model_id: "luna".into(),
            ..super::ModelRole::default()
        },
        ..super::PlannerConfig::default()
    };
    assert!(!cfg.role(Trigger::Scorer).enabled);
    assert_eq!(cfg.role(Trigger::Digest).model_id, "luna");
    let mut cfg = cfg;
    cfg.orchestrator = None;
    assert!(!cfg.role(Trigger::Supervisor).enabled);
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
    let mut backend = Subscription::new(Path::new("/nonexistent-nivra-model-config"));
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

/// The planner audit input is the other multi-megabyte pre-provider artifact:
/// repeated catalog/auth failures must keep one representative bundle per
/// failure window, not the full input on every retry (#23).
#[test]
fn repeated_pre_provider_failures_bound_the_planner_audit_input() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("sessions/2026-11-17/model");
    fs::create_dir_all(&model).unwrap();
    let input = json!({"historical_features":[{"padding":"x".repeat(2_000_000)}]});
    let encoded = serde_json::to_vec(&input).unwrap();
    let input_hash = digest(&encoded);
    let input_bytes = encoded.len() as u64;
    assert!(input_bytes > 2_000_000);
    let mut persisted = 0u64;
    let mut representatives = 0;
    let mut receipts = 0;
    let mut first = None::<String>;
    for attempt in 0..8 {
        let decision_id = format!("2026-11-17-attempt-{attempt}");
        let audit = model.join(&decision_id);
        fs::create_dir(&audit).unwrap();
        let evidence = super::super::harness::retain_planner_input(
            super::super::harness::PlannerInput {
                model: &model,
                archive: &audit,
                run: &decision_id,
                role: "digest",
                model_id: "gpt-6-luna",
                reason: "model_http_401",
                stage: "catalog",
                input_hash: &input_hash,
                input_bytes,
                now: 1_000 + attempt,
            },
            || {
                rollover::atomic(
                    &audit.join("input.json"),
                    &json!({"input":input,"input_hash":input_hash}),
                )
            },
        );
        assert_eq!(evidence["pre_provider"], true);
        assert_eq!(evidence["provider_stage"], "catalog");
        assert_eq!(evidence["reason"], "model_http_401");
        assert_eq!(evidence["input_hash"], input_hash);
        assert_eq!(evidence["input_bytes"], input_bytes);
        assert_eq!(evidence["input_receipt"], "input-receipt.json");
        persisted += fs::metadata(audit.join("input-receipt.json"))
            .unwrap()
            .len();
        if audit.join("input.json").is_file() {
            representatives += 1;
            first.get_or_insert(decision_id.clone());
            assert_eq!(evidence["input_archive"], "input.json");
            assert!(evidence["representative_input"].is_null());
            persisted += fs::metadata(audit.join("input.json")).unwrap().len();
        } else {
            receipts += 1;
            assert!(evidence["input_archive"].is_null());
            assert_eq!(
                evidence["representative_input"]["run"],
                first.clone().unwrap()
            );
            assert_eq!(evidence["representative_input"]["blob"], "input.json");
        }
    }
    assert_eq!(representatives, 1);
    assert_eq!(receipts, 7);
    let representative = fs::metadata(model.join(first.unwrap()).join("input.json"))
        .unwrap()
        .len();
    assert!(
        persisted < representative + 64 * 1024,
        "persisted {persisted} audit bytes for 8 pre-provider failures \
         (one representative is {representative})"
    );
    // A provider turn ends the window; the next outage keeps its own first
    // representative input instead of linking to stale evidence.
    super::super::harness::close_failure_window(&model, "digest", "gpt-6-luna", 2_000);
    let audit = model.join("2026-11-17-recovered");
    fs::create_dir(&audit).unwrap();
    let evidence = super::super::harness::retain_planner_input(
        super::super::harness::PlannerInput {
            model: &model,
            archive: &audit,
            run: "2026-11-17-recovered",
            role: "digest",
            model_id: "gpt-6-luna",
            reason: "model_http_401",
            stage: "catalog",
            input_hash: &input_hash,
            input_bytes,
            now: 2_001,
        },
        || {
            rollover::atomic(
                &audit.join("input.json"),
                &json!({"input":input,"input_hash":input_hash}),
            )
        },
    );
    assert_eq!(evidence["input_archive"], "input.json");
    assert!(audit.join("input.json").is_file());
}

/// A pre-provider failure detected by the planner keeps a bounded audit
/// receipt, the failure evidence and the first representative input.
#[test]
fn pre_provider_failure_keeps_a_bounded_planner_audit_receipt() {
    let mut cfg = paper_cfg();
    cfg.digest = ModelRole {
        enabled: true,
        model_id: "gpt-6-luna".into(),
        transport: nivra_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 30,
        ..Default::default()
    };
    cfg.discovery =
        Some(serde_json::from_value(json!({"max_scan_age_ms":5000,"max_prospects":20})).unwrap());
    let outcome = drive(
        &cfg,
        vec![json!({"fixture_failure":"model_http_401","fixture_provider_stage":"catalog"})],
        vec![0],
        None,
    );
    assert!(outcome.plan.is_err(), "{:?}", outcome.plan);
    let model = outcome.c.directory.join("model");
    let mut audits: Vec<_> = fs::read_dir(&model)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("harness-"))
                && path.join("failure.json").is_file()
        })
        .collect();
    assert_eq!(audits.len(), 1);
    let audit = audits.pop().unwrap();
    let failure: Value =
        serde_json::from_slice(&fs::read(audit.join("failure.json")).unwrap()).unwrap();
    assert_eq!(failure["pre_provider"], true);
    assert_eq!(failure["provider_stage"], "catalog");
    assert_eq!(failure["reason"], "model_http_401");
    assert_eq!(failure["input_archive"], "input.json");
    assert_eq!(failure["input_receipt"], "input-receipt.json");
    assert_eq!(failure["input_hash"].as_str().unwrap().len(), 64);
    assert!(failure["input_bytes"].as_u64().unwrap() > 0);
    assert!(audit.join("input.json").is_file());
    let receipt: Value =
        serde_json::from_slice(&fs::read(audit.join("input-receipt.json")).unwrap()).unwrap();
    assert_eq!(receipt["kind"], "planner_audit_input");
    assert_eq!(receipt["pre_provider"], true);
    assert_eq!(receipt["reason"], "model_http_401");
    assert_eq!(receipt["paper_only"], true);
}
