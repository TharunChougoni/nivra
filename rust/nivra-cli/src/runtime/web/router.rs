//! Pure routing and strict bounded query decoding. Authentication stays upstream.
use std::collections::BTreeMap;
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    Meta,
    Summary,
    Portfolio,
    Positions(Query),
    Orders(Query),
    Fills(Query),
    Plans(Query),
    Scanner(Query),
    Instruments(Query),
    InstrumentChain(Query),
    Sessions(Query),
    Health,
    Alerts(Query),
    /// Bounded orchestrator web-tool call record (#222).
    Tools(Query),
    /// The pi lane turns, tool calls, gate verdicts and lane spend.
    Agent(Query),
    /// One pi lane as a chat: its session files grouped into exchanges.
    AgentTranscript(Query),
    Series(Query),
    Events(Query),
    Backtest,
    BacktestExample,
    UnknownV1,
    Unknown,
}
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Query(BTreeMap<String, String>);
impl Query {
    pub(super) fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }
    pub(super) fn parse(raw: &str, allowed: &[&str]) -> Result<Self, &'static str> {
        // Fixed untrusted-query bound, matching the v1 HTTP contract.
        if raw.len() > 2048 {
            return Err("query_too_long");
        }
        let mut fields = BTreeMap::new();
        if !raw.is_empty() {
            for pair in raw.split('&') {
                let (key, value) = pair.split_once('=').ok_or("query_invalid")?;
                let key = decode(key)?;
                let value = decode(value)?;
                if !allowed.contains(&key.as_str()) {
                    return Err("query_unknown_key");
                }
                if fields.insert(key, value).is_some() {
                    return Err("query_duplicate_key");
                }
            }
        }
        Ok(Self(fields))
    }
}
pub(super) fn decode(text: &str) -> Result<String, &'static str> {
    let hex = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let byte = if bytes[i] == b'%' {
            let hi = bytes
                .get(i + 1)
                .and_then(|b| hex(*b))
                .ok_or("query_encoding_invalid")?;
            let lo = bytes
                .get(i + 2)
                .and_then(|b| hex(*b))
                .ok_or("query_encoding_invalid")?;
            i += 3;
            hi * 16 + lo
        } else {
            let byte = if bytes[i] == b'+' { b' ' } else { bytes[i] };
            i += 1;
            byte
        };
        if byte.is_ascii_control() {
            return Err("query_encoding_invalid");
        }
        output.push(byte);
    }
    String::from_utf8(output).map_err(|_| "query_encoding_invalid")
}
pub(super) fn is_v1(line: &str) -> bool {
    line.split_ascii_whitespace().nth(1).is_some_and(|path| {
        path == "/api/v1" || path.starts_with("/api/v1/") || path.starts_with("/api/v1?")
    })
}
pub(super) fn parse(line: &str) -> Result<Route, &'static str> {
    let mut parts = line.split_ascii_whitespace();
    let method = parts.next().ok_or("request_invalid")?;
    let target = parts.next().ok_or("request_invalid")?;
    if parts.next() != Some("HTTP/1.1") || parts.next().is_some() {
        return Err("request_invalid");
    }
    let (path, raw) = target.split_once('?').unwrap_or((target, ""));
    if is_v1(line) {
        // Dashboard note/aside writes are removed; only offline replay may POST.
        let post_allowed = path == "/api/v1/backtest";
        if method != "GET" && !(method == "POST" && post_allowed) {
            return Err("method_not_allowed");
        }
        return match path {
            "/api/v1/meta" => {
                Query::parse(raw, &[])?;
                Ok(Route::Meta)
            }
            "/api/v1/summary" => {
                Query::parse(raw, &[])?;
                Ok(Route::Summary)
            }
            "/api/v1/portfolio" => {
                Query::parse(raw, &[])?;
                Ok(Route::Portfolio)
            }
            "/api/v1/positions" => Ok(Route::Positions(Query::parse(raw, &["cursor", "limit"])?)),
            "/api/v1/orders" => Ok(Route::Orders(Query::parse(
                raw,
                &["cursor", "limit", "date"],
            )?)),
            "/api/v1/fills" => Ok(Route::Fills(Query::parse(raw, &["cursor", "limit"])?)),
            "/api/v1/plans" => Ok(Route::Plans(Query::parse(raw, &["date"])?)),
            "/api/v1/scanner" => Ok(Route::Scanner(Query::parse(
                raw,
                &["segment", "cursor", "limit"],
            )?)),
            "/api/v1/instruments" => Ok(Route::Instruments(Query::parse(
                raw,
                &["cursor", "limit", "scope", "market_segment", "kind", "q"],
            )?)),
            "/api/v1/instruments/chain" => Ok(Route::InstrumentChain(Query::parse(
                raw,
                &[
                    "cursor",
                    "limit",
                    "underlying_key",
                    "expiry_ms",
                    "market_segment",
                ],
            )?)),
            "/api/v1/sessions" => Ok(Route::Sessions(Query::parse(raw, &["cursor", "limit"])?)),
            "/api/v1/health" => {
                Query::parse(raw, &[])?;
                Ok(Route::Health)
            }
            "/api/v1/alerts" => Ok(Route::Alerts(Query::parse(raw, &["cursor", "limit"])?)),
            "/api/v1/tools" => {
                if method != "GET" {
                    return Err("method_not_allowed");
                }
                Ok(Route::Tools(Query::parse(
                    raw,
                    &["date", "cursor", "limit"],
                )?))
            }
            "/api/v1/agent" => Ok(Route::Agent(Query::parse(raw, &["cursor", "limit"])?)),
            "/api/v1/agent/transcript" => Ok(Route::AgentTranscript(Query::parse(
                raw,
                &["lane", "cursor", "limit"],
            )?)),
            "/api/v1/series" => Ok(Route::Series(Query::parse(
                raw,
                &["instrument", "kind", "from", "to", "limit"],
            )?)),
            "/api/v1/events" => Ok(Route::Events(Query::parse(raw, &["after", "poll"])?)),
            "/api/v1/backtest" => {
                if method != "POST" {
                    return Err("method_not_allowed");
                }
                Query::parse(raw, &[])?;
                Ok(Route::Backtest)
            }
            "/api/v1/backtest/example" => {
                Query::parse(raw, &[])?;
                Ok(Route::BacktestExample)
            }
            _ => Ok(Route::UnknownV1),
        };
    }
    Ok(Route::Unknown)
}
#[cfg(test)]
mod tests {}
