//! Cross-session prospective-observation continuation (Task 18E4).
//!
//! A predecessor observation that is terminal `Succeeded` and recorded
//! `replayed` but was not matured before rollover is continued by the CURRENT
//! successor. The successor reads the ORIGINAL queue/artifacts/ledger/book
//! through the verified carry chain and the original queue locator, evaluates
//! the FIRST original exit with the existing shared engine, and appends ONE
//! continuation receipt to its own lifecycle ledger. The predecessor ledger,
//! queue, book, files and permissions are never written.
use super::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
mod episode;
use episode::*;

const CONTINUATION_VERSION: u32 = 1;
const CONTINUATION_SCOPE: &str = "continuation_original_episode";
const CONTINUATION_MAX_TAPE_BYTES: u64 = 1_048_576;

/// The CURRENT successor scope. It is part of the continuation identity so the
/// same original episode is not re-recorded on every tick, while a genuinely
/// different successor session/runtime is a distinct scope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ContinuationScopeV1 {
    pub version: u32,
    pub current_session: String,
    pub account: String,
    pub cohort_key: String,
    pub runtime_hash: String,
    pub source_ledger_spec_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct ContinuationIdentityV1 {
    version: u32,
    source_session: String,
    source_lifecycle_candidate_id: String,
    source_task_id: String,
    source_revision: u32,
    source_score_sha256: String,
    source_entry_sha256: String,
    scope_sha256: String,
}

struct EpisodeClock {
    entry: nivra_cli::candidate_execution::ObservationEntryV1,
    due: i64,
    deadline: i64,
}

/// Recompute the canonical continuation identity from the body's own immutable
/// source facts and CURRENT recording scope. A self-hash alone is not identity:
/// the reader must reproduce this digest and bind the scope to the actual
/// ledger/source context.
pub(crate) fn continuation_identity_sha256(value: &Value) -> Result<String> {
    let version = value["version"]
        .as_u64()
        .and_then(|version| u32::try_from(version).ok())
        .ok_or("candidate_continuation_invalid")?;
    let source_revision = value["source_revision"]
        .as_u64()
        .and_then(|revision| u32::try_from(revision).ok())
        .ok_or("candidate_continuation_invalid")?;
    let scope = value["current_scope"]
        .as_object()
        .ok_or("candidate_continuation_invalid")?;
    let scope_sha256 = digest(&serde_json::to_vec(scope)?);
    let identity = ContinuationIdentityV1 {
        version,
        source_session: value["source_session"]
            .as_str()
            .ok_or("candidate_continuation_invalid")?
            .to_owned(),
        source_lifecycle_candidate_id: value["source_lifecycle_candidate_id"]
            .as_str()
            .ok_or("candidate_continuation_invalid")?
            .to_owned(),
        source_task_id: value["source_task_id"]
            .as_str()
            .ok_or("candidate_continuation_invalid")?
            .to_owned(),
        source_revision,
        source_score_sha256: value["source_score_sha256"]
            .as_str()
            .ok_or("candidate_continuation_invalid")?
            .to_owned(),
        source_entry_sha256: value["source_entry_sha256"]
            .as_str()
            .ok_or("candidate_continuation_invalid")?
            .to_owned(),
        scope_sha256,
    };
    Ok(digest(&serde_json::to_vec(&identity)?))
}

struct SourceEpisode {
    session: String,
    directory: PathBuf,
    previous_session: chrono::NaiveDate,
    lifecycle_id: String,
    task: jobs::CandidateTaskRecord,
    score: nivra_cli::candidate_execution::ObservationScoreV1,
    entry: nivra_cli::candidate_execution::ObservationEntryV1,
    spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1,
    target: jobs::ObservationTargetV1,
    experiment: experiment_ledger::lifecycle::ExperimentSpec,
    source_state_hash: String,
    source_universe_sha256: String,
    source_cohort_key: String,
    /// The CURRENT recording context is separate from the verified ORIGINAL
    /// source context. A changed current runtime/cohort is a terminal
    /// unsupported-scope UNKNOWN, never a source-authority refusal.
    current_runtime_hash: String,
    current_cohort_key: String,
    current_runtime_changed: bool,
    current_cohort_changed: bool,
    due: i64,
    deadline: i64,
}

enum ContinuationDecision {
    Waiting {
        due_ms: i64,
        gap: Option<&'static str>,
    },
    Pending {
        due_ms: i64,
        deadline_ms: i64,
        gap: Value,
        capture: Value,
    },
    Evaluated(Value),
    Unknown(Value),
}

