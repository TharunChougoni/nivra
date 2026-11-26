use super::{
    super::{market_archive, preparation, wide},
    *,
};
use archive::{CandleUnit, CoveredDays, DailyCoverage};
use download::gaps;
use std::sync::atomic::AtomicBool;

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn instrument(key: &str) -> Instrument {
    Instrument {
        key: key.into(),
        symbol: "FIXTURE".into(),
        series: "EQ".into(),
        tick: nivra_core::finance::Price::parse("0.05").unwrap(),
    }
}

#[test]
fn reviewed_session_gaps_and_admission() {
    let sessions = ["2026-10-16", "2026-10-19", "2026-10-20", "2026-10-21"].map(day);
    let [first, listing, observed, late] = sessions;
    let target = late.succ_opt().unwrap();
    let plan = |answered| {
        gaps(
            first,
            late,
            CandleUnit::Daily,
            Some(&sessions),
            &CoveredDays {
                answered,
                ..Default::default()
            },
        )
        .unwrap()
    };
    assert_eq!(plan(BTreeSet::from([listing, observed])), [(first, late)]);
    assert_eq!(
        plan(BTreeSet::from([first, listing, observed])),
        [(late, late)]
    );
    assert!(plan(sessions.into_iter().collect()).is_empty());
    assert_eq!(
        gaps(
            first,
            late,
            CandleUnit::Daily,
            Some(&sessions),
            &CoveredDays {
                answered: BTreeSet::from([listing, observed]),
                excluded: BTreeSet::from([day("2026-10-17")])
            }
        )
        .unwrap(),
        [(first, first), (late, late)]
    );
    assert_eq!(
        gaps(
            day("2026-01-31"),
            day("2026-03-02"),
            CandleUnit::Minute,
            None,
            &CoveredDays::default()
        )
        .unwrap(),
        [
            (day("2026-01-31"), day("2026-02-27")),
            (day("2026-02-28"), day("2026-03-02"))
        ],
    );

    let dir = tempfile::tempdir().unwrap();
    let mut config = super::super::tests::config(dir.path());
    config.session = target;
    config.history_preparation_enabled = true;
    let full = || DailyCoverage {
        key: "full".into(),
        observed: 4,
        average_volume: Some(500.0),
        covered_empty: 0,
        missing: vec![],
    };
    let partial = || DailyCoverage {
        key: "partial".into(),
        observed: 1,
        average_volume: Some(100.0),
        covered_empty: 1,
        missing: vec![first, listing, late],
    };
    let report = |rows: Vec<_>| {
        preparation::apply_coverage(
            preparation::base_report(target, late, rows.len(), rows.len() * sessions.len()),
            rows,
        )
    };
    let ready = report(vec![full()]);
    assert_eq!(ready.status, preparation::CoverageStatus::Ready);
    assert_eq!(ready.daily_volumes["full"], (500.0, 4));
    let mixed = report(vec![full(), partial()]);
    assert_eq!(mixed.status, preparation::CoverageStatus::Partial);
    assert_eq!(
        (
            mixed.observed_bars,
            mixed.covered_empty_bars,
            mixed.retryable_missing_bars
        ),
        (5, 1, 2)
    );
    assert!(wide::history_admitted(&config, &json!(mixed)));
    assert!(!wide::history_admitted(
        &config,
        &json!(report(vec![partial()]))
    ));
    assert!(!wide::history_admitted(
        &config,
        &json!({"status":"BLOCKED","target_session":target})
    ));
}

fn fixture_archive_config(directory: &Path) -> Config {
    // The integration DSN points at a disposable container and carries its test password.
    let dsn = std::env::var("NIVRA_ARCHIVE_TEST_URL").unwrap();
    let (credentials, host) = dsn.rsplit_once('@').unwrap();
    let (user, password) = credentials.rsplit_once(':').unwrap();
    let password_file = directory.join("archive-password");
    fs::write(&password_file, password).unwrap();
    fs::set_permissions(&password_file, fs::Permissions::from_mode(0o600)).unwrap();
    let mut config = super::super::tests::config(directory);
    config.market_archive = Some(super::super::config::MarketArchiveSettings {
        url: format!("{user}@{host}"),
        password_file,
    });
    config
}

