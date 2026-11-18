use super::*;
use planner::ToolsSettings;
use serde_json::json;

fn limits() -> ToolsSettings {
    ToolsSettings {
        scanner_limit: 3,
        ranking_limit: 4,
        history_limit: 2,
        max_instrument_keys: 2,
        ..ToolsSettings::default()
    }
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
            research_request: true,
            quote_capture: true,
            candidate_observation: true,
            candidate_parent_ref: true,
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
    // The strict research request sends `refs: null` instead of omitting it.
    assert!(
        checked_call_with(
            &call("request_research", json!({"question":"q","refs":null})),
            &owner,
            &settings
        )
        .is_ok(),
        "request_research must accept null refs"
    );
    assert!(
        configured_definitions_with("digest", &["web_search".into()], false, &settings).is_err()
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
