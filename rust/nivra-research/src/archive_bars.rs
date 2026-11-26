//! Trusted historical bar simulation. Only the Python strategy receives batches.
use crate::worker::{AnalysisResult, Launch};
use nivra_core::{
    execution::Portfolio,
    finance::{Amount, FeeArithmetic, Money, Price, Side},
};
use nivra_sandbox::{SandboxConfig, stream::Stream};
use nivra_store::{
    market_archive::{self, ArchiveClient, Bar, BarWindow},
    research_jobs::{CandidateWindows, SpecCapability},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const ABI: &str = "archive_bars_v1";
pub const EVIDENCE: &str = "bar_level";
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub window: BarWindow,
    pub splits: CandidateWindows,
    pub capital: Amount,
    pub costs: FeeArithmetic,
    pub params: Value,
    pub sandbox: SandboxConfig,
    pub archive_url: String,
    pub archive_password_file: PathBuf,
}

pub fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Split {
    Training,
    Test,
}

impl Request {
    pub fn valid(&self) -> bool {
        self.window.validate()
            && self.splits.training_from_ms >= 0
            && self.splits.training_from_ms <= self.splits.training_to_ms
            && self.splits.training_to_ms < self.splits.selection_from_ms
            && self.splits.selection_from_ms <= self.splits.selection_to_ms
            && self.capital > Amount::ZERO
            && self.costs.validate().is_ok()
            && self.params.is_object()
            && self.sandbox.validate().is_ok()
            && !self.sandbox.network
    }

    fn split(&self, at: i64) -> Option<Split> {
        if (self.splits.training_from_ms..=self.splits.training_to_ms).contains(&at) {
            Some(Split::Training)
        } else if (self.splits.selection_from_ms..=self.splits.selection_to_ms).contains(&at) {
            Some(Split::Test)
        } else {
            None
        }
    }

    fn connect(&self) -> Result<ArchiveClient> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&self.archive_password_file)?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.mode() & 0o777 != 0o600
            || meta.nlink() != 1
            || meta.len() > 32768
            || meta.uid() != std::fs::metadata("/proc/self")?.uid()
        {
            return Err("archive_password_file_invalid".into());
        }
        let mut secret = String::new();
        file.read_to_string(&mut secret)?;
        let password = secret.trim_end_matches(['\r', '\n']);
        if password.contains(['\r', '\n', '\0']) {
            return Err("archive_password_invalid".into());
        }
        market_archive::connect(&self.archive_url, password)
    }
}

#[derive(Clone, Copy)]
struct Pending {
    at: i64,
    units: u64,
}

struct Position {
    units: u64,
    paid: Amount,
    mark: i64,
}

#[derive(Serialize)]
struct Fill {
    key: String,
    signal_bar_ms: i64,
    fill_bar_ms: i64,
    side: Side,
    units: u64,
    open_paise: i64,
    price: Price,
    fee: Money,
}

#[derive(Serialize)]
struct Summary {
    bars: usize,
    batches: usize,
    first_bar_ms: i64,
    last_bar_ms: i64,
    signals: u64,
    unfilled_signals: usize,
    rejected_signals: u64,
    fills: u64,
    closed_trades: u64,
    open_positions: usize,
    fees: Amount,
    realized_net_pnl: Amount,
    mark_to_market_pnl: Amount,
    fill_sample: Vec<Fill>,
}

/// Per-split series kept beside the report for offline benches. It is not
/// serialized, so the report bytes and hash do not depend on it.
pub struct Trace {
    /// Mark-to-market equity after the last batch of each IST date, oldest first.
    pub daily_equity: Vec<(chrono::NaiveDate, Amount)>,
    /// Sum of filled notional on both sides.
    pub traded_notional: Amount,
}

struct Book<'a> {
    costs: &'a FeeArithmetic,
    capital: Amount,
    cash: Amount,
    pending: BTreeMap<usize, Pending>,
    held: BTreeMap<usize, Position>,
    summary: Summary,
    trace: Trace,
}

fn inr(paise: i64) -> Result<Amount> {
    Ok(Amount::from_integer(paise).checked_mul(Amount::parse("0.01")?)?)
}

