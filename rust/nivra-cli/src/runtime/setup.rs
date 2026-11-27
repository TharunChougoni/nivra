//! Explicit fresh-account preparation. Original state and shared quotas stay intact.
use super::*;
use nivra_core::finance::Amount;
use serde_json::{Value, json};
use std::os::unix::fs::DirBuilderExt;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Private paper base config with identity, fee, quota and data bindings.
    #[arg(long)]
    paper: PathBuf,
    /// Application pi settings JSON with mode "paper" and a decision lane.
    #[arg(long)]
    agent_settings: PathBuf,
    /// New private directory. Must not already exist.
    #[arg(long)]
    config_directory: PathBuf,
    /// New private state root. Must not already exist.
    #[arg(long)]
    state_directory: PathBuf,
    /// Capital of the cash-equity strategy sleeve.
    #[arg(long)]
    cash_capital: Amount,
    /// Capital of the Nifty options sleeve. Zero leaves options unfunded.
    #[arg(long)]
    options_capital: Amount,
    /// A fresh account is a separate explicit disposition, never an upgrade/reset.
    #[arg(long)]
    confirm_fresh_paper: bool,
}

// Resolve existing ancestors and reject aliases before creating any output.
fn new_directory(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        || path.symlink_metadata().is_ok()
    {
        return Err("setup_new_absolute_directory_required".into());
    }
    let parent = fs::canonicalize(path.parent().ok_or("setup_parent_required")?)?;
    Ok(parent.join(path.file_name().ok_or("setup_name_required")?))
}

