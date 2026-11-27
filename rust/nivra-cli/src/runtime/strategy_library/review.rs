//! Strategy review gate: deterministic Python AST check plus cheap model
//! review, bound to exact source bytes, runner bytes and review-policy version.
use super::super::strategy::runner_sha256;
use crate::runtime::agent::{AgentClient, AgentSettings};
use crate::runtime::planner::StrategyReviewSettings;
use nivra_core::replay::digest;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub(crate) const REVIEW_POLICY_VERSION: u32 = 1;
const AST_CHECK_PY: &str = include_str!("ast_check.py");
const AST_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SOURCE_BYTES: usize = 262_144;

/// Content hash of the review gate itself (policy version, AST rules, runner).
pub(crate) fn review_policy_sha256() -> String {
    digest(
        format!(
            "v={REVIEW_POLICY_VERSION}\n{}\n{}",
            AST_CHECK_PY,
            runner_sha256()
        )
        .as_bytes(),
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewVerdict {
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntryReview {
    pub verdict: String,
    pub model: String,
    pub source_sha256: String,
    pub runner_sha256: String,
    pub policy_sha256: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    pub reviewed_at_ms: i64,
}

impl EntryReview {
    pub(crate) fn is_current(&self, source_sha256: &str) -> bool {
        self.verdict == "safe"
            && self.source_sha256 == source_sha256
            && self.runner_sha256 == runner_sha256()
            && self.policy_sha256 == review_policy_sha256()
    }
}

/// Deterministic AST check on Python `source`. Returns `Ok(())` when safe, or
/// `Err(reasons)` naming every violation.
pub(crate) fn static_check(
    python_path: &Path,
    source: &[u8],
    allow_research_env: bool,
) -> Result<(), Vec<String>> {
    if source.is_empty() || source.len() > MAX_SOURCE_BYTES {
        return Err(vec!["strategy_review_source_size".into()]);
    }
    if std::str::from_utf8(source).is_err() {
        return Err(vec!["invalid_utf8".into()]);
    }
    if !python_path.is_absolute() {
        return Err(vec!["strategy_review_python_invalid".into()]);
    }
    let mut cmd = Command::new(python_path);
    cmd.env_clear()
        .args(["-I", "-S", "-B", "-c", AST_CHECK_PY])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if allow_research_env {
        cmd.arg("--allow-research-env");
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| vec!["strategy_review_python_unavailable".to_owned()])?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(source);
    }
    let deadline = Instant::now() + AST_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return Err(vec!["strategy_review_checker_failed".into()]);
                }
                let output = child
                    .wait_with_output()
                    .map_err(|_| vec!["strategy_review_checker_failed".to_owned()])?;
                let parsed: ReviewVerdict = serde_json::from_slice(&output.stdout)
                    .map_err(|_| vec!["strategy_review_checker_invalid".to_owned()])?;
                return if parsed.verdict == "safe" && parsed.reasons.is_empty() {
                    Ok(())
                } else if parsed.reasons.is_empty() {
                    Err(vec!["strategy_review_unsafe".into()])
                } else {
                    Err(parsed.reasons)
                };
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(vec!["strategy_review_timeout".into()]);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(vec!["strategy_review_checker_failed".into()]);
            }
        }
    }
}

/// Review gate result combining both static AST check and model review.
pub(crate) trait ModelReviewer {
    fn review_source(
        &mut self,
        source: &str,
    ) -> std::result::Result<(String, ReviewVerdict), String>;
}

/// Model reviewer backed by a short-lived `nivra-agent` process in an
/// ephemeral in-memory session (`review_strategy`).
pub(crate) struct PiModelReviewer<'a> {
    pub settings: &'a AgentSettings,
    pub target: &'a StrategyReviewSettings,
    pub request_id: String,
    pub source_sha256: String,
}

