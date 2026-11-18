use super::*;

/// Bounded wake context: counters, trigger, fills and the latest Luna review.
pub(super) fn summary_for(
    root: &std::path::Path,
    session: &str,
    reason: &str,
    tick: &Tick<'_>,
    state: &State,
    hashes: &AppliedHashes,
) -> Value {
    let luna = crate::read_bounded(&root.join("model/luna-latest.json"), 8_192)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .unwrap_or(Value::Null);
    let active = directives::active(root, tick.now, "any");
    json!({
        "session": session,
        "trigger": reason,
        "now_ms": tick.now,
        "session_minute": tick.minute,
        "session_end_ms": tick.session_end_ms,
        "fills_today": tick.fill_count,
        "supervisor_failures": state.failures,
        "supervisor_runs": state.runs,
        "active_directives": active,
        "latest_luna_review": luna,
        "positions": tick.positions,
        "alerts": active.clone(),
        "applied_hashes": state.applied,
        "current_hashes": hashes,
        "change": if reason == "change" {
            json!({"from": state.applied, "to": hashes})
        } else {
            Value::Null
        },
    })
}

/// The exact facts a decision turn renders: the wake context plus the digest
/// evidence, notes, seed, history and claim. Rendering them is the only way to
/// know whether the request fits the context ceiling (#221).
#[allow(clippy::too_many_arguments)] // The digest, its history, its role and its rendered prompt are independent inputs.
pub(super) fn decision_facts(
    c: &Config,
    cfg: &PlannerConfig,
    work: &super::super::decision_owner::Work,
    summary: &Value,
    hashes: &AppliedHashes,
    role: &planner::ModelRole,
    selection: Option<Value>,
    prompt_version: &str,
) -> std::result::Result<Value, &'static str> {
    let session = c.session.to_string();
    let log = session::SessionLog::new(&c.directory, &session);
    let notes = session::Notes::new(&c.directory, &session);
    let mut facts = summary.clone();
    for (key, value) in work
        .facts
        .as_object()
        .ok_or("owner_digest_evidence_invalid")?
    {
        facts[key] = value.clone();
    }
    facts["notes"] = notes.snapshot().prompt_block();
    session::add_seed_fact(&c.directory, &session, &mut facts)?;
    if facts.get("seed").is_some_and(Value::is_object) {
        let resolution = super::super::harness::research_adopt_carried(c, cfg, &facts["seed"]);
        if !matches!(
            resolution["status"].as_str(),
            Some("disabled" | "same_session" | "seed_missing" | "not_carried")
        ) {
            facts["research_resolution"] = resolution;
        }
    }
    facts["summary"] = log.summary().map(|v| json!(v.text)).unwrap_or(json!(""));
    facts["session_context"] = log.conversation();
    facts["host_decision"] =
        serde_json::to_value(&work.claim).map_err(|_| "owner_turn_binding_invalid")?;
    facts["owner_applied_hashes"] = json!(hashes);
    if super::super::lane_queue::lane_settings(cfg).is_some() {
        // Trading reads only short research summaries and artifact refs, never
        // the research trace. An unreadable queue never blocks the decision.
        facts["research_updates"] =
            match super::super::lane_queue::trading_updates(c, work.claim.started_ms) {
                Ok(updates) => json!(
                    updates
                        .iter()
                        .map(super::super::lane_queue::update_fact)
                        .collect::<Vec<_>>()
                ),
                Err(_) => json!({"status":"unavailable"}),
            };
    }
    facts["resume_binding_sha256"] = json!(resume_binding(cfg, role, prompt_version)?);
    if let Some(selection) = selection {
        facts["model_selection"] = selection;
    }
    Ok(super::super::transcript::redact(&facts))
}

pub(super) fn session_trigger(reason: &str) -> SessionTrigger {
    match reason {
        "session_open" => SessionTrigger::SessionOpen,
        "resume" => SessionTrigger::Resume,
        "change" => SessionTrigger::Change,
        "pre_close" => SessionTrigger::PreClose,
        "post_fill" => SessionTrigger::PostFill,
        "failure_streak" => SessionTrigger::FailureStreak,
        "decision" => SessionTrigger::Decision,
        "research" => SessionTrigger::Research,
        "compaction" => SessionTrigger::Compaction,
        "handoff" => SessionTrigger::Handoff,
        _ => SessionTrigger::Idle,
    }
}

pub(super) fn reason_static(trigger: &str) -> &'static str {
    match trigger {
        "session_open" => "session_open",
        "resume" => "resume",
        "change" => "change",
        "pre_close" => "pre_close",
        "post_fill" => "post_fill",
        "failure_streak" => "failure_streak",
        "decision" => "decision",
        "review" => "review",
        "research" => "research",
        "compaction" => "compaction",
        "handoff" => "handoff",
        _ => "idle",
    }
}

