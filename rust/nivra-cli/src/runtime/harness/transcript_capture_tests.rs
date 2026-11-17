use super::*;
use std::collections::VecDeque;
struct Script(VecDeque<Result<Value>>);
impl Backend for Script {
    fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
        Ok(self.0.pop_front().ok_or("fixture_exhausted")??.to_string())
    }
}
fn role() -> ModelRole {
    ModelRole {
        enabled: true,
        model_id: crate::model::LUNA.into(),
        transport: nivra_cli::native_transport::Transport::OpenaiCodex,
        daily_call_budget: 8,
        ..ModelRole::default()
    }
}
fn final_response() -> Value {
    json!({"usage":{"input_tokens":20,"output_tokens":5,"input_tokens_details":{"cached_tokens":4}},"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":json!({"complete":true,"as_of_ms":0,"coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},"gaps":[],"changed_prospects":[],"active_rankings":[],"candidate_rankings":[],"bar_context":[],"spread_depth":[],"holdings":[],"cash":{"available":"0"},"fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},"recent_actions":[],"experiments":[],"evidence_ids":["bundle"]}).to_string()}]}]})
}
fn events(c: &Config) -> Vec<Value> {
    let archive = fs::read_dir(c.directory.join("model"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.is_dir()
                && p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("harness-")
        })
        .unwrap();
    super::super::transcript::read(&archive.join("transcript.ndjson"), 1_000_000).unwrap()
}
#[test]
fn captures_two_turns_two_tools_and_survives_unwritable_capture() {
    let first = json!({"usage":{"input_tokens":10,"output_tokens":3,"input_tokens_details":{"cached_tokens":2}},"output":[{"type":"function_call","call_id":"one","name":"read_evidence","arguments":"{\"evidence_id\":\"bundle\"}"},{"type":"function_call","call_id":"two","name":"read_evidence","arguments":"{\"evidence_id\":\"portfolio\"}"}]});
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let mut backend = Script(VecDeque::from([Ok(first.clone()), Ok(final_response())]));
    let result = run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role(),
        "Capture fixture",
        &json!({"portfolio":{"positions":[]}}),
        &mut backend,
        &planner::preview_guard(),
    )
    .unwrap();
    let events = events(&c);
    let kinds: Vec<_> = events.iter().map(|v| v["kind"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "run_started",
            "system_prompt",
            "input",
            "tool_call",
            "tool_call",
            "turn_usage",
            "tool_result",
            "tool_result",
            "assistant_text",
            "turn_usage",
            "final_output",
            "host_note",
            "run_ended"
        ]
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0]["at_ms"].as_i64() <= pair[1]["at_ms"].as_i64())
    );
    for event in events.iter().filter(|v| v["kind"] == "turn_usage") {
        assert!(event["started_ms"].as_i64() <= event["ended_ms"].as_i64());
    }
    let end = events.last().unwrap();
    assert_eq!(end["outcome"], "accepted");
    assert_eq!(end["tokens_in"], 30);
    assert_eq!(end["tokens_out"], 8);
    assert_eq!(end["tool_calls"], 2);
    assert_eq!(end["tool_failures"], 0);
    assert!(end["cost_estimate_inr"].is_null());
    let index = fs::read_to_string(c.directory.join("model/runs.ndjson")).unwrap();
    let indexed: Value = serde_json::from_str(index.trim()).unwrap();
    assert_eq!(indexed["run"], events[0]["run"]);
    assert_eq!(indexed["tokens_in"], 30);
    assert_eq!(indexed["outcome"], "accepted");
    assert!(indexed["bytes"].as_u64().unwrap() > 0);
    let mut repeated = Script(VecDeque::from([Ok(final_response())]));
    run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role(),
        "Another input, same instructions",
        &json!({}),
        &mut repeated,
        &planner::preview_guard(),
    )
    .unwrap();
    let prompts: Vec<_> = fs::read_dir(c.directory.join("model/prompts"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(prompts.len(), 1);
    let sha = digest(&fs::read(&prompts[0]).unwrap());
    for archive in fs::read_dir(c.directory.join("model"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("run.json").is_file())
    {
        let request: Value =
            serde_json::from_slice(&fs::read(archive.join("turn-00-request.json")).unwrap())
                .unwrap();
        assert_eq!(request["instructions_sha256"], sha);
        let transcript =
            super::super::transcript::read(&archive.join("transcript.ndjson"), 1_000_000).unwrap();
        assert!(
            transcript
                .iter()
                .any(|v| v["kind"] == "system_prompt" && v["path"] == format!("prompts/{sha}.md"))
        );
    }
    struct FrameCapture(Option<Frame>);
    impl Backend for FrameCapture {
        fn call(&mut self, _: &ModelRole, request: &str) -> Result<String> {
            self.0 = Some(serde_json::from_str(request)?);
            Ok(final_response().to_string())
        }
    }
    struct TimeoutCapture(u64);
    impl Backend for TimeoutCapture {
        fn call(&mut self, role: &ModelRole, _: &str) -> Result<String> {
            self.0 = role.timeout_ms;
            Ok(final_response().to_string())
        }
    }
    let timeout_dir = tempfile::tempdir().unwrap();
    let timeout_config = super::super::tests::config(timeout_dir.path());
    let timeout_role = ModelRole {
        timeout_ms: 120_000,
        run_timeout_ms: Some(450_000),
        ..role()
    };
    for kind in ["decision", "compaction", "handoff"] {
        let mut backend = TimeoutCapture(0);
        run(
            &timeout_config,
            &PlannerConfig::default(),
            "supervisor",
            &timeout_role,
            &"large prompt".repeat(10_000),
            &json!({"trigger":{"kind":kind}}),
            &mut backend,
            &planner::preview_guard(),
        )
        .unwrap();
        let ceiling = if kind == "decision" { 120_000 } else { 450_000 };
        assert!(
            backend.0 <= ceiling && backend.0 > ceiling - 10_000,
            "{kind}: {}",
            backend.0
        );
    }
    let supervisor_dir = tempfile::tempdir().unwrap();
    let supervisor_config = super::super::tests::config(supervisor_dir.path());
    let system_prompt = supervisor_dir.path().join("system.md");
    fs::write(&system_prompt, "Persisted orchestrator instructions").unwrap();
    let orchestrator = super::super::orchestrator_model::OrchestratorModelSettings {
        system_prompt: Some(system_prompt),
        ..Default::default()
    };
    let planner_config = PlannerConfig {
        orchestrator: Some(orchestrator),
        ..Default::default()
    };
    let mut capture = FrameCapture(None);
    run(
        &supervisor_config,
        &planner_config,
        "supervisor",
        &role(),
        "Supervisor fixture",
        &json!({"trigger":{"kind":"idle","reason":"idle"}}),
        &mut capture,
        &planner::preview_guard(),
    )
    .unwrap();
    let sent = capture.0.unwrap().instructions.unwrap();
    assert_eq!(
        sent,
        format!("{}\n\nPersisted orchestrator instructions", objective())
    );

    let owner_dir = tempfile::tempdir().unwrap();
    let owner_config = super::super::tests::config(owner_dir.path());
    let owner_policy = PlannerConfig {
        orchestrator: Some(Default::default()),
        ..Default::default()
    };
    let mut owner_capture = FrameCapture(None);
    run(
        &owner_config,
        &owner_policy,
        "supervisor",
        &role(),
        "Orchestrator decision fixture",
        &json!({"trigger":{"kind":"decision","reason":"decision"}}),
        &mut owner_capture,
        &planner::preview_guard(),
    )
    .unwrap();
    assert_eq!(
        owner_capture.0.unwrap().instructions.unwrap(),
        format!("{}\n\n{}", objective(), ORCHESTRATOR_PROMPT)
    );

    // Async research controls are scoped to a live decision-owner
    // invocation, never to legacy review, lifecycle or aside calls.
    let mut task_policy = owner_policy.clone();
    task_policy.research_tools = Some(ResearchSettings {
        queue: owner_dir.path().join("research.sqlite"),
        stage_root: owner_dir.path().join("research-stage"),
        task_ttl_ms: 6 * 60 * 60 * 1000,
        termination_grace_ms: 30_000,
        admission_limits: None,
        library_max_candidates: None,
        library_max_bytes: None,
        candidate_compute: None,
        registrations: Vec::new(),
    });
    let mut owner_guard = planner::preview_guard();
    owner_guard.invocation_id = Some("turn-owner-task-fixture");
    let owner_bundle = json!({
        "trigger":{"kind":"decision"},
        "summary":{"host_decision":{"turn_id":"turn-owner-task-fixture"}}
    });
    // Exercise the real provider preflight with host-built decision/lifecycle
    // frames; missing application config stops before credentials or transport.
    for (workspace, research) in [(true, false), (false, true), (true, true)] {
        let mut policy = task_policy.clone();
        policy.workspace =
            workspace.then(|| serde_json::from_value(json!({"enabled":true})).unwrap());
        if !research {
            policy.research_tools = None;
        }
        for kind in ["decision", "compaction", "handoff"] {
            let mut bundle = owner_bundle.clone();
            bundle["trigger"]["kind"] = json!(kind);
            let tools = definitions_for_call(
                &policy,
                "supervisor",
                &[],
                false,
                &ToolsSettings::default(),
                &bundle,
                &owner_guard,
            )
            .unwrap();
            assert_eq!(
                tools.len(),
                if kind == "decision" {
                    usize::from(workspace) * 5 + usize::from(research) * 4
                } else {
                    0
                }
            );
            let frame = Frame {
                role: "supervisor".into(),
                input: vec![],
                tools,
                owner_tools: owner_tools_for_call(&policy, "supervisor", &bundle, &owner_guard),
                citation_ids: vec![],
                final_answer_only: false,
                review_only: false,
                decision_evidence: false,
                max_instrument_keys: None,
                instructions: None,
                tool_limits: None,
            };
            let mut provenance = Value::Null;
            assert_eq!(
                crate::model::planner_response_with_alerts(
                    Path::new("/nonexistent-nivra-model-config"),
                    &role(),
                    &serde_json::to_string(&frame).unwrap(),
                    &mut provenance,
                    None,
                )
                .unwrap_err(),
                "model_config_unreadable",
                "{provenance}"
            );
            assert_eq!(provenance["stage"], "application_config");
        }
    }
    let task_tools = definitions_for_call(
        &task_policy,
        "supervisor",
        &[],
        false,
        &ToolsSettings::default(),
        &owner_bundle,
        &owner_guard,
    )
    .unwrap();
    let task_names = task_tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        task_names,
        [
            "research_task_enqueue",
            "research_task_status",
            "research_task_result",
            "research_task_cancel"
        ]
    );
    let call = |name: &str, args: Value| {
        json!({
            "call_id":"owner-task-call",
            "name":name,
            "arguments":args.to_string()
        })
    };
    let (_, task_call) = checked_call_with(
        &call(
            "research_task_enqueue",
            json!({
                "task_key":"daily-replay",
                "goal":"Compare registered analysis output",
                "analysis_id":"analysis_v1",
                "dataset_id":"paper_fixture"
            }),
        ),
        &task_tools,
        &ToolsSettings::default(),
    )
    .unwrap();
    assert!(matches!(task_call, Tool::OwnerResearchEnqueue(_)));
    assert!(
        checked_call_with(
            &call(
                "research_task_enqueue",
                json!({
                    "task_key":"daily-replay",
                    "goal":"Compare registered analysis output",
                    "analysis_id":"analysis_v1",
                    "dataset_id":"paper_fixture",
                    "queue_path":"/tmp/no"
                }),
            ),
            &task_tools,
            &ToolsSettings::default(),
        )
        .is_err()
    );
    assert!(
        checked_call_with(
            &call(
                "research_task_result",
                json!({"task_id":"BAD","revision":1}),
            ),
            &task_tools,
            &ToolsSettings::default(),
        )
        .is_err()
    );
    assert!(
        checked_call_with(
            &call(
                "research_task_enqueue",
                json!({
                    "task_key":"daily-replay",
                    "goal":"api_key=private-fixture",
                    "analysis_id":"analysis_v1",
                    "dataset_id":"paper_fixture"
                }),
            ),
            &task_tools,
            &ToolsSettings::default(),
        )
        .is_err()
    );
    assert_eq!(
        super::transcript::redact(&json!("Bearer owner-fixture-secret")),
        json!("[redacted]")
    );
    let lifecycle_tools = definitions_for_call(
        &task_policy,
        "supervisor",
        &[],
        false,
        &ToolsSettings::default(),
        &json!({"trigger":{"kind":"handoff"},"summary":{}}),
        &owner_guard,
    )
    .unwrap();
    assert!(!lifecycle_tools.iter().any(|tool| {
        tool["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("research_task_"))
    }));
    let legacy_task_tools = definitions_for_call(
        &PlannerConfig::default(),
        "supervisor",
        &[],
        false,
        &ToolsSettings::default(),
        &owner_bundle,
        &owner_guard,
    )
    .unwrap();
    assert!(!legacy_task_tools.iter().any(|tool| {
        tool["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("research_task_"))
    }));

    let blocked_dir = tempfile::tempdir().unwrap();
    let blocked = super::super::tests::config(blocked_dir.path());
    super::super::transcript::block_next_start();
    let mut backend = Script(VecDeque::from([Ok(first), Ok(final_response())]));
    assert_eq!(
        run(
            &blocked,
            &PlannerConfig::default(),
            "digest",
            &role(),
            "Capture fixture",
            &json!({"portfolio":{"positions":[]}}),
            &mut backend,
            &planner::preview_guard()
        )
        .unwrap(),
        result
    );
}
#[test]
fn delta_requests_rebuild_exact_provider_bytes_and_reduce_archive_size() {
    struct Recorded {
        requests: Vec<String>,
        responses: VecDeque<Value>,
    }
    impl Backend for Recorded {
        fn call(&mut self, _: &ModelRole, request: &str) -> Result<String> {
            self.requests.push(request.to_owned());
            Ok(self
                .responses
                .pop_front()
                .ok_or("fixture_exhausted")?
                .to_string())
        }
    }
    let tool = |id: &str| json!({"output":[{"type":"function_call","call_id":id,"name":"read_evidence","arguments":"{\"evidence_id\":\"bundle\"}"}]});
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(&dir.path().join("sessions/2026-10-16"));
    let mut backend = Recorded {
        requests: vec![],
        responses: VecDeque::from([tool("one"), tool("two"), final_response()]),
    };
    run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role(),
        &"Fixture context ".repeat(4000),
        &json!({}),
        &mut backend,
        &planner::preview_guard(),
    )
    .unwrap();
    assert_eq!(backend.requests.len(), 3);
    let archive = fs::read_dir(c.directory.join("model"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.join("run.json").is_file())
        .unwrap();
    let rebuilt = super::super::transcript::full_request(&archive, 2, &c.dashboard).unwrap();
    let frame: Frame = serde_json::from_value(rebuilt["request"].clone()).unwrap();
    assert_eq!(serde_json::to_string(&frame).unwrap(), backend.requests[2]);
    assert_eq!(
        digest(serde_json::to_string(&frame).unwrap().as_bytes()),
        digest(backend.requests[2].as_bytes())
    );
    let old_bytes: usize = backend.requests.iter().map(|request|serde_json::to_vec(&json!({"request":serde_json::from_str::<Value>(request).unwrap(),"instructions_sha256":rebuilt["instructions_sha256"]})).unwrap().len()).sum();
    let new_bytes: usize = [
        "turn-00-request.json",
        "turn-01-request-delta.json",
        "turn-02-request-delta.json",
    ]
    .iter()
    .map(|name| fs::metadata(archive.join(name)).unwrap().len() as usize)
    .sum();
    println!("request_archive_delta_bytes={new_bytes} cumulative_bytes={old_bytes}");
    assert!(
        new_bytes * 2 <= old_bytes,
        "delta={new_bytes}, cumulative={old_bytes}"
    );
    let delta_path = archive.join("turn-02-request-delta.json");
    let mut delta: Value = serde_json::from_slice(&fs::read(&delta_path).unwrap()).unwrap();
    assert!(delta["tools"].is_null());
    assert_eq!(
        delta["request_sha256"],
        digest(backend.requests[2].as_bytes())
    );
    delta["append_input"][0] = json!({"tampered":true});
    fs::write(&delta_path, serde_json::to_vec(&delta).unwrap()).unwrap();
    assert_eq!(
        super::super::transcript::full_request(&archive, 2, &c.dashboard)
            .unwrap_err()
            .to_string(),
        "archive_request_rebuild_mismatch"
    );
    // Preserve legacy full-turn readers and history after delta rollout.
    let legacy = dir.path().join("legacy");
    fs::create_dir(&legacy).unwrap();
    fs::copy(
        archive.join("turn-00-request.json"),
        legacy.join("turn-00-request.json"),
    )
    .unwrap();
    let legacy_request = json!({"request":serde_json::from_str::<Value>(&backend.requests[2]).unwrap(),"instructions_sha256":rebuilt["instructions_sha256"]});
    fs::write(
        legacy.join("turn-02-request.json"),
        serde_json::to_vec(&legacy_request).unwrap(),
    )
    .unwrap();
    assert_eq!(
        super::super::transcript::full_request(&legacy, 2, &c.dashboard).unwrap(),
        legacy_request
    );
    let detail = super::super::model_history::run_detail_with_settings(
        &c.directory.join("model/live-stream.json"),
        archive.file_name().unwrap().to_str().unwrap(),
        &c.dashboard,
    )
    .unwrap();
    assert_eq!(detail["run"]["turns"].as_array().unwrap().len(), 3);
}
#[test]
fn daily_index_records_three_finished_runs_and_exact_estimated_cost() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::super::tests::config(dir.path());
    c.dashboard.model_prices.insert(
        role().model_id,
        super::super::ModelPrice {
            input_inr_per_mtok: "10".into(),
            output_inr_per_mtok: "20".into(),
            cached_inr_per_mtok: Some("2".into()),
        },
    );
    let mut accepted = Script(VecDeque::from([Ok(final_response())]));
    run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role(),
        "Accepted index fixture",
        &json!({}),
        &mut accepted,
        &planner::preview_guard(),
    )
    .unwrap();
    let data = events(&c);
    let end = data.last().unwrap();
    assert_eq!(end["cost_estimate_inr"], "0.000308");
    assert_eq!(end["cost_basis"], "configured_estimate");
    let mut abstain = Script(VecDeque::from([Ok(
        json!({"actions":[],"stance":"abstain","notes":"fixture"}),
    )]));
    run(
        &c,
        &PlannerConfig::default(),
        "supervisor",
        &role(),
        "Abstain index fixture",
        &json!({}),
        &mut abstain,
        &planner::preview_guard(),
    )
    .unwrap();
    let mut failed = Script(VecDeque::from([Err("fixture_provider_failed".into())]));
    assert!(
        run(
            &c,
            &PlannerConfig::default(),
            "digest",
            &role(),
            "Failed index fixture",
            &json!({}),
            &mut failed,
            &planner::preview_guard()
        )
        .is_err()
    );
    let mut supervisor = role();
    supervisor.tools.clear();
    let mut archived = Script(VecDeque::from([Ok(final_response())]));
    run(
        &c,
        &PlannerConfig::default(),
        "supervisor",
        &supervisor,
        "Run first",
        &json!({"trigger":{"kind":"session_open","reason":"session_open"},"summary":{}}),
        &mut archived,
        &planner::preview_guard(),
    )
    .unwrap();
    let raw = fs::read_to_string(c.directory.join("model/runs.ndjson")).unwrap();
    let lines: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["outcome"], "accepted");
    assert_eq!(lines[1]["outcome"], "abstain");
    assert_eq!(lines[2]["outcome"], "failure");
    assert_eq!(lines[0]["cost_estimate_inr"], "0.000308");
    assert!(lines[2]["cost_estimate_inr"].is_null());
    assert_eq!(lines[3]["role"], "supervisor");
    assert_eq!(lines[3]["trigger_kind"], "session_open");
    assert_eq!(lines[3]["outcome"], "accepted");
    let archive = c
        .directory
        .join("model")
        .join(lines[3]["run"].as_str().unwrap());
    let events =
        super::super::transcript::read(&archive.join("transcript.ndjson"), 1_000_000).unwrap();
    assert_eq!(events[0]["kind"], "run_started");
    assert_eq!(events[0]["run"], lines[3]["run"]);
    assert_eq!(events[0]["trigger"]["kind"], "session_open");
    assert!(events.iter().any(|event| event["kind"] == "assistant_text"));
    assert!(events.iter().any(|event| event["kind"] == "run_ended"));
}
#[test]
fn captures_failed_tools_and_backend_failure_with_terminal_events() {
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let mut backend = Script(VecDeque::from([
        Ok(
            json!({"output":[{"type":"function_call","call_id":"feature","name":"get_instrument_features","arguments":"{\"instrument_keys\":[\"SYNTHETIC|ONE\"]}"}]}),
        ),
        Ok(final_response()),
    ]));
    run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role(),
        "Failed tool fixture",
        &json!({}),
        &mut backend,
        &planner::preview_guard(),
    )
    .unwrap();
    let data = events(&c);
    assert!(
        data.iter()
            .any(|v| v["kind"] == "tool_result" && v["status"] == "failed")
    );
    assert_eq!(data.last().unwrap()["tool_failures"], 1);
    assert!(data.last().unwrap()["tokens_in"].is_null());
    let failed_dir = tempfile::tempdir().unwrap();
    let failed = super::super::tests::config(failed_dir.path());
    let mut backend = Script(VecDeque::from([Err("fixture_provider_failed".into())]));
    assert!(
        run(
            &failed,
            &PlannerConfig::default(),
            "digest",
            &role(),
            "Backend failure fixture",
            &json!({}),
            &mut backend,
            &planner::preview_guard()
        )
        .is_err()
    );
    assert_eq!(events(&failed).last().unwrap()["outcome"], "failure");
    let capped_dir = tempfile::tempdir().unwrap();
    let capped = super::super::tests::config(capped_dir.path());
    let mut backend = Script(VecDeque::from([
        Ok(
            json!({"output":[{"type":"function_call","call_id":"large-read","name":"read_evidence",
                "arguments":"{\"evidence_id\":\"session_evidence\"}"}]}),
        ),
        Ok(final_response()),
    ]));
    run(
        &capped,
        &PlannerConfig::default(),
        "digest",
        &role(),
        "Oversized read fixture",
        &json!({"session_evidence":"x".repeat(TOOL_OUTPUT_MAX_BYTES+1)}),
        &mut backend,
        &planner::preview_guard(),
    )
    .unwrap();
    let captured = events(&capped);
    assert!(captured.iter().any(|v| v["kind"] == "tool_result"
        && v["status"] == "failed"
        && v["error"] == "harness_tool_output_cap"));
    assert_eq!(captured.last().unwrap()["outcome"], "accepted");
}

