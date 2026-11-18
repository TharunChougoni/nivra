//! Limit fallback (#245, #21) contract: which failures switch, the exact chat
//! request, the reply envelope and readiness. No sockets are opened.
use super::*;
use crate::runtime::planner::{FallbackProfile, ModelRole};
use nivra_cli::native_transport::FallbackTransport;
use std::os::unix::fs::PermissionsExt;

pub(super) fn fallback_contract() {
    // Only the reviewed eligibility classes switch one call; the primary's
    // transport evidence bounds the auth and opaque-200 classes by stage.
    // Timeouts, 5xx, cancellation, malformed output from a real response, a
    // grant-refresh auth failure and local refusals never switch.
    for (error, stage, expected) in [
        // Provider limits (#245).
        (
            "model_http_429 usage_limit_reached The usage limit has been reached",
            "http_response",
            Some("usage_limit_reached"),
        ),
        ("model_http_429", "http_response", Some("rate_limited")),
        (
            "model_http_200 rate_limit_exceeded Rate limit reached",
            "response_validation",
            Some("rate_limited"),
        ),
        (
            "model_http_403 insufficient_quota",
            "http_response",
            Some("quota_exceeded"),
        ),
        // #21: an expired subscription or revoked grant at catalog/inference.
        (
            "model_http_401 invalid_api_key",
            "catalog",
            Some("entitlement_auth_failed"),
        ),
        (
            "model_http_403 permission_denied Missing scope",
            "catalog",
            Some("entitlement_auth_failed"),
        ),
        (
            "model_http_401",
            "inference",
            Some("entitlement_auth_failed"),
        ),
        (
            "model_http_401",
            "http_response",
            Some("entitlement_auth_failed"),
        ),
        // #21: an opaque HTTP 200 provider rejection at response validation.
        (
            "model_http_200 provider rejected the supervisor prompt",
            "response_validation",
            Some("response_validation_rejected"),
        ),
        // The same codes outside their bounded stage never switch.
        ("model_http_401 invalid_api_key", "grant_owner", None),
        ("model_http_401", "transport", None),
        ("model_http_200 provider rejected", "http_response", None),
        ("model_http_200", "output_validation", None),
        // 5xx, timeouts, cancellation, malformed output and local refusals.
        ("model_http_500 server_error", "http_response", None),
        ("model_http_501 not_implemented", "http_response", None),
        ("model_http_503 unavailable", "http_response", None),
        ("model_http_400 bad_request", "http_response", None),
        ("model_http_404 not_found", "catalog", None),
        ("model_timeout", "transport", None),
        ("planner_call_timeout", "transport", None),
        ("planner_cancelled_no_replay", "response_validation", None),
        ("model_response_incomplete", "response_validation", None),
        ("model_response_invalid", "response_validation", None),
        ("model_stream_incomplete", "response_validation", None),
        ("grant_unavailable", "grant_owner", None),
        ("planner_budget_exhausted rate_limit", "inference", None),
    ] {
        assert_eq!(
            fallback_reason(error, &json!({"stage":stage})),
            expected,
            "{error} at {stage}"
        );
    }
    // A missing stage never widens eligibility: both bounded classes need it.
    assert_eq!(fallback_reason("model_http_401", &json!({})), None);
    assert_eq!(fallback_reason("model_http_200", &json!(null)), None);
    // The real codex 429 body keeps its type in the bounded error text.
    let codex = http_error(
        429,
        br#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_in_seconds":4000}}"#,
        &[],
    );
    assert_eq!(
        fallback_reason(&codex, &json!({"stage":"http_response"})),
        Some("usage_limit_reached")
    );

    let profile = FallbackProfile {
        model_id: "deepseek-v4-1-flash-1017".into(),
        transport: FallbackTransport::MistralChat,
        model_config: "/srv/fallback.json".into(),
        output_cap: Some(32_000),
        reasoning_effort: Some("max".into()),
    };
    assert!(!profile.invalid());
    for broken in [
        FallbackProfile {
            model_config: "relative.json".into(),
            ..profile.clone()
        },
        FallbackProfile {
            output_cap: Some(200_000),
            ..profile.clone()
        },
        FallbackProfile {
            reasoning_effort: Some("ultra".into()),
            ..profile.clone()
        },
        FallbackProfile {
            model_id: "bad model".into(),
            ..profile.clone()
        },
    ] {
        assert!(broken.invalid());
    }
    let role = ModelRole {
        output_cap: 8000,
        ..ModelRole::default()
    };
    // A Responses tool round trip becomes system, user, one assistant message
    // with text and tool calls, and a tool reply; reasoning is dropped.
    let input = vec![
        json!({"role":"user","content":"Decide."}),
        json!({"type":"reasoning","summary":[]}),
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"Checking."}]}),
        json!({"type":"function_call","call_id":"call_a1","name":"read_evidence","arguments":"{\"ids\":[\"e1\"]}"}),
        json!({"type":"function_call","call_id":"call_b2","name":"get_session_status","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"call_a1","output":"{\"ok\":true}"}),
        json!({"type":"function_call_output","call_id":"call_b2","output":{"phase":"open"}}),
    ];
    let tools = vec![
        json!({"type":"function","name":"read_evidence","description":"Read.","parameters":{"type":"object"}}),
    ];
    let body = chat_body(&profile, &role, "Standing rules.", &input, Some(&tools)).unwrap();
    assert_eq!(body["model"], "deepseek-v4-1-flash-1017");
    assert_eq!(body["max_tokens"], 32_000);
    assert_eq!(body["reasoning_effort"], "max");
    assert_eq!(body["stream"], false);
    for absent in ["store", "max_output_tokens", "input", "instructions"] {
        assert!(body.get(absent).is_none(), "{absent}");
    }
    let messages = body["messages"].as_array().unwrap();
    let roles: Vec<_> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool", "tool"]);
    assert_eq!(messages[0]["content"], "Standing rules.");
    assert_eq!(messages[2]["content"], "Checking.");
    assert_eq!(messages[2]["tool_calls"][1]["id"], "call_b2");
    assert_eq!(
        messages[2]["tool_calls"][0]["function"]["arguments"],
        "{\"ids\":[\"e1\"]}"
    );
    assert_eq!(messages[4]["content"], "{\"phase\":\"open\"}");
    assert_eq!(body["tools"][0]["function"]["name"], "read_evidence");
    assert_eq!(body["tools"][0]["type"], "function");
    // Without a configured cap the role cap is the `max_tokens`; native
    // provider tools refuse rather than silently vanish.
    let plain = FallbackProfile {
        output_cap: None,
        reasoning_effort: None,
        ..profile.clone()
    };
    let minimal = chat_body(&plain, &role, "", &input[..1], None).unwrap();
    assert_eq!(minimal["max_tokens"], 8000);
    assert!(minimal.get("reasoning_effort").is_none());
    assert_eq!(minimal["messages"][0]["role"], "user");
    assert_eq!(
        chat_body(
            &plain,
            &role,
            "",
            &input,
            Some(&[json!({"type":"web_search"})])
        )
        .unwrap_err(),
        "fallback_tool_unsupported"
    );
    assert_eq!(
        chat_messages("", &[json!({"type":"custom_tool_call"})]).unwrap_err(),
        "fallback_input_unsupported"
    );

    // The reply maps back to the harness envelope with counted usage (#241).
    let reply = json!({"id":"x","model":"deepseek-v4-1-flash-1017","object":"chat.completion",
        "usage":{"prompt_tokens":305,"completion_tokens":50,"prompt_tokens_details":{"cached_tokens":256}},
        "choices":[{"index":0,"finish_reason":"tool_calls","message":{"role":"assistant",
        "content":"I'll check.","tool_calls":[{"id":"chatcmpl-tool-bd43","type":"function",
        "function":{"name":"read_evidence","arguments":"{\"ids\": [\"e1\"]}"}}]}}]});
    let mut evidence = json!({});
    let envelope = chat_envelope(
        200,
        reply.to_string().as_bytes(),
        "fixture-key-0123456789",
        &mut evidence,
    )
    .unwrap();
    assert_eq!(envelope["model"], "deepseek-v4-1-flash-1017");
    assert_eq!(envelope["usage"]["input_tokens"], 305);
    assert_eq!(
        envelope["usage"]["input_tokens_details"]["cached_tokens"],
        256
    );
    assert_eq!(evidence["tokens_in"], 305);
    assert_eq!(evidence["tokens_out"], 50);
    let output = envelope["output"].as_array().unwrap();
    assert_eq!(output[0]["type"], "message");
    assert_eq!(output[0]["content"][0]["type"], "output_text");
    assert_eq!(output[1]["type"], "function_call");
    assert_eq!(output[1]["call_id"], "chatcmpl-tool-bd43");
    assert_eq!(output[1]["arguments"], "{\"ids\": [\"e1\"]}");
    // Thinking parts are dropped; a final answer is exactly one message.
    let thinking = json!({"model":"m","choices":[{"finish_reason":"stop","message":{"role":"assistant",
        "content":[{"type":"thinking","thinking":[{"type":"text","text":"hmm"}]},{"type":"text","text":"{\"ok\":1}"}]}}]});
    let envelope =
        chat_envelope(200, thinking.to_string().as_bytes(), "k", &mut json!({})).unwrap();
    assert_eq!(
        crate::runtime::harness::final_text(envelope["output"].as_array().unwrap()).unwrap(),
        "{\"ok\":1}"
    );
    // A length stop is incomplete, never output; provider errors stay bounded
    // and never echo the key.
    let truncated = json!({"model":"m","usage":{"prompt_tokens":9,"completion_tokens":64},
        "choices":[{"finish_reason":"length","message":{"role":"assistant","content":"par"}}]});
    let mut truncated_evidence = json!({});
    assert_eq!(
        chat_envelope(
            200,
            truncated.to_string().as_bytes(),
            "k",
            &mut truncated_evidence
        )
        .unwrap_err(),
        "model_response_incomplete"
    );
    assert_eq!(truncated_evidence["incomplete_reason"], "max_output_tokens");
    assert_eq!(truncated_evidence["tokens_out"], 64);
    let refused = chat_envelope(
        429,
        br#"{"object":"error","message":"Requests rate limit exceeded for key sk-secret-0123456789abcdef","type":"rate_limited"}"#,
        "sk-secret-0123456789abcdef",
        &mut json!({}),
    )
    .unwrap_err();
    assert!(refused.starts_with("model_http_429"));
    assert!(!refused.contains("sk-secret"));

    // Readiness: the configured profile, its credential config and a private
    // key file; a group-readable key or a transport mismatch blocks it.
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("mistral-api-key");
    std::fs::write(&key, "fixture-mistral-key-0123456789\n").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = dir.path().join("mistral-fallback.json");
    std::fs::write(
        &config,
        json!({"transport":"mistral_chat","api_key_file":key}).to_string(),
    )
    .unwrap();
    let configured = FallbackProfile {
        model_config: config.clone(),
        ..profile.clone()
    };
    let config_bytes = std::fs::read(&configured.model_config).unwrap();
    assert!(parse_fallback_config(&config_bytes).is_ok());
    assert!(
        parse_fallback_config(
            json!({"transport":"openai_codex","api_key_file":key})
                .to_string()
                .as_bytes(),
        )
        .is_err()
    );
}

#[test]
fn fallback_eligibility_reason_codes_are_static_and_secret_free() {
    // The reviewed set is exactly these static operator-visible codes; it is
    // what evidence, logs, alerts and readiness may publish.
    assert_eq!(
        FALLBACK_REASONS,
        [
            "usage_limit_reached",
            "rate_limited",
            "quota_exceeded",
            "entitlement_auth_failed",
            "response_validation_rejected",
        ]
    );
    assert!(FALLBACK_REASONS.iter().all(|code| {
        code.bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    }));
    // A primary error carrying a synthetic credential yields only a static
    // code: provider text never becomes the reason in evidence or the alert.
    let secret = "sk-secret-0123456789abcdef";
    for (error, stage) in [
        (
            format!("model_http_401 invalid_api_key {secret}"),
            "catalog",
        ),
        (
            format!("model_http_200 rejected {secret}"),
            "response_validation",
        ),
        (
            format!("model_http_429 usage_limit_reached {secret}"),
            "http_response",
        ),
    ] {
        let reason = fallback_reason(&error, &json!({"stage":stage})).unwrap();
        assert!(FALLBACK_REASONS.contains(&reason));
        assert!(!reason.contains(secret) && !reason.contains("sk-"));
    }
}
