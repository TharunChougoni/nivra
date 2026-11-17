//! Role-isolated, one-shot Responses tool rounds over original usage authority.
use super::*;
pub(crate) const MAX_INSTRUMENT_KEYS: usize = 20;
const TOOL_OUTPUT_MAX_BYTES: usize = 64_000;
const MAX_EVIDENCE_IDS: usize = 12;
use planner::{Backend, CallGuard, ModelRole, PlannerConfig, ToolsSettings};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::DirBuilderExt;
mod tools;
pub(crate) use tools::*;
mod bundle;
use bundle::*;
mod failure_window;
pub(crate) use failure_window::{
    PlannerInput, close as close_failure_window, pre_provider_stage, retain_planner_input,
};
pub(crate) mod research;
pub(crate) use research::{
    LaunchBook, OwnerResearchRuntime, ResearchLaunch, ResearchSettings,
    adopt_carried_research as research_adopt_carried,
    capture_observation_tick as research_capture_observation, carry_state as research_carry_state,
    helper_entry as research_candidate_helper, observation_status as research_observation_status,
    status as research_status, tick_owner_tasks as research_tick_owner_tasks,
    validate_files as validate_research_files,
};

pub(crate) const DATA_OWNER_PROMPT: &str =
    include_str!("../../../../packaging/rust/prompts/data_owner.md");
pub(crate) const SCORER_PROMPT: &str = include_str!("../../../../packaging/rust/prompts/scorer.md");
pub(crate) const SUPERVISOR_PROMPT: &str =
    include_str!("../../../../packaging/rust/prompts/supervisor.md");
pub(crate) const ORCHESTRATOR_PROMPT: &str =
    include_str!("../../../../packaging/rust/prompts/orchestrator.md");
/// One exact project objective owned by the host and rendered into every role.
pub(crate) const OBJECTIVE: &str = include_str!("../../../../packaging/rust/prompts/objective.md");
pub(crate) fn objective() -> &'static str {
    OBJECTIVE.trim()
}
pub(crate) fn publish_validated(c: &Config, name: &str, role: &ModelRole, raw: &str) {
    if let Ok(binding) = book_binding(c) {
        let _scope = nivra_cli::model_stream::Scope::new(
            c.directory.join("model/live-stream.json"),
            binding,
            None,
        );
        nivra_cli::model_stream::publish(
            name,
            &role.model_id,
            &digest(raw.as_bytes()),
            "host_validated",
            "Output validated by the host; this is not a fill or execution authorization",
        );
    }
}
pub(crate) fn instructions(role: &str) -> Result<&'static str> {
    match role {
        "digest" => Ok(DATA_OWNER_PROMPT),
        "scorer" => Ok(SCORER_PROMPT),
        "supervisor" => Ok(SUPERVISOR_PROMPT),
        _ => Err("harness_role_invalid".into()),
    }
}

pub(crate) fn frame_instructions(frame: &Frame) -> Result<String> {
    frame_instructions_with(None, frame)
}

pub(crate) fn frame_instructions_for(cfg: &PlannerConfig, frame: &Frame) -> Result<String> {
    frame_instructions_with(Some(cfg), frame)
}

fn frame_instructions_with(cfg: Option<&PlannerConfig>, frame: &Frame) -> Result<String> {
    if frame.decision_evidence && frame.role != "digest" {
        return Err("harness_role_invalid".into());
    }
    let text = if let Some(instructions) = &frame.instructions {
        instructions.clone()
    } else if frame.final_answer_only {
        format!(
            "{}\nTools are disabled for this final answer. Return the strict final JSON; preserve the paper mandate, fee realism and evidence limitations.",
            system_instructions_with(cfg, &frame.role, &frame.citation_ids)?
        )
    } else {
        system_instructions_with(cfg, &frame.role, &frame.citation_ids)?
    };
    Ok(text)
}

pub(crate) fn objective_for(cfg: &PlannerConfig) -> &str {
    cfg.prompt_set()
        .map(|p| p.objective())
        .unwrap_or_else(|| OBJECTIVE.trim())
}

#[cfg(test)]
pub(crate) fn system_instructions(role: &str, ids: &[String]) -> Result<String> {
    system_instructions_with(None, role, ids)
}

fn system_instructions_with(
    cfg: Option<&PlannerConfig>,
    role: &str,
    ids: &[String],
) -> Result<String> {
    // Explicit session instructions win, then the experiment-profile role
    // prompt. With a policy and neither override, the owner gets its compiled
    // standing prompt.
    let session_system_prompt = if role == "supervisor" {
        cfg.and_then(|cfg| cfg.orchestrator.as_ref())
            .filter(|orchestrator| orchestrator.system_prompt.is_some())
            .map(|orchestrator| {
                orchestrator
                    .session_prompts()
                    .map(|(prompt, _)| prompt.text)
            })
            .transpose()?
    } else {
        None
    };
    let profile_prompt = cfg
        .and_then(PlannerConfig::prompt_set)
        .and_then(|prompts| prompts.role_for(role));
    let owner_default = role == "supervisor" && cfg.is_some() && profile_prompt.is_none();
    let base = session_system_prompt
        .as_deref()
        .or(profile_prompt)
        .unwrap_or_else(|| {
            if owner_default {
                ORCHESTRATOR_PROMPT
            } else {
                instructions(role).unwrap_or("")
            }
        });
    let base = if base.is_empty() {
        instructions(role)?.to_owned()
    } else {
        base.to_owned()
    };
    // Only the retired post-close review roles carried citation IDs.
    if !ids.is_empty() {
        return Err("harness_citation_ids_invalid".into());
    }
    Ok(if matches!(role, "digest" | "scorer" | "supervisor") {
        let objective = cfg.map(objective_for).unwrap_or_else(|| OBJECTIVE.trim());
        format!("{objective}\n\n{base}")
    } else {
        base
    })
}

