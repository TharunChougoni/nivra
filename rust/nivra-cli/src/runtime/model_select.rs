//! Operator model switch for one planner role.
//!
//! The switch only chooses between bindings the reviewed planner policy already
//! declares (`model_profiles` admitted by `admitted_models`); it never invents a
//! model, credential or budget. A dry run prints the planner diff, the grant and
//! catalog preflight and writes no config, planner, book or audit state. A real
//! switch writes a *new* planner config beside the live one, points the paper
//! config at it and adopts it through the same reviewed apply path as
//! `config-apply --confirm-paper-review`, so the book, usage and journals are
//! only ever touched by that code. Sealed state is never written here.
use super::orchestrator_model::{ModelProfile, Trigger};
use super::planner::PlannerConfig;
use super::{Config, digest, model_selection, planner, rollover, wide};
use nivra_cli::native_transport::Transport;
use serde_json::{Value, json};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

type SelectResult<T> = std::result::Result<T, &'static str>;

/// Roles this command may switch. Each maps to a distinct policy field group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Role {
    /// Persistent orchestrator model (`orchestrator.model`).
    Orchestrator,
    /// Factual digest compiler (`digest`).
    Digest,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::Orchestrator => "orchestrator",
            Self::Digest => "digest",
        }
    }
    /// The role's configured model identity as the policy declares it. The
    /// shared top-level `model_config` remains the fallback the runtime uses.
    fn binding(self, cfg: &PlannerConfig) -> Option<RoleBinding<'_>> {
        match self {
            Self::Orchestrator => {
                let model = &cfg.orchestrator.as_ref()?.model;
                Some(RoleBinding {
                    model_id: &model.model_id,
                    transport: model.transport,
                    model_config: model
                        .model_config
                        .as_deref()
                        .or(cfg.model_config.as_deref()),
                })
            }
            Self::Digest => Some(RoleBinding {
                model_id: &cfg.digest.model_id,
                transport: cfg.digest.transport,
                model_config: cfg
                    .digest
                    .model_config
                    .as_deref()
                    .or(cfg.model_config.as_deref()),
            }),
        }
    }
}

/// A role's configured model identity, read from either policy shape.
struct RoleBinding<'a> {
    model_id: &'a str,
    transport: Transport,
    model_config: Option<&'a Path>,
}

/// One resolved model identity: the exact binding plus the profile name it came
/// from (`default` for the role's configured model).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    profile: String,
    model_id: String,
    transport: Transport,
    model_config: PathBuf,
}

impl Target {
    fn from_profile(name: &str, profile: &ModelProfile) -> Self {
        Self {
            profile: name.to_owned(),
            model_id: profile.model_id.clone(),
            transport: profile.transport,
            model_config: profile.model_config.clone(),
        }
    }
    fn credential_sha256(&self) -> String {
        digest(self.model_config.as_os_str().as_encoded_bytes())
    }
}

/// The role's own configured binding, reported as `default`.
fn configured(cfg: &PlannerConfig, role: Role) -> Option<Target> {
    let binding = role.binding(cfg)?;
    Some(Target {
        profile: model_selection::DEFAULT_PROFILE.to_owned(),
        model_id: binding.model_id.to_owned(),
        transport: binding.transport,
        model_config: binding.model_config?.to_owned(),
    })
}

/// Named profiles the policy declares and admits. `default` is not a profile
/// name; it always resolves to the role's configured model.
fn admitted(cfg: &PlannerConfig) -> SelectResult<Vec<Target>> {
    let Some(orchestrator) = cfg.orchestrator.as_ref() else {
        return Ok(Vec::new());
    };
    orchestrator
        .admitted_models
        .iter()
        .filter(|name| name.as_str() != model_selection::DEFAULT_PROFILE)
        .map(|name| {
            orchestrator
                .model_profiles
                .get(name)
                .map(|profile| Target::from_profile(name, profile))
                .ok_or("model_select_profile_unknown")
        })
        .collect()
}

