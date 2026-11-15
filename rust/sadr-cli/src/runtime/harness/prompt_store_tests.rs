use super::*;
#[test]
fn a_forced_final_turn_stores_distinct_instruction_files() {
    struct Forced<'a> {
        clock: &'a std::cell::Cell<i64>,
        calls: usize,
    }
    impl Backend for Forced<'_> {
        fn call(&mut self, _: &ModelRole, request: &str) -> Result<String> {
            let request: Value = serde_json::from_str(request)?;
            self.calls += 1;
            if self.calls == 1 {
                assert!(!request["final_answer_only"].as_bool().unwrap_or(false));
                self.clock.set(150_000);
                Ok(json!({"output":[{"type":"function_call","call_id":"one","name":"read_evidence","arguments":"{\"evidence_id\":\"bundle\"}"}]}).to_string())
            } else {
                assert_eq!(request["final_answer_only"], true);
                Ok(json!({"actions":[],"stance":"abstain","notes":"fixture"}).to_string())
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let c = super::super::tests::config(dir.path());
    let role = ModelRole {
        enabled: true,
        model_id: "grok-4.7".into(),
        transport: sadr_cli::native_transport::Transport::DirectOauth,
        daily_call_budget: 8,
        timeout_ms: 300_000,
        run_timeout_ms: Some(300_000),
        ..ModelRole::default()
    };
    let clock = std::cell::Cell::new(0);
    let observe = || clock.get();
    let mut guard = planner::preview_guard();
    guard.clock = &observe;
    let mut backend = Forced {
        clock: &clock,
        calls: 0,
    };
    run(
        &c,
        &legacy_test_config(),
        "decide",
        &role,
        "Forced prompt fixture",
        &json!({}),
        &mut backend,
        &guard,
    )
    .unwrap();
    assert_eq!(backend.calls, 2);
    assert_eq!(
        fs::read_dir(c.directory.join("model/prompts"))
            .unwrap()
            .count(),
        2
    );
    let archive = fs::read_dir(c.directory.join("model"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.join("run.json").is_file())
        .unwrap();
    let events =
        super::super::transcript::read(&archive.join("transcript.ndjson"), 1_000_000).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|v| v["kind"] == "system_prompt")
            .count(),
        2
    );
    for turn in 0..2 {
        let request = super::super::transcript::full_request(&archive, turn, &c.dashboard).unwrap();
        let sha = request["instructions_sha256"].as_str().unwrap();
        let bytes = fs::read(c.directory.join(format!("model/prompts/{sha}.md"))).unwrap();
        assert_eq!(digest(&bytes), sha);
    }
}
#[test]
fn prompt_files_are_private_deduplicated_hashed_and_preserve_failed_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let mut frame = Frame {
        role: "decide".into(),
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
    let text = frame_instructions(&frame).unwrap();
    let path = store_prompt(dir.path(), &text).unwrap();
    assert_eq!(store_prompt(dir.path(), &text).unwrap(), path);
    assert_eq!(fs::read_dir(dir.path().join("prompts")).unwrap().count(), 1);
    let bytes = fs::read(dir.path().join(&path)).unwrap();
    assert_eq!(path, format!("prompts/{}.md", digest(&bytes)));
    assert_eq!(bytes, text.as_bytes());
    assert_eq!(
        fs::metadata(dir.path().join(&path))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    frame.final_answer_only = true;
    let final_text = frame_instructions(&frame).unwrap();
    let final_path = store_prompt(dir.path(), &final_text).unwrap();
    assert_ne!(path, final_path);
    assert_eq!(fs::read_dir(dir.path().join("prompts")).unwrap().count(), 2);
    fs::write(dir.path().join(&path), "corrupted fixture").unwrap();
    assert!(store_prompt(dir.path(), &text).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join(&path)).unwrap(),
        "corrupted fixture"
    );
    assert!(store_prompt(dir.path(), "Bearer secret-fixture").is_err());
}
