//! One-time, off-hours move of an installed `ind-sadr` paper host to the
//! `nivra` layout. Dry run by default; `--apply` converges to the same end state
//! however often it runs.
//!
//! Only locations the new binary hard-codes move: the deploy directory, the
//! updater state directory, the release pointer and the units. The book binding
//! (`book_binding`) hashes the serialized paper config, which names the session
//! directory, read journal and token files by absolute path; the planner
//! authority, usage pin, model selection and scorer admission all repeat that
//! binding. Renaming the data or config directories would therefore orphan the
//! live book with no reviewed rebind, so both keep their names.
use super::{Config, planner, rollover};
use crate::update::{self, StagedRelease};
use chrono::{DateTime, FixedOffset};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::ExitCode,
};

type R<T> = std::result::Result<T, &'static str>;

/// Kept directories the configs, units and persisted bindings name.
const CONFIG_DIR: &str = ".config/ind-sadr";
const DATA_DIR: &str = "ind-sadr-data";
const RESEARCH_STAGE: &str = "/var/lib/ind-sadr-research";
/// Home-relative locations the new binary hard-codes; renamed on one filesystem.
const RENAMES: [(&str, &str); 2] = [
    ("ind-sadr-deploy", "nivra-deploy"),
    (".local/state/ind-sadr", ".local/state/nivra"),
];
const OLD_CURRENT: &str = "ind-sadr-rust-current";
const BACKUP_SUFFIX: &str = "pre-nivra";
const NEXT_SUFFIX: &str = "nivra-next";
/// Interrupted updater transactions need their own reviewed recovery first.
const UPDATER_REVIEW: [&str; 3] = [
    "pending.json",
    "pending-review.json",
    "update-recovery.json",
];
const UNITS: [(&str, &str); 4] = [
    (
        "nivra-paper.service",
        include_str!("../../../../packaging/rust/nivra-paper.service"),
    ),
    (
        "nivra-update.service",
        include_str!("../../../../packaging/rust/nivra-update.service"),
    ),
    (
        "nivra-update.timer",
        include_str!("../../../../packaging/rust/nivra-update.timer"),
    ),
    (
        "nivra-alert@.service",
        include_str!("../../../../packaging/rust/nivra-alert@.service"),
    ),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paper,
    Planner,
    Profile,
    Model,
    Fallback,
    Updater,
}

enum Step {
    Install(PathBuf),
    /// Backup the original beside it, then switch in the validated bytes.
    Rewrite {
        path: PathBuf,
        before: Vec<u8>,
        after: Vec<u8>,
        removed: Vec<String>,
        added: Vec<String>,
    },
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Link {
        path: PathBuf,
        target: PathBuf,
    },
    Unit {
        path: PathBuf,
        bytes: Vec<u8>,
    },
}

impl Step {
    fn report(&self) -> Value {
        match self {
            Step::Install(dir) => json!({"step":"install_release","directory":dir}),
            Step::Rewrite {
                path,
                removed,
                added,
                ..
            } => json!({"step":"rewrite_config","path":path,"removed":removed,"added":added}),
            Step::Rename { from, to } => json!({"step":"rename","from":from,"to":to}),
            Step::Link { path, target } => json!({"step":"link","path":path,"target":target}),
            Step::Unit { path, .. } => json!({"step":"install_unit","path":path}),
        }
    }
}

struct Plan {
    steps: Vec<Step>,
    release: StagedRelease,
    paper: Config,
    book: Value,
    next: &'static str,
}

/// Every string leaf under a renamed root moves with it; nothing else changes.
fn repath(value: &mut Value, home: &Path, at: &str, added: &mut Vec<String>) {
    match value {
        Value::String(text) => {
            for (old, new) in RENAMES {
                if let Ok(rest) = Path::new(text.as_str()).strip_prefix(home.join(old)) {
                    *text = home.join(new).join(rest).to_string_lossy().into_owned();
                    added.push(format!("{at}=renamed_root"));
                }
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter_mut().enumerate() {
                repath(item, home, &format!("{at}[{i}]"), added);
            }
        }
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                let at = if at.is_empty() {
                    key.clone()
                } else {
                    format!("{at}.{key}")
                };
                repath(item, home, &at, added);
            }
        }
        _ => {}
    }
}

