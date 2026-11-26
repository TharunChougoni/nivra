use super::*;
use std::collections::BTreeMap;

type RecordedCall = (String, Vec<(String, String)>, u64);

struct Fake {
    responses: BTreeMap<String, HttpResponse>,
    calls: Vec<RecordedCall>,
    delay_ms: u64,
}

impl Fake {
    fn new(responses: BTreeMap<String, HttpResponse>) -> Self {
        Self {
            responses,
            calls: Vec::new(),
            delay_ms: 0,
        }
    }
}

impl Transport for Fake {
    fn get(
        &mut self,
        url: &str,
        headers: &[(&str, String)],
        timeout_ms: u64,
        max_bytes: u64,
    ) -> std::result::Result<HttpResponse, String> {
        self.calls.push((
            url.to_owned(),
            headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
            timeout_ms,
        ));
        if self.delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.delay_ms));
        }
        let response = if let Some(response) = self.responses.get(url) {
            response.clone()
        } else if url.starts_with("https://search.example.test/search") {
            response(200, "application/json", r#"{"results":[]}"#)
        } else {
            return Err("web_tool_http_failed".into());
        };
        if response.bytes.len() as u64 > max_bytes {
            return Err("web_tool_response_too_large".into());
        }
        Ok(response)
    }
}

fn response(status: u16, content_type: &str, body: &str) -> HttpResponse {
    HttpResponse {
        status,
        content_type: Some(content_type.into()),
        bytes: body.as_bytes().to_vec(),
    }
}

fn enabled_search() -> WebToolsSettings {
    WebToolsSettings {
        enabled: true,
        provider: WebProvider::Searxng,
        search_url: Some("https://search.example.test/search?format=json".into()),
        min_interval_ms: 0,
        ..WebToolsSettings::default()
    }
}

fn context() -> CallContext {
    CallContext {
        session: "2026-11-14".into(),
        run: "harness-supervisor-fixture".into(),
        turn: 0,
        call: "call_fixture".into(),
    }
}

