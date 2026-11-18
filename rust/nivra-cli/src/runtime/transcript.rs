//! Bounded display evidence; original run artifacts remain authoritative.
use super::{DashboardSettings, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::Path;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct TranscriptEvent {
    pub v: u8,
    pub run: String,
    pub seq: u64,
    pub at_ms: i64,
    pub kind: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

#[cfg(test)]
thread_local! { static BLOCK_NEXT_START: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestDelta {
    v: u8,
    base_turn: usize,
    append_input: Vec<Value>,
    tools_sha256: String,
    tools: Option<Vec<Value>>,
    final_answer_only: bool,
    request_sha256: String,
    instructions_sha256: String,
}
// Return the original archive wrapper. Serialize its typed Frame to recover the
// exact provider byte order; serializing a generic JSON object sorts its keys.
pub fn full_request(dir: &Path, turn: usize, settings: &DashboardSettings) -> Result<Value> {
    let load = |path: &Path| -> Result<Value> {
        let metadata =
            std::fs::symlink_metadata(path).map_err(|_| "archive_request_unavailable")?;
        if !metadata.is_file() || std::os::unix::fs::MetadataExt::nlink(&metadata) != 1 {
            return Err("archive_request_unavailable".into());
        }
        let bytes = crate::read_bounded(path, settings.history_read_max_bytes as u64)
            .map_err(|_| "archive_request_unavailable")?;
        serde_json::from_slice(&bytes).map_err(|_| "archive_request_rebuild_mismatch".into())
    };
    let legacy = dir.join(format!("turn-{turn:02}-request.json"));
    if legacy.exists() {
        return load(&legacy);
    }
    if turn >= settings.history_max_files {
        return Err("archive_request_unavailable".into());
    }
    let mut archive = load(&dir.join("turn-00-request.json"))?;
    for index in 1..=turn {
        let legacy = dir.join(format!("turn-{index:02}-request.json"));
        if legacy.exists() {
            archive = load(&legacy)?;
            continue;
        }
        let delta: RequestDelta = serde_json::from_value(load(
            &dir.join(format!("turn-{index:02}-request-delta.json")),
        )?)
        .map_err(|_| "archive_request_rebuild_mismatch")?;
        if delta.v != 1 || delta.base_turn != index - 1 {
            return Err("archive_request_rebuild_mismatch".into());
        }
        let mut frame: super::harness::Frame = serde_json::from_value(archive["request"].clone())
            .map_err(|_| "archive_request_rebuild_mismatch")?;
        frame.input.extend(delta.append_input);
        if let Some(tools) = delta.tools {
            frame.tools = tools;
        }
        frame.final_answer_only = delta.final_answer_only;
        let request = serde_json::to_vec(&frame)?;
        if request.len() > settings.history_read_max_bytes {
            return Err("archive_request_unavailable".into());
        }
        if super::digest(&request) != delta.request_sha256
            || super::digest(&serde_json::to_vec(&frame.tools)?) != delta.tools_sha256
            || super::digest(super::harness::frame_instructions(&frame)?.as_bytes())
                != delta.instructions_sha256
        {
            return Err("archive_request_rebuild_mismatch".into());
        }
        archive = json!({"request":frame,"instructions_sha256":delta.instructions_sha256});
    }
    Ok(archive)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RunIndexLine {
    pub v: u8,
    pub run: String,
    pub role: String,
    pub model: String,
    pub trigger_kind: Option<String>,
    pub started_ms: i64,
    pub duration_ms: i64,
    pub outcome: String,
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub cost_estimate_inr: Option<String>,
    pub tool_calls: usize,
    pub tool_failures: usize,
    pub instruments: Vec<String>,
    pub bytes: u64,
    pub mode: String,
    #[serde(default)]
    pub profile_sha256: Option<String>,
}

// Unlike the legacy history projection, transcripts retain non-secret account
// and input fields. Credential keys share the same denylist at write/read time.
pub(super) fn redact(value: &Value) -> Value {
    let mut value = value.clone();
    redact_in_place(&mut value);
    value
}

/// Credential assignments also occur in prose, environment-style text and
/// headers. Inspect the key immediately before each separator; mentioning a
/// credential key without an assignment is not itself a secret.
fn credential_assignment(text: &str) -> bool {
    text.split_inclusive(['=', ':']).any(|segment| {
        let Some(prefix) = segment.strip_suffix(['=', ':']) else {
            return false;
        };
        let prefix = prefix.trim_end().trim_end_matches(['\'', '"', '`']);
        let key = prefix
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
            .next()
            .unwrap_or_default()
            .trim_start_matches('-');
        super::model_history::secret_key(key)
    })
}
fn redact_in_place(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|key, _| !super::model_history::secret_key(key));
            for value in fields.values_mut() {
                redact_in_place(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                redact_in_place(value);
            }
        }
        Value::String(text)
            if text.contains("Bearer ")
                || text.contains("sk-")
                || text.contains("access_token")
                || text.contains("refresh_token")
                || text.contains("-----BEGIN PRIVATE KEY") =>
        {
            *text = "[redacted]".into()
        }
        Value::String(text) if text.trim_start().starts_with(['{', '[']) => {
            if let Ok(mut nested) = serde_json::from_str::<Value>(text) {
                let original = nested.clone();
                redact_in_place(&mut nested);
                if nested != original {
                    *text = nested.to_string();
                }
            } else if credential_assignment(text) {
                *text = "[redacted]".into();
            }
        }
        Value::String(text) if credential_assignment(text) => *text = "[redacted]".into(),
        _ => {}
    }
}
pub fn read(path: &Path, max_bytes: u64) -> Result<Vec<Value>> {
    let bytes = crate::read_bounded(path, max_bytes).map_err(|_| "transcript_read_failed")?;
    let mut events = Vec::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        // A crash may leave a partial final write; it never forms an event.
        if !line.ends_with(b"\n") {
            break;
        }
        let event: TranscriptEvent =
            serde_json::from_slice(line).map_err(|_| "transcript_invalid")?;
        if event.v != 1 || event.seq != events.len() as u64 {
            return Err("transcript_invalid".into());
        }
        events.push(redact(&serde_json::to_value(event)?));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_redacts_legacy_credentials_and_rejects_corrupt_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.ndjson");
        std::fs::write(&path, b"{\"v\":1,\"run\":\"r\",\"seq\":0,\"at_ms\":1,\"kind\":\"input\",\"api_key\":\"secret-fixture\"}\n").unwrap();
        assert!(
            !serde_json::to_string(&read(&path, 1000).unwrap())
                .unwrap()
                .contains("secret-fixture")
        );
        std::fs::write(&path, b"invalid\n").unwrap();
        assert!(read(&path, 1000).is_err());
    }
}

#[cfg(test)]
mod index_tests {}

#[derive(Default, Serialize)]
pub struct RunFilter {
    pub cursor: Option<String>,
    pub limit: usize,
    pub role: Option<String>,
    /// User-visible role group. `orchestrator` matches operations, strategy,
    /// scorer, supervisor and the legacy orchestrator role.
    pub identity: Option<String>,
    pub date: Option<chrono::NaiveDate>,
    pub trigger: Option<String>,
    pub outcome: Option<String>,
    pub instrument: Option<String>,
    pub failed: bool,
}
pub struct RunPage {
    pub body: Value,
    pub etag: String,
}
#[cfg(test)]
thread_local! { static RUN_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
/// Longest run allowance by role (docs/MODEL_HARNESS.md "Budgets"): the digest
/// run cap is 120 s, supervisor/scorer/review turns are bounded at 30 min, and
/// the research lane may be granted up to six hours. A transcript without a
/// terminal event can only still be executing inside its role's allowance.
const DIGEST_RUN_WINDOW_MS: i64 = 120_000;
const RUN_WINDOW_MS: i64 = 30 * 60 * 1000;
const RESEARCH_RUN_WINDOW_MS: i64 = 6 * 60 * 60 * 1000;
fn run_window_ms(role: Option<&str>, trigger: Option<&str>) -> i64 {
    match role {
        Some("digest") => DIGEST_RUN_WINDOW_MS,
        Some("supervisor") if trigger == Some("research") => RESEARCH_RUN_WINDOW_MS,
        _ => RUN_WINDOW_MS,
    }
}
/// A run without `run_ended` is only running while it belongs to the current
/// session and its start lies inside the role's bounded allowance. Archived
/// sessions and older transcripts are interrupted display evidence, never a
/// live claim.
pub(crate) fn run_is_running(
    started_ms: Option<i64>,
    ended: bool,
    current_session: bool,
    role: Option<&str>,
    trigger: Option<&str>,
    now_ms: i64,
) -> bool {
    if ended || !current_session {
        return false;
    }
    let Some(started) = started_ms else {
        return false;
    };
    let window = run_window_ms(role, trigger);
    started <= now_ms.saturating_add(window) && now_ms.saturating_sub(started) <= window
}
fn run_row(
    path: &Path,
    id: &str,
    settings: &DashboardSettings,
    current_session: bool,
    now_ms: i64,
) -> Result<(Value, bool)> {
    #[cfg(test)]
    RUN_READS.with(|reads| reads.set(reads.get() + 1));
    if path.join("transcript.ndjson").is_file() {
        let events = read(
            &path.join("transcript.ndjson"),
            settings.history_read_max_bytes as u64,
        )?;
        if events.iter().any(|event| event["run"] != id) {
            return Err("run_transcript_identity_mismatch".into());
        }
        let first = events.first().ok_or("run_transcript_empty")?;
        let last = events.iter().rev().find(|v| v["kind"] == "run_ended");
        let ended = last.cloned().unwrap_or(Value::Null);
        let running = run_is_running(
            first["at_ms"].as_i64(),
            last.is_some(),
            current_session,
            first["role"].as_str(),
            first["trigger"]["kind"].as_str(),
            now_ms,
        );
        let instruments: std::collections::BTreeSet<_> = events
            .iter()
            .flat_map(|v| v["instruments"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
            .take(16)
            .collect();
        let partial = events.iter().any(|v| v["kind"] == "truncated");
        let calls = events.iter().filter(|v| v["kind"] == "tool_call").count();
        let failures = events
            .iter()
            .filter(|v| {
                v["kind"] == "tool_result"
                    && matches!(v["status"].as_str(), Some("failed" | "refused"))
            })
            .count();
        return Ok((
            json!({"v":1,"run":id,"role":first["role"],"model":first["model"],"trigger_kind":first["trigger"]["kind"],"started_ms":first["at_ms"],"duration_ms":ended["duration_ms"],"outcome":last.map(|v|v["outcome"].clone()).unwrap_or_else(||json!(if running{"running"}else{"interrupted"})),"tokens_in":ended["tokens_in"],"tokens_out":ended["tokens_out"],"cost_estimate_inr":ended["cost_estimate_inr"],"tool_calls":if last.is_some(){ended["tool_calls"].clone()}else if partial{Value::Null}else{json!(calls)},"tool_failures":if last.is_some(){ended["tool_failures"].clone()}else if partial{Value::Null}else{json!(failures)},"instruments":instruments,"bytes":null,"mode":first["mode"],"legacy":false,"running":running}),
            running,
        ));
    }
    let mut row = super::model_history::summary_row(path, id, settings);
    row["v"] = json!(1);
    row["run"] = json!(id);
    row["trigger_kind"] = row["trigger"]["kind"].clone();
    for key in [
        "tokens_in",
        "tokens_out",
        "cost_estimate_inr",
        "tool_calls",
        "tool_failures",
        "bytes",
        "mode",
    ] {
        row[key] = Value::Null;
    }
    row["instruments"] = json!([]);
    row["legacy"] = json!(true);
    row["running"] = json!(false);
    Ok((row, false))
}
fn cursor_encode(date: &str, line: usize) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{date}:{line}"))
}
fn cursor_decode(cursor: &str) -> Result<(String, usize)> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| "run_cursor_invalid")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "run_cursor_invalid")?;
    let (date, line) = text.split_once(':').ok_or("run_cursor_invalid")?;
    let parsed =
        chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| "run_cursor_invalid")?;
    if parsed.to_string() != date || line.is_empty() || !line.bytes().all(|v| v.is_ascii_digit()) {
        return Err("run_cursor_invalid".into());
    }
    Ok((date.into(), line.parse().map_err(|_| "run_cursor_invalid")?))
}
#[allow(clippy::large_enum_variant)]
enum ListRow {
    Index(RunIndexLine),
    Other(Value),
}
fn identity_matches(role: &str, identity: &str) -> bool {
    match identity {
        "orchestrator" => matches!(role, "scorer" | "supervisor" | "orchestrator"),
        other => role == other,
    }
}

