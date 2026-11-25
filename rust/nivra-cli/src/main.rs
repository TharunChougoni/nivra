#![recursion_limit = "512"]
mod alert;
mod backtest;
mod preflight;
mod research;
mod runtime;
mod update;
use clap::{Parser, Subcommand};
use nivra_core::protection_gap::protection_gap_cause;
use nivra_core::research_acceptance::{EvidenceRef, Review, evaluate_novel};
use nivra_core::resources::{MemoryConclusion, MemoryEvidence, assess_memory};
use nivra_core::service_review::{render_loopback_tunnel, review_unit};
use std::{
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

const MAX_EVIDENCE_BYTES: u64 = 16 * 1024;

#[derive(Parser)]
#[command(version, about = "Nivra Rust paper-only runner")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Prepare a fresh current-policy paper installation; never starts trading.
    Setup(runtime::setup::Args),
    /// Compare recorded rankings offline in the bounded Python sandbox.
    ScorerCompare(nivra_cli::scorer_compare::Args),
    /// Offline pipeline inspection, replay and policy-gated notes.
    Orchestrator {
        #[arg(long)]
        workspace: PathBuf,
        /// Offline OrchestratorConfig JSON, independent of runtime configuration.
        #[arg(long)]
        config: Option<PathBuf>,
        #[command(subcommand)]
        command: OrchestratorCommand,
    },
    /// Offline scores-only Python/WASM strategy sandbox; disabled by default.
    StrategyScorer {
        #[command(subcommand)]
        command: nivra_cli::strategy_scorer::Commands,
    },
    /// Offline strategy proposals, recorded-data evaluation and read-only evidence.
    StrategyLab {
        #[command(subcommand)]
        command: nivra_cli::strategy_lab::Commands,
    },
    /// Offline experiment lifecycle: admit, ingest workspace scores, and persist transitions.
    ExperimentLifecycle {
        #[arg(long)]
        book: PathBuf,
        #[arg(long)]
        request: PathBuf,
        /// New report file; existing reports are never replaced.
        #[arg(long)]
        output: PathBuf,
    },
    /// Opt-in model planner preview; never trades.
    Planner {
        #[command(subcommand)]
        command: runtime::planner::Command,
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
    /// Exploratory offline daily-OHLC exit sensitivity; never starts trading.
    ExitBacktest {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        history: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        start: Option<chrono::NaiveDate>,
        #[arg(long)]
        end: Option<chrono::NaiveDate>,
    },
    /// Print the engine fingerprint required in archived replay manifests.
    BacktestFingerprint,
    /// Read-only Upstox candle evidence download; never starts trading.
    HistoryDownload {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long)]
        start: Option<chrono::NaiveDate>,
        #[arg(long)]
        end: Option<chrono::NaiveDate>,
        #[arg(long, value_enum, default_value = "both")]
        interval: runtime::history_download::Interval,
        /// Acquire validated provider receipts without rebuilding replay shards.
        #[arg(long)]
        receipts_only: bool,
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
    /// Summarize an explicitly selected saved timing stream, without session actions.
    ProtectionTimings { path: PathBuf },
    /// Inspect original timing streams without claiming financial or process health.
    ProtectionDiagnostics {
        protection: PathBuf,
        #[arg(long, alias = "supervision-timings")]
        supervision: Option<PathBuf>,
    },
    /// Verify and install an annotated stable paper release; explicit selection also permits rollback.
    Update {
        /// Exact stable release, with or without v; an explicit older tag is rollback.
        #[arg(long, conflicts_with = "sha")]
        tag: Option<String>,
        /// Released source commit or unique prefix (at least seven hex characters).
        #[arg(long, conflicts_with = "tag")]
        sha: Option<String>,
        #[arg(long)]
        check: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        now: Option<chrono::DateTime<chrono::FixedOffset>>,
        #[arg(long)]
        config: PathBuf,
        /// Deploy a selected release to an operator-stopped or closed flat book,
        /// even with carried holdings, a planner halt, or inside the blackout. The
        /// controller is left stopped; clear a closed halt only after the install.
        #[arg(long, requires = "confirm_paper_review")]
        operator_stopped: bool,
        /// Accept a bounded unwatched controller window while the reviewed paper book
        /// is open inside or outside the session. Never bypasses halt, hold or integrity.
        #[arg(long, conflicts_with = "operator_stopped")]
        force_unwatched_window: bool,
        #[arg(long, requires = "operator_stopped")]
        confirm_paper_review: bool,
    },
    /// Hidden fixed candidate helper. Runs only inside an owned systemd unit.
    #[command(hide = true)]
    CandidateSupervise { launch: PathBuf },
    /// Offline pi agent check: one decision turn, its research request and the
    /// next decision turn against the scripted fake provider; never trades.
    AgentCheck(runtime::agent::check::Args),
    /// Inspect the brief or run one shadow pi turn against the current book.
    AgentTurn(runtime::agent::turn_cli::Args),
    /// Summarize recorded pi shadow decisions and the sealed paper session.
    AgentShadowReport(runtime::agent::shadow_report::Args),
    /// Quick offline binary/storage self-check; never opens a session.
    SelfCheck {
        #[arg(long)]
        expected_version: Option<String>,
    },
    /// Calculate budget fit from explicit non-secret JSON evidence. Does not measure a host.
    MemoryCheck { evidence: PathBuf },
    /// Paper-only daily controller and authenticated loopback dashboard.
    Paper {
        #[arg(long)]
        config: PathBuf,
    },
    /// Offline rollover preview, reviewed advance or explicit human acknowledgment; never starts trading.
    #[command(group(clap::ArgGroup::new("timed_rollover").args(["dry_run", "advance"])))]
    #[command(group(clap::ArgGroup::new("reviewed_rollover").args(["acknowledge_closed_book", "accept_stopped_carry", "accept_halted_carry", "advance"])))]
    Rollover {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, requires = "timed_rollover")]
        now: Option<chrono::DateTime<chrono::FixedOffset>>,
        #[arg(
            long,
            required_unless_present_any = ["acknowledge_closed_book", "accept_stopped_carry", "accept_halted_carry", "advance"],
            requires = "now",
            conflicts_with_all = ["acknowledge_closed_book", "accept_stopped_carry", "accept_halted_carry"]
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
    /// Record supplied measurements. The cause stays UNKNOWN and the gate stays at two seconds.
    ProtectionGap {
        #[arg(long, allow_hyphen_values = true)]
        receipt_age: Option<f64>,
        #[arg(long, allow_hyphen_values = true)]
        scheduler_wait: Option<f64>,
        #[arg(long, allow_hyphen_values = true)]
        dropped: Option<i64>,
    },
    /// Review one unit file. Credentials are not printed and nothing is installed.
    ServiceReview { unit: PathBuf },
    /// Print review-only forwarder text. It is not installed or started.
    LoopbackTunnelText { port: i64, hostname: String },
    /// Check novel-strategy evidence. This does not certify or execute code.
    NovelEvaluate {
        strategy: String,
        evidence: PathBuf,
        #[arg(long, default_value = "UNREVIEWED")]
        review: String,
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

#[derive(Subcommand)]
enum OrchestratorCommand {
    /// Print pipeline JSON including proposals.
    View,
    /// Replay recorded feature snapshots; optionally append library results.
    Replay {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        snapshots: PathBuf,
        /// JSON array of regime inputs in snapshot order.
        #[arg(long, requires = "library")]
        regime_inputs: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, requires_all = ["strategy_id", "version"])]
        library: Option<PathBuf>,
        #[arg(long, requires = "library", value_parser = clap::value_parser!(i64).range(1..))]
        strategy_id: Option<i64>,
        #[arg(long, requires = "library", value_parser = clap::value_parser!(i64).range(1..))]
        version: Option<i64>,
    },
    /// Write an own-run note under the configured policy roots.
    Note {
        #[arg(long)]
        path: PathBuf,
        #[arg(long)]
        text: String,
    },
}

