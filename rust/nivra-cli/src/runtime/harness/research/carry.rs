use super::*;

/// Locate the ORIGINAL recorded queue for one verified session. The successor's
/// current policy is never used as a substitute: a dated rollover records the
/// predecessor's own queue in the planner chain, and a missing/unreadable
/// original locator is an explicit unavailable, not a silent fallback.
pub(crate) fn original_queue_settings(
    c: &Config,
    cfg: &PlannerConfig,
    source_session: &str,
) -> Result<ResearchSettings> {
    if source_session == c.session.to_string() {
        return cfg
            .research_tools
            .clone()
            .ok_or_else(|| "harness_analysis_disabled".into());
    }
    let path = c
        .planner_config
        .as_ref()
        .ok_or("research_original_queue_locator_missing")?;
    let session: chrono::NaiveDate = source_session
        .parse()
        .map_err(|_| "research_original_queue_locator_invalid")?;
    let original = PlannerConfig::load_for_session(path, session)
        .map_err(|_| "research_original_queue_locator_unavailable")?;
    original
        .research_tools
        .ok_or_else(|| "research_original_queue_locator_missing".into())
}

/// Strictly read-only ORIGINAL queue access for cross-session continuation.
/// The verified carry chain is read with the immutable closed-book contract.
pub(crate) fn open_original_queue_read_only_strict(
    c: &Config,
    cfg: &PlannerConfig,
    source_session: &str,
) -> Result<Connection> {
    let settings = original_queue_settings(c, cfg, source_session)?;
    if source_session == c.session.to_string() {
        return open_queue_for_carry_read_only(&settings, &c.directory.join("model"));
    }
    let chain = predecessor_chain_verified_immutable(c)?;
    if !chain
        .iter()
        .any(|(session, _, _)| session == source_session)
    {
        return Err("research_carry_session_not_verified".into());
    }
    let mut authorities = Vec::new();
    for (_, directory, _) in &chain {
        let model = directory.join("model");
        if !authorities.contains(&model) {
            authorities.push(model);
        }
    }
    let mut last = Err("research_queue_unavailable".into());
    for authority in authorities {
        match open_queue_for_carry_immutable(&settings, &authority) {
            Ok(db) => return Ok(db),
            Err(error) => last = Err(error),
        }
    }
    last
}

/// Strict predecessor chain for cross-session continuation: every hop is read
/// with an immutable closed-book contract, so no predecessor `-wal`/`-shm`
/// sidecar is created and a non-empty WAL is an explicit refusal.
pub(crate) fn predecessor_chain_verified_immutable(
    c: &Config,
) -> Result<Vec<(String, PathBuf, Value)>> {
    predecessor_chain_verified_with(c, true)
}

fn predecessor_chain_verified_with(
    c: &Config,
    immutable: bool,
) -> Result<Vec<(String, PathBuf, Value)>> {
    if let Some(root) = c.sessions_root.as_ref().filter(|path| path.is_absolute()) {
        let pointer = root.join("current.json");
        if pointer.is_file() {
            let value: Value = serde_json::from_slice(
                &crate::read_bounded(&pointer, 65_536).map_err(|_| "research_carry_unreadable")?,
            )
            .map_err(|_| "research_carry_invalid")?;
            if value["account"] != c.account {
                return Err("research_carry_account_mismatch".into());
            }
        }
    }
    let mut chain = Vec::new();
    let mut current = c.clone();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..64 {
        if !seen.insert(current.session) {
            return Err("research_carry_cycle".into());
        }
        chain.push((
            current.session.to_string(),
            current.directory.clone(),
            Value::Null,
        ));
        let path = current.directory.join("carry-over.json");
        if !path.is_file() {
            break;
        }
        let raw =
            crate::read_bounded(&path, 16_000_000).map_err(|_| "research_carry_unreadable")?;
        let carry: Value = serde_json::from_slice(&raw).map_err(|_| "research_carry_invalid")?;
        if immutable {
            super::super::super::rollover::verify_carry_immutable(&current, &carry).map_err(
                |error| {
                    let code = error.to_string();
                    if code == "source_book_wal_uncheckpointed" {
                        code
                    } else {
                        "research_carry_unverified".into()
                    }
                },
            )?;
        } else {
            super::super::super::rollover::verify_carry(&current, &carry)
                .map_err(|_| "research_carry_unverified")?;
        }
        let source: chrono::NaiveDate = serde_json::from_value(carry["source_session"].clone())
            .map_err(|_| "research_carry_invalid")?;
        if source >= current.session {
            return Err("research_carry_future".into());
        }
        let Some(directory) = session_directory(c, &source.to_string()) else {
            return Err("research_carry_unavailable".into());
        };
        if let Some(last) = chain.last_mut() {
            last.2 = carry.clone();
        }
        current.session = source;
        current.previous_session =
            serde_json::from_value(carry["source_previous_session"].clone()).unwrap_or(source);
        current.directory = directory;
    }
    // A chain that still has a predecessor after the bounded walk is an
    // explicit lineage limit, never a silently complete-looking lineage.
    if current.directory.join("carry-over.json").is_file() {
        return Err("research_carry_lineage_limit".into());
    }
    Ok(chain)
}

