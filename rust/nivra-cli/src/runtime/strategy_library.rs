//! The action-strategy library: accepted `strategy_actions_v1` versions,
//! kept beside the dated session books so no session archive holds them.
//!
//! ```text
//! <sessions_root>/strategy-library/
//!   entries/<strategy_id>.json   accepted replay for each (source, params)
//!   sources/<source_sha256>.py   content-addressed strategy source
//!   reports/<report_sha256>.json content-addressed acceptance replay
//!   controls.json                owner pin/disable/rollback log, append-only
//! ```
//!
//! Every file is written to a temporary name, synced and then linked or
//! renamed into place, so a crash leaves either the old state or the new one.
//! Reacceptance on a new engine or adapter replaces the entry atomically. The
//! old report remains content-addressed, and owner controls keep the same id.
use super::{Config, Result, rollover};
use acceptance::Accepted;
use chrono::NaiveDate;
use fs2::FileExt;
use nivra_cli::canonical_json::{bytes as artifact_bytes, sha256 as artifact_sha256};
use nivra_core::replay::digest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) mod acceptance;

const ENTRY_VERSION: u32 = 1;
const MAX_ENTRY_BYTES: u64 = 65_536;
const MAX_SOURCE_BYTES: u64 = 262_144;
const MAX_CONTROLS_BYTES: u64 = 4 * 1024 * 1024;

/// Who wrote the strategy, where known.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Author {
    pub lane: String,
    pub model: Option<String>,
    pub model_revision: Option<u64>,
    pub turn_id: Option<String>,
    pub session: Option<NaiveDate>,
}

/// One accepted strategy version and its latest build-compatible replay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Entry {
    pub entry_version: u32,
    /// SHA-256 of the canonical `{abi, source_sha256, params}`.
    pub strategy_id: String,
    pub abi: String,
    pub source_sha256: String,
    pub params: Value,
    pub author: Author,
    pub report_sha256: String,
    pub summary: Value,
    pub tape_dates: Vec<String>,
    pub acceptance: EntryAcceptance,
    pub registered_at_ms: i64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryAcceptance {
    pub verdict: String,
    pub thresholds: acceptance::Thresholds,
}

pub(crate) fn strategy_id(source_sha256: &str, params: &Value) -> Result<String> {
    Ok(artifact_sha256(
        &json!({"abi":super::strategy::ABI,"source_sha256":source_sha256,"params":params}),
    )?)
}

/// The build-specific reason an accepted entry cannot run here.
pub(crate) fn build_mismatch(report: &Value) -> Option<&'static str> {
    if report["engine_sha256"].as_str() != Some(&nivra_core::replay::engine_fingerprint()) {
        Some("strategy_engine_changed")
    } else if report["adapter_sha256"].as_str() != Some(&super::strategy::replay::adapter_sha256())
    {
        Some("strategy_adapter_changed")
    } else {
        None
    }
}

