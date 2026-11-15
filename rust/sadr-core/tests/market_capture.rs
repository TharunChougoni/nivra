use sadr_core::{
    market::Calendar, market_capture::MarketCapture,
    market_capture::normalize_upstox_historical_capture,
    market_capture::normalize_upstox_intraday_capture,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct UpstoxIntradayVectors {
    capture: serde_json::Value,
    cases: Vec<UpstoxIntradayCase>,
}
#[derive(Deserialize)]
struct UpstoxIntradayCase {
    id: String,
    mutation: String,
    accepted: bool,
    bars: Option<usize>,
}

#[derive(Deserialize)]
struct UpstoxHistoryVectors {
    capture: serde_json::Value,
    cases: Vec<UpstoxIntradayCase>,
}

#[test]
fn upstox_intraday_capture_requires_exact_completed_session_coverage() {
    let vectors: UpstoxIntradayVectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/upstox_intraday_vectors.json"
    ))
    .unwrap();
    assert_eq!(vectors.cases.len(), 20);
    for case in vectors.cases {
        let mut capture = vectors.capture.clone();
        let mut calendar_day = "2026-10-01";
        let mut body: serde_json::Value =
            serde_json::from_str(capture["body"].as_str().unwrap()).unwrap();
        let mut special = None;
        match case.mutation.as_str() {
            "none" => {}
            "forming" => {
                body["data"]["candles"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!([
                        "2026-10-01T09:16:00+05:30",
                        100,
                        101,
                        99,
                        100,
                        10,
                        0
                    ]));
            }
            "duplicate" => {
                let row = body["data"]["candles"][0].clone();
                body["data"]["candles"].as_array_mut().unwrap().push(row);
            }
            "short-session" => {
                capture["received_at"] = "2026-10-01T03:48:00Z".into();
                special = Some(serde_json::json!(["09:15:00", "09:16:00"]));
            }
            "missing" => body["data"]["candles"] = serde_json::json!([]),
            "off-grid" => body["data"]["candles"][0][0] = "2026-10-01T09:15:30+05:30".into(),
            "boolean-volume" => body["data"]["candles"][0][5] = true.into(),
            "float-volume" => body["data"]["candles"][0][5] = serde_json::json!(1.5),
            "negative-oi" => body["data"]["candles"][0][6] = serde_json::json!(-1),
            "ohlc" => body["data"]["candles"][0][2] = serde_json::json!(99),
            "naive" => body["data"]["candles"][0][0] = "2026-10-01T09:15:00".into(),
            "conflict" => {
                body["data"]["candles"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!([
                        "2026-10-01T09:15:00+05:30",
                        100,
                        101,
                        99,
                        101,
                        10,
                        0
                    ]));
            }
            "gap" => {
                capture["received_at"] = "2026-10-01T03:48:00Z".into();
                body["data"]["candles"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!([
                        "2026-10-01T09:17:00+05:30",
                        100,
                        101,
                        99,
                        100,
                        10,
                        0
                    ]));
            }
            "wrong-route" => {
                capture["path"] = "/v3/historical-candle/intraday/OTHER/minutes/1".into()
            }
            "params" => capture["params"]["instrument_key"] = "OTHER".into(),
            "weekend" => {
                calendar_day = "2026-10-03";
                capture["requested_at"] = "2026-10-03T03:45:00Z".into();
                capture["received_at"] = "2026-10-03T03:46:00Z".into();
                body["data"]["candles"][0][0] = "2026-10-03T09:15:00+05:30".into();
            }
            "rollover" => capture["received_at"] = "2026-10-01T18:31:00Z".into(),
            "pre-open" => capture["requested_at"] = "2026-10-01T03:44:59Z".into(),
            "odd-session" => special = Some(serde_json::json!(["09:15:00", "09:16:30"])),
            "over-limit" => {
                let row = body["data"]["candles"][0].clone();
                body["data"]["candles"] = serde_json::Value::Array(vec![row; 1001]);
            }
            _ => panic!("unknown Upstox minute vector"),
        }
        capture["body"] = body.to_string().into();
        let mut calendar_json = serde_json::json!({
            "source":"synthetic-contract-test-calendar",
            "valid_from":calendar_day,
            "valid_to":calendar_day,
            "synthetic":false
        });
        if let Some(hours) = special {
            let mut sessions = serde_json::Map::new();
            sessions.insert(calendar_day.to_owned(), hours);
            calendar_json["special"] = serde_json::Value::Object(sessions);
        }
        let calendar: Calendar = serde_json::from_value(calendar_json).unwrap();
        let parsed = serde_json::to_vec(&capture)
            .map_err(|_| ())
            .and_then(|raw| MarketCapture::parse(&raw).map_err(|_| ()));
        let result = parsed.and_then(|capture| {
            normalize_upstox_intraday_capture(&capture, "NSE_EQ|FIXTURE", &calendar).map_err(|_| ())
        });
        assert_eq!(result.is_ok(), case.accepted, "{}", case.id);
        if let Ok(bars) = result {
            assert_eq!(bars.len(), case.bars.unwrap(), "{}", case.id);
            assert_eq!(
                bars[0].timestamp.to_string(),
                "2026-10-01T03:45:00Z",
                "{}",
                case.id
            );
            let available = if case.mutation == "short-session" {
                "2026-10-01T03:48:00Z"
            } else {
                "2026-10-01T03:46:00Z"
            };
            assert_eq!(bars[0].available_at.to_string(), available, "{}", case.id);
            assert_eq!(bars[0].volume, 10, "{}", case.id);
            assert_eq!(bars[0].open_interest, Some(0), "{}", case.id);
            assert!(
                bars.iter().all(|bar| bar.available_at > bar.timestamp),
                "{}",
                case.id
            );
        }
    }
}