#[allow(clippy::large_enum_variant)]
enum ContinuationOutcome {
    Evaluated(ObservationExitEvaluation),
    Unknown {
        exit_status: String,
        exit_gap: Option<Value>,
        first_receipt_unknown: bool,
        gaps: Vec<String>,
        reason: &'static str,
        exit_quote: Option<nivra_cli::candidate_execution::FullQuoteRefV1>,
        fees: Option<nivra_core::finance::Amount>,
        turnover: Option<nivra_core::finance::Amount>,
        report_sha256: Option<String>,
        exit_at_ms: Option<i64>,
    },
}

pub(crate) fn is_observation_item(item: &Value) -> bool {
    item["feature_abi"] == nivra_cli::candidate_execution::OBSERVATION_ABI
        || item["strategy_abi"] == nivra_cli::candidate_execution::OBSERVATION_ABI
}

fn observation_settings(cfg: &PlannerConfig) -> Option<&CandidateObservationSettings> {
    cfg.research_tools
        .as_ref()
        .and_then(|settings| settings.candidate_compute.as_ref())
        .and_then(|candidate| candidate.observation.as_ref())
        .filter(|observation| observation.enabled)
}

fn ledger_file(directory: &Path) -> PathBuf {
    crate::runtime::experiment_ledger::file_path(directory)
}

/// Verify every hop's lifecycle ledger BEFORE any earlier episode is trusted.
/// A missing ledger is valid only for the CURRENT session, for the earliest
/// hop with no research feature, or for the earliest hop whose queue is absent
/// (proven no records). An intermediate missing/corrupt ledger is unknown, so
/// no earlier episode may be re-matured and no new CURRENT receipt is minted.
pub(crate) fn verify_lineage_prefix(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, PathBuf, Value)],
) -> Result<()> {
    for (index, (session, directory, _)) in chain.iter().enumerate() {
        let file = ledger_file(directory);
        if !file.is_file() {
            if index == 0 {
                // The CURRENT session may legitimately have no ledger yet.
                continue;
            }
            let settings = match super::carry::original_queue_settings(c, cfg, session) {
                Ok(settings) => settings,
                Err(error) if error.to_string().contains("harness_analysis_disabled") => continue,
                Err(_) => return Err("candidate_predecessor_lifecycle_unknown".into()),
            };
            if !super::carry::candidate_capability(&settings) {
                // Independent policy proof: no candidate capability means no
                // candidate observation records to lose.
                continue;
            }
            if settings.queue.is_file() {
                // A readable original queue with no candidate/job rows is
                // independent proof of no source records. A missing queue is
                // NOT proof: it may have been deleted while its ledger carried
                // the only record of an episode.
                let queue = super::carry::open_original_queue_read_only_strict(c, cfg, session)
                    .map_err(|_| "candidate_predecessor_lifecycle_unknown")?;
                if !super::carry::queue_has_candidate_rows(&queue)
                    .map_err(|_| "candidate_predecessor_lifecycle_unknown")?
                {
                    continue;
                }
            }
            return Err("candidate_predecessor_lifecycle_missing".into());
        }
        // A present ledger must be readable. A legacy ledger without lifecycle
        // tables is an empty library ONLY when the queue independently proves
        // no candidate admissions. An admitted legacy queue is ambiguous and
        // must not be projected as a known zero.
        if index == 0 {
            experiment_ledger::lifecycle::library_bounded(directory, "", 10_000)
                .map_err(|_| "candidate_predecessor_lifecycle_unavailable")?;
            experiment_ledger::lifecycle::continuations(directory)
                .map_err(|_| "candidate_predecessor_continuation_unavailable")?;
            if !experiment_ledger::lifecycle::lifecycle_tables_present(directory, false)
                .map_err(|_| "candidate_predecessor_lifecycle_unavailable")?
            {
                let queue = super::carry::open_original_queue_read_only_strict(c, cfg, session)
                    .map_err(|_| "candidate_predecessor_lifecycle_unknown")?;
                if super::carry::queue_has_candidate_rows(&queue)
                    .map_err(|_| "candidate_predecessor_lifecycle_unknown")?
                {
                    return Err("candidate_predecessor_lifecycle_legacy_ambiguous".into());
                }
            }
        } else {
            experiment_ledger::lifecycle::library_bounded_immutable(directory, "", 10_000)
                .map_err(|error| {
                    let code = error.to_string();
                    if code == "source_lifecycle_wal_uncheckpointed" {
                        code
                    } else {
                        "candidate_predecessor_lifecycle_unavailable".into()
                    }
                })?;
            experiment_ledger::lifecycle::continuations_immutable(directory).map_err(|error| {
                let code = error.to_string();
                if code == "source_lifecycle_wal_uncheckpointed" {
                    code
                } else {
                    "candidate_predecessor_continuation_unavailable".into()
                }
            })?;
            if !experiment_ledger::lifecycle::lifecycle_tables_present(directory, true).map_err(
                |error| {
                    let code = error.to_string();
                    if code == "source_lifecycle_wal_uncheckpointed" {
                        code
                    } else {
                        "candidate_predecessor_lifecycle_unavailable".into()
                    }
                },
            )? {
                let queue = super::carry::open_original_queue_read_only_strict(c, cfg, session)
                    .map_err(|_| "candidate_predecessor_lifecycle_unknown")?;
                if super::carry::queue_has_candidate_rows(&queue)
                    .map_err(|_| "candidate_predecessor_lifecycle_unknown")?
                {
                    return Err("candidate_predecessor_lifecycle_legacy_ambiguous".into());
                }
            }
        }
    }
    Ok(())
}

