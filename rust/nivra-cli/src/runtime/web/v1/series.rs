use super::*;

pub(crate) fn optional_ms(
    query: &router::Query,
    key: &str,
) -> std::result::Result<Option<i64>, &'static str> {
    match query.get(key) {
        None => Ok(None),
        Some(value) if value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit()) => value
            .parse::<i64>()
            .ok()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or("series_range_invalid"),
        Some(_) => Err("series_range_invalid"),
    }
}
pub(crate) fn minute_to_millis(date: NaiveDate, minute: u32) -> Option<i64> {
    let offset = FixedOffset::east_opt(19_800)?;
    let start = date.and_hms_opt(0, 0, 0)?;
    let base = offset
        .from_local_datetime(&start)
        .single()?
        .timestamp_millis();
    base.checked_add(i64::from(minute).checked_mul(60_000)?)
}
pub(crate) fn valid_instrument_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 160
        && key
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'"' | b'\\' | b'<' | b'>'))
}
pub(crate) fn series_gap(from_ms: i64, to_ms: i64, observed: bool) -> serde_json::Value {
    serde_json::json!({
        "from_ms":from_ms,
        "to_ms":to_ms,
        "reason":if observed {"no_trade"} else {"unobserved"},
        "minutes":((to_ms.saturating_sub(from_ms) / 60_000) + 1)
    })
}
pub(crate) fn series(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    settings: &DashboardSettings,
    features: Option<&Arc<Mutex<FeatureStore>>>,
    equity: Option<&Arc<Mutex<EquitySeries>>>,
) -> Response {
    let limit = match page_limit(
        &query,
        "limit",
        settings.series_max_points,
        settings.series_max_points,
    ) {
        Ok(limit) => limit,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let from = match optional_ms(&query, "from") {
        Ok(value) => value,
        Err(reason) => return error("400 Bad Request", reason),
    };
    let to = match optional_ms(&query, "to") {
        Ok(value) => value,
        Err(reason) => return error("400 Bad Request", reason),
    };
    if let (Some(from), Some(to)) = (from, to) {
        if to < from {
            return error("400 Bad Request", "series_range_invalid");
        }
        if to.saturating_sub(from) > settings.series_max_range_ms as i64 {
            return error("400 Bad Request", "series_range_too_large");
        }
    }
    let kind = query.get("kind").unwrap_or("receipt-minute-bar");
    if !["receipt-minute-bar", "account-equity"].contains(&kind) {
        return error("400 Bad Request", "series_kind_invalid");
    }
    let mut body = {
        let Ok(view) = view.lock() else {
            return error("503 Service Unavailable", "status_unavailable");
        };
        envelope(&view)
    };
    body["kind"] = serde_json::json!(kind);
    body["limit"] = serde_json::json!(limit);
    body["range"] = serde_json::json!({"from":from,"to":to});
    if kind == "account-equity" {
        let Some(equity) = equity else {
            return json_reply(head, unavailable(body, "equity_series_not_collected"));
        };
        let Ok(series) = equity.lock() else {
            return error("503 Service Unavailable", "series_unavailable");
        };
        let points = series.points(from, to, limit);
        let watermark = series.last_at();
        body["points"] = serde_json::json!(points);
        body["watermark_ms"] = serde_json::json!(watermark);
        body["unavailable"] = serde_json::json!(false);
        body["source"] = serde_json::json!("recorded_portfolio_checkpoints");
        body["mark_basis"] = serde_json::json!("recorded_position_marks");
        body["note"] = serde_json::json!(
            "Daily allocations are not summed as wealth; unresolved marks are omitted."
        );
        drop(series);
        return json_reply(head, body);
    }
    let Some(instrument) = query.get("instrument") else {
        return error("400 Bad Request", "series_instrument_required");
    };
    if !valid_instrument_key(instrument) {
        return error("400 Bad Request", "series_instrument_invalid");
    }
    body["instrument_key"] = serde_json::json!(instrument);
    let Some(features) = features else {
        return json_reply(head, unavailable(body, "price_series_not_collected"));
    };
    let Ok(store) = features.lock() else {
        return error("503 Service Unavailable", "series_unavailable");
    };
    let Some(date) = store.session() else {
        return json_reply(head, unavailable(body, "series_session_not_started"));
    };
    let Some(bars) = store.bars(instrument) else {
        return json_reply(head, unavailable(body, "instrument_not_observed"));
    };
    let mut rows = Vec::new();
    for bar in bars {
        let Some(at) = minute_to_millis(date, bar.start_minute) else {
            continue;
        };
        if from.is_some_and(|from| at < from) || to.is_some_and(|to| at > to) {
            continue;
        }
        rows.push(serde_json::json!({
            "at":at,
            "open":format!("{}", bar.open),
            "high":format!("{}", bar.high),
            "low":format!("{}", bar.low),
            "close":format!("{}", bar.close),
            "tick_count":bar.tick_count,
            "price_kind":"receipt_price",
            "bar_kind":"receipt_minute",
            "certified_ohlcv":false,
            "volume":serde_json::Value::Null
        }));
    }
    let observed = rows.len();
    let total = rows.len();
    let range_first = rows.first().and_then(|row| row["at"].as_i64());
    let range_last = rows.last().and_then(|row| row["at"].as_i64());
    let truncated = total > limit;
    if truncated {
        rows = rows.split_off(rows.len() - limit);
    }
    let first = rows.first().and_then(|row| row["at"].as_i64());
    let last = rows.last().and_then(|row| row["at"].as_i64());
    let next = truncated
        .then(|| first.map(|first| first.saturating_sub(1)))
        .flatten();
    let history_start_ms = store.history_start_ms();
    let session_open_ms = store.session_open_ms();
    let store_started_after_open = matches!(
        (history_start_ms, session_open_ms),
        (Some(start), Some(open)) if start > open
    );
    let mut gaps = Vec::new();
    'pairs: for pair in rows.windows(2) {
        let (Some(previous), Some(next)) = (pair[0]["at"].as_i64(), pair[1]["at"].as_i64()) else {
            continue;
        };
        if next.saturating_sub(previous) <= 60_000 {
            continue;
        }
        let mut at = previous.saturating_add(60_000);
        let mut run: Option<(i64, i64, bool)> = None;
        while at < next {
            let observed_minute = store.minute_was_observed(at, at.saturating_add(60_000));
            match run {
                Some((from, _, same)) if same == observed_minute => {
                    run = Some((from, at, same));
                }
                Some((from, to, same)) => {
                    gaps.push(series_gap(from, to, same));
                    if gaps.len() == 50 {
                        break 'pairs;
                    }
                    run = Some((at, at, observed_minute));
                }
                None => run = Some((at, at, observed_minute)),
            }
            at = at.saturating_add(60_000);
        }
        if let Some((from, to, observed_minute)) = run {
            gaps.push(series_gap(from, to, observed_minute));
            if gaps.len() == 50 {
                break;
            }
        }
    }
    let expected = range_first.zip(range_last).map_or(0, |(first, last)| {
        ((last.saturating_sub(first) / 60_000) + 1) as usize
    });
    body["points"] = serde_json::json!(rows);
    body["watermark_ms"] = serde_json::json!(last);
    body["coverage"] = serde_json::json!({
        "from_ms":range_first,"to_ms":range_last,"interval_ms":60_000,
        "observed":observed,"expected":expected,
        "gap_count":expected.saturating_sub(observed),
        "gaps":gaps,
        "store_start_ms":history_start_ms,
        "session_open_ms":session_open_ms,
        "store_started_after_open":store_started_after_open
    });
    body["next"] = serde_json::json!(next);
    body["truncated"] = serde_json::json!(truncated);
    body["unavailable"] = serde_json::json!(false);
    body["source"] = serde_json::json!("receipt_derived_feature_store");
    body["note"] = serde_json::json!(
        "Receipt-minute bars are display evidence, not certified exchange OHLCV; tick count is not volume."
    );
    drop(store);
    json_reply(head, body)
}
