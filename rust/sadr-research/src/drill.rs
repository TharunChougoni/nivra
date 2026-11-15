//! Host containment drill for one registered probe. This does not qualify a VM,
//! certify a strategy, or enable trading. Missing bubblewrap, systemd, or a
//! static compiler returns unavailable. There is no subprocess fallback.
use crate::{
    AnalysisArgs, AnalysisTool, Dataset, DevelopmentSplit, RegisteredAnalysis, bubblewrap,
    worker::{
        self, DevelopmentSplit as Split, ExecutionIdentity, ExperimentReport, HostObservation,
        Launch, Metric, Outcome, RegisteredRun, ResourceLimits,
    },
};
use rusqlite::Connection;
use sadr_store::research_jobs::{self as jobs};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PROBE: &str = r#"
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>
static int slurp(const char *path, char *out, size_t cap) {
  FILE *file = fopen(path, "r");
  if (!file) return -1;
  size_t n = fread(out, 1, cap - 1, file);
  fclose(file);
  out[n] = 0;
  while (n > 0 && (out[n - 1] == '\n' || out[n - 1] == '\r')) out[--n] = 0;
  return 0;
}
static int grab(const char *json, const char *key, char *out) {
  char pattern[96];
  snprintf(pattern, sizeof pattern, "\"%s\":\"", key);
  const char *at = strstr(json, pattern);
  if (!at) return -1;
  at += strlen(pattern);
  const char *end = strchr(at, '"');
  if (!end || (size_t)(end - at) != 64) return -1;
  memcpy(out, at, 64);
  out[64] = 0;
  return 0;
}
static int reachable(const char *ip, int port) {
  int fd = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
  if (fd < 0) return 0;
  struct sockaddr_in address;
  memset(&address, 0, sizeof address);
  address.sin_family = AF_INET;
  address.sin_port = htons((unsigned short)port);
  inet_pton(AF_INET, ip, &address.sin_addr);
  int rc = connect(fd, (struct sockaddr *)&address, sizeof address);
  int visible = 0;
  if (rc == 0) visible = 1;
  else if (errno == EINPROGRESS) {
    struct pollfd wait = {.fd = fd, .events = POLLOUT};
    if (poll(&wait, 1, 400) > 0) {
      int err = 0;
      socklen_t len = sizeof err;
      getsockopt(fd, SOL_SOCKET, SO_ERROR, &err, &len);
      visible = err == 0;
    }
  }
  close(fd);
  return visible;
}
static int foreign_iface(void) {
  FILE *file = fopen("/proc/net/dev", "r");
  if (!file) return 0;
  char line[256];
  int header = 0, foreign = 0;
  while (fgets(line, sizeof line, file)) {
    if (header < 2) { header++; continue; }
    if (!strstr(line, "lo:")) foreign = 1;
  }
  fclose(file);
  return foreign;
}
static int exists(const char *path) { return access(path, F_OK) == 0; }
static void emit(const char *job, const char *lease, const char *reg, const char *code,
                  const char *runtime, const char *input, const char *metrics) {
  FILE *file = fopen("/outputs/result.json", "w");
  if (!file) _exit(1);
  fprintf(file,
          "{\"identity\":{\"job_id\":\"%s\",\"lease\":\"%s\",\"registration_sha256\":\"%s\"},"
          "\"script_hash\":\"%s\",\"runtime_hash\":\"%s\",\"input_hash\":\"%s\","
          "\"metrics\":[%s],\"observations\":[\"containment_probe\"]}",
          job, lease, reg, code, runtime, input, metrics);
  fclose(file);
}
int main(void) {
  char mode[64], launch[8192], job[65], lease[65], reg[65], code[65], runtime[65], input[65];
  if (slurp("/inputs/code", mode, sizeof mode) != 0) return 1;
  if (strcmp(mode, "symlink") == 0) {
    symlink("/etc/passwd", "/outputs/result.json");
    return 0;
  }
  if (strcmp(mode, "oversize") == 0) {
    FILE *file = fopen("/outputs/result.json", "w");
    if (!file) return 1;
    for (int i = 0; i < 70000; i++) fputc('A', file);
    fclose(file);
    return 0;
  }
  if (strcmp(mode, "memory") == 0) {
    size_t bytes = 64u * 1024u * 1024u;
    unsigned char *block = malloc(bytes);
    if (!block) return 1;
    for (size_t i = 0; i < bytes; i += 4096) block[i] = 1;
    if (slurp("/inputs/launch.json", launch, sizeof launch) != 0) return 1;
    if (grab(launch, "job_id", job) || grab(launch, "lease", lease) ||
        grab(launch, "registration_sha256", reg) || grab(launch, "script_hash", code) ||
        grab(launch, "runtime_hash", runtime) || grab(launch, "input_hash", input)) return 1;
    emit(job, lease, reg, code, runtime, input, "{\"name\":\"allocated_mb\",\"value\":\"64\"}");
    return 0;
  }
  if (slurp("/inputs/launch.json", launch, sizeof launch) != 0) return 1;
  if (grab(launch, "job_id", job) || grab(launch, "lease", lease) ||
      grab(launch, "registration_sha256", reg) || grab(launch, "script_hash", code) ||
      grab(launch, "runtime_hash", runtime) || grab(launch, "input_hash", input)) return 1;
  if (strcmp(mode, "fork") == 0) {
    int count = 0;
    pid_t kids[32];
    while (count < 32) {
      pid_t pid = fork();
      if (pid < 0) break;
      if (pid == 0) for (;;) pause();
      kids[count++] = pid;
    }
    for (int i = 0; i < count; i++) kill(kids[i], SIGKILL);
    for (int i = 0; i < count; i++) waitpid(kids[i], 0, 0);
    char metrics[96];
    snprintf(metrics, sizeof metrics, "{\"name\":\"fork_count\",\"value\":\"%d\"}", count);
    emit(job, lease, reg, code, runtime, input, metrics);
    return 0;
  }
  if (strcmp(mode, "descendant") == 0) {
    pid_t pid = fork();
    if (pid == 0) for (;;) pause();
    emit(job, lease, reg, code, runtime, input, "{\"name\":\"child_started\",\"value\":\"1\"}");
    return 0;
  }
  int net = foreign_iface() || reachable("1.1.1.1", 443) || reachable("169.254.169.254", 80);
  int home = exists("/home") || exists("/root");
  int host = exists("/etc/passwd") || exists("/usr/bin/gcc");
  int writable = open("/inputs/input", O_WRONLY | O_NOFOLLOW) >= 0;
  int workspace = open("/workspace/note", O_CREAT | O_WRONLY, 0600) >= 0;
  char metrics[256];
  snprintf(metrics, sizeof metrics,
           "{\"name\":\"net_visible\",\"value\":\"%d\"},{\"name\":\"home_visible\",\"value\":\"%d\"},"
           "{\"name\":\"host_file_visible\",\"value\":\"%d\"},{\"name\":\"input_writable\",\"value\":\"%d\"},"
           "{\"name\":\"workspace_writable\",\"value\":\"%d\"}",
           net, home, host, writable, workspace);
  emit(job, lease, reg, code, runtime, input, metrics);
  return 0;
}
"#;

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    memory_bytes: u64,
    tasks: u32,
    expect_success: bool,
}

