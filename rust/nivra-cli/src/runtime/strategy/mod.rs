//! `strategy_actions_v1`: a persistent Python strategy worker (`on_observation(obs)`)
//! hardened with empty env, private cwd, OS rlimits, Landlock, and seccomp.
//! Deterministic code validates returned planner actions with the planner's
//! checks and executes them through `Portfolio::execute_plan`, exactly as it
//! executes a model plan. A strategy owns no cash, fill or protection logic.
use super::Result;
use super::planner::{self, PlanSide};
#[cfg(test)]
use crate::runtime::strategy_library::review;
#[cfg(test)]
use nivra_core::replay::digest;
use nivra_core::{
    execution::{ExitIntent, Instrument, Portfolio, Quote, Refused, Scanner},
    finance::{Amount, FeeArithmetic, Price},
    market::MAX_FINANCIAL_AGE_MS,
    strategy::{Feature, StrategySpec, evaluate, extract_features_with_lookback},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap, io::Write, os::unix::fs::OpenOptionsExt, path::Path, process::ExitCode,
};

pub(crate) mod host;
pub(crate) mod replay;
pub(crate) mod worker;

pub(crate) use worker::{Worker, WorkerConfig, WorkerKey, runner_sha256};

pub(crate) const ABI: &str = "strategy_actions_v1";
/// Actions one call may return; more refuses the whole output.
pub(crate) const MAX_ACTIONS: usize = 32;
pub(crate) const MAX_PROSPECTS: usize = 50;

/// One ranked prospect, best first, with the features the scan holds for it.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct Prospect {
    pub key: String,
    pub rank: usize,
    pub score: f64,
    pub features: BTreeMap<Feature, f64>,
}

/// The built-in ranking of the scanner at `now`, best first, as one
/// observation carries it. The replay and the paper controller share it.
pub(crate) fn prospects(scanner: &Scanner, now: i64, top_n: usize) -> Result<Vec<Prospect>> {
    let spec = StrategySpec::default();
    let snapshot = extract_features_with_lookback(
        scanner,
        now,
        MAX_FINANCIAL_AGE_MS,
        MAX_FINANCIAL_AGE_MS,
        spec.lookback_samples,
    )?;
    Ok(evaluate(&spec, &snapshot)?
        .rows
        .into_iter()
        .filter_map(|row| row.score.map(|score| (row.key, score, row.features)))
        .take(top_n)
        .enumerate()
        .map(|(rank, (key, score, features))| Prospect {
            key,
            rank: rank + 1,
            score,
            features,
        })
        .collect())
}

#[derive(Debug, Serialize)]
struct Held<'a> {
    units: u64,
    entry: Price,
    mark: Price,
    marked_at_ms: i64,
    exit_intent: &'a ExitIntent,
}

/// The frozen input of one call. Prices and rupees are exact decimal strings,
/// times are epoch milliseconds, and only the keys in `instruments` may appear
/// in the output.
#[derive(Debug, Serialize)]
pub(crate) struct Observation<'a> {
    abi: &'static str,
    at_ms: i64,
    params: &'a Value,
    prospects: &'a [Prospect],
    /// Depth quotes no more than two seconds old at `at_ms`.
    quotes: &'a BTreeMap<String, Quote>,
    positions: BTreeMap<&'a str, Held<'a>>,
    instruments: BTreeMap<&'a str, &'a Instrument>,
    cash: Amount,
    costs: &'a FeeArithmetic,
}

const NO_EXIT: ExitIntent = ExitIntent {
    target: None,
    stop: None,
    time: None,
};

