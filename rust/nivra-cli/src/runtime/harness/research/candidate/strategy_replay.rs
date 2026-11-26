use super::*;
use crate::runtime::strategy::{self, replay};
use crate::runtime::strategy_library::{self, Author, Library, acceptance};
use std::os::unix::fs::DirBuilderExt;

/// `backtest` with `strategy: true`: replay the source as a
/// `strategy_actions_v1` strategy over the selection window of this session's
/// frozen quote tape, with the book's capital, costs and exit policy. The
/// report is kept by content hash under `model/strategy-replays/`.
pub(super) fn strategy_backtest(
    c: &Config,
    cfg: &PlannerConfig,
    candidate: &CandidateComputeSettings,
    author: Author,
    args: &CandidateSubmitArgs,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<Value> {
    let Some(settings) = candidate.strategy.as_ref().filter(|s| s.enabled) else {
        return Ok(not_dispatched("candidate_strategy_disabled"));
    };
    settings.validate()?;
    if args.execution.is_some() || args.parent_ref.is_some() {
        return Ok(not_dispatched("candidate_strategy_args_conflict"));
    }
    if !candidate.datasets.iter().any(|dataset| {
        dataset.dataset_id == args.dataset_id
            && matches!(dataset.kind, CandidateDatasetKind::CurrentSessionFeatures)
    }) {
        return Ok(not_dispatched("candidate_strategy_dataset_unsupported"));
    }
    let Some(costs) = c.paper_costs.clone() else {
        return Ok(not_dispatched("paper_costs_required"));
    };
    let capital = c.allocation()?;
    let tape = match crate::runtime::paper_replay::execution_tape_bounded(
        &c.directory,
        c.session,
        candidate_previous_trading_session(c),
        capital,
        settings.max_quote_events,
        settings.max_observations,
        settings.max_tape_bytes,
        now,
    ) {
        Ok(tape) => tape,
        Err(reason) => return Ok(not_dispatched(&reason)),
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
    let started = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(settings.max_wall_ms);
    let stop = || {
        if preempted() {
            Some("strategy_replay_preempted")
        } else if started.elapsed() >= budget {
            Some("strategy_replay_wall_budget")
        } else {
            None
        }
    };
    let report = match replay::run(
        &candidate.sandbox,
        args.source.as_bytes(),
        &parameters,
        &tape.manifest,
        &tape.events,
        &stop,
    ) {
        Ok(Ok(report)) => report,
        // No report is kept for a partial replay.
        Ok(Err(stopped)) => {
            return Ok(json!({
                "status":"not_dispatched",
                "reason":stopped.reason,
                "strategy_calls":stopped.strategy_calls,
                "last_at_ms":stopped.last_at_ms,
                "next_condition":"retry_with_a_narrower_selection_window"
            }));
        }
        Err(error) => {
            return Ok(not_dispatched(&crate::runtime::failure_code(
                error.as_ref(),
            )));
        }
    };
    let bytes = nivra_cli::candidate_execution::artifact_bytes(&report)?;
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
        "task_key":args.task_key,
        "hypothesis":args.hypothesis,
        "report_sha256":report_sha256,
        "source_sha256":report["source_sha256"],
        "summary":report["summary"],
        "tape":report["tape"],
        "action_outcomes":report["action_outcomes"],
        "strategy_failures":report["strategy_failures"],
        "trades":report["trades"].as_array().map(|t| t.iter().take(50).collect::<Vec<_>>()),
        "acceptance":acceptance,
        "paper_only":true
    }))
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
