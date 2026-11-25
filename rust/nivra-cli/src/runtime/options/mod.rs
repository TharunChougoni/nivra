//! Bounded buy-only Nifty weekly option runtime support.
//!
//! Phase 2 keeps the option universe, shortlist and expiry handling separate
//! from the cash scanner: the cash feature store is never reused blindly for
//! premiums and cash liquidity/circuit screens never apply to option prices.
use super::*;
use nivra_core::execution::{Portfolio, Scanner};
use nivra_core::market::{OptionCatalog, Segment};

pub(crate) mod expiry;
mod universe;
pub(crate) use universe::{OptionUniverse, SpotQuote, prepare};

/// NSE_FO keys are the only derivative identities this runtime may read.
pub(crate) fn is_option_key(key: &str) -> bool {
    Segment::from_key(key) == Some(Segment::NseFo)
}

/// Fresh spot from the non-tradeable index feed point for the configured
/// underlying key. The index never enters the cash scanner samples, so the
/// spot is read from the index context, not the cash quote map. Missing or
/// stale spot fails closed upstream with an explicit reason; it is never
/// inferred from a contract's symbol.
pub(crate) fn spot_quote(
    index: &super::wide::IndexContext,
    c: &Config,
    now: i64,
) -> Option<SpotQuote> {
    if !c.nifty_options.enabled {
        return None;
    }
    let (price, source_at, received_at) = index.point(&c.nifty_options.underlying_key)?;
    // The original receipt is the financial freshness clock; the exchange
    // source stamp must not be later than that receipt.
    if received_at <= 0
        || source_at > received_at
        || !now
            .checked_sub(received_at)
            .is_some_and(|age| (0..=nivra_core::market::MAX_FINANCIAL_AGE_MS).contains(&age))
    {
        return None;
    }
    Some(SpotQuote {
        key: c.nifty_options.underlying_key.clone(),
        price,
        source_at,
        received_at,
    })
}

/// Bounded option shortlist plus spot for the scanner/orchestrator. Roughly
/// 80 rows, never a full chain, and no cash liquidity/circuit screens.
pub(crate) fn projection(scan: &Scanner, catalog: &OptionCatalog, now: i64) -> serde_json::Value {
    let mut rows = Vec::with_capacity(catalog.contracts.len());
    for contract in catalog.contracts.values() {
        let sample = scan.samples.get(&contract.key);
        let screening = sample.and_then(|sample| sample.current);
        let screening_receipt = sample
            .map(|sample| sample.seen)
            .filter(|receipt| *receipt > 0);
        let executable = sample.and_then(|sample| sample.executable.as_ref());
        rows.push(serde_json::json!({
            "instrument_key":contract.key,
            "symbol":contract.symbol,
            "option":match contract.option {
                nivra_core::market::InstrumentKind::Call => "call",
                nivra_core::market::InstrumentKind::Put => "put",
                _ => "unknown",
            },
            "strike":contract.strike,
            "expiry":contract.expiry,
            "lot_size":contract.lot_size,
            "tick":contract.tick,
            "freeze_quantity":contract.freeze_quantity,
            "screening_price":screening,
            "screening_age_ms":screening_receipt.and_then(|receipt| now.checked_sub(receipt)),
            "bid":executable.map(|quote| quote.bid),
            "ask":executable.map(|quote| quote.ask),
            "bid_units":executable.map(|quote| quote.bid_units),
            "ask_units":executable.map(|quote| quote.ask_units),
            "quote_receipt_ms":executable.map(|quote| quote.receipt),
            "quote_age_ms":executable.and_then(|quote| now.checked_sub(quote.receipt)),
            "financial_quote":executable.is_some(),
        }));
    }
    serde_json::json!({
        "enabled":true,
        "status":"ready",
        "underlying_key":catalog.underlying_key,
        "underlying_symbol":catalog.underlying_symbol,
        "spot":catalog.spot,
        "session":catalog.session,
        "expiry":catalog.expiry,
        "contract_count":rows.len(),
        "cash_screens_applied":false,
        "margin_ledger":false,
        "rows":rows,
    })
}

