//! Tier 2 scores-only execution. No model-selected executable, environment or paths.
//! Python uses a restricted mathematical AST plus a kernel syscall allowlist.
//! WASM has no imports (including WASI), fuel and linear-memory bounds.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub wall_millis: u64,
    pub cpu_seconds: u64,
    pub memory_bytes: u64,
    pub max_source_bytes: usize,
    pub max_input_bytes: usize,
    pub max_output_bytes: usize,
    pub wasm_fuel: u64,
}
impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wall_millis: 1000,
            cpu_seconds: 1,
            memory_bytes: 128 * 1024 * 1024,
            max_source_bytes: 65536,
            max_input_bytes: 1024 * 1024,
            max_output_bytes: 65536,
            wasm_fuel: 1_000_000,
        }
    }
}
impl SandboxConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=30_000).contains(&self.wall_millis)
            || !(1..=30).contains(&self.cpu_seconds)
            || !(32 * 1024 * 1024..=512 * 1024 * 1024).contains(&self.memory_bytes)
            || !(1..=262144).contains(&self.max_source_bytes)
            || !(1..=4 * 1024 * 1024).contains(&self.max_input_bytes)
            || !(1..=262144).contains(&self.max_output_bytes)
            || !(1..=100_000_000).contains(&self.wasm_fuel)
        {
            return Err("sandbox_limits_invalid");
        }
        Ok(())
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum SandboxError {
    Disabled,
    Invalid,
    Unavailable,
    Timeout,
    FailedTick,
    InvalidScores,
}
/// Feature vectors in a fixed spec-defined order. Keys must match output exactly.
pub type Snapshot = BTreeMap<String, Vec<f64>>;
pub enum Source<'a> {
    Python(&'a str),
    Wasm(&'a [u8]),
}

/// Limits cover runtime startup, parsing/compilation and scorer execution alike.
/// Missing prlimit/runtime is an explicit unavailable tick, never unrestricted execution.
fn limited_command(runtime: &str, config: &SandboxConfig) -> Result<Command, SandboxError> {
    // Check fixed application-selected paths rather than searching PATH or trying
    // an unrestricted fallback. prlimit reports a missing runtime as exit 127,
    // which would otherwise be indistinguishable from a scorer execution error.
    if !Path::new("/usr/bin/prlimit").is_file() || !Path::new(runtime).is_file() {
        return Err(SandboxError::Unavailable);
    }
    let mut command = Command::new("/usr/bin/prlimit");
    command.args([
        format!("--as={}:{}", config.memory_bytes, config.memory_bytes),
        format!("--cpu={}:{}", config.cpu_seconds, config.cpu_seconds),
        "--core=0:0".into(),
        "--".into(),
        runtime.into(),
    ]);
    Ok(command)
}

fn execute(
    mut command: Command,
    input: Vec<u8>,
    config: &SandboxConfig,
) -> Result<Vec<u8>, SandboxError> {
    command
        .env_clear()
        .env("PYTHONHASHSEED", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let start = Instant::now();
    let mut child = command.spawn().map_err(|_| SandboxError::Unavailable)?;
    let mut stdin = child.stdin.take().ok_or(SandboxError::FailedTick)?;
    let mut stdout = child.stdout.take().ok_or(SandboxError::FailedTick)?;
    // Concurrent bounded pipe handling prevents a malicious scorer blocking the host.
    let writer = thread::spawn(move || stdin.write_all(&input));
    let limit = config.max_output_bytes;
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .by_ref()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes);
        (result, bytes)
    });
    let outcome = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(SandboxError::FailedTick)
                };
            }
            Ok(None) if start.elapsed() < Duration::from_millis(config.wall_millis) => {
                thread::sleep(Duration::from_millis(2))
            }
            Ok(None) => break Err(SandboxError::Timeout),
            Err(_) => break Err(SandboxError::FailedTick),
        }
    };
    if outcome.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    let written = writer.join().map_err(|_| SandboxError::FailedTick)?;
    let (read, bytes) = reader.join().map_err(|_| SandboxError::FailedTick)?;
    outcome?;
    written.map_err(|_| SandboxError::FailedTick)?;
    read.map_err(|_| SandboxError::FailedTick)?;
    if bytes.len() > limit {
        return Err(SandboxError::InvalidScores);
    }
    Ok(bytes)
}

