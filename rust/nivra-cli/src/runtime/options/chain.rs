//! The near-the-money Nifty option chain the gate screens. Rows hold only
//! contracts with a recent two-sided depth read, and carry what a long-premium
//! entry needs: spread in ticks, depth in lots, open interest, the cash one
//! lot needs and the round-trip cost. Screening only: execution reads its own
//! quote under the 2 s bound.
use super::SpotQuote;
use crate::runtime::Result;
use chrono::NaiveDate;
use nivra_core::execution::{Portfolio, Scanner};
use nivra_core::finance::{Amount, Money, Price};
use nivra_core::market::{InstrumentKind, OptionCatalog};
use nivra_core::time::ExchangeDate;
use serde::Serialize;

/// A depth read older than this leaves the chain. The controller refreshes
/// option depth every 2 s, so this tolerates one late pass.
pub(crate) const SCREEN_MAX_AGE_MS: i64 = 5_000;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct Chain {
    pub underlying_key: String,
    pub spot: String,
    pub spot_age_ms: i64,
    pub expiry: ExchangeDate,
    pub days_to_expiry: i64,
    pub expiry_day: bool,
    pub atm_strike: String,
    pub rows: Vec<ChainRow>,
    #[serde(skip)]
    pub spot_value: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct ChainRow {
    pub instrument_key: String,
    pub option: &'static str,
    pub strike: String,
    /// Strikes away from the at-the-money strike; negative below it.
    pub offset: i64,
    pub lot_size: u64,
    pub bid: String,
    pub ask: String,
    pub spread_ticks: u64,
    /// Lots on the thinner side of the best level.
    pub depth_lots: u64,
    pub quote_age_ms: i64,
    pub open_interest: Option<u64>,
    /// Cash one lot needs at the ask, with the buy fee.
    pub lot_cash: String,
    pub affordable_lots: u64,
    /// Fees and slippage on both legs plus the spread, in bps of the mid.
    pub round_trip_cost_bps: f64,
    #[serde(skip)]
    pub mid: f64,
}

fn number(value: Amount) -> f64 {
    value.to_string().parse().unwrap_or(f64::NAN)
}

fn date(value: ExchangeDate) -> Result<NaiveDate> {
    Ok(NaiveDate::parse_from_str(&value.to_string(), "%Y-%m-%d")?)
}

/// The chain within `strikes_near` strikes of the strike closest to spot.
pub(crate) fn near_money(
    catalog: &OptionCatalog,
    scanner: &Scanner,
    portfolio: &Portfolio,
    spot: &SpotQuote,
    session: NaiveDate,
    now: i64,
    strikes_near: usize,
) -> Result<Chain> {
    let spot_value = number(spot.price.amount());
    let mut strikes: Vec<Price> = catalog.contracts.values().map(|c| c.strike).collect();
    strikes.sort();
    strikes.dedup();
    let atm = strikes
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (number(a.amount()) - spot_value)
                .abs()
                .total_cmp(&(number(b.amount()) - spot_value).abs())
        })
        .map(|(index, _)| index)
        .ok_or("option_chain_empty")?;
    let mut rows = Vec::new();
    for contract in catalog.contracts.values() {
        let index = strikes.binary_search(&contract.strike).unwrap_or(atm);
        let offset = index as i64 - atm as i64;
        if offset.unsigned_abs() as usize > strikes_near {
            continue;
        }
        let Some(sample) = scanner.samples.get(&contract.key) else {
            continue;
        };
        let Some(quote) = sample.executable.as_ref() else {
            continue;
        };
        let age = now.saturating_sub(quote.receipt);
        if !(0..=SCREEN_MAX_AGE_MS).contains(&age) || quote.bid >= quote.ask {
            continue;
        }
        let fees = portfolio.fee_policy(&contract.key)?;
        let lot = Amount::from_unsigned(contract.lot_size);
        let turnover = Money::try_from(quote.ask.amount().checked_mul(lot)?)?;
        let lot_cash = fees.cash_requirement(turnover, false, 0)?;
        let round_trip = fees.cash_requirement(turnover, true, 2)?;
        let (bid, ask) = (number(quote.bid.amount()), number(quote.ask.amount()));
        let mid = (bid + ask) / 2.0;
        let spread = quote.ask.amount().checked_sub(quote.bid.amount())?;
        let turnover_value = number(turnover.amount());
        let fee_bps = (number(round_trip.amount()) - turnover_value) / turnover_value * 1e4;
        rows.push(ChainRow {
            instrument_key: contract.key.clone(),
            option: match contract.option {
                InstrumentKind::Call => "call",
                _ => "put",
            },
            strike: contract.strike.amount().to_string(),
            offset,
            lot_size: contract.lot_size,
            bid: quote.bid.amount().to_string(),
            ask: quote.ask.amount().to_string(),
            spread_ticks: (number(spread) / number(contract.tick.amount())).round() as u64,
            depth_lots: quote.bid_units.min(quote.ask_units) / contract.lot_size.max(1),
            quote_age_ms: age,
            open_interest: sample.open_interest,
            lot_cash: lot_cash.amount().to_string(),
            affordable_lots: fees.affordable_cash_lots(
                Price::try_from(quote.ask.amount().checked_mul(lot)?)?,
                portfolio.cash,
                i64::MAX,
                false,
                0,
            )?,
            round_trip_cost_bps: ((fee_bps + (ask - bid) / mid * 1e4) * 100.0).round() / 100.0,
            mid,
        });
    }
    rows.sort_by(|a, b| (a.offset, a.option).cmp(&(b.offset, b.option)));
    let expiry = date(catalog.expiry)?;
    Ok(Chain {
        underlying_key: spot.key.clone(),
        spot: spot.price.amount().to_string(),
        spot_age_ms: now.saturating_sub(spot.received_at),
        expiry: catalog.expiry,
        days_to_expiry: (expiry - session).num_days(),
        expiry_day: expiry == session,
        atm_strike: strikes[atm].amount().to_string(),
        rows,
        spot_value,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use nivra_core::execution::Quote;
    use nivra_core::market::OptionContract;

    pub(crate) fn catalog(spot: &str) -> OptionCatalog {
        let contract = |strike: u64, option: InstrumentKind| OptionContract {
            key: format!(
                "NSE_FO|{strike}{}",
                if option == InstrumentKind::Call {
                    "CE"
                } else {
                    "PE"
                }
            ),
            symbol: format!("NIFTY {strike}"),
            underlying_key: "NSE_INDEX|Nifty 50".into(),
            underlying_symbol: "NIFTY".into(),
            expiry: ExchangeDate::parse("2026-10-20").unwrap(),
            strike: Price::parse(&strike.to_string()).unwrap(),
            option,
            lot_size: 65,
            tick: Price::parse("0.05").unwrap(),
            freeze_quantity: Some(1800),
            weekly: true,
        };
        OptionCatalog::try_new(
            ExchangeDate::parse("2026-10-16").unwrap(),
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            ExchangeDate::parse("2026-10-20").unwrap(),
            Price::parse(spot).unwrap(),
            [25_800, 25_900, 26_000, 26_100, 26_200]
                .into_iter()
                .flat_map(|strike| {
                    [
                        contract(strike, InstrumentKind::Call),
                        contract(strike, InstrumentKind::Put),
                    ]
                }),
        )
        .unwrap()
    }

    pub(crate) fn depth(scanner: &mut Scanner, key: &str, bid: &str, ask: &str, at: i64) {
        let sample = scanner.samples.entry(key.into()).or_default();
        sample.executable = Some(Quote {
            bid: Price::parse(bid).unwrap(),
            ask: Price::parse(ask).unwrap(),
            bid_units: 650,
            ask_units: 260,
            source: at,
            receipt: at,
        });
        sample.open_interest = Some(1_000_000);
    }

    pub(crate) fn spot(price: &str, at: i64) -> SpotQuote {
        SpotQuote {
            key: "NSE_INDEX|Nifty 50".into(),
            price: Price::parse(price).unwrap(),
            source_at: at,
            received_at: at,
        }
    }

    #[test]
    fn the_chain_keeps_fresh_near_money_rows_with_their_entry_costs() {
        let now = 1_792_122_300_000;
        let catalog = catalog("26040");
        let mut scanner = Scanner::default();
        depth(&mut scanner, "NSE_FO|26000CE", "100", "100.20", now - 100);
        depth(&mut scanner, "NSE_FO|26100PE", "120", "121", now - 100);
        // Two strikes out, and a stale read: both leave the chain.
        depth(&mut scanner, "NSE_FO|26200CE", "40", "40.05", now - 100);
        depth(&mut scanner, "NSE_FO|25900PE", "60", "60.05", now - 6_000);
        let mut portfolio = crate::runtime::tests::test_portfolio();
        portfolio.cash = Amount::parse("15000").unwrap();
        let session = NaiveDate::from_ymd_opt(2026, 10, 16).unwrap();
        let chain = near_money(
            &catalog,
            &scanner,
            &portfolio,
            &spot("26040", now - 50),
            session,
            now,
            1,
        )
        .unwrap();
        assert_eq!(chain.atm_strike, "26000");
        assert_eq!((chain.days_to_expiry, chain.expiry_day), (4, false));
        assert_eq!(chain.spot_age_ms, 50);
        let keys: Vec<_> = chain
            .rows
            .iter()
            .map(|r| r.instrument_key.as_str())
            .collect();
        assert_eq!(keys, ["NSE_FO|26000CE", "NSE_FO|26100PE"]);
        let call = &chain.rows[0];
        assert_eq!((call.offset, call.option), (0, "call"));
        assert_eq!((call.spread_ticks, call.depth_lots), (4, 4));
        assert_eq!(call.open_interest, Some(1_000_000));
        // 65 x 100.20 = 6,513 premium plus the buy fee: two lots fit ₹15,000.
        assert!(call.lot_cash.parse::<f64>().unwrap() >= 6_513.0);
        assert_eq!(call.affordable_lots, 2);
        assert!(call.round_trip_cost_bps > 20.0 / 100.1);
        assert_eq!(chain.rows[1].spread_ticks, 20);
        // On expiry day the chain says so.
        let expiry = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let today = near_money(
            &catalog,
            &scanner,
            &portfolio,
            &spot("26040", now),
            expiry,
            now,
            1,
        )
        .unwrap();
        assert_eq!((today.days_to_expiry, today.expiry_day), (0, true));
    }
}
