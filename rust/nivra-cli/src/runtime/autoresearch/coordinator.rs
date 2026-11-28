//! One durable transition per step: reconcile the unfinished experiment, run
//! its job, interpret a verdict, review skill drafts, or propose the next
//! idea. The controller and the off-hours timer call the same step under the
//! same SQLite lease.
use super::jobs::{self, JobEnd, Jobs};
use super::skills;
use super::{AutoGrant, AutoresearchSettings, LEASE, TurnKind, enabled, open};
use crate::runtime::agent::Lane;
use crate::runtime::planner::PlannerConfig;
use crate::runtime::workspace::{self, WorkspaceRoot};
use crate::runtime::{Config, Result};
use chrono::NaiveDate;
use nivra_core::autoresearch::{Slot, Stage, ist_day, ist_minute, slot, usd_micros, window};
use nivra_store::research_jobs as queue;
use nivra_store::research_memory::{Attempt, Campaign, Experiment, Outcome, Store};
use serde::Serialize;
use serde_json::{Value, json};

const PROPOSAL_TASK: &str = "You run one step of Nivra's autonomous research loop. Follow the autoresearch skill. Propose exactly one falsifiable idea for the slot below: read research_history first, record the idea with idea_record (mechanism, falsifier, family, regime, comparator), write one strategy at scripts/<name>.py (archive_bars_v1 for stage bars), then call backtest once with the idea id, the script path, its params and the stage. The host freezes the dates, universe, cash-sleeve capital and costs and keeps only what beats cash and its pinned parent. Change one thing from a parent. End the turn after the submission.";
const INTERPRET_TASK: &str = "The host scored one experiment under its frozen evaluator; the verdict below is final. Record what it teaches with finding_record: the observation, where it holds, its limitation and the next test. A negative result is a good result. If the status is invalid and repair_allowed is true, fix the cause in a new script version and call backtest once more with the same idea id; otherwise do not submit. Keep a short note under notes/ that cites the experiment and finding ids.";
const REVIEW_TASK: &str = "Review each learned skill draft below for the research lane. Approve a reproducible procedure supported by findings from at least two distinct experiments that uses only research tools. Reject a performance claim dressed as a procedure, a draft that conflicts with recorded evidence, or one that asks for tools research does not have. Call skill_verdict once per draft with its name and digest.";
/// Maximum trading sessions before `last_sealed` to fall back when the latest
/// sealed session's daily bars are not archived yet.
pub(crate) const MAX_TAPE_LOOKBACK_SESSIONS: usize = 5;

/// One turn the step asks for.
pub(crate) struct TurnPlan {
    pub lane: Lane,
    pub kind: TurnKind,
    pub brief: Value,
    pub grant: AutoGrant,
    pub calls: u64,
    pub cost_usd: f64,
    pub max_tool_calls: usize,
    pub timeout_ms: u64,
}

/// The review lane's typed answer for one draft.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, Serialize)]
pub(crate) struct SkillVerdict {
    pub name: String,
    pub digest: String,
    pub approve: bool,
    pub reason: String,
    #[serde(default)]
    pub finding_ids: Vec<i64>,
}

/// How a turn ended, as far as the loop's budget and records care.
pub(crate) struct TurnEnd {
    pub ok: bool,
    pub calls: u64,
    pub cost_usd: f64,
    pub skill_verdicts: Vec<SkillVerdict>,
}

/// Runs one autonomous lane turn. The controller and the timer run it on
/// the pi agent; tests script the agent.
pub(crate) trait Turns {
    fn run(&mut self, plan: &TurnPlan) -> Result<TurnEnd>;
}

pub(crate) struct Step<'a> {
    pub c: &'a Config,
    pub cfg: &'a PlannerConfig,
    /// The entry point and process holding the lease, e.g. `controller:123`.
    pub owner: &'a str,
    pub clock: &'a dyn Fn() -> i64,
    pub turns: &'a mut dyn Turns,
    pub jobs: &'a mut dyn Jobs,
    /// Set when a decision turn or host pressure needs the machine.
    pub stop: &'a dyn Fn() -> bool,
    /// Whether the archive covers a closed session's bars.
    pub tape: &'a mut dyn FnMut(NaiveDate) -> Result<bool>,
    pub trading_today: bool,
    pub sealed_today: bool,
    /// The newest sealed session: a new campaign ends on it.
    pub last_sealed: NaiveDate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct StepReport {
    /// `disabled`, `waiting` or what the step did.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tape_session: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experiment_id: Option<i64>,
    /// When the caller should step again.
    pub next_step_ms: i64,
}

