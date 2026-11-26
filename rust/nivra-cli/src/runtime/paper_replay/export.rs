//! A closed session's quote tape in the paper capture format that
//! `strategy-lab export-features` and `replay-actions` read. Every event is an
//! original archived receipt with its original receipt time; nothing is
//! fetched, interpolated or filled forward.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;

/// The session's original quote receipts, as `manifest.json` and
/// `events.jsonl` in the new directory `output`. With `keys`, only those
/// instruments' observations are kept, and receipts without one are left out.
pub(crate) fn quote_tape(
    c: &Config,
    keys: &BTreeSet<String>,
    output: &Path,
) -> std::result::Result<Value, String> {
    let instruments = session_universe(&c.directory, c.session)?;
    let db = open_quote_journal(&c.directory)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(output)
        .map_err(|_| "quote_tape_output_unavailable".to_string())?;
    let written = write_tape(c, &db, &instruments, keys, output);
    if written.is_err() {
        // Only the directory created above; the book is never touched.
        let _ = fs::remove_dir_all(output);
    }
    written
}

fn write_tape(
    c: &Config,
    db: &Connection,
    instruments: &[Instrument],
    keys: &BTreeSet<String>,
    output: &Path,
) -> std::result::Result<Value, String> {
    let io = |_| "quote_tape_write_failed".to_string();
    let create = |name: &str| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output.join(name))
            .map_err(io)
    };
    let index = instruments
        .iter()
        .map(|instrument| (instrument.key.as_str(), instrument))
        .collect::<BTreeMap<_, _>>();
    let mut events = std::io::BufWriter::new(create("events.jsonl")?);
    let mut hash = Sha256::new();
    let mut statement = db
        .prepare(
            "SELECT at,receipt,body FROM ticks WHERE event IN ('wide_quotes','scanner_metadata') ORDER BY id",
        )
        .map_err(|_| "quote_tape_unavailable".to_string())?;
    let rows = statement
        .query_map([], quote_row)
        .map_err(|_| "quote_tape_unavailable".to_string())?;
    let (mut count, mut observations, mut compacted, mut max_gap_ms) = (0usize, 0usize, 0usize, 0);
    let (mut first, mut last) = (None, None::<i64>);
    let mut seen = BTreeSet::new();
    for row in c.quote_source().rows(rows) {
        let (at, receipt, raw) = row?;
        // Retention keeps only a hash of a compacted body; it is counted,
        // never replayed.
        if raw.contains("\"kind\":\"compacted_quote_receipt\"") {
            compacted += 1;
            continue;
        }
        let mut receipt = archived_receipt(&index, at, receipt, &raw, i64::MAX)?;
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if !keys.is_empty() {
            receipt
                .observations
                .retain(|row| row["key"].as_str().is_some_and(|key| keys.contains(key)));
        }
        if receipt.observations.is_empty() {
            continue;
        }
        if last.is_some_and(|prior| at <= prior) {
            return Err("quote_tape_chronology".into());
        }
        if let Some(prior) = last {
            max_gap_ms = max_gap_ms.max(at - prior);
        }
        first.get_or_insert(at);
        last = Some(at);
        for row in &receipt.observations {
            if let Some(key) = row["key"].as_str() {
                seen.insert(key.to_owned());
            }
        }
        observations += receipt.observations.len();
        count += 1;
        let line = format!(
            "{}\n",
            json!({"at":at,"available_at":receipt.receipt_ms,"kind":receipt.kind.as_str(),
                "observations":receipt.observations})
        );
        hash.update(line.as_bytes());
        events.write_all(line.as_bytes()).map_err(io)?;
    }
    if count == 0 {
        return Err("quote_tape_empty".into());
    }
    events
        .into_inner()
        .map_err(|_| "quote_tape_write_failed".to_string())?
        .sync_all()
        .map_err(io)?;
    let mut gaps = BTreeMap::new();
    let mut statement = db
        .prepare("SELECT body FROM ticks WHERE event=?1")
        .map_err(|_| "quote_tape_unavailable".to_string())?;
    let bodies = statement
        .query_map([super::super::wide::QUOTE_TAPE_GAP_EVENT], |row| {
            row.get::<_, Option<String>>(0)
        })
        .map_err(|_| "quote_tape_unavailable".to_string())?;
    for body in bodies {
        let body = body.map_err(|_| "quote_tape_unavailable".to_string())?;
        let reason = body
            .and_then(|body| serde_json::from_str::<Value>(&body).ok())
            .and_then(|body| body["reason"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "unknown".into());
        *gaps.entry(reason).or_insert(0u64) += 1;
    }
    let events_sha256: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let universe = instruments
        .iter()
        .filter(|instrument| seen.contains(&instrument.key))
        .collect::<Vec<_>>();
    let manifest = serde_json::to_vec(&json!({
        "schema":1,
        "session":c.session,
        "previous_session":c.previous_session,
        "capital":c.allocation().map_err(|_| "positive_paper_capital_required".to_string())?,
        "universe":universe,
        "events_sha256":events_sha256,
        "synthetic":false,
        "max_observation_gap_ms":max_gap_ms.max(1),
        "history_directory":null,
        "paper_costs":c.paper_costs,
        "provenance":{
            "source":"closed_paper_book_quote_journal_read_only",
            "legacy_quote_purpose_unknown":0,
            "compacted_receipts_skipped":compacted,
            "quote_tape_gaps":gaps,
            "previous_session_semantics":"verified_calendar_trading_day",
            "max_observation_gap_ms_semantics":"observed_max_receipt_gap_not_an_engine_tolerance"
        }
    }))
    .map_err(|_| "quote_tape_write_failed".to_string())?;
    let mut file = create("manifest.json")?;
    file.write_all(&manifest).map_err(io)?;
    file.sync_all().map_err(io)?;
    fs::File::open(output)
        .and_then(|directory| directory.sync_all())
        .map_err(io)?;
    Ok(json!({
        "session":c.session,
        "events":count,
        "observations":observations,
        "instruments":universe.len(),
        "first_at_ms":first,
        "last_at_ms":last,
        "compacted_receipts_skipped":compacted,
        "quote_tape_gaps":gaps,
        "events_sha256":events_sha256,
        "manifest_sha256":digest(&manifest),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_session_tape_exports_original_receipts_that_replays_read() {
        let dir = tempfile::tempdir().unwrap();
        let c = super::super::super::tests::config(dir.path());
        fs::create_dir_all(&c.directory).unwrap();
        let instruments = json!([
            {"key":"NSE_EQ|INE000000001","symbol":"AAA","series":"EQ","tick":"0.05"},
            {"key":"NSE_EQ|INF000000002","symbol":"BBB","series":"EQ","tick":"0.05"}
        ]);
        fs::write(
            c.directory.join("wide-universe.json"),
            json!({"session":c.session,"instruments":instruments}).to_string(),
        )
        .unwrap();
        let open = c
            .session
            .and_hms_opt(4, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let row = |key: &str, at: i64, bid: f64| {
            json!({"instrument_token":key,"timestamp":at,
                "depth":{"buy":[{"price":bid,"quantity":100}],"sell":[{"price":bid + 0.05,"quantity":100}]},
                "lower_circuit_limit":80,"upper_circuit_limit":120,"volume":1_000})
        };
        let db = super::super::super::open_book(&c).unwrap();
        for (offset, purpose, rows) in [
            (
                0,
                "scanner_metadata",
                vec![row("NSE_EQ|INE000000001", open, 100.0)],
            ),
            (
                8_000,
                "execution_or_protection",
                vec![
                    row("NSE_EQ|INE000000001", open + 8_000, 101.0),
                    row("NSE_EQ|INF000000002", open + 8_000, 200.0),
                ],
            ),
            (
                16_000,
                "scanner_metadata",
                vec![row("NSE_EQ|INF000000002", open + 16_000, 201.0)],
            ),
        ] {
            let at = open + offset;
            let data: serde_json::Map<String, Value> = rows
                .into_iter()
                .map(|row| {
                    (
                        row["instrument_token"].as_str().unwrap().replace('|', ":"),
                        row,
                    )
                })
                .collect();
            let body = json!({"status":"success","data":data}).to_string();
            let archive = json!({"source":"upstox-readonly-v1","purpose":purpose,
                "requested_ms":at,"received_ms":at,"body":body});
            db.execute(
                "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,?1,?2,?3)",
                rusqlite::params![
                    at,
                    archive.to_string(),
                    if purpose == "scanner_metadata" {
                        purpose
                    } else {
                        "wide_quotes"
                    }
                ],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,NULL,?2,?3)",
            rusqlite::params![
                open + 9_000,
                json!({"reason":"depth_batch_full","watched":3,"dropped":1}).to_string(),
                super::super::super::wide::QUOTE_TAPE_GAP_EVENT
            ],
        )
        .unwrap();
        drop(db);

        let keys = BTreeSet::from(["NSE_EQ|INE000000001".to_owned()]);
        let output = dir.path().join("tape");
        let summary = quote_tape(&c, &keys, &output).unwrap();
        assert_eq!(
            summary["events"], 2,
            "the receipt without the key is left out"
        );
        assert_eq!(summary["observations"], 2);
        assert_eq!(summary["quote_tape_gaps"]["depth_batch_full"], 1);
        let manifest = fs::read(output.join("manifest.json")).unwrap();
        let events = fs::read(output.join("events.jsonl")).unwrap();
        assert_eq!(summary["events_sha256"], digest(&events));
        let snapshots =
            nivra_cli::strategy_archive::export_features(&manifest, &events, 2_000).unwrap();
        assert!(!snapshots.is_empty(), "export-features reads the tape");
        let first: Value =
            serde_json::from_slice(events.split(|b| *b == b'\n').next().unwrap()).unwrap();
        assert_eq!(
            first["available_at"], open,
            "the original receipt time is kept"
        );

        assert_eq!(
            quote_tape(&c, &keys, &output).unwrap_err(),
            "quote_tape_output_unavailable",
            "an export never replaces an earlier one"
        );

        let check_db = super::super::super::open_book(&c).unwrap();
        let idx_count: i64 = check_db
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='index' AND name IN ('ticks_event_receipt_idx','ticks_event_at_idx')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(idx_count, 2);
        drop(check_db);

        let in_window = super::candidate_quotes(
            c.quote_source(),
            c.session,
            &keys,
            open + 8_000,
            open + 8_000,
            10,
            open + 16_000,
        )
        .unwrap();
        assert_eq!(in_window["coverage"], "quotes_present");
        assert_eq!(in_window["end_of_evidence_ms"], open + 16_000);
        assert_eq!(in_window["quotes"].as_array().unwrap().len(), 1);
        assert_eq!(in_window["quotes"][0]["receipt_ms"], open + 8_000);

        let capped = super::candidate_quotes(
            c.quote_source(),
            c.session,
            &keys,
            open,
            open + 16_000,
            1,
            open + 16_000,
        )
        .unwrap();
        assert_eq!(capped["truncated"], true);
        assert_eq!(capped["quotes"].as_array().unwrap().len(), 1);

        // A later journal event can carry a receipt in the requested range.
        // It must not take a slot from a replay bounded by the host's clock.
        let db = super::super::super::open_book(&c).unwrap();
        db.execute(
            "INSERT INTO ticks(at,receipt,body,event)
             SELECT ?1,?2,body,event FROM ticks WHERE receipt=?3 AND event='wide_quotes'",
            rusqlite::params![open + 17_000, open + 9_000, open + 8_000],
        )
        .unwrap();
        drop(db);
        let bounded = super::candidate_quotes_window_with(
            c.quote_source(),
            c.session,
            &keys,
            open + 8_000,
            open + 9_000,
            1,
            open + 16_000,
            &[QuotePurposeV1::Protection],
            true,
            false,
        )
        .unwrap();
        assert_eq!(bounded["quotes"][0]["receipt_ms"], open + 8_000);
        assert_eq!(bounded["truncated"], false);

        let missing = super::candidate_quotes(
            c.quote_source(),
            c.session,
            &keys,
            open + 2_000,
            open + 3_000,
            10,
            open + 16_000,
        )
        .unwrap();
        assert_eq!(missing["coverage"], "missing_quotes_in_window");

        let ended = super::candidate_quotes(
            c.quote_source(),
            c.session,
            &keys,
            open + 20_000,
            open + 30_000,
            10,
            open + 16_000,
        )
        .unwrap();
        assert_eq!(ended["coverage"], "source_ended_before_window");
    }
}
