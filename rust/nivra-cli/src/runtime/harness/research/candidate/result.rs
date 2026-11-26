use super::*;

/// Explicit, host-built read-only selector for one prospective-observation
/// EPISODE. It is distinct from the authoring-parent ref: the observation task
/// is O, while the authoring candidate is A. A query by this ref returns the
/// verified continuation recorded for EXACTLY this O, never an arbitrary
/// sibling observation of the same A.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct SourceObservationRefV1 {
    pub version: u32,
    pub source_session: String,
    pub observation_task_id: String,
    pub observation_revision: u32,
    pub observation_job_id: String,
    pub observation_lifecycle_candidate_id: String,
    pub observation_result_sha256: Option<String>,
    pub observation_result_status: String,
    pub entry_sha256: Option<String>,
    pub authoring_task_id: String,
    pub authoring_parent_lifecycle_candidate_id: String,
    pub read_only: bool,
    pub activation_authorized: bool,
}

impl SourceObservationRefV1 {
    pub(crate) fn validate(&self) -> bool {
        let hash = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        self.version == 1
            && chrono::NaiveDate::parse_from_str(&self.source_session, "%Y-%m-%d").is_ok()
            && hash(&self.observation_task_id)
            && self.observation_revision == 1
            && hash(&self.observation_job_id)
            && hash(&self.observation_lifecycle_candidate_id)
            && match &self.observation_result_sha256 {
                Some(value) => hash(value) && self.observation_result_status == "verified",
                None => self.observation_result_status == "unavailable",
            }
            && self.entry_sha256.as_deref().is_none_or(hash)
            && hash(&self.authoring_task_id)
            && hash(&self.authoring_parent_lifecycle_candidate_id)
            && self.read_only
            && !self.activation_authorized
    }
}

pub(crate) fn source_observation_ref_from_task(
    record: &jobs::CandidateTaskRecord,
    score: &nivra_cli::candidate_execution::ObservationScoreV1,
) -> Result<Value> {
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &record.spec.purpose else {
        return Err("candidate_observation_target_missing".into());
    };
    let entry_sha256 = score
        .entry
        .as_ref()
        .map(|entry| serde_json::to_vec(entry).map(|bytes| digest(&bytes)))
        .transpose()?;
    let reference = SourceObservationRefV1 {
        version: 1,
        source_session: record.spec.session.clone(),
        observation_task_id: record.spec.task_id.clone(),
        observation_revision: record.spec.revision,
        observation_job_id: record.spec.job_id.clone(),
        observation_lifecycle_candidate_id: record.spec.lifecycle_candidate_id.clone(),
        observation_result_sha256: record.job.result_hash.clone(),
        observation_result_status: if record.job.result_hash.is_some() {
            "verified".into()
        } else {
            "unavailable".into()
        },
        entry_sha256,
        authoring_task_id: target.source_task_id.clone(),
        authoring_parent_lifecycle_candidate_id: target.source_lifecycle_candidate_id.clone(),
        read_only: true,
        activation_authorized: false,
    };
    if !reference.validate() {
        return Err("candidate_observation_ref_invalid".into());
    }
    Ok(serde_json::to_value(reference)?)
}

fn source_observation_ref(
    record: &jobs::CandidateTaskRecord,
    result: &CandidateResultV1,
) -> Result<Value> {
    let score = result
        .observation
        .as_ref()
        .ok_or("candidate_observation_score_missing")?;
    source_observation_ref_from_task(record, score)
}