/// Resolve `--profile` or `--model-id/--transport` against the policy. A named
/// profile must be admitted; an explicit model id must be declared by the policy
/// as either a profile or the role's configured model, so the command can never
/// introduce a model the reviewed policy does not carry.
fn target(
    cfg: &PlannerConfig,
    role: Role,
    profile: Option<&str>,
    model_id: Option<&str>,
    transport: Option<Transport>,
) -> SelectResult<Target> {
    let current = configured(cfg, role);
    let selected = match (profile, model_id) {
        (Some(name), None) => {
            if name == model_selection::DEFAULT_PROFILE {
                current.ok_or("model_select_default_unavailable")?
            } else {
                admitted(cfg)?
                    .into_iter()
                    .find(|target| target.profile == name)
                    .ok_or("model_select_profile_unknown")?
            }
        }
        (None, Some(model_id)) => {
            let transport = transport.ok_or("model_select_transport_required")?;
            admitted(cfg)?
                .into_iter()
                .chain(current)
                .find(|target| target.model_id == model_id && target.transport == transport)
                .ok_or("model_select_model_not_declared")?
        }
        (Some(_), Some(_)) => return Err("model_select_ambiguous_target"),
        (None, None) => return Err("model_select_profile_required"),
    };
    // A named profile must be usable by the role that will call it. The
    // configured default is the existing reviewed state and is not re-judged.
    if selected.profile != model_selection::DEFAULT_PROFILE {
        let tools = match role {
            Role::Orchestrator => cfg.role(Trigger::Supervisor).tools,
            Role::Digest => cfg.digest.tools.clone(),
        };
        super::model_admission::transport_tools(selected.transport, &tools)
            .map_err(profile_reason)?;
    }
    Ok(selected)
}

