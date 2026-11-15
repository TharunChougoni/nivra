use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn write_research_artifact_for_test(
    root: &std::path::Path,
    task: &super::super::owner_tasks::OwnerTask,
    turn: &session::Turn,
    output: Option<&super::super::owner_tasks::ResearchOutputV1>,
    failure: Option<&str>,
    completed_ms: i64,
) {
    use std::os::unix::fs::PermissionsExt;
    let directory = root.join("model/research");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let receipt = super::super::owner_tasks::ResearchReceipt {
        version: 1,
        evidence_id: task.research_identity().unwrap().evidence_id.clone(),
        output_sha256: output
            .map(|output| digest(&serde_json::to_vec(output).unwrap()))
            .unwrap_or_default(),
        model_id: turn.request["model_id"].as_str().unwrap().to_owned(),
        completed_ms,
        status: if output.is_some() {
            "completed".into()
        } else {
            "failed".into()
        },
    };
    let value = json!({
        "version":1,
        "task":task,
        "receipt":receipt,
        "output":output,
        "failure":failure,
        "evidence":Value::Null,
        "clock":{
            "started_wall_ms":turn.started_at_ms,
            "observed_wall_ms":completed_ms,
            "monotonic_elapsed_ms":completed_ms.saturating_sub(turn.started_at_ms),
        }
    });
    let path = directory.join(format!("{}.json", task.id().unwrap()));
    fs::write(&path, super::research::canonical_bytes(&value).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn insert_lifecycle_evidence_for_test(
    directory: &std::path::Path,
    candidate_id: &str,
    state: &str,
    replay_hash: Option<&str>,
) {
    let db = super::super::experiment_ledger::open_directory(directory).unwrap();
    db.execute("DELETE FROM experiment_lifecycle_state", [])
        .unwrap();
    db.execute("DELETE FROM experiment_lifecycle_specs", [])
        .unwrap();
    let spec = json!({
        "version":1,
        "session":"2026-10-16",
        "source_sha256":"a".repeat(64),
        "params_sha256":"b".repeat(64),
        "dataset_hash":"d".repeat(64),
        "feature_abi":"scanner_replay_v1",
        "hypothesis":"lifecycle evidence fixture",
        "training_from_ms":1,
        "training_to_ms":2,
        "selection_from_ms":3,
        "selection_to_ms":4,
        "holding_ms":1000,
        "max_exit_delay_ms":0
    });
    db.execute(
        "INSERT INTO experiment_lifecycle_specs(candidate_id,body,recorded_at_ms) VALUES(?,?,?)",
        rusqlite::params![candidate_id, spec.to_string(), 1i64],
    )
    .unwrap();
    let projection = json!({
        "version":1,
        "marks":[],
        "edge_qualified":false,
        "activation_authorized":false,
        "replay_report_sha256":replay_hash
    });
    db.execute(
        "INSERT INTO experiment_lifecycle_state(candidate_id,state,projection,updated_at_ms) VALUES(?,?,?,?)",
        rusqlite::params![candidate_id, state, projection.to_string(), 1i64],
    )
    .unwrap();
}

/// Run independent exercise chains in order. Each chain runs even when an
/// earlier one fails, and the final assert names every chain that broke
/// (#231). They share one `#[test]` only because of the 400-test budget.
#[cfg(test)]
pub(crate) fn run_named(cases: &[(&str, fn())]) {
    let mut failed = Vec::new();
    for (name, case) in cases {
        if std::panic::catch_unwind(case).is_err() {
            failed.push(*name);
        }
    }
    assert!(failed.is_empty(), "failed exercise chains: {failed:?}");
}

/// The supervisor exercise chains, split so one failure no longer hides the
/// rest (#231).
#[cfg(test)]
pub(crate) fn exercise_chains() -> [(&'static str, fn()); 5] {
    [
        ("model_selection", super::super::model_selection::exercise),
        ("model_select", super::super::model_select::exercise),
        ("supervisor", exercise),
        ("session", super::super::session::exercise),
        ("review", review::exercise),
    ]
}

#[cfg(test)]
pub(crate) fn exercise() {
    let cfg = PlannerConfig::default();
    let role = planner::ModelRole::default();
    let binding = resume_binding(&cfg, &role, "prompt-a").unwrap();
    assert_eq!(binding, resume_binding(&cfg, &role, "prompt-a").unwrap());
    // A selection revision of zero still binds effective default-role changes.
    let mut changed_role = role.clone();
    changed_role.model_id = "changed-default-model".into();
    assert_ne!(
        binding,
        resume_binding(&cfg, &changed_role, "prompt-a").unwrap()
    );
    let mut changed_profile = cfg.clone();
    changed_profile.experiment_profile = Some("changed-profile.json".into());
    assert_ne!(
        binding,
        resume_binding(&changed_profile, &role, "prompt-a").unwrap()
    );
    let mut changed_config = cfg.clone();
    changed_config.enabled = !changed_config.enabled;
    assert_ne!(
        binding,
        resume_binding(&changed_config, &role, "prompt-a").unwrap()
    );
    let pending_dir = tempfile::tempdir().unwrap();
    let pending_log = session::SessionLog::new(pending_dir.path(), "2026-10-26");
    pending_log
        .append(
            1,
            session::Body::Turn {
                turn: session::Turn {
                    id: "pending-binding".into(),
                    session: "2026-10-26".into(),
                    trigger: "idle".into(),
                    prompt_version: "prompt-a".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 1,
                    request: json!({"resume_binding_sha256":binding}),
                },
            },
        )
        .unwrap();
    let pending = pending_log.pending().unwrap();
    assert_eq!(
        check_resume_revisions(&pending, "prompt-a", "changed-binding"),
        Err("supervisor_config_revision_changed")
    );
    assert_eq!(pending_log.pending().unwrap(), pending);
    assert_eq!(
        check_resume_revisions(&pending, "prompt-a", &binding),
        Ok(())
    );
    assert_eq!(
        check_resume_revisions(&pending, "changed-prompt", &binding),
        Err("supervisor_prompt_revision_changed")
    );
    let mut invalid_binding = pending.clone();
    invalid_binding.request["resume_binding_sha256"] = Value::Null;
    assert_eq!(
        check_resume_revisions(&invalid_binding, "prompt-a", &binding),
        Err("supervisor_config_revision_changed")
    );
    let mut legacy_pending = pending.clone();
    legacy_pending.request = json!({"trigger":"idle"});
    assert_eq!(
        check_resume_revisions(&legacy_pending, "prompt-a", &binding),
        Ok(())
    );
    let bounds = Compaction::default();
    let legacy = json!({"notes":[],"trigger":"idle"});
    assert_eq!(
        render_session_turn("Review {{trigger}}", &legacy, &bounds).unwrap(),
        "Review idle"
    );
    let facts = json!({
        "session_context": {"schema_version":1,"summary":null,"exchanges":[{
            "turn":{"id":"prior-turn","request":{"evidence_id":"original-receipt"}},
            "result":{"outcome":"ok","reply":"retain hypothesis","actions":[{"id":"directive-1","digest":"original-digest"}]}
        }]},
        "notes": [], "seed":{"predecessor":"original-session"}
    });
    // Even a template without a history placeholder delivers committed context
    // and seed to the actual model request, including on deterministic replay.
    let prompt = render_session_turn("Review", &facts, &bounds).unwrap();
    assert!(prompt.contains("retain hypothesis"));
    assert!(prompt.contains("original-digest"));
    assert!(prompt.contains("original-receipt"));
    assert!(prompt.contains("original-session"));
    assert_eq!(
        prompt,
        render_session_turn("Review", &facts, &bounds).unwrap()
    );
    let exact = Compaction {
        max_request_bytes: escaped_prompt_bytes(&prompt) + bounds.reserve_bytes,
        ..bounds
    };
    assert!(render_session_turn("Review", &facts, &exact).is_ok());
    let too_small = Compaction {
        max_request_bytes: exact.max_request_bytes - 1,
        ..exact
    };
    assert_eq!(
        render_session_turn("Review", &facts, &too_small),
        Err("supervisor_context_limit")
    );
    // A failed turn can push history beyond the decision ceiling. Lifecycle
    // fitting keeps newest exchanges, declares omissions, and preserves facts.
    let mut oversized = facts.clone();
    oversized["session_context"]["exchanges"].as_array_mut().unwrap().insert(0,
        json!({"turn":{"id":"old-failed","request":{"digest":"x".repeat(1_100_000)}},"result":{"outcome":"failed","reason":"planner_call_timeout"}}));
    let original = oversized.clone();
    assert_eq!(
        render_session_turn("Review", &oversized, &bounds),
        Err("supervisor_context_limit")
    );
    let fitted = render_lifecycle_turn("Review", &mut oversized, &bounds).unwrap();
    assert!(fitted.len() as u64 + bounds.reserve_bytes <= bounds.max_request_bytes);
    assert!(fitted.contains("original-receipt"));
    assert_eq!(oversized["context_fit"]["omitted_exchanges"], 1);
    assert_eq!(oversized["notes"], original["notes"]);
    assert_eq!(oversized["seed"], original["seed"]);
    assert_eq!(
        render_session_turn("Review", &oversized, &bounds).unwrap(),
        fitted,
        "resume is byte identical"
    );
    assert!(original.to_string().len() > bounds.max_request_bytes as usize);
    // Replay the recovery against an append-only session: one failed provider
    // turn, a fitted compaction, then a decision that fits the normal gate.
    let recovery_dir = tempfile::tempdir().unwrap();
    let recovery_log = session::SessionLog::new(recovery_dir.path(), "2026-11-11");
    let original_request = original["session_context"]["exchanges"][0]["turn"]["request"].clone();
    recovery_log
        .append(
            1,
            session::Body::Turn {
                turn: session::Turn {
                    id: "failed-large-turn".into(),
                    session: "2026-11-11".into(),
                    trigger: "decision".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 1,
                    request: original_request.clone(),
                },
            },
        )
        .unwrap();
    recovery_log
        .append(
            2,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: "failed-large-turn".into(),
                    finished_at_ms: 2,
                    outcome: "failed".into(),
                    reason: Some("planner_call_timeout".into()),
                    reply: String::new(),
                    actions: vec![],
                },
            },
        )
        .unwrap();
    let mut recovery_facts = facts.clone();
    recovery_facts["session_context"] = recovery_log.conversation();
    assert!(compaction_due(
        recovery_log.turns_since_summary(),
        recovery_log.context_bytes(),
        &bounds
    ));
    assert_eq!(
        render_session_turn("Review", &recovery_facts, &bounds),
        Err("supervisor_context_limit")
    );
    render_lifecycle_turn("Review", &mut recovery_facts, &bounds).unwrap();
    // Current decision facts can exceed the cap while the measured history is
    // still under the compaction trigger. The refusal itself compacts history
    // instead of skipping every decision until the close (#219).
    let mut heavy_facts = facts.clone();
    heavy_facts["digest"] = json!("d".repeat(bounds.max_request_bytes as usize));
    assert!(!compaction_due(1, 1_000, &bounds));
    assert_eq!(
        render_session_turn("Review", &heavy_facts, &bounds),
        Err("supervisor_context_limit")
    );
    assert!(consider::compacts_on_refusal(
        "supervisor_context_limit",
        true,
        true,
        recovery_log.turns_since_summary()
    ));
    assert!(!consider::compacts_on_refusal(
        "supervisor_context_limit",
        true,
        true,
        0
    ));
    assert!(!consider::compacts_on_refusal(
        "supervisor_context_limit",
        false,
        true,
        1
    ));
    assert!(!consider::compacts_on_refusal(
        "supervisor_context_limit",
        true,
        false,
        1
    ));
    // A decision skipped inside a cycle that already compacted still compacts
    // on the next refusal; the skip names the compaction (#221).
    assert!(consider::compacts_on_refusal(
        "supervisor_context_limit_compacted",
        true,
        true,
        1
    ));
    assert!(!consider::compacts_on_refusal(
        "supervisor_model_revision_changed",
        true,
        true,
        1
    ));
    let compact = begin_test_lifecycle(
        recovery_dir.path(),
        "2026-11-11",
        "fitted-compaction",
        3,
        LifecycleTask::Compaction {
            up_to_seq: recovery_log.last_seq() + 1,
        },
    );
    assert!(handle_reply(
        recovery_dir.path(),
        &compact,
        4,
        "Failed provider turn; no decision committed. Older history explicitly omitted.",
        &[],
        &Limits::default(),
        100_000,
        None
    ));
    recovery_facts["session_context"] = recovery_log.conversation();
    recovery_facts["summary"] = json!(recovery_log.summary().unwrap().text);
    assert!(render_session_turn("Review", &recovery_facts, &bounds).is_ok());
    assert_eq!(
        recovery_log.turns()[0].request,
        original_request,
        "failed evidence is preserved"
    );
    assert_eq!(
        recovery_log.results()[0].reason.as_deref(),
        Some("planner_call_timeout")
    );
    assert_eq!(recovery_log.turns().len(), 2);
    // An oversized previous summary is also bounded, on UTF-8 boundaries.
    let mut large_summary = facts.clone();
    large_summary["summary"] = json!("😀".repeat(300_000));
    large_summary["session_context"]["summary"] =
        json!({"text":"😀".repeat(300_000),"up_to_seq":1});
    let summary_prompt = render_lifecycle_turn("{{summary}}", &mut large_summary, &bounds).unwrap();
    assert!(summary_prompt.len() as u64 + bounds.reserve_bytes <= bounds.max_request_bytes);
    assert!(
        large_summary["context_fit"]["omitted_summary_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    // Version failures are still refused, never treated as truncation requests.
    let mut wrong_version = facts.clone();
    wrong_version["session_context"]["schema_version"] = json!(2);
    assert_eq!(
        render_lifecycle_turn("Review", &mut wrong_version, &bounds),
        Err("supervisor_context_version_invalid")
    );
    let mut unsupported = facts.clone();
    unsupported["session_context"]["schema_version"] = json!(2);
    assert_eq!(
        render_session_turn("Review", &unsupported, &bounds),
        Err("supervisor_context_version_invalid")
    );
    assert!(compaction_due(0, bounds.max_request_bytes, &bounds));
    assert!(
        Compaction::from_profile(Some(
            &json!({"compaction":{"max_request_bytes":4096,"reserve_bytes":100}})
        ))
        .is_ok()
    );
    assert!(
        Compaction::from_profile(Some(&json!({"compaction":{"max_request_bytes":4095}}))).is_err()
    );
    assert!(!compaction_due(COMPACT_AFTER_TURNS - 1, 0, &bounds));
    assert!(compaction_due(COMPACT_AFTER_TURNS, 0, &bounds));
    let measured = Compaction {
        max_context_bytes: Some(1_000),
        reserve_bytes: 100,
        ..bounds
    };
    assert!(!compaction_due(0, 900, &measured));
    assert!(compaction_due(0, 901, &measured));
    assert!(
        Compaction::from_profile(Some(
            &json!({"compaction":{"max_turns":5,"max_context_bytes":10_000,"reserve_bytes":500}})
        ))
        .is_ok()
    );
    assert!(
        Compaction::from_profile(Some(
            &json!({"compaction":{"max_context_bytes":100,"reserve_bytes":100}})
        ))
        .is_err()
    );
    // #224: the request bound covers the full provider request, not only the
    // rendered prompt. A measured envelope makes a prompt that fits the old
    // prompt-only bound refuse, and lifecycle fitting omits history until the
    // whole request fits.
    let envelope_bounds = Compaction {
        request_envelope_bytes: Some(20_000),
        ..bounds
    };
    let mut full_facts = facts.clone();
    full_facts["digest"] = json!("d".repeat(200_000));
    let plain_prompt = render_session_turn("Review", &full_facts, &bounds).unwrap();
    let escaped = escaped_prompt_bytes(&plain_prompt);
    let prompt_only = Compaction {
        max_request_bytes: escaped + bounds.reserve_bytes + 1_000,
        ..bounds
    };
    assert!(request_fits(escaped, &prompt_only));
    let prompt_only_measured = Compaction {
        request_envelope_bytes: Some(20_000),
        ..prompt_only
    };
    // The prompt alone fits the configured cap, but the measured full request
    // does not: the bound must refuse it.
    assert!(!request_fits(escaped, &prompt_only_measured));
    assert_eq!(
        render_session_turn("Review", &full_facts, &prompt_only_measured),
        Err("supervisor_context_limit")
    );
    // The measured envelope also triggers compaction before the prompt-only
    // ceiling would.
    assert!(!compaction_due(0, 1_020_000, &bounds));
    assert!(compaction_due(0, 1_020_000, &envelope_bounds));
    let mut envelope_facts = facts.clone();
    envelope_facts["session_context"]["exchanges"]
        .as_array_mut()
        .unwrap()
        .insert(
            0,
            json!({"turn":{"id":"old-oversized","request":{"digest":"x".repeat(1_100_000)}},
                "result":{"outcome":"failed","reason":"model_response_incomplete"}}),
        );
    assert_eq!(
        render_session_turn("Review", &envelope_facts, &envelope_bounds),
        Err("supervisor_context_limit")
    );
    let envelope_fitted =
        render_lifecycle_turn("Review", &mut envelope_facts, &envelope_bounds).unwrap();
    assert!(request_fits(
        escaped_prompt_bytes(&envelope_fitted),
        &envelope_bounds
    ));
    assert_eq!(envelope_facts["context_fit"]["omitted_exchanges"], 1);
    // Thresholds and bounds are profile configuration, with safe defaults.
    let tuned = Compaction::from_profile(Some(&json!({"compaction":{
        "context_retry_limit":2,"context_backoff_ms":120_000,"near_cap_percent":80
    }})))
    .unwrap();
    assert_eq!(tuned.context_retry_limit, 2);
    assert_eq!(tuned.context_backoff_ms, 120_000);
    assert_eq!(tuned.near_cap_percent, 80);
    assert_eq!(bounds.context_retry_limit, 1);
    assert_eq!(bounds.context_backoff_ms, 900_000);
    assert_eq!(bounds.near_cap_percent, 90);
    assert!(bounds.request_envelope_bytes.is_none());
    assert!(
        Compaction::from_profile(Some(&json!({"compaction":{"context_retry_limit":0}}))).is_err()
    );
    assert!(
        Compaction::from_profile(Some(&json!({"compaction":{"context_backoff_ms":59_999}})))
            .is_err()
    );
    assert!(
        Compaction::from_profile(Some(&json!({"compaction":{"near_cap_percent":101}}))).is_err()
    );

    // #224: a provider-incomplete turn on a near-cap request is context
    // exhaustion. It records bounded evidence, starts one compaction retry in
    // the same cycle with the measured full-request envelope, then backs off when
    // that retry fails, and recovers once a retry succeeds.
    let exhaustion_dir = tempfile::tempdir().unwrap();
    let (exhaustion_c, mut exhaustion_cfg) =
        super::super::decision_owner::owner_fixture_config(exhaustion_dir.path());
    let exhaustion_model = exhaustion_dir.path().join("exhaustion-model.json");
    fs::write(&exhaustion_model, b"{}").unwrap();
    exhaustion_cfg.model_config = Some(exhaustion_model);
    let exhaustion_session = exhaustion_c.session.to_string();
    let exhaustion_log = session::SessionLog::new(&exhaustion_c.directory, &exhaustion_session);
    exhaustion_log
        .append(
            1,
            session::Body::Turn {
                turn: session::Turn {
                    id: "failed-oversized".into(),
                    session: exhaustion_session.clone(),
                    trigger: "idle".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 1,
                    request: json!({"digest":"x".repeat(1_100_000)}),
                },
            },
        )
        .unwrap();
    let (failed_sender, failed_receiver) = mpsc::sync_channel(1);
    let mut failed_worker = test_worker_with(
        failed_receiver,
        &exhaustion_session,
        "failed-oversized",
        RunKind::Supervisor,
    );
    failed_worker.bounds = Compaction {
        max_request_bytes: 1_048_576,
        ..Compaction::default()
    };
    failed_worker.escaped_prompt_bytes = 1_005_366;
    failed_worker.evidence = Arc::new(Mutex::new(Some(json!({
        "stage":"response_validation","http_status":200,
        "incomplete_reason":"max_output_tokens",
        "input_items":1,
        "request_bytes":1_105_225_u64,
        "prompt_bytes":1_005_366_u64,
        "escaped_prompt_bytes":1_005_366_u64,
        "tokens_in":1_105_225_u64,
        "tokens_out":0_u64,
    }))));
    failed_sender
        .send(Err("model_response_incomplete".into()))
        .unwrap();
    let limits = Limits::default();
    assert_eq!(
        poll(
            exhaustion_dir.path(),
            &failed_worker,
            &[],
            &limits,
            3,
            100_000,
            8,
            None
        ),
        Some(false)
    );
    let exhausted = load(exhaustion_dir.path(), &exhaustion_session).unwrap();
    let envelope = exhausted.request_envelope_bytes.unwrap();
    assert_eq!(envelope, 1_105_225 - 1_005_366);
    let recorded = exhausted.context_exhaustion.clone().unwrap();
    assert_eq!(recorded.reason, "supervisor_model_response_incomplete");
    assert_eq!(
        recorded.incomplete_reason.as_deref(),
        Some("max_output_tokens")
    );
    assert_eq!(recorded.request_bytes, Some(1_105_225));
    assert_eq!(recorded.max_request_bytes, 1_048_576);
    assert_eq!(recorded.retries, 0);
    let failure: Value = serde_json::from_slice(
        &fs::read(exhaustion_dir.path().join("model/supervisor-failure.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(failure["reason"], "supervisor_model_response_incomplete");
    assert_eq!(failure["request_bytes"], 1_105_225);
    assert_eq!(failure["incomplete_reason"], "max_output_tokens");
    assert_eq!(failure["tokens_in"], 1_105_225);
    // The same tick starts one bounded compaction retry with the measured envelope.
    let mut worker = None;
    let mut aside = None;
    let exhaustion_tick = |now: i64| Tick {
        now,
        minute: 556,
        in_session: true,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: 100_000,
        positions: json!({}),
    };
    consider(
        &exhaustion_c,
        &exhaustion_cfg,
        &mut worker,
        &mut aside,
        &exhaustion_tick(4),
        None,
    );
    let mut retried = worker.take().expect("in-cycle compaction retry");
    assert_eq!(retried.kind, RunKind::Compaction);
    assert_eq!(retried.bounds.request_envelope_bytes, Some(envelope));
    assert_eq!(
        load(exhaustion_dir.path(), &exhaustion_session)
            .unwrap()
            .context_exhaustion
            .as_ref()
            .unwrap()
            .retries,
        1
    );
    // The retry fails the same way. The bounded retry is exhausted, so the next
    // cycle backs off instead of sending the oversized request again.
    let (retry_sender, retry_receiver) = mpsc::sync_channel(1);
    retried.receiver = retry_receiver;
    retried.escaped_prompt_bytes = 900_000;
    retried.evidence = Arc::new(Mutex::new(Some(json!({
        "stage":"response_validation","http_status":200,
        "incomplete_reason":"max_output_tokens",
        "input_items":1,
        "request_bytes":990_000_u64,
        "prompt_bytes":900_000_u64,
        "escaped_prompt_bytes":900_000_u64,
    }))));
    retry_sender
        .send(Err("model_response_incomplete".into()))
        .unwrap();
    assert_eq!(
        poll(
            exhaustion_dir.path(),
            &retried,
            &[],
            &limits,
            5,
            100_000,
            8,
            None
        ),
        Some(false)
    );
    let backed_off = load(exhaustion_dir.path(), &exhaustion_session).unwrap();
    assert_eq!(backed_off.context_exhaustion.as_ref().unwrap().retries, 1);
    let mut worker = None;
    consider(
        &exhaustion_c,
        &exhaustion_cfg,
        &mut worker,
        &mut aside,
        &exhaustion_tick(6),
        None,
    );
    assert!(
        worker.is_none(),
        "backoff must not start another compaction"
    );
    let skipped =
        serde_json::to_value(load(exhaustion_dir.path(), &exhaustion_session).unwrap()).unwrap();
    assert!(
        skipped["skips"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skip| skip["reason"] == "supervisor_context_exhausted_backoff")
    );
    // After the configured backoff one fresh bounded attempt is allowed.
    let backoff = bounds.context_backoff_ms;
    consider(
        &exhaustion_c,
        &exhaustion_cfg,
        &mut worker,
        &mut aside,
        &exhaustion_tick(5 + backoff),
        None,
    );
    let mut recovered = worker.take().expect("bounded retry after backoff");
    assert_eq!(recovered.kind, RunKind::Compaction);
    // A completed compaction is the recovery evidence: exhaustion clears and
    // the measured envelope stays available for later decisions.
    let (ok_sender, ok_receiver) = mpsc::sync_channel(1);
    recovered.receiver = ok_receiver;
    ok_sender.send(Ok("bounded summary".into())).unwrap();
    assert_eq!(
        poll(
            exhaustion_dir.path(),
            &recovered,
            &[],
            &limits,
            5 + backoff + 1,
            100_000,
            8,
            None
        ),
        Some(true)
    );
    let recovered_state = load(exhaustion_dir.path(), &exhaustion_session).unwrap();
    assert!(recovered_state.context_exhaustion.is_none());
    // The latest provider call re-measured the envelope (990,000-900,000); it
    // stays available for later renders.
    assert_eq!(recovered_state.request_envelope_bytes, Some(90_000));
    assert!(
        exhaustion_log.summary().is_some(),
        "successful compaction appends its summary"
    );
    // The bounded retry/backoff policy is a pure decision over durable state.
    let exhaustion = ContextExhaustion {
        reason: "supervisor_model_response_incomplete".into(),
        incomplete_reason: Some("max_output_tokens".into()),
        request_bytes: Some(1_105_225),
        max_request_bytes: 1_048_576,
        at_ms: 1_000,
        retries: 0,
    };
    assert!(consider::context_recovery_due(&exhaustion, &bounds, 1_000));
    let mut retried_once = exhaustion.clone();
    retried_once.retries = 1;
    assert!(!consider::context_recovery_due(
        &retried_once,
        &bounds,
        1_001
    ));
    assert!(consider::context_recovery_due(
        &retried_once,
        &bounds,
        1_000 + bounds.context_backoff_ms
    ));
    let mut backing_off = State::fresh("2026-11-13");
    backing_off.context_exhaustion = Some(retried_once);
    assert!(consider::context_backoff_active(
        &backing_off,
        &bounds,
        1_001
    ));
    assert!(!consider::context_backoff_active(
        &backing_off,
        &bounds,
        1_000 + bounds.context_backoff_ms
    ));
    assert!(consider::compacts_on_refusal(
        "supervisor_context_exhausted",
        true,
        true,
        1
    ));
    assert!(!consider::compacts_on_refusal(
        "supervisor_context_exhausted",
        true,
        true,
        0
    ));
    // A content-filtered or normal-size incomplete response is not context
    // exhaustion: evidence is retained but no compaction is forced.
    for (incomplete, request_bytes) in [
        (json!("content_filter"), 1_100_000_u64),
        (json!("max_output_tokens"), 100_000_u64),
    ] {
        let small_dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::sync_channel(1);
        let mut small = test_worker_with(
            receiver,
            "2026-11-13",
            "small-incomplete",
            RunKind::Supervisor,
        );
        small.escaped_prompt_bytes = request_bytes.saturating_sub(10_000);
        small.evidence = Arc::new(Mutex::new(Some(json!({
            "stage":"response_validation","http_status":200,
            "incomplete_reason":incomplete,
            "input_items":1,
            "request_bytes":request_bytes,
            "escaped_prompt_bytes":request_bytes.saturating_sub(10_000),
        }))));
        sender
            .send(Err("model_response_incomplete".into()))
            .unwrap();
        assert_eq!(
            poll(small_dir.path(), &small, &[], &limits, 1, 100_000, 8, None),
            Some(false)
        );
        assert!(
            load(small_dir.path(), "2026-11-13")
                .unwrap()
                .context_exhaustion
                .is_none()
        );
    }
    let wake = Wake::default();
    let mut state = State::fresh("2026-10-16");
    let input = Inputs {
        minute: 600,
        in_session: true,
        fill_count: 0,
        enabled: true,
        in_flight: false,
        budget_left: true,
    };
    let hashes = AppliedHashes::default();
    assert_eq!(
        due(&state, &wake, 1_000, &input, &hashes),
        Some("session_open")
    );
    state.open_fired = true;
    state.last_run_ms = Some(1_000);
    state.applied = Some(hashes.clone());
    assert_eq!(due(&state, &wake, 2_000, &input, &hashes), None);
    state.luna_no_decide = 8;
    assert_eq!(
        due(&state, &wake, 1_000 + wake.min_interval_ms, &input, &hashes),
        Some("luna_no_decide_streak")
    );
    state.luna_no_decide = 0;
    let filled = Inputs {
        fill_count: 1,
        ..input
    };
    assert_eq!(
        due(
            &state,
            &wake,
            1_000 + wake.min_interval_ms,
            &filled,
            &hashes
        ),
        Some("post_fill")
    );
    let disabled = Inputs {
        enabled: false,
        ..filled
    };
    assert_eq!(due(&state, &wake, 9_000_000, &disabled, &hashes), None);
    let dir = tempfile::tempdir().unwrap();
    record_skip(dir.path(), "2026-10-16", 10, "supervisor_disabled");
    record_skip(dir.path(), "2026-10-16", 20, "supervisor_disabled");
    let saved = load(dir.path(), "2026-10-16").unwrap();
    assert_eq!(saved.skips.len(), 1);
    for n in 1..=40 {
        record_skip(
            dir.path(),
            "2026-10-16",
            n * 60_000,
            "supervisor_context_limit",
        );
    }
    let latest_skips = serde_json::to_value(load(dir.path(), "2026-10-16").unwrap()).unwrap();
    assert_eq!(latest_skips["skips"].as_array().unwrap().len(), 32);
    assert_eq!(latest_skips["skips"][0]["at_ms"], 9 * 60_000);
    assert_eq!(latest_skips["skips"][31]["at_ms"], 40 * 60_000);
    // A corrupt state file is refused, not silently replaced: a fresh session
    // would lose the handoff counts (#156).
    let corrupt = dir.path().join(STATE_PATH);
    fs::write(&corrupt, b"{not json").unwrap();
    assert_eq!(
        load(dir.path(), "2026-10-16").unwrap_err(),
        "supervisor_state_corrupt"
    );
    note_luna(dir.path(), "2026-10-16", "decide");
    assert_eq!(fs::read(&corrupt).unwrap(), b"{not json");
    assert!(dir.path().join("model/supervisor-error.json").is_file());
    fs::remove_file(&corrupt).unwrap();
    // Concurrent note writers hold one load-modify-save section, so no handoff
    // count is lost between lock acquisitions (#156).
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let root = dir.path().to_path_buf();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    note_luna(&root, "2026-10-16", "decide");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(load(dir.path(), "2026-10-16").unwrap().decides, 400);
    // A decide handed off during a run is not marked seen: finish records only
    // the snapshot the turn was built from (#154).
    let session = "2026-10-17";
    with_state(dir.path(), session, |state| {
        *state = State::fresh(session);
        state.open_fired = true;
        state.last_run_ms = Some(5_000);
        state.applied = Some(hashes.clone());
    })
    .unwrap();
    note_luna(dir.path(), session, "decide");
    let snapshot = load(dir.path(), session).unwrap();
    note_luna(dir.path(), session, "decide");
    finish(
        dir.path(),
        session,
        RunKind::Supervisor,
        "session_open",
        5_000 + wake.min_interval_ms,
        true,
        &hashes,
        snapshot.decides,
        None,
    );
    let after = load(dir.path(), session).unwrap();
    assert_eq!((after.decides, after.seen_decides), (2, 1));
    assert_eq!(
        due(
            &after,
            &wake,
            5_000 + 2 * wake.min_interval_ms,
            &input,
            &hashes
        ),
        Some("post_decide")
    );
    // A failed instruction store is a failed run, not an ok one; the next wake
    // retries and a successful store clears the failure (#155).
    let session = "2026-10-18";
    let blocked = dir.path().join("model/directives.ndjson");
    fs::create_dir_all(&blocked).unwrap();
    let reply = json!({"summary":"ok","wake_luna":false,"directives":[{"text":"watch","target":"luna","ttl_ms":60000}]}).to_string();
    let worker = begin_turn(dir.path(), session, "turn-fail", 10_000);
    assert!(!handle_reply(
        dir.path(),
        &worker,
        10_000,
        &reply,
        &[],
        &Limits::default(),
        100_000,
        None,
    ));
    let failed = load(dir.path(), session).unwrap();
    assert_eq!((failed.runs, failed.failures), (1, 1));
    assert!(dir.path().join("model/supervisor-failure.json").is_file());
    let results = session::SessionLog::new(dir.path(), session).results();
    assert_eq!(results.last().unwrap().outcome, "failed");
    fs::remove_dir(&blocked).unwrap();
    let retry = begin_turn(dir.path(), session, "turn-retry", 20_000);
    assert!(handle_reply(
        dir.path(),
        &retry,
        20_000,
        &reply,
        &[],
        &Limits::default(),
        100_000,
        None,
    ));
    assert_eq!(load(dir.path(), session).unwrap().failures, 0);
    let results = session::SessionLog::new(dir.path(), session).results();
    assert_eq!(results.last().unwrap().outcome, "ok");
    assert_eq!(results.last().unwrap().actions.len(), 1);
    // A note that arrives mid-turn invalidates the commit. The turn records a
    // stale_notes result and no directive is stored (#143).
    let session = "2026-10-19";
    let worker = begin_turn(dir.path(), session, "turn-stale", 30_000);
    session::Notes::new(dir.path(), session)
        .add("new operator note", false, 30_001)
        .unwrap();
    assert!(!handle_reply(
        dir.path(),
        &worker,
        30_002,
        &reply,
        &[],
        &Limits::default(),
        100_000,
        None,
    ));
    let logged = session::SessionLog::new(dir.path(), session);
    assert_eq!(logged.results().last().unwrap().outcome, "stale_notes");
    assert!(session::actions_for(dir.path(), session, "turn-stale").is_empty());
    let retry = load(dir.path(), session).unwrap();
    assert_eq!(retry.note_reruns, 1);
    assert!(
        retry.last_run_ms.is_none(),
        "a stale turn retries on the next tick"
    );

    // Compaction appends a summary; the log keeps every turn.
    let worker = begin_test_lifecycle(
        dir.path(),
        "2026-10-20",
        "turn-compact",
        40_000,
        LifecycleTask::Compaction { up_to_seq: 1 },
    );
    assert!(handle_reply(
        dir.path(),
        &worker,
        40_001,
        "older turns summarised",
        &[],
        &Limits::default(),
        100_000,
        None,
    ));
    let logged = session::SessionLog::new(dir.path(), "2026-10-20");
    assert_eq!(logged.summary().unwrap().text, "older turns summarised");
    assert_eq!(logged.turns().len(), 1, "compaction never rewrites turns");
    assert!(logged.context_bytes() >= "older turns summarised".len() as u64);
    assert!(
        load(dir.path(), "2026-10-20")
            .unwrap()
            .compaction_failed
            .is_none()
    );
    // A failed compaction records the bounded reason; a later success clears it.
    let failed_session = "2026-10-25";
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(Err("supervisor_output_invalid".into()))
        .unwrap();
    let worker = test_worker_with(
        receiver,
        failed_session,
        "turn-compact-fail",
        RunKind::Compaction,
    );
    assert_eq!(
        poll(
            dir.path(),
            &worker,
            &[],
            &Limits::default(),
            41_000,
            100_000,
            8,
            None
        ),
        Some(false)
    );
    assert_eq!(
        load(dir.path(), failed_session)
            .unwrap()
            .compaction_failed
            .as_ref()
            .unwrap()
            .reason,
        "supervisor_output_invalid"
    );
    let retry = begin_test_lifecycle(
        dir.path(),
        failed_session,
        "turn-compact-retry",
        42_000,
        LifecycleTask::Compaction {
            up_to_seq: session::SessionLog::new(dir.path(), failed_session).last_seq() + 1,
        },
    );
    assert!(handle_reply(
        dir.path(),
        &retry,
        42_001,
        "recovered summary",
        &[],
        &Limits::default(),
        100_000,
        None,
    ));
    assert!(
        load(dir.path(), failed_session)
            .unwrap()
            .compaction_failed
            .is_none()
    );

    // Handoff writes tomorrow's seed before non-keep notes expire (#144).
    let session = "2026-10-21";
    let notes = session::Notes::new(dir.path(), session);
    let keep = notes.add("carry this", true, 50_000).unwrap();
    let temporary = notes.add("today only", false, 50_001).unwrap();
    let prior_log = session::SessionLog::new(dir.path(), session);
    prior_log
        .append(
            49_990,
            session::Body::Turn {
                turn: session::Turn {
                    id: "prior-session-turn".into(),
                    session: session.into(),
                    trigger: "post_fill".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 49_990,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    prior_log
        .append(
            49_991,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: "prior-session-turn".into(),
                    finished_at_ms: 49_991,
                    outcome: "ok".into(),
                    reason: None,
                    reply: "completed".into(),
                    actions: vec![],
                },
            },
        )
        .unwrap();
    let directives = json!([{"id":"dir-carry","text":"continue research"}]);
    let unresolved = json!([{"id":"alert-open","reason":"unresolved"}]);
    std::fs::create_dir_all(dir.path().join("model")).unwrap();
    std::fs::write(
        dir.path().join("model/agent-requests.json"),
        json!({"version":1,"instruction_revisions":{},"requests":[
            {"id":"request-17","kind":"ask","session":session,"idempotency_key":"idem-17","request_sha256":"a".repeat(64),"state":"queued","created_at_ms":49_000,"updated_at_ms":49_000,"expires_at_ms":60_000,"revision":0},
            {"id":"request-expired","kind":"ask","session":session,"idempotency_key":"idem-expired","request_sha256":"b".repeat(64),"state":"queued","created_at_ms":40_000,"updated_at_ms":40_000,"expires_at_ms":49_999,"revision":0}
        ]}).to_string(),
    ).unwrap();
    let mut handoff_config = super::super::tests::config(dir.path());
    handoff_config.session = session.parse().unwrap();
    let paper_position = sadr_core::execution::Position {
        instrument: sadr_core::execution::Instrument {
            key: "NSE_EQ|A".into(),
            symbol: "A".into(),
            series: "EQ".into(),
            tick: sadr_core::finance::Price::parse("0.05").unwrap(),
        },
        units: 3,
        entry: sadr_core::finance::Price::parse("100").unwrap(),
        mark: sadr_core::finance::Price::parse("101").unwrap(),
        marked_at: 56_000,
        exit_intent: Some(sadr_core::execution::ExitIntent {
            target: Some(sadr_core::finance::Price::parse("110").unwrap()),
            stop: Some(sadr_core::finance::Price::parse("95").unwrap()),
            time: Some(60_000),
        }),
        decision_id: Some("executable-decision".into()),
    };
    let handoff_facts = json!({"positions":[serde_json::to_value(&paper_position).unwrap()],"active_directives":directives,"alerts":unresolved});
    let prepared = handoff_lifecycle(
        &handoff_config,
        &cfg,
        HandoffBuildContext {
            session: session.into(),
            target: "2026-10-22".into(),
            turn_id: "turn-handoff".into(),
            standing_instructions: "paper-only standing instructions".into(),
            facts: handoff_facts,
        },
        &hashes,
        50_002,
    );
    let LifecycleTask::Handoff { seed, .. } = prepared.task else {
        panic!("handoff lifecycle expected")
    };
    let worker = begin_test_lifecycle(
        dir.path(),
        session,
        "turn-handoff",
        50_002,
        LifecycleTask::Handoff {
            target_session: "2026-10-22".into(),
            seed: seed.clone(),
        },
    );
    persist_handoff_seed(dir.path(), worker.lifecycle.as_ref().unwrap()).unwrap();
    persist_lifecycle_artifact_for(
        dir.path(),
        session,
        worker.lifecycle.as_ref().unwrap(),
        50_003,
        Some("handoff summary"),
        None,
    )
    .unwrap();
    let pending_handoff = session::SessionLog::new(dir.path(), session)
        .pending()
        .unwrap();
    let mut wrong_source = pending_handoff.clone();
    wrong_source.request["host_lifecycle"]["task"]["seed"]["source_turn_id"] =
        json!("different-source-turn");
    assert_eq!(
        lifecycle_for(&wrong_source).unwrap_err(),
        "supervisor_lifecycle_invalid"
    );
    assert!(recover_lifecycle(dir.path(), &pending_handoff, 50_004).unwrap());
    let after_recovery = session::SessionLog::new(dir.path(), session).records();
    assert!(recover_lifecycle(dir.path(), &pending_handoff, 50_005).unwrap());
    assert_eq!(
        session::SessionLog::new(dir.path(), session)
            .records()
            .len(),
        after_recovery.len()
    );
    assert_eq!(
        session::SessionLog::new(dir.path(), session)
            .result_for("turn-handoff")
            .unwrap()
            .outcome,
        "recovered_lifecycle_receipt"
    );
    let target_seed = session::seed_for_request(dir.path(), "2026-10-22").unwrap();
    assert_eq!(target_seed, seed);
    assert_eq!(target_seed["source_session"], session);
    assert_eq!(target_seed["target_session"], "2026-10-22");
    assert_eq!(target_seed["schema_version"], 2);
    assert_eq!(target_seed["source_turn_id"], "turn-handoff");
    assert_eq!(target_seed["source_receipt"]["available"], true);
    assert_eq!(
        target_seed["source_receipt"]["turn_id"],
        "prior-session-turn"
    );
    assert_eq!(target_seed["source_receipt"]["result_seq"], 2);
    assert_eq!(
        target_seed["source_receipt"]["result_sha256"]
            .as_str()
            .unwrap()
            .len(),
        64
    );
    assert_eq!(
        target_seed["standing_instructions"],
        "paper-only standing instructions"
    );
    assert_eq!(target_seed["keep_notes"][0]["id"], keep.id);
    assert_eq!(target_seed["positions"][0]["instrument"]["key"], "NSE_EQ|A");
    assert_eq!(target_seed["positions"][0]["units"], 3);
    assert_eq!(target_seed["positions"][0]["marked_at"], 56_000);
    assert_eq!(target_seed["positions"][0]["entry"], "100");
    assert_eq!(target_seed["positions"][0]["mark"], "101");
    assert!(target_seed["positions"][0]["instrument"]["tick"].is_string());
    assert!(target_seed["positions"][0].get("stop").is_none());
    assert!(target_seed["positions"][0].get("target").is_none());
    assert!(target_seed["positions"][0].get("exit_intent").is_none());
    assert!(target_seed["positions"][0].get("decision_id").is_none());
    assert_eq!(target_seed["directives"], directives);
    assert_eq!(
        target_seed["unresolved_work_refs"]["refs"][0]["request_id"],
        "request-17"
    );
    assert_eq!(
        target_seed["unresolved_work_refs"]["refs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(target_seed["unresolved_alerts"], unresolved);
    // The successor gets only the host seed: no copied executable intent,
    // quote authority or pending turn comes across the day boundary.
    assert!(target_seed.get("orders").is_none());
    assert!(target_seed.get("quotes").is_none());
    let target_log = session::SessionLog::new(dir.path(), "2026-10-22");
    let mut successor_context = json!({"session":"2026-10-22","trigger":"decision","session_context":{"schema_version":1,"summary":null,"exchanges":[]},"owner_digest":{"eligible":true}});
    session::add_seed_fact(dir.path(), "2026-10-22", &mut successor_context).unwrap();
    let owner_turn_prompt = render_session_turn(
        "Owner decision, prior seed={{seed}}",
        &successor_context,
        &Compaction::from_profile(None).unwrap(),
    )
    .unwrap();
    assert!(owner_turn_prompt.contains("paper-only standing instructions"));
    assert!(owner_turn_prompt.contains("NSE_EQ|A"));
    assert!(!owner_turn_prompt.contains("exit_intent"));
    assert!(!owner_turn_prompt.contains("executable-decision"));
    assert!(target_log.pending().is_none());
    assert!(target_log.handoff().is_none());
    let handoff = session::SessionLog::new(dir.path(), session)
        .handoff()
        .unwrap();
    assert_eq!(handoff.target_session, "2026-10-22");
    assert_eq!(handoff.seed, seed);
    let after = notes.snapshot();
    assert_eq!(
        after
            .notes
            .iter()
            .find(|note| note.id == temporary.id)
            .unwrap()
            .state,
        session::NoteState::Expired
    );
    assert_eq!(
        after
            .notes
            .iter()
            .find(|note| note.id == keep.id)
            .unwrap()
            .state,
        session::NoteState::Pinned
    );
    assert!(
        session::SessionLog::new(dir.path(), session)
            .handoff()
            .is_some()
    );
    // Reopen a compaction after its summary committed but before its result.
    // The original prefix, rather than a later clock/log sample, is retained.
    let crash_session = "2026-10-28";
    let compact = begin_test_lifecycle(
        dir.path(),
        crash_session,
        "compact-crash",
        55_000,
        LifecycleTask::Compaction { up_to_seq: 1 },
    );
    let crash_log = session::SessionLog::new(dir.path(), crash_session);
    let original = crash_log.pending().unwrap();
    assert_eq!(
        lifecycle_for(&original).unwrap().unwrap().kind(),
        RunKind::Compaction
    );
    persist_lifecycle_artifact(
        dir.path(),
        &compact,
        55_001,
        Some("exact-prefix summary"),
        None,
    )
    .unwrap();
    assert!(crash_log.pending().is_some());
    assert_eq!(crash_log.summary().unwrap().up_to_seq, 1);
    assert!(recover_lifecycle(dir.path(), &original, 55_002).unwrap());
    let recovered = crash_log.result_for(&original.id).unwrap();
    assert_eq!(recovered.outcome, "recovered_lifecycle_receipt");
    assert_eq!(recovered.actions[0].kind, "compaction");
    assert_eq!(recovered.reply, "exact-prefix summary");
    let committed = crash_log.records();
    assert!(recover_lifecycle(dir.path(), &original, 55_003).unwrap());
    assert_eq!(crash_log.records(), committed);

    // Crash after the terminal Result frame but before State::finish. The
    // result's original clock/hash/trigger replay exactly once with its cursor.
    let result_crash_session = "2026-10-30";
    let result_crash = begin_test_lifecycle(
        dir.path(),
        result_crash_session,
        "compact-result-crash",
        56_000,
        LifecycleTask::Compaction { up_to_seq: 1 },
    );
    let result_crash_log = session::SessionLog::new(dir.path(), result_crash_session);
    let result_turn = result_crash_log.pending().unwrap();
    let lifecycle = result_crash.lifecycle.as_ref().unwrap();
    result_crash_log
        .append(
            56_001,
            session::Body::Summary {
                summary: session::Summary {
                    up_to_seq: 1,
                    text: "durable summary".into(),
                    created_at_ms: 56_001,
                },
            },
        )
        .unwrap();
    let result = session::TurnResult {
        turn_id: result_turn.id.clone(),
        finished_at_ms: 56_002,
        outcome: "ok".into(),
        reason: None,
        reply: "durable summary".into(),
        actions: vec![lifecycle_action(
            &result_turn.id,
            &session::Body::Summary {
                summary: session::Summary {
                    up_to_seq: 1,
                    text: "durable summary".into(),
                    created_at_ms: 56_001,
                },
            },
        )],
    };
    result_crash_log
        .append(56_002, session::Body::Result { result })
        .unwrap();
    with_state(dir.path(), result_crash_session, |state| {
        state.runs = 4;
        state.failures = 2;
        state.last_run_ms = Some(40_000);
        state.compaction_failed = Some(CompactionFailure {
            reason: "supervisor_output_invalid".into(),
            at_ms: 55_000,
        });
    })
    .unwrap();
    assert!(reconcile_lifecycle_results(dir.path(), result_crash_session).unwrap());
    let reconciled = load(dir.path(), result_crash_session).unwrap();
    assert_eq!(reconciled.runs, 5);
    assert_eq!(
        reconciled.failures, 2,
        "compaction cannot erase decision failures"
    );
    assert_eq!(reconciled.last_run_ms, Some(56_002));
    assert_eq!(reconciled.applied, Some(lifecycle.hashes.clone()));
    assert_eq!(reconciled.lifecycle_result_seq, 3);
    assert!(reconciled.compaction_failed.is_none());
    let state_bytes = serde_json::to_value(&reconciled).unwrap();
    assert!(!reconcile_lifecycle_results(dir.path(), result_crash_session).unwrap());
    assert_eq!(
        serde_json::to_value(load(dir.path(), result_crash_session).unwrap()).unwrap(),
        state_bytes
    );

    let duplicate_session = "2026-10-31";
    let duplicate_worker = begin_test_lifecycle(
        dir.path(),
        duplicate_session,
        "duplicate-result-turn",
        57_000,
        LifecycleTask::Compaction { up_to_seq: 1 },
    );
    let duplicate_log = session::SessionLog::new(dir.path(), duplicate_session);
    let duplicate_turn = duplicate_log.pending().unwrap();
    duplicate_log
        .append(
            57_001,
            session::Body::Summary {
                summary: session::Summary {
                    up_to_seq: 1,
                    text: "summary".into(),
                    created_at_ms: 57_001,
                },
            },
        )
        .unwrap();
    let duplicate_result = session::TurnResult {
        turn_id: duplicate_turn.id,
        finished_at_ms: 57_002,
        outcome: "ok".into(),
        reason: None,
        reply: "summary".into(),
        actions: vec![],
    };
    duplicate_log
        .append(
            57_002,
            session::Body::Result {
                result: duplicate_result.clone(),
            },
        )
        .unwrap();
    duplicate_log
        .append(
            57_003,
            session::Body::Result {
                result: duplicate_result,
            },
        )
        .unwrap();
    assert_eq!(
        reconcile_lifecycle_results(dir.path(), duplicate_session),
        Err("supervisor_lifecycle_result_duplicate")
    );
    let duplicate_state = load(dir.path(), duplicate_session).unwrap();
    assert_eq!(duplicate_state.runs, 0);
    assert_eq!(duplicate_state.lifecycle_result_seq, 0);
    assert_eq!(duplicate_worker.lifecycle.as_ref().unwrap().hashes, hashes);

    let mut bad = original.clone();
    bad.request["host_lifecycle"]["version"] = json!(2);
    assert_eq!(
        lifecycle_for(&bad).unwrap_err(),
        "supervisor_lifecycle_invalid"
    );
    bad.request = json!({});
    assert_eq!(
        lifecycle_for(&bad).unwrap_err(),
        "supervisor_legacy_lifecycle_pending"
    );
    assert_eq!(
        crash_log.records(),
        committed,
        "refusals never rewrite the original log"
    );
    assert_eq!(session_trigger("compaction"), SessionTrigger::Compaction);
    assert_eq!(session_trigger("handoff"), SessionTrigger::Handoff);

    // A provider failure leaves the already-written host carry available and
    // a terminal handoff failure, without expiring the original instructions.
    let failed_handoff_session = "2026-10-29";
    let notes = session::Notes::new(dir.path(), failed_handoff_session);
    let keep = notes
        .add("carry after provider failure", true, 56_000)
        .unwrap();
    let carry = session::build_seed(
        dir.path(),
        failed_handoff_session,
        "2026-10-30",
        &session::SeedContext {
            source_turn_id: "handoff-failed".into(),
            standing_instructions: "standing prompt".into(),
            positions: serde_json::to_value(std::slice::from_ref(&paper_position)).unwrap(),
            directives: json!([]),
            alerts: json!([]),
            research_state: json!({"available":false,"reason":"no_durable_research_state_source"}),
            unresolved_work_refs: super::super::agent_cli::unresolved_request_refs(
                dir.path(),
                failed_handoff_session,
                56_001,
            ),
            review_state: Value::Null,
            workspace_state: Value::Null,
        },
    );
    let mut failed_handoff = begin_test_lifecycle(
        dir.path(),
        failed_handoff_session,
        "handoff-failed",
        56_001,
        LifecycleTask::Handoff {
            target_session: "2026-10-30".into(),
            seed: carry.clone(),
        },
    );
    persist_handoff_seed(dir.path(), failed_handoff.lifecycle.as_ref().unwrap()).unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    failed_handoff.receiver = receiver;
    sender.send(Err("model_timeout".into())).unwrap();
    assert_eq!(
        poll(
            dir.path(),
            &failed_handoff,
            &[],
            &Limits::default(),
            56_002,
            100_000,
            8,
            None
        ),
        Some(false)
    );
    assert_eq!(
        session::seed_for_request(dir.path(), "2026-10-30").unwrap(),
        carry
    );
    assert_eq!(notes.keep_for_seed()[0].id, keep.id);
    let failed_log = session::SessionLog::new(dir.path(), failed_handoff_session);
    assert_eq!(
        failed_log.handoff().unwrap().failure.as_deref(),
        Some("supervisor_call_timeout")
    );
    assert!(failed_log.pending().is_none());

    // Successor selection uses the verified calendar, including a weekday
    // holiday; dates beyond its published coverage are explicitly unavailable.
    let mut calendar_config = super::super::tests::config(dir.path());
    calendar_config.session = "2026-10-19".parse().unwrap();
    assert_eq!(next_trading_day(&calendar_config).unwrap(), "2026-10-21");
    calendar_config.session = "2026-12-31".parse().unwrap();
    assert_eq!(
        next_trading_day(&calendar_config),
        Err("supervisor_successor_calendar_unavailable")
    );

    // An exhausted allowance still produces a host-only handoff exactly once.
    calendar_config.session = "2026-11-04".parse().unwrap();
    let standing_path = dir.path().join("parked-standing.md");
    fs::write(&standing_path, "Keep paper-only standing instructions").unwrap();
    let mut parked_cfg = cfg.clone();
    parked_cfg.orchestrator = Some(
        super::super::orchestrator_model::OrchestratorModelSettings {
            system_prompt: Some(standing_path),
            ..Default::default()
        },
    );
    let facts = json!({"positions":[],"active_directives":[],"alerts":[],"decides_today":4});
    host_handoff_failure(
        &calendar_config,
        &parked_cfg,
        &facts,
        57_000,
        &hashes,
        "supervisor_parked_budget",
    )
    .unwrap();
    let parked_log = session::SessionLog::new(dir.path(), "2026-11-04");
    let once = parked_log.records();
    assert_eq!(
        parked_log.handoff().unwrap().failure.as_deref(),
        Some("supervisor_parked_budget")
    );
    assert_eq!(
        session::seed_for_request(dir.path(), "2026-11-05").unwrap()["source_session"],
        "2026-11-04"
    );
    host_handoff_failure(
        &calendar_config,
        &parked_cfg,
        &facts,
        57_001,
        &hashes,
        "supervisor_parked_budget",
    )
    .unwrap();
    assert_eq!(parked_log.records(), once);
    // A read-only aside publishes its reply and clears only its own request.
    let session = "2026-10-23";
    let directory = dir.path().join(session::ROOT_PREFIX).join(session);
    fs::create_dir_all(&directory).unwrap();
    let request =
        json!({"id":"aside-1","session":session,"question":"why skipped?","at_ms":60_000});
    rollover::atomic(&directory.join("aside-request.json"), &request).unwrap();
    let parsed = aside_request(dir.path(), session).unwrap();
    assert_eq!(parsed.question, "why skipped?");
    let (sender, receiver) = mpsc::sync_channel(1);
    sender.send(Ok("answer".into())).unwrap();
    let worker = test_worker_with(receiver, session, "aside-1", RunKind::Aside);
    assert_eq!(poll_aside(dir.path(), &worker, 60_001), Some(true));
    assert!(!directory.join("aside-request.json").exists());
    let reply: Value =
        serde_json::from_slice(&fs::read(directory.join("aside-reply.json")).unwrap()).unwrap();
    assert_eq!(reply["reply"], "answer");
    assert_eq!(reply["read_only"], true);
    // A budget-exhausted turn parks the session with a visible detailed reason
    // instead of repeating a zero-token failed run every wake (#161, #162/A0).
    let session = "2026-10-24";
    let (sender, receiver) = mpsc::sync_channel(1);
    sender.send(Err("planner_budget_exhausted".into())).unwrap();
    let worker = test_worker_with(receiver, session, "turn-budget", RunKind::Supervisor);
    assert_eq!(
        poll(
            dir.path(),
            &worker,
            &[],
            &Limits::default(),
            70_000,
            100_000,
            1,
            None
        ),
        Some(false)
    );
    let parked = load(dir.path(), session).unwrap();
    assert_eq!(
        parked.parked.as_ref().unwrap().reason,
        "supervisor_budget_exhausted"
    );
    let failure: Value = serde_json::from_slice(
        &fs::read(dir.path().join("model/supervisor-failure.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(failure["reason"], "supervisor_budget_exhausted");
    let no_allowance = Inputs {
        minute: 600,
        in_session: true,
        fill_count: 0,
        enabled: true,
        in_flight: false,
        budget_left: parked.parked.is_none(),
    };
    assert_eq!(due(&parked, &wake, 9_000_000, &no_allowance, &hashes), None);
    let parked_budget = parked.parked.as_ref().unwrap();
    assert!(!allowance_recovery_allowed(parked_budget, 1, 1, true));
    assert!(!allowance_recovery_allowed(parked_budget, 1, 0, true));
    assert!(!allowance_recovery_allowed(parked_budget, 2, 2, true));
    assert!(!allowance_recovery_allowed(parked_budget, 3, 3, true));
    assert!(!allowance_recovery_allowed(parked_budget, 3, 2, false));
    assert!(allowance_recovery_allowed(parked_budget, 3, 2, true));
    let legacy_park = Parked {
        reason: parked_budget.reason.clone(),
        at_ms: parked_budget.at_ms,
        budget_at_park: None,
    };
    assert!(!allowance_recovery_allowed(&legacy_park, 3, 2, true));
    let unrelated_park = Parked {
        reason: "supervisor_call_failed".into(),
        at_ms: parked_budget.at_ms,
        budget_at_park: Some(1),
    };
    assert!(!allowance_recovery_allowed(&unrelated_park, 3, 2, true));
    let mut recovered = parked.clone();
    if allowance_recovery_allowed(parked_budget, 3, 2, true) {
        recovered.parked = None;
    }
    assert!(recovered.parked.is_none());
    assert_eq!(recovered.failures, parked.failures);
    assert_eq!(recovered.last_run_ms, parked.last_run_ms);
    let refreshed_allowance = Inputs {
        budget_left: true,
        ..no_allowance
    };
    assert_eq!(
        due(&recovered, &wake, 9_000_000, &refreshed_allowance, &hashes),
        Some("session_open")
    );
    assert_eq!(
        supervisor_failure_reason("model_body_invalid"),
        "supervisor_model_body_invalid"
    );
    assert_eq!(
        supervisor_failure_reason("model_timeout"),
        "supervisor_call_timeout"
    );
    assert_eq!(
        supervisor_failure_reason("model_http_401 secret body"),
        "supervisor_model_http_401"
    );
    assert_eq!(
        supervisor_failure_reason("model_http_429 Too Many Requests token=secret"),
        "supervisor_model_http_429"
    );
    assert_eq!(
        supervisor_failure_reason("model_http_502 upstream detail"),
        "supervisor_model_http_502"
    );
    assert_eq!(
        supervisor_failure_reason("model_http_999 invalid"),
        "supervisor_call_failed"
    );
    // One deterministic local error retains its code and blocks identical
    // contracts across ticks/reloads; transport errors never trip this circuit.
    let contract_dir = tempfile::tempdir().unwrap();
    let contract_session = "2026-11-11";
    let failed_contract = begin_turn(
        contract_dir.path(),
        contract_session,
        "tool-preflight-failed",
        1,
    );
    assert!(!fail_reply(
        contract_dir.path(),
        &failed_contract,
        2,
        "harness_tool_definitions_changed",
        None
    ));
    let contract_state = load(contract_dir.path(), contract_session).unwrap();
    assert!(contract_state.tool_contract_blocked(&failed_contract.hashes));
    assert_eq!(
        contract_state.tool_failure.as_ref().unwrap().reason,
        "harness_tool_definitions_changed"
    );
    for n in 1..=23 {
        assert_eq!(
            due(
                &contract_state,
                &wake,
                n * 900_000,
                &input,
                &failed_contract.hashes
            ),
            None
        );
    }
    assert_eq!(load(contract_dir.path(), contract_session).unwrap().runs, 1);
    assert!(!contract_state.tool_contract_blocked(&AppliedHashes {
        config: "reviewed-change".into(),
        ..failed_contract.hashes.clone()
    }));
    let mut transient = State::fresh(contract_session);
    transient.block_tool_failure("planner_call_timeout", 1, &hashes);
    transient.block_tool_failure("model_http_503", 2, &hashes);
    assert!(transient.tool_failure.is_none());
    assert!(due(&transient, &wake, 9_000_000, &input, &hashes).is_some());
    // Lifecycle success preserves the decision streak for diagnosis and wakes.
    let mut streak = State::fresh(contract_session);
    streak.failures = wake.failure_streak;
    streak.open_fired = true;
    for (kind, reason) in [
        (RunKind::Handoff, "handoff"),
        (RunKind::Compaction, "compaction"),
        (RunKind::Aside, "aside"),
    ] {
        apply_finish(&mut streak, kind, reason, 3, true, &hashes, 0, None);
        assert_eq!(streak.failures, wake.failure_streak);
    }
    assert_eq!(
        due(&streak, &wake, 9_000_000, &input, &hashes),
        Some("failure_streak")
    );
    apply_finish(
        &mut streak,
        RunKind::Decision,
        "decision",
        4,
        true,
        &hashes,
        0,
        None,
    );
    assert_eq!(streak.failures, 0);
    // A failure streak waits for the cooldown instead of relaunching every tick.
    let mut failing = State::fresh("2026-10-16");
    failing.open_fired = true;
    failing.failures = wake.failure_streak;
    failing.last_run_ms = Some(5_000);
    failing.applied = Some(hashes.clone());
    assert_eq!(due(&failing, &wake, 5_001, &input, &hashes), None);
    assert_eq!(
        due(
            &failing,
            &wake,
            5_000 + wake.min_interval_ms,
            &input,
            &hashes
        ),
        Some("failure_streak")
    );
    // Run first: a fresh state fires immediately; the same applied tuple after a
    // restart never refires; one changed tuple produces one debounced wake.
    let mut first = State::fresh("2026-10-16");
    first.open_fired = true;
    first.last_run_ms = Some(5_000);
    first.applied = Some(hashes.clone());
    let stable = 5_000 + wake.min_interval_ms;
    assert_eq!(due(&first, &wake, stable, &input, &hashes), None);
    let mut changed = hashes.clone();
    changed.profile = "changed-profile".into();
    assert_eq!(due(&first, &wake, stable, &input, &changed), Some("change"));
    first.change_last_ms = Some(stable);
    assert_eq!(due(&first, &wake, stable + 1, &input, &changed), None);
    let mut burst = changed.clone();
    burst.config = "changed-config".into();
    assert_eq!(due(&first, &wake, stable + 1, &input, &burst), None);
    assert_eq!(
        due(
            &first,
            &wake,
            stable + wake.change_min_gap_ms,
            &input,
            &burst
        ),
        Some("change")
    );
    first.change_fired = Some(burst.clone());
    first.last_run_ms = Some(stable + 2 * wake.change_min_gap_ms);
    assert_eq!(
        due(
            &first,
            &wake,
            stable + 2 * wake.change_min_gap_ms,
            &input,
            &burst
        ),
        None
    );
    // A slow or failed supervisor releases Luna at the hold deadline.
    assert!(should_hold(true, 10_000, 9_999));
    assert!(!should_hold(true, 10_000, 10_000));
    assert!(!should_hold(false, 10_000, 9_999));
    let overlaid = wake
        .overlay(&json!({
            "run_first":{"on_session_start":false,"on_resume":false,"on_change":true,"hold_luna_ms":30_000},
            "change_min_gap_ms":120_000
        }))
        .unwrap();
    assert!(!overlaid.run_first.on_session_start);
    assert!(!overlaid.run_first.on_resume);
    assert!(overlaid.run_first.on_change);
    assert_eq!(overlaid.run_first.hold_luna_ms, 30_000);
    assert_eq!(overlaid.change_min_gap_ms, 120_000);
    assert!(
        wake.overlay(&json!({"run_first":{"hold_luna_ms":900_001}}))
            .is_err()
    );
    // The harness composes the objective and configured supervisor prompt; the
    // wake message is the only user content and carries no tools.
    let text = super::super::harness::system_instructions("supervisor", &[]).unwrap();
    assert!(text.contains("intraday supervisor"));
    assert_eq!(
        super::super::harness::configured_definitions_with(
            "supervisor",
            &[],
            false,
            false,
            &super::super::planner::ToolsSettings::default()
        )
        .unwrap(),
        Vec::<Value>::new()
    );
    // Directives are read from the final text, not from the provider envelope.
    let envelope = json!({"output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"{\"summary\":\"ok\",\"wake_luna\":true,\"directives\":[]}"}]}],"model":"m"}).to_string();
    let reply = reply_text(&envelope).unwrap();
    let (accepted, rejected, wake_luna) =
        directives::parse_output(&reply, &[], &Limits::default(), 0, 1);
    assert!(accepted.is_empty() && rejected.is_empty() && wake_luna);
    assert!(reply_text("{\"directives\":[]}").is_none());

    // A controller restart finds a durable decision turn and its running ledger
    // row but no committed result. The host refuses the orphaned work with its
    // original evidence instead of resending an ambiguous model request.
    let interrupted_dir = tempfile::tempdir().unwrap();
    let interrupted_now = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:16:00+05:30")
        .unwrap()
        .timestamp_millis();
    let (interrupted_c, interrupted_cfg, mut interrupted_db, interrupted_p, claim) =
        super::super::decision_owner::interruption_fixture(interrupted_dir.path(), interrupted_now);
    let log =
        session::SessionLog::new(&interrupted_c.directory, &interrupted_c.session.to_string());
    log.append(
        interrupted_now,
        session::Body::Turn {
            turn: session::Turn {
                id: claim.turn_id.clone(),
                session: interrupted_c.session.to_string(),
                trigger: "decision".into(),
                prompt_version: "fixture".into(),
                notes_version: 0,
                decides_at_start: 0,
                fills_at_start: 0,
                started_at_ms: interrupted_now,
                request: json!({"host_decision": claim}),
            },
        },
    )
    .unwrap();
    let mut worker = None;
    let mut aside = None;
    let tick = Tick {
        now: interrupted_now + 1,
        minute: 556,
        in_session: true,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: interrupted_now + 3_600_000,
        positions: json!({}),
    };
    let mut context = super::super::decision_owner::Context {
        db: &mut interrupted_db,
        portfolio: &interrupted_p,
        instruments: &[],
    };
    consider_owner(
        &interrupted_c,
        &interrupted_cfg,
        &mut worker,
        &mut aside,
        &tick,
        None,
        &mut context,
    );
    assert!(worker.is_none(), "interrupted decision must not restart");
    let ledger: String = interrupted_db
        .query_row(
            "SELECT state FROM owner_digest_work WHERE turn_id=?",
            [&claim.turn_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ledger, "refused");
    let result = log.result_for(&claim.turn_id).unwrap();
    assert_eq!(result.outcome, "interrupted");
    assert_eq!(result.reason.as_deref(), Some("owner_decision_interrupted"));

    // The committed-receipt side of the same restart: the SQLite plan and
    // receipt exist, but the session result was not appended. Recovery projects
    // the original action receipt without another model call.
    let recovered_dir = tempfile::tempdir().unwrap();
    let recovered_now = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:17:00+05:30")
        .unwrap()
        .timestamp_millis();
    let (recovered_c, recovered_cfg, mut recovered_db, recovered_p, claim) =
        super::super::decision_owner::interruption_fixture(recovered_dir.path(), recovered_now);
    let instrument = super::super::decision_owner::exercise_instrument();
    let mut context = super::super::decision_owner::Context {
        db: &mut recovered_db,
        portfolio: &recovered_p,
        instruments: std::slice::from_ref(&instrument),
    };
    let plan = super::super::decision_owner::commit(
        &mut context,
        &recovered_c,
        &recovered_cfg,
        &claim,
        "{\"actions\":[],\"stance\":\"hold\",\"notes\":\"fixture\"}",
        recovered_now + 1,
    )
    .unwrap();
    assert!(
        super::super::decision_owner::committed(&recovered_db, &recovered_c, &claim)
            .unwrap()
            .is_some()
    );
    let log = session::SessionLog::new(&recovered_c.directory, &recovered_c.session.to_string());
    log.append(
        recovered_now,
        session::Body::Turn {
            turn: session::Turn {
                id: claim.turn_id.clone(),
                session: recovered_c.session.to_string(),
                trigger: "decision".into(),
                prompt_version: "fixture".into(),
                notes_version: 0,
                decides_at_start: 0,
                fills_at_start: 0,
                started_at_ms: recovered_now,
                request: json!({
                    "host_decision": claim,
                    "owner_applied_hashes": AppliedHashes::current(&recovered_cfg),
                }),
            },
        },
    )
    .unwrap();
    let pending = log.pending().unwrap();
    let mut context = super::super::decision_owner::Context {
        db: &mut recovered_db,
        portfolio: &recovered_p,
        instruments: std::slice::from_ref(&instrument),
    };
    assert!(
        recover_decision(
            &recovered_c,
            &pending,
            &mut context,
            recovered_now + 2,
            None
        )
        .unwrap()
    );
    assert!(log.pending().is_none());
    let result = log.result_for(&claim.turn_id).unwrap();
    assert_eq!(result.outcome, "recovered_decision_receipt");
    assert_eq!(result.actions.len(), 1);
    assert_eq!(result.actions[0].kind, "paper_plan");
    assert_eq!(result.actions[0].id, plan["decision_id"].as_str().unwrap());

    // A restart with a committed session action but no result appends the
    // recovered receipt once and never resends the turn to a model.
    let action_dir = tempfile::tempdir().unwrap();
    let action_now = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:18:00+05:30")
        .unwrap()
        .timestamp_millis();
    let action_c = Config {
        directory: action_dir.path().to_path_buf(),
        ..interrupted_c.clone()
    };
    let action_log = session::SessionLog::new(&action_c.directory, &action_c.session.to_string());
    let turn_id = "turn-action-recovered";
    action_log
        .append(
            action_now,
            session::Body::Turn {
                turn: session::Turn {
                    id: turn_id.into(),
                    session: action_c.session.to_string(),
                    trigger: "supervisor".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: action_now,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    let action = session::action("directive", "dir-recovered", &json!({"text":"wait"}));
    session::commit_action(
        &action_c.directory,
        &action_c.session.to_string(),
        turn_id,
        &action,
    )
    .unwrap();
    let mut worker = None;
    let mut aside = None;
    let action_tick = Tick {
        now: action_now + 1,
        minute: 556,
        in_session: true,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: action_now + 3_600_000,
        positions: json!({}),
    };
    consider(
        &action_c,
        &interrupted_cfg,
        &mut worker,
        &mut aside,
        &action_tick,
        None,
    );
    assert!(
        worker.is_none(),
        "recovered action must not start a model turn"
    );
    assert!(action_log.pending().is_none());
    let result = action_log.result_for(turn_id).unwrap();
    assert_eq!(result.outcome, "recovered_action_receipt");
    assert_eq!(result.actions.len(), 1);
    assert_eq!(result.actions[0].id, "dir-recovered");
    // A second restart sees the result and does not append a duplicate.
    consider(
        &action_c,
        &interrupted_cfg,
        &mut worker,
        &mut aside,
        &action_tick,
        None,
    );
    assert_eq!(
        action_log
            .turns()
            .iter()
            .filter(|turn| turn.id == turn_id)
            .count(),
        1
    );
    assert_eq!(
        action_log
            .turns()
            .iter()
            .filter(|turn| action_log.result_for(&turn.id).is_some())
            .count(),
        1
    );

    // A model switch requested while a supervisor turn is in flight remains
    // pending until that turn has a durable result, then applies to the next
    // turn without resetting the session or its turn history.
    let switch_dir = tempfile::tempdir().unwrap();
    let default_model = switch_dir.path().join("default-model.json");
    let grok_model = switch_dir.path().join("grok-model.json");
    fs::write(&default_model, b"{}").unwrap();
    fs::write(&grok_model, b"{}").unwrap();
    let mut switch_value = serde_json::to_value(&interrupted_cfg).unwrap();
    switch_value["orchestrator"]["model"]["model_config"] = json!(default_model.to_string_lossy());
    switch_value["orchestrator"]["model_profiles"]["grok"] = json!({
        "model_id": "grok-4.7",
        "transport": "direct_oauth",
        "model_config": grok_model.to_string_lossy(),
    });
    switch_value["orchestrator"]["admitted_models"] = json!(["grok"]);
    let switch_cfg: planner::PlannerConfig = serde_json::from_value(switch_value).unwrap();
    let switch_c = Config {
        directory: switch_dir.path().to_path_buf(),
        ..interrupted_c.clone()
    };
    super::super::model_selection::request(
        &switch_c,
        &switch_cfg,
        "grok",
        Some(0),
        Some("inflight-switch"),
    )
    .unwrap();
    let switch_log = session::SessionLog::new(&switch_c.directory, &switch_c.session.to_string());
    let switch_turn = "turn-inflight-switch";
    switch_log
        .append(
            action_now + 10,
            session::Body::Turn {
                turn: session::Turn {
                    id: switch_turn.into(),
                    session: switch_c.session.to_string(),
                    trigger: "supervisor".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: action_now + 10,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    session::commit_action(
        &switch_c.directory,
        &switch_c.session.to_string(),
        switch_turn,
        &session::action("directive", "dir-switch", &json!({"text":"hold"})),
    )
    .unwrap();
    let mut worker = None;
    let mut aside = None;
    let tick = |now: i64, in_session: bool| Tick {
        now,
        minute: 556,
        in_session,
        fill_count: 0,
        allowlist: &[],
        session_end_ms: action_now + 3_600_000,
        positions: json!({}),
    };
    consider(
        &switch_c,
        &switch_cfg,
        &mut worker,
        &mut aside,
        &tick(action_now + 11, true),
        None,
    );
    assert!(switch_log.pending().is_none());
    let shown = super::super::model_selection::show(&switch_c, &switch_cfg).unwrap();
    assert_eq!(shown["active"]["profile"], "default");
    assert_eq!(shown["pending"]["profile"], "grok");
    // A settled boundary with no pending turn applies the request and keeps the
    // same session identity and append-only history.
    consider(
        &switch_c,
        &switch_cfg,
        &mut worker,
        &mut aside,
        &tick(action_now + 12, false),
        None,
    );
    let resolved = super::super::model_selection::resolve(&switch_c, &switch_cfg)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.profile, "grok");
    assert_eq!(resolved.model.model_id, "grok-4.7");
    assert_eq!(switch_c.session, action_c.session);
    assert_eq!(
        switch_log
            .turns()
            .iter()
            .filter(|turn| turn.id == switch_turn)
            .count(),
        1
    );
    // Switching back in the other direction follows the same in-flight barrier.
    super::super::model_selection::request(
        &switch_c,
        &switch_cfg,
        "default",
        Some(resolved.revision),
        Some("inflight-switch-back"),
    )
    .unwrap();
    switch_log
        .append(
            action_now + 20,
            session::Body::Turn {
                turn: session::Turn {
                    id: "turn-inflight-switch-back".into(),
                    session: switch_c.session.to_string(),
                    trigger: "supervisor".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: action_now + 20,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    session::commit_action(
        &switch_c.directory,
        &switch_c.session.to_string(),
        "turn-inflight-switch-back",
        &session::action("directive", "dir-switch-back", &json!({"text":"hold"})),
    )
    .unwrap();
    consider(
        &switch_c,
        &switch_cfg,
        &mut worker,
        &mut aside,
        &tick(action_now + 21, true),
        None,
    );
    assert_eq!(
        super::super::model_selection::show(&switch_c, &switch_cfg).unwrap()["active"]["profile"],
        "grok"
    );
    consider(
        &switch_c,
        &switch_cfg,
        &mut worker,
        &mut aside,
        &tick(action_now + 22, false),
        None,
    );
    assert_eq!(
        super::super::model_selection::resolve(&switch_c, &switch_cfg)
            .unwrap()
            .unwrap()
            .profile,
        "default"
    );

    // Repeat the real decision-owner boundary in both model directions. The
    // first direction crashes after SQLite commits and recovers the receipt;
    // the reverse direction completes through poll_decision using a local
    // channel, so neither fixture starts a provider call.
    for (index, base_model, base_transport, profile, profile_model, profile_transport, recover) in [
        (
            0,
            "grok-4.7",
            sadr_cli::native_transport::Transport::DirectOauth,
            "sol",
            "gpt-6.1-sol",
            sadr_cli::native_transport::Transport::OpenaiCodex,
            true,
        ),
        (
            1,
            "gpt-6.1-sol",
            sadr_cli::native_transport::Transport::OpenaiCodex,
            "grok",
            "grok-4.7",
            sadr_cli::native_transport::Transport::DirectOauth,
            false,
        ),
    ] {
        let owner_switch_dir = tempfile::tempdir().unwrap();
        let owner_now = action_now + 30 + index * 10;
        let (owner_c, owner_cfg, mut owner_db, owner_portfolio, owner_claim) =
            super::super::decision_owner::interruption_switch_fixture(
                owner_switch_dir.path(),
                owner_now,
                base_model,
                base_transport,
                profile,
                profile_model,
                profile_transport,
            );
        let mut unsupported = owner_cfg.clone();
        unsupported
            .orchestrator
            .as_mut()
            .unwrap()
            .admitted_models
            .retain(|admitted| admitted != profile);
        assert_eq!(
            super::super::model_selection::request(&owner_c, &unsupported, profile, Some(0), None)
                .unwrap_err(),
            "model_selection_not_admitted"
        );
        assert_eq!(
            super::super::model_selection::show(&owner_c, &owner_cfg).unwrap()["revision"],
            0
        );
        super::super::model_selection::request(
            &owner_c,
            &owner_cfg,
            profile,
            Some(0),
            Some(if recover {
                "owner-inflight-switch-forward"
            } else {
                "owner-inflight-switch-back"
            }),
        )
        .unwrap();
        let owner_log = session::SessionLog::new(&owner_c.directory, &owner_c.session.to_string());
        let owner_prompt_version = "owner-switch-fixture";
        let owner_role = owner_cfg.role(Trigger::Supervisor);
        owner_log
            .append(
                owner_now,
                session::Body::Turn {
                    turn: session::Turn {
                        id: owner_claim.turn_id.clone(),
                        session: owner_c.session.to_string(),
                        trigger: "decision".into(),
                        prompt_version: owner_prompt_version.into(),
                        notes_version: 0,
                        decides_at_start: 0,
                        fills_at_start: owner_portfolio.fills.len(),
                        started_at_ms: owner_now,
                        request: json!({
                            "host_decision":owner_claim.clone(),
                            "owner_applied_hashes":AppliedHashes::current(&owner_cfg),
                            "resume_binding_sha256":resume_binding(&owner_cfg, &owner_role, owner_prompt_version).unwrap(),
                        }),
                    },
                },
            )
            .unwrap();
        let (owner_sender, owner_receiver) = mpsc::sync_channel(1);
        let mut owner_worker = test_worker(&owner_c.session.to_string(), &owner_claim.turn_id, 0);
        owner_worker.kind = RunKind::Decision;
        owner_worker.decision = Some(owner_claim.clone());
        owner_worker.prompt_version = owner_prompt_version.into();
        owner_worker.receiver = owner_receiver;
        let mut owner_worker_slot = Some(owner_worker);
        let mut owner_aside = None;
        let owner_tick = |now| Tick {
            now,
            minute: 556,
            in_session: true,
            fill_count: 0,
            allowlist: &[],
            session_end_ms: owner_now + 3_600_000,
            positions: json!({}),
        };
        let owner_instrument = super::super::decision_owner::exercise_instrument();
        let mut owner_context = super::super::decision_owner::Context {
            db: &mut owner_db,
            portfolio: &owner_portfolio,
            instruments: std::slice::from_ref(&owner_instrument),
        };
        consider_owner(
            &owner_c,
            &owner_cfg,
            &mut owner_worker_slot,
            &mut owner_aside,
            &owner_tick(owner_now + 1),
            None,
            &mut owner_context,
        );
        let pending_owner_switch =
            super::super::model_selection::show(&owner_c, &owner_cfg).unwrap();
        assert_eq!(pending_owner_switch["active"]["profile"], "default");
        assert_eq!(pending_owner_switch["pending"]["profile"], profile);
        assert!(owner_log.pending().is_some());

        if recover {
            // Crash after SQLite commits but before the file action/result.
            super::super::decision_owner::commit(
                &mut owner_context,
                &owner_c,
                &owner_cfg,
                &owner_claim,
                "{\"actions\":[],\"stance\":\"hold\",\"notes\":\"fixture\"}",
                owner_now + 2,
            )
            .unwrap();
            drop(owner_worker_slot.take());
            drop(owner_sender);
            let mut restarted_worker = None;
            consider_owner(
                &owner_c,
                &owner_cfg,
                &mut restarted_worker,
                &mut owner_aside,
                &owner_tick(owner_now + 3),
                None,
                &mut owner_context,
            );
            assert!(restarted_worker.is_none());
            let recovered = owner_log.result_for(&owner_claim.turn_id).unwrap();
            assert_eq!(recovered.outcome, "recovered_decision_receipt");
            assert_eq!(recovered.actions.len(), 1);
            assert_eq!(recovered.actions[0].kind, "paper_plan");
            let still_pending = super::super::model_selection::show(&owner_c, &owner_cfg).unwrap();
            assert_eq!(still_pending["active"]["profile"], "default");
            assert_eq!(still_pending["pending"]["profile"], profile);
            consider_owner(
                &owner_c,
                &owner_cfg,
                &mut restarted_worker,
                &mut owner_aside,
                &owner_tick(owner_now + 4),
                None,
                &mut owner_context,
            );
        } else {
            owner_sender
                .send(Ok(
                    "{\"actions\":[],\"stance\":\"hold\",\"notes\":\"fixture\"}".into(),
                ))
                .unwrap();
            consider_owner(
                &owner_c,
                &owner_cfg,
                &mut owner_worker_slot,
                &mut owner_aside,
                &owner_tick(owner_now + 2),
                None,
                &mut owner_context,
            );
            assert!(owner_worker_slot.is_none());
            assert_eq!(
                owner_log.result_for(&owner_claim.turn_id).unwrap().outcome,
                "ok"
            );
        }
        let applied = super::super::model_selection::resolve(&owner_c, &owner_cfg)
            .unwrap()
            .unwrap();
        assert_eq!(applied.profile, profile);
        assert_eq!(applied.model.model_id, profile_model);
        let applied_state = super::super::model_selection::show(&owner_c, &owner_cfg).unwrap();
        assert!(applied_state["pending"].is_null());
        assert_eq!(applied_state["last_receipt"]["state"], "applied");
        assert_eq!(owner_log.turns().len(), 1);
        assert_eq!(
            owner_log
                .result_for(&owner_claim.turn_id)
                .unwrap()
                .actions
                .len(),
            1
        );
        assert_eq!(
            session::actions_for(
                &owner_c.directory,
                &owner_c.session.to_string(),
                &owner_claim.turn_id,
            )
            .len(),
            1
        );
    }
    // Operational status is a bounded projection of durable state and the
    // pending turn; unknown and malformed sources never read as idle zero.
    let status_dir = tempfile::tempdir().unwrap();
    let status_session = "2026-10-26";
    let status_cfg = PlannerConfig::default();
    let disabled = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::NotConfigured,
        None,
    );
    assert_eq!(disabled.status, SupervisorStatus::Disabled);
    assert_eq!(disabled.config, SupervisorConfigSource::NotConfigured);
    assert!(disabled.config_reason.is_none());
    assert_eq!(disabled.state, SupervisorStateSource::NotConfigured);
    assert_eq!(disabled.pending_source, SupervisorPendingSource::NotChecked);
    assert!(disabled.parked.is_none());
    assert_eq!(disabled.budget.exhausted, None);
    assert_eq!(disabled.backoff.active, None);
    assert_eq!(disabled.backoff.enforced, None);
    assert_eq!(
        disabled.budget.charged_calls_scope,
        "provider_usage_projection_not_turn_runs"
    );
    assert_eq!(
        disabled.backoff.enforced_scope,
        "current_session_context_exhaustion_only"
    );
    assert_eq!(
        disabled.backoff.active_scope,
        "authoritative_current_session_state_only"
    );
    let disabled_json = serde_json::to_value(&disabled).unwrap();
    assert!(disabled_json["budget"]["exhausted"].is_null());
    assert!(disabled_json["backoff"]["active"].is_null());
    let missing = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(missing.status, SupervisorStatus::Unknown);
    assert_eq!(missing.config, SupervisorConfigSource::Configured);
    assert!(missing.config_reason.is_none());
    assert_eq!(missing.state, SupervisorStateSource::Missing);
    assert_eq!(missing.pending_source, SupervisorPendingSource::Missing);
    assert!(missing.last_completed_run.is_none());
    // Static configuration caps stay known while observed consumption and
    // enforcement state stay unknown.
    assert_eq!(missing.budget.configured_daily_calls, Some(0));
    assert_eq!(missing.budget.charged_calls, None);
    assert_eq!(missing.budget.exhausted, None);
    assert_eq!(missing.backoff.configured_ms, Some(900_000));
    assert_eq!(missing.backoff.retry_limit, Some(1));
    assert_eq!(missing.backoff.enforced, Some(true));
    assert_eq!(missing.backoff.active, None);
    assert_eq!(missing.backoff.earliest_retry_at_ms, None);
    let missing_json = serde_json::to_value(&missing).unwrap();
    assert!(missing_json["budget"]["charged_calls"].is_null());
    assert!(missing_json["budget"]["exhausted"].is_null());
    assert!(missing_json["backoff"]["active"].is_null());
    assert!(missing_json["backoff"]["earliest_retry_at_ms"].is_null());
    with_state(status_dir.path(), status_session, |state| {
        state.parked = Some(Parked {
            reason: "supervisor_budget_exhausted".into(),
            at_ms: 900,
            budget_at_park: Some(8),
        });
        state.last_run_ms = Some(800);
        state.runs = 2;
        state.context_exhaustion = Some(ContextExhaustion {
            reason: "supervisor_model_response_incomplete".into(),
            incomplete_reason: Some("max_output_tokens".into()),
            request_bytes: Some(1_000_000),
            max_request_bytes: 1_048_576,
            at_ms: 850,
            retries: 1,
        });
        state.compaction_failed = Some(CompactionFailure {
            reason: "supervisor_compaction_invalid".into(),
            at_ms: 840,
        });
    })
    .unwrap();
    let parked = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::Configured(&status_cfg),
        Some(3),
    );
    assert_eq!(parked.status, SupervisorStatus::Parked);
    assert_eq!(parked.config, SupervisorConfigSource::Configured);
    assert_eq!(parked.state, SupervisorStateSource::Present);
    assert_eq!(parked.pending_source, SupervisorPendingSource::Missing);
    assert_eq!(parked.enabled, Some(false));
    let parked_state = parked.parked.as_ref().unwrap();
    assert_eq!(parked_state.reason, Some("supervisor_budget_exhausted"));
    assert!(parked_state.reason_known);
    assert_eq!(parked_state.budget_at_park, Some(8));
    assert_eq!(parked.budget.configured_daily_calls, Some(0));
    assert_eq!(parked.budget.charged_calls, Some(3));
    assert_eq!(
        parked.budget.charged_calls_scope,
        "provider_usage_projection_not_turn_runs"
    );
    assert_eq!(parked.budget.parked_at_calls, Some(8));
    assert_eq!(parked.budget.exhausted, Some(true));
    assert_eq!(
        parked.budget.exhausted_scope,
        "authoritative_current_session_parked_state_only"
    );
    let context = parked.context.as_ref().unwrap();
    assert_eq!(context.reason, Some("supervisor_model_response_incomplete"));
    assert_eq!(context.incomplete_reason, Some("max_output_tokens"));
    assert_eq!(context.request_bytes, Some(1_000_000));
    assert_eq!(context.max_request_bytes, 1_048_576);
    assert_eq!(context.retries, 1);
    assert_eq!(
        parked.compaction.as_ref().unwrap().reason,
        Some("supervisor_compaction_invalid")
    );
    let backoff = &parked.backoff;
    assert_eq!(backoff.configured_ms, Some(900_000));
    assert_eq!(backoff.retry_limit, Some(1));
    assert_eq!(backoff.enforced, Some(true));
    assert_eq!(
        backoff.enforced_scope,
        "current_session_context_exhaustion_only"
    );
    assert_eq!(backoff.active, Some(true));
    assert_eq!(
        backoff.active_scope,
        "authoritative_current_session_state_only"
    );
    assert_eq!(backoff.earliest_retry_at_ms, Some(900_850));
    let last = parked.last_completed_run.as_ref().unwrap();
    assert_eq!(last.at_ms, 800);
    assert_eq!(last.age_ms, Some(200));
    assert_eq!(last.runs, 2);
    assert!(parked.pending.is_none());
    let encoded = serde_json::to_string(&parked).unwrap();
    for forbidden in [
        "prompt",
        "transcript",
        "payload",
        "credential",
        "webhook",
        "model/supervisor-state.json",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
    assert!(!encoded.contains("\"token\""));
    // The provider-incomplete code is static in both its role-prefixed and raw
    // decision forms, and the compaction allowlist includes the codes this
    // source actually writes rather than only `supervisor_*` names.
    with_state(status_dir.path(), status_session, |state| {
        if let Some(context) = state.context_exhaustion.as_mut() {
            context.reason = "model_response_incomplete".into();
        }
        state.compaction_failed = Some(CompactionFailure {
            reason: "supervisor_output_invalid".into(),
            at_ms: 845,
        });
    })
    .unwrap();
    let raw_codes = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    let raw_context = raw_codes.context.as_ref().unwrap();
    assert_eq!(raw_context.reason, Some("model_response_incomplete"));
    assert!(raw_context.reason_known);
    let raw_compaction = raw_codes.compaction.as_ref().unwrap();
    assert_eq!(raw_compaction.reason, Some("supervisor_output_invalid"));
    assert!(raw_compaction.reason_known);
    with_state(status_dir.path(), status_session, |state| {
        state.compaction_failed = Some(CompactionFailure {
            reason: "session_notes_changed".into(),
            at_ms: 846,
        });
    })
    .unwrap();
    let notes_compaction = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    let notes_compaction = notes_compaction.compaction.as_ref().unwrap();
    assert_eq!(notes_compaction.reason, Some("session_notes_changed"));
    assert!(notes_compaction.reason_known);
    with_state(status_dir.path(), status_session, |state| {
        state.compaction_failed = Some(CompactionFailure {
            reason: "supervisor_harness_tool_invalid".into(),
            at_ms: 847,
        });
    })
    .unwrap();
    let harness_compaction = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    let harness_compaction = harness_compaction.compaction.as_ref().unwrap();
    assert_eq!(
        harness_compaction.reason,
        Some("supervisor_harness_tool_invalid")
    );
    assert!(harness_compaction.reason_known);
    with_state(status_dir.path(), status_session, |state| {
        state.parked = Some(Parked {
            reason: "provider_secret_must_not_leak".into(),
            at_ms: 950,
            budget_at_park: None,
        });
    })
    .unwrap();
    let unknown_reason = operational_status(
        status_dir.path(),
        status_session,
        1_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    let unknown_parked = unknown_reason.parked.as_ref().unwrap();
    assert!(unknown_parked.reason.is_none());
    assert!(!unknown_parked.reason_known);
    // An unknown parked reason cannot prove budget exhaustion either.
    assert_eq!(unknown_reason.budget.exhausted, None);
    assert!(
        !serde_json::to_string(&unknown_reason)
            .unwrap()
            .contains("provider_secret_must_not_leak")
    );
    let unknown_json = serde_json::to_value(&unknown_reason).unwrap();
    assert!(unknown_json["budget"]["exhausted"].is_null());
    with_state(status_dir.path(), status_session, |state| {
        state.parked = None
    })
    .unwrap();
    let log = session::SessionLog::new(status_dir.path(), status_session);
    log.append(
        1_100,
        session::Body::Turn {
            turn: session::Turn {
                id: "status-pending".into(),
                session: status_session.into(),
                trigger: "session_open".into(),
                prompt_version: "prompt".into(),
                notes_version: 0,
                decides_at_start: 0,
                fills_at_start: 0,
                started_at_ms: 1_200,
                request: json!({"prompt":"must-not-leak","transcript":"must-not-leak"}),
            },
        },
    )
    .unwrap();
    let pending = operational_status(
        status_dir.path(),
        status_session,
        1_500,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(pending.status, SupervisorStatus::Pending);
    assert_eq!(pending.pending_source, SupervisorPendingSource::Pending);
    let pending_state = pending.pending.as_ref().unwrap();
    assert_eq!(pending_state.kind, "session_open");
    assert_eq!(pending_state.started_at_ms, 1_200);
    assert_eq!(pending_state.age_ms, Some(300));
    // The present state records no park, so budget exhaustion is authoritatively
    // false while the retained context exhaustion keeps backoff active.
    assert_eq!(pending.budget.exhausted, Some(false));
    assert_eq!(pending.backoff.active, Some(true));
    let encoded = serde_json::to_string(&pending).unwrap();
    assert!(!encoded.contains("must-not-leak"));
    assert!(!encoded.contains("prompt"));
    assert!(!encoded.contains("transcript"));
    // An unrecognized trigger is replaced by a static kind, never echoed.
    log.append(
        1_600,
        session::Body::Turn {
            turn: session::Turn {
                id: "status-unknown-kind".into(),
                session: status_session.into(),
                trigger: "provider_prompt_must_not_leak".into(),
                prompt_version: "prompt".into(),
                notes_version: 0,
                decides_at_start: 0,
                fills_at_start: 0,
                started_at_ms: 1_700,
                request: json!({"prompt":"must-not-leak"}),
            },
        },
    )
    .unwrap();
    let unknown_kind = operational_status(
        status_dir.path(),
        status_session,
        1_800,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(unknown_kind.pending.as_ref().unwrap().kind, "unknown");
    // A malformed state file is unknown, not a fresh zero state.
    let malformed_dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(malformed_dir.path().join("model")).unwrap();
    fs::write(malformed_dir.path().join(STATE_PATH), b"{").unwrap();
    let malformed = operational_status(
        malformed_dir.path(),
        status_session,
        1_900,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(malformed.status, SupervisorStatus::Unknown);
    assert_eq!(malformed.state, SupervisorStateSource::Malformed);
    assert_eq!(malformed.pending_source, SupervisorPendingSource::Missing);
    assert!(malformed.last_completed_run.is_none());
    assert_eq!(malformed.budget.exhausted, None);
    assert_eq!(malformed.backoff.active, None);
    assert_eq!(malformed.backoff.configured_ms, Some(900_000));
    let malformed_json = serde_json::to_value(&malformed).unwrap();
    assert!(malformed_json["budget"]["exhausted"].is_null());
    assert!(malformed_json["backoff"]["active"].is_null());
    // A state file for another session is not this session's state.
    let other_dir = tempfile::tempdir().unwrap();
    with_state(other_dir.path(), "2026-10-27", |state| {
        state.last_run_ms = Some(5);
    })
    .unwrap();
    let other = operational_status(
        other_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(other.state, SupervisorStateSource::OtherSession);
    assert_eq!(other.status, SupervisorStatus::Unknown);
    assert_eq!(other.pending_source, SupervisorPendingSource::Missing);
    assert_eq!(other.budget.exhausted, None);
    assert_eq!(other.backoff.active, None);
    let other_json = serde_json::to_value(&other).unwrap();
    assert!(other_json["budget"]["exhausted"].is_null());
    assert!(other_json["backoff"]["active"].is_null());
    assert!(other_json["backoff"]["earliest_retry_at_ms"].is_null());
    // A corrupt pending source is unknown, not an idle zero. The present state
    // alone cannot prove there is no pending turn when the log cannot be read.
    let corrupt_dir = tempfile::tempdir().unwrap();
    with_state(corrupt_dir.path(), status_session, |_| {}).unwrap();
    let corrupt_log = session::SessionLog::new(corrupt_dir.path(), status_session);
    corrupt_log
        .append(
            1,
            session::Body::Turn {
                turn: session::Turn {
                    id: "corrupt-pending".into(),
                    session: status_session.into(),
                    trigger: "idle".into(),
                    prompt_version: "prompt".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 1,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    let corrupt_path = corrupt_dir
        .path()
        .join(session::ROOT_PREFIX)
        .join(status_session)
        .join("log.bin");
    let mut corrupt_bytes = fs::read(&corrupt_path).unwrap();
    corrupt_bytes[4] ^= 1;
    fs::write(&corrupt_path, &corrupt_bytes).unwrap();
    let corrupt = operational_status(
        corrupt_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(corrupt.status, SupervisorStatus::Unknown);
    assert_eq!(corrupt.pending_source, SupervisorPendingSource::Corrupt);
    assert!(corrupt.pending.is_none());
    // An incomplete EOF tail cannot prove there is no future pending turn.
    let incomplete_dir = tempfile::tempdir().unwrap();
    with_state(incomplete_dir.path(), status_session, |_| {}).unwrap();
    let incomplete_path = incomplete_dir
        .path()
        .join(session::ROOT_PREFIX)
        .join(status_session)
        .join("log.bin");
    fs::create_dir_all(incomplete_path.parent().unwrap()).unwrap();
    fs::write(&incomplete_path, [0u8, 0]).unwrap();
    let incomplete = operational_status(
        incomplete_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(incomplete.status, SupervisorStatus::Unknown);
    assert_eq!(
        incomplete.pending_source,
        SupervisorPendingSource::Incomplete
    );
    // An oversized sparse log is refused by the bounded reader without reading
    // it into memory, and the status stays unknown rather than idle.
    let oversized_dir = tempfile::tempdir().unwrap();
    with_state(oversized_dir.path(), status_session, |_| {}).unwrap();
    let oversized_path = oversized_dir
        .path()
        .join(session::ROOT_PREFIX)
        .join(status_session)
        .join("log.bin");
    fs::create_dir_all(oversized_path.parent().unwrap()).unwrap();
    let oversized = fs::File::create(&oversized_path).unwrap();
    oversized.set_len(64 * 1024 * 1024 + 1).unwrap();
    drop(oversized);
    let oversized_status = operational_status(
        oversized_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(oversized_status.status, SupervisorStatus::Unknown);
    assert_eq!(
        oversized_status.pending_source,
        SupervisorPendingSource::Oversize
    );
    // A non-regular log path and a symlink are unavailable sources, not empty
    // logs. Neither is followed or treated as "no pending".
    let nonregular_dir = tempfile::tempdir().unwrap();
    with_state(nonregular_dir.path(), status_session, |_| {}).unwrap();
    let nonregular_path = nonregular_dir
        .path()
        .join(session::ROOT_PREFIX)
        .join(status_session)
        .join("log.bin");
    fs::create_dir_all(&nonregular_path).unwrap();
    let nonregular = operational_status(
        nonregular_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(nonregular.status, SupervisorStatus::Unknown);
    assert_eq!(
        nonregular.pending_source,
        SupervisorPendingSource::NonRegular
    );
    let symlink_dir = tempfile::tempdir().unwrap();
    with_state(symlink_dir.path(), status_session, |_| {}).unwrap();
    let symlink_path = symlink_dir
        .path()
        .join(session::ROOT_PREFIX)
        .join(status_session)
        .join("log.bin");
    fs::create_dir_all(symlink_path.parent().unwrap()).unwrap();
    let symlink_target = symlink_dir.path().join("empty-log.bin");
    fs::write(&symlink_target, b"").unwrap();
    std::os::unix::fs::symlink(&symlink_target, &symlink_path).unwrap();
    let symlink = operational_status(
        symlink_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(symlink.status, SupervisorStatus::Unknown);
    assert_eq!(symlink.pending_source, SupervisorPendingSource::Symlink);
    // A malformed state with a real checked pending turn stays unknown, while
    // the pending evidence remains separately visible.
    let malformed_pending_dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(malformed_pending_dir.path().join("model")).unwrap();
    fs::write(malformed_pending_dir.path().join(STATE_PATH), b"{").unwrap();
    let malformed_log = session::SessionLog::new(malformed_pending_dir.path(), status_session);
    malformed_log
        .append(
            2_000,
            session::Body::Turn {
                turn: session::Turn {
                    id: "malformed-pending".into(),
                    session: status_session.into(),
                    trigger: "resume".into(),
                    prompt_version: "prompt".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 2_100,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    let malformed_pending = operational_status(
        malformed_pending_dir.path(),
        status_session,
        2_500,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(malformed_pending.status, SupervisorStatus::Unknown);
    assert_eq!(
        malformed_pending.pending_source,
        SupervisorPendingSource::Pending
    );
    assert_eq!(
        malformed_pending
            .pending
            .as_ref()
            .map(|pending| pending.kind),
        Some("resume")
    );
    // An unreadable log parent is an unavailable source, not a missing log.
    let unreadable_dir = tempfile::tempdir().unwrap();
    with_state(unreadable_dir.path(), status_session, |_| {}).unwrap();
    let unreadable_session = unreadable_dir
        .path()
        .join(session::ROOT_PREFIX)
        .join(status_session);
    fs::create_dir_all(unreadable_session.parent().unwrap()).unwrap();
    fs::write(&unreadable_session, b"not-a-directory").unwrap();
    let unreadable = operational_status(
        unreadable_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(unreadable.status, SupervisorStatus::Unknown);
    assert_eq!(
        unreadable.pending_source,
        SupervisorPendingSource::Unreadable
    );
    // A genuinely missing log is a known absence, distinct from no pending in
    // a complete log. Both can be idle when the state is present.
    let no_log_dir = tempfile::tempdir().unwrap();
    with_state(no_log_dir.path(), status_session, |_| {}).unwrap();
    let no_log = operational_status(
        no_log_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(no_log.status, SupervisorStatus::Idle);
    assert_eq!(no_log.pending_source, SupervisorPendingSource::Missing);
    assert!(no_log.pending.is_none());
    let complete_dir = tempfile::tempdir().unwrap();
    with_state(complete_dir.path(), status_session, |_| {}).unwrap();
    let complete_log = session::SessionLog::new(complete_dir.path(), status_session);
    complete_log
        .append(
            1,
            session::Body::Turn {
                turn: session::Turn {
                    id: "complete-turn".into(),
                    session: status_session.into(),
                    trigger: "idle".into(),
                    prompt_version: "prompt".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 1,
                    request: json!({}),
                },
            },
        )
        .unwrap();
    complete_log
        .append(
            2,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: "complete-turn".into(),
                    finished_at_ms: 2,
                    outcome: "ok".into(),
                    reason: None,
                    reply: "{}".into(),
                    actions: Vec::new(),
                },
            },
        )
        .unwrap();
    let complete = operational_status(
        complete_dir.path(),
        status_session,
        3_000,
        SupervisorConfig::Configured(&status_cfg),
        None,
    );
    assert_eq!(complete.status, SupervisorStatus::Idle);
    assert_eq!(complete.pending_source, SupervisorPendingSource::NoPending);
    assert!(complete.pending.is_none());
    // A configured planner whose policy or usage projection is unavailable is
    // unknown, not disabled. Static caps stay visible when the original policy
    // is available; the reason is a static allowlist code, never raw text.
    let unavailable = operational_status(
        no_log_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Unavailable {
            config: Some(&status_cfg),
            reason: "planner_usage_unavailable_recovery_required",
        },
        Some(3),
    );
    assert_eq!(unavailable.status, SupervisorStatus::Unknown);
    assert_eq!(unavailable.config, SupervisorConfigSource::Unavailable);
    assert_eq!(
        unavailable.config_reason,
        Some("planner_usage_unavailable_recovery_required")
    );
    assert_eq!(unavailable.enabled, Some(false));
    assert_eq!(unavailable.budget.configured_daily_calls, Some(0));
    assert_eq!(unavailable.budget.charged_calls, Some(3));
    assert_eq!(unavailable.backoff.configured_ms, Some(900_000));
    assert_eq!(unavailable.backoff.retry_limit, Some(1));
    let unavailable_json = serde_json::to_value(&unavailable).unwrap();
    assert_eq!(unavailable_json["config"], "unavailable");
    assert_eq!(
        unavailable_json["config_reason"],
        "planner_usage_unavailable_recovery_required"
    );
    let unavailable_no_config = operational_status(
        no_log_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Unavailable {
            config: None,
            reason: "planner_config_unavailable",
        },
        None,
    );
    assert_eq!(unavailable_no_config.status, SupervisorStatus::Unknown);
    assert_eq!(
        unavailable_no_config.config,
        SupervisorConfigSource::Unavailable
    );
    assert_eq!(
        unavailable_no_config.config_reason,
        Some("planner_config_unavailable")
    );
    assert_eq!(unavailable_no_config.enabled, None);
    assert_eq!(unavailable_no_config.budget.configured_daily_calls, None);
    assert_eq!(unavailable_no_config.backoff.configured_ms, None);
    assert_eq!(unavailable_no_config.backoff.active, None);
    // Unknown failure text is replaced by a static code, never echoed.
    let unknown_config_reason = operational_status(
        no_log_dir.path(),
        status_session,
        2_000,
        SupervisorConfig::Unavailable {
            config: Some(&status_cfg),
            reason: "provider_secret_must_not_leak",
        },
        None,
    );
    assert_eq!(
        unknown_config_reason.config_reason,
        Some("planner_config_unavailable")
    );
    assert!(
        !serde_json::to_string(&unknown_config_reason)
            .unwrap()
            .contains("provider_secret_must_not_leak")
    );
    // Observation wall time and the monotonic duration budget are independent.
    // A frozen or stepped-back wall must never freeze a deadline, and a duration
    // projection must never be reported as the actual wall observation.
    let wall = std::cell::Cell::new(1_000i64);
    let elapsed = std::cell::Cell::new(0i64);
    let wall_clock = || wall.get();
    let duration = || elapsed.get();
    let clock_guard = planner::CallGuard {
        cancel: None,
        deadline: Some(1_010),
        not_before: Some(1_000),
        configuration: None,
        journal_required: std::cell::Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: None,
        context_hash: None,
        clock: &wall_clock,
        duration: Some(&duration),
        started_wall_ms: Some(1_000),
    };
    assert_eq!(clock_guard.wall_ms(), 1_000);
    assert_eq!(clock_guard.elapsed_ms(), 0);
    wall.set(1_004);
    elapsed.set(4);
    assert_eq!(clock_guard.wall_ms(), 1_004);
    assert_eq!(clock_guard.projected_wall_ms(), 1_004);
    assert!(clock_guard.check_finished().is_ok());
    // The wall is frozen; the monotonic budget still expires before another call.
    wall.set(1_000);
    elapsed.set(10);
    assert!(clock_guard.check_finished().is_ok());
    elapsed.set(11);
    assert_eq!(
        clock_guard.check_finished().unwrap_err().to_string(),
        "planner_event_deadline_expired"
    );
    // A wall backstep is a deterministic refusal, never a fabricated UTC time.
    wall.set(999);
    elapsed.set(0);
    assert_eq!(
        clock_guard.check_finished().unwrap_err().to_string(),
        "planner_clock_anomaly"
    );
    // The observation wall is the actual current wall, not anchor + elapsed.
    wall.set(1_020);
    elapsed.set(0);
    assert_eq!(clock_guard.wall_ms(), 1_020);
    assert_eq!(clock_guard.projected_wall_ms(), 1_020);
    // A forward wall jump past an absolute deadline expires it even while the
    // monotonic duration is still short.
    assert_eq!(
        clock_guard.check_finished().unwrap_err().to_string(),
        "planner_event_deadline_expired"
    );
    // A not-before window is not entered while the actual wall is still early,
    // even if the monotonic projection is later.
    let early_wall = std::cell::Cell::new(1_000i64);
    let early_elapsed = std::cell::Cell::new(30i64);
    let early_wall_clock = || early_wall.get();
    let early_duration = || early_elapsed.get();
    let early_guard = planner::CallGuard {
        cancel: None,
        deadline: Some(2_000),
        not_before: Some(1_025),
        configuration: None,
        journal_required: std::cell::Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: None,
        context_hash: None,
        clock: &early_wall_clock,
        duration: Some(&early_duration),
        started_wall_ms: Some(1_000),
    };
    assert_eq!(
        early_guard.check_finished().unwrap_err().to_string(),
        "planner_event_deadline_expired"
    );

    // Optional failure backoff is absent by default; old state/config bytes stay
    // unchanged and no owner turn is deferred without a configured policy.
    let default_wake = Wake::default();
    assert_eq!(default_wake.failure_backoff_ms, None);
    let default_trigger = super::super::orchestrator_model::TriggerSettings::default();
    let trigger_bytes = serde_json::to_vec(&default_trigger).unwrap();
    assert!(!String::from_utf8_lossy(&trigger_bytes).contains("failure_backoff_ms"));
    let fresh_state = State::fresh("2026-10-26");
    assert_eq!(fresh_state.last_failure_ms, None);
    assert_eq!(fresh_state.last_failure_binding, None);
    assert_eq!(fresh_state.owner_failure_streak, None);
    let state_bytes = serde_json::to_vec(&fresh_state).unwrap();
    assert!(!String::from_utf8_lossy(&state_bytes).contains("last_failure"));
    assert!(!String::from_utf8_lossy(&state_bytes).contains("owner_failure"));
    let mut backoff_wake = Wake {
        failure_backoff_ms: Some(60_000),
        ..Wake::default()
    };
    let mut failed = State::fresh("2026-10-26");
    failed.failures = backoff_wake.failure_streak;
    failed.owner_failure_streak = Some(backoff_wake.failure_streak);
    failed.last_failure_ms = Some(1_000);
    failed.last_failure_binding = Some("default:0".into());
    assert_eq!(
        super::wake::failure_backoff_until(&failed, &backoff_wake, 1_001, Some("default:0")),
        Some(61_000)
    );
    assert_eq!(
        super::wake::failure_backoff_until(&failed, &backoff_wake, 61_000, Some("default:0")),
        None
    );
    // A changed selection profile/revision does not inherit the old backoff.
    assert_eq!(
        super::wake::failure_backoff_until(&failed, &backoff_wake, 1_001, Some("other:1")),
        None
    );
    assert_eq!(
        super::wake::failure_backoff_until(&failed, &backoff_wake, 1_001, None),
        None
    );
    // A legacy state with only the broad failure counter is never retro-guessed
    // into an owner streak from review, compaction or host failures.
    let mut legacy = failed.clone();
    legacy.owner_failure_streak = None;
    assert_eq!(
        super::wake::failure_backoff_until(&legacy, &backoff_wake, 1_001, Some("default:0")),
        None
    );
    // One owner failure plus two non-owner failures stays below the threshold.
    let mut mixed = State::fresh("2026-10-26");
    apply_finish(
        &mut mixed,
        RunKind::Decision,
        "decision",
        1_000,
        false,
        &hashes,
        0,
        Some("default:0"),
    );
    apply_finish(
        &mut mixed,
        RunKind::Review,
        "review",
        1_001,
        false,
        &hashes,
        0,
        Some("default:0"),
    );
    apply_finish(
        &mut mixed,
        RunKind::Compaction,
        "compaction",
        1_002,
        false,
        &hashes,
        0,
        Some("default:0"),
    );
    assert_eq!(mixed.failures, 3);
    assert_eq!(mixed.owner_failure_streak, Some(1));
    assert_eq!(
        super::wake::failure_backoff_until(&mixed, &backoff_wake, 1_003, Some("default:0")),
        None
    );
    // Three actual owner failures arm the backoff from the actual failure time.
    apply_finish(
        &mut mixed,
        RunKind::Decision,
        "decision",
        1_003,
        false,
        &hashes,
        0,
        Some("default:0"),
    );
    apply_finish(
        &mut mixed,
        RunKind::Supervisor,
        "supervisor",
        1_004,
        false,
        &hashes,
        0,
        Some("default:0"),
    );
    assert_eq!(mixed.owner_failure_streak, Some(3));
    assert_eq!(mixed.last_failure_ms, Some(1_004));
    assert_eq!(
        super::wake::failure_backoff_until(&mixed, &backoff_wake, 1_005, Some("default:0")),
        Some(61_004)
    );
    // A success under a different selection neither inherits nor clears the
    // streak; a same-selection success clears it.
    apply_finish(
        &mut mixed,
        RunKind::Decision,
        "decision",
        1_006,
        true,
        &hashes,
        0,
        Some("other:1"),
    );
    assert_eq!(mixed.owner_failure_streak, Some(3));
    apply_finish(
        &mut mixed,
        RunKind::Decision,
        "decision",
        1_007,
        true,
        &hashes,
        0,
        Some("default:0"),
    );
    assert_eq!(mixed.owner_failure_streak, None);
    assert_eq!(mixed.last_failure_ms, None);
    assert_eq!(
        super::wake::failure_backoff_until(&mixed, &backoff_wake, 1_008, Some("default:0")),
        None
    );
    // A recovered action receipt derives its binding from the recorded pending
    // request, never from the current profile. Missing or mistyped evidence is
    // explicit unknown rather than a fabricated default profile.
    let mut recorded_turn = session::Turn {
        id: "recorded-binding".into(),
        session: "2026-10-26".into(),
        trigger: "supervisor".into(),
        prompt_version: "fixture".into(),
        notes_version: 0,
        decides_at_start: 0,
        fills_at_start: 0,
        started_at_ms: 1,
        request: json!({"model_selection":{"profile":"default","revision":7}}),
    };
    assert_eq!(
        super::recorded_selection_binding(&recorded_turn),
        Some("default:7".into())
    );
    recorded_turn.request = json!({"model_selection":{"profile":"default"}});
    assert_eq!(super::recorded_selection_binding(&recorded_turn), None);
    recorded_turn.request = json!({"model_selection":{"profile":"default","revision":"7"}});
    assert_eq!(super::recorded_selection_binding(&recorded_turn), None);
    recorded_turn.request = json!({});
    assert_eq!(super::recorded_selection_binding(&recorded_turn), None);
    let mut recovered = State::fresh("2026-10-26");
    apply_finish(
        &mut recovered,
        RunKind::Supervisor,
        "resume",
        2_000,
        false,
        &hashes,
        0,
        Some("default:7"),
    );
    assert_eq!(recovered.owner_failure_streak, Some(1));
    assert_eq!(recovered.last_failure_binding.as_deref(), Some("default:7"));
    backoff_wake.failure_backoff_ms = None;
    assert_eq!(
        super::wake::failure_backoff_until(&failed, &backoff_wake, 1_001, Some("default:0")),
        None
    );
    case_research_contract();
}

fn case_research_contract() {
    use super::super::owner_tasks::{
        self, OwnerTask, ResearchEvidenceKind, ResearchEvidenceV1, ResearchIdentity,
        ResearchOutputV1, ResearchScheduleInput,
    };
    let evidence = ResearchEvidenceV1 {
        version: 1,
        kind: ResearchEvidenceKind::CandidateResult,
        source_session: "2026-10-16".into(),
        task_id: "a".repeat(64),
        revision: 1,
        result_sha256: "b".repeat(64),
        lifecycle_candidate_id: Some("c".repeat(64)),
        maturity_state: None,
        workspace_refs: Vec::new(),
    };
    let evidence_id = evidence.evidence_id().unwrap();
    // The stable id excludes presentation-only workspace refs.
    let mut with_ref = evidence.clone();
    with_ref.workspace_refs = vec![super::super::workspace::WorkspaceRefV1 {
        version: 1,
        source_session: "2026-10-15".into(),
        path: "notes/day.md".into(),
        sha256: "d".repeat(64),
        captured_at_ms: 1,
        kind: super::super::workspace::WorkspaceRefKind::Notes,
    }];
    assert_eq!(with_ref.evidence_id().unwrap(), evidence_id);
    assert!(evidence.validate().is_ok());
    let mut invalid = evidence.clone();
    invalid.result_sha256 = "not-a-hash".into();
    assert!(invalid.validate().is_err());

    let identity = ResearchIdentity {
        version: 1,
        session: "2026-10-16".into(),
        evidence_id: evidence_id.clone(),
        policy_sha256: "e".repeat(64),
        prompt_version: digest(owner_tasks::RESEARCH_PROMPT.as_bytes()),
    };
    let task = OwnerTask::Research {
        identity: identity.clone(),
        evidence: evidence.clone(),
    };
    let turn = session::Turn {
        id: "turn-research-fixture".into(),
        session: "2026-10-16".into(),
        trigger: "research".into(),
        prompt_version: identity.prompt_version.clone(),
        notes_version: 0,
        decides_at_start: 0,
        fills_at_start: 0,
        started_at_ms: 10,
        request: json!({
            "owner_task":task,
            "turn_id":"turn-research-fixture",
            "research_id":identity.id().unwrap(),
            "model_id":"grok-4.7",
            "cutoff_ms":100,
        }),
    };
    assert!(matches!(
        OwnerTask::from_turn(&turn).unwrap(),
        Some(OwnerTask::Research { .. })
    ));
    let mut forged = turn.clone();
    forged.trigger = "decision".into();
    assert!(OwnerTask::from_turn(&forged).is_err());
    let mut forged = turn.clone();
    forged.request["turn_id"] = json!("other-turn");
    assert!(OwnerTask::from_turn(&forged).is_err());
    let mut forged = turn.clone();
    forged.request["owner_task"]["identity"]["evidence_id"] = json!("f".repeat(64));
    assert!(OwnerTask::from_turn(&forged).is_err());

    // Bounded typed reply: no financial keys, no foreign citations.
    let valid = json!({
        "version":1,
        "evidence_id":evidence_id,
        "summary":"Verified candidate result is advisory only.",
        "observations":["The original result hash is bound."],
        "cited_ids":[evidence_id],
        "workspace_refs":[]
    });
    assert!(ResearchOutputV1::validate(&valid.to_string(), &task, &Default::default()).is_ok());
    let mut financial = valid.clone();
    financial["orders"] = json!([]);
    assert_eq!(
        ResearchOutputV1::validate(&financial.to_string(), &task, &Default::default())
            .unwrap_err()
            .to_string(),
        "owner_research_output_financial_refused"
    );
    let mut foreign = valid.clone();
    foreign["cited_ids"] = json!(["not-supplied"]);
    assert!(ResearchOutputV1::validate(&foreign.to_string(), &task, &Default::default()).is_err());
    let mut oversize = valid.clone();
    oversize["summary"] = json!("x".repeat(4097));
    assert!(ResearchOutputV1::validate(&oversize.to_string(), &task, &Default::default()).is_err());

    // Schedule precedence is pure and explicit.
    let base = ResearchScheduleInput {
        enabled: true,
        pending: false,
        book_runnable: true,
        capped: None,
        window_opens_ms: None,
        next_allowed_ms: None,
        requests: 0,
        idle_due: false,
        source_error: None,
        evidence_ids: vec![evidence_id.clone()],
        unknown: Vec::new(),
        now_ms: 100,
        cutoff_ms: 200,
    };
    let due = owner_tasks::project_research(&base);
    assert!(due.research_due);
    assert_eq!(due.evidence_id.as_deref(), Some(evidence_id.as_str()));
    // The pure projection never invents workspace context; only the producer
    // schedule that actually pinned the selected evidence fills these scalars.
    assert!(due.workspace_ref_count.is_none());
    assert!(due.workspace_snapshot_sha256.is_none());
    let mut expired = base.clone();
    expired.now_ms = 201;
    assert_eq!(
        owner_tasks::project_research(&expired).research_state,
        "expired"
    );
    let mut empty = base.clone();
    empty.evidence_ids.clear();
    assert_eq!(
        owner_tasks::project_research(&empty).research_state,
        "awaiting_evidence"
    );
    // The quiet-trading trigger (#244) only adds a reason to start: it is due
    // with no evidence, but pending, caps and the interval still win.
    let mut idle = empty.clone();
    idle.idle_due = true;
    let idle_due = owner_tasks::project_research(&idle);
    assert!(idle_due.research_due);
    assert_eq!(idle_due.research_reason, Some("research_lane_idle_trigger"));
    assert!(idle_due.evidence_id.is_none());
    for (field, state) in [
        ("pending", "pending"),
        ("capped", "parked"),
        ("interval", "deferred"),
    ] {
        let mut held = idle.clone();
        match field {
            "pending" => held.pending = true,
            "capped" => held.capped = Some("research_lane_call_budget_exhausted"),
            _ => held.next_allowed_ms = Some(150),
        }
        let projected = owner_tasks::project_research(&held);
        assert!(!projected.research_due, "{field}");
        assert_eq!(projected.research_state, state, "{field}");
    }
    let quiet = owner_tasks::IdleTriggerSettings {
        enabled: true,
        quiet_minutes: 30,
    };
    let open = 1_000_000;
    assert!(!quiet.quiet(open + 29 * 60_000, Some(open), None));
    assert!(quiet.quiet(open + 30 * 60_000, Some(open), None));
    assert!(!quiet.quiet(open + 40 * 60_000, Some(open), Some(open + 20 * 60_000)));
    assert!(quiet.quiet(open + 50 * 60_000, Some(open), Some(open + 20 * 60_000)));
    assert!(
        !quiet.quiet(open + 50 * 60_000, None, None),
        "market closed"
    );
    let off = owner_tasks::IdleTriggerSettings {
        enabled: false,
        ..quiet
    };
    assert!(!off.quiet(open + 50 * 60_000, Some(open), None));
    let mut unknown = empty.clone();
    unknown.source_error = Some("research_source_changed");
    assert_eq!(
        owner_tasks::project_research(&unknown).research_state,
        "invalid"
    );
    let mut pending = base.clone();
    pending.pending = true;
    assert_eq!(
        owner_tasks::project_research(&pending).research_state,
        "pending"
    );
    let mut disabled = base.clone();
    disabled.enabled = false;
    assert_eq!(
        owner_tasks::project_research(&disabled).research_state,
        "disabled"
    );

    // Crash windows: no artifact and no action is an explicit interruption;
    // an action without an artifact appends the result only, never a replay.
    let dir = tempfile::tempdir().unwrap();
    let _c = super::super::tests::config(dir.path());
    let log = session::SessionLog::new(dir.path(), "2026-10-16");
    log.append(10, session::Body::Turn { turn: turn.clone() })
        .unwrap();
    assert!(super::research::recover(dir.path(), &turn, 20).unwrap());
    let result = log.result_for(&turn.id).unwrap();
    assert_eq!(result.outcome, "interrupted");
    assert_eq!(result.reason.as_deref(), Some("owner_research_interrupted"));
    assert!(result.actions.is_empty());
    // A second recovery is idempotent and never replays the provider.
    assert!(super::research::recover(dir.path(), &turn, 21).unwrap());
    assert_eq!(
        log.records()
            .iter()
            .filter(|record| matches!(&record.body, session::Body::Result { result } if result.turn_id == turn.id))
            .count(),
        1
    );

    let receipt = owner_tasks::ResearchReceipt {
        version: 1,
        evidence_id: evidence_id.clone(),
        output_sha256: String::new(),
        model_id: "grok-4.7".into(),
        completed_ms: 30,
        status: "failed".into(),
    };
    let action = session::action(
        "owner_research",
        &turn.id,
        &serde_json::to_value(&receipt).unwrap(),
    );
    session::commit_action(dir.path(), &turn.session, &turn.id, &action).unwrap();
    let dir2 = tempfile::tempdir().unwrap();
    let log2 = session::SessionLog::new(dir2.path(), "2026-10-16");
    log2.append(10, session::Body::Turn { turn: turn.clone() })
        .unwrap();
    session::commit_action(dir2.path(), &turn.session, &turn.id, &action).unwrap();
    assert!(super::research::recover(dir2.path(), &turn, 40).unwrap());
    let result = log2.result_for(&turn.id).unwrap();
    assert_eq!(result.outcome, "recovered_action_receipt");
    assert_eq!(result.actions, vec![action]);
    assert!(super::research::recover(dir2.path(), &turn, 41).unwrap());
    assert_eq!(
        log2.records()
            .iter()
            .filter(|record| matches!(&record.body, session::Body::Result { result } if result.turn_id == turn.id))
            .count(),
        1
    );

    // A research turn never becomes a review attempt or panics the review
    // state projection.
    let review_state = super::review::state(dir.path(), "2026-10-16");
    assert_eq!(review_state["available"], false);
    assert_eq!(review_state["reason"], "owner_review_not_attempted");

    // A non-research turn is not claimed by the research recovery path.
    let mut idle = turn.clone();
    idle.trigger = "idle".into();
    idle.request = json!({"trigger":"idle"});
    assert!(!super::research::recover(dir2.path(), &idle, 50).unwrap());

    // A research frame binds its own identity and is never accepted as a
    // review or a decision. A forged trigger, id or evidence id is refused.
    let dir = tempfile::tempdir().unwrap();
    let (c, mut cfg) = super::super::decision_owner::owner_fixture_config(dir.path());
    cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings::default(),
        research: Some(owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(owner_tasks::ResearchLaneSettings {
                during_market: true,
                ..Default::default()
            }),
        }),
    });
    cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    let identity = ResearchIdentity {
        version: 1,
        session: c.session.to_string(),
        evidence_id: evidence_id.clone(),
        policy_sha256: digest(&planner::policy_bytes(&cfg).unwrap()),
        prompt_version: digest(owner_tasks::RESEARCH_PROMPT.as_bytes()),
    };
    let task = OwnerTask::Research {
        identity: identity.clone(),
        evidence: evidence.clone(),
    };
    let turn_id = "turn-research-bundle";
    let facts = json!({
        "owner_task":task,
        "turn_id":turn_id,
        "research_id":identity.id().unwrap(),
        "evidence_id":evidence_id,
        "model_id":"grok-4.7",
        "model_selection":{"revision":0},
    });
    let bundle = json!({"trigger":{"kind":"research","reason":"research"},"summary":facts});
    assert!(owner_tasks::validate_research_bundle(&c, &cfg, &bundle, Some(turn_id)).unwrap());
    assert!(!owner_tasks::validate_review_bundle(&c, &cfg, &bundle, Some(turn_id)).unwrap());
    let mut forged = bundle.clone();
    forged["trigger"]["kind"] = json!("decision");
    assert!(owner_tasks::validate_research_bundle(&c, &cfg, &forged, Some(turn_id)).is_err());
    let mut forged = bundle.clone();
    forged["summary"]["research_id"] = json!("other");
    assert!(owner_tasks::validate_research_bundle(&c, &cfg, &forged, Some(turn_id)).is_err());
    let mut forged = bundle.clone();
    forged["summary"]["evidence_id"] = json!("f".repeat(64));
    assert!(owner_tasks::validate_research_bundle(&c, &cfg, &forged, Some(turn_id)).is_err());

    // No eligible evidence starts no provider call and initializes no usage
    // journal. The schedule exposes the explicit awaiting state instead. An
    // open paper position no longer gates research: it runs in its own lane.
    let mut portfolio = super::super::tests::test_portfolio();
    portfolio.closed = false;
    let usage = c.directory.join("model/usage");
    assert!(!usage.exists());
    assert!(
        super::research::consider_lane(&c, &cfg, 2_500, 2_000, true, None)
            .unwrap()
            .is_none()
    );
    assert!(!usage.exists());
    let schedule = super::research::schedule(&c, &cfg, 2_500, 2_000, true, None).unwrap();
    assert_eq!(schedule.research_state, "awaiting_evidence");
    assert!(!schedule.research_due);

    // Ops keeps the context-exhaustion backoff separate from the owner failure
    // backoff and exposes the captured research as-of time.
    let schedule = owner_tasks::project_research(&base);
    let status = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&PlannerConfig::default()),
        Some(0),
        Some(ResearchOperational {
            config: SupervisorConfigSource::Configured,
            config_reason: None,
            enabled: Some(true),
            schedule: Some(schedule),
            pending: None,
            pending_source: SupervisorPendingSource::NoPending,
            as_of_ms: 123,
        }),
        None,
    );
    let value = serde_json::to_value(status).unwrap();
    assert_eq!(value["research"]["as_of_ms"], 123);
    assert_eq!(value["research"]["due"], true);
    assert_eq!(value["research"]["config"], "configured");
    assert_eq!(value["research"]["pending_source"], "no_pending");
    assert_eq!(
        value["owner_failure_backoff"]["enforced_scope"],
        "owner_model_turns_only"
    );
    assert_eq!(
        value["backoff"]["enforced_scope"],
        "current_session_context_exhaustion_only"
    );
    assert!(value["research"].get("question").is_none());

    // Disabled research leaves old policy/template bytes unchanged.
    let disabled = owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings {
            enabled: true,
            cutoff_minute: 1080,
        },
        research: None,
    };
    assert!(
        serde_json::to_value(&disabled)
            .unwrap()
            .get("research")
            .is_none()
    );
    let mut review_only = PlannerConfig::default();
    let mut policy_orchestrator =
        super::super::orchestrator_model::OrchestratorModelSettings::default();
    policy_orchestrator.triggers.clear();
    policy_orchestrator.triggers.insert(
        super::super::orchestrator_model::Trigger::Supervisor,
        super::super::orchestrator_model::TriggerSettings::default(),
    );
    review_only.orchestrator = Some(policy_orchestrator);
    review_only.owner_tasks = Some(disabled);
    let bytes = planner::policy_bytes(&review_only).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("research_prompt_sha256"));
    assert!(text.contains("review_prompt_sha256"));
    let mut hash_orchestrator =
        super::super::orchestrator_model::OrchestratorModelSettings::default();
    hash_orchestrator.triggers.clear();
    hash_orchestrator.triggers.insert(
        super::super::orchestrator_model::Trigger::Supervisor,
        super::super::orchestrator_model::TriggerSettings::default(),
    );
    let hashes = hash_orchestrator.prompt_hashes().unwrap();
    assert!(hashes.get("trigger_research").is_none());

    // Unknown config and a checked pending research turn are explicit, never
    // idle. The captured as-of time is the supplied snapshot, not request time.
    let pending_research = SupervisorPending {
        kind: "research",
        started_at_ms: 100,
        age_ms: Some(23),
    };
    let unknown = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Unavailable {
            config: None,
            reason: "planner_config_unavailable",
        },
        None,
        Some(ResearchOperational {
            config: SupervisorConfigSource::Unavailable,
            config_reason: Some("planner_config_unavailable"),
            enabled: None,
            schedule: None,
            pending: Some(pending_research),
            pending_source: SupervisorPendingSource::Pending,
            as_of_ms: 123,
        }),
        None,
    );
    let value = serde_json::to_value(unknown).unwrap();
    assert_eq!(value["status"], "unknown");
    assert_eq!(value["research"]["config"], "unavailable");
    assert_eq!(value["research"]["pending"]["kind"], "research");
    assert_eq!(value["research"]["pending_source"], "pending");
    assert_eq!(value["research"]["as_of_ms"], 123);

    // An aside request is never silently lost: parked, backoff and pending
    // owner work expose a static deferred reason and the real backoff expiry.
    let aside_dir = dir.path().join(session::ROOT_PREFIX).join("2026-10-16");
    fs::create_dir_all(&aside_dir).unwrap();
    fs::write(
        aside_dir.join("aside-request.json"),
        json!({"id":"aside-1","session":"2026-10-16","question":"operator question"}).to_string(),
    )
    .unwrap();
    with_state(dir.path(), "2026-10-16", |state| {
        state.parked = Some(super::state::Parked {
            reason: "supervisor_budget_exhausted".into(),
            at_ms: 1,
            budget_at_park: None,
        });
    })
    .unwrap();
    let parked_status = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&PlannerConfig::default()),
        None,
        None,
        None,
    );
    let value = serde_json::to_value(parked_status).unwrap();
    assert_eq!(value["aside"]["requested"], true);
    assert_eq!(value["aside"]["deferred_reason"], "aside_parked");
    assert!(value["aside"].get("question").is_none());
    with_state(dir.path(), "2026-10-16", |state| {
        state.parked = None;
        state.owner_failure_streak = Some(2);
        state.last_failure_ms = Some(100);
        state.last_failure_binding = Some("default:0".into());
    })
    .unwrap();
    let mut backoff_cfg = PlannerConfig::default();
    let mut orchestrator = super::super::orchestrator_model::OrchestratorModelSettings {
        enabled: true,
        ..Default::default()
    };
    let supervisor_trigger = super::super::orchestrator_model::TriggerSettings {
        failure_backoff_ms: Some(60_000),
        failure_streak: Some(1),
        ..Default::default()
    };
    orchestrator.triggers.insert(
        super::super::orchestrator_model::Trigger::Supervisor,
        supervisor_trigger,
    );
    backoff_cfg.orchestrator = Some(orchestrator);
    let backoff_status = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&backoff_cfg),
        None,
        None,
        Some("default:0"),
    );
    let value = serde_json::to_value(backoff_status).unwrap();
    assert_eq!(value["owner_failure_backoff"]["active"], true);
    assert_eq!(value["owner_failure_backoff"]["configured_ms"], 60_000);
    assert_eq!(
        value["owner_failure_backoff"]["earliest_retry_at_ms"],
        60_100
    );
    assert_eq!(
        value["owner_failure_backoff"]["enforced_scope"],
        "owner_model_turns_only"
    );
    assert_eq!(
        value["backoff"]["enforced_scope"],
        "current_session_context_exhaustion_only"
    );
    assert_eq!(value["aside"]["deferred_reason"], "aside_deferred_backoff");
    assert_eq!(value["aside"]["deferred_until_ms"], 60_100);
    // Issue 7: the projection reuses the exact admission binding. A different
    // or unknown current selection is not deferred, and a missing/malformed
    // state source is unknown, not an active or false backoff.
    let other_binding = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&backoff_cfg),
        None,
        None,
        Some("other:1"),
    );
    let value = serde_json::to_value(other_binding).unwrap();
    assert_eq!(value["owner_failure_backoff"]["active"], false);
    assert!(value["owner_failure_backoff"]["earliest_retry_at_ms"].is_null());
    assert!(value["aside"]["deferred_reason"].is_null());
    let no_binding = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&backoff_cfg),
        None,
        None,
        None,
    );
    let value = serde_json::to_value(no_binding).unwrap();
    assert_eq!(value["owner_failure_backoff"]["active"], false);
    with_state(dir.path(), "2026-10-16", |state| {
        state.last_failure_binding = None;
    })
    .unwrap();
    let legacy_binding = operational_status_with_research(
        dir.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&backoff_cfg),
        None,
        None,
        None,
    );
    let value = serde_json::to_value(legacy_binding).unwrap();
    assert_eq!(value["owner_failure_backoff"]["active"], true);
    let missing_state = tempfile::tempdir().unwrap();
    let missing = operational_status_with_research(
        missing_state.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&backoff_cfg),
        None,
        None,
        Some("default:0"),
    );
    let value = serde_json::to_value(missing).unwrap();
    assert!(value["owner_failure_backoff"]["active"].is_null());
    assert!(value["owner_failure_backoff"]["earliest_retry_at_ms"].is_null());
    let corrupt_state = tempfile::tempdir().unwrap();
    fs::create_dir_all(corrupt_state.path().join("model")).unwrap();
    fs::write(
        corrupt_state.path().join("model/supervisor-state.json"),
        b"{not-json",
    )
    .unwrap();
    let corrupt = operational_status_with_research(
        corrupt_state.path(),
        "2026-10-16",
        123,
        SupervisorConfig::Configured(&backoff_cfg),
        None,
        None,
        Some("default:0"),
    );
    let value = serde_json::to_value(corrupt).unwrap();
    assert!(value["owner_failure_backoff"]["active"].is_null());
    assert!(value["owner_failure_backoff"]["earliest_retry_at_ms"].is_null());
    // E2 corrective assertions. These are exercised against the current
    // functions before the fixes; the RED capture records the true behavior.
    // Issue 4: the read-only schedule uses the same inclusive cutoff as
    // admission.
    let mut boundary = base.clone();
    boundary.now_ms = boundary.cutoff_ms;
    assert_eq!(
        owner_tasks::project_research(&boundary).research_state,
        "expired"
    );

    // Issues 1 and 2: a persisted artifact recovered through the lifecycle
    // reconciler is a success, never a failed owner turn, and research never
    // rewrites the decision applied tuple or the reviewed-decides cursor.
    let crash_dir = tempfile::tempdir().unwrap();
    let (crash_c, mut crash_cfg) =
        super::super::decision_owner::owner_fixture_config(crash_dir.path());
    crash_cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings::default(),
        research: Some(owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(owner_tasks::ResearchLaneSettings {
                during_market: true,
                ..Default::default()
            }),
        }),
    });
    crash_cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    let crash_session = crash_c.session.to_string();
    let crash_identity = ResearchIdentity {
        version: 1,
        session: crash_session.clone(),
        evidence_id: evidence_id.clone(),
        policy_sha256: "e".repeat(64),
        prompt_version: digest(owner_tasks::RESEARCH_PROMPT.as_bytes()),
    };
    let crash_task = OwnerTask::Research {
        identity: crash_identity.clone(),
        evidence: evidence.clone(),
    };
    let crash_turn = session::Turn {
        id: "turn-research-crash".into(),
        session: crash_session.clone(),
        trigger: "research".into(),
        prompt_version: crash_identity.prompt_version.clone(),
        notes_version: 0,
        decides_at_start: 9,
        fills_at_start: 0,
        started_at_ms: 10,
        request: json!({
            "owner_task":crash_task,
            "turn_id":"turn-research-crash",
            "research_id":crash_identity.id().unwrap(),
            "model_id":"grok-4.7",
            "cutoff_ms":100_000,
        }),
    };
    session::SessionLog::new(crash_dir.path(), &crash_session)
        .append(
            10,
            session::Body::Turn {
                turn: crash_turn.clone(),
            },
        )
        .unwrap();
    let crash_output = ResearchOutputV1 {
        version: 1,
        evidence_id: evidence_id.clone(),
        summary: "Verified candidate result is advisory only.".into(),
        observations: Vec::new(),
        cited_ids: vec![evidence_id.clone()],
        workspace_refs: Vec::new(),
    };
    write_research_artifact_for_test(
        crash_dir.path(),
        &crash_task,
        &crash_turn,
        Some(&crash_output),
        None,
        20,
    );
    let decision_hashes = AppliedHashes::current(&crash_cfg);
    with_state(crash_dir.path(), &crash_session, |state| {
        state.applied = Some(decision_hashes.clone());
        state.seen_decides = 5;
    })
    .unwrap();
    assert!(super::research::recover(crash_dir.path(), &crash_turn, 20).unwrap());
    let recovered_state = super::state::load(crash_dir.path(), &crash_session).unwrap();
    assert_eq!(recovered_state.failures, 0);
    assert_eq!(recovered_state.owner_failure_streak, None);
    assert_eq!(recovered_state.applied, Some(decision_hashes.clone()));
    assert_eq!(recovered_state.seen_decides, 5);
    assert!(!crash_dir.path().join("model/usage").exists());
    assert!(
        !reconcile_lifecycle_results(crash_dir.path(), &crash_session).unwrap(),
        "a second reconcile must be idempotent"
    );
    assert_eq!(
        super::state::load(crash_dir.path(), &crash_session)
            .unwrap()
            .runs,
        recovered_state.runs
    );

    for outcome in ["ok", "recovered_action_receipt"] {
        let dir = tempfile::tempdir().unwrap();
        let (c, cfg) = super::super::decision_owner::owner_fixture_config(dir.path());
        let session = c.session.to_string();
        let identity = ResearchIdentity {
            version: 1,
            session: session.clone(),
            evidence_id: evidence_id.clone(),
            policy_sha256: "e".repeat(64),
            prompt_version: digest(owner_tasks::RESEARCH_PROMPT.as_bytes()),
        };
        let task = OwnerTask::Research {
            identity: identity.clone(),
            evidence: evidence.clone(),
        };
        let turn_id = format!("turn-research-{outcome}");
        let turn = session::Turn {
            id: turn_id.clone(),
            session: session.clone(),
            trigger: "research".into(),
            prompt_version: identity.prompt_version.clone(),
            notes_version: 0,
            decides_at_start: 9,
            fills_at_start: 0,
            started_at_ms: 10,
            request: json!({
                "owner_task":task,
                "turn_id":turn_id,
                "research_id":identity.id().unwrap(),
                "model_id":"grok-4.7",
                "cutoff_ms":100_000,
            }),
        };
        let log = session::SessionLog::new(dir.path(), &session);
        log.append(10, session::Body::Turn { turn: turn.clone() })
            .unwrap();
        log.append(
            20,
            session::Body::Result {
                result: session::TurnResult {
                    turn_id: turn.id.clone(),
                    finished_at_ms: 20,
                    outcome: outcome.into(),
                    reason: None,
                    reply: String::new(),
                    actions: Vec::new(),
                },
            },
        )
        .unwrap();
        let hashes = AppliedHashes::current(&cfg);
        with_state(dir.path(), &session, |state| {
            state.applied = Some(hashes.clone());
            state.seen_decides = 5;
        })
        .unwrap();
        // A legacy research turn in the trading log never feeds trading
        // accounting: research lives in its own lane (#238).
        assert!(!reconcile_lifecycle_results(dir.path(), &session).unwrap());
        let state = super::state::load(dir.path(), &session).unwrap();
        assert_eq!(state.failures, 0, "{outcome}");
        assert_eq!(state.owner_failure_streak, None, "{outcome}");
        assert_eq!(state.applied, Some(hashes), "{outcome}");
        assert_eq!(state.seen_decides, 5, "{outcome}");
    }

    // Issue 3: corrupt maturity evidence is never admitted; a missing or
    // non-hash result binding is unknown, not a fabricated readable hash.
    let lifecycle_dir = tempfile::tempdir().unwrap();
    let (lifecycle_c, mut lifecycle_cfg) =
        super::super::decision_owner::owner_fixture_config(lifecycle_dir.path());
    lifecycle_cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings::default(),
        research: Some(owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(owner_tasks::ResearchLaneSettings {
                during_market: true,
                ..Default::default()
            }),
        }),
    });
    lifecycle_cfg.workspace = Some(serde_json::from_value(json!({"enabled":true})).unwrap());
    let lifecycle_id = "c".repeat(64);
    for bad in [json!("nothex"), Value::Null] {
        insert_lifecycle_evidence_for_test(
            lifecycle_dir.path(),
            &lifecycle_id,
            "eligible",
            bad.as_str(),
        );
        let projection = super::research::due_evidence(&lifecycle_c, &lifecycle_cfg).unwrap();
        assert!(
            projection
                .evidence
                .iter()
                .all(|item| item.kind != ResearchEvidenceKind::Maturity),
            "corrupt maturity must not be admitted: {bad}"
        );
        assert!(
            projection.unknown.contains(&"research_source_invalid"),
            "corrupt maturity must be an explicit unknown: {bad}"
        );
    }
    let corrupt_schedule =
        super::research::schedule(&lifecycle_c, &lifecycle_cfg, 10, 200_000, true, None).unwrap();
    assert_eq!(corrupt_schedule.research_state, "invalid");
    assert_eq!(
        corrupt_schedule.research_reason,
        Some("owner_research_source_invalid")
    );
    assert_eq!(
        super::research_boundary_reason("research_source_changed"),
        "owner_research_source_changed"
    );
    assert_eq!(
        super::research_boundary_reason("workspace_ref_changed"),
        "owner_research_workspace_changed"
    );

    // A historical turn whose owner_task cannot be verified is a bounded
    // source-unverified reason, never a settings-invalid mislabel. Malformed
    // evidence is source-invalid; unknown errors stay a static bounded code.
    assert_eq!(
        super::research_boundary_reason("owner_research_evidence_invalid"),
        "owner_research_source_invalid"
    );
    assert_eq!(
        super::research_boundary_reason("owner_task_binding_invalid"),
        "owner_research_source_unverified"
    );
    assert_eq!(
        super::research_boundary_reason("private/host/path failure"),
        "owner_research_source_unverified"
    );
    let corrupt_log =
        session::SessionLog::new(&lifecycle_c.directory, &lifecycle_c.session.to_string());
    corrupt_log
        .append(
            1,
            session::Body::Turn {
                turn: session::Turn {
                    id: "corrupt-research-turn".into(),
                    session: lifecycle_c.session.to_string(),
                    trigger: "research".into(),
                    prompt_version: "fixture".into(),
                    notes_version: 0,
                    decides_at_start: 0,
                    fills_at_start: 0,
                    started_at_ms: 1,
                    request: json!({"owner_task":{"kind":"research"}}),
                },
            },
        )
        .unwrap();
    let (view, operational) = super::research_projection(
        &lifecycle_c,
        Some(&lifecycle_cfg),
        None,
        &portfolio,
        10,
        Some(200_000),
    );
    assert_eq!(view["status"], "invalid");
    assert_eq!(view["reason"], "owner_research_source_unverified");
    assert_eq!(view["as_of_ms"], 10);
    assert_eq!(operational.config, SupervisorConfigSource::Unavailable);
    assert_eq!(
        operational.config_reason,
        Some("owner_research_source_unverified")
    );
    assert_eq!(operational.as_of_ms, 10);

    // Issue 5: a changed pinned workspace source is detected before any paid
    // call and mapped to a bounded research reason, never settings-invalid.
    let pinned_dir = tempfile::tempdir().unwrap();
    let (pinned_c, mut pinned_cfg) =
        super::super::decision_owner::owner_fixture_config(pinned_dir.path());
    pinned_cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings::default(),
        research: Some(owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(owner_tasks::ResearchLaneSettings {
                during_market: true,
                ..Default::default()
            }),
        }),
    });
    let workspace_settings: super::super::workspace::WorkspaceSettings =
        serde_json::from_value(json!({"enabled":true})).unwrap();
    pinned_cfg.workspace = Some(workspace_settings.clone());
    insert_lifecycle_evidence_for_test(
        pinned_dir.path(),
        &"f".repeat(64),
        "eligible",
        Some(&"f".repeat(64)),
    );
    let source_session = "2026-10-15";
    let source_book = pinned_c
        .sessions_root
        .as_ref()
        .unwrap()
        .join(source_session);
    fs::create_dir_all(&source_book).unwrap();
    fs::write(
        source_book.join("book-identity.json"),
        serde_json::to_vec(&json!({"binding":super::super::book_binding(&pinned_c).unwrap()}))
            .unwrap(),
    )
    .unwrap();
    super::super::workspace::ensure(&source_book, &workspace_settings).unwrap();
    super::super::workspace::execute(
        &source_book,
        &workspace_settings,
        super::super::workspace::WorkspaceOp::Write {
            path: "notes/pinned.md".into(),
            text: "pinned source".into(),
        },
    )
    .unwrap();
    let workspace_state =
        super::super::workspace::seed_state(&source_book, &workspace_settings, source_session, 50);
    let pinned_context = session::SeedContext {
        source_turn_id: "turn-pinned-source".into(),
        standing_instructions: "standing".into(),
        positions: json!([]),
        directives: json!([]),
        alerts: json!([]),
        research_state: json!({"available":true,"tasks":[]}),
        unresolved_work_refs: json!({"available":true,"refs":[]}),
        review_state: Value::Null,
        workspace_state: workspace_state.clone(),
    };
    let pinned_seed = session::build_seed(
        pinned_dir.path(),
        source_session,
        &pinned_c.session.to_string(),
        &pinned_context,
    );
    session::write_seed(
        pinned_dir.path(),
        &pinned_c.session.to_string(),
        &pinned_seed,
    )
    .unwrap();
    // The SAME selected evidence and pinned snapshot that feed the research
    // frame yield the public scalars at the producer completion sample. The
    // expected hash is computed from the actual pinned read, not a fixture
    // constant, and no raw path/ref array is retained or serialized.
    let mut pinned_portfolio = super::super::tests::test_portfolio();
    pinned_portfolio.closed = true;
    let pinned_schedule =
        super::research::schedule(&pinned_c, &pinned_cfg, 100, 200_000, true, None).unwrap();
    assert_eq!(pinned_schedule.workspace_ref_count, Some(1));
    let reference: super::super::workspace::WorkspaceRefV1 =
        serde_json::from_value(workspace_state["refs"][0].clone()).unwrap();
    let pinned_value =
        super::super::workspace::read_pinned(&source_book, &workspace_settings, &reference)
            .unwrap();
    let pinned_hash = digest(
        &super::research::canonical_bytes(&json!([pinned_value]))
            .expect("canonical pinned snapshot"),
    );
    assert_eq!(
        pinned_schedule.workspace_snapshot_sha256.as_deref(),
        Some(pinned_hash.as_str())
    );
    let serialized = serde_json::to_value(&pinned_schedule).unwrap();
    assert!(serialized.get("workspace_refs").is_none());
    assert!(!serialized.to_string().contains("pinned.md"));
    assert!(!pinned_c.directory.join("model/usage").exists());
    let (pinned_view, _) = super::research_projection(
        &pinned_c,
        Some(&pinned_cfg),
        None,
        &pinned_portfolio,
        100,
        Some(200_000),
    );
    assert_eq!(pinned_view["workspace_ref_count"], 1);
    assert_eq!(
        pinned_view["workspace_snapshot_sha256"],
        serde_json::json!(pinned_hash)
    );
    assert!(pinned_view.get("workspace_refs").is_none());
    assert!(!pinned_view.to_string().contains("pinned.md"));
    fs::write(source_book.join("workspace/notes/pinned.md"), b"changed").unwrap();
    let pinned_error = super::research::schedule(&pinned_c, &pinned_cfg, 100, 200_000, true, None)
        .unwrap_err()
        .to_string();
    assert_eq!(pinned_error, "owner_research_workspace_changed");
    let pinned_status = super::research_projection(
        &pinned_c,
        Some(&pinned_cfg),
        None,
        &pinned_portfolio,
        100,
        Some(200_000),
    )
    .0;
    assert_eq!(pinned_status["status"], "invalid");
    assert_eq!(pinned_status["reason"], "owner_research_workspace_changed");

    // Unknown source sessions and a cross-account predecessor root fail closed
    // without inventing a path or repairing permissions.
    let missing_dir = tempfile::tempdir().unwrap();
    let (missing_c, mut missing_cfg) =
        super::super::decision_owner::owner_fixture_config(missing_dir.path());
    missing_cfg.owner_tasks = Some(owner_tasks::OwnerTasksSettings {
        version: 1,
        agenda: None,
        review: owner_tasks::ReviewSettings::default(),
        research: Some(owner_tasks::OwnerResearchSettings {
            idle_trigger: None,
            enabled: true,
            cutoff_minute: 1020,
            max_evidence: 4,
            quote_capture: None,
            observation_capture: None,
            lane: Some(owner_tasks::ResearchLaneSettings {
                during_market: true,
                ..Default::default()
            }),
        }),
    });
    missing_cfg.workspace = Some(workspace_settings.clone());
    let missing_ref = json!({
        "version":1,
        "source_session":"2026-10-14",
        "path":"notes/pinned.md",
        "sha256":"f".repeat(64),
        "captured_at_ms":1,
        "kind":"notes"
    });
    let missing_context = session::SeedContext {
        source_turn_id: "turn-missing-source".into(),
        standing_instructions: "standing".into(),
        positions: json!([]),
        directives: json!([]),
        alerts: json!([]),
        research_state: json!({"available":true,"tasks":[]}),
        unresolved_work_refs: json!({"available":true,"refs":[]}),
        review_state: Value::Null,
        workspace_state: json!({"version":1,"refs":[missing_ref],"available":true}),
    };
    let missing_seed = session::build_seed(
        missing_dir.path(),
        "2026-10-14",
        &missing_c.session.to_string(),
        &missing_context,
    );
    session::write_seed(
        missing_dir.path(),
        &missing_c.session.to_string(),
        &missing_seed,
    )
    .unwrap();
    let mut missing_evidence = evidence.clone();
    assert_eq!(
        super::research::pinned_workspace(&missing_c, &missing_cfg, &mut missing_evidence)
            .unwrap_err()
            .to_string(),
        "owner_research_workspace_source_missing"
    );
    let cross_source = missing_c.sessions_root.as_ref().unwrap().join("2026-10-14");
    fs::create_dir_all(&cross_source).unwrap();
    fs::write(
        cross_source.join("book-identity.json"),
        serde_json::to_vec(&json!({"binding":"0".repeat(64)})).unwrap(),
    )
    .unwrap();
    let mut cross_evidence = evidence.clone();
    assert_eq!(
        super::research::pinned_workspace(&missing_c, &missing_cfg, &mut cross_evidence)
            .unwrap_err()
            .to_string(),
        "owner_research_workspace_source_invalid"
    );

    // Issue 6: a symlinked research directory or ancestor is refused before
    // any mkdir/chmod touches the outside target.
    let symlink_root = tempfile::tempdir().unwrap();
    fs::create_dir_all(symlink_root.path().join("model")).unwrap();
    fs::set_permissions(
        symlink_root.path().join("model"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let outside_research = outside.path().join("research-target");
    fs::create_dir(&outside_research).unwrap();
    fs::set_permissions(&outside_research, fs::Permissions::from_mode(0o755)).unwrap();
    let canary = outside_research.join("canary");
    fs::write(&canary, b"outside").unwrap();
    fs::set_permissions(&canary, fs::Permissions::from_mode(0o644)).unwrap();
    std::os::unix::fs::symlink(
        &outside_research,
        symlink_root.path().join("model/research"),
    )
    .unwrap();
    assert_eq!(
        super::research::research_directory(symlink_root.path(), true)
            .unwrap_err()
            .to_string(),
        "owner_research_path_invalid"
    );
    assert_eq!(
        fs::metadata(&outside_research).unwrap().mode() & 0o777,
        0o755
    );
    assert_eq!(fs::read(&canary).unwrap(), b"outside");

    let ancestor_root = tempfile::tempdir().unwrap();
    let outside_model = outside.path().join("model-target");
    fs::create_dir(&outside_model).unwrap();
    let outside_model_research = outside_model.join("research");
    fs::create_dir(&outside_model_research).unwrap();
    fs::set_permissions(&outside_model_research, fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(&outside_model, ancestor_root.path().join("model")).unwrap();
    assert_eq!(
        super::research::research_directory(ancestor_root.path(), true)
            .unwrap_err()
            .to_string(),
        "owner_research_path_invalid"
    );
    assert_eq!(
        fs::metadata(&outside_model_research).unwrap().mode() & 0o777,
        0o755
    );

    let private_root = tempfile::tempdir().unwrap();
    let private_directory = super::research::research_directory(private_root.path(), true).unwrap();
    assert_eq!(
        fs::metadata(&private_directory).unwrap().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(private_root.path().join("model"))
            .unwrap()
            .mode()
            & 0o777,
        0o700
    );
}
