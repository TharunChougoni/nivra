//! Offline action-tape replay using original quote receipts and shared cash execution.
//! An attributed schedule is not proof that its author avoided future information.
use nivra_core::{
    execution::{ExitIntent, Instrument, Portfolio, Quote},
    finance::{Amount, CashOrderGrid, FeeArithmetic, Price},
    historical::{Event, EventKind},
    replay::digest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub at_ms: i64,
    pub key: String,
    pub units: u64,
    pub max_price: Price,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub version: u32,
    pub parent_candidate_id: Option<String>,
    pub hypothesis: String,
    pub training_from_ms: i64,
    pub training_to_ms: i64,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
    pub holding_ms: i64,
    pub max_exit_delay_ms: i64,
    pub capital: Amount,
    pub costs: FeeArithmetic,
}

/// Explicit quote authority for one action tape. The default paper authority
/// accepts only original protection receipts. The virtual authority accepts
/// approved research-observation receipts for BOTH the virtual entry and the
/// virtual exit, and may also use an original protection receipt as an exit
/// when one exists. Neither authority turns a screening quote into an
/// executable one, and a research receipt can never fund the default paper
/// book.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QuoteAuthorityV1 {
    ProtectionOnly,
    VirtualObservationOnly,
}
impl QuoteAuthorityV1 {
    pub(crate) fn feature_abi(self) -> &'static str {
        match self {
            Self::ProtectionOnly => "cash_quote_action_tape_v1",
            Self::VirtualObservationOnly => "virtual_observation_tape_v1",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ProtectionOnly => "protection_only",
            Self::VirtualObservationOnly => "research_observation_virtual_only",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QuotePurpose {
    Protection,
    ResearchObservation,
}
impl QuotePurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Protection => "protection",
            Self::ResearchObservation => "research_observation",
        }
    }
}

