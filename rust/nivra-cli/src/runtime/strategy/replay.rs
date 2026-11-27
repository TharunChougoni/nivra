//! After-cost replay of a `strategy_actions_v1` strategy over a recorded quote
//! tape. Selected depth receipts drive calls after held protection. The same
//! selection and executable freshness rules apply to paper calls. Only
//! original receipts fill; nothing is interpolated or carried forward.
use super::{
    ABI, ActionResult, MAX_PROSPECTS, Observation, Worker, WorkerConfig, WorkerKey, apply,
    parse_output,
    selection::{REFRESH_MS, Screen, Selection},
};
use crate::runtime::Result;
use crate::runtime::strategy_library::review;
use chrono::{DateTime, FixedOffset, NaiveDate, Timelike};
use nivra_core::{
    execution::{ExitIntent, ExitPolicy, Instrument, Portfolio, Quote, Scanner},
    finance::{Amount, FeeArithmetic},
    historical::EventKind,
    market::MAX_FINANCIAL_AGE_MS,
    replay::digest,
    strategy::Feature,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Replay options. `capital` and `costs` default to the tape manifest's.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Parameters {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capital: Option<Amount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub costs: Option<FeeArithmetic>,
    /// The planner config's `exit_policy`; absent executes every held stop,
    /// target and time exit, as a model-led paper book does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_policy: Option<ExitPolicy>,
    /// The strategy's own parameter object, passed as `params`.
    #[serde(default = "empty_object")]
    pub params: Value,
    #[serde(default = "default_top_n")]
    pub top_n: usize,
    /// Inclusive window for strategy calls. Held exits run on every receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_ms: Option<i64>,
}
fn empty_object() -> Value {
    json!({})
}
fn default_top_n() -> usize {
    20
}
impl Parameters {
    fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.version != 1
            || !self.params.is_object()
            || !(1..=MAX_PROSPECTS).contains(&self.top_n)
            || self.capital.is_some_and(|c| c <= Amount::ZERO)
            || self.costs.as_ref().is_some_and(|c| c.validate().is_err())
            || self
                .exit_policy
                .as_ref()
                .is_some_and(|p| p.validate().is_err())
            || self.from_ms.zip(self.to_ms).is_some_and(|(f, t)| f > t)
        {
            return Err("strategy_replay_parameters_invalid");
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct Trade {
    key: String,
    exit_at_ms: i64,
    units: u64,
    entry_price: nivra_core::finance::Price,
    exit_price: nivra_core::finance::Price,
    /// `strategy` for a strategy sell, else the held exit that fired.
    exit_reason: &'static str,
    net_pnl: Amount,
}

#[derive(Serialize)]
struct Refusal {
    at_ms: i64,
    index: usize,
    key: Option<String>,
    side: Option<&'static str>,
    reason: String,
}

#[derive(Serialize)]
struct CallFailure {
    at_ms: i64,
    reason: &'static str,
}

#[derive(Default, Serialize)]
struct Coverage {
    prospects: u64,
    fresh_quote: u64,
    calls_with_prospects: u64,
    calls_with_fresh_quote: u64,
    feature_rows: BTreeMap<Feature, u64>,
}

impl Coverage {
    fn observe(&mut self, prospects: &[super::Prospect], quotes: &BTreeMap<String, Quote>) {
        self.prospects += prospects.len() as u64;
        self.calls_with_prospects += u64::from(!prospects.is_empty());
        let mut quoted = 0;
        for prospect in prospects {
            quoted += u64::from(
                quotes
                    .get(&prospect.key)
                    .is_some_and(|quote| quote.bid_units > 0 && quote.ask_units > 0),
            );
            for feature in prospect.features.keys() {
                *self.feature_rows.entry(*feature).or_default() += 1;
            }
        }
        self.fresh_quote += quoted;
        self.calls_with_fresh_quote += u64::from(quoted > 0);
    }
}

const MAX_LISTED: usize = 2_000;

fn local(at: i64) -> std::result::Result<DateTime<FixedOffset>, &'static str> {
    Ok(DateTime::from_timestamp_millis(at)
        .ok_or("strategy_replay_clock_invalid")?
        .with_timezone(&FixedOffset::east_opt(19_800).ok_or("strategy_replay_clock_invalid")?))
}

/// Why `apply_at` sold a held position, checked in `protect`'s order.
fn exit_reason(intent: Option<&ExitIntent>, quote: &Quote, now: i64) -> &'static str {
    let intent = intent.cloned().unwrap_or_default();
    if intent.stop.is_some_and(|stop| quote.bid <= stop) {
        "stop"
    } else if intent.target.is_some_and(|target| quote.bid >= target) {
        "target"
    } else if intent.time.is_some_and(|time| now >= time) {
        "time"
    } else {
        "backstop"
    }
}