/// Bind one stored continuation body to the VERIFIED original episode and the
/// ledger context that recorded it. A recomputed self-hash/id/current_session
/// is not enough: the original score/entry/task/result/runtime/cohort/book/
/// universe and the recording session must all reproduce from source.
fn verify_continuation_context(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, PathBuf, Value)],
    recording_session: &str,
    recorded_at_ms: i64,
    value: &Value,
) -> Result<()> {
    let source_session = value["source_session"]
        .as_str()
        .ok_or("candidate_continuation_source_unverified")?;
    let Some((_, source_directory, _)) = chain
        .iter()
        .find(|(session, _, _)| session == source_session)
    else {
        return Err("candidate_continuation_source_unverified".into());
    };
    let scope = &value["current_scope"];
    if scope["current_session"].as_str() != Some(recording_session)
        || scope["account"].as_str() != Some(c.account.as_str())
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let lifecycle_id = value["source_lifecycle_candidate_id"]
        .as_str()
        .ok_or("candidate_continuation_source_unverified")?;
    let Some((experiment, _state)) =
        experiment_ledger::lifecycle::verified_parent_immutable(source_directory, lifecycle_id)?
    else {
        return Err("candidate_continuation_source_unverified".into());
    };
    if scope["source_ledger_spec_sha256"].as_str()
        != Some(digest(&serde_json::to_vec(&experiment)?).as_str())
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let library =
        experiment_ledger::lifecycle::library_bounded_immutable(source_directory, "", 10_000)?;
    let Some(item) = library["items"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| item["candidate_id"] == lifecycle_id)
    }) else {
        return Err("candidate_continuation_source_unverified".into());
    };
    let score: nivra_cli::candidate_execution::ObservationScoreV1 =
        serde_json::from_value(item["observation_score"].clone())
            .map_err(|_| "candidate_continuation_source_unverified")?;
    nivra_cli::candidate_execution::validate_observation_score(&score)
        .map_err(|_| "candidate_continuation_source_unverified")?;
    let Some(entry) = score.entry.clone() else {
        return Err("candidate_continuation_source_unverified".into());
    };
    if digest(&serde_json::to_vec(&score)?) != value["source_score_sha256"].as_str().unwrap_or("")
        || digest(&serde_json::to_vec(&entry)?)
            != value["source_entry_sha256"].as_str().unwrap_or("")
        || value["source_observation_sha256"].as_str() != Some(score.observation_sha256.as_str())
        || value["source_task_id"].as_str() != Some(score.task_id.as_str())
        || value["source_revision"].as_u64() != Some(u64::from(score.revision))
        || value["source_candidate_id"].as_str() != Some(score.candidate_id.as_str())
        || value["entry"] != serde_json::to_value(&entry)?
        || value["entry_quote"] != serde_json::to_value(&entry.quote)?
        || value["key"].as_str() != Some(entry.key.as_str())
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let mut queue = open_original_queue_read_only_strict(c, cfg, source_session)?;
    let task = jobs::load_candidate(&mut queue, &score.task_id, score.revision)
        .map_err(|_| "candidate_continuation_source_unverified")?;
    if task.status != jobs::TaskState::Succeeded
        || task.spec.session != source_session
        || task.spec.lifecycle_candidate_id != lifecycle_id
        || task.spec.candidate_id != score.candidate_id
    {
        return Err("candidate_continuation_source_unverified".into());
    }
    // The source row must belong to the VERIFIED original book, not a foreign
    // shared-queue row with matching ids.
    super::candidate::verify_task_book_binding(c, chain, &task)
        .map_err(|_| "candidate_continuation_source_unverified")?;
    let target = match &task.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.as_ref().clone(),
        _ => return Err("candidate_continuation_source_unverified".into()),
    };
    // READ-TIME source binding: the stored target is not trusted just because
    // the writer checked it once. Re-validate the target and the exact source
    // task policy/runtime/source/params/dataset/parent/window/holding/grace
    // pins, then re-verify the terminal result envelope and dataset bytes.
    if !target.validate()
        || task.spec.policy_sha256 != target.policy_sha256
        || task.spec.runtime_binding.runtime_hash != target.source_runtime_hash
        || task.spec.runtime_binding.helper_sha256 != target.runtime.helper_sha256
        || task.spec.runtime_binding.unit_policy_sha256 != target.runtime.unit_policy_sha256
        || task.spec.runtime_binding.scorer_compare_sha256 != target.runtime.scorer_compare_sha256
        || task.spec.runtime_binding.sandbox_source_sha256 != target.runtime.sandbox_source_sha256
        || task.spec.source_sha256 != target.runtime.source_sha256
        || task.spec.params_sha256 != target.runtime.params_sha256
        || task.spec.dataset_hash != target.snapshot_sha256
        || task.spec.parent_candidate_id.as_deref() != Some(target.source_candidate_id.as_str())
        || task.spec.parent_lifecycle_candidate_id.as_deref()
            != Some(target.source_lifecycle_candidate_id.as_str())
        || task.spec.windows.selection_from_ms != target.window_from_ms
        || target.window_to_ms > task.spec.windows.selection_to_ms
        || task.spec.horizon_minutes.saturating_mul(60_000) != target.holding_ms
        || task.spec.max_exit_delay_ms != target.max_exit_delay_ms
        || task.spec.abi != "scorer_replay_v1"
        || task.spec.created_at_ms != target.signal_ms
    {
        return Err("candidate_continuation_source_unverified".into());
    }
    let verified_result =
        read_verified_candidate_result(&mut queue, &task.spec.task_id, task.spec.revision)
            .map_err(|_| "candidate_continuation_source_unverified")?
            .ok_or("candidate_continuation_source_unverified")?;
    let Some(verified_score) = verified_result.observation.as_ref() else {
        return Err("candidate_continuation_source_unverified".into());
    };
    if serde_json::to_value(verified_score).ok() != Some(item["observation_score"].clone()) {
        return Err("candidate_continuation_source_unverified".into());
    }
    // The EXACT observation task result hash, never the authoring candidate's
    // result SHA that the score carries as `source_result_sha256`. The status is
    // an EXPLICIT closed enum: a missing/unknown status is invalid, never a
    // default "verified".
    let recorded_status = value["source_result_status"]
        .as_str()
        .ok_or("candidate_continuation_identity_mismatch")?;
    match (
        task.job.result_hash.as_deref(),
        value["source_result_sha256"].as_str(),
        recorded_status,
    ) {
        (Some(expected), Some(actual), "verified") if expected == actual => {}
        (None, None, "unavailable") => {}
        _ => return Err("candidate_continuation_identity_mismatch".into()),
    }
    let source_universe = instrument_universe_sha256_at(source_directory)?;
    let source_cohort = jobs::candidate_cohort_key(
        &c.account,
        &source_universe,
        "wide_quotes",
        "scorer_replay_v1",
        "owner_candidate_v1",
        &task.spec.runtime_binding.runtime_hash,
        &task.spec.runtime_binding.helper_sha256,
        &task.spec.runtime_binding.unit_policy_sha256,
        &task.spec.runtime_binding.scorer_compare_sha256,
        &task.spec.runtime_binding.sandbox_source_sha256,
    );
    let source_state_hash = source_state_hash(source_directory)?;
    // Every historical source field is PINNED and non-null. A forged NULL or a
    // missing field is refused; it is never treated as an acceptable unknown.
    if value["source_spec_sha256"].as_str() != Some(target.spec_sha256.as_str())
        || value["source_quotes_sha256"].as_str() != Some(target.quotes_sha256.as_str())
        || value["source_dataset_sha256"].as_str() != Some(target.snapshot_sha256.as_str())
        || value["source_book_binding"].as_str() != Some(task.spec.book_binding.as_str())
        || value["source_runtime"]["runtime_hash"].as_str()
            != Some(task.spec.runtime_binding.runtime_hash.as_str())
        || value["source_runtime"]["helper_sha256"].as_str()
            != Some(task.spec.runtime_binding.helper_sha256.as_str())
        || value["source_runtime"]["unit_policy_sha256"].as_str()
            != Some(task.spec.runtime_binding.unit_policy_sha256.as_str())
        || value["source_runtime"]["scorer_compare_sha256"].as_str()
            != Some(task.spec.runtime_binding.scorer_compare_sha256.as_str())
        || value["source_runtime"]["sandbox_source_sha256"].as_str()
            != Some(task.spec.runtime_binding.sandbox_source_sha256.as_str())
        || value["source_universe_sha256"].as_str() != Some(source_universe.as_str())
        || value["source_cohort_key"].as_str() != Some(source_cohort.as_str())
        || value["source_state_hash"].as_str() != Some(source_state_hash.as_str())
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let spec_bytes =
        jobs::read_candidate_artifact(&queue, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "candidate_continuation_source_unverified")?;
    if digest(&spec_bytes) != target.spec_sha256 {
        return Err("candidate_continuation_source_unverified".into());
    }
    let spec: nivra_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes)
            .map_err(|_| "candidate_continuation_source_unverified")?;
    nivra_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "candidate_continuation_source_unverified")?;
    if spec.parameters.holding_ms != target.holding_ms
        || spec.parameters.max_exit_delay_ms != target.max_exit_delay_ms
        || spec.parameters.holding_ms != experiment.holding_ms
        || spec.parameters.max_exit_delay_ms != experiment.max_exit_delay_ms
        || spec.parameters.selection_from_ms != target.window_from_ms
        || spec.parameters.selection_to_ms != experiment.selection_to_ms
        || spec.parameters.training_from_ms != experiment.training_from_ms
        || spec.parameters.training_to_ms != experiment.training_to_ms
        || value["capital"] != serde_json::to_value(spec.parameters.capital)?
        || value["observation_projection"] != json!(spec.observation_projection)
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    let due = entry
        .decision_at_ms
        .checked_add(spec.parameters.holding_ms)
        .ok_or("candidate_continuation_source_unverified")?;
    let deadline = due
        .checked_add(spec.parameters.max_exit_delay_ms)
        .ok_or("candidate_continuation_source_unverified")?;
    if value["exit_due_ms"].as_i64() != Some(due)
        || value["exit_deadline_ms"].as_i64() != Some(deadline)
    {
        return Err("candidate_continuation_identity_mismatch".into());
    }
    verify_entry_quote(
        source_directory,
        &entry,
        &source_universe_manifest_sha256(source_directory)?,
    )?;
    verify_entry_archive(&queue, &target, &entry)?;
    // The CURRENT recording runtime/cohort is separate from the historical
    // source proof. It is DERIVED from the recording session's actual
    // configuration, never accepted from the stored body: a forged scope field
    // must not manufacture an unsupported-scope outcome.
    let (current_runtime_hash, current_cohort_key) = if recording_session == c.session.to_string() {
        let runtime = super::candidate::current_candidate_runtime(cfg)
            .map_err(|_| "candidate_continuation_identity_mismatch")?;
        let cohort = super::carry::current_cohort_key(c, cfg)
            .map_err(|_| "candidate_continuation_identity_mismatch")?;
        if scope["runtime_hash"].as_str() != Some(runtime.runtime_hash.as_str())
            || scope["cohort_key"].as_str() != Some(cohort.as_str())
        {
            return Err("candidate_continuation_identity_mismatch".into());
        }
        (runtime.runtime_hash, cohort)
    } else {
        // A CLOSED predecessor receipt keeps the scope recorded by that
        // session; it is bound to the predecessor ledger spec hash above.
        (
            scope["runtime_hash"]
                .as_str()
                .ok_or("candidate_continuation_identity_mismatch")?
                .to_owned(),
            scope["cohort_key"]
                .as_str()
                .ok_or("candidate_continuation_identity_mismatch")?
                .to_owned(),
        )
    };
    let previous_session = chain
        .iter()
        .find(|(_, _, carry)| carry["source_session"] == source_session)
        .and_then(|(_, _, carry)| carry["source_previous_session"].as_str())
        .and_then(|raw| raw.parse().ok())
        .or_else(|| source_session.parse::<chrono::NaiveDate>().ok()?.pred_opt());
    let context = super::candidate::StoredObservationContext {
        source: source_directory,
        session: source_session
            .parse()
            .map_err(|_| "candidate_continuation_source_unverified")?,
        previous_session,
        task: &task,
        score: &score,
        spec: &spec,
        target: &target,
        kind: super::candidate::StoredObservationKind::Continuation,
        immutable: true,
        recorded_at_ms: Some(recorded_at_ms),
        source_runtime_hash: Some(task.spec.runtime_binding.runtime_hash.as_str()),
        source_cohort_key: Some(source_cohort.as_str()),
        current_runtime_hash: Some(current_runtime_hash.as_str()),
        current_cohort_key: Some(current_cohort_key.as_str()),
        // The replayable view is bounded by the AUTHORITATIVE ledger row clock,
        // never by a later read wall clock. A later archived event cannot be
        // consumed by a receipt recorded before it existed.
        now: recorded_at_ms,
    };
    super::candidate::verify_stored_observation(&context, value).map_err(|error| {
        let code = error.to_string();
        if code == "candidate_observation_financial_abi_unsupported" {
            "candidate_continuation_financial_abi_unsupported"
        } else {
            "candidate_continuation_financial_mismatch"
        }
    })?;
    Ok(())
}