/// Remove `path` (`*` matches every key at that level), recording what went.
fn drop_key(value: &mut Value, path: &[&str], at: &str, removed: &mut Vec<String>) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    let join = |key: &str| {
        if at.is_empty() {
            key.to_owned()
        } else {
            format!("{at}.{key}")
        }
    };
    match path {
        [] => {}
        [last] => {
            if map.remove(*last).is_some() {
                removed.push(join(last));
            }
        }
        ["*", rest @ ..] => {
            for (key, item) in map.iter_mut() {
                drop_key(item, rest, &join(key), removed);
            }
        }
        [first, rest @ ..] => {
            if let Some(item) = map.get_mut(*first) {
                drop_key(item, rest, &join(first), removed);
            }
        }
    }
}

/// Keys this branch removed from the config types (`.audit` U4/U5 rewrite list).
fn removed_keys(kind: Kind) -> &'static [&'static [&'static str]] {
    match kind {
        Kind::Paper => &[&["allow_legacy_model_led_default"]],
        Kind::Planner => &[
            &["decision_owner"],
            &["decide"],
            &["handoff_gate"],
            &["post_close"],
            &["digest", "minimum_turn_ms"],
            &["digest", "max_failed_tools"],
            &["orchestrator", "model", "minimum_turn_ms"],
            &["orchestrator", "model", "max_failed_tools"],
            &["orchestrator", "triggers", "operations"],
            &["orchestrator", "triggers", "strategy"],
            &["orchestrator", "triggers", "*", "after_decide"],
            &["orchestrator", "triggers", "*", "luna_no_decide_streak"],
            &["orchestrator", "triggers", "*", "minimum_turn_ms"],
            &["orchestrator", "triggers", "*", "max_failed_tools"],
            &["orchestrator", "trigger_prompts", "post_decide"],
            &["orchestrator", "trigger_prompts", "luna_no_decide_streak"],
        ],
        Kind::Profile => &[
            &["handoff_gate"],
            &["prompts", "decide"],
            &["prompts", "decision_evidence"],
            &["prompts", "decision_evidence_digest"],
            &["roles", "decide"],
        ],
        Kind::Model | Kind::Fallback | Kind::Updater => &[],
    }
}

/// Pin a default the old binary derived from `~/.config/ind-sadr` or
/// `~/ind-sadr-data`, so the renamed binary's new default never applies.
fn pin(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    path: PathBuf,
    added: &mut Vec<String>,
) {
    if !map.contains_key(key) {
        map.insert(key.into(), json!(path));
        added.push(key.into());
    }
}

fn validate(kind: Kind, path: &Path, bytes: &[u8]) -> R<()> {
    match kind {
        Kind::Paper => serde_json::from_slice::<Config>(bytes)
            .map(|_| ())
            .map_err(|_| "host_migrate_paper_config_invalid"),
        Kind::Planner => planner::PlannerConfig::parse(
            bytes,
            path.parent().ok_or("host_migrate_planner_config_invalid")?,
        )
        .map(|_| ())
        .map_err(|_| "host_migrate_planner_config_invalid"),
        Kind::Profile => super::experiment_profile::validate_profile(bytes)
            .map_err(|_| "host_migrate_experiment_profile_invalid"),
        Kind::Model => crate::model::parse_config(bytes)
            .map(|_| ())
            .map_err(|_| "host_migrate_model_config_invalid"),
        Kind::Fallback => crate::model::parse_fallback_config(bytes)
            .map(|_| ())
            .map_err(|_| "host_migrate_fallback_config_invalid"),
        Kind::Updater => update::validate_config(bytes),
    }
}