fn session_directory(c: &Config, session: &str) -> Option<PathBuf> {
    c.sessions_root
        .as_ref()
        .filter(|root| root.is_absolute())
        .map(|root| root.join(session))
}

fn instrument_universe_sha256(c: &Config) -> Result<String> {
    instrument_universe_sha256_at(&c.directory)
}

pub(crate) fn instrument_universe_sha256_at(directory: &Path) -> Result<String> {
    let raw = crate::read_bounded(&directory.join("wide-universe.json"), 8_000_000)
        .map_err(|_| "source_continue_identity_mismatch")?;
    let universe: Value =
        serde_json::from_slice(&raw).map_err(|_| "source_continue_identity_mismatch")?;
    let mut keys = universe["instruments"]
        .as_array()
        .ok_or("source_continue_identity_mismatch")?
        .iter()
        .filter_map(|instrument| instrument["key"].as_str())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    keys.sort();
    keys.dedup();
    Ok(digest(&serde_json::to_vec(&keys)?))
}

/// Stable account/feed/instrument/runtime cohort. It deliberately excludes the
/// per-session `book_binding` and any future snapshot hash.
pub(crate) fn current_cohort_key(c: &Config, cfg: &PlannerConfig) -> Result<String> {
    let runtime = super::candidate::current_candidate_runtime(cfg)?;
    let instruments = instrument_universe_sha256(c)?;
    Ok(jobs::candidate_cohort_key(
        &c.account,
        &instruments,
        "wide_quotes",
        "scorer_replay_v1",
        "owner_candidate_v1",
        &runtime.runtime_hash,
        &runtime.helper_sha256,
        &runtime.unit_policy_sha256,
        &runtime.scorer_compare_sha256,
        &runtime.sandbox_source_sha256,
    ))
}

fn derived_axes(result: &super::candidate::CandidateResultV1) -> Vec<String> {
    let Some(execution) = &result.execution else {
        return Vec::new();
    };
    let mut axes = Vec::new();
    if execution.retrospective {
        axes.push("retrospective".into());
    }
    if execution.prospective {
        axes.push("prospective".into());
    }
    if execution.prospective_authoring {
        axes.push("prospective_authoring".into());
    }
    if execution.no_lookahead_qualified {
        axes.push("no_lookahead_qualified".into());
    }
    if execution.holdout_qualified {
        axes.push("holdout_qualified".into());
    }
    if execution.edge_qualified {
        axes.push("edge_qualified".into());
    }
    axes.push(format!("scientific_state:{}", execution.scientific_state));
    axes
}

