//! Offline Python scoring only. Never starts sessions or selects a runtime strategy.
pub mod library;
pub mod orchestrator_policy;
pub mod selector;
use sadr_core::strategy::{FeatureSnapshot, ScanSnapshot, StrategySpec, evaluate};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScorerSandboxConfig {
    pub python_path: PathBuf,
    pub bwrap_path: PathBuf,
    pub cpu_seconds: u64,
    pub memory_mb: u64,
    pub wall_ms: u64,
    pub max_output_bytes: usize,
    pub max_keys: usize,
    /// Payload ceiling; seccomp currently enforces the stricter single-process bound.
    pub max_processes: u64,
    pub max_open_files: u64,
    /// Explicit opt-in network namespace sharing (#222). Default off; the
    /// orchestrator workspace may enable it for reviewed scanner scripts, while
    /// the scorer trigger refuses it. Serialized only when enabled so existing
    /// configuration bytes and policy hashes stay unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub network: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
}
impl Default for ScorerSandboxConfig {
    fn default() -> Self {
        Self {
            python_path: "/usr/bin/python3".into(),
            bwrap_path: "/usr/bin/bwrap".into(),
            cpu_seconds: 1,
            memory_mb: 128,
            wall_ms: 2000,
            max_output_bytes: 65536,
            max_keys: 4096,
            max_processes: 64,
            max_open_files: 32,
            network: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    Config,
    Input,
    Setup,
    Unavailable,
    Exit,
    WallTimeout,
    OutputLimit,
    OutputInvalid,
}
impl ScorerSandboxConfig {
    pub fn validate(&self) -> Result<(), Failure> {
        if !self.python_path.is_absolute()
            || !self.bwrap_path.is_absolute()
            || !(1..=30).contains(&self.cpu_seconds)
            || !(32..=512).contains(&self.memory_mb)
            || !(1..=30000).contains(&self.wall_ms)
            || !(1..=262144).contains(&self.max_output_bytes)
            || !(1..=4096).contains(&self.max_keys)
            || !(8..=256).contains(&self.max_processes)
            || !(16..=128).contains(&self.max_open_files)
        {
            return Err(Failure::Config);
        }
        Ok(())
    }
}
pub type Scores = BTreeMap<String, f64>;
pub type Features = BTreeMap<String, serde_json::Value>;

// Reject duplicate keys as well as non-numeric JSON values and trailing data.
struct StrictScores(Scores);
impl<'de> Deserialize<'de> for StrictScores {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = StrictScores;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("unique finite score map")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut m: M,
            ) -> Result<Self::Value, M::Error> {
                let mut scores = Scores::new();
                while let Some((k, v)) = m.next_entry::<String, f64>()? {
                    if !v.is_finite() || scores.insert(k, v).is_some() {
                        return Err(serde::de::Error::custom("invalid score"));
                    }
                }
                Ok(StrictScores(scores))
            }
        }
        d.deserialize_map(Visitor)
    }
}
fn output(bytes: &[u8], input: &Features, c: &ScorerSandboxConfig) -> Result<Scores, Failure> {
    if bytes.len() > c.max_output_bytes {
        return Err(Failure::OutputLimit);
    }
    let StrictScores(scores) = serde_json::from_slice(bytes).map_err(|_| Failure::OutputInvalid)?;
    if scores.len() > c.max_keys || scores.keys().any(|k| !input.contains_key(k)) {
        return Err(Failure::OutputInvalid);
    }
    Ok(scores)
}

