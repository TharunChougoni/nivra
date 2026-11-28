#![recursion_limit = "512"]
mod alert;
mod backtest;
mod preflight;
mod research;
mod runtime;
mod update;
use clap::{Parser, Subcommand};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

#[derive(Parser)]
#[command(version, about = "Nivra Rust paper-only runner")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum StrategyLabCommand {
    /// Replay a strategy_actions_v1 strategy over a quote tape after costs.
    ReplayStrategy {
        /// The strategy's Python source.
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        parameters: PathBuf,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        events: PathBuf,
        /// New report file; existing reports are never replaced.
        #[arg(long)]
        output: PathBuf,
    },
    #[command(flatten)]
    Lab(nivra_cli::strategy_lab::Commands),
}
#[derive(Subcommand)]
enum Command {
    /// Prepare a fresh current-policy paper installation; never starts trading.
    Setup(runtime::setup::Args),
    /// Offline strategy proposals, recorded-data evaluation and read-only evidence.
    StrategyLab {
        #[command(subcommand)]
        command: StrategyLabCommand,
    },
    /// Opt-in model planner preview; never trades.
    Planner {
        #[command(subcommand)]
        command: runtime::planner::Command,
    },
    /// Paper strategy library: list, show, pin, disable or roll back a version.
    Strategy {
        /// The paper runtime config; the library sits beside its session books.
        #[arg(long)]
        config: PathBuf,
        #[command(subcommand)]
        command: runtime::strategy_library::Command,
    },
    /// Manage dashboard authentication token; never starts trading.
    DashboardToken {
        #[command(subcommand)]
        command: DashboardTokenCommand,
    },
    /// Send a redacted operator notification; never starts a session.
    Alert {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, conflicts_with = "event", required_unless_present = "event")]
        test: bool,
        #[arg(long, value_enum)]
        event: Option<alert::Event>,
    },
    /// Offline writable-path and systemd sandbox admission; never starts trading.
    Preflight {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        unit_file: Option<PathBuf>,
    },
    /// Offline bounded quote backtest. Prints a report; never starts trading.
    Backtest {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long)]
        events: PathBuf,
    },
    /// Build or verify the read-only Python analysis environment for the research sandbox.
    ResearchEnv {
        #[command(subcommand)]
        command: runtime::research_env::Command,
    },
    /// Exploratory archived daily-OHLC exit sensitivity; never starts trading.
    ExitBacktest {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        start: Option<chrono::NaiveDate>,
        #[arg(long)]
        end: Option<chrono::NaiveDate>,
    },
    /// Print the engine fingerprint required in archived replay manifests.
    BacktestFingerprint,
    /// Initialize or inspect the shared TimescaleDB market archive.
    Archive {
        #[command(subcommand)]
        command: runtime::market_archive::Command,
    },
    /// Read-only Upstox candle evidence download; never starts trading.
    HistoryDownload {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        start: Option<chrono::NaiveDate>,
        #[arg(long)]
        end: Option<chrono::NaiveDate>,
        #[arg(long, value_enum, default_value = "both")]
        interval: runtime::history_download::Interval,
    },
    /// Manually catch up required daily sessions using original quota/receipts.
    HistoryBackfill {
        #[arg(long)]
        config: PathBuf,
        /// Check actual candle coverage without calling the history provider.
        #[arg(long)]
        check_only: bool,
        /// Prepare the dated cash universe from public sources before checking or downloading.
        #[arg(long)]
        prepare_universe: bool,
        /// Use the upcoming reviewed session and the current frozen universe.
        #[arg(long, conflicts_with = "prepare_universe")]
        upcoming: bool,
    },
    /// Export historical scanner features from the sealed paper book.
    ScannerArchive {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=60))]
        max_snapshots: u32,
        /// Write the session's quote tape (manifest.json and events.jsonl) into
        /// the new directory OUTPUT, for strategy-lab replays.
        #[arg(long)]
        tape: bool,
        /// With --tape, keep only this instrument key; repeat for more.
        #[arg(long = "key", requires = "tape")]
        keys: Vec<String>,
    },
    Update(update::Args),
    /// Offline pi agent check: one decision turn, its research request and the
    /// next decision turn against the scripted fake provider; never trades.
    AgentCheck(runtime::agent::check::Args),
    /// Inspect the brief or run one shadow pi turn against the current book.
    AgentTurn(runtime::agent::turn_cli::Args),
    /// Summarize recorded pi shadow decisions and the sealed paper session.
    AgentShadowReport(runtime::agent::shadow_report::Args),
    /// Step the autonomous research loop off-hours, or print its state.
    ResearchLoop(runtime::autoresearch::cli::Args),
    /// Quick offline binary/storage self-check; never opens a session.
    SelfCheck {
        #[arg(long)]
        expected_version: Option<String>,
    },
    /// Paper-only daily controller and authenticated loopback dashboard.
    Paper {
        #[arg(long)]
        config: PathBuf,
    },
    /// Offline rollover preview, reviewed advance or explicit human acknowledgment; never starts trading.
    #[command(group(clap::ArgGroup::new("timed_rollover").args(["dry_run", "advance"])))]
    #[command(group(clap::ArgGroup::new("reviewed_rollover").args(["acknowledge_closed_book", "accept_stopped_carry", "accept_halted_carry", "advance", "open_fresh_book"])))]
    Rollover {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, requires = "timed_rollover")]
        now: Option<chrono::DateTime<chrono::FixedOffset>>,
        #[arg(
            long,
            required_unless_present_any = ["acknowledge_closed_book", "accept_stopped_carry", "accept_halted_carry", "advance", "open_fresh_book"],
            requires = "now",
            conflicts_with_all = ["acknowledge_closed_book", "accept_stopped_carry", "accept_halted_carry", "open_fresh_book"]
        )]
        dry_run: bool,
        /// Seal a matching closed flat book after human inspection; preserves its stop.
        #[arg(long, requires = "confirm_paper_review")]
        acknowledge_closed_book: bool,
        /// Accept carry from a sealed operator-stopped paper model-led book.
        #[arg(long, requires = "confirm_paper_review")]
        accept_stopped_carry: bool,
        /// Accept protection-only carry from a sealed halted model-led book.
        #[arg(long, requires = "confirm_paper_review")]
        accept_halted_carry: bool,
        /// Advance one already-validated rollover while the controller is stopped.
        #[arg(long, requires_all = ["now", "confirm_paper_review"])]
        advance: bool,
        /// Open CONFIG's session as a fresh book funded by its sleeves, beside a
        /// closed, flat predecessor. Moves only the session pointer aside.
        #[arg(long, requires = "confirm_paper_review")]
        open_fresh_book: bool,
        #[arg(long, requires = "reviewed_rollover")]
        confirm_paper_review: bool,
    },
    /// Preview or apply opt-in model-artifact retention for closed past sessions.
    Retention {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        #[arg(long)]
        apply: bool,
    },
    /// Research-only authenticated loopback dashboard (Backtest lab, verification); no controller state.
    Dashboard {
        #[arg(long)]
        token_file: PathBuf,
        /// Paper config whose dashboard settings and public host apply.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Read-only credential/source probe; prints only a redacted outcome.
    Probe {
        #[arg(long)]
        config: PathBuf,
    },
    /// Queue one reviewed analysis from a model tool call. Does not execute it.
    ResearchEnqueue {
        #[arg(long)]
        queue: PathBuf,
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        request: PathBuf,
    },
    /// Judge a finished sandbox result. A measurement does not accept a strategy.
    ResearchAccept {
        #[arg(long)]
        result: PathBuf,
        #[arg(long)]
        code: PathBuf,
        #[arg(long)]
        runtime: PathBuf,
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        split: String,
        #[arg(long)]
        observation: PathBuf,
    },
    /// Read-only next-session approvals, usage, history and rollover readiness; no provider calls.
    Readiness {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        now: Option<chrono::DateTime<chrono::FixedOffset>>,
    },
    /// Audit an eligible flat paper halt, or enter reviewed protection-only mode.
    PlannerRecover {
        #[arg(long)]
        config: PathBuf,
        /// Keep the halt and block new entries while an open model-led book protects exits.
        #[arg(long)]
        protection_only: bool,
        #[arg(long)]
        confirm_paper_review: bool,
    },
    /// Settle one interrupted current-session history attempt as reviewed failed/no-replay.
    HistoryRecover {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        now: Option<chrono::DateTime<chrono::FixedOffset>>,
        #[arg(long)]
        confirm_paper_review: bool,
    },
    /// Audit an edited planner config under the stopped controller lock; never clears halts.
    ConfigApply {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        confirm_paper_review: bool,
    },
    /// Resume a reviewed flat paper book; audit model-led activation on pristine legacy books.
    Resume {
        #[arg(long)]
        config: PathBuf,
        /// Required. Confirms the operator reviewed the paper book.
        #[arg(long)]
        confirm_paper_review: bool,
    },
    /// Request an operator stop of the active paper book. Without it, a service
    /// stop or restart continues the same session; with it, resume is required.
    Stop {
        #[arg(long)]
        config: PathBuf,
        /// Required. Confirms the operator wants the book stopped until resume.
        #[arg(long)]
        confirm_paper_review: bool,
    },
}

