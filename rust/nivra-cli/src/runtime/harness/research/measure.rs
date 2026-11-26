//! Read-only prospective-observation measurement.
//!
//! One observation is one independent episode with its own declared research
//! capital. This module never sums episode PnL, never claims a portfolio return
//! and never reuses account cash. Trial debt is folded from the original
//! ledgers; the family key is a label, not the multiple-testing authority.
use super::*;
use serde_json::json;

/// Copy the ORIGINAL source axes exactly. A Task 8 execution report nests its
/// scientific axes under `execution`; a ranking-only source has no execution
/// axes and must say so instead of pretending every axis is null. An
/// unreadable/unverified source result is UNAVAILABLE, never the authoritative
/// "no source result" projection.
fn source_axes(source: Option<&Value>, status: &str, reason: Option<&str>) -> Value {
    // A changed/unknown source is UNAVAILABLE, never the authoritative
    // "not_present" absence. Only `not_verified`/`not_recorded` may assert that
    // the source task has no terminal result.
    if matches!(status, "unavailable" | "unverified") {
        return json!({
            "status":"unavailable",
            "copied_unchanged":true,
            "reason":reason.unwrap_or("source_candidate_result_unavailable")
        });
    }
    let Some(source) = source else {
        return if matches!(status, "not_verified" | "not_recorded") {
            json!({
                "status":"not_present",
                "copied_unchanged":true,
                "reason":"source_candidate_result_not_available"
            })
        } else {
            json!({
                "status":"unavailable",
                "copied_unchanged":true,
                "reason":"source_candidate_result_unverified"
            })
        };
    };
    if matches!(status, "not_verified" | "not_recorded") {
        return json!({
            "status":"not_present",
            "copied_unchanged":true,
            "reason":"source_candidate_result_not_available"
        });
    }
    if status != "verified" {
        return json!({
            "status":"unavailable",
            "copied_unchanged":true,
            "reason":"source_candidate_result_unverified"
        });
    }
    let Some(axes) = source.get("execution").filter(|value| value.is_object()) else {
        return json!({
            "status":"not_present",
            "copied_unchanged":true,
            "reason":"source_candidate_has_no_task8_execution_report"
        });
    };
    json!({
        "retrospective":axes["retrospective"],
        "prospective":axes["prospective"],
        "prospective_authoring":axes["prospective_authoring"],
        "no_lookahead_qualified":axes["no_lookahead_qualified"],
        "holdout_qualified":axes["holdout_qualified"],
        "edge_qualified":axes["edge_qualified"],
        "promotion_authorized":axes["promotion_authorized"],
        "activation_authorized":axes["activation_authorized"],
        "scientific_state":axes["scientific_state"],
        "assumptions":axes["assumptions"],
        "limitations":axes["limitations"],
        "report_sha256":axes["report_sha256"],
        "copied_unchanged":true,
        "source":"task8_execution_report"
    })
}

/// Read the frozen observation spec for a CURRENT verified task without
/// creating or mutating the queue. This is only used to re-derive a recorded
/// financial measurement; a missing/changed spec is an explicit unavailable.
fn observation_spec_read_only(
    cfg: &PlannerConfig,
    target: &jobs::ObservationTargetV1,
) -> std::result::Result<nivra_cli::candidate_execution::ObservationExecutionSpecV1, &'static str> {
    let settings = cfg
        .research_tools
        .as_ref()
        .ok_or("candidate_observation_spec_unavailable")?;
    let db = rusqlite::Connection::open_with_flags(
        &settings.queue,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|_| "candidate_observation_spec_unavailable")?;
    let bytes = jobs::read_candidate_artifact(&db, &target.spec_sha256, "observation_spec", 65_536)
        .map_err(|_| "candidate_observation_spec_unavailable")?;
    if nivra_core::replay::digest(&bytes) != target.spec_sha256 {
        return Err("candidate_observation_spec_unavailable");
    }
    let spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&bytes).map_err(|_| "candidate_observation_spec_invalid")?;
    nivra_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_observation_spec_invalid")?;
    Ok(spec)
}

pub(crate) fn measurement_content_hash(maturation: &Value) -> Option<String> {
    let mut content = maturation.clone();
    if let Some(object) = content.as_object_mut() {
        object.insert("measured_at_ms".into(), json!(0));
        object.insert("measurement_sha256".into(), json!(""));
    }
    nivra_cli::candidate_execution::observation_artifact_sha256(&content).ok()
}