/// A control record. `rollback` disables `from` and pins `to`; `lane_select`
/// is the decision lane's strategy choice (`strategy_id: None` stands down)
/// and is always overridden by an owner pin, rollback or disable.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Control {
    Pin {
        strategy_id: String,
    },
    Unpin,
    Disable {
        strategy_id: String,
    },
    Enable {
        strategy_id: String,
    },
    Rollback {
        from: String,
        to: String,
    },
    LaneSelect {
        strategy_id: Option<String>,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ControlRecord {
    pub at_ms: i64,
    pub control: Control,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct LaneSelection {
    pub strategy_id: Option<String>,
    pub reason: String,
    pub at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlLog {
    version: u32,
    controls: Vec<ControlRecord>,
}

/// The library folded with its control log.
#[derive(Debug)]
pub(crate) struct Resolved {
    /// In registration order, oldest first.
    pub entries: Vec<Entry>,
    pub disabled: BTreeSet<String>,
    pub pinned: Option<String>,
    pub lane_selection: Option<LaneSelection>,
    /// The pinned entry when eligible, else the lane's selection when present
    /// (with `None` standing down), else the newest eligible one.
    pub active: Option<String>,
}
impl Resolved {
    pub(crate) fn eligible(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|e| !self.disabled.contains(&e.strategy_id))
    }
    fn status(&self, id: &str) -> &'static str {
        if self.active.as_deref() == Some(id) {
            "active"
        } else if self.disabled.contains(id) {
            "disabled"
        } else {
            "eligible"
        }
    }
    /// The eligible entry registered just before the active one.
    fn previous(&self) -> Option<&Entry> {
        let active = self.active.as_ref()?;
        let eligible: Vec<&Entry> = self.eligible().collect();
        let at = eligible.iter().position(|e| &e.strategy_id == active)?;
        at.checked_sub(1).map(|i| eligible[i])
    }
}

/// Pure fold of the entries and the control log.
pub(crate) fn resolve(mut entries: Vec<Entry>, controls: &[ControlRecord]) -> Resolved {
    entries.sort_by(|a, b| {
        (a.registered_at_ms, &a.strategy_id).cmp(&(b.registered_at_ms, &b.strategy_id))
    });
    let (mut disabled, mut pinned, mut lane_selection) = (BTreeSet::new(), None, None);
    for record in controls {
        match &record.control {
            Control::Pin { strategy_id } => pinned = Some(strategy_id.clone()),
            Control::Unpin => pinned = None,
            Control::Disable { strategy_id } => {
                disabled.insert(strategy_id.clone());
            }
            Control::Enable { strategy_id } => {
                disabled.remove(strategy_id);
            }
            Control::Rollback { from, to } => {
                disabled.insert(from.clone());
                pinned = Some(to.clone());
            }
            Control::LaneSelect {
                strategy_id,
                reason,
                turn_id,
            } => {
                lane_selection = Some(LaneSelection {
                    strategy_id: strategy_id.clone(),
                    reason: reason.clone(),
                    at_ms: record.at_ms,
                    turn_id: turn_id.clone(),
                });
            }
        }
    }
    let eligible =
        |id: &String| !disabled.contains(id) && entries.iter().any(|e| &e.strategy_id == id);
    let newest_eligible = || {
        entries
            .iter()
            .rev()
            .find(|e| !disabled.contains(&e.strategy_id))
            .map(|e| e.strategy_id.clone())
    };
    let active = if let Some(pin) = pinned.clone().filter(eligible) {
        Some(pin)
    } else if let Some(selection) = &lane_selection {
        match &selection.strategy_id {
            Some(id) if eligible(id) => Some(id.clone()),
            Some(_) => newest_eligible(),
            None => None,
        }
    } else {
        newest_eligible()
    };
    Resolved {
        entries,
        disabled,
        pinned,
        lane_selection,
        active,
    }
}

pub(crate) struct Registration {
    pub entry: Entry,
    pub status: RegistrationStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RegistrationStatus {
    Registered,
    AlreadyRegistered,
    Reaccepted,
}

pub(crate) struct Library {
    root: PathBuf,
}

/// The library beside the dated session books.
pub(crate) fn root(c: &Config) -> Result<PathBuf> {
    Ok(rollover::root(c)?.join("strategy-library"))
}

impl Library {
    pub(crate) fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// A replay for this build may replace an older build's acceptance under
    /// the same strategy id. A repeat on the current build keeps the first report.
    pub(crate) fn register(
        &self,
        accepted: &Accepted<'_>,
        author: Author,
        now: i64,
    ) -> Result<Registration> {
        let id = strategy_id(&accepted.source_sha256, &accepted.params)?;
        let entries = self.dir("entries")?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.root.join("registration.lock"))?;
        lock.lock_exclusive()?;
        let _lock = nivra_cli::lock::AdvisoryLock::new(lock);
        let previous = self.get(&id)?;
        if let Some(entry) = &previous
            && (self.entry_build_mismatch(entry)?.is_none()
                || build_mismatch(&serde_json::from_slice(accepted.report)?).is_some())
        {
            return Ok(Registration {
                entry: entry.clone(),
                status: RegistrationStatus::AlreadyRegistered,
            });
        }
        let sources = self.dir("sources")?;
        let reports = self.dir("reports")?;
        write_new(
            &sources,
            &format!("{}.py", accepted.source_sha256),
            accepted.source,
        )?;
        write_new(
            &reports,
            &format!("{}.json", accepted.report_sha256),
            accepted.report,
        )?;
        let entry = Entry {
            entry_version: ENTRY_VERSION,
            strategy_id: id.clone(),
            abi: super::strategy::ABI.into(),
            source_sha256: accepted.source_sha256.clone(),
            params: accepted.params.clone(),
            author,
            report_sha256: accepted.report_sha256.clone(),
            summary: accepted.summary.clone(),
            tape_dates: accepted.tape_dates.clone(),
            acceptance: EntryAcceptance {
                verdict: "accepted".into(),
                thresholds: accepted.thresholds.clone(),
            },
            registered_at_ms: now,
        };
        // The entry is the commit point: its sources and report exist first.
        let name = format!("{id}.json");
        let bytes = artifact_bytes(&entry)?;
        let status = if previous.is_some() {
            let temporary = write_temp(&entries, &name, &bytes)?;
            fs::rename(temporary, entries.join(&name))?;
            fs::File::open(&entries)?.sync_all()?;
            RegistrationStatus::Reaccepted
        } else {
            write_new(&entries, &name, &bytes)?;
            RegistrationStatus::Registered
        };
        Ok(Registration {
            entry: self.get(&id)?.ok_or("strategy_library_entry_invalid")?,
            status,
        })
    }

    pub(crate) fn entry_build_mismatch(&self, entry: &Entry) -> Result<Option<&'static str>> {
        self.report(entry).map(|report| build_mismatch(&report))
    }

    /// Read and verify one entry, its source and its report.
    pub(crate) fn get(&self, id: &str) -> Result<Option<Entry>> {
        if id.len() != 64 || !id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err("strategy_id_invalid".into());
        }
        let path = self.root.join("entries").join(format!("{id}.json"));
        if !path.exists() {
            return Ok(None);
        }
        let invalid = || "strategy_library_entry_invalid";
        let entry: Entry = serde_json::from_slice(
            &crate::read_bounded(&path, MAX_ENTRY_BYTES).map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?;
        if entry.entry_version != ENTRY_VERSION
            || entry.strategy_id != id
            || strategy_id(&entry.source_sha256, &entry.params)? != id
            || digest(&self.source(&entry)?) != entry.source_sha256
            || !self.report_verifies(&entry)
        {
            return Err(invalid().into());
        }
        Ok(Some(entry))
    }

    /// The exact source bytes an entry runs.
    pub(crate) fn source(&self, entry: &Entry) -> Result<Vec<u8>> {
        let path = self
            .root
            .join("sources")
            .join(format!("{}.py", entry.source_sha256));
        Ok(crate::read_bounded(&path, MAX_SOURCE_BYTES)
            .map_err(|_| "strategy_library_source_unreadable")?)
    }

    /// The accepted replay report an entry names, checked against its hash.
    pub(crate) fn report(&self, entry: &Entry) -> Result<Value> {
        let path = self
            .root
            .join("reports")
            .join(format!("{}.json", entry.report_sha256));
        let bytes = crate::read_bounded(&path, 32 * 1024 * 1024)
            .map_err(|_| "strategy_library_report_unreadable")?;
        if digest(&bytes) != entry.report_sha256 {
            return Err("strategy_library_entry_invalid".into());
        }
        Ok(serde_json::from_slice(&bytes).map_err(|_| "strategy_library_entry_invalid")?)
    }

    fn report_verifies(&self, entry: &Entry) -> bool {
        let path = self
            .root
            .join("reports")
            .join(format!("{}.json", entry.report_sha256));
        crate::read_bounded(&path, 32 * 1024 * 1024)
            .is_ok_and(|bytes| digest(&bytes) == entry.report_sha256)
    }

    /// Every entry, verified. Any invalid entry fails the whole read.
    pub(crate) fn entries(&self) -> Result<Vec<Entry>> {
        let dir = self.root.join("entries");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for item in fs::read_dir(dir)? {
            let name = item?.file_name();
            let name = name.to_str().ok_or("strategy_library_entry_invalid")?;
            // Temporary names start with a dot and are never entries.
            if name.starts_with('.') {
                continue;
            }
            let id = name
                .strip_suffix(".json")
                .ok_or("strategy_library_entry_invalid")?;
            entries.push(self.get(id)?.ok_or("strategy_library_entry_invalid")?);
        }
        Ok(entries)
    }

    fn controls(&self) -> Result<Vec<ControlRecord>> {
        let path = self.root.join("controls.json");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let log: ControlLog = serde_json::from_slice(
            &crate::read_bounded(&path, MAX_CONTROLS_BYTES)
                .map_err(|_| "strategy_controls_unreadable")?,
        )
        .map_err(|_| "strategy_controls_invalid")?;
        if log.version != 1 {
            return Err("strategy_controls_invalid".into());
        }
        Ok(log.controls)
    }

    pub(crate) fn resolve(&self) -> Result<Resolved> {
        Ok(resolve(self.entries()?, &self.controls()?))
    }

    /// Append one control under the library lock. Earlier records are kept
    /// byte for byte; the file is replaced atomically.
    pub(crate) fn append(&self, record: ControlRecord) -> Result<Resolved> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(self.root.join("controls.lock"))?;
        lock.lock_exclusive()?;
        let _lock = nivra_cli::lock::AdvisoryLock::new(lock);
        let mut controls = self.controls()?;
        controls.push(record);
        let bytes = serde_json::to_vec_pretty(&ControlLog {
            version: 1,
            controls,
        })?;
        let tmp = write_temp(&self.root, "controls.json", &bytes)?;
        fs::rename(&tmp, self.root.join("controls.json"))?;
        fs::File::open(&self.root)?.sync_all()?;
        self.resolve()
    }

    /// Record the decision lane's strategy choice (`None` to run no strategy).
    /// Disabled or unknown ids are refused; owner pins and disables still win.
    pub(crate) fn select_by_lane(
        &self,
        strategy_id: Option<&str>,
        reason: &str,
        turn_id: Option<&str>,
        now: i64,
    ) -> Result<Resolved> {
        let resolved = self.resolve()?;
        let strategy_id = match strategy_id {
            Some(id) => {
                let full = find(&resolved, id)?;
                if resolved.disabled.contains(&full) {
                    return Err("strategy_disabled".into());
                }
                let entry = resolved
                    .entries
                    .iter()
                    .find(|e| e.strategy_id == full)
                    .ok_or("strategy_not_found")?;
                if let Some(reason) = self.entry_build_mismatch(entry)? {
                    return Err(reason.into());
                }
                Some(full)
            }
            None => None,
        };
        self.append(ControlRecord {
            at_ms: now,
            control: Control::LaneSelect {
                strategy_id,
                reason: reason.to_owned(),
                turn_id: turn_id.map(str::to_owned),
            },
        })
    }

    fn dir(&self, name: &str) -> Result<PathBuf> {
        let dir = self.root.join(name);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        Ok(dir)
    }
}