fn fork_filter() -> Result<Vec<u8>, Failure> {
    #[cfg(not(target_arch = "x86_64"))]
    {
        Err(Failure::Setup)
    }
    #[cfg(target_arch = "x86_64")]
    {
        let mut instructions: Vec<(u16, u8, u8, u32)> = vec![
            (0x20, 0, 0, 4), // seccomp_data.arch
            (0x15, 1, 0, 0xc000003e),
            (0x06, 0, 0, 0x80000000), // kill wrong architecture
            (0x20, 0, 0, 0),          // syscall number
            (0x35, 0, 1, 0x40000000), // reject x32 ABI
            (0x06, 0, 0, 0x80000000),
        ];
        for number in [
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_fork,
            libc::SYS_vfork,
        ] {
            instructions.push((0x15, 0, 1, number as u32));
            instructions.push((0x06, 0, 0, 0x00050000 | libc::EPERM as u32));
        }
        instructions.push((0x06, 0, 0, 0x7fff0000));
        let mut bytes = Vec::new();
        for (code, jt, jf, k) in instructions {
            bytes.extend(code.to_ne_bytes());
            bytes.extend([jt, jf]);
            bytes.extend(k.to_ne_bytes());
        }
        Ok(bytes)
    }
}

/// Opt-in network sharing (#222). `--share-net` keeps every other namespace
/// unshared; only the resolver, hosts and CA material the payload needs to make
/// its own reviewed outbound calls are mounted read-only. No host root, home,
/// credential or application directory becomes reachable.
fn network_arguments() -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec!["--share-net".into()];
    args.push("--dir".into());
    args.push("/etc".into());
    for path in [
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/services",
        "/etc/ssl",
        "/etc/pki",
        "/etc/ca-certificates",
        "/usr/share/ca-certificates",
        "/usr/lib/ssl",
    ] {
        let source = Path::new(path);
        // Hosts differ; only mount material that actually exists.
        if source.exists() {
            args.push("--ro-bind".into());
            args.push(source.into());
            args.push(source.into());
        }
    }
    args
}

/// File ABI: Python runs with -I -S -B; argv[1] is immutable JSON input.
/// Print exactly one JSON object mapping a subset of input keys to finite scores.
pub fn score(
    c: &ScorerSandboxConfig,
    scorer: &Path,
    features: &Features,
) -> Result<Scores, Failure> {
    c.validate()?;
    // Snapshot bytes: caller replacement cannot change the mounted program/input.
    let source = fs::File::open(scorer).map_err(|_| Failure::Input)?;
    let mut bytes = Vec::new();
    source
        .take(262145)
        .read_to_end(&mut bytes)
        .map_err(|_| Failure::Input)?;
    if bytes.len() > 262144 {
        return Err(Failure::Input);
    }
    score_source(c, &bytes, features)
}

