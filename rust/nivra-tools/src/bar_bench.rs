//! Run every bar strategy in a directory through the production archive bar
//! engine (`nivra_research::archive_bars::run_traced`) and write a leaderboard.
use chrono::NaiveDate;
use nivra_core::finance::{Amount, FeeArithmetic};
use nivra_research::archive_bars::{ABI, Request, Trace, run_traced};
use nivra_sandbox::SandboxConfig;
use nivra_store::{
    market_archive::{self, BarUnit, BarWindow, ist_midnight},
    research_jobs::CandidateWindows,
};
use serde_json::{Value, json};
use std::{
    fmt::Write as _,
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct Options {
    pub archive_url: String,
    pub password_file: PathBuf,
    pub strategies: PathBuf,
    pub only: Vec<String>,
    pub unit: BarUnit,
    pub from: NaiveDate,
    pub test_from: NaiveDate,
    pub to: NaiveDate,
    pub capital: Amount,
    pub costs: PathBuf,
    pub budget: Duration,
    pub jobs: usize,
    pub output: PathBuf,
    pub json: Option<PathBuf>,
}

/// One strategy's outcome. `stats` holds the training and test splits.
pub struct Entry {
    pub name: String,
    pub result: std::result::Result<[Stats; 2], String>,
    pub seconds: f64,
    report: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    pub return_pct: f64,
    pub realized: f64,
    pub sharpe: f64,
    pub max_drawdown_pct: f64,
    pub turnover: f64,
    pub trades: u64,
    pub fills: u64,
    pub fees: f64,
    pub open_positions: u64,
    pub rejected: u64,
}

fn number(value: &Value) -> f64 {
    match value {
        Value::String(text) => text.parse().unwrap_or(f64::NAN),
        other => other.as_f64().unwrap_or(f64::NAN),
    }
}

fn stats(summary: &Value, trace: &Trace, capital: f64) -> Stats {
    let mut prior = capital;
    let mut peak = capital;
    let mut drawdown: f64 = 0.0;
    let mut returns = Vec::with_capacity(trace.daily_equity.len());
    for (_, equity) in &trace.daily_equity {
        let equity: f64 = equity.to_string().parse().unwrap_or(f64::NAN);
        returns.push(equity / prior - 1.0);
        prior = equity;
        peak = peak.max(equity);
        drawdown = drawdown.max(1.0 - equity / peak);
    }
    let n = returns.len() as f64;
    let mean = returns.iter().sum::<f64>() / n.max(1.0);
    let variance = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    let sharpe = if variance > 0.0 {
        mean / variance.sqrt() * 252f64.sqrt()
    } else {
        0.0
    };
    let traded: f64 = trace
        .traded_notional
        .to_string()
        .parse()
        .unwrap_or(f64::NAN);
    Stats {
        return_pct: number(&summary["mark_to_market_pnl"]) / capital * 100.0,
        realized: number(&summary["realized_net_pnl"]),
        sharpe,
        max_drawdown_pct: drawdown * 100.0,
        turnover: traded / capital,
        trades: summary["closed_trades"].as_u64().unwrap_or(0),
        fills: summary["fills"].as_u64().unwrap_or(0),
        fees: number(&summary["fees"]),
        open_positions: summary["open_positions"].as_u64().unwrap_or(0),
        rejected: summary["rejected_signals"].as_u64().unwrap_or(0),
    }
}

/// `<name>.params.json` declares a bar strategy with
/// `{"abi":"archive_bars_v1","unit":"day"|"minute","params":{...}}`.
fn discover(options: &Options) -> Result<Vec<(String, String, Value)>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(&options.strategies)? {
        let path = entry?.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".params.json"))
        else {
            continue;
        };
        let declared: Value = serde_json::from_slice(&fs::read(&path)?)?;
        if declared["abi"] != ABI
            || serde_json::from_value::<BarUnit>(declared["unit"].clone())? != options.unit
            || (!options.only.is_empty() && !options.only.iter().any(|o| o == name))
        {
            continue;
        }
        let source = fs::read_to_string(path.with_file_name(format!("{name}.py")))?;
        let params = declared["params"].clone();
        if !params.is_object() {
            return Err(format!("bar_bench_params_not_object {name}").into());
        }
        found.push((name.to_owned(), source, params));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(found)
}

fn request(options: &Options, costs: &FeeArithmetic, params: Value) -> Result<Request> {
    let day = |date: NaiveDate| ist_midnight(date);
    let next = |date: NaiveDate| date.succ_opt().ok_or("bar_bench_date_invalid");
    let request = Request {
        window: BarWindow {
            unit: options.unit,
            from: options.from,
            to: options.to,
            instruments: None,
        },
        splits: CandidateWindows {
            training_from_ms: day(options.from),
            training_to_ms: day(options.test_from) - 1,
            selection_from_ms: day(options.test_from),
            selection_to_ms: day(next(options.to)?) - 1,
        },
        capital: options.capital,
        costs: costs.clone(),
        params,
        sandbox: SandboxConfig {
            cpu_seconds: 30,
            memory_mb: 1024,
            wall_ms: 30_000,
            max_output_bytes: 262_144,
            ..SandboxConfig::default()
        },
        archive_url: options.archive_url.clone(),
        archive_password_file: options.password_file.clone(),
    };
    if !request.valid() {
        return Err("bar_bench_request_invalid".into());
    }
    Ok(request)
}