impl StepReport {
    pub(crate) fn progressed(&self) -> bool {
        !matches!(self.state, "disabled" | "waiting")
    }
}

fn pid_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Advance the loop by one transition and publish its state.
pub(crate) fn step(s: &mut Step<'_>) -> Result<StepReport> {
    let now = (s.clock)();
    let Some(settings) = enabled(s.cfg) else {
        return Ok(StepReport {
            state: "disabled",
            reason: None,
            tape_state: None,
            tape_session: None,
            experiment_id: None,
            next_step_ms: i64::MAX,
        });
    };
    settings.validate()?;
    let mut store = open(s.c)?;
    let report = match leased(s, settings, &mut store, now) {
        Ok(report) => report,
        Err(error) => StepReport {
            state: "waiting",
            reason: Some(crate::runtime::harness::failure_reason(error.as_ref())),
            tape_state: None,
            tape_session: None,
            experiment_id: None,
            next_step_ms: now.saturating_add(settings.idle_poll_ms),
        },
    };
    store.set_loop_state(&json!({"at_ms":now,"owner":s.owner,"report":report}))?;
    Ok(report)
}

fn leased(
    s: &mut Step<'_>,
    settings: &AutoresearchSettings,
    store: &mut Store,
    now: i64,
) -> Result<StepReport> {
    let wait = |reason: &str| StepReport {
        state: "waiting",
        reason: Some(reason.to_owned()),
        tape_state: None,
        tape_session: None,
        experiment_id: None,
        next_step_ms: now.saturating_add(settings.idle_poll_ms),
    };
    if let Err(reason) = window(
        s.trading_today,
        s.sealed_today,
        ist_minute(now),
        settings.stop_minute,
    ) {
        return Ok(wait(reason));
    }
    if (s.stop)() {
        return Ok(wait("decision_preempted"));
    }
    let ttl = (settings.turn.timeout_ms as i64).saturating_mul(2) + 900_000;
    let Some(lease) =
        store.acquire_lease(LEASE, s.owner, std::process::id(), now, ttl, &pid_alive)?
    else {
        return Ok(wait("lease_held"));
    };
    let result = advance(s, settings, store, lease.generation, now);
    store.release_lease(LEASE, s.owner)?;
    let mut report = result?;
    if report.state == "waiting" && report.reason.is_none() {
        report.reason = Some("no_eligible_work".into());
    }
    Ok(report)
}