/// Resolve the newest validated terminal continuation for each ORIGINAL episode
/// across the verified chain. The receipt keeps the scope of the session that
/// recorded it; a later successor never rewrites that scope. Each original
/// episode ref and score SHA is emitted at most once.
pub(crate) fn verified_continuations_from_chain(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, PathBuf, Value)],
) -> Result<Vec<Value>> {
    let mut by_episode: std::collections::BTreeMap<String, (i64, String, Value)> =
        std::collections::BTreeMap::new();
    for (index, (recording_session, directory, _)) in chain.iter().enumerate() {
        let receipts =
            experiment_ledger::lifecycle::continuations_with_recorded(directory, index != 0)?;
        for (recorded, value) in receipts {
            verify_continuation_context(c, cfg, chain, recording_session, recorded, &value)?;
            let Some(id) = value["source_lifecycle_candidate_id"].as_str() else {
                continue;
            };
            let cid = value["continuation_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            match by_episode.get(id) {
                Some((existing_recorded, existing_cid, _))
                    if (*existing_recorded, existing_cid.clone()) >= (recorded, cid.clone()) => {}
                _ => {
                    by_episode.insert(id.to_owned(), (recorded, cid, value));
                }
            }
        }
    }
    let mut emitted_scores = std::collections::BTreeSet::new();
    let mut values = Vec::new();
    for (_, (_, _, value)) in by_episode {
        let score = value["source_score_sha256"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if !score.is_empty() && !emitted_scores.insert(score) {
            continue;
        }
        values.push(value);
    }
    Ok(values)
}

pub(crate) fn verified_continuations(c: &Config, cfg: &PlannerConfig) -> Result<Vec<Value>> {
    let chain = predecessor_chain_verified_immutable(c)?;
    verify_lineage_prefix(c, cfg, &chain)?;
    verified_continuations_from_chain(c, cfg, &chain)
}

/// A continuation body that the canonical source validator has checked against
/// the FULL original episode and the recording ledger context. The field is
/// private, so production code cannot mint an unchecked token.
pub(crate) struct ValidatedContinuation(Value);

impl experiment_ledger::lifecycle::ContinuationBody for ValidatedContinuation {
    fn body(&self) -> &Value {
        &self.0
    }
}

/// The ONLY production continuation writer. It re-derives the body's full
/// source context BEFORE the first INSERT, so a forged body with a correct
/// self-hash and a valid source exit quote cannot poison an empty table.
pub(crate) fn record_continuation_checked(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, PathBuf, Value)],
    body: &Value,
    now: i64,
) -> Result<Value> {
    let recording_session = c.session.to_string();
    verify_continuation_context(c, cfg, chain, &recording_session, now, body)?;
    let token = ValidatedContinuation(body.clone());
    experiment_ledger::lifecycle::record_continuation(&c.directory, &token, now)
}