fn skip_false(value: &bool) -> bool {
    !*value
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OwnerTools {
    pub research: bool,
    /// One bounded owner research turn over existing evidence. Omitted unless
    /// the reviewed research capability admitted this exact frame.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub research_turn: bool,
    /// Optional archived-receipt quote reader for a research turn. Omitted
    /// unless the reviewed quote-capture setting is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub quote_capture: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate: bool,
    /// Advertise the optional execution-spec argument only when the reviewed
    /// execution capability is enabled. Omitted from serialized frames when
    /// false so archived request bytes stay unchanged.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate_execution: bool,
    /// Advertise the optional prospective-observation argument only when the
    /// reviewed observation capability is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate_observation: bool,
    /// Advertise the optional typed cross-session parent reference only when
    /// the candidate capability is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub candidate_parent_ref: bool,
    pub workspace: bool,
    /// Config-gated orchestrator web tools (#222). Omitted from serialized
    /// frames unless enabled so archived request bytes stay unchanged. The two
    /// availability flags record exactly which search/news tools were offered,
    /// so the provider-call reconstruction stays byte-identical.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub web: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub web_search: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub web_news: bool,
    /// Trading decision turns may post a typed request to the research lane
    /// (#239). Omitted unless the research lane is enabled.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub research_request: bool,
}
impl OwnerTools {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Frame {
    pub role: String,
    pub input: Vec<Value>,
    pub tools: Vec<Value>,
    #[serde(default, skip_serializing_if = "OwnerTools::is_empty")]
    pub owner_tools: OwnerTools,
    #[serde(default)]
    pub citation_ids: Vec<String>,
    /// Host-forced final answer. Tools are empty and the provider call omits them.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub final_answer_only: bool,
    /// Host-forced sealed owner review: no tools and review-only instructions.
    /// Omitted unless true so archived and legacy request bytes stay unchanged.
    #[serde(default, skip_serializing_if = "skip_false")]
    pub review_only: bool,
    #[serde(default, skip_serializing_if = "skip_false")]
    pub decision_evidence: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_instrument_keys: Option<usize>,
    /// Host-supplied exact instructions for roles whose template is configured
    /// outside the compiled prompts (the scorer). Never includes credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// Non-default call-time retrieval bounds. Absent keeps the historical
    /// default tool schemas byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_limits: Option<planner::ToolsSettings>,
}
mod digest;
#[cfg(test)]
pub(super) use digest::{OwnerDigest, owner_digest_bounds_violation};
pub(super) use digest::{
    normalize_owner_digest, owner_digest_eligible, owner_facts_sha256, owner_feed_usable,
    validate_owner_digest,
};

/// A rejected digest body is retained beside its run record so the next
/// bounds/schema/JSON rejection is diagnosable from the session archive (#220).
/// The copy is bounded and credential-redacted; it never becomes model work.
const REJECTED_DIGEST_MAX_BYTES: usize = 1_048_576;

fn digest_rejection_retained(reason: &str) -> bool {
    matches!(
        reason,
        "harness_digest_json_invalid"
            | "harness_digest_schema_invalid"
            | "harness_digest_bounds_invalid"
    )
}

fn retain_rejected_digest(archive: &Path, raw: &str) -> bool {
    // Prefer field-level redaction of a decodable body so the retained copy
    // stays diagnosable; an undecodable body falls back to the conservative
    // whole-string redactor.
    let redacted = serde_json::from_str::<Value>(raw)
        .map(|value| super::transcript::redact(&value))
        .unwrap_or_else(|_| super::transcript::redact(&json!(raw)));
    let body = match redacted {
        Value::String(text) => text,
        other => other.to_string(),
    };
    let original_bytes = body.len();
    let (body, truncated) = if original_bytes > REJECTED_DIGEST_MAX_BYTES {
        // Keep a deterministic prefix without splitting a UTF-8 boundary.
        let mut end = REJECTED_DIGEST_MAX_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        (body[..end].to_owned(), true)
    } else {
        (body, false)
    };
    let record = json!({
        "body": body,
        "original_bytes": original_bytes,
        "truncated": truncated,
        "max_bytes": REJECTED_DIGEST_MAX_BYTES,
        "paper_only": true,
    });
    match rollover::atomic(&archive.join("rejected-digest.json"), &record) {
        Ok(()) => true,
        Err(_) => {
            eprintln!("harness_rejected_digest_unavailable");
            false
        }
    }
}

/// Normalize one digest body, retaining the rejected text when a bounds,
/// schema or JSON rejection discards it. Returns the original result.
fn normalize_digest_body(
    archive: &Path,
    raw: &str,
    bundle: &Value,
    tool_ids: &BTreeSet<String>,
) -> (Result<String>, bool) {
    let result = normalize_owner_digest(raw, bundle, tool_ids);
    let rejected = result
        .as_ref()
        .is_err_and(|error| digest_rejection_retained(&error.to_string()));
    let retained = rejected && retain_rejected_digest(archive, raw);
    (result, retained)
}

fn checked_output(output: &[Value], native: &[String]) -> Result<()> {
    for item in output {
        match item["type"].as_str() {
            Some("message")
                if item["role"] == "assistant"
                    && (item["phase"].is_null()
                        || ["commentary", "final_answer"]
                            .iter()
                            .any(|p| item["phase"] == *p)) => {}
            Some("reasoning" | "function_call") => {}
            Some("web_search_call") if native.iter().any(|s| s == "web_search") => {}
            Some("x_search_call") if native.iter().any(|s| s == "x_search") => {}
            Some("custom_tool_call")
                if native.iter().any(|s| s == "x_search")
                    && item["call_id"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("xs_"))
                    && item["name"].as_str().is_some_and(|s| {
                        [
                            "x_user_search",
                            "x_keyword_search",
                            "x_semantic_search",
                            "x_thread_fetch",
                        ]
                        .contains(&s)
                    }) => {}
            _ => return Err("harness_output_role_or_tool_invalid".into()),
        }
    }
    Ok(())
}
pub(crate) fn final_text(output: &[Value]) -> Result<String> {
    let explicit: Vec<_> = output
        .iter()
        .filter(|i| i["type"] == "message" && i["phase"] == "final_answer")
        .collect();
    let messages = if explicit.is_empty() {
        output
            .iter()
            .filter(|i| i["type"] == "message" && i["phase"].is_null())
            .collect::<Vec<_>>()
    } else {
        explicit
    };
    if messages.len() != 1 || messages[0]["role"] != "assistant" {
        return Err("harness_final_output_ambiguous".into());
    }
    let text = messages[0]["content"]
        .as_array()
        .ok_or("harness_final_output_missing")?
        .iter()
        .filter(|p| p["type"] == "output_text")
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        return Err("harness_final_output_missing".into());
    }
    Ok(text)
}

pub(super) fn minimal_owner_digest() -> String {
    json!({
        "complete": false,
        "as_of_ms": 0,
        "coverage": {"universe": 0, "scanned": 0, "quoted": 0, "featured": 0, "experiments": 0},
        "gaps": ["digest_run_deadline"],
        "changed_prospects": [],
        "active_rankings": [],
        "candidate_rankings": [],
        "bar_context": [],
        "spread_depth": [],
        "holdings": [],
        "cash": {"available": "0"},
        "fees": {"per_order": "0", "buy_bps": "0", "sell_bps": "0"},
        "recent_actions": [],
        "experiments": [],
        "evidence_ids": []
    })
    .to_string()
}

/// A digest run deadline still returns a minimal digest so the owner can
/// continue. Every other role keeps the original hard stop.
fn stopped_result(name: &str) -> Result<String> {
    match name {
        "digest" => {
            let text = minimal_owner_digest();
            validate_owner_digest(&text, &Value::Null)?;
            Ok(text)
        }
        _ => Err("harness_deadline_expired".into()),
    }
}

/// Two independent readings for one admitted model turn: the actual observation
/// wall (model-visible timestamps) and the monotonic duration budget. They are
/// never mixed into a fabricated UTC value.
#[derive(Clone, Copy)]
pub(crate) struct DomainClock<'a, 'b> {
    guard: &'a CallGuard<'b>,
}

impl<'a, 'b> DomainClock<'a, 'b> {
    pub(crate) fn new(guard: &'a CallGuard<'b>) -> Self {
        Self { guard }
    }
    pub(crate) fn wall_ms(&self) -> i64 {
        self.guard.wall_ms()
    }
    pub(crate) fn elapsed_ms(&self) -> i64 {
        self.guard.elapsed_ms()
    }
    pub(crate) fn projected_wall_ms(&self) -> i64 {
        self.guard.projected_wall_ms()
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run(
    c: &Config,
    cfg: &PlannerConfig,
    name: &str,
    role: &ModelRole,
    prompt: &str,
    bundle: &Value,
    backend: &mut dyn Backend,
    guard: &CallGuard<'_>,
) -> Result<String> {
    run_collect(
        c,
        cfg,
        name,
        role,
        prompt,
        bundle,
        backend,
        guard,
        &mut Vec::new(),
        true,
    )
}

fn store_prompt(model: &Path, instructions: &str) -> Result<String> {
    use std::io::Write;
    // Never persist credential-shaped content, even if future prompt assembly
    // grows beyond today's static instructions and sealed citation identifiers.
    if super::transcript::redact(&json!(instructions)) != json!(instructions) {
        return Err("dashboard_prompt_secret_refused".into());
    }
    let sha = digest(instructions.as_bytes());
    let directory = model.join("prompts");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)?;
    let path = directory.join(format!("{sha}.md"));
    nivra_store::native_grant::file_path_ok(&path)?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(instructions.as_bytes())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let stored = crate::read_bounded(&path, instructions.len() as u64)
                .map_err(|_| "dashboard_prompt_invalid")?;
            if stored != instructions.as_bytes() {
                return Err("dashboard_prompt_invalid".into());
            }
        }
        Err(error) => return Err(error.into()),
    }
    Ok(format!("prompts/{sha}.md"))
}

