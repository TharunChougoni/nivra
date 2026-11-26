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
}

pub fn parse_archive_events(
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
        digest(format!("cash_quote_action_tape_v1\0{source_hash}\0{params_hash}").as_bytes());
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
                match event.kind {
                    // Screening records are not executable depth.
                    EventKind::Screen => continue,
                    EventKind::Protection | EventKind::Entry => {}
                }
                match quotes.get(&o.key) {
                    Some(old)
                        if o.quote.receipt < old.quote.receipt
                            || o.quote.source < old.quote.source =>
                    {
                        return Err("action_replay_quote_regressed".into());
                    }
                    Some(old) if o.quote.receipt == old.quote.receipt => {
                        if o.quote != old.quote {
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
            };
            if let Some(liquidity) = quotes.get_mut(&entry.key) {
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
    Ok(
        json!({"schema":1,"candidate_id":candidate_id,"source_sha256":source_hash,"params_sha256":params_hash,
        "feature_abi":"cash_quote_action_tape_v1","parameters":p,"source":entries,
        "manifest_sha256":digest(manifest),"events_sha256":digest(events),"declared_synthetic":source_manifest.get("synthetic"),
        "engine_sha256":nivra_core::replay::engine_fingerprint(),"adapter_sha256":digest(include_bytes!("strategy_actions.rs")),
        "as_of_ms":last_at,"outcomes":outcomes,"portfolio":portfolio,"fees":fees,"turnover":turnover,
        "net_pnl_if_flat":net_pnl_if_flat,"marked_equity":marked_equity,"missing_mark_keys":missing_marks,
        "scientific_state":"insufficient_evidence","promotion_authorized":false,
        "limitations":["declared_action_schedule_does_not_establish_prospective_authoring_or_no_lookahead",
        "capture_identity_and_cost_assumptions_are_not_provider_certification",
        "no_oos_walk_forward_or_multiple_testing_qualification"]}),
    )
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
        .map(|outcome| outcome.is_ok())
        .map_err(|_| "action_replay_execution_failed".into())
}

#[cfg(test)]
pub(crate) fn verify_action_replay() {
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
        "universe":[{"key":"SYNTHETIC|A","symbol":"FIXTURE","series":"EQ","tick":"0.05"}],"events_sha256":digest(events),"synthetic":true,"max_observation_gap_ms":1000,"provenance":{}})).unwrap()
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
}