impl<'a> Book<'a> {
    fn new(capital: Amount, costs: &'a FeeArithmetic) -> Self {
        Self {
            costs,
            capital,
            cash: capital,
            pending: BTreeMap::new(),
            held: BTreeMap::new(),
            summary: Summary {
                bars: 0,
                batches: 0,
                first_bar_ms: 0,
                last_bar_ms: 0,
                signals: 0,
                unfilled_signals: 0,
                rejected_signals: 0,
                fills: 0,
                closed_trades: 0,
                open_positions: 0,
                fees: Amount::ZERO,
                realized_net_pnl: Amount::ZERO,
                mark_to_market_pnl: Amount::ZERO,
                fill_sample: Vec::new(),
            },
            trace: Trace {
                daily_equity: Vec::new(),
                traded_notional: Amount::ZERO,
            },
        }
    }

    fn equity(&self) -> Result<Amount> {
        self.held.values().try_fold(self.cash, |value, p| {
            Ok(value.checked_add(inr(p.mark)?.checked_mul(Amount::from_unsigned(p.units))?)?)
        })
    }

    fn fill(&mut self, key: &str, bar: &Bar, at: i64, order: Pending) -> Result<()> {
        if order.at >= at {
            return Err("archive_bars_noncausal_fill".into());
        }
        let buy = order.units != 0;
        let side = if buy { Side::Buy } else { Side::Sell };
        let units = match (buy, self.held.get(&bar.instrument)) {
            (true, None) => order.units,
            (false, Some(position)) => position.units,
            _ => {
                self.summary.rejected_signals += 1;
                return Ok(());
            }
        };
        let tick = Price::try_from(inr(1)?)?;
        let raw = Price::try_from(inr(bar.open)?)?;
        // Apply at least one paise adverse allowance, even on a zero-slippage
        // paper schedule. This is a precision unit, not an exchange tick.
        let price = if buy {
            Portfolio::execution_with_costs(raw, tick, true, self.costs)?
                .amount()
                .max(raw.amount().checked_add(tick.amount())?)
        } else {
            // A costed price can round to zero. Such a fill has no valid price.
            let allowance = raw.amount().checked_sub(tick.amount())?;
            if allowance == Amount::ZERO {
                self.summary.rejected_signals += 1;
                return Ok(());
            }
            match Portfolio::execution_with_costs(raw, tick, false, self.costs) {
                Ok(price) => price.amount().min(allowance),
                Err(_) => {
                    self.summary.rejected_signals += 1;
                    return Ok(());
                }
            }
        };
        let price = Price::try_from(price)?;
        let notional = Money::try_from(price.amount().checked_mul(Amount::from_unsigned(units))?)?;
        let fee = self.costs.charge(side, notional)?;
        if buy {
            let paid = notional.amount().checked_add(fee.amount())?;
            if paid > self.cash {
                self.summary.rejected_signals += 1;
                return Ok(());
            }
            self.cash = self.cash.checked_sub(paid)?;
            self.held.insert(
                bar.instrument,
                Position {
                    units,
                    paid,
                    mark: bar.close,
                },
            );
        } else {
            let proceeds = notional.amount().checked_sub(fee.amount())?;
            if self.cash.checked_add(proceeds)? < Amount::ZERO {
                self.summary.rejected_signals += 1;
                return Ok(());
            }
            let position = self
                .held
                .remove(&bar.instrument)
                .ok_or("archive_bars_position_missing")?;
            self.cash = self.cash.checked_add(proceeds)?;
            self.summary.closed_trades += 1;
            self.summary.realized_net_pnl = self
                .summary
                .realized_net_pnl
                .checked_add(proceeds.checked_sub(position.paid)?)?;
        }
        self.trace.traded_notional = self.trace.traded_notional.checked_add(notional.amount())?;
        self.summary.fills += 1;
        self.summary.fees = self.summary.fees.checked_add(fee.amount())?;
        if self.summary.fill_sample.len() < 8 {
            self.summary.fill_sample.push(Fill {
                key: key.to_owned(),
                signal_bar_ms: order.at,
                fill_bar_ms: at,
                side,
                units,
                open_paise: bar.open,
                price,
                fee,
            });
        }
        Ok(())
    }

    fn on_open(&mut self, at: i64, keys: &[String], bars: &[Bar]) -> Result<()> {
        if self.summary.batches == 0 {
            self.summary.first_bar_ms = at;
        }
        self.summary.batches += 1;
        self.summary.bars += bars.len();
        self.summary.last_bar_ms = at;
        for bar in bars {
            if let Some(order) = self.pending.remove(&bar.instrument) {
                self.fill(&keys[bar.instrument], bar, at, order)?;
            }
            if let Some(position) = self.held.get_mut(&bar.instrument) {
                position.mark = bar.close;
            }
        }
        let day = chrono::DateTime::from_timestamp_millis(at + 19_800_000)
            .ok_or("archive_bars_time_invalid")?
            .date_naive();
        let equity = self.equity()?;
        match self.trace.daily_equity.last_mut() {
            Some((last, value)) if *last == day => *value = equity,
            _ => self.trace.daily_equity.push((day, equity)),
        }
        Ok(())
    }

