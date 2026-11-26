use super::{ArchiveClient, Result, normalize};
use nivra_store::market_archive::{QuoteBodies, import_quotes, quote_reference};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};
use serde_json::{Value, json};
use std::path::Path;

/// Import exact quote receipts from a closed book. The archive commits each
/// receipt before its SQLite row can become a reference.
pub(in crate::runtime) fn import(
    db: &mut ArchiveClient,
    directory: &Path,
    rewrite: bool,
) -> Result<Value> {
    let path = directory.join("paper.sqlite");
    let snapshot = crate::runtime::open_book_immutable(&path)?;
    let mut book = if rewrite {
        drop(snapshot);
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(std::time::Duration::ZERO)?;
        connection
    } else {
        snapshot
    };
    let tx = book.transaction_with_behavior(if rewrite {
        TransactionBehavior::Immediate
    } else {
        TransactionBehavior::Deferred
    })?;
    let (binding, state): (String, String) =
        tx.query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let portfolio: nivra_core::execution::Portfolio = serde_json::from_str(&state)?;
    if !portfolio.closed {
        return Err("archive_import_requires_closed_book".into());
    }
    let mut inserted = 0;
    let mut verified = 0;
    let mut compacted = 0;
    let mut updates = Vec::new();
    let mut gaps = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT id, at, receipt, body, event FROM ticks
             WHERE event IN ('wide_quotes','scanner_metadata','research_quotes','archive_quote_gap') ORDER BY id",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let raw: String = row.get(3)?;
            let value: Value = serde_json::from_str(&raw)?;
            let event: String = row.get(4)?;
            if event == "archive_quote_gap" || value["kind"] == "compacted_quote_receipt" {
                compacted += 1;
                if rewrite && event != "archive_quote_gap" {
                    gaps.push((row.get::<_, i64>(0)?, json!({"sha256":value["sha256"]})));
                }
                continue;
            }
            let receipt = row
                .get::<_, Option<i64>>(2)?
                .ok_or("archive_receipt_time_invalid")?;
            let is_reference = value["kind"] == "market_archive_quote";
            let raw = if is_reference {
                QuoteBodies::read(db, &raw, Some(receipt))?
            } else {
                raw
            };
            let batch = normalize::capture_quotes(&raw)?;
            if batch.receipt.received_ms != receipt {
                return Err("archive_receipt_conflict".into());
            }
            let id: i64 = row.get(0)?;
            if import_quotes(db, &batch, &binding, id, row.get(1)?)? {
                inserted += 1;
            } else {
                verified += 1;
            }
            if rewrite && !is_reference {
                let purpose = value["purpose"]
                    .as_str()
                    .ok_or("archive_quote_purpose_invalid")?;
                updates.push((id, quote_reference(purpose, &raw)));
            }
        }
    }
    for (id, reference) in &updates {
        tx.execute(
            "UPDATE ticks SET body=?1 WHERE id=?2",
            params![reference, id],
        )?;
    }
    for (id, body) in &gaps {
        tx.execute(
            "UPDATE ticks SET event='archive_quote_gap', body=?1 WHERE id=?2",
            params![body.to_string(), id],
        )?;
    }
    tx.commit()?;
    if rewrite && !(updates.is_empty() && gaps.is_empty()) {
        book.execute_batch("VACUUM")?;
    }
    Ok(
        json!({"inserted": inserted, "verified": verified, "compacted": compacted,
              "rewritten": updates.len()}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::millis;
    use std::{collections::BTreeSet, fs};

    #[test]
    #[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
    fn archive_import_and_rewrite_preserve_replay_and_book_identity() -> Result<()> {
        use crate::runtime::paper_replay;
        use std::os::unix::fs::MetadataExt;

        let tmp = tempfile::tempdir()?;
        let settings = super::super::test_settings(tmp.path())?;
        let mut archive_db = super::super::connect(&settings)?;
        nivra_store::market_archive::init(&mut archive_db)?;

        let sessions_root = tmp.path().join("sessions");
        let old_dir = sessions_root.join("2026-10-20");
        fs::create_dir_all(&old_dir)?;

        let mut old_cfg = crate::runtime::tests::config(&old_dir);
        old_cfg.session = "2026-10-20".parse()?;
        old_cfg.previous_session = "2026-10-19".parse()?;
        old_cfg.market_archive = Some(settings.clone());

        let nonce = millis();
        let instruments = (0..40)
            .map(|i| {
                json!({
                    "key": format!("NSE_EQ|SYNTHETIC{nonce}-{i}"),
                    "symbol": format!("SYNTHETIC{i}"),
                    "series": "EQ",
                    "tick": "0.05"
                })
            })
            .collect::<Vec<_>>();
        fs::write(
            old_dir.join("wide-universe.json"),
            json!({"session": old_cfg.session, "instruments": instruments}).to_string(),
        )?;

        let template: Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/upstox_full_quote_doc_example.json"
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
                    row["depth"]["sell"] = json!([{"price": 52.10, "quantity": 100}]);
                    (key.replace('|', ":"), row)
                })
                .collect();
            json!({"status": "success", "data": data}).to_string()
        };

        let base = old_cfg
            .session
            .and_hms_opt(4, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            + (nonce % 1_000_000);
        let fill_at = base + 20 * 10_000 + 5_000;
        let fill_key = instruments[0]["key"].as_str().unwrap().to_owned();

        let mut portfolio = crate::runtime::tests::test_portfolio();
        portfolio.closed = true;
        portfolio.fills.push(nivra_core::execution::Fill {
            at: fill_at,
            instrument: nivra_core::execution::Instrument {
                key: fill_key.clone(),
                symbol: "SYNTHETIC0".into(),
                series: "EQ".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
            },
            units: 5,
            buy: true,
            price: nivra_core::finance::Price::parse("52.10").unwrap(),
            fee: nivra_core::finance::Amount::ZERO,
            expected_price: None,
            slippage_bps: None,
            decision_id: None,
            costs: None,
        });
        portfolio.fills.push(nivra_core::execution::Fill {
            at: fill_at + 100,
            instrument: nivra_core::execution::Instrument {
                key: fill_key.clone(),
                symbol: "SYNTHETIC0".into(),
                series: "EQ".into(),
                tick: nivra_core::finance::Price::parse("0.05").unwrap(),
            },
            units: 5,
            buy: false,
            price: nivra_core::finance::Price::parse("52.20").unwrap(),
            fee: nivra_core::finance::Amount::ZERO,
            expected_price: None,
            slippage_bps: None,
            decision_id: None,
            costs: None,
        });
        let binding = crate::runtime::book_binding(&old_cfg)?;
        let state_body = serde_json::to_string(&portfolio)?;

        let db = crate::runtime::open_book(&old_cfg)?;
        db.execute(
            "INSERT INTO state VALUES(1, ?1, ?2)",
            rusqlite::params![binding, state_body],
        )?;
        let mut last = base;
        let mut total_quote_ticks = 0_i64;
        for i in 0..20_i64 {
            let receipt = base + i * 10_000;
            last = receipt + 2;
            let raw = json!({
                "source": "upstox-readonly-v1",
                "purpose": "scanner_metadata",
                "requested_ms": receipt - 1,
                "received_ms": receipt,
                "body": quote_body(40, receipt)
            })
            .to_string();
            db.execute(
                "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'scanner_metadata')",
                rusqlite::params![last, receipt, raw],
            )?;
            total_quote_ticks += 1;

            let scan_receipt = receipt + 4_000;
            last = scan_receipt + 2;
            let scan_raw = json!({
                "source": "upstox-readonly-v1",
                "purpose": "scanner",
                "requested_ms": scan_receipt - 1,
                "received_ms": scan_receipt,
                "body": quote_body(25, scan_receipt)
            })
            .to_string();
            db.execute(
                "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'wide_quotes')",
                rusqlite::params![last, scan_receipt, scan_raw],
            )?;
            total_quote_ticks += 1;
        }
        // One executable quote tick cited by the fill in `portfolio.fills`.
        let fill_raw = json!({
            "source": "upstox-readonly-v1",
            "purpose": "execution_or_protection",
            "requested_ms": fill_at - 1,
            "received_ms": fill_at,
            "body": quote_body(5, fill_at)
        })
        .to_string();
        db.execute(
            "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'wide_quotes')",
            rusqlite::params![fill_at, fill_at, fill_raw],
        )?;
        let fill_tick_id = db.last_insert_rowid();
        total_quote_ticks += 1;
        last = last.max(fill_at + 200);

        // Research-observation ticks are bulk-archived too when market_archive is configured.
        for i in 0..2_i64 {
            let receipt = last + 1_000 + i * 1_000;
            last = receipt + 2;
            let res_raw = json!({
                "source": "upstox-readonly-v1",
                "purpose": "research_observation",
                "requested_ms": receipt - 1,
                "received_ms": receipt,
                "body": quote_body(10, receipt)
            })
            .to_string();
            db.execute(
                "INSERT INTO ticks(at, receipt, body, event) VALUES(?1, ?2, ?3, 'research_quotes')",
                rusqlite::params![last, receipt, res_raw],
            )?;
            total_quote_ticks += 1;
        }
        db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        drop(db);

        let db_path = old_dir.join("paper.sqlite");
        let meta = fs::metadata(&db_path)?;
        fs::write(
            old_dir.join("book-identity.json"),
            serde_json::to_vec(&json!({
                "device": meta.dev(),
                "inode": meta.ino(),
                "binding": binding
            }))?,
        )?;
        let before_len = fs::metadata(&db_path)?.len();
        let keys = BTreeSet::from([fill_key]);
        let before_candidates = paper_replay::candidate_quotes(
            paper_replay::QuoteSource::new(&old_dir, None),
            old_cfg.session,
            &keys,
            base,
            last,
            200,
            last,
        )?;
        let before_receipts = paper_replay::quote_receipts_bounded(
            paper_replay::QuoteSource::new(&old_dir, None),
            old_cfg.session,
            100,
            last,
        )?
        .receipts
        .into_iter()
        .map(|r| (r.at_ms, r.receipt_ms, r.kind, r.observations))
        .collect::<Vec<_>>();

        let inline = paper_replay::quote_receipts_bounded(
            old_cfg.quote_source(),
            old_cfg.session,
            100,
            last,
        );
        assert_eq!(
            inline.err().as_deref(),
            Some("archive_quote_reference_invalid")
        );

        let imported = import(&mut archive_db, &old_dir, false)?;
        assert_eq!(imported["inserted"], total_quote_ticks);

        assert_eq!(fs::metadata(&db_path)?.len(), before_len);
        let applied = import(&mut archive_db, &old_dir, true)?;
        assert_eq!(applied["rewritten"], total_quote_ticks);
        let after_len = fs::metadata(&db_path)?.len();
        assert!(after_len < before_len);
        println!(
            "fixture book size: before={before_len} bytes after={after_len} bytes rewritten={}",
            applied["rewritten"]
        );

        let db = rusqlite::Connection::open(&db_path)?;
        let fill_reference: String =
            db.query_row("SELECT body FROM ticks WHERE id=?1", [fill_tick_id], |r| {
                r.get(0)
            })?;
        assert_eq!(
            fill_reference,
            quote_reference("execution_or_protection", &fill_raw)
        );
        let inline_bulk: i64 = db.query_row(
            "SELECT count(*) FROM ticks
             WHERE event IN ('wide_quotes', 'scanner_metadata', 'research_quotes')
               AND body NOT LIKE '{\"kind\":\"market_archive_quote\"%'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(inline_bulk, 0);
        let (after_binding, after_state): (String, String) =
            db.query_row("SELECT binding, body FROM state WHERE id=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        assert_eq!(after_binding, binding);
        assert_eq!(after_state, state_body);
        drop(db);

        let anchor: Value = serde_json::from_slice(&fs::read(old_dir.join("book-identity.json"))?)?;
        let after_meta = fs::metadata(&db_path)?;
        assert_eq!(anchor["device"], json!(after_meta.dev()));
        assert_eq!(anchor["inode"], json!(after_meta.ino()));

        let after_candidates = paper_replay::candidate_quotes(
            paper_replay::QuoteSource::new(&old_dir, Some(&settings)),
            old_cfg.session,
            &keys,
            base,
            last,
            200,
            last,
        )?;
        assert_eq!(after_candidates, before_candidates);

        let after_receipts = paper_replay::quote_receipts_bounded(
            paper_replay::QuoteSource::new(&old_dir, Some(&settings)),
            old_cfg.session,
            100,
            last,
        )?
        .receipts
        .into_iter()
        .map(|r| (r.at_ms, r.receipt_ms, r.kind, r.observations))
        .collect::<Vec<_>>();
        assert_eq!(after_receipts, before_receipts);

        let reimported = import(&mut archive_db, &old_dir, false)?;
        assert_eq!(reimported["inserted"], 0);
        assert_eq!(reimported["verified"], total_quote_ticks);

        let lost_at = last + 100;
        let lost_hash = "a".repeat(64);
        let db = Connection::open(&db_path)?;
        db.execute(
            "INSERT INTO ticks(at, receipt, body, event) VALUES(?1,?1,?2,'wide_quotes')",
            params![
                lost_at,
                json!({"kind":"compacted_quote_receipt","sha256":lost_hash}).to_string()
            ],
        )?;
        drop(db);
        let second = import(&mut archive_db, &old_dir, true)?;
        assert_eq!(second["rewritten"], 0);
        assert_eq!(second["compacted"], 1);
        let db = Connection::open(&db_path)?;
        let gap: (i64, String, String) = db.query_row(
            "SELECT receipt, event, json_extract(body,'$.sha256') FROM ticks WHERE at=?1",
            [lost_at],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(gap, (lost_at, "archive_quote_gap".into(), lost_hash));
        Ok(())
    }
}
