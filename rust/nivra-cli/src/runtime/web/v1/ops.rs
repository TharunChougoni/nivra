use super::*;

pub(crate) fn meta(
    head: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    settings: &DashboardSettings,
    archive: Option<&ArchiveReader>,
) -> Response {
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let live = LIVE_ENABLED || view["live_enabled"].as_bool().unwrap_or(false);
    let body = serde_json::json!({
        "v":1,"version":env!("CARGO_PKG_VERSION"),"mode":if live {"live"} else {"paper"},
        "paper_only":view["paper_only"].as_bool().unwrap_or(!live),"live_enabled":live,
        "broker":{"data":"upstox","orders":order_authority(live)},"account":view["account"].as_str().map(|alias| super::super::super::transcript::redact(&serde_json::json!(alias))),
        "capabilities":["summary","portfolio","fills","plans","scanner","instruments","sessions","health","alerts","series","agent","agent_transcript","events","backtest"],
        "archive_configured":archive.is_some(),
        "limits":{"sse_max_streams":settings.sse_max_streams,"sse_heartbeat_ms":settings.sse_heartbeat_ms,"longpoll_hold_ms":settings.longpoll_hold_ms,"blob_slice_max_bytes":settings.blob_slice_max_bytes,"transcript_max_bytes":settings.transcript_max_bytes,"transcript_text_max_bytes":settings.transcript_text_max_bytes,"transcript_args_max_bytes":settings.transcript_args_max_bytes,"series_max_points":settings.series_max_points,"series_max_range_ms":settings.series_max_range_ms,"archive_minute_max_range_ms":series::ARCHIVE_MINUTE_MAX_RANGE_MS,"archive_daily_max_range_ms":series::ARCHIVE_DAILY_MAX_RANGE_MS,"equity_max_points":settings.equity_max_points,"equity_sample_interval_ms":settings.equity_sample_interval_ms,"price_batch_instruments":settings.price_batch_instruments,"price_batch_interval_ms":settings.price_batch_interval_ms}
    });
    drop(view);
    let body = body.to_string();
    let etag = format!("\"{}\"", &digest(body.as_bytes())[..16]);
    let matched = values(head, "if-none-match") == [etag.as_str()];
    let mut reply = response(
        if matched {
            "304 Not Modified"
        } else {
            "200 OK"
        },
        "application/json",
        if matched { "" } else { &body },
    );
    reply.cache = Cow::Borrowed("private, no-cache");
    reply.etag = Some(etag);
    reply
}