fn write_temp(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(tmp)
}

/// Write `bytes` as `dir/name` unless that name exists. A hard link never
/// replaces a file, so concurrent writers converge on the first one. Returns
/// whether this call created it.
pub(crate) fn write_new(dir: &Path, name: &str, bytes: &[u8]) -> Result<bool> {
    let tmp = write_temp(dir, name, bytes)?;
    let linked = fs::hard_link(&tmp, dir.join(name));
    fs::remove_file(&tmp)?;
    let created = match linked {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(error) => return Err(error.into()),
    };
    fs::File::open(dir)?.sync_all()?;
    Ok(created)
}

#[derive(clap::Subcommand)]
pub enum Command {
    /// List registered strategies and which one is active.
    List,
    /// Print one entry with its status.
    Show { strategy_id: String },
    /// Make this version active until unpinned.
    Pin { strategy_id: String },
    /// Clear the pin; the newest eligible version becomes active.
    Unpin,
    /// Make this version ineligible.
    Disable { strategy_id: String },
    /// Make a disabled version eligible again.
    Enable { strategy_id: String },
    /// Disable the active version and pin an earlier one (default: the
    /// eligible version registered just before it).
    Rollback {
        #[arg(long)]
        to: Option<String>,
    },
}

fn listing(resolved: &Resolved, entry: &Entry) -> Value {
    json!({
        "strategy_id": entry.strategy_id,
        "status": resolved.status(&entry.strategy_id),
        "pinned": resolved.pinned.as_deref() == Some(entry.strategy_id.as_str()),
        "source_sha256": entry.source_sha256,
        "report_sha256": entry.report_sha256,
        "registered_at_ms": entry.registered_at_ms,
        "author": entry.author,
        "trades": entry.summary["trades"],
        "net_pnl": entry.summary["net_pnl"],
        "worst_drawdown": entry.summary["worst_drawdown"],
        "tape_dates": entry.tape_dates,
    })
}

