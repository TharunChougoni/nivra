//! Offline reconciliation of captured fills against an archive tape (#181).
//! Missing quotes and residual holdings stay unknown; no model is called.
use nivra_core::{
    execution::{Fill, Portfolio, Quote},
    finance::{Amount, FeeArithmetic, Money, Side},
    historical::{Event, EventKind},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FillTape {
    version: u32,
    capital: Amount,
    #[serde(default)]
    costs: Option<FeeArithmetic>,
    fills: Vec<Fill>,
}

fn parse_events(events: &[u8]) -> Result<Vec<Event>, String> {
    let mut tape = Vec::new();
    let mut count = 0usize;
    for line in events
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
    {
        if line.len() > 2_000_000 || tape.len() >= 10_000 {
            return Err("archive_reconcile_input_limit".into());
        }
        let event: Event =
            serde_json::from_slice(line).map_err(|_| "archive_reconcile_event_invalid")?;
        count = count
            .checked_add(event.observations.len())
            .ok_or("archive_reconcile_input_limit")?;
        if count > 100_000
            || tape
                .last()
                .is_some_and(|previous: &Event| previous.at > event.at)
            || event.available_at > event.at
        {
            return Err("archive_reconcile_chronology_invalid".into());
        }
        tape.push(event);
    }
    if tape.is_empty() {
        return Err("archive_reconcile_empty_tape".into());
    }
    Ok(tape)
}

fn quote_as_of<'a>(tape: &'a [Event], key: &str, at: i64) -> Option<&'a Quote> {
    let mut found = None;
    for event in tape {
        if event.at > at || matches!(event.kind, EventKind::Screen) {
            continue;
        }
        for observation in &event.observations {
            if observation.key == key
                && observation.quote.receipt <= at
                && observation.quote.source <= observation.quote.receipt
            {
                found = Some(&observation.quote);
            }
        }
    }
    found
}