/// Each invocation is a failed tick on error; there is no fallback scorer.
pub fn score(
    config: &SandboxConfig,
    source: Source<'_>,
    input: &Snapshot,
) -> Result<BTreeMap<String, f64>, SandboxError> {
    config.validate().map_err(|_| SandboxError::Invalid)?;
    if !config.enabled {
        return Err(SandboxError::Disabled);
    }
    let encoded = serde_json::to_vec(input).map_err(|_| SandboxError::Invalid)?;
    if input.is_empty()
        || input.len() > 4096
        || encoded.len() > config.max_input_bytes
        || input.iter().any(|(key, values)| {
            key.is_empty()
                || key.len() > 128
                || values.len() > 128
                || values.iter().any(|value| !value.is_finite())
        })
    {
        return Err(SandboxError::Invalid);
    }
    let output = match source {
        Source::Python(source) => {
            if source.len() > config.max_source_bytes {
                return Err(SandboxError::Invalid);
            }
            let request = serde_json::to_vec(&serde_json::json!({"source": source, "input": input, "memory": config.memory_bytes, "cpu": config.cpu_seconds})).map_err(|_| SandboxError::Invalid)?;
            if !cfg!(all(target_os = "linux", target_arch = "x86_64")) {
                return Err(SandboxError::Unavailable);
            }
            let mut command = limited_command("/usr/bin/python3", config)?;
            // -I ignores environment and user site; -S prevents site hooks.
            command.args(["-I", "-S", "-c", include_str!("strategy_python.py")]);
            execute(command, request, config)?
        }
        Source::Wasm(module) => {
            if module.len() > config.max_source_bytes || !import_free_wasm(module) {
                return Err(SandboxError::Invalid);
            }
            // WASI and every other import are forbidden. No inherited environment,
            // preopened directories, clocks, entropy, sockets or host callbacks.
            // ABI: score(index, key_count, row_len, row_values..., ...) -> f64.
            // Every call receives ALL rows in lexicographic key order, enabling
            // cross-sectional scorers. Each row's feature/history ordering is
            // fixed by the spec. The module signature must match this shape.
            let mut scores = BTreeMap::new();
            let deadline = Instant::now();
            // Bound the entire sequential batch, not just each key invocation.
            let batch_millis = config.wall_millis.min(config.cpu_seconds * 1000);
            let full_snapshot: Vec<String> = input
                .values()
                .flat_map(|values| {
                    std::iter::once(values.len().to_string())
                        .chain(values.iter().map(ToString::to_string))
                })
                .collect();
            if full_snapshot.len() > 8192 {
                return Err(SandboxError::Invalid);
            }
            for (index, key) in input.keys().enumerate() {
                let elapsed = deadline.elapsed().as_millis() as u64;
                if elapsed >= batch_millis {
                    return Err(SandboxError::Timeout);
                }
                let mut remaining = config.clone();
                remaining.wall_millis = batch_millis - elapsed;
                let mut command = limited_command("/usr/bin/wasmtime", config)?;
                command.args([
                    "run",
                    "-W",
                    &format!("fuel={}", config.wasm_fuel),
                    "-C",
                    "parallel-compilation=n",
                    "-C",
                    "cache=n",
                    // Avoid filesystem-backed CoW images and huge default
                    // virtual reservations under the whole-process AS bound.
                    "-O",
                    "memory-init-cow=n,memory-reservation=0,memory-reservation-for-growth=0,memory-guard-size=0,guard-before-linear-memory=n",
                    "-W",
                    &format!("max-memory-size={}", config.memory_bytes),
                    "-W",
                    "threads=n",
                    "--invoke",
                    "score",
                    "-",
                ]);
                command.arg(index.to_string()).arg(input.len().to_string());
                command.args(&full_snapshot);
                let bytes = execute(command, module.to_vec(), &remaining)?;
                let value: f64 = std::str::from_utf8(&bytes)
                    .map_err(|_| SandboxError::InvalidScores)?
                    .trim()
                    .parse()
                    .map_err(|_| SandboxError::InvalidScores)?;
                scores.insert(key.clone(), value);
            }
            serde_json::to_vec(&scores).map_err(|_| SandboxError::InvalidScores)?
        }
    };
    if output.len() > config.max_output_bytes {
        return Err(SandboxError::InvalidScores);
    }
    let scores: BTreeMap<String, f64> =
        serde_json::from_slice(&output).map_err(|_| SandboxError::InvalidScores)?;
    if scores.keys().ne(input.keys()) || scores.values().any(|value| !value.is_finite()) {
        return Err(SandboxError::InvalidScores);
    }
    Ok(scores)
}
fn leb(bytes: &[u8], cursor: &mut usize) -> Option<usize> {
    let mut result = 0usize;
    for shift in (0..35).step_by(7) {
        let byte = *bytes.get(*cursor)?;
        *cursor += 1;
        if shift == 28 && byte > 15 {
            return None;
        }
        result |= usize::from(byte & 127) << shift;
        if byte & 128 == 0 {
            return Some(result);
        }
    }
    None
}
fn import_free_wasm(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"\0asm\x01\0\0\0") {
        return false;
    }
    let mut cursor = 8;
    while cursor < bytes.len() {
        let section = bytes[cursor];
        cursor += 1;
        let Some(size) = leb(bytes, &mut cursor) else {
            return false;
        };
        let Some(end) = cursor.checked_add(size).filter(|end| *end <= bytes.len()) else {
            return false;
        };
        if section == 2 {
            let mut position = cursor;
            if leb(bytes, &mut position) != Some(0) || position != end {
                return false;
            }
        }
        cursor = end;
    }
    true
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_disabled_and_imports_rejected() {
        assert_eq!(
            score(
                &SandboxConfig::default(),
                Source::Python(""),
                &Snapshot::new()
            ),
            Err(SandboxError::Disabled)
        );
        assert!(!import_free_wasm(b"\0asm\x01\0\0\0\x02\x01\x01"));
        assert!(!import_free_wasm(b"\0asm\x01\0\0\0\x02\xff"));
        assert!(import_free_wasm(b"\0asm\x01\0\0\0"));
        if !Path::new("/usr/bin/wasmtime").is_file() {
            assert_eq!(
                score(
                    &SandboxConfig {
                        enabled: true,
                        ..Default::default()
                    },
                    Source::Wasm(b"\0asm\x01\0\0\0"),
                    &BTreeMap::from([("NSE:fixture".into(), vec![1.0])])
                ),
                Err(SandboxError::Unavailable)
            );
        }
    }
    #[test]
    fn python_math_and_capabilities() {
        if !cfg!(all(target_os = "linux", target_arch = "x86_64"))
            || !Path::new("/usr/bin/python3").is_file()
            || !Path::new("/usr/bin/prlimit").is_file()
        {
            return;
        }
        let config = SandboxConfig {
            enabled: true,
            wall_millis: 2000,
            ..Default::default()
        };
        let input = BTreeMap::from([("NSE:fixture".into(), vec![4.0, 2.0])]);
        let valid = "def score(data):\n return {'NSE:fixture': sqrt(data['NSE:fixture'][0]) + data['NSE:fixture'][1]}";
        assert_eq!(
            score(&config, Source::Python(valid), &input),
            Ok(BTreeMap::from([("NSE:fixture".into(), 4.0)]))
        );
        for source in [
            "import os",
            "import time",
            "import random",
            "import socket",
            "def score(data):\n return eval('1')",
            "def score(data):\n return globals()",
            "def score(data):\n return open('secret')",
            "def score(data):\n return __import__('time')",
            "def score(data):\n return {'NSE:fixture': 1 / 0}",
            "def score(data):\n return {'NSE:fixture': len([0] * 100000000)}",
            "def score(data):\n return data.__class__",
            "def score(data):\n data['NSE:fixture'][0] = 3\n return {}",
            "def score(data):\n alias = data\n alias['NSE:fixture'] = (1,)\n return {}",
            "def score(data):\n return {'NSE:fixture': float('nan')}",
            "def score(data):\n return {'NSE:fixture': 1e309}",
            "def score(data):\n return {'other': 1}",
        ] {
            assert!(score(&config, Source::Python(source), &input).is_err());
        }
        // Exercise the kernel policy independently of AST admission: even a
        // hypothetical source-subset escape must not obtain these capabilities.
        for syscall in [
            "libc.syscall(257, -100, b'/dev/null', 0, 0)", // openat
            "libc.syscall(1, 2, b'x', 1)",                 // write outside the output pipe
            "libc.syscall(302, 0, 7, 0, 0)",               // query/change resource limits
            "libc.syscall(10, 0, 4096, 4)",                // executable memory
        ] {
            let worker = include_str!("strategy_python.py").replace(
                "exec(code, namespace)",
                &format!("{syscall}\nexec(code, namespace)"),
            );
            let mut command = limited_command("/usr/bin/python3", &config).unwrap();
            command.args(["-I", "-S", "-c", &worker]);
            let request = serde_json::to_vec(&serde_json::json!({
                "source": valid, "input": input,
                "memory": config.memory_bytes, "cpu": config.cpu_seconds,
            }))
            .unwrap();
            assert_eq!(
                execute(command, request, &config),
                Err(SandboxError::FailedTick)
            );
        }
        let cross_sectional = "def score(data):\n total = 0\n for key in data:\n  total += data[key][0]\n result = {}\n for key in data:\n  result[key] = data[key][0] / total\n return result";
        let batch = BTreeMap::from([("NSE:a".into(), vec![2.0]), ("NSE:b".into(), vec![6.0])]);
        let expected = BTreeMap::from([("NSE:a".into(), 0.25), ("NSE:b".into(), 0.75)]);
        assert_eq!(
            score(&config, Source::Python(cross_sectional), &batch),
            Ok(expected.clone())
        );
        assert_eq!(
            score(&config, Source::Python(cross_sectional), &batch),
            Ok(expected)
        );
        assert_eq!(
            score(
                &SandboxConfig {
                    max_output_bytes: 1,
                    ..config.clone()
                },
                Source::Python(valid),
                &input
            ),
            Err(SandboxError::InvalidScores)
        );
        let cpu_bound = SandboxConfig {
            wall_millis: 5000,
            ..config.clone()
        };
        assert_eq!(
            score(
                &cpu_bound,
                Source::Python("def score(data):\n while True:\n  pass"),
                &input
            ),
            Err(SandboxError::FailedTick)
        );
        let quick = SandboxConfig {
            wall_millis: 100,
            ..config
        };
        assert_eq!(
            score(
                &quick,
                Source::Python("def score(data):\n while True:\n  pass"),
                &input
            ),
            Err(SandboxError::Timeout)
        );
    }
}
