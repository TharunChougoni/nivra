//! Controller-only prospective-observation quote capture.
//!
//! This is deliberately distinct from the archive-only `research_quote_capture`
//! tool. It may use the existing application quote client, but only when a
//! reviewed controller grant is enabled, the book is runnable, the session is
//! open and the current universe is verified. It appends one original
//! `research_quotes` research-observation receipt and never writes portfolio,
//! state, plan, order or a new financial book.
use super::*;
use chrono::Timelike;
use sadr_core::execution::Portfolio;

fn capture_settings(
    cfg: &PlannerConfig,
) -> Option<&crate::runtime::owner_tasks::ControllerObservationCaptureSettings> {
    cfg.owner_tasks
        .as_ref()
        .and_then(|settings| settings.research.as_ref())
        .and_then(|research| research.observation_capture.as_ref())
        .filter(|capture| capture.enabled)
}

fn observation_enabled(cfg: &PlannerConfig) -> bool {
    cfg.research_tools
        .as_ref()
        .and_then(|settings| settings.candidate_compute.as_ref())
        .and_then(|candidate| candidate.observation.as_ref())
        .is_some_and(|observation| observation.enabled)
}

fn verified_universe_keys(
    c: &Config,
    max_keys: usize,
) -> std::result::Result<Vec<String>, &'static str> {
    // The controller's verified universe reader re-reads the frozen raw
    // archives and recomputes every hash/mapping. There is no permissive
    // manifest-only fallback: a fabricated 64-hex manifest with missing raw
    // files never reaches the provider.
    let instruments = super::super::super::wide::history_instruments(c).map_err(|error| {
        let code = super::super::super::failure_code(error.as_ref());
        if code.contains("session_mismatch") {
            "verified_universe_session_mismatch"
        } else if code.contains("universe_hash") {
            "verified_universe_hash_mismatch"
        } else if code.contains("mapping_changed") {
            "verified_universe_mapping_changed"
        } else {
            "verified_universe_unavailable"
        }
    })?;
    let mut keys = instruments
        .into_iter()
        .map(|instrument| instrument.key)
        .filter(|key| !key.is_empty() && key.len() <= 128)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    if keys.len() > max_keys {
        return Err("verified_universe_budget_exceeded");
    }
    if keys.is_empty() {
        return Err("verified_universe_unavailable");
    }
    Ok(keys)
}

fn session_start_ms(session: NaiveDate) -> i64 {
    session
        .and_hms_opt(0, 0, 0)
        .map(|local| local.and_utc().timestamp_millis() - 19_800_000)
        .unwrap_or(0)
}

/// Verify one clock sample belongs to the VERIFIED `c.session` trading day and
/// the configured cash session window. The admission, request, receipt and
/// completion clocks are checked separately: a request admitted at 15:29:58
/// must not commit a completion observed at 15:30:03.
fn verified_session_clock(c: &Config, at: i64) -> std::result::Result<(), &'static str> {
    let local = super::super::super::local(at);
    if local.date_naive() != c.session {
        return Err("observation_capture_session_date_mismatch");
    }
    match c.is_trading_day(c.session) {
        Ok(true) => {}
        Ok(false) => return Err("not_a_trading_day"),
        Err(_) => return Err("calendar_unavailable"),
    }
    let minute = local
        .hour()
        .saturating_mul(60)
        .saturating_add(local.minute());
    if !(c.execution.session_start_minute..c.execution.session_end_minute).contains(&minute) {
        return Err("market_closed");
    }
    Ok(())
}

fn capture_usage(db: &Connection, session: NaiveDate) -> Option<(u32, Option<i64>)> {
    let start = session_start_ms(session);
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes' AND at>=?",
            [start],
            |row| row.get(0),
        )
        .ok()?;
    let last: Option<i64> = db
        .query_row(
            "SELECT max(at) FROM ticks WHERE event='research_quotes' AND at>=?",
            [start],
            |row| row.get(0),
        )
        .ok()?;
    Some((u32::try_from(count).unwrap_or(u32::MAX), last))
}

