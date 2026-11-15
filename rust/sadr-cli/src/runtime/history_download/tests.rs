use super::*;
use std::sync::atomic::AtomicBool;
struct Fake {
    calls: usize,
    requests: Vec<(String, NaiveDate, NaiveDate)>,
    bad: bool,
    fail_at: Option<usize>,
}
impl Transport for Fake {
    fn fetch(
        &mut self,
        _: &str,
        unit: &str,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<(i64, i64, String)> {
        self.calls += 1;
        self.requests.push((unit.into(), start, end));
        if self.fail_at == Some(self.calls) {
            return Err("fixture_transport_interrupted".into());
        }
        let time = if unit == "days" {
            "00:00:00"
        } else {
            "09:15:00"
        };
        let body = if self.bad {
            "{malformed".into()
        } else {
            json!({"status":"success","data":{"candles":[[format!("{start}T{time}+05:30"),100,102,99,101,1000,0]]}}).to_string()
        };
        Ok((now() - 1000, now(), body))
    }
}
fn now() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-17T10:00:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}
fn instruments() -> Vec<Instrument> {
    vec![Instrument {
        key: "NSE_EQ|INE000000001".into(),
        symbol: "FIXTURE".into(),
        series: "EQ".into(),
        tick: sadr_core::finance::Price::parse("0.05").unwrap(),
    }]
}
fn unavailable_history_does_not_block_other_instruments_or_retry() {
    struct Partial(usize);
    impl Transport for Partial {
        fn skip_unavailable(&self) -> bool {
            true
        }
        fn fetch(
            &mut self,
            key: &str,
            _: &str,
            start: NaiveDate,
            _: NaiveDate,
        ) -> Result<(i64, i64, String)> {
            self.0 += 1;
            if key.ends_with('2') {
                return Err("http_400".into());
            }
            Ok((now()-1000,now(),json!({"status":"success","data":{"candles":[[format!("{start}T00:00:00+05:30"),100,101,99,100,1000,0]]}}).to_string()))
        }
    }
    let d = tempfile::tempdir().unwrap();
    let mut list = instruments();
    let mut other = list[0].clone();
    other.key = "NSE_EQ|INE000000002".into();
    other.symbol = "OTHER".into();
    list.push(other);
    let mut transport = Partial(0);
    for _ in 0..2 {
        download(
            &mut transport,
            now,
            &list,
            d.path(),
            Some(date("2026-10-16")),
            Some(date("2026-10-16")),
            Interval::Days,
        )
        .unwrap();
    }
    assert_eq!(transport.0, 2);
    assert_eq!(
        fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e
                .file_name()
                .to_string_lossy()
                .ends_with(".unavailable.json"))
            .count(),
        1
    );
    assert_eq!(
        fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".receipt.json"))
            .count(),
        1
    );
}
#[test]
fn daily_only_plan_reuses_receipts_across_interval_choices() {
    unavailable_history_does_not_block_other_instruments_or_retry();
    // Real VM receipt bodies and one original shard per monthly range.
    // No config, credentials, transport or account state is loaded.
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust/history-legacy");
    // #63: a later release's engine (seen on the VM on 27 Oct) must also be reused.
    const LATER_ENGINE: &str = "e7fbc33dfab7b33ef132fb5ab4d36909a5d623e713d9db53bb6af6f979ebbfef";
    for previous_engine in [LEGACY_ENGINE, PRE_SCANNER_ENGINE, LATER_ENGINE] {
        let legacy_dir = tempfile::tempdir().unwrap();
        let mut universe = Vec::new();
        let mut originals = Vec::new();
        for entry in fs::read_dir(&samples).unwrap() {
            let entry = entry.unwrap();
            let mut bytes = fs::read(entry.path()).unwrap();
            if entry
                .file_name()
                .to_string_lossy()
                .ends_with(".manifest.json")
                && previous_engine != LEGACY_ENGINE
            {
                let mut value: Value = serde_json::from_slice(&bytes).unwrap();
                value["bindings"]["engine"] = json!(previous_engine);
                bytes = serde_json::to_vec(&value).unwrap();
            }
            if entry
                .file_name()
                .to_string_lossy()
                .ends_with(".manifest.json")
            {
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                universe.push(serde_json::from_value(value["universe"][0].clone()).unwrap());
            }
            let path = legacy_dir.path().join(entry.file_name());
            fs::write(&path, &bytes).unwrap();
            originals.push((path, bytes));
        }
        universe.sort_by(|a: &Instrument, b| a.key.cmp(&b.key));
        universe.dedup_by(|a, b| a.key == b.key);
        let mut legacy_transport = Fake {
            calls: 0,
            requests: Vec::new(),
            bad: false,
            fail_at: None,
        };
        let reuse = |t: &mut Fake| {
            download(
                t,
                now,
                &universe,
                legacy_dir.path(),
                Some(date("2025-10-17")),
                Some(date("2026-10-16")),
                Interval::Days,
            )
        };
        // A sibling test may fork while the download lock is owned, and the
        // child shares the open file description until exec closes it. Keep
        // a duplicate alive across the release: explicit unlock must free the
        // lock before the next download instead of relying on a retry (#77).
        let owner = AdvisoryLock::new(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(legacy_dir.path().join("download.lock"))
                .unwrap(),
        );
        owner.try_lock_exclusive().unwrap();
        let inherited = owner.try_clone().unwrap();
        drop(owner);
        reuse(&mut legacy_transport).unwrap();
        assert_eq!(legacy_transport.calls, 5);
        let mut expected = vec![
            ("days".into(), date("2025-11-17"), date("2026-10-16")),
            ("days".into(), date("2025-10-17"), date("2026-05-16")),
            ("days".into(), date("2026-06-17"), date("2026-10-16")),
            ("days".into(), date("2025-10-17"), date("2026-03-16")),
            ("days".into(), date("2026-04-17"), date("2026-10-16")),
        ];
        expected.sort();
        legacy_transport.requests.sort();
        assert_eq!(legacy_transport.requests, expected);
        reuse(&mut legacy_transport).unwrap();
        assert_eq!(legacy_transport.calls, 5);
        drop(inherited);
        for (path, bytes) in &originals {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        let (manifest, bytes) = originals
            .iter()
            .find(|(p, _)| p.to_string_lossy().ends_with(".manifest.json"))
            .unwrap();
        let mut changed = bytes.clone();
        changed.push(b' ');
        fs::write(manifest, &changed).unwrap();
        let error = reuse(&mut legacy_transport).unwrap_err();
        assert_eq!(
            failure_code(error.as_ref()),
            "history_manifest_artifact_changed"
        );
        assert_eq!(fs::read(manifest).unwrap(), changed);
        assert_eq!(legacy_transport.calls, 5);
        struct ReceiptsOnly<'a>(&'a mut Fake);
        impl Transport for ReceiptsOnly<'_> {
            fn receipts_only(&self) -> bool {
                true
            }
            fn fetch(
                &mut self,
                key: &str,
                unit: &str,
                start: NaiveDate,
                end: NaiveDate,
            ) -> Result<(i64, i64, String)> {
                self.0.fetch(key, unit, start, end)
            }
        }
        download(
            &mut ReceiptsOnly(&mut legacy_transport),
            now,
            &universe,
            legacy_dir.path(),
            Some(date("2025-10-17")),
            Some(date("2026-10-16")),
            Interval::Days,
        )
        .unwrap();
        assert_eq!(fs::read(manifest).unwrap(), changed);
        assert_eq!(
            legacy_transport.calls, 5,
            "valid original receipts must be reused"
        );
        // Receipts-only is not an integrity bypass: a changed raw body hash
        // still refuses and is preserved, even without replay derivation.
        let receipt = originals
            .iter()
            .find(|(p, _)| p.to_string_lossy().ends_with(".receipt.json"))
            .unwrap();
        let mut corrupted: Value = serde_json::from_slice(&receipt.1).unwrap();
        corrupted["body_sha256"] = json!("0".repeat(64));
        let corrupted = serde_json::to_vec(&corrupted).unwrap();
        fs::write(&receipt.0, &corrupted).unwrap();
        let error = download(
            &mut ReceiptsOnly(&mut legacy_transport),
            now,
            &universe,
            legacy_dir.path(),
            Some(date("2025-10-17")),
            Some(date("2026-10-16")),
            Interval::Days,
        )
        .unwrap_err();
        assert_eq!(failure_code(error.as_ref()), "history_receipt_invalid");
        assert_eq!(fs::read(&receipt.0).unwrap(), corrupted);
    }
    let dir = tempfile::tempdir().unwrap();
    let mut t = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    for (interval, expected_calls) in [
        (Interval::Days, 1),
        (Interval::Both, 2),
        (Interval::Days, 2),
        (Interval::Minutes, 2),
    ] {
        download(
            &mut t,
            now,
            &instruments(),
            dir.path(),
            Some(date("2026-10-01")),
            Some(date("2026-10-01")),
            interval,
        )
        .unwrap();
        assert_eq!(t.calls, expected_calls);
        if expected_calls == 1 {
            let path = fs::read_dir(dir.path())
                .unwrap()
                .map(|p| p.unwrap().path())
                .find(|p| p.to_string_lossy().ends_with(".receipt.json"))
                .unwrap();
            let receipt: Receipt = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert_eq!(receipt.unit, "days");
        }
    }
    let receipts: Vec<Receipt> = fs::read_dir(dir.path())
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".receipt.json"))
        .map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap())
        .collect();
    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts.iter().filter(|r| r.unit == "days").count(), 1);
    // Simulate v0.5.6 daily receipts, including partial coverage and disjoint gaps.
    for covered in [0..0, 0..6, 6..12, 0..12, 4..8] {
        let dir = tempfile::tempdir().unwrap();
        let start = date("2025-10-17");
        let end = date("2026-10-16");
        let mut t = Fake {
            calls: 0,
            requests: Vec::new(),
            bad: false,
            fail_at: None,
        };
        for month in covered.clone() {
            let from = start.checked_add_months(Months::new(month)).unwrap();
            download(
                &mut t,
                now,
                &instruments(),
                dir.path(),
                Some(from),
                Some(window_end(from, end, "minutes").unwrap()),
                Interval::Days,
            )
            .unwrap();
        }
        let originals: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect();
        let before = t.calls;
        download(
            &mut t,
            now,
            &instruments(),
            dir.path(),
            Some(start),
            Some(end),
            Interval::Days,
        )
        .unwrap();
        let expected: Vec<_> = match (covered.start, covered.end) {
            (0, 12) => vec![],
            (0, 6) => vec![("days".into(), date("2026-04-17"), end)],
            (6, 12) => vec![("days".into(), start, date("2026-04-16"))],
            (4, 8) => vec![
                ("days".into(), start, date("2026-02-16")),
                ("days".into(), date("2026-06-17"), end),
            ],
            _ => vec![("days".into(), start, end)],
        };
        assert_eq!(&t.requests[before..], expected);
        let after = t.calls;
        download(
            &mut t,
            now,
            &instruments(),
            dir.path(),
            Some(start),
            Some(end),
            Interval::Days,
        )
        .unwrap();
        assert_eq!(t.calls, after);
        for (path, bytes) in originals {
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut t = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    download(
        &mut t,
        now,
        &instruments(),
        dir.path(),
        Some(date("2004-02-29")),
        Some(date("2026-10-16")),
        Interval::Days,
    )
    .unwrap();
    assert_eq!(
        t.requests,
        vec![
            ("days".into(), date("2004-02-29"), date("2014-02-27")),
            ("days".into(), date("2014-02-28"), date("2024-02-27")),
            ("days".into(), date("2024-02-28"), date("2026-10-16")),
        ]
    );
    assert_ne!(
        range_id("fixture", "days", date("2026-01-01"), date("2026-01-31")),
        range_id("fixture", "minutes", date("2026-01-01"), date("2026-01-31"))
    );

    // Coverage requires 21 actual dated bars, not 21 provider requests or
    // valid empty responses. Conflicting immutable receipts fail closed.
    let coverage_dir = tempfile::tempdir().unwrap();
    let universe = instruments();
    let previous = date("2026-10-16");
    let required: Vec<_> = (0..21)
        .rev()
        .map(|offset| previous.checked_sub_days(Days::new(offset)).unwrap())
        .collect();
    for day in &required {
        let body = json!({"status":"success","data":{"candles":[[
            format!("{day}T00:00:00+05:30"),100,102,99,101,1000,0
        ]]}})
        .to_string();
        let receipt = Receipt {
            key: universe[0].key.clone(),
            unit: "days".into(),
            start: *day,
            end: *day,
            requested_ms: now() - 1000,
            received_ms: now(),
            body_sha256: digest(body.as_bytes()),
            body,
        };
        immutable(
            &coverage_dir.path().join(format!(
                "{}.receipt.json",
                range_id(&receipt.key, "days", *day, *day)
            )),
            &serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }
    let ready = super::super::preparation::coverage_report(
        coverage_dir.path(),
        &universe,
        &required,
        date("2026-10-19"),
        previous,
    );
    assert_eq!(
        ready.status,
        super::super::preparation::CoverageStatus::Ready
    );
    assert_eq!(ready.observed_bars, 21);
    assert_eq!(ready.missing_bars, 0);
    assert!(ready.missing_sessions.is_empty());
    let empty_dir = tempfile::tempdir().unwrap();
    for day in required.iter().skip(1) {
        let body = json!({"status":"success","data":{"candles":[[
            format!("{day}T00:00:00+05:30"),100,102,99,101,1000,0
        ]]}})
        .to_string();
        let receipt = Receipt {
            key: universe[0].key.clone(),
            unit: "days".into(),
            start: *day,
            end: *day,
            requested_ms: now() - 1000,
            received_ms: now(),
            body_sha256: digest(body.as_bytes()),
            body,
        };
        immutable(
            &empty_dir.path().join(format!(
                "{}.receipt.json",
                range_id(&receipt.key, "days", *day, *day)
            )),
            &serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }
    let empty_body = json!({"status":"success","data":{"candles":[]}}).to_string();
    let empty_receipt = Receipt {
        key: universe[0].key.clone(),
        unit: "days".into(),
        start: required[0],
        end: required[0],
        requested_ms: now() - 1000,
        received_ms: now(),
        body_sha256: digest(empty_body.as_bytes()),
        body: empty_body,
    };
    immutable(
        &empty_dir.path().join(format!(
            "{}.receipt.json",
            range_id(&empty_receipt.key, "days", required[0], required[0])
        )),
        &serde_json::to_vec(&empty_receipt).unwrap(),
    )
    .unwrap();
    let partial = super::super::preparation::coverage_report(
        empty_dir.path(),
        &universe,
        &required,
        date("2026-10-19"),
        previous,
    );
    assert_eq!(
        partial.status,
        super::super::preparation::CoverageStatus::Partial
    );
    assert_eq!(partial.observed_bars, 20);
    assert_eq!(partial.missing_bars, 1);
    assert_eq!(
        partial.missing_sessions[&universe[0].key],
        vec![required[0]]
    );
    let day = required[0];
    let conflict_body = json!({"status":"success","data":{"candles":[[
        format!("{day}T00:00:00+05:30"),100,103,99,102,1000,0
    ]]}})
    .to_string();
    let conflict = Receipt {
        key: universe[0].key.clone(),
        unit: "days".into(),
        start: day.checked_sub_days(Days::new(1)).unwrap(),
        end: day,
        requested_ms: now() - 1000,
        received_ms: now(),
        body_sha256: digest(conflict_body.as_bytes()),
        body: conflict_body,
    };
    immutable(
        &coverage_dir.path().join(format!(
            "{}.receipt.json",
            range_id(&conflict.key, "days", conflict.start, conflict.end)
        )),
        &serde_json::to_vec(&conflict).unwrap(),
    )
    .unwrap();
    let failed = super::super::preparation::coverage_report(
        coverage_dir.path(),
        &universe,
        &required,
        date("2026-10-19"),
        previous,
    );
    assert_eq!(
        failed.status,
        super::super::preparation::CoverageStatus::Unknown
    );
    assert_eq!(failed.failure.as_deref(), Some("history_receipt_conflict"));

    // The HTTP client accepts historical bodies up to 20 MB. A valid receipt
    // within that limit must remain readable by the preparation coverage gate.
    let large_dir = tempfile::tempdir().unwrap();
    for (index, day) in required.iter().enumerate() {
        let mut body = json!({"status":"success","data":{"candles":[[
            format!("{day}T00:00:00+05:30"),100,102,99,101,1000,0
        ]]}})
        .to_string();
        if index == 0 {
            body.push_str(&" ".repeat(16_000_001 - body.len()));
        }
        let receipt = Receipt {
            key: universe[0].key.clone(),
            unit: "days".into(),
            start: *day,
            end: *day,
            requested_ms: now() - 1000,
            received_ms: now(),
            body_sha256: digest(body.as_bytes()),
            body,
        };
        immutable(
            &large_dir.path().join(format!(
                "{}.receipt.json",
                range_id(&receipt.key, "days", *day, *day)
            )),
            &serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
    }
    let large = super::super::preparation::coverage_report(
        large_dir.path(),
        &universe,
        &required,
        date("2026-10-19"),
        previous,
    );
    // Admission must check cancellation before the first injected request.
    let cancelled = Arc::new(AtomicBool::new(true));
    let control = PreparationControl {
        cancelled,
        deadline_ms: now() + 10_000,
    };
    let mut transport = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    let error = download_controlled(
        &mut transport,
        now,
        &universe,
        tempfile::tempdir().unwrap().path(),
        Some(required[0]),
        Some(previous),
        Interval::Days,
        Some(&control),
        None,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "history_preparation_cancelled");
    assert_eq!(transport.calls, 0);
    // A reviewed weekend/holiday needs no request; split session gaps
    // overlap the immutable middle receipt in one catch-up request.
    let prep_dir = tempfile::tempdir().unwrap();
    let config = super::super::tests::config(prep_dir.path());
    let prep_now = || {
        DateTime::parse_from_rfc3339("2026-10-23T08:00:00+05:30")
            .unwrap()
            .timestamp_millis()
    };
    for (first, last) in [("2026-10-10", "2026-10-11"), ("2026-10-02", "2026-10-02")] {
        download_controlled(
            &mut transport,
            prep_now,
            &instruments(),
            prep_dir.path(),
            Some(date(first)),
            Some(date(last)),
            Interval::Days,
            None,
            Some(&config),
        )
        .unwrap();
    }
    assert_eq!(transport.calls, 0);
    download(
        &mut transport,
        prep_now,
        &instruments(),
        prep_dir.path(),
        Some(date("2026-10-14")),
        Some(date("2026-10-14")),
        Interval::Days,
    )
    .unwrap();
    let originals: Vec<_> = fs::read_dir(prep_dir.path())
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    download_controlled(
        &mut transport,
        prep_now,
        &instruments(),
        prep_dir.path(),
        Some(date("2026-10-10")),
        Some(date("2026-10-16")),
        Interval::Days,
        None,
        Some(&config),
    )
    .unwrap();
    assert_eq!(
        transport.requests[1..],
        [("days".into(), date("2026-10-12"), date("2026-10-16"))]
    );
    for (path, bytes) in originals {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    download_controlled(
        &mut transport,
        prep_now,
        &instruments(),
        prep_dir.path(),
        Some(date("2026-10-10")),
        Some(date("2026-10-16")),
        Interval::Days,
        None,
        Some(&config),
    )
    .unwrap();
    assert_eq!(transport.calls, 2);
    // Calendar coverage is never inferred for unreviewed dates.
    let unreviewed = download_controlled(
        &mut transport,
        prep_now,
        &instruments(),
        prep_dir.path(),
        Some(date("2025-12-31")),
        Some(date("2025-12-31")),
        Interval::Days,
        None,
        Some(&config),
    );
    assert!(unreviewed.is_err());
    assert_eq!(transport.calls, 2);
    assert_eq!(
        large.status,
        super::super::preparation::CoverageStatus::Ready
    );
    let large_receipt_path = fs::read_dir(large_dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.to_string_lossy().ends_with(".receipt.json"))
        .unwrap();
    assert_eq!(read_receipt(&large_receipt_path).unwrap().unit, "days");

    let oversized_dir = tempfile::tempdir().unwrap();
    let oversized_path = oversized_dir.path().join("too-large.receipt.json");
    let oversized_file = fs::File::create(&oversized_path).unwrap();
    oversized_file.set_len(MAX_RECEIPT_BYTES + 1).unwrap();
    assert!(matches!(
        read_receipt(&oversized_path),
        Err("history_receipt_unreadable")
    ));
}
#[test]
fn resume_preserves_receipts_and_produces_consumable_evidence_shards() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join(".history-stage-interrupted"),
        b"partial evidence",
    )
    .unwrap();
    let mut t = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    let run = |t: &mut Fake| {
        download(
            t,
            now,
            &instruments(),
            dir.path(),
            Some(date("2026-09-30")),
            Some(date("2026-10-01")),
            Interval::Both,
        )
    };
    t.fail_at = Some(2);
    assert!(run(&mut t).is_err());
    let first = fs::read_dir(dir.path())
        .unwrap()
        .map(|p| p.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".receipt.json"))
        .unwrap();
    let first_bytes = fs::read(&first).unwrap();
    run(&mut t).unwrap();
    assert_eq!(fs::read(first).unwrap(), first_bytes);
    assert_eq!(t.calls, 3);
    let entries: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .map(|p| p.unwrap().path())
        .collect();
    let before: Vec<_> = entries.iter().map(|p| fs::read(p).unwrap()).collect();
    run(&mut t).unwrap();
    assert_eq!(t.calls, 3);
    for (path, bytes) in entries.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    for path in entries
        .iter()
        .filter(|p| p.to_string_lossy().ends_with(".manifest.json"))
    {
        let prefix = path
            .to_string_lossy()
            .trim_end_matches(".manifest.json")
            .to_owned();
        let report = sadr_core::replay::evaluate(
            &fs::read(path).unwrap(),
            &fs::read(format!("{prefix}.events.jsonl")).unwrap(),
            || true,
        )
        .unwrap();
        assert_eq!(report.report.events, 1);
        assert!(report.report.portfolio.fills.is_empty());
    }
}
#[test]
fn failed_receipt_and_tampering_are_preserved_without_redownload() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: true,
        fail_at: None,
    };
    let run = |t: &mut Fake| {
        download(
            t,
            now,
            &instruments(),
            dir.path(),
            Some(date("2026-10-01")),
            Some(date("2026-10-01")),
            Interval::Both,
        )
    };
    assert!(run(&mut t).is_err());
    assert_eq!(t.calls, 1);
    t.bad = false;
    assert!(run(&mut t).is_err());
    assert_eq!(t.calls, 1);
    let receipt = fs::read_dir(dir.path())
        .unwrap()
        .map(|p| p.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".receipt.json"))
        .unwrap();
    let original = fs::read(&receipt).unwrap();
    let mut value: Value = serde_json::from_slice(&original).unwrap();
    value["body_sha256"] = json!("tampered");
    fs::write(&receipt, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(run(&mut t).is_err());
    assert_eq!(t.calls, 1);
}
#[test]
fn calendar_month_default_chunks_and_invalid_dates_use_no_provider_calls() {
    let dir = tempfile::tempdir().unwrap();
    let mut t = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    download(
        &mut t,
        now,
        &instruments(),
        dir.path(),
        None,
        None,
        Interval::Both,
    )
    .unwrap();
    assert_eq!(t.calls, 13);
    let mut t = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    for (start, end) in [
        ("2021-12-31", "2022-01-01"),
        ("2026-10-17", "2026-10-17"),
        ("2026-10-02", "2026-10-01"),
    ] {
        assert!(
            download(
                &mut t,
                now,
                &instruments(),
                dir.path(),
                Some(date(start)),
                Some(date(end)),
                Interval::Both,
            )
            .is_err()
        );
    }
    assert_eq!(t.calls, 0);
}
#[test]
fn preparation_skips_a_bounded_number_of_failed_instruments_and_continues() {
    struct Flaky {
        calls: usize,
        tolerate: usize,
        error: &'static str,
    }
    impl Transport for Flaky {
        fn max_fetch_failures(&self) -> usize {
            self.tolerate
        }
        fn fetch(
            &mut self,
            key: &str,
            _: &str,
            start: NaiveDate,
            _: NaiveDate,
        ) -> Result<(i64, i64, String)> {
            self.calls += 1;
            if key.ends_with('1') {
                return Err(Fail::new(self.error).into());
            }
            Ok((now()-1000,now(),json!({"status":"success","data":{"candles":[[format!("{start}T00:00:00+05:30"),100,101,99,100,1000,0]]}}).to_string()))
        }
    }
    let mut list = instruments();
    for n in 2..=3 {
        let mut other = list[0].clone();
        other.key = format!("NSE_EQ|INE00000000{n}");
        list.push(other);
    }
    let control = PreparationControl {
        cancelled: Arc::new(AtomicBool::new(false)),
        deadline_ms: i64::MAX,
    };
    let run = |tolerate: usize| {
        let d = tempfile::tempdir().unwrap();
        let mut t = Flaky {
            calls: 0,
            tolerate,
            error: "http_timeout",
        };
        let r = download_controlled(
            &mut t,
            now,
            &list,
            d.path(),
            Some(date("2026-10-16")),
            Some(date("2026-10-16")),
            Interval::Days,
            Some(&control),
            None,
        );
        (r.map_err(|e| e.to_string()), t.calls)
    };
    // Old behaviour (no tolerance): the first failure stops every later instrument.
    assert_eq!(run(0), (Err("http_timeout".into()), 1));
    // Tolerated: the failed one is skipped and the rest are still fetched.
    assert_eq!(run(1), (Ok(1), 3));
    for code in [
        "http_timeout",
        "read_only_http_failed",
        "http_500",
        "http_503",
        "http_429_rate_limited",
        "http_401_unauthorized",
        "http_403_forbidden",
        "http_400",
        "http_404",
        "original_read_journal_changed",
        "read_quota_exhausted",
        "history_preparation_cancelled",
        "history_preparation_deadline",
        "response_too_large",
    ] {
        let d = tempfile::tempdir().unwrap();
        let mut t = Flaky {
            calls: 0,
            tolerate: 1,
            error: code,
        };
        let result = download_controlled(
            &mut t,
            now,
            &list,
            d.path(),
            Some(date("2026-10-16")),
            Some(date("2026-10-16")),
            Interval::Days,
            Some(&control),
            None,
        );
        let transient = matches!(
            code,
            "http_timeout"
                | "read_only_http_failed"
                | "http_500"
                | "http_503"
                | "http_429_rate_limited"
        );
        assert_eq!(result.is_ok(), transient, "{code}");
        assert_eq!(t.calls, if transient { 3 } else { 1 }, "{code}");
        let diagnostics: Vec<_> = fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".failure.json"))
            .collect();
        assert_eq!(diagnostics.len(), 1, "{code}");
        let diagnostic: Value =
            serde_json::from_slice(&fs::read(&diagnostics[0]).unwrap()).unwrap();
        assert_eq!(diagnostic["failure"], code);
        assert_eq!(diagnostic["key"], list[0].key);
    }
    // Same-day resume fetches only the failed instrument; successful immutable
    // receipts survive byte-for-byte and consume no second provider read.
    let d = tempfile::tempdir().unwrap();
    let mut t = Flaky {
        calls: 0,
        tolerate: 1,
        error: "http_timeout",
    };
    let acquire = |t: &mut Flaky, list: &[Instrument]| {
        download_controlled(
            t,
            now,
            list,
            d.path(),
            Some(date("2026-10-16")),
            Some(date("2026-10-16")),
            Interval::Days,
            Some(&control),
            None,
        )
    };
    assert_eq!(acquire(&mut t, &list).unwrap(), 1);
    let receipts = || -> BTreeMap<_, _> {
        fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.to_string_lossy().ends_with(".receipt.json"))
            .map(|p| {
                let bytes = fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect()
    };
    let before = receipts();
    assert_eq!(before.len(), 2);
    let failed = fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.to_string_lossy().ends_with(".failure.json"))
        .unwrap();
    let failed_before = fs::read(&failed).unwrap();
    // Make the previously failing key succeed using a transport without failure.
    let mut recovered = Fake {
        calls: 0,
        requests: Vec::new(),
        bad: false,
        fail_at: None,
    };
    download_controlled(
        &mut recovered,
        now,
        &list,
        d.path(),
        Some(date("2026-10-16")),
        Some(date("2026-10-16")),
        Interval::Days,
        Some(&control),
        None,
    )
    .unwrap();
    assert_eq!(recovered.calls, 1);
    assert_eq!(fs::read(failed).unwrap(), failed_before);
    for (path, bytes) in before {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
#[test]
fn manifest_reuse_tolerates_engine_and_symbol_rename_only() {
    let manifest = |engine: &str, symbol: &str, tick: &str, data: &str| {
        serde_json::to_vec(&json!({"bindings":{"engine":engine,"data":data},"policy":"p","session":"2026-10-16","universe":[{"key":"NSE_EQ|INE726G01019","series":"EQ","symbol":symbol,"tick":tick}]})).unwrap()
    };
    let old = "90ab48f6e1d6fc74505b86388613104f168578b7911ca9647faf4b1685530272";
    let new = "31dbc7aa507c94f2468d1caaf3c227c808a63d2148435bb91751e790435ae10e";
    let rebuilt = manifest(new, "ICICILIFE", "0.05", "d");
    assert!(reusable_manifest(
        &manifest(old, "ICICIPRULI", "0.05", "d"),
        &rebuilt
    ));
    assert!(reusable_manifest(
        &manifest(new, "ICICILIFE", "0.05", "d"),
        &rebuilt
    ));
    assert!(!reusable_manifest(
        &manifest(old, "ICICIPRULI", "0.10", "d"),
        &rebuilt
    ));
    assert!(!reusable_manifest(
        &manifest(old, "ICICIPRULI", "0.05", "x"),
        &rebuilt
    ));
    assert!(!reusable_manifest(
        &manifest("not-hex", "ICICILIFE", "0.05", "d"),
        &rebuilt
    ));
    let mut spaced = manifest(old, "ICICIPRULI", "0.05", "d");
    spaced.push(b' ');
    assert!(!reusable_manifest(&spaced, &rebuilt));
}