fn arithmetic<T, E>(value: std::result::Result<T, E>) -> std::result::Result<T, &'static str> {
    value.map_err(|_| "strategy_replay_arithmetic_failed")
}

fn median(mut values: Vec<Amount>) -> std::result::Result<Option<Amount>, &'static str> {
    if values.is_empty() {
        return Ok(None);
    }
    values.sort();
    let mid = values.len() / 2;
    Ok(Some(if values.len() % 2 == 1 {
        values[mid]
    } else {
        arithmetic(
            arithmetic(values[mid - 1].checked_add(values[mid]))?
                .checked_div(Amount::from_integer(2)),
        )?
    }))
}

/// The hash of the code that turns strategy actions into fills: this file, the
/// contract beside it, and the Python worker runner. The paper controller runs
/// an entry only when its report names this build's hash.
pub(crate) fn adapter_sha256() -> String {
    digest(
        [
            include_str!("mod.rs"),
            include_str!("replay.rs"),
            include_str!("selection.rs"),
            include_str!("../../../../nivra-core/src/strategy.rs"),
            include_str!("runner.py"),
        ]
        .concat()
        .as_bytes(),
    )
}

/// A replay that `stop` ended before the tape did. It has no report.
#[derive(Debug)]
pub(crate) struct Stopped {
    pub reason: &'static str,
    pub strategy_calls: usize,
    pub last_at_ms: Option<i64>,
}

