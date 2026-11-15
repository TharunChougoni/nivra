//! Option expiry is not cash session rollover.
//!
//! A cash book rolls over dated session directories. An F&O contract identity
//! dies at `expiry`: on expiry day the runtime flattens the position through
//! the same executable fill path (sell at bid, F&O fee, tick rounding) and a
//! position that cannot be flattened before the cutoff halts the book instead
//! of carrying a dead key into the next session as if it were equity.
use super::*;
use sadr_core::execution::{ExitIntent, Portfolio, Quote};
use sadr_core::time::ExchangeDate;
use std::collections::BTreeMap;

/// Explicit expiry-day outcome for the runtime status/report.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ExpiryOutcome {
    pub flattened: Vec<String>,
    pub unsettled: Vec<String>,
    pub halted: bool,
}

/// Flatten expiring long options at the bid, or fail closed. Called from the
/// held-position quote commit so the quote map is the same executable read the
/// protection path uses. Never invents a settlement price.
pub(crate) fn flatten_expiring(
    p: &mut Portfolio,
    quotes: &BTreeMap<String, Quote>,
    now: i64,
    cutoff_minute: u32,
) -> Result<ExpiryOutcome> {
    let mut outcome = ExpiryOutcome::default();
    let mut already_expired = false;
    if p.stopped || p.closed {
        return Ok(outcome);
    }
    let Some(catalog) = p.options.clone() else {
        return Ok(outcome);
    };
    let keys = p
        .positions
        .keys()
        .filter(|key| is_option_key(key))
        .cloned()
        .collect::<Vec<_>>();
    for key in keys {
        let Some(position) = p.positions.get(&key).cloned() else {
            continue;
        };
        let Some(contract) = catalog.contract(&key) else {
            outcome.unsettled.push(key);
            continue;
        };
        if contract.expiry > catalog.session {
            continue;
        }
        if contract.expiry < catalog.session {
            // Already past expiry: never pretend this is a live equity row.
            already_expired = true;
            outcome.unsettled.push(key);
            continue;
        }
        let Some(quote) = quotes.get(&key) else {
            outcome.unsettled.push(key);
            continue;
        };
        let filled = p.execute_plan(
            now,
            &position.instrument,
            quote,
            false,
            position.units,
            quote.bid,
            ExitIntent::default(),
            "option_expiry_flatten",
        )?;
        if filled {
            outcome.flattened.push(key);
        } else {
            outcome.unsettled.push(key);
        }
    }
    if already_expired || (!outcome.unsettled.is_empty() && minute(now) >= cutoff_minute) {
        p.halted = true;
        p.health = "option_expiry_unsettled".into();
        outcome.halted = true;
    }
    Ok(outcome)
}

