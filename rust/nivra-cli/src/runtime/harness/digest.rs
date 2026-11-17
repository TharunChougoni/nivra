//! Owner factual digest schema, bounds checks and eligibility helpers.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Strict decode, plus one narrow repair (#40): a reply that closed its object
/// early and then continued with `,"key":...` on a new line. The repaired text
/// must still decode strictly and pass every digest bound below.
fn decode_digest(raw: &str) -> Result<Value> {
    if let Ok(v) = nivra_core::native::decode(raw.as_bytes()) {
        return Ok(v);
    }
    let trimmed = raw.trim();
    for sep in ["}\n,", "}\r\n,"] {
        if trimmed.matches(sep).count() == 1
            && let Some((head, tail)) = trimmed.split_once(sep)
        {
            let joined = format!("{head},{tail}");
            if let Ok(v) = nivra_core::native::decode(joined.as_bytes()) {
                eprintln!("digest_repair reason=premature_object_close");
                return Ok(v);
            }
        }
    }
    Err("harness_digest_json_invalid".into())
}
/// The owner policy's factual digest. It deliberately has no handoff, thesis
/// or holding opinion: the host wakes the persistent orchestrator for every
/// validated, usable digest and the orchestrator owns any later decision.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerDigest {
    complete: bool,
    as_of_ms: i64,
    coverage: OwnerCoverage,
    /// Short prose direction for the decision owner: what deserves attention and
    /// why, in words. The owner reads this and fetches its own detail with tools.
    #[serde(default)]
    brief: String,
    #[serde(default)]
    gaps: Vec<String>,
    #[serde(default)]
    changed_prospects: Vec<OwnerProspect>,
    #[serde(default)]
    active_rankings: Vec<OwnerRanking>,
    #[serde(default)]
    candidate_rankings: Vec<OwnerRanking>,
    #[serde(default)]
    bar_context: Vec<OwnerBar>,
    #[serde(default)]
    spread_depth: Vec<OwnerSpread>,
    #[serde(default)]
    holdings: Vec<OwnerHolding>,
    cash: OwnerCash,
    fees: OwnerFees,
    #[serde(default)]
    recent_actions: Vec<OwnerAction>,
    #[serde(default)]
    experiments: Vec<OwnerExperiment>,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerCoverage {
    universe: usize,
    scanned: usize,
    quoted: usize,
    featured: usize,
    experiments: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerProspect {
    instrument_key: String,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    change_bps: Option<f64>,
    #[serde(default)]
    observed_ms: Option<i64>,
    reason: String,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerRanking {
    instrument_key: String,
    rank: u32,
    #[serde(default)]
    score: Option<f64>,
    source: String,
    #[serde(default)]
    as_of_ms: Option<i64>,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerBar {
    instrument_key: String,
    complete: bool,
    #[serde(default)]
    last_bar_ms: Option<i64>,
    #[serde(default)]
    return_1m_bps: Option<f64>,
    #[serde(default)]
    return_5m_bps: Option<f64>,
    #[serde(default)]
    relative_return_5m_bps: Option<f64>,
    source: String,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerSpread {
    instrument_key: String,
    #[serde(default)]
    bid: Option<String>,
    #[serde(default)]
    ask: Option<String>,
    #[serde(default)]
    bid_units: Option<u64>,
    #[serde(default)]
    ask_units: Option<u64>,
    #[serde(default)]
    spread_bps: Option<f64>,
    #[serde(default)]
    quote_age_ms: Option<i64>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerHolding {
    instrument_key: String,
    units: u64,
    entry: String,
    mark: String,
    #[serde(default)]
    exit_intent: Option<Value>,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerCash {
    available: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerFees {
    per_order: String,
    buy_bps: String,
    sell_bps: String,
    #[serde(default)]
    slippage_bps: Option<String>,
    #[serde(default)]
    rounding_increment: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerAction {
    at_ms: i64,
    action: String,
    #[serde(default)]
    instrument_key: Option<String>,
    #[serde(default)]
    decision_id: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    units: Option<u64>,
    #[serde(default)]
    price: Option<String>,
    #[serde(default)]
    fee: Option<String>,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerExperiment {
    analysis_id: String,
    #[serde(default)]
    dataset_id: Option<String>,
    #[serde(default)]
    input_sha256: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    metric: Option<String>,
    #[serde(default)]
    value: Option<f64>,
    #[serde(default)]
    evidence_ids: Vec<String>,
}

const MAX_OWNER_ROWS: usize = 32;
const MAX_OWNER_GAPS: usize = 16;
const MAX_OWNER_TEXT: usize = 300;
/// Luna's prose direction for the decision owner. The owner fetches detail with
/// its own tools; the brief is never a data dump.
const MAX_OWNER_BRIEF: usize = 2_000;
const MAX_OWNER_SOURCE: usize = 128;
const MAX_OWNER_COVERAGE: usize = 1_000_000;
const MAX_OWNER_RANK: u32 = 1_000_000;
const MAX_OWNER_ROW_EVIDENCE_IDS: usize = 4;

fn owner_evidence_ids(ids: &[String]) -> bool {
    ids.len() <= MAX_OWNER_ROW_EVIDENCE_IDS
        && ids.iter().all(|id| !id.is_empty() && id.len() <= 256)
}

fn owner_text(text: &str, max: usize) -> bool {
    !text.trim().is_empty() && text.chars().count() <= max
}

fn owner_finite(value: Option<f64>) -> bool {
    value.is_none_or(|value| value.is_finite())
}

fn owner_ranking_row_ok(row: &OwnerRanking) -> bool {
    row.rank != 0
        && row.rank <= MAX_OWNER_RANK
        && owner_text(&row.source, MAX_OWNER_SOURCE)
        && owner_finite(row.score)
        && row.as_of_ms.is_none_or(|at| at >= 0)
        && owner_evidence_ids(&row.evidence_ids)
}

/// The same instrument can legitimately appear both as a scanner prospect and
/// as a collapsed price-move trigger (#220). Deduplicate changed prospects by
/// instrument key before the hard bounds check: one duplicate advisory row must
/// not discard the entire factual digest. The first occurrence is kept;
/// duplicates are dropped and counted. Every retained row still passes the
/// allowlist, text, finite-value, observed-time and evidence-id bounds.
fn normalize_owner_changed_prospects(v: &mut OwnerDigest) -> usize {
    let mut seen = BTreeSet::new();
    let before = v.changed_prospects.len();
    v.changed_prospects
        .retain(|row| seen.insert(row.instrument_key.clone()));
    before - v.changed_prospects.len()
}

/// Drop invalid or duplicate advisory ranking rows before the hard bounds
/// check. One malformed row must not discard the entire factual digest.
fn normalize_owner_rankings(v: &mut OwnerDigest, bundle: &Value) -> usize {
    let allowlisted = |key: &str| {
        bundle.is_null()
            || bundle["allowlist"]
                .as_array()
                .is_some_and(|allow| allow.iter().any(|item| item["key"] == key))
    };
    let mut dropped = 0usize;
    for rows in [&mut v.active_rankings, &mut v.candidate_rankings] {
        let mut seen = BTreeSet::new();
        let before = rows.len();
        rows.retain(|row| {
            allowlisted(&row.instrument_key)
                && seen.insert(row.instrument_key.clone())
                && owner_ranking_row_ok(row)
        });
        dropped += before - rows.len();
    }
    dropped
}

pub(crate) fn owner_digest_bounds_violation(
    v: &OwnerDigest,
    bundle: &Value,
) -> Option<&'static str> {
    let allowlisted = |key: &str| {
        bundle.is_null()
            || bundle["allowlist"]
                .as_array()
                .is_some_and(|allow| allow.iter().any(|item| item["key"] == key))
    };
    if v.as_of_ms < 0
        || v.brief.len() > MAX_OWNER_BRIEF
        || v.evidence_ids.len() > MAX_EVIDENCE_IDS
        || v.evidence_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 256)
        || v.gaps.len() > MAX_OWNER_GAPS
        || v.gaps.iter().any(|gap| !owner_text(gap, MAX_OWNER_TEXT))
        || [
            v.changed_prospects.len(),
            v.active_rankings.len(),
            v.candidate_rankings.len(),
            v.bar_context.len(),
            v.spread_depth.len(),
            v.holdings.len(),
            v.recent_actions.len(),
            v.experiments.len(),
        ]
        .iter()
        .any(|count| *count > MAX_OWNER_ROWS)
        || [
            v.coverage.universe,
            v.coverage.scanned,
            v.coverage.quoted,
            v.coverage.featured,
            v.coverage.experiments,
        ]
        .iter()
        .any(|count| *count > MAX_OWNER_COVERAGE)
    {
        return Some("owner_digest_bounds");
    }
    let mut changed = BTreeSet::new();
    for row in &v.changed_prospects {
        if !allowlisted(&row.instrument_key)
            || !changed.insert(&row.instrument_key)
            || !owner_text(&row.reason, MAX_OWNER_TEXT)
            || !owner_finite(row.change_bps)
            || row.observed_ms.is_some_and(|at| at < 0)
            || row
                .symbol
                .as_deref()
                .is_some_and(|symbol| !owner_text(symbol, MAX_OWNER_SOURCE))
            || !owner_evidence_ids(&row.evidence_ids)
        {
            return Some("owner_changed_prospects_invalid");
        }
    }
    for rows in [&v.active_rankings, &v.candidate_rankings] {
        let mut seen = BTreeSet::new();
        for row in rows {
            if !allowlisted(&row.instrument_key)
                || !seen.insert(&row.instrument_key)
                || !owner_ranking_row_ok(row)
            {
                return Some("owner_rankings_invalid");
            }
        }
    }
    for row in &v.bar_context {
        if !allowlisted(&row.instrument_key)
            || !owner_text(&row.source, MAX_OWNER_SOURCE)
            || !owner_finite(row.return_1m_bps)
            || !owner_finite(row.return_5m_bps)
            || !owner_finite(row.relative_return_5m_bps)
            || row.last_bar_ms.is_some_and(|at| at < 0)
            || !owner_evidence_ids(&row.evidence_ids)
        {
            return Some("owner_bar_context_invalid");
        }
    }
    for row in &v.spread_depth {
        if !allowlisted(&row.instrument_key)
            || row
                .bid
                .as_deref()
                .is_some_and(|price| nivra_core::finance::Price::parse(price).is_err())
            || row
                .ask
                .as_deref()
                .is_some_and(|price| nivra_core::finance::Price::parse(price).is_err())
            || !owner_finite(row.spread_bps)
            || row.quote_age_ms.is_some_and(|age| age < 0)
            || row
                .source
                .as_deref()
                .is_some_and(|source| !owner_text(source, MAX_OWNER_SOURCE))
        {
            return Some("owner_spread_depth_invalid");
        }
    }
    let held = bundle["positions"].as_object();
    let mut seen = BTreeSet::new();
    for row in &v.holdings {
        if !allowlisted(&row.instrument_key)
            || !seen.insert(&row.instrument_key)
            || nivra_core::finance::Price::parse(&row.entry).is_err()
            || nivra_core::finance::Price::parse(&row.mark).is_err()
            || row
                .exit_intent
                .as_ref()
                .is_some_and(|exit| !exit.is_object() || exit.to_string().len() > 1024)
            || !owner_evidence_ids(&row.evidence_ids)
        {
            return Some("owner_holdings_invalid");
        }
        if let Some(position) = held.and_then(|held| held.get(&row.instrument_key))
            && position["units"]
                .as_u64()
                .is_some_and(|units| units != row.units)
        {
            return Some("owner_holdings_units");
        }
    }
    if let Some(held) = held
        && (seen.len() != held.len() || !held.keys().all(|key| seen.contains(key)))
    {
        return Some("owner_holdings_missing_key");
    }
    if nivra_core::finance::Amount::parse(&v.cash.available).is_err()
        || nivra_core::finance::Amount::parse(&v.fees.per_order).is_err()
        || nivra_core::finance::Amount::parse(&v.fees.buy_bps).is_err()
        || nivra_core::finance::Amount::parse(&v.fees.sell_bps).is_err()
        || v.fees
            .slippage_bps
            .as_deref()
            .is_some_and(|value| nivra_core::finance::Amount::parse(value).is_err())
        || v.fees
            .rounding_increment
            .as_deref()
            .is_some_and(|value| nivra_core::finance::Price::parse(value).is_err())
    {
        return Some("owner_cash_fees_invalid");
    }
    for row in &v.recent_actions {
        if row.at_ms < 0
            || !owner_text(&row.action, MAX_OWNER_SOURCE)
            || !owner_text(&row.status, MAX_OWNER_SOURCE)
            || row
                .instrument_key
                .as_deref()
                .is_some_and(|key| !allowlisted(key))
            || row
                .decision_id
                .as_deref()
                .is_some_and(|id| !owner_text(id, 256))
            || row
                .price
                .as_deref()
                .is_some_and(|price| nivra_core::finance::Price::parse(price).is_err())
            || row
                .fee
                .as_deref()
                .is_some_and(|fee| nivra_core::finance::Amount::parse(fee).is_err())
            || !owner_evidence_ids(&row.evidence_ids)
        {
            return Some("owner_recent_actions_invalid");
        }
    }
    for row in &v.experiments {
        if !owner_text(&row.analysis_id, MAX_OWNER_SOURCE)
            || row
                .dataset_id
                .as_deref()
                .is_some_and(|id| !owner_text(id, MAX_OWNER_SOURCE))
            || row.input_sha256.as_deref().is_some_and(|sha| {
                sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || !owner_text(&row.status, MAX_OWNER_SOURCE)
            || row
                .metric
                .as_deref()
                .is_some_and(|metric| !owner_text(metric, MAX_OWNER_SOURCE))
            || !owner_finite(row.value)
            || !owner_evidence_ids(&row.evidence_ids)
        {
            return Some("owner_experiments_invalid");
        }
    }
    None
}

/// Validate an owner factual digest without extending its citation namespace.
/// `complete:false` is reserved for a host deadline fallback; model output must
/// be complete before it can become owner work.
pub(crate) fn validate_owner_digest(raw: &str, bundle: &Value) -> Result<()> {
    let mut v: OwnerDigest =
        serde_json::from_value(decode_digest(raw)?).map_err(|_| "harness_digest_schema_invalid")?;
    let dropped_prospects = normalize_owner_changed_prospects(&mut v);
    if dropped_prospects > 0 {
        eprintln!(
            "owner_digest_warning reason=duplicate_changed_prospects_dropped count={dropped_prospects}"
        );
    }
    let dropped = normalize_owner_rankings(&mut v, bundle);
    if dropped > 0 {
        eprintln!("owner_digest_warning reason=invalid_ranking_rows_dropped count={dropped}");
    }
    if let Some(field) = owner_digest_bounds_violation(&v, bundle) {
        eprintln!("owner_digest_bounds_invalid field={field}");
        return Err("harness_digest_bounds_invalid".into());
    }
    if v.brief.trim().is_empty() {
        // Directional prose is the owner's primary input. A digest without it is
        // still factual evidence, so this warns instead of refusing the turn.
        eprintln!("owner_digest_warning reason=brief_missing");
    }
    Ok(())
}

pub(crate) fn normalize_owner_digest(
    raw: &str,
    bundle: &Value,
    tool_ids: &BTreeSet<String>,
) -> Result<String> {
    let allowlisted = |s: &str| {
        bundle["allowlist"]
            .as_array()
            .is_some_and(|a| a.iter().any(|i| i["key"] == s))
    };
    let mut v: OwnerDigest =
        serde_json::from_value(decode_digest(raw)?).map_err(|_| "harness_digest_schema_invalid")?;
    if !v.complete {
        return Err("harness_digest_incomplete".into());
    }
    let dropped_prospects = normalize_owner_changed_prospects(&mut v);
    if dropped_prospects > 0 {
        eprintln!(
            "owner_digest_warning reason=duplicate_changed_prospects_dropped count={dropped_prospects}"
        );
    }
    let dropped_rankings = normalize_owner_rankings(&mut v, bundle);
    if dropped_rankings > 0 {
        eprintln!(
            "owner_digest_warning reason=invalid_ranking_rows_dropped count={dropped_rankings}"
        );
    }
    if let Some(field) = owner_digest_bounds_violation(&v, bundle) {
        eprintln!("owner_digest_bounds_invalid field={field}");
        return Err("harness_digest_bounds_invalid".into());
    }
    let known = |id: &str| {
        allowlisted(id)
            || tool_ids.contains(id)
            || [
                "bundle",
                "portfolio",
                "shortlist",
                "fees",
                "session_evidence",
            ]
            .contains(&id)
    };
    let mut dropped = 0usize;
    let mut retain = |ids: &mut Vec<String>| {
        let before = ids.len();
        ids.retain(|id| known(id));
        dropped += before - ids.len();
    };
    retain(&mut v.evidence_ids);
    for row in &mut v.changed_prospects {
        retain(&mut row.evidence_ids);
    }
    for row in v
        .active_rankings
        .iter_mut()
        .chain(&mut v.candidate_rankings)
    {
        retain(&mut row.evidence_ids);
    }
    for row in &mut v.bar_context {
        retain(&mut row.evidence_ids);
    }
    for row in &mut v.holdings {
        retain(&mut row.evidence_ids);
    }
    for row in &mut v.recent_actions {
        retain(&mut row.evidence_ids);
    }
    for row in &mut v.experiments {
        retain(&mut row.evidence_ids);
    }
    if dropped != 0 {
        eprintln!("digest_citation_warning reason=unknown_evidence_id_dropped count={dropped}");
    }
    if v.evidence_ids.len() > MAX_EVIDENCE_IDS {
        let excess = v.evidence_ids.len() - MAX_EVIDENCE_IDS;
        v.evidence_ids.truncate(MAX_EVIDENCE_IDS);
        eprintln!("digest_citation_warning reason=excess_evidence_ids_truncated count={excess}");
    }
    Ok(serde_json::to_string(&v)?)
}

/// Host eligibility is evidence availability, not a favorable score or a
/// complete trade thesis. A held book remains eligible without scanner
/// prospects; an empty or unusable feed does not.
pub(crate) fn owner_feed_usable(input: &Value) -> bool {
    if input["positions"]
        .as_object()
        .is_some_and(|positions| !positions.is_empty())
    {
        return true;
    }
    if input["discovery"].as_object().is_some_and(|discovery| {
        discovery["fresh_count"].as_u64().unwrap_or(0) > 0
            || discovery["eligible_count"].as_u64().unwrap_or(0) > 0
            || discovery["prospects"]
                .as_array()
                .is_some_and(|prospects| !prospects.is_empty())
    }) {
        return true;
    }
    input["shortlist"]
        .as_array()
        .is_some_and(|rows| !rows.is_empty())
}

pub(crate) fn owner_digest_eligible(digest: &Value, input: &Value) -> bool {
    digest["complete"] == json!(true) && owner_feed_usable(input)
}

/// Stable digest of the factual prose, excluding clocks and quote ages. A
/// cadence digest that repeats these facts is coalesced instead of waking the
/// orchestrator again.
pub(crate) fn owner_facts_sha256(facts: &Value) -> Result<String> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut keys = map.keys().collect::<Vec<_>>();
                keys.sort();
                let mut value = serde_json::Map::new();
                for key in keys {
                    // All `_ms` fields are host clocks or ages, not market
                    // facts. Dropping them keeps a repeated cadence digest
                    // stable without hiding a changed price or holding.
                    if key.ends_with("_ms") {
                        continue;
                    }
                    value.insert(key.clone(), canonical(&map[key]));
                }
                Value::Object(value)
            }
            Value::Array(rows) => Value::Array(rows.iter().map(canonical).collect()),
            _ => value.clone(),
        }
    }
    Ok(digest(&serde_json::to_vec(&canonical(facts))?))
}
