use super::{Config, Result, failure_code, private};
use clap::{Args, Subcommand};
use nivra_store::market_archive;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

mod history;
#[cfg(test)]
pub(super) use history::{ImportSummary, import};

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
    /// Load legacy history receipts and HTTP 400 exclusions.
    ImportHistory {
        #[command(flatten)]
        options: Options,
        directory: PathBuf,
    },
    /// Write bounded daily datasets for the offline analysis runtime.
    ExportDailyHistory {
        #[command(flatten)]
        options: Options,
        directory: PathBuf,
        #[arg(long)]
        start: chrono::NaiveDate,
        #[arg(long)]
        end: chrono::NaiveDate,
    },
}

fn read_config(path: &Path) -> Result<Config> {
    let config = crate::read_bounded(path, 32768).map_err(|_| "archive_config_unreadable")?;
    serde_json::from_slice(&config).map_err(|_| "archive_config_invalid".into())
}

pub(super) fn open(config: &Config) -> Result<market_archive::Client> {
    let archive = config
        .market_archive
        .as_ref()
        .ok_or("archive_config_missing")?;
    archive.validate()?;
    let secret = private(&archive.password_file).map_err(|_| "archive_password_file_invalid")?;
    let password = secret.trim_end_matches(['\r', '\n']);
    if password.is_empty() || password.contains(['\r', '\n', '\0']) {
        return Err("archive_password_invalid".into());
    }
    market_archive::connect(&archive.url, password)
}

fn run(command: Command) -> Result<()> {
    match command {
        Command::Init(options) => {
            market_archive::init(&mut open(&read_config(&options.config)?)?)?;
            println!("archive_initialized");
        }
        Command::Check(options) => {
            let mut db = open(&read_config(&options.config)?)?;
            let counts = market_archive::check(&mut db)?;
            println!("{}", serde_json::to_string(&counts)?);
        }
        Command::ImportHistory { options, directory } => {
            let mut db = open(&read_config(&options.config)?)?;
            let summary = history::import(&mut db, &directory)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::ExportDailyHistory {
            options,
            directory,
            start,
            end,
        } => {
            let config = read_config(&options.config)?;
            history::export(&config, &directory, start, end)?;
        }
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

pub(super) fn from_paise(paise: i64) -> Result<nivra_core::finance::Amount> {
    use nivra_core::finance::Amount;
    Ok(Amount::from_integer(paise).checked_div(Amount::from_integer(100))?)
}
