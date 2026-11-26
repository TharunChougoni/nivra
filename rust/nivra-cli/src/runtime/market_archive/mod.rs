use super::{Config, MarketArchiveSettings, Result, failure_code, private};
use clap::{Args, Subcommand};
use nivra_store::market_archive::{self, ArchiveClient};
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
};

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

pub(super) fn connect(archive: &MarketArchiveSettings) -> Result<ArchiveClient> {
    let secret = private(&archive.password_file).map_err(|_| "archive_password_file_invalid")?;
    let password = secret.trim_end_matches(['\r', '\n']);
    if password.is_empty() || password.contains(['\r', '\n', '\0']) {
        return Err("archive_password_invalid".into());
    }
    market_archive::connect(&archive.url, password)
}

pub(super) fn import_book(db: &mut ArchiveClient, path: &Path) -> Result<serde_json::Value> {
    let book = super::open_book_immutable(&path.join("paper.sqlite"))?;
    let (binding, state): (String, String) =
        book.query_row("SELECT binding, body FROM state WHERE id=1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
    let portfolio: nivra_core::execution::Portfolio = serde_json::from_str(&state)?;
    if !portfolio.closed {
        return Err("archive_import_requires_closed_book".into());
    }
    let mut stmt = book.prepare(
        "SELECT id, at, receipt, body FROM ticks
         WHERE event IN ('wide_quotes','scanner_metadata','research_quotes') ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    let mut counts = [0, 0, 0];
    while let Some(row) = rows.next()? {
        let raw: String = row.get(3)?;
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        if value["kind"] == "compacted_quote_receipt" {
            counts[2] += 1;
            continue;
        }
        let receipt = row
            .get::<_, Option<i64>>(2)?
            .ok_or("archive_receipt_time_invalid")?;
        let raw = nivra_store::market_archive::QuoteBodies::read(db, raw, Some(receipt))?;
        let batch = normalize::capture_quotes(&raw)?;
        if batch.receipt.received_ms != receipt {
            return Err("archive_receipt_conflict".into());
        }
        let inserted =
            market_archive::import_quotes(db, &batch, &binding, row.get(0)?, row.get(1)?)?;
        counts[usize::from(!inserted)] += 1;
    }
    Ok(serde_json::json!({"inserted":counts[0], "verified":counts[1], "compacted":counts[2]}))
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
        Command::ImportBook { options, book_dir } => {
            let mut db = connect_from_config(&options.config)?;
            println!("{}", import_book(&mut db, &book_dir)?);
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
