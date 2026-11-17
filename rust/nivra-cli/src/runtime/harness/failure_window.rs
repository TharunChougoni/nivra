//! Bounded evidence for provider failures that happen before any provider turn.
//!
//! An outage can fail a digest turn every few seconds before the provider is
//! called (for example a catalog/auth failure). The original archive persisted
//! the full multi-megabyte request for every attempt, so one session grew by
//! gigabytes (#23). A pre-provider failure now keeps a bounded receipt
//! (reason, stage, sizes and hashes) and at most one representative request per
//! failure window. Original journals, books and failed artifacts are never
//! deleted: the first failure in a window keeps its full request and later
//! retries link to it.
use super::super::{Result, digest, rollover};
use super::Frame;
use serde_json::{Value, json};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

/// Provider stages that complete before any provider turn. `planner::failure`
/// owns the full allowlist; this is its pre-inference subset. `transport` and
/// later stages are treated as provider turns so their request evidence stays
/// complete.
const PRE_PROVIDER_STAGES: &[&str] = &[
    "frame_decode",
    "tool_definitions",
    "application_config",
    "grant_owner",
    "subscription_approval",
    "catalog",
    "inference",
];

/// A failure window keeps at most one representative full request. Retries
/// inside the window persist only the bounded receipt.
pub(super) const FAILURE_WINDOW_MS: i64 = 60 * 60 * 1000;

/// The receipt is far below any provider request and is the only per-retry
/// artifact a repeated pre-provider failure writes.
pub(super) const REQUEST_RECEIPT_MAX_BYTES: usize = 16 * 1024;

/// The allowlisted provider stage of a failure that never reached the provider.
pub(crate) fn pre_provider_stage(provider: &Value) -> Option<&'static str> {
    let stage = provider.get("stage")?.as_str()?;
    PRE_PROVIDER_STAGES
        .iter()
        .copied()
        .find(|known| *known == stage)
}

/// One failure class: the same archive kind/role/model/reason/stage retries
/// inside one window. A different class, a closed window or a stale window
/// starts a new representative artifact.
pub(crate) struct FailureSignature<'a> {
    /// Which archive the representative lives in: `harness_request` or
    /// `planner_audit_input`. The same role/model never shares a window across
    /// the two archive kinds.
    pub kind: &'a str,
    pub role: &'a str,
    pub model: &'a str,
    pub reason: &'a str,
    pub stage: &'a str,
}

impl FailureSignature<'_> {
    fn value(&self) -> Value {
        json!({"kind": self.kind, "role": self.role, "model": self.model, "reason": self.reason, "stage": self.stage})
    }
    fn path(&self, model: &Path) -> PathBuf {
        let encoded = serde_json::to_vec(&self.value()).unwrap_or_default();
        model
            .join("failure-windows")
            .join(format!("{}.json", digest(&encoded)))
    }
}

pub(crate) enum WindowState {
    /// First failure in the window: persist the representative request.
    Representative,
    /// Repeated failure: link to the first failure's representative request.
    Repeated {
        first_run: String,
        first_turn: usize,
        representative: String,
        count: u64,
    },
}

fn read_window(path: &Path) -> Option<Value> {
    let bytes = crate::read_bounded(path, 4096).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    (value["v"].as_u64() == Some(1)).then_some(value)
}

/// A repeated failure joins the open window only when the window is not closed
/// or stale and the representative request it links to is still on disk.
pub(crate) fn classify(model: &Path, signature: &FailureSignature<'_>, now: i64) -> WindowState {
    let joined = || -> Option<WindowState> {
        let value = read_window(&signature.path(model))?;
        if value["closed_ms"].as_i64().is_some() {
            return None;
        }
        let first_run = value["first_run"].as_str()?.to_owned();
        let representative = value["representative"].as_str()?.to_owned();
        if !model.join(&first_run).join(&representative).is_file() {
            return None;
        }
        let first_turn = usize::try_from(value["first_turn"].as_u64()?).ok()?;
        let last = value["last_at_ms"].as_i64()?;
        if now < last || now.saturating_sub(last) > FAILURE_WINDOW_MS {
            return None;
        }
        Some(WindowState::Repeated {
            first_run,
            first_turn,
            representative,
            count: value["count"].as_u64()?,
        })
    };
    joined().unwrap_or(WindowState::Representative)
}

