//! Bubblewrap inside one transient systemd user unit.
//! Fixed argument arrays only. No shell, network, home, or subprocess fallback.
//! A successful fixture here does not qualify the host.
use crate::worker::{
    CandidateLaunch, CandidateUnitExited, CandidateUnitObservation, CandidateUnitPoll,
    CandidateUnitProof, CandidateUnitRunner, ExecutionIdentity, Launch, ResourceLimits,
    RunnerError, SandboxPoll, SandboxRunner, StoppedReceipt, candidate_unit_name, quota_hundredths,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::Duration,
};

const SYSTEMD_RUN: &str = "/usr/bin/systemd-run";
const SYSTEMCTL: &str = "/usr/bin/systemctl";
const BWRAP: &str = "/usr/bin/bwrap";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandPlan {
    pub unit: String,
    pub argv: Vec<String>,
}

pub struct Runner {
    root: PathBuf,
}

impl Runner {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

fn hash_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn hex_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn forbidden(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("home" | "root" | ".ssh" | ".hermes" | ".grok" | ".codex" | ".openclaw" | ".config")
    )
}

fn lexical_root(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        && !path.components().any(|component| match component {
            Component::Normal(name) => forbidden(name),
            _ => false,
        })
}

fn existing_root(path: &Path) -> bool {
    if !lexical_root(path) {
        return false;
    }
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component);
        let Ok(meta) = fs::symlink_metadata(&cursor) else {
            return false;
        };
        if meta.file_type().is_symlink() || cursor.join(".git").exists() {
            return false;
        }
    }
    fs::metadata(path).is_ok_and(|meta| meta.is_dir() && meta.permissions().mode() & 0o077 == 0)
}

fn cpu_quota(limits: &ResourceLimits, wall_millis: u32) -> Option<String> {
    // systemd accepts at most two decimal places. The shared integer helper
    // rounds down so the quota stays inside the reviewed CPU budget, and the
    // proof compares the same value rather than a separately rounded float.
    let hundredths = quota_hundredths(limits.cpu_millis, wall_millis)?;
    Some(format!("{}.{:02}%", hundredths / 100, hundredths % 100))
}

pub(crate) fn command_plan(launch: &Launch, root: &Path) -> Result<CommandPlan, RunnerError> {
    if !hex_id(&launch.identity.lease) || !lexical_root(root) {
        return Err(RunnerError::Unavailable);
    }
    let quota =
        cpu_quota(&launch.limits, launch.spec.wall_millis).ok_or(RunnerError::Unavailable)?;
    let workspace = launch.limits.workspace_bytes.to_string();
    let stage = root.join(&launch.identity.lease);
    let unit = format!("nivra-rw-{}", launch.identity.lease);
    let bwrap = [
        BWRAP,
        "--unshare-user",
        "--unshare-ipc",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-uts",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--clearenv",
        "--chdir",
        "/workspace",
        "--dir",
        "/inputs",
        "--proc",
        "/proc",
        "--size",
        workspace.as_str(),
        "--tmpfs",
        "/workspace",
    ];
    let mut argv = vec![
        SYSTEMD_RUN.into(),
        "--user".into(),
        "--quiet".into(),
        "--unit".into(),
        unit.clone(),
        "--property".into(),
        format!("MemoryMax={}", launch.spec.memory_bytes),
        "--property".into(),
        "MemorySwapMax=0".into(),
        "--property".into(),
        format!("CPUQuota={quota}"),
        "--property".into(),
        format!("TasksMax={}", launch.limits.tasks),
        "--property".into(),
        format!("RuntimeMaxSec={}ms", launch.spec.wall_millis),
        "--property".into(),
        format!("LimitFSIZE={}", launch.limits.file_bytes),
        // Bubblewrap creates the mandatory network and empty filesystem view.
        // Asking systemd's user executor to create a second namespace first
        // prevents its AppArmor transition into the permitted bwrap profile.
        // Do not disable AppArmor or fall back when bwrap itself is denied.
        "--property".into(),
        "NoNewPrivileges=yes".into(),
        "--property".into(),
        "KillMode=control-group".into(),
        "--property".into(),
        "TimeoutStopSec=2s".into(),
        "--".into(),
    ];
    argv.extend(bwrap.into_iter().map(str::to_owned));
    let stage = stage.as_os_str().to_str().ok_or(RunnerError::Unavailable)?;
    for (name, dest) in [
        ("runtime", "/runtime"),
        ("code", "/inputs/code"),
        ("input", "/inputs/input"),
        ("launch.json", "/inputs/launch.json"),
    ] {
        argv.push("--ro-bind".into());
        argv.push(format!("{stage}/{name}"));
        argv.push(dest.into());
    }
    argv.extend([
        "--bind".into(),
        format!("{stage}/outputs"),
        "/outputs".into(),
        "--".into(),
        "/runtime".into(),
        "/inputs/code".into(),
        "/inputs/input".into(),
    ]);
    if argv
        .iter()
        .any(|arg| arg == "--share-net" || arg.contains("/home") || arg.contains("/root"))
    {
        return Err(RunnerError::Unavailable);
    }
    Ok(CommandPlan { unit, argv })
}

fn write_private(path: &Path, bytes: &[u8], mode: u32) -> Result<(), RunnerError> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| RunnerError::Unavailable)?;
    file.write_all(bytes)
        .map_err(|_| RunnerError::Unavailable)?;
    file.sync_all().map_err(|_| RunnerError::Unavailable)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|_| RunnerError::Unavailable)?;
    let mut check = Vec::new();
    fs::File::open(path)
        .and_then(|mut file| file.read_to_end(&mut check))
        .map_err(|_| RunnerError::Unavailable)?;
    if check != bytes {
        return Err(RunnerError::Ambiguous);
    }
    Ok(())
}

fn stage_inputs(
    launch: &Launch,
    registered_code: &[u8],
    runtime: &[u8],
    input: &[u8],
    root: &Path,
) -> Result<PathBuf, RunnerError> {
    if !existing_root(root)
        || hash_bytes(registered_code) != launch.spec.script_hash
        || hash_bytes(runtime) != launch.spec.runtime_hash
        || hash_bytes(input) != launch.spec.input_hash
    {
        return Err(RunnerError::Unavailable);
    }
    let stage = root.join(&launch.identity.lease);
    fs::create_dir(&stage).map_err(|_| RunnerError::Ambiguous)?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))
        .map_err(|_| RunnerError::Ambiguous)?;
    let outputs = stage.join("outputs");
    fs::create_dir(&outputs).map_err(|_| RunnerError::Ambiguous)?;
    fs::set_permissions(&outputs, fs::Permissions::from_mode(0o700))
        .map_err(|_| RunnerError::Ambiguous)?;
    write_private(&stage.join("runtime"), runtime, 0o500)?;
    write_private(&stage.join("code"), registered_code, 0o400)?;
    write_private(&stage.join("input"), input, 0o400)?;
    let launch_bytes = serde_json::to_vec(launch).map_err(|_| RunnerError::Unavailable)?;
    write_private(&stage.join("launch.json"), &launch_bytes, 0o400)?;
    Ok(stage)
}

