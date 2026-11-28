//! A reviewed fresh book beside a closed, flat predecessor in the same
//! sessions root. The predecessor's book, report and strategy library stay in
//! place; only the session pointer moves aside, so the next start opens a new
//! book funded by the config's sleeves.
use super::*;

const RECEIPT: &str = "fresh-book.json";
const REASON: &str = "operator_fresh_book_closed_flat_predecessor";

/// A fresh-book receipt waiting for the next session to open its SQLite book.
pub(in crate::runtime) struct PendingFreshBook {
    pub predecessor_session: NaiveDate,
    pub predecessor_config: Config,
}

fn moved_pointer_name(session: NaiveDate) -> String {
    format!("current.before-{session}.json")
}

/// Inspect `c.directory/fresh-book.json` and the moved `current.before-<session>.json`
/// pointer when `current.json` was moved aside for `c.session`.
pub(in crate::runtime) fn pending_for(c: &Config) -> Result<Option<PendingFreshBook>> {
    let receipt_path = c.directory.join(RECEIPT);
    if !receipt_path.try_exists()? {
        return Ok(None);
    }
    let bytes = crate::read_bounded(&receipt_path, 32_768)
        .map_err(|_| Fail::new("fresh_book_receipt_unreadable"))?;
    let receipt: Value =
        serde_json::from_slice(&bytes).map_err(|_| Fail::new("fresh_book_receipt_invalid"))?;
    let session: NaiveDate = serde_json::from_value(receipt["session"].clone())
        .map_err(|_| Fail::new("fresh_book_receipt_invalid"))?;
    let predecessor_session: NaiveDate =
        serde_json::from_value(receipt["predecessor"]["source_session"].clone())
            .map_err(|_| Fail::new("fresh_book_receipt_invalid"))?;
    if receipt["reason"] != REASON || session != c.session || predecessor_session >= c.session {
        return Err(Fail::new("fresh_book_receipt_invalid").into());
    }
    let root = root(c)?;
    let moved = root.join(moved_pointer_name(c.session));
    if !moved.try_exists()? {
        return Err(Fail::new("fresh_book_pointer_missing").into());
    }
    let moved_bytes =
        crate::read_bounded(&moved, 4096).map_err(|_| Fail::new("fresh_book_pointer_invalid"))?;
    let moved_json: Value = serde_json::from_slice(&moved_bytes)
        .map_err(|_| Fail::new("fresh_book_pointer_invalid"))?;
    let moved_session: NaiveDate = serde_json::from_value(moved_json["session"].clone())
        .map_err(|_| Fail::new("fresh_book_pointer_invalid"))?;
    if moved_json["account"] != c.account || moved_session != predecessor_session {
        return Err(Fail::new("fresh_book_pointer_invalid").into());
    }
    let mut predecessor_config = c.clone();
    predecessor_config.session = predecessor_session;
    predecessor_config.previous_session = predecessor_config.next_sessions(predecessor_session)?.1;
    predecessor_config.directory = root.join(predecessor_session.to_string());
    Ok(Some(PendingFreshBook {
        predecessor_session,
        predecessor_config,
    }))
}

/// Detect whether `current.json` was moved aside by a fresh-book receipt for a
/// newer session than `c.session`.
pub(in crate::runtime) fn pending_other_session(c: &Config) -> Result<Option<NaiveDate>> {
    let root = root(c)?;
    if !root.is_dir() {
        return Ok(None);
    }
    let mut pending = None;
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(date_str) = name
            .strip_prefix("current.before-")
            .and_then(|rest| rest.strip_suffix(".json"))
        else {
            continue;
        };
        let Ok(session) = date_str.parse::<NaiveDate>() else {
            continue;
        };
        if session <= c.session {
            continue;
        }
        let receipt_path = root.join(session.to_string()).join(RECEIPT);
        if !receipt_path.try_exists()? {
            continue;
        }
        let Ok(moved_bytes) = crate::read_bounded(&entry.path(), 4096) else {
            continue;
        };
        let Ok(moved_json) = serde_json::from_slice::<Value>(&moved_bytes) else {
            continue;
        };
        if moved_json["account"] == c.account {
            pending = Some(pending.map_or(session, |prev: NaiveDate| prev.max(session)));
        }
    }
    Ok(pending)
}

