use super::*;
use crate::runtime::paper_replay::{
    ArchiveWindow, ArchiveWindowReport, ExecutionTape, TapeLimits, archive_window_tape,
    execution_tape_bounded_with_stop,
};
use crate::runtime::strategy::{self, replay};
use crate::runtime::strategy_library::{self, Author, Library, acceptance};
use std::os::unix::fs::DirBuilderExt;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// `backtest` over a quote tape: replay the source as a
/// `strategy_actions_v1` strategy over a frozen quote tape with the book's
/// capital, costs and exit policy. The
/// report is kept by content hash under `model/strategy-replays/`.
pub(super) fn strategy_backtest(
    c: &Config,
    cfg: &PlannerConfig,
    candidate: &CandidateComputeSettings,
    author: Author,
    args: &BacktestArgs,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<Value> {
    let started = Instant::now();
    let Some(settings) = candidate.strategy.as_ref().filter(|s| s.enabled) else {
        return Ok(not_dispatched("candidate_strategy_disabled"));
    };
    settings.validate()?;
    let dataset = candidate
        .datasets
        .iter()
        .find(|d| d.dataset_id == args.dataset_id);
    let window = match dataset.map(|d| &d.kind) {
        Some(CandidateDatasetKind::ArchiveWindow {
            from_session,
            to_session,
        }) => {
            if args.selection_to_ms > now {
                return Ok(not_dispatched("candidate_archive_window_in_future"));
            }
            Some(ArchiveWindow {
                max_days: settings.max_archive_window_days,
                from_session: *from_session,
                to_session: *to_session,
                from_ms: args.selection_from_ms,
                to_ms: args.selection_to_ms,
                daily: args.archive_daily_window,
                receipt_id: args.as_of_receipt_id,
            })
        }
        Some(CandidateDatasetKind::CurrentSessionFeatures)
            if args.archive_daily_window.is_none() && args.as_of_receipt_id.is_none() =>
        {
            None
        }
        _ => return Ok(not_dispatched("candidate_strategy_dataset_unsupported")),
    };
    let Some(costs) = c.paper_costs.clone() else {
        return Ok(not_dispatched("paper_costs_required"));
    };
    let capital = c.allocation()?;
    let budget = Duration::from_millis(settings.max_wall_ms);
    let stop = || {
        if preempted() {
            Some("strategy_replay_preempted")
        } else if started.elapsed() >= budget {
            Some("strategy_replay_wall_budget")
        } else {
            None
        }
    };
    let limits = TapeLimits {
        max_quote_events: settings.max_quote_events,
        max_observations: settings.max_observations,
        max_tape_bytes: settings.max_tape_bytes,
    };
    let (tape, archive) = match build_tape(c.clone(), window, capital, limits, now, &stop) {
        Ok(result) => result,
        Err(reason)
            if matches!(
                reason.as_str(),
                "strategy_replay_preempted" | "strategy_replay_wall_budget"
            ) =>
        {
            return Ok(stopped_replay(&reason, 0, None));
        }
        Err(reason) => return Ok(not_dispatched(&reason)),
    };
    let Some(tape) = tape else {
        return Ok(json!({
            "status":"not_dispatched",
            "reason":"strategy_replay_quotes_unavailable",
            "tape":{"archive_window":archive}
        }));
    };
    let parameters = replay::Parameters {
        version: 1,
        capital: Some(capital),
        costs: Some(costs),
        exit_policy: cfg.exit_policy.clone(),
        params: args.params.clone(),
        top_n: 20,
        from_ms: Some(args.selection_from_ms),
        to_ms: Some(args.selection_to_ms),
    };
    let sandbox = cfg.effective_sandbox(&candidate.sandbox);
    let mut report = match replay::run(
        &sandbox,
        args.source.as_bytes(),
        &parameters,
        &tape.manifest,
        &tape.events,
        &stop,
    ) {
        Ok(Ok(report)) => report,
        // No report is kept for a partial replay.
        Ok(Err(stopped)) => {
            return Ok(stopped_replay(
                stopped.reason,
                stopped.strategy_calls,
                stopped.last_at_ms,
            ));
        }
        Err(error) => {
            return Ok(not_dispatched(&crate::runtime::failure_code(
                error.as_ref(),
            )));
        }
    };
    if let Some(archive) = archive {
        report["tape"]["archive_window"] = json!(archive);
        report["tape"]["events_sha256"] = json!(tape.events_sha256);
    }
    let bytes = nivra_cli::canonical_json::bytes(&report)?;
    let report_sha256 = digest(&bytes);
    let directory = c.directory.join("model").join("strategy-replays");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    // Content-addressed: an existing file already holds these exact bytes.
    let name = format!("{report_sha256}.json");
    strategy_library::write_new(&directory, &name, &bytes)?;
    // Acceptance reads the stored bytes, so a damaged file cannot register.
    let stored = crate::read_bounded(&directory.join(&name), 32 * 1024 * 1024)
        .map_err(|_| "strategy_replay_report_unreadable")?;
    let acceptance = accept_and_register(
        c,
        cfg,
        &stored,
        &report_sha256,
        args.source.as_bytes(),
        author,
        now,
    );
    Ok(json!({
        "status":"completed",
        "abi":strategy::ABI,
        "evidence":"quote_tape",
        "task_key":args.task_key,
        "hypothesis":args.hypothesis,
        "report_sha256":report_sha256,
        "source_sha256":report["source_sha256"],
        "env_sha256":report["sandbox"]["env_sha256"],
        "summary":report["summary"],
        "tape":report["tape"],
        "action_outcomes":report["action_outcomes"],
        "strategy_failures":report["strategy_failures"],
        "trades":report["trades"].as_array().map(|t| t.iter().take(50).collect::<Vec<_>>()),
        "acceptance":acceptance,
        "paper_only":true
    }))
}

fn stopped_replay(reason: &str, strategy_calls: usize, last_at_ms: Option<i64>) -> Value {
    json!({
        "status":"not_dispatched",
        "reason":reason,
        "strategy_calls":strategy_calls,
        "last_at_ms":last_at_ms,
        "next_condition":"retry_with_a_narrower_selection_window"
    })
}

type BuiltTape = std::result::Result<(Option<ExecutionTape>, Option<ArchiveWindowReport>), String>;

fn build_tape(
    c: Config,
    window: Option<ArchiveWindow>,
    capital: Amount,
    limits: TapeLimits,
    now: i64,
    stop: &dyn Fn() -> Option<&'static str>,
) -> BuiltTape {
    if let Some(reason) = stop() {
        return Err(reason.into());
    }
    // A synchronous archive query can be waiting on I/O while a decision
    // turn arrives. Release the lane promptly and let only the in-flight
    // operation finish under the archive connection's timeouts.
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = cancel.clone();
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("strategy-tape".into())
        .spawn(move || {
            let stop = || {
                worker_cancel
                    .load(Ordering::Relaxed)
                    .then_some("strategy_replay_cancelled")
            };
            let result = match window {
                Some(window) => archive_window_tape(&c, &window, capital, limits, &stop)
                    .map(|(tape, archive)| (tape, Some(archive))),
                None => execution_tape_bounded_with_stop(
                    c.quote_source(),
                    c.session,
                    previous_trading_session(&c),
                    capital,
                    limits,
                    now,
                    &stop,
                )
                .map(|tape| (Some(tape), None)),
            };
            let _ = send.send(result);
        })
        .map_err(|_| "strategy_replay_tape_worker_failed")?;
    loop {
        if let Some(reason) = stop() {
            cancel.store(true, Ordering::Relaxed);
            return Err(reason.into());
        }
        match receive.recv_timeout(Duration::from_millis(10)) {
            Ok(result) => return stop().map_or(result, |reason| Err(reason.into())),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("strategy_replay_tape_worker_failed".into());
            }
        }
    }
}

/// Run the acceptance check and register an accepted strategy. The verdict
/// goes back to the model; a registration failure is reported, never hidden.
fn accept_and_register(
    c: &Config,
    cfg: &PlannerConfig,
    report: &[u8],
    report_sha256: &str,
    source: &[u8],
    author: Author,
    now: i64,
) -> Value {
    let thresholds = cfg.strategy_acceptance.clone().unwrap_or_default();
    let accepted = match acceptance::accept(report, report_sha256, source, &thresholds) {
        Ok(accepted) => accepted,
        Err(reasons) => {
            return json!({"verdict":"rejected","reasons":reasons,"thresholds":thresholds});
        }
    };
    let registered = strategy_library::root(c)
        .map(Library::at)
        .and_then(|library| library.register(&accepted, author, now));
    match registered {
        Ok(registration) => json!({
            "verdict":"accepted",
            "strategy_id":registration.entry.strategy_id,
            "registration":if registration.created { "registered" } else { "already_registered" },
            "thresholds":thresholds
        }),
        Err(error) => json!({
            "verdict":"accepted",
            "registration":"failed",
            "reason":crate::runtime::failure_code(error.as_ref()),
            "thresholds":thresholds
        }),
    }
}
