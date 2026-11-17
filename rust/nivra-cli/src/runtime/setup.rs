//! Explicit fresh-account preparation. Original state and shared quotas stay intact.
use super::*;
use nivra_core::finance::Amount;
use serde_json::{Value, json};
use std::os::unix::fs::DirBuilderExt;

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Original application paper config: identities, fees, quota and data bindings only.
    #[arg(long)]
    paper: PathBuf,
    /// Application model configs, explicitly selected for the current template's models.
    #[arg(long)]
    digest_model_config: PathBuf,
    #[arg(long)]
    owner_model_config: PathBuf,
    /// New private directory. Must not already exist.
    #[arg(long)]
    config_directory: PathBuf,
    /// New private state root. Must not already exist.
    #[arg(long)]
    state_directory: PathBuf,
    #[arg(long)]
    capital: Amount,
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
    if !args.confirm_fresh_paper || args.capital <= Amount::ZERO {
        return Err("setup_explicit_fresh_paper_and_positive_capital_required".into());
    }
    let source_bytes = private(&args.paper)?;
    let source: Config = serde_json::from_str(&source_bytes)?;
    source.validate()?;
    if source.next_sessions(source.session)? != (source.session, source.previous_session) {
        return Err("setup_verified_session_pair_required".into());
    }
    for path in [&args.digest_model_config, &args.owner_model_config] {
        if !path.is_absolute() {
            return Err("setup_absolute_application_model_config_required".into());
        }
    }
    let config = new_directory(&args.config_directory)?;
    let state = new_directory(&args.state_directory)?;
    let original_book = fs::canonicalize(&source.directory)?;
    let original_root = source
        .sessions_root
        .as_ref()
        .map(fs::canonicalize)
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
        "history_directory",
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
    paper["capital"] = json!(args.capital.to_string());
    paper["directory"] = json!(state.join(source.session.to_string()));
    paper["sessions_root"] = json!(state.join("sessions"));
    paper["planner_config"] = json!(config.join("planner.json"));
    let generated: Config = serde_json::from_value(paper.clone())?;
    generated.validate()?;
    let mut planner: Value = serde_json::from_str(include_str!(
        "../../../../packaging/rust/planner-current.example.json"
    ))?;
    planner["digest"]["model_config"] = json!(args.digest_model_config);
    planner["orchestrator"]["model"]["model_config"] = json!(args.owner_model_config);
    // No unreviewed alternative profile or automatic provider fallback.
    planner["orchestrator"]
        .as_object_mut()
        .ok_or("setup_template_invalid")?
        .remove("model_profiles");
    planner["orchestrator"]
        .as_object_mut()
        .ok_or("setup_template_invalid")?
        .remove("admitted_models");
    planner["orchestrator"]["system_prompt"] = json!(config.join("prompts/orchestrator.md"));
    planner["orchestrator"]["triggers"]["supervisor"]["prompt"] =
        json!(config.join("prompts/supervisor.md"));

    // Exclusive creation: failures retain prepared artifacts, never replace/retry them.
    fs::DirBuilder::new().mode(0o700).create(&config)?;
    fs::DirBuilder::new().mode(0o700).create(&state)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(generated.directory)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(state.join("sessions"))?;
    let prompts = config.join("prompts");
    fs::DirBuilder::new().mode(0o700).create(&prompts)?;
    macro_rules! prompt {
        ($name:literal) => {
            write_new(
                &prompts.join($name),
                include_bytes!(concat!("../../../../packaging/rust/prompts/", $name)),
            )?;
        };
    }
    prompt!("objective.md");
    prompt!("data.md");
    prompt!("data_owner.md");
    prompt!("decision.md");
    prompt!("profit_under_uncertainty.md");
    prompt!("decision_evidence_digest.md");
    prompt!("supervisor.md");
    prompt!("orchestrator.md");
    prompt!("review.md");
    prompt!("scorer.md");
    write_new(
        &config.join("planner.json"),
        &serde_json::to_vec_pretty(&planner)?,
    )?;
    let loaded = planner::PlannerConfig::load(&config.join("planner.json"))?;
    loaded.validate_decision_owner()?;
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
            "capital":args.capital,"session":source.session,"activated":false,
            "shared_read_journal_preserved":true,"credential_files_modified":false
        }))?,
    )?;
    fs::File::open(&prompts)?.sync_all()?;
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
    let mut args = Args {
        paper: paper.clone(),
        digest_model_config: original.join("unopened-digest.json"),
        owner_model_config: original.join("unopened-owner.json"),
        config_directory: root.path().join("config"),
        state_directory: root.path().join("state"),
        capital: Amount::parse("5000").unwrap(),
        confirm_fresh_paper: false,
    };
    assert!(prepare(&args).is_err());
    assert!(!args.config_directory.exists());
    args.confirm_fresh_paper = true;
    args.state_directory = book.join("replacement");
    assert!(prepare(&args).is_err());
    assert!(!args.config_directory.exists());
    args.state_directory = root.path().join("state");
    prepare(&args).unwrap();
    let generated: Value =
        serde_json::from_slice(&fs::read(args.config_directory.join("paper.json")).unwrap())
            .unwrap();
    assert_eq!(generated["capital"], "5000");
    assert_eq!(generated["read_journal"], json!(source.read_journal));
    assert_eq!(generated["read_journal_inode"], source.read_journal_inode);
    assert_eq!(generated["read_journal_device"], source.read_journal_device);
    assert_eq!(generated["paper_costs"], json!(source.paper_costs));
    assert!(generated.get("prior_config").is_none());
    let planner =
        planner::PlannerConfig::load(&args.config_directory.join("planner.json")).unwrap();
    planner.validate_decision_owner().unwrap();
    assert_eq!(
        planner.digest.model_config,
        Some(args.digest_model_config.clone())
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