/// A provider outage fails before any provider turn. The fixture reports the
/// same bounded stage a real catalog/auth failure does, so the archive path
/// exercises the pre-provider branch.
struct PreProviderFailure {
    stage: &'static str,
    calls: usize,
}
impl Backend for PreProviderFailure {
    fn call(&mut self, _: &ModelRole, _: &str) -> Result<String> {
        self.calls += 1;
        Err("model_http_401".into())
    }
    fn provenance(&self) -> Value {
        json!({"stage": self.stage, "http_status": 401})
    }
}
fn harness_archives(c: &Config) -> Vec<PathBuf> {
    let mut archives: Vec<_> = fs::read_dir(c.directory.join("model"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("harness-"))
        })
        .collect();
    archives.sort();
    archives
}
fn failure_record(archive: &Path) -> Value {
    serde_json::from_slice(&fs::read(archive.join("failure.json")).unwrap()).unwrap()
}
#[test]
fn repeated_pre_provider_failures_keep_the_session_model_directory_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(&dir.path().join("sessions/2026-11-17"));
    // A multi-megabyte request is what the outage retried every few seconds.
    let prompt = "digest envelope ".repeat(90_000);
    assert!(prompt.len() < 2_000_000);
    for attempt in 0..8 {
        let mut backend = PreProviderFailure {
            stage: "catalog",
            calls: 0,
        };
        assert!(
            run(
                &c,
                &PlannerConfig::default(),
                "digest",
                &role(),
                &format!("{prompt}{attempt}"),
                &json!({}),
                &mut backend,
                &planner::preview_guard(),
            )
            .is_err()
        );
        assert_eq!(backend.calls, 1);
    }
    let archives = harness_archives(&c);
    assert_eq!(archives.len(), 8);
    let mut representatives = Vec::new();
    let mut repeated = Vec::new();
    let mut persisted_request_bytes = 0u64;
    for archive in &archives {
        let failure = failure_record(archive);
        assert_eq!(failure["pre_provider"], true, "{}", archive.display());
        assert_eq!(failure["provider_stage"], "catalog");
        assert_eq!(failure["reason"], "model_http_401");
        assert_eq!(failure["provider_reason"], "model_http_401");
        assert_eq!(failure["http_status"], 401);
        assert_eq!(failure["request_sha256"].as_str().unwrap().len(), 64);
        assert!(failure["request_bytes"].as_u64().unwrap() > 1_000_000);
        assert_eq!(failure["request_receipt"], "turn-00-request-receipt.json");
        let receipt = archive.join("turn-00-request-receipt.json");
        let receipt_bytes = fs::metadata(&receipt).unwrap().len();
        assert!(receipt_bytes <= super::failure_window::REQUEST_RECEIPT_MAX_BYTES as u64);
        persisted_request_bytes += receipt_bytes;
        if archive.join("turn-00-request.json").is_file() {
            assert_eq!(failure["request_archive"], "turn-00-request.json");
            assert!(failure["representative_request"].is_null());
            persisted_request_bytes += fs::metadata(archive.join("turn-00-request.json"))
                .unwrap()
                .len();
            representatives.push(archive.clone());
        } else {
            assert!(failure["request_archive"].is_null());
            repeated.push(archive.clone());
        }
    }
    assert_eq!(representatives.len(), 1);
    assert_eq!(repeated.len(), 7);
    let representative = representatives[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    for archive in &repeated {
        let failure = failure_record(archive);
        assert_eq!(failure["representative_request"]["run"], representative);
        assert_eq!(
            failure["representative_request"]["blob"],
            "turn-00-request.json"
        );
        assert!(failure["failure_window_count"].as_u64().unwrap() >= 2);
    }
    let representative_bytes = fs::metadata(representatives[0].join("turn-00-request.json"))
        .unwrap()
        .len();
    assert!(
        persisted_request_bytes < representative_bytes + 64 * 1024,
        "persisted {persisted_request_bytes} request bytes for 8 pre-provider failures \
         (one representative is {representative_bytes})"
    );
    // The archived transcript stays readable: the first failure links its full
    // request, repeated failures link the bounded receipt.
    for archive in &archives {
        let events =
            super::super::transcript::read(&archive.join("transcript.ndjson"), 1_000_000).unwrap();
        let input = events
            .iter()
            .find(|event| event["kind"] == "input")
            .unwrap();
        let blob = if archive == &representatives[0] {
            "turn-00-request.json#/request/input"
        } else {
            "turn-00-request-receipt.json"
        };
        assert_eq!(input["blob"], blob);
        let ended = events.last().unwrap();
        assert_eq!(ended["kind"], "run_ended");
        assert_eq!(ended["outcome"], "failure");
        assert_eq!(ended["reason"], "model_http_401");
    }
    // The dashboard run list keeps every failure visible.
    let source = c.directory.join("model/live-stream.json");
    let page = super::super::transcript::list_runs(
        &source,
        &super::super::transcript::RunFilter {
            limit: 100,
            ..Default::default()
        },
        &c.dashboard,
    )
    .unwrap();
    let rows = page.body["runs"].as_array().unwrap();
    assert_eq!(rows.len(), 8);
    for row in rows {
        assert_eq!(row["role"], "digest");
        assert_eq!(row["model"], json!(role().model_id));
        assert!(row["started_ms"].as_i64().is_some());
        assert_eq!(row["outcome"], "failure");
    }
    let detail = super::super::model_history::run_detail_with_settings(
        &source,
        repeated[0].file_name().unwrap().to_str().unwrap(),
        &c.dashboard,
    )
    .unwrap();
    assert_eq!(detail["run"]["role"], "digest");
    assert_eq!(detail["run"]["model"], json!(role().model_id));
    assert_eq!(detail["run"]["outcome"], "failure");
    assert_eq!(detail["run"]["reason"], "model_http_401");
    // A provider turn ends the window: the next outage keeps its own first
    // failure evidence instead of linking to a stale representative request.
    let mut recovered = Script(VecDeque::from([Ok(final_response())]));
    run(
        &c,
        &PlannerConfig::default(),
        "digest",
        &role(),
        "Recovered digest",
        &json!({}),
        &mut recovered,
        &planner::preview_guard(),
    )
    .unwrap();
    let mut backend = PreProviderFailure {
        stage: "catalog",
        calls: 0,
    };
    assert!(
        run(
            &c,
            &PlannerConfig::default(),
            "digest",
            &role(),
            "Second outage",
            &json!({}),
            &mut backend,
            &planner::preview_guard(),
        )
        .is_err()
    );
    let representatives: Vec<_> = harness_archives(&c)
        .into_iter()
        .filter(|archive| {
            archive.join("turn-00-request.json").is_file()
                && serde_json::from_slice::<Value>(
                    &fs::read(archive.join("failure.json")).unwrap_or_default(),
                )
                .is_ok_and(|failure| failure["pre_provider"] == true)
        })
        .collect();
    assert_eq!(representatives.len(), 2);
    let second = representatives
        .iter()
        .max_by_key(|archive| {
            fs::metadata(archive.join("run.json"))
                .unwrap()
                .modified()
                .unwrap()
        })
        .unwrap();
    let failure = failure_record(second);
    assert_eq!(failure["request_archive"], "turn-00-request.json");
    assert!(failure["representative_request"].is_null());
}
