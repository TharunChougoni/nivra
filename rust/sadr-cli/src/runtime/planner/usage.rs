use super::*;
use sadr_cli::lock::AdvisoryLock;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn require_usage(c: &Config) -> Result<()> {
    usage_directory(c, true)?;
    usage(c)?;
    Ok(())
}

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

/// Provider input+output tokens recorded on one bucket's call results. A call
/// that is still in flight has no result yet and counts at admission next time.
pub(crate) fn bucket_tokens(journal: &Path, role_name: &str) -> Result<u64> {
    let prefix = format!("{role_name}-");
    let entries = match fs::read_dir(journal) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0u64;
    for entry in entries {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || !name.ends_with(".result.json") {
            continue;
        }
        let value: Value = serde_json::from_slice(
            &crate::read_bounded(&journal.join(&name), 1_048_576)
                .map_err(|_| "planner_usage_result_invalid")?,
        )?;
        total = total.saturating_add(result_tokens(&value));
    }
    Ok(total)
}

/// Tokens of one call result: the transport evidence when it carries counts,
/// otherwise the `usage` of the completed provider envelope (#241). Results
/// written before completed calls recorded their usage still count.
fn result_tokens(value: &Value) -> u64 {
    let evidence = &value["transport_evidence"];
    let recorded = ["tokens_in", "tokens_out"]
        .iter()
        .filter_map(|key| evidence[*key].as_u64())
        .fold(None, |sum: Option<u64>, n| {
            Some(sum.unwrap_or(0).saturating_add(n))
        });
    if let Some(total) = recorded {
        return total;
    }
    let usage = value["output"]
        .as_str()
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .map(|envelope| envelope["usage"].clone())
        .unwrap_or(Value::Null);
    ["input_tokens", "output_tokens"]
        .iter()
        .filter_map(|key| usage[*key].as_u64())
        .fold(0u64, u64::saturating_add)
}

