use chrono::NaiveDate;
use nivra_store::market_archive::{
    CandleBar, CandleBatch, CandleUnit, FailedAttempt, QuoteBatch, QuoteObservation, QuotePurpose,
    Receipt, check, init, write_candles, write_failed_attempt, write_quotes,
};
use postgres::{Client, NoTls, error::SqlState};
use serde_json::{Value, json};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn timescale_archive_round_trip() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("NIVRA_ARCHIVE_TEST_URL")?;
    let mut db = Client::connect(&url, NoTls)?;
    init(&mut db)?;
    let before = check(&mut db)?;
    init(&mut db)?;
    assert_eq!(check(&mut db)?, before);

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let now = i64::try_from(nonce / 1_000_000)?;
    let instrument = format!("SYNTHETIC|{nonce}-0");
    let before_body = format!("synthetic receipt {nonce}");
    let mut single = QuoteBatch {
        receipt: Receipt::new(now - 10, now, before_body.as_bytes()),
        purpose: QuotePurpose::Scanner,
        observations: vec![observation(instrument.clone(), now)],
    };
    assert!(write_quotes(&mut db, &single)?);
    assert!(!write_quotes(&mut db, &single)?);
    let stored: Vec<u8> = db
        .query_one(
            "SELECT body FROM receipts WHERE sha256 = $1",
            &[&single.receipt.sha256.as_slice()],
        )?
        .get(0);
    assert_eq!(
        zstd::bulk::decompress(&stored, before_body.len())?,
        before_body.as_bytes()
    );

    single.receipt.requested_ms += 1;
    assert_eq!(
        write_quotes(&mut db, &single).unwrap_err().to_string(),
        "archive_receipt_conflict"
    );
    single.receipt.body = b"conflicting bytes for the same hash";
    assert_eq!(
        write_quotes(&mut db, &single).unwrap_err().to_string(),
        "archive_receipt_conflict"
    );
    println!("init x2, verified retry, and receipt conflict passed");

    let observations: Vec<_> = (0..200)
        .map(|i| observation(format!("SYNTHETIC|{nonce}-{i}"), now))
        .collect();
    let example: Value = serde_json::from_str(include_str!(
        "../../../fixtures/upstox_full_quote_doc_example.json"
    ))?;
    let rows = observations
        .iter()
        .map(|observation| {
            let mut row = example["data"]["NSE_EQ:NHPC"].clone();
            row["instrument_token"] = json!(observation.instrument_key);
            row["timestamp"] = json!(observation.source_ms);
            (observation.instrument_key.clone(), row)
        })
        .collect::<serde_json::Map<_, _>>();
    let batch_body = json!({
        "source": "upstox-readonly-v1",
        "purpose": "scanner_metadata",
        "requested_ms": now + 1,
        "received_ms": now + 2,
        "body": json!({"status": "success", "data": rows}).to_string(),
    })
    .to_string();
    let mut batch = QuoteBatch {
        receipt: Receipt::new(now + 1, now + 2, batch_body.as_bytes()),
        purpose: QuotePurpose::ScannerMetadata,
        observations,
    };
    let start = Instant::now();
    assert!(write_quotes(&mut db, &batch)?);
    let elapsed = start.elapsed();
    let after = check(&mut db)?;
    assert_eq!(after.schema_version, 1);
    assert_eq!(after.receipts - before.receipts, 2);
    assert_eq!(after.instruments - before.instruments, 200);
    assert_eq!(after.quote_obs - before.quote_obs, 201);
    println!(
        "200-quote transaction: {:.3} ms (1 receipt and 200 quotes committed)",
        elapsed.as_secs_f64() * 1000.0
    );

    let bad_body = format!("synthetic invalid quote receipt {nonce}");
    batch.receipt = Receipt::new(now + 1, now + 2, bad_body.as_bytes());
    batch.observations[0].ask = Some(1);
    batch.observations[0].ask_qty = Some(1);
    assert!(write_quotes(&mut db, &batch).is_err());
    assert_eq!(check(&mut db)?, after);

    let day = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
    let bar = || CandleBar {
        day,
        bar_ms: 1792089000000,
        o: 100,
        h: 110,
        l: 90,
        c: 101,
        volume: 200,
        oi: 0,
    };
    let daily_body = format!("synthetic daily receipt {nonce}");
    let mut candles = CandleBatch {
        receipt: Receipt::new(now - 10, now, daily_body.as_bytes()),
        instrument_key: instrument.clone(),
        unit: CandleUnit::Daily,
        start: day,
        end: day.succ_opt().unwrap(),
        bars: vec![bar()],
    };
    assert!(write_candles(&mut db, &candles)?);
    let overlap_body = format!("synthetic daily overlap {nonce}");
    candles.receipt = Receipt::new(now + 1, now + 2, overlap_body.as_bytes());
    assert!(write_candles(&mut db, &candles)?);
    let conflict_body = format!("synthetic daily conflict {nonce}");
    candles.receipt = Receipt::new(now + 3, now + 4, conflict_body.as_bytes());
    candles.bars[0].volume += 1;
    assert_eq!(
        write_candles(&mut db, &candles).unwrap_err().to_string(),
        "archive_candle_conflict"
    );

    let minute_body = format!("synthetic minute receipt {nonce}");
    candles.receipt = Receipt::new(now + 1, now + 2, minute_body.as_bytes());
    candles.unit = CandleUnit::Minute;
    candles.bars[0] = bar();
    assert!(write_candles(&mut db, &candles)?);
    write_failed_attempt(
        &mut db,
        &instrument,
        CandleUnit::Minute,
        day,
        now + 10,
        FailedAttempt::Retryable,
    )?;
    write_failed_attempt(
        &mut db,
        &instrument,
        CandleUnit::Minute,
        day.succ_opt().unwrap(),
        now + 10,
        FailedAttempt::Unavailable,
    )?;

    let coverage: Vec<(String, Option<i64>)> = db
        .query(
            "SELECT status, unavailable_ms FROM coverage c JOIN instruments i ON i.id = c.instrument_id
         WHERE i.key = $1 ORDER BY unit, day",
            &[&instrument],
        )?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    assert_eq!(
        coverage,
        [
            ("observed".into(), None),
            ("empty".into(), None),
            ("observed".into(), None),
            ("empty".into(), Some(now + 10)),
        ]
    );

    let final_counts = check(&mut db)?;
    assert_eq!(final_counts.receipts - after.receipts, 3);
    assert_eq!(final_counts.daily_bars - before.daily_bars, 1);
    assert_eq!(final_counts.minute_bars - before.minute_bars, 1);
    assert_eq!(final_counts.coverage - before.coverage, 4);
    for (table, field) in [
        ("receipts", "requested_ms"),
        ("quote_obs", "ltp"),
        ("daily_bars", "o"),
        ("minute_bars", "o"),
    ] {
        for sql in [
            format!("UPDATE {table} SET {field} = {field}"),
            format!("DELETE FROM {table}"),
        ] {
            assert_eq!(
                db.execute(&sql, &[]).unwrap_err().code(),
                Some(&SqlState::OBJECT_NOT_IN_PREREQUISITE_STATE)
            );
        }
    }
    println!("quote rollback, overlapping candles, coverage, and append-only triggers passed");
    Ok(())
}

fn observation(instrument_key: String, source_ms: i64) -> QuoteObservation {
    QuoteObservation {
        instrument_key,
        source_ms,
        ltp: 5205,
        bid: Some(5205),
        ask: None,
        bid_qty: Some(6917),
        ask_qty: None,
        volume: 24123697,
        lower_circuit: 4250,
        upper_circuit: 6370,
    }
}
