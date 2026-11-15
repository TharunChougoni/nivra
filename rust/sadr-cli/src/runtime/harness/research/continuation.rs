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
    entry: sadr_cli::candidate_execution::ObservationEntryV1,
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
    score: sadr_cli::candidate_execution::ObservationScoreV1,
    entry: sadr_cli::candidate_execution::ObservationEntryV1,
    spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1,
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
        exit_quote: Option<sadr_cli::candidate_execution::FullQuoteRefV1>,
        fees: Option<sadr_core::finance::Amount>,
        turnover: Option<sadr_core::finance::Amount>,
        report_sha256: Option<String>,
        exit_at_ms: Option<i64>,
    },
}

pub(crate) fn is_observation_item(item: &Value) -> bool {
    item["feature_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI
        || item["strategy_abi"] == sadr_cli::candidate_execution::OBSERVATION_ABI
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
    let score: sadr_cli::candidate_execution::ObservationScoreV1 =
        serde_json::from_value(item["observation_score"].clone())
            .map_err(|_| "candidate_continuation_source_unverified")?;
    sadr_cli::candidate_execution::validate_observation_score(&score)
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
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes)
            .map_err(|_| "candidate_continuation_source_unverified")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
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

/// Test-only writer for the ledger idempotency/identity contract. Production
/// code has no unchecked writer; tamper tests may also use direct SQL.
#[cfg(test)]
pub(crate) fn record_continuation_test(directory: &Path, body: &Value, now: i64) -> Result<Value> {
    let token = ValidatedContinuation(body.clone());
    experiment_ledger::lifecycle::record_continuation(directory, &token, now)
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

fn capture_usage_read(c: &Config) -> Option<(u32, Option<i64>)> {
    // The CURRENT book may be live with a non-empty WAL. Read it through a
    // normal read-only snapshot, never the closed-book immutable opener.
    let db =
        super::super::super::open_book_current_read_only(&c.directory.join("paper.sqlite")).ok()?;
    let start = c
        .session
        .and_hms_opt(0, 0, 0)
        .map(|local| local.and_utc().timestamp_millis() - 19_800_000)?;
    let count: i64 = db
        .query_row(
            "SELECT count(*) FROM ticks WHERE event='research_quotes' AND at>=?",
            [start],
            |row| row.get(0),
        )
        .ok()?;
    let last: Option<i64> = db
        .query_row(
            "SELECT max(at) FROM ticks WHERE event='research_quotes' AND at>=?",
            [start],
            |row| row.get(0),
        )
        .ok()?;
    Some((u32::try_from(count).unwrap_or(u32::MAX), last))
}

/// PURE read-only capture preflight for the operational projection. It reports
/// what the CURRENT session could attempt; it never calls the provider, reserves
/// a call or claims a quote. For a cross-session continuation the current
/// capture is NOT authority for an exit inside the ORIGINAL source window, so
/// `eligible_for_attempt` stays false with an explicit unsupported-scope gap.
fn capture_preflight(c: &Config, cfg: &PlannerConfig, now: i64, key: &str) -> Value {
    let observation_enabled = observation_settings(cfg).is_some();
    let capture = cfg
        .owner_tasks
        .as_ref()
        .and_then(|settings| settings.research.as_ref())
        .filter(|research| research.enabled)
        .and_then(|research| research.observation_capture.as_ref());
    let Some(capture) = capture else {
        return json!({
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
        });
    };
    let configured = capture.enabled && observation_enabled && capture.max_keys >= 1;
    let mut current = json!({"status":"unknown","reason":"current_capture_not_evaluated"});
    if !capture.enabled {
        current = json!({"status":"disabled","reason":"observation_capture_disabled"});
    } else if !observation_enabled {
        current = json!({"status":"disabled","reason":"candidate_observation_disabled"});
    } else if capture.max_keys < 1 {
        current = json!({"status":"disabled","reason":"observation_capture_key_budget"});
    } else {
        let portfolio =
            super::super::super::open_book_current_read_only(&c.directory.join("paper.sqlite"))
                .ok()
                .and_then(|db| {
                    db.query_row("SELECT body FROM state WHERE id=1", [], |row| {
                        row.get::<_, String>(0)
                    })
                    .ok()
                })
                .and_then(|body| {
                    serde_json::from_str::<sadr_core::execution::Portfolio>(&body).ok()
                });
        match portfolio {
            Some(portfolio) if portfolio.halted => {
                current = json!({"status":"not_eligible","reason":"book_halted"})
            }
            Some(portfolio) if portfolio.stopped => {
                current = json!({"status":"not_eligible","reason":"book_stopped"})
            }
            Some(portfolio) if portfolio.closed => {
                current = json!({"status":"not_eligible","reason":"book_closed"})
            }
            Some(_) => match c.is_trading_day(c.session) {
                Err(_) => current = json!({"status":"unavailable","reason":"calendar_unavailable"}),
                Ok(false) => {
                    current = json!({"status":"not_eligible","reason":"not_a_trading_day"})
                }
                Ok(true) => {
                    let local = super::super::super::local(now);
                    let minute = local
                        .hour()
                        .saturating_mul(60)
                        .saturating_add(local.minute());
                    if !(c.execution.session_start_minute..c.execution.session_end_minute)
                        .contains(&minute)
                    {
                        current = json!({"status":"not_eligible","reason":"market_closed"});
                    } else {
                        match capture_usage_read(c) {
                            None => {
                                current = json!({"status":"unknown","reason":"observation_capture_journal_unavailable"})
                            }
                            Some((used, last)) => {
                                if used >= capture.max_calls_per_session {
                                    current = json!({
                                        "status":"quota_exhausted",
                                        "used":used,
                                        "max_calls_per_session":capture.max_calls_per_session
                                    });
                                } else if last.is_some_and(|last| {
                                    now.saturating_sub(last) < capture.min_interval_ms
                                }) {
                                    current = json!({
                                        "status":"backoff",
                                        "last_capture_ms":last,
                                        "min_interval_ms":capture.min_interval_ms
                                    });
                                } else {
                                    match super::super::super::wide::history_instruments(c) {
                                        Ok(instruments) => {
                                            let supported = instruments
                                                .iter()
                                                .any(|instrument| instrument.key == key);
                                            current = json!({
                                                "status":if supported {"eligible"} else {"not_eligible"},
                                                "reason":if supported {Value::Null} else {json!("interest_key_not_in_verified_universe")},
                                                "interest_key_supported":supported,
                                                "verified_universe_keys":instruments.len(),
                                                "used":used,
                                                "remaining":capture.max_calls_per_session.saturating_sub(used),
                                                "last_capture_ms":last,
                                                "max_calls_per_session":capture.max_calls_per_session
                                            });
                                        }
                                        Err(_) => {
                                            current = json!({"status":"unavailable","reason":"verified_universe_unavailable"})
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            None => {
                current = json!({"status":"unknown","reason":"current_book_unavailable"});
            }
        }
    }
    json!({
        "configured":configured,
        // The CURRENT capture can only capture CURRENT-session quotes. It is
        // not authority for an exit inside the ORIGINAL source window.
        "eligible_for_attempt":false,
        "status":"unsupported_session_scope",
        "reason":"current_capture_not_eligible_in_source_window",
        "source_window_support":"unsupported_cross_session",
        "current_session":current,
        "key":key,
        "interest_set":[key],
        "max_keys":capture.max_keys,
        "max_rows":capture.max_rows,
        "timeout_ms":capture.timeout_ms,
        "min_interval_ms":capture.min_interval_ms,
        "max_calls_per_session":capture.max_calls_per_session,
        "new_model_call":false,
        "provider_guarantee":false,
        "provider_called":false,
        "state_written":false,
        "source":"current_approved_research_observation_capture_preflight"
    })
}

fn ledger_clock(
    item: &Value,
    experiment: &experiment_ledger::lifecycle::ExperimentSpec,
) -> Option<EpisodeClock> {
    let score: sadr_cli::candidate_execution::ObservationScoreV1 =
        serde_json::from_value(item["observation_score"].clone()).ok()?;
    if sadr_cli::candidate_execution::validate_observation_score(&score).is_err() {
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

fn source_instrument(directory: &Path, key: &str) -> Result<sadr_core::execution::Instrument> {
    let raw = crate::read_bounded(&directory.join("wide-universe.json"), 8_000_000)
        .map_err(|_| "continuation_source_universe_unavailable")?;
    let universe: Value =
        serde_json::from_slice(&raw).map_err(|_| "continuation_source_universe_unavailable")?;
    let instruments: Vec<sadr_core::execution::Instrument> =
        serde_json::from_value(universe["instruments"].clone())
            .map_err(|_| "continuation_source_universe_unavailable")?;
    instruments
        .into_iter()
        .find(|instrument| instrument.key == key)
        .ok_or_else(|| "continuation_source_instrument_scope".into())
}

fn find_source_task(
    queue: &mut Connection,
    session: &str,
    lifecycle_id: &str,
) -> Result<jobs::CandidateTaskRecord> {
    let mut cursor = None;
    let mut scanned = 0usize;
    loop {
        let page = jobs::list_candidates_page(queue, session, None, cursor.as_ref(), 100)
            .map_err(|_| "continuation_original_queue_unavailable")?;
        if page.is_empty() {
            break;
        }
        let full = page.len() == 100;
        scanned = scanned.saturating_add(page.len());
        cursor = page.last().map(|row| jobs::TaskCursor {
            created_at_ms: row.spec.created_at_ms,
            task_id: row.spec.task_id.clone(),
            revision: row.spec.revision,
        });
        if let Some(found) = page
            .into_iter()
            .find(|row| row.spec.lifecycle_candidate_id == lifecycle_id)
        {
            return Ok(found);
        }
        if !full {
            break;
        }
        // The bounded source scan is explicit: a missing row beyond the bound
        // is `truncated`, never a false `missing` authority claim. An exact
        // 10k page is not truncation unless an actual extra row exists.
        if scanned >= 10_000 {
            let extra = jobs::list_candidates_page(queue, session, None, cursor.as_ref(), 1)
                .map_err(|_| "continuation_original_queue_unavailable")?;
            if extra.is_empty() {
                break;
            }
            return Err("continuation_source_truncated".into());
        }
    }
    Err("continuation_source_task_missing".into())
}

fn verify_entry_quote(
    directory: &Path,
    entry: &sadr_cli::candidate_execution::ObservationEntryV1,
    source_manifest_sha256: &str,
) -> Result<()> {
    let quote = &entry.quote;
    if !entry.virtual_observation
        || quote.purpose != "research_observation"
        || quote.evidence_status != "ok"
        || quote.source_ms <= 0
        || quote.receipt_ms <= 0
        || quote.source_ms > quote.receipt_ms
        || quote.receipt_ms > quote.available_at_ms
        || quote.available_at_ms > quote.event_at_ms
        || quote.event_at_ms > entry.decision_at_ms
        || quote.source_age_ms != entry.decision_at_ms.saturating_sub(quote.source_ms)
        || quote.receipt_age_ms != entry.decision_at_ms.saturating_sub(quote.receipt_ms)
        || !(0..=2_000).contains(&quote.source_age_ms)
        || !(0..=2_000).contains(&quote.receipt_age_ms)
        || quote.universe_sha256.as_deref() != Some(source_manifest_sha256)
    {
        return Err("continuation_entry_unverified".into());
    }
    let instrument = source_instrument(directory, &entry.key)?;
    if quote
        .tick
        .as_deref()
        .is_some_and(|tick| tick != instrument.tick.amount().to_string())
    {
        return Err("continuation_entry_unverified".into());
    }
    // The entry receipt must be the frozen entry tape's own archive, not a later
    // receipt or a global multi-key tape hash.
    Ok(())
}

fn verify_entry_archive(
    queue: &Connection,
    target: &jobs::ObservationTargetV1,
    entry: &sadr_cli::candidate_execution::ObservationEntryV1,
) -> Result<()> {
    let bundle_bytes = jobs::read_candidate_artifact(
        queue,
        &target.quotes_sha256,
        "observation_quotes",
        2_000_000,
    )
    .map_err(|_| "continuation_entry_unverified")?;
    if digest(&bundle_bytes) != target.quotes_sha256 {
        return Err("continuation_entry_unverified".into());
    }
    let bundle: Value =
        serde_json::from_slice(&bundle_bytes).map_err(|_| "continuation_entry_unverified")?;
    if bundle["status"] != "ok" {
        return Err("continuation_entry_unverified".into());
    }
    let manifest = bundle["manifest"]
        .as_str()
        .ok_or("continuation_entry_unverified")?;
    let events = bundle["events"]
        .as_str()
        .ok_or("continuation_entry_unverified")?;
    if bundle["manifest_sha256"] != digest(manifest.as_bytes())
        || bundle["events_sha256"] != digest(events.as_bytes())
    {
        return Err("continuation_entry_unverified".into());
    }
    let manifest: Value =
        serde_json::from_str(manifest).map_err(|_| "continuation_entry_unverified")?;
    // A virtual multi-key tape must use the per-key ORIGINAL receipt SHA. A
    // whole-tape/global SHA is never a receipt identity. The only legacy
    // fallback permitted is a provably SINGLE-instrument, SINGLE-receipt tape
    // whose global archive SHA is the one original receipt.
    let per_key = manifest["provenance"]["per_key"][entry.key.as_str()]["archive_sha256"].as_str();
    let legacy_single = manifest["provenance"]["per_key"]
        .as_object()
        .is_none_or(|map| map.is_empty())
        && manifest["provenance"]["archive_sha256"].as_str().is_some()
        && serde_json::from_str::<Value>(events.trim()).is_ok_and(|event| {
            event["observations"]
                .as_array()
                .is_some_and(|observations| {
                    observations.len() == 1
                        && observations[0]["key"].as_str() == Some(entry.key.as_str())
                })
        });
    let recorded = per_key.or_else(|| {
        legacy_single
            .then(|| manifest["provenance"]["archive_sha256"].as_str())
            .flatten()
    });
    if recorded != Some(entry.quote.archive_sha256.as_str()) {
        return Err("continuation_entry_unverified".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn load_source_episode(
    c: &Config,
    cfg: &PlannerConfig,
    session: &str,
    directory: &Path,
    carry: &Value,
    item: &Value,
    experiment: &experiment_ledger::lifecycle::ExperimentSpec,
    clock: &EpisodeClock,
) -> Result<SourceEpisode> {
    let lifecycle_id = item["candidate_id"]
        .as_str()
        .ok_or("continuation_source_lifecycle_missing")?
        .to_owned();
    let score_value = item["observation_score"].clone();
    let score: sadr_cli::candidate_execution::ObservationScoreV1 =
        serde_json::from_value(score_value.clone())
            .map_err(|_| "continuation_source_score_invalid")?;
    sadr_cli::candidate_execution::validate_observation_score(&score)
        .map_err(|_| "continuation_source_score_invalid")?;
    if score.entry.as_ref() != Some(&clock.entry) {
        return Err("continuation_source_entry_changed".into());
    }
    let previous_session: chrono::NaiveDate = carry["source_previous_session"]
        .as_str()
        .and_then(|raw| raw.parse().ok())
        .or_else(|| session.parse::<chrono::NaiveDate>().ok()?.pred_opt())
        .ok_or("continuation_source_session_scope")?;
    let mut queue = open_original_queue_read_only_strict(c, cfg, session)?;
    let task = find_source_task(&mut queue, session, &lifecycle_id)?;
    if task.status != jobs::TaskState::Succeeded
        || task.spec.session != session
        || task.spec.lifecycle_candidate_id != lifecycle_id
    {
        return Err("continuation_source_ineligible".into());
    }
    let target = match &task.spec.purpose {
        Some(jobs::CandidatePurposeV1::Observe { target }) => target.as_ref().clone(),
        _ => return Err("continuation_source_ineligible".into()),
    };
    if !target.validate() {
        return Err("continuation_source_target_invalid".into());
    }
    // The task row must be the SAME immutable source task that authored this
    // observation target, bound to the VERIFIED carry's source book. A shared
    // queue row with the same session/lifecycle id but a different book,
    // policy, parent or source/params identity is refused before the source
    // body or score artifacts are trusted.
    if carry["source_binding"].as_str() != Some(task.spec.book_binding.as_str()) {
        return Err("continuation_source_book_changed".into());
    }
    if task.spec.policy_sha256 != target.policy_sha256 {
        return Err("continuation_source_policy_changed".into());
    }
    if task.spec.parent_candidate_id.as_deref() != Some(target.source_candidate_id.as_str())
        || task.spec.parent_lifecycle_candidate_id.as_deref()
            != Some(target.source_lifecycle_candidate_id.as_str())
    {
        return Err("continuation_source_parent_changed".into());
    }
    if task.spec.source_sha256 != target.runtime.source_sha256
        || task.spec.params_sha256 != target.runtime.params_sha256
        || task.spec.dataset_hash != target.snapshot_sha256
        || task.spec.windows.selection_from_ms != target.window_from_ms
        || target.window_to_ms > task.spec.windows.selection_to_ms
        || task.spec.max_exit_delay_ms != target.max_exit_delay_ms
        || task.spec.abi != "scorer_replay_v1"
    {
        return Err("continuation_source_task_unbound".into());
    }
    let result =
        read_verified_candidate_result(&mut queue, &task.spec.task_id, task.spec.revision)?
            .ok_or("continuation_source_result_unverified")?;
    let Some(recorded_score) = &result.observation else {
        return Err("continuation_source_result_unverified".into());
    };
    if serde_json::to_value(recorded_score).ok() != Some(score_value.clone()) {
        return Err("continuation_ledger_score_mismatch".into());
    }
    let spec_bytes =
        jobs::read_candidate_artifact(&queue, &target.spec_sha256, "observation_spec", 65_536)
            .map_err(|_| "continuation_source_spec_missing")?;
    if digest(&spec_bytes) != target.spec_sha256 {
        return Err("continuation_source_spec_changed".into());
    }
    let spec: sadr_cli::candidate_execution::ObservationExecutionSpecV1 =
        serde_json::from_slice(&spec_bytes).map_err(|_| "continuation_source_spec_invalid")?;
    sadr_cli::candidate_execution::validate_observation_spec(&spec)
        .map_err(|_| "continuation_source_spec_invalid")?;
    // The frozen study spec is immutable. The CURRENT model/policy cannot
    // override the original holding, grace, costs, capital or purpose.
    if spec.parameters.holding_ms != target.holding_ms
        || spec.parameters.max_exit_delay_ms != target.max_exit_delay_ms
        || spec.parameters.selection_from_ms != target.window_from_ms
        || spec.parameters.selection_to_ms != experiment.selection_to_ms
        || spec.parameters.training_from_ms != experiment.training_from_ms
        || spec.parameters.training_to_ms != experiment.training_to_ms
        || spec.parameters.capital <= sadr_core::finance::Amount::ZERO
    {
        return Err("continuation_source_spec_changed".into());
    }
    if experiment.session != session
        || experiment.feature_abi != sadr_cli::candidate_execution::OBSERVATION_ABI
        || experiment.source_sha256 != task.spec.source_sha256
        || experiment.params_sha256 != task.spec.params_sha256
        || experiment.parent_candidate_id.as_deref()
            != Some(target.source_lifecycle_candidate_id.as_str())
        || experiment.holding_ms != target.holding_ms
        || experiment.max_exit_delay_ms != target.max_exit_delay_ms
        || experiment.selection_from_ms != target.window_from_ms
        || experiment.training_from_ms != task.spec.windows.training_from_ms
        || experiment.training_to_ms != task.spec.windows.training_to_ms
    {
        return Err("continuation_source_lifecycle_changed".into());
    }
    let lifecycle_dataset_hash = digest(
        format!(
            "candidate_observation_dataset_v1\0{}\0{}\0{}",
            target.snapshot_sha256, target.quotes_sha256, target.spec_sha256
        )
        .as_bytes(),
    );
    if experiment.dataset_hash != lifecycle_dataset_hash {
        return Err("continuation_source_lifecycle_changed".into());
    }
    if score.source_task_id != target.source_task_id
        || score.source_revision != target.source_revision
        || score.source_candidate_id != target.source_candidate_id
        || score.source_result_sha256 != target.source_result_sha256
        || score.authoring_at_ms != target.authoring_at_ms
        || score.target_frozen_at_ms != target.target_frozen_at_ms
        || score.key.as_deref() != Some(clock.entry.key.as_str())
    {
        return Err("continuation_source_score_changed".into());
    }
    let runtime = &task.spec.runtime_binding;
    if target.runtime.runtime_hash != runtime.runtime_hash
        || target.runtime.helper_sha256 != runtime.helper_sha256
        || target.runtime.unit_policy_sha256 != runtime.unit_policy_sha256
        || target.runtime.scorer_compare_sha256 != runtime.scorer_compare_sha256
        || target.runtime.sandbox_source_sha256 != runtime.sandbox_source_sha256
        || result.runtime_binding != *runtime
    {
        return Err("continuation_runtime_unverified".into());
    }
    let source_manifest_sha256 = source_universe_manifest_sha256(directory)?;
    verify_entry_quote(directory, &clock.entry, &source_manifest_sha256)?;
    verify_entry_archive(&queue, &target, &clock.entry)?;
    let source_universe_sha256 = instrument_universe_sha256_at(directory)?;
    let source_cohort_key = jobs::candidate_cohort_key(
        &c.account,
        &source_universe_sha256,
        "wide_quotes",
        "scorer_replay_v1",
        "owner_candidate_v1",
        &runtime.runtime_hash,
        &runtime.helper_sha256,
        &runtime.unit_policy_sha256,
        &runtime.scorer_compare_sha256,
        &runtime.sandbox_source_sha256,
    );
    let source_state_hash = source_state_hash(directory)?;
    let carry_state_hash = carry["source_state_hash"]
        .as_str()
        .ok_or("continuation_source_state_unavailable")?;
    if source_state_hash != carry_state_hash {
        return Err("continuation_source_state_changed".into());
    }
    // Flush the original artifacts before the current action/measurement. This
    // is a read-only fsync, not a predecessor write.
    for path in [
        directory.join("paper.sqlite"),
        directory.join("model").join("experiment-trials.sqlite"),
        directory.join("wide-universe.json"),
    ] {
        fsync_source(&path)?;
    }
    // CURRENT runtime/cohort changes are NOT source-integrity failures. They are
    // checked only AFTER the original episode's authority is fully verified, so
    // an explicit unknown can be recorded without trusting an unverified
    // source. They are never authority refusals.
    let current_runtime = current_candidate_runtime(cfg)?;
    let current_runtime_changed = current_runtime.runtime_hash != runtime.runtime_hash
        || current_runtime.helper_sha256 != runtime.helper_sha256
        || current_runtime.unit_policy_sha256 != runtime.unit_policy_sha256
        || current_runtime.scorer_compare_sha256 != runtime.scorer_compare_sha256
        || current_runtime.sandbox_source_sha256 != runtime.sandbox_source_sha256;
    let current_cohort = current_cohort_key(c, cfg)?;
    let current_cohort_changed = current_cohort != source_cohort_key;
    Ok(SourceEpisode {
        session: session.to_owned(),
        directory: directory.to_path_buf(),
        previous_session,
        lifecycle_id,
        task,
        score,
        entry: clock.entry.clone(),
        spec,
        target,
        experiment: experiment.clone(),
        source_state_hash,
        source_universe_sha256,
        source_cohort_key,
        current_runtime_hash: current_runtime.runtime_hash,
        current_cohort_key: current_cohort,
        current_runtime_changed,
        current_cohort_changed,
        due: clock.due,
        deadline: clock.deadline,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_continuation(
    c: &Config,
    cfg: &PlannerConfig,
    episode: &SourceEpisode,
    outcome: ContinuationOutcome,
    now: i64,
) -> Result<Value> {
    let current_runtime = current_candidate_runtime(cfg)?;
    let current_cohort = current_cohort_key(c, cfg)?;
    let scope = ContinuationScopeV1 {
        version: CONTINUATION_VERSION,
        current_session: c.session.to_string(),
        account: c.account.clone(),
        cohort_key: current_cohort,
        runtime_hash: current_runtime.runtime_hash.clone(),
        source_ledger_spec_sha256: digest(&serde_json::to_vec(&episode.experiment)?),
    };
    let scope_sha256 = digest(&serde_json::to_vec(&scope)?);
    let score_value = serde_json::to_value(&episode.score)?;
    let entry_value = serde_json::to_value(&episode.entry)?;
    let source_score_sha256 = digest(&serde_json::to_vec(&score_value)?);
    let source_entry_sha256 = digest(&serde_json::to_vec(&entry_value)?);
    let identity = ContinuationIdentityV1 {
        version: CONTINUATION_VERSION,
        source_session: episode.session.clone(),
        source_lifecycle_candidate_id: episode.lifecycle_id.clone(),
        source_task_id: episode.task.spec.task_id.clone(),
        source_revision: episode.task.spec.revision,
        source_score_sha256: source_score_sha256.clone(),
        source_entry_sha256: source_entry_sha256.clone(),
        scope_sha256,
    };
    let continuation_id = digest(&serde_json::to_vec(&identity)?);
    let runtime = &episode.task.spec.runtime_binding;
    let mut body = json!({
        "version":CONTINUATION_VERSION,
        "financial_abi":super::candidate::OBSERVATION_FINANCIAL_ABI,
        "continuation_id":continuation_id,
        "state":match outcome { ContinuationOutcome::Evaluated(_) => "evaluated", ContinuationOutcome::Unknown{..} => "unknown" },
        "source_session":episode.session,
        "source_task_id":episode.task.spec.task_id,
        "source_revision":episode.task.spec.revision,
        "source_candidate_id":episode.task.spec.candidate_id,
        "source_lifecycle_candidate_id":episode.lifecycle_id,
        "source_result_sha256":episode.task.job.result_hash,
        "source_result_status":if episode.task.job.result_hash.is_some() {"verified"} else {"unavailable"},
        "source_score_sha256":source_score_sha256,
        "source_observation_sha256":episode.score.observation_sha256,
        "source_entry_sha256":source_entry_sha256,
        "source_spec_sha256":episode.target.spec_sha256,
        "source_quotes_sha256":episode.target.quotes_sha256,
        "source_dataset_sha256":episode.target.snapshot_sha256,
        "source_runtime":{
            "runtime_hash":runtime.runtime_hash,
            "helper_sha256":runtime.helper_sha256,
            "unit_policy_sha256":runtime.unit_policy_sha256,
            "scorer_compare_sha256":runtime.scorer_compare_sha256,
            "sandbox_source_sha256":runtime.sandbox_source_sha256
        },
        "source_book_binding":episode.task.spec.book_binding,
        "source_state_hash":episode.source_state_hash,
        "source_universe_sha256":episode.source_universe_sha256,
        "source_cohort_key":episode.source_cohort_key,
        "current_scope":serde_json::to_value(&scope)?,
        "key":episode.entry.key,
        "entry":entry_value,
        "entry_quote":serde_json::to_value(&episode.entry.quote)?,
        "exit_due_ms":episode.due,
        "exit_deadline_ms":episode.deadline,
        "exit_quote":Value::Null,
        "exit_status":"unknown",
        "exit_gap":Value::Null,
        "first_receipt_unknown":false,
        "execution":Value::Null,
        "net_pnl_if_flat":Value::Null,
        "fees":Value::Null,
        "turnover":Value::Null,
        "flat":false,
        "accounting_complete":false,
        "gaps":Vec::<String>::new(),
        "report_sha256":Value::Null,
        "source_scope":CONTINUATION_SCOPE,
        "capital_assumption":true,
        "capital":episode.spec.parameters.capital,
        "observation_projection":episode.spec.observation_projection,
        "exit_at_ms":Value::Null,
        "processed_at_ms":now,
        "recorded_at_ms":now,
        "measurement_sha256":""
    });
    match outcome {
        ContinuationOutcome::Evaluated(evaluation) => {
            let executed = evaluation.exit_status == "executed";
            let (net_pnl, fees, turnover) = match (&evaluation.execution, executed) {
                (Some(summary), true) if summary.flat && summary.accounting_complete => (
                    summary.net_pnl_if_flat,
                    Some(summary.fees),
                    Some(summary.turnover),
                ),
                (Some(summary), _) => (None, Some(summary.fees), Some(summary.turnover)),
                _ => (None, None, None),
            };
            let flat = evaluation
                .execution
                .as_ref()
                .is_some_and(|summary| summary.flat);
            let accounting_complete = evaluation
                .execution
                .as_ref()
                .is_some_and(|summary| summary.accounting_complete);
            let mut gaps = evaluation.gaps;
            if now > episode.deadline {
                gaps.push("continuation_processed_after_deadline".into());
            }
            body["state"] = json!("evaluated");
            body["exit_quote"] = serde_json::to_value(&evaluation.exit_quote)?;
            body["exit_status"] = json!(evaluation.exit_status);
            body["exit_gap"] = evaluation.exit_gap.unwrap_or(Value::Null);
            body["first_receipt_unknown"] = json!(evaluation.first_receipt_unknown);
            body["execution"] = serde_json::to_value(&evaluation.execution)?;
            body["net_pnl_if_flat"] = serde_json::to_value(net_pnl)?;
            body["fees"] = serde_json::to_value(fees)?;
            body["turnover"] = serde_json::to_value(turnover)?;
            body["flat"] = json!(flat);
            body["accounting_complete"] = json!(accounting_complete);
            body["gaps"] = json!(gaps);
            body["report_sha256"] = serde_json::to_value(&evaluation.report_sha256)?;
            body["exit_at_ms"] = json!(evaluation.exit_at_ms);
        }
        ContinuationOutcome::Unknown {
            exit_status,
            exit_gap,
            first_receipt_unknown,
            gaps,
            reason,
            exit_quote,
            fees,
            turnover,
            report_sha256,
            exit_at_ms,
        } => {
            let mut gaps = gaps;
            gaps.push(reason.into());
            // A known original exit receipt is preserved even when the shared
            // engine cannot complete the measurement. Only a genuinely absent
            // exit quote gets the no-fabricated-price/SHA-only gap.
            if exit_quote.is_none() {
                gaps.push("no_fabricated_exit_price".into());
                gaps.push("sha_only_gap".into());
            }
            gaps.push("continuation_processed_after_deadline".into());
            body["state"] = json!("unknown");
            body["exit_status"] = json!(exit_status);
            body["exit_gap"] = exit_gap.unwrap_or_else(|| json!({"kind":reason}));
            body["first_receipt_unknown"] = json!(first_receipt_unknown);
            body["exit_quote"] = serde_json::to_value(exit_quote)?;
            body["fees"] = serde_json::to_value(fees)?;
            body["turnover"] = serde_json::to_value(turnover)?;
            body["report_sha256"] = serde_json::to_value(report_sha256)?;
            body["exit_at_ms"] = serde_json::to_value(exit_at_ms)?;
            body["gaps"] = json!(gaps);
        }
    }
    body["measurement_sha256"] = json!(experiment_ledger::lifecycle::continuation_content_sha256(
        &body
    )?);
    Ok(body)
}

#[allow(clippy::too_many_arguments)]
fn build_unknown_from_episode(
    c: &Config,
    episode: &SourceEpisode,
    reason: &str,
    now: i64,
) -> Result<Value> {
    let score_value = serde_json::to_value(&episode.score)?;
    let entry_value = serde_json::to_value(&episode.entry)?;
    let source_result_sha256 = episode.task.job.result_hash.clone();
    let source_result_status = if source_result_sha256.is_some() {
        "verified"
    } else {
        "unavailable"
    };
    let scope = ContinuationScopeV1 {
        version: CONTINUATION_VERSION,
        current_session: c.session.to_string(),
        account: c.account.clone(),
        cohort_key: episode.current_cohort_key.clone(),
        runtime_hash: episode.current_runtime_hash.clone(),
        source_ledger_spec_sha256: digest(&serde_json::to_vec(&episode.experiment)?),
    };
    let scope_sha256 = digest(&serde_json::to_vec(&scope)?);
    let source_score_sha256 = digest(&serde_json::to_vec(&score_value)?);
    let source_entry_sha256 = digest(&serde_json::to_vec(&entry_value)?);
    let identity = ContinuationIdentityV1 {
        version: CONTINUATION_VERSION,
        source_session: episode.session.clone(),
        source_lifecycle_candidate_id: episode.lifecycle_id.clone(),
        source_task_id: episode.task.spec.task_id.clone(),
        source_revision: episode.task.spec.revision,
        source_score_sha256: source_score_sha256.clone(),
        source_entry_sha256: source_entry_sha256.clone(),
        scope_sha256,
    };
    let continuation_id = digest(&serde_json::to_vec(&identity)?);
    let runtime = &episode.task.spec.runtime_binding;
    let mut gaps = vec![reason.to_owned()];
    if now > episode.deadline {
        gaps.push("continuation_processed_after_deadline".into());
    }
    gaps.push("no_fabricated_exit_price".into());
    gaps.push("sha_only_gap".into());
    let mut body = json!({
        "version":CONTINUATION_VERSION,
        "financial_abi":super::candidate::OBSERVATION_FINANCIAL_ABI,
        "continuation_id":continuation_id,
        "state":"unknown",
        "source_session":episode.session,
        "source_task_id":episode.task.spec.task_id,
        "source_revision":episode.task.spec.revision,
        "source_candidate_id":episode.task.spec.candidate_id,
        "source_lifecycle_candidate_id":episode.lifecycle_id,
        "source_result_sha256":source_result_sha256,
        "source_result_status":source_result_status,
        "source_score_sha256":source_score_sha256,
        "source_observation_sha256":episode.score.observation_sha256,
        "source_entry_sha256":source_entry_sha256,
        "source_spec_sha256":episode.target.spec_sha256,
        "source_quotes_sha256":episode.target.quotes_sha256,
        "source_dataset_sha256":episode.target.snapshot_sha256,
        "source_runtime":{
            "runtime_hash":runtime.runtime_hash,
            "helper_sha256":runtime.helper_sha256,
            "unit_policy_sha256":runtime.unit_policy_sha256,
            "scorer_compare_sha256":runtime.scorer_compare_sha256,
            "sandbox_source_sha256":runtime.sandbox_source_sha256
        },
        "source_book_binding":episode.task.spec.book_binding,
        "source_state_hash":episode.source_state_hash,
        "source_universe_sha256":episode.source_universe_sha256,
        "source_cohort_key":episode.source_cohort_key,
        "current_scope":serde_json::to_value(&scope)?,
        "key":episode.entry.key,
        "entry":entry_value,
        "entry_quote":serde_json::to_value(&episode.entry.quote)?,
        "exit_due_ms":episode.due,
        "exit_deadline_ms":episode.deadline,
        "exit_quote":Value::Null,
        "exit_status":"exit_scope_unsupported",
        "exit_gap":{
            "kind":"unsupported_session_scope",
            "reason":reason,
            "source_runtime_hash":runtime.runtime_hash,
            "recorded_runtime_hash":episode.current_runtime_hash,
            "source_cohort_key":episode.source_cohort_key,
            "recorded_cohort_key":episode.current_cohort_key
        },
        "first_receipt_unknown":false,
        "execution":Value::Null,
        "net_pnl_if_flat":Value::Null,
        "fees":Value::Null,
        "turnover":Value::Null,
        "flat":false,
        "accounting_complete":false,
        "gaps":gaps,
        "report_sha256":Value::Null,
        "source_scope":CONTINUATION_SCOPE,
        "capital_assumption":true,
        "capital":episode.spec.parameters.capital,
        "observation_projection":episode.spec.observation_projection,
        "exit_at_ms":Value::Null,
        "processed_at_ms":now,
        "recorded_at_ms":now,
        "measurement_sha256":""
    });
    body["measurement_sha256"] = json!(experiment_ledger::lifecycle::continuation_content_sha256(
        &body
    )?);
    Ok(body)
}

fn decide_episode(
    c: &Config,
    cfg: &PlannerConfig,
    chain: &[(String, PathBuf, Value)],
    episode: &SourceEpisode,
    now: i64,
) -> Result<ContinuationDecision> {
    if now < episode.due {
        return Ok(ContinuationDecision::Waiting {
            due_ms: episode.due,
            gap: None,
        });
    }
    let Some(observation) = observation_settings(cfg) else {
        return Err("candidate_observation_disabled".into());
    };
    let keys = BTreeSet::from([episode.entry.key.clone()]);
    let query_to = now.min(episode.deadline);
    let source_tape = super::super::super::paper_replay::virtual_observation_tape_bounded_immutable(
        &episode.directory,
        episode
            .session
            .parse()
            .map_err(|_| "continuation_source_session_scope")?,
        Some(episode.previous_session),
        &keys,
        episode.due,
        query_to,
        episode.spec.parameters.capital,
        observation.max_quote_events,
        observation.max_observations,
        observation.max_tape_bytes.min(CONTINUATION_MAX_TAPE_BYTES),
        now,
        super::super::super::paper_replay::TapeMode::ExitWindow,
    );
    match source_tape {
        Ok(tape) => match evaluate_observation_exit(&episode.score, &episode.spec, &tape)? {
            ObservationExitOutcome::Evaluated(evaluation) => {
                let body = build_continuation(
                    c,
                    cfg,
                    episode,
                    ContinuationOutcome::Evaluated(evaluation),
                    now,
                )?;
                let recorded = record_continuation_checked(c, cfg, chain, &body, now)?;
                Ok(ContinuationDecision::Evaluated(recorded))
            }
            ObservationExitOutcome::MeasurementUnavailable(unavailable) => {
                if now <= episode.deadline {
                    Ok(ContinuationDecision::Pending {
                        due_ms: episode.due,
                        deadline_ms: episode.deadline,
                        gap: json!({"kind":"exit_measurement_unavailable"}),
                        capture: capture_preflight(c, cfg, now, &episode.entry.key),
                    })
                } else {
                    let body = build_continuation(
                        c,
                        cfg,
                        episode,
                        ContinuationOutcome::Unknown {
                            exit_status: "exit_measurement_unavailable".into(),
                            exit_gap: Some(json!({"kind":"exit_measurement_unavailable"})),
                            first_receipt_unknown: false,
                            gaps: vec!["exit_measurement_unavailable".into()],
                            reason: "exit_measurement_unavailable",
                            exit_quote: Some(unavailable.exit_quote),
                            fees: None,
                            turnover: None,
                            report_sha256: None,
                            exit_at_ms: None,
                        },
                        now,
                    )?;
                    let recorded = record_continuation_checked(c, cfg, chain, &body, now)?;
                    Ok(ContinuationDecision::Unknown(recorded))
                }
            }
        },
        Err(gap) => {
            if now <= episode.deadline {
                return Ok(ContinuationDecision::Pending {
                    due_ms: episode.due,
                    deadline_ms: episode.deadline,
                    gap: gap.gap,
                    capture: capture_preflight(c, cfg, now, &episode.entry.key),
                });
            }
            // The CURRENT successor's capture is NOT authority for an exit in
            // the ORIGINAL source window. Never read the current book or a
            // current-session tick as a substitute for the original receipt.
            // The gap stays explicit; no original_no_data/network promise is
            // invented for an unsupported cross-session scope.
            // Keep the typed CAUSE from the tape gap. A quote-query failure is
            // NOT an absence of data: only a genuine missing receipt may claim
            // original_no_data.
            let (exit_status, first_receipt_unknown, reason) = match gap.gap["kind"].as_str() {
                Some("compacted_receipt") => (
                    "exit_first_receipt_unknown",
                    true,
                    "original_quote_compacted",
                ),
                Some("first_receipt_unknown") => (
                    "exit_first_receipt_unknown",
                    true,
                    "original_first_receipt_unknown",
                ),
                Some("coverage_budget_truncated") => {
                    ("exit_quote_missing", false, "original_coverage_truncated")
                }
                Some("missing_quote") => ("exit_quote_missing", false, "original_no_data"),
                Some("quote_query_unavailable") => (
                    "exit_measurement_unavailable",
                    false,
                    "original_query_unavailable",
                ),
                Some(
                    "invalid_tape_request"
                    | "screen_purpose"
                    | "event_encoding"
                    | "universe_unavailable"
                    | "universe_invalid"
                    | "previous_session",
                ) => (
                    "exit_measurement_unavailable",
                    false,
                    "original_tape_invalid",
                ),
                Some("observation_budget" | "tape_bytes") => (
                    "exit_measurement_unavailable",
                    false,
                    "original_tape_too_large",
                ),
                _ => (
                    "exit_measurement_unavailable",
                    false,
                    "original_query_unavailable",
                ),
            };
            let body = build_continuation(
                c,
                cfg,
                episode,
                ContinuationOutcome::Unknown {
                    exit_status: exit_status.into(),
                    exit_gap: Some(gap.gap),
                    first_receipt_unknown,
                    gaps: vec![reason.into()],
                    reason,
                    exit_quote: None,
                    fees: None,
                    turnover: None,
                    report_sha256: None,
                    exit_at_ms: None,
                },
                now,
            )?;
            let recorded = record_continuation_checked(c, cfg, chain, &body, now)?;
            Ok(ContinuationDecision::Unknown(recorded))
        }
    }
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
                                "capture":capture_preflight(c, cfg, now, &clock.entry.key),
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