impl ListRow {
    fn matches(&self, filter: &RunFilter) -> bool {
        match self {
            Self::Index(row) => {
                filter.role.as_ref().is_none_or(|v| row.role == *v)
                    && filter
                        .identity
                        .as_deref()
                        .is_none_or(|v| identity_matches(&row.role, v))
                    && filter
                        .trigger
                        .as_ref()
                        .is_none_or(|v| row.trigger_kind.as_ref() == Some(v))
                    && filter.outcome.as_ref().is_none_or(|v| row.outcome == *v)
                    && filter
                        .instrument
                        .as_ref()
                        .is_none_or(|v| row.instruments.contains(v))
                    && (!filter.failed || row.tool_failures > 0)
            }
            Self::Other(row) => {
                filter.role.as_deref().is_none_or(|v| row["role"] == v)
                    && filter.identity.as_deref().is_none_or(|v| {
                        identity_matches(row["role"].as_str().unwrap_or_default(), v)
                    })
                    && filter
                        .trigger
                        .as_deref()
                        .is_none_or(|v| row["trigger_kind"] == v)
                    && filter
                        .outcome
                        .as_deref()
                        .is_none_or(|v| row["outcome"] == v)
                    && filter.instrument.as_deref().is_none_or(|key| {
                        row["instruments"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|v| v == key))
                    })
                    && (!filter.failed || row["tool_failures"].as_u64().is_some_and(|v| v > 0))
            }
        }
    }
    fn into_value(self) -> Result<Value> {
        match self {
            Self::Index(row) => {
                let mut row = serde_json::to_value(row)?;
                row["legacy"] = json!(false);
                row["running"] = json!(false);
                Ok(row)
            }
            Self::Other(row) => Ok(row),
        }
    }
}
fn redact_string(text: &mut String) {
    if text.contains("Bearer ")
        || text.contains("sk-")
        || text.contains("access_token")
        || text.contains("refresh_token")
        || credential_assignment(text)
        || text.contains("-----BEGIN PRIVATE KEY")
    {
        *text = "[redacted]".into();
    }
}
fn index_lines(path: &Path, settings: &DashboardSettings) -> Result<Vec<RunIndexLine>> {
    let bytes = crate::read_bounded(path, settings.history_read_max_bytes as u64)
        .map_err(|_| "run_index_unavailable")?;
    let mut lines = Vec::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if !line.ends_with(b"\n") {
            break;
        }
        if line.len() > 2048 {
            return Err("run_index_invalid".into());
        }
        let mut row: RunIndexLine =
            serde_json::from_slice(line).map_err(|_| "run_index_invalid")?;
        if row.v != 1 || !super::model_history::valid_id(&row.run) || row.instruments.len() > 16 {
            return Err("run_index_invalid".into());
        }
        // Keep validated rows typed until paging selects them. Redact their
        // scalar schema directly rather than allocating a JSON tree per run.
        for value in [
            &mut row.run,
            &mut row.role,
            &mut row.model,
            &mut row.outcome,
            &mut row.mode,
        ] {
            redact_string(value);
        }
        for value in [&mut row.trigger_kind, &mut row.cost_estimate_inr]
            .into_iter()
            .flatten()
        {
            redact_string(value);
        }
        for value in &mut row.instruments {
            redact_string(value);
        }
        lines.push(row);
    }
    Ok(lines)
}
pub fn list_runs(
    source: &Path,
    filter: &RunFilter,
    settings: &DashboardSettings,
) -> Result<RunPage> {
    let cursor = filter.cursor.as_deref().map(cursor_decode).transpose()?;
    let mut models = super::model_history::model_dirs(source).ok_or("runs_unavailable")?;
    models.sort_by(|a, b| b.cmp(a));
    // The live file's own session is the only one whose runs can still execute.
    let current_model = source.parent();
    let now_ms = super::millis();
    let mut rows = Vec::new();
    let mut running = Vec::new();
    let mut stamps = Vec::new();
    for model in models {
        let date = model
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|v| v.to_str())
            .ok_or("runs_unavailable")?
            .to_owned();
        if filter.date.is_some_and(|value| value.to_string() != date) {
            continue;
        }
        let index = model.join("runs.ndjson");
        let index_rows = match std::fs::symlink_metadata(&index) {
            Ok(meta) => {
                if !meta.is_file() {
                    return Err("run_index_unavailable".into());
                }
                stamps.push((
                    date.clone(),
                    meta.len(),
                    meta.modified()?
                        .duration_since(std::time::UNIX_EPOCH)?
                        .as_nanos(),
                ));
                index_lines(&index, settings)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(_) => return Err("run_index_unavailable".into()),
        };
        let known: std::collections::BTreeSet<_> =
            index_rows.iter().map(|row| row.run.clone()).collect();
        rows.extend(
            index_rows
                .into_iter()
                .enumerate()
                .rev()
                .map(|(line, row)| (date.clone(), line, ListRow::Index(row))),
        );
        let mut directories: Vec<_> = std::fs::read_dir(&model)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        directories.sort_by(|a, b| b.cmp(a));
        for (ordinal, path) in directories.into_iter().enumerate() {
            let Some(id) = path
                .file_name()
                .and_then(|v| v.to_str())
                .filter(|id| super::model_history::valid_id(id))
            else {
                continue;
            };
            if known.contains(id) || ["prompts", "transcripts", "usage"].contains(&id) {
                continue;
            }
            if !id.starts_with("harness-") && !path.join("input.json").is_file() {
                continue;
            }
            let (row, is_running) = run_row(
                &path,
                id,
                settings,
                current_model == Some(model.as_path()),
                now_ms,
            )?;
            let position = (date.clone(), usize::MAX - ordinal, ListRow::Other(row));
            if is_running {
                running.push(position);
            } else {
                rows.push(position);
            }
        }
    }
    running.extend(rows);
    let rows: Vec<_> = running
        .into_iter()
        .filter(|(_, _, row)| row.matches(filter))
        .collect();
    let offset = match cursor {
        Some((date, line)) => rows
            .iter()
            .position(|(d, l, _)| *d == date && *l == line)
            .ok_or("run_cursor_invalid")?,
        None => 0,
    };
    let next = rows
        .get(offset.saturating_add(filter.limit))
        .map(|(date, line, _)| cursor_encode(date, *line));
    let page_rows: Vec<_> = rows
        .into_iter()
        .skip(offset)
        .take(filter.limit)
        .map(|(_, _, row)| row.into_value())
        .collect::<Result<_>>()?;
    let body = Value::Object(Map::from_iter([
        ("v".into(), json!(1)),
        ("runs".into(), Value::Array(page_rows)),
        (
            "next".into(),
            next.map(Value::String).unwrap_or(Value::Null),
        ),
    ]));
    let mut fingerprint = serde_json::to_vec(&stamps)?;
    fingerprint.extend(serde_json::to_vec(filter)?);
    fingerprint.extend(serde_json::to_vec(&body)?);
    let etag = format!("\"{}\"", &super::digest(&fingerprint)[..16]);
    Ok(RunPage { body, etag })
}

#[cfg(test)]
mod list_tests {}
