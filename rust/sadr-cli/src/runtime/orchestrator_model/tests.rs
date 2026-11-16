use super::*;
use serde_json::json;

#[test]
fn parses_target_shape_defaults_overrides_and_rejects_invalid() {
    let settings: OrchestratorModelSettings = serde_json::from_value(json!({
        "enabled": true,
        "model": {
            "model_config": "model/grok.json",
            "model_id": "grok-4.7",
            "transport": "direct_oauth",
            "timeout_ms": 120000,
            "output_cap": 8000,
            "approval_auto_renew": {
                "enabled": true,
                "lead_hours": 24,
                "expiry_time_ist": "18:30",
                "validity_days": 1,
                "owner_wait_attempts": 120,
                "owner_wait_ms": 1000
            }
        },
        "daily_call_budget": 200,
        "sandbox": {
            "cpu_seconds": 30,
            "memory_mb": 512,
            "wall_ms": 60000,
            "network": false
        },
        "triggers": {
            "operations": {
                "prompt": "prompts/operations.md",
                "tools": [],
                "max_turns": 2,
                "daily_call_budget": 1
            },
            "strategy": {
                "prompt": "prompts/strategy.md",
                "tools": ["x_search"],
                "max_turns": 8,
                "daily_call_budget": 2
            },
            "scorer": {
                "prompt": "prompts/scorer.md",
                "tools": ["sandbox_write", "replay"],
                "max_turns": 10,
                "daily_call_budget": 4
            }
        }
    }))
    .unwrap();

    assert!(settings.enabled);
    assert_eq!(settings.model.model_id, "grok-4.7");
    assert_eq!(settings.model.transport, Transport::DirectOauth);
    assert!(settings.model.approval_auto_renew.is_some());
    assert_eq!(settings.daily_call_budget, 200);
    assert_eq!(settings.sandbox, SandboxSettings::default());
    assert_eq!(settings.triggers.len(), Trigger::ALL.len());

    let operations = settings.role_for(Trigger::Operations);
    assert!(operations.enabled);
    assert_eq!(operations.model_id, "grok-4.7");
    assert!(operations.tools.is_empty());
    assert_eq!(operations.max_turns, Some(2));
    assert_eq!(operations.daily_call_budget, 1);
    assert_eq!(operations.timeout_ms, 120_000);
    assert_eq!(operations.output_cap, 8000);

    let scorer = settings.role_for(Trigger::Scorer);
    assert_eq!(scorer.tools, vec!["sandbox_write", "replay"]);
    assert_eq!(scorer.max_turns, Some(10));
    assert_eq!(scorer.daily_call_budget, 4);
    let settings = OrchestratorModelSettings::default();
    assert!(!settings.enabled);
    assert_eq!(settings.daily_call_budget, 200);
    assert_eq!(settings.max_prompt_bytes, 64 * 1024);
    assert_eq!(settings.model.timeout_ms, 120_000);
    assert_eq!(settings.model.output_cap, 8000);
    assert_eq!(settings.sandbox.cpu_seconds, 30);
    assert_eq!(settings.sandbox.memory_mb, 512);
    assert_eq!(settings.sandbox.wall_ms, 60_000);
    assert!(!settings.sandbox.network);
    assert_eq!(settings.triggers.len(), Trigger::ALL.len());

    let operations = settings.role_for(Trigger::Operations);
    assert!(!operations.enabled);
    assert!(operations.tools.is_empty());
    assert_eq!(operations.max_turns, Some(2));
    assert_eq!(operations.daily_call_budget, 1);
    assert_eq!(operations.timeout_ms, 120_000);
    assert_eq!(operations.output_cap, 8000);

    let strategy = settings.role_for(Trigger::Strategy);
    assert!(!strategy.enabled);
    assert_eq!(strategy.tools, vec!["web_search", "x_search"]);
    assert_eq!(strategy.max_turns, Some(8));
    assert_eq!(strategy.daily_call_budget, 2);
    let settings: OrchestratorModelSettings = serde_json::from_value(json!({
        "model": {
            "model_id": "grok-4.7",
            "transport": "direct_oauth",
            "timeout_ms": 90000,
            "output_cap": 4096,
            "max_turns": 3,
            "daily_call_budget": 9
        },
        "triggers": {
            "operations": {
                "prompt": "prompts/operations.md",
                "tools": ["x_search"],
                "max_turns": 4,
                "daily_call_budget": 7,
                "timeout_ms": 20000,
                "output_cap": 2048
            }
        }
    }))
    .unwrap();

    let operations = settings.role_for(Trigger::Operations);
    assert_eq!(operations.model_id, "grok-4.7");
    assert_eq!(operations.tools, vec!["x_search"]);
    assert_eq!(operations.max_turns, Some(4));
    assert_eq!(operations.daily_call_budget, 7);
    assert_eq!(operations.timeout_ms, 20_000);
    assert_eq!(operations.output_cap, 2048);

    let strategy = settings.role_for(Trigger::Strategy);
    assert_eq!(strategy.model_id, "grok-4.7");
    assert!(strategy.tools.is_empty());
    assert_eq!(strategy.max_turns, Some(3));
    assert_eq!(strategy.daily_call_budget, 9);
    assert_eq!(strategy.timeout_ms, 90_000);
    assert_eq!(strategy.output_cap, 4096);
    for value in [
        json!({"unknown": true}),
        json!({"model": {"unknown": true}}),
        json!({"sandbox": {"unknown": true}}),
        json!({"triggers": {"digest": {"unknown": true}}}),
        json!({"triggers": {"not_a_trigger": {}}}),
    ] {
        assert!(
            serde_json::from_value::<OrchestratorModelSettings>(value.clone()).is_err(),
            "accepted {value}"
        );
    }
    for trigger in Trigger::ALL {
        assert_eq!(
            serde_json::to_value(trigger).unwrap(),
            json!(trigger.as_str())
        );
    }
}