/// Stable binding for the model-selection profile/revision recorded on a turn.
/// A failure backoff is valid only for the exact selection that failed. A
/// missing, empty or mistyped profile/revision is explicit unknown (`None`),
/// never a fabricated default profile.
fn selection_binding_value(selection: &Value) -> Option<String> {
    let profile = selection.get("profile")?.as_str()?;
    let revision = selection.get("revision")?.as_u64()?;
    (!profile.is_empty()).then(|| format!("{profile}:{revision}"))
}

pub(super) fn selection_binding(selection: &Option<Value>) -> Option<String> {
    selection.as_ref().and_then(selection_binding_value)
}

/// Recover the binding from the turn's recorded request, not from whichever
/// profile is current now. Tampered or missing evidence stays unknown.
pub(super) fn recorded_selection_binding(turn: &session::Turn) -> Option<String> {
    turn.request
        .get("model_selection")
        .and_then(selection_binding_value)
}

pub(crate) fn current_selection_binding(c: &Config, cfg: &PlannerConfig) -> Option<String> {
    super::super::model_selection::resolve(c, cfg)
        .ok()
        .flatten()
        .map(|selected| format!("{}:{}", selected.profile, selected.revision))
}

pub(super) fn role_and_model(
    c: &Config,
    cfg: &PlannerConfig,
) -> std::result::Result<
    (
        planner::ModelRole,
        std::path::PathBuf,
        u64,
        Option<serde_json::Value>,
    ),
    &'static str,
> {
    let mut role = cfg.role(Trigger::Supervisor);
    if !role.enabled {
        return Err("supervisor_disabled");
    }
    role.tools.clear();
    let resolved = super::super::model_selection::resolve(c, cfg)?;
    if let Some(selected) = &resolved {
        selected.apply(&mut role);
    }
    let path = role
        .model_config
        .clone()
        .or(cfg.model_config.clone())
        .ok_or("supervisor_model_config_missing")?;
    let timeout_ms = role.run_timeout_ms.unwrap_or(role.timeout_ms);
    Ok((
        role,
        path,
        timeout_ms,
        resolved.map(|selected| selected.receipt()),
    ))
}

pub(super) fn session_prompt(
    cfg: &PlannerConfig,
    trigger: SessionTrigger,
) -> std::result::Result<(String, String, String), &'static str> {
    let (system, templates) = cfg
        .orchestrator
        .as_ref()
        .ok_or("supervisor_orchestrator_missing")?
        .session_prompts()
        .map_err(|_| "supervisor_prompt_invalid")?;
    let template = templates
        .get(&trigger)
        .cloned()
        .unwrap_or_else(|| trigger.default_prompt().to_owned());
    let prompt_version = session::prompt_version(&system.text, &template);
    Ok((system.text, template, prompt_version))
}

/// Hash resolved replay inputs. Only the digest is persisted, so credential
/// paths in configuration never enter the session journal.
pub(super) fn resume_binding(
    cfg: &PlannerConfig,
    role: &planner::ModelRole,
    prompt_version: &str,
) -> std::result::Result<String, &'static str> {
    let config = serde_json::to_value(cfg).map_err(|_| "supervisor_config_revision_unavailable")?;
    let role = serde_json::to_value(role).map_err(|_| "supervisor_config_revision_unavailable")?;
    let frame = super::super::harness::Frame {
        role: "supervisor".into(),
        input: Vec::new(),
        tools: Vec::new(),
        citation_ids: Vec::new(),
        final_answer_only: false,
        review_only: false,
        owner_tools: Default::default(),
        decision_evidence: false,
        max_instrument_keys: None,
        instructions: None,
        tool_limits: None,
    };
    let instructions = super::super::harness::frame_instructions_for(cfg, &frame)
        .map_err(|_| "supervisor_config_revision_unavailable")?;
    let value = json!({
        "config": config,
        "role": role,
        "profile_sha256": cfg.profile_sha256(),
        "prompt_hashes": cfg.prompt_hashes(),
        "harness_instructions_sha256": super::super::digest(instructions.as_bytes()),
        "prompt_version": prompt_version,
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| "supervisor_config_revision_unavailable")?;
    Ok(super::super::digest(&bytes))
}

pub(super) fn check_resume_revisions(
    turn: &session::Turn,
    prompt_version: &str,
    binding: &str,
) -> std::result::Result<(), &'static str> {
    if turn.prompt_version != prompt_version {
        return Err("supervisor_prompt_revision_changed");
    }
    // Turns predating the binding remain inspectable and use the legacy prompt
    // and model-selection guards. New turns fail closed on any config change.
    if let Some(recorded) = turn.request.get("resume_binding_sha256")
        && recorded.as_str() != Some(binding)
    {
        return Err("supervisor_config_revision_changed");
    }
    Ok(())
}