/// Map the shared admission refusals onto this command's codes.
fn profile_reason(reason: &'static str) -> &'static str {
    match reason {
        "model_selection_profile_tools_incompatible" => "model_select_transport_incompatible",
        _ => "model_select_profile_invalid",
    }
}

/// Write the switch into a copy of the policy. Only model identity moves; tools,
/// budgets, timeouts, sandbox and prompts stay with the reviewed policy.
fn switched(cfg: &PlannerConfig, role: Role, target: &Target) -> SelectResult<PlannerConfig> {
    let mut next = cfg.clone();
    match role {
        Role::Orchestrator => {
            let model = &mut next
                .orchestrator
                .as_mut()
                .ok_or("model_select_orchestrator_unavailable")?
                .model;
            model.model_id = target.model_id.clone();
            model.transport = target.transport;
            model.model_config = Some(target.model_config.clone());
        }
        Role::Digest => {
            next.digest.model_id = target.model_id.clone();
            next.digest.transport = target.transport;
            next.digest.model_config = Some(target.model_config.clone());
        }
    }
    Ok(next)
}

/// A switch is only expressible when the policy's shared top-level `model_config`
/// cannot shadow the role's own credential. A role that falls back to that
/// shared path cannot be repointed without silently changing another role.
fn switchable(cfg: &PlannerConfig, target: &Target) -> SelectResult<()> {
    if cfg
        .model_config
        .as_deref()
        .is_some_and(|fallback| fallback != target.model_config.as_path())
    {
        return Err("model_select_shared_credential");
    }
    Ok(())
}

/// The live file must be the policy in force for this session. A future-dated
/// schedule would otherwise write a switch that only takes effect later.
fn effective(cfg: &PlannerConfig, c: &Config) -> SelectResult<()> {
    if cfg.effective_from.is_some_and(|date| c.session < date) {
        return Err("model_select_policy_not_effective");
    }
    Ok(())
}

/// Refuse a no-op switch: it would stop the book for a policy that did not move.
fn changed(cfg: &PlannerConfig, role: Role, target: &Target) -> SelectResult<()> {
    let same = configured(cfg, role).is_some_and(|current| {
        current.model_id == target.model_id
            && current.transport == target.transport
            && current.model_config == target.model_config
    });
    if same {
        return Err("model_select_unchanged");
    }
    Ok(())
}

/// The role's current binding as the policy declares it.
fn current(cfg: &PlannerConfig, role: Role) -> Value {
    match configured(cfg, role) {
        Some(current) => json!({
            "model_id": current.model_id,
            "transport": current.transport,
            "credential_sha256": current.credential_sha256(),
        }),
        None => {
            json!({"model_id": "", "transport": Transport::default(), "credential_sha256": Value::Null})
        }
    }
}

/// Grant presence and status for the target binding. Read-only: it never
/// refreshes, never contacts a provider and never prints grant contents.
fn grant(target: &Target) -> Value {
    let now = super::local(super::millis());
    let deadline = now + chrono::Duration::hours(24);
    match crate::model::readiness(
        &target.model_config,
        target.transport,
        &target.model_id,
        now,
        deadline,
    ) {
        Ok(details) => json!({"status": details["status"], "reason": details["reason"],
            "refresh_owner": details["refresh_owner"]}),
        Err(code) => json!({"status": "blocked", "reason": code.as_str()}),
    }
}

/// Refuse a target whose transport has no authenticated grant. A missing or
/// invalid target credential config is a different, equally explicit refusal.
fn authenticated(grant: &Value) -> SelectResult<()> {
    if grant["status"] != "blocked" {
        return Ok(());
    }
    Err(match grant["reason"].as_str().unwrap_or_default() {
        "grant_unavailable"
        | "grant_invalid"
        | "grant_login_required"
        | "grant_expired_during_refresh" => "model_select_grant_required",
        _ => "model_select_model_config_invalid",
    })
}

/// Live catalog membership for the target model. The check is read-only and
/// advisory: it reads the target's own application grant without refreshing it
/// and reports an unavailable precondition instead of touching credentials.
fn catalog(target: &Target) -> Value {
    crate::model::catalog_membership(&target.model_config, target.transport, &target.model_id)
}

/// Structural planner diff: the policy view with only the role's model identity
/// replaced, so default-valued fields never show up as changes.
fn diff(cfg: &PlannerConfig, role: Role, target: &Target) -> Value {
    let before = planner::policy_bytes(cfg).ok().and_then(view);
    let after = switched(cfg, role, target)
        .ok()
        .and_then(|next| planner::policy_bytes(&next).ok())
        .and_then(view);
    let (Some(mut before), Some(mut after)) = (before, after) else {
        return json!({"status": "unavailable", "reason": "model_select_policy_invalid"});
    };
    let at = match role {
        Role::Orchestrator => "orchestrator/model",
        Role::Digest => "digest",
    };
    let path: Vec<&str> = at.split('/').collect();
    json!({
        "status": "changed",
        "path": at,
        "before": take(&mut before, &path),
        "after": take(&mut after, &path),
    })
}

fn view(bytes: Vec<u8>) -> Option<Value> {
    serde_json::from_slice(&bytes).ok()
}

fn take(value: &mut Value, path: &[&str]) -> Value {
    let mut node = value;
    for key in path {
        let Some(next) = node.get_mut(*key) else {
            return Value::Null;
        };
        node = next;
    }
    node.clone()
}

/// The next session the switch takes effect in, or the refusal when a paper
/// session is open. Mirrors the reviewed apply path: the root controller lock and
/// the session's own `controller.lock` both have to be free. The locks are
/// released before returning, so a dry run holds nothing.
fn session(c: &Config) -> SelectResult<Value> {
    let active = rollover::current(c).map_err(|_| "model_select_session_unavailable")?;
    // The root lock is the controller's own startup fence: holding it proves no
    // controller is starting or running. The session lock file is only opened
    // when it already exists, so a dry run never creates book-directory state.
    let root = rollover::controller_lock(c).map_err(|_| "model_select_session_open")?;
    let lock_path = active.directory.join("controller.lock");
    let busy = if lock_path.exists() {
        let lock = std::fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .map_err(|_| "model_select_session_unavailable")?;
        let busy = fs2::FileExt::try_lock_exclusive(&lock).is_err();
        drop(lock);
        busy
    } else {
        false
    };
    drop(root);
    if busy {
        return Err("model_select_session_open");
    }
    Ok(json!({"session": active.session.to_string(), "open": false}))
}

/// Dry run: report the diff, grant and catalog preflight and change nothing.
fn preview(
    c: &Config,
    cfg: &PlannerConfig,
    role: Role,
    target: &Target,
    live: &Path,
    path: &Path,
) -> Value {
    let session = session(c);
    let grant = grant(target);
    let refusal = session
        .as_ref()
        .err()
        .copied()
        .or_else(|| authenticated(&grant).err())
        .or_else(|| switchable(cfg, target).err())
        .or_else(|| effective(cfg, c).err())
        .or_else(|| changed(cfg, role, target).err())
        .or_else(|| model_selection::same_path(live, path).then_some("model_select_same_path"));
    let policy_sha256 = switched(cfg, role, target)
        .ok()
        .and_then(|next| planner::policy_bytes(&next).ok())
        .map(|bytes| digest(&bytes));
    json!({
        "status": "dry_run",
        "role": role.as_str(),
        "profile": target.profile,
        "model_id": target.model_id,
        "transport": target.transport,
        "credential_sha256": target.credential_sha256(),
        "current": current(cfg, role),
        "policy_sha256": policy_sha256,
        "diff": diff(cfg, role, target),
        "grant": grant,
        "catalog": catalog(target),
        "session": session.unwrap_or_else(|code| json!({"status": "blocked", "reason": code})),
        "refusal": refusal,
        "writes": false,
        "applies": "next_session",
    })
}

/// Write the switched policy to a new file beside the live planner config. The
/// directory is unchanged so relative prompts resolve exactly as before, and the
/// live file is never rewritten.
fn write(
    cfg: &PlannerConfig,
    live: &Path,
    session: chrono::NaiveDate,
    role: Role,
    target: &Target,
) -> SelectResult<(PathBuf, String)> {
    let next = switched(cfg, role, target)?;
    let parent = live.parent().ok_or("model_select_unwritable")?;
    let stem = live
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("model_select_unwritable")?;
    let policy_sha256 = planner::policy_bytes(&next)
        .map(|bytes| digest(&bytes))
        .map_err(|_| "model_select_policy_invalid")?;
    let suffix = format!(
        "{}-{}-{}",
        role.as_str(),
        target.profile,
        &policy_sha256[..8]
    );
    let mut text = serde_json::to_string_pretty(&next)
        .map_err(|_| "model_select_unwritable")?
        .into_bytes();
    text.push(b'\n');
    for attempt in 0..4 {
        let out = parent.join(format!(
            "{stem}.{suffix}{}.json",
            if attempt == 0 {
                String::new()
            } else {
                format!("-{attempt}")
            }
        ));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&out)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("model_select_unwritable"),
        };
        if file.write_all(&text).is_err() || file.sync_all().is_err() {
            let _ = std::fs::remove_file(&out);
            return Err("model_select_unwritable");
        }
        drop(file);
        // Prove the written file resolves to the target for this session under
        // the same reviewed prerequisites, and that only the policy hash moved.
        let loaded = match PlannerConfig::load_for_session(&out, session) {
            Ok(loaded) => loaded,
            Err(_) => {
                let _ = std::fs::remove_file(&out);
                return Err("model_select_invalid");
            }
        };
        let actual = role.binding(&loaded);
        if actual.is_none_or(|actual| {
            actual.model_id != target.model_id
                || actual.transport != target.transport
                || actual.model_config != Some(target.model_config.as_path())
        }) || planner::policy_bytes(&loaded)
            .map(|bytes| digest(&bytes))
            .ok()
            .as_deref()
            != Some(policy_sha256.as_str())
        {
            let _ = std::fs::remove_file(&out);
            return Err("model_select_invalid");
        }
        return Ok((out, policy_sha256));
    }
    Err("model_select_out_exists")
}