/// Read-only budget view for the session-status tool. A missing journal means
/// no calls have been reserved yet; any read failure is explicit unknown.
pub(crate) fn budget_status(c: &Config, role_name: &str, role: &ModelRole) -> Value {
    let budget = role.daily_call_budget as usize;
    if usage_path(c).is_err() {
        return json!({"status":"unknown","reason":"usage_namespace_unavailable","daily_call_budget":budget});
    }
    let used = match usage(c) {
        Ok(counts) => counts[role_name].as_u64().unwrap_or(0) as usize,
        Err(_) => {
            return json!({"status":"unknown","reason":"usage_journal_unreadable","daily_call_budget":budget});
        }
    };
    json!({
        "status":"available",
        "daily_call_budget":budget,
        "used":used,
        "remaining":budget.saturating_sub(used),
        "budget_unit":role.budget_unit.as_deref().unwrap_or("request")
    })
}
pub(crate) fn usage_path(c: &Config) -> Result<PathBuf> {
    let cfg = c
        .planner_config
        .as_deref()
        .map(|path| PlannerConfig::load_for_session(path, c.session))
        .transpose()?;
    Ok(c.directory.join("model").join(
        cfg.as_ref()
            .and_then(|c| c.usage_namespace.as_deref())
            .unwrap_or("usage"),
    ))
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
        // `decide-` charges written before the owner-only policy stay valid,
        // readable journal records.
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
    let cfg = c
        .planner_config
        .as_deref()
        .map(|path| PlannerConfig::load_for_session(path, c.session))
        .transpose()?;
    let derived = cfg
        .as_ref()
        .map(|cfg| {
            let digest_role = cfg.role(Trigger::Digest);
            let operations_role = cfg.role(Trigger::Operations);
            let strategy_role = cfg.role(Trigger::Strategy);
            let scorer_role = cfg.role(Trigger::Scorer);
            let supervisor_role = cfg.role(Trigger::Supervisor);
            let mut roles = vec![&digest_role, &scorer_role, &supervisor_role];
            if cfg.post_close.is_some() {
                roles.extend([&operations_role, &strategy_role]);
            }
            roles
                .into_iter()
                .fold(0usize, |sum, role| {
                    sum.saturating_add((role.daily_call_budget as usize).saturating_mul(
                        if role.budget_unit.as_deref() == Some("event") {
                            role.max_turns.unwrap_or(12)
                        } else {
                            1
                        },
                    ))
                })
                .max(4096)
        })
        .unwrap_or(4096);
    let current = reservations(
        journal,
        cfg.as_ref()
            .and_then(|c| c.usage_max_records)
            .unwrap_or(derived),
    )?;
    let max_bytes = cfg
        .as_ref()
        .and_then(|c| c.usage_manifest_max_bytes)
        .unwrap_or((derived as u64).saturating_mul(256).max(1_000_000));
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
        sadr_core::native::decode_bounded(&raw, max_bytes as usize)
            .map_err(|_| "planner_usage_manifest_invalid")?,
    )
    .map_err(|_| "planner_usage_manifest_invalid")?;
    if expected.session_binding != book_binding(c)? || expected.reservations != current {
        return Err("planner_usage_history_changed".into());
    }
    Ok(current)
}
#[allow(clippy::too_many_arguments)] // Reserve under one local lock, released before the injected/provider call.
pub(crate) fn reserve_usage(
    c: &Config,
    cfg: &PlannerConfig,
    role_name: &str,
    role: &ModelRole,
    prompt: &str,
    guard: &CallGuard<'_>,
) -> Result<(PathBuf, usize, usize)> {
    if ![
        "digest",
        "operations",
        "strategy",
        "scorer",
        "supervisor",
        "research",
    ]
    .contains(&role_name)
    {
        return Err("planner_usage_role_invalid".into());
    }
    let journal = usage_directory(c, guard.journal_required.get())?;
    guard.journal_required.set(true);
    let _lock = usage_lock(c, true)?;
    usage_directory(c, true)?;
    let mut records = checked_reservations(c, &journal)?;
    // The research lane has its own bucket and token cap. It never reads or
    // charges the trading `supervisor` allowance (#238).
    if role_name == "research" {
        let lane =
            super::super::lane_queue::lane_settings(cfg).ok_or("research_lane_settings_missing")?;
        if bucket_tokens(&journal, "research")? >= lane.max_tokens_per_session {
            return Err("research_lane_token_budget_exhausted".into());
        }
    }
    let used = records
        .keys()
        .filter(|name| name.starts_with(&format!("{role_name}-")))
        .count();
    let mut charged = std::collections::BTreeSet::new();
    for name in records
        .keys()
        .filter(|n| n.starts_with(&format!("{role_name}-")))
    {
        if attempt_failed(&journal, name)? {
            continue;
        }
        let prior: Value = serde_json::from_slice(
            &crate::read_bounded(&journal.join(name), 4096)
                .map_err(|_| "planner_usage_reservation_invalid")?,
        )?;
        charged.insert(prior["budget_event"].as_str().unwrap_or(name).to_owned());
    }
    let budget_event = guard
        .invocation_id
        .filter(|_| role.budget_unit.as_deref() == Some("event"));
    if !budget_event.is_some_and(|id| charged.contains(id))
        && charged.len() >= role.daily_call_budget as usize
    {
        return Err("planner_budget_exhausted".into());
    }
    if (charged.len() as u64) < guard.minimum_prior_calls {
        return Err("planner_usage_history_missing".into());
    }
    let name = format!("{role_name}-{used:04}.attempt.json");
    let reservation = journal.join(&name);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&reservation)?;
    use std::io::Write;
    file.write_all(&serde_json::to_vec(&json!({"context_sha256":guard.context_hash,"budget_event":budget_event,"provider":cfg.provider,"transport":role.transport,"origin":role.transport.responses_url(),"model_admission":"provider_listed_required","model":role.model_id,"prompt_hash":digest(prompt.as_bytes()),"reserved_ms":(guard.clock)()}))?)?;
    file.sync_all()?;
    fs::File::open(&journal)?.sync_all()?;
    records.insert(name, reservation_stamp(&reservation)?);
    let root = c.directory.join("model");
    let temp = root.join(format!("usage-reservations-{role_name}-{used:04}.tmp"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)?;
    file.write_all(&serde_json::to_vec(&UsageManifest {
        session_binding: book_binding(c)?,
        reservations: records,
    })?)?;
    file.sync_all()?;
    fs::rename(&temp, root.join("usage-reservations.json"))?;
    fs::File::open(&root)?.sync_all()?;
    Ok((journal, used, charged.len()))
}

#[cfg(test)]
pub(crate) fn fixture_reservation(c: &Config, cfg: &PlannerConfig) -> PathBuf {
    let (journal, used, _) = reserve_usage(
        c,
        cfg,
        "digest",
        &cfg.digest,
        "synthetic fixture",
        &CallGuard {
            cancel: None,
            deadline: None,
            not_before: None,
            configuration: None,
            journal_required: std::cell::Cell::new(true),
            minimum_prior_calls: 0,
            invocation_id: None,
            context_hash: None,
            clock: &|| 0,
            duration: None,
            started_wall_ms: None,
        },
    )
    .unwrap();
    journal.join(format!("digest-{used:04}.attempt.json"))
}

