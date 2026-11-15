use super::*;
use serde_json::{Value, json};
struct Fixture {
    _dir: tempfile::TempDir,
    source: PathBuf,
    settings: DashboardSettings,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let source = dir
            .path()
            .join("sessions/2026-10-28/model/live-stream.json");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        Self {
            _dir: dir,
            source,
            settings: DashboardSettings::default(),
        }
    }
    fn send(&self, path: &str, extra: &str) -> Response {
        dispatch_source(
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n{extra}\r\n"
            ),
            b"",
            "fixture",
            &Arc::new(Mutex::new(json!({}))),
            None,
            None,
            Some(&self.source),
            &self.settings,
        )
    }
    fn post(&self, path: &str, body: &str) -> Response {
        dispatch_source(
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\nContent-Type: application/json\r\n\r\n"
            ),
            body.as_bytes(),
            "fixture",
            &Arc::new(Mutex::new(json!({}))),
            None,
            None,
            Some(&self.source),
            &self.settings,
        )
    }
    fn add(&self, id: &str, outcome: Option<&str>) -> PathBuf {
        let dir = self.source.parent().unwrap().join(id);
        fs::create_dir(&dir).unwrap();
        let clock = || 100;
        let mut writer = super::super::super::transcript::Transcript::start(
            &dir,
            id,
            &self.settings,
            json!({"role":"digest","model":"fixture","trigger":{"kind":"fill"},"mode":"paper"}),
            &clock,
        )
        .unwrap();
        writer.event("input",json!({"turn":0,"bytes":1,"summary":{"input_items":1},"blob":"turn-00-request.json#/request/input"})).unwrap();
        writer.event("tool_call",json!({"turn":0,"call":"one","name":"read_evidence","args":{"evidence_id":"bundle"},"instruments":[]})).unwrap();
        let blob = format!("tool-{}.result.json", digest(b"one"));
        fs::write(dir.join(&blob),serde_json::to_vec(&json!({"cash":"10","api_key":"planted-fixture-secret","id_token":"id-fixture-secret","output":"{\"Authorization\":\"opaque-nested-fixture\",\"cash\":10}"})).unwrap()).unwrap();
        writer.event("tool_result",json!({"turn":0,"call":"one","name":"read_evidence","status":if outcome==Some("failure"){"failed"}else{"ok"},"duration_ms":1,"bytes":1,"summary":{},"blob":blob})).unwrap();
        let prose = "Long fixture prose ".repeat(400);
        writer
            .event("assistant_text", json!({"turn":0,"text":prose}))
            .unwrap();
        fs::write(dir.join("turn-00-response.json"),serde_json::to_vec(&json!({"output":[{"type":"message","role":"assistant","content":[{"text":prose}]}]})).unwrap()).unwrap();
        fs::write(
            dir.join("turn-00-request.json"),
            serde_json::to_vec(
                &json!({"request":{"input":[{"cash":"10","api_key":"request-fixture-secret"}]}}),
            )
            .unwrap(),
        )
        .unwrap();
        if let Some(outcome) = outcome {
            if outcome == "failure" {
                fs::write(dir.join("failure.json"), br#"{"reason":"fixture_failure"}"#).unwrap();
            } else {
                fs::write(dir.join("output.json"), br#"{"output":{"summary":"done"}}"#).unwrap();
                writer
                    .event("final_output", json!({"output":{"summary":"done"}}))
                    .unwrap();
            }
            writer.end(json!({"outcome":outcome,"reason":null,"tokens_in":null,"tokens_out":null,"cost_estimate_inr":null,"duration_ms":2,"tool_calls":1,"tool_failures":usize::from(outcome=="failure")})).unwrap();
        }
        dir
    }
}
#[test]
fn transcripts_distinguish_finished_running_failed_legacy_and_retained_runs() {
    let fixture = Fixture::new();
    let normal = fixture.add("harness-digest-100-normal", Some("accepted"));
    fixture.add("harness-digest-200-running", None);
    fixture.add("harness-digest-300-failed", Some("failure"));
    for (id, running, outcome) in [
        ("harness-digest-100-normal", false, Some("accepted")),
        ("harness-digest-200-running", true, None),
        ("harness-digest-300-failed", false, Some("failure")),
    ] {
        let reply = fixture.send(&format!("/api/v1/runs/{id}"), "");
        assert_eq!(reply.code, "200 OK");
        assert_eq!(reply.cache.contains("immutable"), !running);
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["running"], running);
        assert_eq!(body["legacy"], false);
        assert_eq!(body["run"], id);
        if let Some(outcome) = outcome {
            assert_eq!(
                body["events"].as_array().unwrap().last().unwrap()["outcome"],
                outcome
            );
        }
        let text = body["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["kind"] == "assistant_text")
            .unwrap();
        assert!(text.get("text").is_none());
        assert_eq!(text["blob"], "turn-00-response.json");
        let cached = fixture.send(
            &format!("/api/v1/runs/{id}"),
            &format!("If-None-Match: {}\r\n", reply.etag.unwrap()),
        );
        assert_eq!(cached.code, "304 Not Modified");
    }
    let legacy = fixture
        .source
        .parent()
        .unwrap()
        .join("harness-decide-50-legacy");
    fs::create_dir(&legacy).unwrap();
    fs::write(
        legacy.join("run.json"),
        br#"{"role":"decide","model":"legacy","started_ms":1,"completed_ms":2}"#,
    )
    .unwrap();
    fs::write(
        legacy.join("input.json"),
        br#"{"input":{"cash":10,"api_key":"legacy-fixture-secret"}}"#,
    )
    .unwrap();
    fs::write(
        legacy.join("output.json"),
        br#"{"output":{"actions":[],"stance":"abstain"}}"#,
    )
    .unwrap();
    fs::write(legacy.join("turn-00-response.json"),br#"{"output":[{"type":"function_call","call_id":"legacy-one","name":"read_evidence","arguments":"{}"}]}"#).unwrap();
    let legacy_blob = format!("tool-{}.result.json", digest(b"legacy-one"));
    fs::write(
        legacy.join(&legacy_blob),
        br#"{"status":"ok","cash":15,"api_key":"legacy-tool-fixture-secret"}"#,
    )
    .unwrap();
    let reply = fixture.send("/api/v1/runs/harness-decide-50-legacy", "");
    let body: Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["legacy"], true);
    assert_eq!(body["running"], false);
    assert!(
        body["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["kind"] == "input" && v["blob"] == "input.json")
    );
    assert!(
        body["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "tool_result" && event["blob"] == legacy_blob)
    );
    let legacy_result = fixture.send(
        &format!("/api/v1/runs/harness-decide-50-legacy/blobs/{legacy_blob}"),
        "",
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&legacy_result.body).unwrap()["cash"],
        15
    );
    assert!(!String::from_utf8_lossy(&legacy_result.body).contains("legacy-tool-fixture-secret"));
    let full_input = fixture.send("/api/v1/runs/harness-decide-50-legacy/blobs/input.json", "");
    assert_eq!(full_input.code, "200 OK");
    assert_eq!(
        serde_json::from_slice::<Value>(&full_input.body).unwrap()["input"]["cash"],
        10
    );
    assert!(!String::from_utf8_lossy(&full_input.body).contains("legacy-fixture-secret"));
    let unfinished = fixture
        .source
        .parent()
        .unwrap()
        .join("harness-digest-25-legacy-running");
    fs::create_dir(&unfinished).unwrap();
    fs::write(unfinished.join("turn-00-response.json"),br#"{"output":[{"type":"message","role":"assistant","content":[{"text":"intermediate prose"}]}]}"#).unwrap();
    let partial = fixture.send("/api/v1/runs/harness-digest-25-legacy-running", "");
    assert_eq!(
        serde_json::from_slice::<Value>(&partial.body).unwrap()["running"],
        true
    );
    assert!(!partial.cache.contains("immutable"));
    let retained = fixture.source.parent().unwrap().join("transcripts");
    fs::create_dir(&retained).unwrap();
    fs::copy(
        normal.join("transcript.ndjson"),
        retained.join("harness-digest-100-normal.ndjson"),
    )
    .unwrap();
    fs::remove_dir_all(normal).unwrap();
    assert_eq!(
        fixture
            .send("/api/v1/runs/harness-digest-100-normal", "")
            .code,
        "200 OK"
    );
    // Recorded notes and aside requests remain read-only evidence through
    // GET /api/v1/session; the dashboard write routes are gone.
    let root = fixture.source.parent().unwrap().parent().unwrap();
    let notes = session_store::Notes::new(root, "2026-10-28");
    let note = notes.add("no new pharma", true, 100).unwrap();
    let session = fixture.send("/api/v1/session?date=2026-10-28", "");
    assert_eq!(session.code, "200 OK");
    let session_body: Value = serde_json::from_slice(&session.body).unwrap();
    assert_eq!(session_body["notes"]["notes"][0]["id"], note.id);
    assert_eq!(session_body["notes"]["notes"][0]["text"], "no new pharma");
    assert_eq!(session_body["notes"]["version"], 1);
    let aside_dir = root.join(session_store::ROOT_PREFIX).join("2026-10-28");
    fs::create_dir_all(&aside_dir).unwrap();
    fs::write(
            aside_dir.join("aside-request.json"),
            r#"{"id":"aside-1","session":"2026-10-28","question":"why was pharma skipped?","at_ms":100}"#,
        )
        .unwrap();
    let session = fixture.send("/api/v1/session?date=2026-10-28", "");
    let session_body: Value = serde_json::from_slice(&session.body).unwrap();
    assert_eq!(session_body["status"]["aside_request"]["id"], "aside-1");
    assert_eq!(
        session_body["status"]["aside_request"]["question"],
        "why was pharma skipped?"
    );
    for path in [
        "/api/v1/session/notes",
        "/api/v1/session/notes/clear",
        "/api/v1/session/notes/keep",
        "/api/v1/session/aside",
    ] {
        assert_eq!(
            fixture.post(path, r#"{"session":"2026-10-28"}"#).code,
            "405 Method Not Allowed"
        );
        assert_eq!(
            fixture.send(path, "").code,
            "404 Not Found",
            "removed route {path}"
        );
    }
    assert_eq!(
        fixture.send("/api/v1/session?date=2026-02-30", "").code,
        "400 Bad Request"
    );
}
#[test]
fn blobs_are_allowlisted_sliced_pointer_selected_and_redacted_including_encoded_json() {
    let fixture = Fixture::new();
    let dir = fixture.add("harness-digest-100-normal", Some("accepted"));
    let name = format!("tool-{}.result.json", digest(b"one"));
    let path = format!("/api/v1/runs/harness-digest-100-normal/blobs/{name}");
    let full = fixture.send(&path, "");
    assert_eq!(full.code, "200 OK");
    let body: Value = serde_json::from_slice(&full.body).unwrap();
    assert_eq!(body["cash"], "10");
    let text = String::from_utf8_lossy(&full.body);
    assert!(!text.contains("planted-fixture-secret"));
    assert!(!text.contains("opaque-nested-fixture"));
    assert!(!text.contains("id-fixture-secret"));
    assert_eq!(
        serde_json::from_str::<Value>(body["output"].as_str().unwrap()).unwrap()["cash"],
        10
    );
    let sliced = fixture.send(&format!("{path}?offset=3&len=7"), "");
    assert_eq!(sliced.body, full.body[3..10]);
    assert_eq!(sliced.blob_size, Some(full.body.len()));
    let wire = String::from_utf8(encode(sliced)).unwrap();
    assert!(wire.contains("X-Blob-Size:"));
    assert!(wire.contains("Vary: Authorization"));
    let pointer = fixture.send(&format!("{path}%23%2Fcash"), "");
    assert_eq!(
        serde_json::from_slice::<Value>(&pointer.body).unwrap(),
        "10"
    );
    let input = fixture.send(
        "/api/v1/runs/harness-digest-100-normal/blobs/turn-00-request.json%23%2Frequest%2Finput",
        "",
    );
    assert_eq!(input.code, "200 OK");
    assert!(!String::from_utf8_lossy(&input.body).contains("request-fixture-secret"));
    assert_eq!(
        fixture
            .send(
                &format!("{path}?len={}", fixture.settings.blob_slice_max_bytes + 1),
                ""
            )
            .code,
        "400 Bad Request"
    );
    assert_eq!(
        fixture
            .send(&format!("{path}?offset={}", full.body.len() + 1), "")
            .code,
        "416 Range Not Satisfiable"
    );
    for path in [
        "/api/v1/runs/../blobs/output.json",
        "/api/v1/runs/%2e%2e",
        "/api/v1/runs/harness-digest-100-normal/blobs/..%2F..%2Fsecret",
        "/api/v1/runs/harness-digest-100-normal/blobs/.env",
        "/api/v1/runs/harness-digest-100-normal/blobs/unknown.json",
    ] {
        assert_eq!(fixture.send(path, "").code, "404 Not Found", "{path}");
    }
    fs::remove_file(dir.join(&name)).unwrap();
    std::os::unix::fs::symlink(dir.join("output.json"), dir.join(&name)).unwrap();
    assert_eq!(fixture.send(&path, "").code, "404 Not Found");
    fs::remove_file(dir.join(&name)).unwrap();
    fs::hard_link(dir.join("output.json"), dir.join(&name)).unwrap();
    assert_eq!(fixture.send(&path, "").code, "404 Not Found");
}
#[test]
fn prompts_validate_hash_and_request_blobs_rebuild_deltas() {
    let fixture = Fixture::new();
    let dir = fixture.add("harness-digest-100-normal", Some("accepted"));
    let model = fixture.source.parent().unwrap();
    fs::create_dir(model.join("prompts")).unwrap();
    let text = "Fixture system instructions";
    let sha = digest(text.as_bytes());
    fs::write(model.join(format!("prompts/{sha}.md")), text).unwrap();
    let reply = fixture.send(&format!("/api/v1/prompts/{sha}"), "");
    assert_eq!(reply.code, "200 OK");
    assert_eq!(reply.body, text.as_bytes());
    assert_eq!(reply.kind, "text/plain; charset=utf-8");
    assert!(reply.cache.contains("immutable"));
    for sha in ["bad".to_owned(), "A".repeat(64), "f".repeat(64)] {
        assert_eq!(
            fixture.send(&format!("/api/v1/prompts/{sha}"), "").code,
            "404 Not Found"
        );
    }
    let mut frame = super::super::super::harness::Frame {
        role: "digest".into(),
        input: vec![json!({"role":"user","content":"one"})],
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
    let instructions = digest(
        super::super::super::harness::frame_instructions(&frame)
            .unwrap()
            .as_bytes(),
    );
    let mut archive = super::super::super::transcript::RequestArchive::default();
    archive
        .write(
            &dir,
            0,
            &frame,
            &serde_json::to_string(&frame).unwrap(),
            &instructions,
        )
        .unwrap();
    frame.input.push(json!({"role":"user","content":"two"}));
    archive
        .write(
            &dir,
            1,
            &frame,
            &serde_json::to_string(&frame).unwrap(),
            &instructions,
        )
        .unwrap();
    let reply = fixture.send(
        "/api/v1/runs/harness-digest-100-normal/blobs/turn-01-request.json",
        "",
    );
    assert_eq!(reply.code, "200 OK");
    let body: Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["request"]["input"].as_array().unwrap().len(), 2);
}