fn effective_uid() -> Result<u32, RunnerError> {
    let status = fs::read_to_string("/proc/self/status").map_err(|_| RunnerError::Unavailable)?;
    let line = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .ok_or(RunnerError::Unavailable)?;
    // real, effective, saved, filesystem
    line.split_whitespace()
        .nth(1)
        .and_then(|uid| uid.parse().ok())
        .ok_or(RunnerError::Unavailable)
}

fn spawn(argv: &[String]) -> Result<(), RunnerError> {
    if argv.first().map(String::as_str) != Some(SYSTEMD_RUN) {
        return Err(RunnerError::Unavailable);
    }
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| RunnerError::Unavailable)?;
    let status = child.wait().map_err(|_| RunnerError::Ambiguous)?;
    if status.success() {
        Ok(())
    } else {
        Err(RunnerError::Ambiguous)
    }
}

#[derive(Debug)]
struct UnitView {
    active: String,
    main_pid: u64,
    tasks: Option<u64>,
    control_group: String,
    cgroup_events: Option<String>,
    exit_code: i32,
    observation: CandidateUnitObservation,
}
impl UnitView {
    fn missing() -> Self {
        Self {
            active: "inactive".into(),
            main_pid: 0,
            tasks: None,
            control_group: String::new(),
            cgroup_events: None,
            exit_code: 0,
            observation: CandidateUnitObservation::default(),
        }
    }
}

fn unit_name(identity: &ExecutionIdentity) -> Result<String, RunnerError> {
    if !hex_id(&identity.lease) {
        return Err(RunnerError::Ambiguous);
    }
    Ok(format!("nivra-rw-{}", identity.lease))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShowFailure {
    UnitMissing,
    Unavailable,
    Ambiguous,
}

/// Classify a failed `systemctl show` without guessing. A missing unit is a
/// truthful absent view; a bus/permission failure is unavailable; anything
/// else is ambiguous and must retain the lease.
fn classify_show_failure(expected_service: &str, exit_code: i32, stderr: &[u8]) -> ShowFailure {
    if exit_code == 0 {
        return ShowFailure::Ambiguous;
    }
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    // Bus, transport, permission and manager-unavailable failures are never
    // unit absence, even when their text contains "no such file".
    if text.contains("failed to connect to bus")
        || text.contains("failed to connect to session bus")
        || text.contains("connection refused")
        || text.contains("permission denied")
        || text.contains("access denied")
        || text.contains("not permitted")
        || text.contains("authentication")
        || text.contains("no medium found")
        || text.contains("transport endpoint is not connected")
        || text.contains("not been booted with systemd")
        || text.contains("not booted with systemd")
        || text.contains("systemd is not running")
    {
        return ShowFailure::Unavailable;
    }
    let expected = expected_service.to_ascii_lowercase();
    if expected.is_empty() {
        return ShowFailure::Ambiguous;
    }
    // Only an authoritative, exact-unit not-found message may prove absence.
    // Generic phrases such as "not found" or "no such file" are ambiguous.
    let exact = [
        format!("unit {expected} could not be found"),
        format!("unit {expected} not found"),
        format!("unit {expected} not loaded"),
        format!("unit {expected} does not exist"),
    ];
    if exact.iter().any(|pattern| text.contains(pattern)) {
        return ShowFailure::UnitMissing;
    }
    if text.contains("nosuchunit") && text.contains(&expected) {
        return ShowFailure::UnitMissing;
    }
    ShowFailure::Ambiguous
}

#[derive(Debug)]
struct ShowProperties {
    active: String,
    main_pid: u64,
    tasks: Option<u64>,
    control_group: String,
    exit_code: i32,
    observation: CandidateUnitObservation,
}

fn parse_show_properties(text: &str) -> Result<ShowProperties, RunnerError> {
    let mut active = None;
    let mut main_pid = None;
    let mut tasks = None;
    let mut control_group = None;
    let mut exit_code = None;
    let mut memory_max = None;
    let mut memory_swap_max = None;
    let mut cpu_quota_usec = None;
    let mut tasks_max = None;
    let mut runtime_max_usec = None;
    let mut limit_nofile = None;
    let mut kill_mode = None;
    let mut no_new_privileges = None;
    let mut timeout_stop_usec = None;
    let mut restart = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            return Err(RunnerError::Ambiguous);
        };
        match key {
            // Id/LoadState are consumed by the not-found projection before
            // full property parsing; accepting them here keeps a loaded
            // property surface parseable.
            "Id" | "LoadState" => {}
            "ActiveState" => active = Some(value.to_owned()),
            "MainPID" => main_pid = value.parse().ok(),
            "TasksCurrent" => {
                tasks = if matches!(value, "" | "[not set]" | "[uninitialized]" | "n/a") {
                    None
                } else {
                    Some(value.parse().map_err(|_| RunnerError::Ambiguous)?)
                }
            }
            "ControlGroup" => control_group = Some(value.to_owned()),
            "ExecMainStatus" => exit_code = value.parse().ok(),
            "MemoryMax" => memory_max = value.parse().ok(),
            "MemorySwapMax" => memory_swap_max = value.parse().ok(),
            "CPUQuotaPerSecUSec" => cpu_quota_usec = parse_systemd_usec(value),
            "TasksMax" => tasks_max = value.parse().ok(),
            "RuntimeMaxUSec" => runtime_max_usec = parse_systemd_usec(value),
            "LimitNOFILE" => limit_nofile = value.parse().ok(),
            "KillMode" => kill_mode = Some(value.to_owned()),
            "NoNewPrivileges" => {
                no_new_privileges = match value {
                    "yes" | "true" => Some(true),
                    "no" | "false" => Some(false),
                    _ => None,
                }
            }
            "TimeoutStopUSec" => timeout_stop_usec = parse_systemd_usec(value),
            "Restart" => restart = Some(value.to_owned()),
            _ => return Err(RunnerError::Ambiguous),
        }
    }
    let active = active.ok_or(RunnerError::Ambiguous)?;
    let control_group = control_group.ok_or(RunnerError::Ambiguous)?;
    if !control_group.is_empty()
        && (!control_group.starts_with('/')
            || control_group.contains("..")
            || control_group.len() > 1024)
    {
        return Err(RunnerError::Ambiguous);
    }
    Ok(ShowProperties {
        active,
        main_pid: main_pid.ok_or(RunnerError::Ambiguous)?,
        tasks,
        control_group,
        exit_code: exit_code.unwrap_or(0),
        observation: CandidateUnitObservation {
            memory_max,
            memory_swap_max,
            cpu_quota_usec,
            tasks_max,
            runtime_max_usec,
            limit_nofile,
            kill_mode,
            no_new_privileges,
            timeout_stop_usec,
            restart,
        },
    })
}