impl<'a> Observation<'a> {
    pub(crate) fn new(
        at_ms: i64,
        params: &'a Value,
        prospects: &'a [Prospect],
        quotes: &'a BTreeMap<String, Quote>,
        portfolio: &'a Portfolio,
        universe: &'a BTreeMap<String, Instrument>,
        costs: &'a FeeArithmetic,
    ) -> Self {
        let mut instruments = BTreeMap::new();
        for key in prospects
            .iter()
            .map(|p| &p.key)
            .chain(quotes.keys())
            .filter_map(|key| universe.get(key))
        {
            instruments.insert(key.key.as_str(), key);
        }
        let positions = portfolio
            .positions
            .iter()
            .map(|(key, p)| {
                instruments.insert(key.as_str(), &p.instrument);
                (
                    key.as_str(),
                    Held {
                        units: p.units,
                        entry: p.entry,
                        mark: p.mark,
                        marked_at_ms: p.marked_at,
                        exit_intent: p.exit_intent.as_ref().unwrap_or(&NO_EXIT),
                    },
                )
            })
            .collect();
        Self {
            abi: ABI,
            at_ms,
            params,
            prospects,
            quotes,
            positions,
            instruments,
            cash: portfolio.cash,
            costs,
        }
    }

    /// The only instruments the call's actions may name.
    pub(crate) fn instruments(&self) -> impl Iterator<Item = &Instrument> {
        self.instruments.values().copied()
    }
}

/// Run the strategy once through a persistent `Worker` (used by one-off tests).
/// `Err` is the explicit reason the whole call produced no actions; `Ok` holds
/// raw actions for [`apply`].
#[cfg(test)]
pub(crate) fn call(
    config: &WorkerConfig,
    source: &[u8],
    observation: &Observation<'_>,
) -> std::result::Result<Vec<Value>, &'static str> {
    review::static_check(&config.python_path, source, config.env_dir.is_some())
        .map_err(|_| "strategy_review_unsafe")?;
    let input = serde_json::to_vec(observation).map_err(|_| "strategy_input_limit")?;
    let params_bytes =
        serde_json::to_vec(observation.params).map_err(|_| "strategy_input_limit")?;
    let key = WorkerKey {
        source_sha256: digest(source),
        params_sha256: digest(&params_bytes),
    };
    let mut worker = Worker::start(
        key,
        source.to_vec(),
        observation.params.clone(),
        config.clone(),
        observation.at_ms,
        None,
    )?;
    let bytes = worker.call_sync(&input, observation.at_ms)?;
    parse_output(&bytes, observation)
}

pub(crate) fn parse_output(
    bytes: &[u8],
    observation: &Observation<'_>,
) -> std::result::Result<Vec<Value>, &'static str> {
    parse(bytes, |key| observation.instruments.contains_key(key))
}

pub(crate) fn parse_actions(
    bytes: &[u8],
    instruments: &[Instrument],
) -> std::result::Result<Vec<Value>, &'static str> {
    parse(bytes, |key| instruments.iter().any(|i| i.key == key))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    actions: Vec<Value>,
}

fn parse(
    bytes: &[u8],
    has_instrument: impl Fn(&str) -> bool,
) -> std::result::Result<Vec<Value>, &'static str> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| {
        // Python's json module prints these bare tokens for non-finite floats.
        let text = String::from_utf8_lossy(bytes);
        if ["NaN", "Infinity"].iter().any(|token| text.contains(token)) {
            "strategy_output_non_finite"
        } else {
            "strategy_output_invalid"
        }
    })?;
    let Output { actions } = serde_json::from_value(value).map_err(|_| "strategy_output_schema")?;
    if actions.len() > MAX_ACTIONS {
        return Err("strategy_too_many_actions");
    }
    if actions.iter().any(|action| {
        action["instrument_key"]
            .as_str()
            .is_some_and(|key| !has_instrument(key))
    }) {
        return Err("strategy_unknown_instrument");
    }
    Ok(actions)
}

/// What became of one action, in output order.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ActionResult {
    pub index: usize,
    pub key: Option<String>,
    pub side: Option<&'static str>,
    pub units: u64,
    /// `filled`, `exit_revised` or the refusal code.
    pub outcome: String,
    /// The limit a buy or sell carried.
    #[serde(skip)]
    pub max_price: Option<Price>,
    /// The execution gate's refusal with the numbers its check used.
    #[serde(skip)]
    pub refused: Option<Refused>,
    #[serde(skip)]
    pub gate_at_ms: Option<i64>,
}

fn side_name(side: &PlanSide) -> &'static str {
    match side {
        PlanSide::Buy => "buy",
        PlanSide::Sell => "sell",
        PlanSide::ReviseExit => "revise_exit",
    }
}

