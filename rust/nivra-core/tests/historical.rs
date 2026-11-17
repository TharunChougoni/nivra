use nivra_core::{
    execution::{self, Instrument, Portfolio, Quote, Scanner, a},
    finance::Price,
    historical::{self, Event, EventKind, Manifest, Observation, Runner},
};
use std::collections::BTreeMap;
fn at() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn instrument(key: &str) -> Instrument {
    Instrument {
        key: key.into(),
        symbol: key.into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }
}
fn manifest() -> Manifest {
    serde_json::from_value(serde_json::json!({"session":"2026-10-16","warmup_start":"2026-10-15","paper_costs":{"per_order":"0.01","buy_bps":"5","sell_bps":"7","slippage_bps":"5","rounding_increment":"0.01"},"policy":execution::POLICY,"split":"development","receipt_clock":"virtual_historical","seed":7,"bindings":{"engine":"a".repeat(64),"strategy":"b".repeat(64),"data":"c".repeat(64),"calendar":"d".repeat(64),"fees":"e".repeat(64),"slippage":"f".repeat(64)},"universe":[instrument("fixture-a"),instrument("fixture-b")]})).unwrap()
}
fn event(now: i64, price: i64, kind: EventKind) -> Event {
    Event {
        at: now,
        available_at: now,
        kind,
        observations: ["fixture-a", "fixture-b"]
            .into_iter()
            .map(|key| Observation {
                key: key.into(),
                quote: Quote {
                    bid: Price::parse(&price.to_string()).unwrap(),
                    ask: Price::parse(&format!("{price}.05")).unwrap(),
                    bid_units: 100000,
                    ask_units: 100000,
                    source: now,
                    receipt: now,
                },
                lower_circuit_limit: a("1"),
                upper_circuit_limit: a("10000"),
                volume: 2_000_000,
                archive_sha256: None,
                event_at_ms: None,
                available_at_ms: None,
                purpose: None,
            })
            .collect(),
    }
}
fn paper_event(p: &mut Portfolio, s: &mut Scanner, event: &Event, universe: &[Instrument]) {
    if matches!(event.kind, EventKind::Screen) {
        for obs in &event.observations {
            let row = serde_json::json!({"lower_circuit_limit":1,"upper_circuit_limit":10000,"volume":obs.volume});
            s.observe(
                event.at,
                universe.iter().find(|i| i.key == obs.key).unwrap(),
                &obs.quote,
                &row,
            )
            .unwrap();
        }
    }
    let entries = if matches!(event.kind, EventKind::Entry) {
        s.ranked(event.at, p, universe)
    } else {
        vec![]
    };
    let quotes: BTreeMap<_, _> = event
        .observations
        .iter()
        .map(|o| (o.key.clone(), o.quote.clone()))
        .collect();
    p.apply_at(
        chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        event.at,
        &quotes,
        &entries,
    )
    .unwrap();
}
#[test]
fn shared_paper_and_streamed_replay_abstain_without_a_model_plan() {
    let manifest = manifest();
    let universe = manifest.universe.clone();
    let mut replay = Runner::new(manifest).unwrap();
    let mut paper = Portfolio::new();
    paper.costs = replay.portfolio().costs.clone();
    let mut scanner = Scanner::default();
    let mut events = vec![];
    for n in 0..12 {
        events.push(event(at() + n * 30000, 100 + n, EventKind::Screen));
    }
    events.push(event(at() + 330001, 112, EventKind::Entry));
    events.push(event(at() + 332000, 116, EventKind::Protection));
    for event in events {
        paper_event(&mut paper, &mut scanner, &event, &universe);
        replay.apply(event).unwrap();
        assert_eq!(replay.portfolio(), &paper);
    }
    let report = replay.finish().unwrap();
    assert!(report.portfolio.positions.is_empty());
    assert_eq!(report.portfolio.fills.len(), 0);
    assert!(report.fees == a("0"));
    assert!(report.turnover == a("0"));
    assert!(!report.promotion_authorized);
    assert!(!report.operationally_accepted);
    assert_eq!(
        serde_json::to_value(report.portfolio).unwrap(),
        serde_json::to_value(paper).unwrap()
    );
}
#[test]
fn leakage_reordering_unknown_identity_and_future_receipts_fail_before_any_mutation() {
    let mut runner = Runner::new(manifest()).unwrap();
    let original = runner.portfolio().clone();
    let mut bad = event(at(), 100, EventKind::Screen);
    bad.available_at += 1;
    assert_eq!(runner.apply(bad), Err(historical::Error::Leakage));
    assert_eq!(runner.portfolio(), &original);
    let mut bad = event(at(), 100, EventKind::Screen);
    bad.observations[1].quote.receipt += 1;
    assert_eq!(runner.apply(bad), Err(historical::Error::Leakage));
    let mut bad = event(at(), 100, EventKind::Screen);
    bad.observations[1].key = "unknown".into();
    assert_eq!(runner.apply(bad), Err(historical::Error::Identity));
    assert_eq!(runner.portfolio(), &original);
    runner.apply(event(at(), 100, EventKind::Screen)).unwrap();
    assert_eq!(
        runner.apply(event(at(), 100, EventKind::Entry)),
        Err(historical::Error::Chronology)
    );
    let mut bad = event(at() + 1, 100, EventKind::Entry);
    bad.observations[0].quote.ask = Price::parse("99").unwrap();
    assert_eq!(runner.apply(bad), Err(historical::Error::Quote));
}
#[test]
fn virtual_clock_is_explicit_holdout_is_not_accepted_and_freshness_cannot_overflow() {
    let mut m = serde_json::to_value(serde_json::json!({"split":"holdout"})).unwrap();
    m["receipt_clock"] = serde_json::json!("live");
    assert!(serde_json::from_value::<Manifest>(m).is_err());
    let mut q = event(at(), 100, EventKind::Screen)
        .observations
        .remove(0)
        .quote;
    assert!(q.fresh(at() + 2000, 2000));
    assert!(!q.fresh(at() + 2001, 2000));
    q.source = i64::MIN;
    assert!(!q.fresh(i64::MAX, 2000));
    let mut scanner = Scanner::default();
    scanner.ranking.push((a("1"), "fixture-a".into(), at() + 1));
    assert!(
        scanner
            .ranked(at(), &Portfolio::new(), &manifest().universe)
            .is_empty()
    );
}