fn classify_cgroup_events(result: std::io::Result<String>) -> Result<Option<String>, RunnerError> {
    match result {
        Ok(events) if events.len() <= 4096 => Ok(Some(events)),
        // A non-empty unit control group with a missing events file is
        // ambiguous, never the truthful absent variant.
        Ok(_) => Err(RunnerError::Ambiguous),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(RunnerError::Ambiguous),
    }
}

fn read_cgroup_events(control_group: &str) -> Result<Option<String>, RunnerError> {
    if control_group.is_empty() {
        return Ok(None);
    }
    classify_cgroup_events(fs::read_to_string(format!(
        "/sys/fs/cgroup{control_group}/cgroup.events"
    )))
}

fn show_output(
    expected_service: &str,
    output: std::process::Output,
    read_cgroup: impl Fn(&str) -> Result<Option<String>, RunnerError>,
) -> Result<UnitView, RunnerError> {
    if !output.status.success() {
        if output.stderr.len() > 4096 {
            return Err(RunnerError::Ambiguous);
        }
        return match classify_show_failure(
            expected_service,
            output.status.code().unwrap_or(-1),
            &output.stderr,
        ) {
            ShowFailure::UnitMissing => Ok(UnitView::missing()),
            ShowFailure::Unavailable => Err(RunnerError::Unavailable),
            ShowFailure::Ambiguous => Err(RunnerError::Ambiguous),
        };
    }
    if output.stdout.len() > 8192 || output.stderr.len() > 4096 {
        return Err(RunnerError::Ambiguous);
    }
    let text = String::from_utf8(output.stdout).map_err(|_| RunnerError::Ambiguous)?;
    // The successful not-found projection is absence only when systemd names
    // the exact unit that was requested. Check it before requiring the full
    // loaded property surface, because a not-found unit may omit defaults.
    let mut id = None;
    let mut load_state = None;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "Id" => id = Some(value),
                "LoadState" => load_state = Some(value),
                _ => {}
            }
        }
    }
    if load_state == Some("not-found") {
        return if id == Some(expected_service) {
            Ok(UnitView::missing())
        } else {
            Err(RunnerError::Ambiguous)
        };
    }
    let properties = parse_show_properties(&text)?;
    let cgroup_events = read_cgroup(&properties.control_group)?;
    Ok(UnitView {
        active: properties.active,
        main_pid: properties.main_pid,
        tasks: properties.tasks,
        control_group: properties.control_group,
        cgroup_events,
        exit_code: properties.exit_code,
        observation: properties.observation,
    })
}

fn show(unit: &str) -> Result<UnitView, RunnerError> {
    let service = format!("{unit}.service");
    let output = Command::new(SYSTEMCTL)
        .args([
            "--user",
            "show",
            "--property=Id",
            "--property=LoadState",
            "--property=ActiveState",
            "--property=MainPID",
            "--property=TasksCurrent",
            "--property=ExecMainStatus",
            "--property=ControlGroup",
            "--property=MemoryMax",
            "--property=MemorySwapMax",
            "--property=CPUQuotaPerSecUSec",
            "--property=TasksMax",
            "--property=RuntimeMaxUSec",
            "--property=LimitNOFILE",
            "--property=KillMode",
            "--property=NoNewPrivileges",
            "--property=TimeoutStopUSec",
            "--property=Restart",
            &service,
        ])
        // Keep the failure vocabulary stable for classification. This is a
        // per-command locale, never a process-wide environment mutation.
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| RunnerError::Unavailable)?;
    show_output(&service, output, read_cgroup_events)
}

fn absent(view: &UnitView) -> bool {
    matches!(view.active.as_str(), "inactive" | "failed")
        && view.main_pid == 0
        && view.tasks.is_none_or(|tasks| tasks == 0)
}