fn orchestrator_handle(
    workspace: &Path,
    config: Option<&Path>,
    command: OrchestratorCommand,
) -> Result<serde_json::Value, ()> {
    use nivra_cli::orchestrator::{Orchestrator, OrchestratorConfig, ReplayRecording};
    let config: OrchestratorConfig = match config {
        Some(path) => serde_json::from_slice(&read_bounded(path, 1_048_576)?).map_err(|_| ())?,
        None => OrchestratorConfig::default(),
    };
    let orchestrator = Orchestrator::new(workspace, config).map_err(|_| ())?;
    match command {
        OrchestratorCommand::View => {
            serde_json::to_value(orchestrator.pipeline_view().map_err(|_| ())?).map_err(|_| ())
        }
        OrchestratorCommand::Replay {
            spec,
            snapshots,
            regime_inputs,
            out,
            library,
            strategy_id,
            version,
        } => {
            let report = if let Some(library) = library {
                let snapshots_data: Vec<nivra_core::strategy::FeatureSnapshot> =
                    serde_json::from_slice(
                        &std::fs::read(workspace.join(&snapshots)).map_err(|_| ())?,
                    )
                    .map_err(|_| ())?;
                let inputs = if let Some(path) = regime_inputs {
                    serde_json::from_slice(&std::fs::read(workspace.join(path)).map_err(|_| ())?)
                        .map_err(|_| ())?
                } else {
                    // Legacy snapshots contain no aligned market-regime evidence.
                    snapshots_data
                        .iter()
                        .map(|_| nivra_core::regime::RegimeInputs {
                            session_return: 0.0,
                            opening_gap: 0.0,
                            realised_volatility: 0.0,
                            breadth: 0.0,
                            instrument_count: 0,
                            minute_count: 0,
                        })
                        .collect::<Vec<_>>()
                };
                orchestrator.run_replay_recorded(
                    spec,
                    snapshots,
                    out,
                    ReplayRecording {
                        library_path: &library,
                        strategy_id: strategy_id.ok_or(())?,
                        version: version.ok_or(())?,
                        inputs: &inputs,
                        recorded_at_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_err(|_| ())?
                            .as_millis()
                            .try_into()
                            .map_err(|_| ())?,
                    },
                )
            } else {
                if strategy_id.is_some() || version.is_some() || regime_inputs.is_some() {
                    return Err(());
                }
                orchestrator.run_replay(spec, snapshots, out)
            }
            .map_err(|_| ())?;
            serde_json::to_value(report).map_err(|_| ())
        }
        OrchestratorCommand::Note { path, text } => {
            orchestrator.write_note(path, &text).map_err(|_| ())?;
            Ok(serde_json::json!({"note_written": true}))
        }
    }
}

