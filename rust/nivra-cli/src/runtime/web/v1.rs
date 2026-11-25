//! Authenticated v1 projections. No financial writes or execution authority.
use super::*;
use chrono::{FixedOffset, NaiveDate, TimeZone};
use nivra_core::execution::Portfolio;
use nivra_core::feature_store::FeatureStore;
use router::Route;

mod book;
mod journal;
mod market;
mod ops;
mod revision;
mod series;
mod stream;
mod summary;
mod transcript;
use book::*;
use market::*;
use ops::*;
use revision::*;
use series::*;
#[cfg(test)]
pub(crate) use stream::TestFeed;
use stream::*;
pub(crate) use stream::{PriceBatches, poll_events, start_sse, wants_sse};
#[cfg(test)]
pub(crate) use summary::reset_prepared_summary;
use summary::*;
pub(crate) use summary::{publish_summary, update_prepared_summary};

pub(crate) fn error(code: &'static str, reason: &str) -> Response {
    response(
        code,
        "application/json",
        serde_json::json!({"v":1,"error":reason}).to_string(),
    )
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn dispatch(
    route: std::result::Result<Route, &'static str>,
    head: &str,
    _body: &[u8],
    view: &Arc<Mutex<serde_json::Value>>,
    settings: &DashboardSettings,
    session_root: Option<&Path>,
    features: Option<&Arc<Mutex<FeatureStore>>>,
    equity: Option<&Arc<Mutex<EquitySeries>>>,
    instrument_catalog: Option<&Arc<Mutex<Option<crate::runtime::instrument_display::Catalog>>>>,
) -> Response {
    match route {
        Ok(Route::Meta) => meta(head, view, settings),
        Ok(Route::Summary) => summary(head, view, session_root),
        Ok(Route::Portfolio) => portfolio(head, view),
        Ok(Route::Positions(query)) => positions(head, query, view),
        Ok(Route::Orders(query)) => orders(head, query, view),
        Ok(Route::Fills(query)) => fills(head, query, view),
        Ok(Route::Plans(query)) => plans(head, query, view),
        Ok(Route::Scanner(query)) => scanner(head, query, view, settings),
        Ok(Route::Instruments(query)) => instruments(head, query, view, instrument_catalog),
        Ok(Route::InstrumentChain(query)) => {
            instrument_chain(head, query, view, instrument_catalog)
        }
        Ok(Route::Sessions(query)) => sessions(head, query, view),
        Ok(Route::Health) => health(head, view),
        Ok(Route::Alerts(query)) => alerts(head, query, view),
        Ok(Route::Tools(query)) => tool_calls(head, query, view, session_root),
        Ok(Route::Agent(query)) => agent_turns(head, query, session_root),
        Ok(Route::AgentTranscript(query)) => {
            transcript::agent_transcript(head, query, session_root, settings)
        }
        Ok(Route::Series(query)) => series(head, query, view, settings, features, equity),
        Ok(Route::Events(query)) => poll_source(head, query, EventSource::Global, settings),
        Ok(_) => error("404 Not Found", "route_not_found"),
        Err("method_not_allowed") => error("405 Method Not Allowed", "method_not_allowed"),
        Err(reason) => error("400 Bad Request", reason),
    }
}

fn json_reply(head: &str, body: serde_json::Value) -> Response {
    let body = super::super::transcript::redact(&body);
    let text = body.to_string();
    let etag = format!("\"{}\"", &digest(text.as_bytes())[..16]);
    let matched = values(head, "if-none-match") == [etag.as_str()];
    let mut reply = response(
        if matched {
            "304 Not Modified"
        } else {
            "200 OK"
        },
        "application/json",
        if matched { "" } else { &text },
    );
    reply.cache = Cow::Borrowed("private, no-cache");
    reply.etag = Some(etag);
    reply
}
/// Order authority is explicit: paper mode executes against the deterministic
/// paper ledger, so it is reported as `paper`. A live order path is not
/// implemented here and stays unknown instead of implying a live broker (#24).
fn order_authority(live: bool) -> serde_json::Value {
    if live {
        serde_json::Value::Null
    } else {
        serde_json::json!("paper")
    }
}
fn envelope(view: &serde_json::Value) -> serde_json::Value {
    let live = LIVE_ENABLED || view["live_enabled"].as_bool().unwrap_or(false);
    let account = view["account"].as_str().unwrap_or("");
    let session = view["authorized_session"].as_str().unwrap_or("");
    serde_json::json!({
        "v":1,
        "mode":if live {"live"} else {"paper"},
        "paper_only":view["paper_only"].as_bool().unwrap_or(!live),
        "live_enabled":live,
        "account":account,
        "broker":{"data":"upstox","orders":order_authority(live)},
        "session":session,
        "binding":format!("upstox|{account}|{session}"),
        "as_of_ms":view["as_of_ms"],
        "execution_authority":false
    })
}
fn unavailable(mut body: serde_json::Value, reason: &str) -> serde_json::Value {
    body["unavailable"] = serde_json::json!(true);
    body["reason"] = serde_json::json!(reason);
    body
}

fn page_limit(
    query: &router::Query,
    key: &str,
    default: usize,
    max: usize,
) -> std::result::Result<usize, &'static str> {
    match query.get(key) {
        None => Ok(default),
        Some(value) if value.len() <= 6 && value.bytes().all(|b| b.is_ascii_digit()) => value
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=max).contains(n))
            .ok_or("page_limit_invalid"),
        Some(_) => Err("page_limit_invalid"),
    }
}
fn page_offset(query: &router::Query, key: &str) -> std::result::Result<usize, &'static str> {
    match query.get(key) {
        None => Ok(0),
        Some(value) if value.len() <= 12 && value.bytes().all(|b| b.is_ascii_digit()) => {
            value.parse::<usize>().map_err(|_| "page_cursor_invalid")
        }
        Some(_) => Err("page_cursor_invalid"),
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod event_tests;