fn stop(unit: &str) -> Result<(), RunnerError> {
    let status = Command::new(SYSTEMCTL)
        .args(["--user", "stop", &format!("{unit}.service")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| RunnerError::Ambiguous)?;
    // A finished transient unit is already unloaded. Stopping it exits 5.
    if !status.success() {
        let view = show(unit)?;
        if !absent(&view) {
            return Err(RunnerError::Ambiguous);
        }
    }
    let _ = Command::new(SYSTEMCTL)
        .args(["--user", "reset-failed", &format!("{unit}.service")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    Ok(())
}

fn prove_absent(unit: &str) -> Result<UnitView, RunnerError> {
    stop(unit)?;
    for _ in 0..10 {
        let view = show(unit)?;
        if absent(&view) {
            return Ok(view);
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(RunnerError::Ambiguous)
}

fn read_output(stage: &Path, limit: u64) -> Result<(i32, Vec<u8>), RunnerError> {
    let path = stage.join("outputs").join("result.json");
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return Ok((1, Vec::new()));
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > limit || meta.len() > 65536
    {
        return Ok((1, Vec::new()));
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| RunnerError::Ambiguous)?;
    if bytes.len() as u64 != meta.len() {
        return Err(RunnerError::Ambiguous);
    }
    Ok((0, bytes))
}

fn receipt(identity: &ExecutionIdentity, view: &UnitView) -> Result<StoppedReceipt, RunnerError> {
    let proof = format!(
        "unit={} active={} pid={} tasks={:?} cgroup={} exit={}",
        unit_name(identity)?,
        view.active,
        view.main_pid,
        view.tasks,
        view.control_group,
        view.exit_code
    );
    Ok(StoppedReceipt {
        identity: identity.clone(),
        evidence_sha256: hash_bytes(proof.as_bytes()),
    })
}

/// Parse the exact microsecond value systemd prints for a time span. The
/// output can be fractional and compound (`416.600ms`, `1min 30s`,
/// `2min 3.456000s`). Every step is checked integer arithmetic; a value that
/// cannot be represented exactly in microseconds, overflows, or is unset /
/// infinite is invalid rather than silently truncated.
fn parse_systemd_usec(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty()
        || matches!(
            value,
            "infinity" | "[not set]" | "[uninitialized]" | "n/a" | "unset"
        )
    {
        return None;
    }
    let mut total = 0u64;
    let mut components = 0usize;
    for component in value.split_whitespace() {
        components = components.checked_add(1)?;
        if components > 8 {
            return None;
        }
        let (number, scale) = split_duration_component(component)?;
        let micros = parse_duration_number(number, scale)?;
        total = total.checked_add(micros)?;
    }
    (components > 0).then_some(total)
}

/// Longest suffixes first: `ms`/`us`/`min` must not be read as `s` or `m`.
/// A bare integer is interpreted as microseconds, matching systemd's raw
/// serialization.
fn split_duration_component(component: &str) -> Option<(&str, u64)> {
    for (suffix, scale) in [
        ("min", 60_000_000u64),
        ("ms", 1_000),
        ("us", 1),
        ("µs", 1),
        ("μs", 1),
        ("s", 1_000_000),
        ("h", 3_600_000_000),
        ("d", 86_400_000_000),
        ("w", 604_800_000_000),
    ] {
        if let Some(number) = component.strip_suffix(suffix) {
            return Some((number, scale));
        }
    }
    Some((component, 1))
}

fn parse_duration_number(number: &str, scale: u64) -> Option<u64> {
    if number.is_empty() {
        return None;
    }
    let (whole, fraction) = match number.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (number, None),
    };
    let whole = if whole.is_empty() {
        0
    } else {
        whole.parse::<u64>().ok()?
    };
    let whole = whole.checked_mul(scale)?;
    let Some(fraction) = fraction else {
        return Some(whole);
    };
    if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let fraction_digits = u32::try_from(fraction.len()).ok()?;
    if fraction_digits > 19 {
        return None;
    }
    let fraction_value = fraction.parse::<u64>().ok()?;
    let divisor = 10u64.checked_pow(fraction_digits)?;
    let scaled = fraction_value.checked_mul(scale)?;
    if scaled % divisor != 0 {
        return None;
    }
    whole.checked_add(scaled / divisor)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn machine_id() -> Result<String, RunnerError> {
    for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
        if let Ok(text) = fs::read_to_string(path) {
            let id = text.trim();
            if !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control) {
                return Ok(id.to_owned());
            }
        }
    }
    Err(RunnerError::Unavailable)
}

fn candidate_proof(
    launch: &CandidateLaunch,
    view: &UnitView,
    observation: &CandidateUnitObservation,
    origin: &str,
) -> Result<CandidateUnitProof, RunnerError> {
    if !matches!(view.active.as_str(), "inactive" | "failed")
        || view.main_pid != 0
        || view.tasks.is_some_and(|tasks| tasks != 0)
    {
        return Err(RunnerError::Ambiguous);
    }
    let (cgroup_absent, cgroup_events) = if view.control_group.is_empty() {
        (true, String::new())
    } else {
        let events = view
            .cgroup_events
            .as_deref()
            .ok_or(RunnerError::Ambiguous)?;
        if !events.contains("populated 0") {
            return Err(RunnerError::Ambiguous);
        }
        (false, events.to_owned())
    };
    let mut proof = CandidateUnitProof {
        version: 1,
        unit: launch.unit.clone(),
        lease: launch.lease.clone(),
        machine: machine_id()?,
        policy_sha256: launch.unit_policy.policy_sha256(),
        active_state: view.active.clone(),
        main_pid: view.main_pid,
        tasks_current: view.tasks.unwrap_or(0),
        tasks_present: view.tasks.is_some(),
        control_group: view.control_group.clone(),
        cgroup_absent,
        cgroup_events_sha256: hash_bytes(cgroup_events.as_bytes()),
        cgroup_events,
        exit_code: view.exit_code.max(0),
        checked_at_ms: now_ms(),
        observation: observation.clone(),
        properties_observed: observation.complete(),
        proof_origin: origin.to_owned(),
        proof_sha256: String::new(),
    };
    proof.proof_sha256 = proof.hash().map_err(|_| RunnerError::Ambiguous)?;
    if !proof.verify(launch) {
        return Err(RunnerError::Ambiguous);
    }
    Ok(proof)
}

fn candidate_stage(launch: &CandidateLaunch, root: &Path) -> Result<PathBuf, RunnerError> {
    let stage = Path::new(&launch.stage_dir);
    if !lexical_root(stage)
        || !stage.starts_with(root)
        || stage == root
        || stage.file_name().is_none()
    {
        return Err(RunnerError::Unavailable);
    }
    let meta = fs::symlink_metadata(stage).map_err(|_| RunnerError::Unavailable)?;
    if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0 {
        return Err(RunnerError::Unavailable);
    }
    Ok(stage.to_owned())
}

fn candidate_helper(launch: &CandidateLaunch, root: &Path) -> Result<PathBuf, RunnerError> {
    let helper = Path::new(&launch.helper_path);
    let expected = root.join("helpers").join(&launch.unit_policy.helper_sha256);
    if helper != expected {
        return Err(RunnerError::Unavailable);
    }
    let meta = fs::symlink_metadata(helper).map_err(|_| RunnerError::Unavailable)?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.permissions().mode() & 0o777 != 0o500
    {
        return Err(RunnerError::Unavailable);
    }
    let mut bytes = Vec::new();
    fs::File::open(helper)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| RunnerError::Unavailable)?;
    if bytes.len() > 256 * 1024 * 1024 || hash_bytes(&bytes) != launch.unit_policy.helper_sha256 {
        return Err(RunnerError::Unavailable);
    }
    Ok(helper.to_owned())
}

fn candidate_quota(cpu_millis: u32, wall_millis: u32) -> Option<String> {
    let hundredths = quota_hundredths(cpu_millis, wall_millis)?;
    Some(format!("{}.{:02}%", hundredths / 100, hundredths % 100))
}

pub(crate) fn candidate_command_plan(
    launch: &CandidateLaunch,
    root: &Path,
) -> Result<CommandPlan, RunnerError> {
    if !hex_id(&launch.lease) || !lexical_root(root) {
        return Err(RunnerError::Unavailable);
    }
    let unit = candidate_unit_name(&launch.lease).ok_or(RunnerError::Unavailable)?;
    if launch.unit != unit || launch.version != 2 {
        return Err(RunnerError::Unavailable);
    }
    let stage = candidate_stage(launch, root)?;
    let helper = candidate_helper(launch, root)?;
    if stage.starts_with("/tmp") || stage.starts_with("/var/tmp") {
        // PrivateTmp=yes would hide the stage from the helper.
        return Err(RunnerError::Unavailable);
    }
    let quota = candidate_quota(
        launch.unit_policy.unit_cpu_millis,
        launch.unit_policy.unit_wall_millis,
    )
    .ok_or(RunnerError::Unavailable)?;
    let launch_path = stage.join("launch.json");
    let stage_text = stage.as_os_str().to_str().ok_or(RunnerError::Unavailable)?;
    let helper_text = helper
        .as_os_str()
        .to_str()
        .ok_or(RunnerError::Unavailable)?;
    let launch_text = launch_path
        .as_os_str()
        .to_str()
        .ok_or(RunnerError::Unavailable)?;
    let argv = vec![
        SYSTEMD_RUN.into(),
        "--user".into(),
        "--quiet".into(),
        "--unit".into(),
        unit.clone(),
        "--property".into(),
        format!("MemoryMax={}", launch.unit_policy.unit_memory_bytes),
        "--property".into(),
        "MemorySwapMax=0".into(),
        "--property".into(),
        format!("CPUQuota={quota}"),
        "--property".into(),
        format!("TasksMax={}", launch.unit_policy.unit_tasks),
        "--property".into(),
        format!("RuntimeMaxSec={}ms", launch.unit_policy.unit_wall_millis),
        "--property".into(),
        format!("LimitNOFILE={}", launch.unit_policy.unit_open_files),
        "--property".into(),
        "KillMode=control-group".into(),
        "--property".into(),
        "TimeoutStopSec=2s".into(),
        "--property".into(),
        "NoNewPrivileges=yes".into(),
        "--property".into(),
        "Restart=no".into(),
        "--property".into(),
        "PrivateTmp=yes".into(),
        "--property".into(),
        "UMask=0077".into(),
        "--property".into(),
        "UnsetEnvironment=*".into(),
        "--property".into(),
        format!("WorkingDirectory={stage_text}"),
        "--".into(),
        helper_text.into(),
        "candidate-supervise".into(),
        launch_text.into(),
    ];
    if argv
        .iter()
        .any(|arg| arg == "--share-net" || arg.contains("/home/") && arg.contains(".."))
    {
        return Err(RunnerError::Unavailable);
    }
    Ok(CommandPlan { unit, argv })
}

fn read_candidate_result(launch: &CandidateLaunch, root: &Path) -> Result<Vec<u8>, RunnerError> {
    let stage = candidate_stage(launch, root)?;
    let path = stage.join("result.json");
    let Ok(meta) = fs::symlink_metadata(&path) else {
        return Ok(Vec::new());
    };
    let cap = launch.unit_policy.max_result_bytes as u64;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() == 0 || meta.len() > cap {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    fs::File::open(&path)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|_| RunnerError::Ambiguous)?;
    if bytes.len() as u64 != meta.len() {
        return Err(RunnerError::Ambiguous);
    }
    Ok(bytes)
}

/// Filesystem-only readiness for the configured candidate stage root. The
/// stage must be private, outside PrivateTmp, and free of forbidden host
/// components.
pub fn candidate_stage_ready(path: &Path) -> std::result::Result<(), &'static str> {
    if !lexical_root(path) {
        return Err("candidate_stage_path_invalid");
    }
    if path.starts_with("/tmp") || path.starts_with("/var/tmp") {
        return Err("candidate_stage_private_tmp");
    }
    if path.exists() {
        let meta = fs::symlink_metadata(path).map_err(|_| "candidate_stage_unavailable")?;
        if !meta.is_dir() || meta.file_type().is_symlink() || meta.permissions().mode() & 0o077 != 0
        {
            return Err("candidate_stage_invalid");
        }
    }
    Ok(())
}

/// Public readiness scope. These checks prove configured prerequisites only;
/// they do not prove a live user manager, delegated cgroup or unprivileged user
/// namespace until a separate host qualification drill observes them.
pub const CANDIDATE_READINESS_SCOPE: &str = "configured_prerequisites_only";

/// Pure configured-prerequisite check. Callers inject the OS observations so
/// tests never open a socket or run a host unit. A missing known prerequisite
/// blocks; a passing check never claims runtime or host qualification.
pub fn candidate_readiness_for(
    uid: u32,
    executable: impl Fn(&Path) -> bool,
    cgroup_v2: bool,
    user_bus: bool,
    machine_id: Option<&str>,
) -> std::result::Result<(), &'static str> {
    if uid == 0 {
        return Err("candidate_readiness_root");
    }
    for (path, code) in [
        (Path::new(SYSTEMD_RUN), "candidate_readiness_systemd_run"),
        (Path::new(SYSTEMCTL), "candidate_readiness_systemctl"),
        (Path::new(BWRAP), "candidate_readiness_bwrap"),
        (Path::new("/usr/bin/python3"), "candidate_readiness_python"),
    ] {
        if !executable(path) {
            return Err(code);
        }
    }
    if !cgroup_v2 {
        return Err("candidate_readiness_cgroup_v2");
    }
    if !user_bus {
        return Err("candidate_readiness_user_bus");
    }
    if !machine_id
        .is_some_and(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
    {
        return Err("candidate_readiness_machine_identity");
    }
    Ok(())
}

/// Filesystem-only readiness for the owned candidate unit. No subprocess,
/// socket, session or queue write is performed here. The result is a configured
/// prerequisite check, not a live-manager or host-qualification claim.
pub fn candidate_readiness() -> std::result::Result<(), &'static str> {
    let uid = effective_uid().map_err(|_| "candidate_readiness_uid")?;
    let executable = |path: &Path| {
        fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    };
    let cgroup_v2 = Path::new("/sys/fs/cgroup/cgroup.controllers").is_file();
    let user_bus = std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|runtime| Path::new(&runtime).join("bus").exists());
    let machine = machine_id().ok();
    candidate_readiness_for(uid, executable, cgroup_v2, user_bus, machine.as_deref())
}