/// What the predecessor's stored state says, read as plain JSON so a book of
/// an older format can be inspected without being opened as a portfolio.
fn predecessor(root: &Path) -> Result<(NaiveDate, Value)> {
    let pointer: Value = serde_json::from_slice(
        &crate::read_bounded(&root.join("current.json"), 4096)
            .map_err(|_| Fail::new("fresh_book_pointer_missing"))?,
    )?;
    let session: NaiveDate = serde_json::from_value(pointer["session"].clone())
        .map_err(|_| Fail::new("fresh_book_pointer_invalid"))?;
    let db = super::super::open_book_current_read_only(
        &root.join(session.to_string()).join("paper.sqlite"),
    )?;
    let body: String = db.query_row("SELECT body FROM state WHERE id=1", [], |r| r.get(0))?;
    let state: Value = serde_json::from_str(&body)?;
    if state["closed"] != true
        || state["halted"] != false
        || state["positions"].as_object().is_none_or(|p| !p.is_empty())
    {
        return Err(Fail::new("fresh_book_requires_closed_flat_unhalted_predecessor").into());
    }
    Ok((
        session,
        json!({"source_session":session,"source_state_sha256":digest(body.as_bytes()),
            "source_cash":state["cash"],"source_fills":state["fills"].as_array().map_or(0, Vec::len)}),
    ))
}

fn open_fresh(c: &Config, confirmed: bool, now: i64) -> Result<Value> {
    if !confirmed {
        return Err(Fail::new("fresh_book_confirm_paper_review_required").into());
    }
    c.validate()?;
    if LIVE_ENABLED {
        return Err(Fail::new("fresh_book_live_enabled").into());
    }
    let root = root(c)?;
    let _root_lock = controller_lock(c)?;
    let target = c.directory.clone();
    if target != root.join(c.session.to_string()) {
        return Err(Fail::new("fresh_book_directory_must_be_dated_session").into());
    }
    let receipt_path = target.join(RECEIPT);
    let moved = root.join(format!("current.before-{}.json", c.session));
    if receipt_path.try_exists()? && moved.try_exists()? && !root.join("current.json").exists() {
        // Already done: the pointer moved after the receipt was written.
        return Ok(serde_json::from_str(&private(&receipt_path)?)?);
    }
    let (source, evidence) = predecessor(&root)?;
    if source >= c.session {
        return Err(Fail::new("fresh_book_session_not_after_predecessor").into());
    }
    let source_lock = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(root.join(source.to_string()).join("controller.lock"))?;
    source_lock.try_lock_exclusive()?;
    let _source_lock = AdvisoryLock::new(source_lock);
    if target.exists() {
        let names: Vec<_> = fs::read_dir(&target)?.collect::<std::io::Result<Vec<_>>>()?;
        if names.iter().any(|e| e.file_name() != RECEIPT) {
            return Err(Fail::new("fresh_book_target_not_empty").into());
        }
    } else {
        fs::create_dir(&target)?;
    }
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700))?;
    let receipt = json!({"reason":REASON,"accepted_at_ms":now,"session":c.session,
        "capital":c.allocation()?,"sleeves":c.sleeves,"predecessor":evidence,
        "original_state_preserved":true,"live_enabled":false});
    if receipt_path.try_exists()? {
        let existing: Value = serde_json::from_str(&private(&receipt_path)?)?;
        if existing["predecessor"] != receipt["predecessor"]
            || existing["sleeves"] != receipt["sleeves"]
        {
            return Err(Fail::new("fresh_book_receipt_changed").into());
        }
    } else {
        // A single append-only receipt: never truncate or replace it.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&receipt_path)?;
        std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(&receipt)?)?;
        file.sync_all()?;
        fs::File::open(&target)?.sync_all()?;
    }
    fs::rename(root.join("current.json"), &moved)?;
    fs::File::open(&root)?.sync_all()?;
    Ok(serde_json::from_str(&private(&receipt_path)?)?)
}

