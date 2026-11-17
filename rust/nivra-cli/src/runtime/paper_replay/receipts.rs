use super::*;

/// Explicit archived quote purpose. Research-observation receipts are virtual
/// authority only: they are never mapped to protection or screen and never
/// become executable paper evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuotePurposeV1 {
    Protection,
    Screen,
    ResearchObservation,
}
impl QuotePurposeV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Protection => "protection",
            Self::Screen => "screen",
            Self::ResearchObservation => "research_observation",
        }
    }
    fn archive_str(self) -> &'static str {
        match self {
            Self::Protection => "execution_or_protection",
            Self::Screen => "scanner",
            Self::ResearchObservation => "research_observation",
        }
    }
    pub(super) fn from_archive(value: Option<&str>) -> Option<Self> {
        match value {
            Some("scanner" | "scanner_metadata") => Some(Self::Screen),
            Some("execution_or_protection") => Some(Self::Protection),
            Some("research_observation") => Some(Self::ResearchObservation),
            _ => None,
        }
    }
}
/// Original quote receipts shared by feature export and lifecycle marks. Only
/// archived receipt times and source observation times are returned; nothing is
/// interpolated or filled forward.
pub(crate) struct QuoteReceipt {
    pub at_ms: i64,
    pub receipt_ms: i64,
    pub kind: QuotePurposeV1,
    pub observations: Vec<Value>,
}
pub(crate) struct QuoteEvidence {
    pub instruments: Vec<Instrument>,
    pub receipts: Vec<QuoteReceipt>,
}
/// Quote receipts whose original source observation is at or before the host
/// eligibility clock. A future row can never enter a candidate dataset or a
/// maturation window.
pub(crate) fn quote_receipts_bounded(
    source: &Path,
    session: NaiveDate,
    max_rows: usize,
    max_source_ms: i64,
) -> std::result::Result<QuoteEvidence, String> {
    if !source.is_absolute() || !(1..=10_000).contains(&max_rows) || max_source_ms < 0 {
        return Err("scorer_snapshots_unavailable".into());
    }
    let universe: Value = serde_json::from_slice(
        &crate::read_bounded(&source.join("wide-universe.json"), 8_000_000)
            .map_err(|_| "scorer_snapshots_unavailable".to_string())?,
    )
    .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let declared: NaiveDate = serde_json::from_value(universe["session"].clone())
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    if declared != session || instruments.is_empty() || instruments.len() > 10_000 {
        return Err("scorer_snapshots_unavailable".into());
    }
    let db = Connection::open_with_flags(
        source.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    db.pragma_update(None, "query_only", true)
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let mut statement = db
        .prepare(
            "SELECT at,receipt,body FROM ticks WHERE event IN ('wide_quotes','scanner_metadata') AND at<=?2 ORDER BY id DESC LIMIT ?1",
        )
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let rows = statement
        .query_map(rusqlite::params![max_rows as i64, max_source_ms], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    let mut reversed = Vec::new();
    for row in rows {
        reversed.push(row.map_err(|_| "scorer_snapshots_unavailable".to_string())?);
    }
    reversed.reverse();
    let mut receipts = Vec::with_capacity(reversed.len());
    let mut previous = None;
    for (at, receipt, raw) in reversed {
        if raw.len() > 2_000_000 {
            return Err("scorer_snapshots_unavailable".into());
        }
        let receipt = receipt.ok_or("scorer_snapshots_receipt_missing")?;
        let archive: Value =
            serde_json::from_str(&raw).map_err(|_| "scorer_snapshots_invalid".to_string())?;
        if archive["kind"] == "compacted_quote_receipt"
            || archive["received_ms"] != receipt
            || receipt > at
            || at > max_source_ms
            || receipt > max_source_ms
        {
            return Err("scorer_snapshots_receipt_binding".into());
        }
        let body: Value = serde_json::from_str(
            archive["body"]
                .as_str()
                .ok_or("scorer_snapshots_not_quote_receipt")?,
        )
        .map_err(|_| "scorer_snapshots_invalid".to_string())?;
        if body["status"] != "success" {
            return Err("scorer_snapshots_quote_status".into());
        }
        let kind = QuotePurposeV1::from_archive(archive["purpose"].as_str())
            .ok_or("scorer_snapshots_purpose_unknown")?;
        // The ORIGINAL archived receipt SHA for every observation. It is never
        // the whole-tape digest and never another key's SHA.
        let archive_sha256 = nivra_core::replay::digest(raw.as_bytes());
        let mut observations = Vec::new();
        let rows = body["data"]
            .as_object()
            .ok_or("scorer_snapshots_quote_rows")?;
        for row in rows.values() {
            let Some(instrument) = instruments
                .iter()
                .find(|instrument| row["instrument_token"] == instrument.key)
            else {
                continue;
            };
            let quote = match data::parse_row(
                row,
                receipt,
                &instrument.key,
                &instrument.symbol,
                instrument.tick,
            ) {
                Ok(quote) => quote,
                Err(_) => continue,
            };
            let (lower, upper, volume) = match (
                row["lower_circuit_limit"].as_number(),
                row["upper_circuit_limit"].as_number(),
                row["volume"].as_u64(),
            ) {
                (Some(lower), Some(upper), Some(volume)) => {
                    (lower.to_string(), upper.to_string(), volume)
                }
                _ => continue,
            };
            observations.push(json!({
                "key":instrument.key,"quote":quote,
                "lower_circuit_limit":lower,"upper_circuit_limit":upper,"volume":volume,
                "archive_sha256":archive_sha256,
                "purpose":kind.archive_str()
            }));
        }
        if previous.is_some_and(|prior| at <= prior) {
            return Err("scorer_snapshots_chronology".into());
        }
        previous = Some(at);
        receipts.push(QuoteReceipt {
            at_ms: at,
            receipt_ms: receipt,
            kind,
            observations,
        });
    }
    if receipts.is_empty() {
        return Err("scorer_snapshots_unavailable".into());
    }
    Ok(QuoteEvidence {
        instruments,
        receipts,
    })
}

/// Frozen executable quote tape for the optional after-cost cash replay.
///
/// It is built from the same `quote_receipts_bounded` reader as the scorer
/// dataset, so there is no second quote normalizer. Only receipts whose
/// original source observation is at or before the submission clock enter the
/// tape; a later exit quote is never fetched. Screen receipts stay in the tape
/// as ranking-only evidence and the adapter refuses them for fills.
pub(crate) struct ExecutionTape {
    pub manifest: Vec<u8>,
    pub events: Vec<u8>,
    pub manifest_sha256: String,
    pub events_sha256: String,
    pub protection_receipts: usize,
    pub protection_observations: usize,
    pub screen_receipts: usize,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execution_tape_bounded(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    capital: Amount,
    max_quote_events: usize,
    max_observations: usize,
    max_tape_bytes: u64,
    now: i64,
) -> std::result::Result<ExecutionTape, String> {
    if capital <= Amount::ZERO
        || !(1..=10_000).contains(&max_quote_events)
        || !(1..=100_000).contains(&max_observations)
        || !(1..=16 * 1024 * 1024).contains(&max_tape_bytes)
        || now < 0
    {
        return Err("candidate_execution_tape_invalid".into());
    }
    let evidence = quote_receipts_bounded(source, session, max_quote_events, now)?;
    let mut events = Vec::new();
    let mut observations = 0usize;
    let mut protection_receipts = 0usize;
    let mut protection_observations = 0usize;
    let mut screen_receipts = 0usize;
    let mut max_gap_ms = 0i64;
    let mut previous_receipt_ms = None;
    for receipt in &evidence.receipts {
        // Research-observation receipts are virtual authority only. The
        // historical protection tape skips them entirely instead of relabelling
        // them as screening evidence.
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if let Some(previous) = previous_receipt_ms {
            max_gap_ms = max_gap_ms.max(receipt.at_ms.saturating_sub(previous));
        }
        previous_receipt_ms = Some(receipt.at_ms);
        observations = observations
            .checked_add(receipt.observations.len())
            .ok_or("candidate_execution_tape_too_large")?;
        if observations > max_observations {
            return Err("candidate_execution_tape_too_large".into());
        }
        if receipt.kind == QuotePurposeV1::Protection {
            protection_receipts = protection_receipts.saturating_add(1);
            protection_observations =
                protection_observations.saturating_add(receipt.observations.len());
        } else {
            screen_receipts = screen_receipts.saturating_add(1);
        }
        events.extend_from_slice(
            serde_json::to_string(&json!({
                "at":receipt.at_ms,
                "available_at":receipt.receipt_ms,
                "kind":receipt.kind.as_str(),
                "observations":receipt.observations
            }))
            .map_err(|_| "candidate_execution_tape_invalid".to_string())?
            .as_bytes(),
        );
        events.push(b'\n');
        if events.len() as u64 > max_tape_bytes {
            return Err("candidate_execution_tape_too_large".into());
        }
    }
    if protection_receipts == 0 || protection_observations == 0 {
        return Err("candidate_execution_quotes_unavailable".into());
    }
    let events_sha256 = nivra_core::replay::digest(&events);
    // The archive validator needs a strictly earlier date for this field, but
    // the action engine never uses it. Prefer the caller's verified calendar
    // walkback; otherwise keep a truthful calendar-day placeholder and mark the
    // semantics instead of inventing a holiday-aware session.
    let previous_session_verified = previous_session.is_some();
    let previous_session = previous_session.unwrap_or(
        session
            .pred_opt()
            .ok_or("candidate_execution_tape_invalid")?,
    );
    let manifest = json!({
        "schema":1,
        "session":session,
        "previous_session":previous_session,
        "capital":capital,
        "universe":evidence.instruments,
        "events_sha256":events_sha256,
        "synthetic":false,
        "max_observation_gap_ms":max_gap_ms.max(1),
        "history_directory":null,
        "paper_costs":null,
        "provenance":{
            "source":"original_paper_quote_journal_read_only",
            "research_capital_assumption":true,
            "research_costs_assumption":true,
            "account_funds_claim":false,
            "legacy_quote_purpose_unknown":0,
            "protection_receipts":protection_receipts,
            "screen_receipts":screen_receipts,
            "previous_session_semantics":if previous_session_verified{"verified_calendar_trading_day"}else{"calendar_day_not_verified_trading_day"},
            "max_observation_gap_ms_semantics":"observed_max_receipt_gap_not_an_engine_tolerance"
        }
    });
    let manifest = serde_json::to_vec(&manifest)
        .map_err(|_| "candidate_execution_tape_invalid".to_string())?;
    if manifest.len() > 512 * 1024 {
        return Err("candidate_execution_tape_too_large".into());
    }
    nivra_cli::strategy_archive::validate_archive(&manifest, &events)?;
    Ok(ExecutionTape {
        manifest_sha256: nivra_core::replay::digest(&manifest),
        events_sha256,
        manifest,
        events,
        protection_receipts,
        protection_observations,
        screen_receipts,
    })
}

/// Build the session's own paper export in memory and extract the same feature
/// snapshots as `strategy-lab export-features`. The source book is opened
/// read-only; no manifest, journal or runtime state is created or changed.
pub(crate) fn scorer_snapshots(
    source: &Path,
    session: NaiveDate,
    max_snapshots: usize,
    max_age_ms: i64,
    lookback_samples: usize,
) -> std::result::Result<Vec<nivra_core::strategy::FeatureSnapshot>, String> {
    scorer_snapshots_bounded(
        source,
        session,
        None,
        max_snapshots,
        max_age_ms,
        lookback_samples,
        i64::MAX,
    )
}

/// Candidate freeze path: only quote receipts whose original source time is at
/// or before the submitted host clock enter the dataset. Later ticks cannot
/// mutate the frozen bytes because the caller stores them before admission.
pub(crate) fn scorer_snapshots_bounded(
    source: &Path,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    max_snapshots: usize,
    max_age_ms: i64,
    lookback_samples: usize,
    now: i64,
) -> std::result::Result<Vec<nivra_core::strategy::FeatureSnapshot>, String> {
    if !source.is_absolute()
        || !(1..=10_000).contains(&max_snapshots)
        || !(1..=300_000).contains(&max_age_ms)
        || !(1..=20).contains(&lookback_samples)
        || now < 0
    {
        return Err("scorer_snapshots_unavailable".into());
    }
    let warmup = max_snapshots.saturating_add(lookback_samples).min(10_000);
    let evidence = quote_receipts_bounded(source, session, warmup, now)?;
    let mut events = String::new();
    let mut observations = 0usize;
    let mut max_gap_ms = 0i64;
    let mut previous_receipt_ms = None;
    for receipt in &evidence.receipts {
        // Research-observation receipts are virtual authority only. The
        // historical protection tape skips them entirely instead of relabelling
        // them as screening evidence.
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if let Some(previous) = previous_receipt_ms {
            max_gap_ms = max_gap_ms.max(receipt.at_ms.saturating_sub(previous));
        }
        previous_receipt_ms = Some(receipt.at_ms);
        observations = observations.saturating_add(receipt.observations.len());
        events.push_str(&format!(
            "{}\n",
            json!({"at":receipt.at_ms,"available_at":receipt.receipt_ms,"kind":receipt.kind,"observations":receipt.observations})
        ));
    }
    let previous_session_verified = previous_session.is_some();
    let previous_session =
        previous_session.unwrap_or(session.pred_opt().ok_or("scorer_snapshots_unavailable")?);
    let manifest = json!({
        "schema":1,"session":session,
        "previous_session":previous_session,
        "capital":"5000","universe":evidence.instruments,
        "events_sha256":nivra_core::replay::digest(events.as_bytes()),
        "synthetic":false,"max_observation_gap_ms":max_gap_ms.max(1),"history_directory":null,
        "provenance":{
            "legacy_quote_purpose_unknown":0,
            "source":"session_paper_quotes_read_only",
            "previous_session_semantics":if previous_session_verified{"verified_calendar_trading_day"}else{"calendar_day_not_verified_trading_day"},
            "max_observation_gap_ms_semantics":"observed_max_receipt_gap_not_an_engine_tolerance"
        }
    });
    let config = nivra_cli::strategy_archive::ArchiveConfig {
        max_archive_bytes: (events.len() as u64).clamp(1, 128 * 1024 * 1024),
        max_observations: observations.clamp(1, 1_000_000),
        from_ms: None,
        to_ms: None,
    };
    let mut snapshots = nivra_cli::strategy_archive::export_features_configured(
        &serde_json::to_vec(&manifest).map_err(|_| "scorer_snapshots_invalid".to_string())?,
        events.as_bytes(),
        max_age_ms,
        lookback_samples,
        &config,
    )?;
    snapshots.retain(|snapshot| snapshot.observed_ms <= now);
    if snapshots.len() > max_snapshots {
        snapshots.drain(..snapshots.len() - max_snapshots);
    }
    if snapshots.is_empty() {
        return Err("scorer_snapshots_unavailable".into());
    }
    Ok(snapshots)
}

/// Original-receipt marks for lifecycle maturation. Only quotes whose archived
/// receipt time is inside the requested window are returned; the source clock is
/// the tick observation time. No future or interpolated quote is invented.
pub(crate) fn candidate_quotes(
    source: &Path,
    session: NaiveDate,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    max_rows: usize,
    now: i64,
) -> std::result::Result<Value, String> {
    if keys.is_empty()
        || from_ms < 0
        || to_ms < from_ms
        || now < 0
        || !(1..=10_000).contains(&max_rows)
    {
        return Err("candidate_quotes_invalid".into());
    }
    let evidence = quote_receipts_bounded(source, session, 10_000, now)?;
    let end_of_evidence_ms = evidence
        .receipts
        .last()
        .map(|receipt| receipt.at_ms)
        .unwrap_or(0);
    let mut quotes = Vec::new();
    let mut truncated = false;
    for receipt in &evidence.receipts {
        // Old lifecycle marks are protection/screen evidence only. A research
        // receipt is not a mark and is never substituted for one.
        if receipt.kind == QuotePurposeV1::ResearchObservation {
            continue;
        }
        if receipt.receipt_ms < from_ms || receipt.receipt_ms > to_ms {
            continue;
        }
        for observation in &receipt.observations {
            let Some(key) = observation["key"].as_str() else {
                continue;
            };
            if !keys.contains(key) {
                continue;
            }
            if quotes.len() == max_rows {
                truncated = true;
                break;
            }
            quotes.push(json!({
                "key":key,
                "source_ms":receipt.at_ms,
                "receipt_ms":receipt.receipt_ms,
                "interpolated":false
            }));
        }
        if truncated {
            break;
        }
    }
    let coverage = if end_of_evidence_ms < from_ms {
        "source_ended_before_window"
    } else if quotes.is_empty() {
        "missing_quotes_in_window"
    } else {
        "quotes_present"
    };
    Ok(json!({
        "quotes":quotes,
        "end_of_evidence_ms":end_of_evidence_ms,
        "coverage":coverage,
        "truncated":truncated,
        "interpolated":false
    }))
}

/// One original quote receipt row from the archived journal. `receipt_ms` is
/// the archived financial receipt clock and `event_at_ms` is the journal insert
/// clock; neither is replaced by a later read time. A retention-compacted row
/// keeps its original archive SHA and explicit unknown key/purpose instead of
/// being silently dropped.
#[allow(clippy::too_many_arguments)]
pub(super) fn candidate_quotes_window_with(
    source: &Path,
    session: NaiveDate,
    keys: &BTreeSet<String>,
    from_ms: i64,
    to_ms: i64,
    max_rows: usize,
    now: i64,
    purposes: &[QuotePurposeV1],
    newest_first: bool,
    immutable: bool,
) -> std::result::Result<Value, String> {
    if keys.is_empty()
        || from_ms < 0
        || to_ms < from_ms
        || now < 0
        || !(1..=10_000).contains(&max_rows)
        || purposes.is_empty()
    {
        return Err("candidate_quotes_invalid".into());
    }
    let raw = crate::read_bounded(&source.join("wide-universe.json"), 8_000_000)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let universe: Value =
        serde_json::from_slice(&raw).map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let declared: NaiveDate = serde_json::from_value(universe["session"].clone())
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let instruments: Vec<Instrument> = serde_json::from_value(universe["instruments"].clone())
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    if declared != session || instruments.is_empty() || instruments.len() > 10_000 {
        return Err("candidate_quotes_unavailable".into());
    }
    let universe_sha256 = digest(&raw);
    let db = if immutable {
        // A CLOSED predecessor book is read without creating `-wal`/`-shm`
        // sidecars. A non-empty WAL is refused by the immutable opener.
        super::super::open_book_immutable(&source.join("paper.sqlite"))
            .map_err(|_| "candidate_quotes_unavailable".to_string())?
    } else {
        Connection::open_with_flags(
            source.join("paper.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| "candidate_quotes_unavailable".to_string())?
    };
    db.pragma_update(None, "query_only", true)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    // Purpose and financial receipt window are filtered before the bounded row
    // limit. The old reader ordered by insert id and applied the limit first,
    // so a late read could miss the first eligible receipt in the window.
    // Research-observation receipts have their own tick event (#240), so
    // trading retention never compacts them; receipts written before that
    // split stay readable in `wide_quotes`. Trading purposes never read the
    // research event.
    let purpose_filter = purposes
        .iter()
        .map(|purpose| {
            let events = match purpose {
                QuotePurposeV1::ResearchObservation => {
                    format!(
                        "'{}','wide_quotes'",
                        super::super::wide::RESEARCH_QUOTES_EVENT
                    )
                }
                QuotePurposeV1::Protection | QuotePurposeV1::Screen => "'wide_quotes'".into(),
            };
            format!(
                "(event IN ({events}) AND instr(body,'\"purpose\":\"{}\"')>0)",
                purpose.archive_str()
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    let order = if newest_first {
        "receipt DESC,id DESC"
    } else {
        "receipt ASC,id ASC"
    };
    let sql = format!(
        "SELECT at,receipt,body FROM ticks WHERE receipt IS NOT NULL AND receipt>0 AND receipt>=?1 AND receipt<=?2 AND ({purpose_filter}) ORDER BY {order} LIMIT ?3"
    );
    let mut statement = db
        .prepare(&sql)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let rows = statement
        .query_map(
            rusqlite::params![from_ms, to_ms, (max_rows + 1) as i64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let mut receipt_rows = Vec::new();
    for row in rows {
        receipt_rows.push(row.map_err(|_| "candidate_quotes_unavailable".to_string())?);
    }
    let truncated = receipt_rows.len() > max_rows;
    receipt_rows.truncate(max_rows);
    // A compacted row has no readable body, but its original receipt time and
    // archive SHA are retained so a first-eligible exit can never be assumed.
    // Entry freeze needs the NEWEST compacted receipt (it may hide the latest
    // entry); exit needs the OLDEST (it may hide the first exit). Ordering both
    // ascending would hide a newer compacted entry beyond the row limit.
    let compacted_order = if newest_first {
        "receipt DESC,id DESC"
    } else {
        "receipt ASC,id ASC"
    };
    let compacted_sql = format!(
        "SELECT at,receipt,body FROM ticks WHERE event='wide_quotes' AND receipt IS NOT NULL AND receipt>0 AND receipt>=?1 AND receipt<=?2 AND instr(body,'\"kind\":\"compacted_quote_receipt\"')>0 ORDER BY {compacted_order} LIMIT ?3"
    );
    let mut compacted_statement = db
        .prepare(&compacted_sql)
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let compacted_rows = compacted_statement
        .query_map(rusqlite::params![from_ms, to_ms, max_rows as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|_| "candidate_quotes_unavailable".to_string())?;
    let mut compacted = Vec::new();
    for row in compacted_rows {
        let (at, receipt, raw) = row.map_err(|_| "candidate_quotes_unavailable".to_string())?;
        let archive: Value =
            serde_json::from_str(&raw).map_err(|_| "candidate_quotes_invalid".to_string())?;
        compacted.push(json!({
            "key":null,
            "source_ms":at,
            "receipt_ms":receipt,
            "event_at_ms":at,
            "available_at_ms":receipt,
            "purpose":null,
            "evidence_status":"compacted",
            // Only a recorded original archive SHA is provenance. A digest of
            // the compacted body is NOT the original receipt and is never
            // substituted; an unrecorded SHA stays explicitly null.
            "archive_sha256":archive["sha256"].as_str().map(str::to_owned)
        }));
    }
    let mut quotes = Vec::new();
    for (at, receipt, raw) in receipt_rows {
        // The replayable view is bounded by the caller's authoritative clock.
        // A later archived event is NOT consumed and does not poison an
        // otherwise valid earlier receipt: it is simply not yet knowable.
        if at > now {
            continue;
        }
        if receipt > at {
            return Err("candidate_quotes_receipt_binding".into());
        }
        if raw.len() > 2_000_000 {
            return Err("candidate_quotes_unavailable".into());
        }
        let archive: Value =
            serde_json::from_str(&raw).map_err(|_| "candidate_quotes_invalid".to_string())?;
        let purpose = QuotePurposeV1::from_archive(archive["purpose"].as_str())
            .ok_or("candidate_quotes_purpose_unknown")?;
        if !purposes.contains(&purpose) {
            continue;
        }
        if archive["received_ms"] != receipt {
            return Err("candidate_quotes_receipt_binding".into());
        }
        let body: Value = serde_json::from_str(
            archive["body"]
                .as_str()
                .ok_or("candidate_quotes_not_receipt")?,
        )
        .map_err(|_| "candidate_quotes_invalid".to_string())?;
        if body["status"] != "success" {
            return Err("candidate_quotes_status".into());
        }
        let archive_sha256 = digest(raw.as_bytes());
        let rows = body["data"].as_object().ok_or("candidate_quotes_rows")?;
        for row in rows.values() {
            let Some(instrument) = instruments
                .iter()
                .find(|instrument| row["instrument_token"] == instrument.key)
            else {
                continue;
            };
            if !keys.contains(&instrument.key) {
                continue;
            }
            let Ok(quote) = data::parse_row(
                row,
                receipt,
                &instrument.key,
                &instrument.symbol,
                instrument.tick,
            ) else {
                continue;
            };
            let (Some(lower), Some(upper), Some(volume)) = (
                row["lower_circuit_limit"].as_number(),
                row["upper_circuit_limit"].as_number(),
                row["volume"].as_u64(),
            ) else {
                continue;
            };
            quotes.push(json!({
                "key":instrument.key,
                // The original quote source clock comes from the archived body;
                // `at` is the actual completion/event clock, which may be later
                // than the data receipt. They are never conflated.
                "source_ms":quote.source,
                "receipt_ms":receipt,
                "event_at_ms":at,
                "available_at_ms":receipt,
                "bid":quote.bid,
                "ask":quote.ask,
                "bid_units":quote.bid_units,
                "ask_units":quote.ask_units,
                "lower_circuit_limit":lower.to_string(),
                "upper_circuit_limit":upper.to_string(),
                "volume":volume,
                "purpose":purpose.archive_str(),
                "evidence_status":"ok",
                "archive_sha256":archive_sha256,
                "symbol":instrument.symbol,
                "series":instrument.series,
                "tick":instrument.tick.amount().to_string(),
                "universe_sha256":universe_sha256
            }));
        }
    }
    let coverage = if quotes.is_empty() {
        if !compacted.is_empty() {
            "compacted_only"
        } else {
            "missing_quotes_in_window"
        }
    } else {
        "quotes_present"
    };
    Ok(json!({
        "quotes":quotes,
        "compacted":compacted,
        "coverage":coverage,
        "truncated":truncated,
        "interpolated":false,
        "universe_sha256":universe_sha256,
        "session":session.to_string(),
        "window":{"from_ms":from_ms,"to_ms":to_ms},
        "purposes":purposes.iter().map(|purpose| purpose.as_str()).collect::<Vec<_>>()
    }))
}
