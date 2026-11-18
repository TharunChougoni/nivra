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
use super::*;
use nivra_core::execution::ExitIntent;

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
fn plan_validation_reserves_cash_and_holdings() {
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
}
