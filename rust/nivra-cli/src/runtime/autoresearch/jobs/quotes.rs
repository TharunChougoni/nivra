use super::*;
use harness::BacktestArgs;
use nivra_core::autoresearch::{Decision, Score, Verdict};
use nivra_core::finance::Amount;
use nivra_core::finance::FeeArithmetic;
use nivra_store::market_archive::BarSnapshot;
use nivra_store::research_memory::Outcome;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A quote replay's frozen tape window, capital, costs and comparator.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QuoteManifest {
    pub protocol: u32,
    pub abi: String,
    pub dataset_id: String,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
    pub receipt_id: i64,
    pub sessions: Vec<NaiveDate>,
    pub sizing: nivra_core::sleeve::SizingPolicy,
    pub exit_policy: Option<nivra_core::execution::ExitPolicy>,
    pub worker: crate::runtime::strategy::WorkerConfig,
    pub max_drawdown_bps: i64,
    pub translation: Option<Translation>,
    pub comparator_experiment_id: Option<i64>,
    pub capital: Amount,
    pub costs: FeeArithmetic,
    pub comparator: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Translation {
    pub(super) origin_id: i64,
    pub(super) snapshot: BarSnapshot,
}

/// Replay a quote-stage experiment inside the call. An accepted replay
/// registers in the library as every replay does.
pub(in crate::runtime::autoresearch) fn run_quote(
    c: &Config,
    cfg: &PlannerConfig,
    store: &Store,
    experiment: &Experiment,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<(Outcome, Value)> {
    let manifest: QuoteManifest =
        serde_json::from_slice(&store.object(&experiment.manifest_blob)?)?;
    let idea = store
        .idea(experiment.idea_id)?
        .ok_or("research_idea_unknown")?;
    let source = String::from_utf8(store.object(&experiment.source_blob)?)
        .map_err(|_| "research_source_unreadable")?;
    let params: Value = serde_json::from_slice(&store.object(&experiment.params_blob)?)?;
    let Some(slot) = harness::compute_slot(c)? else {
        return Ok((
            Outcome::Preempted {
                reason: "research_compute_busy".into(),
            },
            json!({"reason":"research_compute_busy"}),
        ));
    };
    let parity = if let Some(translation) = &manifest.translation {
        match translation_parity(c, cfg, store, &manifest, translation, preempted) {
            Ok(parity) => Some(parity),
            Err(error) => {
                let reason = crate::runtime::harness::failure_reason(error.as_ref());
                let outcome = if reason == "decision_preempted" {
                    Outcome::Preempted {
                        reason: reason.clone(),
                    }
                } else {
                    Outcome::Invalid {
                        reason: reason.clone(),
                    }
                };
                return Ok((outcome, json!({"reason":reason})));
            }
        }
    } else {
        None
    };
    let comparator = manifest
        .comparator_experiment_id
        .map(|id| store.experiment(id))
        .transpose()?
        .flatten();
    let parent_source = comparator
        .as_ref()
        .map(|e| store.object(&e.source_blob))
        .transpose()?;
    let parent_params: Option<Value> = comparator
        .as_ref()
        .map(|e| -> Result<Value> { Ok(serde_json::from_slice(&store.object(&e.params_blob)?)?) })
        .transpose()?;
    let mut book = c.clone();
    book.sleeves.cash.capital = manifest.capital;
    book.paper_costs = Some(manifest.costs.clone());
    let (_, candidate) = loop_settings(c, cfg)?;
    let author = crate::runtime::strategy_library::Author {
        lane: "research".into(),
        model: Some("autoresearch".into()),
        model_revision: Some(0),
        turn_id: Some(format!("autoresearch-{}", experiment.id)),
        session: Some(c.session),
    };
    let args = BacktestArgs {
        task_key: task_key(experiment),
        hypothesis: idea.hypothesis.chars().take(1000).collect(),
        source,
        params,
        dataset_id: manifest.dataset_id.clone(),
        training_from_ms: manifest.selection_from_ms - 2,
        training_to_ms: manifest.selection_from_ms - 1,
        selection_from_ms: manifest.selection_from_ms,
        selection_to_ms: manifest.selection_to_ms,
        archive_daily_window: None,
        as_of_receipt_id: Some(manifest.receipt_id),
        archive_bars: None,
    };
    let value = harness::strategy_backtest(
        &book,
        cfg,
        &candidate,
        author,
        &args,
        now,
        preempted,
        Some(harness::ReplayGate {
            sessions: &manifest.sessions,
            parent: parent_source.as_deref().zip(parent_params.as_ref()),
            max_drawdown_bps: manifest.max_drawdown_bps,
            sizing: &manifest.sizing,
            exit_policy: &manifest.exit_policy,
            worker: &manifest.worker,
            slot: &slot,
            parity,
        }),
    )?;
    let reason = || {
        value["reason"]
            .as_str()
            .unwrap_or("not_dispatched")
            .to_owned()
    };
    if value["status"] != "completed" {
        let reason = reason();
        return Ok((
            if matches!(
                reason.as_str(),
                "strategy_replay_preempted" | "research_compute_busy"
            ) {
                Outcome::Preempted { reason }
            } else if reason.contains("unavailable") || reason.contains("calendar") {
                Outcome::Failed { reason }
            } else {
                Outcome::Invalid { reason }
            },
            value,
        ));
    }
    let sha = value["report_sha256"]
        .as_str()
        .filter(|sha| nivra_core::autoresearch::sha256_ok(sha))
        .ok_or("strategy_replay_report_unreadable")?;
    let bytes = crate::read_bounded(
        &book
            .directory
            .join("model/strategy-replays")
            .join(format!("{sha}.json")),
        32 * 1024 * 1024,
    )
    .map_err(|_| "strategy_replay_report_unreadable")?;
    if digest(&bytes) != sha {
        return Err("strategy_replay_report_unreadable".into());
    }
    #[derive(Deserialize)]
    struct Summary {
        net_pnl: Amount,
        worst_drawdown: Amount,
        trades: u64,
        open_positions: u64,
    }
    let summary: Summary = serde_json::from_value(value["summary"].clone())
        .map_err(|_| "strategy_replay_report_unreadable")?;
    let drawdown = summary
        .worst_drawdown
        .checked_mul(Amount::from_integer(10_000))
        .and_then(|d| d.checked_floor_ratio(manifest.capital))
        .map_err(|_| "strategy_replay_report_unreadable")?;
    let score = Score {
        net_pnl: summary.net_pnl,
        max_drawdown_bps: i64::try_from(drawdown).unwrap_or(i64::MAX),
        closed_trades: summary.trades,
        open_positions: summary.open_positions,
    };
    let accepted = value["acceptance"]["verdict"] == "accepted";
    let decision = Decision {
        verdict: if accepted {
            Verdict::Keep
        } else {
            Verdict::Reject
        },
        reason: if accepted {
            "quote_replay_accepted"
        } else {
            "quote_replay_rejected"
        },
        delta: serde_json::from_slice::<Value>(&bytes)?["qualification"]["paired_net_pnl"]
            .as_str()
            .map(Amount::parse)
            .transpose()?
            .unwrap_or(Amount::ZERO),
    };
    Ok((
        Outcome::Scored {
            report_blob: store.put_object(&bytes)?,
            score,
            decision,
        },
        value,
    ))
}

fn translation_parity(
    c: &Config,
    cfg: &PlannerConfig,
    store: &Store,
    quote: &QuoteManifest,
    translation: &Translation,
    stop: &dyn Fn() -> bool,
) -> Result<crate::runtime::strategy::replay::TranslationParity> {
    let origin = store
        .experiment(translation.origin_id)?
        .filter(|e| e.status == "scored")
        .ok_or("research_finalist_unavailable")?;
    let mut bar: BarManifest = serde_json::from_slice(&store.object(&origin.manifest_blob)?)?;
    let (first, last) = quote
        .sessions
        .first()
        .zip(quote.sessions.last())
        .ok_or("translation_parity_unavailable")?;
    bar.window.to = *last;
    bar.splits.training_to_ms = ist_midnight(*first) - 1;
    bar.splits.selection_from_ms = ist_midnight(*first);
    bar.splits.selection_to_ms =
        ist_midnight(last.succ_opt().ok_or("translation_parity_unavailable")?) - 1;
    let archive = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let (_, candidate) = loop_settings(c, cfg)?;
    let params = serde_json::from_slice(&store.object(&origin.params_blob)?)?;
    let source = String::from_utf8(store.object(&origin.source_blob)?)?;
    let mut request = harness::bar_request(
        archive,
        bar.window,
        bar.splits,
        bar.capital,
        bar.costs,
        params,
        &candidate,
    )
    .ok_or("translation_request_invalid")?;
    request.sandbox = bar.sandbox;
    request.qualification = Some(nivra_research::archive_bars::Qualification {
        snapshot: translation.snapshot.clone(),
        fold_boundaries_ms: None,
        embargo_sessions: bar.embargo_sessions,
    });
    let mut db = crate::runtime::market_archive::connect(archive)
        .map_err(|_| "archive_connection_failed")?;
    let probes = nivra_research::archive_bars::translation_probes(
        &request,
        &source,
        &mut db,
        Duration::from_millis(candidate.wall_millis.into()).min(Duration::from_secs(300)),
        stop,
    )?;
    Ok(crate::runtime::strategy::replay::TranslationParity::new(
        probes,
    ))
}