/// Point the paper config at the new planner file. The operator config is
/// rewritten atomically; only `planner_config` changes, and every other field
/// (including operator-only fields the runtime never re-serializes) is kept.
fn repoint(path: &Path, live: &Path, out: &Path) -> SelectResult<()> {
    let bytes = crate::read_bounded(path, 16_384).map_err(|_| "model_select_config_unreadable")?;
    let mut value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "model_select_config_invalid")?;
    if value
        .get("planner_config")
        .and_then(Value::as_str)
        .map(Path::new)
        != Some(live)
    {
        return Err("model_select_config_changed");
    }
    value["planner_config"] = json!(out);
    let c: Config =
        serde_json::from_value(value.clone()).map_err(|_| "model_select_config_invalid")?;
    if c.planner_config.as_deref() != Some(out) {
        return Err("model_select_config_invalid");
    }
    rollover::atomic(path, &value).map_err(|_| "model_select_config_unwritable")
}

/// Audit context for the reviewed config transition. The book's own
/// `operator_config_apply_confirmed_paper_review` tick remains the durable
/// financial-state record; this names the role, profile and exact binding.
fn audit_context(role: Role, target: &Target, planner_config: &Path) -> Value {
    json!({
        "role": role.as_str(),
        "profile": target.profile,
        "model_id": target.model_id,
        "transport": target.transport,
        "credential_sha256": target.credential_sha256(),
        "planner_config": planner_config,
        "applies": "next_session",
        "live_enabled": false,
    })
}

/// One operator request: the paper config, the role, exactly one target form and
/// the two explicit operator switches.
pub(crate) struct Request<'a> {
    pub path: &'a Path,
    pub role: Role,
    pub profile: Option<&'a str>,
    pub model_id: Option<&'a str>,
    pub transport: Option<Transport>,
    pub dry_run: bool,
    pub confirm: bool,
    pub json: bool,
}

