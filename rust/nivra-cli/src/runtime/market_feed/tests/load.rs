//! Local synthetic probe for the feed reader and observation preparation.
use super::*;
use crate::runtime::{
    controller_events::{Event, Inbox},
    strategy::{
        Observation,
        selection::{Screen, Selection, executable},
    },
};
use nivra_core::execution::Portfolio;
use std::time::Instant;

const RATE: usize = 540;
const DURATION: Duration = Duration::from_secs(6);
const NAMES: usize = 2_000;
const FULL: usize = 20;

fn cpu_ns() -> u64 {
    std::fs::read_to_string("/proc/thread-self/schedstat")
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
#[ignore = "synthetic timing probe; set NIVRA_FEED_LOAD=1 and run in release mode"]
fn synthetic_feed_load() {
    if std::env::var_os("NIVRA_FEED_LOAD").is_none() {
        return;
    }
    for _ in 0..5 {
        sample(false);
        sample(true);
    }
}

fn sample(loaded: bool) {
    let instruments = (0..NAMES)
        .map(|n| {
            serde_json::from_value::<Instrument>(json!({
                "key":format!("NSE_EQ|LOAD{n:04}"),"symbol":format!("LOAD{n:04}"),
                "series":"EQ","tick":"0.05",
            }))
            .unwrap()
        })
        .collect::<Vec<_>>();
    let keys: BTreeSet<_> = instruments.iter().map(|i| i.key.clone()).collect();
    let selected: BTreeSet<_> = keys.iter().take(FULL).cloned().collect();
    let index = instruments
        .iter()
        .map(|i| (i.key.clone(), i.clone()))
        .collect::<BTreeMap<_, _>>();
    let settings = Settings {
        mode: Mode::Ltpc,
        depth_mode: "full".into(),
        ..Settings::default()
    };
    let inbox = Inbox::new();
    let now = millis();
    let point = Point {
        price: Price::parse("100").unwrap(),
        source: now - 10,
        receipt: now - 10,
        last_trade: now - 10,
        prior_close: None,
        depth: None,
    };
    let cache = Arc::new(Mutex::new(Cache {
        points: keys
            .iter()
            .map(|key| {
                let price = if selected.contains(key) {
                    Price::parse("90").unwrap()
                } else {
                    point.price
                };
                (
                    key.clone(),
                    Point {
                        price,
                        ..point.clone()
                    },
                )
            })
            .collect(),
        screening_keys: keys.clone(),
        strategy_keys: selected.clone(),
        controller_notify: Some(inbox.notify()),
        connected: true,
        connected_since: Some(now),
        last_activity: Some(now),
        last_tick: Some(now),
        ..Cache::default()
    }));
    let mut controller = Controller {
        keys: keys.iter().cloned().collect(),
        allowed: keys.clone(),
        mixed: Some(Mixed {
            core: keys.clone(),
            requested: Default::default(),
            current: Plan::mixed(&keys, selected),
        }),
        revision: None,
        token_checked: now,
        retry_ms: 1_000,
        settings: settings.clone(),
        cache: cache.clone(),
    };
    let worker = Worker {
        cache,
        promoted: Default::default(),
        universe: keys,
        cancelled: Default::default(),
        handle: None,
        settings: settings.clone(),
    };
    let bytes = full_fixture();
    let full = Response::decode(bytes.as_slice())
        .unwrap()
        .feeds
        .into_values()
        .next()
        .unwrap();
    let ltpc = Response::decode(frame(now, 102.0).as_slice())
        .unwrap()
        .feeds
        .into_values()
        .next()
        .unwrap();
    let start = Instant::now();
    let end = start + DURATION;
    let reader = thread::spawn(move || {
        let mut transport = Mock {
            revision: "fixture".into(),
            polls: VecDeque::new(),
            subscriptions: Vec::new(),
            changes: Vec::new(),
            active: true,
        };
        let reader_cpu = cpu_ns();
        let mut poll_returns = 0;
        if loaded {
            for n in 0..RATE * DURATION.as_secs() as usize {
                let deadline = start + Duration::from_secs_f64(n as f64 / RATE as f64);
                thread::sleep(deadline.saturating_duration_since(Instant::now()));
                let receipt = millis();
                let full_message = n.is_multiple_of(NAMES / FULL);
                let index = if full_message {
                    (n / (NAMES / FULL)) % FULL
                } else {
                    FULL + n % (NAMES - FULL)
                };
                let key = &controller.keys[index];
                let payload = Response {
                    kind: 1,
                    current_ts: receipt - 1,
                    feeds: BTreeMap::from([(
                        key.clone(),
                        if full_message {
                            full.clone()
                        } else {
                            ltpc.clone()
                        },
                    )]),
                };
                controller.ingest(&mut transport, &payload.encode_to_vec(), receipt);
                poll_returns += 1;
            }
        } else {
            while Instant::now() < end {
                thread::sleep(Duration::from_millis(
                    controller.settings.read_poll_timeout_ms
                        + controller.settings.worker_idle_sleep_ms,
                ));
                poll_returns += 1;
            }
        }
        let cache = controller.cache.lock().unwrap();
        (
            cpu_ns() - reader_cpu,
            cache.ticks_total,
            cache.invalid_ltpc,
            poll_returns,
        )
    });

    let mut scan = Scanner::default();
    let mut selection = Selection::default();
    let screen =
        Screen::parse(&json!({"promotion":{"order":"rising","min_move_bps":300}})).unwrap();
    let params = json!({});
    let costs =
        serde_json::from_str(include_str!("../../../../../../fixtures/paper-costs.json")).unwrap();
    let p = Portfolio::new();
    let mut refresh_at = start;
    let mut notifications = 0;
    let mut refreshes = 0;
    let mut wait_returns = 0;
    let mut observations = 0;
    let mut full_updates = 0;
    let controller_cpu = cpu_ns();
    while Instant::now() < end {
        if Instant::now() >= refresh_at {
            let (points, _, _, at, activity) = worker.snapshot().unwrap();
            observe_cache(
                &mut scan,
                &instruments,
                &points,
                &settings,
                at,
                activity,
                None,
            )
            .unwrap();
            selection.refresh(
                &scan,
                at,
                screen.as_ref(),
                &BTreeSet::new(),
                instruments.iter().take(FULL).map(|i| i.key.as_str()),
                FULL,
            );
            refresh_at += Duration::from_secs(1);
            refreshes += 1;
        }
        let events = inbox.wait(
            refresh_at
                .min(end)
                .saturating_duration_since(Instant::now()),
        );
        wait_returns += 1;
        if !events.contains(Event::Feed) {
            continue;
        }
        notifications += 1;
        let updates = worker.drain_depth().unwrap();
        let now = millis();
        full_updates += updates.prospects.len();
        for (key, point) in updates.prospects {
            observe_point(
                &mut scan,
                &index[&key],
                &point,
                &settings,
                now,
                Some(point.receipt),
            )
            .unwrap();
        }
        let prospects = selection.prospects(&scan, now, FULL, |key| {
            executable(&scan, key, now).is_some()
        });
        let quotes = prospects
            .iter()
            .filter_map(|p| executable(&scan, &p.key, now).map(|q| (p.key.clone(), q)))
            .collect::<BTreeMap<_, _>>();
        if !quotes.is_empty() {
            let obs = Observation::new(now, &params, &prospects, &quotes, &p, &index, &costs);
            std::hint::black_box(serde_json::to_vec(&obs).unwrap());
            observations += 1;
        }
    }
    let controller_cpu = cpu_ns() - controller_cpu;
    let (reader_cpu, messages, invalid, poll_returns) = reader.join().unwrap();
    let seconds = start.elapsed().as_secs_f64();
    println!(
        "feed_load loaded={loaded} seconds={seconds:.3} messages={messages} invalid={invalid} reader_cpu_ms={:.3} controller_cpu_ms={:.3} depth_notifications={notifications} poll_returns={poll_returns} wait_returns={wait_returns} refreshes={refreshes} full_updates={full_updates} observations={observations}",
        reader_cpu as f64 / 1e6,
        controller_cpu as f64 / 1e6
    );
    assert_eq!(invalid, 0);
    if loaded {
        assert_eq!(messages, (RATE * DURATION.as_secs() as usize) as u64);
        assert_eq!(observations, notifications);
        assert!(notifications > 0 && notifications < messages);
    } else {
        assert_eq!((messages, notifications, observations), (0, 0, 0));
    }
}