fn source_namespace(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    if absolute
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err("setup_source_namespace_invalid".into());
    }
    for ancestor in absolute.ancestors() {
        match ancestor.symlink_metadata() {
            Ok(_) => {
                return Ok(fs::canonicalize(ancestor)?.join(absolute.strip_prefix(ancestor)?));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("setup_source_namespace_invalid".into())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn prepare(args: &Args) -> Result<()> {
    let capital = args.cash_capital.checked_add(args.options_capital)?;
    if !args.confirm_fresh_paper
        || args.cash_capital < Amount::ZERO
        || args.options_capital < Amount::ZERO
        || capital <= Amount::ZERO
    {
        return Err("setup_explicit_fresh_paper_and_positive_capital_required".into());
    }
    let source_bytes = private(&args.paper)?;
    let source: Config = serde_json::from_str(&source_bytes)?;
    source.validate()?;
    if source.next_sessions(source.session)? != (source.session, source.previous_session) {
        return Err("setup_verified_session_pair_required".into());
    }
    let agent_bytes = crate::read_bounded(&args.agent_settings, 65536)
        .map_err(|_| "setup_agent_settings_unreadable")?;
    let agent_settings: agent::AgentSettings =
        serde_json::from_slice(&agent_bytes).map_err(|_| "setup_agent_settings_invalid")?;
    if agent_settings.mode != agent::AgentMode::Paper {
        return Err("setup_pi_paper_mode_required".into());
    }
    let config = new_directory(&args.config_directory)?;
    let state = new_directory(&args.state_directory)?;
    let original_book = source_namespace(&source.directory)?;
    let original_root = source
        .sessions_root
        .as_ref()
        .map(|path| source_namespace(path))
        .transpose()?;
    let original_config = fs::canonicalize(args.paper.parent().ok_or("setup_parent_required")?)?;
    for output in [&config, &state] {
        for original in [
            Some(&original_book),
            original_root.as_ref(),
            Some(&original_config),
        ]
        .into_iter()
        .flatten()
        {
            if output.starts_with(original) || original.starts_with(output) {
                return Err("setup_original_namespace_overlap".into());
            }
        }
    }
    if config.starts_with(&state) || state.starts_with(&config) {
        return Err("setup_output_namespace_overlap".into());
    }
    // Start from the current schema, never deserialize/re-serialize an old planner.
    let mut paper: Value = serde_json::from_str(include_str!(
        "../../../../packaging/rust/paper-current.example.json"
    ))?;
    let original: Value = serde_json::from_str(&source_bytes)?;
    for key in [
        "account",
        "session",
        "previous_session",
        "calendar_source",
        "token_file",
        "dashboard_token_file",
        "read_journal",
        "read_scope",
        "read_journal_device",
        "read_journal_inode",
        "paper_costs",
        "market_archive",
        "dashboard_public_host",
        "alert_webhook_file",
    ] {
        if let Some(value) = original.get(key) {
            paper[key] = value.clone();
        } else {
            paper
                .as_object_mut()
                .ok_or("setup_template_invalid")?
                .remove(key);
        }
    }
    paper["capital"] = json!(capital.to_string());
    paper["sleeves"]["cash"]["capital"] = json!(args.cash_capital.to_string());
    paper["sleeves"]["options"]["capital"] = json!(args.options_capital.to_string());
    paper["directory"] = json!(state.join(source.session.to_string()));
    paper["sessions_root"] = json!(state.join("sessions"));
    paper["planner_config"] = json!(config.join("planner.json"));
    let generated: Config = serde_json::from_value(paper.clone())?;
    generated.validate()?;
    let planner = serde_json::to_vec_pretty(&json!({
        "enabled": true,
        "model_led": true,
        "agent": agent_settings,
        "discovery": {
            "max_scan_age_ms": 180_000,
            "max_prospects": 200,
            "market_feed": {"mode": "ltpc"}
        }
    }))?;
    let loaded = planner::PlannerConfig::parse(&planner)?;

    // Exclusive creation: failures retain prepared artifacts, never replace/retry them.
    fs::DirBuilder::new().mode(0o700).create(&config)?;
    fs::DirBuilder::new().mode(0o700).create(&state)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(generated.directory)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(state.join("sessions"))?;
    write_new(&config.join("planner.json"), &planner)?;
    write_new(
        &config.join("paper.json"),
        &serde_json::to_vec_pretty(&paper)?,
    )?;
    write_new(
        &config.join("setup.json"),
        &serde_json::to_vec_pretty(&json!({
            "schema_version":1,"disposition":"fresh_paper_originals_preserved",
            "version":env!("CARGO_PKG_VERSION"),"source_config_sha256":digest(source_bytes.as_bytes()),
            "paper_config_sha256":digest(&serde_json::to_vec_pretty(&paper)?),
            "planner_policy_sha256":digest(&planner::policy_bytes(&loaded)?),
            "capital":capital,"session":source.session,"activated":false,
            "shared_read_journal_preserved":true,"credential_files_modified":false
        }))?,
    )?;
    fs::File::open(&config)?.sync_all()?;
    fs::File::open(&state)?.sync_all()?;
    Ok(())
}

pub(crate) fn entry(args: Args) -> ExitCode {
    match prepare(&args) {
        Ok(()) => {
            println!("fresh_paper_prepared_no_activation");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!(
                "setup_refused: verify explicit fresh-paper mandate, current inputs and unused private paths; original state preserved"
            );
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
pub(super) fn exercise() {
    let root = tempfile::tempdir().unwrap();
    let original = root.path().join("original");
    fs::create_dir(&original).unwrap();
    let book = original.join("book");
    fs::create_dir(&book).unwrap();
    fs::write(book.join("paper.sqlite"), b"original-stopped-book").unwrap();
    let source = super::tests::config(&book);
    let source_bytes = serde_json::to_vec_pretty(&source).unwrap();
    let paper = original.join("paper.json");
    write_new(&paper, &source_bytes).unwrap();
    let agent_settings = original.join("agent.json");
    let mut settings: Value = serde_json::from_str(include_str!(
        "../../../../packaging/rust/agent-settings.example.json"
    ))
    .unwrap();
    settings["agent_dir"] = json!(original.join("unopened-pi"));
    settings["mode"] = json!("shadow");
    write_new(&agent_settings, &serde_json::to_vec(&settings).unwrap()).unwrap();
    let mut args = Args {
        paper: paper.clone(),
        agent_settings,
        config_directory: root.path().join("config"),
        state_directory: root.path().join("state"),
        cash_capital: Amount::parse("5000").unwrap(),
        options_capital: Amount::parse("5000").unwrap(),
        confirm_fresh_paper: false,
    };
    assert!(prepare(&args).is_err());
    assert!(!args.config_directory.exists());
    args.confirm_fresh_paper = true;
    assert!(prepare(&args).is_err());
    assert!(!args.config_directory.exists());
    settings["mode"] = json!("paper");
    fs::write(&args.agent_settings, serde_json::to_vec(&settings).unwrap()).unwrap();

    args.state_directory = book.join("replacement");
    assert!(prepare(&args).is_err());
    assert!(!args.config_directory.exists());
    args.state_directory = root.path().join("state");
    let mut bindings: Value = serde_json::from_slice(&source_bytes).unwrap();
    let first_book = original.join("unused").join("book");
    let first_sessions = original.join("unused").join("sessions");
    bindings["directory"] = json!(first_book);
    bindings["sessions_root"] = json!(first_sessions);
    args.paper = original.join("base.json");
    write_new(&args.paper, &serde_json::to_vec(&bindings).unwrap()).unwrap();
    prepare(&args).unwrap();
    assert!(!first_book.exists());
    assert!(!first_sessions.exists());
    let generated: Value =
        serde_json::from_slice(&fs::read(args.config_directory.join("paper.json")).unwrap())
            .unwrap();
    assert_eq!(generated["capital"], "10000");
    assert_eq!(generated["sleeves"]["options"]["capital"], "5000");
    assert_eq!(generated["read_journal"], json!(source.read_journal));
    assert_eq!(generated["read_journal_inode"], source.read_journal_inode);
    assert_eq!(generated["read_journal_device"], source.read_journal_device);
    assert_eq!(generated["paper_costs"], json!(source.paper_costs));
    assert!(generated.get("prior_config").is_none());
    let planner =
        planner::PlannerConfig::load(&args.config_directory.join("planner.json")).unwrap();
    planner.validate_decision_lane().unwrap();
    assert!(planner.uses_pi_paper());
    assert!(!args.config_directory.join("prompts").exists());
    assert!(!original.join("unopened-pi").exists());
    assert_eq!(
        serde_json::to_value(planner.agent.unwrap()).unwrap(),
        settings
    );
    let receipt: Value =
        serde_json::from_slice(&fs::read(args.config_directory.join("setup.json")).unwrap())
            .unwrap();
    assert_eq!(receipt["activated"], false);
    assert!(
        !Path::new(generated["directory"].as_str().unwrap())
            .join("paper.sqlite")
            .exists()
    );
    assert_eq!(
        fs::metadata(args.config_directory.join("paper.json"))
            .unwrap()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        prepare(&args).is_err(),
        "a fresh setup must never overwrite an earlier attempt"
    );
    assert_eq!(fs::read(paper).unwrap(), source_bytes);
    assert_eq!(
        fs::read(book.join("paper.sqlite")).unwrap(),
        b"original-stopped-book"
    );
}