/// A full id, or a unique prefix of at least 8 characters.
fn find(resolved: &Resolved, id: &str) -> Result<String> {
    if id.len() < 8 {
        return Err("strategy_id_invalid".into());
    }
    let mut found = resolved
        .entries
        .iter()
        .filter(|e| e.strategy_id.starts_with(id));
    match (found.next(), found.next()) {
        (Some(entry), None) => Ok(entry.strategy_id.clone()),
        (None, _) => Err("strategy_not_found".into()),
        (Some(_), Some(_)) => Err("strategy_id_ambiguous".into()),
    }
}

pub(crate) fn run(library: &Library, command: Command, now: i64) -> Result<Value> {
    let resolved = library.resolve()?;
    let control = match command {
        Command::List => {
            return Ok(json!({
                "status": "strategy_library",
                "active": resolved.active,
                "pinned": resolved.pinned,
                "lane_selection": resolved.lane_selection,
                "strategies": resolved.entries.iter().rev()
                    .map(|e| listing(&resolved, e)).collect::<Vec<_>>(),
                "paper_only": true,
            }));
        }
        Command::Show { strategy_id } => {
            let id = find(&resolved, &strategy_id)?;
            let entry = resolved
                .entries
                .iter()
                .find(|e| e.strategy_id == id)
                .ok_or("strategy_not_found")?;
            return Ok(json!({
                "status": resolved.status(&id),
                "pinned": resolved.pinned.as_deref() == Some(id.as_str()),
                "entry": entry,
                "paper_only": true,
            }));
        }
        Command::Pin { strategy_id } => {
            let id = find(&resolved, &strategy_id)?;
            if resolved.disabled.contains(&id) {
                return Err("strategy_disabled".into());
            }
            Control::Pin { strategy_id: id }
        }
        Command::Unpin => Control::Unpin,
        Command::Disable { strategy_id } => Control::Disable {
            strategy_id: find(&resolved, &strategy_id)?,
        },
        Command::Enable { strategy_id } => Control::Enable {
            strategy_id: find(&resolved, &strategy_id)?,
        },
        Command::Rollback { to } => {
            let from = resolved.active.clone().ok_or("strategy_none_active")?;
            let to = match to {
                Some(to) => find(&resolved, &to)?,
                None => resolved
                    .previous()
                    .ok_or("strategy_rollback_no_previous")?
                    .strategy_id
                    .clone(),
            };
            if to == from || resolved.disabled.contains(&to) {
                return Err("strategy_rollback_target_invalid".into());
            }
            Control::Rollback { from, to }
        }
    };
    let resolved = library.append(ControlRecord {
        at_ms: now,
        control: control.clone(),
    })?;
    Ok(json!({
        "status": "strategy_control_recorded",
        "control": control,
        "active": resolved.active,
        "pinned": resolved.pinned,
        "paper_only": true,
    }))
}

/// `nivra strategy --config PAPER_CONFIG <command>`. Reads and writes only
/// the library; never opens a book, a broker or a credential.
pub fn entry(config: &Path, command: Command) -> ExitCode {
    let result = (|| -> Result<Value> {
        let bytes = crate::read_bounded(config, 16_384).map_err(|_| "config_unreadable")?;
        let c: Config = serde_json::from_slice(&bytes).map_err(|_| "config_invalid")?;
        c.validate()?;
        run(&Library::at(root(&c)?), command, super::millis())
    })();
    match result {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("strategy_refused: {}", super::failure_code(error.as_ref()));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests;