/// Persist the window cursor. A failure to write it never changes the model
/// result; the worst case is one extra representative request.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record(
    model: &Path,
    run: &str,
    turn: usize,
    signature: &FailureSignature<'_>,
    archive_sha: &str,
    archive_bytes: u64,
    representative: &str,
    state: &WindowState,
    now: i64,
) -> Result<()> {
    let path = signature.path(model);
    if std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(model.join("failure-windows"))
        .is_err()
    {
        return Err("harness_failure_window_unavailable".into());
    }
    let mut value = match state {
        WindowState::Representative => json!({
            "v": 1,
            "kind": signature.kind,
            "role": signature.role,
            "model": signature.model,
            "reason": signature.reason,
            "stage": signature.stage,
            "first_run": run,
            "first_turn": turn,
            "representative": representative,
            "count": 1,
            "opened_ms": now,
            "last_at_ms": now,
            "window_ms": FAILURE_WINDOW_MS,
        }),
        WindowState::Repeated {
            first_run,
            first_turn,
            representative,
            count,
        } => json!({
            "v": 1,
            "kind": signature.kind,
            "role": signature.role,
            "model": signature.model,
            "reason": signature.reason,
            "stage": signature.stage,
            "first_run": first_run,
            "first_turn": first_turn,
            "representative": representative,
            "count": count.saturating_add(1),
            "opened_ms": read_window(&path)
                .and_then(|value| value["opened_ms"].as_i64())
                .unwrap_or(now),
            "last_at_ms": now,
            "window_ms": FAILURE_WINDOW_MS,
        }),
    };
    value["archive_sha256"] = json!(archive_sha);
    value["archive_bytes"] = json!(archive_bytes);
    rollover::atomic(&path, &value)
}

/// A provider turn (success or post-provider failure) ends the pre-provider
/// failure window for this role/model so a later outage keeps its own first
/// failure evidence.
pub(crate) fn close(model: &Path, role: &str, model_id: &str, now: i64) {
    let Ok(entries) = std::fs::read_dir(model.join("failure-windows")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(mut value) = read_window(&path) else {
            continue;
        };
        if value["role"] != role
            || value["model"] != model_id
            || value["closed_ms"].as_i64().is_some()
        {
            continue;
        }
        value["closed_ms"] = json!(now);
        if rollover::atomic(&path, &value).is_err() {
            eprintln!("harness_failure_window_unavailable");
        }
    }
}

/// Bounded fields retained for a failure that never reached the provider. The
/// full request is referenced, never copied again.
pub(super) struct RequestEvidence<'a> {
    pub request: &'a str,
    pub frame: &'a Frame,
    pub instructions_sha: &'a str,
    pub reason: &'a str,
    pub stage: &'a str,
    /// Full request retained in this run directory, when this failure opened
    /// the window. Repeated failures leave it null and link the first one.
    pub request_archive: Option<&'a str>,
    /// Representative request file in the first failure's run directory.
    pub representative: Option<&'a str>,
    pub representative_run: Option<&'a str>,
    pub window_count: u64,
}

impl RequestEvidence<'_> {
    pub(super) fn value(&self) -> Result<Value> {
        let prompt_bytes = self
            .frame
            .input
            .iter()
            .filter_map(|item| item["content"].as_str())
            .map(str::len)
            .sum::<usize>();
        Ok(json!({
            "pre_provider": true,
            "reason": self.reason,
            "provider_stage": self.stage,
            "request_bytes": self.request.len(),
            "request_sha256": digest(self.request.as_bytes()),
            "prompt_bytes": prompt_bytes,
            "input_items": self.frame.input.len(),
            "tools_sha256": digest(&serde_json::to_vec(&self.frame.tools)?),
            "instructions_sha256": self.instructions_sha,
            "request_receipt": Value::Null,
            "request_archive": self.request_archive,
            "representative_request": self
                .representative_run
                .map(|run| json!({"run": run, "blob": self.representative}))
                .unwrap_or(Value::Null),
            "failure_window_count": self.window_count,
        }))
    }
}

pub(super) struct PreProviderRequest<'a> {
    pub model: &'a Path,
    pub archive: &'a Path,
    pub run: &'a str,
    pub turn: usize,
    pub frame: &'a Frame,
    pub request: &'a str,
    pub instructions_sha: &'a str,
    pub signature: FailureSignature<'a>,
    pub now: i64,
}

pub(super) struct Retained {
    /// Transcript blob for this failure: the representative request when it
    /// was written, otherwise the bounded receipt.
    pub blob: Option<String>,
    pub evidence: Value,
}