impl ModelReviewer for PiModelReviewer<'_> {
    fn review_source(
        &mut self,
        source: &str,
    ) -> std::result::Result<(String, ReviewVerdict), String> {
        let mut client = AgentClient::spawn(self.settings, &[])
            .map_err(|e| crate::runtime::failure_code(e.as_ref()))?;
        let outcome = client.review_strategy(
            &self.request_id,
            &self.target.model,
            self.target.fallback.as_ref(),
            source,
            &self.source_sha256,
            Duration::from_millis(self.target.timeout_ms),
        );
        let _ = client.shutdown();
        outcome.map_err(|e| crate::runtime::failure_code(e.as_ref()))
    }
}

/// Run both static AST check and model review (reusing a cached safe review if
/// `cached` matches current source, runner, and policy hashes).
pub(crate) fn review_source(
    python_path: &Path,
    source: &[u8],
    allow_research_env: bool,
    cached: Option<&EntryReview>,
    reviewer: &mut dyn ModelReviewer,
    now_ms: i64,
) -> std::result::Result<EntryReview, Vec<String>> {
    static_check(python_path, source, allow_research_env)?;
    let source_sha256 = digest(source);
    if let Some(existing) = cached
        && existing.is_current(&source_sha256)
    {
        return Ok(existing.clone());
    }
    let text = std::str::from_utf8(source).map_err(|_| vec!["invalid_utf8".to_owned()])?;
    let (model, verdict) = reviewer.review_source(text).map_err(|err| vec![err])?;
    if verdict.verdict != "safe" {
        return Err(if verdict.reasons.is_empty() {
            vec!["strategy_review_model_unsafe".to_owned()]
        } else {
            verdict.reasons
        });
    }
    Ok(EntryReview {
        verdict: "safe".into(),
        model,
        source_sha256,
        runner_sha256: runner_sha256(),
        policy_sha256: review_policy_sha256(),
        reasons: verdict.reasons,
        reviewed_at_ms: now_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn py() -> PathBuf {
        PathBuf::from("/usr/bin/python3")
    }

    #[test]
    fn static_ast_check_enforces_allowlist_and_rejects_escapes() {
        let safe = b"import json, math\nfrom decimal import Decimal\ndef on_observation(obs):\n    return {'actions': []}\n";
        assert_eq!(static_check(&py(), safe, false), Ok(()));

        // 1-arg type(x) is allowed (used in shipped strategies), 3-arg type() is rejected.
        let type_ok = b"def on_observation(obs):\n    return {'actions': []} if type(obs) is dict else {'actions': []}\n";
        assert_eq!(static_check(&py(), type_ok, false), Ok(()));

        // sys.stdin/stdout/argv allowed, sys.modules/exit/alias rejected.
        let sys_ok =
            b"import sys\ndef on_observation(obs):\n    _ = sys.argv\n    return {'actions': []}\n";
        assert_eq!(static_check(&py(), sys_ok, false), Ok(()));

        let rejected: [(&str, &str, &str); 18] = [
            (
                "open",
                "def on_observation(obs):\n    open('/etc/passwd')\n    return []\n",
                "forbidden_builtin:open",
            ),
            (
                "exec",
                "def on_observation(obs):\n    exec('1+1')\n    return []\n",
                "forbidden_builtin:exec",
            ),
            (
                "eval",
                "def on_observation(obs):\n    eval('1+1')\n    return []\n",
                "forbidden_builtin:eval",
            ),
            (
                "compile",
                "def on_observation(obs):\n    compile('1','','eval')\n    return []\n",
                "forbidden_builtin:compile",
            ),
            (
                "__import__",
                "def on_observation(obs):\n    __import__('os')\n    return []\n",
                "forbidden_builtin:__import__",
            ),
            (
                "globals",
                "def on_observation(obs):\n    return globals()\n",
                "forbidden_builtin:globals",
            ),
            (
                "locals",
                "def on_observation(obs):\n    return locals()\n",
                "forbidden_builtin:locals",
            ),
            (
                "subclasses",
                "def on_observation(obs):\n    return ().__class__.__subclasses__()\n",
                "forbidden_attribute:__subclasses__",
            ),
            (
                "dunder_globals",
                "def on_observation(obs):\n    return on_observation.__globals__\n",
                "forbidden_attribute:__globals__",
            ),
            (
                "dunder_builtins",
                "def on_observation(obs):\n    return __builtins__\n",
                "forbidden_builtin:__builtins__",
            ),
            (
                "os",
                "import os\ndef on_observation(obs):\n    return []\n",
                "disallowed_import:os",
            ),
            (
                "subprocess",
                "import subprocess\ndef on_observation(obs):\n    return []\n",
                "disallowed_import:subprocess",
            ),
            (
                "socket",
                "import socket\ndef on_observation(obs):\n    return []\n",
                "disallowed_import:socket",
            ),
            (
                "ctypes",
                "import ctypes\ndef on_observation(obs):\n    return []\n",
                "disallowed_import:ctypes",
            ),
            (
                "pathlib",
                "from pathlib import Path\ndef on_observation(obs):\n    return []\n",
                "disallowed_import:pathlib",
            ),
            (
                "sys_exit",
                "import sys\ndef on_observation(obs):\n    sys.exit(1)\n",
                "disallowed_sys_attr:exit",
            ),
            (
                "format_traversal",
                "def on_observation(obs):\n    '{0.__class__}'.format(obs)\n    return []\n",
                "forbidden_format_attr",
            ),
            (
                "missing_entry",
                "import math\nx = math.sqrt(4)\n",
                "missing_on_observation",
            ),
        ];
        for (label, src, expected) in rejected {
            let err = static_check(&py(), src.as_bytes(), false).unwrap_err();
            assert!(
                err.iter().any(|r| r == expected),
                "{label}: expected {expected} in {err:?}"
            );
        }

        // research_env imports are gated on allow_research_env.
        let np_src = b"import numpy, json\ndef on_observation(obs):\n    return {'actions': []}\n";
        assert!(static_check(&py(), np_src, false).is_err());
        assert_eq!(static_check(&py(), np_src, true), Ok(()));
    }

    struct StubReviewer {
        calls: usize,
        verdict: &'static str,
        reasons: Vec<&'static str>,
    }

    impl ModelReviewer for StubReviewer {
        fn review_source(
            &mut self,
            _source: &str,
        ) -> std::result::Result<(String, ReviewVerdict), String> {
            self.calls += 1;
            Ok((
                "fake:gate".into(),
                ReviewVerdict {
                    verdict: self.verdict.into(),
                    reasons: self.reasons.iter().map(|s| (*s).to_owned()).collect(),
                },
            ))
        }
    }

    #[test]
    fn model_review_caches_by_source_runner_and_policy_and_blocks_unsafe() {
        let safe_src = b"def on_observation(obs):\n    return {'actions': []}\n";
        let mut ok_reviewer = StubReviewer {
            calls: 0,
            verdict: "safe",
            reasons: Vec::new(),
        };
        let review = review_source(&py(), safe_src, false, None, &mut ok_reviewer, 10).unwrap();
        assert_eq!(ok_reviewer.calls, 1);
        assert!(review.is_current(&digest(safe_src)));

        // Cached safe review skips a second model call for identical source+runner+policy.
        let again =
            review_source(&py(), safe_src, false, Some(&review), &mut ok_reviewer, 20).unwrap();
        assert_eq!(again, review);
        assert_eq!(ok_reviewer.calls, 1);

        // Editing source bytes invalidates the cache and triggers re-review.
        let modified = b"def on_observation(obs):\n    return {'actions': [], 'x': 1}\n";
        assert!(!review.is_current(&digest(modified)));
        let updated =
            review_source(&py(), modified, false, Some(&review), &mut ok_reviewer, 30).unwrap();
        assert_eq!(ok_reviewer.calls, 2);
        assert_eq!(updated.source_sha256, digest(modified));

        // Model flagging unsafe blocks approval.
        let mut bad_reviewer = StubReviewer {
            calls: 0,
            verdict: "unsafe",
            reasons: vec!["suspicious_loop"],
        };
        let err = review_source(&py(), safe_src, false, None, &mut bad_reviewer, 40).unwrap_err();
        assert_eq!(err, vec!["suspicious_loop".to_owned()]);
    }
}