/// Same measurement with an explicit lifecycle-read status. A missing/corrupt
/// ledger is UNAVAILABLE, distinct from the authoritative "no measurement yet"
/// (`not_matured`) of a readable ledger.
#[allow(clippy::too_many_arguments)]
pub(crate) fn measure_observation_with_status(
    c: &Config,
    cfg: &PlannerConfig,
    task: &jobs::CandidateTaskRecord,
    result: &crate::runtime::harness::research::CandidateResultV1,
    maturation: Option<&Value>,
    source_result: Option<&Value>,
    source_result_status: &str,
    source_result_reason: Option<&str>,
    maturation_unavailable: bool,
    now: i64,
) -> Value {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.spec.purpose else {
        return json!({"status":"unavailable","reason":"candidate_observation_target_missing"});
    };
    let Some(score) = &result.observation else {
        return json!({"status":"unavailable","reason":"candidate_observation_score_missing"});
    };
    if maturation_unavailable {
        return json!({
            "status":"unavailable",
            "reason":"candidate_observation_measurement_unavailable",
            "candidate_id":task.spec.candidate_id,
            "task_id":task.spec.task_id,
            "revision":task.spec.revision,
            "observation_sha256":score.observation_sha256,
            "pnl_claimed":false,
            "activation_authorized":false
        });
    }
    // A stored maturation is usable ONLY when the canonical closed-typed
    // reader re-derives it from the ORIGINAL verified score/spec and the FIRST
    // original archived exit in the ORIGINAL window. The check is
    // unconditional: a numeric/absent financial field is never a reason to
    // skip validation. A stale or tampered value is unknown, never PnL.
    // The frozen study spec is REQUIRED even before maturation: without it the
    // study scope, capital assumption and projection are unknown. Never invent
    // a default capital or a default `single_entry` projection.
    let spec = match observation_spec_read_only(cfg, target) {
        Ok(spec) => spec,
        Err(reason) => {
            return json!({
                "status":"unavailable",
                "reason":reason,
                "candidate_id":task.spec.candidate_id,
                "task_id":task.spec.task_id,
                "revision":task.spec.revision,
                "observation_sha256":score.observation_sha256,
                "pnl_claimed":false,
                "activation_authorized":false
            });
        }
    };
    let mut verified = None;
    if let Some(maturation) = maturation {
        let context = super::candidate::StoredObservationContext {
            source: c.quote_source(),
            session: c.session,
            previous_session: super::candidate::candidate_previous_trading_session(c),
            task,
            score,
            spec: &spec,
            target,
            kind: super::candidate::StoredObservationKind::CurrentMaturation,
            immutable: false,
            recorded_at_ms: None,
            source_runtime_hash: None,
            source_cohort_key: None,
            current_runtime_hash: None,
            current_cohort_key: None,
            now,
        };
        match super::candidate::verify_stored_observation(&context, maturation) {
            Ok(value) => verified = Some(value),
            Err(_) => {
                return json!({
                    "status":"unavailable",
                    "reason":"candidate_observation_measurement_unverified",
                    "candidate_id":task.spec.candidate_id,
                    "task_id":task.spec.task_id,
                    "revision":task.spec.revision,
                    "observation_sha256":score.observation_sha256,
                    "pnl_claimed":false,
                    "activation_authorized":false
                });
            }
        }
        let bound = maturation["task_id"].as_str() == Some(task.spec.task_id.as_str())
            && maturation["revision"].as_u64() == Some(u64::from(task.spec.revision))
            && maturation["observation_sha256"].as_str() == Some(score.observation_sha256.as_str())
            && maturation["measurement_sha256"].as_str()
                == measurement_content_hash(maturation).as_deref();
        if !bound {
            return json!({
                "status":"unavailable",
                "reason":"candidate_observation_measurement_unbound",
                "candidate_id":task.spec.candidate_id,
                "task_id":task.spec.task_id,
                "revision":task.spec.revision,
                "observation_sha256":score.observation_sha256,
                "pnl_claimed":false,
                "activation_authorized":false
            });
        }
    }
    let cohort = super::carry::current_cohort_key(c, cfg).ok();
    let current_runtime = current_candidate_runtime(cfg).ok();
    let runtime_matched = current_runtime
        .as_ref()
        .map(|runtime| runtime.runtime_hash == target.runtime.runtime_hash);
    let trials = trial_fold(c, cfg);
    let exit_status = verified
        .as_ref()
        .map(|verified| verified.exit_status.as_str())
        .unwrap_or("not_matured");
    let executed = exit_status == "executed";
    let execution = maturation
        .and_then(|maturation| maturation.get("execution"))
        .cloned();
    let flat = maturation
        .and_then(|maturation| maturation.get("flat"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let accounting_complete = maturation
        .and_then(|maturation| maturation.get("accounting_complete"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let after_cost = if executed
        && flat
        && accounting_complete
        && verified.as_ref().is_some_and(|verified| verified.matured)
    {
        maturation.map(|maturation| {
            json!({
                "net_pnl_if_flat":maturation["net_pnl_if_flat"],
                "fees":maturation["fees"],
                "turnover":maturation["turnover"],
                "flat":maturation["flat"],
                "accounting_complete":maturation["accounting_complete"],
                "exit_status":maturation["exit_status"],
                "gaps":maturation["gaps"]
            })
        })
    } else {
        None
    };
    // Counts derive from the ORIGINAL score/entry/exit refs, never from a
    // hardcoded 0/1. The scope stays episode-key-only; the whole tape is not
    // claimed. A raw republication count is not recorded in the durable
    // episode evidence, so it is explicitly null instead of a fake zero.
    // Count only the actual entry receipt. A non-entry/stale diagnostic quote
    // is not an entry receipt and must not inflate the episode count.
    let entry_quote_sha256 = score
        .entry
        .as_ref()
        .map(|entry| entry.quote.archive_sha256.clone());
    let exit_quote = maturation
        .and_then(|maturation| maturation.get("exit_quote"))
        .filter(|quote| quote.is_object());
    let exit_quote_sha256 = exit_quote
        .and_then(|quote| quote["archive_sha256"].as_str())
        .map(str::to_owned);
    let mut distinct = std::collections::BTreeSet::new();
    if let Some(sha) = &entry_quote_sha256 {
        distinct.insert(sha.clone());
    }
    if let Some(sha) = &exit_quote_sha256 {
        distinct.insert(sha.clone());
    }
    let quote_receipts =
        u64::from(entry_quote_sha256.is_some()) + u64::from(exit_quote_sha256.is_some());
    let signals = u64::from(score.entry.is_some());
    // Capital and the study projection are DERIVED from the verified frozen
    // spec, never copied from a display-only body field and never defaulted.
    let capital = serde_json::to_value(spec.parameters.capital).unwrap_or(Value::Null);
    let observation_projection = spec.observation_projection.as_str();
    let recorded_at_ms = maturation
        .and_then(|maturation| maturation.get("measured_at_ms"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    // IMMUTABLE EPISODE EVIDENCE: source refs, the original entry/exit and the
    // durable recorded measurement. New admissions/cohorts cannot change this
    // hash or the past episode's financial fields.
    let episode = json!({
        "version":1,
        "kind":"forward_observation_measurement",
        "candidate_id":task.spec.candidate_id,
        "task_id":task.spec.task_id,
        "revision":task.spec.revision,
        "result_sha256":task.job.result_hash,
        "evaluation_scope":"independent_episodes",
        "strategy_portfolio_claimed":false,
        "portfolio_return":null,
        "aggregate_net_pnl":null,
        "episode_count":1,
        "episodes":[{
            "candidate_id":task.spec.candidate_id,
            "key":score.key,
            "decision_at_ms":score.decision_at_ms,
            "exit_due_ms":verified.as_ref().map(|verified| verified.due_ms).unwrap_or(score.exit_due_ms),
            "exit_deadline_ms":verified.as_ref().map(|verified| verified.deadline_ms),
            "outcome_state":verified.as_ref().map(|verified| verified.state),
            "status":score.status,
            "entry":score.entry.is_some(),
            "entry_quote_sha256":entry_quote_sha256,
            "exit_quote_sha256":exit_quote_sha256,
            "exit_status":exit_status,
            "execution":execution,
            "after_cost":after_cost,
            "maturation_gaps":maturation.map(|maturation| maturation["gaps"].clone()).unwrap_or(Value::Null),
            "residual_positions":maturation.map(|maturation| maturation["execution"]["residual_positions"].clone()).unwrap_or(Value::Null),
            "missing_marks":maturation.map(|maturation| maturation["execution"]["unknown_marks"].clone()).unwrap_or(Value::Null),
            "original_report_sha256":maturation.map(|maturation| maturation["report_sha256"].clone()).unwrap_or(Value::Null)
        }],
        "original_refs":{
            "source_task_id":target.source_task_id,
            "source_revision":target.source_revision,
            "source_candidate_id":target.source_candidate_id,
            "source_lifecycle_candidate_id":target.source_lifecycle_candidate_id,
            "source_result_sha256":target.source_result_sha256,
            "source_runtime_hash":target.source_runtime_hash,
            "source_sha256":task.spec.source_sha256,
            "params_sha256":task.spec.params_sha256,
            "dataset_hash":task.spec.dataset_hash,
            "observation_sha256":score.observation_sha256
        },
        "authorship":{
            "authoring_at_ms":target.authoring_at_ms,
            "target_frozen_at_ms":target.target_frozen_at_ms,
            "score_produced_at_ms":score.score_produced_at_ms,
            "decision_at_ms":score.decision_at_ms,
            "clock_source":"recorded_helper_completion_clock",
            "original_strategy_measurement":spec.original_strategy_measurement,
            "observation_projection":observation_projection,
            "study_projection":{
                "selection_top_n":spec.selection_top_n,
                "max_entries":spec.max_entries,
                "entry_price_offset_ticks":spec.entry_price_offset_ticks,
                "original_multi_entry_strategy":spec.original_strategy_measurement,
                "study_only":true
            }
        },
        "replay":source_axes(source_result, source_result_status, source_result_reason),
        "actual_scoring":{
            "result_kind":result.result_kind,
            "status":score.status,
            "score":score.score,
            "score_metric":score.score_metric,
            "score_sha256":score.score_sha256,
            "evidence_scope":result.evidence_scope,
            "partial_evidence":result.partial_evidence,
            "qualified":false,
            "gaps":result.gaps
        },
        "counts":{
            "episodes":1,
            "sessions":1,
            "signals":signals,
            "quote_receipts":quote_receipts,
            "raw_republications":Value::Null,
            "raw_republications_status":"not_recorded_in_durable_episode_evidence",
            "distinct_quotes":u64::try_from(distinct.len()).unwrap_or(u64::MAX),
            "source_scope":"episode_key_only",
            "raw_counts_scope":"episode_key_only",
            "all_tape_quotes_counted":false
        },
        "capital":{
            "research_capital_assumption":true,
            "account_funds_claimed":false,
            "amount":capital,
            "source":"observation_spec_parameters_research_capital_assumption"
        },
        "qualification":{
            "qualified":false,
            "promotion_authorized":false,
            "activation_authorized":false,
            "broker_funds_claimed":false,
            "return_claimed":false,
            "reason":"independent_episode_observation_not_a_funded_portfolio"
        },
        "recorded_measurement_at_ms":recorded_at_ms,
        "source":"original_observation_result_and_lifecycle_read_only"
    });
    // LIVE CONTEXT: trial debt, current cohort and read time. It may legitimately
    // change as new sources are admitted; it never changes the episode hash.
    let live = json!({
        "cohort":{
            "account":c.account,
            "cohort_key":cohort,
            "source_cohort_key":null,
            "source_cohort_key_reason":"source_cohort_not_recorded_in_target",
            "status":match runtime_matched {
                Some(true) => "runtime_matched_cohort_unknown",
                Some(false) => "runtime_changed",
                None => "runtime_unknown"
            },
            "runtime_matched":runtime_matched,
            "cohort_matched":null,
            "feed":"wide_quotes",
            "dataset_abi":"scorer_replay_v1",
            "lineage":"owner_candidate_v1"
        },
        "trials":{
            "status":trials["status"],
            "counts_lower_bound":trials["counts_lower_bound"],
            "scope":trials["scope"],
            "known_lower_bound_counts":trials["known_lower_bound_counts"],
            "global_tested_trial_count":trials["global_tested_trial_count"],
            "admitted_trial_count":trials["admitted_trial_count"],
            "executed_trial_count":trials["executed_trial_count"],
            "terminal_trial_count":trials["terminal_trial_count"],
            "unknown_exposure_count":trials["unknown_exposure_count"],
            "distinct_terminal_tasks":trials["distinct_terminal_tasks"],
            "distinct_lifecycle_candidates":trials["distinct_lifecycle_candidates"],
            "distinct_sessions":trials["distinct_sessions"],
            "distinct_sources":trials["distinct_sources"],
            "gaps":trials["gaps"],
            "reason":trials["reason"],
            "distinct_source_coverage":trials["distinct_source_coverage"],
            "distinct_lifecycle_coverage":trials["distinct_lifecycle_coverage"],
            "sessions":trials["sessions"],
            "family_key":{
                "source_sha256":task.spec.source_sha256,
                "params_sha256":task.spec.params_sha256,
                "training_from_ms":task.spec.windows.training_from_ms,
                "training_to_ms":task.spec.windows.training_to_ms,
                "feature_abi":task.spec.abi
            },
            "family_key_is_label_only":true,
            "family_count_scope":"global_verified_lineage",
            "multiple_testing":"unadjusted",
            "reset":false
        },
        "baseline":{
            "matched":false,
            "delta":null,
            "reason":"no_exact_common_cohort_baseline"
        }
    });
    let projection_sha256 = nivra_core::replay::digest(
        serde_json::to_string(&episode)
            .unwrap_or_default()
            .as_bytes(),
    );
    let live_context_sha256 =
        nivra_core::replay::digest(serde_json::to_string(&live).unwrap_or_default().as_bytes());
    let mut measurement = episode;
    measurement["cohort"] = live["cohort"].clone();
    measurement["trials"] = live["trials"].clone();
    measurement["baseline"] = live["baseline"].clone();
    measurement["live_context"] = live;
    measurement["projection_scope"] = json!("immutable_episode_evidence");
    measurement["projection_sha256"] = json!(projection_sha256);
    measurement["live_context_sha256"] = json!(live_context_sha256);
    measurement["as_of_ms"] = json!(now);
    measurement
}
