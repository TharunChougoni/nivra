//! A reviewed fresh book beside a closed, flat predecessor in the same
//! sessions root. The predecessor's book, report and strategy library stay in
//! place; only the session pointer moves aside, so the next start opens a new
//! book funded by the config's sleeves.
use super::*;

const RECEIPT: &str = "fresh-book.json";
const REASON: &str = "operator_fresh_book_closed_flat_predecessor";

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
}
