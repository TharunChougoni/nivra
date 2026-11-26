use super::{Config, Result, failure_code, private};
use clap::{Args, Subcommand};
use nivra_store::market_archive;
use std::{path::PathBuf, process::ExitCode};

// The controller and history backfill will call these when archive capture is wired.
#[allow(dead_code)]
pub(super) mod normalize;

#[derive(Args)]
pub(crate) struct Options {
    /// Paper configuration containing market_archive.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Create the versioned schema. Safe to repeat.
    Init(Options),
    /// Print the schema version and table counts.
    Check(Options),
}

fn run(command: Command) -> Result<()> {
    let (options, init) = match command {
        Command::Init(options) => (options, true),
        Command::Check(options) => (options, false),
    };
    let config =
        crate::read_bounded(&options.config, 32768).map_err(|_| "archive_config_unreadable")?;
    let config: Config = serde_json::from_slice(&config).map_err(|_| "archive_config_invalid")?;
    let archive = config.market_archive.ok_or("archive_config_missing")?;
    archive.validate()?;
    let secret = private(&archive.password_file).map_err(|_| "archive_password_file_invalid")?;
    let password = secret.trim_end_matches(['\r', '\n']);
    if password.is_empty() || password.contains(['\r', '\n', '\0']) {
        return Err("archive_password_invalid".into());
    }
    let mut db = market_archive::connect(&archive.url, password)?;
    if init {
        market_archive::init(&mut db)?;
        println!("archive_initialized");
    } else {
        println!(
            "{}",
            serde_json::to_string(&market_archive::check(&mut db)?)?
        );
    }
    Ok(())
}

pub(crate) fn entry(command: Command) -> ExitCode {
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let code = failure_code(error.as_ref());
            if code.starts_with("archive_") {
                eprintln!("{code}");
            } else {
                eprintln!("archive_request_failed");
            }
            ExitCode::FAILURE
        }
    }
}