#[test]
fn upstox_historical_capture_requires_a_complete_past_window() {
    let vectors: UpstoxHistoryVectors = serde_json::from_str(include_str!(
        "../../../fixtures/rust/upstox_history_vectors.json"
    ))
    .unwrap();
    assert_eq!(vectors.cases.len(), 15);
    for case in vectors.cases {
        let mut capture = vectors.capture.clone();
        let mut body: serde_json::Value =
            serde_json::from_str(capture["body"].as_str().unwrap()).unwrap();
        let mut special = serde_json::json!(["09:15:00", "09:17:00"]);
        match case.mutation.as_str() {
            "none" => {}
            "skip-closed" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2026-10-03/2026-10-01".into();
                capture["requested_at"] = "2026-10-05T03:45:00Z".into();
                capture["received_at"] = "2026-10-05T03:46:00Z".into();
            }
            "closed-window" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2026-10-03/2026-10-03".into();
                capture["requested_at"] = "2026-10-05T03:45:00Z".into();
                capture["received_at"] = "2026-10-05T03:46:00Z".into();
                body["data"]["candles"] = serde_json::json!([]);
            }
            "missing" => {
                body["data"]["candles"].as_array_mut().unwrap().pop();
            }
            "request-day" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2026-10-02/2026-10-02".into();
            }
            "span" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2026-10-01/2026-09-03".into();
            }
            "before-2022" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2021-12-31/2021-12-31".into();
            }
            "reversed" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2026-09-30/2026-10-01".into();
            }
            "params" => capture["params"]["instrument_key"] = "OTHER".into(),
            "intraday" => {
                capture["path"] = "/v3/historical-candle/intraday/NSE_EQ%7CFIXTURE/minutes/1".into()
            }
            "weekend-candle" => {
                capture["path"] =
                    "/v3/historical-candle/NSE_EQ%7CFIXTURE/minutes/1/2026-10-03/2026-10-03".into();
                capture["requested_at"] = "2026-10-05T03:45:00Z".into();
                capture["received_at"] = "2026-10-05T03:46:00Z".into();
                body["data"]["candles"] =
                    serde_json::json!([["2026-10-03T09:15:00+05:30", 100, 101, 99, 100, 10, 0]]);
            }
            "odd-session" => special = serde_json::json!(["09:15:00", "09:16:30"]),
            "conflict" => {
                body["data"]["candles"]
                    .as_array_mut()
                    .unwrap()
                    .push(serde_json::json!([
                        "2026-10-01T09:15:00+05:30",
                        100,
                        101,
                        99,
                        101,
                        10,
                        0
                    ]));
            }
            "off-grid" => body["data"]["candles"][0][0] = "2026-10-01T09:15:30+05:30".into(),
            "over-limit" => {
                body["data"]["candles"] =
                    serde_json::Value::Array(vec![serde_json::json!([]); 50_001]);
            }
            _ => panic!("unknown Upstox history vector"),
        }
        capture["body"] = body.to_string().into();
        let calendar: Calendar = serde_json::from_value(serde_json::json!({
            "source": "synthetic-contract-test-calendar",
            "valid_from": "2026-09-01",
            "valid_to": "2026-10-06",
            "holidays": ["2026-10-02"],
            "special": {"2026-10-01": special},
            "synthetic": false
        }))
        .unwrap();
        let parsed = serde_json::to_vec(&capture)
            .map_err(|_| ())
            .and_then(|raw| MarketCapture::parse(&raw).map_err(|_| ()));
        let result = parsed.and_then(|capture| {
            normalize_upstox_historical_capture(&capture, "NSE_EQ|FIXTURE", &calendar)
                .map_err(|_| ())
        });
        assert_eq!(result.is_ok(), case.accepted, "{}", case.id);
        if let Ok(bars) = result {
            assert_eq!(bars.len(), case.bars.unwrap(), "{}", case.id);
            if case.mutation == "none" || case.mutation == "skip-closed" {
                assert_eq!(
                    bars[0].timestamp.to_string(),
                    "2026-10-01T03:45:00Z",
                    "{}",
                    case.id
                );
                assert_eq!(
                    bars[1].timestamp.to_string(),
                    "2026-10-01T03:46:00Z",
                    "{}",
                    case.id
                );
                assert_eq!(bars[0].volume, 10, "{}", case.id);
                assert_eq!(bars[1].volume, 12, "{}", case.id);
                let available = if case.mutation == "skip-closed" {
                    "2026-10-05T03:46:00Z"
                } else {
                    "2026-10-02T03:46:00Z"
                };
                assert!(
                    bars.iter()
                        .all(|bar| bar.available_at.to_string() == available),
                    "{}",
                    case.id
                );
            }
        }
    }
}