pub struct CandidateRunner {
    root: PathBuf,
}

impl CandidateRunner {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl CandidateUnitRunner for CandidateRunner {
    fn start(&mut self, launch: &CandidateLaunch) -> Result<(), RunnerError> {
        if effective_uid()? == 0
            || !Path::new(BWRAP).is_file()
            || !Path::new(SYSTEMD_RUN).is_file()
            || !Path::new(SYSTEMCTL).is_file()
        {
            return Err(RunnerError::Unavailable);
        }
        let plan = candidate_command_plan(launch, &self.root)?;
        if show(&plan.unit).is_ok_and(|view| !absent(&view)) {
            return Err(RunnerError::Ambiguous);
        }
        spawn(&plan.argv)
    }

    fn poll(&mut self, launch: &CandidateLaunch) -> Result<CandidateUnitPoll, RunnerError> {
        let view = show(&launch.unit)?;
        if matches!(
            view.active.as_str(),
            "active" | "activating" | "deactivating"
        ) {
            return Ok(CandidateUnitPoll::Running);
        }
        if !absent(&view) {
            return Err(RunnerError::Ambiguous);
        }
        let observation = if view.control_group.is_empty() {
            CandidateUnitObservation::default()
        } else {
            view.observation.clone()
        };
        let proved = prove_absent(&launch.unit)?;
        let proof = candidate_proof(launch, &proved, &observation, "systemd_user_unit")?;
        let result = read_candidate_result(launch, &self.root)?;
        Ok(CandidateUnitPoll::Exited(Box::new(CandidateUnitExited {
            exit_code: proved.exit_code,
            proof,
            result,
        })))
    }

