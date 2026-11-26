use super::*;

pub(crate) fn portfolio(head: &str, view: &Arc<Mutex<serde_json::Value>>) -> Response {
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let p = &view["portfolio"];
    if !p.is_object() {
        return json_reply(head, unavailable(envelope(&view), "portfolio_not_reported"));
    }
    let mut body = envelope(&view);
    body["capital"] = view["capital"].clone();
    body["policy"] = p["policy"].clone();
    body["costs"] = p["costs"].clone();
    body["model_led"] = p["model_led"].clone();
    body["cash"] = p["cash"].clone();
    body["equity"] = portfolio_equity(p)
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    body["initial_allocation"] = p["initial_allocation"].clone();
    body["high_water"] = p["high_water"].clone();
    body["stopped"] = p["stopped"].clone();
    body["halted"] = p["halted"].clone();
    body["closed"] = p["closed"].clone();
    body["health"] = p["health"].clone();
    body["positions"] = serde_json::json!(portfolio_positions(p));
    body["fills_count"] = serde_json::json!(p["fills"].as_array().map_or(0, Vec::len));
    body["recorded_fees"] = fill_fees(p)
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    body["realised_net_pnl_by_fill"] = view["realised_net_pnl_by_fill"].clone();
    body["fee_policy"] = view["fee_policy"].clone();
    drop(view);
    json_reply(head, body)
}

pub(crate) fn fills(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
) -> Response {
    let limit = match page_limit(&query, "limit", 100, 500) {
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
    let Some(fills) = p["fills"].as_array() else {
        return json_reply(head, unavailable(envelope(&view), "fills_not_reported"));
    };
    let total = fills.len();
    let mut body = envelope(&view);
    body["total"] = serde_json::json!(total);
    body["items"] = serde_json::json!(
        fills
            .iter()
            .rev()
            .skip(offset)
            .take(limit)
            .cloned()
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
pub(crate) fn plans(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
) -> Response {
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let session = view["authorized_session"].as_str().unwrap_or("");
    if query.get("date").is_some_and(|date| date != session) {
        return json_reply(
            head,
            unavailable(envelope(&view), "requested_session_not_loaded"),
        );
    }
    let mut body = envelope(&view);
    let mut planner = view["planner"].clone();
    let mut take = |key: &str| {
        planner
            .as_object_mut()
            .and_then(|fields| fields.shift_remove(key))
            .unwrap_or_default()
    };
    body["actions"] = take("actions");
    body["execution"] = take("execution");
    body["planner"] = planner;
    body["note"] = serde_json::json!(
        "Raw actions and host validation are distinct; execution verdicts are not fills."
    );
    drop(view);
    json_reply(head, body)
}
