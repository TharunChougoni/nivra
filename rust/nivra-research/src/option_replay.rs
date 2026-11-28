//! Historical, hypothetical F&O gate comparisons. Inputs are archived gate
//! observations and option touches. The only ledger is a fresh virtual book.
use chrono::NaiveDate;
use nivra_core::{
    autoresearch::ist_minute,
    execution::{ExitIntent, Instrument, Portfolio, Quote},
    finance::{Amount, FeeArithmetic, FeeSchedule, Price},
    market::{MAX_FINANCIAL_AGE_MS, OptionCatalog, OptionContract, Segment},
    sleeve::{Allocation, SizingInput, SizingPolicy, Sleeve, size},
    time::ExchangeDate,
};
use nivra_store::market_archive::{
    ArchiveClient,
    options::{
        GateCandidate, GateObservation, OptionSpot, option_gates, option_master, option_window,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub const ABI: &str = "option_gate_replay_v1";
pub const TAPE_UNAVAILABLE: &str = "option_tape_unavailable";
const HOLD_MS: i64 = 10_000;
const MAX_POLL_GAP_MS: i64 = 4_000;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ranking {
    Gate,
    TightestSpread,
    LowestCost,
    DeepestTouch,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub fees: FeeArithmetic,
    pub sizing: SizingPolicy,
    pub capital: Amount,
    pub session_start_minute: u32,
    pub session_end_minute: u32,
}

#[derive(Debug, Serialize)]
pub struct Cohort {
    session: NaiveDate,
    observations: Vec<Opportunity>,
}

impl Cohort {
    pub fn observations(&self) -> usize {
        self.observations.len()
    }
}

#[derive(Debug, Serialize)]
struct Opportunity {
    at: i64,
    gate: GateObservation,
    contracts: BTreeMap<String, OptionContract>,
    exits: BTreeMap<String, Option<Vec<Quote>>>,
}

/// Freeze the first non-overlapping gate observations in a single IST
/// session. The poll following the holding deadline is shared by both arms.
/// No filter here depends on a proposed ranking or a subsequent return.
pub fn read(
    db: &mut ArchiveClient,
    session: NaiveDate,
    watermark: i64,
    stop: &dyn Fn() -> bool,
) -> Result<Cohort> {
    let mut observations = Vec::new();
    let mut next = 0;
    for (at, gate) in option_gates(db, session, watermark)? {
        if stop() {
            return Err("decision_preempted".into());
        }
        if at < next {
            continue;
        }
        next = at.saturating_add(HOLD_MS + MAX_POLL_GAP_MS + 1);
        let contracts = option_master(db, session, at, watermark)?
            .into_iter()
            .flat_map(|master| master.contracts)
            .map(|contract| (contract.key.clone(), contract))
            .collect();
        let window = option_window(db, session, at, next - 1, watermark)?;
        let mut polls = window
            .iter()
            .flatten()
            .filter(|(time, _)| *time > at)
            .take_while(|(time, _)| *time < next)
            .scan(false, |end, poll| {
                if *end {
                    return None;
                }
                *end = poll.0 >= at + HOLD_MS;
                Some(poll)
            })
            .collect::<Vec<_>>();
        let complete = polls.last().is_some_and(|poll| poll.0 >= at + HOLD_MS)
            && polls
                .first()
                .is_some_and(|poll| poll.0 - at <= MAX_POLL_GAP_MS)
            && polls
                .windows(2)
                .all(|pair| pair[1].0 - pair[0].0 <= MAX_POLL_GAP_MS);
        if !complete {
            polls.clear();
        }
        let exits = gate
            .candidates
            .iter()
            .map(|candidate| {
                let key = &candidate.instrument_key;
                let series = (!polls.is_empty())
                    .then(|| {
                        polls
                            .iter()
                            .map(|(time, poll)| {
                                poll.rows
                                    .iter()
                                    .find(|row| &row.instrument_key == key)?
                                    .quote
                                    .as_ref()
                                    .filter(|quote| quote.receipt == *time)
                                    .cloned()
                            })
                            .collect::<Option<Vec<_>>>()
                    })
                    .flatten();
                (key.clone(), series)
            })
            .collect();
        observations.push(Opportunity {
            at,
            gate,
            contracts,
            exits,
        });
    }
    Ok(Cohort {
        session,
        observations,
    })
}

#[derive(Debug, PartialEq, Serialize)]
pub struct TradeOutcome {
    at_ms: i64,
    instrument_key: Option<String>,
    reason: &'static str,
    units: u64,
    net_pnl: Option<Amount>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Arm {
    pub net_pnl: Option<Amount>,
    pub trades: usize,
    pub equity_sleeve_cash: Amount,
    pub outcomes: Vec<TradeOutcome>,
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub session: NaiveDate,
    pub stage: &'static str,
    pub ranking: Ranking,
    pub reason: Option<&'static str>,
    pub observations: usize,
    pub starting_options_cash: Amount,
    pub proposed: Arm,
    pub frozen_gate: Arm,
    pub hypothetical_difference: Option<Amount>,
    pub execution_authorized: bool,
}

fn virtual_book(policy: &Policy) -> Portfolio {
    let mut book = Portfolio::with_allocations(vec![Allocation {
        at_ms: 0,
        sleeve: Sleeve::Options,
        amount: policy.capital,
        reason: ABI.into(),
    }]);
    book.fee_schedule = Some(
        FeeSchedule::new(policy.fees.clone()).with_segment(Segment::NseFo, policy.fees.clone()),
    );
    book.settings.session_start_minute = policy.session_start_minute;
    book.settings.session_end_minute = policy.session_end_minute;
    book
}

fn selection(gate: &GateObservation, ranking: Ranking) -> Option<&str> {
    if matches!(ranking, Ranking::Gate) {
        let verdict = gate.verdict.as_ref()?;
        if verdict["escalate"] != true {
            return None;
        }
        return verdict
            .get("prospects")?
            .get(0)?
            .get("instrument_key")?
            .as_str();
    }
    gate.candidates
        .iter()
        .filter(|candidate| candidate.tradable)
        .min_by(|a, b| {
            let order = match ranking {
                Ranking::Gate => unreachable!("frozen replay returns above"),
                Ranking::TightestSpread => a.spread_ticks.cmp(&b.spread_ticks),
                Ranking::LowestCost => a.round_trip_cost_bps.total_cmp(&b.round_trip_cost_bps),
                Ranking::DeepestTouch => b.depth_lots.cmp(&a.depth_lots),
            };
            order
                .then_with(|| a.offset.unsigned_abs().cmp(&b.offset.unsigned_abs()))
                .then_with(|| a.instrument_key.cmp(&b.instrument_key))
        })
        .map(|candidate| candidate.instrument_key.as_str())
}

fn spot_fresh(spot: &OptionSpot, at: i64) -> bool {
    spot.source_ms > 0
        && spot.source_ms <= spot.receipt_ms
        && (0..=MAX_FINANCIAL_AGE_MS).contains(&at.saturating_sub(spot.receipt_ms))
}

struct Trade<'a> {
    session: ExchangeDate,
    at: i64,
    spot: &'a OptionSpot,
    contract: &'a OptionContract,
    candidate: &'a GateCandidate,
    exits: Option<&'a [Quote]>,
}

fn trade(
    book: &mut Portfolio,
    policy: &Policy,
    t: Trade<'_>,
) -> Result<std::result::Result<(u64, Amount), &'static str>> {
    if t.contract.expiry <= t.session {
        return Ok(Err("option_contract_invalid"));
    }
    let entry = &t.candidate.quote;
    if entry.source <= 0 || entry.source > entry.receipt || !entry.fresh(t.at, MAX_FINANCIAL_AGE_MS)
    {
        return Ok(Err("stale_quote"));
    }
    let session = policy.session_start_minute..policy.session_end_minute;
    if !session.contains(&ist_minute(t.at))
        || !session.contains(&ist_minute(t.at + HOLD_MS + MAX_POLL_GAP_MS))
    {
        return Ok(Err("outside_session"));
    }
    let tick = t.contract.tick;
    let fill_price = Portfolio::execution_with_costs(entry.ask, tick, true, &policy.fees)?;
    let stop_ticks = entry
        .ask
        .amount()
        .checked_mul(Amount::parse("0.8")?)?
        .checked_floor_ratio(tick.amount())?;
    let Ok(stop) = Price::try_from(
        tick.amount()
            .checked_mul(Amount::from_integer(i64::try_from(stop_ticks)?))?,
    ) else {
        return Ok(Err("size_stop_not_below_price"));
    };
    let cash = book.sleeve_cash(Sleeve::Options)?;
    let input = SizingInput {
        sleeve_equity: cash,
        sleeve_cash: cash,
        open_positions: 0,
        opens_position: true,
        held_value: Amount::ZERO,
        max_price: fill_price,
        stop,
        ask_units: entry.ask_units,
        lot: t.contract.lot_size,
        costs: &policy.fees,
        conviction: Amount::from_integer(1),
    };
    let units = match size(&policy.sizing, &input, None)? {
        Ok(units) => units,
        Err(reason) => return Ok(Err(reason.code())),
    };
    let Some(exits) = t.exits else {
        return Ok(Err(TAPE_UNAVAILABLE));
    };
    if exits.iter().any(|quote| {
        quote.source <= 0
            || quote.source > quote.receipt
            || !quote.fresh(quote.receipt, MAX_FINANCIAL_AGE_MS)
    }) {
        return Ok(Err("stale_exit_quote"));
    }
    let Some(exit) = exits
        .iter()
        .find(|quote| quote.bid <= stop || quote.receipt >= t.at + HOLD_MS)
    else {
        return Ok(Err(TAPE_UNAVAILABLE));
    };
    if exit.bid_units < units {
        return Ok(Err("insufficient_depth"));
    }

    let mut next = book.clone();
    let contract = t.contract;
    next.options = Some(OptionCatalog::try_new(
        t.session,
        contract.underlying_key.clone(),
        contract.underlying_symbol.clone(),
        contract.expiry,
        t.spot.price,
        [contract.clone()],
    )?);
    let instrument = Instrument {
        key: contract.key.clone(),
        symbol: contract.symbol.clone(),
        series: String::from(
            if contract.option == nivra_core::market::InstrumentKind::Call {
                "CE"
            } else {
                "PE"
            },
        ),
        tick,
    };
    for (at, quote, buy, limit) in [
        (t.at, entry, true, fill_price),
        (exit.receipt, exit, false, exit.bid),
    ] {
        if let Err(refusal) = next.execute_plan(
            at,
            &instrument,
            quote,
            buy,
            units,
            limit,
            ExitIntent::default(),
            ABI,
        )? {
            return Ok(Err(refusal.reason.code()));
        }
    }
    let pnl = next.sleeve_cash(Sleeve::Options)?.checked_sub(cash)?;
    *book = next;
    Ok(Ok((units, pnl)))
}

fn arm(policy: &Policy, cohort: &Cohort, ranking: Ranking) -> Result<Arm> {
    let mut book = virtual_book(policy);
    let session = ExchangeDate::parse(&cohort.session.to_string())?;
    let mut outcomes = Vec::new();
    for opportunity in &cohort.observations {
        let gate = &opportunity.gate;
        let key = selection(gate, ranking);
        let result = match (gate.spot.as_ref(), gate.verdict.as_ref(), key) {
            (Some(spot), Some(_), key) if spot_fresh(spot, opportunity.at) => match key {
                Some(key) => match (
                    opportunity.contracts.get(key),
                    gate.candidates.iter().find(|row| row.instrument_key == key),
                ) {
                    (Some(contract), Some(candidate)) if contract.underlying_key == spot.key => {
                        trade(
                            &mut book,
                            policy,
                            Trade {
                                session,
                                at: opportunity.at,
                                spot,
                                contract,
                                candidate,
                                exits: opportunity.exits.get(key).and_then(Option::as_deref),
                            },
                        )?
                    }
                    _ => Err(TAPE_UNAVAILABLE),
                },
                None if !gate.candidates.is_empty() => Err("no_prospect"),
                None => Err(TAPE_UNAVAILABLE),
            },
            _ => Err(TAPE_UNAVAILABLE),
        };
        let (units, reason, net_pnl) = match result {
            Ok((units, pnl)) => (units, "filled", Some(pnl)),
            Err(reason) => (0, reason, None),
        };
        outcomes.push(TradeOutcome {
            at_ms: opportunity.at,
            instrument_key: key.map(str::to_owned),
            reason,
            units,
            net_pnl,
        });
    }
    let complete = !outcomes.is_empty()
        && !outcomes.iter().any(|o| {
            matches!(
                o.reason,
                TAPE_UNAVAILABLE | "stale_exit_quote" | "insufficient_depth"
            )
        });
    Ok(Arm {
        net_pnl: complete.then_some(
            book.sleeve_cash(Sleeve::Options)?
                .checked_sub(policy.capital)?,
        ),
        trades: book.fills.len() / 2,
        equity_sleeve_cash: book.sleeve_cash(Sleeve::Cash)?,
        outcomes,
    })
}

pub fn compare(policy: &Policy, cohort: &Cohort, ranking: Ranking) -> Result<Comparison> {
    policy.fees.validate()?;
    policy.sizing.validate()?;
    if policy.capital <= Amount::ZERO {
        return Err("option_sleeve_capital_required".into());
    }
    let mut frozen_gate = arm(policy, cohort, Ranking::Gate)?;
    let mut proposed = arm(policy, cohort, ranking)?;
    let hypothetical_difference = proposed
        .net_pnl
        .zip(frozen_gate.net_pnl)
        .map(|(proposed, gate)| proposed.checked_sub(gate))
        .transpose()?;
    let reason = hypothetical_difference
        .is_none()
        .then_some(TAPE_UNAVAILABLE);
    if reason.is_some() {
        proposed.net_pnl = None;
        frozen_gate.net_pnl = None;
        for outcome in proposed
            .outcomes
            .iter_mut()
            .chain(&mut frozen_gate.outcomes)
        {
            outcome.net_pnl = None;
        }
    }
    Ok(Comparison {
        session: cohort.session,
        stage: ABI,
        ranking,
        reason,
        observations: cohort.observations(),
        starting_options_cash: policy.capital,
        proposed,
        frozen_gate,
        hypothetical_difference,
        execution_authorized: false,
    })
}
