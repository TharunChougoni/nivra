use super::*;

#[test]
fn restart_metadata_uses_original_receipt_and_configured_age() {
    let db = Connection::open_in_memory().unwrap();
    let instrument = instrument(1);
    let receipt = start();
    let mut original = Scanner::default();
    let fields = nivra_core::execution::MarketFields {
        session_vwap: None,
        best_bid: None,
        best_bid_qty: None,
        best_ask: None,
        best_ask_qty: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        lower_circuit_limit: a("50"),
        upper_circuit_limit: a("150"),
        volume: 100000,
    };
    original
        .observe_fields(receipt, &instrument, &quote(receipt, 100), &fields)
        .unwrap();
    checkpoint_scanner(&db, &original).unwrap();
    let now = receipt + 600000;
    for (limit, usable) in [(900000, true), (599999, false), (0, false)] {
        let mut restored = load_scanner(&db, limit, now, &Default::default()).unwrap();
        assert_eq!(restored.samples[&instrument.key].metadata_preloaded, usable);
        let rows = serde_json::json!({"row": {"instrument_token": instrument.key, "symbol": instrument.symbol}});
        scan_rest_rows(
            &mut restored,
            std::slice::from_ref(&instrument),
            rows.as_object().unwrap(),
            now,
            now,
            2000,
            true,
            None,
        )
        .unwrap();
        let points = std::collections::BTreeMap::from([(
            instrument.key.clone(),
            super::super::super::market_feed::Point {
                price: Price::parse("100").unwrap(),
                source: now,
                receipt: now,
                last_trade: now,
                prior_close: None,
            },
        )]);
        super::super::super::market_feed::observe_cache(
            &mut restored,
            std::slice::from_ref(&instrument),
            &points,
            &super::super::super::market_feed::Settings {
                metadata_preload_max_age_ms: limit,
                ..Default::default()
            },
            now,
            Some(now),
            None,
        )
        .unwrap();
        let sample = &restored.samples[&instrument.key];
        assert_eq!(sample.market_fields_receipt, usable.then_some(receipt));
        assert_eq!(
            sample.reason,
            if usable {
                "eligible_no_signal"
            } else {
                "missing_market_fields"
            }
        );
    }
}
#[test]
fn dated_session_evidence_is_built_from_the_successor_policy() {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open_in_memory().unwrap();
    let instrument = instrument(1);
    let receipt = super::super::super::millis() - 600_000;
    let mut restored = Scanner::default();
    let fields = nivra_core::execution::MarketFields {
        session_vwap: None,
        best_bid: None,
        best_bid_qty: None,
        best_ask: None,
        best_ask_qty: None,
        total_buy_quantity: None,
        total_sell_quantity: None,
        lower_circuit_limit: a("50"),
        upper_circuit_limit: a("150"),
        volume: 100000,
    };
    restored
        .observe_fields(receipt, &instrument, &quote(receipt, 100), &fields)
        .unwrap();
    checkpoint_scanner(&db, &restored).unwrap();
    let policy = |metadata_preload_max_age_ms: i64| crate::runtime::planner::PlannerConfig {
        discovery: Some(
            serde_json::from_value(serde_json::json!({
                "max_scan_age_ms":2000,"max_prospects":10,
                "market_feed":{"metadata_preload_max_age_ms":metadata_preload_max_age_ms}
            }))
            .unwrap(),
        ),
        ..Default::default()
    };
    let mut next = super::super::super::tests::config(dir.path());
    next.session = next.session.succ_opt().unwrap();
    let evidence =
        super::super::controller::SessionEvidence::load(&db, &next, Some(&policy(900_000)))
            .unwrap();
    // Each dated book owns its stores, and its scanner preload follows that
    // book's policy instead of the session it replaced.
    assert_eq!(evidence.feature_store.session(), Some(next.session));
    assert_eq!(evidence.index_features.session(), Some(next.session));
    assert!(evidence.scan.samples[&instrument.key].metadata_preloaded);
    assert_eq!(evidence.option_status["status"], "disabled");
    let tight =
        super::super::controller::SessionEvidence::load(&db, &next, Some(&policy(0))).unwrap();
    assert!(!tight.scan.samples[&instrument.key].metadata_preloaded);
    let mut with_options = next.clone();
    with_options.nifty_options.enabled = true;
    let option_evidence =
        super::super::controller::SessionEvidence::load(&db, &with_options, None).unwrap();
    assert_eq!(option_evidence.option_status["status"], "waiting_for_spot");
}
#[test]
fn full_quote_documented_optional_features_and_receipt_minutes() {
    use nivra_core::strategy::{Feature, extract_features};
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../../fixtures/upstox_full_quote_doc_example.json"
    ))
    .unwrap();
    let documented = fixture["data"]["NSE_EQ:NHPC"].clone();
    let mut i = instrument(0);
    i.key = documented["instrument_token"].as_str().unwrap().into();
    i.symbol = "NHPC".into();
    // Screening depth remains optional; zero on either part removes the whole level.
    for (bid, ask, buy, sell, expected_bid, expected_ask, spread, imbalance) in [
        (
            serde_json::json!({"price": 52.05, "quantity": 6917}),
            serde_json::json!({"price": 0, "quantity": 0}),
            Some(6917_u64),
            Some(0_u64),
            Some(52.05),
            None,
            None,
            Some(1.0),
        ),
        (
            serde_json::json!({"price": 100, "quantity": 10}),
            serde_json::json!({"price": 102, "quantity": 20}),
            Some(10),
            Some(30),
            Some(100.0),
            Some(102.0),
            Some(2.0 / 101.0),
            Some(0.25),
        ),
        (
            serde_json::json!({"price": 102, "quantity": 10}),
            serde_json::json!({"price": 100, "quantity": 20}),
            Some(0),
            Some(30),
            Some(102.0),
            Some(100.0),
            None,
            Some(0.0),
        ),
        (
            serde_json::json!({"price": 100, "quantity": 10}),
            serde_json::json!({"price": 100, "quantity": 20}),
            Some(u64::MAX),
            Some(u64::MAX),
            Some(100.0),
            Some(100.0),
            Some(0.0),
            Some(0.5),
        ),
        (
            serde_json::json!({"price": 0, "quantity": 10}),
            serde_json::json!({"price": 102, "quantity": 0}),
            Some(0),
            Some(0),
            None,
            None,
            None,
            None,
        ),
        (
            serde_json::json!({"price": 100, "quantity": 0}),
            serde_json::json!({"price": 0, "quantity": 20}),
            None,
            Some(30),
            None,
            None,
            None,
            None,
        ),
        (
            serde_json::Value::Null,
            serde_json::Value::Null,
            Some(10),
            None,
            None,
            None,
            None,
            None,
        ),
    ] {
        let mut row = documented.clone();
        row["timestamp"] = serde_json::json!(start());
        row["depth"] = serde_json::json!({"buy": [bid], "sell": [ask]});
        row["total_buy_quantity"] = serde_json::json!(buy);
        row["total_sell_quantity"] = serde_json::json!(sell);
        if buy.is_none() {
            row.as_object_mut().unwrap().remove("depth");
        }
        let rows = serde_json::Map::from_iter([("NSE_EQ:NHPC".into(), row)]);
        let mut scan = Scanner::default();
        scan_rest_rows(
            &mut scan,
            std::slice::from_ref(&i),
            &rows,
            start(),
            start(),
            2000,
            false,
            None,
        )
        .unwrap();
        let fields = scan.samples[&i.key].market_fields.as_ref().unwrap();
        assert_eq!(fields.best_bid, expected_bid);
        assert_eq!(fields.best_ask, expected_ask);
        assert_eq!(
            fields.best_bid_qty,
            expected_bid.map(|_| if expected_bid == Some(52.05) {
                6917
            } else {
                10
            })
        );
        assert_eq!(fields.best_ask_qty, expected_ask.map(|_| 20));
        assert_eq!(fields.total_buy_quantity, buy);
        assert_eq!(fields.total_sell_quantity, sell);
        let snapshot = extract_features(&scan, start(), 2000);
        let features = &snapshot.rows[0].features;
        for (feature, expected) in [
            (Feature::BestBid, expected_bid),
            (Feature::BestAsk, expected_ask),
            (Feature::BestBidQty, fields.best_bid_qty.map(|v| v as f64)),
            (Feature::BestAskQty, fields.best_ask_qty.map(|v| v as f64)),
            (Feature::SpreadFraction, spread),
            (Feature::BookImbalance, imbalance),
        ] {
            assert_eq!(features.get(&feature).copied(), expected, "{feature:?}");
        }
        let mut legacy = serde_json::to_value(fields).unwrap();
        for name in [
            "best_bid",
            "best_ask",
            "best_bid_qty",
            "best_ask_qty",
            "total_buy_quantity",
            "total_sell_quantity",
        ] {
            legacy.as_object_mut().unwrap().remove(name);
        }
        let restored: nivra_core::execution::MarketFields =
            serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(restored).unwrap(), legacy);
        assert!(
            extract_features(&scan, start() + 2001, 2000)
                .rows
                .is_empty()
        );
    }
    // Use a synthetic fresh source clock; only the archived receipt sets elapsed time.
    for (offset, expected_minutes) in [(-60_000, None), (0, Some(0.0)), (60_000, Some(1.0))] {
        for (average, expected_vwap) in [
            (Some(serde_json::json!(52.56)), Some(52.56)),
            (None, None),
            (Some(serde_json::json!(0)), None),
            (Some(serde_json::json!(-1)), None),
            (Some(serde_json::json!("NaN")), None),
            (Some(serde_json::Value::Null), None),
        ] {
            let mut row = documented.clone();
            row["timestamp"] = serde_json::json!(start() + offset - 500);
            row.as_object_mut().unwrap().remove("average_price");
            if let Some(value) = average {
                row["average_price"] = value;
            }
            let rows = serde_json::Map::from_iter([("NSE_EQ:NHPC".into(), row)]);
            let receipt = start() + offset;
            let mut scan = Scanner::default();
            scan_rest_rows(
                &mut scan,
                std::slice::from_ref(&i),
                &rows,
                receipt,
                receipt,
                2000,
                false,
                None,
            )
            .unwrap();
            let fields = scan.samples[&i.key].market_fields.as_ref().unwrap();
            assert_eq!(fields.session_vwap, expected_vwap);
            let features = extract_features(&scan, receipt + 1000, 2000);
            assert_eq!(
                features.rows[0]
                    .features
                    .get(&Feature::SessionVwap)
                    .copied(),
                expected_vwap
            );
            assert_eq!(
                features.rows[0]
                    .features
                    .get(&Feature::MinutesSinceOpen)
                    .copied(),
                expected_minutes
            );
            let mut legacy = serde_json::to_value(fields).unwrap();
            legacy.as_object_mut().unwrap().remove("session_vwap");
            assert!(
                serde_json::from_value::<nivra_core::execution::MarketFields>(legacy)
                    .unwrap()
                    .session_vwap
                    .is_none()
            );
        }
    }
}
#[test]
fn scanner_screens_key_matching_missing_keys_and_whole_market_baseline() {
    let now = start() + 60_000;
    let universe = (0..108).map(instrument).collect::<Vec<_>>();
    let mut scan = Scanner {
        screens: Some(nivra_core::execution::ScreenPolicy::recommended()),
        ..Scanner::default()
    };
    let mut rows = serde_json::Map::new();
    for (n, instrument) in universe.iter().enumerate().take(106) {
        let (price, volume, low, high) = match n {
            101 => (5.0, 1_000_000, 1.0, 10.0),
            102 => (100.0, 0, 80.0, 120.0),
            103 => (119.5, 20000, 80.0, 120.0),
            104 => (80.5, 20000, 80.0, 120.0),
            n if n < 40 => (90.0, 20000, 50.0, 150.0),
            _ => (100.0, 20000, 80.0, 120.0),
        };
        let fields = nivra_core::execution::MarketFields {
            session_vwap: None,
            best_bid: None,
            best_bid_qty: None,
            best_ask: None,
            best_ask_qty: None,
            total_buy_quantity: None,
            total_sell_quantity: None,
            lower_circuit_limit: a("50"),
            upper_circuit_limit: a("150"),
            volume: 20000,
        };
        scan.observe_price(
            now - 31_000,
            &instrument.key,
            Price::parse("100").unwrap(),
            now - 31_000,
            now - 31_000,
            180000,
            Some(&fields),
        )
        .unwrap();
        rows.insert(format!("renamed-provider-row-{n}"),serde_json::json!({"instrument_token":instrument.key,"symbol":if n==105 {"RENAMED"}else{&instrument.symbol},
                "timestamp":now,"last_price":price,"volume":volume,"lower_circuit_limit":low,"upper_circuit_limit":high,
                "depth":{"buy":[{"price":price,"quantity":100}],"sell":[{"price":price,"quantity":100}]}}));
    }
    // A previously seen key disappears; a new key never returns a row.
    let previous = scan.samples.entry(universe[106].key.clone()).or_default();
    previous.seen = now - 20_000;
    previous.reason = "eligible_no_signal".into();
    scan_rest_rows(&mut scan, &universe, &rows, now, now, 180000, false, None).unwrap();
    assert_eq!(scan.samples[&universe[101].key].reason, "below_min_price");
    assert_eq!(
        scan.samples[&universe[102].key].reason,
        "below_min_traded_value"
    );
    assert_eq!(
        scan.samples[&universe[103].key].reason,
        "near_upper_circuit"
    );
    assert_eq!(
        scan.samples[&universe[104].key].reason,
        "near_lower_circuit"
    );
    assert_eq!(
        scan.samples[&universe[105].key].diagnostic.as_deref(),
        Some("symbol_mismatch")
    );
    assert!(
        data::parse_row(
            rows.values()
                .find(|r| r["instrument_token"] == universe[105].key)
                .unwrap(),
            now,
            &universe[105].key,
            &universe[105].symbol,
            universe[105].tick
        )
        .is_ok()
    );
    assert_eq!(scan.samples[&universe[106].key].seen, now - 20_000);
    assert_eq!(scan.samples[&universe[107].key].seen, 0);
    let settings: super::super::super::discovery::Settings =
        serde_json::from_value(serde_json::json!({"max_scan_age_ms":180000,"max_prospects":40}))
            .unwrap();
    let projection = super::super::super::discovery::project(&scan, &universe, &settings, now);
    assert_eq!(
        projection["market_median_bps"], 0.0,
        "101-stock market is flat despite negative top-40"
    );
    assert_eq!(projection["market_sample_count"], 106);
    assert_eq!(projection["fresh_count"], 106);
    assert_eq!(projection["stale_count"], 1);
    assert_eq!(projection["unseen_count"], 1);
    assert_eq!(projection["oldest_fresh_age_ms"], 0);
    assert_eq!(projection["screen_reasons"]["no_quote_returned"], 2);
    assert_eq!(projection["symbol_mismatch_count"], 1);
    assert!(
        projection["prospects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p["symbol"].as_str().unwrap()[1..].parse::<usize>().unwrap() < 40)
    );
    for item in &universe[101..=104] {
        assert!(!Scanner::market_allows_with_policy(
            now,
            item,
            &data::parse_row(
                rows.values()
                    .find(|r| r["instrument_token"] == item.key)
                    .unwrap(),
                now,
                &item.key,
                &item.symbol,
                item.tick
            )
            .unwrap(),
            rows.values()
                .find(|r| r["instrument_token"] == item.key)
                .unwrap(),
            &nivra_core::execution::ScreenPolicy::recommended()
        ));
    }
    let seen = scan.samples[&universe[0].key].seen;
    scan_rest_rows(
        &mut scan,
        &universe,
        &rows,
        now + 1000,
        now + 1000,
        180000,
        true,
        None,
    )
    .unwrap();
    assert_eq!(
        scan.samples[&universe[0].key].seen, seen,
        "metadata cannot renew screening prices"
    );
    assert_eq!(
        scan.samples[&universe[107].key].seen, 0,
        "REST metadata cannot hide missing feed keys"
    );
    // Missing/stale keys never inflate fresh scan age or stay eligible.
    let mut wrong = rows.values().next().unwrap().clone();
    wrong["instrument_token"] = serde_json::json!("NSE_EQ|OTHER");
    assert!(
        data::parse_row(
            &wrong,
            now,
            &universe[0].key,
            &universe[0].symbol,
            universe[0].tick
        )
        .is_err()
    );
}
#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn archive_capture_replays_the_same_session_as_sqlite() -> Result<()> {
    use crate::runtime::{market_archive, paper_replay};
    use serde_json::{Value, json};
    use std::time::{Duration, Instant};

    let tmp = tempfile::tempdir()?;
    let settings = crate::runtime::market_archive::test_settings(tmp.path())?;
    let mut archive_db = market_archive::connect(&settings)?;
    nivra_store::market_archive::init(&mut archive_db)?;
    let before = nivra_store::market_archive::check(&mut archive_db)?;
    drop(archive_db);
    let mut archived = super::super::super::tests::config(&tmp.path().join("archive-on"));
    archived.market_archive = Some(settings.clone());
    let legacy = super::super::super::tests::config(&tmp.path().join("archive-off"));
    let (metadata_batches, screen_batches): (u32, u32) =
        if std::env::var_os("NIVRA_ARCHIVE_SIZE_REPLAY").is_some() {
            (2_062, 2_315)
        } else {
            (3, 3)
        };
    let nonce = millis();
    let instruments = (0..200)
        .map(|i| {
            json!({
                "key":format!("NSE_EQ|SYNTHETIC{nonce}-{i}"), "symbol":format!("SYNTHETIC{i}"),
                "series":"EQ", "tick":"0.05"
            })
        })
        .collect::<Vec<_>>();
    for c in [&archived, &legacy] {
        fs::create_dir_all(&c.directory)?;
        fs::write(
            c.directory.join("wide-universe.json"),
            json!({"session":c.session,"instruments":instruments}).to_string(),
        )?;
    }
    let mut new_book = open_book(&archived)?;
    let mut old_book = open_book(&legacy)?;
    let spool_path = archived.directory.join("archive-spool.sqlite");
    let unavailable_password = tmp.path().join("archive-password.offline");
    if metadata_batches == 3 {
        fs::rename(&settings.password_file, &unavailable_password)?;
    }
    let mut writer =
        market_archive::capture::Writer::start(&archived.directory, tmp.path(), settings)?;
    let mut archived_p = super::super::super::tests::test_portfolio();
    let mut legacy_p = archived_p.clone();
    let base = archived
        .session
        .and_hms_opt(4, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let template: Value = serde_json::from_str(include_str!(
        "../../../../../../fixtures/upstox_full_quote_doc_example.json"
    ))?;
    let quote_body = |count: usize, received: i64| {
        let data: serde_json::Map<String, Value> = instruments
            .iter()
            .take(count)
            .map(|instrument| {
                let key = instrument["key"].as_str().unwrap();
                let mut row = template["data"]["NSE_EQ:NHPC"].clone();
                row["instrument_token"] = json!(key);
                row["symbol"] = instrument["symbol"].clone();
                row["timestamp"] = json!(received);
                row["depth"]["sell"] = json!([{"price":52.10,"quantity":100}]);
                (key.replace('|', ":"), row)
            })
            .collect();
        json!({"status":"success","data":data}).to_string()
    };
    let mut pace_db = (metadata_batches > 3)
        .then(|| market_archive::connect(archived.market_archive.as_ref().unwrap()))
        .transpose()?;
    let mut submitted = 0;
    let mut last = base;
    for i in 0..metadata_batches.max(screen_batches) {
        for (purpose, count) in [
            (
                "scanner_metadata",
                if i < metadata_batches { 200 } else { 0 },
            ),
            ("scanner", if i < screen_batches { 40 } else { 0 }),
        ] {
            if count == 0 {
                continue;
            }
            let received = last + 4_900;
            last = received + 2;
            let executable = purpose == "scanner" && (i % 100 == 0 || i + 1 == screen_batches);
            let purpose = if executable {
                "execution_or_protection"
            } else {
                purpose
            };
            let raw = json!({
                "source":"upstox-readonly-v1","purpose":purpose,"requested_ms":received-1,
                "received_ms":received,"body":quote_body(count, received)
            })
            .to_string();
            if purpose == "scanner_metadata" {
                persist_scanner_metadata(
                    &mut old_book,
                    &legacy,
                    &legacy_p,
                    last,
                    received,
                    raw.clone(),
                    None,
                )?;
                persist_scanner_metadata(
                    &mut new_book,
                    &archived,
                    &archived_p,
                    last,
                    received,
                    raw,
                    Some(&writer),
                )?;
            } else {
                commit_quotes_screened(
                    &mut old_book,
                    &legacy,
                    &mut legacy_p,
                    &BTreeMap::new(),
                    received,
                    raw.as_str().into(),
                    || last,
                    None,
                    false,
                    None,
                )?;
                commit_quotes_screened(
                    &mut new_book,
                    &archived,
                    &mut archived_p,
                    &BTreeMap::new(),
                    received,
                    raw.into(),
                    || last,
                    None,
                    false,
                    Some(ArchivedQuote {
                        writer: &writer,
                        purpose,
                    }),
                )?;
            }
            submitted += 1;
            // Wait for the archive between groups in the accelerated fixture.
            if submitted % 8 == 0
                && let Some(db) = &mut pace_db
            {
                let deadline = Instant::now() + Duration::from_secs(20);
                loop {
                    let receipts: i64 = db.query_one("SELECT count(*) FROM receipts", &[])?.get(0);
                    if receipts >= before.receipts + submitted {
                        break;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "archive did not commit the paced fixture"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    if metadata_batches == 3 {
        let deadline = Instant::now() + Duration::from_secs(20);
        let spool = Connection::open(&spool_path)?;
        let backlog = || {
            spool.query_row("SELECT count(*) FROM batches", [], |row| {
                row.get::<_, i64>(0)
            })
        };
        assert_eq!(backlog()?, submitted);
        fs::rename(
            unavailable_password,
            &archived.market_archive.as_ref().unwrap().password_file,
        )?;
        while backlog()? != 0 {
            assert!(
                Instant::now() < deadline,
                "writer did not drain the spool on reconnect"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    writer.stop();
    assert_eq!(writer.take_dropped(), 0);
    let expected_receipts = i64::from(metadata_batches + screen_batches);
    let mut archive_db = market_archive::connect(archived.market_archive.as_ref().unwrap())?;
    let after = nivra_store::market_archive::check(&mut archive_db)?;
    assert_eq!(after.receipts - before.receipts, expected_receipts);
    assert_eq!(
        after.quote_obs - before.quote_obs,
        i64::from(200 * metadata_batches + 40 * screen_batches)
    );
    let compare = |c: &Config| -> Result<_> {
        let evidence = paper_replay::quote_receipts_bounded(c.quote_source(), c.session, 6, last)?;
        Ok(evidence
            .receipts
            .into_iter()
            .map(|r| (r.at_ms, r.receipt_ms, r.kind, r.observations))
            .collect::<Vec<_>>())
    };
    assert_eq!(compare(&archived)?, compare(&legacy)?);
    let keys = BTreeSet::from([instruments[0]["key"].as_str().unwrap().to_owned()]);
    let query = |c: &Config| {
        paper_replay::candidate_quotes(
            c.quote_source(),
            c.session,
            &keys,
            last - 30_000,
            last,
            10,
            last,
        )
    };
    assert_eq!(query(&archived)?, query(&legacy)?);
    let replay = |c: &Config| {
        paper_replay::execution_tape_bounded(
            c.quote_source(),
            c.session,
            Some(c.previous_session),
            c.allocation().unwrap(),
            6,
            2_000,
            2_000_000,
            last,
        )
    };
    let archived_tape = replay(&archived)?;
    let legacy_tape = replay(&legacy)?;
    assert_eq!(archived_tape.events, legacy_tape.events);
    assert_eq!(archived_tape.manifest, legacy_tape.manifest);
    let references: i64 = new_book.query_row(
        "SELECT count(*) FROM ticks WHERE json_extract(body,'$.kind')='market_archive_quote'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(references, expected_receipts);
    archived_p.closed = true;
    persist(
        &mut new_book,
        &archived,
        &archived_p,
        last + 1,
        None,
        None,
        "session_closed",
    )?;
    legacy_p.closed = true;
    persist(
        &mut old_book,
        &legacy,
        &legacy_p,
        last + 1,
        None,
        None,
        "session_closed",
    )?;
    drop(new_book);
    drop(old_book);
    let size =
        |c: &Config| -> Result<u64> { Ok(fs::metadata(c.directory.join("paper.sqlite"))?.len()) };
    let (without, with) = (size(&legacy)?, size(&archived)?);
    assert!(with < without);
    println!(
        "synthetic replay processed {expected_receipts} receipts without gaps: paper.sqlite off={without} bytes on={with} bytes"
    );

    // The short fixture also verifies a legacy-only receipt is inserted once.
    if metadata_batches == 3 {
        let book_path = legacy.directory.join("paper.sqlite");
        let mut legacy_db = Connection::open(&book_path)?;
        let receipt = last + 2;
        let raw = json!({
            "source":"upstox-readonly-v1","purpose":"scanner_metadata","requested_ms":receipt-1,
            "received_ms":receipt,"body":quote_body(1, receipt)
        })
        .to_string();
        persist_scanner_metadata(
            &mut legacy_db,
            &legacy,
            &legacy_p,
            receipt,
            receipt,
            raw,
            None,
        )?;
        drop(legacy_db);
        let bytes = fs::read(&book_path)?;
        for inserted in [1, 0] {
            let result = market_archive::import_book(&mut archive_db, &legacy.directory, false)?;
            assert_eq!(result["inserted"], inserted);
            assert_eq!(result["verified"], expected_receipts + 1 - inserted);
            assert_eq!(fs::read(&book_path)?, bytes);
        }
        let archive_before = nivra_store::market_archive::check(&mut archive_db)?;
        let changed = Connection::open(&book_path)?;
        changed.execute(
            "UPDATE ticks SET body=json_set(body,'$.requested_ms',receipt-2) WHERE id=1",
            [],
        )?;
        drop(changed);
        assert_eq!(
            market_archive::import_book(&mut archive_db, &legacy.directory, false)
                .unwrap_err()
                .to_string(),
            "archive_receipt_conflict"
        );
        assert_eq!(
            nivra_store::market_archive::check(&mut archive_db)?,
            archive_before
        );
    }
    Ok(())
}