pub(crate) fn sessions(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
) -> Response {
    let limit = match page_limit(&query, "limit", 50, 200) {
        Ok(limit) => limit,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let offset = match page_offset(&query, "cursor") {
        Ok(offset) => offset,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let p = &view["portfolio"];
    let current_envelope = envelope(&view);
    let rows = vec![serde_json::json!({
        "id":{"broker":"upstox","account":view["account"],"date":view["authorized_session"]},
        "mode":current_envelope["mode"],"paper_only":current_envelope["paper_only"],"live_enabled":current_envelope["live_enabled"],
        "status":if p["stopped"]==true {"stopped"} else if p["halted"]==true {"halted"} else if p["closed"]==true {"closed"} else {"open_snapshot"},
        "phase":view["status"],"current":true,"as_of_ms":view["as_of_ms"],
        "capital":view["capital"],"cash":p["cash"],
        "positions":p["positions"].as_object().map_or(0,|v|v.len()),
        "fills":p["fills"].as_array().map_or(0,|v|v.len()),
        "provenance":"live_controller_snapshot","trading_controls":false
    })];
    let total = rows.len();
    let mut body = envelope(&view);
    body["total"] = serde_json::json!(total);
    body["items"] = serde_json::json!(
        rows.into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>()
    );
    body["next"] = if offset.saturating_add(limit) < total {
        serde_json::json!(offset + limit)
    } else {
        serde_json::Value::Null
    };
    drop(view);
    json_reply(head, body)
}
pub(crate) fn health(head: &str, view: &Arc<Mutex<serde_json::Value>>) -> Response {
    health_at(head, view, millis())
}

pub(crate) fn health_at(head: &str, view: &Arc<Mutex<serde_json::Value>>, now_ms: i64) -> Response {
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let body = health_body(&view, now_ms);
    drop(view);
    // The body carries the live liveness age; the ETag does not, so a repeat
    // read with unchanged content answers 304.
    let mut content = body.clone();
    strip_clocks(&mut content);
    content_reply(head, body, &content)
}

pub(super) fn health_body(view: &serde_json::Value, now_ms: i64) -> serde_json::Value {
    let p = &view["portfolio"];
    let alerts = derived_alerts(view);
    let mut body = envelope(view);
    body["status"] = view["status"].clone();
    body["controller_liveness"] = serde_json::json!(
        crate::runtime::operational_health::controller_liveness(view, now_ms)
    );
    if let Some(operational) = view.get("operational_health") {
        body["operational_health"] = operational.clone();
    }
    if let Some(heartbeat) = view.get("controller_heartbeat") {
        body["controller_heartbeat"] = serde_json::json!({
            "version":heartbeat["version"].as_u64(),
            "completed_at_ms":heartbeat["completed_at_ms"].as_i64(),
            "next_deadline_ms":heartbeat["next_deadline_ms"].as_i64(),
            "loop_generation":heartbeat["loop_generation"].as_u64()
        });
    }
    body["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
    body["process_memory"] = summary::process_memory();
    body["updater"] = view["updater"].clone();
    body["model_status"] = view["model_status"].clone();
    body["entry_status"] = view["entry_status"].clone();
    body["book"] = serde_json::json!({
        "stopped":p["stopped"],"halted":p["halted"],"closed":p["closed"],
        "health":p["health"],"read_failure_reason":view["read_failure_reason"],"stop_classification":stop_classification(p)
    });
    body["readiness"] = view["readiness"].clone();
    body["activity_attribution"] = view["activity_attribution"].clone();
    body["model_initialization"] = view["model_initialization"].clone();
    body["history_preparation"] = view["history_preparation"].clone();
    body["market_feed"] = view["market_feed"].clone();
    body["oldest_scan_age_ms"] = view["oldest_scan_age_ms"].clone();
    if let Some(strategy_selection) = view
        .get("discovery")
        .and_then(|d| d.get("strategy_selection"))
    {
        body["strategy_selection"] = strategy_selection.clone();
    }
    body["research_library"] = view["research_library"].clone();
    // Copy the controller's already-captured read-only projections verbatim.
    // This GET never samples a clock, reads the queue/state, recomputes the
    // research status or renews freshness; an absent field stays absent.
    if let Some(value) = view.get("research_tasks") {
        body["research_tasks"] = value.clone();
    }
    body["retention"] = view["retention"].clone();
    body["planner"] = serde_json::json!({
        "status":view["planner"]["status"],"budget_used":view["planner"]["budget_used"],
        "budgets":view["planner"]["budgets"],"pending_attempt":view["planner"]["pending_attempt"]
    });
    body["independent_telemetry"] = serde_json::json!({
        "status":"not_connected","reason":"hosted_worker_telemetry_unavailable"
    });
    body["alerts"] = serde_json::json!(alerts);
    body["alert_count"] = serde_json::json!(alerts.len());
    body
}
pub(crate) fn alerts(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
) -> Response {
    let limit = match page_limit(&query, "limit", 50, 200) {
        Ok(limit) => limit,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let offset = match page_offset(&query, "cursor") {
        Ok(offset) => offset,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let rows = derived_alerts(&view);
    let total = rows.len();
    let mut body = envelope(&view);
    body["total"] = serde_json::json!(total);
    body["items"] = serde_json::json!(
        rows.into_iter()
            .skip(offset)
            .take(limit)
            .collect::<Vec<_>>()
    );
    body["next"] = if offset.saturating_add(limit) < total {
        serde_json::json!(offset + limit)
    } else {
        serde_json::Value::Null
    };
    drop(view);
    json_reply(head, body)
}

/// Files under `model/agent` that `/api/v1/agent` reads.
pub(super) const AGENT_FILES: [&str; 3] = ["turns.ndjson", "triggers.ndjson", "usage.json"];

/// Newest bytes of each lane journal the dashboard holds; older rows are
/// reported as not loaded.
pub(super) const AGENT_JOURNAL_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// One journalled pi turn, without its brief or model text: lane, model,
/// outcome, usage, tool calls, the plan's stance and the gate's verdict.
pub(super) fn agent_turn_row(turn: serde_json::Value) -> serde_json::Value {
    let trigger = &turn["trigger"];
    let instruments: Vec<&serde_json::Value> = trigger["triggers"]
        .as_array()
        .map(|batch| {
            batch
                .iter()
                .filter_map(|t| t.get("instrument_key"))
                .collect()
        })
        .unwrap_or_default();
    let plan = &turn["plan"];
    serde_json::json!({
        "turn_id":turn["turn_id"],"lane":turn["lane"],"at_ms":turn["at_ms"],
        "status":turn["status"],"error":turn["error"],"provider":turn["provider"],"model":turn["model"],
        "elapsed_ms":turn["elapsed_ms"],"usage":turn["usage"],"fallback":turn["fallback"],
        "trigger":{"kind":trigger["kind"],"reason":trigger["reason"],"instruments":instruments},
        "tool_calls":turn["tool_calls"],
        "plan":if plan.is_null() { serde_json::Value::Null } else {
            serde_json::json!({"status":plan["status"],"stance":plan["plan"]["stance"],
                "actions":plan["plan"]["actions"].as_array().map_or(0, Vec::len),"error":plan["error"]})
        },
        "verdict":turn["verdict"]
    })
}

/// The gate-count bucket of one trigger journal line, or null.
fn trigger_event(line: serde_json::Value) -> serde_json::Value {
    match (line["event"].as_str(), line["escalate"].as_bool()) {
        (Some("gated"), Some(true)) => "gate_escalated".into(),
        (Some("gated"), _) => "gate_declined".into(),
        (Some(event), _) => event.into(),
        _ => serde_json::Value::Null,
    }
}

pub(crate) fn agent_turns(head: &str, query: router::Query, root: Option<&Path>) -> Response {
    agent_turns_within(head, query, root, AGENT_JOURNAL_MAX_BYTES)
}

pub(super) fn agent_turns_within(
    head: &str,
    query: router::Query,
    root: Option<&Path>,
    max_bytes: u64,
) -> Response {
    let Some(root) = root else {
        return error("503 Service Unavailable", "session_unavailable");
    };
    let limit = match query.get("limit") {
        Some(value) => match value.parse::<usize>() {
            Ok(limit) if (1..=200).contains(&limit) => limit,
            _ => return error("400 Bad Request", "agent_limit_invalid"),
        },
        None => 50,
    };
    let offset = match query.get("cursor") {
        Some(value) => match value.parse::<usize>() {
            Ok(offset) if offset <= 100_000 => offset,
            _ => return error("400 Bad Request", "agent_cursor_invalid"),
        },
        None => 0,
    };
    let dir = root.join("model").join("agent");
    let read = journal::with_journals(
        [
            (dir.join(AGENT_FILES[0]), agent_turn_row as journal::Project),
            (dir.join(AGENT_FILES[1]), trigger_event),
        ],
        max_bytes,
        |[turns, triggers]| {
            let total = turns.rows().count();
            let page: Vec<serde_json::Value> = turns
                .rows()
                .rev()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect();
            let mut events = std::collections::BTreeMap::<&str, u64>::new();
            for event in triggers.rows().filter_map(serde_json::Value::as_str) {
                *events.entry(event).or_default() += 1;
            }
            let next = (offset + page.len() < total).then(|| (offset + page.len()).to_string());
            serde_json::json!({
                "v":1,"turns":page,"total":total,"next":next,"triggers":events,
                "truncated":turns.not_loaded + triggers.not_loaded > 0,
                "rows_not_loaded":{"turns":turns.not_loaded,"triggers":triggers.not_loaded}
            })
        },
    );
    let Ok(mut body) = read else {
        return error("503 Service Unavailable", "agent_journal_unavailable");
    };
    let usage = std::fs::read(dir.join(AGENT_FILES[2]))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .map_or(
            serde_json::Value::Null,
            |usage| serde_json::json!({"date":usage["date"],"lanes":usage["lanes"]}),
        );
    body["usage"] = usage;
    body["execution_authorized"] = serde_json::json!(false);
    json_reply(head, body)
}
