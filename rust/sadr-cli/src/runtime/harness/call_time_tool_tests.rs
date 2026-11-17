use super::*;
use planner::ToolsSettings;
use serde_json::json;
use std::collections::VecDeque;

fn limits() -> ToolsSettings {
    ToolsSettings {
        scanner_limit: 3,
        ranking_limit: 4,
        history_limit: 2,
        max_instrument_keys: 2,
        ..ToolsSettings::default()
    }
}

fn instrument(key: &str) -> Value {
    json!({"key":key,"symbol":key.rsplit('|').next().unwrap_or(key),"series":"EQ","tick":"0.05"})
}

fn bundle() -> Value {
    json!({
        "session":"2026-10-16",
        "allowlist":[instrument("NSE_EQ|A"),instrument("NSE_EQ|B")],
        "discovery":{
            "prospects":[
                {"instrument_key":"NSE_EQ|A","symbol":"A","sample_change_bps":12.5,"bid":"100","ask":"100.05","bid_units":10,"ask_units":12,"quote_age_ms":100,"screening_age_ms":200,"observed_ms":123,"screening_reason":"move"},
                {"instrument_key":"NSE_EQ|B","symbol":"B","sample_change_bps":5.0,"bid":"50","ask":"50.05","bid_units":5,"ask_units":6,"quote_age_ms":110,"screening_age_ms":210,"observed_ms":124,"screening_reason":"move"}
            ],
            "market_median_bps":5.0
        },
        "historical_features":[
            {"instrument":instrument("NSE_EQ|A"),"history_as_of":"2026-10-15","history_current":true,"return_20d":0.1,"liquidity":"100000"},
            {"instrument":instrument("NSE_EQ|B"),"history_as_of":"2026-10-15","history_current":false,"return_20d":-0.1,"liquidity":"200000"}
        ],
        "cash":"5000",
        "positions":{},
        "book_state":{"halted":false,"stopped":false,"closed":false},
        "decision_history":{"luna":null,"grok":[],"advisory_memory":null},
        "trigger":{"kind":"scanner","reason":"move","observed_ms":123}
    })
}

