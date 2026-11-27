//! `action-bench`: replay every `strategy_actions_v1` reference strategy over
//! archived quote tapes and rank them after costs. It runs the production
//! pieces of the research lane's `backtest` call, in order: the archive-window
//! tape (`archive_window_tape`), the sandboxed after-cost replay
//! (`strategy::replay::run`) and the acceptance check
//! (`strategy_library::acceptance::accept`) with the default thresholds. Only
//! the tool glue differs: no model call, no library registration and no
//! per-call admission caps beyond the tape builder's hard limits.
//!
//! Run it with the ignored test `action_bench`; `strategies/BENCHMARK.md`
//! holds the command and the results.
use super::*;
use crate::runtime::MarketArchiveSettings;
use crate::runtime::paper_replay::{ArchiveWindow, TapeLimits, archive_window_tape};
use crate::runtime::strategy::replay;
use crate::runtime::strategy_library::acceptance::{self, Thresholds};
use chrono::NaiveDate;
use nivra_core::replay::digest;
use nivra_store::market_archive::{self, quote_receipt_bodies, quote_receipt_ids};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;

type Res<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// What to replay. Strategy files are `<strategies_dir>/<strategy>.py` with
/// params from `<strategy>.params.json`; a variant's `params` override those.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub strategies_dir: PathBuf,
    /// A `paper_costs` schedule, bare or wrapped as `{"paper_costs":{..}}`.
    pub costs: PathBuf,
    pub capital: Amount,
    /// Keep the N most liquid names by 20-session traded value before
    /// `universe_before`, or every name seen when 0. The tape builder caps a
    /// replay at 100,000 observations and 16 MiB, so a full session needs this.
    pub universe_size: usize,
    pub universe_before: Option<NaiveDate>,
    pub tapes: Vec<TapePlan>,
    pub variants: Vec<VariantPlan>,
    /// Aggregate rows over these named tapes, one leaderboard each.
    pub leaderboards: Vec<LeaderboardPlan>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TapePlan {
    pub name: String,
    pub from_session: NaiveDate,
    pub to_session: NaiveDate,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VariantPlan {
    pub name: String,
    pub strategy: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LeaderboardPlan {
    pub title: String,
    pub tapes: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Metrics {
    pub trades: u64,
    pub wins: u64,
    pub net_pnl: f64,
    pub fees: f64,
    pub entry_notional: f64,
    pub worst_drawdown: f64,
    pub calls: u64,
    pub failures: u64,
    pub open_positions: u64,
    /// Entry and exit actions the replay filled, and those it refused.
    pub filled: u64,
    pub refused: u64,
    pub verdict: String,
    pub reasons: Vec<String>,
    pub report_sha256: String,
}

pub(super) struct RunResult {
    pub variant: String,
    pub tape: String,
    pub outcome: std::result::Result<Metrics, String>,
}

pub(super) struct TapeInfo {
    pub name: String,
    pub events: usize,
    pub observations: usize,
    pub bytes: usize,
    pub events_sha256: String,
    pub sessions: Vec<NaiveDate>,
    pub missing: Vec<NaiveDate>,
}

pub(super) struct Results {
    pub tapes: Vec<TapeInfo>,
    pub universe: usize,
    pub runs: Vec<RunResult>,
}

fn dec(value: &Value) -> f64 {
    match value {
        Value::String(text) => text.parse().unwrap_or(f64::NAN),
        other => other.as_f64().unwrap_or(f64::NAN),
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// The `size` most liquid names by mean traded value over the sessions that
/// closed before `before`. No quote from the replay window is used.
fn liquid_keys(
    db: &mut market_archive::ArchiveClient,
    before: NaiveDate,
    size: usize,
) -> Res<BTreeSet<String>> {
    db.batch_execute("SET statement_timeout = 0")?;
    let rows = db.query(
        "SELECT i.key FROM daily_bars b JOIN instruments i ON i.id = b.instrument_id
         WHERE b.day < $1::text::date AND b.day >= $1::text::date - 35
           AND i.key LIKE 'NSE\\_EQ|%'
         GROUP BY i.key HAVING count(*) >= 10
         ORDER BY avg(b.c::numeric * b.volume::numeric) DESC, i.key LIMIT $2::bigint",
        &[&before.to_string(), &(size as i64)],
    )?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

/// Instrument metadata for the replay universe, read from the archived quote
/// bodies themselves: the symbol the broker reported and the tick, taken as
/// the greatest common divisor of every displayed price. Nothing is invented;
/// a name with no priced level is left out.
fn universe(
    db: &mut market_archive::ArchiveClient,
    from_ms: i64,
    to_ms: i64,
    only: Option<&BTreeSet<String>>,
) -> Res<Vec<Value>> {
    let watermark = market_archive::quote_watermark(db)?;
    let ids = quote_receipt_ids(db, from_ms, to_ms, watermark, 1_000_000)?;
    let mut seen = BTreeMap::<String, (String, i64)>::new();
    for page in ids.chunks(64) {
        for raw in quote_receipt_bodies(db, page)? {
            let archive: Value = serde_json::from_str(&raw)?;
            let body: Value = serde_json::from_str(archive["body"].as_str().ok_or("body")?)?;
            for row in body["data"].as_object().ok_or("data")?.values() {
                let (Some(key), Some(symbol)) =
                    (row["instrument_token"].as_str(), row["symbol"].as_str())
                else {
                    continue;
                };
                if only.is_some_and(|keys| !keys.contains(key)) {
                    continue;
                }
                let entry = seen
                    .entry(key.to_owned())
                    .or_insert_with(|| (symbol.to_owned(), 0));
                for side in ["buy", "sell"] {
                    for level in row["depth"][side].as_array().into_iter().flatten() {
                        if let Some(price) = level["price"].as_f64().filter(|p| *p > 0.0) {
                            entry.1 = gcd(entry.1, (price * 100.0).round() as i64);
                        }
                    }
                }
            }
        }
    }
    Ok(seen
        .into_iter()
        .filter(|(_, (_, paise))| *paise > 0)
        .map(|(key, (symbol, paise))| {
            json!({"key":key,"symbol":symbol,"series":"EQ","tick":format!("{}.{:02}", paise / 100, paise % 100)})
        })
        .collect())
}

fn ist_ms(day: NaiveDate, end: bool) -> i64 {
    let midnight = nivra_store::market_archive::ist_midnight(day);
    if end {
        midnight + 86_400_000 - 1
    } else {
        midnight
    }
}

fn costs_file(path: &Path) -> Res<nivra_core::finance::FeeArithmetic> {
    let mut value: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if let Some(inner) = value.get("paper_costs") {
        value = inner.clone();
    }
    let costs: nivra_core::finance::FeeArithmetic = serde_json::from_value(value)?;
    costs.validate()?;
    Ok(costs)
}

fn outcomes(report: &Value, keep: impl Fn(&str) -> bool) -> u64 {
    report["action_outcomes"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| keep(name))
        .filter_map(|(_, n)| n.as_u64())
        .sum()
}

fn metrics(report: &Value, bytes: &[u8], source: &[u8]) -> Metrics {
    let summary = &report["summary"];
    let entry_notional = report["trades"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| dec(&t["units"]) * dec(&t["entry_price"]))
        .sum();
    let sha = digest(bytes);
    let (verdict, reasons) = match acceptance::accept(bytes, &sha, source, &Thresholds::default()) {
        Ok(_) => ("accepted".to_owned(), Vec::new()),
        Err(reasons) => (
            "rejected".to_owned(),
            reasons
                .iter()
                .map(|r| {
                    serde_json::to_value(r)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                })
                .collect(),
        ),
    };
    Metrics {
        trades: summary["trades"].as_u64().unwrap_or(0),
        wins: summary["wins"].as_u64().unwrap_or(0),
        net_pnl: dec(&summary["net_pnl"]),
        fees: dec(&summary["fees"]),
        entry_notional,
        worst_drawdown: dec(&summary["worst_drawdown"]),
        calls: report["tape"]["strategy_calls"].as_u64().unwrap_or(0),
        failures: report["strategy_failures"]
            .as_object()
            .map_or(0, |m| m.values().filter_map(Value::as_u64).sum()),
        open_positions: summary["open_positions"].as_u64().unwrap_or(0),
        filled: outcomes(report, |name| name == "filled"),
        refused: outcomes(report, |name| name != "filled" && name != "exit_revised"),
        verdict,
        reasons,
        report_sha256: sha,
    }
}

/// Replay the plan against the archive. `work` is a scratch directory.
pub(super) fn run_plan(
    plan: &Plan,
    root: &Path,
    archive: &MarketArchiveSettings,
    work: &Path,
    jobs: usize,
) -> Res<Results> {
    let costs = costs_file(&root.join(&plan.costs))?;
    let mut c = crate::runtime::tests::config(work);
    let last = plan
        .tapes
        .iter()
        .map(|t| t.to_session)
        .max()
        .ok_or("no tapes")?;
    c.session = last;
    c.paper_costs = Some(costs.clone());
    c.capital = Some(plan.capital);
    c.market_archive = Some(archive.clone());
    let mut db = crate::runtime::market_archive::connect(archive)?;
    let first = plan
        .tapes
        .iter()
        .map(|t| t.from_session)
        .min()
        .ok_or("no tapes")?;
    let keep = match plan.universe_size {
        0 => None,
        size => Some(liquid_keys(
            &mut db,
            plan.universe_before.unwrap_or(first),
            size,
        )?),
    };
    let instruments = universe(
        &mut db,
        ist_ms(first, false),
        ist_ms(last, true),
        keep.as_ref(),
    )?;
    let mut day = first;
    while day <= last {
        let directory = if day == c.session {
            c.directory.clone()
        } else {
            c.sessions_root
                .as_ref()
                .ok_or("root")?
                .join(day.to_string())
        };
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("wide-universe.json"),
            serde_json::to_vec(&json!({"session":day,"instruments":instruments}))?,
        )?;
        day = day.succ_opt().ok_or("date")?;
    }
    let limits = TapeLimits {
        max_quote_events: 10_000,
        max_observations: 100_000,
        max_tape_bytes: 16 * 1024 * 1024,
    };
    let no_stop = || None;
    let mut tapes = Vec::new();
    let mut infos = Vec::new();
    for plan_tape in &plan.tapes {
        let window = ArchiveWindow {
            max_days: 60,
            from_session: plan_tape.from_session,
            to_session: plan_tape.to_session,
            from_ms: ist_ms(plan_tape.from_session, false),
            to_ms: ist_ms(plan_tape.to_session, true),
            daily: None,
            receipt_id: None,
        };
        let (tape, coverage) = archive_window_tape(&c, &window, plan.capital, limits, &no_stop)?;
        let tape = tape.ok_or_else(|| format!("tape {} has no quotes", plan_tape.name))?;
        let text = String::from_utf8_lossy(&tape.events);
        infos.push(TapeInfo {
            name: plan_tape.name.clone(),
            events: text.lines().count(),
            observations: text.matches("\"archive_sha256\"").count(),
            bytes: tape.events.len(),
            events_sha256: tape.events_sha256.clone(),
            sessions: coverage.sessions,
            missing: coverage.missing_sessions,
        });
        tapes.push((plan_tape, window, tape));
    }
    let worker = crate::runtime::strategy::WorkerConfig {
        call_deadline_ms: 250,
        ..crate::runtime::strategy::WorkerConfig::default()
    };
    let mut sources = BTreeMap::<String, (Vec<u8>, Value)>::new();
    for variant in &plan.variants {
        let dir = root.join(&plan.strategies_dir);
        let source = std::fs::read(dir.join(format!("{}.py", variant.strategy)))?;
        let declared: Value = serde_json::from_slice(&std::fs::read(
            dir.join(format!("{}.params.json", variant.strategy)),
        )?)?;
        let mut params = declared["params"].clone();
        for (key, value) in variant.params.as_object().into_iter().flatten() {
            params[key] = value.clone();
        }
        sources.insert(variant.name.clone(), (source, params));
    }
    let queue: Vec<(&VariantPlan, usize)> = plan
        .variants
        .iter()
        .flat_map(|v| (0..tapes.len()).map(move |t| (v, t)))
        .collect();
    let next = AtomicUsize::new(0);
    let done = std::sync::Mutex::new(Vec::<(usize, RunResult)>::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&(variant, t)) = queue.get(index) else {
                        return;
                    };
                    let (plan_tape, window, tape) = &tapes[t];
                    let (source, params) = &sources[&variant.name];
                    let parameters = replay::Parameters {
                        version: 1,
                        capital: Some(plan.capital),
                        costs: Some(costs.clone()),
                        exit_policy: None,
                        params: params.clone(),
                        top_n: 20,
                        from_ms: Some(window.from_ms),
                        to_ms: Some(window.to_ms),
                    };
                    let outcome = match replay::run(
                        &worker,
                        source,
                        &parameters,
                        &tape.manifest,
                        &tape.events,
                        &|| None,
                    ) {
                        Ok(Ok(report)) => nivra_cli::canonical_json::bytes(&report)
                            .map(|bytes| metrics(&report, &bytes, source))
                            .map_err(|e| e.to_string()),
                        Ok(Err(stopped)) => Err(stopped.reason.to_owned()),
                        Err(error) => Err(crate::runtime::failure_code(error.as_ref())),
                    };
                    done.lock().expect("results").push((
                        index,
                        RunResult {
                            variant: variant.name.clone(),
                            tape: plan_tape.name.clone(),
                            outcome,
                        },
                    ));
                }
            });
        }
    });
    let mut runs = done.into_inner().expect("results");
    runs.sort_by_key(|(index, _)| *index);
    Ok(Results {
        tapes: infos,
        universe: instruments.len(),
        runs: runs.into_iter().map(|(_, run)| run).collect(),
    })
}