/// Resolve the newest trading session (within `MAX_TAPE_LOOKBACK_SESSIONS` of
/// `last_sealed`) whose daily bars are archived.
pub(super) fn resolve_tape(
    c: &Config,
    last_sealed: NaiveDate,
    tape: &mut dyn FnMut(NaiveDate) -> Result<bool>,
) -> Result<Option<NaiveDate>> {
    if tape(last_sealed)? {
        return Ok(Some(last_sealed));
    }
    let mut day = last_sealed;
    let mut sessions_checked = 0usize;
    for _ in 0..60 {
        let Some(prev) = day.pred_opt() else {
            break;
        };
        day = prev;
        match c.is_trading_day(day) {
            Ok(true) => {
                sessions_checked += 1;
                if tape(day)? {
                    return Ok(Some(day));
                }
                if sessions_checked >= MAX_TAPE_LOOKBACK_SESSIONS {
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    }
    Ok(None)
}

fn advance(
    s: &mut Step<'_>,
    settings: &AutoresearchSettings,
    store: &mut Store,
    generation: i64,
    now: i64,
) -> Result<StepReport> {
    let wait_with = |reason: &str, previous_tape: Option<NaiveDate>| StepReport {
        state: "waiting",
        reason: Some(reason.to_owned()),
        tape_state: previous_tape.map(|_| "using_previous_tape"),
        tape_session: previous_tape,
        experiment_id: None,
        next_step_ms: now.saturating_add(settings.idle_poll_ms),
    };
    let done_with =
        |state: &'static str, experiment_id: Option<i64>, previous_tape: Option<NaiveDate>| {
            StepReport {
                state,
                reason: None,
                tape_state: previous_tape.map(|_| "using_previous_tape"),
                tape_session: previous_tape,
                experiment_id,
                next_step_ms: now,
            }
        };
    let workspace_settings = s
        .cfg
        .workspace
        .as_ref()
        .filter(|w| w.enabled)
        .ok_or("harness_workspace_disabled")?;
    let root = workspace::ensure(&WorkspaceRoot::research(s.c)?, workspace_settings)?;
    skills::import_books(s.c, &workspace_settings.root, &root, store, now)?;
    skills::stage_proposals(&root, store, now)?;
    skills::materialize(&root, store)?;
    // Only the lease holder runs turns, so an open reservation is a dead turn's.
    store.settle_abandoned_turns(None)?;
    super::options::refresh(s.c, store, s.last_sealed, now, s.stop)?;
    if s.c.nifty_options.enabled
        && let Some(stage) = store.option_stage(s.last_sealed)?
        && stage.cohort_digest.is_some()
        && stage.proposed_ms.is_none()
    {
        let brief = json!({
            "kind":"option_ranking_proposal",
            "evidence":super::options::status(s.c, store)?,
            "instructions":"Propose one falsifiable prospect-ranking hypothesis. Call option_compare once with tightest_spread, lowest_cost or deepest_touch. The host compares that ranking with the recorded gate on the same sealed observations using a separate virtual options sleeve. End the turn after the result.",
        });
        let plan = plan(
            settings,
            Lane::Research,
            TurnKind::OptionProposal,
            brief,
            generation,
        );
        if let Ok(end) = gated_turn(s, settings, store, &plan, now)? {
            store.mark_option_proposed(s.last_sealed, now)?;
            return Ok(done_with(
                if end.ok {
                    "options_turn_finished"
                } else {
                    "options_turn_failed"
                },
                None,
                None,
            ));
        }
    }
    let (campaign, resolved_tape) = match store.campaign(&settings.campaign.name)? {
        Some(campaign) => (campaign, None),
        None => match resolve_tape(s.c, s.last_sealed, s.tape)? {
            Some(tape_day) => (
                jobs::ensure_campaign(store, s.c, s.cfg, settings, tape_day, now)?,
                Some(tape_day),
            ),
            None => return Ok(wait_with("waiting_for_tape", None)),
        },
    };
    let previous_tape = resolved_tape
        .filter(|tape_day| *tape_day < s.last_sealed)
        .or_else(|| {
            jobs::complete_month_end(s.last_sealed)
                .filter(|expected_to| campaign.development_to < *expected_to)
                .map(|_| campaign.development_to)
        });
    let wait = |reason: &str| wait_with(reason, previous_tape);
    let done = |state: &'static str, experiment_id: Option<i64>| {
        done_with(state, experiment_id, previous_tape)
    };
    if campaign.status != "active" {
        return Ok(wait("campaign_closed"));
    }
    if campaign.trial_cap != settings.campaign.trial_cap {
        store.set_trial_cap(campaign.id, settings.campaign.trial_cap)?;
    }
    let day_start = nivra_store::market_archive::ist_midnight(ist_day(now));
    if let Some((experiment, attempt)) = store.unfinished(campaign.id)? {
        if store.consecutive_failures(day_start)? >= settings.max_consecutive_failures {
            return Ok(wait("infrastructure_failures"));
        }
        let state = drive(s, settings, store, generation, &experiment, &attempt, now)?;
        return Ok(if state == "research_compute_busy" {
            wait(state)
        } else {
            done(state, Some(experiment.id))
        });
    }
    if let Some(experiment) = store.awaiting_interpretation(campaign.id)? {
        let brief = interpretation_brief(store, &experiment)?;
        let plan = plan(
            settings,
            Lane::Research,
            TurnKind::Interpretation,
            brief,
            generation,
        );
        // The verdict is already committed; a failed interpretation does not undo it.
        return Ok(match gated_turn(s, settings, store, &plan, now)? {
            Err(reason) => wait(reason),
            Ok(_) => {
                store.mark_interpreted(experiment.id)?;
                done("interpreted", Some(experiment.id))
            }
        });
    }
    let drafts = store.skills(Some("proposed"))?;
    let reviewer = s
        .cfg
        .agent
        .as_ref()
        .is_some_and(|agent| agent.lanes.contains_key(&Lane::Review));
    if settings.skill_review && reviewer && !drafts.is_empty() {
        let mut items = Vec::new();
        for draft in drafts.iter().take(4) {
            let text = String::from_utf8_lossy(&store.object(&draft.digest)?).into_owned();
            let findings = draft
                .finding_ids
                .iter()
                .filter_map(|id| store.finding(*id).ok().flatten())
                .collect::<Vec<_>>();
            items.push(
                json!({"name":draft.name,"digest":draft.digest,"text":text,"findings":findings}),
            );
        }
        let brief = json!({"kind":"skill_review","drafts":items,"instructions":REVIEW_TASK});
        let plan = plan(
            settings,
            Lane::Review,
            TurnKind::SkillReview,
            brief,
            generation,
        );
        return Ok(match gated_turn(s, settings, store, &plan, now)? {
            Err(reason) => wait(reason),
            Ok(end) => {
                for draft in drafts.iter().take(4) {
                    let verdict = end
                        .skill_verdicts
                        .iter()
                        .find(|v| v.name == draft.name && v.digest == draft.digest);
                    let (approve, reason) = match verdict {
                        Some(v) => (v.approve, v.reason.as_str()),
                        None => (false, "review_no_verdict"),
                    };
                    store.review_skill(
                        &draft.name,
                        &draft.digest,
                        approve,
                        reason,
                        "review",
                        now,
                    )?;
                }
                skills::materialize(&root, store)?;
                done("skills_reviewed", None)
            }
        });
    }
    let trials = store.trials(campaign.id)?;
    let cap = campaign.trial_cap.min(settings.campaign.trial_cap);
    if trials >= cap.saturating_sub(1).max(1) {
        let state = super::qualification::advance(
            s.c,
            s.cfg,
            store,
            &campaign,
            s.last_sealed,
            now,
            s.stop,
        )?;
        if state != "holdout_passed" || trials >= cap {
            return Ok(wait(
                if state == "qualification_no_finalist" && trials >= cap {
                    "trial_allowance_spent"
                } else {
                    state
                },
            ));
        }
    }
    if store.consecutive_failures(day_start)? >= settings.max_consecutive_failures {
        return Ok(wait("infrastructure_failures"));
    }
    if let Some(reason) = settings
        .day_limits()
        .variant_refusal(&store.day_spend(ist_day(now))?)
    {
        return Ok(wait(reason));
    }
    let mut brief = proposal_brief(store, settings, &campaign, now)?;
    if s.c.nifty_options.enabled {
        brief["options"] = super::options::status(s.c, store)?;
        brief["options_instructions"] = json!(
            "For a prospect-ranking hypothesis, call option_compare with the hypothesis and gate, tightest_spread, lowest_cost or deepest_touch. The result compares hypothetical selection with the frozen gate. A result is research evidence for the gate and decision lanes."
        );
    }
    let plan = plan(
        settings,
        Lane::Research,
        TurnKind::Proposal,
        brief,
        generation,
    );
    Ok(match gated_turn(s, settings, store, &plan, now)? {
        Err(reason) => wait(reason),
        Ok(end) if end.ok => done("proposed", None),
        Ok(_) => done("proposal_turn_failed", None),
    })
}

fn plan(
    settings: &AutoresearchSettings,
    lane: Lane,
    kind: TurnKind,
    brief: Value,
    generation: i64,
) -> TurnPlan {
    TurnPlan {
        lane,
        kind,
        brief,
        grant: AutoGrant { generation, kind },
        calls: settings.turn.calls,
        cost_usd: settings.turn.usd,
        max_tool_calls: settings.turn.max_tool_calls,
        timeout_ms: settings.turn.timeout_ms,
    }
}

/// Reserve the turn's worst case against today's ceilings, run it, then
/// settle the reservation to what the provider reported, once.
fn gated_turn(
    s: &mut Step<'_>,
    settings: &AutoresearchSettings,
    store: &mut Store,
    plan: &TurnPlan,
    now: i64,
) -> Result<std::result::Result<TurnEnd, &'static str>> {
    let key = format!(
        "turn:{}:{now}:{}",
        s.owner,
        json!(plan.kind).as_str().unwrap_or("turn")
    );
    let reserved = usd_micros(plan.cost_usd);
    if let Err(reason) = store.reserve_turn(
        &key,
        ist_day(now),
        plan.calls,
        reserved,
        &settings.day_limits(),
        now,
    )? {
        return Ok(Err(reason));
    }
    match s.turns.run(plan) {
        Ok(end) => {
            store.settle_turn(&key, end.calls, usd_micros(end.cost_usd))?;
            Ok(Ok(end))
        }
        Err(error) => {
            // The provider may have billed before the failure: keep the worst case.
            store.settle_turn(&key, plan.calls, reserved)?;
            Err(error)
        }
    }
}

