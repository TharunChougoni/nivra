use super::*;

use nivra_store::market_archive::{MAX_QUOTE_BODY_BYTES, QuoteBodies};

#[derive(Clone, Copy)]
pub(crate) struct QuoteSource<'a> {
    pub(super) directory: &'a Path,
    archive: Option<&'a MarketArchiveSettings>,
}

impl<'a> QuoteSource<'a> {
    pub(crate) fn new(directory: &'a Path, archive: Option<&'a MarketArchiveSettings>) -> Self {
        Self { directory, archive }
    }

    pub(super) fn rows(
        self,
        mut rows: impl Iterator<Item = rusqlite::Result<QuoteRow>>,
    ) -> impl Iterator<Item = std::result::Result<QuoteRow, String>> {
        // Bound decompression independently of the caller's total row limit.
        const PAGE_SIZE: usize = 8;
        let mut bodies = QuoteBodies::default();
        let mut page = Vec::new().into_iter();
        std::iter::from_fn(move || {
            if let Some(row) = page.next() {
                return Some(row);
            }
            let mut times = Vec::with_capacity(PAGE_SIZE);
            let mut raw = Vec::with_capacity(PAGE_SIZE);
            let mut journal_error = None;
            for row in rows.by_ref().take(PAGE_SIZE) {
                match row {
                    Ok((at, received, body)) => {
                        times.push((at, received));
                        raw.push((received, body));
                    }
                    Err(error) => {
                        journal_error = Some(Err(snapshot_unavailable(error)));
                        break;
                    }
                }
            }
            let resolved = if cfg!(test) && self.archive.is_none() {
                raw.into_iter().map(|(_, body)| Ok(body)).collect()
            } else {
                bodies.resolve_page(raw, || {
                    super::super::market_archive::connect(
                        self.archive.ok_or("archive_config_missing")?,
                    )
                })
            };
            page = times
                .into_iter()
                .zip(resolved)
                .map(|((at, received), body)| {
                    body.map(|body| (at, received, body)).map_err(|error| {
                        let code = failure_code(error.as_ref());
                        if code.starts_with("archive_") {
                            code
                        } else {
                            "archive_quote_read_failed".into()
                        }
                    })
                })
                .chain(journal_error)
                .collect::<Vec<_>>()
                .into_iter();
            page.next()
        })
    }
}

pub(super) type QuoteRow = (i64, Option<i64>, String);

impl Config {
    pub(crate) fn quote_source(&self) -> QuoteSource<'_> {
        QuoteSource::new(&self.directory, self.market_archive.as_ref())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum QuotePurposeV1 {
    Protection,
    Screen,
}
impl QuotePurposeV1 {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Protection => "protection",
            Self::Screen => "screen",
        }
    }
    pub(super) fn archive_str(self) -> &'static str {
        match self {
            Self::Protection => "execution_or_protection",
            Self::Screen => "scanner",
        }
    }
    pub(super) fn from_archive(value: Option<&str>) -> Option<Self> {
        match value {
            Some("scanner" | "scanner_metadata") => Some(Self::Screen),
            Some("execution_or_protection") => Some(Self::Protection),
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
const SNAPSHOT_UNAVAILABLE: &str = "scorer_snapshots_unavailable";

fn snapshot_unavailable<E>(_: E) -> String {
    SNAPSHOT_UNAVAILABLE.to_owned()
}

/// Quote receipts whose original source observation is at or before the host
/// eligibility clock. A future row can never enter a candidate dataset or a
/// maturation window.
pub(crate) fn quote_receipts_bounded(
    source: QuoteSource<'_>,
    session: NaiveDate,
    max_rows: usize,
    max_source_ms: i64,
) -> std::result::Result<QuoteEvidence, String> {
    let mut receipts = Vec::new();
    let instruments = visit_quote_receipts(
        source,
        session,
        max_rows,
        max_source_ms,
        &|| None,
        |receipt| {
            receipts.push(receipt);
            Ok(())
        },
    )?;
    if receipts.is_empty() {
        return Err(SNAPSHOT_UNAVAILABLE.into());
    }
    Ok(QuoteEvidence {
        instruments,
        receipts,
    })
}

fn visit_quote_receipts(
    source: QuoteSource<'_>,
    session: NaiveDate,
    max_rows: usize,
    max_source_ms: i64,
    stop: &dyn Fn() -> Option<&'static str>,
    mut visit: impl FnMut(QuoteReceipt) -> std::result::Result<(), String>,
) -> std::result::Result<Vec<Instrument>, String> {
    check_tape_stop(stop)?;
    if !source.directory.is_absolute() || !(1..=10_000).contains(&max_rows) || max_source_ms < 0 {
        return Err(SNAPSHOT_UNAVAILABLE.into());
    }
    let instruments = session_universe(source.directory, session)?;
    let db = open_quote_journal(source.directory)?;
    let mut statement = db
        .prepare(
            "SELECT at,receipt,body FROM ticks WHERE id IN (
                SELECT id FROM ticks WHERE event IN ('wide_quotes','scanner_metadata','archive_quote_gap')
                AND at<=?2 ORDER BY id DESC LIMIT ?1
            ) ORDER BY id ASC",
        )
        .map_err(snapshot_unavailable)?;
    let rows = statement
        .query_map(rusqlite::params![max_rows as i64, max_source_ms], quote_row)
        .map_err(snapshot_unavailable)?;
    let index = instruments
        .iter()
        .map(|instrument| (instrument.key.as_str(), instrument))
        .collect::<BTreeMap<_, _>>();
    let mut previous = None;
    for row in source.rows(rows) {
        check_tape_stop(stop)?;
        let (at, receipt, raw) = row?;
        let receipt = archived_receipt(&index, at, receipt, &raw, max_source_ms)?;
        if previous.is_some_and(|prior| at <= prior) {
            return Err("scorer_snapshots_chronology".into());
        }
        previous = Some(at);
        visit(receipt)?;
    }
    Ok(instruments)
}

pub(super) fn quote_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<QuoteRow> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
}