fn main() -> ExitCode {
    let Args { command } = Args::parse();
    match command {
        Command::Setup(args) => runtime::setup::entry(args),
        Command::Orchestrator {
            workspace,
            config,
            command,
        } => diagnostic_output(
            orchestrator_handle(&workspace, config.as_deref(), command),
            "orchestrator_failed: check offline configuration, policy and recorded evidence",
        ),
        Command::ScorerCompare(args) => match nivra_cli::scorer_compare::run(args) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("scorer_compare_failed: {error}");
                ExitCode::FAILURE
            }
        },
        Command::StrategyScorer { command } => match nivra_cli::strategy_scorer::run(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => {
                eprintln!("strategy_scorer_failed: check configuration and recorded evidence");
                ExitCode::FAILURE
            }
        },
        Command::StrategyLab { command } => match nivra_cli::strategy_lab::run(command) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => {
                eprintln!("strategy_lab_failed: check configuration and recorded evidence");
                ExitCode::FAILURE
            }
        },
        Command::ExperimentLifecycle {
            book,
            request,
            output,
        } => match runtime::experiment_ledger::lifecycle::entry(&book, &request, &output) {
            Ok(report) => {
                println!("{report}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("experiment_lifecycle_refused: {error}");
                ExitCode::FAILURE
            }
        },
        Command::Planner { command } => runtime::planner::entry(command),
        Command::Alert {
            config,
            test,
            event,
        } => alert::entry(config.as_deref(), test, event),
        Command::Preflight { config, unit_file } => preflight::entry(&config, unit_file.as_deref()),
        Command::Backtest { manifest, events } => backtest::entry(&manifest, &events),
        Command::HistoryBackfill {
            config,
            check_only,
            prepare_universe,
        } => runtime::history_download::backfill_entry(&config, check_only, prepare_universe),
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
            output,
            start,
            end,
            interval,
            receipts_only,
        } => {
            runtime::history_download::entry(&config, &output, start, end, interval, receipts_only)
        }
        Command::ExitBacktest {
            config,
            history,
            output,
            start,
            end,
        } => runtime::exit_backtest::entry(&history, &output, start, end, config.as_deref()),
        Command::BacktestFingerprint => {
            println!("{}", nivra_core::replay::engine_fingerprint());
            ExitCode::SUCCESS
        }
        Command::ProtectionTimings { path } => diagnostic_output(
            nivra_store::timing_journal::summarize_timings(&path).map_err(|_| ()),
            "Protection timing summary unavailable",
        ),
        Command::ProtectionDiagnostics {
            protection,
            supervision,
        } => diagnostic_output(
            nivra_store::protection_diagnostics::build(&protection, supervision.as_deref())
                .map_err(|_| ()),
            "Protection diagnostics unavailable",
        ),
        Command::Update {
            config,
            check,
            dry_run,
            now,
            tag,
            sha,
            operator_stopped,
            force_unwatched_window,
            confirm_paper_review: _,
        } => update::entry(
            &config,
            check,
            dry_run,
            now,
            tag.as_deref(),
            sha.as_deref(),
            operator_stopped,
            force_unwatched_window,
        ),
        Command::CandidateSupervise { launch } => {
            match runtime::harness::research_candidate_helper(&launch) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("candidate helper failed: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::AgentCheck(args) => runtime::agent::check::run(args),
        Command::AgentTurn(args) => runtime::agent::turn_cli::run(args),
        Command::AgentShadowReport(args) => runtime::agent::shadow_report::run(args),
        Command::SelfCheck { expected_version } => self_check(expected_version.as_deref()),
        Command::MemoryCheck { evidence } => memory_check(&evidence),
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
            confirm_paper_review,
        } => {
            if acknowledge_closed_book {
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
        Command::ProtectionGap {
            receipt_age,
            scheduler_wait,
            dropped,
        } => protection_gap(receipt_age, scheduler_wait, dropped),
        Command::ServiceReview { unit } => service_review(&unit),
        Command::LoopbackTunnelText { port, hostname } => loopback_tunnel(port, &hostname),
        Command::NovelEvaluate {
            strategy,
            evidence,
            review,
        } => novel_evaluate(&strategy, &evidence, &review),
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

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ()> {
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

fn fixed_error(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::from(2)
}

fn protection_gap(
    receipt_age: Option<f64>,
    scheduler_wait: Option<f64>,
    dropped: Option<i64>,
) -> ExitCode {
    let dropped = match dropped {
        Some(value) if (0..=1_000_000).contains(&value) => Some(value as u32),
        Some(_) => return fixed_error("Protection gap cause unavailable"),
        None => None,
    };
    match protection_gap_cause(receipt_age, scheduler_wait, dropped) {
        Ok(cause) => match serde_json::to_string(&cause) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(_) => fixed_error("Protection gap cause unavailable"),
        },
        Err(_) => fixed_error("Protection gap cause unavailable"),
    }
}

fn service_review(path: &Path) -> ExitCode {
    let review = (|| {
        let bytes = read_bounded(path, 80_000)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
        review_unit(text).map_err(|_| ())
    })();
    match review {
        Ok(review) => match serde_json::to_string(&review) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(_) => fixed_error("Service review unavailable"),
        },
        Err(()) => fixed_error("Service review unavailable"),
    }
}

fn loopback_tunnel(port: i64, hostname: &str) -> ExitCode {
    match render_loopback_tunnel(port, hostname) {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(_) => fixed_error("Service review unavailable"),
    }
}

fn novel_evaluate(strategy: &str, path: &Path, review: &str) -> ExitCode {
    let review = match review {
        "UNREVIEWED" => Review::Unreviewed,
        "ACCEPTED" => Review::Accepted,
        "REJECTED" => Review::Rejected,
        _ => return fixed_error("Novel evaluation unavailable"),
    };
    let result = (|| {
        let bytes = read_bounded(path, 65_536)?;
        let rows: Vec<Box<serde_json::value::RawValue>> =
            serde_json::from_slice(&bytes).map_err(|_| ())?;
        let mut evidence = Vec::new();
        for row in rows {
            evidence.push(EvidenceRef::parse(row.get().as_bytes()).map_err(|_| ())?);
        }
        evaluate_novel(strategy, &evidence, review).map_err(|_| ())
    })();
    match result {
        Ok(evaluation) => match std::str::from_utf8(evaluation.canonical_json()) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(_) => fixed_error("Novel evaluation unavailable"),
        },
        Err(()) => fixed_error("Novel evaluation unavailable"),
    }
}

fn diagnostic_output(result: Result<serde_json::Value, ()>, error: &'static str) -> ExitCode {
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(()) => {
            eprintln!("{error}");
            ExitCode::from(2)
        }
    }
}