/// `nivra rollover --open-fresh-book`: CONFIG names the new session.
pub fn open_fresh_entry(path: &Path, confirmed: bool) -> ExitCode {
    let result = (|| -> Result<Value> {
        let c: Config = serde_json::from_slice(&fs::read(path)?)?;
        open_fresh(&c, confirmed, millis())
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "fresh_book_refused_original_state_preserved: {}",
                failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old_book(root: &Path, session: &str, body: &str) {
        let dir = root.join(session);
        fs::create_dir_all(&dir).unwrap();
        let _ = fs::remove_file(dir.join("paper.sqlite"));
        let db = Connection::open(dir.join("paper.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY, binding TEXT, body TEXT);")
            .unwrap();
        db.execute("INSERT INTO state VALUES(1,'old',?)", [body])
            .unwrap();
        fs::write(
            root.join("current.json"),
            json!({"account":"tharun-upstox","session":session}).to_string(),
        )
        .unwrap();
    }

    fn fresh_config(dir: &Path) -> Config {
        let mut c = super::super::super::tests::config(dir);
        let root = c.sessions_root.clone().unwrap();
        c.session = "2026-11-30".parse().unwrap();
        c.previous_session = "2026-11-27".parse().unwrap();
        c.directory = root.join("2026-11-30");
        c.capital = Some(nivra_core::execution::a("10000"));
        c.sleeves = super::super::super::tests::test_sleeves("5000", "5000");
        c
    }

    #[test]
    fn a_closed_flat_book_hands_over_to_a_fresh_sleeved_book_once() {
        let d = tempfile::tempdir().unwrap();
        let c = fresh_config(d.path());
        let root = c.sessions_root.clone().unwrap();
        let open = r#"{"cash":"4985.88","closed":false,"halted":false,"positions":{},"fills":[]}"#;
        old_book(&root, "2026-11-27", open);
        assert_eq!(
            failure_code(open_fresh(&c, true, 1).unwrap_err().as_ref()),
            "fresh_book_requires_closed_flat_unhalted_predecessor"
        );
        assert!(!c.directory.exists());
        assert_eq!(
            failure_code(open_fresh(&c, false, 1).unwrap_err().as_ref()),
            "fresh_book_confirm_paper_review_required"
        );

        let closed =
            r#"{"cash":"4985.88","closed":true,"halted":false,"positions":{},"fills":[{},{}]}"#;
        old_book(&root, "2026-11-27", closed);
        fs::create_dir_all(root.join("strategy-library")).unwrap();
        fs::write(root.join("strategy-library/keep.json"), b"{}").unwrap();
        let receipt = open_fresh(&c, true, 7).unwrap();
        assert_eq!(receipt["predecessor"]["source_cash"], "4985.88");
        assert_eq!(receipt["predecessor"]["source_fills"], 2);
        assert_eq!(receipt["capital"], "10000");
        assert!(!root.join("current.json").exists());
        assert!(root.join("current.before-2026-11-30.json").exists());
        assert_eq!(open_fresh(&c, true, 9).unwrap(), receipt);
        assert!(root.join("strategy-library/keep.json").exists());

        // The next start opens the new book from the sleeves.
        assert_eq!(current(&c).unwrap().directory, c.directory);
        let mut db = super::super::super::open_book(&c).unwrap();
        let p = wide::load_wide(&mut db, &c).unwrap();
        assert_eq!(p.allocations.len(), 2);
        assert_eq!(p.cash, nivra_core::execution::a("10000"));
    }

    #[test]
    fn pending_fresh_book_resolves_universe_and_readiness_without_masked_error() {
        let d = tempfile::tempdir().unwrap();
        let mut c = fresh_config(d.path());
        let archive_pw = d.path().join("archive-pw");
        fs::write(&archive_pw, "test-pw").unwrap();
        fs::set_permissions(&archive_pw, fs::Permissions::from_mode(0o600)).unwrap();
        c.market_archive = Some(crate::runtime::MarketArchiveSettings {
            url: "postgresql://nivra@127.0.0.1/nivra_market".into(),
            password_file: archive_pw,
        });
        let root = c.sessions_root.clone().unwrap();
        let closed = r#"{"cash":"4985.88","closed":true,"halted":false,"positions":{},"fills":[]}"#;
        old_book(&root, "2026-11-27", closed);
        open_fresh(&c, true, 7).unwrap();

        // Write a valid cached universe in the predecessor directory (2026-11-27);
        // 2026-11-30 only holds fresh-book.json.
        let mut pred = c.clone();
        pred.session = "2026-11-27".parse().unwrap();
        pred.previous_session = "2026-11-26".parse().unwrap();
        pred.directory = root.join("2026-11-27");
        let gz = |s: &str| {
            use std::io::Write;
            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(s.as_bytes()).unwrap();
            enc.finish().unwrap()
        };
        let sec =
            gz("TckrSymb,SctySrs,ISIN,NewBrdLotQty,ElgbltyNrmlMkt\nAAA,EQ,INE000000001,1,1\n");
        let brok = gz(
            &json!([{"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"AAA","instrument_type":"EQ","isin":"INE000000001","instrument_key":"NSE_EQ|INE000000001","lot_size":1,"tick_size":5,"security_type":"NORMAL"}]).to_string(),
        );
        let susp = gz("[]");
        fs::write(pred.directory.join("NSE_CM_security_26112026.csv.gz"), &sec).unwrap();
        fs::write(pred.directory.join("upstox-NSE.json.gz"), &brok).unwrap();
        fs::write(pred.directory.join("upstox-suspended.json.gz"), &susp).unwrap();
        let universe = json!({
            "instruments":[{"key":"NSE_EQ|INE000000001","symbol":"AAA","series":"EQ","tick":"0.05"}],
            "exclusions":{},
            "file_date":"2026-11-26",
            "session":"2026-11-27",
            "security_sha256":digest(&sec),
            "broker_sha256":digest(&brok),
            "suspended_sha256":digest(&susp),
            "received_ms":1
        });
        fs::write(
            pred.directory.join("wide-universe.json"),
            universe.to_string(),
        )
        .unwrap();

        // History instruments resolve from the predecessor when current.json was moved aside.
        let instruments = wide::history_instruments(&c).unwrap();
        assert_eq!(instruments.len(), 1);
        assert_eq!(instruments[0].key, "NSE_EQ|INE000000001");

        // Readiness resolves the pending fresh book without opening paper.sqlite.
        let at_sat = DateTime::parse_from_rfc3339("2026-11-28T12:00:00+05:30")
            .unwrap()
            .timestamp_millis();
        let ready = super::super::super::readiness::snapshot_for_config(&c, at_sat).unwrap();
        assert_eq!(ready["target_session"], "2026-11-30");
        assert_eq!(ready["lifecycle"]["status"], "pending_fresh_book");
        assert_eq!(ready["lifecycle"]["predecessor_session"], "2026-11-27");
        assert_eq!(ready["history"]["universe_session"], "2026-11-27");

        // `history-backfill --check-only` resolves the pending session instead of failing with ENOENT.
        let cfg_path = d.path().join("paper-fresh.json");
        fs::write(&cfg_path, serde_json::to_vec(&c).unwrap()).unwrap();
        assert_ne!(
            super::super::super::history_download::backfill_entry(&cfg_path, true, false, false),
            ExitCode::SUCCESS
        );

        // An outdated config pointing to the predecessor gets a specific pending code,
        // never silent fallback or paper_runtime_unavailable.
        let old_err = current(&pred).err().unwrap();
        assert_eq!(
            failure_code(old_err.as_ref()),
            "session_pointer_pending_fresh_book"
        );

        // Missing files produce specific snake_case codes, never paper_runtime_unavailable.
        fs::remove_file(pred.directory.join("wide-universe.json")).unwrap();
        let missing_uni = wide::history_instruments(&c).unwrap_err();
        assert_ne!(
            failure_code(missing_uni.as_ref()),
            "paper_runtime_unavailable"
        );
        let missing_book = bound_state(&c).unwrap_err();
        assert_ne!(
            failure_code(missing_book.as_ref()),
            "paper_runtime_unavailable"
        );
    }
}