/// Host-built, verified, opaque cross-session parent reference. The model may
/// echo this object; it never invents hashes or reads a predecessor account.
/// Every field is derived from the original queue row, verified result, source
/// state and the verified carry chain.
pub(crate) fn host_parent_ref(
    c: &Config,
    _cfg: &PlannerConfig,
    db: &mut Connection,
    task: &jobs::CandidateTaskRecord,
) -> Result<Value> {
    if task.status != jobs::TaskState::Succeeded || task.spec.purpose.is_some() {
        return Err("source_continue_ineligible".into());
    }
    let chain = predecessor_chain_verified_immutable(c)?;
    let Some((_, source_directory, _)) = chain
        .iter()
        .find(|(session, _, _)| session == &task.spec.session)
    else {
        return Err("source_continue_account_mismatch".into());
    };
    let result = super::candidate::read_verified_candidate_result(
        db,
        &task.spec.task_id,
        task.spec.revision,
    )?
    .ok_or("source_continue_result_unverified")?;
    let source_db = if task.spec.session == c.session.to_string() {
        super::super::super::open_book_current_read_only(&source_directory.join("paper.sqlite"))
    } else {
        super::super::super::open_book_immutable(&source_directory.join("paper.sqlite"))
    }
    .map_err(|_| "source_continue_account_mismatch")?;
    let body: String = source_db
        .query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
        .map_err(|_| "source_continue_account_mismatch")?;
    let source_state_hash = digest(body.as_bytes());
    let instrument_universe_sha256 = instrument_universe_sha256_at(source_directory)?;
    let cohort_key = jobs::candidate_cohort_key(
        &c.account,
        &instrument_universe_sha256,
        "wide_quotes",
        "scorer_replay_v1",
        "owner_candidate_v1",
        &task.spec.runtime_binding.runtime_hash,
        &task.spec.runtime_binding.helper_sha256,
        &task.spec.runtime_binding.unit_policy_sha256,
        &task.spec.runtime_binding.scorer_compare_sha256,
        &task.spec.runtime_binding.sandbox_source_sha256,
    );
    let carry_sha256 = match predecessor_chain_verified_immutable(c) {
        Ok(chain) => {
            let hop = chain
                .iter()
                .find(|(session, _, _)| session == &task.spec.session);
            match hop {
                Some((_, _, carry)) if carry.is_object() => digest(&serde_json::to_vec(carry)?),
                Some(_) => digest(b"same_session"),
                None => return Err("source_continue_account_mismatch".into()),
            }
        }
        Err(reason) => return Err(reason),
    };
    let axes = derived_axes(&result);
    let reference = jobs::CandidateParentRefV1 {
        version: 1,
        source_session: task.spec.session.clone(),
        task_id: task.spec.task_id.clone(),
        revision: task.spec.revision,
        candidate_id: task.spec.candidate_id.clone(),
        lifecycle_candidate_id: task.spec.lifecycle_candidate_id.clone(),
        source_book_binding: task.spec.book_binding.clone(),
        source_state_hash,
        carry_sha256,
        account: c.account.clone(),
        instrument_universe_sha256,
        cohort_key,
        source_sha256: task.spec.source_sha256.clone(),
        params_sha256: task.spec.params_sha256.clone(),
        dataset_hash: task.spec.dataset_hash.clone(),
        content_key: task.spec.content_key.clone(),
        result_sha256: task
            .job
            .result_hash
            .clone()
            .ok_or("source_continue_result_unverified")?,
        runtime_hash: task.spec.runtime_binding.runtime_hash.clone(),
        result_kind: result.result_kind.clone(),
        axes,
    };
    if !reference.validate() {
        return Err("source_continue_identity_mismatch".into());
    }
    Ok(json!({
        "status":"available",
        "version":1,
        "ref":reference,
        "display":{
            "source_session":task.spec.session,
            "task_id":task.spec.task_id,
            "revision":task.spec.revision,
            "candidate_id":task.spec.candidate_id,
            "result_kind":result.result_kind,
            "source_sha256":task.spec.source_sha256,
            "params_sha256":task.spec.params_sha256,
            "dataset_hash":task.spec.dataset_hash,
            "runtime_hash":task.spec.runtime_binding.runtime_hash,
            "axes":reference.axes
        },
        "opaque":true,
        "account_payload":false,
        "paths":false,
        "launched":false,
        "activation_authorized":false
    }))
}