    fn terminate(&mut self, launch: &CandidateLaunch) -> Result<CandidateUnitProof, RunnerError> {
        let observation = show(&launch.unit)
            .map(|view| {
                if view.control_group.is_empty() {
                    CandidateUnitObservation::default()
                } else {
                    view.observation
                }
            })
            .unwrap_or_default();
        let view = prove_absent(&launch.unit)?;
        candidate_proof(launch, &view, &observation, "systemd_user_unit")
    }
}

impl SandboxRunner for Runner {
    fn start(
        &mut self,
        launch: &Launch,
        registered: &crate::worker::RegisteredRun,
    ) -> Result<(), RunnerError> {
        if effective_uid()? == 0
            || !Path::new(BWRAP).is_file()
            || !Path::new(SYSTEMD_RUN).is_file()
            || !Path::new(SYSTEMCTL).is_file()
        {
            return Err(RunnerError::Unavailable);
        }
        let plan = command_plan(launch, &self.root)?;
        if show(&plan.unit).is_ok_and(|view| !absent(&view) && view.active != "inactive") {
            return Err(RunnerError::Ambiguous);
        }
        stage_inputs(
            launch,
            &registered.code,
            &registered.runtime,
            &registered.input,
            &self.root,
        )?;
        spawn(&plan.argv)
    }

    fn poll(&mut self, identity: &ExecutionIdentity) -> Result<SandboxPoll, RunnerError> {
        let unit = unit_name(identity)?;
        let view = show(&unit)?;
        if matches!(
            view.active.as_str(),
            "active" | "activating" | "deactivating"
        ) {
            return Ok(SandboxPoll::Running);
        }
        if !absent(&view) {
            return Err(RunnerError::Ambiguous);
        }
        let proved = prove_absent(&unit)?;
        let stage = self.root.join(&identity.lease);
        let (mut exit_code, output) = read_output(&stage, 65536)?;
        if view.exit_code != 0 {
            exit_code = view.exit_code;
        }
        Ok(SandboxPoll::Exited {
            stopped: receipt(identity, &proved)?,
            exit_code,
            output,
        })
    }