/// Resolve one EXACT source-observation ref read-only through the verified
/// carry chain. It never opens an arbitrary source path, never launches or
/// replays work and never attaches a sibling observation's receipt.
pub(crate) fn candidate_result_for_observation_ref(
    c: &Config,
    cfg: &PlannerConfig,
    reference: &Value,
    now: i64,
) -> Result<Value> {
    // The explicit source-observation selector is available only when the
    // reviewed observation capability is configured. Omission keeps every
    // historical tool call/schema byte unchanged.
    let observation_enabled = cfg
        .research_tools
        .as_ref()
        .and_then(|settings| settings.candidate_compute.as_ref())
        .and_then(|candidate| candidate.observation.as_ref())
        .is_some_and(|observation| observation.enabled);
    if !observation_enabled {
        return Err("candidate_observation_disabled".into());
    }
    let reference: SourceObservationRefV1 = serde_json::from_value(reference.clone())
        .map_err(|_| "candidate_observation_ref_invalid")?;
    if !reference.validate() {
        return Err("candidate_observation_ref_invalid".into());
    }
    let chain = super::super::carry::predecessor_chain_verified_immutable(c)?;
    super::super::continuation::verify_lineage_prefix(c, cfg, &chain)?;
    let Some((_, source_directory, _)) = chain
        .iter()
        .find(|(session, _, _)| session == &reference.source_session)
    else {
        return Err("candidate_observation_ref_unavailable".into());
    };
    let mut queue = super::super::carry::open_original_queue_read_only_strict(
        c,
        cfg,
        &reference.source_session,
    )?;
    let task = jobs::load_candidate(
        &mut queue,
        &reference.observation_task_id,
        reference.observation_revision,
    )
    .map_err(|_| "candidate_observation_ref_unavailable")?;
    let Some(jobs::CandidatePurposeV1::Observe { target }) = &task.spec.purpose else {
        return Err("candidate_observation_ref_not_observation".into());
    };
    if task.spec.session != reference.source_session
        || task.spec.task_id != reference.observation_task_id
        || task.spec.revision != reference.observation_revision
        || task.spec.job_id != reference.observation_job_id
        || task.spec.lifecycle_candidate_id != reference.observation_lifecycle_candidate_id
        || target.source_task_id != reference.authoring_task_id
        || target.source_lifecycle_candidate_id != reference.authoring_parent_lifecycle_candidate_id
        || task.job.result_hash != reference.observation_result_sha256
    {
        return Err("candidate_observation_ref_identity_mismatch".into());
    }
    verify_task_book_binding(c, &chain, &task)?;
    let result = read_verified_candidate_result(
        &mut queue,
        &reference.observation_task_id,
        reference.observation_revision,
    )?
    .ok_or("candidate_observation_ref_result_unavailable")?;
    let score = result
        .observation
        .as_ref()
        .ok_or("candidate_observation_ref_result_unavailable")?;
    let entry_sha256 = score
        .entry
        .as_ref()
        .map(|entry| serde_json::to_vec(entry).map(|bytes| digest(&bytes)))
        .transpose()?;
    if entry_sha256 != reference.entry_sha256 {
        return Err("candidate_observation_ref_identity_mismatch".into());
    }
    let continuations =
        super::super::continuation::verified_continuations_from_chain(c, cfg, &chain)?;
    let continuation = continuations.iter().find(|continuation| {
        continuation["source_lifecycle_candidate_id"]
            == reference.observation_lifecycle_candidate_id.as_str()
    });
    // The native selector resolves the OBSERVATION O, but its provenance is the
    // ORIGINAL AUTHORING task A. Load and fully verify A against the target pins
    // before publishing any source result or parent ref. A tampered A result or
    // a target pointing at another Observation is UNVERIFIED context, never the
    // pinned authoring parent.
    let authoring =
        load_verified_authoring_parent(c, &chain, &mut queue, &reference.source_session, target);
    let (authoring_result, source_result_status, source_result_reason) = match authoring {
        Ok(authoring) => (Some(candidate_result_view(&authoring)), "verified", None),
        Err(error) => {
            let code = bounded_context_reason(error.as_ref());
            if code == "candidate_result_unverified" {
                (None, "unavailable", Some(code))
            } else {
                (None, "unverified", Some(code))
            }
        }
    };
    let context_status = if source_result_status == "verified" {
        "complete"
    } else {
        "partial"
    };
    let authoring_parent_ref = if source_result_status == "verified" {
        json!({
            "source_session":reference.source_session,
            "task_id":target.source_task_id,
            "revision":target.source_revision,
            "candidate_id":target.source_candidate_id,
            "lifecycle_candidate_id":target.source_lifecycle_candidate_id,
            "result_sha256":target.source_result_sha256,
            "runtime_hash":target.source_runtime_hash,
            "policy_sha256":target.policy_sha256,
            "read_only":true,
            "activation_authorized":false
        })
    } else {
        Value::Null
    };
    let mut response = json!({
        "source_observation_ref":serde_json::to_value(&reference)?,
        "source_session":reference.source_session,
        "observation_task_id":reference.observation_task_id,
        "observation_revision":reference.observation_revision,
        "observation_job_id":reference.observation_job_id,
        "observation_lifecycle_candidate_id":reference.observation_lifecycle_candidate_id,
        "source_result":authoring_result,
        "source_result_status":source_result_status,
        "authoring_parent_ref":authoring_parent_ref,
        "authoring_parent_ref_status":source_result_status,
        "read_only":true,
        "activation_authorized":false,
        "as_of_ms":now
    });
    if let Some(reason) = source_result_reason {
        response["source_result_reason"] = json!(reason);
    }
    match continuation {
        Some(continuation) => {
            response["parent_continuation"] = continuation.clone();
            response["parent_continuation_recorded_in_session"] =
                continuation["current_scope"]["current_session"].clone();
            response["parent_continuation_status"] = json!("verified");
        }
        None => {
            response["parent_continuation_status"] = json!("not_recorded");
        }
    }
    // The native selector reports the AUTHORING source replay status. A
    // changed/unavailable A is an explicit unavailable replay, never an
    // authoritative absence and never a copied financial result.
    response["replay"] = json!({
        "status": if source_result_status == "verified" { "available" } else { "unavailable" },
        "reason": source_result_reason,
        "source": "authoring_parent_a"
    });
    if context_status == "partial" {
        response["context_status"] = json!("partial");
    } else {
        response["context_status"] = json!("complete");
    }
    let _ = source_directory;
    Ok(response)
}