#[test]
fn schemas_use_configured_limits_and_digest_has_no_search() {
    let settings = limits();
    let digest = configured_definitions_with("digest", &[], false, &settings).unwrap();
    let names: Vec<&str> = digest
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for name in [
        "read_evidence",
        "get_instrument_features",
        "get_market_snapshot",
        "get_scanner_prospects",
        "rank_universe",
        "get_portfolio",
        "get_recent_decisions",
        "get_session_status",
    ] {
        assert!(names.contains(&name), "missing {name}");
    }
    assert!(!names.contains(&"get_luna_tool_results"));
    assert!(!names.contains(&"web_search"));
    let tool = |name: &str| {
        digest
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        tool("get_instrument_features")["parameters"]["properties"]["instrument_keys"]["maxItems"],
        2
    );
    assert_eq!(
        tool("get_scanner_prospects")["parameters"]["properties"]["limit"]["maximum"],
        3
    );
    assert_eq!(
        tool("rank_universe")["parameters"]["properties"]["limit"]["maximum"],
        4
    );
    assert_eq!(
        tool("get_recent_decisions")["parameters"]["properties"]["limit"]["maximum"],
        2
    );
    assert!(configured_definitions_with("decide", &[], false, &settings).is_err());
    // OpenAI strict function schemas require every property in `required`;
    // optional arguments are nullable instead (#123).
    let owner = call_tool_definitions(
        "supervisor",
        &[],
        false,
        &settings,
        OwnerTools {
            research: true,
            candidate: true,
            candidate_execution: false,
            workspace: true,
            web: true,
            web_search: true,
            web_news: true,
            ..Default::default()
        },
    )
    .unwrap();
    // Config-gated orchestrator web tools (#222) are advertised only for the
    // enabled capabilities, and only on the supervisor/owner role.
    let candidate_submit = owner
        .iter()
        .find(|tool| tool["name"] == "candidate_submit")
        .unwrap();
    assert_eq!(
        candidate_submit["parameters"]["properties"]["parent_task_id"]["type"],
        json!(["string", "null"])
    );
    assert!(
        candidate_submit["parameters"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("parent_task_id"))
    );
    assert_eq!(
        candidate_submit["parameters"]["properties"]["params"]["type"],
        "string"
    );
    // The execution argument is advertised only when the reviewed capability
    // is enabled; the disabled schema keeps its historical bytes.
    assert!(
        candidate_submit["parameters"]["properties"]
            .get("execution")
            .is_none()
    );
    assert!(
        !candidate_submit["parameters"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("execution"))
    );
    let execution_owner = call_tool_definitions(
        "supervisor",
        &[],
        false,
        &settings,
        OwnerTools {
            research: true,
            candidate: true,
            candidate_execution: true,
            ..OwnerTools::default()
        },
    )
    .unwrap();
    let execution_submit = execution_owner
        .iter()
        .find(|tool| tool["name"] == "candidate_submit")
        .unwrap();
    assert_eq!(
        execution_submit["parameters"]["properties"]["execution"]["type"],
        json!(["string", "null"])
    );
    assert!(
        execution_submit["parameters"]["required"]
            .as_array()
            .unwrap()
            .contains(&json!("execution"))
    );
    // A research turn gets only candidate and workspace list/read/write plus
    // the optional archived-receipt quote reader. It never gets TradePlan,
    // market/financial/web/policy, workspace_run/install or registered-analysis
    // enqueue, and review remains tool-less.
    let research_turn = call_tool_definitions(
        "supervisor",
        &[],
        false,
        &settings,
        OwnerTools {
            research_turn: true,
            candidate: true,
            workspace: true,
            quote_capture: true,
            ..Default::default()
        },
    )
    .unwrap();
    let research_names = research_turn
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    for name in [
        "candidate_submit",
        "candidate_status",
        "candidate_result",
        "candidate_cancel",
        "workspace_list",
        "workspace_read",
        "workspace_write",
        "research_quote_capture",
    ] {
        assert!(research_names.iter().any(|tool| tool == name), "{name}");
    }
    for name in [
        "research_task_enqueue",
        "workspace_run",
        "workspace_install",
        "web_search",
        "web_fetch",
        "news_lookup",
        "get_market_snapshot",
        "get_portfolio",
        "read_evidence",
    ] {
        assert!(!research_names.iter().any(|tool| tool == name), "{name}");
    }
    let default_tools = serde_json::to_value(OwnerTools::default()).unwrap();
    assert!(default_tools.get("research_turn").is_none());
    assert!(default_tools.get("quote_capture").is_none());
    let enabled_tools = serde_json::to_value(OwnerTools {
        research_turn: true,
        quote_capture: true,
        ..Default::default()
    })
    .unwrap();
    assert_eq!(enabled_tools["research_turn"], true);
    assert_eq!(enabled_tools["quote_capture"], true);
    let web_names = |owner: OwnerTools| -> Vec<String> {
        call_tool_definitions("supervisor", &[], false, &settings, owner)
            .unwrap()
            .iter()
            .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
            .filter(|name| name.starts_with("web_") || name == "news_lookup")
            .collect()
    };
    assert!(web_names(OwnerTools::default()).is_empty());
    assert_eq!(
        web_names(OwnerTools {
            web: true,
            web_search: true,
            ..OwnerTools::default()
        }),
        vec!["web_search", "web_fetch"]
    );
    assert_eq!(
        web_names(OwnerTools {
            web: true,
            web_news: true,
            ..OwnerTools::default()
        }),
        vec!["web_fetch", "news_lookup"]
    );
    assert!(
        call_tool_definitions(
            "digest",
            &[],
            false,
            &settings,
            OwnerTools {
                web: true,
                ..OwnerTools::default()
            },
        )
        .is_err()
    );
    fn strict_schema(schema: &Value) {
        let object = schema["type"] == "object"
            || schema["type"]
                .as_array()
                .is_some_and(|types| types.contains(&json!("object")));
        if object {
            assert_eq!(schema["additionalProperties"], false);
            let properties = schema["properties"].as_object().unwrap();
            let required = schema["required"].as_array().unwrap();
            assert_eq!(properties.len(), required.len());
            for (name, value) in properties {
                assert!(required.contains(&json!(name)));
                strict_schema(value);
            }
        }
        if let Some(items) = schema.get("items") {
            strict_schema(items);
        }
        for key in ["anyOf", "allOf", "oneOf"] {
            if let Some(parts) = schema[key].as_array() {
                for part in parts {
                    strict_schema(part);
                }
            }
        }
    }
    let run = owner
        .iter()
        .find(|tool| tool["name"] == "workspace_run")
        .unwrap();
    assert_eq!(
        run["parameters"]["properties"]["input"]["type"],
        json!(["string", "null"])
    );
    assert_eq!(
        run["parameters"]["properties"]["input_path"]["type"],
        json!(["string", "null"])
    );
    for tool in digest.iter().chain(owner.iter()) {
        strict_schema(&tool["parameters"]);
        if tool["strict"] != true {
            continue;
        }
        let mut properties: Vec<&String> = tool["parameters"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        let mut required: Vec<&str> = tool["parameters"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        properties.sort();
        required.sort();
        assert_eq!(
            properties.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            required,
            "{} must require every property",
            tool["name"]
        );
    }
    let call = |name: &str, args: Value| json!({"type":"function_call","call_id":"call_fixture","name":name,"arguments":args.to_string()});
    for (name, args) in [
        ("get_scanner_prospects", json!({"limit":null,"sort":null})),
        (
            "rank_universe",
            json!({"sort_by":null,"order":null,"limit":null,"min_liquidity":null}),
        ),
        ("get_recent_decisions", json!({"limit":null})),
    ] {
        assert!(
            checked_call_with(&call(name, args), &digest, &settings).is_ok(),
            "{name} must accept null defaults"
        );
    }
    assert!(
        configured_definitions_with("digest", &["web_search".into()], false, &settings).is_err()
    );
}

#[test]
fn call_limits_unknown_data_and_execution_authority_are_explicit() {
    let settings = limits();
    let tools = configured_definitions_with("digest", &[], false, &settings).unwrap();
    let call = |name: &str, args: Value| json!({"type":"function_call","call_id":"call_fixture","name":name,"arguments":args.to_string()});
    assert!(
        checked_call_with(
            &call("get_scanner_prospects", json!({"limit":4})),
            &tools,
            &settings
        )
        .is_err()
    );
    assert!(
        checked_call_with(
            &call("rank_universe", json!({"limit":5})),
            &tools,
            &settings
        )
        .is_err()
    );
    assert!(
        checked_call_with(
            &call("get_recent_decisions", json!({"limit":3})),
            &tools,
            &settings
        )
        .is_err()
    );
    assert!(
        checked_call_with(
            &call(
                "get_instrument_features",
                json!({"instrument_keys":["A","B","C"]})
            ),
            &tools,
            &settings
        )
        .is_err()
    );
    let (_, folded) = checked_call_with(
        &call(
            "get_instrument_features",
            json!({"instrument_keys":["A","A"]}),
        ),
        &tools,
        &settings,
    )
    .unwrap();
    match folded {
        Tool::Features(keys) => assert_eq!(keys, vec!["A".to_owned()]),
        _ => panic!("wrong tool"),
    }

    let scanner = scanner_prospects(&json!({}), 3, "scanner").unwrap();
    assert_eq!(scanner["status"], "unknown");
    assert_eq!(scanner["execution_authorized"], false);
    let empty = scanner_prospects(&json!({"discovery":{"prospects":[]}}), 3, "scanner").unwrap();
    assert_eq!(empty["status"], "no_fresh_prospects");
    let available = scanner_prospects(&bundle(), 3, "change_bps").unwrap();
    assert_eq!(available["status"], "available");
    assert_eq!(available["prospects"][0]["instrument_key"], "NSE_EQ|A");
    assert_eq!(available["prospects"][0]["market_relative_bps"], 7.5);
    assert_eq!(available["execution_authorized"], false);

    let ranked = rank_universe(
        &bundle(),
        &RankArgs {
            sort_by: Some("return_20d".into()),
            order: Some("desc".into()),
            limit: Some(4),
            min_liquidity: None,
        },
    )
    .unwrap();
    assert_eq!(ranked["rows"][0]["instrument_key"], "NSE_EQ|A");
    assert_eq!(ranked["rows"][1]["history_current"], false);
    assert_eq!(ranked["execution_authorized"], false);
    let mut wide = bundle();
    wide["allowlist"]
        .as_array_mut()
        .unwrap()
        .extend((0..2733).map(|index| instrument(&format!("fixture-unavailable-{index}"))));
    let args = RankArgs {
        sort_by: None,
        order: None,
        limit: Some(4),
        min_liquidity: None,
    };
    let bounded = rank_universe(&wide, &args).unwrap();
    assert!(serde_json::to_vec(&bounded).unwrap().len() <= TOOL_OUTPUT_MAX_BYTES);
    assert_eq!(bounded["unavailable_count"], 2733);
    assert_eq!(bounded["unavailable_reasons"]["history_unavailable"], 2733);
    assert_eq!(bounded["unavailable"].as_array().unwrap().len(), 4);
    assert_eq!(bounded["unavailable_truncated"], true);
    assert_eq!(bounded["rows"], ranked["rows"]);
    // Even a large individual row cannot exceed the tool byte budget or
    // be silently presented as a complete ranking.
    wide["historical_features"][0]["gap"] = json!("x".repeat(TOOL_OUTPUT_MAX_BYTES));
    let bounded = rank_universe(&wide, &args).unwrap();
    assert!(serde_json::to_vec(&bounded).unwrap().len() <= TOOL_OUTPUT_MAX_BYTES);
    assert_eq!(bounded["rows_truncated"], true);
    assert_eq!(bounded["execution_authorized"], false);
    let unknown = rank_universe(
        &json!({"allowlist":[instrument("NSE_EQ|A")]}),
        &RankArgs {
            sort_by: None,
            order: None,
            limit: None,
            min_liquidity: None,
        },
    )
    .unwrap();
    assert_eq!(unknown["status"], "unknown");
    assert_eq!(unknown["execution_authorized"], false);

    let portfolio = portfolio_view(&bundle(), 123, ToolsSettings::default().fill_limit);
    assert_eq!(portfolio["status"], "no_open_positions");
    assert_eq!(portfolio["execution_authorized"], false);

    // Lots and exact net P&L stay tied to the recorded fills, not to the
    // current position average alone.
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-16T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let held = json!({
        "session":"2026-10-16",
        "cash":"3900",
        "fees":serde_json::to_value(super::super::tests::test_costs()).unwrap(),
        "positions":{"NSE_EQ|A":{"instrument":instrument("NSE_EQ|A"),"units":10,
            "entry":"100","mark":"110","marked_at":at,
            "exit_intent":{"target":"120","stop":"90"},"decision_id":"d1"}},
        "fills":[{"at":at,"instrument":instrument("NSE_EQ|A"),"units":10,"buy":true,
            "price":"100","fee":"1","decision_id":"d1"}],
        "fills_total":1,
        "account_performance":{"session_realized_pnl_net":"0","fees_paid_session":"1"},
        "book_state":{"halted":false,"stopped":false,"closed":false},
        "previous_luna":{"digest":{"positions_review":[{"instrument_key":"NSE_EQ|A",
            "opinion":"hold","thesis_status":"intact","worth_keeping_reason":"fixture",
            "evidence_ids":["portfolio"],"confidence":"med"}]}},
        "decision_history":{"grok":[{"decision_id":"d1","parsed_plan":{"actions":[
            {"instrument_key":"NSE_EQ|A","side":"buy","rationale":"fixture rationale"}]}}]}
    });
    let view = portfolio_view(&held, at + 60_000, 20);
    assert_eq!(view["positions"][0]["units"], 10);
    assert_eq!(view["positions"][0]["lots"][0]["units"], 10);
    assert_eq!(view["positions"][0]["lots"][0]["decision_id"], "d1");
    assert!(view["positions"][0]["avg_entry_after_fees"].is_string());
    assert!(view["positions"][0]["unrealized_pnl_net"].is_string());
    assert!(view["positions"][0]["unrealized_bps"].is_string());
    assert_eq!(view["positions"][0]["entry_rationale"], "fixture rationale");
    assert_eq!(view["positions"][0]["last_review"]["opinion"], "hold");
    assert_eq!(view["positions"][0]["held_minutes"], 1);
    assert_eq!(view["fills_today"].as_array().unwrap().len(), 1);
    assert_eq!(view["fees_paid_session"], "1");
    let recent = recent_decisions(&bundle(), 2);
    assert_eq!(recent["status"], "no_prior_decisions");
    assert_eq!(recent["execution_authorized"], false);
    let c = crate::runtime::tests::config(std::path::Path::new("fixture"));
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-16T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let session = session_status(&c, &bundle(), "digest", &ModelRole::default(), now);
    assert_eq!(session["status"], "available");
    assert_eq!(session["execution_authorized"], false);
    assert_eq!(session["book"]["closed"], false);
    assert_eq!(session["budget"]["status"], "available");
}

#[test]
fn prompts_share_the_code_owned_objective_and_stay_short() {
    for prompt in [objective(), ORCHESTRATOR_PROMPT] {
        assert!(prompt.contains("host-provided paper mandate"));
        assert!(prompt.contains("Nifty"));
        assert!(!prompt.contains("equities/ETFs only"));
    }
    for role in ["digest", "scorer"] {
        let text = system_instructions(role, &[]).unwrap();
        assert!(
            text.starts_with(objective()),
            "objective missing for {role}"
        );
        assert!(text.contains(instructions(role).unwrap()));
    }
    assert!(
        SCORER_PROMPT.len() <= 2048,
        "prompt is {} bytes",
        SCORER_PROMPT.len()
    );
    assert!(!SCORER_PROMPT.contains("universe_features"));
    assert!(!SCORER_PROMPT.contains("shortlist"));
    assert!(!SCORER_PROMPT.contains("Rs5000"));
    assert!(!SCORER_PROMPT.contains("maximize the continuing paper account"));
    assert!(DATA_OWNER_PROMPT.len() <= 4096);
    assert!(DATA_OWNER_PROMPT.contains("facts"));
    let prompts = crate::runtime::experiment_profile::PromptSet::default();
    assert_eq!(
        prompts.hashes()["digest"],
        json!(digest(DATA_OWNER_PROMPT.as_bytes()))
    );
    assert!(prompts.hashes().get("decide").is_none());

    let owner_cfg = PlannerConfig {
        ..Default::default()
    };
    let owner_frame = Frame {
        role: "digest".into(),
        input: vec![],
        tools: vec![],
        citation_ids: vec![],
        final_answer_only: false,
        review_only: false,
        owner_tools: Default::default(),
        decision_evidence: true,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let owner_instructions = frame_instructions_for(&owner_cfg, &owner_frame).unwrap();
    assert!(owner_instructions.contains(DATA_OWNER_PROMPT));
    // The owner digest carries its own evidence rules; no handoff fragment is
    // appended, with or without a policy.
    assert_eq!(
        frame_instructions(&owner_frame).unwrap(),
        owner_instructions
    );

    let frame = Frame {
        role: "supervisor".into(),
        input: vec![],
        tools: vec![],
        citation_ids: vec![],
        final_answer_only: false,
        review_only: false,
        owner_tools: Default::default(),
        decision_evidence: false,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let default_cfg = PlannerConfig::default();
    let default_instructions = frame_instructions_for(&default_cfg, &frame).unwrap();
    assert_eq!(
        default_instructions,
        format!("{}\n\n{}", objective(), ORCHESTRATOR_PROMPT)
    );
    assert!(
        system_instructions("supervisor", &[])
            .unwrap()
            .contains(SUPERVISOR_PROMPT)
    );

    let prompt_dir = tempfile::tempdir().unwrap();
    let system_prompt = prompt_dir.path().join("orchestrator-system.md");
    let custom_text = "Persistent orchestrator standing instructions.";
    fs::write(&system_prompt, custom_text).unwrap();
    let orchestrator = super::super::orchestrator_model::OrchestratorModelSettings {
        system_prompt: Some(system_prompt),
        ..Default::default()
    };
    let configured = PlannerConfig {
        orchestrator: Some(orchestrator),
        ..Default::default()
    };
    let sent_instructions = frame_instructions_for(&configured, &frame).unwrap();
    assert_eq!(
        sent_instructions,
        format!("{}\n\n{custom_text}", objective())
    );
    let stored_path = store_prompt(prompt_dir.path(), &sent_instructions).unwrap();
    let stored = fs::read(prompt_dir.path().join(stored_path)).unwrap();
    assert_eq!(stored, sent_instructions.as_bytes());
    assert!(!String::from_utf8_lossy(&stored).contains("api_key"));

    let secret_dir = tempfile::tempdir().unwrap();
    assert_eq!(
        store_prompt(secret_dir.path(), "api_key=secret-fixture")
            .unwrap_err()
            .to_string(),
        "dashboard_prompt_secret_refused"
    );
    assert!(!secret_dir.path().join("prompts").exists());
}

#[test]
fn final_answer_frames_use_the_roles_own_prompt() {
    let frame = |role: &str| Frame {
        role: role.into(),
        input: vec![],
        tools: vec![],
        citation_ids: vec![],
        final_answer_only: true,
        review_only: false,
        owner_tools: Default::default(),
        decision_evidence: false,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let digest = frame_instructions(&frame("digest")).unwrap();
    assert!(digest.contains(DATA_OWNER_PROMPT));
    assert!(digest.starts_with(objective()));
    assert!(frame_instructions(&frame("decide")).is_err());
    let mut configured = frame("scorer");
    configured.instructions = Some("host configured scorer prompt".into());
    assert_eq!(
        frame_instructions(&configured).unwrap(),
        "host configured scorer prompt"
    );
}

#[test]
fn prompt_tool_lists_match_the_configured_definitions() {
    let settings = ToolsSettings::default();
    let digest = configured_definitions_with("digest", &[], false, &settings).unwrap();
    for tool in &digest {
        let name = tool["name"].as_str().unwrap();
        assert!(DATA_OWNER_PROMPT.contains(name), "Luna prompt omits {name}");
    }
    assert!(SCORER_PROMPT.contains("sandbox_write"));
    assert!(SCORER_PROMPT.contains("replay"));
}

#[test]
fn successful_call_result_is_archived_without_authority() {
    struct Fake {
        responses: VecDeque<Value>,
    }
    impl Backend for Fake {
        fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
            Ok(self
                .responses
                .pop_front()
                .ok_or("fixture_exhausted")?
                .to_string())
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let cfg = PlannerConfig::default();
    let role = ModelRole {
        enabled: true,
        model_id: crate::model::LUNA.into(),
        transport: sadr_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 8,
        ..Default::default()
    };
    let call = |id: &str, name: &str, arguments: Value| json!({"output":[{"type":"function_call","call_id":id,"name":name,"arguments":arguments.to_string()}]});
    let digest = json!({"complete":true,"as_of_ms":0,"coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},"gaps":[],"changed_prospects":[],"active_rankings":[],"candidate_rankings":[],"bar_context":[],"spread_depth":[],"holdings":[],"cash":{"available":"0"},"fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},"recent_actions":[],"experiments":[],"evidence_ids":["call_scan","call_rank","call_portfolio","call_history","call_status"]});
    let final_turn = json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":digest.to_string()}]}]});
    let mut fake = Fake {
        responses: VecDeque::from([
            call("call_scan", "get_scanner_prospects", json!({"limit":1})),
            call("call_rank", "rank_universe", json!({})),
            call("call_portfolio", "get_portfolio", json!({})),
            call("call_history", "get_recent_decisions", json!({})),
            call("call_status", "get_session_status", json!({})),
            final_turn,
        ]),
    };
    let result = run(
        &c,
        &cfg,
        "digest",
        &role,
        "fixture trigger",
        &bundle(),
        &mut fake,
        &planner::preview_guard(),
    )
    .unwrap();
    validate_owner_digest(&result, &bundle()).unwrap();
    let archive = std::fs::read_dir(c.directory.join("model"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("harness-digest-"))
        })
        .unwrap();
    let archived = std::fs::read_dir(&archive)
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("tool-") && name.ends_with(".result.json"))
        })
        .collect::<Vec<_>>();
    assert_eq!(archived.len(), 5, "one archive per call-time tool");
    for path in archived {
        let body: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(body["execution_authorized"], false);
        assert!(body["status"].is_string());
    }
}