/// Recompute fees, slippage and cash from recorded fills bound to archive bytes.
/// Residual holdings leave marked equity and net P&L unknown.
pub fn reconcile(manifest: &[u8], events: &[u8], fills: &[u8]) -> Result<Value, String> {
    if manifest.len() > 512 * 1024
        || events.len() > 16 * 1024 * 1024
        || fills.len() > 2 * 1024 * 1024
    {
        return Err("archive_reconcile_input_limit".into());
    }
    let (_first, _last, universe) = crate::strategy_archive::archive_identity(manifest, events)?;
    let instruments: BTreeMap<_, _> = universe.into_iter().map(|i| (i.key.clone(), i)).collect();
    let manifest_value: Value =
        serde_json::from_slice(manifest).map_err(|_| "archive_reconcile_manifest_invalid")?;
    let manifest_costs = match manifest_value.get("paper_costs") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            serde_json::from_value::<FeeArithmetic>(value.clone())
                .map_err(|_| "archive_reconcile_costs_invalid")?,
        ),
    };
    let tape: FillTape =
        serde_json::from_slice(fills).map_err(|_| "archive_reconcile_fills_invalid")?;
    if tape.version != 1
        || tape.capital <= Amount::ZERO
        || tape.fills.is_empty()
        || tape.fills.len() > 10_000
    {
        return Err("archive_reconcile_fills_invalid".into());
    }
    if let (Some(left), Some(right)) = (&tape.costs, &manifest_costs)
        && left != right
    {
        return Err("archive_reconcile_costs_mismatch".into());
    }
    let costs = tape
        .costs
        .clone()
        .or(manifest_costs)
        .ok_or("archive_reconcile_costs_required")?;
    costs
        .validate()
        .map_err(|_| "archive_reconcile_costs_invalid")?;
    let event_tape = parse_events(events)?;
    let max_gap_ms = manifest_value["max_observation_gap_ms"].as_i64();
    let mut largest_gap_ms = 0i64;
    let mut missing_intervals = Vec::new();
    for window in event_tape.windows(2) {
        let gap = window[1].at.saturating_sub(window[0].at);
        largest_gap_ms = largest_gap_ms.max(gap);
        if max_gap_ms.is_some_and(|limit| gap > limit) {
            missing_intervals
                .push(json!({"from_ms":window[0].at,"to_ms":window[1].at,"gap_ms":gap}));
        }
    }
    let mut cash = tape.capital;
    let mut held: BTreeMap<String, u64> = BTreeMap::new();
    let mut fee_mismatches = 0u64;
    let mut price_mismatches = 0u64;
    let mut missing_quotes = 0u64;
    let mut rows = Vec::new();
    let mut recorded_fees = Amount::ZERO;
    let mut recomputed_fees = Amount::ZERO;
    for (index, fill) in tape.fills.iter().enumerate() {
        if fill.units == 0 || !instruments.contains_key(&fill.instrument.key) {
            return Err("archive_reconcile_fill_invalid".into());
        }
        let policy = fill.costs.as_ref().unwrap_or(&costs);
        policy
            .validate()
            .map_err(|_| "archive_reconcile_costs_invalid")?;
        let notional = fill
            .price
            .amount()
            .checked_mul(Amount::from_unsigned(fill.units))
            .map_err(|_| "archive_reconcile_arithmetic")?;
        let expected_fee = policy
            .charge(
                if fill.buy { Side::Buy } else { Side::Sell },
                Money::try_from(notional).map_err(|_| "archive_reconcile_arithmetic")?,
            )
            .map_err(|_| "archive_reconcile_arithmetic")?
            .amount();
        recorded_fees = recorded_fees
            .checked_add(fill.fee)
            .map_err(|_| "archive_reconcile_arithmetic")?;
        recomputed_fees = recomputed_fees
            .checked_add(expected_fee)
            .map_err(|_| "archive_reconcile_arithmetic")?;
        if fill.fee != expected_fee {
            fee_mismatches += 1;
        }
        let quote = quote_as_of(&event_tape, &fill.instrument.key, fill.at);
        let (price_match, slippage) = if let Some(quote) = quote {
            let raw = if fill.buy { quote.ask } else { quote.bid };
            let recomputed =
                Portfolio::execution_with_costs(raw, fill.instrument.tick, fill.buy, policy)
                    .map_err(|_| "archive_reconcile_arithmetic")?;
            if recomputed != fill.price {
                price_mismatches += 1;
            }
            (json!(recomputed == fill.price), json!(fill.slippage_bps))
        } else {
            missing_quotes += 1;
            (Value::Null, Value::Null)
        };
        if fill.buy {
            cash = cash
                .checked_sub(
                    notional
                        .checked_add(fill.fee)
                        .map_err(|_| "archive_reconcile_arithmetic")?,
                )
                .map_err(|_| "archive_reconcile_arithmetic")?;
            *held.entry(fill.instrument.key.clone()).or_insert(0) += fill.units;
        } else {
            let remaining = held
                .get_mut(&fill.instrument.key)
                .ok_or("archive_reconcile_sell_without_position")?;
            if *remaining < fill.units {
                return Err("archive_reconcile_sell_without_position".into());
            }
            *remaining -= fill.units;
            if *remaining == 0 {
                held.remove(&fill.instrument.key);
            }
            cash = cash
                .checked_add(
                    notional
                        .checked_sub(fill.fee)
                        .map_err(|_| "archive_reconcile_arithmetic")?,
                )
                .map_err(|_| "archive_reconcile_arithmetic")?;
        }
        rows.push(json!({
            "index": index,
            "at": fill.at,
            "key": fill.instrument.key,
            "buy": fill.buy,
            "units": fill.units,
            "recorded_fee": fill.fee,
            "recomputed_fee": expected_fee,
            "fee_match": fill.fee == expected_fee,
            "price_match": price_match,
            "recorded_slippage_bps": slippage,
            "quote_present": quote.is_some()
        }));
    }
    let residual: Vec<_> = held
        .into_iter()
        .filter(|(_, units)| *units > 0)
        .map(|(key, units)| json!({"key":key,"units":units}))
        .collect();
    let flat = residual.is_empty();
    let mut portfolio = Portfolio::with_capital(tape.capital);
    portfolio.costs = Some(costs.clone());
    portfolio.fills = tape.fills.clone();
    let realised = portfolio.realised_net_pnl();
    Ok(json!({
        "schema": 1,
        "offline": true,
        "paper_only": true,
        "manifest_sha256": nivra_core::replay::digest(manifest),
        "events_sha256": nivra_core::replay::digest(events),
        "fills_sha256": nivra_core::replay::digest(fills),
        "coverage": {
            "event_count": event_tape.len(),
            "largest_gap_ms": largest_gap_ms,
            "max_observation_gap_ms": max_gap_ms,
            "missing_intervals": missing_intervals
        },
        "fills": rows,
        "recorded_fees": recorded_fees,
        "recomputed_fees": recomputed_fees,
        "fee_mismatches": fee_mismatches,
        "price_mismatches": price_mismatches,
        "missing_quotes": missing_quotes,
        "cash": cash,
        "residual_holdings": residual,
        "residual_unknown": !flat,
        "net_pnl_if_flat": if flat { json!(cash.checked_sub(tape.capital).map_err(|_| "archive_reconcile_arithmetic")?) } else { Value::Null },
        "marked_equity": if flat { json!(cash) } else { Value::Null },
        "realised_net_pnl_by_fill": realised,
        "activation_authorized": false
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn digest(bytes: &[u8]) -> String {
        nivra_core::replay::digest(bytes)
    }

    fn fixture(sell: bool, extra_gap: bool) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-16T10:00:00+05:30")
            .unwrap()
            .timestamp_millis();
        let quote = |now, bid, ask| json!({"key":"SYNTHETIC|A","quote":{"bid":bid,"ask":ask,"bid_units":10,"ask_units":10,"source":now,"receipt":now},"lower_circuit_limit":"50","upper_circuit_limit":"200","volume":10000});
        let mut rows = vec![
            json!({"at":at,"available_at":at,"kind":"entry","observations":[quote(at,"99","100")]}),
            json!({"at":at+1000,"available_at":at+1000,"kind":"protection","observations":[quote(at+1000,"110","111")]}),
        ];
        if extra_gap {
            rows.push(json!({"at":at+70_000,"available_at":at+70_000,"kind":"protection","observations":[]}));
        }
        let events = rows
            .iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>()
            .into_bytes();
        let manifest = serde_json::to_vec(&json!({
            "schema":1,
            "session":"2026-10-16",
            "previous_session":"2026-10-15",
            "capital":"5000",
            "universe":[{"key":"SYNTHETIC|A","symbol":"FIXTURE","series":"EQ","tick":"0.05"}],
            "events_sha256": digest(&events),
            "synthetic":true,
            "max_observation_gap_ms":1000,
            "history_directory":null,
            "provenance":{}
        }))
        .unwrap();
        let instrument =
            json!({"key":"SYNTHETIC|A","symbol":"FIXTURE","series":"EQ","tick":"0.05"});
        let mut fills = vec![json!({
            "at": at,
            "instrument": instrument,
            "units": 10,
            "buy": true,
            "price": "100",
            "fee": "1"
        })];
        if sell {
            fills.push(json!({
                "at": at + 1000,
                "instrument": instrument,
                "units": 10,
                "buy": false,
                "price": "110",
                "fee": "1"
            }));
        }
        let tape = serde_json::to_vec(&json!({
            "version":1,
            "capital":"5000",
            "costs":{"per_order":"1","buy_bps":"0","sell_bps":"0","slippage_bps":"0","rounding_increment":"0.01"},
            "fills": fills
        }))
        .unwrap();
        (manifest, events, tape)
    }

    #[test]
    fn archive_reconcile_keeps_residuals_unknown() {
        let (manifest, events, tape) = fixture(false, true);
        let open = reconcile(&manifest, &events, &tape).unwrap();
        assert_eq!(open["residual_unknown"], true);
        assert!(open["net_pnl_if_flat"].is_null());
        assert!(open["marked_equity"].is_null());
        assert_eq!(open["recomputed_fees"], "1");
        assert_eq!(open["fee_mismatches"], 0);
        assert_eq!(open["fills"][0]["fee_match"], true);
        assert_eq!(open["activation_authorized"], false);
        assert_eq!(
            open["coverage"]["missing_intervals"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let (manifest, events, tape) = fixture(true, false);
        let flat = reconcile(&manifest, &events, &tape).unwrap();
        assert_eq!(flat["residual_unknown"], false);
        assert_eq!(flat["net_pnl_if_flat"], "98");
        assert_eq!(flat["marked_equity"], "5098");
        assert_eq!(flat["recomputed_fees"], "2");
        assert_eq!(flat["realised_net_pnl_by_fill"][0], Value::Null);
        assert_eq!(flat["realised_net_pnl_by_fill"][1], "98");
        let mut bad = serde_json::from_slice::<Value>(&tape).unwrap();
        bad["fills"][0]["fee"] = json!("9");
        let mismatched = reconcile(&manifest, &events, &serde_json::to_vec(&bad).unwrap()).unwrap();
        assert_eq!(mismatched["fee_mismatches"], 1);
        assert_eq!(mismatched["fills"][0]["fee_match"], false);
    }
}