pub(crate) fn candidate_result(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    now: i64,
) -> Result<Value> {
    if let Some((mut db, record)) = archive_bars::load(c, cfg, task_id, revision)? {
        return archive_bars::result(&mut db, &record);
    }
    let (mut db, record) = candidate_task(c, cfg, task_id, revision)?;
    // Verify the recorded result before any lifecycle transition. A corrupt
    // binding is reported as an unverified result, never as a lifecycle fault.
    let result = read_verified_candidate_result(&mut db, task_id, revision)
        .map_err(|_| "candidate_result_unverified")?;
    let lifecycle = reconcile_candidate(c, cfg, &mut db, &record)
        .map_err(|_| "candidate_lifecycle_unavailable")?;
    let Some(result) = result else {
        return Ok(json!({
            "task":candidate_summary(&record),
            "result":null,
            "result_status":"not_verified",
            "lifecycle":lifecycle
        }));
    };
    let mut response = json!({
        "task":candidate_summary(&record),
        "result":candidate_result_view(&result),
        "result_status":"verified",
        "lifecycle":lifecycle
    });
    let mut context_status = "complete";
    if record.spec.purpose.is_none() {
        match super::super::carry::host_parent_ref(c, cfg, &mut db, &record) {
            Ok(reference) => {
                response["parent_ref"] = reference;
                response["parent_ref_status"] = json!("verified");
            }
            Err(error) => {
                // The own result may be verified, but the cross-session parent
                // context is explicitly unavailable. This is not the same fact
                // as "there is no parent".
                response["parent_ref_status"] = json!("unavailable");
                response["parent_ref_reason"] = json!(bounded_context_reason(error.as_ref()));
                context_status = "partial";
            }
        }
    }
    // The continuation identity is the OBSERVATION episode O, never the
    // authoring parent A. One authoring candidate may have several observation
    // episodes; only the exact O's verified receipt may be attached.
    if record.spec.purpose.is_some() {
        response["source_observation_ref"] = source_observation_ref(&record, &result)?;
        let observation_id = record.spec.lifecycle_candidate_id.clone();
        match super::super::verified_continuations(c, cfg) {
            Ok(continuations) => {
                if let Some(continuation) = continuations.iter().find(|continuation| {
                    continuation["source_lifecycle_candidate_id"] == observation_id.as_str()
                }) {
                    response["parent_continuation"] = continuation.clone();
                    response["parent_continuation_recorded_in_session"] =
                        continuation["current_scope"]["current_session"].clone();
                    response["parent_continuation_status"] = json!("verified");
                } else {
                    response["parent_continuation_status"] = json!("not_recorded");
                }
            }
            Err(error) => {
                response["parent_continuation_status"] = json!("unavailable");
                response["parent_continuation_reason"] =
                    json!(bounded_context_reason(error.as_ref()));
                context_status = "partial";
            }
        }
    } else {
        // An authoring candidate A has no observation episode by itself. It is
        // NOT the continuation identity; a single arbitrary O receipt must
        // never be attached to it. A bounded LIST of legitimate observation
        // episodes is safe and lets the model select the exact one.
        response["parent_continuation_status"] = json!("not_applicable");
        response["parent_continuation_reason"] = json!("no_source_observation_episode");
        let mut related = Vec::new();
        let mut related_truncated = false;
        let mut source_scan_truncated = false;
        let mut related_partial = false;
        let mut related_source_error_reason: Option<&'static str> = None;
        let mut scanned = 0usize;
        let mut cursor = None;
        let mut exhausted = false;
        let current_binding = book_binding(c).ok();
        let mut note_source_error = |code: &'static str| {
            related_partial = true;
            if related_source_error_reason.is_none() {
                related_source_error_reason = Some(code);
            }
        };
        for _ in 0..10 {
            let Ok(page) = jobs::list_candidates_page(
                &mut db,
                &record.spec.session,
                None,
                cursor.as_ref(),
                100,
            ) else {
                note_source_error("candidate_observation_task_unavailable");
                break;
            };
            if page.is_empty() {
                exhausted = true;
                break;
            }
            let full = page.len() == 100;
            scanned = scanned.saturating_add(page.len());
            cursor = page.last().map(|row| jobs::TaskCursor {
                created_at_ms: row.spec.created_at_ms,
                task_id: row.spec.task_id.clone(),
                revision: row.spec.revision,
            });
            for row in page {
                // A shared queue may hold a foreign book/account row. It is
                // never this authoring task's related episode and its ref is
                // never exposed.
                if current_binding.as_deref() != Some(row.spec.book_binding.as_str()) {
                    note_source_error("candidate_observation_book_binding_mismatch");
                    continue;
                }
                let Some(jobs::CandidatePurposeV1::Observe { target }) = &row.spec.purpose else {
                    continue;
                };
                // The related episode must point at THIS exact authoring task,
                // not merely echo its lifecycle id. A forged metadata-only
                // target is not a legitimate observation of A.
                if target.source_lifecycle_candidate_id != record.spec.lifecycle_candidate_id
                    || target.source_task_id != record.spec.task_id
                    || target.source_revision != record.spec.revision
                    || target.source_candidate_id != record.spec.candidate_id
                    || record.job.result_hash.as_deref()
                        != Some(target.source_result_sha256.as_str())
                {
                    continue;
                }
                let source_ref =
                    read_verified_candidate_result(&mut db, &row.spec.task_id, row.spec.revision)
                        .ok()
                        .flatten()
                        .and_then(|result| {
                            result.observation.as_ref().and_then(|score| {
                                source_observation_ref_from_task(&row, score).ok()
                            })
                        });
                related.push(json!({
                    "task_id":row.spec.task_id,
                    "revision":row.spec.revision,
                    "lifecycle_candidate_id":row.spec.lifecycle_candidate_id,
                    "source_session":row.spec.session,
                    "result_sha256":row.job.result_hash,
                    "source_observation_ref":source_ref,
                    "read_only":true
                }));
                // Buffer ONE row past the display bound: exactly 16 matches is
                // complete; only an actual 17th match is truncation.
                if related.len() > 16 {
                    related.truncate(16);
                    related_truncated = true;
                    break;
                }
            }
            if related_truncated {
                break;
            }
            if !full {
                exhausted = true;
                break;
            }
        }
        // The 10-page scan is bounded. If it ended on a FULL page, an extra row
        // may remain; probe exactly one row so a short list is never labelled
        // complete when the source scan was truncated.
        if !exhausted && !related_truncated && scanned >= 1_000 {
            match jobs::list_candidates_page(
                &mut db,
                &record.spec.session,
                None,
                cursor.as_ref(),
                1,
            ) {
                Ok(extra) if !extra.is_empty() => {
                    source_scan_truncated = true;
                }
                Ok(_) => {}
                Err(_) => note_source_error("candidate_observation_task_unavailable"),
            }
        }
        // A source error is never hidden behind a resource-bound truncation.
        let related_status = if related_partial {
            "partial"
        } else if related_truncated || source_scan_truncated {
            "truncated"
        } else {
            "available"
        };
        response["related_observations"] = json!(related);
        response["related_observations_status"] = json!(related_status);
        response["related_observations_truncated"] = json!(related_truncated);
        response["related_observations_partial"] = json!(related_partial);
        response["related_observations_source_scan_truncated"] = json!(source_scan_truncated);
        response["related_observations_scanned"] = json!(scanned);
        response["related_observations_limit"] = json!(16);
        response["related_observations_reason"] = if related_truncated {
            json!("observation_ref_limit")
        } else if source_scan_truncated {
            json!("source_scan_truncated")
        } else {
            Value::Null
        };
        response["related_observations_source_error_reason"] = json!(related_source_error_reason);
    }
    if let Some(jobs::CandidatePurposeV1::Observe { target }) = &record.spec.purpose {
        // Direct bounded lookup by candidate id: a target outside the 10k
        // library page is an authoritative absence, not a false not_matured.
        let (maturation, maturation_unavailable) =
            match experiment_ledger::lifecycle::observation_item(
                &c.directory,
                &record.spec.lifecycle_candidate_id,
            ) {
                Ok(Some(item)) => {
                    let measurement = &item["observation_measurement"];
                    let present = item["observation_measurement_present"]
                        .as_bool()
                        .unwrap_or(measurement.is_object());
                    if !present && measurement.is_null() {
                        // Absent/NULL is the authoritative not-yet-matured state.
                        (None, false)
                    } else if measurement.is_object() {
                        (Some(measurement.clone()), false)
                    } else {
                        // A PRESENT non-object body is corrupt evidence, never a
                        // silent not_matured and never raw financial data.
                        (None, true)
                    }
                }
                Ok(None) => (None, false),
                Err(_) => (None, true),
            };
        // Read the ORIGINAL verified result through the original queue locator.
        // This is a typed read-only source reference, never a caller hash. A
        // read error is UNAVAILABLE, not the authoritative absence of a result.
        // The SAME central loader used by the native observation-ref path pins
        // A's task/revision/candidate/lifecycle/result/policy/runtime/source/
        // params identity and refuses an Observation task as an authoring parent.
        let (source_result, source_result_status, source_result_reason) =
            match super::super::carry::open_original_queue_read_only_strict(
                c,
                cfg,
                &record.spec.session,
            ) {
                Ok(mut source_db) => {
                    match load_verified_authoring_parent(
                        c,
                        &[],
                        &mut source_db,
                        &record.spec.session,
                        target,
                    ) {
                        Ok(authoring) => {
                            let view = candidate_result_view(&authoring);
                            response["source_result"] = view.clone();
                            (Some(view), "verified", None)
                        }
                        Err(error) => {
                            // ANY unavailable/unverified authoring parent is an
                            // explicitly PARTIAL context. The observation's own
                            // result may still be verified, but A's result is
                            // never copied and the missing context is never
                            // reported as complete.
                            context_status = "partial";
                            let code = bounded_context_reason(error.as_ref());
                            if code == "candidate_result_unverified" {
                                (None, "unavailable", Some(code))
                            } else {
                                (None, "unverified", Some(code))
                            }
                        }
                    }
                }
                Err(error) => {
                    context_status = "partial";
                    (
                        None,
                        "unavailable",
                        Some(bounded_context_reason(error.as_ref())),
                    )
                }
            };
        response["source_result_status"] = json!(source_result_status);
        response["authoring_parent_ref_status"] = json!(source_result_status);
        response["authoring_parent_ref"] = if source_result_status == "verified" {
            json!({
                "source_session":record.spec.session,
                "task_id":target.source_task_id,
                "revision":target.source_revision,
                "candidate_id":target.source_candidate_id,
                "lifecycle_candidate_id":target.source_lifecycle_candidate_id,
                "result_sha256":target.source_result_sha256,
                "runtime_hash":target.source_runtime_hash,
                "policy_sha256":target.policy_sha256,
                "target_frozen_at_ms":target.target_frozen_at_ms,
                "window_from_ms":target.window_from_ms,
                "window_to_ms":target.window_to_ms,
                "holding_ms":target.holding_ms,
                "max_exit_delay_ms":target.max_exit_delay_ms,
                "snapshot_sha256":target.snapshot_sha256,
                "quotes_sha256":target.quotes_sha256,
                "spec_sha256":target.spec_sha256,
                "quote_keys":target.quote_keys,
                "quote_budget_truncated":target.quote_budget_truncated,
                "read_only":true,
                "activation_authorized":false
            })
        } else {
            Value::Null
        };
        if let Some(reason) = &source_result_reason {
            response["source_result_reason"] = json!(reason);
        }
        response["measurement"] = super::super::measure::measure_observation_with_status(
            c,
            cfg,
            &record,
            &result,
            maturation.as_ref(),
            source_result.as_ref(),
            source_result_status,
            source_result_reason,
            maturation_unavailable,
            now,
        );
    }
    if context_status == "partial" {
        response["context_status"] = json!("partial");
    }
    Ok(response)
}