/// The session's frozen cash universe from `wide-universe.json`.
pub(super) fn session_universe(
    source: &Path,
    session: NaiveDate,
) -> std::result::Result<Vec<Instrument>, String> {
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
    Ok(instruments)
}

/// The book's quote journal, opened read-only.
pub(super) fn open_quote_journal(source: &Path) -> std::result::Result<Connection, String> {
    let db = Connection::open_with_flags(
        source.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    db.pragma_update(None, "query_only", true)
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    db.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, 2_000_000)
        .map_err(|_| "scorer_snapshots_unavailable".to_string())?;
    Ok(db)
}

/// One archived quote receipt as observations, parsed from its original body
/// with its original receipt time. Rows outside the universe or without a
/// parseable quote and circuit limits are left out, never repaired.
pub(super) fn archived_receipt(
    index: &BTreeMap<&str, &Instrument>,
    at: i64,
    receipt: Option<i64>,
    raw: &str,
    max_source_ms: i64,
) -> std::result::Result<QuoteReceipt, String> {
    archived_receipt_filtered(index, at, receipt, raw, max_source_ms, None)
}

fn archived_receipt_filtered(
    index: &BTreeMap<&str, &Instrument>,
    at: i64,
    receipt: Option<i64>,
    raw: &str,
    max_source_ms: i64,
    only_keys: Option<&BTreeSet<String>>,
) -> std::result::Result<QuoteReceipt, String> {
    if raw.len() > MAX_QUOTE_BODY_BYTES {
        return Err("scorer_snapshots_unavailable".into());
    }
    let receipt = receipt.ok_or("scorer_snapshots_receipt_missing")?;
    let archive: Value =
        serde_json::from_str(raw).map_err(|_| "scorer_snapshots_invalid".to_string())?;
    if archive["received_ms"] != receipt
        || receipt > at
        || at > max_source_ms
        || receipt > max_source_ms
    {
        return Err("scorer_snapshots_receipt_binding".into());
    }
    let kind = QuotePurposeV1::from_archive(archive["purpose"].as_str())
        .ok_or("scorer_snapshots_purpose_unknown")?;
    let body: Value = serde_json::from_str(
        archive["body"]
            .as_str()
            .ok_or("scorer_snapshots_not_quote_receipt")?,
    )
    .map_err(|_| "scorer_snapshots_invalid".to_string())?;
    if body["status"] != "success" {
        return Err("scorer_snapshots_quote_status".into());
    }
    let rows = body["data"]
        .as_object()
        .ok_or("scorer_snapshots_quote_rows")?;
    // The ORIGINAL archived receipt SHA for every observation. It is never
    // the whole-tape digest and never another key's SHA.
    let mut archive_sha256: Option<String> = None;
    let mut observations = Vec::new();
    for row in rows.values() {
        let Some(token) = row["instrument_token"].as_str() else {
            continue;
        };
        if only_keys.is_some_and(|keys| !keys.contains(token)) {
            continue;
        }
        let Some(instrument) = index.get(token) else {
            continue;
        };
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
        let sha = archive_sha256.get_or_insert_with(|| nivra_core::replay::digest(raw.as_bytes()));
        observations.push(json!({
            "key":instrument.key,"quote":quote,
            "lower_circuit_limit":lower.to_string(),"upper_circuit_limit":upper.to_string(),"volume":volume,
            "archive_sha256":sha,
            "purpose":kind.archive_str()
        }));
    }
    Ok(QuoteReceipt {
        at_ms: at,
        receipt_ms: receipt,
        kind,
        observations,
    })
}