struct Row {
    variant: String,
    trades: u64,
    wins: u64,
    net: f64,
    fees: f64,
    notional: f64,
    drawdown_bps: f64,
    calls: u64,
    failures: u64,
    open: u64,
    filled: u64,
    refused: u64,
    verdicts: Vec<String>,
    errors: Vec<String>,
}

fn rows(results: &Results, tapes: &[String], capital: f64) -> Vec<Row> {
    let variants: Vec<&str> = results
        .runs
        .iter()
        .map(|r| r.variant.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut rows: Vec<Row> = variants
        .into_iter()
        .map(|name| {
            let mut row = Row {
                variant: name.into(),
                trades: 0,
                wins: 0,
                net: 0.0,
                fees: 0.0,
                notional: 0.0,
                drawdown_bps: 0.0,
                calls: 0,
                failures: 0,
                open: 0,
                filled: 0,
                refused: 0,
                verdicts: vec![],
                errors: vec![],
            };
            for run in results
                .runs
                .iter()
                .filter(|r| r.variant == name && tapes.contains(&r.tape))
            {
                match &run.outcome {
                    Ok(m) => {
                        row.trades += m.trades;
                        row.wins += m.wins;
                        row.net += m.net_pnl;
                        row.fees += m.fees;
                        row.notional += m.entry_notional;
                        row.drawdown_bps = row.drawdown_bps.max(m.worst_drawdown / capital * 1e4);
                        row.calls += m.calls;
                        row.failures += m.failures;
                        row.open += m.open_positions;
                        row.filled += m.filled;
                        row.refused += m.refused;
                        let reasons = if m.reasons.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", m.reasons.join(", "))
                        };
                        row.verdicts
                            .push(format!("{}: {}{reasons}", run.tape, m.verdict));
                    }
                    Err(error) => row.errors.push(format!("{}: {error}", run.tape)),
                }
            }
            row
        })
        .collect();
    rows.sort_by(|a, b| b.net.total_cmp(&a.net).then(a.variant.cmp(&b.variant)));
    rows
}