/// Keep only a bounded static reason code. A raw error string, path or account
/// payload is never projected to the model.
pub(crate) fn bounded_context_reason(error: &dyn std::error::Error) -> &'static str {
    let raw = error.to_string();
    if raw.len() <= 64
        && !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        // The static codes are compile-time literals; map back to a literal so
        // the caller cannot receive a heap string built from arbitrary bytes.
        match raw.as_str() {
            "candidate_result_unverified" => "candidate_result_unverified",
            "candidate_lifecycle_unavailable" => "candidate_lifecycle_unavailable",
            "source_continue_ineligible" => "source_continue_ineligible",
            "source_continue_account_mismatch" => "source_continue_account_mismatch",
            "source_continue_result_unverified" => "source_continue_result_unverified",
            "source_continue_identity_mismatch" => "source_continue_identity_mismatch",
            "source_continue_cohort_changed" => "source_continue_cohort_changed",
            "source_continue_runtime_changed" => "source_continue_runtime_changed",
            "source_continue_lifecycle_unavailable" => "source_continue_lifecycle_unavailable",
            "source_continue_lifecycle_missing" => "source_continue_lifecycle_missing",
            "research_carry_session_not_verified" => "research_carry_session_not_verified",
            "research_queue_unavailable" => "research_queue_unavailable",
            "research_queue_identity_changed" => "research_queue_identity_changed",
            "research_queue_identity_invalid" => "research_queue_identity_invalid",
            "research_original_binding_missing" => "research_original_binding_missing",
            "research_queue_wal_uncheckpointed" => "research_queue_wal_uncheckpointed",
            "source_book_wal_uncheckpointed" => "source_book_wal_uncheckpointed",
            "source_lifecycle_wal_uncheckpointed" => "source_lifecycle_wal_uncheckpointed",
            "candidate_continuation_source_unverified" => {
                "candidate_continuation_source_unverified"
            }
            "candidate_continuation_identity_mismatch" => {
                "candidate_continuation_identity_mismatch"
            }
            "candidate_continuation_invalid" => "candidate_continuation_invalid",
            "candidate_observation_source_unavailable" => {
                "candidate_observation_source_unavailable"
            }
            "candidate_observation_source_unverified" => "candidate_observation_source_unverified",
            "candidate_observation_source_not_authoring" => {
                "candidate_observation_source_not_authoring"
            }
            "candidate_observation_target_invalid" => "candidate_observation_target_invalid",
            _ => "candidate_context_unavailable",
        }
    } else {
        "candidate_context_unavailable"
    }
}