fn run_one(
    options: &Options,
    password: &str,
    costs: &FeeArithmetic,
    item: &(String, String, Value),
) -> Entry {
    let started = Instant::now();
    let capital: f64 = options.capital.to_string().parse().unwrap_or(f64::NAN);
    let outcome = (|| -> Result<(Value, [Stats; 2])> {
        let request = request(options, costs, item.2.clone())?;
        let mut db =
            market_archive::connect(&options.archive_url, password).map_err(|e| e.to_string())?;
        let (bytes, [training, test]) =
            run_traced(&request, &item.1, &mut db, options.budget).map_err(|e| e.to_string())?;
        let report: Value = serde_json::from_slice(&bytes)?;
        let split = [
            stats(&report["training"], &training, capital),
            stats(&report["test"], &test, capital),
        ];
        Ok((report, split))
    })();
    let seconds = started.elapsed().as_secs_f64();
    match outcome {
        Ok((report, split)) => Entry {
            name: item.0.clone(),
            result: Ok(split),
            seconds,
            report: Some(report),
        },
        Err(error) => Entry {
            name: item.0.clone(),
            result: Err(error.to_string()),
            seconds,
            report: None,
        },
    }
}

pub fn run(options: &Options) -> Result<Vec<Entry>> {
    if !(options.from < options.test_from && options.test_from <= options.to) {
        return Err("bar_bench_split_invalid: need from < test_from <= to".into());
    }
    let password = fs::read_to_string(&options.password_file)?;
    let password = password.trim_end_matches(['\r', '\n']).to_owned();
    let costs: FeeArithmetic = serde_json::from_slice(&fs::read(&options.costs)?)?;
    costs
        .validate()
        .map_err(|e| format!("bar_bench_costs_invalid {e:?}"))?;
    let items = discover(options)?;
    if items.is_empty() {
        return Err("bar_bench_no_strategies".into());
    }
    let next = AtomicUsize::new(0);
    let entries = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..options.jobs.clamp(1, items.len()) {
            scope.spawn(|| {
                while let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let entry = run_one(options, &password, &costs, item);
                    eprintln!(
                        "bar_bench {} {:.1}s {}",
                        entry.name,
                        entry.seconds,
                        entry.result.as_ref().err().map_or("ok", String::as_str)
                    );
                    entries.lock().expect("bench results").push(entry);
                }
            });
        }
    });
    let mut entries = entries.into_inner().expect("bench results");
    entries.sort_by(|a, b| {
        let key = |e: &Entry| {
            e.result
                .as_ref()
                .map_or(f64::NEG_INFINITY, |s| s[1].return_pct)
        };
        key(b).total_cmp(&key(a)).then_with(|| a.name.cmp(&b.name))
    });
    fs::write(&options.output, markdown(options, &costs, &entries))?;
    if let Some(path) = &options.json {
        fs::write(
            path,
            serde_json::to_vec_pretty(&json_report(options, &entries))?,
        )?;
    }
    Ok(entries)
}

fn unit_name(unit: BarUnit) -> &'static str {
    match unit {
        BarUnit::Day => "day",
        BarUnit::Minute => "minute",
    }
}

fn markdown(options: &Options, costs: &FeeArithmetic, entries: &[Entry]) -> String {
    let mut out = String::new();
    let last_training = options.test_from.pred_opt().unwrap_or(options.test_from);
    let _ = writeln!(
        out,
        "Unit `{}`. Training {} to {}, test {} to {} (inclusive IST dates). Capital ₹{}. Costs `{}`: {}.\n",
        unit_name(options.unit),
        options.from,
        last_training,
        options.test_from,
        options.to,
        options.capital,
        options.costs.display(),
        serde_json::to_string(costs).unwrap_or_default(),
    );
    out.push_str("| Strategy | Test return | Test Sharpe | Test max DD | Test turnover | Test trades | Test fees ₹ | Train return | Train Sharpe | Train max DD | Train trades | Open at test end |\n");
    out.push_str(
        "| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |\n",
    );
    for entry in entries {
        match &entry.result {
            Ok([train, test]) => {
                let _ = writeln!(
                    out,
                    "| `{}` | {:+.2}% | {:.2} | {:.2}% | {:.1}x | {} | {:.0} | {:+.2}% | {:.2} | {:.2}% | {} | {} |",
                    entry.name,
                    test.return_pct,
                    test.sharpe,
                    test.max_drawdown_pct,
                    test.turnover,
                    test.trades,
                    test.fees,
                    train.return_pct,
                    train.sharpe,
                    train.max_drawdown_pct,
                    train.trades,
                    test.open_positions,
                );
            }
            Err(error) => {
                let _ = writeln!(out, "| `{}` | failed: `{error}` |||||||||||", entry.name);
            }
        }
    }
    out
}

fn json_report(options: &Options, entries: &[Entry]) -> Value {
    let split = |s: &Stats| {
        json!({
            "return_pct": s.return_pct, "realized_net_pnl": s.realized, "sharpe": s.sharpe,
            "max_drawdown_pct": s.max_drawdown_pct, "turnover": s.turnover,
            "closed_trades": s.trades, "fills": s.fills, "fees": s.fees,
            "open_positions": s.open_positions, "rejected_signals": s.rejected,
        })
    };
    json!({
        "unit": unit_name(options.unit),
        "from": options.from, "test_from": options.test_from, "to": options.to,
        "capital": options.capital,
        "strategies": entries.iter().map(|e| match &e.result {
            Ok([train, test]) => json!({
                "name": e.name, "seconds": e.seconds,
                "training": split(train), "test": split(test),
                "source_sha256": e.report.as_ref().map(|r| r["source_sha256"].clone()),
                "input_bars_sha256": e.report.as_ref().map(|r| r["input_bars_sha256"].clone()),
            }),
            Err(error) => json!({"name": e.name, "seconds": e.seconds, "error": error}),
        }).collect::<Vec<_>>(),
    })
}

/// Parse `--unit` for clap.
pub fn parse_unit(text: &str) -> std::result::Result<BarUnit, String> {
    serde_json::from_value(json!(text)).map_err(|_| "expected day or minute".into())
}
