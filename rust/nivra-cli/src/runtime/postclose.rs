//! Sealed post-close evidence for the owner's session review. Financial
//! reports, rollover and protection remain independent.
use super::*;
use nivra_core::{execution::Portfolio, postclose as contract};
use serde_json::{Value, json};
// Evidence bounds stay at the values sealed reviews were built with.
const MAX_FILL_EVIDENCE: usize = 128;
const MAX_PLANNER_EVIDENCE_ROWS: usize = 24;
const MAX_TICK_EVIDENCE_ROWS: usize = 48;
fn account(c: &Config) -> String {
    digest(format!("upstox\0{}", c.account).as_bytes())
}
fn decode<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T> {
    if raw.len() > 1_000_000 {
        return Err("postclose_journal_bound".into());
    }
    Ok(serde_json::from_value(nivra_core::native::decode(
        raw.as_bytes(),
    )?)?)
}
pub(crate) fn seal_evidence(
    db: &Connection,
    c: &Config,
    p: &Portfolio,
) -> Result<contract::SealedEvidence> {
    if !p.closed {
        return Err("postclose_requires_closed_report".into());
    }
    p.reconcile()?;
    let report = crate::read_bounded(&c.directory.join("report.json"), 16_000_000)
        .map_err(|_| "postclose_report_unreadable")?;
    let v: Value = serde_json::from_slice(&report)?;
    let report_p: Portfolio = serde_json::from_value(v["portfolio"].clone())?;
    let (binding, body): (String, String) =
        db.query_row("SELECT binding,body FROM state WHERE id=1", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    if binding != book_binding(c)?
        || v["account"] != c.account
        || v["session"] != c.session.to_string()
        || v["broker"] != "upstox"
        || v["paper_only"] != true
        || v["live_enabled"] != false
        || !report_p.closed
        || report_p != *p
        || serde_json::from_str::<Portfolio>(&body)? != *p
    {
        return Err("postclose_report_state_mismatch".into());
    }
    let report_hash = digest(&report);
    let mut items=vec![contract::EvidenceItem {id:"report".into(),kind:contract::EvidenceKind::Report,artifact_sha256:report_hash.clone(),summary:json!({"cash":p.cash,"account_allocation":p.allocation()?,"positions":p.positions.len(),"cumulative_fill_count":p.fills.len(),"closed":true,"stopped":p.stopped,"halted":p.halted,"paper_only":true,"live_enabled":false,"daily_pnl":"not_inferred_from_cumulative_book"}).to_string()}];
    // Fills carry across days: only original current-session timestamps count here.
    let daily = p
        .fills
        .iter()
        .enumerate()
        .filter(|(_, f)| local(f.at).date_naive() == c.session)
        .collect::<Vec<_>>();
    let fees = daily
        .iter()
        .try_fold(nivra_core::finance::Amount::ZERO, |sum, (_, f)| {
            sum.checked_add(f.fee)
        })?;
    items.push(contract::EvidenceItem {id:"daily_activity".into(),kind:contract::EvidenceKind::Health,artifact_sha256:digest(body.as_bytes()),summary:json!({"session_fill_count":daily.len(),"session_fill_fees":fees,"individual_fill_coverage":daily.len().min(MAX_FILL_EVIDENCE),"attribution":"original_fill_session_timestamp"}).to_string()});
    for (index, f) in daily.into_iter().take(MAX_FILL_EVIDENCE) {
        items.push(contract::EvidenceItem {id:format!("fill:{index}"),kind:contract::EvidenceKind::Fill,artifact_sha256:digest(&serde_json::to_vec(f)?),summary:json!({"at":f.at,"units":f.units,"buy":f.buy,"price":f.price,"fee":f.fee,"instrument_hash":digest(f.instrument.key.as_bytes()),"decision_hash":f.decision_id.as_ref().map(|id|digest(id.as_bytes()))}).to_string()});
    }
    // Hash original rows, expose only typed identity/timing, never raw model text/logs.
    for (table, kind, prefix) in [
        ("planner_event_log", contract::EvidenceKind::Event, "event"),
        (
            "planner_attempt_log",
            contract::EvidenceKind::Attempt,
            "attempt",
        ),
    ] {
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            let mut stmt = db.prepare(&format!(
                "SELECT id,body,state FROM {table} ORDER BY rowid DESC LIMIT ?"
            ))?;
            for row in stmt.query_map([MAX_PLANNER_EVIDENCE_ROWS as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })? {
                let (id, body, state) = row?;
                let record: Value = decode(&body)?;
                let state = if [
                    "pending",
                    "reserved",
                    "expired",
                    "interrupted",
                    "planned",
                    "superseded",
                    "failed",
                ]
                .contains(&state.as_str())
                {
                    state.as_str()
                } else {
                    "unrecognized"
                };
                let trigger = record
                    .get("kind")
                    .or_else(|| record.get("trigger").and_then(|v| v.get("kind")))
                    .and_then(Value::as_str)
                    .filter(|s| {
                        [
                            "price_move",
                            "volume_move",
                            "gap",
                            "proximity",
                            "news",
                            "fill",
                            "initial",
                        ]
                        .contains(s)
                    });
                items.push(contract::EvidenceItem {
                    id: format!("{prefix}:{}", digest(id.as_bytes())),
                    kind: kind.clone(),
                    artifact_sha256: digest(body.as_bytes()),
                    summary: json!({"record_hash":digest(id.as_bytes()),"state":state,"trigger":trigger,"observed_ms":record["observed_ms"].as_i64(),"started_ms":record["started_ms"].as_i64(),"deadline_ms":record["deadline_ms"].as_i64(),"retained":true,"coverage":format!("latest_{}_records_per_kind", MAX_PLANNER_EVIDENCE_ROWS)})
                        .to_string(),
                });
            }
        }
    }
    let mut stmt = db.prepare("SELECT id,at,receipt,event FROM ticks ORDER BY id DESC LIMIT ?")?;
    for row in stmt.query_map([MAX_TICK_EVIDENCE_ROWS as i64], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, String>(3)?,
        ))
    })? {
        let (id, at, receipt, event) = row?;
        let code = if [
            "wide_quotes",
            "post_close",
            "planner_authority_invalid",
            "operator_stopped",
            "stopped",
            "wide_reconciled",
            "wide_read_failure",
            "wide_read_failed",
            "read_unavailable",
            "invalid_quote_json",
            "invalid_quote_status",
            "invalid_quote_data",
            "duplicate_quote",
            "read_failed",
            "wide_data_failure",
            "planner_failed",
            "daily_session_created",
        ]
        .contains(&event.as_str())
        {
            event.as_str()
        } else {
            "other_retained_event"
        };
        items.push(contract::EvidenceItem {id:format!("tick:{id}"),kind:contract::EvidenceKind::Health,artifact_sha256:digest(&serde_json::to_vec(&json!([id,at,receipt,event]))?),summary:json!({"at":at,"receipt":receipt,"receipt_age_ms":receipt.and_then(|r|at.checked_sub(r)),"category":code,"coverage":format!("latest_{}_journal_events", MAX_TICK_EVIDENCE_ROWS)}).to_string()});
    }
    let ticks: i64 = db.query_row("SELECT COUNT(*) FROM ticks", [], |r| r.get(0))?;
    items.push(contract::EvidenceItem {id:"health".into(),kind:contract::EvidenceKind::Health,artifact_sha256:digest(body.as_bytes()),summary:json!({"journal_events":ticks,"stopped":p.stopped,"halted":p.halted,"note":"Raw error logs and provider responses excluded"}).to_string()});
    contract::seal(contract::EvidenceInput {
        account: account(c),
        session: c.session,
        session_binding: binding,
        report_sha256: report_hash,
        state_sha256: digest(body.as_bytes()),
        evidence: items,
    })
}
#[cfg(test)]
pub(super) fn behavioral_assertions() {
    use nivra_core::{
        execution::{ExitIntent, Instrument, Quote, a},
        finance::Price,
    };
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("sessions");
    let mut c = super::tests::config(&root.join("2026-10-16"));
    c.sessions_root = Some(root.clone());
    fs::create_dir_all(&c.directory).unwrap();
    let db = open_book(&c).unwrap();
    let at = DateTime::parse_from_rfc3339("2026-10-16T15:40:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut p = crate::runtime::tests::test_portfolio();
    p.model_led = true;
    let i = Instrument {
        key: "NSE_EQ|SYNTHETIC".into(),
        symbol: "SYNTHETIC".into(),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    for time in [at - 86400000 - 18000000, at - 18000000] {
        let q = Quote {
            bid: Price::parse("100").unwrap(),
            ask: Price::parse("100").unwrap(),
            bid_units: 100,
            ask_units: 100,
            source: time,
            receipt: time,
        };
        p.protect(time, &BTreeMap::from([(i.key.clone(), q.clone())]))
            .unwrap();
        assert!(
            p.execute_plan(
                time,
                &i,
                &q,
                true,
                1,
                Price::parse("101").unwrap(),
                ExitIntent::default(),
                "synthetic"
            )
            .unwrap()
        );
    }
    p.closed = true;
    let body = serde_json::to_string(&p).unwrap();
    db.execute(
        "INSERT INTO state VALUES(1,?,?)",
        rusqlite::params![book_binding(&c).unwrap(), body],
    )
    .unwrap();
    db.execute(
        "INSERT INTO ticks(at,receipt,event) VALUES(?,?,?)",
        rusqlite::params![at - 60000, at - 60001, "wide_quotes"],
    )
    .unwrap();
    let report = json!({"paper_only":true,"live_enabled":false,"broker":"upstox","account":c.account,"session":c.session,"portfolio":p});
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let before = p.clone();
    let sealed = seal_evidence(&db, &c, &p).unwrap();
    let daily: Value = serde_json::from_str(
        &sealed
            .input
            .evidence
            .iter()
            .find(|e| e.id == "daily_activity")
            .unwrap()
            .summary,
    )
    .unwrap();
    assert_eq!(daily["session_fill_count"], 1);
    assert_eq!(
        daily["session_fill_fees"],
        serde_json::to_value(p.fills[1].fee).unwrap()
    );
    assert_eq!(
        sealed
            .input
            .evidence
            .iter()
            .filter(|e| e.kind == contract::EvidenceKind::Fill)
            .count(),
        1
    );
    assert!(sealed.input.evidence.iter().any(|e| e.id == "tick:1"));
    for (field, value) in [
        ("broker", json!("other")),
        ("paper_only", json!(false)),
        ("live_enabled", json!(true)),
    ] {
        let mut wrong = report.clone();
        wrong[field] = value;
        fs::write(c.directory.join("report.json"), wrong.to_string()).unwrap();
        assert!(seal_evidence(&db, &c, &p).is_err());
    }
    fs::write(
        c.directory.join("report.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    assert_eq!(p, before);
    assert_eq!(p.allocation().unwrap(), a("5000"));
}
