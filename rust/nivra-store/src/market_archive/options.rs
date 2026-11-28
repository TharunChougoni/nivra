//! Bounded option evidence. A poll stores the whole requested set together,
//! including empty touches, so replay can distinguish a gap from a price.
use super::{Client, Result};
use chrono::NaiveDate;
use nivra_core::{execution::Quote, finance::Price, market::OptionContract};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptionSpot {
    pub key: String,
    pub price: Price,
    pub source_ms: i64,
    pub receipt_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ContractMaster {
    pub contracts: Vec<OptionContract>,
    pub spot: OptionSpot,
    pub contract_sha256: String,
    pub bod_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptionObservation {
    pub instrument_key: String,
    pub source_ms: Option<i64>,
    pub bid: Option<(Price, u64)>,
    pub ask: Option<(Price, u64)>,
    pub ltp: Option<Price>,
    pub oi: Option<u64>,
    /// The same validated two-sided touch the scanner admits.
    pub quote: Option<Quote>,
    pub gap: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OptionPoll {
    pub requested_ms: i64,
    pub spot: Option<OptionSpot>,
    pub rows: Vec<OptionObservation>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GateCandidate {
    pub instrument_key: String,
    pub quote: Quote,
    pub offset: i64,
    pub spread_ticks: u64,
    pub depth_lots: u64,
    pub round_trip_cost_bps: f64,
    pub tradable: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GateObservation {
    pub spot: Option<OptionSpot>,
    pub candidates: Vec<GateCandidate>,
    /// Exact chain and triggers shown to the gate, with effective thresholds.
    pub input: Value,
    /// The recorded gate answer, with validated rank and dropped rows.
    pub verdict: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OptionGap {
    pub reason: String,
}

#[derive(Debug)]
pub enum OptionEvidence {
    Master(ContractMaster),
    Poll(OptionPoll),
    Held(OptionPoll),
    Gate(GateObservation),
    Gap(OptionGap),
    CaptureGap(OptionGap),
}

pub fn write_option_evidence(
    db: &mut Client,
    session: NaiveDate,
    at_ms: i64,
    evidence: &OptionEvidence,
) -> Result<()> {
    let (quotes, missing_quotes, has_spot) = match evidence {
        OptionEvidence::Poll(poll) | OptionEvidence::Held(poll) => {
            let quotes = poll.rows.iter().filter(|row| row.quote.is_some()).count();
            (
                i32::try_from(quotes)?,
                i32::try_from(poll.rows.len() - quotes)?,
                poll.spot.is_some(),
            )
        }
        _ => (0, 0, false),
    };
    let (kind, body) = match evidence {
        OptionEvidence::Master(v) => ("master", serde_json::to_value(v)?),
        OptionEvidence::Poll(v) => ("poll", serde_json::to_value(v)?),
        OptionEvidence::Held(v) => ("held", serde_json::to_value(v)?),
        OptionEvidence::Gate(v) => ("gate", serde_json::to_value(v)?),
        OptionEvidence::Gap(v) => ("gap", serde_json::to_value(v)?),
        OptionEvidence::CaptureGap(v) => ("capture_gap", serde_json::to_value(v)?),
    };
    let inserted = db.execute(
        "INSERT INTO option_evidence (session,at_ms,kind,body,quotes,missing_quotes,has_spot)
         VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",
        &[
            &session,
            &at_ms,
            &kind,
            &body,
            &quotes,
            &missing_quotes,
            &has_spot,
        ],
    )?;
    if inserted == 0
        && !db
            .query_one(
                "SELECT body=$4 FROM option_evidence WHERE session=$1 AND at_ms=$2 AND kind=$3",
                &[&session, &at_ms, &kind, &body],
            )?
            .get::<_, bool>(0)
    {
        return Err("archive_option_evidence_conflict".into());
    }
    Ok(())
}

pub const MAX_GATE_OBSERVATIONS: usize = 128;

#[derive(Debug, Serialize)]
pub struct OptionCoverage {
    pub masters: i64,
    pub polls: i64,
    pub gates: i64,
    pub gaps: i64,
    pub quotes: i64,
    pub missing_quotes: i64,
    pub polls_without_spot: i64,
}

pub fn option_coverage(db: &mut Client, session: NaiveDate) -> Result<OptionCoverage> {
    let row = db.query_one(
        "SELECT count(*) FILTER (WHERE kind='master'), count(*) FILTER (WHERE kind='poll'),
                count(*) FILTER (WHERE kind='gate'),
                count(*) FILTER (WHERE kind IN ('gap','capture_gap')),
                coalesce(sum(quotes), 0), coalesce(sum(missing_quotes), 0),
                count(*) FILTER (WHERE kind='poll' AND NOT has_spot)
         FROM option_evidence WHERE session=$1",
        &[&session],
    )?;
    Ok(OptionCoverage {
        masters: row.get(0),
        polls: row.get(1),
        gates: row.get(2),
        gaps: row.get(3),
        quotes: row.get(4),
        missing_quotes: row.get(5),
        polls_without_spot: row.get(6),
    })
}

/// The visible tail of a session when its research cohort was sealed.
pub fn option_watermark(db: &mut Client, session: NaiveDate) -> Result<i64> {
    Ok(db
        .query_one(
            "SELECT coalesce(max(id),0) FROM option_evidence WHERE session=$1",
            &[&session],
        )?
        .get(0))
}

/// One sealed session. Never silently truncate a cohort.
pub fn option_gates(
    db: &mut Client,
    session: NaiveDate,
    watermark: i64,
) -> Result<Vec<(i64, GateObservation)>> {
    let rows = db.query(
        "SELECT at_ms,body FROM option_evidence WHERE session=$1 AND kind='gate' AND id <= $2
         ORDER BY at_ms LIMIT $3",
        &[&session, &watermark, &((MAX_GATE_OBSERVATIONS + 1) as i64)],
    )?;
    if rows.len() > MAX_GATE_OBSERVATIONS {
        return Err("option_gate_cohort_too_large".into());
    }
    rows.into_iter()
        .map(|row| Ok((row.get(0), serde_json::from_value(row.get(1))?)))
        .collect()
}

pub fn option_master(
    db: &mut Client,
    session: NaiveDate,
    observed_ms: i64,
    watermark: i64,
) -> Result<Option<ContractMaster>> {
    db.query_opt(
        "SELECT body FROM option_evidence WHERE session=$1 AND kind='master' AND at_ms <= $2 AND id <= $3
         ORDER BY at_ms DESC LIMIT 1",
        &[&session, &observed_ms, &watermark],
    )?
    .map(|row| Ok(serde_json::from_value(row.get(0))?))
    .transpose()
}

/// Read only the causal entry/exit neighborhood. A gap remains visible.
pub fn option_window(
    db: &mut Client,
    session: NaiveDate,
    from_ms: i64,
    to_ms: i64,
    watermark: i64,
) -> Result<Option<Vec<(i64, OptionPoll)>>> {
    const LIMIT: i64 = 64;
    let rows = db.query(
        "SELECT at_ms, kind, body FROM option_evidence
         WHERE session=$1 AND kind IN ('poll','gap','capture_gap') AND at_ms BETWEEN $2 AND $3 AND id <= $4
         ORDER BY at_ms,kind LIMIT $5",
        &[&session, &from_ms, &to_ms, &watermark, &(LIMIT + 1)],
    )?;
    if rows.len() as i64 > LIMIT || rows.iter().any(|row| row.get::<_, &str>(1) != "poll") {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter()
            .map(|row| Ok((row.get(0), serde_json::from_value(row.get(2))?)))
            .collect::<Result<_>>()?,
    ))
}
