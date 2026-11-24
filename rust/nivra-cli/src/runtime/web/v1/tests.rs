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
            &DashboardSettings::default(),
        )
    };
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
    assert_eq!(ord_body["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        ord_body["items"][0]["action"]["instrument_key"],
        "NSE_EQ|INE002A01018"
    );
    assert!(ord_body["items"][1]["action"].is_null());
    assert!(ord_body["items"][0].get("fill").is_none());
    assert!(ord_body["items"][1].get("fill").is_none());
    assert!(ord_body.get("orders").is_none());
    assert!(ord_body["note"].as_str().unwrap().contains("not fills"));
    let plans_body: serde_json::Value =
        serde_json::from_slice(&send(&request("").replace("/api/v1/meta", "/api/v1/plans")).body)
            .unwrap();
    assert_eq!(plans_body["actions"].as_array().unwrap().len(), 2);
    assert_eq!(plans_body["execution"][0]["status"], "paper_simulated");
    assert!(plans_body["planner"].get("actions").is_none());
    assert!(plans_body["planner"].get("execution").is_none());
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
    assert_eq!(plain["revisions"], revisions(&view.lock().unwrap(), None));

    // Loop clocks move no revision; a new fill moves only the projections
    // that read fills; an identical summary is announced once.
    let snapshot = view.lock().unwrap().clone();
    let mut later = snapshot.clone();
    later["as_of_ms"] = serde_json::json!(snapshot["as_of_ms"].as_i64().unwrap_or(0) + 2000);
    later["controller_heartbeat"] = serde_json::json!({"version":1,"completed_at_ms":2});
    later["operational_health"]["captured_at_ms"] = serde_json::json!(902_000);
    assert_eq!(revisions(&later, None), revisions(&snapshot, None));
    later["portfolio"]["fills"] = serde_json::json!([{"fee":"1.00"}]);
    let (before, after) = (revisions(&snapshot, None), revisions(&later, None));
    let moved = before
        .as_object()
        .unwrap()
        .iter()
        .filter(|(name, revision)| after[name.as_str()] != **revision)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(moved, ["portfolio", "fills", "sessions"]);
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
    // and is read incrementally, with a torn final line held back.
    let revision = || revisions(&serde_json::json!({}), Some(dir.path()))["agent"].clone();
    assert_ne!(revision(), revisions(&serde_json::json!({}), None)["agent"]);
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
    assert_ne!(revision(), before);
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
