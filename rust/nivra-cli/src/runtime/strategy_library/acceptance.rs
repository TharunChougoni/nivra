//! Deterministic after-cost acceptance of a `strategy_actions_v1` replay
//! report. Pure: the same report bytes, source bytes and thresholds give the
//! same verdict. Integrity checks run first and stop at the first failure;
//! the threshold checks then report every rule the report misses.
use crate::runtime::strategy::ABI;
use nivra_core::{finance::Amount, replay::digest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// The planner config's `strategy_acceptance` block. Absent means these
/// defaults. Drawdown and failures are rates, so they scale with capital and
/// tape length.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Thresholds {
    /// Closed round trips the replay must make.
    pub min_trades: u64,
    /// Closed net P&L after costs must be strictly above this. Never below 0.
    pub min_net_pnl: Amount,
    /// Worst equity drawdown, in basis points of the replay's capital.
    pub max_drawdown_bps: u32,
    /// Distinct session dates on the tape.
    pub min_tape_dates: usize,
    /// Failed strategy calls, in basis points of all calls.
    pub max_failure_bps: u32,
}
impl Default for Thresholds {
    fn default() -> Self {
        Self {
            min_trades: 10,
            min_net_pnl: Amount::ZERO,
            max_drawdown_bps: 300,
            min_tape_dates: 3,
            max_failure_bps: 100,
        }
    }
}
impl Thresholds {
    pub(crate) fn validate(&self) -> crate::runtime::Result<()> {
        if self.min_trades == 0
            || self.min_net_pnl < Amount::ZERO
            || !(1..=10_000).contains(&self.max_drawdown_bps)
            || !(1..=366).contains(&self.min_tape_dates)
            || self.max_failure_bps > 10_000
        {
            return Err("strategy_acceptance_thresholds_invalid".into());
        }
        Ok(())
    }
}

/// Why a report was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    ReportHashMismatch,
    ReportInvalid,
    ReportAbiUnsupported,
    ReportNotPaperOnly,
    SourceHashMismatch,
    TooFewTrades,
    NetPnlTooLow,
    DrawdownTooDeep,
    TooFewTapeDates,
    TooManyStrategyFailures,
    OpenPositionsAtEnd,
}

#[derive(Deserialize)]
struct Tape {
    dates: BTreeSet<String>,
    strategy_calls: u64,
}
#[derive(Deserialize)]
struct Summary {
    trades: u64,
    net_pnl: Amount,
    worst_drawdown: Amount,
    open_positions: u64,
}
#[derive(Deserialize)]
struct Params {
    params: Value,
}
/// The report fields acceptance and the library read.
#[derive(Deserialize)]
struct Report {
    abi: String,
    report_version: u32,
    paper_only: bool,
    source_sha256: String,
    parameters: Params,
    capital: Amount,
    tape: Tape,
    summary: Summary,
    strategy_failures: BTreeMap<String, u64>,
}

/// A report that passed every check. Only [`accept`] makes one, so only an
/// accepted strategy can be registered.
pub(crate) struct Accepted<'a> {
    pub report: &'a [u8],
    pub report_sha256: String,
    pub source: &'a [u8],
    pub source_sha256: String,
    pub params: Value,
    pub summary: Value,
    pub tape_dates: Vec<String>,
    pub thresholds: Thresholds,
}

pub(crate) fn accept<'a>(
    report: &'a [u8],
    report_sha256: &str,
    source: &'a [u8],
    thresholds: &Thresholds,
) -> Result<Accepted<'a>, Vec<Reason>> {
    if digest(report) != report_sha256 {
        return Err(vec![Reason::ReportHashMismatch]);
    }
    let value: Value = serde_json::from_slice(report).map_err(|_| vec![Reason::ReportInvalid])?;
    let parsed: Report = Report::deserialize(&value).map_err(|_| vec![Reason::ReportInvalid])?;
    if parsed.abi != ABI || parsed.report_version != 1 {
        return Err(vec![Reason::ReportAbiUnsupported]);
    }
    if !parsed.paper_only {
        return Err(vec![Reason::ReportNotPaperOnly]);
    }
    if digest(source) != parsed.source_sha256 {
        return Err(vec![Reason::SourceHashMismatch]);
    }
    let rejected = misses(&parsed, thresholds).map_err(|_| vec![Reason::ReportInvalid])?;
    if !rejected.is_empty() {
        return Err(rejected);
    }
    Ok(Accepted {
        report,
        report_sha256: report_sha256.to_owned(),
        source,
        source_sha256: parsed.source_sha256,
        params: parsed.parameters.params,
        summary: value["summary"].clone(),
        tape_dates: parsed.tape.dates.into_iter().collect(),
        thresholds: thresholds.clone(),
    })
}

fn misses(r: &Report, t: &Thresholds) -> Result<Vec<Reason>, nivra_core::finance::FinancialError> {
    let bps = |value: u32| Amount::from_unsigned(u64::from(value));
    let scale = Amount::from_integer(10_000);
    let failures: u64 = r.strategy_failures.values().sum();
    let checks = [
        (r.summary.trades < t.min_trades, Reason::TooFewTrades),
        (r.summary.net_pnl <= t.min_net_pnl, Reason::NetPnlTooLow),
        (
            r.summary.worst_drawdown.checked_mul(scale)?
                > r.capital.checked_mul(bps(t.max_drawdown_bps))?,
            Reason::DrawdownTooDeep,
        ),
        (
            r.tape.dates.len() < t.min_tape_dates,
            Reason::TooFewTapeDates,
        ),
        (
            u128::from(failures) * 10_000
                > u128::from(r.tape.strategy_calls) * u128::from(t.max_failure_bps),
            Reason::TooManyStrategyFailures,
        ),
        (r.summary.open_positions > 0, Reason::OpenPositionsAtEnd),
    ];
    Ok(checks
        .into_iter()
        .filter_map(|(missed, reason)| missed.then_some(reason))
        .collect())
}