#[derive(Subcommand)]
enum DashboardTokenCommand {
    /// Rotate dashboard token file (mode 0600) and require restart.
    Rotate {
        #[arg(long)]
        config: PathBuf,
        /// Optional explicit token (minimum 32 alphanumeric bytes).
        #[arg(long)]
        token: Option<String>,
    },
}

fn main() -> ExitCode {
    let Args { command } = Args::parse();
    match command {
        Command::Setup(args) => runtime::setup::entry(args),
        Command::StrategyLab {
            command:
                StrategyLabCommand::ReplayStrategy {
                    source,
                    parameters,
                    manifest,
                    events,
                    output,
                },
        } => runtime::strategy::replay_entry(&source, &parameters, &manifest, &events, &output),
        Command::StrategyLab {
            command: StrategyLabCommand::Lab(command),
        } => match nivra_cli::strategy_lab::run(
            command,
            runtime::market_archive::connect_from_config,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => {
                eprintln!("strategy_lab_failed: check configuration and recorded evidence");
                ExitCode::FAILURE
            }
        },
        Command::Planner { command } => runtime::planner::entry(command),
        Command::Strategy { config, command } => runtime::strategy_library::entry(&config, command),
        Command::Alert {
            config,
            test,
            event,
        } => alert::entry(config.as_deref(), test, event),
        Command::Preflight { config, unit_file } => preflight::entry(&config, unit_file.as_deref()),
        Command::Backtest { manifest, events } => backtest::entry(&manifest, &events),
        Command::ResearchEnv { command } => runtime::research_env::entry(command),
        Command::Archive { command } => runtime::market_archive::entry(command),
        Command::HistoryBackfill {
            config,
            check_only,
            prepare_universe,
            upcoming,
        } => runtime::history_download::backfill_entry(
            &config,
            check_only,
            prepare_universe,
            upcoming,
        ),
        Command::ScannerArchive {
            config,
            output,
            max_snapshots,
            tape,
            keys,
        } => runtime::paper_replay::scanner_archive_entry(
            &config,
            &output,
            max_snapshots as usize,
            tape,
            &keys,
        ),
        Command::HistoryDownload {
            config,
            start,
            end,
            interval,
        } => runtime::history_download::entry(&config, start, end, interval),
        Command::ExitBacktest {
            config,
            output,
            start,
            end,
        } => runtime::exit_backtest::entry(&config, &output, start, end),
        Command::BacktestFingerprint => {
            println!("{}", nivra_core::replay::engine_fingerprint());
            ExitCode::SUCCESS
        }
        Command::Update(args) => update::entry(args),
        Command::AgentCheck(args) => runtime::agent::check::run(args),
        Command::AgentTurn(args) => runtime::agent::turn_cli::run(args),
        Command::AgentShadowReport(args) => runtime::agent::shadow_report::run(args),
        Command::ResearchLoop(args) => runtime::autoresearch::cli::run(args),
        Command::SelfCheck { expected_version } => self_check(expected_version.as_deref()),
        Command::Retention {
            config,
            dry_run: _,
            apply,
        } => runtime::retention::entry(&config, apply),
        Command::Dashboard { token_file, config } => {
            runtime::research_dashboard::entry(&token_file, config.as_deref())
        }
        Command::Rollover {
            config,
            now,
            dry_run: _,
            acknowledge_closed_book,
            accept_stopped_carry,
            accept_halted_carry,
            advance,
            open_fresh_book,
            confirm_paper_review,
        } => {
            if open_fresh_book {
                runtime::rollover::open_fresh_entry(&config, confirm_paper_review)
            } else if acknowledge_closed_book {
                runtime::rollover::acknowledge_entry(&config, confirm_paper_review)
            } else if accept_stopped_carry {
                runtime::rollover::accept_stopped_carry_entry(&config, confirm_paper_review)
            } else if accept_halted_carry {
                runtime::rollover::accept_halted_carry_entry(&config, confirm_paper_review)
            } else if advance {
                runtime::rollover::advance_entry(&config, now, confirm_paper_review)
            } else {
                runtime::rollover::entry(
                    &config,
                    now.expect("required with --dry-run").timestamp_millis(),
                )
            }
        }
        Command::Paper { config } => runtime::entry(&config, false),
        Command::Probe { config } => runtime::entry(&config, true),
        Command::ResearchEnqueue {
            queue,
            catalog,
            request,
        } => research::enqueue(&queue, &catalog, &request),
        Command::ResearchAccept {
            result,
            code,
            runtime,
            input,
            split,
            observation,
        } => research::accept(&result, &code, &runtime, &input, &split, &observation),
        Command::Readiness { config, now } => runtime::readiness::entry(
            &config,
            now.map(|v| v.timestamp_millis())
                .unwrap_or_else(runtime::millis),
        ),
        Command::PlannerRecover {
            config,
            protection_only,
            confirm_paper_review,
        } => runtime::planner_recover_entry(&config, protection_only, confirm_paper_review),
        Command::HistoryRecover {
            config,
            now,
            confirm_paper_review,
        } => runtime::history_recover_entry(&config, now, confirm_paper_review),
        Command::ConfigApply {
            config,
            confirm_paper_review,
        } => runtime::config_apply_entry(&config, confirm_paper_review),
        Command::Resume {
            config,
            confirm_paper_review,
        } => runtime::resume_entry(&config, confirm_paper_review),
        Command::Stop {
            config,
            confirm_paper_review,
        } => runtime::stop_entry(&config, confirm_paper_review),
        Command::DashboardToken { command } => match command {
            DashboardTokenCommand::Rotate { config, token } => {
                runtime::dashboard_token_rotate_entry(&config, token.as_deref())
            }
        },
    }
}