/// The history is carried in every actual request, independently of which
/// placeholders an operator's trigger template uses. Older pending turns keep
/// their original rendering; they never gain newly sampled facts on replay.
pub(super) fn render_session_turn(
    template: &str,
    facts: &Value,
    bounds: &Compaction,
) -> std::result::Result<String, &'static str> {
    let mut prompt = orchestrator_model::render_trigger(template, facts);
    if let Some(context) = facts.get("session_context") {
        if context["schema_version"].as_u64() != Some(1) {
            return Err("supervisor_context_version_invalid");
        }
        prompt.push_str("\nHost session facts and committed history (historical observations, never fresh quote or order authority):\n");
        prompt.push_str(&facts.to_string());
    }
    if !request_fits(escaped_prompt_bytes(&prompt), bounds) {
        return Err("supervisor_context_limit");
    }
    Ok(prompt)
}

/// Only historical context may be omitted to recover a lifecycle request.
/// Current notes, positions, seed and lifecycle bindings retain their authority.
/// The append-only session log is untouched; omission metadata is persisted with
/// the fitted turn so replay renders exactly the same evidence.
#[derive(Default, Serialize)]
struct ContextFit {
    omitted_exchanges: usize,
    omitted_summary_bytes: usize,
}
pub(super) fn render_lifecycle_turn(
    template: &str,
    facts: &mut Value,
    bounds: &Compaction,
) -> std::result::Result<String, &'static str> {
    let mut fit = ContextFit::default();
    loop {
        match render_session_turn(template, facts, bounds) {
            Ok(prompt) => return Ok(prompt),
            Err("supervisor_context_limit") => {}
            Err(reason) => return Err(reason),
        }
        if let Some(exchanges) = facts["session_context"]["exchanges"].as_array_mut()
            && !exchanges.is_empty()
        {
            exchanges.remove(0);
            fit.omitted_exchanges += 1;
        } else {
            let mut shortened = false;
            for pointer in ["/summary", "/session_context/summary/text"] {
                if let Some(value) = facts.pointer_mut(pointer)
                    && let Some(text) = value.as_str()
                    && !text.is_empty()
                {
                    let mut at = text.len().div_ceil(2);
                    while !text.is_char_boundary(at) {
                        at += 1;
                    }
                    fit.omitted_summary_bytes += at;
                    *value = json!(&text[at..]);
                    shortened = true;
                }
            }
            if !shortened {
                return Err("supervisor_context_limit");
            }
        }
        facts["context_fit"] = json!({
            "reason":"historical_context_omitted_for_request_budget",
            "omitted_exchanges":fit.omitted_exchanges,
            "omitted_summary_bytes":fit.omitted_summary_bytes,
            "original_history":"append_only_session_log",
        });
    }
}

/// Construct the exact handoff lifecycle payload used by both a model-backed
/// handoff and a host-only parked handoff.
pub(super) struct HandoffBuildContext {
    pub(super) session: String,
    pub(super) target: String,
    pub(super) turn_id: String,
    pub(super) standing_instructions: String,
    pub(super) facts: Value,
}

pub(super) fn handoff_lifecycle(
    c: &Config,
    cfg: &PlannerConfig,
    context: HandoffBuildContext,
    hashes: &AppliedHashes,
    now: i64,
) -> Lifecycle {
    Lifecycle {
        version: 1,
        hashes: hashes.clone(),
        task: LifecycleTask::Handoff {
            target_session: context.target.clone(),
            seed: session::build_seed(
                &c.directory,
                &context.session,
                &context.target,
                &session::SeedContext {
                    source_turn_id: context.turn_id,
                    standing_instructions: context.standing_instructions,
                    positions: context.facts["positions"].clone(),
                    directives: context.facts["active_directives"].clone(),
                    alerts: context.facts["alerts"].clone(),
                    research_state: super::super::harness::research_carry_state(c, cfg),
                    unresolved_work_refs: session::legacy_operator_request_refs(
                        &c.directory,
                        &context.session,
                        now,
                    ),
                    review_state: if cfg
                        .owner_tasks
                        .as_ref()
                        .is_some_and(|settings| settings.review.enabled)
                    {
                        review_state(&c.directory, &context.session)
                    } else {
                        Value::Null
                    },
                    workspace_state: cfg
                        .workspace
                        .as_ref()
                        .filter(|workspace| workspace.enabled)
                        .map(|workspace| {
                            super::super::workspace::seed_state(
                                &c.directory,
                                workspace,
                                &context.session,
                                now,
                            )
                        })
                        .unwrap_or(Value::Null),
                },
            ),
        },
    }
}