    fn orders(&mut self, at: i64, bars: &[Bar], mut actions: Vec<(usize, u64)>) -> Result<()> {
        actions.sort_unstable_by_key(|order| order.0);
        if actions.len() > bars.len() || actions.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("archive_bars_actions_invalid".into());
        }
        for (row, units) in actions {
            let bar = bars.get(row).ok_or("archive_bars_actions_invalid")?;
            if units > 1_000_000 {
                return Err("archive_bars_actions_invalid".into());
            }
            self.pending.insert(bar.instrument, Pending { at, units });
            self.summary.signals += 1;
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(Summary, Trace)> {
        self.summary.unfilled_signals = self.pending.len();
        self.summary.open_positions = self.held.len();
        self.summary.mark_to_market_pnl = self.equity()?.checked_sub(self.capital)?;
        Ok((self.summary, self.trace))
    }
}

#[derive(Serialize)]
struct Report<'a> {
    abi: &'static str,
    evidence: &'static str,
    paper_only: bool,
    fill_rule: &'static str,
    unit: market_archive::BarUnit,
    from: chrono::NaiveDate,
    to: chrono::NaiveDate,
    requested_instruments: Option<usize>,
    instrument_filter_sha256: String,
    splits: &'a CandidateWindows,
    source_sha256: String,
    params_sha256: String,
    input_bars_sha256: String,
    capital: Amount,
    costs: &'a FeeArithmetic,
    training: Summary,
    test: Summary,
}

pub fn run(
    request: &Request,
    source: &str,
    db: &mut ArchiveClient,
    budget: Duration,
) -> Result<Vec<u8>> {
    Ok(run_traced(request, source, db, budget)?.0)
}

/// [`run`] plus the training and test [`Trace`]s, for `nivra-tools bar-bench`.
pub fn run_traced(
    request: &Request,
    source: &str,
    db: &mut ArchiveClient,
    budget: Duration,
) -> Result<(Vec<u8>, [Trace; 2])> {
    if !request.valid() || budget.is_zero() || budget > Duration::from_secs(600) {
        return Err("archive_bars_request_invalid".into());
    }
    let started = Instant::now();
    let mut stream = Stream::start(
        &request.sandbox,
        include_bytes!("bar_strategy.py"),
        &serde_json::to_vec(&json!({"params":request.params,"source":source}))?,
        budget.as_secs().clamp(1, 600),
    )
    .map_err(|_| "archive_bars_strategy_start_failed")?;
    let mut training = Book::new(request.capital, &request.costs);
    let mut test = Book::new(request.capital, &request.costs);
    let mut hash = Sha256::new();
    let mut input = Vec::new();
    let mut response = Vec::new();
    market_archive::read_bar_batches(db, &request.window, budget, |at, keys, bars| {
        if started.elapsed() >= budget {
            return Err("archive_bars_wall_budget".into());
        }
        let Some(split) = request.split(at) else {
            return Ok(());
        };
        let book = match split {
            Split::Training => &mut training,
            Split::Test => &mut test,
        };
        book.on_open(at, keys, bars)?;
        input.clear();
        let batch: Vec<_> = bars
            .iter()
            .map(|bar| {
                (
                    keys[bar.instrument].as_str(),
                    bar.open,
                    bar.high,
                    bar.low,
                    bar.close,
                    bar.volume,
                )
            })
            .collect();
        serde_json::to_writer(&mut input, &(at, split, batch))?;
        input.push(b'\n');
        hash.update(&input);
        stream
            .exchange(&input, &mut response)
            .map_err(|_| "archive_bars_strategy_exchange_failed")?;
        let (reported_at, orders): (i64, Vec<(usize, u64)>) = serde_json::from_slice(&response)?;
        if reported_at != at {
            return Err("archive_bars_actions_invalid".into());
        }
        book.orders(at, bars, orders)
    })?;
    stream
        .finish()
        .map_err(|_| "archive_bars_strategy_exit_failed")?;
    if training.summary.batches == 0 || test.summary.batches == 0 {
        return Err("archive_bars_split_empty".into());
    }
    if started.elapsed() >= budget {
        return Err("archive_bars_wall_budget".into());
    }
    let (training, training_trace) = training.finish()?;
    let (test, test_trace) = test.finish()?;
    let report = serde_json::to_vec(&Report {
        abi: ABI,
        evidence: EVIDENCE,
        paper_only: true,
        fill_rule: "next observed open in the same split, paper slippage with at least one paise adverse, plus paper fees",
        unit: request.window.unit,
        from: request.window.from,
        to: request.window.to,
        requested_instruments: request.window.instruments.as_ref().map(Vec::len),
        instrument_filter_sha256: sha256(&serde_json::to_vec(&request.window.instruments)?),
        splits: &request.splits,
        source_sha256: sha256(source.as_bytes()),
        params_sha256: sha256(&serde_json::to_vec(&request.params)?),
        input_bars_sha256: hex(&hash.finalize()),
        capital: request.capital,
        costs: &request.costs,
        training,
        test,
    })?;
    Ok((report, [training_trace, test_trace]))
}