pub(crate) fn candidate_cancel(
    c: &Config,
    cfg: &PlannerConfig,
    task_id: &str,
    revision: u32,
    now: i64,
) -> Result<Value> {
    if let Some((mut db, _)) = archive_bars::load(c, cfg, task_id, revision)? {
        let record = jobs::cancel_scoped(&mut db, task_id, revision, revision, now)
            .map_err(|_| "research_task_cancel_refused")?;
        return Ok(archive_bars::status(&record));
    }
    let (mut db, _) = candidate_task(c, cfg, task_id, revision)?;
    let record = jobs::cancel_candidate(&mut db, task_id, revision, revision, now)
        .map_err(|_| "candidate_task_cancel_refused")?;
    Ok(json!({
        "task":candidate_summary(&record),
        "cancellation":"requested",
        "terminal":task_state_terminal(record.status),
        "promotion_authorized":false,
        "activation_authorized":false
    }))
}

fn candidate_result_view(result: &CandidateResultV1) -> Value {
    let mut body = candidate_result_view_body(result);
    if let Some(observation) = &result.observation {
        body["observation"] = json!(observation);
        body["result_kind"] = json!(result.result_kind);
    }
    body
}

fn candidate_result_view_body(result: &CandidateResultV1) -> Value {
    let ticks = result
        .report
        .ticks
        .iter()
        .rev()
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|tick| {
            json!({
                "observed_ms":tick.observed_ms,
                "status":tick.status,
                "failure":tick.failure,
                "top_n_overlap":tick.top_n_overlap,
                "kendall":tick.kendall,
                "spearman":tick.spearman,
                "default_top_n":tick.default_top_n,
                "candidate_top_n":tick.candidate_top_n
            })
        })
        .collect::<Vec<_>>();
    let horizons = result
        .report
        .horizons
        .iter()
        .map(|horizon| {
            json!({
                "horizon_minutes":horizon.horizon_minutes,
                "ticks":horizon.ticks,
                "selected_rows":horizon.selected_rows,
                "missing_forward_marks":horizon.missing_forward_marks,
                "mean_forward_return_bps":horizon.mean_forward_return_bps,
                "cost_adjusted":horizon.cost_adjusted
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "schema":result.report.schema,
        "status":result.report.status,
        "reason":result.report.reason,
        "scorer_sha256":result.report.scorer_sha256,
        "params_sha256":result.report.params_sha256,
        "params_effective_sha256":result.report.params_effective_sha256,
        "dataset_hash":result.report.dataset_hash,
        "snapshots":result.report.snapshots,
        "valid_snapshots":result.report.valid_snapshots,
        "failed_snapshots":result.report.failed_snapshots,
        "min_valid_snapshots":result.report.min_valid_snapshots,
        "top_n":result.report.top_n,
        "mean_top_n_overlap":result.report.mean_top_n_overlap,
        "mean_kendall":result.report.mean_kendall,
        "mean_spearman":result.report.mean_spearman,
        "ticks":ticks,
        "ticks_omitted":result.report.ticks.len().saturating_sub(10),
        "horizons":horizons,
        "report_hash":result.report.report_hash
    });
    let mut body = json!({
        "version":result.version,
        "task_id":result.task_id,
        "revision":result.revision,
        "job_id":result.job_id,
        "candidate_id":result.candidate_id,
        "lifecycle_candidate_id":result.lifecycle_candidate_id,
        "abi":result.abi,
        "source_sha256":result.source_sha256,
        "params_sha256":result.params_sha256,
        "dataset_id":result.dataset_id,
        "dataset_hash":result.dataset_hash,
        "windows":result.windows,
        "horizon_minutes":result.horizon_minutes,
        "runtime_binding":result.runtime_binding,
        "sandbox":result.sandbox,
        "unit_policy":result.unit_policy,
        "unit_proof":result.unit_proof,
        "unit_proof_sha256":result.unit_proof_sha256,
        "result_produced_at_ms":result.result_produced_at_ms,
        "result_kind":result.result_kind,
        "evidence_scope":result.evidence_scope,
        "partial_evidence":result.partial_evidence,
        "gaps":result.gaps,
        "advisory":result.advisory,
        "qualified":result.qualified,
        "analysis_status":result.analysis_status,
        "report":report,
        "report_sha256":result.report_sha256,
        "promotion_authorized":result.promotion_authorized,
        "activation_authorized":result.activation_authorized,
        "edge_qualified":result.edge_qualified,
        "host_qualified":result.host_qualified
    });
    if let Some(execution) = &result.execution {
        body["execution"] = json!(execution);
        body["execution_status"] = json!(execution.execution_status);
        body["execution_result_kind"] = json!(execution.result_kind);
        body["action_tape_sha256"] = json!(execution.action_tape_sha256);
        body["execution_spec_sha256"] = json!(execution.execution_spec_sha256);
        body["execution_manifest_sha256"] = json!(execution.manifest_sha256);
        body["execution_events_sha256"] = json!(execution.events_sha256);
    }
    body
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn candidate_stopped(
    launch: &worker::CandidateLaunch,
    proof: worker::CandidateUnitProof,
    cause: jobs::CandidateStopCause,
    outcome: CandidateStopOutcome,
    failure_code: Option<&str>,
    result_sha256: Option<String>,
    result_at_ms: Option<i64>,
    result_bytes: Option<u64>,
    stopped_at_ms: i64,
) -> CandidateStopped {
    let checked_at_ms = proof.checked_at_ms;
    let failure_code = failure_code.map(|code| {
        code.chars()
            .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || *ch == '_')
            .take(64)
            .collect::<String>()
    });
    CandidateStopped {
        version: 2,
        kind: "candidate_unit_stopped".into(),
        job_id: launch.job_id.clone(),
        lease: launch.lease.clone(),
        task_id: launch.task_id.clone(),
        revision: launch.revision,
        candidate_id: launch.candidate_id.clone(),
        unit: launch.unit.clone(),
        proof,
        cause,
        outcome,
        failure_code: failure_code.filter(|code| !code.is_empty()),
        result_bytes,
        result_sha256,
        result_at_ms,
        source_sha256: launch.source_sha256.clone(),
        params_sha256: launch.params_sha256.clone(),
        dataset_hash: launch.dataset_hash.clone(),
        runtime_hash: launch.runtime_binding.runtime_hash.clone(),
        stopped_at_ms: stopped_at_ms.max(checked_at_ms),
    }
}

pub(super) fn stopped_hash(
    stopped: &CandidateStopped,
) -> std::result::Result<String, &'static str> {
    let bytes = serde_json::to_vec(stopped).map_err(|_| "candidate_stopped_invalid")?;
    Ok(digest(&bytes))
}

pub(crate) fn write_stopped(
    db: &Connection,
    launch: &worker::CandidateLaunch,
    stopped: &CandidateStopped,
) -> std::result::Result<(), &'static str> {
    let bytes = serde_json::to_vec(stopped).map_err(|_| "candidate_stopped_invalid")?;
    worker::save_receipt(
        db,
        &launch.job_id,
        &launch.lease,
        "candidate_stopped",
        &bytes,
    )
    .map_err(|_| "candidate_stopped_receipt_failed")
}