fn memory_check(path: &std::path::Path) -> ExitCode {
    let report = (|| {
        // Read only an explicitly supplied public fixture/evidence file; never discover stores.
        // Bound allocation even if a file grows after opening. Do not echo paths or file contents.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
            .map_err(|_| "memory_evidence_unreadable")?;
        if !file
            .metadata()
            .map_err(|_| "memory_evidence_unreadable")?
            .is_file()
        {
            return Err("memory_evidence_not_regular_file");
        }
        let mut bytes = Vec::new();
        file.take(MAX_EVIDENCE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "memory_evidence_unreadable")?;
        if bytes.len() as u64 > MAX_EVIDENCE_BYTES {
            return Err("memory_evidence_too_large");
        }
        let input: MemoryEvidence =
            serde_json::from_slice(&bytes).map_err(|_| "invalid_memory_evidence")?;
        assess_memory(&input).map_err(|error| match error {
            nivra_core::resources::MemoryError::UnsupportedSchema => "unsupported_memory_schema",
            nivra_core::resources::MemoryError::InvalidCapacity => "invalid_memory_capacity",
            nivra_core::resources::MemoryError::ArithmeticOverflow => "memory_arithmetic_overflow",
        })
    })();
    match report {
        Ok(report) => {
            let exit = if report.conclusion == MemoryConclusion::FitsDeclaredBudget {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(2)
            };
            match serde_json::to_string(&report) {
                Ok(json) => println!("{json}"),
                Err(_) => {
                    eprintln!("memory_report_unavailable");
                    return ExitCode::FAILURE;
                }
            }
            exit
        }
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
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
mod orchestrator_tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(
            ["nivra", "orchestrator"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    }

    #[test]
    fn orchestrator_cli_parse() {
        for args in [
            vec!["--workspace", "root", "view"],
            vec![
                "--workspace",
                "root",
                "--config",
                "offline.json",
                "note",
                "--path",
                "orchestrator_notes/n.txt",
                "--text",
                "review",
            ],
            vec![
                "--workspace",
                "root",
                "replay",
                "--spec",
                "s.json",
                "--snapshots",
                "f.json",
                "--out",
                "proposals/r.json",
            ],
        ] {
            assert!(parse(&args).is_ok());
        }
        assert!(parse(&["view"]).is_err());
        assert!(parse(&["--workspace", "root", "note", "--path", "n"]).is_err());
        let base = [
            "--workspace",
            "root",
            "replay",
            "--spec",
            "s",
            "--snapshots",
            "f",
            "--out",
            "o",
        ];
        for extra in [
            vec!["--regime-inputs", "r.json"],
            vec!["--library", "l"],
            vec!["--strategy-id", "1"],
            vec!["--version", "1"],
            vec!["--library", "l", "--strategy-id", "0", "--version", "1"],
        ] {
            assert!(parse(&[base.as_slice(), &extra].concat()).is_err());
        }
        assert!(
            parse(
                &[
                    base.as_slice(),
                    &[
                        "--library",
                        "l",
                        "--strategy-id",
                        "1",
                        "--version",
                        "2",
                        "--regime-inputs",
                        "r.json"
                    ]
                ]
                .concat()
            )
            .is_ok()
        );
    }

    #[test]
    fn orchestrator_handlers_offline_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("spec.json"),
            serde_json::to_vec(&nivra_core::strategy::StrategySpec::default()).unwrap(),
        )
        .unwrap();
        std::fs::write(
            root.join("snapshots.json"),
            br#"[{"observed_ms":1000,"lookback_samples":20,"rows":[]}]"#,
        )
        .unwrap();
        let config = root.join("offline.json");
        std::fs::write(
            &config,
            br#"{"policy":{"proposals_dir":"ideas","notes_dir":"notes"},"max_proposals":1}"#,
        )
        .unwrap();
        let run = |command| orchestrator_handle(root, Some(&config), command);
        let replay = |library| OrchestratorCommand::Replay {
            spec: "spec.json".into(),
            snapshots: "snapshots.json".into(),
            regime_inputs: None,
            out: "ideas/report.json".into(),
            library,
            strategy_id: Some(1),
            version: Some(1),
        };
        let mut plain = replay(None);
        if let OrchestratorCommand::Replay {
            strategy_id,
            version,
            ..
        } = &mut plain
        {
            *strategy_id = None;
            *version = None;
        }
        let report = run(plain).unwrap();
        assert_eq!(report["ticks"], 1);
        let stored: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("ideas/report.json")).unwrap())
                .unwrap();
        assert_eq!(stored, report);
        assert_eq!(
            run(OrchestratorCommand::View).unwrap()["proposals"][0]["path"],
            "ideas/report.json"
        );
        run(OrchestratorCommand::Note {
            path: "notes/review.txt".into(),
            text: "offline review".into(),
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("notes/review.txt")).unwrap(),
            "offline review"
        );
        run(replay(Some("ideas/library.sqlite".into()))).unwrap();
        assert!(root.join("ideas/library.sqlite").is_file());
        // A measured mark is attributed using the selection snapshot's evidence.
        std::fs::write(
            root.join("spec.json"),
            br#"{
            "id":"fixture", "version":1, "cooldown_ms":0,
            "filters":[], "weights":[{"feature":"move_bps","weight":1.0}],
            "normalization":"none", "shortlist_size":1, "trigger_score":0.0
        }"#,
        )
        .unwrap();
        std::fs::write(root.join("snapshots.json"), br#"[
            {"observed_ms":1000,"lookback_samples":20,"rows":[{"key":"fixture","features":{"price":100.0,"move_bps":10.0}}]},
            {"observed_ms":2000,"lookback_samples":20,"rows":[{"key":"fixture","features":{"price":101.0,"move_bps":10.0}}]}
        ]"#).unwrap();
        std::fs::write(
            &config,
            br#"{"policy":{"proposals_dir":"ideas","notes_dir":"notes"},
            "replay":{"horizon_ms":1000,"max_mark_delay_ms":0,
            "costs":{"notional":"5000","round_trip_spread_bps":10,"fees":{"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"}}}}"#,
        )
        .unwrap();
        let mut library =
            nivra_sandbox::library::Library::open(root.join("ideas/library.sqlite")).unwrap();
        library
            .add_version(nivra_sandbox::library::NewVersion {
                name: "fixture",
                file_body: "{}",
                author_model: "fixture",
                intent: "offline",
                inputs: &serde_json::json!({}),
                created_at_ms: 1,
            })
            .unwrap();
        drop(library);
        let recorded = || {
            let mut command = replay(Some("ideas/library.sqlite".into()));
            if let OrchestratorCommand::Replay {
                regime_inputs, out, ..
            } = &mut command
            {
                *regime_inputs = Some("regimes.json".into());
                *out = "ideas/classified/report.json".into();
            }
            command
        };
        let before = std::fs::read(root.join("ideas/library.sqlite")).unwrap();
        std::fs::write(root.join("regimes.json"), b"[]").unwrap();
        assert!(run(recorded()).is_err());
        assert!(!root.join("ideas/classified").exists());
        assert_eq!(
            std::fs::read(root.join("ideas/library.sqlite")).unwrap(),
            before
        );
        let inputs = vec![
            nivra_core::regime::RegimeInputs {
                session_return: 0.01,
                opening_gap: 0.0,
                realised_volatility: 0.001,
                breadth: 0.7,
                instrument_count: 200,
                minute_count: 15,
            };
            2
        ];
        std::fs::write(
            root.join("regimes.json"),
            serde_json::to_vec(&inputs).unwrap(),
        )
        .unwrap();
        run(recorded()).unwrap();
        let rows =
            nivra_sandbox::library::Library::open_read_only(root.join("ideas/library.sqlite"))
                .unwrap()
                .regime_performance()
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].regime.starts_with("up_trend"));
        assert!(!rows[0].regime.contains("unknown"));
        assert_eq!(rows[0].sample_count, 1);

        assert!(
            run(OrchestratorCommand::Note {
                path: "denied/n.txt".into(),
                text: "rejected".into()
            })
            .is_err()
        );
        assert!(!root.join("denied").exists());
        std::fs::write(&config, br#"{"runtime":true}"#).unwrap();
        assert!(run(OrchestratorCommand::View).is_err());
    }
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