/// Stage-local worker entry. Only this trusted process sees the archive settings.
pub fn worker_entry(stage: &Path) -> Result<()> {
    let launch: Launch = serde_json::from_slice(&std::fs::read(stage.join("launch.json"))?)?;
    let code = std::fs::read_to_string(stage.join("code"))?;
    let raw = std::fs::read(stage.join("input"))?;
    if launch.spec.capability != SpecCapability::ArchiveBars
        || sha256(code.as_bytes()) != launch.spec.script_hash
        || sha256(&raw) != launch.spec.input_hash
    {
        return Err("archive_bars_input_changed".into());
    }
    let request: Request = serde_json::from_slice(&raw)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let remaining = launch.deadline_ms.saturating_sub(now).max(0) as u64;
    let deadline = Instant::now() + Duration::from_millis(remaining);
    let mut db = request.connect()?;
    let report = run(
        &request,
        &code,
        &mut db,
        deadline.saturating_duration_since(Instant::now()),
    )?;
    let mut observations = vec![sha256(&report)];
    let mut text = std::str::from_utf8(&report)?;
    while !text.is_empty() {
        let end = text.floor_char_boundary(text.len().min(1000));
        observations.push(text[..end].to_owned());
        text = &text[end..];
    }
    if observations.len() > 32 {
        return Err("archive_bars_report_limit".into());
    }
    let result = serde_json::to_vec(&AnalysisResult {
        identity: launch.identity,
        script_hash: launch.spec.script_hash,
        runtime_hash: launch.spec.runtime_hash,
        input_hash: launch.spec.input_hash,
        metrics: Vec::new(),
        observations,
    })?;
    if result.len() as u64 > launch.spec.output_bytes {
        return Err("archive_bars_report_limit".into());
    }
    OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(stage.join("outputs/result.json"))?
        .write_all(&result)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_signal_fills_only_at_next_open_and_pays_both_legs() -> Result<()> {
        let costs = serde_json::from_value(json!({
            "per_order":"2","buy_bps":"0","sell_bps":"0",
            "slippage_bps":"100","rounding_increment":"0.01"
        }))?;
        let mut book = Book::new(Amount::from_integer(5000), &costs);
        let keys = ["NSE_EQ|fixture".to_owned()];
        let mut bar = Bar {
            instrument: 0,
            open: 9_000,
            high: 19_000,
            low: 8_000,
            close: 18_000,
            volume: 100,
        };
        book.on_open(1, &keys, &[bar])?;
        book.orders(1, &[bar], vec![(0, 1)])?;
        assert_eq!(book.summary.fills, 0);
        bar.open = 10_000;
        book.on_open(2, &keys, &[bar])?;
        book.orders(2, &[bar], vec![(0, 0)])?;
        bar.open = 12_000;
        book.on_open(3, &keys, &[bar])?;
        book.orders(3, &[bar], vec![(0, 1)])?;
        let (summary, trace) = book.finish()?;
        assert_eq!(trace.traded_notional, Amount::parse("219.8")?);
        assert_eq!(summary.unfilled_signals, 1);
        assert_eq!(summary.closed_trades, 1);
        assert_eq!(summary.fees, Amount::from_integer(4));
        assert_eq!(summary.realized_net_pnl, Amount::parse("13.8")?);
        assert_eq!(
            summary.fill_sample[0].price.amount(),
            Amount::from_integer(101)
        );
        assert_eq!(
            summary.fill_sample[1].price.amount(),
            Amount::parse("118.8")?
        );
        Ok(())
    }
}