/// The leaderboards as Markdown, one table per plan leaderboard.
pub(super) fn render(plan: &Plan, results: &Results) -> String {
    let capital = dec(&json!(plan.capital.to_string()));
    let mut out = String::new();
    for board in &plan.leaderboards {
        let _ = writeln!(out, "### {}\n", board.title);
        out.push_str("| # | Variant | Trades | Net P&L ₹ | Net bps/trade | Win % | Max DD bps | Fees ₹ | Failed calls | Filled/refused actions | Open at end | Acceptance per tape |\n");
        out.push_str(
            "| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |\n",
        );
        for (rank, row) in rows(results, &board.tapes, capital).iter().enumerate() {
            let bps = if row.notional > 0.0 {
                format!("{:.1}", row.net / row.notional * 1e4)
            } else {
                "–".into()
            };
            let win = if row.trades > 0 {
                format!("{:.0}", row.wins as f64 / row.trades as f64 * 100.0)
            } else {
                "–".into()
            };
            let verdict = row
                .verdicts
                .iter()
                .chain(&row.errors)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ");
            let _ = writeln!(
                out,
                "| {} | `{}` | {} | {:.2} | {} | {} | {:.0} | {:.2} | {}/{} | {}/{} | {} | {} |",
                rank + 1,
                row.variant,
                row.trades,
                row.net,
                bps,
                win,
                row.drawdown_bps,
                row.fees,
                row.failures,
                row.calls,
                row.filled,
                row.refused,
                row.open,
                verdict
            );
        }
        out.push('\n');
    }
    let _ = writeln!(out, "Universe: {} instruments.\n", results.universe);
    out.push_str("| Tape | Sessions | Missing | Events | Observations | Bytes | `events_sha256` |\n| --- | --- | --- | ---: | ---: | ---: | --- |\n");
    for tape in &results.tapes {
        let days = |d: &[NaiveDate]| {
            d.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | `{}` |",
            tape.name,
            days(&tape.sessions),
            days(&tape.missing),
            tape.events,
            tape.observations,
            tape.bytes,
            &tape.events_sha256[..16]
        );
    }
    out
}