/// Validate the actions with the planner's checks, then execute each accepted
/// one through the shared execution gate, as the paper controller executes a
/// saved plan. A fill consumes depth from `quotes`, so a later action on the
/// same receipt cannot reuse it. `decision` is recorded on fills and exits.
pub(crate) fn apply(
    portfolio: &mut Portfolio,
    clock: &dyn Fn() -> i64,
    quotes: &mut BTreeMap<String, Quote>,
    instruments: &[Instrument],
    actions: Vec<Value>,
    decision: &str,
) -> Result<Vec<ActionResult>> {
    let keys: Vec<Option<String>> = actions
        .iter()
        .map(|a| a["instrument_key"].as_str().map(str::to_owned))
        .collect();
    let verdicts = planner::validate_actions(actions, portfolio, instruments, clock())?;
    let mut results = Vec::with_capacity(verdicts.len());
    for verdict in verdicts {
        let key = keys[verdict.index].clone();
        let Some(action) = verdict.action else {
            results.push(ActionResult {
                index: verdict.index,
                key,
                side: None,
                units: 0,
                outcome: verdict.reason,
                max_price: None,
                refused: None,
                gate_at_ms: None,
            });
            continue;
        };
        let instrument = instruments
            .iter()
            .find(|i| i.key == action.instrument_key)
            .ok_or("strategy_instrument_missing")?;
        let side = side_name(&action.side);
        let mut refused = None;
        let mut gate_at_ms = None;
        let outcome = if matches!(action.side, PlanSide::ReviseExit) {
            if portfolio.revise_exit(instrument, &action.exit_intent, decision)? {
                "exit_revised"
            } else {
                "exit_revision_rejected"
            }
        } else if let Some(quote) = quotes.get_mut(&action.instrument_key) {
            let buy = matches!(action.side, PlanSide::Buy);
            let max_price = action
                .max_price
                .ok_or("strategy_action_max_price_missing")?;
            let now = clock();
            gate_at_ms = Some(now);
            match portfolio.execute_plan(
                now,
                instrument,
                quote,
                buy,
                verdict.units,
                max_price,
                action.exit_intent,
                decision,
            )? {
                Ok(()) => {
                    if buy {
                        quote.ask_units -= verdict.units;
                    } else {
                        quote.bid_units -= verdict.units;
                    }
                    "filled"
                }
                Err(refusal) => {
                    let code = refusal.code();
                    refused = Some(refusal);
                    code
                }
            }
        } else {
            "quote_unavailable"
        };
        results.push(ActionResult {
            index: verdict.index,
            key,
            side: Some(side),
            units: verdict.units,
            outcome: outcome.into(),
            max_price: action.max_price,
            refused,
            gate_at_ms,
        });
    }
    Ok(results)
}

/// `nivra strategy-lab replay-strategy`: replay the source over the tape with
/// the default sandbox limits and write the canonical report to a new file.
pub(crate) fn replay_entry(
    source: &Path,
    parameters: &Path,
    manifest: &Path,
    events: &Path,
    output: &Path,
) -> ExitCode {
    let result = (|| -> Result<Value> {
        let read = |path: &Path, limit: u64| {
            crate::read_bounded(path, limit).map_err(|_| "strategy_replay_input_unreadable")
        };
        let parameters: replay::Parameters = serde_json::from_slice(&read(parameters, 65_536)?)
            .map_err(|_| "strategy_replay_parameters_invalid")?;
        let report = replay::run(
            &WorkerConfig::default(),
            &read(source, 262_144)?,
            &parameters,
            &read(manifest, 512 * 1024)?,
            &read(
                events,
                nivra_cli::strategy_archive::MAX_EXECUTION_TAPE_BYTES as u64,
            )?,
            &|| None,
        )?
        .map_err(|stopped| stopped.reason)?;
        let bytes = nivra_cli::canonical_json::bytes(&report)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)
            .map_err(|_| "strategy_replay_output_unwritable")?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(serde_json::json!({
            "report_sha256": nivra_core::replay::digest(&bytes),
            "summary": report["summary"],
            "tape": report["tape"],
            "strategy_failures": report["strategy_failures"],
        }))
    })();
    match result {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!(
                "strategy_replay_failed: {}",
                super::failure_code(error.as_ref())
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