const CASES: [Case; 6] = [
    Case {
        name: "visibility",
        memory_bytes: 64 * 1024 * 1024,
        tasks: 8,
        expect_success: true,
    },
    Case {
        name: "symlink",
        memory_bytes: 64 * 1024 * 1024,
        tasks: 8,
        expect_success: false,
    },
    Case {
        name: "oversize",
        memory_bytes: 64 * 1024 * 1024,
        tasks: 8,
        expect_success: false,
    },
    Case {
        name: "fork",
        memory_bytes: 64 * 1024 * 1024,
        tasks: 8,
        expect_success: true,
    },
    Case {
        name: "memory",
        memory_bytes: 32 * 1024 * 1024,
        tasks: 8,
        expect_success: false,
    },
    Case {
        name: "descendant",
        memory_bytes: 64 * 1024 * 1024,
        tasks: 8,
        expect_success: true,
    },
];

#[derive(Serialize)]
pub struct CaseResult {
    name: String,
    passed: bool,
}

#[derive(Serialize)]
pub struct DrillReport {
    pub sandbox_experiment: String,
    pub scientific_acceptance: String,
    pub certification: String,
    pub promotion: String,
    pub execution: String,
    pub live_enabled: bool,
    pub host_qualified: bool,
    pub cases: Vec<CaseResult>,
    pub missing: Vec<String>,
    pub kernel: String,
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}

