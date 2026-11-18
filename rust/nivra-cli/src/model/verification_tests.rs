use super::*;
#[test]
fn provider_diagnostics_are_bounded_and_redacted() {
    // SSE wire volume can exceed 1 MB while its single terminal snapshot
    // stays small. Delta traffic remains observable, not authority.
    use nivra_cli::native_transport::read_response_body;
    let terminal = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"model\":\"fixture\",\"output\":[]}}\r\n\r\n";
    let delta = format!(
        "data: {}\r\n\r\n",
        json!({"type":"response.output_text.delta","delta":"😀".repeat(250)})
    );
    let raw = format!("{}{terminal}data: [DONE]\r\n\r\n", delta.repeat(1200));
    assert!(raw.len() > 1_000_000);
    let mut observed = 0;
    let retained = read_response_body(
        &mut std::io::Cursor::new(raw.as_bytes()),
        1_000_000,
        true,
        Some(&mut |bytes| observed += bytes.len()),
    )
    .unwrap();
    assert_eq!(observed, raw.len());
    assert!(retained.len() < 1000);
    assert_eq!(
        subscription_response(200, &retained, "fixture-token").unwrap()["model"],
        "fixture"
    );
    let collect = |raw: &str| {
        read_response_body(
            &mut std::io::Cursor::new(raw.as_bytes()),
            1_000_000,
            true,
            None,
        )
    };
    assert_eq!(
        collect(&format!("{terminal}{terminal}")).unwrap_err(),
        "model_stream_invalid"
    );
    assert_eq!(
        collect(&format!("{terminal}data: {{")).unwrap_err(),
        "model_stream_incomplete"
    );
    assert_eq!(
        collect(&format!(
            "{terminal}data: {{\"type\":\"delta\",\"type\":\"delta\"}}\n\n"
        ))
        .unwrap_err(),
        "model_stream_invalid"
    );
    let failed = collect(&format!(
        "{terminal}data: {{\"type\":\"response.incomplete\"}}\n\n"
    ))
    .unwrap();
    assert_eq!(
        subscription_response(200, &failed, "fixture-token")
            .unwrap_err()
            .to_string(),
        "model_response_incomplete"
    );
    // The provider-reported incomplete cause and token counts are retained as
    // bounded evidence; unknown cause text is replaced, never echoed (#224).
    let incomplete_event = collect(&format!(
        "data: {}\n\n",
        json!({"type":"response.incomplete","response":{
            "status":"incomplete",
            "incomplete_details":{"reason":"max_output_tokens"},
            "usage":{"input_tokens":1_105_225,"output_tokens":0}
        }})
    ))
    .unwrap();
    let mut incomplete_evidence = json!({});
    assert_eq!(
        subscription_response_with_evidence(
            200,
            &incomplete_event,
            "fixture-token",
            &mut incomplete_evidence
        )
        .unwrap_err()
        .to_string(),
        "model_response_incomplete"
    );
    assert_eq!(
        incomplete_evidence["incomplete_reason"],
        "max_output_tokens"
    );
    assert_eq!(incomplete_evidence["tokens_in"], 1_105_225);
    assert_eq!(incomplete_evidence["tokens_out"], 0);
    // A completed response's usage is recorded too (#241).
    let mut completed_evidence = json!({});
    record_usage(
        &json!({"status":"completed","usage":{"input_tokens":61_000,"output_tokens":900}}),
        &mut completed_evidence,
    );
    assert_eq!(completed_evidence["tokens_in"], 61_000);
    assert_eq!(completed_evidence["tokens_out"], 900);
    assert!(completed_evidence.get("incomplete_reason").is_none());
    super::fallback_tests::fallback_contract();
    let mut filtered_evidence = json!({});
    let filtered = json!({"status":"incomplete",
        "incomplete_details":{"reason":"content_filter"}});
    assert_eq!(
        subscription_response_with_evidence(
            200,
            filtered.to_string().as_bytes(),
            "fixture-token",
            &mut filtered_evidence
        )
        .unwrap_err()
        .to_string(),
        "model_response_incomplete"
    );
    assert_eq!(filtered_evidence["incomplete_reason"], "content_filter");
    let mut unknown_evidence = json!({});
    let unknown = json!({"status":"incomplete",
        "incomplete_details":{"reason":"provider secret detail"}});
    assert_eq!(
        subscription_response_with_evidence(
            200,
            unknown.to_string().as_bytes(),
            "fixture-token",
            &mut unknown_evidence
        )
        .unwrap_err()
        .to_string(),
        "model_response_incomplete"
    );
    assert_eq!(unknown_evidence["incomplete_reason"], "other");
    assert!(
        !unknown_evidence
            .to_string()
            .contains("provider secret detail")
    );
    let json_snapshot = r#"{"status":"completed","model":"fixture","output":[]}"#;
    assert_eq!(collect(json_snapshot).unwrap(), json_snapshot.as_bytes());
    assert_eq!(
        collect(&format!("{{\"padding\":\"{}\"}}", "x".repeat(1_000_000))).unwrap_err(),
        "model_body_too_large"
    );
    assert_eq!(
        collect(&format!(
            "data: {}\n\n",
            json!({"type":"delta","padding":"x".repeat(1_000_000)})
        ))
        .unwrap_err(),
        "model_body_too_large"
    );
    assert_eq!(
        collect(&delta.repeat(8000)).unwrap_err(),
        "model_body_too_large"
    );
    // Byte-fragmented SSE and secrets split across provider text deltas.
    for split in 1.."fixture-secret".len() {
        let id = format!("stream-fixture-{split}");
        let captured = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let sink = captured.clone();
        let mut decoder = nivra_cli::model_stream::Decoder::new(
            "digest",
            LUNA,
            &id,
            vec!["fixture-secret".into()],
        )
        .with_observer(Box::new(move |kind, text| {
            if kind == "text_delta" {
                sink.lock().unwrap().push_str(text);
            }
        }));
        for text in [
            &"fixture-secret"[..split],
            &"fixture-secret"[split..],
            " public 😀",
        ] {
            let wire = format!(
                "data: {}\r\n\r\n",
                json!({"type":"response.output_text.delta","delta":text})
            );
            for byte in wire.as_bytes() {
                decoder.bytes(&[*byte]);
            }
        }
        decoder.bytes(b"data: {\"type\":\"response.reasoning_text.delta\",\"delta\":\"private-reasoning\"}\n\n");
        decoder.finish();
        let text = captured.lock().unwrap();
        assert_eq!(*text, "[redacted] public 😀");
        assert!(!text.contains("private-reasoning"));
    }
    for _ in 0..10_000 {
        nivra_cli::model_stream::publish("digest", LUNA, "flood-fixture", "text_delta", "bounded");
    }
    // The process-global ring is shared with parallel tests; a contended
    // try_lock snapshot is an unavailable placeholder, not a test failure.
    let settled = |after| {
        for _ in 0..10_000 {
            let snapshot = nivra_cli::model_stream::snapshot(after);
            if snapshot["unavailable"] != true {
                return snapshot;
            }
            std::thread::yield_now();
        }
        panic!("model stream remained unavailable");
    };
    let feed = settled(1);
    assert!(feed["events"].as_array().unwrap().len() <= 128);
    assert_eq!(feed["gap"], true);
    assert!(feed["dropped"].as_u64().unwrap() > 0);
    assert_eq!(settled(u64::MAX)["gap"], true);
    let role = crate::runtime::planner::ModelRole {
        model_id: "grok-4.7".into(),
        transport: Transport::DirectOauth,
        ..Default::default()
    };
    let frame = crate::runtime::harness::Frame {
        role: "digest".into(),
        input: vec![json!({"role":"user","content":"synthetic"})],
        tools: crate::runtime::harness::definitions("digest", &role.tools).unwrap(),
        citation_ids: vec![],
        final_answer_only: false,
        review_only: false,
        owner_tools: Default::default(),
        decision_evidence: false,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let mut provenance = Value::Null;
    let error = planner_response(
        Path::new("/nonexistent-nivra-model-config"),
        &role,
        &serde_json::to_string(&frame).unwrap(),
        &mut provenance,
    )
    .unwrap_err();
    assert_eq!(
        error, "model_config_unreadable",
        "stage: {}",
        provenance["stage"]
    );
    let context_frame = crate::runtime::harness::Frame {
        decision_evidence: true,
        max_instrument_keys: Some(1),
        tools: crate::runtime::harness::configured_definitions("digest", &role.tools, true, 1)
            .unwrap(),
        ..serde_json::from_value(serde_json::to_value(&frame).unwrap()).unwrap()
    };
    assert_eq!(
        planner_response(
            Path::new("/nonexistent-nivra-model-config"),
            &role,
            &serde_json::to_string(&context_frame).unwrap(),
            &mut provenance
        )
        .unwrap_err(),
        "model_config_unreadable"
    );
    let final_only = crate::runtime::harness::Frame {
        role: "digest".into(),
        input: frame.input.clone(),
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
    let error = planner_response(
        Path::new("/nonexistent-nivra-model-config"),
        &role,
        &serde_json::to_string(&final_only).unwrap(),
        &mut provenance,
    )
    .unwrap_err();
    assert_eq!(
        error, "harness_final_tools_invalid",
        "only a sealed review may be final-only, stage: {}",
        provenance["stage"]
    );
    assert_eq!(provenance["stage"], "tool_definitions");
    use nivra_cli::native_transport::{
        DIRECT_MODELS, DIRECT_RESPONSES, SUBSCRIPTION_RESPONSES, Transport, response_body_limit,
        response_headers,
    };
    assert_eq!(Transport::default(), Transport::SubscriptionProxy);
    assert_eq!(
        serde_json::from_str::<Transport>("\"direct_oauth\"").unwrap(),
        Transport::DirectOauth
    );
    assert!(serde_json::from_str::<Transport>("\"unknown\"").is_err());
    assert_eq!(Transport::DirectOauth.models_url(), DIRECT_MODELS);
    assert_eq!(Transport::DirectOauth.responses_url(), DIRECT_RESPONSES);
    let direct = vec![
        ("Accept", "text/event-stream"),
        ("Accept-Encoding", "identity"),
    ];
    assert_eq!(
        response_headers(Transport::DirectOauth, "grok-4.7", None).unwrap(),
        direct
    );
    assert_eq!(
        response_headers(Transport::DirectOauth, "grok-4.7", Some("invalid")).unwrap(),
        direct
    );
    assert!(response_headers(Transport::SubscriptionProxy, "grok-4.7", None).is_err());
    assert_eq!(response_body_limit(DIRECT_RESPONSES), 1_000_000);
    assert_eq!(response_body_limit(SUBSCRIPTION_RESPONSES), 1_000_000);
    assert_eq!(response_body_limit(DIRECT_MODELS), 262144);
    // A body read that hits the global deadline is a timeout, not invalid
    // decoding; an oversized body keeps its own bounded code (A0/#162).
    assert_eq!(
        nivra_cli::native_transport::body_read_error(&std::io::Error::from(
            std::io::ErrorKind::TimedOut
        )),
        "model_timeout"
    );
    assert_eq!(
        nivra_cli::native_transport::body_read_error(&std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        )),
        "model_body_invalid"
    );
    // The read starts before the request deadline, then expires inside IO.
    // Simulate the configured clock without sockets or wall-clock sleeps.
    struct DeadlineRead(std::io::ErrorKind);
    impl std::io::Read for DeadlineRead {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(self.0.into())
        }
    }
    for kind in [
        std::io::ErrorKind::Other,
        std::io::ErrorKind::UnexpectedEof,
        std::io::ErrorKind::ConnectionReset,
    ] {
        let mut checks = 0;
        assert_eq!(
            nivra_cli::native_transport::read_response_body_until(
                &mut DeadlineRead(kind),
                1000,
                false,
                None,
                || {
                    checks += 1;
                    checks > 1
                },
            ),
            Err("model_timeout")
        );
        assert_eq!(
            nivra_cli::native_transport::read_response_body_until(
                &mut DeadlineRead(kind),
                1000,
                false,
                None,
                || false,
            ),
            Err("model_body_invalid")
        );
    }
    assert_eq!(
        nivra_cli::native_transport::read_response_body_until(
            &mut std::io::Cursor::new(Vec::<u8>::new()),
            1000,
            false,
            None,
            || true,
        ),
        Err("model_timeout")
    );
    assert_eq!(
        nivra_cli::native_transport::body_read_error(
            &ureq::Error::Timeout(ureq::Timeout::Global).into_io()
        ),
        "model_timeout"
    );
    assert!(admit_model(FAST_VARIANT, &[]).is_err());
    assert!(admit_model("grok-4.7", &[]).is_err());
    assert!(admit_model("grok-4.7", &["grok-4.7".into()]).is_ok());
    assert!(admit_model(FAST_VARIANT, &[FAST_VARIANT.into()]).is_err());
    let fixture = tempfile::tempdir_in("/var/tmp").unwrap();
    let stream_file = fixture.path().join("model-stream.json");
    let streamed = {
        let _scope = nivra_cli::model_stream::Scope::new(
            stream_file.clone(),
            "synthetic-binding".into(),
            None,
        );
        // Telemetry deliberately drops on lock contention. Other parallel
        // fixtures publish too; exercise the persisted projection without
        // treating a permitted dropped display event as a provider failure.
        (0..100)
            .find_map(|_| {
                nivra_cli::model_stream::publish(
                    "digest",
                    LUNA,
                    "file-fixture",
                    "host_validated",
                    "Synthetic valid output",
                );
                let snapshot =
                    nivra_cli::model_stream::read_snapshot(&stream_file, "synthetic-binding", 0)
                        .ok()
                        .filter(|v| {
                            v["events"].as_array().is_some_and(|events| {
                                events.iter().any(|e| e["text"] == "Synthetic valid output")
                            })
                        });
                if snapshot.is_none() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                snapshot
            })
            .expect("bounded telemetry fixture publication")
    };
    assert!(
        streamed["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["text"] == "Synthetic valid output")
    );
    assert!(nivra_cli::model_stream::read_snapshot(&stream_file, "other-binding", 0).is_err());
    let secret = "synthetic-access-credential-only";
    let error = json!({"error":{"code":"permission_denied","message":format!("Missing scope; Bearer {secret} refresh_token: short-secret xai-fixturekey eyJfixture.payload.sig {}", "界 ".repeat(500))},"access_token":"must-not-appear"});
    let error = check_http(403, &serde_json::to_vec(&error).unwrap(), secret).unwrap_err();
    assert!(error.starts_with("model_http_403 permission_denied Missing scope;"));
    for forbidden in [
        secret,
        "short-secret",
        "xai-fixturekey",
        "eyJfixture",
        "must-not-appear",
    ] {
        assert!(!error.contains(forbidden));
    }
    assert!(
        error
            .strip_prefix("model_http_403 ")
            .unwrap()
            .chars()
            .count()
            <= 300
    );
    assert_eq!(
        check_http(502, b"<html>private upstream</html>", secret).unwrap_err(),
        "model_http_502"
    );
    assert_eq!(
        check_http(401, b"not json", secret).unwrap_err(),
        "model_http_401"
    );
    assert!(check_http(201, b"", secret).is_ok());
    assert!(!safe_text("hello\u{1b}[31m world", secret, 80).contains('\u{1b}'));
    use nivra_cli::native_transport::context_headers;
    let context = "d01fb75e-9238-4b1a-88a4-11e2ca85c50e";
    assert!(
        context_headers(Transport::DirectOauth, None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        context_headers(Transport::DirectOauth, Some(context)).unwrap(),
        vec![("x-grok-conv-id", context)]
    );
    assert_eq!(
        context_headers(Transport::SubscriptionProxy, Some(context)).unwrap(),
        vec![("x-grok-conv-id", context), ("x-grok-session-id", context)]
    );
    for invalid in [
        "short",
        "abcdefghijklmnop/",
        "abcdefghijklmnop\n",
        "abcdefghijklmnop界",
        &"a".repeat(65),
    ] {
        assert!(context_headers(Transport::DirectOauth, Some(invalid)).is_err());
    }
    let snapshot =
        json!({"model":"provider-echo","status":"completed","output":[{"type":"web_search_call"}]});
    let event = json!({"type":"response.completed","response":snapshot});
    let sse = format!(
        "event: response.created\r\ndata: {{\"type\":\"response.created\"}}\r\n\r\nevent: response.completed\r\ndata: {event}\r\n\r\ndata: [DONE]\r\n\r\n"
    );
    let streamed = subscription_response(200, sse.as_bytes(), secret).unwrap();
    assert_eq!(streamed["output"][0]["type"], "web_search_call");
    for broken in [
        "data: [DONE]\n\n".to_owned(),
        "data: {bad json}\n\n".to_owned(),
        format!("data: {event}"),
        format!("data: {event}\n\ndata: {event}\n\n"),
        "data: {\"type\":\"response.incomplete\"}\n\n".to_owned(),
    ] {
        assert!(subscription_response(200, broken.as_bytes(), secret).is_err());
    }
    let failed = format!(
        "data: {}\n\n",
        json!({"type":"response.failed","response":{"error":{"message":format!("Denied {secret}")}}})
    );
    let error = subscription_response(200, failed.as_bytes(), secret).unwrap_err();
    assert!(error.starts_with("model_http_200 Denied"));
    assert!(!error.contains(secret));
    assert_eq!(
        subscription_catalog(br#"[{"model":"grok-4.7-build-fast"},{"id":"grok-4.7"}]"#).unwrap(),
        vec!["grok-4.7", "grok-4.7-build-fast"]
    );
    assert!(subscription_catalog(br#"{"data":[{"id":"duplicate"},{"id":"duplicate"}]}"#).is_err());
    assert_eq!(
        subscription_catalog(br#"{"models":[{"slug":"gpt-6-luna"},{"slug":"gpt-6.1-sol"}]}"#)
            .unwrap(),
        vec!["gpt-6-luna", "gpt-6.1-sol"]
    );
    assert!(subscription_catalog(br#"{"models":[]}"#).is_err());
    let account = "acct-0123456789abcdef";
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"https://api.openai.com/auth":{"chatgpt_account_id":account,"chatgpt_data_residency":"us"}})).unwrap(),
        );
    let jwt = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
    let identity = nivra_cli::native_transport::codex_identity(&jwt).unwrap();
    assert_eq!(identity.account_id, account);
    assert_eq!(identity.residency.as_deref(), Some("us"));
    assert!(nivra_cli::native_transport::codex_identity("not-a-jwt").is_err());
    let codex_headers = response_headers(Transport::OpenaiCodex, LUNA, None).unwrap();
    assert!(codex_headers.contains(&("originator", "nivra")));
    assert!(
        !codex_headers
            .iter()
            .any(|(name, _)| *name == "x-grok-model-override")
    );
    assert_eq!(
        Transport::OpenaiCodex.responses_url(),
        nivra_cli::native_transport::CODEX_RESPONSES
    );
    let codex_at = chrono::DateTime::parse_from_rfc3339("2026-10-18T00:00:00Z").unwrap();
    let envelope = json!({"token_type":"bearer","access_token":jwt,"refresh_token":"refresh-token","expires_in":3600,"scope":"openid profile email offline_access"});
    assert!(
        nivra_core::token_envelope::validate_codex(
            &serde_json::to_vec(&envelope).unwrap(),
            codex_at
        )
        .is_ok()
    );
    let mut ten_day = envelope.clone();
    ten_day["expires_in"] = json!(864_000);
    let accepted = nivra_core::token_envelope::validate_codex(
        &serde_json::to_vec(&ten_day).unwrap(),
        codex_at,
    )
    .unwrap();
    assert_eq!(
        accepted.expires_at,
        codex_at + chrono::Duration::seconds(864_000)
    );
    ten_day["expires_in"] = json!(864_001);
    assert!(
        nivra_core::token_envelope::validate_codex(
            &serde_json::to_vec(&ten_day).unwrap(),
            codex_at
        )
        .is_err()
    );
    assert!(
        nivra_core::token_envelope::validate(&serde_json::to_vec(&envelope).unwrap(), codex_at)
            .is_err()
    );
    let challenge = nivra_core::device_challenge::DeviceChallenge::parse(
            br#"{"verification_uri":"https://auth.openai.com/codex/device","user_code":"ABCD-EFGH","expires_in":900}"#,
        )
        .unwrap();
    assert_eq!(
        challenge.verification_uri(),
        "https://auth.openai.com/codex/device"
    );
    let openai_grant = json!({"version":"ind-sadr-openai-grant-v1","issuer":"https://auth.openai.com","client_id":nivra_core::openai_grant::CLIENT,"scope":"openai-codex-device","state":"authenticated","grant_id":"a".repeat(32),"revision":1,"login_attempts":1,"updated_at":codex_at,"expires_at":codex_at+chrono::Duration::hours(1),"account_hash":"b".repeat(64),"access_token":jwt,"refresh_token":"refresh-token"});
    let openai_bytes = serde_json::to_vec(&openai_grant).unwrap();
    let parsed = nivra_core::openai_grant::GrantRecord::parse(&openai_bytes).unwrap();
    assert_eq!(parsed.status(codex_at).state, "authenticated");
    assert_eq!(parsed.status(codex_at).provider, "openai_codex");
    assert_eq!(parsed.updated_at(), codex_at);
    assert!(!format!("{parsed:?}").contains(&jwt));
    assert!(GrantRecord::parse(&openai_bytes).is_err());
    let separated = tempfile::tempdir_in("/var/tmp").unwrap();
    let openai_path = separated.path().join("openai-grant");
    nivra_store::native_grant::create_openai_file(&openai_path, &openai_bytes).unwrap();
    assert!(nivra_store::native_grant::read_file(&openai_path).is_err());
    assert_eq!(
        nivra_store::native_grant::read_openai_file(&openai_path).unwrap(),
        openai_bytes
    );
    let openai_config = separated.path().join("openai-model.json");
    std::fs::write(
        &openai_config,
        serde_json::to_vec(&json!({"transport":"openai_codex"})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        config(&openai_config).err().as_deref(),
        Some("openai_grant_path_required")
    );
    assert_eq!(
        nivra_cli::native_transport::subscription_headers("fixture-model", Some("1.0.13")).unwrap(),
        [
            ("X-XAI-Token-Auth", "xai-grok-cli"),
            ("x-grok-client-version", "1.0.13"),
            ("x-grok-model-override", "fixture-model"),
        ]
    );
    assert_eq!(
        nivra_cli::native_transport::subscription_headers("fixture-model", None).unwrap_err(),
        "model_proxy_client_version_missing"
    );
    for invalid in [
        "",
        "1",
        ".1",
        "1.",
        "1..13",
        "1.0.13-beta",
        "1. 13",
        "１.0",
        &"1.".repeat(17),
    ] {
        assert_eq!(
            nivra_cli::native_transport::subscription_headers("fixture-model", Some(invalid))
                .unwrap_err(),
            "model_proxy_client_version_invalid"
        );
    }
    assert!(serde_json::from_value::<Config>(json!({"conversation_id":context})).is_err());
    assert!(serde_json::from_value::<Config>(json!({"proxy_client_version":13})).is_err());
    // Application-owned fixtures only: exclusive ownership, rotation, account
    // binding, omitted refresh preservation and durable ambiguous-failure fence.
    use nivra_store::native_grant::{FileOwner, create_file};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let directory = tempfile::tempdir_in("/var/tmp").unwrap();
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-17T00:00:00Z").unwrap();
    let original = json!({"state":"authenticated","grant_id":"a".repeat(32),"revision":1,"login_attempts":1,"updated_at":at,"expires_at":at,"account_hash":hash(b"fixture-account"),"access_token":secret,"refresh_token":"fixture-refresh"});
    let discovery = json!({"issuer":"https://auth.x.ai","device_authorization_endpoint":"https://auth.x.ai/oauth2/device/code","token_endpoint":"https://auth.x.ai/oauth2/token","userinfo_endpoint":"https://auth.x.ai/oauth2/userinfo","grant_types_supported":["urn:ietf:params:oauth:grant-type:device_code","refresh_token"],"token_endpoint_auth_methods_supported":["none"],"scopes_supported":SCOPE.split_whitespace().collect::<Vec<_>>()});
    for scenario in [
        "rotate",
        "retain",
        "timeout",
        "denied",
        "wrong_account",
        "corrupt",
    ] {
        let path = directory.path().join(scenario);
        create_file(&path, &serde_json::to_vec(&original).unwrap()).unwrap();
        let owner = FileOwner::acquire(&path).unwrap();
        assert!(FileOwner::acquire(&path).is_err());
        let mut calls = 0;
        let result = grant_with(&owner, at, |url, form, body, token| {
            calls += 1;
            assert!(body.is_none());
            assert_eq!(
                native::decode(&owner.read().unwrap()).unwrap()["state"],
                "refreshing"
            );
            match calls {
                1 => {
                    assert_eq!(url, "https://auth.x.ai/.well-known/openid-configuration");
                    assert!(form.is_none() && token.is_none());
                    Ok((200, serde_json::to_vec(&discovery).unwrap()))
                }
                2 => {
                    assert_eq!(url, "https://auth.x.ai/oauth2/token");
                    assert_eq!(
                        form.unwrap(),
                        [
                            ("grant_type", "refresh_token"),
                            ("client_id", CLIENT),
                            ("refresh_token", "fixture-refresh")
                        ]
                    );
                    assert!(token.is_none());
                    if scenario == "timeout" {
                        return Err("model_timeout".into());
                    }
                    if scenario == "denied" {
                        return Ok((
                            401,
                            br#"{"error":"invalid_grant","error_description":"fixture-refresh"}"#
                                .to_vec(),
                        ));
                    }
                    if scenario == "corrupt" {
                        return Ok((200, vec![0xff]));
                    }
                    let mut tokens = json!({"token_type":"Bearer","access_token":"fixture-rotated-access","expires_in":3600});
                    if scenario != "retain" {
                        tokens["refresh_token"] = json!("fixture-rotated-refresh");
                    }
                    Ok((200, serde_json::to_vec(&tokens).unwrap()))
                }
                3 => {
                    assert_eq!(url, "https://auth.x.ai/oauth2/userinfo");
                    assert_eq!(token, Some("fixture-rotated-access"));
                    assert!(form.is_none());
                    Ok((200, serde_json::to_vec(&json!({"sub":if scenario == "wrong_account" { "other-account" } else { "fixture-account" }})).unwrap()))
                }
                _ => panic!("unexpected provider request"),
            }
        });
        let stored = native::decode(&owner.read().unwrap()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(stored["grant_id"], original["grant_id"]);
        assert_eq!(stored["account_hash"], original["account_hash"]);
        assert_eq!(stored["revision"], 2);
        assert_eq!(stored["refresh_attempts"], 1);
        if ["rotate", "retain"].contains(&scenario) {
            assert!(result.is_ok());
            assert_eq!(stored["state"], "authenticated");
            assert_eq!(stored["access_token"], "fixture-rotated-access");
            assert_eq!(
                stored["refresh_token"],
                if scenario == "retain" {
                    "fixture-refresh"
                } else {
                    "fixture-rotated-refresh"
                }
            );
            assert_eq!(
                stored["expires_at"],
                json!(at + chrono::Duration::seconds(3600))
            );
            assert!(
                grant_with(&owner, at, |_, _, _, _| panic!(
                    "cached grant must not refresh"
                ))
                .is_ok()
            );
        } else {
            let error = result.unwrap_err();
            assert!(!error.contains("fixture-refresh"));
            assert_eq!(stored["state"], "refreshing");
            assert_eq!(stored["access_token"], original["access_token"]);
            assert!(
                grant_with(&owner, at, |_, _, _, _| panic!(
                    "ambiguous refresh must not repeat"
                ))
                .is_err()
            );
        }
        assert!(
            owner
                .replace(b"wrong original", &serde_json::to_vec(&original).unwrap())
                .is_err()
        );
        drop(owner);
        assert!(FileOwner::acquire(&path).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(nivra_store::native_grant::read_file(&path).is_err());
    }
    // Refresh expiry follows expires_in, then access JWT exp, never id_token.
    use base64::Engine;
    let jwt = format!(
        "fixture.{}.sig",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::to_vec(&json!({"exp":(at + chrono::Duration::seconds(900)).timestamp()}))
                .unwrap()
        )
    );
    let tokens = json!({"access_token":jwt,"id_token":"ignored-session-token"});
    let accept = |bytes: &[u8]| GrantRecord::parse(bytes).map(|_| ()).map_err(|_| ());
    let rotated = refresh_rotation(
        &original,
        refresh_pending(&original, at).unwrap(),
        &serde_json::to_vec(&tokens).unwrap(),
        at,
        accept,
    )
    .unwrap();
    assert_eq!(
        rotated["expires_at"],
        json!(at + chrono::Duration::seconds(900))
    );
    assert!(
        refresh_rotation(
            &original,
            refresh_pending(&original, at).unwrap(),
            br#"{"access_token":"opaque","id_token":"ignored-session-token"}"#,
            at,
            |bytes| GrantRecord::parse(bytes).map(|_| ()).map_err(|_| ())
        )
        .is_err()
    );
    let mut tokens = tokens;
    tokens["expires_in"] = json!(1800);
    assert_eq!(
        refresh_rotation(
            &original,
            refresh_pending(&original, at).unwrap(),
            &serde_json::to_vec(&tokens).unwrap(),
            at,
            |bytes| GrantRecord::parse(bytes).map(|_| ()).map_err(|_| ())
        )
        .unwrap()["expires_at"],
        json!(at + chrono::Duration::seconds(1800))
    );
    let now = chrono::DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(parse_retry_after("3", now), Some(Duration::from_secs(3)));
    assert_eq!(
        parse_retry_after("Thu, 05 Nov 2026 00:00:05 GMT", now),
        Some(Duration::from_secs(5))
    );
    assert_eq!(model_429_backoff(None, 0, now), Duration::from_millis(250));
    assert_eq!(model_429_backoff(None, 1, now), Duration::from_millis(500));
    assert_eq!(model_429_backoff(Some("7"), 0, now), Duration::from_secs(7));
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::RefCell::new(Vec::new());
    let retried = retry_model_429(
        Instant::now() + Duration::from_secs(10),
        || {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            Ok(nivra_cli::native_transport::HttpResponse {
                status: if attempt == 0 { 429 } else { 200 },
                body: Vec::new(),
                retry_after: (attempt == 0).then(|| "2".to_owned()),
            })
        },
        |delay| delays.borrow_mut().push(delay),
    )
    .unwrap();
    assert_eq!(retried.status, 200);
    assert_eq!(attempts.get(), 2);
    assert_eq!(&*delays.borrow(), &[Duration::from_secs(2)]);
    let attempts = std::cell::Cell::new(0);
    let delayed = retry_model_429(
        Instant::now() + Duration::from_millis(100),
        || {
            attempts.set(attempts.get() + 1);
            Ok(nivra_cli::native_transport::HttpResponse {
                status: 429,
                body: Vec::new(),
                retry_after: Some("1".into()),
            })
        },
        |_| panic!("do not sleep past the original deadline"),
    )
    .unwrap();
    assert_eq!(delayed.status, 429);
    assert_eq!(attempts.get(), 1);
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::Cell::new(0);
    let exhausted = retry_model_429(
        Instant::now() + Duration::from_secs(10),
        || {
            attempts.set(attempts.get() + 1);
            Ok(nivra_cli::native_transport::HttpResponse {
                status: 429,
                body: Vec::new(),
                retry_after: None,
            })
        },
        |_| delays.set(delays.get() + 1),
    )
    .unwrap();
    assert_eq!(exhausted.status, 429);
    assert_eq!(attempts.get(), MODEL_429_MAX_RETRIES + 1);
    assert_eq!(delays.get(), MODEL_429_MAX_RETRIES);
    let attempts = std::cell::Cell::new(0);
    let delays = std::cell::RefCell::new(Vec::new());
    let retried_body = retry_model_429(
        Instant::now() + Duration::from_secs(10),
        || {
            let attempt = attempts.get();
            attempts.set(attempt + 1);
            if attempt == 0 {
                Err("model_body_invalid".to_owned())
            } else {
                Ok(nivra_cli::native_transport::HttpResponse {
                    status: 200,
                    body: Vec::new(),
                    retry_after: None,
                })
            }
        },
        |delay| delays.borrow_mut().push(delay),
    )
    .unwrap();
    assert_eq!(retried_body.status, 200);
    assert_eq!(attempts.get(), 2);
    assert_eq!(&*delays.borrow(), &[Duration::from_millis(250)]);
    let attempts = std::cell::Cell::new(0);
    let refused = retry_model_429(
        Instant::now() + Duration::from_secs(10),
        || {
            attempts.set(attempts.get() + 1);
            Err("model_http_401".to_owned())
        },
        |_| panic!("authorization failures are never retried"),
    );
    assert_eq!(refused.unwrap_err(), "model_http_401");
    assert_eq!(attempts.get(), 1);
    crate::runtime::readiness::check_boundaries();
}