/// PURE read-only capture preflight for the operational projection. It reports
/// what the CURRENT session could attempt; it never calls the provider, reserves
/// a call or claims a quote. For a cross-session continuation the current
/// capture is NOT authority for an exit inside the ORIGINAL source window, so
/// `eligible_for_attempt` stays false with an explicit unsupported-scope gap.
fn capture_preflight(key: &str) -> Value {
    json!({
        "configured":false,
        "eligible_for_attempt":false,
        "status":"not_configured",
        "reason":"observation_capture_not_configured",
        "source_window_support":"unsupported_cross_session",
        "key":key,
        "interest_set":[key],
        "new_model_call":false,
        "provider_guarantee":false,
        "provider_called":false,
        "state_written":false
    })
}

fn ledger_clock(
    item: &Value,
    experiment: &experiment_ledger::lifecycle::ExperimentSpec,
) -> Option<EpisodeClock> {
    let score: nivra_cli::candidate_execution::ObservationScoreV1 =
        serde_json::from_value(item["observation_score"].clone()).ok()?;
    if nivra_cli::candidate_execution::validate_observation_score(&score).is_err() {
        return None;
    }
    let entry = score.entry.clone()?;
    let due = entry.decision_at_ms.checked_add(experiment.holding_ms)?;
    if score.exit_due_ms != due {
        return None;
    }
    let deadline = due.checked_add(experiment.max_exit_delay_ms)?;
    Some(EpisodeClock {
        entry,
        due,
        deadline,
    })
}