#[test]
fn web_tools_are_config_gated_bounded_and_never_leak_credentials() {
    // Default policy: disabled, no provider URL compiled in, no tools exposed.
    let default = WebToolsSettings::default();
    assert!(!default.enabled);
    assert_eq!(default.provider, WebProvider::None);
    assert!(default.search_url.is_none());
    let fetch_only = tool_definitions(false, false);
    assert_eq!(fetch_only.len(), 1);
    assert_eq!(fetch_only[0]["name"], "web_fetch");
    assert!(default.validate().is_ok());
    assert!(
        serde_json::from_value::<WebToolsSettings>(json!({
            "enabled": true,
            "upstox_token": "must-not-be-accepted"
        }))
        .is_err(),
        "provider credential keys are not configuration fields"
    );

    // Enabled policy requires an explicit provider/endpoint; paid providers
    // require an application-owned key file. No endpoint is hardcoded.
    let invalid = WebToolsSettings {
        enabled: true,
        ..WebToolsSettings::default()
    };
    assert_eq!(
        invalid.validate().unwrap_err().to_string(),
        "web_tools_endpoint_required"
    );
    let no_provider = WebToolsSettings {
        enabled: true,
        search_url: Some("https://search.example.test/search".into()),
        ..WebToolsSettings::default()
    };
    assert_eq!(
        no_provider.validate().unwrap_err().to_string(),
        "web_tools_provider_required"
    );
    let brave = WebToolsSettings {
        enabled: true,
        provider: WebProvider::Brave,
        search_url: Some("https://api.example.test/res/v1/web/search".into()),
        ..WebToolsSettings::default()
    };
    assert_eq!(
        brave.validate().unwrap_err().to_string(),
        "web_tools_key_required"
    );
    for url in [
        "http://search.example.test/search",
        "https://user:pass@search.example.test/search",
        "https://127.0.0.1/search",
        "https://10.1.2.3/search",
        "https://localhost/search",
        "https://search.example.test:8443/search",
        "https://search.example.test/search?token=fixture-secret",
        "https://[::1]/search",
        "https://[::ffff:127.0.0.1]/search",
    ] {
        assert!(
            validate_public_https(url).is_err(),
            "unsafe URL admitted: {url}"
        );
    }
    assert!(validate_public_https("https://search.example.test/search").is_ok());
    assert!(parse_tool("web_fetch", json!({"url":"https://example.test/a"})).is_ok());
    assert!(parse_tool("place_order", json!({"url":"https://example.test/a"})).is_err());
    assert!(parse_tool("web_search", json!({"query":"token=fixture-secret"})).is_err());
    assert!(parse_tool("web_search", json!({"query":""})).is_err());

    // Tool schemas appear only for configured capabilities and stay strict.
    let settings = enabled_search();
    settings.validate().unwrap();
    let definitions = tool_definitions(true, false);
    let names: Vec<&str> = definitions
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert_eq!(names, vec!["web_search", "web_fetch"]);
    let mut with_news = settings.clone();
    with_news.news_feeds = vec!["https://feeds.example.test/announcements.xml".into()];
    with_news.validate().unwrap();
    assert_eq!(tool_definitions(true, true).len(), 3);
    for tool in tool_definitions(true, true) {
        let properties = tool["parameters"]["properties"].as_object().unwrap();
        let required = tool["parameters"]["required"].as_array().unwrap();
        assert_eq!(properties.len(), required.len());
        assert!(
            tool["name"]
                .as_str()
                .is_some_and(|name| name.starts_with("web_") || name == "news_lookup")
        );
    }

    // Fake transport: no socket is opened. Search parses provider rows.
    let search_url = "https://search.example.test/search?format=json&q=nifty%20bank&count=8";
    let mut fake = Fake::new(BTreeMap::from([(
        search_url.to_owned(),
        response(
            200,
            "application/json",
            r#"{"results":[{"title":"Bank note","url":"https://news.example.test/a","content":"<b>Nifty</b> &amp; bank context"},{"title":"","url":"https://ignored.example.test"}]}"#,
        ),
    )]));
    let dir = tempfile::tempdir().unwrap();
    let model = dir.path().join("model");
    fs::create_dir_all(&model).unwrap();
    let mut budget = WebToolBudget::default();
    let value = execute_with(
        &mut fake,
        &settings,
        &mut budget,
        &context(),
        WebToolOp::Search {
            query: "nifty bank".into(),
        },
        1_000,
        Some(&model),
    )
    .unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["provider"], "searxng");
    assert_eq!(value["count"], 1);
    assert_eq!(value["results"][0]["snippet"], "Nifty & bank context");
    assert_eq!(value["execution_authorized"], false);
    assert_eq!(fake.calls[0].0, search_url);
    assert_eq!(fake.calls[0].2, settings.timeout_ms);
    assert!(
        fake.calls[0]
            .1
            .iter()
            .all(|(name, _)| name != "X-Subscription-Token")
    );

    // Fetch strips HTML, bounds text and refuses binary content types.
    let fetch_url = "https://docs.example.test/announcement";
    let mut fake = Fake::new(BTreeMap::from([
        (
            fetch_url.to_owned(),
            response(
                200,
                "text/html; charset=utf-8",
                "<HTML><STYLE>x</STYLE><SCRIPT>alert(1)</SCRIPT><body>Board &amp; meeting — ₹ notice</body></HTML>",
            ),
        ),
        (
            "https://docs.example.test/file.bin".to_owned(),
            response(200, "application/octet-stream", "binary"),
        ),
    ]));
    let value = execute_with(
        &mut fake,
        &settings,
        &mut budget,
        &context(),
        WebToolOp::Fetch {
            url: fetch_url.into(),
        },
        2_000,
        Some(&model),
    )
    .unwrap();
    assert_eq!(value["text"], "Board & meeting — ₹ notice");
    assert_eq!(value["truncated"], false);
    assert_eq!(value["execution_authorized"], false);
    assert!(
        execute_with(
            &mut fake,
            &settings,
            &mut budget,
            &context(),
            WebToolOp::Fetch {
                url: "https://docs.example.test/file.bin".into()
            },
            3_000,
            Some(&model),
        )
        .is_err()
    );

    // News lookup reads configured free feeds and filters by query.
    let feed = "https://feeds.example.test/announcements.xml";
    let mut news = settings.clone();
    news.news_feeds = vec![feed.into()];
    let mut fake = Fake::new(BTreeMap::from([(
        feed.to_owned(),
        response(
            200,
            "application/rss+xml",
            r#"<rss><channel><item><title>Board meeting</title><link>https://nse.example.test/a</link><pubDate>Sat, 14 Nov 2026 10:00:00 GMT</pubDate><description>Quarterly results</description></item><item><title>Dividend</title><link>https://nse.example.test/b</link></item></channel></rss>"#,
        ),
    )]));
    let value = execute_with(
        &mut fake,
        &news,
        &mut budget,
        &context(),
        WebToolOp::News {
            query: Some("board".into()),
        },
        4_000,
        Some(&model),
    )
    .unwrap();
    assert_eq!(value["count"], 1);
    assert_eq!(value["items"][0]["title"], "Board meeting");
    assert_eq!(value["items"][0]["source"], "feeds.example.test");

    // Paid provider key is read from the private file, sent as a header and
    // never appears in the result or the session record.
    let key_path = dir.path().join("search.key");
    fs::write(&key_path, "fixture-provider-key\n").unwrap();
    let mut paid = enabled_search();
    paid.provider = WebProvider::Brave;
    paid.api_key_file = Some(key_path);
    paid.search_url = Some("https://api.example.test/res/v1/web/search".into());
    paid.validate().unwrap();
    let paid_url = "https://api.example.test/res/v1/web/search?q=nifty&count=8";
    let mut fake = Fake::new(BTreeMap::from([(
        paid_url.to_owned(),
        response(
            200,
            "application/json",
            r#"{"web":{"results":[{"title":"Fixture","url":"https://news.example.test/b","description":"context"}]}}"#,
        ),
    )]));
    let value = execute_with(
        &mut fake,
        &paid,
        &mut budget,
        &context(),
        WebToolOp::Search {
            query: "nifty".into(),
        },
        5_000,
        Some(&model),
    )
    .unwrap();
    assert_eq!(value["provider"], "brave");
    assert!(
        fake.calls[0]
            .1
            .iter()
            .any(|(name, value)| name == "X-Subscription-Token" && value == "fixture-provider-key")
    );
    assert!(!value.to_string().contains("fixture-provider-key"));

    // Per-run budget and minimum spacing are enforced before any call.
    let mut budget = WebToolBudget::default();
    let mut limited = enabled_search();
    limited.max_calls_per_run = 1;
    limited.min_interval_ms = 1_000;
    let mut fake = Fake::new(BTreeMap::from([(
        search_url.to_owned(),
        response(200, "application/json", r#"{"results":[]}"#),
    )]));
    assert!(
        execute_with(
            &mut fake,
            &limited,
            &mut budget,
            &context(),
            WebToolOp::Search {
                query: "one".into()
            },
            10_000,
            None
        )
        .is_ok()
    );
    assert_eq!(
        execute_with(
            &mut fake,
            &limited,
            &mut budget,
            &context(),
            WebToolOp::Search {
                query: "two".into()
            },
            10_100,
            None
        )
        .unwrap_err()
        .to_string(),
        "web_tool_budget_exhausted"
    );
    let mut budget = WebToolBudget::default();
    let mut paced = enabled_search();
    paced.max_calls_per_run = 4;
    paced.min_interval_ms = 1_000;
    execute_with(
        &mut fake,
        &paced,
        &mut budget,
        &context(),
        WebToolOp::Search {
            query: "one".into(),
        },
        20_000,
        None,
    )
    .unwrap();
    assert_eq!(
        execute_with(
            &mut fake,
            &paced,
            &mut budget,
            &context(),
            WebToolOp::Search {
                query: "two".into()
            },
            20_500,
            None
        )
        .unwrap_err()
        .to_string(),
        "web_tool_rate_limited"
    );
    assert!(
        execute_with(
            &mut fake,
            &paced,
            &mut budget,
            &context(),
            WebToolOp::Search {
                query: "three".into()
            },
            21_000,
            None
        )
        .is_ok()
    );

    // A slow provider response is reported as a timeout inside the configured
    // deadline; no partial result becomes evidence.
    let mut slow = Fake::new(BTreeMap::from([(
        search_url.to_owned(),
        response(200, "application/json", r#"{"results":[]}"#),
    )]));
    slow.delay_ms = 20;
    let mut tight = enabled_search();
    tight.timeout_ms = 1;
    assert_eq!(
        execute_with(
            &mut slow,
            &tight,
            &mut WebToolBudget::default(),
            &context(),
            WebToolOp::Search {
                query: "slow".into()
            },
            30_000,
            None
        )
        .unwrap_err()
        .to_string(),
        "web_tool_timeout"
    );

    // Each call appends one redacted row to the session record.
    let record = fs::read_to_string(model.join("web-tools.ndjson")).unwrap();
    assert!(!record.contains("fixture-provider-key"));
    assert!(record.contains("\"query\":\"nifty\""));
    let rows: Vec<Value> = record
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["tool"], "web_search");
    assert_eq!(rows[0]["session"], "2026-11-14");
    assert_eq!(rows[0]["status"], "ok");
    assert_eq!(rows[0]["execution_authorized"], false);
    assert!(rows[0]["latency_ms"].as_u64().is_some());
    assert!(rows[0]["bytes"].as_u64().is_some());

    // A disabled policy refuses every call and exposes no tool definitions.
    let mut disabled = settings.clone();
    disabled.enabled = false;
    assert!(!disabled.search_available());
    assert!(!disabled.news_available());
    assert_eq!(
        execute_with(
            &mut fake,
            &disabled,
            &mut WebToolBudget::default(),
            &context(),
            WebToolOp::News { query: None },
            40_000,
            None
        )
        .unwrap_err()
        .to_string(),
        "web_tools_disabled"
    );
}
