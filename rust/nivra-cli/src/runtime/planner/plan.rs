use super::*;
use nivra_core::execution::{ExitIntent, Instrument, Portfolio, Quote};
use nivra_core::finance::{Amount, FinancialError, Money, Price, Side};
use nivra_core::market::{OptionOrder, PaperMandate, Segment};
use nivra_core::sleeve::{self, SizingInput, Sleeve};
use serde_json::Value;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PlanSide {
    Buy,
    Sell,
    #[serde(rename = "revise_exit")]
    ReviseExit,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Action {
    pub instrument_key: String,
    pub side: PlanSide,
    /// Required for a sell. A buy omits it and the sleeve policy sizes it; a
    /// decision-lane buy may name it, capped by the same policy.
    #[serde(default)]
    pub units: Option<u64>,
    /// Scales a buy's risk budget, in (0, 1]. Absent means 1.
    #[serde(default)]
    pub conviction: Option<Amount>,
    /// Required for buy/sell; absent for `revise_exit`.
    #[serde(default)]
    pub max_price: Option<Price>,
    pub rationale: String,
    pub exit_intent: ExitIntent,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TradePlan {
    pub actions: Vec<Value>,
    pub stance: String,
    pub notes: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_reasons: Vec<CandidateReason>,
    /// Optional advisory acknowledgements of directive IDs delivered to decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directive_acknowledgements: Option<Vec<String>>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CandidateReason {
    pub instrument_key: String,
    pub reason: String,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Verdict {
    pub index: usize,
    pub action: Option<Action>,
    pub units: u64,
    pub reason: String,
}
fn balanced_object_spans(raw: &str) -> Vec<(usize, usize)> {
    let bytes = raw.as_bytes();
    let mut spans = Vec::new();
    let mut stack = Vec::new();
    let mut in_string = false;
    let mut escape = false;
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match *byte {
            b'"' => in_string = true,
            b'{' => stack.push(index),
            b'}' => {
                if let Some(start) = stack.pop() {
                    spans.push((start, index + 1));
                }
            }
            _ => {}
        }
    }
    spans
}
fn decode_plan(raw: &str) -> Option<Value> {
    let value = nivra_core::native::decode(raw.as_bytes()).ok()?;
    serde_json::from_value::<TradePlan>(value.clone()).ok()?;
    Some(value)
}
/// The whole document, or the last balanced object that is a trade plan.
/// Prose before the JSON object is kept out of schema validation.
pub(crate) fn plan_document(raw: &str) -> Result<Value> {
    let mut found = decode_plan(raw);
    if found.is_none() {
        for (start, end) in balanced_object_spans(raw) {
            if let Some(value) = decode_plan(&raw[start..end]) {
                found = Some(value);
            }
        }
    }
    found.ok_or_else(|| "planner_json_invalid".into())
}
/// Reserve sequential cash/units so individually valid actions cannot overspend together.
#[cfg(test)]
pub(crate) fn validate(
    raw: &str,
    p: &Portfolio,
    instruments: &[Instrument],
    now: i64,
) -> Result<Vec<Verdict>> {
    validate_for_decision(raw, p, instruments, now, false)
}
/// Validate a decision-lane plan, optionally refusing cash-equity buys when an
/// active strategy owns cash entries. Risk-reducing sells, exit revisions and
/// Nifty options remain allowed.
pub(crate) fn validate_for_decision(
    raw: &str,
    p: &Portfolio,
    instruments: &[Instrument],
    now: i64,
    strategy_owns_cash_entries: bool,
) -> Result<Vec<Verdict>> {
    let decoded = plan_document(raw)?;
    let plan: TradePlan = serde_json::from_value(decoded).map_err(|_| "planner_schema_invalid")?;
    if plan.actions.len() > 200
        || plan.stance.len() > 256
        || plan.notes.len() > 8192
        || plan.candidate_reasons.len() > 200
        || plan.candidate_reasons.iter().any(|r| {
            r.instrument_key.is_empty()
                || r.instrument_key.len() > 256
                || r.reason.is_empty()
                || r.reason.len() > 2048
        })
    {
        return Err("planner_plan_bounds".into());
    }
    validate_actions_by(
        plan.actions,
        p,
        instruments,
        now,
        Proposer::Decision {
            strategy_owns_cash_entries,
        },
        &|_| None,
    )
}
/// Who proposed the actions. A strategy never sizes its buys; the decision
/// lane may name units, which the sleeve policy caps.
#[derive(Clone, Copy)]
pub(crate) enum Proposer {
    Strategy,
    Decision { strategy_owns_cash_entries: bool },
}
/// The per-action half of [`validate`] for a strategy's actions: the same
/// schema, allowlist, sizing, cash and held-unit checks as a model plan. A buy
/// is sized against the displayed ask depth in `quotes`.
pub(crate) fn validate_actions(
    actions: Vec<Value>,
    p: &Portfolio,
    instruments: &[Instrument],
    now: i64,
    quotes: &BTreeMap<String, Quote>,
) -> Result<Vec<Verdict>> {
    validate_actions_by(actions, p, instruments, now, Proposer::Strategy, &|key| {
        quotes.get(key).map(|q| q.ask_units)
    })
}
/// Sleeve state a plan's earlier accepted actions have already used.
struct Reserved {
    cash: BTreeMap<Sleeve, Amount>,
    held: BTreeMap<String, u64>,
}
fn validate_actions_by(
    actions: Vec<Value>,
    p: &Portfolio,
    instruments: &[Instrument],
    now: i64,
    proposer: Proposer,
    ask_units: &dyn Fn(&str) -> Option<u64>,
) -> Result<Vec<Verdict>> {
    if actions.is_empty() {
        return Ok(Vec::new());
    }
    let costs = p.costs.clone().ok_or("paper_costs_required")?;
    costs.validate()?;
    let mut reserved = Reserved {
        cash: Sleeve::ALL
            .into_iter()
            .map(|s| Ok((s, p.sleeve_cash(s)?)))
            .collect::<Result<_>>()?,
        held: p
            .positions
            .iter()
            .map(|(k, v)| (k.clone(), v.units))
            .collect(),
    };
    Ok(actions
        .into_iter()
        .enumerate()
        .map(|(index, v)| {
            match check_action(v, p, instruments, now, proposer, ask_units, &mut reserved) {
                Ok((a, units)) => Verdict {
                    index,
                    action: Some(a),
                    units,
                    reason: "accepted".into(),
                },
                Err(e) => Verdict {
                    index,
                    action: None,
                    units: 0,
                    reason: match e.to_string().as_str() {
                        code @ ("action_schema"
                        | "action_allowlist"
                        | "action_bounds"
                        | "action_time_exit_expired"
                        | "action_stop_required"
                        | "action_strategy_buy_units"
                        | "action_sizing_policy_missing"
                        | "sleeve_positions_full"
                        | "size_stop_not_below_price"
                        | "size_below_lot"
                        | "sleeve_cash_insufficient"
                        | "size_below_min_order"
                        | "sizing_policy_exceeded"
                        | "action_zero_units"
                        | "action_sell_unheld"
                        | "action_sell_units"
                        | "action_index_untradeable"
                        | "action_strategy_owns_cash_entries"
                        | "action_fee_schedule"
                        | "action_option_scope"
                        | "action_option_underlying"
                        | "action_option_quantity"
                        | "action_option_short_open"
                        | "action_option_expiry") => code.to_owned(),
                        _ => "action_arithmetic_invalid".into(),
                    },
                },
            }
        })
        .collect())
}
fn check_action(
    v: Value,
    p: &Portfolio,
    instruments: &[Instrument],
    now: i64,
    proposer: Proposer,
    ask_units: &dyn Fn(&str) -> Option<u64>,
    reserved: &mut Reserved,
) -> Result<(Action, u64)> {
    let a: Action = serde_json::from_value(v).map_err(|_| "action_schema")?;
    let i = instruments
        .iter()
        .find(|i| i.key == a.instrument_key)
        .ok_or("action_allowlist")?;
    let revise = matches!(a.side, PlanSide::ReviseExit);
    if a.rationale.len() > 2048
        || (revise && (a.units.is_some() || a.max_price.is_some()))
        || (!matches!(a.side, PlanSide::Buy) && a.conviction.is_some())
        || a.conviction
            .is_some_and(|c| c <= Amount::ZERO || c > Amount::from_integer(1))
        || a.exit_intent.time.is_some_and(|t| t <= 0)
        || a.exit_intent
            .target
            .zip(a.exit_intent.stop)
            .is_some_and(|(t, s)| t <= s)
    {
        return Err("action_bounds".into());
    }
    if revise {
        if !p.positions.contains_key(&i.key) {
            return Err("action_revise_unheld".into());
        }
        if a.exit_intent == ExitIntent::default() {
            return Err("action_revise_empty".into());
        }
        return Ok((a, 0));
    }
    let buy = matches!(a.side, PlanSide::Buy);
    if buy && a.exit_intent.time.is_some_and(|time| time <= now) {
        return Err("action_time_exit_expired".into());
    }
    let max_price = a.max_price.ok_or("action_bounds")?;
    // Segment identity is explicit: index keys are never tradeable, NSE_FO
    // keys need the bounded option catalog, and cash keeps the lot-1 path.
    let segment = Segment::from_key(&i.key);
    if segment == Some(Segment::NseIndex) {
        return Err("action_index_untradeable".into());
    }
    let is_fo = segment == Some(Segment::NseFo);
    if let Proposer::Decision {
        strategy_owns_cash_entries: true,
    } = proposer
        && !is_fo
        && buy
    {
        return Err("action_strategy_owns_cash_entries".into());
    }
    let fees = p.fee_policy(&i.key).map_err(|_| "action_fee_schedule")?;
    let option = p.option_contract(&i.key);
    if is_fo && option.is_none() {
        return Err("action_option_scope".into());
    }
    let sleeve = Sleeve::of_key(&i.key);
    let units = if buy {
        let stop = a.exit_intent.stop.ok_or("action_stop_required")?;
        if matches!(proposer, Proposer::Strategy) && a.units.is_some() {
            return Err("action_strategy_buy_units".into());
        }
        let policy = p
            .sizing
            .as_ref()
            .ok_or("action_sizing_policy_missing")?
            .get(sleeve);
        let opened = reserved
            .held
            .keys()
            .filter(|key| Sleeve::of_key(key) == sleeve)
            .count();
        let input = SizingInput {
            sleeve_equity: p.sleeve_equity(sleeve)?,
            sleeve_cash: reserved.cash[&sleeve],
            open_positions: u32::try_from(opened)?,
            opens_position: !reserved.held.contains_key(&i.key),
            held_value: max_price.amount().checked_mul(Amount::from_unsigned(
                reserved.held.get(&i.key).copied().unwrap_or(0),
            ))?,
            max_price,
            stop,
            ask_units: ask_units(&i.key).unwrap_or(u64::MAX),
            lot: option.map_or(1, |contract| contract.lot_size),
            costs: &fees,
            conviction: a.conviction.unwrap_or(Amount::from_integer(1)),
        };
        sleeve::size(policy, &input, a.units)?.map_err(|refusal| refusal.code())?
    } else {
        a.units.ok_or("action_sell_units")?
    };
    if units == 0 {
        return Err("action_zero_units".into());
    }
    if let Some(contract) = option {
        let catalog = p.options.as_ref().ok_or("action_option_scope")?;
        let order = OptionOrder {
            contract,
            buy,
            units,
            held_units: reserved.held.get(&i.key).copied().unwrap_or(0),
            today: catalog.session,
        };
        PaperMandate::NiftyOptionLong
            .admit_option(&order, &catalog.mandate())
            .map_err(option_action_reason)?;
    }
    if buy {
        let notional = max_price
            .amount()
            .checked_mul(Amount::from_unsigned(units))?;
        let cost =
            notional.checked_add(fees.charge(Side::Buy, Money::try_from(notional)?)?.amount())?;
        let cash = reserved.cash.get_mut(&sleeve).ok_or("action_sleeve")?;
        *cash = cash.checked_sub(cost)?;
        *reserved.held.entry(i.key.clone()).or_default() += units;
    } else {
        let n = reserved.held.get_mut(&i.key).ok_or("action_sell_unheld")?;
        if units > *n {
            return Err("action_sell_units".into());
        }
        *n -= units;
    }
    Ok((a, units))
}

fn option_action_reason(error: FinancialError) -> &'static str {
    match error {
        FinancialError::InvalidOptionMandate => "action_option_scope",
        FinancialError::OptionUnderlyingMismatch => "action_option_underlying",
        FinancialError::OptionQuantityGrid => "action_option_quantity",
        FinancialError::OptionShortOpen => "action_option_short_open",
        FinancialError::OptionExpiry => "action_option_expiry",
        _ => "action_option_scope",
    }
}