fn bars_body(days: &[NaiveDate]) -> String {
    let rows: Vec<_> = days
        .iter()
        .map(|day| json!([format!("{day}T00:00:00+05:30"), 100, 102, 99, 101, 500, 0]))
        .collect();
    json!({"status":"success","data":{"candles":rows}}).to_string()
}

struct FixtureProvider {
    calls: Vec<(String, NaiveDate, NaiveDate)>,
}
impl Transport for FixtureProvider {
    fn max_fetch_failures(&self) -> usize {
        1
    }
    fn skip_unavailable(&self) -> bool {
        true
    }
    fn fetch(
        &mut self,
        key: &str,
        _: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<(i64, i64, String)> {
        self.calls.push((key.into(), start, end));
        if key.ends_with("-retry") {
            return Err("http_503".into());
        }
        if key.ends_with("-unavailable") {
            return Err("http_400".into());
        }
        if key.ends_with("-quota") {
            return Err(data::RateLimited {
                retry_at_ms: Some(123),
                provider_response: true,
            }
            .into());
        }
        Ok((millis(), millis(), bars_body(&[start])))
    }
}

#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn import_history_twice_and_measure_coverage() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let config = fixture_archive_config(dir.path());
    let mut db = market_archive::open(&config)?;
    archive::init(&mut db)?;
    let namespace = format!("SYNTHETIC|ARCHIVE-E-{}", millis());
    let inputs = [
        "full",
        "partial",
        "excluded",
        "retry",
        "unavailable",
        "quota",
    ]
    .map(|suffix| instrument(&format!("{namespace}-{suffix}")));
    let sessions = ["2026-10-14", "2026-10-15", "2026-10-16"].map(day);
    let [first, middle, previous] = sessions;
    let target = previous.succ_opt().unwrap();
    let root = dir.path().join("history");
    fs::create_dir(&root)?;
    let write_fixture = |instrument: &Instrument, bar_days: &[NaiveDate]| -> Result<_> {
        let body = bars_body(bar_days);
        let raw = serde_json::to_vec(&json!({
            "key":instrument.key,"unit":"days","start":first,"end":previous,
            "requested_ms":1792200000000_i64,"received_ms":1792200000001_i64,
            "body":body,"body_sha256":digest(body.as_bytes()),
        }))?;
        let file = root.join(format!(
            "{}.receipt.json",
            range_id(&instrument.key, "days", first, previous)
        ));
        fs::write(&file, &raw)?;
        Ok((file, raw))
    };
    let (file, mut original) = write_fixture(&inputs[0], &sessions)?;
    write_fixture(&inputs[1], &[middle])?;
    let minute_key = format!("{namespace}-minute");
    let minute = include_bytes!("../../../../../fixtures/market-archive-candle-receipt.json");
    let mut minute: Value = serde_json::from_slice(minute)?;
    minute["key"] = json!(minute_key);
    fs::write(
        root.join(format!(
            "{}.receipt.json",
            range_id(&minute_key, "minutes", previous, previous)
        )),
        serde_json::to_vec(&minute)?,
    )?;
    let exclusion = json!({"key":inputs[2].key,"unit":"days","start":first,"end":previous,
        "recorded_ms":1792200000002_i64,"status":"provider_http_400_unavailable"});
    fs::write(
        root.join(format!(
            "{}.unavailable.json",
            range_id(&inputs[2].key, "days", first, previous)
        )),
        serde_json::to_vec(&exclusion)?,
    )?;

    let first_import = market_archive::import(&mut db, &root)?;
    assert_eq!(
        (
            first_import.receipts_inserted,
            first_import.unavailable_inserted
        ),
        (3, 1)
    );
    assert_eq!(
        market_archive::import(&mut db, &root)?,
        market_archive::ImportSummary::default()
    );
    assert_eq!(fs::read(&file)?, original);
    original.push(b'\n');
    fs::write(&file, original)?;
    assert_eq!(
        market_archive::import(&mut db, &root)
            .unwrap_err()
            .to_string(),
        "archive_history_import_conflict"
    );