fn json_report(results: &Results) -> Value {
    json!({
        "universe": results.universe,
        "tapes": results.tapes.iter().map(|t| json!({"name":t.name,"events":t.events,"observations":t.observations,"bytes":t.bytes,"events_sha256":t.events_sha256,"sessions":t.sessions,"missing":t.missing})).collect::<Vec<_>>(),
        "runs": results.runs.iter().map(|r| match &r.outcome {
            Ok(m) => json!({"variant":r.variant,"tape":r.tape,"trades":m.trades,"wins":m.wins,"net_pnl":m.net_pnl,"fees":m.fees,"entry_notional":m.entry_notional,"worst_drawdown":m.worst_drawdown,"calls":m.calls,"failures":m.failures,"open_positions":m.open_positions,"filled":m.filled,"refused":m.refused,"verdict":m.verdict,"reasons":m.reasons,"report_sha256":m.report_sha256}),
            Err(e) => json!({"variant":r.variant,"tape":r.tape,"error":e}),
        }).collect::<Vec<_>>(),
    })
}

/// Ignored: needs an archive, bubblewrap and python. Environment:
/// `NIVRA_ACTION_BENCH_PLAN` (plan JSON), `NIVRA_ACTION_BENCH_URL` (archive URL
/// without a password), `NIVRA_ACTION_BENCH_PASSWORD_FILE` and
/// `NIVRA_ACTION_BENCH_OUT` (directory for `leaderboard.md` and `results.json`).
#[test]
#[ignore = "requires an archive with quote tapes and python; see strategies/BENCHMARK.md"]
fn action_bench() -> Res<()> {
    // A runner, not a check: without a plan it has nothing to run.
    if std::env::var_os("NIVRA_ACTION_BENCH_PLAN").is_none() {
        println!("action_bench skipped: NIVRA_ACTION_BENCH_PLAN is not set");
        return Ok(());
    }
    let var = |name: &str| std::env::var(name).map_err(|_| format!("{name} is required"));
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let plan: Plan = serde_json::from_slice(&std::fs::read(var("NIVRA_ACTION_BENCH_PLAN")?)?)?;
    let archive = MarketArchiveSettings {
        url: var("NIVRA_ACTION_BENCH_URL")?,
        password_file: var("NIVRA_ACTION_BENCH_PASSWORD_FILE")?.into(),
    };
    let out = PathBuf::from(var("NIVRA_ACTION_BENCH_OUT")?);
    std::fs::create_dir_all(&out)?;
    let work = tempfile::tempdir()?;
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    let results = run_plan(&plan, &root, &archive, work.path(), jobs)?;
    std::fs::write(out.join("leaderboard.md"), render(&plan, &results))?;
    std::fs::write(
        out.join("results.json"),
        serde_json::to_vec_pretty(&json_report(&results))?,
    )?;
    Ok(())
}

