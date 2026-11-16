//! Sealed closed-book proof. A closed book whose report and review seal match its
//! state is never reopened by planner initialization or incident handling.
use super::*;
use sadr_core::execution::Portfolio;
use serde_json::Value;

// The report timestamp is the durable close boundary, authenticated by review.json.
fn sealed(c: &Config, p: &Portfolio) -> Result<()> {
    let report = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| "disposition_report_unreadable")?;
    let review = crate::read_bounded(&c.directory.join("review.json"), 1_048_576)
        .map_err(|_| "disposition_review_unreadable")?;
    let r: Value = sadr_core::native::decode(&report)?;
    let seal: Value = sadr_core::native::decode(&review)?;
    let reported: Portfolio = serde_json::from_value(r["portfolio"].clone())?;
    reported.reconcile()?;
    if LIVE_ENABLED
        || r["account"] != c.account
        || r["session"] != c.session.to_string()
        || r["paper_only"] != true
        || r["live_enabled"] != false
        || !reported.closed
        || seal["session"] != c.session.to_string()
        || seal["closed"] != true
        || seal["halted"] != reported.halted
        || seal["open_unresolved"] != 0
        || seal["report_hash"] != digest(&report)
        || seal["state_hash"] != digest(serde_json::to_string(&reported)?.as_bytes())
    {
        return Err("disposition_report_seal_mismatch".into());
    }
    let mut comparable = p.clone();
    comparable.health.clone_from(&reported.health);
    comparable.stopped = reported.stopped;
    comparable.halted = reported.halted;
    if comparable != reported || !reported.positions.is_empty() {
        return Err("disposition_financial_state_changed".into());
    }
    Ok(())
}
pub(super) fn closed_sealed(c: &Config, p: &Portfolio) -> bool {
    p.closed && sealed(c, p).is_ok()
}

#[cfg(test)]
pub(super) fn check_boundaries() {
    use serde_json::json;
    let d = tempfile::tempdir().unwrap();
    let mut c = super::tests::config(d.path());
    c.sessions_root = Some(d.path().join("sessions"));
    let cfg: planner::PlannerConfig = serde_json::from_value(json!({"enabled":true,"events":{"enabled":true,"cooldown_ms":0,"max_pending":10,"policy":{"price_move_bps":100,"max_event_age_ms":10000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}}})).unwrap();
    let now = DateTime::parse_from_rfc3339("2026-10-16T16:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut db = open_book(&c).unwrap();
    let mut p = Portfolio::with_capital(c.allocation().unwrap());
    redecision::initialize(&mut db, &c, &p, Some(&cfg), now - 60_000).unwrap();
    p.closed = true;
    p.health = "closed".into();
    p.reconcile().unwrap();
    let sealed_body = serde_json::to_string(&p).unwrap();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![book_binding(&c).unwrap(), sealed_body],
    )
    .unwrap();
    let report = serde_json::to_vec(&json!({"paper_only":true,"live_enabled":false,"account":c.account,"session":c.session,"as_of_ms":now-60_000,"portfolio":p})).unwrap();
    fs::write(c.directory.join("report.json"), &report).unwrap();
    fs::write(c.directory.join("review.json"), serde_json::to_vec(&json!({"session":c.session,"closed":true,"halted":false,"open_unresolved":0,"state_hash":digest(sealed_body.as_bytes()),"report_hash":digest(&report)})).unwrap()).unwrap();
    assert!(closed_sealed(&c, &p));
    // Neither initialization nor incident handling may mutate this sealed book.
    assert!(!redecision::initialize(&mut db, &c, &p, Some(&cfg), now).unwrap());
    wide::check_closed_incident_guard(&mut db, &c, &mut p, now);
    // A changed report, open book or live flag breaks the seal.
    let mut open = p.clone();
    open.closed = false;
    assert!(!closed_sealed(&c, &open));
    let mut r: Value = serde_json::from_slice(&report).unwrap();
    r["live_enabled"] = json!(true);
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&r).unwrap(),
    )
    .unwrap();
    assert!(!closed_sealed(&c, &p));
}
