//! The research lane's `backtest` tool. A strategy either replays a recorded
//! quote tape after costs inside the tool call (the current session or an
//! `archive_window` dataset), or runs over archived bars as a queued research
//! job (`archive_bars`). Nothing here writes the paper book.
use super::*;
use nivra_core::finance::Amount;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
mod action_bench;
mod archive_bars;
mod recent_archive;
mod strategy_replay;
pub(crate) use archive_bars::{ARCHIVE_BARS_DATASET_ID, bar_worker_run};
pub(crate) use recent_archive::{RECENT_ARCHIVE_DATASET_ID, RecentArchiveWindow};

fn deserialize_params<'de, D>(deserializer: D) -> std::result::Result<Value, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // The strict tool schema carries params as a JSON-object string.
    let text = String::deserialize(deserializer)?;
    let value: Value = serde_json::from_str(&text).map_err(serde::de::Error::custom)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(
            "backtest params must be a JSON object string",
        ))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BacktestArgs {
    pub task_key: String,
    pub hypothesis: String,
    pub source: String,
    #[serde(deserialize_with = "deserialize_params")]
    pub params: Value,
    pub dataset_id: String,
    pub training_from_ms: i64,
    pub training_to_ms: i64,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
    #[serde(default)]
    pub archive_daily_window: Option<crate::runtime::paper_replay::DailyWindow>,
    #[serde(default)]
    pub as_of_receipt_id: Option<i64>,
    #[serde(default)]
    pub archive_bars: Option<Box<nivra_store::market_archive::BarWindow>>,
}

fn not_dispatched(reason: &str) -> Value {
    json!({"status":"not_dispatched","reason":reason})
}

/// The archive manifest's `previous_session`. A missing calendar gives
/// `None`; the replay only uses the session window.
fn previous_trading_session(c: &Config) -> Option<chrono::NaiveDate> {
    let mut day = c.session.pred_opt()?;
    for _ in 0..10 {
        match c.is_trading_day(day) {
            Ok(true) => return Some(day),
            Ok(false) => day = day.pred_opt()?,
            Err(_) => return None,
        }
    }
    None
}

fn text_ok(text: &str, max: usize) -> bool {
    !text.trim().is_empty()
        && text.len() <= max
        && !text.chars().any(char::is_control)
        && super::super::super::transcript::redact(&json!(text)).as_str() == Some(text)
}

/// `preempted` stops a strategy replay when a higher-priority turn waits.
pub(crate) fn backtest_submit(
    c: &Config,
    cfg: &PlannerConfig,
    model_id: &str,
    parent_turn_id: &str,
    args: BacktestArgs,
    now: i64,
    preempted: &dyn Fn() -> bool,
) -> Result<Value> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("harness_analysis_disabled")?;
    settings.validate()?;
    let backtest = settings
        .candidate_compute
        .as_ref()
        .filter(|backtest| backtest.enabled)
        .ok_or("candidate_compute_disabled")?;
    if settings.admission_limits.is_none() {
        return Ok(not_dispatched("research_compute_policy_required"));
    }
    if now < 0
        || model_id.trim().is_empty()
        || model_id.len() > 128
        || parent_turn_id.trim().is_empty()
        || parent_turn_id.len() > 256
    {
        return Err("candidate_task_args_invalid".into());
    }
    if args.task_key.is_empty()
        || args.task_key.len() > 48
        || !args
            .task_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Ok(not_dispatched("candidate_task_key_invalid"));
    }
    if !text_ok(&args.hypothesis, 1024) {
        return Ok(not_dispatched("candidate_hypothesis_invalid"));
    }
    if args.source.is_empty()
        || args.source.len() > backtest.max_source_bytes as usize
        || args
            .source
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t' | '\r'))
        || super::super::super::transcript::redact(&json!(args.source)).as_str()
            != Some(args.source.as_str())
    {
        return Ok(not_dispatched("candidate_source_invalid"));
    }
    if super::super::super::transcript::redact(&args.params) != args.params
        || serde_json::to_vec(&args.params)?.len() > backtest.max_params_bytes as usize
    {
        return Ok(not_dispatched("candidate_params_invalid"));
    }
    let windows = jobs::BacktestWindows {
        training_from_ms: args.training_from_ms,
        training_to_ms: args.training_to_ms,
        selection_from_ms: args.selection_from_ms,
        selection_to_ms: args.selection_to_ms,
    };
    if !windows.validate() {
        return Ok(not_dispatched("candidate_windows_invalid"));
    }
    if !backtest.strategy.as_ref().is_some_and(|s| s.enabled) {
        return Ok(not_dispatched("candidate_strategy_disabled"));
    }
    let dataset = backtest
        .datasets
        .iter()
        .find(|dataset| dataset.dataset_id == args.dataset_id);
    let bar_window = match (args.archive_bars.as_deref(), dataset.map(|d| &d.kind)) {
        (Some(window), _) if args.dataset_id == ARCHIVE_BARS_DATASET_ID => Some(window),
        (Some(_), _) => return Ok(not_dispatched("candidate_archive_bars_dataset_conflict")),
        (None, Some(CandidateDatasetKind::ArchiveBars(window))) => Some(window),
        _ => None,
    };
    match bar_window {
        Some(window) => archive_bars::enqueue(
            c,
            cfg,
            settings,
            backtest,
            model_id,
            parent_turn_id,
            &args,
            windows,
            window.clone(),
            now,
        ),
        None => strategy_replay::strategy_backtest(
            c,
            cfg,
            backtest,
            crate::runtime::strategy_library::Author {
                lane: "research".into(),
                model: Some(model_id.to_owned()),
                model_revision: Some(0),
                turn_id: Some(parent_turn_id.to_owned()),
                session: Some(c.session),
            },
            &args,
            now,
            preempted,
        ),
    }
}

/// Status, result and cancel apply to archive bar jobs; a strategy replay
/// answers inside its `backtest` call.
fn bar_job(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<(Connection, jobs::TaskRecord)> {
    archive_bars::load(c, cfg, task_id, revision)?
        .ok_or_else(|| "candidate_task_unavailable".into())
}

pub(crate) fn backtest_status(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<Value> {
    Ok(archive_bars::status(&bar_job(c, cfg, task_id, revision)?.1))
}

pub(crate) fn backtest_result(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
) -> Result<Value> {
    let (mut db, record) = bar_job(c, cfg, task_id, revision)?;
    archive_bars::result(&mut db, &record)
}

pub(crate) fn backtest_cancel(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    now: i64,
) -> Result<Value> {
    let (mut db, _) = bar_job(c, cfg, task_id, revision)?;
    let record = jobs::cancel_scoped(&mut db, task_id, revision, revision, now)
        .map_err(|_| "research_task_cancel_refused")?;
    Ok(archive_bars::status(&record))
}