/// Refuse to carry an expired option into a later session. The caller halts
/// the book with this explicit reason; it never resumes the row as equity.
pub(crate) fn resume_guard(p: &Portfolio, today: ExchangeDate) -> Result<()> {
    let option_keys = p
        .positions
        .keys()
        .filter(|key| is_option_key(key))
        .collect::<Vec<_>>();
    if option_keys.is_empty() {
        return Ok(());
    }
    let catalog = p.options.as_ref().ok_or("option_catalog_unavailable")?;
    for key in option_keys {
        let contract = catalog.contract(key).ok_or("option_contract_unknown")?;
        if contract.expiry < today {
            return Err("option_expiry_unsettled".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sadr_core::execution::{Instrument, Portfolio, Quote, a};
    use sadr_core::finance::{FeeArithmetic, FeeSchedule, Price};
    use sadr_core::market::{InstrumentKind, OptionCatalog, OptionContract, Segment};
    use sadr_core::time::ExchangeDate;

    fn instrument() -> Instrument {
        Instrument {
            key: "NSE_FO|26000CE".into(),
            symbol: "NIFTY 26000 CE 20 OCT 26".into(),
            series: "CE".into(),
            tick: Price::parse("0.05").unwrap(),
        }
    }

    fn book(session: &str) -> Portfolio {
        let contract = OptionContract {
            key: "NSE_FO|26000CE".into(),
            symbol: "NIFTY 26000 CE 20 OCT 26".into(),
            underlying_key: "NSE_INDEX|Nifty 50".into(),
            underlying_symbol: "NIFTY".into(),
            expiry: ExchangeDate::parse("2026-10-20").unwrap(),
            strike: Price::parse("26000").unwrap(),
            option: InstrumentKind::Call,
            lot_size: 65,
            tick: Price::parse("0.05").unwrap(),
            freeze_quantity: Some(1800),
            weekly: true,
        };
        let cash: FeeArithmetic = serde_json::from_str(include_str!(
            "../../../../../fixtures/test-legacy-paper-costs.json"
        ))
        .unwrap();
        let fo: FeeArithmetic = serde_json::from_value(serde_json::json!({
            "per_order":"0.01","buy_bps":"0","sell_bps":"0",
            "slippage_bps":"10","rounding_increment":"0.05"
        }))
        .unwrap();
        let mut p = Portfolio::with_capital(a("5000"));
        p.costs = Some(cash.clone());
        p.fee_schedule = Some(FeeSchedule::new(cash).with_segment(Segment::NseFo, fo));
        p.options = Some(
            OptionCatalog::try_new(
                ExchangeDate::parse("2026-10-16").unwrap(),
                "NSE_INDEX|Nifty 50",
                "NIFTY",
                contract.expiry,
                Price::parse("26000").unwrap(),
                [contract],
            )
            .unwrap(),
        );
        // Open the long premium through the shared fill path on a normal
        // session so the book is reconciled; the caller then advances the
        // catalog session to the expiry day under test.
        let at = 1_792_122_300_000_i64;
        let opened = p
            .execute_plan(
                at,
                &instrument(),
                &Quote {
                    bid: Price::parse("50").unwrap(),
                    ask: Price::parse("50").unwrap(),
                    bid_units: 130,
                    ask_units: 130,
                    source: at,
                    receipt: at,
                },
                true,
                65,
                Price::parse("50.05").unwrap(),
                ExitIntent::default(),
                "option-open",
            )
            .unwrap();
        assert!(opened);
        if let Some(catalog) = p.options.as_mut() {
            catalog.session = ExchangeDate::parse(session).unwrap();
        }
        p
    }

    fn quote(at: i64) -> Quote {
        Quote {
            bid: Price::parse("50").unwrap(),
            ask: Price::parse("50.05").unwrap(),
            bid_units: 130,
            ask_units: 130,
            source: at,
            receipt: at,
        }
    }

    #[test]
    fn expiry_flattens_at_bid_or_halts_instead_of_carrying_the_key() {
        let at = 1_792_122_300_000_i64;
        // Before expiry: no flatten, no halt, position carried normally.
        let mut p = book("2026-10-16");
        let outcome = flatten_expiring(
            &mut p,
            &BTreeMap::from([(instrument().key, quote(at))]),
            at,
            900,
        )
        .unwrap();
        assert!(outcome.flattened.is_empty() && outcome.unsettled.is_empty());
        assert_eq!(p.positions.len(), 1);
        assert!(!p.halted);
        // Expiry day before the cutoff with no executable quote: wait.
        let mut p = book("2026-10-20");
        let outcome = flatten_expiring(&mut p, &BTreeMap::new(), at, 900).unwrap();
        assert_eq!(outcome.unsettled, vec![instrument().key]);
        assert!(!p.halted);
        // Expiry day with a fresh bid: flatten through the shared fill path.
        let mut p = book("2026-10-20");
        let outcome = flatten_expiring(
            &mut p,
            &BTreeMap::from([(instrument().key, quote(at))]),
            at,
            900,
        )
        .unwrap();
        assert_eq!(outcome.flattened, vec![instrument().key]);
        assert!(p.positions.is_empty());
        assert!(!p.halted);
        let fill = p.fills.last().unwrap();
        assert!(!fill.buy);
        assert_eq!(fill.price, Price::parse("49.95").unwrap());
        assert_eq!(fill.expected_price, Some(Price::parse("50").unwrap()));
        assert_eq!(fill.fee, a("0.05"));
        assert_eq!(fill.decision_id.as_deref(), Some("option_expiry_flatten"));
        p.reconcile().unwrap();
        // Past expiry with the key still held: fail closed, never carry it.
        let mut p = book("2026-10-21");
        let outcome = flatten_expiring(
            &mut p,
            &BTreeMap::from([(instrument().key, quote(at))]),
            at,
            900,
        )
        .unwrap();
        assert_eq!(outcome.unsettled, vec![instrument().key]);
        assert!(p.halted);
        assert_eq!(p.health, "option_expiry_unsettled");
        // A stale quote after the cutoff also halts instead of carrying.
        let mut p = book("2026-10-20");
        let late = at + 6 * 3_600_000;
        let stale = quote(late - 2_001);
        let outcome = flatten_expiring(
            &mut p,
            &BTreeMap::from([(instrument().key, stale)]),
            late,
            900,
        )
        .unwrap();
        assert!(outcome.halted);
        assert_eq!(p.positions.len(), 1);
        // Resume guard refuses an expired option and accepts a live one.
        let mut p = book("2026-10-20");
        assert!(resume_guard(&p, ExchangeDate::parse("2026-10-21").unwrap()).is_err());
        assert!(resume_guard(&p, ExchangeDate::parse("2026-10-20").unwrap()).is_ok());
        p.options = None;
        assert_eq!(
            resume_guard(&p, ExchangeDate::parse("2026-10-20").unwrap())
                .unwrap_err()
                .to_string(),
            "option_catalog_unavailable"
        );
    }
}