/// Frozen executable quote tape for the optional after-cost cash replay.
pub(crate) struct ExecutionTape {
    pub manifest: Vec<u8>,
    pub events: Vec<u8>,
    pub events_sha256: String,
}

#[derive(Clone, Copy)]
pub(crate) struct TapeLimits {
    pub max_quote_events: usize,
    pub max_observations: usize,
    pub max_tape_bytes: u64,
}

pub(super) fn check_tape_stop(
    stop: &dyn Fn() -> Option<&'static str>,
) -> std::result::Result<(), String> {
    stop().map_or(Ok(()), |reason| Err(reason.into()))
}

pub(crate) fn execution_tape_bounded_with_stop(
    source: QuoteSource<'_>,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    capital: Amount,
    limits: TapeLimits,
    now: i64,
    stop: &dyn Fn() -> Option<&'static str>,
) -> std::result::Result<ExecutionTape, String> {
    let mut builder = ExecutionTapeBuilder::new(capital, limits)?;
    let instruments = visit_quote_receipts(
        source,
        session,
        limits.max_quote_events,
        now,
        stop,
        |receipt| builder.push(receipt),
    )?;
    check_tape_stop(stop)?;
    if builder.events.is_empty() {
        return Err(SNAPSHOT_UNAVAILABLE.into());
    }
    builder.finish(
        session,
        previous_session,
        instruments,
        json!({
            "source":"original_paper_quote_journal_read_only"
        }),
        true,
    )
}

pub(super) struct ExecutionTapeBuilder {
    capital: Amount,
    limits: TapeLimits,
    events: Vec<u8>,
    observations: usize,
    protection_receipts: usize,
    protection_observations: usize,
    screen_receipts: usize,
    max_gap_ms: i64,
    previous_receipt_ms: Option<i64>,
}

impl ExecutionTapeBuilder {
    pub(super) fn new(capital: Amount, limits: TapeLimits) -> std::result::Result<Self, String> {
        if capital <= Amount::ZERO
            || !(1..=10_000).contains(&limits.max_quote_events)
            || !(1..=100_000).contains(&limits.max_observations)
            || !(1..=16 * 1024 * 1024).contains(&limits.max_tape_bytes)
        {
            return Err("candidate_execution_tape_invalid".into());
        }
        Ok(Self {
            capital,
            limits,
            events: Vec::new(),
            observations: 0,
            protection_receipts: 0,
            protection_observations: 0,
            screen_receipts: 0,
            max_gap_ms: 0,
            previous_receipt_ms: None,
        })
    }

    pub(super) fn push(&mut self, receipt: QuoteReceipt) -> std::result::Result<(), String> {
        if let Some(previous) = self.previous_receipt_ms {
            self.max_gap_ms = self.max_gap_ms.max(receipt.at_ms.saturating_sub(previous));
        }
        self.previous_receipt_ms = Some(receipt.at_ms);
        self.observations += receipt.observations.len();
        if self.observations > self.limits.max_observations {
            return Err("candidate_execution_tape_too_large".into());
        }
        if receipt.kind == QuotePurposeV1::Protection {
            self.protection_receipts += 1;
            self.protection_observations += receipt.observations.len();
        } else {
            self.screen_receipts += 1;
        }
        serde_json::to_writer(
            &mut self.events,
            &json!({
                "at":receipt.at_ms,
                "available_at":receipt.receipt_ms,
                "kind":receipt.kind.as_str(),
                "observations":receipt.observations
            }),
        )
        .map_err(|_| "candidate_execution_tape_invalid".to_string())?;
        self.events.push(b'\n');
        if self.events.len() as u64 > self.limits.max_tape_bytes {
            return Err("candidate_execution_tape_too_large".into());
        }
        Ok(())
    }