fn read(path: &Path) -> R<Vec<u8>> {
    let meta = fs::symlink_metadata(path).map_err(|_| "host_migrate_config_unreadable")?;
    if !meta.is_file() || meta.len() > 262_144 {
        return Err("host_migrate_config_unreadable");
    }
    fs::read(path).map_err(|_| "host_migrate_config_unreadable")
}

fn parse(bytes: &[u8]) -> R<Value> {
    serde_json::from_slice(bytes).map_err(|_| "host_migrate_config_invalid")
}

/// The rewritten bytes for one config, or `None` when it is already migrated.
fn rewrite(home: &Path, path: &Path, kind: Kind, fresh: Option<Value>) -> R<Option<Step>> {
    let before = read(path)?;
    let original = parse(&before)?;
    let mut value = fresh.unwrap_or_else(|| original.clone());
    let (mut removed, mut added) = (Vec::new(), Vec::new());
    for key in removed_keys(kind) {
        drop_key(&mut value, key, "", &mut removed);
    }
    repath(&mut value, home, "", &mut added);
    let config = home.join(CONFIG_DIR);
    let webhook = config.join("alert-webhook.json");
    let map = value.as_object_mut().ok_or("host_migrate_config_invalid")?;
    if matches!(kind, Kind::Paper | Kind::Model | Kind::Updater)
        && fs::symlink_metadata(&webhook).is_ok()
    {
        pin(map, "alert_webhook_file", webhook, &mut added);
    }
    if kind == Kind::Paper {
        pin(
            map,
            "sessions_root",
            home.join(DATA_DIR).join("sessions"),
            &mut added,
        );
    }
    if kind == Kind::Model && map.get("transport").and_then(Value::as_str) != Some("openai_codex") {
        pin(map, "grant_file", config.join("xai-grant"), &mut added);
    }
    if value == original {
        return Ok(None);
    }
    let mut after = serde_json::to_vec_pretty(&value).map_err(|_| "host_migrate_config_invalid")?;
    after.push(b'\n');
    validate(kind, path, &after)?;
    let backup = sibling(path, BACKUP_SUFFIX);
    if fs::symlink_metadata(&backup).is_ok() && fs::read(&backup).ok().as_ref() != Some(&before) {
        return Err("host_migrate_backup_conflict");
    }
    Ok(Some(Step::Rewrite {
        path: path.to_owned(),
        before,
        after,
        removed,
        added,
    }))
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".");
    name.push(suffix);
    PathBuf::from(name)
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// Every `model_config` file the planner names, deduplicated in order.
fn model_configs(value: &Value, out: &mut Vec<PathBuf>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                if key == "model_config"
                    && let Some(path) = item.as_str()
                    && !out.iter().any(|seen| seen == Path::new(path))
                {
                    out.push(path.into());
                }
                model_configs(item, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| model_configs(item, out)),
        _ => {}
    }
}

/// `%h`-relative form of an absolute path for a unit file.
fn unit_path(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) => format!("%h/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Packaged units with the kept config/data/research paths substituted.
fn render(home: &Path, paper: &Path, updater: &Path, template: &str) -> R<Vec<u8>> {
    let text = template
        .replace("%h/.config/nivra/rust-paper.json", &unit_path(home, paper))
        .replace("%h/.config/nivra/updater.json", &unit_path(home, updater))
        .replace("%h/.config/nivra", &format!("%h/{CONFIG_DIR}"))
        .replace("%h/nivra-data", &format!("%h/{DATA_DIR}"))
        .replace("/var/lib/nivra-research", RESEARCH_STAGE);
    if text.contains(".config/nivra") || text.contains("nivra-data") {
        return Err("host_migrate_unit_render_invalid");
    }
    Ok(text.into_bytes())
}

/// An existing lock file held by another process refuses; an absent one is free.
fn lock_free(path: &Path) -> R<()> {
    let Ok(file) = fs::OpenOptions::new().write(true).open(path) else {
        return Ok(());
    };
    file.try_lock_exclusive()
        .map_err(|_| "host_migrate_lock_held")?;
    Ok(())
}

/// Held for the whole apply so no controller, operator command or updater
/// starts while directories and configs change.
fn hold(path: &Path) -> R<fs::File> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| "host_migrate_lock_unavailable")?;
    file.try_lock_exclusive()
        .map_err(|_| "host_migrate_lock_held")?;
    Ok(file)
}

