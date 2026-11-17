use super::*;
#[test]
fn runs_endpoint_validates_filters_and_revalidates_index_data() {
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("sessions/2026-10-28/model");
    fs::create_dir_all(&model).unwrap();
    let line = super::super::super::transcript::RunIndexLine {
        v: 1,
        run: "harness-digest-1-fixture".into(),
        role: "digest".into(),
        model: "fixture".into(),
        trigger_kind: Some("fill".into()),
        started_ms: 1,
        duration_ms: 2,
        outcome: "accepted".into(),
        tokens_in: Some(1),
        tokens_out: Some(2),
        cost_estimate_inr: None,
        tool_calls: 0,
        tool_failures: 0,
        instruments: vec![],
        bytes: 1,
        mode: "paper".into(),
        profile_sha256: None,
    };
    super::super::super::transcript::append_index_line(&model, &line).unwrap();
    let source = model.join("live-stream.json");
    let view = Arc::new(Mutex::new(serde_json::json!({})));
    let send = |query: &str, extra: &str| {
        dispatch_source(
            &format!(
                "GET /api/v1/runs{query} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n{extra}\r\n"
            ),
            b"",
            "fixture",
            &view,
            None,
            None,
            Some(&source),
            &DashboardSettings::default(),
        )
    };
    let first = send("?role=digest", "");
    assert_eq!(first.code, "200 OK");
    assert_eq!(first.cache, "private, no-cache");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&first.body).unwrap()["runs"][0]["run"],
        line.run
    );
    let cached = send(
        "?role=digest",
        &format!("If-None-Match: {}\r\n", first.etag.unwrap()),
    );
    assert_eq!(cached.code, "304 Not Modified");
    assert!(cached.body.is_empty());
    for query in [
        "?limit=0",
        "?limit=101",
        "?date=bad",
        "?failed=0",
        "?cursor=bad",
        "?unknown=1",
        "?identity=all-access",
        "?role=digest&role=decide",
    ] {
        assert_eq!(send(query, "").code, "400 Bad Request", "{query}");
    }
}
#[test]
fn meta_is_authenticated_conditional_and_only_projects_explicit_identity() {
    for (portfolio, severity, classification) in [
        (
            serde_json::json!({"stopped":true,"halted":false,"closed":false}),
            "info",
            "expected_not_failure",
        ),
        (
            serde_json::json!({"stopped":true,"halted":true,"closed":false}),
            "critical",
            "operator_review_required",
        ),
        (
            serde_json::json!({"stopped":true}),
            "critical",
            "operator_review_required",
        ),
    ] {
        let alerts = super::derived_alerts(&serde_json::json!({"portfolio":portfolio}));
        let stopped = alerts
            .iter()
            .find(|row| row["id"] == "book_stopped")
            .unwrap();
        assert_eq!(stopped["severity"], severity);
        assert_eq!(super::stop_classification(&portfolio), Some(classification));
        assert_eq!(
            stopped["message"]
                .as_str()
                .unwrap()
                .contains("expected_not_failure"),
            severity == "info"
        );
    }

    let activity = serde_json::json!({
        "session":"2026-10-29",
        "conversion":{"candidates_evaluated":12,"fills_executed":1},
        "financials":{"net_after_cost_pnl":"13.00"},
        "experiment_attributions":[{"source_hash":"fixture-hash","net_after_cost_pnl":"13.00"}]
    });
    let operational = serde_json::json!({
        "version":1,
        "captured_at_ms":900_000,
        "supervisor":{
            "status":"unknown",
            "config":"unavailable",
            "config_reason":"planner_usage_unavailable",
            "state":"present",
            "pending_source":"corrupt",
            "pending":null,
            "research":{
                "config":"configured",
                "enabled":true,
                "due":true,
                "state":"due",
                "reason":null,
                "evidence_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "evidence_count":1,
                "cutoff_ms":901000,
                "deferred_until_ms":null,
                "pending_source":"no_pending",
                "pending":null,
                "as_of_ms":900000
            }
        },
        "calendar":{"source":"NSE/CMTR/71775","covered_to":"2026-12-31"},
        "alert_transport":{"status":"missing","counts":null}
    });
    // Captured read-only research projections. The API must copy these exact
    // objects and timestamps; it must not sample a new clock or recompute state.
    let owner_research = serde_json::json!({
        "version":1,
        "status":"unavailable",
        "reason":"planner_config_unavailable",
        "activation_authorized":false,
        "as_of_ms":900_000
    });
    let research_tasks = serde_json::json!({
        "status":"unavailable",
        "reason":"candidate_task_state_unavailable",
        "observation":{
            "status":"unavailable",
            "reason":"candidate_task_state_unavailable",
            "as_of_ms":900_000,
            "lifecycle":{"status":"unavailable","recorded":null,"matured":null,"milestones":[]}
        }
    });
    let view = Arc::new(Mutex::new(
        serde_json::json!({"paper_only":true,"live_enabled":false,"account":"fixture-alias","authorized_session":"2026-10-29","capital":"5000","portfolio":{"cash":"5000","positions":{},"fills":[]},"private":"must-not-be-served","activity_attribution":activity,"owner_research":owner_research.clone(),"research_tasks":research_tasks.clone()}),
    ));
    let request = |extra: &str| {
        format!(
            "GET /api/v1/meta HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n{extra}\r\n"
        )
    };
    let send = |head: &str| {
        dispatch_source(
            head,
            b"",
            "fixture",
            &view,
            None,
            None,
            None,
            &DashboardSettings::default(),
        )
    };
    let first = send(&request(""));
    assert_eq!(first.code, "200 OK");
    assert_eq!(first.cache, "private, no-cache");
    let body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!(body["mode"], "paper");
    assert_eq!(body["account"], "fixture-alias");
    assert_eq!(body["broker"]["orders"], serde_json::Value::Null);
    assert_eq!(body["limits"]["sse_max_streams"], 3);
    assert!(!String::from_utf8_lossy(&first.body).contains("must-not-be-served"));
    let sessions_reply = send(&request("").replace("/api/v1/meta", "/api/v1/sessions"));
    assert_eq!(sessions_reply.code, "200 OK");
    let sessions_body: serde_json::Value = serde_json::from_slice(&sessions_reply.body).unwrap();
    assert_eq!(sessions_body["items"][0]["mode"], "paper");
    assert_eq!(sessions_body["items"][0]["paper_only"], true);
    assert_eq!(sessions_body["items"][0]["live_enabled"], false);
    let cached = send(&request(&format!(
        "If-None-Match: {}\r\n",
        first.etag.unwrap()
    )));
    assert_eq!(cached.code, "304 Not Modified");
    assert!(cached.body.is_empty());
    for (head, code) in [
        (
            request("").replace("Bearer fixture", "Bearer wrong"),
            "401 Unauthorized",
        ),
        (
            request("").replace("127.0.0.1:8771", "evil.test"),
            "403 Forbidden",
        ),
        (
            request("").replace("/api/v1/meta", "/api/v1/unknown"),
            "404 Not Found",
        ),
        (
            request("").replace("/api/v1/meta", "/api/v1/meta?unknown=1"),
            "400 Bad Request",
        ),
    ] {
        let reply = send(&head);
        assert_eq!(reply.code, code);
        let error: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(error["v"], 1);
        assert!(error.get("account").is_none());
    }
    view.lock().unwrap()["live_enabled"] = serde_json::json!(true);
    let live: serde_json::Value = serde_json::from_slice(&send(&request("")).body).unwrap();
    assert_eq!(live["mode"], "live");
    view.lock().unwrap()["account"] = serde_json::json!({"token":"must-not-be-served"});
    let invalid_alias: serde_json::Value =
        serde_json::from_slice(&send(&request("")).body).unwrap();
    assert!(invalid_alias["account"].is_null());

    // Verify positions endpoint
    view.lock().unwrap()["portfolio"] = serde_json::json!({
        "cash": "5000",
        "positions": {
            "NSE_EQ|INE002A01018": {
                "instrument": "NSE_EQ|INE002A01018",
                "quantity": 10,
                "average_price": "2500.50"
            }
        }
    });
    let pos_reply = send(&request("").replace("/api/v1/meta", "/api/v1/positions"));
    assert_eq!(pos_reply.code, "200 OK");
    let pos_body: serde_json::Value = serde_json::from_slice(&pos_reply.body).unwrap();
    assert_eq!(pos_body["total"], 1);
    assert_eq!(pos_body["positions"].as_array().unwrap().len(), 1);

    // Verify orders endpoint
    view.lock().unwrap()["authorized_session"] = serde_json::json!("2026-10-28");
    view.lock().unwrap()["planner"] = serde_json::json!({
        "actions": [
            {
                "index": 0,
                "action": null,
                "units": 0,
                "reason": "instrument_not_eligible"
            },
            {
                "index": 1,
                "action": {
                    "instrument_key": "NSE_EQ|INE002A01018",
                    "side": "buy",
                    "units": 10,
                    "max_price": "2500.50"
                },
                "units": 10,
                "reason": "accepted"
            }
        ],
        "execution": [{"status": "paper_simulated"}]
    });
    let ord_reply = send(&request("").replace("/api/v1/meta", "/api/v1/orders?date=2026-10-28"));
    assert_eq!(ord_reply.code, "200 OK");
    let ord_body: serde_json::Value = serde_json::from_slice(&ord_reply.body).unwrap();
    assert_eq!(ord_body["total"], 2);
    assert_eq!(ord_body["orders"].as_array().unwrap().len(), 2);
    assert_eq!(
        ord_body["orders"][0]["action"]["instrument_key"],
        "NSE_EQ|INE002A01018"
    );
    assert!(ord_body["orders"][1]["action"].is_null());
    assert!(ord_body["orders"][0].get("fill").is_none());
    assert!(ord_body["orders"][1].get("fill").is_none());
    assert!(ord_body["note"].as_str().unwrap().contains("not fills"));
    let date_mismatch =
        send(&request("").replace("/api/v1/meta", "/api/v1/orders?date=2026-10-29"));
    assert_eq!(date_mismatch.code, "200 OK");
    let date_mismatch_body: serde_json::Value =
        serde_json::from_slice(&date_mismatch.body).unwrap();
    assert_eq!(date_mismatch_body["unavailable"], true);
    assert_eq!(date_mismatch_body["reason"], "requested_session_not_loaded");

    // Verify precomputed summary caching
    view.lock().unwrap()["operational_health"] = operational.clone();
    reset_prepared_summary();
    let s1 = send(&request("").replace("/api/v1/meta", "/api/v1/summary"));
    assert_eq!(s1.code, "200 OK");
    assert_eq!(s1.encoding, None);
    let etag1 = s1.etag.expect("summary should have etag");
    let s2 = send(
        &request(&format!("If-None-Match: {etag1}\r\n")).replace("/api/v1/meta", "/api/v1/summary"),
    );
    assert_eq!(s2.code, "304 Not Modified");
    assert!(s2.body.is_empty());
    let s_gz =
        send(&request("Accept-Encoding: gzip\r\n").replace("/api/v1/meta", "/api/v1/summary"));
    assert_eq!(s_gz.code, "200 OK");
    assert_eq!(s_gz.encoding, Some("gzip"));
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(s_gz.body.as_slice())
        .read_to_end(&mut decoded)
        .expect("summary gzip must decode");
    let plain: serde_json::Value = serde_json::from_slice(&s1.body).unwrap();
    assert_eq!(plain["operational_health"], operational);
    // The captured research projections are copied verbatim, including their
    // producer timestamps and explicit unavailable source. No fake availability
    // or renewed sample is introduced by the GET.
    assert_eq!(plain["owner_research"], owner_research);
    assert_eq!(plain["research_tasks"], research_tasks);
    assert_eq!(plain["owner_research"]["as_of_ms"], 900_000);
    assert_eq!(plain["research_tasks"]["observation"]["as_of_ms"], 900_000);
    assert_eq!(
        plain["operational_health"]["supervisor"]["research"]["as_of_ms"],
        900_000
    );
    assert_eq!(
        plain["operational_health"]["supervisor"]["config"],
        "unavailable"
    );
    assert_eq!(
        plain["operational_health"]["supervisor"]["config_reason"],
        "planner_usage_unavailable"
    );
    assert_eq!(
        plain["operational_health"]["supervisor"]["pending_source"],
        "corrupt"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&decoded)
            .expect("summary gzip must contain JSON, not a nested gzip stream"),
        plain
    );
    reset_prepared_summary();

    let health_reply = health("", &view);
    assert_eq!(health_reply.code, "200 OK");
    let health_body: serde_json::Value = serde_json::from_slice(&health_reply.body).unwrap();
    assert_eq!(health_body["activity_attribution"], activity);
    assert_eq!(health_body["operational_health"], operational);
    assert_eq!(health_body["owner_research"], owner_research);
    assert_eq!(health_body["research_tasks"], research_tasks);
    assert_eq!(health_body["owner_research"]["as_of_ms"], 900_000);
    assert_eq!(health_body["controller_liveness"]["status"], "unknown");
    {
        let mut snapshot = view.lock().unwrap();
        snapshot["as_of_ms"] = serde_json::json!(1);
        snapshot["status"] = serde_json::json!("scanning");
    }
    let stale_reply = health("", &view);
    let stale_body: serde_json::Value = serde_json::from_slice(&stale_reply.body).unwrap();
    assert_eq!(stale_body["status"], "scanning");
    assert_eq!(stale_body["as_of_ms"], 1);
    assert_eq!(stale_body["controller_liveness"]["status"], "stale");
    let now = 1_000_000;
    for (as_of, expected) in [
        (serde_json::Value::Null, "unknown"),
        (serde_json::json!("1000000"), "malformed"),
        (serde_json::json!(0), "malformed"),
        (serde_json::json!(now + 1), "future"),
        (serde_json::json!(now - 360_001), "stale"),
        (serde_json::json!(now - 360_000), "healthy"),
        (serde_json::json!(now), "healthy"),
    ] {
        view.lock().unwrap()["as_of_ms"] = as_of.clone();
        let reply = ops::health_at("", &view, now);
        let projected: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(projected["controller_liveness"]["status"], expected);
        assert_eq!(projected["as_of_ms"], as_of);
        assert_eq!(projected["status"], "scanning");
    }
    {
        let mut snapshot = view.lock().unwrap();
        snapshot["controller_heartbeat"] = serde_json::json!({
            "version":1,"completed_at_ms":now,"next_deadline_ms":now+2_000,"loop_generation":42
        });
    }
    for (at, expected) in [(now + 62_000, "healthy"), (now + 62_001, "stale")] {
        let reply = ops::health_at("", &view, at);
        let projected: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(projected["controller_liveness"]["status"], expected);
        assert_eq!(projected["controller_liveness"]["age_ms"], at - now);
        assert_eq!(projected["controller_liveness"]["max_age_ms"], 62_000);
        assert_eq!(projected["controller_liveness"]["loop_generation"], 42);
        assert_eq!(
            projected["controller_liveness"]["last_completed_at_ms"],
            now
        );
        assert_eq!(
            projected["controller_liveness"]["expected_deadline_ms"],
            now + 2_000
        );
        assert_eq!(projected["controller_heartbeat"]["completed_at_ms"], now);
    }
    view.lock().unwrap()["controller_heartbeat"]["completed_at_ms"] = serde_json::json!(now + 1);
    let reply = ops::health_at("", &view, now);
    let malformed: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(malformed["controller_liveness"]["status"], "malformed");
    // Request time never freshens the controller's captured operational view.
    let reply = ops::health_at("", &view, now + 3_600_000);
    let projected: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(projected["operational_health"], operational);
    assert_eq!(projected["operational_health"]["captured_at_ms"], 900_000);
    assert_eq!(
        projected["operational_health"]["supervisor"]["research"]["as_of_ms"],
        900_000
    );
    assert_eq!(
        projected["operational_health"]["supervisor"]["research"]["state"],
        "due"
    );
    assert_eq!(projected["as_of_ms"], now);
    view.lock().unwrap()["owner_review"] = serde_json::json!({
        "version":1,"status":"due","review_due":true,"as_of_ms":now
    });
    let reply = ops::health_at("", &view, now);
    let projected: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(projected["owner_review"]["status"], "due");
    assert_eq!(projected["owner_review"]["review_due"], true);
    assert_eq!(
        health_body["activity_attribution"]["financials"]["net_after_cost_pnl"],
        "13.00"
    );
    assert_eq!(
        health_body["activity_attribution"]["experiment_attributions"][0]["source_hash"],
        "fixture-hash"
    );
    // An absent captured projection stays absent; the API never fabricates a
    // disabled/unavailable research block on its own.
    {
        let mut snapshot = view.lock().unwrap();
        let object = snapshot.as_object_mut().unwrap();
        object.remove("owner_research");
        object.remove("research_tasks");
    }
    reset_prepared_summary();
    let absent_summary: serde_json::Value =
        serde_json::from_slice(&send(&request("").replace("/api/v1/meta", "/api/v1/summary")).body)
            .unwrap();
    assert!(absent_summary.get("owner_research").is_none());
    assert!(absent_summary.get("research_tasks").is_none());
    let absent_health: serde_json::Value =
        serde_json::from_slice(&ops::health_at("", &view, now).body).unwrap();
    assert!(absent_health.get("owner_research").is_none());
    assert!(absent_health.get("research_tasks").is_none());
}
#[test]
#[ignore = "release-mode Phase 1 budget measurement (600-run fixture)"]
fn phase1_request_budgets_600_run_fixture() {
    use std::time::Instant;
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("sessions/2026-10-28/model");
    fs::create_dir_all(&model).unwrap();
    let run = "harness-digest-202610280000-bench";
    let run_dir = model.join(run);
    fs::create_dir(&run_dir).unwrap();
    fn event(run: &str, seq: u64, kind: &str, fields: serde_json::Value) -> serde_json::Value {
        let mut map = fields.as_object().cloned().unwrap_or_default();
        map.insert("v".into(), serde_json::json!(1));
        map.insert("run".into(), serde_json::json!(run));
        map.insert("seq".into(), serde_json::json!(seq));
        map.insert(
            "at_ms".into(),
            serde_json::json!(1_793_163_000_000i64 + seq as i64),
        );
        map.insert("kind".into(), serde_json::json!(kind));
        serde_json::Value::Object(map)
    }
    let mut events = vec![
        event(
            run,
            0,
            "run_started",
            serde_json::json!({"role":"digest","model":"fixture-model","trigger":"timer","session":"2026-10-28","mode":"paper","prompt_sha":null}),
        ),
        event(
            run,
            1,
            "input",
            serde_json::json!({"turn":0,"bytes":12000,"summary":{},"blob":"turn-00-request.json"}),
        ),
    ];
    let mut tools = 0usize;
    for turn in 0..12usize {
        events.push(event(
                run,
                events.len() as u64,
                "assistant_text",
                serde_json::json!({"turn":turn,"text":"Fixture assistant prose for the transcript budget measurement. ".repeat(8),"truncated":false}),
            ));
        let calls = if turn < 4 { 2 } else { 1 };
        for _ in 0..calls {
            let call = format!("call-{tools:02}");
            events.push(event(
                    run,
                    events.len() as u64,
                    "tool_call",
                    serde_json::json!({"turn":turn,"call":call,"name":"read_evidence","args":{"evidence_id":format!("fixture-{tools}")},"instruments":[]}),
                ));
            events.push(event(
                    run,
                    events.len() as u64,
                    "tool_result",
                    serde_json::json!({"turn":turn,"call":call,"name":"read_evidence","status":"ok","duration_ms":3,"bytes":8192,"summary":{"keys":["evidence_id","status"]},"blob":"tool-00.result.json","error":null}),
                ));
            tools += 1;
        }
    }
    assert_eq!(tools, 16);
    events.push(event(
        run,
        events.len() as u64,
        "final_output",
        serde_json::json!({"turn":11,"output":{"actions":[],"stance":"abstain"}}),
    ));
    events.push(event(
            run,
            events.len() as u64,
            "run_ended",
            serde_json::json!({"outcome":"accepted","reason":null,"duration_ms":900,"tokens_in":500,"tokens_out":40,"cost_estimate_inr":null,"cost_basis":null,"tool_calls":16,"tool_failures":0}),
        ));
    let mut body = String::new();
    for value in &events {
        body.push_str(&serde_json::to_string(value).unwrap());
        body.push('\n');
    }
    fs::write(run_dir.join("transcript.ndjson"), &body).unwrap();
    fs::write(
            run_dir.join("tool-00.result.json"),
            serde_json::to_vec(&serde_json::json!({
                "evidence_id":"fixture-0",
                "status":"ok",
                "rows":(0..64).map(|row| serde_json::json!({"row":row,"value":"x".repeat(96)})).collect::<Vec<_>>(),
            }))
            .unwrap(),
        )
        .unwrap();
    for index in 0..600usize {
        let id = if index == 0 {
            run.to_owned()
        } else {
            format!("harness-digest-20261028{index:04}-bench")
        };
        let line = super::super::super::transcript::RunIndexLine {
            v: 1,
            run: id,
            role: "digest".into(),
            model: "fixture-model".into(),
            trigger_kind: Some("timer".into()),
            started_ms: index as i64,
            duration_ms: 900,
            outcome: "accepted".into(),
            tokens_in: Some(500),
            tokens_out: Some(40),
            cost_estimate_inr: None,
            tool_calls: 16,
            tool_failures: 0,
            instruments: vec![],
            bytes: body.len() as u64,
            mode: "paper".into(),
            profile_sha256: None,
        };
        super::super::super::transcript::append_index_line(&model, &line).unwrap();
    }
    let view = Arc::new(Mutex::new(serde_json::json!({
        "paper_only":true,
        "live_enabled":false,
        "account":"bench-alias",
    })));
    let settings = DashboardSettings::default();
    let source = model.join("live-stream.json");
    let measure = |name: &str, path: &str| {
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n\r\n"
        );
        let mut best = u128::MAX;
        for _ in 0..5 {
            let started = Instant::now();
            let reply = dispatch_source(
                &head,
                b"",
                "fixture",
                &view,
                None,
                None,
                Some(&source),
                &settings,
            );
            assert_eq!(reply.code, "200 OK", "{name}");
            best = best.min(started.elapsed().as_micros());
        }
        println!("{name}_600_us={best}");
        best
    };
    let meta = measure("meta", "/api/v1/meta");
    let runs = measure("runs_list", "/api/v1/runs?limit=50");
    let detail = measure("transcript", &format!("/api/v1/runs/{run}"));
    let blob = measure(
        "blob",
        &format!("/api/v1/runs/{run}/blobs/tool-00.result.json"),
    );
    assert!(meta <= 1_000, "meta exceeded 1 ms: {meta} us");
    assert!(runs <= 5_000, "runs list exceeded 5 ms: {runs} us");
    assert!(detail <= 10_000, "transcript exceeded 10 ms: {detail} us");
    assert!(blob <= 5_000, "blob exceeded 5 ms: {blob} us");
}
#[test]
fn series_projects_receipt_bars_and_equity_without_volume() {
    let date = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
    let instrument = "NSE_EQ|INE00U801010";
    let session_open = minute_to_millis(date, 554).unwrap();
    let history_start = minute_to_millis(date, 555).unwrap();
    let mut store =
        FeatureStore::new(nivra_core::feature_store::FeatureStoreSettings::default()).unwrap();
    store.reset_for_session_at(date, Some(history_start), Some(session_open));
    store.record_feed_health(history_start, true);
    store.record_feed_health(minute_to_millis(date, 558).unwrap(), true);
    store.record_feed_health(minute_to_millis(date, 558).unwrap() + 1_000, false);
    store.record_feed_health(minute_to_millis(date, 560).unwrap(), false);
    for (index, (minute, price)) in [(555, 100.0), (555, 101.0), (557, 102.5), (560, 104.0)]
        .into_iter()
        .enumerate()
    {
        store
            .observe(
                instrument,
                price,
                minute_to_millis(date, minute).unwrap() + 1_000 + index as i64,
            )
            .unwrap();
    }
    let features = Arc::new(Mutex::new(store));
    let equity = Arc::new(Mutex::new(EquitySeries::new(15_000, 100)));
    {
        let mut series = equity.lock().unwrap();
        series.observe(1_000_000, "5000", "5000");
        series.observe(1_020_000, "5000", "5010.25");
        series.observe(1_020_000, "5000", "5011.25");
        series.observe(900_000, "4000", "4000");
    }
    let view = Arc::new(Mutex::new(serde_json::json!({
        "paper_only":true,"live_enabled":false,"account":"fixture-alias",
        "authorized_session":"2026-10-29","as_of_ms":1_760_000_000_000i64,
        "portfolio":{"cash":"5000","positions":{}}
    })));
    let catalog = Arc::new(Mutex::new(Some(serde_json::json!({
        "source":"validated_nse_eq_universe","segment":"NSE_EQ","series":"EQ",
        "file_date":"2026-10-29","session":"2026-10-29","received_at_ms":1_760_000_000_000i64,
        "source_hashes":{"nse_cm_security_sha256":"a","upstox_nse_bod_sha256":"b","upstox_suspended_bod_sha256":"c"},
        "total":2,"items":[
            {"instrument_key":"NSE_EQ|INE00U801010","symbol":"FIXTURE","segment":"NSE_EQ","series":"EQ","tick":"0.05","tick_unit":"INR"},
            {"instrument_key":"NSE_EQ|INE00U801011","symbol":"FIXTURE2","segment":"NSE_EQ","series":"EQ","tick":"0.05","tick_unit":"INR"}
        ]
    }))));
    let send = |path: &str| {
        dispatch_source_with(
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n\r\n"
            ),
            b"",
            "fixture",
            &view,
            None,
            None,
            None,
            &DashboardSettings::default(),
            Some(&features),
            Some(&equity),
            Some(&catalog),
        )
    };
    let reply = send("/api/v1/instruments?limit=1");
    assert_eq!(reply.code, "200 OK");
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["v"], 1);
    assert_eq!(body["mode"], "paper");
    assert_eq!(body["paper_only"], true);
    assert_eq!(body["live_enabled"], false);
    assert_eq!(body["execution_authority"], false);
    assert_eq!(body["account"], "fixture-alias");
    assert_eq!(body["session"], "2026-10-29");
    assert_eq!(body["binding"], "upstox|fixture-alias|2026-10-29");
    assert_eq!(body["source"], "validated_nse_eq_universe");
    assert_eq!(body["source_hashes"]["upstox_nse_bod_sha256"], "b");
    assert_eq!(body["total"], 2);
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["items"][0]["tick_unit"], "INR");
    assert_eq!(body["next"], 1);
    assert!(body.get("portfolio").is_none());
    let reply = send("/api/v1/instruments?cursor=1&limit=1");
    assert_eq!(reply.code, "200 OK");
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["items"][0]["symbol"], "FIXTURE2");
    assert_eq!(body["next"], serde_json::Value::Null);
    for path in [
        "/api/v1/instruments?limit=0",
        "/api/v1/instruments?limit=501",
        "/api/v1/instruments?cursor=-1",
        "/api/v1/instruments?cursor=bad",
        "/api/v1/instruments?unknown=1",
    ] {
        assert_eq!(send(path).code, "400 Bad Request", "{path}");
    }
    // D5 display metadata never expands the original cash execution catalog.
    let raw = serde_json::json!([
        {"instrument_key":"NSE_EQ|INE00U801010","trading_symbol":"FIXTURE","segment":"NSE_EQ","exchange":"NSE","instrument_type":"EQ","lot_size":1,"tick_size":5},
        {"instrument_key":"NSE_FO|SYNTHETIC-F","trading_symbol":"FIXTURE FUTURE","segment":"NSE_FO","exchange":"NSE","instrument_type":"FUT","underlying_key":"NSE_EQ|SYNTHETIC-U","expiry":1800000000000i64,"lot_size":25,"tick_size":5},
        {"instrument_key":"NSE_FO|SYNTHETIC-C","segment":"NSE_FO","instrument_type":"CE","underlying_key":"NSE_EQ|SYNTHETIC-U","expiry":1800000000000i64,"strike_price":100,"lot_size":25,"tick_size":5},
        {"instrument_key":"NSE_FO|SYNTHETIC-P","segment":"NSE_FO","instrument_type":"PE","underlying_key":"NSE_EQ|SYNTHETIC-U","expiry":1800000000000i64,"strike_price":100,"lot_size":25,"tick_size":5},
        {"instrument_key":"NSE_FO|SYNTHETIC-C2","segment":"NSE_FO","instrument_type":"CE","underlying_key":"NSE_EQ|SYNTHETIC-U","expiry":1800000000000i64,"strike_price":110,"lot_size":25,"tick_size":5},
        {"instrument_key":"NSE_FO|SYNTHETIC-AMB1","segment":"NSE_FO","instrument_type":"CE","underlying_key":"NSE_EQ|SYNTHETIC-U","expiry":1800000000000i64,"strike_price":200,"lot_size":25},
        {"instrument_key":"NSE_FO|SYNTHETIC-AMB2","segment":"NSE_FO","instrument_type":"CE","underlying_key":"NSE_EQ|SYNTHETIC-U","expiry":1800000000000i64,"strike_price":200,"lot_size":25},
        {"instrument_key":"BSE_EQ|SYNTHETIC-ETF","segment":"BSE_EQ","exchange":"BSE","instrument_type":"ETF","lot_size":1,"tick_size":1},
        {"instrument_key":"NSE_INDEX|SYNTHETIC-I","segment":"NSE_INDEX","instrument_type":"INDEX","lot_size":0,"tick_size":0},
        {"instrument_key":"NSE_FO|SYNTHETIC-DUP","segment":"NSE_FO","instrument_type":"CE"},
        {"instrument_key":"NSE_FO|SYNTHETIC-DUP","segment":"NSE_FO","instrument_type":"PE"},
        {"instrument_key":"BSE_FO|WRONG-SEGMENT","segment":"NSE_FO","instrument_type":"CE"}
    ]);
    let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut zip, &serde_json::to_vec(&raw).unwrap()).unwrap();
    let bytes = zip.finish().unwrap();
    let hash = crate::runtime::digest(&bytes);
    let cash = catalog.lock().unwrap().as_ref().unwrap().clone();
    let settings = crate::runtime::instrument_display::Settings::default();
    let display = crate::runtime::instrument_display::project(
        &bytes,
        &settings,
        &cash,
        123456,
        &hash,
        "fixture_complete_bod",
    )
    .unwrap();
    assert_eq!(display["rejected_rows"], 3);
    assert_eq!(display["segments"]["BSE_EQ"], 1);
    assert_eq!(
        display["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "index")
            .unwrap()["lot_size"],
        serde_json::Value::Null
    );
    let mut limited = settings.clone();
    limited.max_rows = 1;
    assert!(
        crate::runtime::instrument_display::project(
            &bytes, &limited, &cash, 123456, &hash, "fixture"
        )
        .is_err()
    );
    limited = settings.clone();
    limited.max_decoded_bytes = 1;
    assert!(
        crate::runtime::instrument_display::project(
            &bytes, &limited, &cash, 123456, &hash, "fixture"
        )
        .is_err()
    );
    assert!(
        crate::runtime::instrument_display::project(
            &bytes,
            &settings,
            &cash,
            123456,
            "wrong-hash",
            "fixture"
        )
        .is_err()
    );
    crate::runtime::instrument_display::storage_fixture(&bytes, &cash);
    catalog.lock().unwrap().as_mut().unwrap()["display"] = display;
    let body: serde_json::Value =
        serde_json::from_slice(&send("/api/v1/instruments?scope=all&kind=future").body).unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["lot_size"], "25");
    assert_eq!(body["items"][0]["tick"], serde_json::Value::Null);
    assert_eq!(body["items"][0]["provider_tick"], "5");
    assert_eq!(body["items"][0]["cash_execution_verified"], false);
    assert_eq!(body["received_at_ms"], 123456);
    assert_eq!(body["execution_authority"], false);
    let body: serde_json::Value =
        serde_json::from_slice(&send("/api/v1/instruments").body).unwrap();
    assert_eq!(body["total"], 2);
    let chain = "/api/v1/instruments/chain?underlying_key=NSE_EQ%7CSYNTHETIC-U&expiry_ms=1800000000000&market_segment=NSE_FO";
    let body: serde_json::Value = serde_json::from_slice(&send(chain).body).unwrap();
    assert_eq!(body["total"], 3);
    assert_eq!(body["futures_total"], 1);
    assert_eq!(body["items"][0]["strike"], "100");
    assert_eq!(
        body["items"][0]["call"]["instrument_key"],
        "NSE_FO|SYNTHETIC-C"
    );
    assert_eq!(body["items"][1]["put"], serde_json::Value::Null);
    assert_eq!(body["items"][2]["ambiguous_call"], true);
    assert_eq!(body["items"][2]["call"], serde_json::Value::Null);
    assert_eq!(body["quotes_available"], false);
    assert_eq!(body["margin_available"], false);
    assert_eq!(body["execution_authority"], false);
    assert_eq!(
        send("/api/v1/instruments/chain?underlying_key=foo&expiry_ms=bad").code,
        "400 Bad Request"
    );
    assert_eq!(
        send("/api/v1/instruments?scope=invalid").code,
        "400 Bad Request"
    );
    *catalog.lock().unwrap() = None;
    let reply = send("/api/v1/instruments");
    assert_eq!(reply.code, "200 OK");
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["unavailable"], true);
    assert_eq!(body["reason"], "universe_not_prepared");
    assert_eq!(body["execution_authority"], false);
    assert!(body["items"].is_null());
    let from = minute_to_millis(date, 550).unwrap();
    let to = minute_to_millis(date, 560).unwrap();
    let reply = send(&format!(
        "/api/v1/series?instrument=NSE_EQ%7CINE00U801010&kind=receipt-minute-bar&from={from}&to={to}"
    ));
    assert_eq!(reply.code, "200 OK");
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["mode"], "paper");
    assert_eq!(body["source"], "receipt_derived_feature_store");
    assert_eq!(body["instrument_key"], instrument);
    let points = body["points"].as_array().unwrap();
    assert_eq!(points.len(), 3);
    assert_eq!(points[0]["close"], "101");
    assert_eq!(points[1]["close"], "102.5");
    assert_eq!(points[2]["close"], "104");
    assert_eq!(points[0]["volume"], serde_json::Value::Null);
    assert_eq!(points[0]["tick_count"], 2);
    assert_eq!(points[0]["certified_ohlcv"], false);
    assert_eq!(points[0]["price_kind"], "receipt_price");
    assert_eq!(body["coverage"]["expected"], 6);
    assert_eq!(body["coverage"]["observed"], 3);
    assert_eq!(body["coverage"]["gap_count"], 3);
    assert_eq!(body["coverage"]["store_start_ms"], history_start);
    assert_eq!(body["coverage"]["session_open_ms"], session_open);
    assert_eq!(body["coverage"]["store_started_after_open"], true);
    let gaps = body["coverage"]["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 2);
    assert_eq!(gaps[0]["from_ms"], minute_to_millis(date, 556).unwrap());
    assert_eq!(gaps[0]["to_ms"], minute_to_millis(date, 556).unwrap());
    assert_eq!(gaps[0]["reason"], "no_trade");
    assert_eq!(gaps[0]["minutes"], 1);
    assert_eq!(gaps[1]["from_ms"], minute_to_millis(date, 558).unwrap());
    assert_eq!(gaps[1]["to_ms"], minute_to_millis(date, 559).unwrap());
    assert_eq!(gaps[1]["reason"], "unobserved");
    assert_eq!(gaps[1]["minutes"], 2);
    assert!(!String::from_utf8_lossy(&reply.body).contains("volume\":1"));
    let equity_reply = send("/api/v1/series?kind=account-equity");
    assert_eq!(equity_reply.code, "200 OK");
    let equity_body: serde_json::Value = serde_json::from_slice(&equity_reply.body).unwrap();
    let points = equity_body["points"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(points[1]["equity"], "5011.25");
    assert_eq!(points[1]["mark_basis"], "recorded_position_marks");
    assert_eq!(equity_body["source"], "recorded_portfolio_checkpoints");
    for path in [
        "/api/v1/series?kind=receipt-minute-bar",
        "/api/v1/series?instrument=bad%22key&kind=receipt-minute-bar",
        "/api/v1/series?instrument=NSE_EQ%7CINE00U801010&kind=unknown",
        "/api/v1/series?kind=account-equity&from=10&to=5",
        &format!(
            "/api/v1/series?kind=account-equity&from=1&to={}",
            86_400_002
        ),
        "/api/v1/series?kind=account-equity&limit=0",
        "/api/v1/series?kind=account-equity&unknown=1",
    ] {
        assert_eq!(send(path).code, "400 Bad Request", "{path}");
    }
}
#[test]
fn price_batch_is_bounded_session_tagged_and_has_no_model_cursor_id() {
    let date = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
    let mut store =
        FeatureStore::new(nivra_core::feature_store::FeatureStoreSettings::default()).unwrap();
    store.reset_for_session(date);
    for (key, minute, price) in [("A", 555, 10.0), ("B", 556, 20.0), ("C", 557, 30.0)] {
        store
            .observe(key, price, minute_to_millis(date, minute).unwrap())
            .unwrap();
    }
    let features = Arc::new(Mutex::new(store));
    let settings = DashboardSettings {
        price_batch_instruments: 2,
        ..DashboardSettings::default()
    };
    let (seq, frame) = price_batch_frame(&features, &settings, 7, 1_760_000_000_000).unwrap();
    assert_eq!(seq, 7);
    let text = String::from_utf8(frame).unwrap();
    assert!(text.starts_with("event: price_batch\ndata: "));
    assert!(!text.contains("\nid:"));
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(body["session"], "2026-10-29");
    assert_eq!(body["batch_seq"], 7);
    assert_eq!(body["source"], "receipt_derived_feature_store");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["instrument_key"], "C");
    assert_eq!(items[1]["instrument_key"], "B");
    assert_eq!(items[0]["execution_authority"], false);
    assert_eq!(items[0]["price_kind"], "receipt_price");
}