fn kernel() -> String {
    let Ok(text) = fs::read_to_string("/proc/sys/kernel/osrelease") else {
        return "unknown".into();
    };
    let token = text.split_whitespace().next().unwrap_or("unknown");
    if token.len() > 64 || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
        "unknown".into()
    } else {
        token.to_owned()
    }
}

fn forbidden(name: &std::ffi::OsStr) -> bool {
    matches!(
        name.to_str(),
        Some("home" | "root" | ".ssh" | ".hermes" | ".grok" | ".codex" | ".openclaw" | ".config")
    )
}

fn root_ok(path: &Path) -> bool {
    path.is_absolute()
        && !path.components().any(|component| {
            matches!(component, Component::ParentDir | Component::CurDir)
                || matches!(component, Component::Normal(name) if forbidden(name))
        })
}

fn unavailable(status: &str) -> DrillReport {
    DrillReport {
        sandbox_experiment: status.into(),
        scientific_acceptance: "NOT_ACCEPTED".into(),
        certification: "NOT_CERTIFIED".into(),
        promotion: "NOT_AUTHORIZED".into(),
        execution: "NOT_AUTHORIZED".into(),
        live_enabled: false,
        host_qualified: false,
        cases: vec![],
        missing: [
            "preregistration",
            "out_of_sample",
            "cost_stress",
            "walk_forward",
            "holdout",
            "shared_core_backtest",
            "historical_bundle",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        kernel: kernel(),
    }
}

fn write_new(path: &Path, bytes: &[u8]) -> bool {
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => file,
        Err(_) => return false,
    };
    file.write_all(bytes).is_ok()
}

