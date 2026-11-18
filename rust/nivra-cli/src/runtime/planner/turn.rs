//! Digest turn production, bounded reuse, trigger envelopes and durable results.
use super::*;

/// Feature rows ranked into the brief before tools answer the rest.
const SHORTLIST_N: usize = 200;
/// The evidence every decision tool reads: features, discovery, mandate,
/// book state, recent fills and session performance. Shared by the harness
/// and the pi lanes so both serve the same facts.
pub(in crate::runtime) fn call_time_input(
    c: &Config,
    cfg: &PlannerConfig,
    p: &Portfolio,
    instruments: &[Instrument],
    clock: &dyn Fn() -> i64,
) -> Result<(Value, Value)> {
    let (mut input, references) = bundle_with_references(c, p, instruments, SHORTLIST_N)?;
    if let Some(settings) = &cfg.discovery {
        input["discovery"] = super::super::discovery::snapshot(c, instruments, settings, clock())?;
    }
    input["paper_mandate"] = json!({"paper_only":true,"capital":c.allocation()?,"risk_caps":"none","overnight_holds_allowed":true,"initialization":"validated_universe_then_history_then_pi_decision_then_fresh_quote_execution"});
    input["paper_mandate"]["products"] = if c.nifty_options.enabled {
        json!(["cash_equity", "nifty_option_long"])
    } else {
        json!(["cash_equity"])
    };
    // Additive bounded option shortlist plus spot; never the cash feature store.
    if let Some(nifty_options) = super::super::options::snapshot(c, p, clock())? {
        input["nifty_options"] = nifty_options;
    }
    input["book_state"] = json!({"halted":p.halted,"stopped":p.stopped,"closed":p.closed,"health":p.health,"model_led":p.model_led});
    // Call-time portfolio context: bounded original fills and exact session
    // performance. The financial ledger remains authoritative; this is display
    // and review evidence only.
    let fill_limit = cfg.effective_tools().fill_limit;
    let start = p.fills.len().saturating_sub(fill_limit);
    input["fills"] = json!(&p.fills[start..]);
    input["fills_total"] = json!(p.fills.len());
    let realized = p.realised_net_pnl();
    let mut session_realized = Some(Amount::ZERO);
    let mut session_fees = Amount::ZERO;
    for (fill, pnl) in p.fills.iter().zip(realized.iter()) {
        if local(fill.at).date_naive() == c.session {
            session_fees = session_fees.checked_add(fill.fee)?;
            session_realized = match (session_realized, pnl) {
                (Some(total), Some(value)) => Some(total.checked_add(*value)?),
                _ => None,
            };
        }
    }
    input["account_performance"] = json!({
        "session_realized_pnl_net":session_realized.map(|value| value.to_string()),
        "fees_paid_session":session_fees.to_string(),
    });
    Ok((input, references))
}