/// Read-only capture coverage for the operational projection. It opens the
/// CURRENT paper book read-only (never the research queue, never creates a
/// schema or row), never calls the provider and never renews a receipt. A
/// missing/corrupt book or journal is `unknown` with null counts, not empty.
pub(crate) fn status(c: &Config, cfg: &PlannerConfig, now: i64) -> Value {
    let Some(settings) = capture_settings(cfg) else {
        return json!({"enabled":false,"coverage":"disabled"});
    };
    let db = rusqlite::Connection::open_with_flags(
        c.directory.join("paper.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    );
    let Ok(db) = db else {
        return json!({
            "enabled":true,
            "status":"unknown",
            "coverage":"book_unavailable",
            "used":null,
            "remaining":null,
            "last_capture_ms":null,
            "last_capture_age_ms":null,
            "provider_called":false,
            "state_written":false
        });
    };
    let _ = db.busy_timeout(std::time::Duration::ZERO);
    let Some((used, last)) = capture_usage(&db, c.session) else {
        return json!({
            "enabled":true,
            "status":"unknown",
            "coverage":"journal_unavailable",
            "used":null,
            "remaining":null,
            "last_capture_ms":null,
            "last_capture_age_ms":null,
            "provider_called":false,
            "state_written":false
        });
    };
    let remaining = settings.max_calls_per_session.saturating_sub(used);
    json!({
        "enabled":true,
        // Journal readability is NOT capture eligibility. This projection never
        // evaluates the current book/session/universe and never claims a
        // provider guarantee; the separate `current_eligibility` label says so.
        "status":"journal_readable",
        "coverage":"journal_readable",
        "coverage_label":"journal_readability_only",
        "current_eligibility":"not_evaluated",
        "eligible_for_attempt":false,
        "max_keys":settings.max_keys,
        "max_calls_per_session":settings.max_calls_per_session,
        "used":used,
        "remaining":remaining,
        "last_capture_ms":last,
        "last_capture_age_ms":last.map(|last| now.saturating_sub(last)),
        "min_interval_ms":settings.min_interval_ms,
        "timeout_ms":settings.timeout_ms,
        "max_rows":settings.max_rows,
        "receipts":if used == 0 { "none" } else { "present" },
        "receipts_status":if used == 0 { "not_present" } else { "present" },
        "provider_called":false,
        "state_written":false,
        "as_of_ms":now
    })
}

/// Capture one bounded research-observation quote receipt for the current
/// verified universe. The quote closure is injectable so tests never open a
/// socket. A disabled grant, stopped/halted book, closed session, missing
/// universe, exhausted quota or provider refusal is an explicit static status;
/// it never unhalts, restarts, fails over or fabricates a quote.
pub(crate) fn tick_with(
    db: &mut Connection,
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    now: i64,
    clock: impl Fn() -> i64,
    quotes: impl FnOnce(&[&str]) -> std::result::Result<data::BoundedQuote, data::BoundedQuoteError>,
) -> Value {
    let Some(settings) = capture_settings(cfg) else {
        return json!({"status":"disabled","request_attempted":false,"provider_called":false,"state_written":false});
    };
    if !observation_enabled(cfg) {
        return json!({"status":"disabled","reason":"candidate_observation_disabled","request_attempted":false,"provider_called":false,"state_written":false});
    }
    if p.halted || p.stopped || p.closed {
        return json!({"status":"not_eligible","reason":if p.halted {"book_halted"} else if p.stopped {"book_stopped"} else {"book_closed"},"request_attempted":false,"provider_called":false,"state_written":false});
    }
    let Ok(day) = c.is_trading_day(c.session) else {
        return json!({"status":"not_eligible","reason":"calendar_unavailable","request_attempted":false,"provider_called":false,"state_written":false});
    };
    if !day {
        return json!({"status":"not_eligible","reason":"not_a_trading_day","request_attempted":false,"provider_called":false,"state_written":false});
    }
    let local = super::super::super::local(now);
    // ADMISSION DATE: a process that survived into the next calendar day must
    // not call the provider for a stale session and only discover the date
    // mismatch at commit time.
    if local.date_naive() != c.session {
        return json!({"status":"not_eligible","reason":"observation_capture_admission_session_date_mismatch","request_attempted":false,"provider_called":false,"state_written":false});
    }
    let minute = local
        .hour()
        .saturating_mul(60)
        .saturating_add(local.minute());
    if !(c.execution.session_start_minute..c.execution.session_end_minute).contains(&minute) {
        return json!({"status":"not_eligible","reason":"market_closed","request_attempted":false,"provider_called":false,"state_written":false});
    }
    let Some((used, last)) = capture_usage(&*db, c.session) else {
        return json!({"status":"unavailable","reason":"observation_capture_journal_unavailable","request_attempted":false,"provider_called":false,"state_written":false});
    };
    if used >= settings.max_calls_per_session {
        return json!({"status":"quota_exhausted","used":used,"max_calls_per_session":settings.max_calls_per_session,"request_attempted":false,"provider_called":false,"state_written":false});
    }
    if last.is_some_and(|last| now.saturating_sub(last) < settings.min_interval_ms) {
        return json!({"status":"not_due","last_capture_ms":last,"min_interval_ms":settings.min_interval_ms,"request_attempted":false,"provider_called":false,"state_written":false});
    }
    let keys = match verified_universe_keys(c, settings.max_keys) {
        Ok(keys) => keys,
        Err(reason) => {
            return json!({"status":"not_eligible","reason":reason,"request_attempted":false,"provider_called":false,"state_written":false});
        }
    };
    let key_refs = keys.iter().map(String::as_str).collect::<Vec<_>>();
    let started = std::time::Instant::now();
    let quote = match quotes(&key_refs) {
        Ok(quote) => quote,
        Err(error) => {
            // Pre-network token/key/journal failures never claim an HTTP
            // request; a started request that failed cannot prove the provider
            // received it, so `provider_called` stays null.
            return json!({
                "status":"unavailable",
                "reason":error.code,
                "request_attempted":error.request_attempted,
                "provider_called":error.provider_called,
                "state_written":false
            });
        }
    };
    // The completion observation clock is sampled AFTER the I/O returns. It is
    // the tick event time; the original data receipt is never renewed. BOTH the
    // request start and the actual completion must still belong to the verified
    // session window, not just the earlier admission sample.
    let completed = clock();
    if completed < now {
        return json!({"status":"refused","reason":"observation_capture_clock_backwards","request_attempted":quote.request_attempted,"provider_called":quote.provider_called,"state_written":false});
    }
    if quote.requested_ms < now {
        return json!({"status":"refused","reason":"observation_capture_request_clock_backwards","request_attempted":quote.request_attempted,"provider_called":quote.provider_called,"state_written":false});
    }
    if quote.receipt_ms < quote.requested_ms || quote.receipt_ms > completed {
        return json!({"status":"refused","reason":"observation_capture_clock_invalid","request_attempted":quote.request_attempted,"provider_called":quote.provider_called,"state_written":false});
    }
    for (at, reason) in [
        (
            quote.requested_ms,
            "observation_capture_requested_outside_session",
        ),
        (
            quote.receipt_ms,
            "observation_capture_receipt_outside_session",
        ),
        (completed, "observation_capture_completed_outside_session"),
    ] {
        if let Err(gap) = verified_session_clock(c, at) {
            return json!({
                "status":"refused",
                "reason":reason,
                "session_gap":gap,
                "at_ms":at,
                "request_attempted":quote.request_attempted,
                "provider_called":quote.provider_called,
                "state_written":false
            });
        }
    }
    if started.elapsed() > std::time::Duration::from_millis(settings.timeout_ms) {
        return json!({
            "status":"timeout",
            "reason":"observation_capture_deadline_exceeded",
            "elapsed_ms":started.elapsed().as_millis(),
            "timeout_ms":settings.timeout_ms,
            "request_attempted":quote.request_attempted,
            "provider_called":quote.provider_called,
            "state_written":false
        });
    }
    match super::super::super::wide::commit_research_quotes(
        db,
        c,
        quote.requested_ms,
        quote.receipt_ms,
        &quote.body,
        completed,
        settings.max_rows,
    ) {
        Ok(()) => json!({
            "status":"captured",
            "purpose":"research_observation",
            "keys":keys.len(),
            "requested_ms":quote.requested_ms,
            "receipt_ms":quote.receipt_ms,
            "event_at_ms":completed,
            "request_attempted":quote.request_attempted,
            "provider_called":quote.provider_called,
            "state_written":false,
            "portfolio_written":false,
            "order_written":false
        }),
        Err(error) => {
            json!({"status":"refused","reason":error.to_string(),"request_attempted":quote.request_attempted,"provider_called":quote.provider_called,"state_written":false})
        }
    }
}

/// Controller entry point using the existing application quote client. The
/// admission clock (`now`) is separate from the completion clock sampled after
/// the HTTP response; a real post-network receipt is never compared to a stale
/// loop-start sample.
pub(crate) fn tick(
    db: &mut Connection,
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    client: &data::Client,
    now: i64,
) -> Value {
    let timeout_ms = capture_settings(cfg)
        .map(|settings| settings.timeout_ms)
        .unwrap_or(5_000);
    tick_with(db, c, cfg, p, now, super::super::super::millis, |keys| {
        client.quotes_bounded(c, keys, timeout_ms)
    })
}