/// Controller (sessions root, live session) and updater (deploy, state) lock files.
fn lock_paths(home: &Path, paper: &Config, live: &Config) -> R<Vec<PathBuf>> {
    let root = paper
        .sessions_root
        .clone()
        .ok_or("host_migrate_sessions_root_required")?;
    let mut paths = vec![
        root.join("controller.lock"),
        live.directory.join("controller.lock"),
    ];
    for ((old, new), file) in RENAMES.iter().zip([".lock", "updater.lock"]) {
        let dir = if home.join(old).exists() {
            home.join(old)
        } else {
            home.join(new)
        };
        paths.push(dir.join(file));
    }
    Ok(paths)
}

fn book_facts(live: &Config) -> R<(Value, &'static str)> {
    let anchor: Value = serde_json::from_slice(
        &fs::read(live.directory.join("book-identity.json"))
            .map_err(|_| "host_migrate_book_identity_missing")?,
    )
    .map_err(|_| "host_migrate_book_identity_invalid")?;
    let binding = super::book_binding(live).map_err(|_| "host_migrate_book_binding_invalid")?;
    // The renamed binary must open the live book under the rewritten config.
    if anchor["binding"] != binding {
        return Err("host_migrate_book_binding_mismatch");
    }
    let body = super::open_book_immutable(&live.directory.join("paper.sqlite"))
        .ok()
        .and_then(|db| {
            db.query_row("SELECT body FROM state WHERE id=1", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
        })
        .and_then(|body| serde_json::from_str::<nivra_core::execution::Portfolio>(&body).ok());
    let Some(p) = body else {
        return Ok((
            json!({"session":live.session.to_string(),"binding":"matches","state":"unreadable"}),
            "inspect_book_readiness",
        ));
    };
    let next = if p.halted {
        "planner_recover_required"
    } else if p.closed {
        "start_service_successor_binds_new_policy"
    } else {
        "config_apply_then_resume"
    };
    Ok((
        json!({"session":live.session.to_string(),"binding":"matches","closed":p.closed,
            "stopped":p.stopped,"halted":p.halted,"open_positions":p.positions.len()}),
        next,
    ))
}

fn plan(home: &Path, stage: &Path) -> R<Plan> {
    let release = StagedRelease::read(stage)?;
    if release.tag() != format!("v{}", env!("CARGO_PKG_VERSION")) {
        return Err("host_migrate_release_version_mismatch");
    }
    let config = home.join(CONFIG_DIR);
    let updater_path = config.join("updater.json");
    let updater = parse(&read(&updater_path)?)?;
    let paper_path = PathBuf::from(
        string_at(&updater, "paper_config").ok_or("host_migrate_updater_paper_config_missing")?,
    );
    // A staged second updater config must name the same paper config.
    if let Ok(bytes) = read(&config.join("updater-nivra.json"))
        && string_at(&parse(&bytes)?, "paper_config") != paper_path.to_str()
    {
        return Err("host_migrate_updater_configs_disagree");
    }
    let mut fresh = json!({"remote":update::CANONICAL_REMOTE,"paper_config":paper_path});
    if let Some(webhook) = updater.get("alert_webhook_file") {
        fresh["alert_webhook_file"] = webhook.clone();
    }

    let mut rewrites = Vec::new();
    let paper_step = rewrite(home, &paper_path, Kind::Paper, None)?;
    let paper_bytes = match &paper_step {
        Some(Step::Rewrite { after, .. }) => after.clone(),
        _ => read(&paper_path)?,
    };
    rewrites.extend(paper_step);
    let paper: Config =
        serde_json::from_slice(&paper_bytes).map_err(|_| "host_migrate_paper_config_invalid")?;
    let planner_path = paper
        .planner_config
        .clone()
        .ok_or("host_migrate_planner_config_required")?;
    let planner_value = parse(&read(&planner_path)?)?;
    rewrites.extend(rewrite(home, &planner_path, Kind::Planner, None)?);
    if let Some(profile) = string_at(&planner_value, "experiment_profile") {
        rewrites.extend(rewrite(home, Path::new(profile), Kind::Profile, None)?);
    }
    let mut models = Vec::new();
    model_configs(&planner_value, &mut models);
    for model in models {
        let kind = if parse(&read(&model)?)?.get("api_key_file").is_some() {
            Kind::Fallback
        } else {
            Kind::Model
        };
        rewrites.extend(rewrite(home, &model, kind, None)?);
    }
    rewrites.extend(rewrite(home, &updater_path, Kind::Updater, Some(fresh))?);

    let live = rollover::current(&paper).map_err(|_| "host_migrate_session_unavailable")?;
    let (book, next) = book_facts(&live)?;
    for path in lock_paths(home, &paper, &live)? {
        lock_free(&path)?;
    }
    let (old_state, new_state) = RENAMES[1];
    for dir in [home.join(old_state), home.join(new_state)] {
        if UPDATER_REVIEW.iter().any(|f| dir.join(f).exists()) {
            return Err("host_migrate_update_transaction_requires_review");
        }
    }

    let mut steps = Vec::new();
    let releases = home.join("nivra-releases");
    let target = release.directory(&releases);
    if !release.installed_at(&target) {
        if target.exists() {
            return Err("host_migrate_release_directory_conflict");
        }
        steps.push(Step::Install(target.clone()));
    }
    for (old, new) in RENAMES {
        let (from, to) = (home.join(old), home.join(new));
        match (
            fs::symlink_metadata(&from).is_ok(),
            fs::symlink_metadata(&to).is_ok(),
        ) {
            (true, true) => return Err("host_migrate_rename_conflict"),
            (true, false) => steps.push(Step::Rename { from, to }),
            _ => {}
        }
    }
    let current = home.join("nivra-current");
    match fs::canonicalize(&current) {
        Ok(existing) if existing == target => {}
        Ok(_) => return Err("host_migrate_current_conflict"),
        Err(_) if fs::symlink_metadata(&current).is_ok() => {
            return Err("host_migrate_current_conflict");
        }
        Err(_) => steps.push(Step::Link {
            path: current,
            target: target.clone(),
        }),
    }
    let previous = home.join("nivra-previous");
    if fs::symlink_metadata(&previous).is_err()
        && let Ok(old) = fs::canonicalize(home.join(OLD_CURRENT))
    {
        steps.push(Step::Link {
            path: previous,
            target: old,
        });
    }
    let units = home.join(".config/systemd/user");
    for (name, template) in UNITS {
        let path = units.join(name);
        let bytes = render(home, &paper_path, &updater_path, template)?;
        match fs::read(&path) {
            Ok(existing) if existing == bytes => {}
            Ok(_) => return Err("host_migrate_unit_conflict"),
            Err(_) => steps.push(Step::Unit { path, bytes }),
        }
    }
    // Config switches come last: until then every original is untouched.
    steps.extend(rewrites);
    Ok(Plan {
        steps,
        release,
        paper,
        book,
        next,
    })
}

fn write_private(path: &Path, bytes: &[u8], mode: u32, create_new: bool) -> R<()> {
    let mut options = fs::OpenOptions::new();
    options
        .write(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "host_migrate_write_failed")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "host_migrate_write_failed")
}

