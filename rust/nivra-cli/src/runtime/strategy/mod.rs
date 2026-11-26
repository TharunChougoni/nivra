//! `strategy_actions_v1`: a sandboxed Python strategy called once per
//! observation. It reads one frozen [`Observation`] and prints zero or more
//! planner actions. Deterministic code validates them with the planner's
//! checks and executes them through `Portfolio::execute_plan`, exactly as it
//! executes a model plan. A strategy owns no cash, fill or protection logic.
use super::Result;
use super::planner::{self, PlanSide};
use nivra_core::{
    execution::{ExitIntent, Instrument, Portfolio, Quote},
    finance::{Amount, FeeArithmetic, Price},
    strategy::Feature,
};
use nivra_sandbox::{Failure, SandboxConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap, io::Write, os::unix::fs::OpenOptionsExt, path::Path, process::ExitCode,
};

pub(crate) mod replay;

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
}

fn failure_code(failure: Failure) -> &'static str {
    match failure {
        Failure::Config => "strategy_sandbox_config",
        Failure::Input => "strategy_input_limit",
        Failure::Setup => "strategy_sandbox_setup",
        Failure::Unavailable => "strategy_sandbox_unavailable",
        Failure::Exit => "strategy_exit",
        Failure::WallTimeout => "strategy_wall_timeout",
        Failure::OutputLimit => "strategy_output_limit",
        Failure::OutputInvalid => "strategy_output_invalid",
    }
}

/// Run the strategy once. `Err` is the explicit reason the whole call produced
/// no actions; `Ok` holds raw actions for [`apply`].
pub(crate) fn call(
    sandbox: &SandboxConfig,
    source: &[u8],
    observation: &Observation<'_>,
) -> std::result::Result<Vec<Value>, &'static str> {
    let input = serde_json::to_value(observation).map_err(|_| "strategy_input_limit")?;
    let bytes = nivra_sandbox::run_document(sandbox, source, &input).map_err(failure_code)?;
    parse(&bytes, observation)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    actions: Vec<Value>,
}

fn parse(
    bytes: &[u8],
    observation: &Observation<'_>,
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
            .is_some_and(|key| !observation.instruments.contains_key(key))
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
    now: i64,
    quotes: &mut BTreeMap<String, Quote>,
    instruments: &[Instrument],
    actions: Vec<Value>,
    decision: &str,
) -> Result<Vec<ActionResult>> {
    let keys: Vec<Option<String>> = actions
        .iter()
        .map(|a| a["instrument_key"].as_str().map(str::to_owned))
        .collect();
    let verdicts = planner::validate_actions(actions, portfolio, instruments, now)?;
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
            });
            continue;
        };
        let instrument = instruments
            .iter()
            .find(|i| i.key == action.instrument_key)
            .ok_or("strategy_instrument_missing")?;
        let side = side_name(&action.side);
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
                Err(refusal) => refusal.code(),
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
            &SandboxConfig::default(),
            &read(source, 262_144)?,
            &parameters,
            &read(manifest, 512 * 1024)?,
            &read(events, 16 * 1024 * 1024)?,
            &|| None,
        )?
        .map_err(|stopped| stopped.reason)?;
        let bytes = nivra_cli::candidate_execution::artifact_bytes(&report)?;
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
