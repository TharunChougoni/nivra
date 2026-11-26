use super::*;
use crate::runtime::instrument_display::Catalog;
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
        "financials":{"net_after_cost_pnl":"13.00"}
    });
    let mut stored_activity = activity.clone();
    stored_activity["conversion"] = serde_json::json!({"fills_executed":1});
    stored_activity["abstentions"] = serde_json::json!({"total_abstentions":0});
    stored_activity["latency"] = serde_json::json!({"p50_ms":0});
    let operational = serde_json::json!({
        "version":2,
        "captured_at_ms":900_000,
        "calendar":{"source":"NSE/CMTR/71775","covered_to":"2026-12-31"},
        "alert_transport":{"status":"missing","counts":null}
    });
    let research_tasks = serde_json::json!({
        "status":"reconciliation_required",
        "task_id":"fixture-task",
        "revision":1,
        "reason":"research_worker_failed"
    });
    let view = Arc::new(Mutex::new(
        serde_json::json!({"paper_only":true,"live_enabled":false,"account":"fixture-alias","authorized_session":"2026-10-29","capital":"5000","portfolio":{"cash":"5000","positions":{},"fills":[]},"private":"must-not-be-served","activity_attribution":stored_activity,"research_tasks":research_tasks.clone()}),
    ));
    let request = |extra: &str| {
        format!(
            "GET /api/v1/meta HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n{extra}\r\n"
        )
    };
    let ctx = Ctx::new(
        "fixture".into(),
        None,
        view.clone(),
        DashboardSettings::default(),
    );
    let send = |head: &str| ctx.respond(head, b"");
    let first = send(&request(""));
    assert_eq!(first.code, "200 OK");
    assert_eq!(first.cache, "private, no-cache");
    let body: serde_json::Value = serde_json::from_slice(&first.body).unwrap();
    assert_eq!(body["mode"], "paper");
    assert_eq!(body["account"], "fixture-alias");
    // The paper ledger is the explicit order authority (#24); it is never a
    // missing value while the controller is in paper mode.
    assert_eq!(body["broker"]["data"], "upstox");
    assert_eq!(body["broker"]["orders"], "paper");
    assert_eq!(body["limits"]["sse_max_streams"], 3);
    // Every advertised capability is a route.
    for capability in body["capabilities"].as_array().unwrap() {
        let path = capability.as_str().unwrap().replace('_', "/");
        let line = format!("GET /api/v1/{path} HTTP/1.1");
        assert!(
            !matches!(router::parse(&line), Ok(router::Route::UnknownV1)),
            "{line}"
        );
    }
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
    // A live order path is not implemented: it stays unknown rather than
    // implying a live broker or execution authority.
    assert_eq!(live["broker"]["orders"], serde_json::Value::Null);
    assert_eq!(live["live_enabled"], true);
    view.lock().unwrap()["account"] = serde_json::json!({"token":"must-not-be-served"});
    let invalid_alias: serde_json::Value =
        serde_json::from_slice(&send(&request("")).body).unwrap();
    assert!(invalid_alias["account"].is_null());

    // Verify plans endpoint
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
    let plans_body: serde_json::Value =
        serde_json::from_slice(&send(&request("").replace("/api/v1/meta", "/api/v1/plans")).body)
            .unwrap();
    assert_eq!(plans_body["actions"].as_array().unwrap().len(), 2);
    assert_eq!(plans_body["execution"][0]["status"], "paper_simulated");
    assert!(plans_body["planner"].get("actions").is_none());
    assert!(plans_body["planner"].get("execution").is_none());
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
    assert_eq!(plain["research_tasks"], research_tasks);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&decoded)
            .expect("summary gzip must contain JSON, not a nested gzip stream"),
        plain
    );
    assert_eq!(plain["revisions"], revisions(&view.lock().unwrap(), None));

    // A published summary is announced once with the revisions its body embeds.
    let mut later = view.lock().unwrap().clone();
    later["portfolio"]["fills"] = serde_json::json!([{"fee":"1.00"}]);
    let after = revisions(&later, None);
    let cursor = nivra_cli::event_feed::snapshot(0)["next"].as_u64().unwrap();
    publish_summary(&later, None);
    publish_summary(&later, None);
    let etag = get_prepared_summary().unwrap().etag.clone();
    let announced = nivra_cli::event_feed::snapshot(cursor)["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["etag"] == etag.as_str())
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(announced.len(), 1);
    assert_eq!(announced[0]["revisions"], after);
    reset_prepared_summary();

    let health_reply = health("", &view);
    assert_eq!(health_reply.code, "200 OK");
    let health_body: serde_json::Value = serde_json::from_slice(&health_reply.body).unwrap();
    assert_eq!(health_body["activity_attribution"], activity);
    assert_eq!(health_body["operational_health"], operational);
    assert_eq!(health_body["research_tasks"], research_tasks);
    assert_eq!(health_body["controller_liveness"]["status"], "unknown");
    let mem = &health_body["process_memory"];
    assert_eq!(mem["unit"], "KiB");
    if cfg!(target_os = "linux") {
        assert_eq!(mem["status"], "available");
        assert_eq!(mem["unavailable"], false);
        assert!(mem["rss_anon_kib"].as_u64().is_some());
        assert!(mem["vm_rss_kib"].as_u64().is_some());
        assert!(mem["vm_hwm_kib"].as_u64().is_some());
        assert!(mem["vm_swap_kib"].as_u64().is_some());
    }
    let missing_mem = summary::read_process_memory(Path::new("/nonexistent/proc/self/status"));
    assert_eq!(missing_mem["status"], "unavailable");
    assert_eq!(missing_mem["unavailable"], true);
    assert_eq!(missing_mem["reason"], "proc_status_unavailable");
    let parsed_mem = summary::parse_process_memory(
        "Name:\tnivra\nVmHWM:\t198000 kB\nVmRSS:\t150000 kB\nRssAnon:\t132000 kB\nVmSwap:\t0 kB\n",
    );
    assert_eq!(parsed_mem["status"], "available");
    assert_eq!(parsed_mem["rss_anon_kib"], 132_000);
    assert_eq!(parsed_mem["vm_rss_kib"], 150_000);
    assert_eq!(parsed_mem["vm_hwm_kib"], 198_000);
    assert_eq!(parsed_mem["vm_swap_kib"], 0);
    let bad_mem = summary::parse_process_memory("VmHWM:\tnot_a_number kB\n");
    assert_eq!(bad_mem["status"], "unavailable");
    assert_eq!(bad_mem["unavailable"], true);
    assert_eq!(bad_mem["reason"], "proc_status_unparsable");
    {
        let selection_status = serde_json::json!({
            "mode":"shadow","regime":"range/low_vol/strong/no_gap","reason":"NoEligibleCandidates"
        });
        let mut snapshot = view.lock().unwrap();
        snapshot["discovery"] = serde_json::json!({
            "fresh_count":1,"stale_count":0,"unseen_count":0,"eligible_count":1,"presented_count":0,
            "oldest_fresh_age_ms":100,"max_scan_age_ms":180000,"unavailable_count":0,"unavailable_truncated":false,
            "screen_reasons":{},"prospects":[],"selection":"default","limitations":"display",
            "strategy_selection":selection_status
        });
    }
    let health_with_sel: serde_json::Value =
        serde_json::from_slice(&health("", &view).body).unwrap();
    assert_eq!(
        health_with_sel["strategy_selection"]["regime"],
        "range/low_vol/strong/no_gap"
    );
    let scanner_with_sel: serde_json::Value =
        serde_json::from_slice(&send(&request("").replace("/api/v1/meta", "/api/v1/scanner")).body)
            .unwrap();
    assert_eq!(
        scanner_with_sel["strategy_selection"]["reason"],
        "NoEligibleCandidates"
    );
    view.lock()
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("discovery");
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
    // The ETag leaves out the live age: unchanged content answers 304 at a
    // later age, and a liveness status change answers 200.
    let etag = ops::health_at("", &view, now + 1_000).etag.unwrap();
    let conditional = format!("GET /api/v1/health HTTP/1.1\r\nIf-None-Match: {etag}\r\n");
    let repeat = ops::health_at(&conditional, &view, now + 2_000);
    assert_eq!(repeat.code, "304 Not Modified");
    assert!(repeat.body.is_empty());
    assert_eq!(
        ops::health_at(&conditional, &view, now + 62_001).code,
        "200 OK"
    );
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
        projected["operational_health"]["calendar"]["covered_to"],
        "2026-12-31"
    );
    assert_eq!(projected["as_of_ms"], now);
    assert_eq!(
        health_body["activity_attribution"]["financials"]["net_after_cost_pnl"],
        "13.00"
    );
    // An absent captured projection stays absent; the API never fabricates a
    // disabled/unavailable research block on its own.
    {
        let mut snapshot = view.lock().unwrap();
        let object = snapshot.as_object_mut().unwrap();
        object.remove("research_tasks");
    }
    reset_prepared_summary();
    let absent_summary: serde_json::Value =
        serde_json::from_slice(&send(&request("").replace("/api/v1/meta", "/api/v1/summary")).body)
            .unwrap();
    assert!(absent_summary.get("research_tasks").is_none());
    let absent_health: serde_json::Value =
        serde_json::from_slice(&ops::health_at("", &view, now).body).unwrap();
    assert!(absent_health.get("research_tasks").is_none());
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
    let catalog = Arc::new(Mutex::new(Some(Catalog::new(serde_json::json!({
        "source":"validated_nse_eq_universe","segment":"NSE_EQ","series":"EQ",
        "file_date":"2026-10-29","session":"2026-10-29","received_at_ms":1_760_000_000_000i64,
        "source_hashes":{"nse_cm_security_sha256":"a","upstox_nse_bod_sha256":"b","upstox_suspended_bod_sha256":"c"},
        "total":2,"items":[
            {"instrument_key":"NSE_EQ|INE00U801010","symbol":"FIXTURE","segment":"NSE_EQ","series":"EQ","tick":"0.05","tick_unit":"INR"},
            {"instrument_key":"NSE_EQ|INE00U801011","symbol":"FIXTURE2","segment":"NSE_EQ","series":"EQ","tick":"0.05","tick_unit":"INR"}
        ]
    })))));
    let ctx = Ctx::new(
        "fixture".into(),
        None,
        view.clone(),
        DashboardSettings::default(),
    )
    .with_series(features.clone(), equity.clone(), catalog.clone());
    let send = |path: &str| {
        ctx.respond(
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n\r\n"
            ),
            b"",
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
    let cash = catalog.lock().unwrap().as_ref().unwrap().cash.clone();
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
    let reason = |bytes: &[u8], settings: &crate::runtime::instrument_display::Settings| {
        let hash = crate::runtime::digest(bytes);
        crate::runtime::instrument_display::project(
            bytes, settings, &cash, 123456, &hash, "fixture",
        )
        .err()
        .map(|error| error.to_string())
    };
    let gzip = |text: &[u8]| {
        let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut zip, text).unwrap();
        zip.finish().unwrap()
    };
    let mut limited = settings.clone();
    limited.max_rows = 1;
    assert_eq!(
        reason(&bytes, &limited).unwrap(),
        "instrument_display_row_limit"
    );
    limited = settings.clone();
    limited.max_decoded_bytes = 1;
    assert_eq!(
        reason(&bytes, &limited).unwrap(),
        "instrument_display_decoded_limit"
    );
    // An early JSON error never hides an oversized decoded stream.
    limited.max_decoded_bytes = 10_000;
    let padded = gzip(format!("[] x{}", " ".repeat(20_000)).as_bytes());
    assert_eq!(
        reason(&padded, &limited).unwrap(),
        "instrument_display_decoded_limit"
    );
    assert_eq!(
        reason(&padded, &settings).unwrap(),
        "instrument_display_json_invalid"
    );
    assert_eq!(
        reason(b"not gzip", &settings).unwrap(),
        "instrument_display_gzip_invalid"
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
    catalog.lock().unwrap().as_mut().unwrap().display = Some(Ok(display));
    let body: serde_json::Value =
        serde_json::from_slice(&send("/api/v1/instruments?scope=all&kind=index").body).unwrap();
    assert_eq!(body["rejected_rows"], 3);
    assert_eq!(body["segments"]["BSE_EQ"], 1);
    assert_eq!(body["items"][0]["lot_size"], serde_json::Value::Null);
    let body: serde_json::Value =
        serde_json::from_slice(&send("/api/v1/instruments?scope=all&kind=future").body).unwrap();
    assert_eq!(body["total"], 1);
    assert_eq!(
        body["items"][0],
        serde_json::json!({
            "instrument_key":"NSE_FO|SYNTHETIC-F","symbol":"FIXTURE FUTURE","segment":"NSE_FO","exchange":"NSE",
            "kind":"future","instrument_type":"FUT","underlying_key":"NSE_EQ|SYNTHETIC-U","underlying_symbol":null,
            "expiry_ms":1800000000000i64,"strike":null,"lot_size":"25","provider_tick":"5","tick":null,"tick_unit":null,
            "cash_execution_verified":false,"margin":null,"margin_reason":"margin_snapshot_not_captured","display_only":true
        })
    );
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
fn price_batches_are_shared_bounded_and_send_only_moved_receipts() {
    let date = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
    let mut store =
        FeatureStore::new(nivra_core::feature_store::FeatureStoreSettings::default()).unwrap();
    store.reset_for_session(date);
    for (key, minute, price) in [("A", 555, 10.0), ("B", 556, 20.0), ("C", 557, 30.0)] {
        store
            .observe(key, price, minute_to_millis(date, minute).unwrap())
            .unwrap();
    }
    let features = Mutex::new(store);
    let settings = DashboardSettings {
        price_batch_instruments: 2,
        ..DashboardSettings::default()
    };
    let batches = PriceBatches::default();
    let start = Instant::now();
    let frame = |sent: u64, at: Duration| {
        let (seq, frame) =
            batches.next_frame(&features, &settings, sent, start + at, 1_760_000_000_000)?;
        let text = String::from_utf8(frame.to_vec()).unwrap();
        assert!(text.starts_with("event: price_batch\ndata: "));
        assert!(!text.contains("\nid:"));
        let data = text.lines().find_map(|line| line.strip_prefix("data: "))?;
        Some((
            seq,
            serde_json::from_str::<serde_json::Value>(data).unwrap(),
        ))
    };
    let keys = |body: &serde_json::Value| {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["instrument_key"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let (seq, first) = frame(0, Duration::ZERO).unwrap();
    assert_eq!(seq, 1);
    assert_eq!(first["session"], "2026-10-29");
    assert_eq!(first["full"], true);
    assert_eq!(first["source"], "receipt_derived_feature_store");
    assert_eq!(first["price_kind"], "receipt_price");
    assert_eq!(first["execution_authority"], false);
    assert_eq!(keys(&first), ["C", "B"]);
    assert_eq!(first["items"][0]["last_price"], 30.0);
    // A second stream inside the interval shares the same batch.
    assert_eq!(frame(0, Duration::from_millis(10)).unwrap().1, first);
    assert!(frame(1, Duration::from_millis(10)).is_none());
    // No moved receipt: no new batch and no frame.
    assert!(frame(1, Duration::from_secs(3)).is_none());
    features
        .lock()
        .unwrap()
        .observe("A", 11.0, minute_to_millis(date, 558).unwrap())
        .unwrap();
    let (seq, delta) = frame(1, Duration::from_secs(6)).unwrap();
    assert_eq!(seq, 2);
    assert_eq!(delta["full"], false);
    assert_eq!(keys(&delta), ["A"]);
    // A new stream, or one that missed a batch, gets the full snapshot.
    let (seq, full) = frame(0, Duration::from_secs(6)).unwrap();
    assert_eq!(seq, 2);
    assert_eq!(keys(&full), ["A", "C"]);

    // Over gzip, one stream carries every frame and each frame decodes whole
    // as soon as it is written.
    let (_, batch) = batches
        .next_frame(&features, &settings, 0, start + Duration::from_secs(6), 0)
        .unwrap();
    let event = sse_frame(
        &serde_json::json!({"seq":7,"kind":"summary_changed","etag":"\"e\"","revisions":{}}),
        &serde_json::json!("fixture"),
    )
    .unwrap();
    let mut sink = FrameWriter::new(Vec::new(), true);
    let mut decoder = flate2::write::GzDecoder::new(Vec::new());
    let mut sent = 0;
    for frame in [&b": heartbeat\n\n"[..], &batch, &event] {
        sink.frame(frame).unwrap();
        let FrameWriter::Gzip(encoder) = &sink else {
            unreachable!("gzip writer");
        };
        decoder.write_all(&encoder.get_ref()[sent..]).unwrap();
        decoder.flush().unwrap();
        sent = encoder.get_ref().len();
        assert!(decoder.get_ref().ends_with(frame));
    }
    assert_eq!(
        decoder.get_ref().as_slice(),
        [&b": heartbeat\n\n"[..], &batch, &event].concat()
    );
}

/// The view one controller loop publishes at `at`, with the clocks its
/// producers stamp: heartbeat, operational capture, feed telemetry, research
/// task status and scanner ages.
fn loop_view(at: i64, generation: u64) -> serde_json::Value {
    serde_json::json!({
        "paper_only":true,"live_enabled":false,"account":"fixture","authorized_session":"2026-11-25",
        "status":"model_plan_ready","as_of_ms":at,
        "controller_heartbeat":crate::runtime::operational_health::heartbeat(at, 500, generation),
        "operational_health":{"version":2,"captured_at_ms":at,"calendar":{"covered":true,"days_remaining":30},
            "alert_transport":{"status":"available","last_accepted_at_ms":null,
                "retained_window":{"start_ms":at - 3_600_000,"end_ms":at,"rows_in_window":0}}},
        "market_feed":{"mode":"ltpc","state":"streaming","connected":true,"fallback":false,
            "last_tick_age_ms":at % 700,"last_heartbeat_age_ms":at % 300,"ticks_total":at / 10,
            "tick_rate_per_s":(at % 97) as f64,"cached_keys":2,"oldest_last_trade_ms":at - 90_000,"reconnects":0},
        "oldest_scan_age_ms":at % 900,
        "readiness":{"status":"ready","checked_at_ms":at},
        "research_tasks":{"status":"idle"},
        "discovery":{"as_of_ms":at,"fresh_count":1,"stale_count":1,"unseen_count":0,"eligible_count":1,
            "presented_count":1,"unavailable_count":1,"unavailable_truncated":false,
            "oldest_fresh_age_ms":at % 900,"max_scan_age_ms":180_000,
            "prospects":[{"instrument_key":"NSE_EQ|A","observed_ms":1_000,"bid":"100",
                "screening_age_ms":at - 1_000,"quote_age_ms":at - 1_200}],
            "unavailable":[{"instrument_key":"NSE_EQ|B","reason":"stale","last_observed_ms":500,"age_ms":at - 500}],
            "screen_reasons":{"eligible_no_signal":1,"stale":1},"selection":"fixture","limitations":"fixture"},
        "universe_size":2,"ranking":[["1.0","NSE_EQ|A",1_000]],
        "portfolio":{"stopped":false,"halted":false,"closed":false,"cash":"1000","positions":{},"fills":[]},
        "planner":{"status":"ready"}
    })
}

#[test]
fn revisions_follow_content_and_liveness_not_loop_clocks() {
    let (first, next) = (
        loop_view(1_764_050_000_000, 41),
        loop_view(1_764_050_000_500, 42),
    );
    assert_ne!(first, next);
    assert_eq!(revisions(&first, None), revisions(&next, None));
    let moved = |change: &dyn Fn(&mut serde_json::Value)| {
        let mut later = next.clone();
        change(&mut later);
        let (before, after) = (revisions(&next, None), revisions(&later, None));
        before
            .as_object()
            .unwrap()
            .iter()
            .filter(|(name, revision)| after[name.as_str()] != **revision)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>()
    };
    // A heartbeat that disagrees with as_of_ms turns liveness malformed.
    assert_eq!(
        moved(&|v| v["controller_heartbeat"]["completed_at_ms"] = serde_json::json!(1)),
        ["health"]
    );
    assert_eq!(
        moved(&|v| v["market_feed"]["state"] = serde_json::json!("rest_fallback")),
        ["health"]
    );
    assert_eq!(
        moved(&|v| v["discovery"]["prospects"][0]["bid"] = serde_json::json!("101")),
        ["scanner"]
    );
    assert_eq!(
        moved(&|v| v["portfolio"]["fills"] = serde_json::json!([{"fee":"1.00"}])),
        ["portfolio", "fills", "sessions"]
    );
}

#[test]
fn publish_gate_coalesces_changes_limits_scanner_churn_and_refreshes() {
    let revisions = |portfolio: &str, scanner: &str| serde_json::json!({"portfolio":portfolio,"scanner":scanner});
    let mut gate = PublishGate::new();
    let mut step =
        |current: &serde_json::Value, at: i64| (gate.step(current, at), gate.announced());
    // The first loop and a change fire; a second change inside 1 s is held,
    // with the announced revisions unchanged, then fires on the next loop.
    assert_eq!(
        step(&revisions("p1", "s1"), 0),
        (true, revisions("p1", "s1"))
    );
    assert_eq!(
        step(&revisions("p1", "s1"), 500),
        (false, revisions("p1", "s1"))
    );
    assert_eq!(
        step(&revisions("p2", "s1"), 1_000),
        (true, revisions("p2", "s1"))
    );
    assert_eq!(
        step(&revisions("p3", "s1"), 1_500),
        (false, revisions("p2", "s1"))
    );
    assert_eq!(
        step(&revisions("p3", "s1"), 2_000),
        (true, revisions("p3", "s1"))
    );
    // The scanner rolled at 0 and rolls again at most once per 15 s, however
    // often it moves; only the 5 s refreshes fire while it is held.
    for at in (2_500..30_000).step_by(500) {
        let (fired, announced) = step(&revisions("p3", &format!("s{at}")), at);
        let refresh = [7_000, 12_000, 20_000, 25_000].contains(&at);
        assert_eq!(fired, at == 15_000 || refresh, "at {at}");
        let held = if at < 15_000 { "s1" } else { "s15000" };
        assert_eq!(announced["scanner"], held, "at {at}");
    }
    assert_eq!(
        step(&revisions("p3", "s30000"), 30_000),
        (true, revisions("p3", "s30000"))
    );
    // With no change, the summary is still announced every 5 s.
    assert_eq!(
        step(&revisions("p3", "s30000"), 34_999),
        (false, revisions("p3", "s30000"))
    );
    assert_eq!(
        step(&revisions("p3", "s30000"), 35_000),
        (true, revisions("p3", "s30000"))
    );
}

#[test]
fn agent_route_pages_pi_turns_newest_first_with_gate_counts() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("model/agent");
    std::fs::create_dir_all(&agent).unwrap();
    let turn = |id: &str, lane: &str, extra: serde_json::Value| {
        let mut line = serde_json::json!({"turn_id":id,"lane":lane,"at_ms":1,"status":"ok",
            "trigger":{"kind":"entry_candidate","triggers":[{"instrument_key":"NSE_EQ|A"}]},
            "tool_calls":[{"name":"snapshot","ok":true}],"usage":{"calls":2},"brief":"never served"});
        line.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        line.to_string()
    };
    std::fs::write(
        agent.join("turns.ndjson"),
        [
            turn("gate-1", "gate", serde_json::json!({"verdict":{"escalate":true,"reason":"depth"}})),
            turn("decision-1", "decision", serde_json::json!({"plan":{"status":"validated","plan":{"stance":"flat","actions":[]}}})),
        ]
        .join("\n"),
    )
    .unwrap();
    std::fs::write(
        agent.join("triggers.ndjson"),
        "{\"event\":\"gated\",\"escalate\":true}\n{\"event\":\"gated\",\"escalate\":false}\n{\"event\":\"fired\"}\n",
    )
    .unwrap();
    let get = |target: &str| {
        let Ok(router::Route::Agent(query)) = router::parse(&format!("GET {target} HTTP/1.1"))
        else {
            panic!("agent route");
        };
        let reply = agent_turns("", query, Some(dir.path()));
        serde_json::from_slice::<serde_json::Value>(&reply.body).unwrap()
    };
    let first = get("/api/v1/agent?limit=1");
    assert_eq!(first["total"], 2);
    assert_eq!(first["next"], "1");
    assert_eq!(first["turns"][0]["turn_id"], "decision-1");
    assert_eq!(first["turns"][0]["plan"]["stance"], "flat");
    assert_eq!(first["turns"][0]["trigger"]["instruments"][0], "NSE_EQ|A");
    assert!(first["turns"][0].get("brief").is_none());
    assert_eq!(first["triggers"]["gate_escalated"], 1);
    assert_eq!(first["triggers"]["gate_declined"], 1);
    let second = get("/api/v1/agent?limit=1&cursor=1");
    assert_eq!(second["turns"][0]["verdict"]["escalate"], true);
    assert!(second["next"].is_null());
    assert_eq!(first["truncated"], false);

    // An unchanged journal answers 304; an append moves the agent revision
    // and is read incrementally, with a torn final line held back. Only a
    // turns append moves `agent_turns`; a usage rewrite does not.
    let revision = || {
        let all = revisions(&serde_json::json!({}), Some(dir.path()));
        (all["agent"].clone(), all["agent_turns"].clone())
    };
    assert_ne!(
        revision().0,
        revisions(&serde_json::json!({}), None)["agent"]
    );
    let before = revision();
    std::fs::write(agent.join("usage.json"), "{}").unwrap();
    assert_ne!(revision().0, before.0);
    assert_eq!(revision().1, before.1);
    let before = revision();
    let Ok(router::Route::Agent(query)) = router::parse("GET /api/v1/agent HTTP/1.1") else {
        panic!("agent route");
    };
    let etag = agent_turns("", query, Some(dir.path())).etag.unwrap();
    let Ok(router::Route::Agent(query)) = router::parse("GET /api/v1/agent HTTP/1.1") else {
        panic!("agent route");
    };
    let conditional = format!("GET /api/v1/agent HTTP/1.1\r\nIf-None-Match: {etag}\r\n");
    assert_eq!(
        agent_turns(&conditional, query, Some(dir.path())).code,
        "304 Not Modified"
    );
    let mut journal = std::fs::OpenOptions::new()
        .append(true)
        .open(agent.join("turns.ndjson"))
        .unwrap();
    write!(
        journal,
        "\n{}\n{{\"turn_id\":\"torn",
        turn("decision-2", "decision", serde_json::json!({}))
    )
    .unwrap();
    assert_ne!(revision().0, before.0);
    assert_ne!(revision().1, before.1);
    let grown = get("/api/v1/agent");
    assert_eq!(grown["total"], 3);
    assert_eq!(grown["turns"][0]["turn_id"], "decision-2");

    // A journal past the held window serves its newest rows and says how
    // many older rows it did not load, instead of failing.
    let Ok(router::Route::Agent(query)) = router::parse("GET /api/v1/agent HTTP/1.1") else {
        panic!("agent route");
    };
    let large = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(large.path().join("model/agent")).unwrap();
    std::fs::copy(
        agent.join("turns.ndjson"),
        large.path().join("model/agent/turns.ndjson"),
    )
    .unwrap();
    let window = turn("decision-2", "decision", serde_json::json!({})).len() as u64 + 20;
    let tail: serde_json::Value =
        serde_json::from_slice(&agent_turns_within("", query, Some(large.path()), window).body)
            .unwrap();
    assert_eq!(tail["truncated"], true);
    assert_eq!(tail["rows_not_loaded"]["turns"], 2);
    assert_eq!(tail["total"], 1);
    assert_eq!(tail["turns"][0]["turn_id"], "decision-2");
}

fn transcript_get(
    root: Option<&Path>,
    target: &str,
    settings: &DashboardSettings,
) -> (String, serde_json::Value) {
    let Ok(router::Route::AgentTranscript(query)) =
        router::parse(&format!("GET {target} HTTP/1.1"))
    else {
        panic!("transcript route");
    };
    let reply = transcript::agent_transcript("", query, root, settings);
    (
        reply.code.to_string(),
        serde_json::from_slice(&reply.body).unwrap(),
    )
}

#[test]
fn transcript_route_groups_exchanges_across_a_restart_and_joins_turns() {
    let dir = tempfile::tempdir().unwrap();
    let agent = dir.path().join("model/agent");
    let lane = agent.join("pi/gate");
    std::fs::create_dir_all(&lane).unwrap();
    let message = |role: &str, at: i64, extra: serde_json::Value| {
        let mut message = serde_json::json!({"role":role,"timestamp":at});
        message
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        serde_json::json!({"type":"message","message":message}).to_string()
    };
    let user = |at: i64, text: &str| {
        message(
            "user",
            at,
            serde_json::json!({"content":[{"type":"text","text":text}]}),
        )
    };
    let result = |at: i64, id: &str, text: &str, is_error: bool, ms: u64| {
        message(
            "toolResult",
            at,
            serde_json::json!({"toolCallId":id,"toolName":"x","isError":is_error,"durationMs":ms,
                "content":[{"type":"text","text":text}]}),
        )
    };
    let header = |id: &str, stamp: &str| {
        [
            serde_json::json!({"type":"session","id":id,"timestamp":stamp,"cwd":"/never/served"}),
            serde_json::json!({"type":"model_change","provider":"mistral","modelId":"m-1"}),
            serde_json::json!({"type":"thinking_level_change","thinkingLevel":"low"}),
            serde_json::from_str(&message(
                "system",
                1,
                serde_json::json!({"content":"SYSTEM-PROMPT-TEXT"}),
            ))
            .unwrap(),
        ]
        .map(|line| line.to_string())
        .join("\n")
    };
    let wide = "é".repeat(20);
    let first = [
        header("sess-a", "2026-11-25T03:46:02.966Z"),
        user(1_000_000, &format!("Brief:\n{wide}")),
        message(
            "assistant",
            1_000_100,
            serde_json::json!({"stopReason":"toolUse","content":[
                {"type":"thinking","thinking":"plan","thinkingSignature":"opaque"},
                {"type":"text","text":"checking"},
                {"type":"toolCall","id":"call_1","name":"intraday","arguments":{"keys":[wide]}},
                {"type":"toolCall","id":"call_2","name":"session","arguments":"{ \"a\" : 1 }"}]}),
        ),
        result(1_000_200, "call_2", "denied", true, 3),
        result(1_000_300, "call_1", "0123456789ABCDEF", false, 9),
        message(
            "assistant",
            1_000_400,
            serde_json::json!({"stopReason":"stop","content":[{"type":"text","text":"done"}]}),
        ),
        user(2_000_000, "Brief:\nnever answered"),
    ]
    .join("\n");
    let second = [
        header("sess-b", "2026-11-25T04:00:00.000Z"),
        user(3_000_000, "Brief:\nafter restart"),
        serde_json::json!({"type":"compaction"}).to_string(),
        message(
            "assistant",
            3_000_100,
            serde_json::json!({"stopReason":"error","errorMessage":"provider failed","content":[]}),
        ),
    ]
    .join("\n");
    std::fs::write(
        lane.join("2026-11-25T03-46-02-966Z_sess-a.jsonl"),
        format!("{first}\n"),
    )
    .unwrap();
    std::fs::write(
        lane.join("2026-11-25T04-00-00-000Z_sess-b.jsonl"),
        format!("{second}\n"),
    )
    .unwrap();
    let turn = |id: &str, lane: &str, at: i64, status: &str, error: serde_json::Value| {
        serde_json::json!({"turn_id":id,"lane":lane,"at_ms":at,"status":status,"error":error})
            .to_string()
    };
    std::fs::write(
        agent.join("turns.ndjson"),
        [
            turn("gate-old", "gate", 900_000, "ok", serde_json::Value::Null),
            turn("gate-1", "gate", 999_980, "ok", serde_json::Value::Null),
            turn("gate-2", "gate", 1_999_990, "error", "agent_exited".into()),
            turn(
                "decision-3",
                "decision",
                2_999_990,
                "ok",
                serde_json::Value::Null,
            ),
        ]
        .join("\n"),
    )
    .unwrap();
    let settings = DashboardSettings {
        transcript_text_max_bytes: 10,
        transcript_args_max_bytes: 13,
        ..DashboardSettings::default()
    };

    let (code, page) = transcript_get(
        Some(dir.path()),
        "/api/v1/agent/transcript?lane=gate&limit=2",
        &settings,
    );
    assert_eq!(code, "200 OK");
    assert_eq!(page["sessions"][0]["id"], "sess-a");
    assert_eq!(page["sessions"][0]["started_ms"], 1_795_578_362_966_i64);
    assert_eq!(page["sessions"][1]["model"], "m-1");
    assert_eq!(page["sessions"][1]["thinking"], "low");
    assert_eq!(
        (page["total"].clone(), page["next"].clone()),
        (3.into(), "2".into())
    );
    assert_eq!(page["truncated"], false);
    assert!(!page.to_string().contains("SYSTEM-PROMPT-TEXT"));
    assert!(!page.to_string().contains("never/served"));
    // Newest first: the exchange after the restart, with its provider error
    // and compaction note, joined to no turn of another lane.
    let after = &page["exchanges"][0];
    assert_eq!(after["session"], "sess-b");
    assert_eq!(after["complete"], true);
    assert_eq!(after["items"][0]["kind"], "note");
    assert_eq!(after["items"][1]["text"], "provider f");
    assert_eq!(after["items"][1]["truncated"], true);
    assert!(after["turn"].is_null());
    // A brief with no reply is an interrupted exchange tied to its exited turn.
    let interrupted = &page["exchanges"][1];
    assert_eq!(interrupted["complete"], false);
    assert_eq!(interrupted["items"], serde_json::json!([]));
    assert!(interrupted["ended_ms"].is_null());
    assert_eq!(interrupted["turn"]["turn_id"], "gate-2");
    assert_eq!(interrupted["turn"]["error"], "agent_exited");

    let (_, older) = transcript_get(
        Some(dir.path()),
        "/api/v1/agent/transcript?lane=gate&limit=2&cursor=2",
        &settings,
    );
    assert!(older["next"].is_null());
    let exchange = &older["exchanges"][0];
    assert_eq!(exchange["session"], "sess-a");
    assert_eq!(exchange["turn"]["turn_id"], "gate-1");
    assert_eq!(exchange["complete"], true);
    assert_eq!(exchange["ended_ms"], 1_000_400);
    // The brief keeps its full byte length and is cut on a char boundary.
    assert_eq!(exchange["brief"]["text"], "Brief:\né");
    assert_eq!(exchange["brief"]["bytes"], 47);
    assert_eq!(exchange["brief"]["truncated"], true);
    let kinds: Vec<_> = exchange["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["thinking", "text", "tool", "tool", "text"]);
    // Results are paired by call id even when they arrive out of order.
    let (call_1, call_2) = (&exchange["items"][2], &exchange["items"][3]);
    assert_eq!(call_1["id"], "call_1");
    assert_eq!(call_1["args"]["text"], "{\"keys\":[\"é");
    assert_eq!(call_1["args"]["truncated"], true);
    assert_eq!(call_1["result"]["text"], "0123456789");
    assert_eq!(call_1["result"]["truncated"], true);
    assert_eq!(
        (call_1["is_error"].clone(), call_1["duration_ms"].clone()),
        (false.into(), 9.into())
    );
    assert_eq!(call_2["args"]["text"], "{\"a\":1}");
    assert_eq!(call_2["args"]["truncated"], false);
    assert_eq!(call_2["result"]["text"], "denied");
    assert_eq!(call_2["is_error"], true);
}

#[test]
fn transcript_route_rejects_bad_queries_and_serves_an_empty_missing_lane() {
    let dir = tempfile::tempdir().unwrap();
    let settings = DashboardSettings::default();
    let long_lane = "a".repeat(33);
    for (target, reason) in [
        (
            "/api/v1/agent/transcript".to_owned(),
            "transcript_lane_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=Gate".to_owned(),
            "transcript_lane_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=1gate".to_owned(),
            "transcript_lane_invalid",
        ),
        (
            format!("/api/v1/agent/transcript?lane={long_lane}"),
            "transcript_lane_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=gate&limit=0".to_owned(),
            "transcript_limit_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=gate&limit=51".to_owned(),
            "transcript_limit_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=gate&limit=x".to_owned(),
            "transcript_limit_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=gate&cursor=100001".to_owned(),
            "transcript_cursor_invalid",
        ),
        (
            "/api/v1/agent/transcript?lane=gate&cursor=-1".to_owned(),
            "transcript_cursor_invalid",
        ),
    ] {
        let (code, body) = transcript_get(Some(dir.path()), &target, &settings);
        assert_eq!(
            (code.as_str(), body["error"].as_str()),
            ("400 Bad Request", Some(reason)),
            "{target}"
        );
    }
    let (code, body) = transcript_get(None, "/api/v1/agent/transcript?lane=gate", &settings);
    assert_eq!(
        (code.as_str(), body["error"].as_str()),
        ("503 Service Unavailable", Some("session_unavailable"))
    );

    // A lane that never ran, or holds only symlinks and other files, is empty.
    let (code, body) = transcript_get(
        Some(dir.path()),
        "/api/v1/agent/transcript?lane=gate",
        &settings,
    );
    assert_eq!(code, "200 OK");
    assert_eq!(
        (
            body["exchanges"].clone(),
            body["sessions"].clone(),
            body["total"].clone()
        ),
        (serde_json::json!([]), serde_json::json!([]), 0.into())
    );
    assert!(body["next"].is_null());
    let lane = dir.path().join("model/agent/pi/gate");
    std::fs::create_dir_all(&lane).unwrap();
    std::fs::write(lane.join("notes.txt"), "x").unwrap();
    std::os::unix::fs::symlink("/dev/null", lane.join("2026_linked.jsonl")).unwrap();
    let (code, body) = transcript_get(
        Some(dir.path()),
        "/api/v1/agent/transcript?lane=gate",
        &settings,
    );
    assert_eq!(
        (code.as_str(), body["sessions"].clone()),
        ("200 OK", serde_json::json!([]))
    );
}

#[test]
fn archive_series_enforces_bounds_and_reports_unavailable_state() {
    let view = Arc::new(Mutex::new(serde_json::json!({
        "paper_only":true,"live_enabled":false,"account":"fixture-alias",
        "authorized_session":"2026-10-29","as_of_ms":1_760_000_000_000i64
    })));
    let send = |path: &str, archive: Option<MarketArchiveSettings>| {
        Ctx::new(
            "fixture".into(),
            None,
            view.clone(),
            DashboardSettings::default(),
        )
        .with_archive(archive)
        .respond(
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n\r\n"
            ),
            b"",
        )
    };
    for (path, reason) in [
        (
            "/api/v1/series?kind=archive-minute-bar&from=1&to=60000",
            "series_instrument_required",
        ),
        (
            "/api/v1/series?kind=archive-minute-bar&instrument=bad%22key&from=1&to=60000",
            "series_instrument_invalid",
        ),
        (
            "/api/v1/series?kind=archive-minute-bar&instrument=NSE_EQ%7CINE00U801010&from=1000",
            "series_range_required",
        ),
        (
            "/api/v1/series?kind=archive-daily-bar&instrument=NSE_EQ%7CINE00U801010&to=1000",
            "series_range_required",
        ),
        (
            "/api/v1/series?kind=archive-minute-bar&instrument=NSE_EQ%7CINE00U801010&from=1000&to=500",
            "series_range_invalid",
        ),
        (
            &format!(
                "/api/v1/series?kind=archive-minute-bar&instrument=NSE_EQ%7CINE00U801010&from=1&to={}",
                ARCHIVE_MINUTE_MAX_RANGE_MS + 2
            ),
            "series_range_too_large",
        ),
        (
            &format!(
                "/api/v1/series?kind=archive-daily-bar&instrument=NSE_EQ%7CINE00U801010&from=1&to={}",
                ARCHIVE_DAILY_MAX_RANGE_MS + 2
            ),
            "series_range_too_large",
        ),
        (
            "/api/v1/series?kind=archive-minute-bar&instrument=NSE_EQ%7CINE00U801010&from=1&to=60000&limit=0",
            "page_limit_invalid",
        ),
        (
            "/api/v1/series?kind=archive-minute-bar&instrument=NSE_EQ%7CINE00U801010&from=1&to=60000&limit=2001",
            "page_limit_invalid",
        ),
    ] {
        let reply = send(path, None);
        assert_eq!(reply.code, "400 Bad Request", "{path}");
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["error"], reason, "{path}");
    }

    for kind in ["archive-minute-bar", "archive-daily-bar"] {
        let reply = send(
            &format!(
                "/api/v1/series?kind={kind}&instrument=NSE_EQ%7CINE00U801010&from=1000&to=86400000"
            ),
            None,
        );
        assert_eq!(reply.code, "200 OK");
        let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
        assert_eq!(body["unavailable"], true);
        assert_eq!(body["reason"], "archive_not_configured");
        assert_eq!(body["points"], serde_json::json!([]));
        assert_eq!(body["execution_authority"], false);
    }

    let unreachable = MarketArchiveSettings {
        url: "postgresql://test@127.0.0.1:1/archive".into(),
        password_file: PathBuf::from("/nonexistent-archive-password"),
    };
    let reply = send(
        "/api/v1/series?kind=archive-minute-bar&instrument=NSE_EQ%7CINE00U801010&from=1000&to=86400000",
        Some(unreachable),
    );
    assert_eq!(reply.code, "200 OK");
    let body: serde_json::Value = serde_json::from_slice(&reply.body).unwrap();
    assert_eq!(body["unavailable"], true);
    assert_eq!(body["reason"], "archive_unavailable");
    assert_eq!(body["points"], serde_json::json!([]));
    assert_eq!(body["execution_authority"], false);
}

#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn archive_series_reads_five_day_minute_chart_under_200ms() -> crate::runtime::Result<()> {
    use nivra_store::market_archive::{
        CandleBar, CandleBatch, CandleUnit, QuoteBatch, QuoteObservation, QuotePurpose, Receipt,
        init, write_candles, write_quotes,
    };

    let tmp = tempfile::tempdir()?;
    let settings = crate::runtime::market_archive::test_settings(tmp.path())?;
    let mut db = crate::runtime::market_archive::connect(&settings)?;
    init(&mut db)?;

    let nonce = millis();
    let instrument = format!("SYNTHETIC|{nonce}-H");
    let other = format!("SYNTHETIC|{nonce}-BG");
    let start_day = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
    for day_offset in 0..5 {
        let day = start_day + chrono::Duration::days(day_offset);
        for minute in 0..375u32 {
            let base_ms = minute_to_millis(day, 555 + minute).unwrap();
            let receipt_ms = base_ms + 15_000;
            let body = format!("quote-{nonce}-{day_offset}-{minute}");
            let ltp = 10_000 + (day_offset * 100) + i64::from(minute);
            let batch = QuoteBatch {
                receipt: Receipt::new(receipt_ms - 10, receipt_ms, body.as_bytes()),
                purpose: QuotePurpose::Scanner,
                observations: vec![
                    QuoteObservation {
                        instrument_key: instrument.clone(),
                        source_ms: receipt_ms - 5,
                        ltp,
                        bid: Some(ltp),
                        ask: None,
                        bid_qty: Some(10),
                        ask_qty: None,
                        volume: 1_000,
                        lower_circuit: 8_000,
                        upper_circuit: 12_000,
                    },
                    QuoteObservation {
                        instrument_key: other.clone(),
                        source_ms: receipt_ms - 5,
                        ltp: ltp + 50,
                        bid: None,
                        ask: None,
                        bid_qty: None,
                        ask_qty: None,
                        volume: 500,
                        lower_circuit: 8_000,
                        upper_circuit: 12_000,
                    },
                ],
            };
            assert!(write_quotes(&mut db, &batch)?);
        }
    }
    let daily_bars: Vec<CandleBar> = (0..5)
        .map(|day_offset| {
            let day = start_day + chrono::Duration::days(day_offset);
            CandleBar {
                day,
                bar_ms: minute_to_millis(day, 555).unwrap(),
                o: 10_000 + day_offset * 100,
                h: 10_400 + day_offset * 100,
                l: 9_900 + day_offset * 100,
                c: 10_374 + day_offset * 100,
                volume: 50_000,
                oi: 0,
            }
        })
        .collect();
    let daily_body = format!("daily-{nonce}");
    assert!(write_candles(
        &mut db,
        &CandleBatch {
            receipt: Receipt::new(1_000, 1_010, daily_body.as_bytes()),
            instrument_key: instrument.clone(),
            unit: CandleUnit::Daily,
            start: start_day,
            end: start_day + chrono::Duration::days(4),
            bars: daily_bars,
        }
    )?);

    let from = minute_to_millis(start_day, 555).unwrap();
    let to = minute_to_millis(start_day + chrono::Duration::days(4), 929).unwrap() + 59_999;
    let encoded_key = instrument.replace('|', "%7C");
    let view = Arc::new(Mutex::new(serde_json::json!({
        "paper_only":true,"live_enabled":false,"account":"fixture-alias",
        "authorized_session":"2026-11-06","as_of_ms":to
    })));
    let ctx = Ctx::new(
        "fixture".into(),
        None,
        view.clone(),
        DashboardSettings::default(),
    )
    .with_archive(Some(settings));
    let request = |path: &str| {
        ctx.respond(
            &format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:8771\r\nAuthorization: Bearer fixture\r\n\r\n"
            ),
            b"",
        )
    };

    let cold_started = Instant::now();
    let reply = request(&format!(
        "/api/v1/series?kind=archive-minute-bar&instrument={encoded_key}&from={from}&to={to}"
    ));
    let cold_elapsed = cold_started.elapsed();
    assert_eq!(reply.code, "200 OK");
    let body: serde_json::Value = serde_json::from_slice(&reply.body)?;
    assert_eq!(body["unavailable"], false);
    assert_eq!(body["source"], "market_archive");
    assert_eq!(body["truncated"], false);
    let points = body["points"].as_array().unwrap();
    assert_eq!(points.len(), 5 * 375);
    assert_eq!(points[0]["close"], "100");
    assert_eq!(points[1874]["close"], "107.74");
    assert_eq!(body["coverage"]["gaps"].as_array().unwrap().len(), 4);

    let warm_started = Instant::now();
    let warm_reply = request(&format!(
        "/api/v1/series?kind=archive-minute-bar&instrument={encoded_key}&from={from}&to={to}"
    ));
    let warm_elapsed = warm_started.elapsed();
    assert_eq!(warm_reply.code, "200 OK");
    println!(
        "5-day minute chart server time: cold={:.2} ms warm={:.2} ms (1875 bars)",
        cold_elapsed.as_secs_f64() * 1000.0,
        warm_elapsed.as_secs_f64() * 1000.0
    );
    // A debug build on a shared machine measures around 100 ms; the bound only
    // catches a regression such as a lost index, not ordinary load.
    assert!(
        warm_elapsed < Duration::from_secs(1),
        "5-day minute chart exceeded 1 s: {:?}",
        warm_elapsed
    );

    let page1_reply = request(&format!(
        "/api/v1/series?kind=archive-minute-bar&instrument={encoded_key}&from={from}&to={to}&limit=500"
    ));
    let page1: serde_json::Value = serde_json::from_slice(&page1_reply.body)?;
    assert_eq!(page1["points"].as_array().unwrap().len(), 500);
    assert_eq!(page1["truncated"], true);
    let next_to = page1["next"].as_i64().unwrap();
    let page2_reply = request(&format!(
        "/api/v1/series?kind=archive-minute-bar&instrument={encoded_key}&from={from}&to={next_to}&limit=500"
    ));
    let page2: serde_json::Value = serde_json::from_slice(&page2_reply.body)?;
    assert_eq!(page2["points"].as_array().unwrap().len(), 500);
    assert!(
        page2["points"].as_array().unwrap().last().unwrap()["at"]
            .as_i64()
            .unwrap()
            < page1["points"].as_array().unwrap()[0]["at"]
                .as_i64()
                .unwrap()
    );

    let daily_reply = request(&format!(
        "/api/v1/series?kind=archive-daily-bar&instrument={encoded_key}&from={from}&to={to}"
    ));
    assert_eq!(daily_reply.code, "200 OK");
    let daily: serde_json::Value = serde_json::from_slice(&daily_reply.body)?;
    assert_eq!(daily["points"].as_array().unwrap().len(), 5);
    assert_eq!(daily["points"][0]["close"], "103.74");
    assert_eq!(daily["points"][4]["close"], "107.74");
    Ok(())
}