/// End to end over a tiny archive: the fixture strategy's tape is written as
/// quote receipts, then the bench reads them back through the archive window,
/// replays the strategy in the sandbox and ranks it.
#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn action_bench_runs_a_strategy_over_a_fixture_archive() -> Res<()> {
    use nivra_store::market_archive::{init, write_quotes};
    const MANIFEST: &str = include_str!("../../../../../../../fixtures/strategy/manifest.json");
    const EVENTS: &str = include_str!("../../../../../../../fixtures/strategy/events.jsonl");
    const SOURCE: &str = include_str!("../../../../../../../fixtures/strategy/strategy.py");
    const PARAMETERS: &str = include_str!("../../../../../../../fixtures/strategy/parameters.json");
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path();
    let archive = crate::runtime::market_archive::test_settings(dir)?;
    let mut writer = crate::runtime::market_archive::connect(&archive)?;
    init(&mut writer)?;
    let day: NaiveDate = "2026-11-25".parse()?;
    let start = ist_ms(day, false) + (9 * 60 + 20) * 60_000;
    for (step, event) in EVENTS
        .lines()
        .map(serde_json::from_str::<Value>)
        .enumerate()
    {
        let at = start + 1_000 * step as i64;
        let data: serde_json::Map<String, Value> = event?["observations"]
            .as_array()
            .ok_or("observations")?
            .iter()
            .map(|o| {
                let key = o["key"].as_str().expect("key");
                let price = |side: &str| o["quote"][side].as_str().expect("price").parse::<serde_json::Number>().expect("number");
                let row = json!({
                    "instrument_token":key,"symbol":key.trim_start_matches("NSE_EQ|"),"timestamp":at-100,
                    "last_price":price("bid"),"volume":o["volume"],"lower_circuit_limit":10,"upper_circuit_limit":1000,
                    "depth":{
                        "buy":[{"price":price("bid"),"quantity":o["quote"]["bid_units"]}],
                        "sell":[{"price":price("ask"),"quantity":o["quote"]["ask_units"]}]
                    }
                });
                (key.to_owned(), row)
            })
            .collect();
        let raw = json!({
            "source":"upstox-readonly-v1","purpose":"execution_or_protection",
            "requested_ms":at-200,"received_ms":at,
            "body":json!({"status":"success","data":data}).to_string()
        })
        .to_string();
        write_quotes(
            &mut writer,
            &crate::runtime::market_archive::normalize::capture_quotes(&raw)?,
        )?;
    }
    let manifest: Value = serde_json::from_str(MANIFEST)?;
    std::fs::create_dir_all(dir.join("strategies"))?;
    std::fs::write(dir.join("strategies/fixture.py"), SOURCE)?;
    std::fs::write(dir.join("strategies/fixture.params.json"), PARAMETERS)?;
    std::fs::write(
        dir.join("costs.json"),
        serde_json::to_vec(&manifest["paper_costs"])?,
    )?;
    let plan: Plan = serde_json::from_value(json!({
        "strategies_dir":"strategies","costs":"costs.json","capital":"5000","universe_size":0,
        "tapes":[{"name":"fixture-day","from_session":day,"to_session":day}],
        "variants":[
            {"name":"fixture","strategy":"fixture","params":{"start_ms":start}},
            {"name":"fixture-wide-target","strategy":"fixture","params":{"start_ms":start,"target":"5.00"}}
        ],
        "leaderboards":[{"title":"Fixture","tapes":["fixture-day"]}]
    }))?;
    let work = tempfile::tempdir()?;
    let results = run_plan(&plan, dir, &archive, work.path(), 2)?;
    assert_eq!(results.universe, 2);
    assert_eq!(results.tapes[0].sessions, vec![day]);
    assert!(results.tapes[0].missing.is_empty());
    let run = &results.runs[0];
    let m = run.outcome.as_ref().map_err(|e| e.clone())?;
    // Six strategy calls; the fixture strategy enters, revises, exits and is refused.
    assert_eq!(
        (run.variant.as_str(), m.calls, m.failures),
        ("fixture", 6, 0)
    );
    assert_eq!(m.trades, 2, "{:?}", m.reasons);
    assert_eq!(m.verdict, "rejected");
    assert!(
        m.reasons.contains(&"too_few_trades".to_owned()),
        "{:?}",
        m.reasons
    );
    assert!(
        m.reasons.contains(&"too_few_tape_dates".to_owned()),
        "{:?}",
        m.reasons
    );
    assert_eq!(results.runs.len(), 2);
    let board = render(&plan, &results);
    assert!(board.contains("| 1 | `fixture"), "{board}");
    Ok(())
}