    fn terminate(&mut self, identity: &ExecutionIdentity) -> Result<StoppedReceipt, RunnerError> {
        let unit = unit_name(identity)?;
        let view = prove_absent(&unit)?;
        receipt(identity, &view)
    }
}

#[cfg(test)]
pub(crate) fn candidate_pure_contract() {
    use nivra_store::research_jobs as jobs;
    use std::os::unix::fs::PermissionsExt;

    let lease = "a".repeat(64);
    let helper_sha = hash_bytes(b"candidate-pure-helper");
    let unit_policy = jobs::CandidateUnitPolicy {
        version: 1,
        helper_sha256: helper_sha.clone(),
        helper_abi: "candidate_helper_v1".into(),
        unit_tasks: 16,
        unit_memory_bytes: 512 * 1024 * 1024,
        unit_cpu_millis: 120_000,
        unit_wall_millis: 120_000,
        unit_open_files: 128,
        max_result_bytes: jobs::CANDIDATE_RESULT_MAX_BYTES,
        restart: "no".into(),
        payload_max_processes: 16,
        payload_max_open_files: 32,
        kill_mode: "control-group".into(),
        proof_abi: "candidate_unit_proof_v1".into(),
    };
    let sandbox = jobs::CandidateSandboxPolicy {
        abi: "scorer_replay_v1".into(),
        engine: "sadr_sandbox::score_source".into(),
        network: false,
        cpu_seconds: 1,
        memory_mb: 64,
        wall_ms: 1_000,
        max_output_bytes: 1024,
        max_keys: 64,
        max_processes: 16,
        max_open_files: 32,
        executable_scope: "python_and_bwrap_content_only".into(),
        python_sha256: "0".repeat(64),
        bwrap_sha256: "1".repeat(64),
    };
    let limits = jobs::ResourceCaps {
        cpu_millis: 120_000,
        tasks: 1,
        workspace_bytes: 1_048_576,
        file_bytes: 65_536,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024,
        wall_millis: 120_000,
    };
    let runtime_binding = jobs::CandidateRuntimeBinding {
        kind: "source_fingerprint".into(),
        abi: "scorer_replay_v1".into(),
        engine: "sadr_sandbox::score_source".into(),
        scorer_compare_sha256: "2".repeat(64),
        sandbox_source_sha256: "3".repeat(64),
        helper_sha256: helper_sha.clone(),
        unit_policy_sha256: unit_policy.policy_sha256(),
        runtime_hash: "4".repeat(64),
    };
    let root = std::path::PathBuf::from("/dev/shm")
        .join(format!("nivra-candidate-pure-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("helpers")).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(root.join("helpers"), fs::Permissions::from_mode(0o700)).unwrap();
    let helper = root.join("helpers").join(&helper_sha);
    fs::write(&helper, b"candidate-pure-helper").unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o500)).unwrap();
    let stage = root.join(format!("candidate-{lease}"));
    fs::create_dir_all(&stage).unwrap();
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700)).unwrap();
    let launch = CandidateLaunch {
        version: 2,
        job_id: "b".repeat(64),
        lease: lease.clone(),
        task_id: "c".repeat(64),
        revision: 1,
        candidate_id: "d".repeat(64),
        lifecycle_candidate_id: "e".repeat(64),
        source_sha256: "f".repeat(64),
        params_sha256: "0".repeat(64),
        dataset_id: "dataset".into(),
        dataset_hash: "1".repeat(64),
        abi: "scorer_replay_v1".into(),
        runtime_binding,
        sandbox,
        unit_policy: unit_policy.clone(),
        limits,
        replay_json: "{}".into(),
        stage_dir: stage.to_string_lossy().into_owned(),
        helper_path: helper.to_string_lossy().into_owned(),
        python_path: root
            .join("executables")
            .join("python")
            .to_string_lossy()
            .into_owned(),
        bwrap_path: root
            .join("executables")
            .join("bwrap")
            .to_string_lossy()
            .into_owned(),
        unit: format!("nivra-candidate-{lease}"),
        machine: "pure-machine".into(),
        execution: None,
        observation: None,
        started_ms: 1_000,
        deadline_ms: 121_000,
        task_deadline_ms: 121_000,
    };
    let plan = candidate_command_plan(&launch, &root).unwrap();
    assert_eq!(plan.unit, launch.unit);
    assert_eq!(plan.argv.first().map(String::as_str), Some(SYSTEMD_RUN));
    for expected in [
        format!("MemoryMax={}", unit_policy.unit_memory_bytes),
        "MemorySwapMax=0".into(),
        "CPUQuota=100.00%".into(),
        format!("TasksMax={}", unit_policy.unit_tasks),
        format!("RuntimeMaxSec={}ms", unit_policy.unit_wall_millis),
        format!("LimitNOFILE={}", unit_policy.unit_open_files),
        "KillMode=control-group".into(),
        "TimeoutStopSec=2s".into(),
        "NoNewPrivileges=yes".into(),
        "Restart=no".into(),
        "PrivateTmp=yes".into(),
        "UMask=0077".into(),
        "UnsetEnvironment=*".into(),
        format!("WorkingDirectory={}", launch.stage_dir),
        launch.helper_path.clone(),
        "candidate-supervise".into(),
        stage.join("launch.json").to_string_lossy().into_owned(),
    ] {
        assert!(
            plan.argv.iter().any(|arg| arg == &expected),
            "missing {expected} in {:?}",
            plan.argv
        );
    }
    assert!(!plan.argv.iter().any(|arg| arg.contains("/bin/sh")));
    assert!(!plan.argv.iter().any(|arg| arg == "--share-net"));

    // A different unit name or lease never addresses another attempt.
    let mut wrong_unit = launch.clone();
    wrong_unit.unit = format!("nivra-candidate-{}", "b".repeat(64));
    assert_eq!(
        candidate_command_plan(&wrong_unit, &root).unwrap_err(),
        RunnerError::Unavailable
    );
    let mut wrong_lease = launch.clone();
    wrong_lease.lease = "b".repeat(64);
    assert_eq!(
        candidate_command_plan(&wrong_lease, &root).unwrap_err(),
        RunnerError::Unavailable
    );
    // A symlinked stage is refused rather than followed.
    let link = root.join("linked-stage");
    std::os::unix::fs::symlink(&stage, &link).unwrap();
    let mut linked = launch.clone();
    linked.stage_dir = link.to_string_lossy().into_owned();
    assert_eq!(
        candidate_command_plan(&linked, &root).unwrap_err(),
        RunnerError::Unavailable
    );
    // A helper with the wrong mode is refused.
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        candidate_command_plan(&launch, &root).unwrap_err(),
        RunnerError::Unavailable
    );
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o500)).unwrap();

    // Show parsing distinguishes a loaded unit, an unset TasksCurrent marker
    // and missing limit observations.
    let loaded = parse_show_properties(
        "ActiveState=inactive\nMainPID=0\nTasksCurrent=0\nExecMainStatus=0\n\
         ControlGroup=/user.slice/ours.service\nMemoryMax=536870912\nMemorySwapMax=0\n\
         CPUQuotaPerSecUSec=1s\nTasksMax=16\nRuntimeMaxUSec=2min\nLimitNOFILE=128\n\
         KillMode=control-group\nNoNewPrivileges=yes\nTimeoutStopUSec=2s\nRestart=no\n",
    )
    .unwrap();
    assert_eq!(loaded.tasks, Some(0));
    assert_eq!(loaded.observation.restart.as_deref(), Some("no"));
    assert!(loaded.observation.complete());
    assert!(loaded.observation.matches(&unit_policy));
    let unset = parse_show_properties(
        "ActiveState=inactive\nMainPID=0\nTasksCurrent=[not set]\nControlGroup=\n\
         Restart=no\n",
    )
    .unwrap();
    assert_eq!(unset.tasks, None);
    assert!(!unset.observation.complete());
    let missing_restart =
        parse_show_properties("ActiveState=inactive\nMainPID=0\nTasksCurrent=0\nControlGroup=\n")
            .unwrap();
    assert_eq!(missing_restart.observation.restart, None);
    assert!(!missing_restart.observation.complete());

    assert_eq!(
        classify_show_failure("foo.service", 1, b"Unit foo.service could not be found."),
        ShowFailure::UnitMissing
    );
    assert_eq!(
        classify_show_failure(
            "nivra-candidate-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.service",
            1,
            b"Failed to connect to bus: Permission denied",
        ),
        ShowFailure::Unavailable
    );
    assert_eq!(
        classify_show_failure(
            "nivra-candidate-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.service",
            1,
            b"unexpected manager error",
        ),
        ShowFailure::Ambiguous
    );
    assert_eq!(
        classify_cgroup_events(Ok("populated 0\n".into())).unwrap(),
        Some("populated 0\n".into())
    );
    assert_eq!(
        classify_cgroup_events(Err(std::io::Error::from(std::io::ErrorKind::NotFound))).unwrap(),
        None
    );
    assert_eq!(
        classify_cgroup_events(Err(std::io::Error::from(
            std::io::ErrorKind::PermissionDenied
        )))
        .unwrap_err(),
        RunnerError::Ambiguous
    );

    assert_eq!(
        candidate_readiness_for(0, |_| true, true, true, Some("fixture-machine")).unwrap_err(),
        "candidate_readiness_root"
    );
    assert_eq!(
        candidate_readiness_for(1000, |_| false, true, true, Some("fixture-machine")).unwrap_err(),
        "candidate_readiness_systemd_run"
    );
    assert_eq!(
        candidate_readiness_for(1000, |_| true, false, true, Some("fixture-machine")).unwrap_err(),
        "candidate_readiness_cgroup_v2"
    );
    assert_eq!(
        candidate_readiness_for(1000, |_| true, true, false, Some("fixture-machine")).unwrap_err(),
        "candidate_readiness_user_bus"
    );
    assert!(candidate_readiness_for(1000, |_| true, true, true, Some("fixture-machine")).is_ok());
    assert_eq!(
        candidate_unit_name(&lease).as_deref(),
        Some(launch.unit.as_str())
    );

    // A failed show is absent only for the exact requested unit. Bus,
    // transport and permission failures are unavailable and retain the lease;
    // a generic "no such file" is never unit absence.
    let expected = format!("nivra-candidate-{lease}.service");
    let other = format!("nivra-candidate-{}.service", "b".repeat(64));
    for (stderr, expected_failure) in [
        (
            "Failed to connect to bus: No such file or directory",
            ShowFailure::Unavailable,
        ),
        (
            "Failed to connect to bus: Permission denied",
            ShowFailure::Unavailable,
        ),
        (
            "Failed to connect to bus: Connection refused",
            ShowFailure::Unavailable,
        ),
        (
            "Failed to connect to bus: No medium found",
            ShowFailure::Unavailable,
        ),
        (
            "System has not been booted with systemd as init system (PID 1). Can't operate.",
            ShowFailure::Unavailable,
        ),
        ("No such file or directory", ShowFailure::Ambiguous),
        ("", ShowFailure::Ambiguous),
        ("unexpected manager error", ShowFailure::Ambiguous),
    ] {
        assert_eq!(
            classify_show_failure(&expected, 1, stderr.as_bytes()),
            expected_failure,
            "stderr={stderr:?}"
        );
    }
    assert_eq!(
        classify_show_failure(
            &expected,
            1,
            format!("Unit {expected} could not be found.").as_bytes()
        ),
        ShowFailure::UnitMissing
    );
    assert_eq!(
        classify_show_failure(
            &expected,
            1,
            format!("Unit {other} could not be found.").as_bytes()
        ),
        ShowFailure::Ambiguous
    );
    let not_found = std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(0),
        stdout: format!(
            "Id={expected}\nLoadState=not-found\nActiveState=inactive\nMainPID=0\n\
             TasksCurrent=0\nControlGroup=\n"
        )
        .into_bytes(),
        stderr: Vec::new(),
    };
    let view = show_output(&expected, not_found, |_: &str| Ok(None)).unwrap();
    assert_eq!(view.active, "inactive");
    assert_eq!(view.main_pid, 0);
    let wrong_id = std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(0),
        stdout: format!(
            "Id={other}\nLoadState=not-found\nActiveState=inactive\nMainPID=0\n\
             TasksCurrent=0\nControlGroup=\n"
        )
        .into_bytes(),
        stderr: Vec::new(),
    };
    assert_eq!(
        show_output(&expected, wrong_id, |_: &str| Ok(None)).unwrap_err(),
        RunnerError::Ambiguous
    );
    let bus = std::process::Output {
        status: std::os::unix::process::ExitStatusExt::from_raw(1 << 8),
        stdout: Vec::new(),
        stderr: b"Failed to connect to bus: No such file or directory".to_vec(),
    };
    assert_eq!(
        show_output(&expected, bus, |_: &str| Ok(None)).unwrap_err(),
        RunnerError::Unavailable
    );
    assert_eq!(
        candidate_readiness_for(1000, |_| true, true, true, None).unwrap_err(),
        "candidate_readiness_machine_identity"
    );

    // systemd prints fractional and compound spans. The parser must be exact,
    // and the observation must match the same rounded hundredths as the
    // command's CPUQuota rather than a separately rounded float.
    for (text, expected) in [
        ("416.600ms", 416_600u64),
        ("1min 30s", 90_000_000),
        ("2min 3.456000s", 123_456_000),
        ("1h 2min 3.456000s", 3_723_456_000),
        ("1.5s", 1_500_000),
        ("500us", 500),
        ("500µs", 500),
        ("500μs", 500),
        ("1d", 86_400_000_000),
        ("1w", 604_800_000_000),
        ("0.000001s", 1),
        ("1min 30.5s", 90_500_000),
    ] {
        assert_eq!(parse_systemd_usec(text), Some(expected), "text={text:?}");
    }
    for text in [
        "",
        "infinity",
        "[not set]",
        "[uninitialized]",
        "n/a",
        "unset",
        "18446744073709551615s",
        "18446744073709551616us",
        "1.0000001s",
        "1.5us",
        "-1s",
        "1..5s",
        "not-a-duration",
    ] {
        assert_eq!(parse_systemd_usec(text), None, "text={text:?}");
    }
    assert_eq!(quota_hundredths(50_000, 120_000), Some(4_166));
    assert_eq!(crate::worker::quota_usec(50_000, 120_000), Some(416_600));
    assert_eq!(candidate_quota(50_000, 120_000).as_deref(), Some("41.66%"));
    assert_eq!(
        candidate_quota(120_000, 120_000).as_deref(),
        Some("100.00%")
    );
    assert_eq!(candidate_quota(1, 600_000), None);
    let quota_policy = jobs::CandidateUnitPolicy {
        unit_cpu_millis: 50_000,
        unit_wall_millis: 120_000,
        ..unit_policy.clone()
    };
    let loaded = parse_show_properties(&format!(
        "Id={expected}\nLoadState=loaded\nActiveState=inactive\nMainPID=0\nTasksCurrent=0\n\
         ExecMainStatus=0\nControlGroup=/user.slice/ours.service\nMemoryMax=536870912\n\
         MemorySwapMax=0\nCPUQuotaPerSecUSec=416.600ms\nTasksMax=16\nRuntimeMaxUSec=2min\n\
         LimitNOFILE=128\nKillMode=control-group\nNoNewPrivileges=yes\nTimeoutStopUSec=2s\n\
         Restart=no\n"
    ))
    .unwrap();
    assert!(loaded.observation.complete());
    assert!(
        loaded.observation.matches(&quota_policy),
        "observation {:?}",
        loaded.observation
    );

    let _ = fs::remove_dir_all(&root);
}
