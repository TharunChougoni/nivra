use super::*;
use nivra_cli::lock::AdvisoryLock;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn prepare_review_usage(c: &Config) -> Result<()> {
    usage_directory(c, false)?;
    usage(c)?;
    Ok(())
}

/// An immutable reservation is the usage journal: failures and crashes consume budget.
/// A failed provider/model attempt is recorded for audit but does not consume
/// the daily call allowance. A missing result means the call is still in flight.
pub(crate) fn attempt_failed(journal: &Path, name: &str) -> Result<bool> {
    let result = journal.join(name.replace(".attempt.json", ".result.json"));
    if !result.exists() {
        return Ok(false);
    }
    let value: Value = serde_json::from_slice(
        &crate::read_bounded(&result, 1_048_576).map_err(|_| "planner_usage_result_invalid")?,
    )?;
    Ok(value["failed"].as_bool().unwrap_or(false))
}

pub(crate) fn usage_path(c: &Config) -> Result<PathBuf> {
    Ok(c.directory.join("model/usage"))
}
pub(crate) fn usage_directory(c: &Config, required: bool) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    #[derive(Serialize, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Binding {
        session_binding: String,
        root_device: u64,
        root_inode: u64,
        device: u64,
        inode: u64,
    }
    let root = c.directory.join("model");
    let journal = usage_path(c)?;
    let pin = root.join("usage-identity.json");
    if required && !pin.exists() {
        return Err("planner_usage_identity_missing".into());
    }
    if !pin.exists() {
        let legacy = root.join("usage");
        if journal != legacy && legacy.exists() && fs::read_dir(&legacy)?.next().is_some() {
            return Err("planner_usage_original_binding_required".into());
        }
        if journal.exists() {
            if !fs::symlink_metadata(&journal)?.is_dir() {
                return Err("planner_usage_namespace_invalid".into());
            }
            if fs::read_dir(&journal)?.next().is_some() {
                return Err("planner_usage_original_binding_required".into());
            }
        }
        fs::create_dir_all(&root)?;
        if !fs::symlink_metadata(&root)?.is_dir() {
            return Err("planner_usage_namespace_invalid".into());
        }
        if !journal.exists() {
            fs::DirBuilder::new().mode(0o700).create(&journal)?;
        }
    }
    let parent = fs::symlink_metadata(&root)?;
    let directory = fs::symlink_metadata(&journal)?;
    if !parent.is_dir() || !directory.is_dir() {
        return Err("planner_usage_namespace_invalid".into());
    }
    let actual = Binding {
        session_binding: book_binding(c)?,
        root_device: parent.dev(),
        root_inode: parent.ino(),
        device: directory.dev(),
        inode: directory.ino(),
    };
    if pin.exists() {
        let expected: Binding =
            serde_json::from_str(&private(&pin)?).map_err(|_| "planner_usage_identity_invalid")?;
        if actual != expected {
            return Err("planner_usage_namespace_changed".into());
        }
    } else {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&pin)?;
        use std::io::Write;
        file.write_all(&serde_json::to_vec(&actual)?)?;
        file.sync_all()?;
        fs::File::open(&root)?.sync_all()?;
    }
    Ok(journal)
}

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReservationStamp {
    device: u64,
    inode: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UsageManifest {
    pub(crate) session_binding: String,
    pub(crate) reservations: BTreeMap<String, ReservationStamp>,
}
/// The usage lock covers only a short journal/manifest write. A competing
/// reservation or the read-only usage projection can hold it for milliseconds;
/// aborting a turn on the first `EWOULDBLOCK` discarded already-earned evidence
/// (#161). Wait this long before reporting an explicit timeout, and never retry
/// inference itself — the lock is acquired before a provider call.
const USAGE_LOCK_WAIT_MS: u64 = 1_000;
/// A read projection is best effort: do not stall the controller behind a writer.
const USAGE_LOCK_READ_WAIT_MS: u64 = 250;
const USAGE_LOCK_POLL_MS: u64 = 10;
pub(crate) fn usage_lock(c: &Config, exclusive: bool) -> Result<AdvisoryLock> {
    usage_lock_with(
        c,
        exclusive,
        if exclusive {
            USAGE_LOCK_WAIT_MS
        } else {
            USAGE_LOCK_READ_WAIT_MS
        },
    )
}
pub(crate) fn usage_lock_with(c: &Config, exclusive: bool, wait_ms: u64) -> Result<AdvisoryLock> {
    use std::time::{Duration, Instant};
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(c.directory.join("model/usage.lock"))?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.uid() != fs::metadata("/proc/self")?.uid()
    {
        return Err("planner_usage_lock_invalid".into());
    }
    let started = Instant::now();
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let acquired = if exclusive {
            file.try_lock_exclusive()
        } else {
            fs2::FileExt::try_lock_shared(&file)
        };
        match acquired {
            Ok(()) => {
                let waited_ms = started.elapsed().as_millis() as u64;
                if waited_ms > 0 {
                    eprintln!(
                        "planner_usage_lock_contended waited_ms={waited_ms} attempts={attempts} exclusive={exclusive}"
                    );
                }
                return Ok(AdvisoryLock::new(file));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if started.elapsed() >= Duration::from_millis(wait_ms) {
                    eprintln!(
                        "planner_usage_lock_timeout waited_ms={} attempts={attempts} exclusive={exclusive}",
                        started.elapsed().as_millis()
                    );
                    return Err("planner_usage_lock_timeout".into());
                }
                std::thread::sleep(Duration::from_millis(USAGE_LOCK_POLL_MS));
            }
            Err(_) => return Err("planner_usage_lock_invalid".into()),
        }
    }
}
pub(crate) fn reservation_stamp(path: &Path) -> Result<ReservationStamp> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.len() > 4096
        || meta.uid() != fs::metadata("/proc/self")?.uid()
    {
        return Err("planner_usage_reservation_invalid".into());
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(&mut file, 4097), &mut bytes)?;
    if bytes.len() > 4096 {
        return Err("planner_usage_reservation_invalid".into());
    }
    Ok(ReservationStamp {
        device: meta.dev(),
        inode: meta.ino(),
        sha256: digest(&bytes),
    })
}
pub(crate) fn reservations(
    journal: &Path,
    max_records: usize,
) -> Result<BTreeMap<String, ReservationStamp>> {
    let mut records = BTreeMap::new();
    for item in fs::read_dir(journal)? {
        let item = item?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| "planner_usage_name_invalid")?;
        if !name.ends_with(".attempt.json") {
            continue;
        }
        // `research` is the separate research-lane bucket charged by the
        // orchestrator role (#238); it must validate like every other role or a
        // completed research turn poisons the whole usage journal.
        // `decide-`, `operations-` and `strategy-` charges written by retired
        // roles stay valid, readable journal records.
        let valid = [
            "digest-",
            "decide-",
            "operations-",
            "strategy-",
            "scorer-",
            "supervisor-",
            "research-",
        ]
        .iter()
        .any(|prefix| {
            name.strip_prefix(prefix)
                .and_then(|s| s.strip_suffix(".attempt.json"))
                .is_some_and(|s| {
                    (4..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit())
                })
        });
        if !valid || records.len() >= max_records {
            return Err("planner_usage_reservations_invalid".into());
        }
        records.insert(name, reservation_stamp(&item.path())?);
    }
    Ok(records)
}
pub(crate) fn checked_reservations(
    c: &Config,
    journal: &Path,
) -> Result<BTreeMap<String, ReservationStamp>> {
    const MAX_RECORDS: usize = 4096;
    let current = reservations(journal, MAX_RECORDS)?;
    let max_bytes = (MAX_RECORDS as u64).saturating_mul(256).max(1_000_000);
    let path = c.directory.join("model/usage-reservations.json");
    if !path.exists() {
        if !current.is_empty() {
            return Err("planner_usage_manifest_missing".into());
        }
        return Ok(current);
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.uid() != fs::metadata("/proc/self")?.uid()
    {
        return Err("planner_usage_manifest_invalid".into());
    }
    let raw =
        crate::read_bounded(&path, max_bytes).map_err(|_| "planner_usage_manifest_invalid")?;
    let expected: UsageManifest = serde_json::from_value(
        nivra_core::native::decode_bounded(&raw, max_bytes as usize)
            .map_err(|_| "planner_usage_manifest_invalid")?,
    )
    .map_err(|_| "planner_usage_manifest_invalid")?;
    if expected.session_binding != book_binding(c)? || expected.reservations != current {
        return Err("planner_usage_history_changed".into());
    }
    Ok(current)
}

pub(crate) struct CallGuard<'a> {
    pub(crate) cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub(crate) deadline: Option<i64>,
    pub(crate) not_before: Option<i64>,
    pub(crate) configuration: Option<(&'a Config, &'a PlannerConfig)>,
    /// Actual observation wall clock. This is the only source for model-visible
    /// observation timestamps; it is never derived from an anchor plus elapsed.
    pub(crate) clock: &'a dyn Fn() -> i64,
    /// Monotonic elapsed milliseconds for duration budgets. Absent callers keep
    /// the historical wall-clock budget fallback.
    pub(crate) duration: Option<&'a dyn Fn() -> i64>,
    /// Wall time the duration clock started from. Absent means no projection.
    pub(crate) started_wall_ms: Option<i64>,
}
impl CallGuard<'_> {
    fn check_configuration(&self) -> Result<()> {
        if let Some((c, cfg)) = self.configuration
            && let Some(path) = &c.planner_config
        {
            let current = PlannerConfig::load_for_session(path, c.session)?;
            if policy_bytes(&current)? != policy_bytes(cfg)? {
                return Err("planner_event_configuration_changed".into());
            }
        }
        Ok(())
    }
    /// The actual current wall observation. Never `started + elapsed`.
    pub(crate) fn wall_ms(&self) -> i64 {
        (self.clock)()
    }
    /// A conservative admission time for deadline/not-before checks: the later
    /// of the actual wall observation and the monotonic duration projection.
    /// An absolute UTC deadline must respect a forward wall jump or a suspended
    /// host even when the monotonic reading is slow; a frozen wall must not
    /// freeze it either. This is not an observation timestamp and must never be
    /// persisted as UTC.
    pub(crate) fn projected_wall_ms(&self) -> i64 {
        let projected = match (self.started_wall_ms, self.duration) {
            (Some(start), Some(duration)) => start.saturating_add((duration)().max(0)),
            _ => (self.clock)(),
        };
        projected.max(self.wall_ms())
    }
    /// A wall backstep or negative monotonic reading refuses new calls instead
    /// of silently fabricating a later UTC observation.
    pub(crate) fn check_clock(&self) -> Result<()> {
        if self
            .started_wall_ms
            .is_some_and(|start| self.wall_ms() < start)
            || self.duration.is_some_and(|duration| (duration)() < 0)
        {
            return Err("planner_clock_anomaly".into());
        }
        Ok(())
    }
    /// Config change, clock anomaly and a passed event deadline reject a
    /// finished answer or a new call.
    pub(crate) fn check_finished(&self) -> Result<()> {
        self.check_configuration()?;
        self.check_clock()?;
        let wall = self.wall_ms();
        let projected = self.projected_wall_ms();
        // An absolute deadline is passed when either the actual wall or the
        // monotonic projection is past it. A not-before window is not entered
        // while the actual wall is still before it, even if a monotonic reading
        // claims otherwise.
        if self.deadline.is_some_and(|deadline| projected > deadline)
            || self
                .not_before
                .is_some_and(|not_before| wall < not_before || projected < not_before)
        {
            return Err("planner_event_deadline_expired".into());
        }
        Ok(())
    }
    pub(crate) fn check_cancel(&self) -> Result<()> {
        if self
            .cancel
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire))
        {
            Err("planner_cancelled_no_replay".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn check(&self) -> Result<()> {
        self.check_finished()?;
        self.check_cancel()
    }
}
pub(crate) fn preview_guard() -> CallGuard<'static> {
    CallGuard {
        cancel: None,
        deadline: None,
        not_before: None,
        configuration: None,
        clock: &millis,
        duration: None,
        started_wall_ms: None,
    }
}
