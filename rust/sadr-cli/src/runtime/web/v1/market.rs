use super::*;

pub(crate) fn scanner(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    settings: &DashboardSettings,
) -> Response {
    let limit = match page_limit(&query, "limit", 100, 500) {
        Ok(limit) => limit,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let offset = match page_offset(&query, "cursor") {
        Ok(offset) => offset,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let segment = query.get("segment").unwrap_or("prospects");
    if !["prospects", "missing", "screens"].contains(&segment) {
        return error("400 Bad Request", "scanner_segment_invalid");
    }
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let Some(discovery) = view["discovery"].as_object() else {
        return json_reply(head, unavailable(envelope(&view), "scanner_not_reported"));
    };
    let (source, total) = if segment == "screens" {
        let rows = discovery
            .get("screen_reasons")
            .and_then(|value| value.as_object())
            .map(|reasons| {
                reasons
                    .iter()
                    .map(|(reason, total)| serde_json::json!({"reason":reason,"total":total}))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let total = rows.len();
        (rows, total)
    } else {
        let key = if segment == "missing" {
            "unavailable"
        } else {
            "prospects"
        };
        let rows = discovery
            .get(key)
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let total = rows.len();
        (rows, total)
    };
    let mut body = envelope(&view);
    body["segment"] = serde_json::json!(segment);
    body["total"] = serde_json::json!(total);
    body["items"] = serde_json::json!(if segment == "screens" {
        source
    } else {
        source.into_iter().skip(offset).take(limit).collect()
    });
    body["next"] = if segment != "screens" && offset.saturating_add(limit) < total {
        serde_json::json!(offset + limit)
    } else {
        serde_json::Value::Null
    };
    body["coverage"] = serde_json::json!({
        "market_segment":"NSE_EQ", "instrument_kind":"cash", "display_only":true,
        "fresh":discovery.get("fresh_count"),"stale":discovery.get("stale_count"),
        "unseen":discovery.get("unseen_count"),"eligible":discovery.get("eligible_count"),
        "presented":discovery.get("presented_count"),"universe":view["universe_size"],
        "oldest_fresh_age_ms":discovery.get("oldest_fresh_age_ms"),
        "max_scan_age_ms":discovery.get("max_scan_age_ms"),
        "unavailable_count":discovery.get("unavailable_count"),
        "unavailable_truncated":discovery.get("unavailable_truncated")
    });
    body["screen_reasons"] = discovery
        .get("screen_reasons")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    body["ranking"] = view["ranking"].clone();
    body["settings"] = serde_json::json!({
        "series_max_points":settings.series_max_points,
        "price_batch_instruments":settings.price_batch_instruments,
        "price_batch_interval_ms":settings.price_batch_interval_ms
    });
    body["selection"] = discovery.get("selection").cloned().unwrap_or_default();
    body["limitations"] = discovery.get("limitations").cloned().unwrap_or_default();
    drop(view);
    json_reply(head, body)
}
pub(crate) fn instruments(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    catalog: Option<&Arc<Mutex<Option<serde_json::Value>>>>,
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
    let mut body = envelope(&view);
    drop(view);
    let Some(catalog) = catalog else {
        return json_reply(head, unavailable(body, "universe_not_prepared"));
    };
    let Ok(catalog) = catalog.lock() else {
        return error("503 Service Unavailable", "instrument_catalog_unavailable");
    };
    let Some(catalog) = catalog.as_ref() else {
        return json_reply(head, unavailable(body, "universe_not_prepared"));
    };
    let scope = query.get("scope").unwrap_or("cash");
    let catalog = match scope {
        "cash" => catalog,
        "all" => match catalog.get("display") {
            Some(display) if display["unavailable"] != true => display,
            Some(display) => {
                return json_reply(
                    head,
                    unavailable(
                        body,
                        display["reason"]
                            .as_str()
                            .unwrap_or("instrument_display_unavailable"),
                    ),
                );
            }
            None => return json_reply(head, unavailable(body, "instrument_display_loading")),
        },
        _ => return error("400 Bad Request", "instrument_catalog_scope_invalid"),
    };
    let Some(items) = catalog["items"].as_array() else {
        return error("503 Service Unavailable", "instrument_catalog_invalid");
    };
    let search = query.get("q").unwrap_or("").to_lowercase();
    let items = items
        .iter()
        .filter(|row| {
            query
                .get("market_segment")
                .is_none_or(|segment| row["segment"] == segment)
                && query.get("kind").is_none_or(|kind| {
                    row["kind"]
                        .as_str()
                        .unwrap_or(if scope == "cash" { "cash" } else { "unknown" })
                        == kind
                })
                && (search.is_empty()
                    || ["symbol", "instrument_key", "underlying_symbol"]
                        .iter()
                        .any(|key| {
                            row[*key]
                                .as_str()
                                .is_some_and(|value| value.to_lowercase().contains(&search))
                        }))
        })
        .collect::<Vec<_>>();
    let total = items.len();
    for key in [
        "source",
        "segment",
        "series",
        "file_date",
        "received_at_ms",
        "source_hashes",
        "cash_source_hashes",
        "segments",
        "kinds",
        "rows_seen",
        "rejected_rows",
        "display_only",
        "quotes_available",
        "margin_available",
        "coverage_kind",
    ] {
        body[key] = catalog[key].clone();
    }
    body["scope"] = serde_json::json!(scope);
    body["total"] = serde_json::json!(total);
    body["items"] = serde_json::json!(
        items
            .iter()
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
    json_reply(head, body)
}
pub(crate) fn instrument_chain(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    catalog: Option<&Arc<Mutex<Option<serde_json::Value>>>>,
) -> Response {
    let limit = match page_limit(&query, "limit", 100, 500) {
        Ok(value) => value,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let offset = match page_offset(&query, "cursor") {
        Ok(value) => value,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let Some(underlying) = query.get("underlying_key").filter(|s| !s.is_empty()) else {
        return error("400 Bad Request", "chain_underlying_required");
    };
    let Some(expiry) = query
        .get("expiry_ms")
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| *n > 0)
    else {
        return error("400 Bad Request", "chain_expiry_required");
    };
    let Ok(view) = view.lock() else {
        return error("503 Service Unavailable", "status_unavailable");
    };
    let mut body = envelope(&view);
    drop(view);
    let Some(catalog) = catalog else {
        return json_reply(head, unavailable(body, "universe_not_prepared"));
    };
    let Ok(catalog) = catalog.lock() else {
        return error("503 Service Unavailable", "instrument_catalog_unavailable");
    };
    let Some(display) = catalog.as_ref().and_then(|c| c.get("display")) else {
        return json_reply(head, unavailable(body, "instrument_display_loading"));
    };
    if display["unavailable"] == true {
        return json_reply(
            head,
            unavailable(
                body,
                display["reason"]
                    .as_str()
                    .unwrap_or("instrument_display_unavailable"),
            ),
        );
    }
    let Some(items) = display["items"].as_array() else {
        return error("503 Service Unavailable", "instrument_catalog_invalid");
    };
    let mut strikes = std::collections::BTreeMap::<
        sadr_core::finance::Amount,
        (Vec<&serde_json::Value>, Vec<&serde_json::Value>),
    >::new();
    let mut futures = Vec::new();
    let mut unavailable_strikes = 0;
    for row in items.iter().filter(|row| {
        row["underlying_key"] == underlying
            && row["expiry_ms"] == expiry
            && query
                .get("market_segment")
                .is_none_or(|s| row["segment"] == s)
    }) {
        match row["kind"].as_str() {
            Some("future") => futures.push(row),
            Some(kind @ ("call" | "put")) => {
                let Some(strike) = row["strike"]
                    .as_str()
                    .and_then(|s| sadr_core::finance::Amount::parse(s).ok())
                    .filter(|v| *v > sadr_core::finance::Amount::ZERO)
                else {
                    unavailable_strikes += 1;
                    continue;
                };
                let pair = strikes.entry(strike).or_default();
                if kind == "call" {
                    pair.0.push(row);
                } else {
                    pair.1.push(row);
                }
            }
            _ => {}
        }
    }
    let total = strikes.len();
    body["items"] = serde_json::json!(strikes.into_iter().skip(offset).take(limit).map(|(strike,(calls,puts))| serde_json::json!({
        "strike":strike.to_string(), "call":if calls.len()==1 {Some(calls[0])} else {None}, "put":if puts.len()==1 {Some(puts[0])} else {None},
        "ambiguous_call":calls.len()>1,"ambiguous_put":puts.len()>1
    })).collect::<Vec<_>>());
    body["total"] = serde_json::json!(total);
    body["next"] = if offset.saturating_add(limit) < total {
        serde_json::json!(offset + limit)
    } else {
        serde_json::Value::Null
    };
    body["futures_total"] = serde_json::json!(futures.len());
    body["futures_truncated"] = serde_json::json!(futures.len() > limit);
    body["futures"] = serde_json::json!(futures.into_iter().take(limit).collect::<Vec<_>>());
    for key in [
        "source",
        "source_hashes",
        "cash_source_hashes",
        "received_at_ms",
        "coverage_kind",
    ] {
        body[key] = display[key].clone();
    }
    body["underlying_key"] = serde_json::json!(underlying);
    body["expiry_ms"] = serde_json::json!(expiry);
    body["unavailable_strikes"] = serde_json::json!(unavailable_strikes);
    body["display_only"] = serde_json::json!(true);
    body["quotes_available"] = serde_json::json!(false);
    body["margin_available"] = serde_json::json!(false);
    body["note"] = serde_json::json!(
        "Captured BOD contract metadata only; missing legs, quotes and margins remain unknown. No execution admission."
    );
    json_reply(head, body)
}