/// Resolve a typed cross-session parent reference read-only. Only a terminal
/// `Succeeded` task with a fully verified original result and an unbroken
/// carry/account chain is eligible. The predecessor is never launched,
/// reconciled, cancelled, replayed or rewritten.
pub(crate) fn resolve_parent_ref(
    c: &Config,
    cfg: &PlannerConfig,
    reference: &jobs::CandidateParentRefV1,
) -> Result<Value> {
    if !reference.validate() {
        return Err("source_continue_identity_mismatch".into());
    }
    if reference.account != c.account {
        return Err("source_continue_account_mismatch".into());
    }
    // Walk the original carry-over chain from the current session to the source
    // session. Every hop must revalidate the original predecessor state.
    let chain = predecessor_chain_verified_immutable(c)?;
    let Some((_, source_directory, carry)) = chain
        .iter()
        .find(|(session, _, _)| session == &reference.source_session)
    else {
        return Err("source_continue_account_mismatch".into());
    };
    let carry_sha256 = if carry.is_object() {
        digest(&serde_json::to_vec(carry)?)
    } else {
        digest(b"same_session")
    };
    if carry_sha256 != reference.carry_sha256 {
        return Err("source_continue_identity_mismatch".into());
    }
    let source_universe = instrument_universe_sha256_at(source_directory)?;
    if source_universe != reference.instrument_universe_sha256 {
        return Err("source_continue_identity_mismatch".into());
    }
    let mut queue = open_original_queue_read_only_strict(c, cfg, &reference.source_session)?;
    let task = jobs::load_candidate(&mut queue, &reference.task_id, reference.revision)
        .map_err(|_| "source_continue_ineligible")?;
    if task.spec.session != reference.source_session
        || task.spec.book_binding != reference.source_book_binding
        || task.spec.candidate_id != reference.candidate_id
        || task.spec.lifecycle_candidate_id != reference.lifecycle_candidate_id
        || task.spec.content_key != reference.content_key
        || task.spec.source_sha256 != reference.source_sha256
        || task.spec.params_sha256 != reference.params_sha256
        || task.spec.dataset_hash != reference.dataset_hash
        || task.spec.runtime_binding.runtime_hash != reference.runtime_hash
    {
        return Err("source_continue_identity_mismatch".into());
    }
    if task.status != jobs::TaskState::Succeeded {
        return Err("source_continue_ineligible".into());
    }
    // The host never advertises an observation task as an authoring parent, so
    // the resolver must apply the SAME policy instead of accepting a forged
    // observation parent. A future observation continuation needs its own
    // typed reuse path.
    if task.spec.purpose.is_some() {
        return Err("source_continue_ineligible".into());
    }
    // A same-session parent is the CURRENT live book; a predecessor is a CLOSED
    // book read immutably. Neither creates schema/rows/sidecars.
    let source_db = if reference.source_session == c.session.to_string() {
        super::super::super::open_book_current_read_only(&source_directory.join("paper.sqlite"))
    } else {
        super::super::super::open_book_immutable(&source_directory.join("paper.sqlite"))
    }
    .map_err(|_| "source_continue_account_mismatch")?;
    let body: String = source_db
        .query_row("SELECT body FROM state WHERE id=1", [], |row| row.get(0))
        .map_err(|_| "source_continue_account_mismatch")?;
    if digest(body.as_bytes()) != reference.source_state_hash {
        return Err("source_continue_identity_mismatch".into());
    }
    let result = super::candidate::read_verified_candidate_result(
        &mut queue,
        &reference.task_id,
        reference.revision,
    )?
    .ok_or("source_continue_result_unverified")?;
    if task.job.result_hash.as_deref() != Some(reference.result_sha256.as_str())
        || result.result_kind != reference.result_kind
        || derived_axes(&result) != reference.axes
    {
        return Err("source_continue_result_unverified".into());
    }
    // The stable cohort is derived from the SOURCE account/feed/instruments and
    // the source task runtime, not from a caller claim or the current snapshot.
    let source_cohort_key = jobs::candidate_cohort_key(
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
    if source_cohort_key != reference.cohort_key {
        return Err("source_continue_cohort_changed".into());
    }
    if let Some(current_runtime) = super::candidate::current_candidate_runtime(cfg).ok()
        && current_runtime.runtime_hash != task.spec.runtime_binding.runtime_hash
    {
        return Err("source_continue_runtime_changed".into());
    }
    Ok(json!({
        "status":"resolved",
        "source_session":reference.source_session,
        "task_id":reference.task_id,
        "revision":reference.revision,
        "candidate_id":reference.candidate_id,
        "lifecycle_candidate_id":reference.lifecycle_candidate_id,
        "result_sha256":reference.result_sha256,
        "result_kind":reference.result_kind,
        "cohort_key":source_cohort_key,
        "carry_sha256":carry_sha256,
        "carry_hops":chain.len().saturating_sub(1),
        "launched":false,
        "replayed":false,
        "activation_authorized":false
    }))
}

/// Verify the ORIGINAL source lifecycle row for a host-built cross-session
/// parent. The successor records only its own new version/event pointing at the
/// original id; the predecessor ledger is read, never copied or migrated.
pub(crate) fn verify_source_lifecycle(
    c: &Config,
    reference: &jobs::CandidateParentRefV1,
) -> Result<()> {
    let chain = predecessor_chain_verified_immutable(c)?;
    let Some((_, source_directory, _)) = chain
        .iter()
        .find(|(session, _, _)| session == &reference.source_session)
    else {
        return Err("source_continue_account_mismatch".into());
    };
    let row = if reference.source_session == c.session.to_string() {
        experiment_ledger::lifecycle::verified_parent(
            source_directory,
            &reference.lifecycle_candidate_id,
        )
    } else {
        experiment_ledger::lifecycle::verified_parent_immutable(
            source_directory,
            &reference.lifecycle_candidate_id,
        )
    }
    .map_err(|_| "source_continue_lifecycle_unavailable")?;
    let Some((spec, _state)) = row else {
        return Err("source_continue_lifecycle_missing".into());
    };
    if spec.session != reference.source_session
        || spec.source_sha256 != reference.source_sha256
        || spec.params_sha256 != reference.params_sha256
    {
        return Err("source_continue_identity_mismatch".into());
    }
    Ok(())
}

fn terminal_state(state: &str) -> bool {
    matches!(state, "Terminated" | "Succeeded" | "Cancelled" | "Refused")
}

fn table_exists(db: &Connection, name: &str) -> Result<bool> {
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |row| row.get::<_, bool>(0),
    )
    .map_err(|_| "research_task_state_unavailable".into())
}