    let report = preparation::coverage_report(&config, &inputs[..3], &sessions, target, previous);
    assert_eq!(report.status, preparation::CoverageStatus::Partial);
    assert_eq!(report.ready_instruments, 1);
    assert_eq!(
        (
            report.observed_bars,
            report.covered_empty_bars,
            report.retryable_missing_bars
        ),
        (4, 1, 4)
    );
    let mut provider = FixtureProvider { calls: vec![] };
    let window = || DownloadWindow {
        start: Some(first),
        end: Some(previous),
        interval: Interval::Days,
        sessions: Some(&sessions),
    };
    let control = PreparationControl {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline_ms: i64::MAX,
    };
    let result = download(
        market_archive::open(&config)?,
        &mut provider,
        &inputs,
        window(),
        Some(&control),
    );
    let failed = preparation::apply_run_outcome(report, result);
    assert_eq!(failed.status, preparation::CoverageStatus::Failed);
    assert_eq!(failed.retry_at_ms, Some(123));
    assert_eq!(
        provider.calls,
        [
            (inputs[1].key.clone(), previous, previous),
            (inputs[3].key.clone(), first, previous),
            (inputs[4].key.clone(), first, previous),
            (inputs[5].key.clone(), first, previous),
        ]
    );
    let statuses = db
        .query(
            "SELECT c.status FROM coverage c JOIN instruments i ON i.id = c.instrument_id
         WHERE i.key = ANY($1) AND day = $2 ORDER BY i.key",
            &[&[&inputs[3].key, &inputs[4].key].as_slice(), &first],
        )?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    assert_eq!(statuses, ["failed_retryable", "unavailable"]);
    provider.calls.clear();
    assert_eq!(
        download(
            market_archive::open(&config)?,
            &mut provider,
            &inputs[..5],
            window(),
            Some(&control)
        )?,
        1
    );
    assert_eq!(provider.calls, [(inputs[3].key.clone(), first, previous)]);
    let csv = archive::latest_daily_bars(&mut db, &[&inputs[0].key], target, 2)?;
    assert_eq!(
        csv.iter().map(|bar| bar.day).collect::<Vec<_>>(),
        [middle, previous]
    );
    assert_eq!(
        archive::daily_bars(&mut db, &[&inputs[0].key], Some(middle), Some(middle))?.len(),
        1
    );

    let instruments: Vec<_> = (0..2760)
        .map(|i| instrument(&format!("{namespace}-perf-{i}")))
        .collect();
    let keys: Vec<_> = instruments.iter().map(|i| i.key.as_str()).collect();
    let sessions: Vec<_> = (0..21)
        .map(|i| NaiveDate::from_ymd_opt(2026, 9, i / 5 * 7 + i % 5 + 1).unwrap())
        .collect();
    let mut tx = db.transaction()?;
    let receipt: i64 = tx
        .query_one("SELECT id FROM receipts WHERE kind = 2 LIMIT 1", &[])?
        .get(0);
    tx.execute(
        "INSERT INTO instruments (key) SELECT unnest($1::text[])",
        &[&keys],
    )?;
    tx.execute(
        "INSERT INTO daily_bars (instrument_id, day, o, h, l, c, volume, oi, receipt_id)
         SELECT i.id, day, 100, 103, 99, 102, 1000, 0, $3
         FROM instruments i CROSS JOIN unnest($2::date[]) AS day WHERE i.key = ANY($1)",
        &[&keys, &sessions, &receipt],
    )?;
    tx.execute(
        "INSERT INTO coverage (instrument_id, unit, day, status, attempted_ms, receipt_id)
         SELECT i.id, 2, day, 'observed', 0, $3
         FROM instruments i CROSS JOIN unnest($2::date[]) AS day WHERE i.key = ANY($1)",
        &[&keys, &sessions, &receipt],
    )?;
    tx.commit()?;
    db.batch_execute("ANALYZE instruments; ANALYZE coverage; ANALYZE daily_bars;")?;
    let last = *sessions.last().unwrap();
    for _ in 0..5 {
        let start = Instant::now();
        let report = preparation::coverage_report(
            &config,
            &instruments,
            &sessions,
            last.succ_opt().unwrap(),
            last,
        );
        let elapsed = start.elapsed();
        assert_eq!(
            report.status,
            preparation::CoverageStatus::Ready,
            "{report:?}"
        );
        assert_eq!(report.observed_bars, 57_960);
        assert_eq!(report.daily_volumes.len(), 2760);
        assert!(elapsed < Duration::from_millis(500), "{elapsed:?}");
        println!("2,760 instruments × 21 sessions coverage report: {elapsed:?}");
    }
    Ok(())
}