/// The job of an attempt the loop may adopt: still running or succeeded.
fn adoptable_job(
    c: &Config,
    cfg: &PlannerConfig,
    store: &Store,
    experiment: &Experiment,
) -> Result<Option<(String, u32)>> {
    let (research, _) = jobs::loop_settings(c, cfg)?;
    let root = super::research_root(c)?;
    let Ok(mut db) = crate::runtime::harness::research::open_queue(&research, &root, false) else {
        return Ok(None);
    };
    Ok(
        queue::latest_scoped(&mut db, &jobs::task_id(store, experiment)?)
            .ok()
            .flatten()
            .filter(|record| {
                !crate::runtime::harness::research::task_state_terminal(record.status)
                    || record.status == queue::TaskState::Succeeded
            })
            .map(|record| (record.spec.task_id, record.spec.revision)),
    )
}

/// Move the unfinished experiment one transition forward.
fn drive(
    s: &mut Step<'_>,
    settings: &AutoresearchSettings,
    store: &mut Store,
    generation: i64,
    experiment: &Experiment,
    attempt: &Attempt,
    now: i64,
) -> Result<&'static str> {
    let deadline = now.saturating_add(settings.turn.timeout_ms as i64);
    match attempt.state.as_str() {
        "reserved" if experiment.stage == Stage::QuoteReplay => {
            let (outcome, _) = jobs::run_quote(s.c, s.cfg, store, experiment, now, s.stop)?;
            store.finish(attempt.id, generation, &outcome, now)?;
            Ok(outcome_state(&outcome))
        }
        "reserved" => {
            let job = match adoptable_job(s.c, s.cfg, store, experiment)? {
                Some(job) => Ok(job),
                None => {
                    let goal = store
                        .idea(experiment.idea_id)?
                        .map(|idea| idea.hypothesis)
                        .unwrap_or_default();
                    jobs::admit_bar(s.c, s.cfg, store, experiment, &goal, now)?
                }
            };
            match job {
                Ok((task_id, revision)) => {
                    store.bind_job(attempt.id, &task_id, revision, generation, now)?;
                    Ok("queued")
                }
                Err(_) => {
                    store.finish(
                        attempt.id,
                        generation,
                        &Outcome::Failed {
                            reason: "research_task_admission_refused".into(),
                        },
                        now,
                    )?;
                    Ok("failed")
                }
            }
        }
        "running" => {
            let (Some(task_id), Some(revision)) = (&attempt.job_id, attempt.job_revision) else {
                return Err("research_attempt_job_missing".into());
            };
            let outcome = match s.jobs.run(task_id, revision, s.stop)? {
                JobEnd::Busy => return Ok("research_compute_busy"),
                JobEnd::Report(report) => jobs::bar_outcome(store, experiment, &report)?,
                JobEnd::Invalid(reason) => Outcome::Invalid { reason },
                JobEnd::Failed(reason) => Outcome::Failed { reason },
                JobEnd::Stopped => Outcome::Preempted {
                    reason: "decision_preempted".into(),
                },
            };
            store.finish(attempt.id, generation, &outcome, (s.clock)())?;
            Ok(outcome_state(&outcome))
        }
        // The attempt ended without a verdict: run the frozen experiment again
        // as a new attempt, or give up after the allowed attempts.
        _ if attempt.state == "failed" && attempt.attempt_no >= settings.max_attempts => {
            store.abandon(experiment.id, "research_attempts_exhausted", now)?;
            Ok("abandoned")
        }
        _ => {
            store.next_attempt(experiment.id, generation, deadline, now)?;
            Ok("retrying")
        }
    }
}