fn fsync_source(path: &Path) -> Result<()> {
    fs::File::open(path)
        .map_err(|_| "continuation_source_fsync_unavailable")?
        .sync_all()
        .map_err(|_| "continuation_source_fsync_unavailable".into())
}

fn source_state_hash(directory: &Path) -> Result<String> {
    let db = super::super::super::open_book_immutable(&directory.join("paper.sqlite"))
        .map_err(|_| "continuation_source_state_unavailable")?;
    let body: String = db
        .query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
        .map_err(|_| "continuation_source_state_unavailable")?;
    Ok(digest(body.as_bytes()))
}

fn source_universe_manifest_sha256(directory: &Path) -> Result<String> {
    let raw = crate::read_bounded(&directory.join("wide-universe.json"), 8_000_000)
        .map_err(|_| "continuation_source_universe_unavailable")?;
    Ok(digest(&raw))
}

fn source_instrument(directory: &Path, key: &str) -> Result<nivra_core::execution::Instrument> {
    let raw = crate::read_bounded(&directory.join("wide-universe.json"), 8_000_000)
        .map_err(|_| "continuation_source_universe_unavailable")?;
    let universe: Value =
        serde_json::from_slice(&raw).map_err(|_| "continuation_source_universe_unavailable")?;
    let instruments: Vec<nivra_core::execution::Instrument> =
        serde_json::from_value(universe["instruments"].clone())
            .map_err(|_| "continuation_source_universe_unavailable")?;
    instruments
        .into_iter()
        .find(|instrument| instrument.key == key)
        .ok_or_else(|| "continuation_source_instrument_scope".into())
}