fn select_entry(request: &Request<'_>) -> SelectResult<Value> {
    let Request {
        path,
        role,
        profile,
        model_id,
        transport,
        dry_run,
        confirm,
        ..
    } = *request;
    let (c, _) = model_selection::load_current(path)?;
    let live = c
        .planner_config
        .as_deref()
        .ok_or("model_selection_planner_missing")?;
    // Read the live file itself, not the resolved session view: the switch must
    // preserve the operator's schedule and prompt bindings exactly.
    let cfg = PlannerConfig::load(live).map_err(|_| "model_select_policy_invalid")?;
    let target = target(&cfg, role, profile, model_id, transport)?;
    if dry_run {
        return Ok(preview(&c, &cfg, role, &target, live, path));
    }
    if !confirm {
        return Err("model_select_confirm_paper_review_required");
    }
    // Refuse before writing anything: an open session never takes a switch.
    session(&c)?;
    let grant = grant(&target);
    authenticated(&grant)?;
    switchable(&cfg, &target)?;
    effective(&cfg, &c)?;
    changed(&cfg, role, &target)?;
    if model_selection::same_path(live, path) {
        return Err("model_select_same_path");
    }
    let (out, policy_sha256) = write(&cfg, live, c.session, role, &target)?;
    let mut applied = c.clone();
    applied.planner_config = Some(out.clone());
    // Same reviewed apply path as `config-apply --confirm-paper-review`, with
    // the switch recorded on the audited config transition.
    if let Err(error) = wide::config_apply_with(
        &applied,
        true,
        Some(audit_context(role, &target, &out)),
        false,
    ) {
        let _ = std::fs::remove_file(&out);
        eprintln!(
            "model_select_apply_refused: {}",
            super::failure_code(error.as_ref())
        );
        return Err("model_select_apply_refused");
    }
    // The book is now stopped under the new policy; the config must name it
    // before the next controller start. A failure here is recoverable: the
    // printed path is the reviewed planner file the operator points at.
    if let Err(code) = repoint(path, live, &out) {
        eprintln!(
            "model_select_config_unwritten: point planner_config at {} and run resume",
            out.display()
        );
        return Err(code);
    }
    Ok(json!({
        "status": "applied",
        "role": role.as_str(),
        "profile": target.profile,
        "model_id": target.model_id,
        "transport": target.transport,
        "session": c.session.to_string(),
        "planner_config": out,
        "policy_sha256": policy_sha256,
        "applies": "next_session",
        "resume_required": true,
    }))
}