fn outcome_state(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Scored { .. } => "scored",
        Outcome::Invalid { .. } => "invalid",
        Outcome::Failed { .. } => "failed",
        Outcome::Preempted { .. } => "preempted",
    }
}

fn experiment_fact(store: &Store, experiment: &Experiment) -> Result<Value> {
    let idea = store.idea(experiment.idea_id)?;
    Ok(json!({
        "experiment_id":experiment.id,"ordinal":experiment.ordinal,"stage":experiment.stage,
        "status":experiment.status,"verdict":experiment.verdict,"reason":experiment.reason,
        "report_sha256":experiment.report_blob,
        "net_pnl":experiment.score.as_ref().map(|s| s.net_pnl),
        "max_drawdown_bps":experiment.score.as_ref().map(|s| s.max_drawdown_bps),
        "source_path":experiment.source_path,"source_sha256":experiment.source_blob,
        "idea":idea.map(|idea| json!({"idea_id":idea.id,"family":idea.family,"regime":idea.regime,
            "hypothesis":idea.hypothesis,"falsifier":idea.falsifier,"parent_experiment_id":idea.parent_experiment_id})),
    }))
}

fn proposal_brief(
    store: &Store,
    settings: &AutoresearchSettings,
    campaign: &Campaign,
    now: i64,
) -> Result<Value> {
    let manifest: jobs::BarManifest =
        serde_json::from_slice(&store.object(&campaign.manifest_digest)?)
            .map_err(|_| "autoresearch_campaign_upgrade_required")?;
    let holdout = store.holdout(campaign.id)?;
    let confirmed = holdout
        .as_ref()
        .filter(|h| h.verdict.as_deref() == Some("keep"));
    let trials = store.trials(campaign.id)?;
    let recent = store.recent_experiments(campaign.id, 8)?;
    let cells = store.cells(campaign.id)?;
    let failure = recent
        .iter()
        .any(|e| e.verdict.as_deref() == Some("reject") || e.status == "invalid");
    let next = slot(u64::from(trials), !cells.is_empty(), failure);
    let mut incumbents = Vec::new();
    for cell in &cells {
        if let Some(experiment) = store.experiment(cell.incumbent_experiment_id)? {
            incumbents.push(experiment_fact(store, &experiment)?);
        }
    }
    let mut stalled = Vec::new();
    for family in ["trend", "reversal", "breakout"] {
        if store.family_stall(campaign.id, family)? >= settings.family_stall {
            stalled.push(family);
        }
    }
    let spent = store.day_spend(ist_day(now))?;
    let limits = settings.day_limits();
    Ok(json!({
        "kind":"autoresearch_proposal",
        "campaign":{"name":campaign.name,"segment":campaign.segment,
            "development_from":campaign.development_from,"development_to":campaign.development_to,
            "selection_from":campaign.selection_from,"capital_rupees":manifest.capital,
            "trials_used":trials,"trial_cap":campaign.trial_cap},
        "confirmed_finalist":confirmed.map(|h| json!({"experiment_id":h.finalist,"evidence":h.evidence})),
        "slot":next,
        "slot_meaning":match next {
            Slot::MutateIncumbent => "Change one thing in an incumbent below and name it as the parent.",
            Slot::RevisitFailure => "Revisit a recorded failure with a distinct causal fix.",
            Slot::NewFamily => "Try a new mechanism from a family without an incumbent.",
        },
        "incumbents":incumbents,
        "recent":recent.iter().map(|e| experiment_fact(store, e)).collect::<Result<Vec<_>>>()?,
        "closed_families":stalled,
        "allowance":{"day_variants_left":limits.variants.saturating_sub(spent.variants),
            "day_turns_left":limits.turns.saturating_sub(spent.turns),
            "trials_left":campaign.trial_cap.saturating_sub(trials)},
        "instructions":if confirmed.is_some() {
            "Confirmation is complete. Record a translation idea with the confirmed bar experiment as its parent, submit an action ABI source with stage quote_replay, and wait for unseen tape if needed."
        } else { PROPOSAL_TASK },
    }))
}