fn item_for_continuation(continuation: &Value) -> Value {
    json!({
        "candidate_id":continuation["source_lifecycle_candidate_id"],
        "state":continuation["state"],
        "observation":continuation,
        "origin_session":continuation["source_session"],
        "continued":true,
        "activation_authorized":false,
        "edge_qualified":false
    })
}

/// Mature every eligible ORIGINAL open observation episode that crossed the
/// rollover. The successor appends only its own continuation receipt; it never
/// launches, reconciles, cancels, replays or rewrites the predecessor.
pub(crate) fn mature_predecessors(c: &Config, cfg: &PlannerConfig, now: i64) -> Result<Vec<Value>> {
    let current_session = c.session.to_string();
    let chain = predecessor_chain_verified_immutable(c)?;
    // Fail closed on an unknown prefix BEFORE any earlier episode is trusted.
    verify_lineage_prefix(c, cfg, &chain)?;
    let continuations = verified_continuations_from_chain(c, cfg, &chain)?;
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    // A terminal continuation already recorded by ANY verified hop is the
    // latest state of that episode. A later successor reuses it, preserving the
    // recording session's scope, and never mints a second receipt.
    for continuation in &continuations {
        if let Some(id) = continuation["source_lifecycle_candidate_id"].as_str() {
            seen.insert(id.to_owned());
        }
        let recording = continuation["current_scope"]["current_session"]
            .as_str()
            .unwrap_or_default();
        let mut item = item_for_continuation(continuation);
        item["recorded_in_session"] = json!(recording);
        item["reused"] = json!(recording != current_session);
        items.push(item);
    }
    for (session, directory, _carry) in chain
        .iter()
        .filter(|(session, _, _)| session != &current_session)
    {
        let library =
            match experiment_ledger::lifecycle::library_bounded_immutable(directory, "", 10_000) {
                Ok(library) => library,
                Err(_) => {
                    items.push(json!({
                        "candidate_id":Value::Null,
                        "state":"unavailable",
                        "reason":"candidate_predecessor_lifecycle_unavailable",
                        "origin_session":session,
                        "activation_authorized":false,
                        "edge_qualified":false
                    }));
                    continue;
                }
            };
        for item in library["items"].as_array().into_iter().flatten() {
            if !is_observation_item(item) || item["state"] != "replayed" {
                continue;
            }
            let Some(id) = item["candidate_id"].as_str() else {
                continue;
            };
            if seen.contains(id) {
                continue;
            }
            let Some((experiment, state)) =
                experiment_ledger::lifecycle::verified_parent_immutable(directory, id)?
            else {
                // A replayed observation item whose verified parent cannot be
                // read is explicit UNKNOWN, never a silently empty library.
                items.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_continuation_source_unverified",
                    "origin_session":session,
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            };
            if state != "replayed" {
                items.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_continuation_state_mismatch",
                    "origin_session":session,
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            }
            let Some(clock) = ledger_clock(item, &experiment) else {
                items.push(json!({
                    "candidate_id":id,
                    "state":"unavailable",
                    "reason":"candidate_continuation_clock_invalid",
                    "origin_session":session,
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            };
            if now < clock.due {
                items.push(json!({
                    "candidate_id":id,
                    "state":"waiting",
                    "due_ms":clock.due,
                    "origin_session":session,
                    "activation_authorized":false,
                    "edge_qualified":false
                }));
                continue;
            }
            let source_carry = chain
                .iter()
                .find(|(_, _, carry)| carry["source_session"] == session.as_str())
                .map(|(_, _, carry)| carry)
                .unwrap_or(&Value::Null);
            match load_source_episode(
                c,
                cfg,
                session,
                directory,
                source_carry,
                item,
                &experiment,
                &clock,
            ) {
                Ok(episode) => {
                    let unsupported = if episode.current_runtime_changed {
                        Some("continuation_runtime_changed")
                    } else if episode.current_cohort_changed {
                        Some("continuation_cohort_changed")
                    } else {
                        None
                    };
                    if let Some(reason) = unsupported {
                        if now <= clock.deadline {
                            items.push(json!({
                                "candidate_id":id,
                                "state":"pending_capture",
                                "due_ms":clock.due,
                                "deadline_ms":clock.deadline,
                                "gap":{"kind":"unsupported_session_scope","reason":reason},
                                "capture":capture_preflight(&clock.entry.key),
                                "origin_session":session,
                                "activation_authorized":false,
                                "edge_qualified":false
                            }));
                            continue;
                        }
                        // The ORIGINAL source facts are fully verified and
                        // pinned; only the CURRENT recording runtime/cohort
                        // differs. Record a terminal UNKNOWN with the full
                        // source proof so every later reader can verify it.
                        let body = build_unknown_from_episode(c, &episode, reason, now)?;
                        let recorded = record_continuation_checked(c, cfg, &chain, &body, now)?;
                        items.push(item_for_continuation(&recorded));
                        continue;
                    }
                    match decide_episode(c, cfg, &chain, &episode, now) {
                        Ok(ContinuationDecision::Waiting { due_ms, gap }) => items.push(json!({
                            "candidate_id":id,
                            "state":"waiting",
                            "due_ms":due_ms,
                            "gap":gap,
                            "origin_session":session,
                            "activation_authorized":false,
                            "edge_qualified":false
                        })),
                        Ok(ContinuationDecision::Pending {
                            due_ms,
                            deadline_ms,
                            gap,
                            capture,
                        }) => items.push(json!({
                            "candidate_id":id,
                            "state":"pending_capture",
                            "due_ms":due_ms,
                            "deadline_ms":deadline_ms,
                            "gap":gap,
                            "capture":capture,
                            "origin_session":session,
                            "activation_authorized":false,
                            "edge_qualified":false
                        })),
                        Ok(ContinuationDecision::Evaluated(continuation)) => {
                            items.push(item_for_continuation(&continuation))
                        }
                        Ok(ContinuationDecision::Unknown(continuation)) => {
                            items.push(item_for_continuation(&continuation))
                        }
                        Err(error) => items.push(json!({
                            "candidate_id":id,
                            "state":"unavailable",
                            "reason":"candidate_continuation_unavailable",
                            "detail":error.to_string().chars().take(256).collect::<String>(),
                            "origin_session":session,
                            "activation_authorized":false,
                            "edge_qualified":false
                        })),
                    }
                }
                Err(reason) => {
                    // A task/book/policy/parent mismatch is an authority
                    // refusal, not a measurement gap: never mint a receipt.
                    let code = reason.to_string();
                    items.push(json!({
                        "candidate_id":id,
                        "state":"unavailable",
                        "reason":"candidate_continuation_source_authority_refused",
                        "detail":code.chars().take(256).collect::<String>(),
                        "origin_session":session,
                        "activation_authorized":false,
                        "edge_qualified":false
                    }));
                }
            }
        }
    }
    Ok(items)
}
