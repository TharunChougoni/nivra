use super::*;
use nivra_core::{execution::Sample, finance::Price};
use serde_json::json;

fn sample(current: &str, now: i64) -> Sample {
    Sample {
        current: Some(Price::parse(current).unwrap()),
        history: vec![Price::parse("100").unwrap()],
        seen: now,
        source: Some(now),
        ..Sample::default()
    }
}

#[test]
fn nominations_displace_rank_only_after_residency_or_on_switch() {
    let now = 1_800_000_000_000;
    let mut scan = Scanner::default();
    scan.samples.insert("rise".into(), sample("103", now));
    scan.samples.insert("fall".into(), sample("97", now));
    scan.samples.insert("flat".into(), sample("100", now));
    let rising = Screen::parse(&json!({"promotion":{"order":"rising","min_move_bps":50}})).unwrap();
    let falling =
        Screen::parse(&json!({"promotion":{"order":"falling","min_move_bps":50}})).unwrap();
    let mut selection = Selection::default();
    let ranked = ["flat", "fall", "rise"];
    let held = BTreeSet::new();
    selection.refresh(&scan, now, rising.as_ref(), &held, ranked, 2);
    assert_eq!(selection.keys(), ["rise", "flat"]);
    assert_eq!(selection.nominees().collect::<Vec<_>>(), ["rise"]);

    for row in scan.samples.values_mut() {
        row.source = Some(now + 1_000);
        row.seen = now + 1_000;
    }
    scan.samples
        .insert("rise".into(), sample("98", now + 1_000));
    scan.samples
        .insert("fall".into(), sample("106", now + 1_000));
    selection.refresh(&scan, now + 1_000, rising.as_ref(), &held, ranked, 1);
    assert_eq!(selection.keys(), ["rise"]);
    for row in scan.samples.values_mut() {
        row.source = Some(now + 10_000);
        row.seen = now + 10_000;
    }
    selection.refresh(&scan, now + 10_000, rising.as_ref(), &held, ranked, 1);
    assert_eq!(selection.keys(), ["fall"]);

    selection.clear();
    selection.refresh(&scan, now + 10_001, falling.as_ref(), &held, ranked, 2);
    assert_eq!(selection.keys(), ["rise", "flat"]);
    let held = BTreeSet::from(["rise".to_owned()]);
    selection.refresh(&scan, now + 10_002, falling.as_ref(), &held, ranked, 2);
    assert_eq!(selection.keys(), ["flat", "fall"]);
    assert_eq!(selection.nominees().count(), 0);
}

#[test]
fn screening_caps_nominations_and_rejects_bad_declarations() {
    let now = 1_800_000_000_000;
    let scan = Scanner {
        samples: (0..50)
            .map(|i| (format!("stock-{i:02}"), sample("103", now)))
            .collect(),
        ..Scanner::default()
    };
    let mut selection = Selection::default();
    let screen = Screen::parse(&json!({"promotion":{"order":"largest_move"}})).unwrap();
    selection.refresh(&scan, now, screen.as_ref(), &BTreeSet::new(), [], 40);
    assert_eq!(selection.nominees().count(), 20);
    assert_eq!(selection.keys()[0], "stock-00");
    assert!(Screen::parse(&json!({"promotion":{"order":"typo"}})).is_err());
}
