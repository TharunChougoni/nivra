use super::*;
use nivra_store::market_archive::{
    ArchiveClient, archive_daily_bars, archive_minute_bars, is_connection_error,
};

pub(crate) const ARCHIVE_MINUTE_MAX_RANGE_MS: i64 = 14 * 86_400_000;
pub(crate) const ARCHIVE_DAILY_MAX_RANGE_MS: i64 = 366 * 86_400_000;

/// Lazily opened single-connection archive reader used only on dashboard worker threads.
pub(crate) struct ArchiveReader {
    settings: MarketArchiveSettings,
    client: Mutex<Option<ArchiveClient>>,
}

impl ArchiveReader {
    pub(crate) fn new(settings: MarketArchiveSettings) -> Self {
        Self {
            settings,
            client: Mutex::new(None),
        }
    }

    pub(crate) fn with_client<T>(
        &self,
        mut f: impl FnMut(&mut ArchiveClient) -> crate::runtime::Result<T>,
    ) -> crate::runtime::Result<T> {
        let mut guard = self
            .client
            .lock()
            .map_err(|_| "archive_unavailable".to_string())?;
        if guard.as_ref().is_some_and(ArchiveClient::is_closed) {
            *guard = None;
        }
        if guard.is_none() {
            *guard = Some(crate::runtime::market_archive::connect(&self.settings)?);
        }
        let result = {
            let client = guard.as_mut().expect("archive client");
            f(client)
        };
        match result {
            Ok(value) => Ok(value),
            Err(first_err) => {
                let closed = guard.as_ref().is_some_and(ArchiveClient::is_closed);
                if !closed && !is_connection_error(first_err.as_ref()) {
                    return Err(first_err);
                }
                *guard = None;
                let mut reconnected = crate::runtime::market_archive::connect(&self.settings)?;
                match f(&mut reconnected) {
                    Ok(value) => {
                        *guard = Some(reconnected);
                        Ok(value)
                    }
                    Err(second_err) => Err(second_err),
                }
            }
        }
    }
}

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
fn millis_to_ist_date(ms: i64) -> Option<NaiveDate> {
    let offset = FixedOffset::east_opt(19_800)?;
    chrono::DateTime::from_timestamp_millis(ms).map(|dt| dt.with_timezone(&offset).date_naive())
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
fn paise_text(paise: i64) -> crate::runtime::Result<String> {
    Ok(format!(
        "{}",
        crate::runtime::market_archive::from_paise(paise)?
    ))
}
fn archive_series(
    head: &str,
    kind: &str,
    instrument: &str,
    (from, to): (i64, i64),
    limit: usize,
    mut body: serde_json::Value,
    archive: Option<&ArchiveReader>,
) -> Response {
    let Some(archive) = archive else {
        body["points"] = serde_json::json!([]);
        return json_reply(head, unavailable(body, "archive_not_configured"));
    };
    if kind == "archive-daily-bar" {
        let (Some(start), Some(end)) = (millis_to_ist_date(from), millis_to_ist_date(to)) else {
            return error("400 Bad Request", "series_range_invalid");
        };
        let Ok((bars, truncated)) =
            archive.with_client(|db| archive_daily_bars(db, instrument, start, end, limit))
        else {
            body["points"] = serde_json::json!([]);
            return json_reply(head, unavailable(body, "archive_unavailable"));
        };
        let mut rows = Vec::with_capacity(bars.len());
        for bar in bars {
            let (Some(at), Ok(open), Ok(high), Ok(low), Ok(close)) = (
                minute_to_millis(bar.day, 0),
                paise_text(bar.open_paise),
                paise_text(bar.high_paise),
                paise_text(bar.low_paise),
                paise_text(bar.close_paise),
            ) else {
                continue;
            };
            rows.push(serde_json::json!({
                "at":at,
                "day":bar.day.to_string(),
                "open":open,
                "high":high,
                "low":low,
                "close":close,
                "tick_count":serde_json::Value::Null,
                "price_kind":"archive_candle",
                "bar_kind":"archive_daily",
                "certified_ohlcv":false,
                "volume":bar.volume
            }));
        }
        let observed = rows.len();
        let first = rows.first().and_then(|row| row["at"].as_i64());
        let last = rows.last().and_then(|row| row["at"].as_i64());
        let next = truncated
            .then(|| first.map(|first| first.saturating_sub(1)))
            .flatten();
        body["points"] = serde_json::json!(rows);
        body["watermark_ms"] = serde_json::json!(last);
        body["coverage"] = serde_json::json!({
            "from_ms":first,
            "to_ms":last,
            "interval_ms":86_400_000,
            "observed":observed,
            "expected":observed,
            "gap_count":0,
            "gaps":[],
            "store_start_ms":serde_json::Value::Null,
            "session_open_ms":serde_json::Value::Null,
            "store_started_after_open":false
        });
        body["next"] = serde_json::json!(next);
        body["truncated"] = serde_json::json!(truncated);
        body["unavailable"] = serde_json::json!(false);
        body["source"] = serde_json::json!("market_archive");
        return json_reply(head, body);
    }

    let Ok((bars, truncated)) =
        archive.with_client(|db| archive_minute_bars(db, instrument, from, to, limit))
    else {
        body["points"] = serde_json::json!([]);
        return json_reply(head, unavailable(body, "archive_unavailable"));
    };
    let mut rows = Vec::with_capacity(bars.len());
    for bar in bars {
        let (Ok(open), Ok(high), Ok(low), Ok(close)) = (
            paise_text(bar.open_paise),
            paise_text(bar.high_paise),
            paise_text(bar.low_paise),
            paise_text(bar.close_paise),
        ) else {
            continue;
        };
        rows.push(serde_json::json!({
            "at":bar.bar_ms,
            "open":open,
            "high":high,
            "low":low,
            "close":close,
            "tick_count":bar.tick_count,
            "price_kind":if bar.from_quote { "receipt_price" } else { "archive_candle" },
            "bar_kind":if bar.from_quote { "archive_quote_minute" } else { "archive_minute" },
            "certified_ohlcv":false,
            "volume":bar.volume
        }));
    }
    let observed = rows.len();
    let first = rows.first().and_then(|row| row["at"].as_i64());
    let last = rows.last().and_then(|row| row["at"].as_i64());
    let next = truncated
        .then(|| first.map(|first| first.saturating_sub(1)))
        .flatten();
    let mut gaps = Vec::new();
    for pair in rows.windows(2) {
        let (Some(previous), Some(next_at)) = (pair[0]["at"].as_i64(), pair[1]["at"].as_i64())
        else {
            continue;
        };
        if next_at.saturating_sub(previous) > 60_000 {
            gaps.push(series_gap(
                previous.saturating_add(60_000),
                next_at.saturating_sub(60_000),
                false,
            ));
            if gaps.len() == 50 {
                break;
            }
        }
    }
    let expected = first.zip(last).map_or(0, |(first, last)| {
        ((last.saturating_sub(first) / 60_000) + 1) as usize
    });
    body["points"] = serde_json::json!(rows);
    body["watermark_ms"] = serde_json::json!(last);
    body["coverage"] = serde_json::json!({
        "from_ms":first,
        "to_ms":last,
        "interval_ms":60_000,
        "observed":observed,
        "expected":expected,
        "gap_count":expected.saturating_sub(observed),
        "gaps":gaps,
        "store_start_ms":serde_json::Value::Null,
        "session_open_ms":serde_json::Value::Null,
        "store_started_after_open":false
    });
    body["next"] = serde_json::json!(next);
    body["truncated"] = serde_json::json!(truncated);
    body["unavailable"] = serde_json::json!(false);
    body["source"] = serde_json::json!("market_archive");
    json_reply(head, body)
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn series(
    head: &str,
    query: router::Query,
    view: &Arc<Mutex<serde_json::Value>>,
    settings: &DashboardSettings,
    features: Option<&Arc<Mutex<FeatureStore>>>,
    equity: Option<&Arc<Mutex<EquitySeries>>>,
    archive: Option<&ArchiveReader>,
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
    let kind = query.get("kind").unwrap_or("receipt-minute-bar");
    if ![
        "receipt-minute-bar",
        "account-equity",
        "archive-minute-bar",
        "archive-daily-bar",
    ]
    .contains(&kind)
    {
        return error("400 Bad Request", "series_kind_invalid");
    }
    let is_archive = matches!(kind, "archive-minute-bar" | "archive-daily-bar");
    let max_range_ms = match kind {
        "archive-minute-bar" => ARCHIVE_MINUTE_MAX_RANGE_MS,
        "archive-daily-bar" => ARCHIVE_DAILY_MAX_RANGE_MS,
        _ => settings.series_max_range_ms as i64,
    };
    if is_archive && (from.is_none() || to.is_none()) {
        return error("400 Bad Request", "series_range_required");
    }
    if let (Some(from), Some(to)) = (from, to) {
        if to < from {
            return error("400 Bad Request", "series_range_invalid");
        }
        if to.saturating_sub(from) > max_range_ms {
            return error("400 Bad Request", "series_range_too_large");
        }
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
    if is_archive {
        return archive_series(
            head,
            kind,
            instrument,
            (from.expect("checked"), to.expect("checked")),
            limit,
            body,
            archive,
        );
    }
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
        let Some(at) = minute_to_millis(date, bar.start_minute()) else {
            continue;
        };
        if from.is_some_and(|from| at < from) || to.is_some_and(|to| at > to) {
            continue;
        }
        rows.push(serde_json::json!({
            "at":at,
            "open":format!("{}", bar.open()),
            "high":format!("{}", bar.high()),
            "low":format!("{}", bar.low()),
            "close":format!("{}", bar.close()),
            "tick_count":bar.tick_count(),
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