/// Advisory local status for one bwrap leader. The private PID namespace and
/// `--die-with-parent` are real containment, but this receipt is not a
/// race-free proof of whole-attempt absence. A release decision must come from
/// the owned unit/cgroup proof, never from these fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BwrapLeaderReceipt {
    pub version: u32,
    pub started: bool,
    pub leader_reaped: bool,
    /// Always false: a post-reap process-group probe races with PID reuse and is
    /// deliberately not performed. The field exists to keep the advisory shape
    /// explicit rather than implying an observed-empty group.
    pub group_gone: bool,
    pub cleanup: String,
    pub waited: bool,
    pub advisory: bool,
}
impl BwrapLeaderReceipt {
    pub fn not_started() -> Self {
        Self {
            version: 2,
            started: false,
            leader_reaped: false,
            group_gone: false,
            cleanup: "not_required_no_sandbox_started".into(),
            waited: false,
            advisory: true,
        }
    }
    /// Local diagnostic only. Never use this to release a durable lease.
    pub fn advisory_ok(&self) -> bool {
        self.version == 2 && self.started && self.leader_reaped && self.waited && self.advisory
    }
}
#[derive(Clone, Debug)]
pub struct ScorerExecution {
    pub result: Result<Scores, Failure>,
    pub receipt: BwrapLeaderReceipt,
}
#[allow(dead_code)]
struct Prepared {
    cmd: Command,
    _filter: std::fs::File,
    _dir: tempfile::TempDir,
}
fn prepare(
    c: &ScorerSandboxConfig,
    source: &[u8],
    features: &Features,
) -> Result<Prepared, Failure> {
    c.validate()?;
    if source.len() > 262144 {
        return Err(Failure::Input);
    }
    if features.len() > c.max_keys || features.keys().any(|k| k.is_empty() || k.len() > 512) {
        return Err(Failure::Input);
    }
    let input = serde_json::to_vec(features).map_err(|_| Failure::Input)?;
    if input.len() > 4 * 1024 * 1024 {
        return Err(Failure::Input);
    }
    let dir = tempfile::tempdir().map_err(|_| Failure::Setup)?;
    let program = dir.path().join("scorer.py");
    let data = dir.path().join("input.json");
    fs::write(&program, source).map_err(|_| Failure::Setup)?;
    fs::write(&data, input).map_err(|_| Failure::Setup)?;
    let python = fs::canonicalize(&c.python_path).map_err(|_| Failure::Setup)?;
    // bwrap installs this filter only in the payload, after its own setup forks.
    // Deny all payload process creation independently of host UID process counts.
    // Do not set RLIMIT_NPROC: it can prevent bwrap setup forks on busy hosts.
    let filter_bytes = fork_filter()?;
    let mut filter = tempfile::tempfile().map_err(|_| Failure::Setup)?;
    use std::io::{Seek, Write};
    filter
        .write_all(&filter_bytes)
        .map_err(|_| Failure::Setup)?;
    filter.rewind().map_err(|_| Failure::Setup)?;
    use std::os::fd::AsRawFd;
    let filter_fd = filter.as_raw_fd();
    let mut cmd = Command::new(&c.bwrap_path);
    cmd.arg("--seccomp").arg(filter_fd.to_string());
    cmd.env_clear().args([
        "--unshare-all",
        "--clearenv",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--setenv",
        "PATH",
        "/usr/bin",
        "--setenv",
        "LANG",
        "C",
        "--dir",
        "/usr",
        "--dir",
        "/usr/bin",
        "--dir",
        "/usr/lib",
    ]);
    if c.network {
        cmd.args(network_arguments());
    }
    // Only interpreter, stdlib and dynamic libraries; no host root/home/tmp mounts.
    cmd.arg("--ro-bind").arg(python).arg("/usr/bin/python3");
    for entry in fs::read_dir("/usr/lib").map_err(|_| Failure::Setup)? {
        let p = entry.map_err(|_| Failure::Setup)?.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if (name.starts_with("python3.") || name.ends_with("-linux-gnu")) && p.is_dir() {
            cmd.arg("--ro-bind").arg(&p).arg(&p);
        }
    }
    cmd.args(["--symlink", "usr/lib", "/lib"]);
    if Path::new("/usr/lib64").exists() {
        // On merged-lib hosts the loader searches /usr/lib, while the
        // read-only library tree is mounted at /usr/lib64 in this sandbox.
        if fs::canonicalize("/usr/lib64").ok().as_deref() == Some(Path::new("/usr/lib")) {
            cmd.args(["--setenv", "LD_LIBRARY_PATH", "/usr/lib64"]);
        }
        cmd.args([
            "--ro-bind",
            "/usr/lib64",
            "/usr/lib64",
            "--symlink",
            "usr/lib64",
            "/lib64",
        ]);
    }
    cmd.arg("--ro-bind")
        .arg(&program)
        .arg("/scorer.py")
        .arg("--ro-bind")
        .arg(&data)
        .arg("/input.json");
    cmd.args([
        "--proc",
        "/proc",
        "--remount-ro",
        "/proc",
        "--remount-ro",
        "/",
        "--chdir",
        "/",
        "--",
        "/usr/bin/python3",
        "-I",
        "-S",
        "-B",
        "/scorer.py",
        "/input.json",
    ]);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let limits = [
        (libc::RLIMIT_CPU, c.cpu_seconds),
        (libc::RLIMIT_AS, c.memory_mb * 1024 * 1024),
        (libc::RLIMIT_FSIZE, 0),
        (libc::RLIMIT_NOFILE, c.max_open_files),
    ];
    // SAFETY: the child hook only invokes async-signal-safe libc calls, touches
    // captured plain integers, and returns OS errors. No allocation or locks.
    unsafe {
        cmd.pre_exec(move || {
            if libc::fcntl(filter_fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if libc::setpgid(0, 0) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            for (resource, value) in limits {
                let limit = libc::rlimit {
                    rlim_cur: value,
                    rlim_max: value,
                };
                if libc::setrlimit(resource, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(Prepared {
        cmd,
        _filter: filter,
        _dir: dir,
    })
}

fn run_child(
    c: &ScorerSandboxConfig,
    features: &Features,
    child: &mut std::process::Child,
) -> (Result<Scores, Failure>, bool) {
    use std::os::fd::AsRawFd;
    let Some(mut pipe) = child.stdout.take() else {
        return (Err(Failure::Setup), false);
    };
    // Nonblocking read keeps output floods and silent hangs under the same deadline.
    // SAFETY: pipe is a live owned fd; flags do not change its ownership.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
        return (Err(Failure::Setup), false);
    }
    let start = Instant::now();
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0; 4096];
        match pipe.read(&mut buf) {
            Ok(n) => {
                bytes.extend_from_slice(&buf[..n]);
                if bytes.len() > c.max_output_bytes {
                    return (Err(Failure::OutputLimit), false);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
            Err(_) => return (Err(Failure::OutputInvalid), false),
        }
        if start.elapsed() >= Duration::from_millis(c.wall_ms) {
            return (Err(Failure::WallTimeout), false);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // Drain final bytes, still bounded even if a descendant owns stdout.
                let mut tail = Vec::new();
                let _ = pipe
                    .by_ref()
                    .take(c.max_output_bytes as u64 + 1)
                    .read_to_end(&mut tail);
                bytes.extend(tail);
                return (
                    if status.success() {
                        output(&bytes, features, c)
                    } else {
                        Err(Failure::Exit)
                    },
                    true,
                );
            }
            Ok(None) => (),
            Err(_) => return (Err(Failure::Setup), false),
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn cleanup(child: &mut std::process::Child, pid: i32, normal_exit: bool) -> BwrapLeaderReceipt {
    // Signal the leader's process group only while the leader is still
    // unreaped, so the group id cannot have been reused. After `wait` the pid
    // and pgid are historical and must never be signalled or probed.
    let killed = if normal_exit {
        false
    } else {
        // SAFETY: this child owns the process group created in pre_exec and has
        // not been reaped yet; no memory is touched.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        true
    };
    let waited = child.wait().is_ok();
    BwrapLeaderReceipt {
        version: 2,
        started: true,
        leader_reaped: waited,
        group_gone: false,
        cleanup: if killed {
            "killed_leader_group_before_reap"
        } else {
            "leader_reaped_no_signal"
        }
        .into(),
        waited,
        advisory: true,
    }
}

/// Typed execution result. Pre-spawn validation/setup failures are
/// `NotStarted`; a spawned run carries advisory local status only. The result
/// is not downgraded by an unproven process-group probe: the owned unit/cgroup
/// proof is the release authority.
pub fn score_source_with_receipt(
    c: &ScorerSandboxConfig,
    source: &[u8],
    features: &Features,
) -> ScorerExecution {
    let mut prepared = match prepare(c, source, features) {
        Ok(prepared) => prepared,
        Err(failure) => {
            return ScorerExecution {
                result: Err(failure),
                receipt: BwrapLeaderReceipt::not_started(),
            };
        }
    };
    let mut child = match prepared.cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let failure = if error.kind() == std::io::ErrorKind::NotFound {
                Failure::Unavailable
            } else {
                Failure::Setup
            };
            return ScorerExecution {
                result: Err(failure),
                receipt: BwrapLeaderReceipt::not_started(),
            };
        }
    };
    let pid = child.id() as i32;
    #[cfg(test)]
    tests::LAST_PID.with(|slot| slot.set(pid));
    let (result, normal_exit) = run_child(c, features, &mut child);
    let receipt = cleanup(&mut child, pid, normal_exit);
    ScorerExecution { result, receipt }
}

/// Score already-loaded source bytes (for example an exact library version).
/// The caller cannot substitute a different path between lookup and execution.
/// This compatibility API delegates to the typed runner and fails closed if
/// cleanup cannot be proven.
pub fn score_source(
    c: &ScorerSandboxConfig,
    source: &[u8],
    features: &Features,
) -> Result<Scores, Failure> {
    score_source_with_receipt(c, source, features).result
}

#[derive(Debug)]
pub enum Ranking {
    Candidate(Scores),
    Default(ScanSnapshot),
}
#[derive(Debug)]
pub struct Outcome {
    pub ranking: Ranking,
    pub reason: Option<Failure>,
}
/// Default evaluation errors are returned explicitly; no invented ranking.
/// The default is computed by the core evaluator, never supplied by a scorer.
pub fn score_or_default(
    c: &ScorerSandboxConfig,
    scorer: &Path,
    snapshot: &FeatureSnapshot,
) -> Result<Outcome, String> {
    let default = evaluate(&StrategySpec::default(), snapshot)?;
    let features = snapshot
        .rows
        .iter()
        .map(|r| {
            (
                r.key.clone(),
                serde_json::to_value(&r.features).unwrap_or(serde_json::Value::Null),
            )
        })
        .collect();
    match score(c, scorer, &features) {
        Ok(scores) => Ok(Outcome {
            ranking: Ranking::Candidate(scores),
            reason: None,
        }),
        Err(reason) => Ok(Outcome {
            ranking: Ranking::Default(default),
            reason: Some(reason),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    thread_local! {
        pub(super) static LAST_PID: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
    }
    fn assert_reaped() {
        LAST_PID.with(|slot| {
            let pid = slot.get();
            assert!(pid > 0);
            // SAFETY: signal zero only checks existence; no signal is delivered.
            for target in [pid, -pid] {
                assert_eq!(unsafe { libc::kill(target, 0) }, -1);
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
            }
        });
    }
    fn snapshot() -> FeatureSnapshot {
        FeatureSnapshot {
            observed_ms: 0,
            lookback_samples: 20,
            rows: vec![sadr_core::strategy::FeatureRow {
                key: "a".into(),
                features: BTreeMap::new(),
            }],
        }
    }
    fn run(c: &ScorerSandboxConfig, source: &str) -> Outcome {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("candidate.py");
        fs::write(&file, source).unwrap();
        score_or_default(c, &file, &snapshot()).unwrap()
    }
    fn sandbox_ready(c: &ScorerSandboxConfig) -> bool {
        c.python_path.is_file()
            && c.bwrap_path.is_file()
            && Command::new(&c.bwrap_path)
                .args(["--unshare-all", "--ro-bind", "/", "/", "/usr/bin/true"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
    }
    fn skip_unavailable(c: &ScorerSandboxConfig, fixture: &str) -> bool {
        if sandbox_ready(c) {
            return false;
        }
        assert!(
            std::env::var_os("SADR_REQUIRE_SCORER_SANDBOX").is_none(),
            "scorer sandbox required but python3 or bubblewrap is unavailable"
        );
        eprintln!("skipping {fixture}: python3 or bwrap unavailable on host");
        true
    }
    fn fallback(out: Outcome, expected: Option<Failure>) {
        assert!(out.reason.is_some());
        if let Some(reason) = expected {
            assert_eq!(out.reason, Some(reason));
        }
        let Ranking::Default(scan) = out.ranking else {
            panic!("failure admitted candidate")
        };
        let default = evaluate(&StrategySpec::default(), &snapshot()).unwrap();
        assert_eq!(
            serde_json::to_value(scan).unwrap(),
            serde_json::to_value(default).unwrap()
        );
    }
    #[test]
    fn missing_bwrap_and_invalid_config_fail_closed() {
        let mut c = ScorerSandboxConfig {
            bwrap_path: "/nonexistent/sadr-bwrap".into(),
            ..Default::default()
        };
        fallback(run(&c, "print('{\"a\":1}')"), Some(Failure::Unavailable));
        c.bwrap_path = "/usr/bin/bwrap".into();
        c.python_path = "/nonexistent/sadr-python".into();
        fallback(run(&c, "print('{}')"), Some(Failure::Setup));
        c.memory_mb = 0;
        fallback(run(&c, "print('{}')"), Some(Failure::Config));
    }
    #[test]
    fn cost_aware_momentum_file_orders_leaders_and_rejects_ineligible() {
        let c = ScorerSandboxConfig::default();
        if skip_unavailable(&c, "cost-aware momentum fixture") {
            return;
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let params: serde_json::Value = serde_json::from_str(include_str!(
            "../../../strategies/cost_aware_momentum.params.json"
        ))
        .unwrap();
        let base = serde_json::json!({
            "price": 500.0, "move_bps": 150.0, "market_relative_bps": 180.0,
            "traded_value": 50_000_000.0, "circuit_distance_bps": 800.0,
            "volatility_bps": 120.0, "range_position": 0.9,
            "best_bid": 499.9, "best_ask": 500.0,
            "best_bid_qty": 500.0, "best_ask_qty": 600.0,
            "spread_fraction": 0.0002, "book_imbalance": 0.2,
            "session_vwap": 495.0, "rel_volume_tod": 2.5,
            "minutes_since_open": 120.0
        });
        let mut hot = base.clone();
        hot["move_bps"] = 300.0.into();
        hot["market_relative_bps"] = 330.0.into();
        hot["session_vwap"] = 490.0.into();
        hot["rel_volume_tod"] = 3.0.into();
        let mut down = base.clone();
        down["move_bps"] = (-150.0).into();
        down["market_relative_bps"] = (-120.0).into();
        down["session_vwap"] = 505.0.into();
        let mut wide = base.clone();
        wide["best_bid"] = 498.0.into();
        wide["spread_fraction"] = 0.004.into();
        let mut missing = base.clone();
        missing.as_object_mut().unwrap().remove("session_vwap");
        let mut input = BTreeMap::from([
            ("params".into(), params),
            ("GOOD".into(), base),
            ("HOT".into(), hot),
            ("DOWN".into(), down),
            ("WIDE".into(), wide),
            ("MISSING".into(), missing),
        ]);
        let file = root.join("strategies/cost_aware_momentum.py");
        let scores = score(&c, &file, &input).expect("sandbox scorer fixture failed");
        assert_eq!(
            score_source(
                &c,
                include_bytes!("../../../strategies/cost_aware_momentum.py"),
                &input
            )
            .unwrap(),
            scores
        );
        assert_eq!(scores.len(), 2);
        assert!(scores["HOT"] > scores["GOOD"]);
        assert!(scores["GOOD"] > 0.0);
        for key in ["DOWN", "WIDE", "MISSING", "params"] {
            assert!(!scores.contains_key(key));
        }
        // Input params, rather than an embedded Python policy, control admission.
        input.get_mut("params").unwrap()["max_spread_bps"] = 0.0.into();
        assert!(score(&c, &file, &input).unwrap().is_empty());
    }

    #[test]
    fn strict_output_contract_and_caps() {
        let c = ScorerSandboxConfig {
            max_output_bytes: 32,
            max_keys: 1,
            ..Default::default()
        };
        let input = BTreeMap::from([("a".into(), serde_json::json!([1]))]);
        assert_eq!(output(b"{\"a\":1}", &input, &c).unwrap()["a"], 1.0);
        for bytes in [
            b"NaN".as_slice(),
            b"{\"a\":NaN}",
            b"{\"a\":1e999}",
            b"{\"x\":1}",
            b"{\"a\":1,\"a\":2}",
            b"{} {}",
            b"[]",
            b"{\"a\":true}",
        ] {
            assert_eq!(output(bytes, &input, &c), Err(Failure::OutputInvalid));
        }
        assert_eq!(output(&[b' '; 33], &input, &c), Err(Failure::OutputLimit));
        assert_eq!(
            score_source(
                &ScorerSandboxConfig::default(),
                &vec![b' '; 262_145],
                &BTreeMap::new()
            ),
            Err(Failure::Input)
        );
    }
    fn containment_config() -> ScorerSandboxConfig {
        let c = ScorerSandboxConfig {
            cpu_seconds: 1,
            memory_mb: 48,
            wall_ms: 2500,
            max_output_bytes: 128,
            max_keys: 2,
            max_processes: 8,
            ..Default::default()
        };
        // Installed-but-broken containment must fail, never silently skip.
        assert!(
            run(&c, "print('{\"a\":1}')").reason.is_none(),
            "sandbox setup/execution failed"
        );
        c
    }
    #[test]
    fn network_is_off_by_default_and_environment_is_isolated() {
        // Network sharing is explicit config only (#222): the default config
        // omits the field from serialized bytes and keeps the payload isolated.
        let default = ScorerSandboxConfig::default();
        assert!(!default.network);
        assert!(
            serde_json::to_value(&default)
                .unwrap()
                .get("network")
                .is_none(),
            "disabled network must not change serialized configuration"
        );
        let enabled: ScorerSandboxConfig =
            serde_json::from_value(serde_json::json!({"network":true})).unwrap();
        assert!(enabled.network);
        enabled.validate().unwrap();
        let arguments = network_arguments();
        assert_eq!(arguments[0], "--share-net");
        assert!(!arguments.iter().any(|arg| arg == "--unshare-all"));
        // Mounts are read-only host resolver/CA material only, never the host root.
        for path in ["/etc/resolv.conf", "/etc/hosts", "/etc/ssl"] {
            if !Path::new(path).exists() {
                continue;
            }
            let source = std::ffi::OsString::from(path);
            assert!(
                arguments
                    .windows(3)
                    .any(|window| window[0] == "--ro-bind" && window[1] == source),
                "missing read-only mount for {path}"
            );
        }
        assert!(
            !arguments
                .windows(3)
                .any(|window| window[0] == "--ro-bind" && window[1] == "/"),
            "network mode must never bind the host root"
        );
        let c = containment_config();
        for host in ["1.1.1.1", "127.0.0.1"] {
            fallback(
                run(
                    &c,
                    &format!(
                        "import socket; s=socket.socket(); s.settimeout(.1); s.connect(({host:?},53)); print('{{}}')"
                    ),
                ),
                Some(Failure::Exit),
            );
        }
        // The sandbox environment is cleared even when the parent carries secrets.
        if std::env::var_os("SADR_SANDBOX_TEST_CHILD").is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tests::network_is_off_by_default_and_environment_is_isolated",
                    "--nocapture",
                ])
                .env("SADR_SANDBOX_TEST_CHILD", "1")
                .env("SECRET", "fake-parent-secret")
                .status()
                .unwrap();
            assert!(status.success(), "environment containment child failed");
            return;
        }
        assert_eq!(std::env::var("SECRET").unwrap(), "fake-parent-secret");
        let c = containment_config();
        fallback(
            run(&c, "import os; print(os.environ['SECRET'])"),
            Some(Failure::Exit),
        );
    }
    #[test]
    fn host_paths_are_isolated_and_read_only() {
        let c = containment_config();
        let secret = tempfile::NamedTempFile::new().unwrap();
        fs::write(secret.path(), "fixture-secret").unwrap();
        for path in ["/home", "/root"] {
            fallback(
                run(
                    &c,
                    &format!("import os; os.listdir({path:?}); print('{{}}')"),
                ),
                Some(Failure::Exit),
            );
        }
        let path = secret.path().to_str().unwrap();
        fallback(
            run(&c, &format!("open({path:?}).read()")),
            Some(Failure::Exit),
        );
        let c = containment_config();
        for path in ["created", "/tmp/created", "/created", "/scorer.py"] {
            fallback(
                run(&c, &format!("open({path:?}, 'w').write('changed')")),
                Some(Failure::Exit),
            );
        }
        let c = containment_config();
        fallback(
            run(&c, "open('/input.json', 'w').write('{}')"),
            Some(Failure::Exit),
        );
    }

    #[test]
    fn resource_limits_are_enforced() {
        let c = containment_config();
        fallback(
            run(
                &c,
                &format!(
                    "import resource\nassert resource.getrlimit(resource.RLIMIT_CPU)==({0},{0})\nwhile True: pass",
                    c.cpu_seconds
                ),
            ),
            Some(Failure::Exit),
        );
        let c = containment_config();
        fallback(
            run(
                &c,
                &format!("x=bytearray({})", (c.memory_mb + 1) * 1024 * 1024),
            ),
            Some(Failure::Exit),
        );
        let c = ScorerSandboxConfig {
            wall_ms: 100,
            ..containment_config()
        };
        let start = Instant::now();
        fallback(
            run(&c, "import time; time.sleep(10)"),
            Some(Failure::WallTimeout),
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert_reaped();
        let c = ScorerSandboxConfig {
            wall_ms: 300,
            ..containment_config()
        };
        // Every process creation attempt must be denied; an unexpected child
        // exits immediately, avoiding an actual bomb if containment regresses.
        let source = format!(
            r#"
import os, errno
for i in range({}):
 try:
  pid = os.fork()
 except OSError as e:
  assert e.errno == errno.EPERM
 else:
  if pid == 0: os._exit(1)
  os.waitpid(pid, 0)
  raise AssertionError('fork allowed')
assert len([p for p in os.listdir('/proc') if p.isdigit()]) <= {}
while True: pass
"#,
            c.max_processes * 4,
            c.max_processes
        );
        fallback(run(&c, &source), Some(Failure::WallTimeout));
        assert_reaped();
    }

    #[test]
    fn scorer_fixture_matrix_reports_documented_failure_codes() {
        // Known-good case: a real python3 process inside the sandbox scores the key.
        let c = containment_config();
        let good = run(&c, "import json,sys; print(json.dumps({'a': 1.0}))");
        assert!(
            good.reason.is_none(),
            "good fixture failed: {:?}",
            good.reason
        );
        assert!(matches!(good.ranking, Ranking::Candidate(_)));

        // Known-bad cases: every class the scorer trigger must treat as a failed
        // snapshot rather than admit a ranking or publish artifacts.
        for (source, expected) in [
            ("def broken(:", Failure::Exit),
            ("print('{\"a\":NaN}')", Failure::OutputInvalid),
            ("print('{\"a\":1e999}')", Failure::OutputInvalid),
            ("print('{\"not_in_input\":1}')", Failure::OutputInvalid),
            ("print('x'*1000)", Failure::OutputLimit),
            ("print('{\"a\":1,' )", Failure::OutputInvalid),
            ("raise RuntimeError('missing input key')", Failure::Exit),
        ] {
            fallback(run(&c, source), Some(expected));
        }

        let timed = ScorerSandboxConfig {
            wall_ms: 100,
            ..containment_config()
        };
        fallback(
            run(&timed, "import time; time.sleep(10)"),
            Some(Failure::WallTimeout),
        );
        fallback(
            run(
                &c,
                "import socket; s=socket.socket(); s.settimeout(.1); s.connect(('1.1.1.1',53)); print('{\"a\":1}')",
            ),
            Some(Failure::Exit),
        );
    }
}
