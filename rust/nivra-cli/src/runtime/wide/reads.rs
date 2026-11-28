//! Scanner and executable quote reads for the wide controller.
use super::*;
use nivra_core::{
    execution::MarketFields,
    intraday::{Clock, Features, Receipt},
};

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
        let fields = MarketFields::from_row(row);
        let sample = scan.samples.entry(instrument.key.clone()).or_default();
        sample.diagnostic = (row["symbol"] != instrument.symbol).then(|| "symbol_mismatch".into());
        if let Ok(fields) = &fields {
            if sample
                .market_fields_receipt
                .is_none_or(|previous| receipt >= previous)
            {
                sample.metadata_preloaded = false;
                sample.market_fields = Some(fields.clone());
                sample.market_fields_receipt = Some(receipt);
            }
            if let Ok(source) = data::quote_source(row) {
                let touch = data::parse_row(
                    row,
                    receipt,
                    &instrument.key,
                    &instrument.symbol,
                    instrument.tick,
                )
                .ok();
                sample.observe_intraday(Features::reduce(Receipt::from_fields(
                    Clock { source, receipt },
                    fields,
                    touch,
                )));
            }
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
    pub(super) receipt: i64,
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
    Held {
        depth_sink: Option<&'a mut Scanner>,
    },
    Plan,
    Options {
        scan: &'a mut Scanner,
        spot: Option<super::super::options::SpotQuote>,
    },
    Depth {
        scan: &'a mut Scanner,
    },
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
    let mixed = event_config
        .and_then(|cfg| cfg.discovery.as_ref())
        .and_then(|discovery| discovery.market_feed.as_ref())
        .is_some_and(market_feed::Settings::full_depth);
    let result = if mixed {
        // A rotating REST read supplies the price for names outside the LTPC
        // pool. Names with a streaming price only need the REST metadata.
        let (streamed, overflow): (Vec<_>, Vec<_>) = instruments.iter().cloned().partition(|i| {
            scan.samples
                .get(&i.key)
                .is_some_and(|sample| sample.scan_activity.is_some())
        });
        let now = millis();
        scan_rest_rows(scan, &streamed, &rows, receipt, now, 2000, true, None)
            .and_then(|()| scan_rest_rows(scan, &overflow, &rows, receipt, now, 2000, false, None))
    } else {
        scan_rest_rows(
            scan,
            instruments,
            &rows,
            receipt,
            millis(),
            2000,
            true,
            None,
        )
    };
    if result.is_err() {
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
    for row in rows.values() {
        let Some(key) = row["instrument_token"].as_str() else {
            continue;
        };
        let (Some(sample), Some(quote)) = (scan.samples.get_mut(key), quotes.get(key)) else {
            continue;
        };
        if let Ok(fields) = MarketFields::from_row(row) {
            if sample
                .market_fields_receipt
                .is_none_or(|previous| quote.receipt >= previous)
            {
                sample.metadata_preloaded = false;
                sample.market_fields = Some(fields.clone());
                sample.market_fields_receipt = Some(quote.receipt);
            }
            sample.observe_intraday(Features::reduce(Receipt::from_fields(
                Clock {
                    source: quote.source,
                    receipt: quote.receipt,
                },
                &fields,
                Some(quote.clone()),
            )));
        } else if sample.intraday.as_ref().is_none_or(|previous| {
            quote.source >= previous.clock.source && quote.receipt > previous.clock.receipt
        }) {
            // Options or incomplete rows can still carry valid executable depth.
            sample.observe_intraday(Features::reduce(Receipt {
                clock: Clock {
                    source: quote.source,
                    receipt: quote.receipt,
                },
                vwap: None,
                volume: None,
                buy_quantity: None,
                sell_quantity: None,
                touch: Some(quote.clone()),
                levels: Default::default(),
            }));
        }
        // Open interest follows the depth read the sample now carries, matched by
        // receipt and source time so a later edit to that depth still counts.
        if sample
            .executable
            .as_ref()
            .is_some_and(|q| (q.receipt, q.source) == (quote.receipt, quote.source))
        {
            sample.open_interest = row["oi"]
                .as_f64()
                .filter(|oi| oi.is_finite() && *oi >= 0.0)
                .map(|oi| oi as u64);
        }
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
            if matches!(target, QuoteReadTarget::Options { .. })
                && let Some(writer) = writer
            {
                writer.options.offer(
                    c.session,
                    receipt,
                    crate::runtime::market_archive::options::gap("option_duplicate_quote"),
                );
            }
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
    use crate::runtime::market_archive::options as evidence;
    use nivra_store::market_archive::options::{OptionEvidence, OptionPoll};
    let option_poll = matches!(target, QuoteReadTarget::Options { .. });
    let held_depth = matches!(
        target,
        QuoteReadTarget::Held {
            depth_sink: Some(_)
        }
    );
    if let Some(writer) = writer.filter(|_| option_poll || held_depth) {
        let observations = instruments
            .iter()
            .filter(|i| crate::runtime::options::is_option_key(&i.key))
            .map(|i| {
                let row = rows.values().find(|row| row["instrument_token"] == i.key);
                evidence::observation(&i.key, row, quotes.get(&i.key))
            })
            .collect::<Vec<_>>();
        if !observations.is_empty() {
            let spot = match &target {
                QuoteReadTarget::Options { spot, .. } => spot.as_ref().map(evidence::spot),
                _ => None,
            };
            let poll = OptionPoll {
                requested_ms: requested,
                spot,
                rows: observations,
            };
            let event = if option_poll {
                OptionEvidence::Poll(poll)
            } else {
                OptionEvidence::Held(poll)
            };
            writer.options.offer(c.session, receipt, event);
        }
    }
    // A depth read attaches the executable quote to the sample it read (#38).
    match &mut target {
        QuoteReadTarget::Held {
            depth_sink: Some(scan),
        }
        | QuoteReadTarget::Options { scan, .. }
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