// Display capture is best effort. A failure disables this writer once and never
// alters the authoritative archives, model result or trading validation.
fn capture_event(
    capture: &mut Option<super::transcript::Transcript<'_>>,
    kind: &str,
    fields: Value,
) {
    if capture
        .as_mut()
        .is_some_and(|writer| writer.event(kind, fields).is_err())
    {
        *capture = None;
        eprintln!("dashboard_transcript_unavailable");
    }
}
fn capture_usage(
    capture: &mut Option<super::transcript::Transcript<'_>>,
    turn: usize,
    started: i64,
    ended: i64,
    usage: &Value,
    totals: &mut [Option<u64>; 3],
) {
    let counts = [
        usage["input_tokens"].as_u64(),
        usage["output_tokens"].as_u64(),
        usage["input_tokens_details"]["cached_tokens"]
            .as_u64()
            .or(usage["cached_tokens"].as_u64()),
    ];
    for (total, count) in totals.iter_mut().zip(counts) {
        *total = total.zip(count).and_then(|(a, b)| a.checked_add(b));
    }
    capture_event(
        capture,
        "turn_usage",
        json!({"turn":turn,"started_ms":started,"ended_ms":ended,"tokens_in":counts[0],"tokens_out":counts[1],"tokens_cached":counts[2]}),
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) fn run_collect(
    c: &Config,
    cfg: &PlannerConfig,
    name: &str,
    role: &ModelRole,
    prompt: &str,
    bundle: &Value,
    backend: &mut dyn Backend,
    guard: &CallGuard<'_>,
    observations: &mut Vec<Value>,
    include_catalog: bool,
) -> Result<String> {
    instructions(name)?;
    let owner_review = owner_review_for_call(cfg, name, bundle, guard)?;
    let owner_research = owner_research_for_call(cfg, name, bundle, guard)?;
    let review_instructions = owner_review.then(|| {
        format!(
            "{}\n\n{}",
            objective_for(cfg),
            super::owner_tasks::REVIEW_PROMPT
        )
    });
    let context = cfg.decision_evidence.is_some() && name == "digest";
    let tool_limits = cfg.effective_tools();
    // The owner's standing prompt is never the compiled role default, so its
    // frames always carry their exact instructions.
    let custom_prompt =
        cfg.prompt_set().is_some_and(|prompts| prompts.custom()) || name == "supervisor";
    let tools = definitions_for_call(cfg, name, &role.tools, context, &tool_limits, bundle, guard)?;
    // Provider-native calls are admitted only when this frame advertises them.
    // A sealed review advertises none even when the role configures web tools.
    let advertised_native: Vec<String> = tools
        .iter()
        .filter_map(|tool| tool["type"].as_str())
        .filter(|kind| matches!(*kind, "web_search" | "x_search"))
        .map(str::to_owned)
        .collect();
    let catalog = (include_catalog && !owner_review && !owner_research)
        .then_some(cfg.research_tools.as_ref())
        .flatten()
        .map(|s| {
            s.registrations
                .iter()
                .map(|r| {
                    json!({"analysis_id":r.analysis_id,"dataset_id":r.dataset_id,"input_sha256":r.input_sha256,"split":"development","wall_millis":r.wall_millis})
                })
                .collect::<Vec<_>>()
        });
    let content = match catalog {
        Some(catalog) => {
            let encoded = json!(catalog).to_string();
            if encoded.len() <= 2048 {
                format!("{prompt}\nHost registered analysis catalog (aliases only): {encoded}")
            } else {
                format!(
                    "{prompt}\nHost registered analysis catalog omitted ({} aliases); request an alias directly.",
                    catalog.len()
                )
            }
        }
        None => prompt.to_owned(),
    };
    let mut input = vec![json!({"role":"user","content":content})];
    let mut seen = BTreeSet::new();
    let mut owner_task_keys = BTreeSet::new();
    let mut tool_citations = BTreeSet::new();
    let mut reads = 0;
    let mut experiments = 0;
    let mut failed_tools = 0usize;
    let mut web_budget = super::web_tools::WebToolBudget::default();
    let clock = DomainClock::new(guard);
    let now = || clock.wall_ms();
    let budget_start = clock.elapsed_ms();
    let start = now();
    let archive = c.directory.join("model").join(format!(
        "harness-{name}-{}-{}",
        millis(),
        &digest(prompt.as_bytes())[..12]
    ));
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&archive)?;
    let mut metadata = json!({"role":name,"model":role.model_id,"started_ms":millis(),"trigger":bundle["trigger"],"invocation_id":guard.invocation_id});
    if let Some(profile) = cfg.profile_sha256() {
        metadata["profile_sha256"] = json!(profile);
        metadata["prompt_sha256"] = cfg.prompt_hashes().unwrap_or(Value::Null);
    }
    rollover::atomic(&archive.join("run.json"), &metadata)?;
    let run_id = archive
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    let mut initial_frame = Frame {
        role: name.to_owned(),
        input: input.clone(),
        tools: tools.clone(),
        owner_tools: owner_tools_for_call(cfg, name, bundle, guard),
        citation_ids: Vec::new(),
        final_answer_only: owner_review,
        review_only: owner_review,
        decision_evidence: context,
        max_instrument_keys: (tool_limits.max_instrument_keys != MAX_INSTRUMENT_KEYS)
            .then_some(tool_limits.max_instrument_keys),
        instructions: review_instructions.clone(),
        tool_limits: (!tool_limits.is_default()).then(|| tool_limits.clone()),
    };
    let initial_instructions = frame_instructions_for(cfg, &initial_frame)?;
    if custom_prompt {
        initial_frame.instructions = Some(initial_instructions.clone());
    }
    let initial_prompt_sha = digest(initial_instructions.as_bytes());
    let mut final_instructions = None;
    let mut prompt_error_logged = false;
    let mut capture = match super::transcript::Transcript::start(
        &archive,
        run_id,
        &c.dashboard,
        json!({"role":name,"model":role.model_id,"trigger":bundle["trigger"],"invocation_id":guard.invocation_id,"session":c.session.to_string(),"mode":"paper","prompt_sha":initial_prompt_sha,"profile_sha256":cfg.profile_sha256()}),
        guard.clock,
    ) {
        Ok(writer) => Some(writer),
        Err(_) => {
            eprintln!("dashboard_transcript_unavailable");
            None
        }
    };
    let mut request_archive = super::transcript::RequestArchive::default();
    let mut prompt_shas = BTreeSet::new();
    let mut token_totals = [Some(0u64); 3];
    let mut transcript_tool_calls = 0usize;
    let mut transcript_tool_failures = 0usize;
    let mut transcript_instruments = BTreeSet::new();
    let mut stopped = false;
    let mut rejected_digest = false;
    // Bounded evidence for a failure that never reached the provider (#23).
    let mut pre_provider_evidence: Option<Value> = None;
    let mut stream_binding = None;
    let result = (|| -> Result<String> {
        let binding = book_binding(c)?;
        stream_binding = Some(binding.clone());
        let _stream = nivra_cli::model_stream::Scope::new(
            c.directory.join("model/live-stream.json"),
            binding,
            archive
                .file_name()
                .and_then(|v| v.to_str())
                .map(str::to_owned),
        );
        nivra_cli::model_stream::publish(
            name,
            &role.model_id,
            run_id,
            "run_started",
            "Harness started",
        );
        let budget_ms = role.run_budget_ms() as i64;
        for turn in 0..role.max_turns.unwrap_or(12) {
            // A passed event deadline or a changed config is never turned into a plan.
            guard.check_finished()?;
            // Duration admission is monotonic and independent of the observation
            // wall, which may be frozen or stepped backward by the host clock.
            let elapsed = clock.elapsed_ms().saturating_sub(budget_start);
            // Legacy callers without a monotonic duration still read the wall;
            // a backward step must refuse rather than extend the budget.
            if elapsed < 0 {
                return Err("planner_clock_anomaly".into());
            }
            if elapsed > budget_ms {
                stopped = true;
                return stopped_result(name);
            }
            let remaining = (budget_ms - elapsed).min(
                guard
                    .deadline
                    .map(|deadline| deadline.saturating_sub(clock.projected_wall_ms()))
                    .unwrap_or(i64::MAX),
            );
            if guard.check_cancel().is_err() {
                if name == "digest" {
                    stopped = true;
                    return stopped_result(name);
                }
                return Err("planner_cancelled_no_replay".into());
            }
            let mut frame = Frame {
                role: name.to_owned(),
                input: input.clone(),
                tools: tools.clone(),
                owner_tools: owner_tools_for_call(cfg, name, bundle, guard),
                citation_ids: Vec::new(),
                final_answer_only: owner_review,
                review_only: owner_review,
                decision_evidence: context,
                max_instrument_keys: (tool_limits.max_instrument_keys != MAX_INSTRUMENT_KEYS)
                    .then_some(tool_limits.max_instrument_keys),
                instructions: review_instructions.clone(),
                tool_limits: (!tool_limits.is_default()).then(|| tool_limits.clone()),
            };
            let instructions = if frame.final_answer_only {
                if final_instructions.is_none() {
                    let text = frame_instructions_for(cfg, &frame)?;
                    if custom_prompt {
                        frame.instructions = Some(text.clone());
                    }
                    final_instructions = Some(text);
                }
                final_instructions
                    .as_deref()
                    .expect("final instructions initialized")
            } else {
                if custom_prompt {
                    frame.instructions = Some(initial_instructions.clone());
                }
                initial_instructions.as_str()
            };
            let prompt_sha = digest(instructions.as_bytes());
            if prompt_shas.insert(prompt_sha.clone()) {
                let path = match store_prompt(&c.directory.join("model"), instructions) {
                    Ok(path) => Some(path),
                    Err(_) => {
                        if !prompt_error_logged {
                            eprintln!("dashboard_prompt_unavailable");
                            prompt_error_logged = true;
                        }
                        None
                    }
                };
                capture_event(
                    &mut capture,
                    "system_prompt",
                    json!({"sha":prompt_sha,"bytes":instructions.len(),"path":path}),
                );
            }
            let request = serde_json::to_string(&frame)?;
            let stream_id = digest(request.as_bytes());
            // Turn deltas are small and append-only: keep the historical pre-call
            // write so a crash during the call still leaves the appended input.
            if turn > 0 {
                request_archive.write(&archive, turn, &frame, &request, &prompt_sha)?;
            }
            let mut turn_role = role.clone();
            // Lifecycle turns have no tool round trips. Their single request
            // may consume the configured whole-run allowance (large summaries).
            let request_ms = if name == "supervisor"
                && matches!(
                    bundle["trigger"]["kind"].as_str(),
                    Some("compaction" | "handoff")
                ) {
                role.run_budget_ms()
            } else {
                role.timeout_ms
            };
            turn_role.timeout_ms = request_ms.min(remaining as u64);
            let call_started = now();
            // A research lane turn charges its own bucket, never the trading
            // `supervisor` allowance (#238).
            let usage_name = if owner_research { "research" } else { name };
            let called = planner::call_guarded_as(
                c, cfg, name, usage_name, &turn_role, &request, backend, guard,
            );
            let call_ended = now();
            let provider = backend.provenance();
            // The first request of a run is archived only after the provider
            // result is known. A failure before any provider turn persists a
            // bounded receipt and at most one representative request per failure
            // window instead of the full request on every retry (#23).
            if turn == 0 {
                let pre_provider = if called.is_err() {
                    failure_window::pre_provider_stage(&provider)
                } else {
                    None
                };
                let input_blob = match pre_provider {
                    Some(stage) => {
                        let reason = called
                            .as_ref()
                            .err()
                            .map(|error| tool_failure_reason(error.as_ref()))
                            .unwrap_or_else(|| "provider_failure_redacted".into());
                        let retained = failure_window::retain(
                            failure_window::PreProviderRequest {
                                model: &c.directory.join("model"),
                                archive: &archive,
                                run: run_id,
                                turn,
                                frame: &frame,
                                request: &request,
                                instructions_sha: &prompt_sha,
                                signature: failure_window::FailureSignature {
                                    kind: "harness_request",
                                    role: name,
                                    model: &role.model_id,
                                    reason: &reason,
                                    stage,
                                },
                                now: now(),
                            },
                            || request_archive.write(&archive, turn, &frame, &request, &prompt_sha),
                        );
                        pre_provider_evidence = Some(retained.evidence);
                        retained.blob
                    }
                    None => {
                        // A provider turn ends any open pre-provider failure
                        // window so the next outage keeps its own first evidence.
                        failure_window::close(
                            &c.directory.join("model"),
                            name,
                            &role.model_id,
                            now(),
                        );
                        request_archive.write(&archive, turn, &frame, &request, &prompt_sha)?;
                        Some("turn-00-request.json#/request/input".to_owned())
                    }
                };
                let content_bytes: usize = frame
                    .input
                    .iter()
                    .filter_map(|item| item["content"].as_str())
                    .map(str::len)
                    .sum();
                capture_event(
                    &mut capture,
                    "input",
                    json!({"turn":0,"bytes":serde_json::to_vec(&frame.input)?.len(),"summary":{"input_items":frame.input.len(),"content_bytes":content_bytes},"blob":input_blob}),
                );
            }
            let raw = match called {
                Ok(raw) => raw,
                Err(error) => {
                    // A failed response can still report its token counts; retain
                    // them instead of recording an unexplained all-null usage (#224).
                    capture_usage(
                        &mut capture,
                        turn,
                        call_started,
                        call_ended,
                        &json!({"input_tokens":provider["tokens_in"],"output_tokens":provider["tokens_out"]}),
                        &mut token_totals,
                    );
                    if turn > 0
                        && let Some(stage) = failure_window::pre_provider_stage(&provider)
                    {
                        let delta = format!("turn-{turn:02}-request-delta.json");
                        let reason = tool_failure_reason(error.as_ref());
                        pre_provider_evidence = failure_window::RequestEvidence {
                            request: &request,
                            frame: &frame,
                            instructions_sha: &prompt_sha,
                            reason: &reason,
                            stage,
                            request_archive: Some(&delta),
                            representative: None,
                            representative_run: None,
                            window_count: 0,
                        }
                        .value()
                        .ok();
                    }
                    if error.to_string() == "planner_cancelled_no_replay" && name == "digest" {
                        stopped = true;
                        return stopped_result(name);
                    }
                    return Err(error);
                }
            };
            // Archive before any post-call rejection so a finished body is kept on disk.
            let response: Value = nivra_core::native::decode(raw.as_bytes())
                .map_err(|_| "harness_response_invalid")?;
            rollover::atomic(
                &archive.join(format!("turn-{turn:02}-response.json")),
                &response,
            )?;
            for item in response["output"].as_array().into_iter().flatten() {
                match item["type"].as_str() {
                    Some("message") if item["role"] == "assistant" => {
                        for content in item["content"].as_array().into_iter().flatten() {
                            if let Some(text) = content["text"].as_str() {
                                capture_event(
                                    &mut capture,
                                    "assistant_text",
                                    json!({"turn":turn,"text":text}),
                                );
                            }
                        }
                    }
                    Some("function_call") => {
                        transcript_tool_calls += 1;
                        let args = item["arguments"]
                            .as_str()
                            .map(|v| serde_json::from_str::<Value>(v).unwrap_or_else(|_| json!(v)))
                            .unwrap_or(Value::Null);
                        let instruments: Vec<_> = args["instrument_keys"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .take(16)
                            .collect();
                        transcript_instruments.extend(instruments.iter().map(|v| (*v).to_owned()));
                        capture_event(
                            &mut capture,
                            "tool_call",
                            json!({"turn":turn,"call":item["call_id"],"name":item["name"],"args":args,"instruments":instruments}),
                        );
                    }
                    Some("web_search_call" | "x_search_call" | "custom_tool_call") => {
                        capture_event(
                            &mut capture,
                            "provider_tool",
                            json!({"turn":turn,"name":item["name"].as_str().or_else(||item["type"].as_str().and_then(|v|v.strip_suffix("_call"))),"action":item["action"]}),
                        )
                    }
                    _ => {}
                }
            }
            capture_usage(
                &mut capture,
                turn,
                call_started,
                call_ended,
                &response["usage"],
                &mut token_totals,
            );
            guard.check_finished()?;
            // Fixture backends may return a final structured result directly.
            let Some(output) = response.get("output").and_then(Value::as_array) else {
                if name == "digest" {
                    let (normalized, retained) =
                        normalize_digest_body(&archive, &raw, bundle, &tool_citations);
                    rejected_digest |= retained;
                    return normalized;
                }
                return Ok(raw);
            };
            checked_output(output, &advertised_native)?;
            let mut calls = vec![];
            for item in output {
                if item["type"] == "function_call" {
                    let (id, tool) = match checked_call_with(item, &tools, &tool_limits) {
                        Ok(call) => call,
                        Err(error) => {
                            transcript_tool_failures += 1;
                            capture_event(
                                &mut capture,
                                "tool_result",
                                json!({"turn":turn,"call":item["call_id"],"name":item["name"],"status":"refused","duration_ms":0,"bytes":0,"summary":{},"blob":null,"error":tool_failure_reason(error.as_ref())}),
                            );
                            return Err(error);
                        }
                    };
                    if !seen.insert(id.clone()) {
                        return Err("harness_duplicate_call_no_replay".into());
                    }
                    if let Tool::OwnerResearchEnqueue(args) = &tool
                        && !owner_task_keys.insert(args.task_key.clone())
                    {
                        return Err("harness_owner_research_duplicate_no_replay".into());
                    }
                    if let Tool::CandidateSubmit(args) = &tool
                        && !owner_task_keys.insert(args.task_key.clone())
                    {
                        return Err("harness_owner_research_duplicate_no_replay".into());
                    }
                    calls.push((id, tool));
                }
            }
            if calls.is_empty() {
                let mut text = final_text(output)?;
                if text.is_empty() {
                    return Err("harness_final_output_missing".into());
                }
                if name == "digest" {
                    let (normalized, retained) =
                        normalize_digest_body(&archive, &text, bundle, &tool_citations);
                    rejected_digest |= retained;
                    text = normalized?;
                    publish_validated(c, name, role, &text);
                }
                return Ok(text);
            }
            let after_elapsed = clock.elapsed_ms().saturating_sub(budget_start);
            if after_elapsed < 0 {
                return Err("planner_clock_anomaly".into());
            }
            let expired = after_elapsed > budget_ms;
            // Provider web search is not a local call. Local tools cannot continue
            // once the run budget has expired or the run was cancelled.
            let stop_tools = expired || guard.check_cancel().is_err();
            if stop_tools {
                if name == "digest" {
                    stopped = true;
                    return stopped_result(name);
                }
                return Err("harness_deadline_expired".into());
            }
            if calls.iter().any(|(_, tool)| match tool {
                Tool::Snapshot(keys) | Tool::Features(keys) | Tool::Context(keys) => {
                    keys.len() > tool_limits.max_instrument_keys
                }
                _ => false,
            }) {
                return Err("harness_snapshot_args_invalid".into());
            }
            reads += calls
                .iter()
                .filter(|(_, t)| {
                    !matches!(t, Tool::OwnerResearchEnqueue(_) | Tool::CandidateSubmit(_))
                        && !matches!(t, Tool::Workspace(op) if op.is_analysis())
                })
                .count();
            experiments += calls
                .iter()
                .filter(|(_, t)| {
                    matches!(t, Tool::OwnerResearchEnqueue(_) | Tool::CandidateSubmit(_))
                        || matches!(t, Tool::Workspace(op) if op.is_analysis())
                })
                .count();
            if reads > role.max_read_tools.unwrap_or(16) || experiments > role.max_analysis_tools {
                return Err("harness_tool_budget_exhausted".into());
            }
            input.extend(output.iter().cloned());
            for (id, tool) in calls {
                guard.check()?;
                // A durable claim precedes any tool effect. No automatic replay.
                let key = digest(id.as_bytes());
                let path = archive.join(format!("tool-{key}.claim"));
                let _claim = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(path)?;
                _claim.sync_all()?;
                fs::File::open(&archive)?.sync_all()?;
                let tool_name = match &tool {
                    Tool::Evidence(_) => "read_evidence",
                    Tool::Snapshot(_) => "get_market_snapshot",
                    Tool::Features(_) => "get_instrument_features",
                    Tool::Context(_) => "get_intraday_context",
                    Tool::OwnerResearchEnqueue(_) => "research_task_enqueue",
                    Tool::OwnerResearchStatus(_, _) => "research_task_status",
                    Tool::OwnerResearchResult(_, _) => "research_task_result",
                    Tool::OwnerResearchCancel(_, _) => "research_task_cancel",
                    Tool::CandidateSubmit(_) => "candidate_submit",
                    Tool::CandidateStatus(_, _) => "candidate_status",
                    Tool::CandidateResult(_, _, _) => "candidate_result",
                    Tool::CandidateCancel(_, _) => "candidate_cancel",
                    Tool::QuoteCapture(_, _, _) => "research_quote_capture",
                    Tool::ResearchRequest(_) => "request_research",
                    Tool::Scanner(_, _) => "get_scanner_prospects",
                    Tool::Ranking(_) => "rank_universe",
                    Tool::Portfolio => "get_portfolio",
                    Tool::History(_) => "get_recent_decisions",
                    Tool::Session => "get_session_status",
                    Tool::Workspace(op) => op.name(),
                    Tool::Web(op) => op.name(),
                };
                let tool_started = std::time::Instant::now();
                let result = match tool {
                    Tool::Evidence(id) => Ok(json!({"evidence_id":id,"data":evidence(bundle,&id)})),
                    Tool::Snapshot(keys) => match backend.snapshot(bundle, &keys, now()) {
                        Some(result) => result,
                        None => snapshot(c, bundle, &keys, &archive, &key),
                    },
                    Tool::Features(keys) => instrument_features(bundle, &keys),
                    Tool::Context(keys) => backend
                        .intraday_context(&keys, now())
                        .unwrap_or_else(|| Err("decision_evidence_unavailable".into())),
                    Tool::OwnerResearchEnqueue(args) => owner_research_task(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::Enqueue(args),
                        now(),
                    ),
                    Tool::OwnerResearchStatus(task_id, revision) => owner_research_task(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::Status(task_id, revision),
                        now(),
                    ),
                    Tool::OwnerResearchResult(task_id, revision) => owner_research_task(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::Result(task_id, revision),
                        now(),
                    ),
                    Tool::OwnerResearchCancel(task_id, revision) => owner_research_task(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::Cancel(task_id, revision),
                        now(),
                    ),
                    Tool::CandidateSubmit(args) => owner_candidate_operation(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::CandidateSubmit(args),
                        now(),
                    ),
                    Tool::CandidateStatus(task_id, revision) => owner_candidate_operation(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::CandidateStatus(task_id, revision),
                        now(),
                    ),
                    Tool::CandidateResult(task_id, revision, observation_ref) => {
                        owner_candidate_operation(
                            c,
                            cfg,
                            name,
                            role,
                            bundle,
                            guard,
                            OwnerResearchOperation::CandidateResult(
                                task_id,
                                revision,
                                observation_ref,
                            ),
                            now(),
                        )
                    }
                    Tool::CandidateCancel(task_id, revision) => owner_candidate_operation(
                        c,
                        cfg,
                        name,
                        role,
                        bundle,
                        guard,
                        OwnerResearchOperation::CandidateCancel(task_id, revision),
                        now(),
                    ),
                    Tool::QuoteCapture(keys, from_ms, to_ms) => {
                        if !owner_research_turn(cfg, name, bundle, guard) {
                            Err("harness_owner_research_turn_scope_invalid".into())
                        } else {
                            research_quote_capture(c, cfg, keys, from_ms, to_ms, now())
                        }
                    }
                    Tool::ResearchRequest(args) => {
                        match (
                            research_request_allowed(cfg, name, bundle, guard),
                            guard.invocation_id,
                        ) {
                            (true, Some(turn_id)) => super::lane_queue::post_request(
                                c,
                                cfg,
                                turn_id,
                                &args.question,
                                args.refs,
                                now(),
                            ),
                            _ => Err("harness_research_request_scope_invalid".into()),
                        }
                    }
                    Tool::Scanner(limit, sort) => scanner_prospects(bundle, limit, &sort),
                    Tool::Ranking(args) => rank_universe(bundle, &args),
                    Tool::Portfolio => Ok(portfolio_view(bundle, now(), tool_limits.fill_limit)),
                    Tool::History(limit) => Ok(recent_decisions(bundle, limit)),
                    Tool::Session => Ok(session_status(c, bundle, name, role, now())),
                    Tool::Workspace(op) => {
                        if !owner_workspace_tools_allowed(cfg, name, bundle, guard) {
                            Err("harness_workspace_scope_invalid".into())
                        } else {
                            let scope = if owner_research_turn(cfg, name, bundle, guard) {
                                super::workspace::WorkspaceScope::Research
                            } else {
                                super::workspace::WorkspaceScope::Decision
                            };
                            super::workspace::execute_scoped(
                                &c.directory,
                                cfg.workspace.as_ref().ok_or("harness_workspace_disabled")?,
                                scope,
                                op,
                            )
                        }
                    }
                    Tool::Web(op) => {
                        if !owner_web_tools_allowed(cfg, name, bundle, guard) {
                            Err("harness_web_tools_scope_invalid".into())
                        } else if let Some(settings) =
                            cfg.web_tools.as_ref().filter(|web| web.enabled)
                        {
                            super::web_tools::execute(
                                c,
                                settings,
                                &mut web_budget,
                                &super::web_tools::CallContext {
                                    session: c.session.to_string(),
                                    run: run_id.to_string(),
                                    turn,
                                    call: id.clone(),
                                },
                                op,
                                now(),
                            )
                        } else {
                            Err("harness_web_tools_disabled".into())
                        }
                    }
                };
                let duration_ms = tool_started.elapsed().as_millis();
                let mut failed = result.is_err();
                let mut value = match result {
                    Ok(v) => v,
                    Err(e) => {
                        json!({"evidence_id":id,"status":"unavailable_no_retry","reason":tool_failure_reason(e.as_ref()),"measured":false})
                    }
                };
                let mut encoded = serde_json::to_string(&value)?;
                if encoded.len() > TOOL_OUTPUT_MAX_BYTES {
                    // Return an explicit unavailable result, never a partial
                    // market/account value or a replay of an effectful call.
                    value = json!({"status":"unavailable_no_retry","reason":"harness_tool_output_cap",
                        "original_result_bytes":encoded.len(),"original_result_sha256":digest(encoded.as_bytes()),
                        "max_result_bytes":TOOL_OUTPUT_MAX_BYTES,"execution_authorized":false,
                        "read_guidance":"Narrow a new read request; do not repeat effectful calls."});
                    encoded = serde_json::to_string(&value)?;
                    failed = true;
                }
                transcript_tool_failures += usize::from(failed);
                let keys: Vec<_> = value
                    .as_object()
                    .into_iter()
                    .flat_map(|v| v.keys())
                    .cloned()
                    .collect();
                capture_event(
                    &mut capture,
                    "tool_result",
                    json!({"turn":turn,"call":id,"name":tool_name,"status":if failed {"failed"} else {"ok"},"duration_ms":duration_ms,"bytes":encoded.len(),"summary":{"keys":keys},"blob":format!("tool-{key}.result.json"),"error":if failed {value["reason"].clone()} else {Value::Null}}),
                );
                nivra_cli::model_stream::publish_tool(
                    name,
                    &role.model_id,
                    &stream_id,
                    if failed {
                        "tool_failed"
                    } else {
                        "tool_completed"
                    },
                    tool_name,
                    &if failed {
                        format!(
                            "{tool_name} failed: {}",
                            value["reason"].as_str().unwrap_or("tool_unavailable")
                        )
                    } else {
                        format!(
                            "{tool_name} completed in {} ms",
                            tool_started.elapsed().as_millis()
                        )
                    },
                );
                if encoded.len() > TOOL_OUTPUT_MAX_BYTES {
                    return Err("harness_tool_output_cap".into());
                }
                rollover::atomic(&archive.join(format!("tool-{key}.result.json")), &value)?;
                if failed {
                    failed_tools += 1;
                }
                if !failed && name == "digest" {
                    observations.push(json!({"tool":tool_name,"result":value.clone()}));
                }
                tool_citations.insert(id.clone());
                if let Some(evidence_id) = value["evidence_id"].as_str() {
                    tool_citations.insert(evidence_id.to_owned());
                }
                input.push(json!({"type":"function_call_output","call_id":id,"output":encoded}));
            }
        }
        match name {
            "digest" => {
                stopped = true;
                stopped_result(name)
            }
            _ => Err("harness_turn_budget_exhausted".into()),
        }
    })();
    let mut provider_evidence = Value::Null;
    let result = (|| -> Result<String> {
        metadata["completed_ms"] = json!(millis());
        match &result {
            Ok(raw) => {
                let output: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
                rollover::atomic(&archive.join("output.json"), &json!({"output":output}))?;
            }
            Err(error) => {
                let status = planner::reason(error.as_ref());
                let mut failure =
                    json!({"status":status,"reason":tool_failure_reason(error.as_ref())});
                let evidence =
                    planner::failure_evidence(status, &error.to_string(), &backend.provenance());
                if let Value::Object(fields) = &evidence {
                    failure
                        .as_object_mut()
                        .expect("failure record is an object")
                        .extend(fields.clone());
                }
                if let Some(Value::Object(fields)) = &pre_provider_evidence {
                    failure
                        .as_object_mut()
                        .expect("failure record is an object")
                        .extend(fields.clone());
                    metadata["pre_provider"] = json!(true);
                    metadata["request_receipt"] = failure["request_receipt"].clone();
                    metadata["representative_request"] = failure["representative_request"].clone();
                }
                if rejected_digest {
                    failure["rejected_digest"] = json!("rejected-digest.json");
                }
                provider_evidence = evidence;
                rollover::atomic(&archive.join("failure.json"), &failure)?;
            }
        }
        rollover::atomic(&archive.join("run.json"), &metadata)?;
        result
    })();
    let ended_elapsed = clock.elapsed_ms().saturating_sub(budget_start);
    let (outcome, reason) = match &result {
        Ok(raw) => {
            let output: Value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
            if serde_json::to_vec(&output).is_ok_and(|bytes| bytes.len() <= 65_536) {
                capture_event(&mut capture, "final_output", json!({"output":output}));
            } else {
                capture_event(&mut capture, "final_output", json!({"blob":"output.json"}));
            }
            transcript_instruments.extend(
                output["actions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["instrument_key"].as_str().map(str::to_owned)),
            );
            let outcome = if stopped {
                "stopped"
            } else if output["stance"] == "wait" || output["handoff"]["decision"] == "wait" {
                "wait"
            } else if output["actions"].as_array().is_some_and(Vec::is_empty) {
                "abstain"
            } else {
                "accepted"
            };
            capture_event(
                &mut capture,
                "host_note",
                json!({"text":if name == "digest" {"Digest schema validated by the host"} else {"Harness output archived; downstream host validation and execution remain separate"}}),
            );
            (outcome, Value::Null)
        }
        Err(error) => ("failure", json!(tool_failure_reason(error.as_ref()))),
    };
    let cost = super::transcript::estimated_cost(&c.dashboard, &role.model_id, token_totals);
    let cost_basis = cost.as_ref().map(|_| "configured_estimate");
    if let Some(writer) = capture
        && writer.end(json!({"outcome":outcome,"reason":reason,"tokens_in":token_totals[0],"tokens_out":token_totals[1],"tokens_cached":token_totals[2],"cost_estimate_inr":cost,"cost_basis":cost_basis,"duration_ms":ended_elapsed,"tool_calls":transcript_tool_calls,"tool_failures":transcript_tool_failures,"provider_evidence":provider_evidence})).is_err() { eprintln!("dashboard_transcript_unavailable"); }
    let indexed = (|| -> Result<()> {
        let line = super::transcript::RunIndexLine {
            v: 1,
            run: run_id.into(),
            role: name.into(),
            model: role.model_id.clone(),
            trigger_kind: bundle["trigger"]["kind"].as_str().map(str::to_owned),
            started_ms: start,
            duration_ms: ended_elapsed,
            outcome: outcome.into(),
            tokens_in: token_totals[0],
            tokens_out: token_totals[1],
            cost_estimate_inr: cost,
            tool_calls: transcript_tool_calls,
            tool_failures: transcript_tool_failures,
            instruments: transcript_instruments.into_iter().take(16).collect(),
            bytes: super::transcript::directory_bytes(&archive)?,
            mode: "paper".into(),
            profile_sha256: cfg.profile_sha256().map(str::to_owned),
        };
        super::transcript::append_index_line(&c.directory.join("model"), &line)
    })();
    if indexed.is_err() {
        eprintln!("dashboard_run_index_unavailable");
    }
    if let Some(binding) = stream_binding {
        let _scope = nivra_cli::model_stream::Scope::new(
            c.directory.join("model/live-stream.json"),
            binding,
            Some(run_id.to_owned()),
        );
        nivra_cli::model_stream::publish(name, &role.model_id, run_id, "run_ended", outcome);
    }
    result
}

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
pub(super) use boundary_tests::check_boundaries;

#[cfg(test)]
mod digest_repair_tests;

#[cfg(test)]
mod snapshot_args_tests;

#[cfg(test)]
mod transcript_capture_tests;

#[cfg(test)]
mod prompt_store_tests;

#[cfg(test)]
mod call_time_tool_tests;
#[cfg(test)]
mod lane_tool_tests;