#[cfg(test)]
pub(crate) fn call(
    c: &Config,
    cfg: &PlannerConfig,
    role_name: &str,
    role: &ModelRole,
    prompt: &str,
    backend: &mut impl Backend,
) -> Result<String> {
    call_guarded(
        c,
        cfg,
        role_name,
        role,
        prompt,
        backend,
        &CallGuard {
            cancel: None,
            deadline: None,
            not_before: None,
            configuration: None,
            journal_required: std::cell::Cell::new(false),
            minimum_prior_calls: 0,
            invocation_id: None,
            context_hash: None,
            clock: &millis,
            duration: None,
            started_wall_ms: None,
        },
    )
}
pub(crate) struct CallGuard<'a> {
    pub(crate) cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub(crate) deadline: Option<i64>,
    pub(crate) not_before: Option<i64>,
    pub(crate) configuration: Option<(&'a Config, &'a PlannerConfig)>,
    pub(crate) journal_required: std::cell::Cell<bool>,
    pub(crate) minimum_prior_calls: u64,
    pub(crate) invocation_id: Option<&'a str>,
    pub(crate) context_hash: Option<&'a str>,
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
    /// Monotonic duration when configured, otherwise the historical wall
    /// reading. Callers that need a duration subtract their own first reading.
    pub(crate) fn elapsed_ms(&self) -> i64 {
        match self.duration {
            Some(duration) => (duration)().max(0),
            None => (self.clock)(),
        }
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
        journal_required: std::cell::Cell::new(false),
        minimum_prior_calls: 0,
        invocation_id: None,
        context_hash: None,
        clock: &millis,
        duration: None,
        started_wall_ms: None,
    }
}
#[cfg(test)]
pub(crate) fn call_guarded(
    c: &Config,
    cfg: &PlannerConfig,
    role_name: &str,
    role: &ModelRole,
    prompt: &str,
    backend: &mut dyn Backend,
    guard: &CallGuard<'_>,
) -> Result<String> {
    call_guarded_as(c, cfg, role_name, role_name, role, prompt, backend, guard)
}

/// `call_guarded`, charged to `usage_name`'s bucket. The research lane runs as
/// the orchestrator role but charges its own `research` bucket (#238).
#[allow(clippy::too_many_arguments)] // The admitted role and its usage bucket are independent inputs.
pub(crate) fn call_guarded_as(
    c: &Config,
    cfg: &PlannerConfig,
    role_name: &str,
    usage_name: &str,
    role: &ModelRole,
    prompt: &str,
    backend: &mut dyn Backend,
    guard: &CallGuard<'_>,
) -> Result<String> {
    guard.check()?;
    if prompt.len() > 2_000_000 {
        return Err("planner_input_cap".into());
    }
    backend.validate_role(c, cfg, role_name, role)?;
    let (journal, used, charged) = reserve_usage(c, cfg, usage_name, role, prompt, guard)?;
    guard.check()?;
    let start = std::time::Instant::now();
    let result = backend.call(role, prompt).and_then(|raw| {
        if start.elapsed() > Duration::from_millis(role.timeout_ms) {
            return Err("planner_call_timeout".into());
        }
        if role.provider_envelope_code(&raw).is_some() {
            Err("planner_output_or_timeout_cap".into())
        } else {
            Ok(raw)
        }
    });
    let provider = backend.provenance();
    let mut evidence = json!({"latency_ms":start.elapsed().as_millis(),"budget_used":charged+1,"output":result.as_ref().ok(),"failed":result.is_err(),"transport_evidence":provider});
    let error_text = result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();
    if let Value::Object(fields) = provider_failure_evidence(&error_text, &provider) {
        for (key, value) in fields {
            evidence[key] = value;
        }
    }
    rollover::atomic(
        &journal.join(format!("{usage_name}-{used:04}.result.json")),
        &evidence,
    )?;
    result
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn review_call(
    c: &Config,
    cfg: &PlannerConfig,
    role_name: &str,
    role: &ModelRole,
    prompt: &str,
    evidence: &Value,
    cancel: &std::sync::atomic::AtomicBool,
    started: i64,
    deadline: i64,
    backend: &mut impl Backend,
) -> Result<String> {
    super::harness::run(
        c,
        cfg,
        role_name,
        role,
        prompt,
        &json!({"session_evidence":evidence}),
        backend,
        &CallGuard {
            cancel: Some(cancel),
            deadline: Some(deadline),
            not_before: Some(started),
            configuration: Some((c, cfg)),
            journal_required: std::cell::Cell::new(true),
            minimum_prior_calls: 0,
            invocation_id: None,
            context_hash: None,
            clock: &millis,
            duration: None,
            started_wall_ms: None,
        },
    )
}
