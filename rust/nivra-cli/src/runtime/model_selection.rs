//! Operator-selected orchestrator model binding.
//!
//! The reviewed planner policy names exact model/credential profiles and the
//! profile names it admits. A selection is a separate, versioned runtime setting
//! so a model-only switch never rewrites the sealed policy, merges role charges
//! or resets the persistent session. Requests are durable pending/active
//! revisions; the controller promotes a pending revision only at a turn boundary.
use super::{Config, digest, planner, rollover};
use nivra_cli::lock::AdvisoryLock;
use nivra_cli::native_transport::Transport;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::ExitCode,
};

pub(crate) const DEFAULT_PROFILE: &str = "default";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 65_536;
const MAX_RECEIPTS: usize = 16;
const LOCK_WAIT_MS: u64 = 1_000;

type SelectionResult<T> = std::result::Result<T, &'static str>;

/// Exact model/credential binding. The credential path is application-owned and
/// never inferred from another role, environment or another application's cache.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub model_id: String,
    pub transport: Transport,
    pub model_config: PathBuf,
}

impl Binding {
    pub(crate) fn credential_sha256(&self) -> String {
        digest(self.model_config.as_os_str().as_encoded_bytes())
    }
    fn as_json(&self) -> Value {
        json!({
            "model_id": self.model_id,
            "transport": self.transport,
            "credential_sha256": self.credential_sha256(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    revision: u64,
    profile: String,
    model: Binding,
    policy_sha256: String,
    request_id: String,
    applied_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    revision: u64,
    profile: String,
    model: Binding,
    policy_sha256: String,
    request_id: String,
    requested_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    revision: u64,
    request_id: String,
    profile: String,
    model_id: String,
    transport: Transport,
    policy_sha256: String,
    /// requested | applied | refused | superseded
    state: String,
    #[serde(default)]
    reason: Option<String>,
    at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    version: u32,
    session: String,
    session_binding: String,
    revision: u64,
    #[serde(default)]
    active: Option<Active>,
    #[serde(default)]
    pending: Option<Pending>,
    #[serde(default)]
    receipts: Vec<Receipt>,
}

/// Operator-maintained runtime selection input. It contains only an admitted
/// profile name; model bindings and credentials remain in the reviewed planner
/// policy. Kept in its own file so it cannot alter the sealed paper config.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SelectionConfig {
    version: u32,
    profile: String,
}

impl SelectionConfig {
    fn load(path: &Path) -> SelectionResult<Self> {
        let bytes =
            crate::read_bounded(path, 4096).map_err(|_| "model_selection_config_unreadable")?;
        let value: Self =
            serde_json::from_slice(&bytes).map_err(|_| "model_selection_config_invalid")?;
        if value.version != 1 {
            return Err("model_selection_config_invalid");
        }
        Ok(value)
    }
}

/// A validated active selection applied to one model turn.
#[derive(Clone, Debug)]
pub(crate) struct Resolved {
    pub(crate) revision: u64,
    pub(crate) profile: String,
    pub(crate) model: Binding,
    pub(crate) policy_sha256: String,
    pub(crate) applied_ms: i64,
}

impl Resolved {
    pub(crate) fn receipt(&self) -> Value {
        let mut value = self.model.as_json();
        value["revision"] = json!(self.revision);
        value["profile"] = json!(self.profile);
        value["policy_sha256"] = json!(self.policy_sha256);
        value["applied_ms"] = json!(self.applied_ms);
        value
    }
    /// Apply only model identity. Tools, budgets, timeouts and sandbox stay with
    /// the reviewed trigger policy.
    pub(crate) fn apply(&self, role: &mut planner::ModelRole) {
        role.model_id = self.model.model_id.clone();
        role.transport = self.model.transport;
        role.model_config = Some(self.model.model_config.clone());
    }
}

fn state_path(c: &Config) -> PathBuf {
    c.directory.join("model/model-selection.json")
}

fn lock_path(c: &Config) -> PathBuf {
    c.directory.join("model/model-selection.lock")
}

fn fresh(c: &Config) -> SelectionResult<Selection> {
    Ok(Selection {
        version: VERSION,
        session: c.session.to_string(),
        session_binding: super::book_binding(c).map_err(|_| "model_selection_binding_invalid")?,
        revision: 0,
        active: None,
        pending: None,
        receipts: Vec::new(),
    })
}

fn lock(c: &Config) -> SelectionResult<AdvisoryLock> {
    use std::time::{Duration, Instant};
    let path = lock_path(c);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|_| "model_selection_unwritable")?;
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|_| "model_selection_unwritable")?;
    let meta = file
        .metadata()
        .map_err(|_| "model_selection_lock_invalid")?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.uid()
            != fs::metadata("/proc/self")
                .map_err(|_| "model_selection_lock_invalid")?
                .uid()
    {
        return Err("model_selection_lock_invalid");
    }
    let started = Instant::now();
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(AdvisoryLock::new(file)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if started.elapsed() >= Duration::from_millis(LOCK_WAIT_MS) {
                    return Err("model_selection_busy");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Err("model_selection_lock_invalid"),
        }
    }
}

fn load(c: &Config) -> SelectionResult<Selection> {
    let path = state_path(c);
    if !path.exists() {
        return fresh(c);
    }
    let bytes = crate::read_bounded(&path, MAX_BYTES).map_err(|_| "model_selection_unreadable")?;
    let state: Selection = serde_json::from_slice(&bytes).map_err(|_| "model_selection_invalid")?;
    if state.version != VERSION
        || state.session != c.session.to_string()
        || state.session_binding
            != super::book_binding(c).map_err(|_| "model_selection_binding_invalid")?
        || state.receipts.len() > MAX_RECEIPTS
    {
        return Err("model_selection_invalid");
    }
    Ok(state)
}

fn save(c: &Config, state: &Selection) -> SelectionResult<()> {
    let value = serde_json::to_value(state).map_err(|_| "model_selection_unwritable")?;
    rollover::atomic(&state_path(c), &value).map_err(|_| "model_selection_unwritable")
}

fn policy_sha256(cfg: &planner::PlannerConfig) -> SelectionResult<String> {
    planner::policy_bytes(cfg)
        .map(|bytes| digest(&bytes))
        .map_err(|_| "model_selection_policy_invalid")
}

/// Resolve a policy-admitted profile to its exact binding. `default` is the
/// configured orchestrator model and never needs a separate admission entry so
/// existing configurations keep their behavior.
pub(super) fn binding(cfg: &planner::PlannerConfig, name: &str) -> SelectionResult<Binding> {
    let orchestrator = cfg
        .orchestrator
        .as_ref()
        .ok_or("model_selection_orchestrator_missing")?;
    if !orchestrator.enabled {
        return Err("model_selection_orchestrator_disabled");
    }
    if name == DEFAULT_PROFILE {
        let model_id = orchestrator.model.model_id.clone();
        let model_config = cfg
            .model_config
            .clone()
            .or_else(|| orchestrator.model.model_config.clone())
            .ok_or("model_selection_default_config_missing")?;
        if model_id.is_empty() {
            return Err("model_selection_default_model_missing");
        }
        return validate_owner_binding(
            cfg,
            Binding {
                model_id,
                transport: orchestrator.model.transport,
                model_config,
            },
        );
    }
    if !orchestrator
        .admitted_models
        .iter()
        .any(|admitted| admitted == name)
    {
        return Err("model_selection_not_admitted");
    }
    let profile = orchestrator
        .model_profiles
        .get(name)
        .ok_or("model_selection_profile_missing")?;
    validate_owner_binding(
        cfg,
        Binding {
            model_id: profile.model_id.clone(),
            transport: profile.transport,
            model_config: profile.model_config.clone(),
        },
    )
}

fn validate_owner_binding(
    cfg: &planner::PlannerConfig,
    model: Binding,
) -> SelectionResult<Binding> {
    super::model_admission::binding_shape(&model)?;
    super::model_admission::transport_tools(
        model.transport,
        &cfg.role(super::orchestrator_model::Trigger::Supervisor)
            .tools,
    )?;
    Ok(model)
}

fn profile_json(cfg: &planner::PlannerConfig) -> Value {
    let Some(orchestrator) = cfg.orchestrator.as_ref() else {
        return json!({"profiles": [], "admitted": []});
    };
    json!({
        "default": binding(cfg, DEFAULT_PROFILE).map(|b| b.as_json()).unwrap_or(Value::Null),
        "profiles": orchestrator.model_profiles.iter().map(|(name, profile)| {
            json!({"name": name, "model_id": profile.model_id, "transport": profile.transport,
                "credential_sha256": digest(profile.model_config.as_os_str().as_encoded_bytes()),
                "admitted": orchestrator.admitted_models.iter().any(|admitted| admitted == name)})
        }).collect::<Vec<_>>(),
        "admitted": orchestrator.admitted_models,
    })
}

fn active_json(active: &Option<Active>) -> Value {
    match active {
        Some(active) => json!({
            "revision": active.revision,
            "profile": active.profile,
            "model_id": active.model.model_id,
            "transport": active.model.transport,
            "credential_sha256": active.model.credential_sha256(),
            "policy_sha256": active.policy_sha256,
            "request_id": active.request_id,
            "applied_ms": active.applied_ms,
        }),
        None => json!({"revision": 0, "profile": DEFAULT_PROFILE, "applied_ms": Value::Null}),
    }
}

fn pending_json(pending: &Option<Pending>) -> Value {
    match pending {
        Some(pending) => json!({
            "revision": pending.revision,
            "profile": pending.profile,
            "model_id": pending.model.model_id,
            "transport": pending.model.transport,
            "credential_sha256": pending.model.credential_sha256(),
            "policy_sha256": pending.policy_sha256,
            "request_id": pending.request_id,
            "requested_ms": pending.requested_ms,
        }),
        None => Value::Null,
    }
}

/// Show configured, admitted, active and pending models without changing state.
pub(crate) fn show(c: &Config, cfg: &planner::PlannerConfig) -> SelectionResult<Value> {
    let state = load(c)?;
    let configured = profile_json(cfg);
    let last = state.receipts.last().cloned();
    Ok(json!({
        "status": "ok",
        "session": c.session.to_string(),
        "revision": state.revision,
        "configured": configured["default"].clone(),
        "profiles": configured["profiles"].clone(),
        "admitted": configured["admitted"].clone(),
        "active": active_json(&state.active),
        "pending": pending_json(&state.pending),
        "receipts": state.receipts,
        "last_receipt": last.map(|receipt| json!(receipt)).unwrap_or(Value::Null),
    }))
}

/// Request an admitted profile for the next orchestrator turn. The request is
/// durable and serialized by revision; it never changes the sealed policy.
pub(crate) fn request(
    c: &Config,
    cfg: &planner::PlannerConfig,
    name: &str,
    expect_revision: Option<u64>,
    request_id: Option<&str>,
) -> SelectionResult<Value> {
    let model = binding(cfg, name)?;
    let policy_sha256 = policy_sha256(cfg)?;
    let _lock = lock(c)?;
    let mut state = load(c)?;
    if let Some(expected) = expect_revision
        && expected != state.revision
    {
        return Err("model_selection_revision_changed");
    }
    let request_id = request_id.map(str::to_owned).unwrap_or_else(|| {
        format!(
            "model-{}-{}",
            c.session,
            digest(
                format!(
                    "{}:{}:{}:{}",
                    state.revision,
                    name,
                    model.model_id,
                    super::millis()
                )
                .as_bytes()
            )[..16]
                .to_owned()
        )
    });
    if let Some(active) = &state.active
        && active.profile == name
        && active.model == model
        && active.policy_sha256 == policy_sha256
        && state.pending.is_none()
    {
        return Ok(json!({
            "status": "unchanged",
            "revision": state.revision,
            "active": active_json(&state.active),
        }));
    }
    if let Some(pending) = &state.pending
        && pending.profile == name
        && pending.model == model
        && pending.policy_sha256 == policy_sha256
    {
        return Ok(json!({
            "status": "pending",
            "revision": state.revision,
            "pending": pending_json(&state.pending),
        }));
    }
    let now = super::millis();
    if let Some(previous) = state.pending.take() {
        state.receipts.push(Receipt {
            revision: previous.revision,
            request_id: previous.request_id,
            profile: previous.profile,
            model_id: previous.model.model_id,
            transport: previous.model.transport,
            policy_sha256: previous.policy_sha256,
            state: "superseded".into(),
            reason: Some("model_selection_superseded".into()),
            at_ms: now,
        });
    }
    state.revision = state.revision.saturating_add(1);
    state.pending = Some(Pending {
        revision: state.revision,
        profile: name.to_owned(),
        policy_sha256,
        request_id: request_id.clone(),
        requested_ms: now,
        model,
    });
    state.receipts.push(Receipt {
        revision: state.revision,
        request_id: request_id.clone(),
        profile: name.to_owned(),
        model_id: state
            .pending
            .as_ref()
            .map(|p| p.model.model_id.clone())
            .unwrap_or_default(),
        transport: state
            .pending
            .as_ref()
            .map(|p| p.model.transport)
            .unwrap_or_default(),
        policy_sha256: state
            .pending
            .as_ref()
            .map(|p| p.policy_sha256.clone())
            .unwrap_or_default(),
        state: "requested".into(),
        reason: None,
        at_ms: now,
    });
    truncate_receipts(&mut state);
    save(c, &state)?;
    Ok(json!({
        "status": "pending",
        "revision": state.revision,
        "request_id": request_id,
        "pending": pending_json(&state.pending),
    }))
}

fn truncate_receipts(state: &mut Selection) {
    if state.receipts.len() > MAX_RECEIPTS {
        let excess = state.receipts.len() - MAX_RECEIPTS;
        state.receipts.drain(..excess);
    }
}

/// Apply a pending revision at an idle turn boundary. A pending profile that is
/// no longer admitted or no longer matches its exact binding is refused; the
/// last valid active selection is retained and the refusal is durable.
pub(crate) fn promote(
    c: &Config,
    cfg: &planner::PlannerConfig,
    now: i64,
) -> SelectionResult<Option<Value>> {
    let _lock = lock(c)?;
    let mut state = load(c)?;
    let Some(pending) = state.pending.clone() else {
        return Ok(None);
    };
    let current_policy_sha256 = policy_sha256(cfg)?;
    let result = binding(cfg, &pending.profile).and_then(|model| {
        if model != pending.model || pending.policy_sha256 != current_policy_sha256 {
            Err("model_selection_policy_changed")
        } else {
            Ok(model)
        }
    });
    let (state_name, reason) = match &result {
        Ok(_) => ("applied".to_owned(), None),
        Err(reason) => ("refused".to_owned(), Some((*reason).to_owned())),
    };
    state.pending = None;
    if let Ok(model) = result {
        state.active = Some(Active {
            revision: pending.revision,
            profile: pending.profile.clone(),
            model,
            policy_sha256: pending.policy_sha256.clone(),
            request_id: pending.request_id.clone(),
            applied_ms: now,
        });
    }
    state.receipts.push(Receipt {
        revision: pending.revision,
        request_id: pending.request_id,
        profile: pending.profile,
        model_id: pending.model.model_id,
        transport: pending.model.transport,
        policy_sha256: pending.policy_sha256,
        state: state_name.clone(),
        reason: reason.clone(),
        at_ms: now,
    });
    truncate_receipts(&mut state);
    save(c, &state)?;
    let mut value = json!({
        "status": state_name,
        "revision": pending.revision,
        "active": active_json(&state.active),
    });
    if let Some(reason) = reason {
        value["reason"] = json!(reason);
    }
    Ok(Some(value))
}

/// Resolve the active selection for a new turn. A selection invalidated by a
/// changed policy is an explicit failure, never a silent fallback to another
/// model. An absent selection keeps the configured orchestrator model.
pub(crate) fn resolve(
    c: &Config,
    cfg: &planner::PlannerConfig,
) -> SelectionResult<Option<Resolved>> {
    let state = load(c)?;
    let Some(active) = state.active else {
        return Ok(None);
    };
    if active.policy_sha256 != policy_sha256(cfg)? {
        return Err("model_selection_policy_changed");
    }
    let model = binding(cfg, &active.profile).map_err(|_| "model_selection_policy_changed")?;
    if model != active.model {
        return Err("model_selection_policy_changed");
    }
    Ok(Some(Resolved {
        revision: active.revision,
        profile: active.profile,
        model,
        policy_sha256: active.policy_sha256,
        applied_ms: active.applied_ms,
    }))
}

pub(super) fn load_current(path: &Path) -> SelectionResult<(Config, planner::PlannerConfig)> {
    let bytes = crate::read_bounded(path, MAX_BYTES).map_err(|_| "config_unreadable")?;
    let paper: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
    let current = rollover::current(&paper).map_err(|_| "model_selection_session_unavailable")?;
    let planner_path = current
        .planner_config
        .as_deref()
        .ok_or("model_selection_planner_missing")?;
    let cfg = planner::PlannerConfig::load_for_session(planner_path, current.session)
        .map_err(|_| "model_selection_policy_invalid")?;
    Ok((current, cfg))
}

pub(super) fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[derive(clap::Subcommand)]
pub(crate) enum Commands {
    /// Read host and session state without a model call.
    Status {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Queue one read-only question through the controller's aside slot.
    Ask {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        question: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long)]
        ttl_ms: Option<i64>,
    },
    /// Enqueue one explicitly requested, expiring owner instruction.
    Instruct {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        text: String,
        /// `review_only` or `decision_context`; never an execution authority.
        #[arg(long)]
        scope: String,
        /// Exact current session reported by `agent status`.
        #[arg(long)]
        expect_session: String,
        /// Exact current instruction revision reported by `agent status`.
        #[arg(long)]
        expect_revision: u64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        idempotency_key: Option<String>,
        #[arg(long)]
        ttl_ms: Option<i64>,
    },
    /// Poll one durable request id without resubmitting it.
    Result {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel only that question or instruction, never the trading instance.
    Cancel {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or request the orchestrator model binding.
    Model {
        #[command(subcommand)]
        command: ModelCommands,
    },
}

#[derive(clap::Subcommand)]
pub(crate) enum ModelCommands {
    /// Show configured, active and pending orchestrator models.
    Show {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Request an admitted model profile for the next orchestrator turn.
    Set {
        #[arg(long)]
        config: PathBuf,
        /// Profile name from the policy's admitted list, or `default`.
        #[arg(long)]
        model: String,
        #[arg(long)]
        json: bool,
        /// Refuse if the durable selection revision has changed.
        #[arg(long)]
        expect_revision: Option<u64>,
        #[arg(long)]
        request_id: Option<String>,
    },
    /// Reload a versioned profile-name selection file through the same durable
    /// selection validator used by `set`.
    Reload {
        #[arg(long)]
        config: PathBuf,
        /// Versioned JSON runtime selection file: {"version":1,"profile":"NAME"}.
        #[arg(long)]
        selection_config: PathBuf,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        expect_revision: Option<u64>,
        #[arg(long)]
        request_id: Option<String>,
    },
}

pub(crate) fn run(command: Commands) -> ExitCode {
    let (result, prefix) = match command {
        Commands::Status { config, json } => (
            super::agent_cli::status_entry(&config, json),
            "agent_refused",
        ),
        Commands::Ask {
            config,
            question,
            json,
            request_id,
            idempotency_key,
            ttl_ms,
        } => (
            super::agent_cli::ask_entry(
                &config,
                &question,
                request_id.as_deref(),
                idempotency_key.as_deref(),
                ttl_ms,
                json,
            ),
            "agent_refused",
        ),
        Commands::Instruct {
            config,
            text,
            scope,
            expect_session,
            expect_revision,
            json,
            request_id,
            idempotency_key,
            ttl_ms,
        } => (
            super::agent_cli::instruct_entry(
                &config,
                &text,
                &scope,
                &expect_session,
                expect_revision,
                request_id.as_deref(),
                idempotency_key.as_deref(),
                ttl_ms,
                json,
            ),
            "agent_refused",
        ),
        Commands::Result {
            config,
            request_id,
            json,
        } => (
            super::agent_cli::result_entry(&config, &request_id, json),
            "agent_refused",
        ),
        Commands::Cancel {
            config,
            request_id,
            json,
        } => (
            super::agent_cli::cancel_entry(&config, &request_id, json),
            "agent_refused",
        ),
        Commands::Model { command } => (
            match command {
                ModelCommands::Show { config, json } => show_entry(&config, json),
                ModelCommands::Set {
                    config,
                    model,
                    json,
                    expect_revision,
                    request_id,
                } => set_entry(
                    &config,
                    &model,
                    expect_revision,
                    request_id.as_deref(),
                    json,
                ),
                ModelCommands::Reload {
                    config,
                    selection_config,
                    json,
                    expect_revision,
                    request_id,
                } => reload_entry(
                    &config,
                    &selection_config,
                    expect_revision,
                    request_id.as_deref(),
                    json,
                ),
            },
            "model_selection_refused",
        ),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            eprintln!("{prefix}: {reason}");
            ExitCode::FAILURE
        }
    }
}

fn show_entry(path: &Path, json_out: bool) -> SelectionResult<()> {
    let (c, cfg) = load_current(path)?;
    let value = show(&c, &cfg)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|_| "model_selection_unwritable")?
        );
    } else {
        let active = &value["active"];
        println!(
            "session={} revision={} active={} pending={}",
            value["session"].as_str().unwrap_or_default(),
            value["revision"].as_u64().unwrap_or_default(),
            active["profile"].as_str().unwrap_or(DEFAULT_PROFILE),
            value["pending"]["profile"].as_str().unwrap_or("-")
        );
    }
    Ok(())
}

