use super::*;
use nivra_core::finance::Price;
use prost::Message;
use std::collections::VecDeque;
use wire::{Feed, FeedKind, Ltpc, Response};
struct Mock {
    revision: String,
    polls: VecDeque<std::result::Result<Poll, &'static str>>,
    subscriptions: Vec<Vec<String>>,
    changes: Vec<(String, String, Vec<String>)>,
    active: bool,
}
impl Transport for Mock {
    fn revision(&self) -> std::result::Result<String, &'static str> {
        Ok(self.revision.clone())
    }
    fn connect(&mut self, keys: &[String]) -> std::result::Result<(), &'static str> {
        assert!(
            !self.active,
            "never open a second connection before closing the first"
        );
        self.active = true;
        self.subscriptions.push(keys.to_vec());
        Ok(())
    }
    fn subscription(
        &mut self,
        method: &str,
        mode: &str,
        keys: &[String],
    ) -> std::result::Result<(), &'static str> {
        assert!(self.active);
        self.changes
            .push((method.into(), mode.into(), keys.to_vec()));
        Ok(())
    }
    fn poll(&mut self) -> std::result::Result<Poll, &'static str> {
        self.polls.pop_front().unwrap_or(Ok(Poll::Idle))
    }
    fn close(&mut self) {
        self.active = false;
    }
}
fn frame(source: i64, price: f64) -> Vec<u8> {
    Response {
        kind: 1,
        current_ts: source,
        feeds: BTreeMap::from([(
            "NSE_EQ|A".into(),
            Feed {
                kind: Some(FeedKind::Ltpc(Ltpc {
                    ltp: price,
                    ltt: source,
                    ltq: 2,
                    cp: 100.0,
                })),
            },
        )]),
    }
    .encode_to_vec()
}
#[test]
fn provider_depth_frame_decodes_touch_and_features() {
    let allowed = BTreeSet::from(["NSE_EQ|A".into()]);
    for (index, hex) in [
            "080112610a084e53455f45517c41125512530a510a1709000000000000594010e307180521000000000080584012180a16080b11cdccccccccfc58401807213333333333035940290000000000e05840309875490000000000c0724051000000000000594018e807",
            "080112420a084e53455f45517c4112361a340a1709000000000000594010e30718052100000000008058401216080b11cdccccccccfc5840180721333333333303594020987518e807",
        ].into_iter().enumerate() {
            let bytes: Vec<_> = (0..hex.len())
                .step_by(2)
                .map(|n| u8::from_str_radix(&hex[n..n + 2], 16).unwrap())
                .collect();
            let points = decode(&bytes, 1001, 0, &allowed).unwrap().points;
            assert_eq!(
                points.get("NSE_EQ|A").map(|point| point.price),
                Some(Price::parse("100").unwrap()),
            );
            let point = &points["NSE_EQ|A"];
            assert_eq!(point.last_trade, 995);
            let data = point.depth.as_ref().unwrap();
            assert_eq!((data.clock.source, data.clock.receipt), (1000, 1001));
            assert_eq!(data.volume, Some(15_000));
            assert_eq!(data.vwap, (index == 0).then_some(99.5));
            assert_eq!(data.imbalance, (index == 0).then_some(0.75));
            let quote = data.touch.as_ref().unwrap();
            assert_eq!((quote.bid_units, quote.ask_units), (11, 7));
            assert_eq!(quote.bid, Price::parse("99.95").unwrap());
            assert_eq!(quote.ask, Price::parse("100.05").unwrap());
        }
}
#[test]
fn mixed_mode_replays_the_current_promotions_after_reconnect() {
    let keys = vec!["NSE_EQ|A".to_owned(), "NSE_EQ|B".to_owned()];
    let requested = Arc::new(Mutex::new(BTreeSet::from([keys[0].clone()])));
    let settings = Settings {
        mode: Mode::Ltpc,
        depth_mode: "full".into(),
        reconnect_initial_ms: 100,
        ..Settings::default()
    };
    settings.validate().unwrap();
    let mut controller = Controller {
        mixed: Some(Mixed {
            core: keys.iter().cloned().collect(),
            requested: requested.clone(),
            current: Plan::default(),
        }),
        keys: keys.clone(),
        allowed: BTreeSet::new(),
        retry_ms: settings.reconnect_initial_ms,
        settings,
        revision: None,
        token_checked: 0,
        cache: Arc::new(Mutex::new(Cache::default())),
    };
    let mut transport = Mock {
        revision: "fixture".into(),
        polls: VecDeque::new(),
        subscriptions: vec![],
        changes: vec![],
        active: false,
    };
    controller.step(&mut transport, 1000);
    let command = |method: &str, mode: &str, key: &str| {
        (method.to_owned(), mode.to_owned(), vec![key.to_owned()])
    };
    assert_eq!(
        transport.changes,
        [
            command("sub", "ltpc", &keys[1]),
            command("sub", "full", &keys[0]),
        ]
    );

    *requested.lock().unwrap() = BTreeSet::from([keys[1].clone()]);
    transport.changes.clear();
    transport.polls.push_back(Err("feed_closed"));
    controller.step(&mut transport, 1100);
    assert!(!transport.active);
    assert_eq!(controller.cache.lock().unwrap().full_keys, 0);
    assert_eq!(
        transport.changes,
        [
            command("unsub", "ltpc", &keys[1]),
            command("unsub", "full", &keys[0]),
            command("sub", "ltpc", &keys[0]),
            command("sub", "full", &keys[1]),
        ]
    );

    transport.changes.clear();
    controller.step(&mut transport, 1200);
    assert_eq!(transport.subscriptions.len(), 2);
    assert_eq!(
        transport.changes,
        [
            command("sub", "ltpc", &keys[0]),
            command("sub", "full", &keys[1]),
        ]
    );
}
#[test]
fn mock_v3_feed_decoding_reconnect_rotation_staleness_and_fallback() {
    let first = ConnectionPermit::acquire(2).unwrap();
    let second = ConnectionPermit::acquire(2).unwrap();
    assert!(ConnectionPermit::acquire(2).is_err());
    drop((first, second));
    let keys = vec!["NSE_EQ|A".to_owned()];
    let allowed = keys.iter().cloned().collect();
    // Independent provider-wire fixture (not a round-trip of our decoder).
    let hex =
        "080112250a084e53455f45517c4112190a1709000000000000594010e807180521000000000000594018e807";
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|n| u8::from_str_radix(&hex[n..n + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let points = decode(&bytes, 1001, 0, &allowed).unwrap().points;
    assert_eq!(points[&keys[0]].price, Price::parse("100").unwrap());
    assert_eq!(
        (points[&keys[0]].source, points[&keys[0]].receipt),
        (1000, 1001)
    );
    assert!(decode(&bytes, 999, 0, &allowed).is_err());
    assert!(decode(&[0xff], 1001, 0, &allowed).is_err());
    assert!(
        decode(&bytes, 1001, 0, &BTreeSet::new())
            .unwrap()
            .points
            .is_empty()
    );
    // Configured NSE_INDEX keys join the same subscription list; disabled
    // feeds add nothing and a duplicate key is refused, not folded.
    let index_key = "NSE_INDEX|Nifty 50".to_owned();
    let universe = vec![Instrument {
        key: keys[0].clone(),
        symbol: "A".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }];
    let mut c = super::super::tests::config(Path::new("fixture"));
    assert_eq!(subscription_keys(&c, &universe).unwrap(), keys);
    c.index_feeds.enabled = true;
    c.quote_keys.prefixes.push("NSE_INDEX|".into());
    c.validate().unwrap();
    let subscribed = subscription_keys(&c, &universe).unwrap();
    assert_eq!(subscribed.len(), keys.len() + 3);
    assert_eq!(&subscribed[keys.len()..], c.index_feeds.keys.as_slice());
    assert!(subscription_keys(&c, &[]).is_err());
    let mut duplicate = c.clone();
    duplicate.index_feeds.keys = vec![keys[0].clone()];
    assert!(subscription_keys(&duplicate, &universe).is_err());
    // Index prices decode only for the configured allowlist; the LTPC point
    // keeps its prior close for read-only context.
    let index_frame = Response {
        kind: 1,
        current_ts: 1000,
        feeds: BTreeMap::from([(
            index_key.clone(),
            Feed {
                kind: Some(FeedKind::Ltpc(Ltpc {
                    ltp: 24_500.0,
                    ltt: 1000,
                    ltq: 0,
                    cp: 24_400.0,
                })),
            },
        )]),
    }
    .encode_to_vec();
    let index_allowed = BTreeSet::from([index_key.clone()]);
    let index_points = decode(&index_frame, 1001, 0, &index_allowed)
        .unwrap()
        .points;
    assert_eq!(
        index_points[&index_key].price,
        Price::parse("24500").unwrap()
    );
    assert_eq!(
        index_points[&index_key].prior_close,
        Some(Price::parse("24400").unwrap())
    );
    assert!(
        decode(&index_frame, 1001, 0, &allowed)
            .unwrap()
            .points
            .is_empty(),
        "cash subscription must not accept index keys"
    );
    let settings = Settings {
        tick_ms: 100,
        stale_age_ms: 500,
        metadata_poll_ms: 100,
        metadata_stale_ms: 500,
        reconnect_initial_ms: 100,
        reconnect_max_ms: 400,
        token_check_ms: 100,
        heartbeat_timeout_ms: 1000,
        scan_activity_timeout_ms: 500,
        source_clock_skew_ms: 0,
        ..Settings::default()
    };
    settings.validate().unwrap();
    assert_eq!(settings.rest_interval_ms(true), settings.fallback_poll_ms);
    assert_eq!(settings.rest_interval_ms(false), settings.metadata_poll_ms);
    let cache = Arc::new(Mutex::new(Cache::default()));
    let mut controller = Controller {
        keys: keys.clone(),
        allowed,
        mixed: None,
        revision: None,
        token_checked: 0,
        retry_ms: 100,
        settings: settings.clone(),
        cache: cache.clone(),
    };
    let mut mock = Mock {
        revision: "day-one".into(),
        polls: VecDeque::from([Ok(Poll::Data(bytes, 1001))]),
        subscriptions: vec![],
        changes: Vec::new(),
        active: false,
    };
    controller.step(&mut mock, 1001);
    assert!(!cache.lock().unwrap().fallback(1001, &settings));
    assert_eq!(
        cache.lock().unwrap().health(1001, &settings)["ticks_total"],
        1
    );
    controller.ingest(&mut mock, &frame(1100, 101.0), 1100);
    // Invalid and delayed points must not inflate accepted tick telemetry.
    controller.ingest(&mut mock, &frame(1100, 0.0), 1100);
    controller.ingest(&mut mock, &frame(1000, 100.0), 1100);
    let health = cache.lock().unwrap().health(1100, &settings);
    assert_eq!(health["ticks_total"], 2);
    assert_eq!(health["tick_rate_per_s"], 2.0 / 60.0);
    assert_eq!(
        cache.lock().unwrap().health(61001, &settings)["tick_rate_per_s"],
        1.0 / 60.0
    );
    assert_eq!(
        cache.lock().unwrap().health(61100, &settings)["tick_rate_per_s"],
        0.0
    );
    // Index-only points never mask a missing cash screening price.
    let mut index_only = Cache {
        screening_keys: BTreeSet::from([keys[0].clone()]),
        connected: true,
        last_activity: Some(1001),
        ..Cache::default()
    };
    index_only.points.insert(
        index_key.clone(),
        Point {
            price: Price::parse("24500").unwrap(),
            source: 1000,
            receipt: 1001,
            last_trade: 1000,
            prior_close: None,
            depth: None,
        },
    );
    assert!(index_only.fallback(1001, &settings));
    index_only.points.insert(
        keys[0].clone(),
        Point {
            price: Price::parse("100").unwrap(),
            source: 1000,
            receipt: 1001,
            last_trade: 1000,
            prior_close: None,
            depth: None,
        },
    );
    assert!(!index_only.fallback(1001, &settings));

    mock.polls.push_back(Err("feed_closed"));
    controller.step(&mut mock, 1101);
    assert_eq!(
        cache.lock().unwrap().health(1101, &settings)["reconnects"],
        1
    );
    assert!(cache.lock().unwrap().fallback(1101, &settings));
    controller.step(&mut mock, 1200);
    assert_eq!(mock.subscriptions.len(), 1);
    controller.step(&mut mock, 1201);
    assert_eq!(mock.subscriptions.len(), 2);
    assert!(
        cache.lock().unwrap().points.is_empty(),
        "reconnect waits for a new snapshot"
    );
    mock.polls
        .push_back(Ok(Poll::Data(frame(1202, 101.0), 1203)));
    controller.step(&mut mock, 1203);
    assert!(!cache.lock().unwrap().fallback(1203, &settings));
    // Daily application-token renewal reauthorizes/resubscribes the same universe.
    mock.revision = "day-two".into();
    controller.step(&mut mock, 1304);
    assert_eq!(mock.subscriptions.len(), 3);
    assert!(mock.subscriptions.iter().all(|sub| sub == &keys));
    assert!(cache.lock().unwrap().fallback(1304, &settings));
    mock.polls
        .push_back(Ok(Poll::Data(frame(1400, 102.0), 1401)));
    controller.step(&mut mock, 1401);
    let instrument = Instrument {
        key: keys[0].clone(),
        symbol: "A".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let mut scan = Scanner {
        screens: Some(nivra_core::execution::ScreenPolicy::recommended()),
        ..Scanner::default()
    };
    let sample = scan.samples.entry(keys[0].clone()).or_default();
    sample.market_fields = Some(nivra_core::execution::MarketFields {
        session_vwap: None,
        best_bid: None,
        best_bid_qty: None,
        best_ask: None,
        best_ask_qty: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        lower_circuit_limit: nivra_core::execution::a("80"),
        upper_circuit_limit: nivra_core::execution::a("120"),
        volume: 20000,
    });
    sample.market_fields_receipt = Some(1000);
    let points = cache.lock().unwrap().points.clone();
    observe_cache(
        &mut scan,
        std::slice::from_ref(&instrument),
        &points,
        &settings,
        1402,
        Some(1401),
        None,
    )
    .unwrap();
    assert_eq!(scan.samples[&keys[0]].reason, "eligible_no_signal");
    assert_eq!(scan.samples[&keys[0]].market_fields_receipt, Some(1000));
    observe_cache(
        &mut scan,
        std::slice::from_ref(&instrument),
        &points,
        &settings,
        1902,
        Some(1401),
        None,
    )
    .unwrap();
    assert_eq!(scan.samples[&keys[0]].reason, "stale");
    assert_eq!(
        scan.samples[&keys[0]].seen, 1401,
        "clock samples never renew quote receipt"
    );
    mock.polls
        .push_back(Ok(Poll::Data(frame(1950, 103.0), 1951)));
    controller.step(&mut mock, 1951);
    observe_cache(
        &mut scan,
        std::slice::from_ref(&instrument),
        &cache.lock().unwrap().points,
        &settings,
        1952,
        Some(1951),
        None,
    )
    .unwrap();
    assert_eq!(
        scan.samples[&keys[0]].reason, "missing_market_fields",
        "LTPC cannot renew volume/band metadata"
    );
    assert!(cache.lock().unwrap().fallback(2452, &settings));
    assert_eq!(
        cache.lock().unwrap().health(2452, &settings)["reason"],
        "feed_stale_or_no_ltpc"
    );
    // Heartbeats keep the transport alive but do not refresh price timestamps.
    mock.polls.push_back(Ok(Poll::Heartbeat));
    controller.step(&mut mock, 2452);
    assert!(!cache.lock().unwrap().fallback(2452, &settings));
    controller.step(&mut mock, 3453);
    assert_eq!(
        cache.lock().unwrap().last_error,
        Some("feed_heartbeat_timeout")
    );
    let retry = cache.lock().unwrap().next_retry;
    mock.polls.push_back(Err("feed_read_failed"));
    controller.step(&mut mock, retry);
    assert_eq!(
        controller.retry_ms, 400,
        "bounded exponential reconnect backoff"
    );
    assert!(cache.lock().unwrap().fallback(retry, &settings));
    assert!(!mock.active);
    assert!(
        Settings {
            max_connections: 3,
            ..settings
        }
        .validate()
        .is_err()
    );
}

#[test]
fn transport_settings_preserve_defaults_and_reject_invalid_bounds() {
    let settings: Settings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(settings.max_connect_addresses, 2);
    assert_eq!(settings.connect_timeout_ms, 3000);
    assert_eq!(settings.handshake_io_timeout_ms, 3000);
    assert_eq!(settings.read_poll_timeout_ms, 250);
    assert_eq!(settings.worker_idle_sleep_ms, 25);
    settings.validate().unwrap();
    let defaults = serde_json::to_value(&settings).unwrap();
    for (field, maximum) in [
        ("max_connect_addresses", 64),
        ("connect_timeout_ms", 86_400_000),
        ("handshake_io_timeout_ms", 86_400_000),
        ("read_poll_timeout_ms", 86_400_000),
        ("worker_idle_sleep_ms", 86_400_000),
    ] {
        for value in [0, maximum + 1] {
            let mut config = defaults.clone();
            config[field] = json!(value);
            let invalid: Settings = serde_json::from_value(config).unwrap();
            assert!(invalid.validate().is_err(), "{field}={value}");
        }
        for value in [1, maximum] {
            let mut config = defaults.clone();
            config[field] = json!(value);
            let valid: Settings = serde_json::from_value(config).unwrap();
            assert!(valid.validate().is_ok(), "{field}={value}");
        }
    }
}

#[test]
fn fresh_drop_fraction_defaults_and_bounds() {
    let settings: Settings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(settings.fresh_drop_alert_fraction, 0.5);
    for fraction in [0.0, -0.1, 1.01, f64::NAN, f64::INFINITY] {
        assert!(
            Settings {
                fresh_drop_alert_fraction: fraction,
                ..settings.clone()
            }
            .validate()
            .is_err()
        );
    }
    for fraction in [0.01, 0.5, 1.0] {
        assert!(
            Settings {
                fresh_drop_alert_fraction: fraction,
                ..settings.clone()
            }
            .validate()
            .is_ok()
        );
    }
}

#[test]
fn quiet_prices_follow_connection_liveness_without_renewing_financial_receipts() {
    let settings: Settings = serde_json::from_value(json!({})).unwrap();
    assert_eq!(settings.scan_activity_timeout_ms, 30_000);
    assert_eq!(settings.telemetry_window_ms, 60_000);
    for window in [0, 99, 86_400_001] {
        assert!(
            Settings {
                telemetry_window_ms: window,
                ..settings.clone()
            }
            .validate()
            .is_err()
        );
    }
    for timeout in [0, 99, 86_400_001] {
        assert!(
            Settings {
                scan_activity_timeout_ms: timeout,
                ..settings.clone()
            }
            .validate()
            .is_err()
        );
    }
    let key = "NSE_EQ|A".to_owned();
    let instrument = Instrument {
        key: key.clone(),
        symbol: "A".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    let cache = Arc::new(Mutex::new(Cache::default()));
    let mut controller = Controller {
        keys: vec![key.clone()],
        allowed: BTreeSet::from([key.clone()]),
        mixed: None,
        revision: None,
        token_checked: 0,
        retry_ms: settings.reconnect_initial_ms,
        settings: settings.clone(),
        cache: cache.clone(),
    };
    let mut mock = Mock {
        revision: "fixture".into(),
        polls: VecDeque::from([Ok(Poll::Heartbeat)]),
        subscriptions: vec![],
        changes: Vec::new(),
        active: false,
    };
    controller.step(&mut mock, 1000);
    assert!(
        cache.lock().unwrap().fallback(1000, &settings),
        "heartbeat alone supplies no valid price"
    );
    mock.polls
        .push_back(Ok(Poll::Data(frame(1001, 100.0), 1001)));
    controller.step(&mut mock, 1001);
    mock.polls.push_back(Ok(Poll::Heartbeat));
    controller.step(&mut mock, 25_000);
    let mut scan = Scanner::default();
    let sample = scan.samples.entry(key.clone()).or_default();
    sample.market_fields = Some(nivra_core::execution::MarketFields {
        session_vwap: None,
        best_bid: None,
        best_bid_qty: None,
        best_ask: None,
        best_ask_qty: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        lower_circuit_limit: nivra_core::execution::a("80"),
        upper_circuit_limit: nivra_core::execution::a("120"),
        volume: 20_000,
    });
    sample.market_fields_receipt = Some(1000);
    let c = cache.lock().unwrap();
    assert!(!c.fallback(55_000, &settings));
    assert!(c.fallback(55_001, &settings));
    observe_cache(
        &mut scan,
        std::slice::from_ref(&instrument),
        &c.points,
        &settings,
        25_000,
        c.last_activity,
        None,
    )
    .unwrap();
    assert_eq!(scan.samples[&key].seen, 1001);
    assert_eq!(scan.samples[&key].source, Some(1001));
    assert!(scan.samples[&key].executable.is_none());
    let discovery: super::super::discovery::Settings = serde_json::from_value(json!({
        "max_scan_age_ms": 2000, "max_prospects": 10, "market_feed": { "mode": "ltpc" }
    }))
    .unwrap();
    for now in [25_000, 55_000] {
        let projection = super::super::discovery::project(
            &scan,
            std::slice::from_ref(&instrument),
            &discovery,
            now,
        );
        assert_eq!(projection["fresh_count"], 1);
        assert_eq!(projection["eligible_count"], 1);
        assert_eq!(
            nivra_core::strategy::extract_features(&scan, now, 2000)
                .rows
                .len(),
            1
        );
    }
    let projection = super::super::discovery::project(
        &scan,
        std::slice::from_ref(&instrument),
        &discovery,
        55_001,
    );
    assert_eq!(projection["fresh_count"], 0);
    drop(c);
    controller.step(&mut mock, 55_001);
    assert!(!cache.lock().unwrap().connected);
    let retry = cache.lock().unwrap().next_retry;
    mock.polls.push_back(Ok(Poll::Heartbeat));
    controller.step(&mut mock, retry);
    let c = cache.lock().unwrap();
    assert!(c.points.is_empty(), "new connection requires its own price");
    assert!(c.fallback(retry, &settings));
    observe_cache(
        &mut scan,
        &[instrument],
        &c.points,
        &settings,
        retry,
        c.last_activity,
        None,
    )
    .unwrap();
    assert_eq!(scan.samples[&key].reason, "no_quote_returned");
}

fn two(source: i64, good: Ltpc, bad: Ltpc) -> Vec<u8> {
    Response {
        kind: 1,
        current_ts: source,
        feeds: BTreeMap::from([
            (
                "NSE_EQ|A".into(),
                Feed {
                    kind: Some(FeedKind::Ltpc(good)),
                },
            ),
            (
                "NSE_EQ|B".into(),
                Feed {
                    kind: Some(FeedKind::Ltpc(bad)),
                },
            ),
        ]),
    }
    .encode_to_vec()
}
fn ltpc(ltp: f64, ltt: i64) -> Ltpc {
    Ltpc {
        ltp,
        ltt,
        ltq: 1,
        cp: 100.0,
    }
}
#[test]
fn one_invalid_instrument_or_small_clock_skew_never_drops_the_frame() {
    let allowed: BTreeSet<String> = ["NSE_EQ|A".into(), "NSE_EQ|B".into()].into();
    // Untraded at the open (ltp 0) or NaN: skip that key, keep the rest.
    for bad in [ltpc(0.0, 0), ltpc(f64::NAN, 900), ltpc(10.0, -1)] {
        let d = decode(&two(1000, ltpc(100.0, 990), bad), 1001, 0, &allowed).unwrap();
        assert_eq!((d.points.len(), d.invalid), (1, 1));
        assert!(d.points.contains_key("NSE_EQ|A"));
    }
    // Exchange/provider clocks a little ahead of ours are tolerated within config.
    let ahead = two(1500, ltpc(100.0, 1400), ltpc(101.0, 1500));
    assert!(decode(&ahead, 1001, 0, &allowed).is_err());
    let d = decode(&ahead, 1001, 500, &allowed).unwrap();
    assert_eq!((d.points.len(), d.invalid), (2, 0));
    assert_eq!(
        d.points["NSE_EQ|A"].source, 1001,
        "source never leads receipt"
    );
    assert!(decode(&ahead, 1001, 400, &allowed).is_err());
    assert!(
        Settings {
            source_clock_skew_ms: 5001,
            ..Settings::default()
        }
        .validate()
        .is_err()
    );
}
#[test]
fn invalid_opening_ticks_preserve_connection_but_cannot_hide_staleness() {
    let settings = Settings {
        stale_age_ms: 500,
        source_clock_skew_ms: 100,
        reconnect_initial_ms: 100,
        ..Settings::default()
    };
    settings.validate().unwrap();
    let keys = vec!["NSE_EQ|A".into(), "NSE_EQ|B".into()];
    let cache = Arc::new(Mutex::new(Cache::default()));
    let mut controller = Controller {
        allowed: keys.iter().cloned().collect(),
        keys: keys.clone(),
        mixed: None,
        revision: None,
        token_checked: 0,
        retry_ms: settings.reconnect_initial_ms,
        settings: settings.clone(),
        cache: cache.clone(),
    };
    let mut mock = Mock {
        revision: "fixture".into(),
        polls: VecDeque::new(),
        subscriptions: vec![],
        changes: Vec::new(),
        active: false,
    };
    // A valid key at the exact configured skew boundary survives an invalid peer.
    mock.polls.push_back(Ok(Poll::Data(
        two(1100, ltpc(100.0, 1100), ltpc(0.0, 0)),
        1000,
    )));
    controller.step(&mut mock, 1000);
    {
        let c = cache.lock().unwrap();
        assert!(!c.fallback(1000, &settings));
        assert_eq!(c.invalid_ltpc, 1);
        assert_eq!(
            (c.points[&keys[0]].source, c.points[&keys[0]].receipt),
            (1000, 1000)
        );
    }
    // All-invalid frames are transport activity, never new price evidence.
    mock.polls.push_back(Ok(Poll::Data(
        two(1500, ltpc(0.0, 0), ltpc(101.0, 1601)),
        1500,
    )));
    controller.step(&mut mock, 1500);
    {
        let c = cache.lock().unwrap();
        assert!(!c.fallback(1500, &settings));
        assert!(!c.fallback(1501, &settings));
        assert_eq!(c.invalid_ltpc, 3);
        assert_eq!(c.last_activity, Some(1500));
        assert_eq!(c.last_tick, Some(1000));
        assert_eq!(c.points[&keys[0]].receipt, 1000);
        assert_eq!(c.reconnects, 0);
    }
    assert!(mock.active);
    assert_eq!(mock.subscriptions.len(), 1);
    // A delayed frame keeps transport alive but cannot replace retained price evidence.
    mock.polls.push_back(Ok(Poll::Data(frame(999, 99.0), 1600)));
    controller.step(&mut mock, 1600);
    assert!(!cache.lock().unwrap().fallback(1600, &settings));
    assert_eq!(cache.lock().unwrap().points[&keys[0]].receipt, 1000);
    mock.polls
        .push_back(Ok(Poll::Data(frame(1700, 102.0), 1700)));
    controller.step(&mut mock, 1700);
    assert!(!cache.lock().unwrap().fallback(1700, &settings));
    assert_eq!(mock.subscriptions.len(), 1, "recovery needs no reconnect");
    // Frame-wide invalid source time remains a real failure, beyond tolerance.
    mock.polls
        .push_back(Ok(Poll::Data(frame(1901, 103.0), 1800)));
    controller.step(&mut mock, 1800);
    {
        let c = cache.lock().unwrap();
        assert_eq!(c.last_error, Some("feed_source_time_invalid"));
        assert!(c.fallback(1800, &settings));
        assert_eq!(c.next_retry, 1900);
        assert_eq!(c.points[&keys[0]].receipt, 1700);
    }
    assert!(!mock.active);
    controller.step(&mut mock, 1899);
    assert_eq!(mock.subscriptions.len(), 1);
    mock.polls
        .push_back(Ok(Poll::Data(frame(1900, 104.0), 1900)));
    controller.step(&mut mock, 1900);
    assert_eq!(mock.subscriptions, vec![keys.clone(), keys]);
    assert!(!cache.lock().unwrap().fallback(1900, &settings));
    assert_eq!(cache.lock().unwrap().last_error, None);
}

#[test]
fn ticks_newer_than_a_stale_loop_clock_are_not_reported_as_fallback() {
    let settings = Settings::default();
    let cache = Cache {
        connected: true,
        connected_since: Some(0),
        last_tick: Some(100_000),
        last_activity: Some(100_000),
        points: decode(
            &frame(100_000, 100.0),
            100_000,
            0,
            &BTreeSet::from(["NSE_EQ|A".into()]),
        )
        .unwrap()
        .points,
        ..Cache::default()
    };
    // A loop-start clock sampled before the tick sees a negative age (the #34 flap).
    assert!(cache.fallback(99_000, &settings));
    // The worker snapshot reads its own clock, which is never behind its receipts.
    let worker = Worker {
        cache: Arc::new(Mutex::new(Cache {
            last_tick: Some(millis()),
            last_activity: Some(millis()),
            ..cache
        })),
        cancelled: Arc::new(AtomicBool::new(false)),
        handle: None,
        promoted: Arc::new(Mutex::new(BTreeSet::new())),
        universe: BTreeSet::new(),
        settings,
    };
    let (_, health, fallback, now, _) = worker.snapshot().unwrap();
    assert!(!fallback);
    assert_eq!(health["state"], "streaming");
    assert!(now >= millis() - 1000);
}

#[test]
fn broad_prices_stay_in_the_cache_and_full_receipts_notify_the_book() {
    let inbox = super::super::controller_events::Inbox::new();
    let key = "NSE_EQ|A".to_string();
    let mut cache = Cache {
        controller_notify: Some(inbox.notify()),
        last_tick: Some(999),
        strategy_keys: BTreeSet::from([key.clone()]),
        ..Cache::default()
    };
    let mut controller = Controller {
        keys: vec![key.clone()],
        allowed: BTreeSet::from([key.clone()]),
        mixed: Some(Mixed {
            core: BTreeSet::from([key.clone()]),
            requested: Default::default(),
            current: Plan::mixed(&BTreeSet::new(), BTreeSet::from([key.clone()])),
        }),
        revision: None,
        token_checked: 0,
        retry_ms: 1000,
        settings: Settings::default(),
        cache: Arc::new(Mutex::new(std::mem::take(&mut cache))),
    };
    let mut mock = Mock {
        revision: "fixture".into(),
        polls: VecDeque::new(),
        subscriptions: Vec::new(),
        changes: Vec::new(),
        active: false,
    };
    controller.ingest(&mut mock, &frame(1_000, 100.0), 1_001);
    assert!(
        !inbox
            .wait(Duration::ZERO)
            .contains(super::super::controller_events::Event::Feed)
    );
    let hex = "080112610a084e53455f45517c41125512530a510a1709000000000000594010e307180521000000000080584012180a16080b11cdccccccccfc58401807213333333333035940290000000000e05840309875490000000000c0724051000000000000594018e807";
    let full: Vec<_> = (0..hex.len())
        .step_by(2)
        .map(|n| u8::from_str_radix(&hex[n..n + 2], 16).unwrap())
        .collect();
    controller.ingest(&mut mock, &full, 1_002);
    assert!(
        inbox
            .wait(Duration::ZERO)
            .contains(super::super::controller_events::Event::Feed)
    );
}