fn sync_parent(path: &Path) -> R<()> {
    fs::File::open(path.parent().ok_or("host_migrate_write_failed")?)
        .and_then(|dir| dir.sync_all())
        .map_err(|_| "host_migrate_write_failed")
}

fn apply(home: &Path, plan: &Plan) -> R<()> {
    let live = rollover::current(&plan.paper).map_err(|_| "host_migrate_session_unavailable")?;
    let _locks = lock_paths(home, &plan.paper, &live)?
        .iter()
        .filter(|path| path.parent().is_some_and(Path::exists))
        .map(|path| hold(path))
        .collect::<R<Vec<_>>>()?;
    // Stage every config change before any original moves.
    for step in &plan.steps {
        if let Step::Rewrite {
            path,
            before,
            after,
            ..
        } = step
        {
            let backup = sibling(path, BACKUP_SUFFIX);
            if fs::symlink_metadata(&backup).is_err() {
                write_private(&backup, before, 0o600, true)?;
            }
            write_private(&sibling(path, NEXT_SUFFIX), after, 0o600, false)?;
        }
    }
    for step in &plan.steps {
        match step {
            Step::Install(dir) => {
                let releases = dir
                    .parent()
                    .ok_or("host_migrate_release_directory_conflict")?;
                fs::create_dir_all(releases).map_err(|_| "host_migrate_write_failed")?;
                if plan.release.install(releases)? != *dir {
                    return Err("host_migrate_release_directory_conflict");
                }
            }
            Step::Rename { from, to } => {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(|_| "host_migrate_write_failed")?;
                }
                fs::rename(from, to).map_err(|_| "host_migrate_rename_failed")?;
                sync_parent(to)?;
            }
            Step::Link { path, target } => update::swap(path, target)?,
            Step::Unit { path, bytes } => {
                fs::create_dir_all(path.parent().ok_or("host_migrate_write_failed")?)
                    .map_err(|_| "host_migrate_write_failed")?;
                write_private(path, bytes, 0o644, true)?;
            }
            Step::Rewrite { .. } => {}
        }
    }
    for step in &plan.steps {
        if let Step::Rewrite { path, .. } = step {
            fs::rename(sibling(path, NEXT_SUFFIX), path)
                .map_err(|_| "host_migrate_switch_failed")?;
            sync_parent(path)?;
        }
    }
    Ok(())
}

