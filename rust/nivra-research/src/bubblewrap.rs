//! Bubblewrap inside one transient systemd user unit.
//! Fixed argument arrays only. No shell, network, home, or subprocess fallback.
//! A successful fixture here does not qualify the host.
use crate::worker::{
    ExecutionIdentity, Launch, ResourceLimits, RunnerError, SandboxPoll, SandboxRunner,
    StoppedReceipt, quota_hundredths,
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
    let stage = stage.as_os_str().to_str().ok_or(RunnerError::Unavailable)?;
    if launch.spec.capability == nivra_store::research_jobs::SpecCapability::ArchiveBars {
        // Only the fixed Rust worker runs with archive access. It launches
        // Python inside the separate no-network strategy sandbox.
        argv.pop(); // Insert the properties before the command separator.
        argv.extend([
            "--property".into(),
            "UMask=0077".into(),
            "--".into(),
            "/usr/bin/env".into(),
            "-i".into(),
            format!("{stage}/runtime"),
            "--archive-bars".into(),
            stage.into(),
        ]);
        return Ok(CommandPlan { unit, argv });
    }
    argv.extend(bwrap.into_iter().map(str::to_owned));
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
    exit_code: i32,
}
impl UnitView {
    fn missing() -> Self {
        Self {
            active: "inactive".into(),
            main_pid: 0,
            tasks: None,
            control_group: String::new(),
            exit_code: 0,
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
}

fn parse_show_properties(text: &str) -> Result<ShowProperties, RunnerError> {
    let mut active = None;
    let mut main_pid = None;
    let mut tasks = None;
    let mut control_group = None;
    let mut exit_code = None;
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
    })
}

fn show_output(
    expected_service: &str,
    output: std::process::Output,
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
    Ok(UnitView {
        active: properties.active,
        main_pid: properties.main_pid,
        tasks: properties.tasks,
        control_group: properties.control_group,
        exit_code: properties.exit_code,
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
    show_output(&service, output)
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
