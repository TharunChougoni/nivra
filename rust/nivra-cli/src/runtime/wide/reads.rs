//! Scanner and executable quote reads for the wide controller.
use super::*;

#[allow(clippy::too_many_arguments)] // Preserve original receipt separately from the processing clock.
pub(super) fn scan_rest_rows(
    scan: &mut Scanner,
    instruments: &[Instrument],
    rows: &serde_json::Map<String, serde_json::Value>,
    receipt: i64,
    now: i64,
    stale_ms: i64,
    metadata_only: bool,
    mut feature_store: Option<&mut FeatureStore>,
) -> Result<()> {
    for instrument in instruments {
        let matched = rows
            .values()
            .filter(|r| r["instrument_token"] == instrument.key)
            .collect::<Vec<_>>();
        if matched.len() > 1 {
            return Err("duplicate_quote".into());
        }
        let Some(row) = matched.first() else {
            if metadata_only {
                scan.samples
                    .entry(instrument.key.clone())
                    .or_default()
                    .diagnostic = Some("no_quote_returned".into());
            } else {
                scan.missing(&instrument.key, "no_quote_returned");
            }
            continue;
        };
        let fields = (|| -> Result<nivra_core::execution::MarketFields> {
            Ok(nivra_core::execution::MarketFields {
                session_vwap: nivra_core::execution::session_vwap(row),
                best_bid: nivra_core::execution::book_level(row, "buy").0,
                best_bid_qty: nivra_core::execution::book_level(row, "buy").1,
                best_ask: nivra_core::execution::book_level(row, "sell").0,
                best_ask_qty: nivra_core::execution::book_level(row, "sell").1,
                total_buy_quantity: row["total_buy_quantity"].as_u64(),
                total_sell_quantity: row["total_sell_quantity"].as_u64(),
                lower_circuit_limit: nivra_core::execution::number(&row["lower_circuit_limit"])?,
                upper_circuit_limit: nivra_core::execution::number(&row["upper_circuit_limit"])?,
                volume: row["volume"].as_u64().ok_or("volume_missing")?,
            })
        })();
        let sample = scan.samples.entry(instrument.key.clone()).or_default();
        sample.diagnostic = (row["symbol"] != instrument.symbol).then(|| "symbol_mismatch".into());
        if let Ok(fields) = &fields {
            sample.metadata_preloaded = false;
            sample.market_fields = Some(fields.clone());
            sample.market_fields_receipt = Some(receipt);
        }
        // Incomplete REST metadata must not erase retained fields or renew their receipt.
        if metadata_only {
            continue;
        }
        let price =
            nivra_core::execution::number(&row["last_price"]).and_then(|p| Ok(Price::try_from(p)?));
        match (price, data::quote_source(row)) {
            (Ok(price), Ok(source)) => {
                if let Some(store) = feature_store.as_deref_mut() {
                    observe_feature(store, &instrument.key, price, receipt);
                }
                scan.observe_price(
                    now,
                    &instrument.key,
                    price,
                    source,
                    receipt,
                    stale_ms,
                    fields.as_ref().ok(),
                )?;
            }
            _ => scan.missing(&instrument.key, "invalid_quote"),
        }
    }
    Ok(())
}
pub(super) struct FetchedQuoteRows {
    requested: i64,
    receipt: i64,
    raw: String,
    /// The validated `data` object of the provider body.
    rows: serde_json::Map<String, serde_json::Value>,
}

pub(super) fn stage_quote(
    writer: Option<&super::super::market_archive::capture::Writer>,
    purpose: &str,
    raw: &str,
) -> Result<String> {
    writer.ok_or("archive_config_missing")?.stage(raw)?;
    Ok(nivra_store::market_archive::quote_reference(purpose, raw))
}