fn compile(root: &Path) -> Option<Vec<u8>> {
    let source = root.join("probe.c");
    let runtime = root.join("runtime");
    if !write_new(&source, PROBE.as_bytes()) {
        return None;
    }
    let status = Command::new("/usr/bin/gcc")
        .args([
            "-static",
            "-O2",
            "-std=c11",
            "-o",
            runtime.to_str()?,
            source.to_str()?,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let meta = fs::symlink_metadata(&runtime).ok()?;
    if !meta.is_file()
        || meta.file_type().is_symlink()
        || meta.len() == 0
        || meta.len() > 32 * 1024 * 1024
    {
        return None;
    }
    let bytes = fs::read(&runtime).ok()?;
    (bytes.len() as u64 == meta.len()).then_some(bytes)
}

fn plan_contains(launch: &Launch, root: &Path) -> bool {
    let Ok(plan) = bubblewrap::command_plan(launch, root) else {
        return false;
    };
    plan.argv.iter().any(|arg| arg == "--unshare-net")
        && plan.argv.iter().any(|arg| arg == "--unshare-user")
        && plan.argv.iter().any(|arg| arg == "--disable-userns")
        && !plan.argv.iter().any(|arg| arg.contains("/home"))
        && !plan
            .argv
            .iter()
            .any(|arg| arg == "--share-net" || arg.contains("/home") || arg.contains("/root"))
}

fn unit_absent(lease: &str) -> bool {
    let output = Command::new("/usr/bin/systemctl")
        .args([
            "--user",
            "show",
            "--property=ActiveState",
            "--property=MainPID",
            "--property=TasksCurrent",
            &format!("sadr-rw-{lease}.service"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    let Ok(text) = String::from_utf8(output.stdout) else {
        return false;
    };
    let mut active = None;
    let mut pid = None;
    let mut tasks = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        match key {
            "ActiveState" => active = Some(value),
            "MainPID" => pid = value.parse::<u64>().ok(),
            "TasksCurrent" => tasks = Some(value),
            _ => return false,
        }
    }
    matches!(active, Some("inactive" | "failed"))
        && pid == Some(0)
        && matches!(
            tasks,
            Some("0" | "" | "[not set]" | "[uninitialized]" | "n/a")
        )
}

fn metric(metrics: &[Metric], name: &str) -> Option<i64> {
    metrics
        .iter()
        .find(|metric| metric.name == name)
        .and_then(|metric| metric.value.parse().ok())
}

fn limits(case: Case) -> ResourceLimits {
    ResourceLimits {
        cpu_millis: 4000,
        tasks: case.tasks,
        workspace_bytes: 1_048_576,
        file_bytes: 4096,
    }
}

fn drive(
    db: &mut Connection,
    registered: &RegisteredRun,
    lease: &str,
    root: &Path,
) -> Option<Outcome> {
    let mut runner = bubblewrap::Runner::new(root.to_path_buf());
    let mut worker = worker::start_one(db, registered, lease, &mut runner, now_ms).ok()?;
    let deadline = now_ms().saturating_add(i64::from(registered.spec.wall_millis) + 20_000);
    loop {
        if now_ms() > deadline {
            return None;
        }
        match worker.poll(db, &mut runner, now_ms, false) {
            Ok(Outcome::Running | Outcome::AwaitingTermination { .. }) => {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(outcome) => return Some(outcome),
            Err(_) => return None,
        }
    }
}

fn execute_case(
    tool: &AnalysisTool,
    case: Case,
    runtime: &[u8],
    runtime_hash: &str,
    input: &[u8],
    input_hash: &str,
    root: &Path,
) -> (bool, Option<ExperimentReport>) {
    let Ok(queued) = tool.enqueue(AnalysisArgs {
        experiment_id: case.name.into(),
        analysis_id: case.name.into(),
        dataset_id: "probe".into(),
    }) else {
        return (false, None);
    };
    if queued.execution_started || queued.promotion_authorized {
        return (false, None);
    }
    let queue = tool.connection();
    let Ok(mut db) = queue.lock() else {
        return (false, None);
    };
    let Ok(job) = jobs::load(&mut db, &queued.job_id) else {
        return (false, None);
    };
    let registered = RegisteredRun {
        spec: job.spec.clone(),
        split: Split::Development,
        limits: limits(case),
        code: case.name.as_bytes().to_vec(),
        runtime: runtime.to_vec(),
        input: input.to_vec(),
    };
    let lease = sha(format!("lease-{}", case.name).as_bytes());
    let preview = Launch {
        identity: ExecutionIdentity {
            job_id: job.spec.id.clone(),
            lease: lease.clone(),
            registration_sha256: sha(b"preview"),
        },
        spec: job.spec,
        split: Split::Development,
        limits: limits(case),
        started_ms: 1,
        deadline_ms: 2,
    };
    if !plan_contains(&preview, root) {
        return (false, None);
    }
    let Some(outcome) = drive(&mut db, &registered, &lease, root) else {
        return (false, None);
    };
    let passed = case_passed(case, &outcome) && unit_absent(&lease);
    let measured = if case.name == "visibility"
        && let Outcome::Succeeded(result) = &outcome
    {
        Some(worker::accept_measured_experiment(
            result,
            &sha(case.name.as_bytes()),
            runtime_hash,
            input_hash,
            &HostObservation {
                network_unshared: true,
                home_unmounted: true,
                inputs_read_only: true,
                cgroup_absent: passed,
            },
        ))
    } else {
        None
    };
    (passed, measured)
}

fn case_passed(case: Case, outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Succeeded(result) if case.expect_success => match case.name {
            "visibility" => {
                metric(&result.metrics, "net_visible") == Some(0)
                    && metric(&result.metrics, "home_visible") == Some(0)
                    && metric(&result.metrics, "host_file_visible") == Some(0)
                    && metric(&result.metrics, "input_writable") == Some(0)
                    && metric(&result.metrics, "workspace_writable") == Some(1)
            }
            "fork" => {
                metric(&result.metrics, "fork_count").is_some_and(|count| (1..=16).contains(&count))
            }
            "descendant" => metric(&result.metrics, "child_started") == Some(1),
            _ => false,
        },
        Outcome::Succeeded(_) => false,
        Outcome::Terminated => !case.expect_success,
        Outcome::Running | Outcome::AwaitingTermination { .. } => false,
    }
}

pub fn run(root: &Path) -> DrillReport {
    run_with_runtime(root, None)
}
/// Import an operator-reviewed off-host probe, with its exact hash, to avoid
/// compiling on a trading VM. This still does not certify scientific acceptance.
pub fn run_with_runtime(root: &Path, provided: Option<(&Path, &str)>) -> DrillReport {
    if !root_ok(root) || root.exists() || fs::create_dir(root).is_err() {
        return unavailable("UNAVAILABLE");
    }
    if fs::set_permissions(root, fs::Permissions::from_mode(0o700)).is_err() {
        return unavailable("UNAVAILABLE");
    }
    let runtime = if let Some((path, expected)) = provided {
        fs::symlink_metadata(path)
            .ok()
            .filter(|m| {
                m.is_file()
                    && !m.file_type().is_symlink()
                    && m.len() > 0
                    && m.len() <= 32 * 1024 * 1024
            })
            .and_then(|_| fs::read(path).ok())
            .filter(|bytes| sha(bytes) == expected)
    } else {
        compile(root)
    };
    let Some(runtime) = runtime else {
        return unavailable("UNAVAILABLE");
    };
    let runtime_hash = sha(&runtime);
    let input = b"probe-input-v1".to_vec();
    let input_hash = sha(&input);
    let queue_path = root.join("queue.sqlite");
    let Ok(mut db) = Connection::open(&queue_path) else {
        return unavailable("UNAVAILABLE");
    };
    if db.pragma_update(None, "synchronous", "FULL").is_err() || jobs::initialize(&mut db).is_err()
    {
        return unavailable("UNAVAILABLE");
    }
    let mut analyses = vec![];
    for case in CASES {
        let script_hash = sha(case.name.as_bytes());
        if jobs::register_reviewed(&mut db, &script_hash, &runtime_hash).is_err() {
            return unavailable("UNAVAILABLE");
        }
        analyses.push(RegisteredAnalysis {
            id: case.name.into(),
            script_hash,
            runtime_hash: runtime_hash.clone(),
            memory_bytes: case.memory_bytes,
            output_bytes: 4096,
            wall_millis: 8000,
        });
    }
    let queue = Arc::new(Mutex::new(db));
    let mut cases = vec![];
    let mut measured: Option<ExperimentReport> = None;
    for (case, analysis) in CASES.into_iter().zip(analyses) {
        let name = case.name;
        let Ok(tool) = AnalysisTool::bind(
            sha(format!("drill-review-{name}").as_bytes()),
            Arc::clone(&queue),
            vec![analysis],
            vec![Dataset {
                id: "probe".into(),
                input_hash: input_hash.clone(),
                split: DevelopmentSplit::Development,
            }],
            Arc::new(now_ms),
        ) else {
            cases.push(CaseResult {
                name: name.into(),
                passed: false,
            });
            continue;
        };
        let (passed, acceptance) = execute_case(
            &tool,
            case,
            &runtime,
            &runtime_hash,
            &input,
            &input_hash,
            root,
        );
        if let Some(acceptance) = acceptance {
            measured = Some(acceptance);
        }
        cases.push(CaseResult {
            name: name.into(),
            passed,
        });
    }
    let all_passed = cases.iter().all(|case| case.passed);
    let acceptance = measured.unwrap_or_else(|| {
        worker::accept_measured_experiment(
            &worker::AnalysisResult {
                identity: ExecutionIdentity {
                    job_id: "0".repeat(64),
                    lease: "0".repeat(64),
                    registration_sha256: "0".repeat(64),
                },
                script_hash: "0".repeat(64),
                runtime_hash: "0".repeat(64),
                input_hash: "0".repeat(64),
                metrics: vec![],
                observations: vec![],
            },
            &sha(b"visibility"),
            &runtime_hash,
            &input_hash,
            &HostObservation {
                network_unshared: false,
                home_unmounted: false,
                inputs_read_only: false,
                cgroup_absent: false,
            },
        )
    });
    DrillReport {
        sandbox_experiment: if all_passed && acceptance.sandbox_experiment == "MEASURED" {
            "MEASURED".into()
        } else {
            "DRILL_FAILED".into()
        },
        scientific_acceptance: acceptance.scientific_acceptance,
        certification: acceptance.certification,
        promotion: acceptance.promotion,
        execution: acceptance.execution,
        live_enabled: false,
        host_qualified: false,
        cases,
        missing: acceptance.missing,
        kernel: kernel(),
    }
}