pub(crate) fn parameters_valid(p: &Parameters) -> bool {
    p.version == 1
        && p.parent_candidate_id
            .as_ref()
            .is_none_or(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        && !p.hypothesis.trim().is_empty()
        && p.hypothesis.len() <= 1024
        && !p.hypothesis.chars().any(char::is_control)
        && p.training_from_ms >= 0
        && p.training_from_ms <= p.training_to_ms
        && p.training_to_ms < p.selection_from_ms
        && p.selection_from_ms <= p.selection_to_ms
        && (1..=86_400_000).contains(&p.holding_ms)
        && (0..=60_000).contains(&p.max_exit_delay_ms)
        && p.capital > Amount::ZERO
        && p.costs.validate().is_ok()
}

struct Liquidity {
    quote: Quote,
    bought: u64,
    sold: u64,
    purpose: QuotePurpose,
}
impl Liquidity {
    fn remaining(&self) -> Quote {
        let mut q = self.quote.clone();
        q.ask_units -= self.bought;
        q.bid_units -= self.sold;
        q
    }
}

#[derive(Serialize)]
struct Outcome {
    entry_index: usize,
    key: String,
    requested_at_ms: i64,
    units: u64,
    due_ms: i64,
    deadline_ms: i64,
    state: &'static str,
    entry_fill_index: Option<usize>,
    exit_fill_index: Option<usize>,
    entry_quote_source_ms: Option<i64>,
    entry_quote_receipt_ms: Option<i64>,
    exit_quote_source_ms: Option<i64>,
    exit_quote_receipt_ms: Option<i64>,
    exit_at_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry_quote_purpose: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_quote_purpose: Option<&'static str>,
}

pub(crate) struct ArchiveView {
    pub(crate) instruments: BTreeMap<String, Instrument>,
    pub(crate) tape: Vec<Event>,
    /// Original per-key archived receipt SHA from the manifest provenance.
    pub(crate) per_key_archive_sha: BTreeMap<String, String>,
    /// Legacy single-key global receipt SHA. Never used for a multi-key tape.
    pub(crate) single_key_archive_sha: Option<String>,
}

pub(crate) fn archive_view(manifest: &[u8], events: &[u8]) -> Result<ArchiveView, String> {
    archive_view_with(manifest, events, false)
}

/// Virtual-observation v1 archive view. The global `provenance.archive_sha256`
/// fallback is removed unless the tape provably carries EXACTLY ONE instrument
/// and ONE actual observation/receipt. A single-key tape with two receipts
/// (for example an entry and an exit) has no unambiguous global receipt SHA, so
/// the per-key provenance is required. The default Task 8 protection path keeps
/// [`archive_view`] and its legacy compatibility unchanged.
pub(crate) fn archive_view_virtual(manifest: &[u8], events: &[u8]) -> Result<ArchiveView, String> {
    archive_view_with(manifest, events, true)
}

fn archive_view_with(
    manifest: &[u8],
    events: &[u8],
    strict_single_receipt: bool,
) -> Result<ArchiveView, String> {
    let (first_day, last_day, universe) =
        crate::strategy_archive::archive_identity(manifest, events)?;
    let instruments: BTreeMap<_, _> = universe.into_iter().map(|i| (i.key.clone(), i)).collect();
    let tape = parse_archive_events(events, first_day, last_day, &instruments)?;
    let manifest_value: Value =
        serde_json::from_slice(manifest).map_err(|_| "action_replay_manifest_invalid")?;
    let per_key_archive_sha = manifest_value["provenance"]["per_key"]
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| {
                    value["archive_sha256"]
                        .as_str()
                        .map(|sha| (key.clone(), sha.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default();
    let observation_count = tape
        .iter()
        .map(|event| event.observations.len())
        .sum::<usize>();
    let single_key_archive_sha =
        if instruments.len() == 1 && (!strict_single_receipt || observation_count == 1) {
            manifest_value["provenance"]["archive_sha256"]
                .as_str()
                .map(str::to_owned)
        } else {
            None
        };
    Ok(ArchiveView {
        instruments,
        tape,
        per_key_archive_sha,
        single_key_archive_sha,
    })
}

pub(crate) fn parse_archive_events(
    events: &[u8],
    first_day: chrono::NaiveDate,
    last_day: chrono::NaiveDate,
    instruments: &BTreeMap<String, Instrument>,
) -> Result<Vec<Event>, String> {
    let mut tape = Vec::<Event>::new();
    let mut count = 0usize;
    for line in events
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        if line.len() > 2_000_000 || tape.len() >= 10_000 {
            return Err("action_replay_input_limit".into());
        }
        let event: Event =
            serde_json::from_slice(line).map_err(|_| "action_replay_event_invalid")?;
        count = count
            .checked_add(event.observations.len())
            .ok_or("action_replay_input_limit")?;
        let day = chrono::DateTime::from_timestamp_millis(event.at)
            .ok_or("action_replay_clock_invalid")?
            .with_timezone(&chrono::FixedOffset::east_opt(19800).expect("IST"))
            .date_naive();
        if count > 100_000
            || day < first_day
            || day > last_day
            || tape.last().is_some_and(|previous| previous.at > event.at)
            || event.available_at > event.at
        {
            return Err("action_replay_chronology_invalid".into());
        }
        let mut seen = BTreeSet::new();
        for o in &event.observations {
            let i = instruments
                .get(&o.key)
                .ok_or("action_replay_quote_identity")?;
            let q = &o.quote;
            let grid = CashOrderGrid::new(1, i.tick).map_err(|_| "action_replay_quote_invalid")?;
            if !seen.insert(&o.key)
                || q.source > q.receipt
                || q.receipt > event.available_at
                || q.ask < q.bid
                || grid.validate(1, q.bid).is_err()
                || grid.validate(1, q.ask).is_err()
            {
                return Err("action_replay_quote_invalid".into());
            }
        }
        tape.push(event);
    }
    if tape.is_empty() {
        return Err("action_replay_empty_tape".into());
    }
    Ok(tape)
}

/// Inputs and outputs are bounded by the CLI and again here for library callers.
/// Only recorded bids/asks can fill; ranking prices and carried stale marks cannot.
pub fn evaluate(
    source: &[u8],
    parameters: &[u8],
    manifest: &[u8],
    events: &[u8],
) -> Result<Value, String> {
    evaluate_tape(
        source,
        parameters,
        manifest,
        events,
        QuoteAuthorityV1::ProtectionOnly,
    )
}

/// Virtual-observation authority. It runs the same loop, portfolio, cash, fee,
/// fill and freshness engine as [`evaluate`]; only the accepted quote purpose
/// and the recorded authority differ. A research quote can never enter the
/// default paper path, and a protection quote can never fund a virtual entry.
/// A flat virtual book has no real protection receipt, so an approved research
/// receipt is also the exit authority; an original protection exit remains
/// acceptable when one exists.
pub fn evaluate_virtual_observation(
    source: &[u8],
    parameters: &[u8],
    manifest: &[u8],
    events: &[u8],
) -> Result<Value, String> {
    evaluate_tape(
        source,
        parameters,
        manifest,
        events,
        QuoteAuthorityV1::VirtualObservationOnly,
    )
}

fn evaluate_tape(
    source: &[u8],
    parameters: &[u8],
    manifest: &[u8],
    events: &[u8],
    authority: QuoteAuthorityV1,
) -> Result<Value, String> {
    if source.len() > 2 * 1024 * 1024
        || parameters.len() > 16 * 1024
        || manifest.len() > 512 * 1024
        || events.len() > 16 * 1024 * 1024
    {
        return Err("action_replay_input_limit".into());
    }
    let entries: Vec<Entry> =
        serde_json::from_slice(source).map_err(|_| "action_replay_source_invalid")?;
    let p: Parameters =
        serde_json::from_slice(parameters).map_err(|_| "action_replay_parameters_invalid")?;
    if !parameters_valid(&p)
        || entries.is_empty()
        || entries.len() > 5_000
        || entries.windows(2).any(|w| w[0].at_ms > w[1].at_ms)
    {
        return Err("action_replay_parameters_invalid".into());
    }
    let (first_day, last_day, universe) =
        crate::strategy_archive::archive_identity(manifest, events)?;
    let instruments: BTreeMap<_, _> = universe.into_iter().map(|i| (i.key.clone(), i)).collect();
    for e in &entries {
        if e.at_ms < p.selection_from_ms
            || e.at_ms > p.selection_to_ms
            || e.units == 0
            || !instruments.contains_key(&e.key)
            || e.at_ms
                .checked_add(p.holding_ms)
                .and_then(|due| due.checked_add(p.max_exit_delay_ms))
                .is_none()
        {
            return Err("action_replay_entry_invalid".into());
        }
        CashOrderGrid::new(1, instruments[&e.key].tick)
            .and_then(|grid| {
                grid.validate(
                    i64::try_from(e.units)
                        .map_err(|_| nivra_core::finance::FinancialError::DecimalRange)?,
                    e.max_price,
                )
            })
            .map_err(|_| "action_replay_entry_invalid")?;
    }
    let tape = parse_archive_events(events, first_day, last_day, &instruments)?;
    let source_hash = digest(source);
    let source_manifest: Value =
        serde_json::from_slice(manifest).map_err(|_| "action_replay_manifest_invalid")?;
    let params_hash = digest(parameters);
    let candidate_id =
        digest(format!("{}\0{source_hash}\0{params_hash}", authority.feature_abi()).as_bytes());
    let mut portfolio = Portfolio::with_capital(p.capital);
    portfolio.costs = Some(p.costs.clone());
    portfolio.model_led = true;
    let mut quotes = BTreeMap::<String, Liquidity>::new();
    let mut outcomes = Vec::<Outcome>::new();
    let mut event_index = 0;
    let mut entry_index = 0;
    let mut last_at = tape[0].at;
    while event_index < tape.len() || entry_index < entries.len() {
        let now = match (tape.get(event_index), entries.get(entry_index)) {
            (Some(event), Some(entry)) => event.at.min(entry.at_ms),
            (Some(event), None) => event.at,
            (None, Some(entry)) => entry.at_ms,
            (None, None) => unreachable!(),
        };
        last_at = now;
        while let Some(event) = tape.get(event_index).filter(|event| event.at == now) {
            for o in &event.observations {
                // Screening records do not acquire depth/execution purpose here.
                let purpose = match event.kind {
                    EventKind::Screen => continue,
                    EventKind::Protection => QuotePurpose::Protection,
                    EventKind::VirtualObservation => {
                        if authority != QuoteAuthorityV1::VirtualObservationOnly {
                            return Err("action_replay_virtual_observation_unauthorized".into());
                        }
                        QuotePurpose::ResearchObservation
                    }
                    EventKind::Entry => {
                        // Historical action tapes may carry an `entry` quote
                        // event. The default paper authority keeps treating it
                        // as a protection-purpose quote event; the virtual
                        // authority refuses it because it is not a research
                        // observation.
                        if authority == QuoteAuthorityV1::VirtualObservationOnly {
                            return Err("action_replay_unsupported_event_kind".into());
                        }
                        QuotePurpose::Protection
                    }
                };
                match quotes.get(&o.key) {
                    Some(old)
                        if o.quote.receipt < old.quote.receipt
                            || o.quote.source < old.quote.source =>
                    {
                        return Err("action_replay_quote_regressed".into());
                    }
                    Some(old) if o.quote.receipt == old.quote.receipt => {
                        if o.quote != old.quote || purpose != old.purpose {
                            return Err("action_replay_quote_receipt_changed".into());
                        }
                    }
                    _ => {
                        quotes.insert(
                            o.key.clone(),
                            Liquidity {
                                quote: o.quote.clone(),
                                bought: 0,
                                sold: 0,
                                purpose,
                            },
                        );
                    }
                }
            }
            event_index += 1;
        }
        // Refresh only from the original fresh quote. Missing marks stay absent
        // in the final report and never unlock a buy through a stale held mark.
        for (key, position) in &mut portfolio.positions {
            if let Some(q) = quotes
                .get(key)
                .map(|l| &l.quote)
                .filter(|q| q.fresh(now, nivra_core::market::MAX_FINANCIAL_AGE_MS))
            {
                position.mark = q.bid;
                position.marked_at = q.source.min(q.receipt);
            }
        }
        for outcome in outcomes
            .iter_mut()
            .filter(|o| o.state == "awaiting_forward_data")
        {
            if now > outcome.deadline_ms {
                outcome.state = "missing_exit_evidence";
            } else if now >= outcome.due_ms
                && let Some(liquidity) = quotes.get_mut(&outcome.key)
                && liquidity.quote.source >= outcome.due_ms
                && liquidity.quote.receipt >= outcome.due_ms
            {
                let q = liquidity.remaining();
                if execute(
                    &mut portfolio,
                    now,
                    &instruments[&outcome.key],
                    &q,
                    false,
                    outcome.units,
                    q.bid,
                    &candidate_id,
                )? {
                    liquidity.sold += outcome.units;
                    outcome.state = "closed";
                    outcome.exit_fill_index = Some(portfolio.fills.len() - 1);
                    outcome.exit_quote_source_ms = Some(q.source);
                    outcome.exit_quote_receipt_ms = Some(q.receipt);
                    outcome.exit_at_ms = Some(now);
                    if authority == QuoteAuthorityV1::VirtualObservationOnly {
                        outcome.exit_quote_purpose = Some(liquidity.purpose.as_str());
                    }
                }
            }
        }
        while let Some(entry) = entries.get(entry_index).filter(|entry| entry.at_ms == now) {
            let mut outcome = Outcome {
                entry_index,
                key: entry.key.clone(),
                requested_at_ms: now,
                units: entry.units,
                due_ms: now + p.holding_ms,
                deadline_ms: now + p.holding_ms + p.max_exit_delay_ms,
                state: "entry_quote_unavailable",
                entry_fill_index: None,
                exit_fill_index: None,
                entry_quote_source_ms: None,
                entry_quote_receipt_ms: None,
                exit_quote_source_ms: None,
                exit_quote_receipt_ms: None,
                exit_at_ms: None,
                entry_quote_purpose: None,
                exit_quote_purpose: None,
            };
            if let Some(liquidity) = quotes.get_mut(&entry.key) {
                // A virtual entry may only use an approved research-observation
                // quote. A protection receipt is an exit authority, never a
                // virtual entry; the outcome stays explicitly unavailable.
                let purpose_allowed = authority != QuoteAuthorityV1::VirtualObservationOnly
                    || liquidity.purpose == QuotePurpose::ResearchObservation;
                if purpose_allowed {
                    let q = liquidity.remaining();
                    if q.fresh(now, nivra_core::market::MAX_FINANCIAL_AGE_MS) {
                        outcome.state = "entry_execution_rejected";
                        if execute(
                            &mut portfolio,
                            now,
                            &instruments[&entry.key],
                            &q,
                            true,
                            entry.units,
                            entry.max_price,
                            &candidate_id,
                        )? {
                            liquidity.bought += entry.units;
                            outcome.state = "awaiting_forward_data";
                            outcome.entry_fill_index = Some(portfolio.fills.len() - 1);
                            outcome.entry_quote_source_ms = Some(q.source);
                            outcome.entry_quote_receipt_ms = Some(q.receipt);
                            if authority == QuoteAuthorityV1::VirtualObservationOnly {
                                outcome.entry_quote_purpose =
                                    Some(QuotePurpose::ResearchObservation.as_str());
                            }
                        }
                    }
                }
            }
            outcomes.push(outcome);
            entry_index += 1;
        }
    }
    portfolio
        .reconcile()
        .map_err(|_| "action_replay_reconciliation_failed")?;
    let mut fees = Amount::ZERO;
    let mut turnover = Amount::ZERO;
    for fill in &portfolio.fills {
        fees = fees
            .checked_add(fill.fee)
            .map_err(|_| "action_replay_arithmetic_failed")?;
        turnover = turnover
            .checked_add(
                fill.price
                    .amount()
                    .checked_mul(Amount::from_unsigned(fill.units))
                    .map_err(|_| "action_replay_arithmetic_failed")?,
            )
            .map_err(|_| "action_replay_arithmetic_failed")?;
    }
    let missing_marks: Vec<_> = portfolio
        .positions
        .keys()
        .filter(|key| {
            !quotes.get(*key).is_some_and(|l| {
                l.quote
                    .fresh(last_at, nivra_core::market::MAX_FINANCIAL_AGE_MS)
            })
        })
        .cloned()
        .collect();
    let marked_equity = if missing_marks.is_empty() {
        Some(
            portfolio
                .equity()
                .map_err(|_| "action_replay_arithmetic_failed")?,
        )
    } else {
        None
    };
    let net_pnl_if_flat = if portfolio.positions.is_empty() {
        Some(
            portfolio
                .cash
                .checked_sub(p.capital)
                .map_err(|_| "action_replay_arithmetic_failed")?,
        )
    } else {
        None
    };
    let mut report = json!({"schema":1,"candidate_id":candidate_id,"source_sha256":source_hash,"params_sha256":params_hash,
        "feature_abi":"cash_quote_action_tape_v1","parameters":p,"source":entries,
        "manifest_sha256":digest(manifest),"events_sha256":digest(events),"declared_synthetic":source_manifest.get("synthetic"),
        "engine_sha256":nivra_core::replay::engine_fingerprint(),"adapter_sha256":digest(include_bytes!("strategy_actions.rs")),
        "as_of_ms":last_at,"outcomes":outcomes,"portfolio":portfolio,"fees":fees,"turnover":turnover,
        "net_pnl_if_flat":net_pnl_if_flat,"marked_equity":marked_equity,"missing_mark_keys":missing_marks,
        "scientific_state":"insufficient_evidence","promotion_authorized":false,
        "limitations":["declared_action_schedule_does_not_establish_prospective_authoring_or_no_lookahead",
        "capture_identity_and_cost_assumptions_are_not_provider_certification",
        "no_oos_walk_forward_or_multiple_testing_qualification"]});
    if authority == QuoteAuthorityV1::VirtualObservationOnly {
        report["feature_abi"] = json!(authority.feature_abi());
        report["quote_authority"] = json!(authority.label());
        report["not_protection"] = json!(true);
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
fn execute(
    portfolio: &mut Portfolio,
    now: i64,
    i: &Instrument,
    q: &Quote,
    buy: bool,
    units: u64,
    max_price: Price,
    id: &str,
) -> Result<bool, String> {
    portfolio
        .execute_plan(now, i, q, buy, units, max_price, ExitIntent::default(), id)
        .map_err(|_| "action_replay_execution_failed".into())
}

#[cfg(test)]
pub(crate) fn verify_action_replay() {
    use crate::candidate_execution::{self, CandidateExecutionSpecV1, ScoredSignal};
    let at = chrono::DateTime::parse_from_rfc3339("2026-10-16T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let quote = |now, bid, ask| json!({"key":"SYNTHETIC|A","quote":{"bid":bid,"ask":ask,"bid_units":10,"ask_units":10,"source":now,"receipt":now},"lower_circuit_limit":"50","upper_circuit_limit":"200","volume":10000});
    let first = quote(at, "99", "100");
    let rows = vec![
        json!({"at":at,"available_at":at,"kind":"entry","observations":[first.clone()]}),
        json!({"at":at+100,"available_at":at+100,"kind":"entry","observations":[first.clone()]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[quote(at+1000,"110","111")]}),
    ];
    let encode = |rows: &[Value]| {
        rows.iter()
            .map(|r| format!("{r}\n"))
            .collect::<String>()
            .into_bytes()
    };
    let manifest = |events: &[u8]| {
        serde_json::to_vec(&json!({"schema":1,"session":"2026-10-16","previous_session":"2026-10-15","capital":"5000",
        "universe":[{"key":"SYNTHETIC|A","symbol":"FIXTURE","series":"EQ","tick":"0.05"}],"events_sha256":digest(events),"synthetic":true,"max_observation_gap_ms":1000,"history_directory":null,"provenance":{}})).unwrap()
    };
    let source = serde_json::to_vec(
        &json!([{"at_ms":at,"key":"SYNTHETIC|A","units":10,"max_price":"100"},
        {"at_ms":at+100,"key":"SYNTHETIC|A","units":1,"max_price":"100"}]),
    )
    .unwrap();
    let parameters = json!({"version":1,"parent_candidate_id":null,"hypothesis":"Synthetic executable cost and gap fixture","training_from_ms":at-2000,"training_to_ms":at-1,"selection_from_ms":at,"selection_to_ms":at+100,
        "holding_ms":1000,"max_exit_delay_ms":2000,"capital":"5000","costs":{"per_order":"1","buy_bps":"0","sell_bps":"0","slippage_bps":"0","rounding_increment":"0.01"}});
    let params = serde_json::to_vec(&parameters).unwrap();
    let events = encode(&rows);
    let report = evaluate(&source, &params, &manifest(&events), &events).unwrap();
    assert_eq!(report["net_pnl_if_flat"], "98");
    assert_eq!(report["fees"], "2");
    assert_eq!(report["turnover"], "2100");
    assert_eq!(report["portfolio"]["fills"].as_array().unwrap().len(), 2);
    assert_eq!(report["outcomes"][0]["state"], "closed");
    assert_eq!(report["outcomes"][1]["state"], "entry_execution_rejected");
    let mut short_cash = parameters.clone();
    short_cash["capital"] = json!("1000");
    assert_eq!(
        evaluate(
            &source,
            &serde_json::to_vec(&short_cash).unwrap(),
            &manifest(&events),
            &events
        )
        .unwrap()["outcomes"][0]["state"],
        "entry_execution_rejected"
    );
    let mut adverse = parameters.clone();
    adverse["costs"]["slippage_bps"] = json!("5");
    assert!(
        evaluate(
            &source,
            &serde_json::to_vec(&adverse).unwrap(),
            &manifest(&events),
            &events
        )
        .unwrap()["portfolio"]["fills"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut gaps = rows[..2].to_vec();
    gaps.push(json!({"at":at+3001,"available_at":at+3001,"kind":"protection","observations":[]}));
    let events = encode(&gaps);
    let gap = evaluate(&source, &params, &manifest(&events), &events).unwrap();
    assert_eq!(gap["outcomes"][0]["state"], "missing_exit_evidence");
    assert!(gap["marked_equity"].is_null());
    assert!(gap["net_pnl_if_flat"].is_null());
    assert_eq!(gap["portfolio"]["fills"].as_array().unwrap().len(), 1);
    let mut screen = rows.clone();
    for row in &mut screen {
        row["kind"] = json!("screen");
    }
    let events = encode(&screen);
    assert!(
        evaluate(&source, &params, &manifest(&events), &events).unwrap()["portfolio"]["fills"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut leaked = parameters.clone();
    leaked["training_to_ms"] = json!(at);
    assert!(
        evaluate(
            &source,
            &serde_json::to_vec(&leaked).unwrap(),
            &manifest(&events),
            &events
        )
        .is_err()
    );
    let mut mutated = rows.clone();
    mutated[1]["observations"][0]["quote"]["ask_units"] = json!(20);
    let events = encode(&mutated);
    assert_eq!(
        evaluate(&source, &params, &manifest(&events), &events).unwrap_err(),
        "action_replay_quote_receipt_changed"
    );

    let candidate_manifest = |events: &[u8]| {
        serde_json::to_vec(&json!({"schema":1,"session":"2026-10-16","previous_session":"2026-10-15","capital":"5000",
        "universe":[
            {"key":"SYNTHETIC|A","symbol":"AAA","series":"EQ","tick":"0.05"},
            {"key":"SYNTHETIC|B","symbol":"BBB","series":"EQ","tick":"0.05"}
        ],"events_sha256":digest(events),"synthetic":true,"max_observation_gap_ms":1000,"history_directory":null,"provenance":{}})).unwrap()
    };
    let exec_quote = |key: &str, source: i64, receipt: i64, bid: &str, ask: &str, units: u64| {
        json!({
            "key":key,
            "quote":{"bid":bid,"ask":ask,"bid_units":units,"ask_units":units,"source":source,"receipt":receipt},
            "lower_circuit_limit":"50",
            "upper_circuit_limit":"200",
            "volume":10000,
            // Original per-quote receipt identity for the synthetic fixture.
            "archive_sha256":digest(format!("{key}\0{source}\0{receipt}\0{bid}\0{ask}").as_bytes()),
            "purpose":"protection"
        })
    };
    let costs = |per_order: &str, slippage_bps: &str| json!({"per_order":per_order,"buy_bps":"0","sell_bps":"0","slippage_bps":slippage_bps,"rounding_increment":"0.01"});
    let candidate_spec = |capital: &str,
                          slippage_bps: &str,
                          max_entries: u32,
                          top_n: u32,
                          offset: u32,
                          first: bool,
                          stress: Vec<Value>| {
        serde_json::from_value::<CandidateExecutionSpecV1>(json!({
            "version":1,"segment":"cash",
            "parameters":{"version":1,"parent_candidate_id":null,"hypothesis":"Synthetic executable adapter fixture",
                "training_from_ms":at-2000,"training_to_ms":at-1,"selection_from_ms":at,"selection_to_ms":at+5000,
                "holding_ms":1000,"max_exit_delay_ms":2000,"capital":capital,
                "costs":costs("1", slippage_bps)},
            "units":10,"max_entries":max_entries,"selection_top_n":top_n,"entry_price_offset_ticks":offset,
            "entry_at":"signal","exit":"time","first_signal_per_key":first,"cost_stress":stress
        }))
        .unwrap()
    };

    let entry_at = at;
    let exit_at = at + 1000;
    let base_rows = vec![
        json!({"at":entry_at,"available_at":entry_at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",entry_at,entry_at,"99","100",10)]}),
        json!({"at":exit_at,"available_at":exit_at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",exit_at,exit_at,"110","111",10)]}),
    ];
    let base_events = encode(&base_rows);
    let base_manifest = candidate_manifest(&base_events);
    let base_signals = vec![ScoredSignal {
        at_ms: entry_at,
        ordered_keys: vec!["SYNTHETIC|A".into()],
    }];
    let base_spec = candidate_spec("5000", "0", 1, 1, 0, true, vec![costs("1", "5")]);
    let plain_spec = candidate_spec("5000", "0", 1, 1, 0, true, vec![]);
    let base_report =
        candidate_execution::execute(&base_signals, &base_spec, &base_manifest, &base_events)
            .unwrap();
    assert_eq!(base_report.result_kind, "executable_replay");
    assert_eq!(
        base_report.execution_status,
        candidate_execution::ExecutionStatus::Executed
    );
    assert_eq!(
        base_report.artifact_encoding,
        "canonical_json_sorted_keys_v1"
    );
    assert_eq!(base_report.feature_abi, "cash_quote_action_tape_v1");
    assert!(base_report.replay_executed);
    assert!(base_report.retrospective);
    assert!(!base_report.prospective);
    assert!(!base_report.prospective_authoring);
    assert!(!base_report.no_lookahead_qualified);
    assert!(!base_report.holdout_qualified);
    assert!(!base_report.edge_qualified);
    assert!(!base_report.promotion_authorized);
    assert!(!base_report.activation_authorized);
    assert_eq!(base_report.scientific_state, "insufficient_evidence");
    assert_eq!(
        base_report.initial_capital_assumption,
        Amount::parse("5000").unwrap()
    );
    assert_eq!(base_report.scored_signals, base_signals);
    assert_eq!(base_report.action_tape.len(), 1);
    let base_summary = base_report.summary.as_ref().unwrap();
    assert_eq!(base_summary.cash, Amount::parse("5098").unwrap());
    assert_eq!(base_summary.fees, Amount::parse("2").unwrap());
    assert_eq!(base_summary.turnover, Amount::parse("2100").unwrap());
    assert_eq!(base_summary.fills, 2);
    assert_eq!(base_summary.entry_fills, 1);
    assert_eq!(base_summary.exit_fills, 1);
    assert_eq!(
        base_summary.net_pnl_if_flat,
        Some(Amount::parse("98").unwrap())
    );
    assert_eq!(
        base_summary.marked_equity,
        Some(Amount::parse("5098").unwrap())
    );
    assert!(base_summary.residual_positions.is_empty());
    assert!(base_summary.unknown_marks.is_empty());
    assert!(base_summary.flat);
    assert!(base_summary.accounting_complete);
    assert!(base_summary.adapter_rejections.is_empty());
    assert_eq!(base_summary.engine_outcomes.get("closed"), Some(&1));
    assert_eq!(base_report.decision_timeline[0].signal_ms, entry_at);
    assert_eq!(base_report.decision_timeline[0].quote.event_at_ms, entry_at);
    assert_eq!(
        base_report.decision_timeline[0].quote.available_at_ms,
        entry_at
    );
    assert_eq!(base_report.decision_timeline[0].quote.source_ms, entry_at);
    assert_eq!(base_report.decision_timeline[0].quote.receipt_ms, entry_at);
    assert_eq!(
        base_report.decision_timeline[0].quote.ask,
        Price::parse("100").unwrap()
    );
    assert_eq!(
        base_report.decision_timeline[0].max_price,
        Price::parse("100").unwrap()
    );
    let raw = base_report.report.as_ref().unwrap();
    assert_eq!(raw["outcomes"][0]["state"], "closed");
    assert_eq!(raw["outcomes"][0]["entry_quote_source_ms"], entry_at);
    assert_eq!(raw["outcomes"][0]["exit_quote_source_ms"], exit_at);
    assert_eq!(raw["net_pnl_if_flat"], "98");
    assert_eq!(
        raw["source"],
        serde_json::to_value(&base_report.action_tape).unwrap()
    );
    assert_eq!(base_report.source_sha256, base_report.action_tape_sha256);
    assert_eq!(
        base_report.action_tape_sha256,
        candidate_execution::artifact_sha256(&base_report.action_tape).unwrap()
    );
    assert_eq!(
        base_report.action_tape_sha256,
        digest(&candidate_execution::artifact_bytes(&base_report.action_tape).unwrap())
    );
    assert_eq!(base_report.params_sha256, base_report.parameters_sha256);
    assert_eq!(
        base_report.params_sha256,
        candidate_execution::artifact_sha256(&base_spec.parameters).unwrap()
    );
    assert_eq!(
        base_report.execution_spec_sha256,
        candidate_execution::artifact_sha256(&base_spec).unwrap()
    );
    assert_eq!(
        base_report.execution_spec_sha256,
        candidate_execution::artifact_sha256(&base_report.execution_spec).unwrap()
    );
    assert_eq!(
        base_report.scoring.scored_signals_sha256,
        candidate_execution::artifact_sha256(&base_signals).unwrap()
    );
    assert_eq!(base_report.manifest_sha256, digest(&base_manifest));
    assert_eq!(base_report.events_sha256, digest(&base_events));
    assert_eq!(
        base_report.adapter_sha256,
        digest(include_bytes!("candidate_execution.rs"))
    );
    assert_eq!(
        base_report.shared_engine_adapter_sha256,
        digest(include_bytes!("strategy_actions.rs"))
    );
    assert_eq!(
        base_report.engine_sha256,
        nivra_core::replay::engine_fingerprint()
    );
    assert_eq!(
        base_report.candidate_id,
        digest(
            format!(
                "cash_quote_action_tape_v1\0{}\0{}",
                base_report.source_sha256, base_report.params_sha256
            )
            .as_bytes()
        )
    );
    assert_eq!(
        base_report.report_sha256.as_deref(),
        Some(candidate_execution::artifact_sha256(raw).unwrap().as_str())
    );
    assert_eq!(
        raw["candidate_id"].as_str(),
        Some(base_report.candidate_id.as_str())
    );
    assert_eq!(
        raw["source_sha256"].as_str(),
        Some(base_report.action_tape_sha256.as_str())
    );
    assert_eq!(
        raw["params_sha256"].as_str(),
        Some(base_report.params_sha256.as_str())
    );
    assert_eq!(
        candidate_execution::artifact_sha256(&raw["parameters"]).unwrap(),
        base_report.params_sha256
    );
    assert_eq!(
        candidate_execution::artifact_sha256(&raw["source"]).unwrap(),
        base_report.action_tape_sha256
    );
    let base_value = serde_json::to_value(&base_report).unwrap();
    assert_eq!(base_value["result_kind"], "executable_replay");
    assert_eq!(base_value["execution_status"], "executed");
    assert_eq!(
        base_value["artifact_encoding"],
        "canonical_json_sorted_keys_v1"
    );
    assert_eq!(base_value["retrospective"], true);
    assert_eq!(base_value["prospective"], false);
    assert_eq!(base_value["prospective_authoring"], false);
    assert_eq!(base_value["no_lookahead_qualified"], false);
    assert_eq!(base_value["holdout_qualified"], false);
    assert_eq!(base_value["edge_qualified"], false);
    assert_eq!(base_value["promotion_authorized"], false);
    assert_eq!(base_value["activation_authorized"], false);
    assert_eq!(base_value["scientific_state"], "insufficient_evidence");
    assert_eq!(base_value["initial_capital_assumption"], "5000");
    assert_eq!(base_value["summary"]["cash"], "5098");
    assert_eq!(base_value["summary"]["fees"], "2");
    assert_eq!(base_value["summary"]["fills"], 2);
    assert_eq!(
        base_value["execution_spec_sha256"],
        candidate_execution::artifact_sha256(&base_value["execution_spec"]).unwrap()
    );
    assert_eq!(
        base_value["action_tape_sha256"],
        candidate_execution::artifact_sha256(&base_value["action_tape"]).unwrap()
    );
    assert_eq!(
        base_value["params_sha256"],
        candidate_execution::artifact_sha256(&base_value["execution_spec"]["parameters"]).unwrap()
    );
    assert_eq!(base_value["parameters_sha256"], base_value["params_sha256"]);
    assert_eq!(
        base_value["scoring"]["scored_signals_sha256"],
        candidate_execution::artifact_sha256(&base_value["scored_signals"]).unwrap()
    );
    assert_eq!(
        base_value["report_sha256"],
        candidate_execution::artifact_sha256(&base_value["report"]).unwrap()
    );
    assert_eq!(
        base_value["cost_stress"][0]["costs_sha256"],
        candidate_execution::artifact_sha256(&base_value["cost_stress"][0]["costs"]).unwrap()
    );
    assert_eq!(
        base_value["cost_stress"][0]["params_sha256"],
        candidate_execution::artifact_sha256(&base_value["cost_stress"][0]["parameters"]).unwrap()
    );
    assert_eq!(
        base_value["cost_stress"][0]["report_sha256"],
        candidate_execution::artifact_sha256(&base_value["cost_stress"][0]["report"]).unwrap()
    );
    assert_eq!(
        base_value["cost_stress"][0]["summary"]["net_pnl_if_flat"],
        "0"
    );
    assert_eq!(base_report.cost_stress.len(), 1);
    let stress = &base_report.cost_stress[0];
    assert_eq!(stress.costs, base_spec.cost_stress[0]);
    assert_eq!(stress.parameters.costs, stress.costs);
    assert_eq!(stress.summary.fills, 0);
    assert_eq!(stress.summary.fees, Amount::ZERO);
    assert_eq!(stress.summary.turnover, Amount::ZERO);
    assert_eq!(stress.summary.net_pnl_if_flat, Some(Amount::ZERO));
    assert_eq!(
        stress.summary.marked_equity,
        Some(Amount::parse("5000").unwrap())
    );
    assert_eq!(
        stress.costs_sha256,
        candidate_execution::artifact_sha256(&stress.costs).unwrap()
    );
    assert_eq!(
        stress.params_sha256,
        candidate_execution::artifact_sha256(&stress.parameters).unwrap()
    );
    assert_eq!(
        stress.report_sha256,
        candidate_execution::artifact_sha256(&stress.report).unwrap()
    );
    assert_eq!(
        stress.report["params_sha256"].as_str(),
        Some(stress.params_sha256.as_str())
    );
    assert_ne!(stress.params_sha256, base_report.params_sha256);
    assert_ne!(
        Some(stress.report_sha256.as_str()),
        base_report.report_sha256.as_deref()
    );

    // The pure verifier must bind the outer engine/adapter hashes to the
    // embedded shared report, not merely check their syntax. A recorded old
    // code hash remains valid because the verifier never compares it with the
    // current executable bytes.
    candidate_execution::verify_report(&base_report).unwrap();
    candidate_execution::verify_report_against_manifest(&base_report, &base_manifest, &base_events)
        .unwrap();
    let mut outer_engine = base_report.clone();
    outer_engine.engine_sha256 = "0".repeat(64);
    assert!(candidate_execution::verify_report(&outer_engine).is_err());
    let mut bad_candidate_id = base_report.clone();
    bad_candidate_id.candidate_id = "0".repeat(64);
    assert!(candidate_execution::verify_report(&bad_candidate_id).is_err());
    let mut bad_attempts = base_report.clone();
    bad_attempts.attempts = 0;
    assert!(candidate_execution::verify_report(&bad_attempts).is_err());
    let mut outer_adapter = base_report.clone();
    outer_adapter.shared_engine_adapter_sha256 = "0".repeat(64);
    assert!(candidate_execution::verify_report(&outer_adapter).is_err());
    let mut old_code = base_report.clone();
    old_code.adapter_sha256 = "5".repeat(64);
    old_code.engine_sha256 = "1".repeat(64);
    old_code.shared_engine_adapter_sha256 = "2".repeat(64);
    let mut old_shared = old_code.report.clone().unwrap();
    old_shared["engine_sha256"] = json!(old_code.engine_sha256);
    old_shared["adapter_sha256"] = json!(old_code.shared_engine_adapter_sha256);
    old_code.report_sha256 = Some(candidate_execution::artifact_sha256(&old_shared).unwrap());
    old_code.report = Some(old_shared);
    for stress in &mut old_code.cost_stress {
        let mut report = stress.report.clone();
        report["engine_sha256"] = json!(old_code.engine_sha256);
        report["adapter_sha256"] = json!(old_code.shared_engine_adapter_sha256);
        stress.report_sha256 = candidate_execution::artifact_sha256(&report).unwrap();
        stress.report = report;
    }
    candidate_execution::verify_report(&old_code).unwrap();
    let mut embedded_engine = base_report.clone();
    let mut embedded_shared = embedded_engine.report.clone().unwrap();
    embedded_shared["engine_sha256"] = json!("3".repeat(64));
    embedded_engine.report_sha256 =
        Some(candidate_execution::artifact_sha256(&embedded_shared).unwrap());
    embedded_engine.report = Some(embedded_shared);
    assert!(candidate_execution::verify_report(&embedded_engine).is_err());
    let mut fake_summary = base_report.clone();
    fake_summary.summary.as_mut().unwrap().net_pnl_if_flat = Some(Amount::parse("1").unwrap());
    assert!(candidate_execution::verify_report(&fake_summary).is_err());
    let mut fake_stress_summary = base_report.clone();
    fake_stress_summary.cost_stress[0].summary.cash = Amount::parse("1").unwrap();
    assert!(candidate_execution::verify_report(&fake_stress_summary).is_err());
    let mut fake_stress_report = base_report.clone();
    let mut stress_report = fake_stress_report.cost_stress[0].report.clone();
    stress_report["params_sha256"] = json!("4".repeat(64));
    fake_stress_report.cost_stress[0].report = stress_report;
    fake_stress_report.cost_stress[0].report_sha256 =
        candidate_execution::artifact_sha256(&fake_stress_report.cost_stress[0].report).unwrap();
    assert!(candidate_execution::verify_report(&fake_stress_report).is_err());

    for (reason, expected) in [
        (
            candidate_execution::AdapterRejectionReason::QuoteUnavailable,
            "entry_quote_unavailable",
        ),
        (
            candidate_execution::AdapterRejectionReason::QuoteStale,
            "entry_quote_stale",
        ),
        (
            candidate_execution::AdapterRejectionReason::InstrumentUnsupported,
            "entry_instrument_unsupported",
        ),
        (
            candidate_execution::AdapterRejectionReason::SegmentUnsupported,
            "entry_segment_unsupported",
        ),
        (
            candidate_execution::AdapterRejectionReason::PriceInvalid,
            "entry_price_invalid",
        ),
    ] {
        assert_eq!(serde_json::to_value(reason).unwrap(), json!(expected));
        assert_eq!(reason.as_str(), expected);
    }

    let before = at - 500;
    let between_rows = vec![
        json!({"at":before,"available_at":before,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",before,before,"99","100",10)]}),
        json!({"at":at+500,"available_at":at+500,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+500,at+500,"101","102",10)]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10)]}),
    ];
    let between_events = encode(&between_rows);
    let between_manifest = candidate_manifest(&between_events);
    let between_report = candidate_execution::execute(
        &base_signals,
        &plain_spec,
        &between_manifest,
        &between_events,
    )
    .unwrap();
    assert_eq!(between_report.decision_timeline[0].quote.source_ms, before);
    assert_eq!(between_report.decision_timeline[0].quote.receipt_ms, before);
    assert_eq!(between_report.decision_timeline[0].signal_ms, at);
    let between_raw = between_report.report.as_ref().unwrap();
    assert_eq!(between_raw["outcomes"][0]["requested_at_ms"], at);
    assert_eq!(between_raw["outcomes"][0]["entry_quote_source_ms"], before);
    assert_eq!(
        between_raw["outcomes"][0]["exit_quote_source_ms"],
        at + 1000
    );
    assert_eq!(between_raw["portfolio"]["fills"][0]["at"], at);

    let stale = at - 2001;
    let stale_rows = vec![
        json!({"at":stale,"available_at":stale,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",stale,stale,"99","100",10)]}),
        json!({"at":at+500,"available_at":at+500,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+500,at+500,"101","102",10)]}),
    ];
    let stale_events = encode(&stale_rows);
    let stale_manifest = candidate_manifest(&stale_events);
    let stale_report =
        candidate_execution::execute(&base_signals, &plain_spec, &stale_manifest, &stale_events)
            .unwrap();
    assert!(!stale_report.replay_executed);
    assert!(stale_report.retrospective);
    assert_eq!(stale_report.result_kind, "execution_unavailable");
    assert_eq!(
        stale_report.execution_status,
        candidate_execution::ExecutionStatus::Unavailable
    );
    assert!(stale_report.summary.is_none());
    assert_eq!(
        stale_report.initial_capital_assumption,
        Amount::parse("5000").unwrap()
    );
    assert!(stale_report.action_tape.is_empty());
    assert_eq!(stale_report.attempts, 1);
    assert_eq!(stale_report.rejections.len(), 1);
    assert_eq!(
        stale_report.rejections[0].reason,
        candidate_execution::AdapterRejectionReason::QuoteStale
    );
    assert_eq!(
        stale_report.rejections[0]
            .latest_quote
            .as_ref()
            .unwrap()
            .source_ms,
        stale
    );
    assert_eq!(
        stale_report.rejections[0]
            .latest_quote
            .as_ref()
            .unwrap()
            .receipt_ms,
        stale
    );
    assert!(stale_report.report.is_none());
    assert_eq!(stale_report.report_sha256, None);
    assert_eq!(stale_report.evidence.executable_observations, 2);
    let stale_value = serde_json::to_value(&stale_report).unwrap();
    assert_eq!(stale_value["result_kind"], "execution_unavailable");
    assert_eq!(stale_value["execution_status"], "unavailable");
    assert_eq!(stale_value["retrospective"], true);
    assert_eq!(stale_value["prospective"], false);
    assert_eq!(stale_value["prospective_authoring"], false);
    assert_eq!(stale_value["no_lookahead_qualified"], false);
    assert_eq!(stale_value["holdout_qualified"], false);
    assert_eq!(stale_value["edge_qualified"], false);
    assert_eq!(stale_value["promotion_authorized"], false);
    assert_eq!(stale_value["activation_authorized"], false);
    assert!(stale_value["summary"].is_null());
    assert_eq!(stale_value["initial_capital_assumption"], "5000");
    assert!(stale_value["report"].is_null());
    assert!(stale_value["report_sha256"].is_null());
    assert_eq!(stale_value["rejections"][0]["reason"], "entry_quote_stale");
    assert_eq!(
        stale_value["action_tape_sha256"],
        candidate_execution::artifact_sha256(&stale_value["action_tape"]).unwrap()
    );
    assert_eq!(
        stale_value["execution_spec_sha256"],
        candidate_execution::artifact_sha256(&stale_value["execution_spec"]).unwrap()
    );
    assert_eq!(
        stale_value["params_sha256"],
        candidate_execution::artifact_sha256(&stale_value["execution_spec"]["parameters"]).unwrap()
    );
    assert_eq!(
        stale_value["scoring"]["scored_signals_sha256"],
        candidate_execution::artifact_sha256(&stale_value["scored_signals"]).unwrap()
    );

    candidate_execution::verify_report(&stale_report).unwrap();
    candidate_execution::verify_report_against_manifest(
        &stale_report,
        &stale_manifest,
        &stale_events,
    )
    .unwrap();
    let unavailable_spec = candidate_spec("5000", "0", 2, 1, 0, true, vec![]);
    let unavailable_report = candidate_execution::execute(
        &base_signals,
        &unavailable_spec,
        &stale_manifest,
        &stale_events,
    )
    .unwrap();
    assert_eq!(
        unavailable_report.execution_status,
        candidate_execution::ExecutionStatus::Unavailable
    );
    candidate_execution::verify_report(&unavailable_report).unwrap();
    let mut bad_unavailable_id = unavailable_report.clone();
    bad_unavailable_id.candidate_id = "0".repeat(64);
    assert!(candidate_execution::verify_report(&bad_unavailable_id).is_err());
    let mut bad_unavailable_time = unavailable_report.clone();
    bad_unavailable_time.as_of_ms += 1;
    assert!(
        candidate_execution::verify_report_against_manifest(
            &bad_unavailable_time,
            &stale_manifest,
            &stale_events,
        )
        .is_err()
    );
    let mut bad_unavailable_truncated = unavailable_report.clone();
    bad_unavailable_truncated.truncated = true;
    assert!(candidate_execution::verify_report(&bad_unavailable_truncated).is_err());
    let mut bad_unavailable_rejections = unavailable_report.clone();
    bad_unavailable_rejections.rejections.clear();
    assert!(candidate_execution::verify_report(&bad_unavailable_rejections).is_err());

    let missing_rows = vec![
        json!({"at":at+500,"available_at":at+500,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+500,at+500,"101","102",10)]}),
    ];
    let missing_events = encode(&missing_rows);
    let missing_manifest = candidate_manifest(&missing_events);
    let missing_report = candidate_execution::execute(
        &base_signals,
        &plain_spec,
        &missing_manifest,
        &missing_events,
    )
    .unwrap();
    assert!(missing_report.action_tape.is_empty());
    assert!(missing_report.summary.is_none());
    assert_eq!(missing_report.result_kind, "execution_unavailable");
    assert_eq!(
        missing_report.execution_status,
        candidate_execution::ExecutionStatus::Unavailable
    );
    assert_eq!(
        missing_report.rejections[0].reason,
        candidate_execution::AdapterRejectionReason::QuoteUnavailable
    );
    assert!(missing_report.rejections[0].latest_quote.is_none());
    assert_eq!(
        missing_report.rejections[0].reason.as_str(),
        "entry_quote_unavailable"
    );

    let edge = at - 2000;
    let edge_rows = vec![
        json!({"at":edge,"available_at":edge,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",edge,edge,"99","100",10)]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10)]}),
    ];
    let edge_events = encode(&edge_rows);
    let edge_manifest = candidate_manifest(&edge_events);
    let edge_report =
        candidate_execution::execute(&base_signals, &plain_spec, &edge_manifest, &edge_events)
            .unwrap();
    assert_eq!(edge_report.action_tape.len(), 1);
    assert_eq!(edge_report.summary.as_ref().unwrap().entry_fills, 1);

    let repeated_rows = vec![
        json!({"at":at,"available_at":at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+100,"available_at":at+100,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10)]}),
    ];
    let repeated_events = encode(&repeated_rows);
    let repeated_manifest = candidate_manifest(&repeated_events);
    let repeated_signals = vec![
        ScoredSignal {
            at_ms: at,
            ordered_keys: vec!["SYNTHETIC|A".into()],
        },
        ScoredSignal {
            at_ms: at + 500,
            ordered_keys: vec!["SYNTHETIC|A".into()],
        },
    ];
    let repeated_spec = candidate_spec("5000", "0", 2, 1, 0, false, vec![]);
    let repeated_report = candidate_execution::execute(
        &repeated_signals,
        &repeated_spec,
        &repeated_manifest,
        &repeated_events,
    )
    .unwrap();
    assert_eq!(repeated_report.action_tape.len(), 2);
    assert_eq!(repeated_report.evidence.executable_observations, 3);
    assert_eq!(
        repeated_report.evidence.unique_executable_observations,
        Some(2)
    );
    assert_eq!(repeated_report.evidence.identical_republications, Some(1));
    let repeated_summary = repeated_report.summary.as_ref().unwrap();
    assert_eq!(repeated_summary.fills, 2);
    assert_eq!(repeated_summary.entry_fills, 1);
    assert_eq!(repeated_summary.exit_fills, 1);
    assert_eq!(
        repeated_summary
            .engine_outcomes
            .get("entry_execution_rejected"),
        Some(&1)
    );
    assert_eq!(
        repeated_report.report.as_ref().unwrap()["outcomes"][1]["state"],
        "entry_execution_rejected"
    );
    assert_eq!(repeated_report.decision_timeline[0].quote.event_at_ms, at);
    assert_eq!(
        repeated_report.decision_timeline[0].quote.available_at_ms,
        at
    );
    assert_eq!(repeated_report.decision_timeline[0].quote.source_ms, at);
    assert_eq!(repeated_report.decision_timeline[0].quote.receipt_ms, at);
    assert_eq!(repeated_report.decision_timeline[1].quote.event_at_ms, at);
    assert_eq!(
        repeated_report.decision_timeline[1].quote.available_at_ms,
        at
    );
    assert_eq!(repeated_report.decision_timeline[1].quote.source_ms, at);
    assert_eq!(repeated_report.decision_timeline[1].quote.receipt_ms, at);

    let republish_rows = vec![
        json!({"at":at,"available_at":at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+100,"available_at":at+100,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+1500,"available_at":at+1500,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1500,at+1500,"110","111",10)]}),
    ];
    let republish_events = encode(&republish_rows);
    let republish_manifest = candidate_manifest(&republish_events);
    let republish_signals = vec![ScoredSignal {
        at_ms: at + 500,
        ordered_keys: vec!["SYNTHETIC|A".into()],
    }];
    let republish_report = candidate_execution::execute(
        &republish_signals,
        &plain_spec,
        &republish_manifest,
        &republish_events,
    )
    .unwrap();
    assert_eq!(republish_report.evidence.executable_observations, 3);
    assert_eq!(
        republish_report.evidence.unique_executable_observations,
        Some(2)
    );
    assert_eq!(republish_report.evidence.identical_republications, Some(1));
    let republish_summary = republish_report.summary.as_ref().unwrap();
    assert_eq!(republish_summary.entry_fills, 1);
    assert_eq!(republish_summary.exit_fills, 1);
    assert!(republish_summary.accounting_complete);
    assert_eq!(republish_report.decision_timeline[0].quote.event_at_ms, at);
    assert_eq!(
        republish_report.decision_timeline[0].quote.available_at_ms,
        at
    );
    assert_eq!(republish_report.decision_timeline[0].quote.source_ms, at);
    assert_eq!(republish_report.decision_timeline[0].quote.receipt_ms, at);
    assert_eq!(republish_report.decision_timeline[0].signal_ms, at + 500);
    let republish_raw = republish_report.report.as_ref().unwrap();
    assert_eq!(republish_raw["outcomes"][0]["entry_quote_source_ms"], at);
    assert_eq!(republish_raw["outcomes"][0]["entry_quote_receipt_ms"], at);
    assert_eq!(
        republish_raw["outcomes"][0]["exit_quote_source_ms"],
        at + 1500
    );
    assert_eq!(republish_raw["portfolio"]["fills"][0]["at"], at + 500);

    let metadata_report = |observation: Value| {
        let rows = vec![
            json!({"at":at,"available_at":at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
            json!({"at":at+100,"available_at":at+100,"kind":"protection","observations":[observation]}),
            json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10)]}),
        ];
        let events = encode(&rows);
        let manifest = candidate_manifest(&events);
        candidate_execution::execute(&base_signals, &plain_spec, &manifest, &events)
    };
    let mut changed_volume = exec_quote("SYNTHETIC|A", at, at, "99", "100", 10);
    changed_volume["volume"] = json!(10001);
    assert_eq!(
        metadata_report(changed_volume).unwrap_err(),
        "candidate_execution_quote_metadata_changed"
    );
    let mut changed_circuits = exec_quote("SYNTHETIC|A", at, at, "99", "100", 10);
    changed_circuits["lower_circuit_limit"] = json!("51");
    assert_eq!(
        metadata_report(changed_circuits).unwrap_err(),
        "candidate_execution_quote_metadata_changed"
    );
    let mut changed_depth = exec_quote("SYNTHETIC|A", at, at, "99", "100", 10);
    changed_depth["quote"]["ask_units"] = json!(20);
    assert_eq!(
        metadata_report(changed_depth).unwrap_err(),
        "candidate_execution_quote_metadata_changed"
    );

    let changed_rows = vec![
        json!({"at":at,"available_at":at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+100,"available_at":at+100,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",20)]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10)]}),
    ];
    let changed_events = encode(&changed_rows);
    let changed_manifest = candidate_manifest(&changed_events);
    assert_eq!(
        candidate_execution::execute(
            &base_signals,
            &plain_spec,
            &changed_manifest,
            &changed_events
        )
        .unwrap_err(),
        "candidate_execution_quote_metadata_changed"
    );

    let short_spec = candidate_spec("1000", "0", 1, 1, 0, true, vec![]);
    let short_report =
        candidate_execution::execute(&base_signals, &short_spec, &base_manifest, &base_events)
            .unwrap();
    assert_eq!(short_report.action_tape.len(), 1);
    assert_eq!(
        short_report.execution_status,
        candidate_execution::ExecutionStatus::Executed
    );
    let short_summary = short_report.summary.as_ref().unwrap();
    assert_eq!(short_summary.fills, 0);
    assert_eq!(short_summary.fees, Amount::ZERO);
    assert_eq!(short_summary.turnover, Amount::ZERO);
    assert_eq!(short_summary.cash, Amount::parse("1000").unwrap());
    assert_eq!(short_summary.net_pnl_if_flat, Some(Amount::ZERO));
    assert_eq!(
        short_summary.marked_equity,
        Some(Amount::parse("1000").unwrap())
    );
    assert!(short_summary.flat);
    assert!(short_summary.accounting_complete);
    assert_eq!(
        short_report.report.as_ref().unwrap()["outcomes"][0]["state"],
        "entry_execution_rejected"
    );

    let gap_rows = vec![
        json!({"at":at,"available_at":at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+3001,"available_at":at+3001,"kind":"protection","observations":[]}),
    ];
    let gap_events = encode(&gap_rows);
    let gap_manifest = candidate_manifest(&gap_events);
    let gap_report =
        candidate_execution::execute(&base_signals, &plain_spec, &gap_manifest, &gap_events)
            .unwrap();
    let gap_summary = gap_report.summary.as_ref().unwrap();
    assert_eq!(gap_summary.fills, 1);
    assert!(!gap_summary.flat);
    assert!(!gap_summary.accounting_complete);
    assert_eq!(gap_summary.net_pnl_if_flat, None);
    assert_eq!(gap_summary.marked_equity, None);
    assert_eq!(
        gap_summary.residual_positions,
        vec!["SYNTHETIC|A".to_string()]
    );
    assert_eq!(gap_summary.unknown_marks, vec!["SYNTHETIC|A".to_string()]);
    assert_eq!(
        gap_summary.engine_outcomes.get("missing_exit_evidence"),
        Some(&1)
    );
    assert!(
        gap_summary
            .warnings
            .iter()
            .any(|warning| warning == "missing_exit_evidence")
    );
    assert!(gap_report.report.as_ref().unwrap()["net_pnl_if_flat"].is_null());
    assert!(gap_report.report.as_ref().unwrap()["marked_equity"].is_null());

    candidate_execution::verify_report(&gap_report).unwrap();
    let mut gap_fake_shared = gap_report.clone();
    let mut gap_shared = gap_fake_shared.report.clone().unwrap();
    gap_shared["net_pnl_if_flat"] = json!("1");
    gap_shared["marked_equity"] = json!("1");
    gap_fake_shared.report_sha256 =
        Some(candidate_execution::artifact_sha256(&gap_shared).unwrap());
    gap_fake_shared.report = Some(gap_shared);
    assert!(candidate_execution::verify_report(&gap_fake_shared).is_err());
    let mut gap_fake_summary = gap_report.clone();
    gap_fake_summary.summary.as_mut().unwrap().net_pnl_if_flat = Some(Amount::parse("1").unwrap());
    assert!(candidate_execution::verify_report(&gap_fake_summary).is_err());

    let params_bytes = serde_json::to_vec(&base_spec.parameters).unwrap();
    let actual_source = serde_json::to_vec(
        &json!([{"at_ms":entry_at,"key":"SYNTHETIC|A","units":10,"max_price":"100"}]),
    )
    .unwrap();
    let actual_report =
        evaluate(&actual_source, &params_bytes, &base_manifest, &base_events).unwrap();
    assert_eq!(actual_report["source_sha256"], digest(&actual_source));
    assert_eq!(actual_report["params_sha256"], digest(&params_bytes));
    let tampered_source = serde_json::to_vec(
        &json!([{"at_ms":entry_at,"key":"SYNTHETIC|A","units":11,"max_price":"100"}]),
    )
    .unwrap();
    let tampered_report = evaluate(
        &tampered_source,
        &params_bytes,
        &base_manifest,
        &base_events,
    )
    .unwrap();
    assert_ne!(
        tampered_report["source_sha256"].as_str(),
        Some(base_report.action_tape_sha256.as_str())
    );
    assert_ne!(
        tampered_report["candidate_id"].as_str(),
        Some(base_report.candidate_id.as_str())
    );
    let mut tampered_manifest_value: Value = serde_json::from_slice(&base_manifest).unwrap();
    tampered_manifest_value["events_sha256"] = json!("0".repeat(64));
    let tampered_manifest = serde_json::to_vec(&tampered_manifest_value).unwrap();
    assert_eq!(
        candidate_execution::execute(&base_signals, &base_spec, &tampered_manifest, &base_events)
            .unwrap_err(),
        "strategy_archive_binding_mismatch"
    );
    let mut tampered_events_value: Vec<Value> = base_events
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).unwrap())
        .collect();
    tampered_events_value[0]["observations"][0]["quote"]["ask"] = json!("101");
    let tampered_events = encode(&tampered_events_value);
    assert_eq!(
        candidate_execution::execute(&base_signals, &base_spec, &base_manifest, &tampered_events)
            .unwrap_err(),
        "strategy_archive_binding_mismatch"
    );
    let mut altered_spec_value = serde_json::to_value(&base_spec).unwrap();
    altered_spec_value["parameters"]["costs"]["per_order"] = json!("2");
    let altered_spec: CandidateExecutionSpecV1 =
        serde_json::from_value(altered_spec_value).unwrap();
    let altered_report =
        candidate_execution::execute(&base_signals, &altered_spec, &base_manifest, &base_events)
            .unwrap();
    assert_ne!(altered_report.params_sha256, base_report.params_sha256);
    assert_ne!(altered_report.candidate_id, base_report.candidate_id);

    let mut nfo_spec_value = serde_json::to_value(&base_spec).unwrap();
    nfo_spec_value["segment"] = json!("nfo");
    let nfo_spec: CandidateExecutionSpecV1 = serde_json::from_value(nfo_spec_value).unwrap();
    assert_eq!(
        candidate_execution::execute(&base_signals, &nfo_spec, &base_manifest, &base_events)
            .unwrap_err(),
        "candidate_execution_unsupported_segment"
    );
    let mut open_spec_value = serde_json::to_value(&base_spec).unwrap();
    open_spec_value["entry_at"] = json!("open");
    let open_spec: CandidateExecutionSpecV1 = serde_json::from_value(open_spec_value).unwrap();
    assert_eq!(
        candidate_execution::execute(&base_signals, &open_spec, &base_manifest, &base_events)
            .unwrap_err(),
        "candidate_execution_unsupported_entry_rule"
    );
    let mut signal_exit_value = serde_json::to_value(&base_spec).unwrap();
    signal_exit_value["exit"] = json!("signal");
    let signal_exit: CandidateExecutionSpecV1 = serde_json::from_value(signal_exit_value).unwrap();
    assert_eq!(
        candidate_execution::execute(&base_signals, &signal_exit, &base_manifest, &base_events)
            .unwrap_err(),
        "candidate_execution_unsupported_exit_rule"
    );

    let screen_rows = vec![
        json!({"at":at,"available_at":at,"kind":"screen","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10)]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"screen","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10)]}),
    ];
    let screen_events = encode(&screen_rows);
    let screen_manifest = candidate_manifest(&screen_events);
    let screen_report =
        candidate_execution::execute(&base_signals, &plain_spec, &screen_manifest, &screen_events)
            .unwrap();
    assert!(screen_report.action_tape.is_empty());
    assert!(!screen_report.replay_executed);
    assert_eq!(screen_report.result_kind, "execution_unavailable");
    assert!(screen_report.summary.is_none());
    assert_eq!(screen_report.evidence.protection_events, 0);
    assert_eq!(screen_report.evidence.scanner_events, 2);
    assert_eq!(
        screen_report.evidence.missing_evidence,
        vec!["execution_or_protection".to_string()]
    );
    assert_eq!(
        screen_report.rejections[0].reason,
        candidate_execution::AdapterRejectionReason::QuoteUnavailable
    );
    assert!(screen_report.report.is_none());

    let nfo_manifest = serde_json::to_vec(&json!({"schema":1,"session":"2026-10-16","previous_session":"2026-10-15","capital":"5000",
        "universe":[
            {"key":"SYNTHETIC|A","symbol":"AAA","series":"EQ","tick":"0.05"},
            {"key":"NSE_FO|X","symbol":"FUT","series":"EQ","tick":"0.05"}
        ],"events_sha256":digest(&screen_events),"synthetic":true,"max_observation_gap_ms":1000,"history_directory":null,"provenance":{}})).unwrap();
    let nfo_signals = vec![ScoredSignal {
        at_ms: at,
        ordered_keys: vec!["NSE_FO|X".into()],
    }];
    let nfo_key_report =
        candidate_execution::execute(&nfo_signals, &plain_spec, &nfo_manifest, &screen_events)
            .unwrap();
    assert!(nfo_key_report.action_tape.is_empty());
    assert_eq!(nfo_key_report.result_kind, "execution_unavailable");
    assert!(nfo_key_report.summary.is_none());
    assert_eq!(
        nfo_key_report.rejections[0].reason,
        candidate_execution::AdapterRejectionReason::SegmentUnsupported
    );

    let mut bad_windows_value = serde_json::to_value(&plain_spec).unwrap();
    bad_windows_value["parameters"]["training_to_ms"] = json!(at);
    let bad_windows: CandidateExecutionSpecV1 = serde_json::from_value(bad_windows_value).unwrap();
    assert_eq!(
        candidate_execution::execute(&base_signals, &bad_windows, &base_manifest, &base_events)
            .unwrap_err(),
        "candidate_execution_parameters_invalid"
    );
    let early_signals = vec![ScoredSignal {
        at_ms: at - 1,
        ordered_keys: vec!["SYNTHETIC|A".into()],
    }];
    assert_eq!(
        candidate_execution::execute(&early_signals, &plain_spec, &base_manifest, &base_events)
            .unwrap_err(),
        "candidate_execution_signal_out_of_range"
    );
    let late_signals = vec![ScoredSignal {
        at_ms: at + 5001,
        ordered_keys: vec!["SYNTHETIC|A".into()],
    }];
    assert_eq!(
        candidate_execution::execute(&late_signals, &plain_spec, &base_manifest, &base_events)
            .unwrap_err(),
        "candidate_execution_signal_out_of_range"
    );
    let unordered_signals = vec![
        ScoredSignal {
            at_ms: at + 100,
            ordered_keys: vec!["SYNTHETIC|A".into()],
        },
        ScoredSignal {
            at_ms: at,
            ordered_keys: vec!["SYNTHETIC|A".into()],
        },
    ];
    assert_eq!(
        candidate_execution::execute(
            &unordered_signals,
            &plain_spec,
            &base_manifest,
            &base_events
        )
        .unwrap_err(),
        "candidate_execution_signals_invalid"
    );

    let first_signals = vec![
        ScoredSignal {
            at_ms: at,
            ordered_keys: vec!["SYNTHETIC|A".into()],
        },
        ScoredSignal {
            at_ms: at + 100,
            ordered_keys: vec!["SYNTHETIC|A".into()],
        },
    ];
    let first_spec = candidate_spec("5000", "0", 2, 1, 0, true, vec![]);
    let first_report =
        candidate_execution::execute(&first_signals, &first_spec, &base_manifest, &base_events)
            .unwrap();
    assert_eq!(first_report.attempts, 1);
    assert_eq!(first_report.action_tape.len(), 1);
    assert_eq!(first_report.summary.as_ref().unwrap().entry_fills, 1);

    let two_key_rows = vec![
        json!({"at":at,"available_at":at,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at,at,"99","100",10),exec_quote("SYNTHETIC|B",at,at,"199","200",10)]}),
        json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[exec_quote("SYNTHETIC|A",at+1000,at+1000,"110","111",10),exec_quote("SYNTHETIC|B",at+1000,at+1000,"190","191",10)]}),
    ];
    let two_key_events = encode(&two_key_rows);
    let two_key_manifest = candidate_manifest(&two_key_events);
    let two_key_signals = vec![ScoredSignal {
        at_ms: at,
        ordered_keys: vec!["SYNTHETIC|A".into(), "SYNTHETIC|B".into()],
    }];
    let top_one_spec = candidate_spec("5000", "0", 1, 1, 0, true, vec![]);
    let top_one_report = candidate_execution::execute(
        &two_key_signals,
        &top_one_spec,
        &two_key_manifest,
        &two_key_events,
    )
    .unwrap();
    assert_eq!(top_one_report.attempts, 1);
    assert_eq!(top_one_report.action_tape[0].key, "SYNTHETIC|A");
    assert!(!top_one_report.truncated);
    let top_two_spec = candidate_spec("5000", "0", 1, 2, 0, true, vec![]);
    let top_two_report = candidate_execution::execute(
        &two_key_signals,
        &top_two_spec,
        &two_key_manifest,
        &two_key_events,
    )
    .unwrap();
    assert_eq!(top_two_report.attempts, 1);
    assert!(top_two_report.truncated);

    let offset_spec = candidate_spec("5000", "5", 1, 1, 1, true, vec![]);
    let offset_report =
        candidate_execution::execute(&base_signals, &offset_spec, &base_manifest, &base_events)
            .unwrap();
    assert_eq!(
        offset_report.decision_timeline[0].max_price,
        Price::parse("100.05").unwrap()
    );
    assert_eq!(offset_report.summary.as_ref().unwrap().entry_fills, 1);
    let no_offset_spec = candidate_spec("5000", "5", 1, 1, 0, true, vec![]);
    let no_offset_report =
        candidate_execution::execute(&base_signals, &no_offset_spec, &base_manifest, &base_events)
            .unwrap();
    assert_eq!(no_offset_report.summary.as_ref().unwrap().entry_fills, 0);

    // Virtual v1: the global archive SHA is NOT a receipt identity when one
    // key carries two actual observations/receipts. The strict virtual view
    // refuses the fallback; the default Task 8 view keeps its legacy behavior.
    let one_key_manifest = |events: &[u8]| {
        serde_json::to_vec(&json!({
            "schema":1,"session":"2026-10-16","previous_session":"2026-10-15","capital":"5000",
            "universe":[{"key":"SYNTHETIC|A","symbol":"AAA","series":"EQ","tick":"0.05"}],
            "events_sha256":digest(events),"synthetic":true,"max_observation_gap_ms":1000,
            "history_directory":null,
            "provenance":{"archive_sha256":digest(b"whole-tape-last-receipt"),"per_key":{}}
        }))
        .unwrap()
    };
    let receipt_quote = json!({
        "key":"SYNTHETIC|A",
        "quote":{"bid":"99","ask":"100","bid_units":10,"ask_units":10,"source":at,"receipt":at},
        "lower_circuit_limit":"50","upper_circuit_limit":"200","volume":10000,
        "archive_sha256":digest(b"original-receipt"),
        "purpose":"research_observation"
    });
    let mut second_receipt = receipt_quote.clone();
    second_receipt["quote"]["receipt"] = json!(at + 1);
    second_receipt["quote"]["source"] = json!(at + 1);
    second_receipt["archive_sha256"] = json!(digest(b"second-original-receipt"));
    let two_receipts = encode(&[
        json!({
            "at":at,
            "available_at":at,
            "kind":"entry",
            "observations":[receipt_quote.clone()]
        }),
        json!({
            "at":at + 1,
            "available_at":at + 1,
            "kind":"entry",
            "observations":[second_receipt]
        }),
    ]);
    let strict_two = archive_view_virtual(&one_key_manifest(&two_receipts), &two_receipts).unwrap();
    assert!(
        strict_two.single_key_archive_sha.is_none(),
        "virtual v1 accepted a global archive SHA for two receipts"
    );
    let legacy_two = archive_view(&one_key_manifest(&two_receipts), &two_receipts).unwrap();
    assert!(
        legacy_two.single_key_archive_sha.is_some(),
        "the default Task 8 legacy view changed"
    );
    let one_receipt = encode(&[json!({
        "at":at,
        "available_at":at,
        "kind":"entry",
        "observations":[receipt_quote.clone()]
    })]);
    let strict_one = archive_view_virtual(&one_key_manifest(&one_receipt), &one_receipt).unwrap();
    assert!(
        strict_one.single_key_archive_sha.is_some(),
        "the provable single-observation legacy fallback was removed"
    );
}