pub(super) enum QuoteReadTarget<'a> {
    Held { depth_sink: Option<&'a mut Scanner> },
    Plan,
    Depth { scan: &'a mut Scanner },
    Scan(&'a mut Scanner),
}

pub(super) enum QuoteReadError {
    Transport(String),
    Invalid { receipt: i64, reason: &'static str },
}

pub(super) fn fetch_quote_rows(
    client: &data::Client,
    c: &Config,
    instruments: &[Instrument],
) -> std::result::Result<FetchedQuoteRows, QuoteReadError> {
    let keys = instruments
        .iter()
        .map(|i| i.key.as_str())
        .collect::<Vec<_>>();
    let (requested, receipt, raw) = client
        .quotes(c, &keys)
        .map_err(|error| QuoteReadError::Transport(failure_code(error.as_ref())))?;
    let invalid = |reason| QuoteReadError::Invalid { receipt, reason };
    let mut body: serde_json::Value =
        serde_json::from_str(&raw).map_err(|_| invalid("invalid_quote_json"))?;
    if body["status"] != "success" {
        return Err(invalid("invalid_quote_status"));
    }
    let serde_json::Value::Object(rows) = body["data"].take() else {
        return Err(invalid("invalid_quote_data"));
    };
    Ok(FetchedQuoteRows {
        requested,
        receipt,
        raw,
        rows,
    })
}

pub(super) fn record_read_failure(
    error: QuoteReadError,
    db: &mut Connection,
    c: &Config,
    p: &mut Portfolio,
    supervised: bool,
) -> Result<String> {
    match error {
        QuoteReadError::Transport(reason) => {
            if supervised {
                block_read_failure(db, c, p, &reason);
            }
            p.health = "read_unavailable_no_financial_action".into();
            persist(db, c, p, millis(), None, None, "read_unavailable")?;
            Ok(reason)
        }
        QuoteReadError::Invalid { receipt, reason } => {
            if supervised {
                block_data_failure(db, c, p);
            }
            persist(db, c, p, millis(), Some(receipt), None, reason)?;
            Ok(reason.into())
        }
    }
}

pub(super) fn update_regime_references(
    scan: &mut Scanner,
    rows: &serde_json::Map<String, serde_json::Value>,
    receipt: i64,
    event_config: Option<&planner::PlannerConfig>,
) {
    if !event_config.is_some_and(|cfg| {
        cfg.runs_strategies()
            || cfg
                .discovery
                .as_ref()
                .and_then(|d| d.strategy_selection.as_ref())
                .is_some_and(|s| s.enabled)
    }) {
        return;
    }
    for row in rows.values() {
        if let Some(sample) = row["instrument_token"]
            .as_str()
            .and_then(|key| scan.samples.get_mut(key))
        {
            sample.regime_references = (|| {
                let close =
                    Price::try_from(nivra_core::execution::number(&row["ohlc"]["close"]).ok()?)
                        .ok()?;
                let open =
                    Price::try_from(nivra_core::execution::number(&row["ohlc"]["open"]).ok()?)
                        .ok()?;
                Some((receipt, close, open))
            })();
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply_scanner_metadata(
    fetched: FetchedQuoteRows,
    c: &Config,
    db: &mut Connection,
    p: &mut Portfolio,
    instruments: &[Instrument],
    scan: &mut Scanner,
    event_config: Option<&planner::PlannerConfig>,
    writer: Option<&super::super::market_archive::capture::Writer>,
) -> Result<Option<String>> {
    let FetchedQuoteRows {
        requested,
        receipt,
        raw,
        rows,
    } = fetched;
    if scan_rest_rows(
        scan,
        instruments,
        &rows,
        receipt,
        millis(),
        2000,
        true,
        None,
    )
    .is_err()
    {
        persist(
            db,
            c,
            p,
            millis(),
            Some(receipt),
            None,
            "duplicate_metadata_quote",
        )?;
        return Ok(Some("duplicate_quote".into()));
    }
    update_regime_references(scan, &rows, receipt, event_config);
    let archive=serde_json::json!({"source":"upstox-readonly-v1","purpose":"scanner_metadata","requested_ms":requested,"received_ms":receipt,"body":raw}).to_string();
    let body = stage_quote(writer, "scanner_metadata", &archive)?;
    persist_scanner_metadata(db, c, p, millis(), receipt, &body)?;
    Ok(None)
}

pub(super) fn persist_scanner_metadata(
    db: &mut Connection,
    c: &Config,
    p: &Portfolio,
    at: i64,
    receipt: i64,
    body: &str,
) -> Result<()> {
    persist(db, c, p, at, Some(receipt), Some(body), "scanner_metadata")?;
    Ok(())
}

/// A depth read attaches the executable quote, and the open interest the
/// same row reports, to the sample it read (#38).
pub(super) fn attach_depth(
    scan: &mut Scanner,
    quotes: &BTreeMap<String, Quote>,
    rows: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, quote) in quotes {
        let Some(sample) = scan.samples.get_mut(key) else {
            continue;
        };
        let stamp = (quote.receipt, quote.source);
        match sample.executable.as_ref().map(|q| (q.receipt, q.source)) {
            Some(current) if current > stamp => continue,
            // Preserve any depth already used on this receipt.
            Some(current) if current == stamp => {}
            _ => sample.executable = Some(quote.clone()),
        }
        sample.open_interest = rows
            .values()
            .find(|row| row["instrument_token"] == key.as_str())
            .and_then(|row| row["oi"].as_f64())
            .filter(|oi| oi.is_finite() && *oi >= 0.0)
            .map(|oi| oi as u64);
    }
}

#[allow(clippy::too_many_arguments)] // Explicit original receipt tracking; never resample it.
pub(super) fn apply_executable_quotes(
    fetched: FetchedQuoteRows,
    c: &Config,
    db: &mut Connection,
    p: &mut Portfolio,
    instruments: &[Instrument],
    last_fresh_receipt: &mut Option<i64>,
    event_config: Option<&planner::PlannerConfig>,
    feature_store: &Arc<Mutex<FeatureStore>>,
    mut target: QuoteReadTarget<'_>,
    writer: Option<&super::super::market_archive::capture::Writer>,
) -> Result<Option<String>> {
    let FetchedQuoteRows {
        requested,
        receipt,
        raw,
        rows,
    } = fetched;
    let mut quotes = BTreeMap::new();
    let mut duplicate = BTreeSet::new();
    for row in rows.values() {
        let Some(i) = instruments
            .iter()
            .find(|i| row["instrument_token"] == i.key)
        else {
            continue;
        };
        if !duplicate.insert(i.key.clone()) {
            if event_config.is_some() {
                block_data_failure(db, c, p);
            }
            persist(db, c, p, millis(), Some(receipt), None, "duplicate_quote")?;
            return Ok(Some("duplicate_quote".into()));
        }
        if let Ok(q) = data::parse_row(row, receipt, &i.key, &i.symbol, i.tick) {
            quotes.insert(i.key.clone(), q);
        }
    }
    // A depth read attaches the executable quote to the sample it read (#38).
    match &mut target {
        QuoteReadTarget::Held {
            depth_sink: Some(scan),
        }
        | QuoteReadTarget::Depth { scan, .. } => attach_depth(scan, &quotes, &rows),
        QuoteReadTarget::Scan(scan) => {
            let mut store = feature_store
                .lock()
                .map_err(|_| Fail::new("feature_store_lock"))?;
            scan_rest_rows(
                scan,
                instruments,
                &rows,
                receipt,
                millis(),
                event_config
                    .and_then(|c| c.discovery.as_ref())
                    .map(|d| d.max_scan_age_ms)
                    .unwrap_or(2000),
                false,
                Some(&mut store),
            )?;
            update_regime_references(scan, &rows, receipt, event_config);
        }
        QuoteReadTarget::Held { depth_sink: None } | QuoteReadTarget::Plan => {}
    }
    let purpose = if matches!(target, QuoteReadTarget::Scan(_)) {
        "scanner"
    } else {
        "execution_or_protection"
    };
    let held_positions = matches!(target, QuoteReadTarget::Held { .. });
    let archive=serde_json::json!({"source":"upstox-readonly-v1","purpose":purpose,"requested_ms":requested,"received_ms":receipt,"body":raw}).to_string();
    let policy = event_config
        .and_then(|cfg| cfg.discovery.as_ref())
        .and_then(|d| d.screens.clone())
        .unwrap_or_else(nivra_core::execution::ScreenPolicy::recommended);
    let allowed_buys = instruments
        .iter()
        .filter(|instrument| {
            // Cash liquidity/circuit screens never apply to option premiums.
            // An option buy is screened only by a fresh executable receipt.
            if nivra_core::market::Segment::from_key(&instrument.key)
                == Some(nivra_core::market::Segment::NseFo)
            {
                return quotes.get(&instrument.key).is_some_and(|quote| {
                    quote.fresh(millis(), nivra_core::market::MAX_FINANCIAL_AGE_MS)
                        && quote.ask >= quote.bid
                });
            }
            quotes
                .get(&instrument.key)
                .zip(
                    rows.values()
                        .find(|row| row["instrument_token"] == instrument.key),
                )
                .is_some_and(|(quote, row)| {
                    Scanner::market_allows_with_policy(millis(), instrument, quote, row, &policy)
                })
        })
        .map(|i| i.key.clone())
        .collect::<BTreeSet<_>>();
    let body = stage_quote(writer, purpose, &archive)?;
    commit_quotes_screened(
        db,
        c,
        p,
        &quotes,
        receipt,
        &body,
        millis,
        Some(&allowed_buys),
        held_positions,
    )?;
    if !event_config.is_some_and(planner::PlannerConfig::uses_pi_paper)
        && let Ok(binding) = book_binding(c)
    {
        for review in &p.exit_reviews {
            let _ = redecision::enqueue_exit_review(
                db,
                &binding,
                &review.instrument_key,
                &review.reason,
                review.at_ms,
            );
        }
    }
    if !p.halted && event_config.is_some() && redecision::check_existing_authority(db, c).is_err() {
        planner_incident(db, c, p, millis())?;
    }
    if let Some(receipt) = quotes
        .values()
        .filter(|q| q.fresh(millis(), nivra_core::market::MAX_FINANCIAL_AGE_MS))
        .map(|q| q.receipt)
        .max()
    {
        *last_fresh_receipt = Some(receipt);
    }
    Ok(None)
}