/// A session policy can only have candidate trial rows when the reviewed
/// candidate capability is enabled. This is an INDEPENDENT policy fact, not an
/// inference from a missing file.
pub(crate) fn candidate_capability(settings: &ResearchSettings) -> bool {
    settings
        .candidate_compute
        .as_ref()
        .is_some_and(|candidate| candidate.enabled)
}

/// Independent proof that a readable original queue has no candidate
/// admissions. A missing queue is NOT proof: the file may have been deleted
/// while its ledger still records debt.
pub(crate) fn queue_has_candidate_rows(db: &Connection) -> Result<bool> {
    for table in ["candidate_tasks", "scoped_tasks"] {
        if !table_exists(db, table)? {
            continue;
        }
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .map_err(|_| "research_task_state_unavailable")?;
        if count > 0 {
            return Ok(true);
        }
    }
    if table_exists(db, "jobs")? {
        let count: i64 = db
            .query_row("SELECT count(*) FROM jobs", [], |row| row.get(0))
            .map_err(|_| "research_task_state_unavailable")?;
        if count > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Read-only global trial fold over every verified predecessor queue and
/// lifecycle ledger. It counts original admissions, attempts, terminal
/// budget-charged failures/cancellations and unknown exposure without a second
/// counter authority. A partial read exposes NULL main counts plus explicit
/// lower bounds; it never fabricates an available zero.
pub(crate) fn trial_fold(c: &Config, cfg: &PlannerConfig) -> Value {
    let null_counts = || {
        json!({
            "global_tested_trial_count":null,
            "admitted_trial_count":null,
            "executed_trial_count":null,
            "terminal_trial_count":null,
            "unknown_exposure_count":null,
            "distinct_terminal_tasks":null,
            "distinct_lifecycle_candidates":null,
            "distinct_sessions":null,
            "distinct_sources":null
        })
    };
    if cfg.research_tools.is_none() {
        let mut value = null_counts();
        value["status"] = json!("disabled");
        value["sessions"] = json!([]);
        value["known_lower_bound_counts"] = Value::Null;
        value["counts_lower_bound"] = json!(false);
        value["scope"] = json!("research_disabled");
        value["family_key_is_label_only"] = json!(true);
        value["reset"] = json!(false);
        return value;
    }
    let chain = match predecessor_chain_verified_immutable(c) {
        Ok(chain) => chain,
        Err(reason) => {
            let mut value = null_counts();
            value["status"] = json!("unavailable");
            value["reason"] = json!(reason.to_string());
            value["sessions"] = json!([]);
            value["known_lower_bound_counts"] = Value::Null;
            value["counts_lower_bound"] = json!(false);
            value["scope"] = json!("verified_chain_unavailable");
            value["family_key_is_label_only"] = json!(true);
            value["reset"] = json!(false);
            return value;
        }
    };
    let mut admitted = std::collections::BTreeSet::new();
    let mut executed = std::collections::BTreeSet::new();
    let mut terminal = std::collections::BTreeSet::new();
    let mut unknown = std::collections::BTreeSet::new();
    let mut lifecycles = std::collections::BTreeSet::new();
    let mut sources = std::collections::BTreeSet::new();
    let mut sessions = std::collections::BTreeSet::new();
    let mut partial = false;
    let mut gaps = std::collections::BTreeSet::new();
    // `scoped_tasks` carries the candidate id/state/attempts but not the
    // source/lifecycle body fields. When it contributes admissions, the
    // distinct-source/lifecycle counts are explicitly candidate-only.
    let mut scoped_rows = false;
    let mut scoped_fields = false;
    // The prefix authority is checked for EVERY consumer of trial debt, not
    // only the maturation path. An unknown prefix makes the main counts NULL;
    // the readable sessions still provide explicit lower bounds.
    if let Err(reason) = super::continuation::verify_lineage_prefix(c, cfg, &chain) {
        let code = reason.to_string();
        if code.contains("wal_uncheckpointed") {
            // A non-empty predecessor WAL is an explicit unavailable: an
            // immutable read would be stale and a WAL read would create a
            // sidecar. Never degrade this to a numeric zero.
            let mut value = null_counts();
            value["status"] = json!("unavailable");
            value["reason"] = json!(code);
            value["sessions"] = json!([]);
            value["known_lower_bound_counts"] = Value::Null;
            value["counts_lower_bound"] = json!(false);
            value["scope"] = json!("verified_prefix_unavailable");
            value["family_key_is_label_only"] = json!(true);
            value["reset"] = json!(false);
            return value;
        }
        partial = true;
        gaps.insert(match code.as_str() {
            "candidate_predecessor_lifecycle_missing" => "trial_prefix_lifecycle_missing",
            "candidate_predecessor_lifecycle_unavailable" => "trial_prefix_lifecycle_unavailable",
            "candidate_predecessor_lifecycle_unknown" => "trial_prefix_lifecycle_unknown",
            "candidate_predecessor_lifecycle_legacy_ambiguous" => {
                "trial_lifecycle_legacy_ambiguous"
            }
            _ => "trial_prefix_unavailable",
        });
    }
    for (index, (session, directory, _)) in chain.iter().enumerate() {
        // A session with no research feature has no trial ledger by design.
        let settings = match original_queue_settings(c, cfg, session) {
            Ok(settings) => settings,
            Err(error) if error.to_string().contains("harness_analysis_disabled") => continue,
            Err(_) => {
                partial = true;
                gaps.insert("trial_source_policy_unavailable");
                continue;
            }
        };
        if !candidate_capability(&settings) {
            // Independent policy proof: no candidate capability means no
            // candidate admissions/trials for this session.
            continue;
        }
        if !settings.queue.is_file() {
            // A deleted/absent queue is NOT a known zero. The lifecycle ledger
            // (or an earlier admission) may still exist and carry debt.
            partial = true;
            gaps.insert("trial_queue_missing");
            continue;
        }
        let Ok(db) = open_original_queue_read_only_strict(c, cfg, session) else {
            partial = true;
            gaps.insert("trial_queue_unreadable");
            continue;
        };
        sessions.insert(session.clone());
        let mut any_table = false;
        let mut session_admitted = 0usize;
        let mut session_keys = std::collections::BTreeSet::new();
        for table in ["candidate_tasks", "scoped_tasks"] {
            match table_exists(&db, table) {
                Ok(true) => any_table = true,
                Ok(false) => continue,
                Err(_) => {
                    partial = true;
                    gaps.insert("trial_queue_schema_unreadable");
                    continue;
                }
            }
            let sql = format!(
                "SELECT t.task_id,t.revision,j.state,j.attempts FROM {table} t JOIN jobs j ON j.id=t.job_id WHERE t.session=?"
            );
            let Ok(mut statement) = db.prepare(&sql) else {
                partial = true;
                gaps.insert("trial_queue_schema_unreadable");
                continue;
            };
            let Ok(rows) = statement.query_map([session], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, u32>(3)?,
                ))
            }) else {
                partial = true;
                gaps.insert("trial_queue_read_unavailable");
                continue;
            };
            for row in rows {
                if table == "scoped_tasks" {
                    scoped_rows = true;
                }
                let Ok((task_id, revision, state, attempts)) = row else {
                    partial = true;
                    gaps.insert("trial_queue_row_invalid");
                    continue;
                };
                let key = (session.clone(), task_id, revision);
                if session_keys.insert(key.clone()) {
                    session_admitted = session_admitted.saturating_add(1);
                }
                admitted.insert(key.clone());
                if attempts > 0 {
                    executed.insert(key.clone());
                }
                if terminal_state(&state) {
                    terminal.insert(key.clone());
                } else {
                    unknown.insert(key.clone());
                }
            }
            if table == "candidate_tasks" || table == "scoped_tasks" {
                let Ok(mut statement) =
                    db.prepare(&format!("SELECT body FROM {table} WHERE session=?"))
                else {
                    partial = true;
                    gaps.insert("trial_queue_read_unavailable");
                    continue;
                };
                let Ok(rows) = statement.query_map([session], |row| row.get::<_, String>(0)) else {
                    partial = true;
                    gaps.insert("trial_queue_read_unavailable");
                    continue;
                };
                for row in rows {
                    let Ok(body) = row else {
                        partial = true;
                        gaps.insert("trial_queue_row_invalid");
                        continue;
                    };
                    // A corrupt task body is an explicit partial read, never a
                    // silently skipped row that makes the fold look complete.
                    match serde_json::from_str::<Value>(&body) {
                        Ok(spec) => {
                            if table == "scoped_tasks"
                                && (spec["lifecycle_candidate_id"].is_string()
                                    || spec["source_sha256"].is_string())
                            {
                                scoped_fields = true;
                            }
                            if let Some(id) = spec["lifecycle_candidate_id"].as_str() {
                                lifecycles.insert(id.to_owned());
                            }
                            if let Some(source) = spec["source_sha256"].as_str() {
                                sources.insert(source.to_owned());
                            }
                        }
                        Err(_) => {
                            partial = true;
                            gaps.insert("trial_queue_body_invalid");
                        }
                    }
                }
            }
        }
        if !any_table {
            // A queue that has never admitted a candidate has no candidate
            // tables. A queue with job rows but no candidate table is a partial
            // schema read, not a fake zero.
            match table_exists(&db, "jobs") {
                Ok(false) => {}
                Ok(true) => {
                    let jobs: i64 = db
                        .query_row("SELECT count(*) FROM jobs", [], |row| row.get(0))
                        .unwrap_or(-1);
                    if jobs != 0 {
                        partial = true;
                        gaps.insert("trial_candidate_schema_missing");
                    }
                }
                Err(_) => {
                    partial = true;
                    gaps.insert("trial_queue_schema_unreadable");
                }
            }
        }
        let file = directory.join("model").join("experiment-trials.sqlite");
        if !file.is_file() {
            // A missing expected ledger with admitted rows is unknown, not a
            // known zero. A fresh session with no admissions has no ledger.
            if session_admitted > 0 {
                partial = true;
                gaps.insert("trial_lifecycle_missing");
            }
            continue;
        }
        let ledger: Result<Connection> = if index == 0 {
            rusqlite::Connection::open_with_flags(
                &file,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|error| error.into())
        } else {
            super::super::super::open_ledger_immutable(&file)
        };
        match ledger {
            Ok(ledger) => match table_exists(&ledger, "experiment_lifecycle_state") {
                Ok(false) => {
                    // A legacy ledger without lifecycle tables is only a known
                    // zero when the policy/queue prove no candidate records.
                    if session_admitted > 0 {
                        partial = true;
                        gaps.insert("trial_lifecycle_legacy_ambiguous");
                    }
                }
                Ok(true) => {
                    let Ok(mut statement) = ledger.prepare(
                        "SELECT candidate_id,state FROM experiment_lifecycle_state WHERE state IN ('replayed','forward_shadow','evaluated','eligible','rejected','insufficient','paper_selected')",
                    ) else {
                        partial = true;
                        gaps.insert("trial_lifecycle_read_unavailable");
                        continue;
                    };
                    let Ok(rows) = statement.query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    }) else {
                        partial = true;
                        gaps.insert("trial_lifecycle_read_unavailable");
                        continue;
                    };
                    for row in rows {
                        match row {
                            Ok((id, _)) => {
                                lifecycles.insert(id);
                            }
                            Err(_) => {
                                partial = true;
                                gaps.insert("trial_lifecycle_row_invalid");
                            }
                        }
                    }
                }
                Err(_) => {
                    partial = true;
                    gaps.insert("trial_lifecycle_schema_unreadable");
                }
            },
            Err(_) => {
                partial = true;
                gaps.insert("trial_lifecycle_unreadable");
            }
        }
    }
    let tested = terminal
        .iter()
        .filter(|key| executed.contains(*key))
        .count();
    let lower = json!({
        "global_tested_trial_count":tested,
        "admitted_trial_count":admitted.len(),
        "executed_trial_count":executed.len(),
        "terminal_trial_count":terminal.len(),
        "unknown_exposure_count":unknown.len(),
        "distinct_terminal_tasks":terminal.len(),
        "distinct_lifecycle_candidates":lifecycles.len(),
        "distinct_sessions":sessions.len(),
        "distinct_sources":sources.len()
    });
    let mut value = if partial {
        json!({
            "status":"partial",
            "reason":"trial_ledger_partial_read",
            "gaps":gaps.into_iter().collect::<Vec<_>>(),
            "counts_lower_bound":true,
            "scope":"verified_prefix_lower_bound",
            "known_lower_bound_counts":lower,
            "distinct_source_coverage":if scoped_rows && !scoped_fields {"candidate_tasks_only"} else if scoped_fields {"candidate_and_scoped_tasks"} else {"candidate_tasks"},
            "distinct_lifecycle_coverage":if scoped_rows && !scoped_fields {"candidate_tasks_only"} else if scoped_fields {"candidate_and_scoped_tasks"} else {"candidate_tasks"},
            "sessions":sessions.into_iter().collect::<Vec<_>>(),
            "family_key_is_label_only":true,
            "reset":false,
            "source":"original_queues_and_lifecycle_ledgers_read_only"
        })
    } else {
        let mut value = lower;
        value["status"] = json!("available");
        value["counts_lower_bound"] = json!(false);
        value["scope"] = json!("verified_prefix");
        value["known_lower_bound_counts"] = Value::Null;
        value["distinct_source_coverage"] = json!(if scoped_rows && !scoped_fields {
            "candidate_tasks_only"
        } else if scoped_fields {
            "candidate_and_scoped_tasks"
        } else {
            "candidate_tasks"
        });
        value["distinct_lifecycle_coverage"] = json!(if scoped_rows && !scoped_fields {
            "candidate_tasks_only"
        } else if scoped_fields {
            "candidate_and_scoped_tasks"
        } else {
            "candidate_tasks"
        });
        value["sessions"] = json!(sessions.into_iter().collect::<Vec<_>>());
        value["family_key_is_label_only"] = json!(true);
        value["reset"] = json!(false);
        value["source"] = json!("original_queues_and_lifecycle_ledgers_read_only");
        value
    };
    if partial {
        for key in [
            "global_tested_trial_count",
            "admitted_trial_count",
            "executed_trial_count",
            "terminal_trial_count",
            "unknown_exposure_count",
            "distinct_terminal_tasks",
            "distinct_lifecycle_candidates",
            "distinct_sessions",
            "distinct_sources",
        ] {
            value[key] = Value::Null;
        }
    }
    value
}