fn interpretation_brief(store: &Store, experiment: &Experiment) -> Result<Value> {
    let report = match &experiment.report_blob {
        Some(blob) => {
            let mut report: Value = serde_json::from_slice(&store.object(blob)?)?;
            fn summarize(summary: &mut Value) {
                if let Some(summary) = summary.as_object_mut() {
                    summary.remove("fill_sample");
                    summary.remove("daily_net_pnl");
                    summary.remove("daily_equity");
                }
            }
            for split in ["training", "test"] {
                summarize(&mut report[split]);
            }
            if let Some(folds) = report["folds"].as_array_mut() {
                for fold in folds {
                    summarize(fold);
                }
            }
            if let Some(object) = report.as_object_mut() {
                for key in ["trades", "action_outcomes", "costs", "bar_snapshot"] {
                    object.remove(key);
                }
            }
            report
        }
        None => Value::Null,
    };
    let idea = store
        .idea(experiment.idea_id)?
        .ok_or("research_idea_unknown")?;
    Ok(json!({
        "kind":"autoresearch_interpretation",
        "experiment":experiment_fact(store, experiment)?,
        "report":report,
        "repair_allowed":experiment.status == "invalid" && idea.repairs == 0,
        "instructions":INTERPRET_TASK,
    }))
}

/// The loop's state for the dashboard: the last step, today's budget left,
/// the campaign allowance and the latest verdict. Read only.
pub(crate) fn status(c: &Config, cfg: &PlannerConfig, now: i64) -> Value {
    let Some(settings) = enabled(cfg) else {
        return json!({"enabled":false});
    };
    let read = || -> Result<Value> {
        let store = open(c)?;
        let state = store.loop_state()?;
        let campaign = store.campaign(&settings.campaign.name)?;
        let spent = store.day_spend(ist_day(now))?;
        let limits = settings.day_limits();
        let (trials, latest, kept) = match &campaign {
            Some(campaign) => (
                Some(store.trials(campaign.id)?),
                store
                    .recent_experiments(campaign.id, 1)?
                    .first()
                    .map(|e| experiment_fact(&store, e))
                    .transpose()?,
                store.cells(campaign.id)?.len(),
            ),
            None => (None, None, 0),
        };
        let report = state
            .as_ref()
            .map(|s| s["report"].clone())
            .unwrap_or(Value::Null);
        let last_sealed = super::cli::calendar(c, now).2;
        let previous_tape = campaign
            .as_ref()
            .filter(|campaign| {
                jobs::complete_month_end(last_sealed)
                    .is_some_and(|expected_to| campaign.development_to < expected_to)
            })
            .map(|campaign| campaign.development_to);
        let tape_state = previous_tape
            .map(|_| Value::String("using_previous_tape".into()))
            .unwrap_or_else(|| report["tape_state"].clone());
        let tape_session = previous_tape
            .map(|day| Value::String(day.to_string()))
            .unwrap_or_else(|| report["tape_session"].clone());
        let holdout = campaign
            .as_ref()
            .map(|campaign| store.holdout(campaign.id))
            .transpose()?
            .flatten();
        Ok(json!({
            "enabled":true,
            "state":report["state"],"reason":report["reason"],
            "tape_state":tape_state,"tape_session":tape_session,
            "next_step_ms":report["next_step_ms"],
            "last_step_ms":state.as_ref().map(|s| s["at_ms"].clone()),
            "budget":{
                "variants_left":limits.variants.saturating_sub(spent.variants),
                "turns_left":limits.turns.saturating_sub(spent.turns),
                "calls_left":limits.calls.saturating_sub(spent.calls),
                "usd_left":(limits.usd_micros - spent.usd_micros).max(0) as f64 / 1_000_000.0,
                "variants":limits.variants,"turns":limits.turns,"calls":limits.calls,
                "usd":settings.day.usd,
            },
            "campaign":campaign.as_ref().map(|campaign| json!({"name":campaign.name,
                "trials_used":trials,"trial_cap":campaign.trial_cap,
                "development_from":campaign.development_from,"development_to":campaign.development_to})),
            "holdout":holdout.map(|h| json!({"finalist":h.finalist,"state":h.state,
                "from":h.from,"to":h.to,"verdict":h.verdict,"evidence":h.evidence})),
            "latest":latest,
            "options":super::options::status(c, &store)?,
            "incumbents":kept,
        }))
    };
    read().unwrap_or_else(|error| {
        json!({"enabled":true,"state":"unavailable",
            "reason":crate::runtime::harness::failure_reason(error.as_ref())})
    })
}
