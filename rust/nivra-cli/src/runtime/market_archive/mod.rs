use super::{Config, MarketArchiveSettings, Result, failure_code, private};
use clap::{Args, Subcommand};
use nivra_store::market_archive::{self, ArchiveClient};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

mod book;
#[cfg(test)]
pub(super) use book::import as import_book;
pub(super) mod capture;
mod history;
#[cfg(test)]
pub(super) use history::{ImportSummary, import};

pub(super) mod normalize;

#[derive(Args)]
pub(crate) struct Options {
    /// Paper configuration containing market_archive.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Create or upgrade the archive schema. Safe to repeat.
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
    /// Import receipts from a closed, checkpointed paper book.
    ImportBook {
        #[command(flatten)]
        options: Options,
        book_dir: PathBuf,
        /// Replace verified legacy quote bodies with archive references.
        #[arg(long)]
        rewrite: bool,
    },
}

fn read_config(path: &Path) -> Result<Config> {
    let config = crate::read_bounded(path, 32768).map_err(|_| "archive_config_unreadable")?;
    serde_json::from_slice(&config).map_err(|_| "archive_config_invalid".into())
}

pub(super) fn open(config: &Config) -> Result<ArchiveClient> {
    let archive = config
        .market_archive
        .as_ref()
        .ok_or("archive_config_missing")?;
    archive.validate()?;
    connect(archive)
}

pub(crate) fn connect_from_config(path: &Path) -> Result<ArchiveClient> {
    open(&read_config(path)?)
}

pub(crate) fn connect(archive: &MarketArchiveSettings) -> Result<ArchiveClient> {
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
            market_archive::init(&mut connect_from_config(&options.config)?)?;
            println!("archive_initialized");
        }
        Command::Check(options) => {
            let mut db = connect_from_config(&options.config)?;
            println!(
                "{}",
                serde_json::to_string(&market_archive::check(&mut db)?)?
            );
        }
        Command::ImportHistory { options, directory } => {
            let mut db = connect_from_config(&options.config)?;
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
        Command::ImportBook {
            options,
            book_dir,
            rewrite,
        } => {
            let mut db = connect_from_config(&options.config)?;
            println!("{}", book::import(&mut db, &book_dir, rewrite)?);
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

pub(crate) fn from_paise(paise: i64) -> Result<nivra_core::finance::Amount> {
    use nivra_core::finance::Amount;
    Ok(Amount::from_integer(paise).checked_div(Amount::from_integer(100))?)
}

/// Settings for a new, empty database on the disposable TimescaleDB that
/// `NIVRA_ARCHIVE_TEST_URL` names, so ignored tests never see each other's rows.
#[cfg(test)]
pub(crate) fn test_settings(dir: &Path) -> Result<MarketArchiveSettings> {
    use std::{
        io::Write,
        os::unix::fs::OpenOptionsExt,
        sync::atomic::{AtomicU32, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let url = std::env::var("NIVRA_ARCHIVE_TEST_URL")?;
    let (scheme, rest) = url.split_once("://").ok_or("fixture_url")?;
    let (auth, location) = rest.rsplit_once('@').ok_or("fixture_url")?;
    let (user, password) = auth.split_once(':').ok_or("fixture_url")?;
    let host = location.split_once('/').map_or(location, |(host, _)| host);
    let database = format!(
        "nivra_test_{}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_micros()
    );
    market_archive::connect(&format!("{scheme}://{user}@{host}/postgres"), password)?
        .batch_execute(&format!("CREATE DATABASE {database}"))?;
    let password_file = dir.join("archive-password");
    std::fs::OpenOptions::new()
        .create_new(true)
        .mode(0o600)
        .write(true)
        .open(&password_file)?
        .write_all(password.as_bytes())?;
    Ok(MarketArchiveSettings {
        url: format!("{scheme}://{user}@{host}/{database}"),
        password_file,
    })
}