fn set_entry(
    path: &Path,
    model: &str,
    expect_revision: Option<u64>,
    request_id: Option<&str>,
    json_out: bool,
) -> SelectionResult<()> {
    let (c, cfg) = load_current(path)?;
    let value = request(&c, &cfg, model, expect_revision, request_id)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|_| "model_selection_unwritable")?
        );
    } else {
        println!(
            "status={} revision={} profile={} applies=next_idle_turn",
            value["status"].as_str().unwrap_or_default(),
            value["revision"].as_u64().unwrap_or_default(),
            value["pending"]["profile"].as_str().unwrap_or(model)
        );
    }
    Ok(())
}

/// Queue the profile named by the versioned runtime selection file. The profile
/// resolves against the currently referenced reviewed planner config; reload
/// does not alter that policy and applies only at the next idle turn boundary.
fn reload_entry(
    path: &Path,
    selection_path: &Path,
    expect_revision: Option<u64>,
    request_id: Option<&str>,
    json_out: bool,
) -> SelectionResult<()> {
    let value = reload_from_config(path, selection_path, expect_revision, request_id)?;
    if json_out {
        println!(
            "{}",
            serde_json::to_string(&value).map_err(|_| "model_selection_unwritable")?
        );
    } else {
        let profile = value["pending"]["profile"]
            .as_str()
            .or_else(|| value["active"]["profile"].as_str())
            .unwrap_or("unknown");
        println!(
            "status={} revision={} profile={} applies=next_idle_turn",
            value["status"].as_str().unwrap_or_default(),
            value["revision"].as_u64().unwrap_or_default(),
            profile,
        );
    }
    Ok(())
}