pub(crate) fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > limit {
        return Err(());
    }
    Ok(bytes)
}

fn self_check(expected: Option<&str>) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    if expected.is_some_and(|v| v.strip_prefix('v').unwrap_or(v) != version) {
        eprintln!(
            "self_check_version_mismatch: expected={}, actual=v{version}",
            expected
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-')
                .take(64)
                .collect::<String>()
        );
        return ExitCode::FAILURE;
    }
    let check = (|| -> Result<(), Box<dyn std::error::Error>> {
        let db = rusqlite::Connection::open_in_memory()?;
        db.execute_batch("CREATE TABLE self_check(id INTEGER PRIMARY KEY)")?;
        let one = nivra_core::finance::Amount::from_integer(1);
        if one.checked_add(one)? != nivra_core::finance::Amount::from_integer(2) {
            return Err("self_check_arithmetic_failed".into());
        }
        Ok(())
    })();
    if let Err(error) = check {
        let reason = match error.to_string().as_str() {
            "self_check_arithmetic_failed" => "self_check_arithmetic_failed",
            _ => "self_check_storage_failed",
        };
        eprintln!("self_check_failed: {reason}");
        return ExitCode::FAILURE;
    }
    println!(
        "{}",
        serde_json::json!({"self_check":"ok","version":version,"live_enabled":false})
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod rollover_cli_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(
            ["nivra", "rollover"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    }

    #[test]
    fn rollover_actions_keep_explicit_cli_contracts() {
        let now = "2026-10-19T08:30:00+05:30";
        for valid in [
            vec![
                "--config",
                "c",
                "--accept-stopped-carry",
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--accept-halted-carry",
                "--confirm-paper-review",
            ],
            vec!["--config", "c", "--now", now, "--dry-run"],
            vec![
                "--config",
                "c",
                "--acknowledge-closed-book",
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--now",
                now,
                "--advance",
                "--confirm-paper-review",
            ],
        ] {
            assert!(parse(&valid).is_ok(), "{valid:?}");
        }
        for invalid in [
            vec!["--config", "c", "--accept-stopped-carry"],
            vec!["--config", "c", "--accept-halted-carry"],
            vec![
                "--config",
                "c",
                "--accept-halted-carry",
                "--accept-stopped-carry",
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--accept-halted-carry",
                "--advance",
                "--now",
                now,
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--accept-stopped-carry",
                "--acknowledge-closed-book",
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--accept-stopped-carry",
                "--dry-run",
                "--now",
                now,
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--accept-stopped-carry",
                "--advance",
                "--now",
                now,
                "--confirm-paper-review",
            ],
            vec!["--config", "c", "--now", now, "--advance"],
            vec!["--config", "c", "--advance", "--confirm-paper-review"],
            vec!["--config", "c", "--dry-run"],
            vec!["--config", "c", "--now", now],
            vec!["--config", "c", "--dry-run", "--confirm-paper-review"],
            vec![
                "--config",
                "c",
                "--now",
                now,
                "--advance",
                "--dry-run",
                "--confirm-paper-review",
            ],
            vec![
                "--config",
                "c",
                "--now",
                now,
                "--advance",
                "--acknowledge-closed-book",
                "--confirm-paper-review",
            ],
        ] {
            assert!(parse(&invalid).is_err(), "{invalid:?}");
        }

        // Verify dashboard-token rotate CLI parsing
        let res =
            Args::try_parse_from(["nivra", "dashboard-token", "rotate", "--config", "c.json"]);
        assert!(res.is_ok());
        if let Ok(Args {
            command:
                Command::DashboardToken {
                    command: DashboardTokenCommand::Rotate { config, token },
                },
        }) = res
        {
            assert_eq!(config, PathBuf::from("c.json"));
            assert_eq!(token, None);
        } else {
            panic!("expected DashboardTokenCommand::Rotate");
        }
        let res_tok = Args::try_parse_from([
            "nivra",
            "dashboard-token",
            "rotate",
            "--config",
            "c.json",
            "--token",
            "customToken1234567890abcdef0123456789",
        ]);
        assert!(res_tok.is_ok());
        assert!(Args::try_parse_from(["nivra", "dashboard-token", "rotate"]).is_err());
    }
}