fn load_scan(c: &Config) -> Result<Scanner> {
    if !c.directory.join("paper.sqlite").exists() {
        return Ok(Scanner::default());
    }
    let db = Connection::open_with_flags(
        c.directory.join("paper.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    use rusqlite::OptionalExtension;
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='scanner')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        return Ok(Scanner::default());
    }
    let Some(bytes) = db
        .query_row("SELECT body FROM scanner WHERE id=1", [], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    else {
        return Ok(Scanner::default());
    };
    if bytes.len() > 16_000_000 {
        return Err("option_scanner_snapshot_invalid".into());
    }
    Ok(serde_json::from_str(&bytes)?)
}

/// Additive digest evidence for the orchestrator: the bounded option
/// shortlist and spot, or an explicit unavailable reason. Never the cash
/// feature store.
pub(crate) fn snapshot(c: &Config, p: &Portfolio, now: i64) -> Result<Option<serde_json::Value>> {
    if !c.nifty_options.enabled {
        return Ok(None);
    }
    let Some(catalog) = p.options.as_ref() else {
        return Ok(Some(serde_json::json!({
            "enabled":true,
            "status":"unavailable",
            "reason":"option_catalog_unavailable",
            "cash_screens_applied":false,
        })));
    };
    let scan = load_scan(c)?;
    Ok(Some(projection(&scan, catalog, now)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nivra_core::finance::Price;
    use nivra_core::market::{OptionCatalog, OptionContract};
    use nivra_core::time::ExchangeDate;

    fn config(dir: &std::path::Path) -> Config {
        let mut c = super::super::tests::config(dir);
        c.nifty_options.enabled = true;
        c
    }

    fn catalog() -> OptionCatalog {
        let contract = |strike: u64, option, key: &str| OptionContract {
            key: key.into(),
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
            Price::parse("26000").unwrap(),
            [
                contract(
                    26_000,
                    nivra_core::market::InstrumentKind::Call,
                    "NSE_FO|26000CE",
                ),
                contract(
                    26_000,
                    nivra_core::market::InstrumentKind::Put,
                    "NSE_FO|26000PE",
                ),
            ],
        )
        .unwrap()
    }

    #[test]
    fn spot_and_shortlist_are_bounded_fresh_and_not_cash_screened() {
        let dir = tempfile::tempdir().unwrap();
        let c = config(dir.path());
        let now = 1_792_122_300_000_i64;
        let mut scan = Scanner::default();
        let store = nivra_core::feature_store::FeatureStore::new(Default::default()).unwrap();
        let index = |source: i64, receipt: i64| {
            let point = super::super::market_feed::Point {
                price: Price::parse("26000").unwrap(),
                source,
                receipt,
                last_trade: source,
                prior_close: None,
            };
            let points = [(c.nifty_options.underlying_key.clone(), point)].into();
            super::super::wide::IndexContext::capture(
                &c,
                &points,
                &store,
                &Default::default(),
                receipt,
                Some(receipt),
            )
        };
        // No index point: fail closed with no invented price, even when the
        // cash scanner holds a sample under the same key.
        let missing = super::super::wide::IndexContext::default();
        scan.observe_price(
            now,
            "NSE_INDEX|Nifty 50",
            Price::parse("25000").unwrap(),
            now,
            now,
            2000,
            None,
        )
        .unwrap();
        assert!(spot_quote(&missing, &c, now).is_none());
        // A fresh index feed point supplies the spot with its original times.
        let spot = spot_quote(&index(now - 5, now), &c, now).unwrap();
        assert_eq!(spot.price, Price::parse("26000").unwrap());
        assert_eq!(spot.key, "NSE_INDEX|Nifty 50");
        assert_eq!((spot.source_at, spot.received_at), (now - 5, now));
        assert!(spot_quote(&index(now, now), &c, now + 2_000).is_some());
        // A stale or future receipt, or a source later than receipt, never
        // becomes spot.
        assert!(spot_quote(&index(now, now), &c, now + 2_001).is_none());
        assert!(spot_quote(&index(now, now), &c, now - 1).is_none());
        assert!(spot_quote(&index(now + 1, now), &c, now).is_none());
        // Bounded rows: only catalog contracts, no cash screens, spot included.
        let catalog = catalog();
        scan.observe_price(
            now,
            "NSE_FO|26000CE",
            Price::parse("50").unwrap(),
            now,
            now,
            2000,
            None,
        )
        .unwrap();
        let projection = projection(&scan, &catalog, now);
        assert_eq!(projection["status"], "ready");
        assert_eq!(projection["contract_count"], 2);
        assert_eq!(projection["cash_screens_applied"], false);
        assert_eq!(projection["margin_ledger"], false);
        assert_eq!(projection["spot"], "26000");
        assert_eq!(projection["rows"][0]["screening_price"], "50");
        assert_eq!(
            projection["rows"][1]["screening_price"],
            serde_json::Value::Null
        );
        // The digest evidence is additive and never serialized into the book.
        let mut p = Portfolio::new();
        p.costs = Some(
            serde_json::from_str(include_str!(
                "../../../../../fixtures/test-legacy-paper-costs.json"
            ))
            .unwrap(),
        );
        p.options = Some(catalog);
        let bytes = serde_json::to_vec(&p).unwrap();
        let evidence = snapshot(&c, &p, now).unwrap().unwrap();
        assert_eq!(evidence["contract_count"], 2);
        assert_eq!(serde_json::to_vec(&p).unwrap(), bytes);
        let mut disabled = c.clone();
        disabled.nifty_options.enabled = false;
        assert!(snapshot(&disabled, &p, now).unwrap().is_none());
        p.options = None;
        assert_eq!(
            snapshot(&c, &p, now).unwrap().unwrap()["reason"],
            "option_catalog_unavailable"
        );
    }
}