    pub(super) fn finish(
        self,
        session: NaiveDate,
        previous_session: Option<NaiveDate>,
        instruments: Vec<Instrument>,
        mut provenance: Value,
        require_protection: bool,
    ) -> std::result::Result<ExecutionTape, String> {
        if require_protection && self.protection_observations == 0 {
            return Err("candidate_execution_quotes_unavailable".into());
        }
        let events_sha256 = nivra_core::replay::digest(&self.events);
        let previous_session_verified = previous_session.is_some();
        let previous_session = previous_session.unwrap_or(
            session
                .pred_opt()
                .ok_or("candidate_execution_tape_invalid")?,
        );
        provenance["research_capital_assumption"] = json!(true);
        provenance["research_costs_assumption"] = json!(true);
        provenance["account_funds_claim"] = json!(false);
        provenance["legacy_quote_purpose_unknown"] = json!(0);
        provenance["protection_receipts"] = json!(self.protection_receipts);
        provenance["screen_receipts"] = json!(self.screen_receipts);
        provenance["previous_session_semantics"] = json!(if previous_session_verified {
            "verified_calendar_trading_day"
        } else {
            "calendar_day_not_verified_trading_day"
        });
        provenance["max_observation_gap_ms_semantics"] =
            json!("observed_max_receipt_gap_not_an_engine_tolerance");
        let manifest = serde_json::to_vec(&json!({
            "schema":1,
            "session":session,
            "previous_session":previous_session,
            "capital":self.capital,
            "universe":instruments,
            "events_sha256":events_sha256,
            "synthetic":false,
            "max_observation_gap_ms":self.max_gap_ms.max(1),
            "paper_costs":null,
            "provenance":provenance
        }))
        .map_err(|_| "candidate_execution_tape_invalid".to_string())?;
        if manifest.len() > 512 * 1024 {
            return Err("candidate_execution_tape_too_large".into());
        }
        nivra_cli::strategy_archive::validate_archive(&manifest, &self.events)?;
        Ok(ExecutionTape {
            events_sha256,
            manifest,
            events: self.events,
        })
    }
}

/// Candidate freeze path: only quote receipts whose original source time is at
/// or before the submitted host clock enter the dataset. Later ticks cannot
/// mutate the frozen bytes because the caller stores them before admission.
pub(crate) fn session_feature_snapshots(
    source: QuoteSource<'_>,
    session: NaiveDate,
    previous_session: Option<NaiveDate>,
    max_snapshots: usize,
    max_age_ms: i64,
    lookback_samples: usize,
    now: i64,
) -> std::result::Result<Vec<nivra_core::strategy::FeatureSnapshot>, String> {
    if !source.directory.is_absolute()
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
        "synthetic":false,"max_observation_gap_ms":max_gap_ms.max(1),
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
    source: QuoteSource<'_>,
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
    let instruments = session_universe(source.directory, session)?;
    let db = open_quote_journal(source.directory)?;
    let end_of_evidence_ms: Option<i64> = db
        .query_row(
            "SELECT MAX(at) FROM ticks WHERE event IN ('wide_quotes','scanner_metadata','archive_quote_gap') AND at<=?1",
            rusqlite::params![now],
            |row| row.get(0),
        )
        .map_err(snapshot_unavailable)?;
    let Some(end_of_evidence_ms) = end_of_evidence_ms else {
        return Err(SNAPSHOT_UNAVAILABLE.into());
    };
    let index = instruments
        .iter()
        .map(|instrument| (instrument.key.as_str(), instrument))
        .collect::<BTreeMap<_, _>>();
    let mut statement = db
        .prepare(
            "SELECT at,receipt,body FROM ticks WHERE event IN ('wide_quotes','scanner_metadata','archive_quote_gap') AND at<=?1 AND (receipt IS NULL OR (receipt>=?2 AND receipt<=?3)) ORDER BY id ASC",
        )
        .map_err(snapshot_unavailable)?;
    let rows = statement
        .query_map(rusqlite::params![now, from_ms, to_ms], quote_row)
        .map_err(snapshot_unavailable)?;
    let mut quotes = Vec::new();
    let mut truncated = false;
    let mut previous = None;
    for row in source.rows(rows) {
        let (at, receipt, raw) = row?;
        if previous.is_some_and(|prior| at <= prior) {
            return Err("scorer_snapshots_chronology".into());
        }
        previous = Some(at);
        let receipt = archived_receipt_filtered(&index, at, receipt, &raw, now, Some(keys))?;
        for observation in &receipt.observations {
            let Some(key) = observation["key"].as_str() else {
                continue;
            };
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_accepts_a_large_envelope_of_a_bounded_provider_body() {
        // Escaping the original JSON when it is nested in the receipt adds bytes.
        let body = json!({"status":"success","data":{},"padding":"\"".repeat(700_000)}).to_string();
        let envelope =
            json!({"received_ms":1000,"purpose":"scanner_metadata","body":body}).to_string();
        assert!(body.len() < 2_000_000 && envelope.len() > 2_000_000);
        let quote = archived_receipt(&BTreeMap::new(), 1001, Some(1000), &envelope, 1001).unwrap();
        assert_eq!(quote.receipt_ms, 1000);
    }
}
