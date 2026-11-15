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
    Approvals,
    Alerts(Query),
    /// Persistent orchestrator session view.
    Session(Query),
    /// Bounded orchestrator web-tool call record (#222).
    Tools(Query),
    Series(Query),
    Runs(Query),
    Events(Query),
    Run(String),
    Blob {
        run: String,
        name: String,
        query: Query,
    },
    Prompt(String),
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
            "/api/v1/approvals" => {
                Query::parse(raw, &[])?;
                Ok(Route::Approvals)
            }
            "/api/v1/alerts" => Ok(Route::Alerts(Query::parse(raw, &["cursor", "limit"])?)),
            "/api/v1/session" => {
                if method != "GET" {
                    return Err("method_not_allowed");
                }
                Ok(Route::Session(Query::parse(raw, &["date"])?))
            }
            "/api/v1/tools" => {
                if method != "GET" {
                    return Err("method_not_allowed");
                }
                Ok(Route::Tools(Query::parse(
                    raw,
                    &["date", "cursor", "limit"],
                )?))
            }
            "/api/v1/series" => Ok(Route::Series(Query::parse(
                raw,
                &["instrument", "kind", "from", "to", "limit"],
            )?)),
            "/api/v1/runs" => Ok(Route::Runs(Query::parse(
                raw,
                &[
                    "cursor",
                    "limit",
                    "role",
                    "identity",
                    "date",
                    "trigger",
                    "outcome",
                    "instrument",
                    "failed",
                ],
            )?)),
            "/api/v1/events" => Ok(Route::Events(Query::parse(raw, &["after", "poll"])?)),
            path if path.starts_with("/api/v1/runs/") => {
                let rest = path.strip_prefix("/api/v1/runs/").unwrap();
                if let Some((id, name)) = rest.split_once("/blobs/") {
                    let query = Query::parse(raw, &["offset", "len"])?;
                    let id = decode(id).map_err(|_| "run_not_found")?;
                    if !super::super::model_history::valid_id(&id) {
                        return Err("run_not_found");
                    }
                    Ok(Route::Blob {
                        run: id,
                        name: decode(name).map_err(|_| "blob_not_found")?,
                        query,
                    })
                } else {
                    Query::parse(raw, &[])?;
                    let id = decode(rest).map_err(|_| "run_not_found")?;
                    if !super::super::model_history::valid_id(&id) {
                        return Err("run_not_found");
                    }
                    Ok(Route::Run(id))
                }
            }
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
            path if path.starts_with("/api/v1/prompts/") => {
                Query::parse(raw, &[])?;
                let sha = path.strip_prefix("/api/v1/prompts/").unwrap();
                if sha.len() != 64
                    || !sha
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err("prompt_not_found");
                }
                Ok(Route::Prompt(sha.into()))
            }
            _ => Ok(Route::UnknownV1),
        };
    }
    Ok(Route::Unknown)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes_reject_removed_legacy_and_strictly_decode_v1_queries() {
        for (path, route) in [
            ("/api/v1/meta", Route::Meta),
            ("/api/v1/summary", Route::Summary),
            ("/api/v1/portfolio", Route::Portfolio),
            ("/api/v1/positions", Route::Positions(Query::default())),
            ("/api/v1/orders", Route::Orders(Query::default())),
            ("/api/v1/fills", Route::Fills(Query::default())),
            (
                "/api/v1/plans?date=2026-10-29",
                Route::Plans(Query::parse("date=2026-10-29", &["date"]).unwrap()),
            ),
            (
                "/api/v1/scanner?segment=prospects",
                Route::Scanner(
                    Query::parse("segment=prospects", &["segment", "cursor", "limit"]).unwrap(),
                ),
            ),
            (
                "/api/v1/instruments?cursor=100&limit=50",
                Route::Instruments(
                    Query::parse("cursor=100&limit=50", &["cursor", "limit"]).unwrap(),
                ),
            ),
            ("/api/v1/sessions", Route::Sessions(Query::default())),
            (
                "/api/v1/runs?identity=orchestrator",
                Route::Runs(Query::parse("identity=orchestrator", &["identity"]).unwrap()),
            ),
            ("/api/v1/health", Route::Health),
            ("/api/v1/approvals", Route::Approvals),
            ("/api/v1/alerts", Route::Alerts(Query::default())),
            (
                "/api/v1/session?date=2026-10-31",
                Route::Session(Query::parse("date=2026-10-31", &["date"]).unwrap()),
            ),
            (
                "/api/v1/tools?date=2026-10-31&limit=25",
                Route::Tools(
                    Query::parse("date=2026-10-31&limit=25", &["date", "cursor", "limit"]).unwrap(),
                ),
            ),
            (
                "/api/v1/series?instrument=NSE_EQ%7CINE00U801010",
                Route::Series(
                    Query::parse(
                        "instrument=NSE_EQ%7CINE00U801010",
                        &["instrument", "kind", "from", "to", "limit"],
                    )
                    .unwrap(),
                ),
            ),
            ("/api/v1/backtest/example", Route::BacktestExample),
            ("/api/v1/unknown", Route::UnknownV1),
            ("/", Route::Unknown),
            ("/legacy/", Route::Unknown),
            ("/assets/theme.js", Route::Unknown),
            ("/api/status", Route::Unknown),
            ("/api/model-history?offset=0", Route::Unknown),
            ("/api/model-run?id=x", Route::Unknown),
            ("/api/model-stream?after=0", Route::Unknown),
            ("/api/backtest", Route::Unknown),
            ("/api/backtest/example", Route::Unknown),
            ("/old", Route::Unknown),
        ] {
            assert_eq!(parse(&format!("GET {path} HTTP/1.1")), Ok(route));
        }
        assert_eq!(parse("POST /api/v1/backtest HTTP/1.1"), Ok(Route::Backtest));
        // Note/aside writes are removed from the dashboard surface; GET
        // /api/v1/session still serves their recorded evidence read-only.
        for path in [
            "/api/v1/session/notes",
            "/api/v1/session/notes/clear",
            "/api/v1/session/notes/keep",
            "/api/v1/session/aside",
        ] {
            assert_eq!(
                parse(&format!("POST {path} HTTP/1.1")),
                Err("method_not_allowed")
            );
            assert_eq!(parse(&format!("GET {path} HTTP/1.1")), Ok(Route::UnknownV1));
        }
        assert_eq!(
            parse("GET /api/v1/backtest HTTP/1.1"),
            Err("method_not_allowed")
        );
        assert_eq!(
            parse("POST /api/v1/meta HTTP/1.1"),
            Err("method_not_allowed")
        );
        assert_eq!(
            parse("POST /api/v1/tools HTTP/1.1"),
            Err("method_not_allowed")
        );
        assert_eq!(
            parse("GET /api/v1/tools?offset=1 HTTP/1.1"),
            Err("query_unknown_key")
        );
        assert_eq!(
            parse("GET /api/v1/meta?unexpected=1 HTTP/1.1"),
            Err("query_unknown_key")
        );
        assert_eq!(
            Query::parse("key=%E2%82%B9+value", &["key"]).unwrap().0["key"],
            "₹ value"
        );
        for raw in ["key=%", "key=%1", "key=%GG", "key=%FF", "key=%00", "key"] {
            assert!(Query::parse(raw, &["key"]).is_err(), "{raw}");
        }
        assert_eq!(
            Query::parse("key=1&%6Bey=2", &["key"]),
            Err("query_duplicate_key")
        );
        assert_eq!(
            Query::parse(&format!("key={}", "x".repeat(2048)), &["key"]),
            Err("query_too_long")
        );
    }
}