/// Plan (and with `apply`, perform) the migration at `now`.
pub(crate) fn run(
    home: &Path,
    stage: &Path,
    apply_changes: bool,
    now: DateTime<FixedOffset>,
) -> R<Value> {
    if !home.is_absolute() {
        return Err("host_migrate_absolute_home_required");
    }
    let owner = fs::metadata("/proc/self")
        .map_err(|_| "host_migrate_owner_unavailable")?
        .uid();
    if fs::metadata(home)
        .map_err(|_| "host_migrate_home_unavailable")?
        .uid()
        != owner
    {
        return Err("host_migrate_run_as_home_owner");
    }
    let ist = now.with_timezone(&FixedOffset::east_opt(19800).expect("IST"));
    if update::market_open(ist) {
        return Err("host_migrate_activation_blackout");
    }
    let plan = plan(home, stage)?;
    let status = if plan.steps.is_empty() {
        "already_migrated"
    } else if apply_changes {
        apply(home, &plan)?;
        "applied"
    } else {
        "dry_run"
    };
    Ok(json!({
        "status": status,
        "paper_only": true,
        "live_enabled": super::LIVE_ENABLED,
        "release": plan.release.tag(),
        "steps": plan.steps.iter().map(Step::report).collect::<Vec<_>>(),
        "kept": [home.join(DATA_DIR), home.join(CONFIG_DIR), home.join(OLD_CURRENT), home.join("ind-sadr-rust-releases")],
        "book": plan.book,
        "operator_next": plan.next,
    }))
}

pub(crate) fn entry(home: Option<&Path>, release: &Path, apply_changes: bool) -> ExitCode {
    let home = home
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    let result = home
        .ok_or("host_migrate_home_unavailable")
        .and_then(|home| {
            run(
                &home,
                release,
                apply_changes,
                DateTime::from_timestamp_millis(super::millis())
                    .expect("system clock range")
                    .fixed_offset(),
            )
        });
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(code) => {
            eprintln!("host_migrate_refused: {code}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
