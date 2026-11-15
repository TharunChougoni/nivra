use super::*;

pub(crate) fn meta(
    head: &str,
    view: &Arc<Mutex<serde_json::Value>>,
    settings: &DashboardSettings,
) -> Response {
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let live = LIVE_ENABLED || view["live_enabled"].as_bool().unwrap_or(false);
    let body = serde_json::json!({
        "v":1,"version":env!("CARGO_PKG_VERSION"),"mode":if live {"live"} else {"paper"},
        "paper_only":view["paper_only"].as_bool().unwrap_or(!live),"live_enabled":live,
        "broker":{"data":"upstox","orders":null},"account":view["account"].as_str().map(|alias| super::super::super::transcript::redact(&serde_json::json!(alias))),
        "roles":["digest","orchestrator"],
        "capabilities":["summary","portfolio","positions","orders","fills","plans","scanner","instruments","sessions","health","approvals","alerts","series","runs","events","tools","backtest"],
        "limits":{"sse_max_streams":settings.sse_max_streams,"sse_heartbeat_ms":settings.sse_heartbeat_ms,"longpoll_hold_ms":settings.longpoll_hold_ms,"blob_slice_max_bytes":settings.blob_slice_max_bytes,"transcript_max_bytes":settings.transcript_max_bytes,"transcript_text_max_bytes":settings.transcript_text_max_bytes,"transcript_args_max_bytes":settings.transcript_args_max_bytes,"series_max_points":settings.series_max_points,"series_max_range_ms":settings.series_max_range_ms,"equity_max_points":settings.equity_max_points,"equity_sample_interval_ms":settings.equity_sample_interval_ms,"price_batch_instruments":settings.price_batch_instruments,"price_batch_interval_ms":settings.price_batch_interval_ms}
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
    let mut rows = vec![serde_json::json!({
        "id":{"broker":"upstox","account":view["account"],"date":view["authorized_session"]},
        "mode":current_envelope["mode"],"paper_only":current_envelope["paper_only"],"live_enabled":current_envelope["live_enabled"],
        "status":if p["stopped"]==true {"stopped"} else if p["halted"]==true {"halted"} else if p["closed"]==true {"closed"} else {"open_snapshot"},
        "phase":view["status"],"current":true,"as_of_ms":view["as_of_ms"],
        "capital":view["capital"],"cash":p["cash"],
        "positions":p["positions"].as_object().map_or(0,|v|v.len()),
        "fills":p["fills"].as_array().map_or(0,|v|v.len()),
        "provenance":"live_controller_snapshot","trading_controls":false
    })];
    let history = &view["history"];
    let mut history_unavailable = false;
    if let Some(records) = history["records"].as_array() {
        for record in records {
            if rows.iter().any(|row| row["id"] == record["id"]) {
                continue;
            }
            let mut record = record.clone();
            record["current"] = serde_json::json!(false);
            rows.push(record);
        }
    } else if history["unavailable"] == true {
        history_unavailable = true;
    }
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
    body["history_unavailable"] = serde_json::json!(history_unavailable);
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
    let p = &view["portfolio"];
    let alerts = derived_alerts(&view);
    let mut body = envelope(&view);
    body["status"] = view["status"].clone();
    body["controller_liveness"] = serde_json::json!(
        crate::runtime::operational_health::controller_liveness(&view, now_ms)
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
    body["research_library"] = view["research_library"].clone();
    // Copy the controller's already-captured read-only projections verbatim.
    // This GET never samples a clock, reads the queue/state, recomputes the
    // research status or renews freshness; an absent field stays absent.
    if let Some(value) = view.get("owner_research") {
        body["owner_research"] = value.clone();
    }
    if let Some(value) = view.get("research_tasks") {
        body["research_tasks"] = value.clone();
    }
    body["post_close"] = view["post_close"].clone();
    body["owner_review"] = view["owner_review"].clone();
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
    drop(view);
    json_reply(head, body)
}
pub(crate) fn approvals(head: &str, view: &Arc<Mutex<serde_json::Value>>) -> Response {
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let readiness = &view["readiness"];
    let mut body = envelope(&view);
    body["models"] = readiness["models"].clone();
    body["planner_enabled"] = readiness["planner_enabled"].clone();
    body["current_usage"] = readiness["current_usage"].clone();
    body["future_usage"] = readiness["future_usage"].clone();
    body["note"] = serde_json::json!(
        "Role approvals and trigger-task identities are distinct. Separate legacy grants are never added together."
    );
    body["identities"] = serde_json::json!([]);
    if let Some(roles) = readiness["models"]["roles"].as_array() {
        body["identities"] = serde_json::json!(["digest", "orchestrator"]
            .into_iter()
            .map(|identity| {
                let role = if identity == "orchestrator" {
                    roles
                        .iter()
                        .find(|role| role["role"] == "orchestrator")
                        .or_else(|| roles.iter().find(|role| role["role"] == "operations"))
                } else {
                    roles.iter().find(|role| role["role"] == identity)
                };
                serde_json::json!({
                    "identity":identity,
                    "role":role,
                    "tasks":match identity {
                        "orchestrator" => roles.iter().filter(|role| ["operations","strategy","scorer"].contains(&role["role"].as_str().unwrap_or(""))).cloned().collect::<Vec<_>>(),
                        _ => Vec::<serde_json::Value>::new()
                    }
                })
            })
            .collect::<Vec<_>>());
    }
    drop(view);
    json_reply(head, body)
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

pub(crate) fn request_session(
    value: &serde_json::Value,
    view: &Arc<Mutex<serde_json::Value>>,
) -> std::result::Result<String, &'static str> {
    if let Some(session) = value["session"].as_str() {
        let parsed =
            NaiveDate::parse_from_str(session, "%Y-%m-%d").map_err(|_| "session_date_invalid")?;
        if parsed.to_string() != session {
            return Err("session_date_invalid");
        }
        return Ok(session.to_owned());
    }
    let view = view.lock().map_err(|_| "status_unavailable")?;
    if let Some(session) = view["session"].as_str().filter(|value| !value.is_empty()) {
        return Ok(session.to_owned());
    }
    Ok(local(millis()).date_naive().to_string())
}

pub(crate) fn session_view(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    root: Option<&Path>,
) -> Response {
    let Some(root) = root else {
        return error("503 Service Unavailable", "session_unavailable");
    };
    let session = match query.get("date") {
        Some(value) => {
            let parsed = match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(parsed) if parsed.to_string() == value => parsed,
                _ => return error("400 Bad Request", "session_date_invalid"),
            };
            parsed.to_string()
        }
        None => match request_session(&serde_json::Value::Null, view) {
            Ok(session) => session,
            Err(reason) => return error("503 Service Unavailable", reason),
        },
    };
    let log = session_store::SessionLog::new(root, &session);
    let notes = session_store::Notes::new(root, &session);
    let mut turns: Vec<session_store::Turn> = log.turns();
    if turns.len() > 40 {
        turns.drain(..turns.len() - 40);
    }
    let turns: Vec<serde_json::Value> = turns
        .into_iter()
        .map(|turn| {
            let result = log.result_for(&turn.id);
            serde_json::json!({
                "id":turn.id,
                "trigger":turn.trigger,
                "prompt_version":turn.prompt_version,
                "notes_version":turn.notes_version,
                "decides_at_start":turn.decides_at_start,
                "fills_at_start":turn.fills_at_start,
                "started_at_ms":turn.started_at_ms,
                "result":result.map(|result| serde_json::json!({
                    "outcome":result.outcome,
                    "reason":result.reason,
                    "reply":result.reply,
                    "actions":result.actions,
                    "finished_at_ms":result.finished_at_ms,
                })),
            })
        })
        .collect();
    let compactions = log
        .records()
        .into_iter()
        .filter(|record| matches!(record.body, session_store::Body::Summary { .. }))
        .count();
    let failure = crate::read_bounded(&root.join("model/supervisor-failure.json"), 4096)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .unwrap_or(serde_json::Value::Null);
    let aside = crate::read_bounded(
        &root
            .join(session_store::ROOT_PREFIX)
            .join(&session)
            .join("aside-reply.json"),
        32_768,
    )
    .ok()
    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    .unwrap_or(serde_json::Value::Null);
    let aside_request = crate::read_bounded(
        &root
            .join(session_store::ROOT_PREFIX)
            .join(&session)
            .join("aside-request.json"),
        8192,
    )
    .ok()
    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    .unwrap_or(serde_json::Value::Null);
    let pending = log.pending_checked();
    let body = serde_json::json!({
        "v":1,
        "session":session,
        "turns":turns,
        "notes":notes.snapshot(),
        "status":{
            "pending":pending.turn.as_ref().map(|turn| turn.id.clone()),
            "pending_source":pending.source,
            "pending_scope":"checked_bounded_session_log",
            "summary":log.summary().map(|summary| serde_json::json!({
                "up_to_seq":summary.up_to_seq,"created_at_ms":summary.created_at_ms
            })),
            "handoff":log.handoff().map(|handoff| serde_json::json!({
                "target_session":handoff.target_session,
                "created_at_ms":handoff.created_at_ms,
                "failure":handoff.failure,
            })),
            "compactions":compactions,
            "failure":failure,
            "aside":aside,
            "aside_request":aside_request,
        },
    });
    json_reply(head, body)
}

/// Bounded read-only projection of the orchestrator web-tool call record
/// (#222). Every row is the session's original query/URL, latency, bytes and
/// outcome; the endpoint never calls a provider or changes a book.
pub(crate) fn tool_calls(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    root: Option<&Path>,
) -> Response {
    let Some(root) = root else {
        return error("503 Service Unavailable", "session_unavailable");
    };
    let session = match query.get("date") {
        Some(value) => {
            let parsed = match NaiveDate::parse_from_str(value, "%Y-%m-%d") {
                Ok(parsed) if parsed.to_string() == value => parsed,
                _ => return error("400 Bad Request", "session_date_invalid"),
            };
            parsed.to_string()
        }
        None => match request_session(&serde_json::Value::Null, view) {
            Ok(session) => session,
            Err(reason) => return error("503 Service Unavailable", reason),
        },
    };
    let limit = match query.get("limit") {
        Some(value) => match value.parse::<usize>() {
            Ok(limit) if (1..=100).contains(&limit) => limit,
            _ => return error("400 Bad Request", "tools_limit_invalid"),
        },
        None => 50,
    };
    let offset = match query.get("cursor") {
        Some(value) => match value.parse::<usize>() {
            Ok(offset) if offset <= 10_000 => offset,
            _ => return error("400 Bad Request", "tools_cursor_invalid"),
        },
        None => 0,
    };
    match super::super::super::web_tools::read_log(root, Some(&session), offset, limit) {
        Ok((calls, total)) => {
            let next = (offset + calls.len() < total).then(|| (offset + calls.len()).to_string());
            json_reply(
                head,
                serde_json::json!({
                    "v":1,"session":session,"calls":calls,"total":total,"next":next,
                    "execution_authorized":false
                }),
            )
        }
        Err(reason) => error("503 Service Unavailable", &reason.to_string()),
    }
}
