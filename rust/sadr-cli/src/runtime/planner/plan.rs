use super::*;
use sadr_core::execution::{ExitIntent, Instrument, Portfolio};
use sadr_core::finance::{Amount, FinancialError, Money, Price, Side};
use sadr_core::market::{OptionOrder, PaperMandate, Segment};
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
    pub rupees: Option<Amount>,
    pub units: Option<u64>,
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
    let value = sadr_core::native::decode(raw.as_bytes()).ok()?;
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
pub(crate) fn validate(
    raw: &str,
    p: &Portfolio,
    instruments: &[Instrument],
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
    if plan.actions.is_empty() {
        return Ok(Vec::new());
    }
    let costs = p.costs.clone().ok_or("paper_costs_required")?;
    costs.validate()?;
    let mut cash = p.cash;
    let mut held: BTreeMap<_, _> = p
        .positions
        .iter()
        .map(|(k, v)| (k.clone(), v.units))
        .collect();
    Ok(plan
        .actions
        .into_iter()
        .enumerate()
        .map(|(index, v)| {
            let result = (|| -> Result<(Action, u64)> {
                let a: Action = serde_json::from_value(v).map_err(|_| "action_schema")?;
                let i = instruments
                    .iter()
                    .find(|i| i.key == a.instrument_key)
                    .ok_or("action_allowlist")?;
                let revise = matches!(a.side, PlanSide::ReviseExit);
                if a.rationale.len() > 2048
                    || (!revise && a.rupees.is_some() == a.units.is_some())
                    || (revise
                        && (a.rupees.is_some() || a.units.is_some() || a.max_price.is_some()))
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
                let max_price = a.max_price.ok_or("action_bounds")?;
                // Segment identity is explicit: index keys are never tradeable,
                // NSE_FO keys need the bounded option catalog, and cash keeps
                // the unchanged lot-1 path.
                let segment = Segment::from_key(&i.key);
                if segment == Some(Segment::NseIndex) {
                    return Err("action_index_untradeable".into());
                }
                let fees = p.fee_policy(&i.key).map_err(|_| "action_fee_schedule")?;
                let option = p.option_contract(&i.key);
                let is_fo = segment == Some(Segment::NseFo);
                if is_fo && option.is_none() {
                    return Err("action_option_scope".into());
                }
                let units = match (is_fo, a.units) {
                    (true, None) => {
                        let contract = option.ok_or("action_option_scope")?;
                        let per_lot = Price::try_from(
                            max_price
                                .amount()
                                .checked_mul(Amount::from_unsigned(contract.lot_size))?,
                        )?;
                        fees.affordable_cash_lots(
                            per_lot,
                            a.rupees.ok_or("action_size")?,
                            i64::MAX,
                            false,
                            0,
                        )?
                        .checked_mul(contract.lot_size)
                        .ok_or("action_zero_units")?
                    }
                    (_, Some(n)) => n,
                    (false, None) => fees.affordable_cash_lots(
                        max_price,
                        a.rupees.ok_or("action_size")?,
                        i64::MAX,
                        false,
                        0,
                    )?,
                };
                if units == 0 {
                    return Err("action_zero_units".into());
                }
                if is_fo {
                    let contract = option.ok_or("action_option_scope")?;
                    let catalog = p.options.as_ref().ok_or("action_option_scope")?;
                    let order = OptionOrder {
                        contract,
                        buy: matches!(a.side, PlanSide::Buy),
                        units,
                        held_units: held.get(&i.key).copied().unwrap_or(0),
                        today: catalog.session,
                    };
                    PaperMandate::NiftyOptionLong
                        .admit_option(&order, &catalog.mandate())
                        .map_err(option_action_reason)?;
                }
                match a.side {
                    PlanSide::Buy => {
                        let notional = max_price
                            .amount()
                            .checked_mul(Amount::from_unsigned(units))?;
                        let cost = notional.checked_add(
                            fees.charge(Side::Buy, Money::try_from(notional)?)?.amount(),
                        )?;
                        if cost > cash || a.rupees.is_some_and(|r| r <= Amount::ZERO || cost > r) {
                            return Err("action_cash".into());
                        }
                        cash = cash.checked_sub(cost)?;
                    }
                    PlanSide::Sell => {
                        let n = held.get_mut(&i.key).ok_or("action_sell_unheld")?;
                        if a.units.is_none() || units > *n {
                            return Err("action_sell_units".into());
                        }
                        *n -= units;
                    }
                    PlanSide::ReviseExit => unreachable!("handled above"),
                }
                Ok((a, units))
            })();
            match result {
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
                        | "action_size"
                        | "action_zero_units"
                        | "action_existing_position"
                        | "action_cash"
                        | "action_sell_unheld"
                        | "action_sell_units"
                        | "action_index_untradeable"
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