#[test]
fn prompts_load_stably_and_reject_missing_or_oversize() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/rust/prompts");
    let defaults = OrchestratorModelSettings::default();
    for trigger in Trigger::ALL {
        let mut settings = defaults.clone();
        settings.triggers.get_mut(&trigger).unwrap().prompt =
            dir.join(format!("{}.md", trigger.as_str()));
        let first = settings.load_prompt(trigger).unwrap();
        let second = settings.load_prompt(trigger).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.sha256.len(), 64);
        assert!(!first.text.trim().is_empty());
        assert!(first.text.len() <= 4096);
        if matches!(trigger, Trigger::Operations | Trigger::Strategy) {
            assert!(first.text.starts_with(&format!(
                "# Trigger: {} (template version 1)",
                trigger.as_str()
            )));
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let settings = OrchestratorModelSettings {
        triggers: BTreeMap::from([(
            Trigger::Operations,
            TriggerSettings {
                prompt: dir.path().join("missing.md"),
                ..TriggerSettings::default()
            },
        )]),
        ..OrchestratorModelSettings::default()
    };
    let error = settings.load_prompt(Trigger::Operations).unwrap_err();
    assert_eq!(error.to_string(), "orchestrator_prompt_missing");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oversize.md");
    std::fs::write(&path, vec![b'x'; 17]).unwrap();
    let settings = OrchestratorModelSettings {
        max_prompt_bytes: 16,
        triggers: BTreeMap::from([(
            Trigger::Operations,
            TriggerSettings {
                prompt: path,
                ..TriggerSettings::default()
            },
        )]),
        ..OrchestratorModelSettings::default()
    };
    let error = settings.load_prompt(Trigger::Operations).unwrap_err();
    assert_eq!(error.to_string(), "orchestrator_prompt_oversize");
}

#[test]
fn session_prompts_bind_content_and_reject_unknown_placeholders() {
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("session-system.md");
    std::fs::write(&system, "standing orchestrator role").unwrap();
    let mut settings = OrchestratorModelSettings {
        system_prompt: Some(system),
        triggers: BTreeMap::new(),
        ..OrchestratorModelSettings::default()
    };
    settings.trigger_prompts.insert(
        SessionTrigger::Idle,
        "Idle {{session}} at {{session_minute}}. Notes: {{notes}}".into(),
    );
    settings.validate_session_prompts().unwrap();
    let policy = settings.policy_value().unwrap();
    assert!(policy["system_prompt"].is_null());
    assert_eq!(policy["system_prompt_sha256"].as_str().unwrap().len(), 64);
    assert!(policy["trigger_prompts"]["idle"].is_string());
    let before = serde_json::to_vec(&policy).unwrap();
    settings.trigger_prompts.insert(
        SessionTrigger::Idle,
        "Idle {{session}} at {{unknown_fact}}".into(),
    );
    assert_eq!(
        settings.validate_session_prompts().unwrap_err().to_string(),
        "orchestrator_trigger_prompt_unknown_placeholder"
    );
    settings
        .trigger_prompts
        .insert(SessionTrigger::Idle, "Idle {{session}}".into());
    assert_ne!(
        serde_json::to_vec(&settings.policy_value().unwrap()).unwrap(),
        before
    );
    let hashes = settings.prompt_hashes().unwrap();
    assert_eq!(hashes["session_system"].as_str().unwrap().len(), 64);
    assert_eq!(hashes["trigger_idle"].as_str().unwrap().len(), 64);
    let facts = json!({"session":"2026-10-31","session_minute":600});
    assert_eq!(
        render_trigger("S {{session}} m {{session_minute}} x {{missing}}", &facts),
        "S 2026-10-31 m 600 x null"
    );
    for trigger in SessionTrigger::ALL {
        assert!(!trigger.default_prompt().is_empty());
    }
    assert_eq!(SessionTrigger::ALL.len(), 13);
    assert!(
        SessionTrigger::Decision
            .facts()
            .contains(&"decision_evidence")
    );
    assert!(SessionTrigger::Decision.facts().contains(&"digest"));
    assert!(
        serde_json::to_value(OrchestratorModelSettings::default())
            .unwrap()
            .get("trigger_prompts")
            .is_none(),
        "adding a built-in trigger must not serialize a new default override"
    );
    assert!(
        serde_json::from_value::<OrchestratorModelSettings>(
            json!({"trigger_prompts":{"not_a_trigger":"x"}})
        )
        .is_err()
    );
}

#[test]
fn trigger_blocks_round_trip_and_omit_default_enabled() {
    let settings = OrchestratorModelSettings::default();
    let encoded = serde_json::to_value(&settings).unwrap();
    for trigger in Trigger::ALL {
        assert!(
            encoded["triggers"][trigger.as_str()]
                .get("enabled")
                .is_none(),
            "{trigger:?}"
        );
    }
    let decoded: OrchestratorModelSettings = serde_json::from_value(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
}