/// Replay `source` over the tape on a persistent strategy worker. The report is
/// deterministic for the same source, parameters, tape, engine and worker limits,
/// holds no floats, and is hashed with the canonical artifact encoding. `stop`
/// is asked before each strategy call; a reason ends the replay with no report.
pub(crate) fn run(
    worker_config: &WorkerConfig,
    source: &[u8],
    parameters: &Parameters,
    manifest: &[u8],
    events: &[u8],
    stop: &dyn Fn() -> Option<&'static str>,
) -> Result<std::result::Result<Value, Stopped>> {
    parameters.validate()?;
    let screen = Screen::parse(&parameters.params)?;
    if source.is_empty()
        || source.len() > 262_144
        || manifest.len() > 512 * 1024
        || events.len() > nivra_cli::strategy_archive::MAX_EXECUTION_TAPE_BYTES
    {
        return Err("strategy_replay_input_limit".into());
    }
    if let Err(reasons) = review::static_check(
        &worker_config.python_path,
        source,
        worker_config.env_dir.is_some(),
    ) {
        return Err(format!("strategy_review_unsafe: {}", reasons.join(",")).into());
    }
    let (first_day, last_day, universe) =
        nivra_cli::strategy_archive::archive_identity(manifest, events)?;
    let universe: BTreeMap<String, Instrument> =
        universe.into_iter().map(|i| (i.key.clone(), i)).collect();
    let tape =
        nivra_cli::strategy_actions::parse_archive_events(events, first_day, last_day, &universe)?;
    let manifest_value: Value =
        serde_json::from_slice(manifest).map_err(|_| "strategy_replay_manifest_invalid")?;
    let capital = match parameters.capital {
        Some(capital) => capital,
        None => serde_json::from_value(manifest_value["capital"].clone())
            .map_err(|_| "strategy_replay_capital_required")?,
    };
    let costs: FeeArithmetic = match &parameters.costs {
        Some(costs) => costs.clone(),
        None => serde_json::from_value(manifest_value["paper_costs"].clone())
            .map_err(|_| "strategy_replay_costs_required")?,
    };
    costs
        .validate()
        .map_err(|_| "strategy_replay_costs_required")?;
    let env_sha256 = worker_config
        .env_sha256()
        .map_err(|_| "strategy_replay_env_invalid")?;
    let source_sha256 = digest(source);
    let params_bytes =
        serde_json::to_vec(&parameters.params).map_err(|_| "strategy_replay_parameters_invalid")?;
    let replay_worker_cfg = WorkerConfig {
        backoff_base_ms: 0,
        quarantine_failures: 0,
        ..worker_config.clone()
    };
    let mut worker = Worker::start(
        WorkerKey {
            source_sha256: source_sha256.clone(),
            params_sha256: digest(&params_bytes),
        },
        source.to_vec(),
        parameters.params.clone(),
        replay_worker_cfg,
        tape.first().map(|e| e.at).unwrap_or(0),
        None,
    )?;
    let mut portfolio = Portfolio::with_capital(capital);
    portfolio.costs = Some(costs.clone());
    portfolio.model_led = true;
    portfolio.settings.exit_policy = parameters.exit_policy.clone();
    let mut scanner = Scanner::default();
    nivra_cli::strategy_archive::load_daily_volume_baselines(&mut scanner, manifest)?;
    let mut coverage = Coverage::default();
    let mut selection = Selection::default();
    let mut next_refresh = 0;
    let mut session = None;
    // Remaining depth of the latest original receipt per key.
    let mut depth = BTreeMap::<String, Quote>::new();
    let mut dates = BTreeSet::<NaiveDate>::new();
    let (mut depth_events, mut calls) = (0usize, 0usize);
    let mut last_call_at = None::<i64>;
    let mut trades = Vec::<Trade>::new();
    let mut refusals = Vec::<Refusal>::new();
    let mut outcomes = BTreeMap::<String, usize>::new();
    let mut failures = BTreeMap::<&'static str, usize>::new();
    let mut first_failures = Vec::<CallFailure>::new();
    let mut high_water = capital;
    let mut worst_drawdown = Amount::ZERO;
    for event in &tape {
        let now = event.at;
        let day = local(now)?.date_naive();
        if let Some(reason) = stop() {
            return Ok(Err(Stopped {
                reason,
                strategy_calls: calls,
                last_at_ms: last_call_at,
            }));
        }
        if session != Some(day) {
            session = Some(day);
            next_refresh = now;
            selection.clear();
            depth.clear();
            for sample in scanner.samples.values_mut() {
                *sample = nivra_core::execution::Sample {
                    avg_daily_volume: sample.avg_daily_volume,
                    avg_daily_volume_sessions: sample.avg_daily_volume_sessions,
                    ..Default::default()
                };
            }
        }
        let screening = matches!(event.kind, EventKind::Screen);
        let mut changed = BTreeSet::new();
        for o in &event.observations {
            let executable = !screening || o.archive_sha256.is_some();
            if scanner.samples.get(&o.key).is_none_or(|previous| {
                o.quote.receipt > previous.seen
                    && previous
                        .source
                        .is_none_or(|source| o.quote.source >= source)
            }) {
                let instrument = &universe[&o.key];
                if executable {
                    scanner.observe_fields(now, instrument, &o.quote, &o.fields())?;
                } else {
                    scanner.observe_price(
                        now,
                        &o.key,
                        o.quote.bid,
                        o.quote.source,
                        o.quote.receipt,
                        MAX_FINANCIAL_AGE_MS,
                        Some(&o.fields()),
                    )?;
                }
            }
            if executable
                && depth
                    .get(&o.key)
                    .is_none_or(|old| o.quote.receipt > old.receipt && o.quote.source >= old.source)
            {
                depth.insert(o.key.clone(), o.quote.clone());
                if o.quote.fresh(now, MAX_FINANCIAL_AGE_MS) {
                    changed.insert(o.key.as_str());
                }
            }
        }
        let fresh_quote = |key: &str| {
            depth
                .get(key)
                .filter(|quote| quote.fresh(now, MAX_FINANCIAL_AGE_MS))
        };
        let held_keys: BTreeSet<_> = portfolio.positions.keys().cloned().collect();
        if now >= next_refresh {
            let ranked = super::prospects(&scanner, now, 40)?;
            selection.refresh(
                &scanner,
                now,
                screen.as_ref(),
                &held_keys,
                ranked.iter().map(|prospect| prospect.key.as_str()),
                40,
            );
            next_refresh = now.saturating_add(REFRESH_MS);
        }
        if changed.is_empty() {
            continue;
        }
        depth_events += 1;
        let mut fresh: BTreeMap<String, Quote> = held_keys
            .iter()
            .filter_map(|key| fresh_quote(key).map(|quote| (key.clone(), quote.clone())))
            .collect();
        // Held exits, exactly as the paper controller's quote commit runs them.
        let held = portfolio.positions.clone();
        let before = portfolio.fills.len();
        portfolio.apply_at(day, now, &fresh, &[])?;
        for fill in &portfolio.fills[before..] {
            let position = &held[&fill.instrument.key];
            let quote = &fresh[&fill.instrument.key];
            trades.push(Trade {
                key: fill.instrument.key.clone(),
                exit_at_ms: now,
                units: fill.units,
                entry_price: position.entry,
                exit_price: fill.price,
                exit_reason: exit_reason(position.exit_intent.as_ref(), quote, now),
                net_pnl: Amount::ZERO,
            });
        }
        for fill in &portfolio.fills[before..] {
            let quote = fresh.get_mut(&fill.instrument.key).expect("exit quote");
            quote.bid_units -= fill.units;
            depth.insert(fill.instrument.key.clone(), quote.clone());
        }
        let minute = local(now)?.hour() * 60 + local(now)?.minute();
        let in_session = (portfolio.settings.session_start_minute
            ..portfolio.settings.session_end_minute)
            .contains(&minute);
        let in_window = parameters.from_ms.is_none_or(|from| now >= from)
            && parameters.to_ms.is_none_or(|to| now <= to);
        let prospects = selection.prospects(&scanner, now, parameters.top_n, |key| {
            !portfolio.positions.contains_key(key)
                && depth.get(key).is_some_and(|quote| {
                    quote.fresh(now, MAX_FINANCIAL_AGE_MS)
                        && quote.bid_units > 0
                        && quote.ask_units > 0
                })
        });
        let relevant = held_keys.iter().any(|key| changed.contains(key.as_str()))
            || prospects
                .iter()
                .any(|prospect| changed.contains(prospect.key.as_str()));
        if in_session && in_window && relevant {
            fresh.retain(|key, _| portfolio.positions.contains_key(key));
            fresh.extend(
                prospects
                    .iter()
                    .map(|prospect| (prospect.key.clone(), depth[&prospect.key].clone())),
            );
            if let Some(reason) = stop() {
                return Ok(Err(Stopped {
                    reason,
                    strategy_calls: calls,
                    last_at_ms: last_call_at,
                }));
            }
            dates.insert(day);
            calls += 1;
            last_call_at = Some(now);
            let observation = Observation::new(
                now,
                &parameters.params,
                &prospects,
                &fresh,
                &portfolio,
                &universe,
                &costs,
            );
            coverage.observe(&prospects, &fresh);
            let called = serde_json::to_vec(&observation)
                .map_err(|_| "strategy_input_limit")
                .and_then(|input| worker.call_sync(&input, now))
                .and_then(|bytes| parse_output(&bytes, &observation));
            let instruments: Vec<Instrument> = observation
                .instruments
                .values()
                .map(|i| (*i).clone())
                .collect();
            match called {
                Err(reason) => {
                    *failures.entry(reason).or_default() += 1;
                    if first_failures.len() < 20 {
                        first_failures.push(CallFailure { at_ms: now, reason });
                    }
                }
                Ok(actions) => {
                    let held = portfolio.positions.clone();
                    let before = portfolio.fills.len();
                    let results = apply(
                        &mut portfolio,
                        &|| now,
                        &mut fresh,
                        &instruments,
                        actions,
                        &source_sha256,
                    )?;
                    for fill in portfolio.fills[before..].iter().filter(|f| !f.buy) {
                        trades.push(Trade {
                            key: fill.instrument.key.clone(),
                            exit_at_ms: now,
                            units: fill.units,
                            // A sell passes the planner check only on a key held before the call.
                            entry_price: held
                                .get(&fill.instrument.key)
                                .ok_or("strategy_replay_trade_mismatch")?
                                .entry,
                            exit_price: fill.price,
                            exit_reason: "strategy",
                            net_pnl: Amount::ZERO,
                        });
                    }
                    for (key, quote) in &fresh {
                        depth.insert(key.clone(), quote.clone());
                    }
                    record(now, results, &mut outcomes, &mut refusals);
                }
            }
        }
        let equity = portfolio.equity()?;
        high_water = high_water.max(equity);
        worst_drawdown = worst_drawdown.max(high_water.checked_sub(equity)?);
    }
    if let Some(reason) = stop() {
        return Ok(Err(Stopped {
            reason,
            strategy_calls: calls,
            last_at_ms: last_call_at,
        }));
    }
    portfolio
        .reconcile()
        .map_err(|_| "strategy_replay_reconciliation_failed")?;
    // FIFO net realised P&L per sell fill, after both legs' fees, in fill order.
    let realised: Vec<Amount> = portfolio
        .fills
        .iter()
        .zip(portfolio.realised_net_pnl())
        .filter(|(fill, _)| !fill.buy)
        .map(|(_, pnl)| pnl.ok_or("strategy_replay_pnl_unavailable"))
        .collect::<std::result::Result<_, _>>()?;
    if realised.len() != trades.len() {
        return Err("strategy_replay_trade_mismatch".into());
    }
    for (trade, pnl) in trades.iter_mut().zip(&realised) {
        trade.net_pnl = *pnl;
    }
    let mut net_pnl = Amount::ZERO;
    let mut wins = 0u64;
    for pnl in &realised {
        net_pnl = net_pnl.checked_add(*pnl)?;
        wins += u64::from(*pnl > Amount::ZERO);
    }
    let (mut fees, mut turnover) = (Amount::ZERO, Amount::ZERO);
    for fill in &portfolio.fills {
        fees = fees.checked_add(fill.fee)?;
        turnover = turnover.checked_add(
            fill.price
                .amount()
                .checked_mul(Amount::from_unsigned(fill.units))?,
        )?;
    }
    let win_rate = if realised.is_empty() {
        None
    } else {
        Some(arithmetic(
            Amount::from_unsigned(wins).checked_div(Amount::from_unsigned(realised.len() as u64)),
        )?)
    };
    let as_of = tape.last().map(|e| e.at).unwrap_or_default();
    let unmarked: Vec<&String> = portfolio
        .positions
        .iter()
        .filter(|(key, _)| {
            !depth
                .get(*key)
                .is_some_and(|q| q.fresh(as_of, MAX_FINANCIAL_AGE_MS))
        })
        .map(|(key, _)| key)
        .collect();
    let truncated = refusals.len().saturating_sub(MAX_LISTED);
    refusals.truncate(MAX_LISTED);
    Ok(Ok(json!({
        "abi": ABI,
        "report_version": 1,
        "paper_only": true,
        "source_sha256": source_sha256,
        "parameters": parameters,
        "manifest_sha256": digest(manifest),
        "events_sha256": digest(events),
        "engine_sha256": nivra_core::replay::engine_fingerprint(),
        "adapter_sha256": adapter_sha256(),
        "worker": {
            "call_deadline_ms": worker_config.call_deadline_ms,
            "memory_mb": worker_config.memory_mb,
            "max_output_bytes": worker_config.max_output_bytes,
            "env_sha256": env_sha256,
            "runner_sha256": super::runner_sha256(),
        },
        "capital": capital,
        "costs": costs,
        "tape": {
            "dates": dates,
            "first_at_ms": tape.first().map(|e| e.at),
            "last_at_ms": as_of,
            "events": tape.len(),
            "depth_events": depth_events,
            "strategy_calls": calls,
            "observation_coverage": coverage,
        },
        "summary": {
            "trades": realised.len(),
            "wins": wins,
            "win_rate": win_rate,
            "net_pnl": net_pnl,
            "median_trade": median(realised)?,
            "worst_drawdown": worst_drawdown,
            "fees": fees,
            "turnover": turnover,
            "ending_cash": portfolio.cash,
            "marked_equity": if unmarked.is_empty() { Some(portfolio.equity()?) } else { None },
            "open_positions": portfolio.positions.len(),
            "unmarked_positions": unmarked,
        },
        "trades": trades,
        "fills": portfolio.fills,
        "open_positions": portfolio.positions,
        "action_outcomes": outcomes,
        "refusals": refusals,
        "refusals_truncated": truncated,
        "strategy_failures": failures,
        "first_strategy_failures": first_failures,
    })))
}

fn record(
    now: i64,
    results: Vec<ActionResult>,
    outcomes: &mut BTreeMap<String, usize>,
    refusals: &mut Vec<Refusal>,
) {
    for result in results {
        *outcomes.entry(result.outcome.clone()).or_default() += 1;
        if result.outcome != "filled" && result.outcome != "exit_revised" {
            refusals.push(Refusal {
                at_ms: now,
                index: result.index,
                key: result.key,
                side: result.side,
                reason: result.outcome,
            });
        }
    }
}