/// Retain one bounded receipt for a pre-provider failure. The first failure in
/// a window also keeps the representative full request; later retries link to
/// it instead of writing the multi-megabyte request again.
pub(super) fn retain(
    input: PreProviderRequest<'_>,
    write_representative: impl FnOnce() -> Result<()>,
) -> Retained {
    let window = classify(input.model, &input.signature, input.now);
    let retained = match &window {
        WindowState::Representative => match write_representative() {
            Ok(()) => Some(format!("turn-{:02}-request.json", input.turn)),
            Err(_) => {
                eprintln!("harness_request_archive_unavailable");
                None
            }
        },
        WindowState::Repeated { .. } => None,
    };
    let (representative_run, representative, window_count) = match &window {
        WindowState::Repeated {
            first_run,
            representative,
            count,
            ..
        } => (
            Some(first_run.as_str()),
            Some(representative.as_str()),
            count.saturating_add(1),
        ),
        WindowState::Representative => (None, None, 1),
    };
    let mut evidence = RequestEvidence {
        request: input.request,
        frame: input.frame,
        instructions_sha: input.instructions_sha,
        reason: input.signature.reason,
        stage: input.signature.stage,
        request_archive: retained.as_deref(),
        representative,
        representative_run,
        window_count,
    }
    .value()
    .unwrap_or_else(|_| json!({"pre_provider": true, "provider_stage": input.signature.stage}));
    let receipt_name = format!("turn-{:02}-request-receipt.json", input.turn);
    let mut receipt = evidence.clone();
    receipt["v"] = json!(1);
    receipt["turn"] = json!(input.turn);
    receipt["paper_only"] = json!(true);
    let receipt = match serde_json::to_vec(&receipt) {
        Ok(bytes) if bytes.len() <= REQUEST_RECEIPT_MAX_BYTES => {
            match rollover::atomic(&input.archive.join(&receipt_name), &receipt) {
                Ok(()) => Some(receipt_name),
                Err(_) => {
                    eprintln!("harness_request_receipt_unavailable");
                    None
                }
            }
        }
        _ => {
            eprintln!("harness_request_receipt_unavailable");
            None
        }
    };
    evidence["request_receipt"] = receipt
        .as_ref()
        .map(|name| json!(name))
        .unwrap_or(Value::Null);
    if record(
        input.model,
        input.run,
        input.turn,
        &input.signature,
        &digest(input.request.as_bytes()),
        input.request.len() as u64,
        &format!("turn-{:02}-request.json", input.turn),
        &window,
        input.now,
    )
    .is_err()
    {
        eprintln!("harness_failure_window_unavailable");
    }
    let blob = retained
        .map(|name| format!("{name}#/request/input"))
        .or_else(|| receipt.clone());
    Retained { blob, evidence }
}

/// The planner audit input: the full digest bundle archived beside each
/// attempt. It is the other multi-megabyte pre-provider artifact (#23).
pub(crate) struct PlannerInput<'a> {
    pub model: &'a Path,
    pub archive: &'a Path,
    pub run: &'a str,
    pub role: &'a str,
    pub model_id: &'a str,
    pub reason: &'a str,
    pub stage: &'a str,
    pub input_hash: &'a str,
    pub input_bytes: u64,
    pub now: i64,
}

/// Retain one bounded receipt for a pre-provider planner failure. The first
/// failure in a window also keeps the representative full `input.json`; later
/// retries link to it instead of writing the multi-megabyte bundle again.
pub(crate) fn retain_planner_input(
    input: PlannerInput<'_>,
    write_representative: impl FnOnce() -> Result<()>,
) -> Value {
    let signature = FailureSignature {
        kind: "planner_audit_input",
        role: input.role,
        model: input.model_id,
        reason: input.reason,
        stage: input.stage,
    };
    let window = classify(input.model, &signature, input.now);
    let retained = match &window {
        WindowState::Representative => match write_representative() {
            Ok(()) => Some("input.json"),
            Err(_) => {
                eprintln!("planner_audit_input_unavailable");
                None
            }
        },
        WindowState::Repeated { .. } => None,
    };
    let (representative_run, representative, window_count) = match &window {
        WindowState::Repeated {
            first_run,
            representative,
            count,
            ..
        } => (
            Some(first_run.as_str()),
            Some(representative.as_str()),
            count.saturating_add(1),
        ),
        WindowState::Representative => (None, None, 1),
    };
    let mut evidence = json!({
        "pre_provider": true,
        "reason": input.reason,
        "provider_stage": input.stage,
        "input_hash": input.input_hash,
        "input_bytes": input.input_bytes,
        "input_receipt": Value::Null,
        "input_archive": retained,
        "representative_input": representative_run
            .map(|run| json!({"run": run, "blob": representative}))
            .unwrap_or(Value::Null),
        "failure_window_count": window_count,
    });
    let mut receipt = evidence.clone();
    receipt["v"] = json!(1);
    receipt["kind"] = json!("planner_audit_input");
    receipt["paper_only"] = json!(true);
    let receipt = match serde_json::to_vec(&receipt) {
        Ok(bytes) if bytes.len() <= REQUEST_RECEIPT_MAX_BYTES => {
            match rollover::atomic(&input.archive.join("input-receipt.json"), &receipt) {
                Ok(()) => Some("input-receipt.json"),
                Err(_) => {
                    eprintln!("planner_audit_receipt_unavailable");
                    None
                }
            }
        }
        _ => {
            eprintln!("planner_audit_receipt_unavailable");
            None
        }
    };
    evidence["input_receipt"] = receipt.map(|name| json!(name)).unwrap_or(Value::Null);
    if record(
        input.model,
        input.run,
        0,
        &signature,
        input.input_hash,
        input.input_bytes,
        "input.json",
        &window,
        input.now,
    )
    .is_err()
    {
        eprintln!("harness_failure_window_unavailable");
    }
    evidence
}