fn reload_from_config(
    path: &Path,
    selection_path: &Path,
    expect_revision: Option<u64>,
    request_id: Option<&str>,
) -> SelectionResult<Value> {
    let (c, cfg) = load_current(path)?;
    let selection = SelectionConfig::load(selection_path)?;
    request(&c, &cfg, &selection.profile, expect_revision, request_id)
}

#[cfg(test)]
pub(crate) fn exercise() {
    use super::planner::PlannerConfig;
    super::agent_cli::exercise();
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::tests::config(dir.path());
    c.session = "2026-10-24".parse().unwrap();
    let base = dir.path().join("openai-model.json");
    let grok = dir.path().join("grok-model.json");
    fs::write(&base, b"{}").unwrap();
    fs::write(&grok, b"{}").unwrap();
    let cfg: PlannerConfig = serde_json::from_value(json!({
        "enabled": true,
        "provider": "xai_subscription",
        "shortlist_n": 40,
        "digest": {"enabled": true, "model_id": "gpt-6-luna", "model_config": dir.path().join("digest-model.json"), "transport": "openai_codex", "daily_call_budget": 10, "timeout_ms": 20000},
        "events": {"enabled": true, "cooldown_ms": 0, "max_pending": 10, "policy": {"price_move_bps":100,"max_event_age_ms":300000,"max_news_age_ms":60000,"max_instruments":20,"max_identities":100}},
        "orchestrator": {
            "enabled": true,
            "daily_call_budget": 40,
            "model": {"model_config": base.to_string_lossy(), "model_id": "gpt-6-luna", "transport": "openai_codex", "daily_call_budget": 20},
            "triggers": {"supervisor": {"prompt": dir.path().join("supervisor.md").to_string_lossy(), "daily_call_budget": 40}},
            "model_profiles": {
                "grok-4.7": {"model_id": "grok-4.7", "transport": "direct_oauth", "model_config": grok.to_string_lossy()}
            },
            "admitted_models": ["grok-4.7"]
        }
    }))
    .unwrap();

    let digest_role = cfg.role(super::orchestrator_model::Trigger::Digest);
    let digest_path = digest_role.model_config.as_deref().unwrap();
    assert_eq!(
        super::model_admission::validate(&c, &cfg, "digest", &digest_role, digest_path).unwrap(),
        "digest"
    );
    let mut wrong = digest_role.clone();
    wrong.model_id = "unconfigured-model".into();
    assert!(super::model_admission::validate(&c, &cfg, "digest", &wrong, digest_path).is_err());
    assert!(super::model_admission::validate(&c, &cfg, "digest", &digest_role, &base).is_err());
    let mut future = cfg.clone();
    future.digest.model_id = "configured-factual-compiler".into();
    assert!(
        super::model_admission::validate(&c, &future, "digest", &future.digest, digest_path)
            .is_ok()
    );
    let mut incompatible = cfg.clone();
    incompatible
        .orchestrator
        .as_mut()
        .unwrap()
        .triggers
        .get_mut(&super::orchestrator_model::Trigger::Supervisor)
        .unwrap()
        .tools = Some(vec!["x_search".into()]);
    assert_eq!(
        binding(&incompatible, "default").unwrap_err(),
        "model_selection_profile_tools_incompatible"
    );
    let default_role = cfg.role(super::orchestrator_model::Trigger::Supervisor);
    assert_eq!(
        super::model_admission::validate(&c, &cfg, "supervisor", &default_role, &base).unwrap(),
        "orchestrator.default"
    );
    let shown = show(&c, &cfg).unwrap();
    assert_eq!(shown["active"]["profile"], DEFAULT_PROFILE);
    assert_eq!(shown["revision"], 0);
    assert_eq!(shown["admitted"][0], "grok-4.7");

    let requested = request(&c, &cfg, "grok-4.7", Some(0), Some("req-1")).unwrap();
    assert_eq!(requested["status"], "pending");
    assert_eq!(requested["revision"], 1);
    assert!(resolve(&c, &cfg).unwrap().is_none());
    let mut pending_role = default_role.clone();
    pending_role.model_id = "grok-4.7".into();
    pending_role.transport = Transport::DirectOauth;
    pending_role.model_config = Some(grok.clone());
    assert!(
        super::model_admission::validate(&c, &cfg, "supervisor", &pending_role, &grok).is_err()
    );

    assert_eq!(
        request(&c, &cfg, "default", Some(0), None).unwrap_err(),
        "model_selection_revision_changed"
    );
    assert_eq!(
        request(&c, &cfg, "missing", None, None).unwrap_err(),
        "model_selection_not_admitted"
    );

    // Promotion is the idle turn boundary; only model identity changes.
    let applied = promote(&c, &cfg, 1_000).unwrap().unwrap();
    assert_eq!(applied["status"], "applied");
    let resolved = resolve(&c, &cfg).unwrap().unwrap();
    assert_eq!(
        (resolved.revision, resolved.profile.as_str()),
        (1, "grok-4.7")
    );
    let mut role = cfg.role(super::orchestrator_model::Trigger::Supervisor);
    let tools = role.tools.clone();
    let budget = role.daily_call_budget;
    resolved.apply(&mut role);
    assert_eq!(
        super::model_admission::validate(&c, &cfg, "supervisor", &role, &grok).unwrap(),
        "orchestrator.grok-4.7"
    );
    assert!(
        super::model_admission::validate(&c, &cfg, "supervisor", &default_role, &base).is_err()
    );

    assert_eq!(role.model_id, "grok-4.7");
    assert_eq!(role.transport, Transport::DirectOauth);
    assert_eq!(role.model_config.as_ref(), Some(&grok));
    assert_eq!((role.tools, role.daily_call_budget), (tools, budget));

    // Switch back in the other direction without resetting the session.
    request(&c, &cfg, "default", Some(1), Some("req-2")).unwrap();
    assert_eq!(
        promote(&c, &cfg, 2_000).unwrap().unwrap()["active"]["profile"],
        DEFAULT_PROFILE
    );
    let resolved = resolve(&c, &cfg).unwrap().unwrap();
    assert_eq!(resolved.profile, DEFAULT_PROFILE);
    assert_eq!(resolved.model.model_id, "gpt-6-luna");
    assert_eq!(
        super::model_admission::validate(&c, &cfg, "supervisor", &default_role, &base).unwrap(),
        "orchestrator.default"
    );
    assert!(
        super::model_admission::validate(&c, &cfg, "supervisor", &pending_role, &grok).is_err()
    );

    // A policy edit that no longer binds the pending profile refuses the switch
    // and keeps the last valid active model, with a durable receipt.
    request(&c, &cfg, "grok-4.7", Some(2), Some("req-3")).unwrap();
    let mut changed = cfg.clone();
    changed
        .orchestrator
        .as_mut()
        .unwrap()
        .model_profiles
        .get_mut("grok-4.7")
        .unwrap()
        .model_id = "grok-5".into();
    let refused = promote(&c, &changed, 3_000).unwrap().unwrap();
    assert_eq!(refused["status"], "refused");
    assert_eq!(refused["reason"], "model_selection_policy_changed");
    assert_eq!(
        resolve(&c, &changed).unwrap_err(),
        "model_selection_policy_changed"
    );
    assert!(promote(&c, &changed, 3_001).unwrap().is_none());

    // Re-select the same active profile under the reviewed policy revision;
    // policy drift cannot silently preserve its old attribution.
    assert_eq!(
        request(&c, &changed, "default", Some(3), Some("req-4")).unwrap()["status"],
        "pending"
    );
    assert_eq!(
        promote(&c, &changed, 3_500).unwrap().unwrap()["status"],
        "applied"
    );

    // A new request supersedes an unconsumed pending revision.
    request(&c, &changed, "grok-4.7", Some(4), Some("req-5")).unwrap();
    assert_eq!(
        request(&c, &changed, "default", Some(5), Some("req-6")).unwrap()["status"],
        "pending"
    );
    assert_eq!(
        promote(&c, &changed, 4_000).unwrap().unwrap()["active"]["profile"],
        DEFAULT_PROFILE
    );

    // Admission/profile validation is shared by the CLI and config load.
    let mut invalid = changed.clone();
    invalid
        .orchestrator
        .as_mut()
        .unwrap()
        .admitted_models
        .push("missing".into());
    assert!(
        invalid
            .orchestrator
            .as_ref()
            .unwrap()
            .validate_models()
            .is_err()
    );
    // A versioned runtime selection file carries only a profile name. Reload
    // loads the actual paper and planner config files, then uses the same
    // validator/receipt path as `set` without changing the sealed policy hash.
    let supervisor_prompt = dir.path().join("supervisor.md");
    fs::write(&supervisor_prompt, "fixture").unwrap();
    let planner_path = dir.path().join("reviewed-planner.json");
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let paper_path = dir.path().join("paper.json");
    let mut paper_value = serde_json::to_value(&c).unwrap();
    paper_value["planner_config"] = json!(planner_path);
    fs::write(&paper_path, serde_json::to_vec(&paper_value).unwrap()).unwrap();
    let selection_path = dir.path().join("model-selection-config.json");
    fs::write(
        &selection_path,
        serde_json::to_vec(&json!({"version": 1, "profile": "grok-4.7"})).unwrap(),
    )
    .unwrap();
    let policy_hash = policy_sha256(&cfg).unwrap();
    let loaded_policy = load_current(&paper_path).unwrap().1;
    assert_eq!(policy_sha256(&loaded_policy).unwrap(), policy_hash);
    let reloaded =
        reload_from_config(&paper_path, &selection_path, Some(6), Some("reload-grok")).unwrap();
    assert_eq!(reloaded["status"], "pending");
    assert_eq!(reloaded["pending"]["profile"], "grok-4.7");
    assert_eq!(reloaded["pending"]["model_id"], "grok-4.7");
    assert_eq!(
        promote(&c, &cfg, 5_000).unwrap().unwrap()["status"],
        "applied"
    );

    // Reload back in the opposite direction using the same durable file path.
    fs::write(
        &selection_path,
        serde_json::to_vec(&json!({"version": 1, "profile": "default"})).unwrap(),
    )
    .unwrap();
    let reloaded = reload_from_config(
        &paper_path,
        &selection_path,
        Some(7),
        Some("reload-default"),
    )
    .unwrap();
    assert_eq!(reloaded["status"], "pending");
    assert_eq!(reloaded["pending"]["profile"], "default");
    assert_eq!(
        promote(&c, &cfg, 6_000).unwrap().unwrap()["status"],
        "applied"
    );
    assert_eq!(resolve(&c, &cfg).unwrap().unwrap().profile, DEFAULT_PROFILE);

    // Invalid versions and unadmitted names fail before request mutation.
    let revision = show(&c, &cfg).unwrap()["revision"].as_u64().unwrap();
    fs::write(
        &selection_path,
        serde_json::to_vec(&json!({"version": 2, "profile": "grok-4.7"})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        reload_from_config(&paper_path, &selection_path, None, None).unwrap_err(),
        "model_selection_config_invalid"
    );
    fs::write(
        &selection_path,
        serde_json::to_vec(&json!({"version": 1, "profile": "missing"})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        reload_from_config(&paper_path, &selection_path, None, None).unwrap_err(),
        "model_selection_not_admitted"
    );
    assert_eq!(show(&c, &cfg).unwrap()["revision"], revision);
    let mut relative = changed.clone();
    relative
        .orchestrator
        .as_mut()
        .unwrap()
        .model_profiles
        .get_mut("grok-4.7")
        .unwrap()
        .model_config = PathBuf::from("relative.json");
    assert!(
        relative
            .orchestrator
            .as_ref()
            .unwrap()
            .validate_models()
            .is_err()
    );

    // CLI load path: without a current.json pointer the config's own session is
    // used, and show/set operate on the same durable revision.
    fs::write(dir.path().join("supervisor.md"), b"standing role").unwrap();
    let planner_path = dir.path().join("planner.json");
    fs::write(&planner_path, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let mut paper = c.clone();
    paper.session = "2026-10-24".parse().unwrap();
    paper.planner_config = Some(planner_path.clone());
    let paper_path = dir.path().join("paper.json");
    let mut paper_value = serde_json::to_value(&paper).unwrap();
    // The runtime never re-serializes operator-only paths; write them explicitly.
    paper_value["planner_config"] = json!(planner_path);
    fs::write(&paper_path, serde_json::to_vec(&paper_value).unwrap()).unwrap();
    show_entry(&paper_path, true).unwrap();
    set_entry(&paper_path, "grok-4.7", Some(8), Some("cli-1"), true).unwrap();
    assert_eq!(
        promote(&c, &cfg, 10_000).unwrap().unwrap()["active"]["profile"],
        "grok-4.7"
    );
    assert_eq!(resolve(&c, &cfg).unwrap().unwrap().revision, 9);

    // Decision-owner prerequisites are reported by the same validator used at
    // load. The durable authority transition is checked at controller startup.
    assert!(cfg.decision_owner_requirements().is_empty());
    cfg.validate_decision_owner().unwrap();
    for (missing, reason) in [
        ("events", "decision_owner_events_required"),
        ("digest", "decision_owner_digest_required"),
    ] {
        let mut incomplete = cfg.clone();
        match missing {
            "events" => incomplete.events = None,
            _ => incomplete.digest.enabled = false,
        }
        assert_eq!(
            incomplete
                .validate_decision_owner()
                .unwrap_err()
                .to_string(),
            reason
        );
    }

    // Non-binding policy drift invalidates active and pending attribution. The
    // same model must be explicitly reselected under the new policy revision;
    // original application receipts remain in the durable history.
    let policy_dir = tempfile::tempdir().unwrap();
    let mut policy_c = super::tests::config(policy_dir.path());
    policy_c.session = "2026-10-24".parse().unwrap();
    request(&policy_c, &cfg, "grok-4.7", Some(0), Some("policy-old")).unwrap();
    assert_eq!(
        promote(&policy_c, &cfg, 7_000).unwrap().unwrap()["status"],
        "applied"
    );
    let original_policy_sha256 = policy_sha256(&cfg).unwrap();
    let mut drift = cfg.clone();
    drift.shortlist_n = drift.shortlist_n.saturating_add(1);
    let drift_policy_sha256 = policy_sha256(&drift).unwrap();
    assert_ne!(original_policy_sha256, drift_policy_sha256);
    assert_eq!(
        resolve(&policy_c, &drift).unwrap_err(),
        "model_selection_policy_changed"
    );
    let reselect = request(
        &policy_c,
        &drift,
        "grok-4.7",
        Some(1),
        Some("policy-reselect"),
    )
    .unwrap();
    assert_eq!(reselect["status"], "pending");
    assert_eq!(reselect["revision"], 2);
    assert_eq!(reselect["pending"]["policy_sha256"], drift_policy_sha256);
    assert_eq!(
        promote(&policy_c, &drift, 8_000).unwrap().unwrap()["status"],
        "applied"
    );
    let restored = resolve(&policy_c, &drift).unwrap().unwrap();
    assert_eq!(
        (restored.revision, restored.profile.as_str()),
        (2, "grok-4.7")
    );
    let receipts = show(&policy_c, &drift).unwrap()["receipts"].clone();
    assert!(
        receipts.as_array().unwrap().iter().any(|receipt| {
            receipt["request_id"] == "policy-old" && receipt["state"] == "applied"
        })
    );
    assert!(receipts.as_array().unwrap().iter().any(|receipt| {
        receipt["request_id"] == "policy-reselect" && receipt["state"] == "applied"
    }));

    request(
        &policy_c,
        &drift,
        "default",
        Some(2),
        Some("policy-pending-drift"),
    )
    .unwrap();
    let mut drift_again = drift.clone();
    drift_again.shortlist_n = drift_again.shortlist_n.saturating_add(1);
    let refused = promote(&policy_c, &drift_again, 9_000).unwrap().unwrap();
    assert_eq!(refused["status"], "refused");
    assert_eq!(refused["reason"], "model_selection_policy_changed");
    let after_refusal = show(&policy_c, &drift).unwrap();
    assert_eq!(after_refusal["active"]["revision"], 2);
    assert_eq!(after_refusal["active"]["profile"], "grok-4.7");
    assert!(
        after_refusal["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| {
                receipt["request_id"] == "policy-pending-drift"
                    && receipt["state"] == "refused"
                    && receipt["reason"] == "model_selection_policy_changed"
            })
    );
}