pub(crate) fn entry(request: &Request<'_>) -> ExitCode {
    match select_entry(request) {
        Ok(value) => {
            if request.json {
                println!("{}", serde_json::to_string(&value).unwrap_or_default());
            } else if request.dry_run {
                println!(
                    "status=dry_run role={} profile={} model={} transport={}",
                    value["role"].as_str().unwrap_or_default(),
                    value["profile"].as_str().unwrap_or_default(),
                    value["model_id"].as_str().unwrap_or_default(),
                    value["transport"].as_str().unwrap_or_default(),
                );
                println!("diff={}", value["diff"]);
                println!("grant={}", value["grant"]);
                println!("catalog={}", value["catalog"]);
                println!("session={}", value["session"]);
                if let Some(refusal) = value["refusal"].as_str() {
                    println!("refusal={refusal}");
                }
                println!("writes=false applies=next_session");
            } else {
                println!(
                    "status={} role={} model={} transport={} applies={} resume_required=true",
                    value["status"].as_str().unwrap_or_default(),
                    value["role"].as_str().unwrap_or_default(),
                    value["model_id"].as_str().unwrap_or_default(),
                    value["transport"].as_str().unwrap_or_default(),
                    value["applies"].as_str().unwrap_or_default(),
                );
            }
            ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("model_select_refused: {code}");
            match code {
                "model_select_grant_required" => {
                    eprintln!("model_select_hint: run `nivra model login` for the target transport")
                }
                "model_select_confirm_paper_review_required" => eprintln!(
                    "model_select_hint: review the paper book, then pass --confirm-paper-review"
                ),
                _ => {}
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
pub(crate) fn exercise() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let c = super::tests::config(root);
    let model = |name: &str, transport: &str| -> PathBuf {
        let path = root.join(name);
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "transport": transport,
                "grant_file": root.join(format!("missing-{name}-grant")),
            }))
            .unwrap(),
        )
        .unwrap();
        path
    };
    let base = model("base-model.json", "openai_codex");
    let digest_model = model("digest-model.json", "openai_codex");
    let grok = model("grok-model.json", "direct_oauth");
    let sol = model("sol-model.json", "openai_codex");
    let live = root.join("planner.json");
    let cfg: PlannerConfig = serde_json::from_value(json!({
        "enabled": true,
        "provider": "xai_subscription",
        "shortlist_n": 40,
        "digest": {"enabled": true, "model_id": "gpt-6-luna", "model_config": digest_model, "transport": "openai_codex", "daily_call_budget": 10, "timeout_ms": 20000},
        "events": {"enabled": true, "cooldown_ms": 0, "max_pending": 10, "policy": {"price_move_bps":100,"max_event_age_ms":300000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}},
        "orchestrator": {
            "enabled": true,
            "daily_call_budget": 40,
            "model": {"model_config": base, "model_id": "gpt-6-luna", "transport": "openai_codex", "daily_call_budget": 20},
            "triggers": {"supervisor": {"prompt": root.join("supervisor.md").to_string_lossy(), "daily_call_budget": 40}},
            "model_profiles": {
                "grok-owner": {"model_id": "grok-4.7", "transport": "direct_oauth", "model_config": grok},
                "sol-owner": {"model_id": "gpt-6.1-sol", "transport": "openai_codex", "model_config": sol}
            },
            "admitted_models": ["default", "grok-owner", "sol-owner"]
        }
    }))
    .unwrap();
    std::fs::write(root.join("supervisor.md"), b"fixture").unwrap();
    std::fs::write(&live, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let mut paper = c.clone();
    paper.planner_config = Some(live.clone());
    let paper_path = root.join("paper.json");
    let mut value = serde_json::to_value(&paper).unwrap();
    value["planner_config"] = json!(live);
    value["sessions_root"] = json!(root.join("sessions"));
    value["history_directory"] = json!(root.join("history"));
    std::fs::write(&paper_path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();

    // Unknown or unadmitted names, an undeclared model id and ambiguous target
    // forms are refused before anything is written. `default` is admitted but
    // never treated as a profile.
    assert_eq!(
        target(&cfg, Role::Orchestrator, Some("missing"), None, None).unwrap_err(),
        "model_select_profile_unknown"
    );
    assert_eq!(
        admitted(&cfg).unwrap().len(),
        2,
        "admitted `default` is not a profile"
    );
    assert_eq!(
        target(
            &cfg,
            Role::Orchestrator,
            None,
            Some("grok-5"),
            Some(Transport::DirectOauth)
        )
        .unwrap_err(),
        "model_select_model_not_declared"
    );
    assert_eq!(
        target(&cfg, Role::Orchestrator, None, None, None).unwrap_err(),
        "model_select_profile_required"
    );
    assert_eq!(
        target(&cfg, Role::Orchestrator, None, Some("gpt-6.1-sol"), None).unwrap_err(),
        "model_select_transport_required"
    );
    assert_eq!(
        target(
            &cfg,
            Role::Orchestrator,
            Some("sol-owner"),
            Some("gpt-6.1-sol"),
            None
        )
        .unwrap_err(),
        "model_select_ambiguous_target"
    );
    let sol_target = target(&cfg, Role::Orchestrator, Some("sol-owner"), None, None).unwrap();
    assert_eq!(
        (sol_target.model_id.as_str(), sol_target.transport),
        ("gpt-6.1-sol", Transport::OpenaiCodex)
    );
    assert_eq!(
        target(
            &cfg,
            Role::Orchestrator,
            None,
            Some("gpt-6.1-sol"),
            Some(Transport::OpenaiCodex)
        )
        .unwrap()
        .profile,
        "sol-owner"
    );
    // The first-use switch: orchestrator grok-4.7 (direct_oauth) -> gpt-6.1-sol
    // (openai_codex) resolves to the declared binding and nothing else.
    let grok_target = target(&cfg, Role::Orchestrator, Some("grok-owner"), None, None).unwrap();
    assert_eq!(grok_target.transport, Transport::DirectOauth);
    assert_eq!(grok_target.model_config, grok);
    assert_eq!(
        target(&cfg, Role::Orchestrator, Some("default"), None, None)
            .unwrap()
            .model_id,
        "gpt-6-luna"
    );

    // Dry run reports the structural diff and the grant/catalog preflight, and
    // changes no planner or paper config.
    let before = std::fs::read(&live).unwrap();
    let paper_before = std::fs::read(&paper_path).unwrap();
    let previewed = preview(
        &paper,
        &cfg,
        Role::Orchestrator,
        &sol_target,
        &live,
        &paper_path,
    );
    assert_eq!(previewed["status"], "dry_run");
    assert_eq!(previewed["writes"], false);
    assert_eq!(previewed["diff"]["path"], "orchestrator/model");
    assert_eq!(previewed["diff"]["before"]["model_id"], "gpt-6-luna");
    assert_eq!(previewed["diff"]["after"]["model_id"], "gpt-6.1-sol");
    assert_eq!(previewed["diff"]["after"]["transport"], "openai_codex");
    assert_eq!(previewed["current"]["model_id"], "gpt-6-luna");
    // The fixture grants are absent, so the preflight refuses without reading or
    // printing any credential content and the catalog is reported unavailable.
    assert_eq!(previewed["grant"]["status"], "blocked");
    assert_eq!(previewed["grant"]["reason"], "grant_unavailable");
    assert_eq!(previewed["catalog"]["status"], "unavailable");
    assert_eq!(previewed["catalog"]["reason"], "grant_unavailable");
    assert_eq!(previewed["session"]["open"], false);
    assert_eq!(previewed["refusal"], "model_select_grant_required");
    assert_eq!(std::fs::read(&live).unwrap(), before);
    assert_eq!(std::fs::read(&paper_path).unwrap(), paper_before);

    // A switch writes a new planner file beside the live one and leaves the live
    // file byte-identical; the new file carries only the model identity change.
    let (out, policy_sha256) =
        write(&cfg, &live, c.session, Role::Orchestrator, &sol_target).unwrap();
    assert_eq!(out.parent(), live.parent());
    assert_eq!(std::fs::read(&live).unwrap(), before);
    let switched_cfg = PlannerConfig::load(&out).unwrap();
    let orchestrator = switched_cfg.orchestrator.as_ref().unwrap();
    assert_eq!(orchestrator.model.model_id, "gpt-6.1-sol");
    assert_eq!(orchestrator.model.transport, Transport::OpenaiCodex);
    assert_eq!(
        orchestrator.model.model_config.as_deref(),
        Some(sol.as_path())
    );
    assert_eq!(orchestrator.model.daily_call_budget, 20);
    assert_eq!(switched_cfg.digest.model_id, "gpt-6-luna");
    assert_eq!(switched_cfg.shortlist_n, cfg.shortlist_n);
    assert_eq!(
        digest(&planner::policy_bytes(&switched_cfg).unwrap()),
        policy_sha256
    );
    assert_ne!(
        planner::policy_bytes(&cfg).unwrap(),
        planner::policy_bytes(&switched_cfg).unwrap()
    );
    // Round trip back to the configured default restores the original policy.
    let back = target(&cfg, Role::Orchestrator, Some("default"), None, None).unwrap();
    let restored = switched(&switched_cfg, Role::Orchestrator, &back).unwrap();
    assert_eq!(
        planner::policy_bytes(&restored).unwrap(),
        planner::policy_bytes(&cfg).unwrap()
    );
    // ...and back again to the same profile is the same policy bytes.
    let forward = switched(&restored, Role::Orchestrator, &sol_target).unwrap();
    assert_eq!(
        planner::policy_bytes(&forward).unwrap(),
        planner::policy_bytes(&switched_cfg).unwrap()
    );

    // Digest switches move only the digest role and keep the orchestrator.
    let digest = target(&cfg, Role::Digest, Some("grok-owner"), None, None).unwrap();
    assert_eq!(digest.model_id, "grok-4.7");
    let (digest_out, _) = write(&cfg, &live, c.session, Role::Digest, &digest).unwrap();
    let digest_cfg = PlannerConfig::load(&digest_out).unwrap();
    assert_eq!(digest_cfg.digest.model_id, "grok-4.7");
    assert_eq!(digest_cfg.digest.transport, Transport::DirectOauth);
    assert_eq!(
        digest_cfg.digest.model_config.as_deref(),
        Some(grok.as_path())
    );
    assert_eq!(
        digest_cfg.orchestrator.as_ref().unwrap().model.model_id,
        "gpt-6-luna"
    );
    assert_eq!(
        preview(&paper, &cfg, Role::Digest, &digest, &live, &paper_path)["diff"]["path"],
        "digest"
    );
    // A digest switch never resolves through the orchestrator's own block.
    assert_eq!(
        target(&cfg, Role::Digest, Some("default"), None, None)
            .unwrap()
            .model_id,
        "gpt-6-luna"
    );
    assert_eq!(
        target(&cfg, Role::Digest, Some("default"), None, None)
            .unwrap()
            .model_config,
        digest_model
    );

    // The paper config is repointed atomically and keeps every operator-only
    // field, including the ones the runtime never re-serializes.
    repoint(&paper_path, &live, &out).unwrap();
    let repointed: Value = serde_json::from_slice(&std::fs::read(&paper_path).unwrap()).unwrap();
    assert_eq!(repointed["planner_config"], json!(out));
    assert_eq!(repointed["sessions_root"], json!(root.join("sessions")));
    assert_eq!(repointed["history_directory"], json!(root.join("history")));
    assert_eq!(repointed["directory"], json!(root));
    // A second repoint of the same live path is refused: the config moved.
    assert_eq!(
        repoint(&paper_path, &live, &out).unwrap_err(),
        "model_select_config_changed"
    );
    std::fs::write(&paper_path, &paper_before).unwrap();

    // Switching to the binding the role already uses is a no-op refusal: it
    // would stop the book for a policy that did not move.
    let default_target = target(&cfg, Role::Orchestrator, Some("default"), None, None).unwrap();
    assert_eq!(
        changed(&cfg, Role::Orchestrator, &default_target).unwrap_err(),
        "model_select_unchanged"
    );
    assert_eq!(changed(&cfg, Role::Orchestrator, &sol_target), Ok(()));

    // A role whose effective credential comes from the shared top-level
    // `model_config` cannot be repointed without changing another role.
    let mut shared = cfg.clone();
    shared.model_config = Some(base.clone());
    assert_eq!(
        switchable(&shared, &sol_target).unwrap_err(),
        "model_select_shared_credential"
    );
    let mut same = cfg.clone();
    same.model_config = Some(sol.clone());
    assert_eq!(switchable(&same, &sol_target), Ok(()));

    // A future-dated live schedule would write a switch that is not in force for
    // this session; the command refuses instead of silently deferring it.
    let mut scheduled = cfg.clone();
    scheduled.effective_from = Some(c.session + chrono::Duration::days(1));
    scheduled.previous_config = Some(root.join("previous.json"));
    assert_eq!(
        effective(&scheduled, &paper).unwrap_err(),
        "model_select_policy_not_effective"
    );
    assert_eq!(effective(&cfg, &paper), Ok(()));

    // The audit context names the role, profile, exact binding and planner file.
    let audit = audit_context(Role::Orchestrator, &sol_target, &out);
    assert_eq!(audit["role"], "orchestrator");
    assert_eq!(audit["profile"], "sol-owner");
    assert_eq!(audit["model_id"], "gpt-6.1-sol");
    assert_eq!(audit["transport"], "openai_codex");
    assert_eq!(audit["applies"], "next_session");
    assert_eq!(audit["live_enabled"], false);
    assert_eq!(audit["planner_config"], json!(out));

    // A real request: confirmed, not a dry run.
    let request = |confirm: bool, dry_run: bool| Request {
        path: &paper_path,
        role: Role::Orchestrator,
        profile: Some("sol-owner"),
        model_id: None,
        transport: None,
        dry_run,
        confirm,
        json: true,
    };

    // A running paper session refuses the switch before anything is written.
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(c.directory.join("controller.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    assert_eq!(session(&c).unwrap_err(), "model_select_session_open");
    assert_eq!(
        select_entry(&request(true, false)).unwrap_err(),
        "model_select_session_open"
    );
    fs2::FileExt::unlock(&lock).unwrap();
    drop(lock);
    assert_eq!(session(&c).unwrap()["open"], false);
    // The controller's root startup fence is the other half of the same check.
    let root_lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("sessions/controller.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&root_lock).unwrap();
    assert_eq!(session(&c).unwrap_err(), "model_select_session_open");
    fs2::FileExt::unlock(&root_lock).unwrap();
    drop(root_lock);

    // Without an authenticated grant for the target transport the switch is
    // refused before the paper config or the live planner file is touched.
    assert_eq!(
        authenticated(&grant(&sol_target)).unwrap_err(),
        "model_select_grant_required"
    );
    assert_eq!(
        select_entry(&request(true, false)).unwrap_err(),
        "model_select_grant_required"
    );
    assert_eq!(std::fs::read(&paper_path).unwrap(), paper_before);
    assert_eq!(std::fs::read(&live).unwrap(), before);

    // A real switch needs the operator's paper-review confirmation, exactly like
    // `config-apply`; without it nothing is written.
    assert_eq!(
        select_entry(&request(false, false)).unwrap_err(),
        "model_select_confirm_paper_review_required"
    );
    assert_eq!(std::fs::read(&paper_path).unwrap(), paper_before);
    assert_eq!(std::fs::read(&live).unwrap(), before);
}
