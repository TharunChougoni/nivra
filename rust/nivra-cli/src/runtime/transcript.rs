//! Bounded display evidence; original run artifacts remain authoritative.
use super::{DashboardSettings, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

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

pub struct Transcript<'a> {
    file: File,
    run: String,
    seq: u64,
    bytes: u64,
    cap: u64,
    text_cap: usize,
    args_cap: usize,
    truncated: bool,
    clock: &'a dyn Fn() -> i64,
}
#[cfg(test)]
thread_local! { static BLOCK_NEXT_START: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
#[cfg(test)]
pub(super) fn block_next_start() {
    BLOCK_NEXT_START.with(|flag| flag.set(true));
}
impl<'a> Transcript<'a> {
    pub fn start(
        dir: &Path,
        run: &str,
        settings: &DashboardSettings,
        started: Value,
        clock: &'a dyn Fn() -> i64,
    ) -> Result<Self> {
        let path = dir.join("transcript.ndjson");
        // One-shot fixture obstruction exercises a real open failure even when
        // tests run as root (where chmod would not make the file unwritable).
        #[cfg(test)]
        if BLOCK_NEXT_START.with(|flag| flag.replace(false)) {
            std::fs::create_dir(&path)?;
        }
        nivra_store::native_grant::file_path_ok(&path)?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        let mut writer = Self {
            file,
            run: run.to_owned(),
            seq: 0,
            bytes: 0,
            cap: settings.transcript_max_bytes as u64,
            text_cap: settings.transcript_text_max_bytes,
            args_cap: settings.transcript_args_max_bytes,
            truncated: false,
            clock,
        };
        writer.event("run_started", started)?;
        Ok(writer)
    }
    pub fn event(&mut self, kind: &str, fields: Value) -> Result<()> {
        if self.truncated && kind != "run_ended" {
            return Ok(());
        }
        let mut fields = redact(&fields)
            .as_object()
            .cloned()
            .ok_or("transcript_fields_invalid")?;
        if let Some(Value::String(text)) = fields.get_mut("text") {
            let clipped = text.len() > self.text_cap;
            *text = clip(text, self.text_cap).to_owned();
            if clipped {
                fields.insert("truncated".into(), json!(true));
            } else if kind == "assistant_text" {
                fields.insert("truncated".into(), json!(false));
            }
        }
        if let Some(args) = fields.get("args") {
            let encoded = serde_json::to_string(args)?;
            if encoded.len() > self.args_cap {
                // JSON strings add quotes and can double escapes. This prefix
                // keeps the serialized replacement within the argument budget.
                fields.insert(
                    "args".into(),
                    json!(clip(&encoded, self.args_cap.saturating_sub(2) / 2)),
                );
                fields.insert("args_truncated".into(), json!(true));
            }
        }
        for key in ["v", "run", "seq", "at_ms", "kind"] {
            fields.remove(key);
        }
        let event = TranscriptEvent {
            v: 1,
            run: self.run.clone(),
            seq: self.seq,
            at_ms: (self.clock)(),
            kind: kind.into(),
            fields,
        };
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        if kind != "run_ended" && self.bytes.saturating_add(line.len() as u64) > self.cap {
            self.truncated = true;
            let marker = TranscriptEvent {
                v: 1,
                run: self.run.clone(),
                seq: self.seq,
                at_ms: (self.clock)(),
                kind: "truncated".into(),
                fields: json!({"dropped_bytes":line.len()})
                    .as_object()
                    .unwrap()
                    .clone(),
            };
            line = serde_json::to_vec(&marker)?;
            line.push(b'\n');
        }
        self.file.write_all(&line)?;
        self.bytes += line.len() as u64;
        self.seq += 1;
        Ok(())
    }
    pub fn end(mut self, fields: Value) -> Result<()> {
        self.event("run_ended", fields)?;
        self.file.sync_all()?;
        Ok(())
    }
}
fn clip(text: &str, cap: usize) -> &str {
    let mut end = text.len().min(cap);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
#[derive(Default)]
pub struct RequestArchive {
    input_len: usize,
    input_sha: Option<String>,
    tools_sha: Option<String>,
}
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
impl RequestArchive {
    pub fn write(
        &mut self,
        dir: &Path,
        turn: usize,
        frame: &super::harness::Frame,
        request: &str,
        instructions_sha: &str,
    ) -> Result<()> {
        let tools_sha = super::digest(&serde_json::to_vec(&frame.tools)?);
        if turn == 0 {
            super::rollover::atomic(
                &dir.join("turn-00-request.json"),
                &json!({"request":frame,"instructions_sha256":instructions_sha}),
            )?;
        } else {
            let prefix = frame
                .input
                .get(..self.input_len)
                .ok_or("archive_request_input_regressed")?;
            if self.input_sha.as_deref()
                != Some(super::digest(&serde_json::to_vec(prefix)?).as_str())
            {
                return Err("archive_request_input_regressed".into());
            }
            let delta = RequestDelta {
                v: 1,
                base_turn: turn - 1,
                append_input: frame.input[self.input_len..].to_vec(),
                tools_sha256: tools_sha.clone(),
                tools: (self.tools_sha.as_ref() != Some(&tools_sha)).then(|| frame.tools.clone()),
                final_answer_only: frame.final_answer_only,
                request_sha256: super::digest(request.as_bytes()),
                instructions_sha256: instructions_sha.to_owned(),
            };
            super::rollover::atomic(
                &dir.join(format!("turn-{turn:02}-request-delta.json")),
                &serde_json::to_value(delta)?,
            )?;
        }
        self.input_len = frame.input.len();
        self.input_sha = Some(super::digest(&serde_json::to_vec(&frame.input)?));
        self.tools_sha = Some(tools_sha);
        Ok(())
    }
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
pub fn append_index_line(model: &Path, line: &RunIndexLine) -> Result<()> {
    let path = model.join("runs.ndjson");
    nivra_store::native_grant::file_path_ok(&path)?;
    let mut value = redact(&serde_json::to_value(line)?);
    if let Some(instruments) = value["instruments"].as_array_mut() {
        instruments.truncate(16);
    }
    let encode = |value: &Value| -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        Ok(bytes)
    };
    let mut bytes = encode(&value)?;
    if bytes.len() > 2048 {
        value["instruments"] = json!([]);
        bytes = encode(&value)?;
    }
    if bytes.len() > 2048 {
        return Err("dashboard_run_index_limit".into());
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(&bytes)?;
    Ok(())
}
pub fn directory_bytes(dir: &Path) -> Result<u64> {
    let mut bytes = 0u64;
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let meta = std::fs::symlink_metadata(entry.path())?;
        let size = if meta.is_file() {
            meta.len()
        } else if meta.is_dir() {
            directory_bytes(&entry.path())?
        } else {
            0
        };
        bytes = bytes
            .checked_add(size)
            .ok_or("dashboard_run_size_overflow")?;
    }
    Ok(bytes)
}
pub fn estimated_cost(
    settings: &DashboardSettings,
    model: &str,
    tokens: [Option<u64>; 3],
) -> Option<String> {
    use nivra_core::finance::Amount;
    let price = settings.model_prices.get(model)?;
    let [Some(input), Some(output), Some(cached)] = tokens else {
        return None;
    };
    let input_rate = Amount::parse(&price.input_inr_per_mtok).ok()?;
    let output_rate = Amount::parse(&price.output_inr_per_mtok).ok()?;
    let cached_rate = price
        .cached_inr_per_mtok
        .as_deref()
        .map(Amount::parse)
        .transpose()
        .ok()?;
    if input_rate < Amount::ZERO
        || output_rate < Amount::ZERO
        || cached_rate.is_some_and(|v| v < Amount::ZERO)
    {
        return None;
    }
    let mut amount = Amount::from_unsigned(input)
        .checked_mul(input_rate)
        .ok()?
        .checked_add(
            Amount::from_unsigned(output)
                .checked_mul(output_rate)
                .ok()?,
        )
        .ok()?;
    if let Some(rate) = cached_rate {
        amount = amount
            .checked_add(Amount::from_unsigned(cached).checked_mul(rate).ok()?)
            .ok()?;
    }
    amount
        .checked_mul(Amount::parse("0.000001").ok()?)
        .ok()
        .map(|v| v.to_string())
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
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn private_bounded_transcript_preserves_order_clips_and_redacts() {
        let dir = tempfile::tempdir_in("/var/tmp").unwrap();
        let settings = DashboardSettings {
            transcript_max_bytes: 65_536,
            transcript_text_max_bytes: 1024,
            transcript_args_max_bytes: 256,
            ..DashboardSettings::default()
        };
        let clock = || 1234;
        let mut transcript = Transcript::start(dir.path(), "fixture-run", &settings, json!({"role":"digest","Authorization":"private-auth","api_key":"private-key","nested":{"refresh_token":"private-token"},"cash":12}), &clock).unwrap();
        let path = dir.path().join("transcript.ndjson");
        assert!(
            Transcript::start(dir.path(), "fixture-run", &settings, json!({}), &clock).is_err()
        );
        transcript
            .event("assistant_text", json!({"text":"🙂".repeat(400)}))
            .unwrap();
        transcript
            .event(
                "tool_call",
                json!({"args":{"data":"\\n".repeat(1000)},"name":"fixture"}),
            )
            .unwrap();
        transcript
            .event("assistant_text", json!({"text":"Bearer private-bearer"}))
            .unwrap();
        for _ in 0..100 {
            transcript
                .event("assistant_text", json!({"text":"x".repeat(1024)}))
                .unwrap();
        }
        transcript.end(json!({"outcome":"accepted"})).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        for secret in [
            "private-auth",
            "private-key",
            "private-token",
            "private-bearer",
        ] {
            assert!(!raw.contains(secret));
        }
        let events = read(&path, 100_000).unwrap();
        assert_eq!(events[0]["cash"], 12);
        assert_eq!(events[1]["text"].as_str().unwrap().len(), 1024);
        assert_eq!(events[1]["truncated"], true);
        assert_eq!(events[2]["args_truncated"], true);
        assert!(serde_json::to_vec(&events[2]["args"]).unwrap().len() <= 256);
        assert_eq!(
            events
                .iter()
                .filter(|event| event["kind"] == "truncated")
                .count(),
            1
        );
        assert_eq!(events.last().unwrap()["kind"], "run_ended");
        for (seq, event) in events.iter().enumerate() {
            assert_eq!(event["seq"], seq);
            assert_eq!(event["at_ms"], 1234);
            assert_eq!(event["run"], "fixture-run");
        }
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"torn\":")
            .unwrap();
        assert_eq!(read(&path, 100_000).unwrap(), events);
        assert!(read(&path, 1).is_err());
    }
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
mod index_tests {
    use super::*;
    #[test]
    fn index_is_bounded_private_ordered_redacted_and_never_created_by_an_unfinished_run() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir_in("/var/tmp").unwrap();
        let crash = dir.path().join("harness-digest-crash");
        std::fs::create_dir(&crash).unwrap();
        let clock = || 1;
        let writer = Transcript::start(
            &crash,
            "crash",
            &DashboardSettings::default(),
            json!({}),
            &clock,
        )
        .unwrap();
        drop(writer);
        assert!(!dir.path().join("runs.ndjson").exists());
        for (index, outcome) in ["accepted", "abstain", "failure"].iter().enumerate() {
            let line = RunIndexLine {
                v: 1,
                run: format!("run-{index}"),
                role: "digest".into(),
                model: "fixture".into(),
                trigger_kind: Some("fill".into()),
                started_ms: index as i64,
                duration_ms: 1,
                outcome: (*outcome).into(),
                tokens_in: Some(10),
                tokens_out: Some(2),
                cost_estimate_inr: None,
                tool_calls: 1,
                tool_failures: usize::from(*outcome == "failure"),
                instruments: vec!["x".repeat(1000); 16],
                bytes: 1,
                mode: "paper".into(),
                profile_sha256: None,
            };
            append_index_line(dir.path(), &line).unwrap();
        }
        let path = dir.path().join("runs.ndjson");
        let raw = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = raw
            .lines()
            .map(|line| {
                assert!(line.len() < 2048);
                serde_json::from_str(line).unwrap()
            })
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["outcome"], "accepted");
        assert_eq!(lines[1]["outcome"], "abstain");
        assert_eq!(lines[2]["outcome"], "failure");
        assert!(lines.iter().all(|line| line["instruments"] == json!([])));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let mut line: RunIndexLine = serde_json::from_value(lines[0].clone()).unwrap();
        line.model = "Bearer fixture-secret".into();
        append_index_line(dir.path(), &line).unwrap();
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("fixture-secret")
        );
    }
    #[test]
    fn cost_uses_exact_configured_decimals_and_keeps_unknowns_unknown() {
        let mut settings = DashboardSettings::default();
        assert!(estimated_cost(&settings, "fixture", [Some(1); 3]).is_none());
        settings.model_prices.insert(
            "fixture".into(),
            super::super::ModelPrice {
                input_inr_per_mtok: "10".into(),
                output_inr_per_mtok: "20".into(),
                cached_inr_per_mtok: Some("2".into()),
            },
        );
        assert_eq!(
            estimated_cost(
                &settings,
                "fixture",
                [Some(1_000_000), Some(500_000), Some(100_000)]
            ),
            Some("20.2".into())
        );
        assert!(estimated_cost(&settings, "fixture", [Some(1), None, Some(0)]).is_none());
        assert!(estimated_cost(&settings, "fixture", [Some(1), Some(1), None]).is_none());
    }
}

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
mod list_tests {
    use super::*;
    fn indexed(model: &Path, index: usize, role: &str) -> RunIndexLine {
        std::fs::create_dir_all(model).unwrap();
        let date = model
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace('-', "");
        let row = RunIndexLine {
            v: 1,
            run: format!("harness-{role}-{date}{index:04}-fixture"),
            role: role.into(),
            model: "fixture-model".into(),
            trigger_kind: Some(
                if index.is_multiple_of(2) {
                    "fill"
                } else {
                    "timer"
                }
                .into(),
            ),
            started_ms: index as i64,
            duration_ms: 1,
            outcome: if index == 1 { "failure" } else { "accepted" }.into(),
            tokens_in: Some(10),
            tokens_out: Some(2),
            cost_estimate_inr: None,
            tool_calls: 1,
            tool_failures: usize::from(index == 1),
            instruments: vec![format!("SYNTHETIC|{index}")],
            bytes: 100,
            mode: "paper".into(),
            profile_sha256: None,
        };
        std::fs::create_dir(model.join(&row.run)).unwrap();
        append_index_line(model, &row).unwrap();
        row
    }
    #[test]
    fn list_pages_all_days_filters_and_skips_indexed_run_reads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        for date in ["2026-10-28", "2026-10-27"] {
            for (index, role) in ["digest", "supervisor", "scorer", "orchestrator"]
                .iter()
                .enumerate()
            {
                indexed(&root.join(date).join("model"), index, role);
            }
        }
        let legacy = root.join("2026-10-26/model/harness-supervisor-100-old");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(
            legacy.join("run.json"),
            br#"{"role":"supervisor","model":"legacy","trigger":{"kind":"fill"}}"#,
        )
        .unwrap();
        std::fs::write(
            legacy.join("output.json"),
            br#"{"output":{"actions":[],"stance":"abstain"}}"#,
        )
        .unwrap();
        let running = root.join("2026-10-28/model/harness-digest-9999-running");
        std::fs::create_dir(&running).unwrap();
        let clock = || super::super::millis();
        drop(Transcript::start(&running,"harness-digest-9999-running",&DashboardSettings::default(),json!({"role":"digest","model":"fixture-model","trigger":{"kind":"timer"},"mode":"paper"}),&clock).unwrap());
        let source = root.join("2026-10-28/model/live-stream.json");
        let settings = DashboardSettings::default();
        let mut filter = RunFilter {
            limit: 3,
            ..RunFilter::default()
        };
        let mut found = Vec::new();
        loop {
            let page = list_runs(&source, &filter, &settings).unwrap();
            if filter.cursor.is_none() {
                assert_eq!(page.body["runs"][0]["outcome"], "running");
            }
            found.extend(
                page.body["runs"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| format!("{}:{}", row["run"], row["started_ms"])),
            );
            filter.cursor = page.body["next"].as_str().map(str::to_owned);
            if filter.cursor.is_none() {
                break;
            }
        }
        assert_eq!(found.len(), 10);
        assert_eq!(
            found
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            10
        );
        filter = RunFilter {
            limit: 100,
            ..RunFilter::default()
        };
        let all = list_runs(&source, &filter, &settings).unwrap();
        assert_eq!(
            all.body["runs"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|v| v["legacy"] == true)
                .count(),
            1
        );
        filter.role = Some("supervisor".into());
        assert_eq!(
            list_runs(&source, &filter, &settings).unwrap().body["runs"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        filter.role = None;
        // The identity filter is applied before paging: every orchestrator group
        // role is included and a page never hides older rows behind digest volume.
        filter = RunFilter {
            limit: 2,
            identity: Some("orchestrator".into()),
            ..RunFilter::default()
        };
        let mut orchestrator = Vec::new();
        loop {
            let page = list_runs(&source, &filter, &settings).unwrap();
            let rows = page.body["runs"].as_array().unwrap();
            assert!(rows.iter().all(|row| {
                matches!(
                    row["role"].as_str(),
                    Some("scorer" | "supervisor" | "orchestrator")
                )
            }));
            orchestrator.extend(rows.iter().map(|row| row["run"].clone()));
            filter.cursor = page.body["next"].as_str().map(str::to_owned);
            if filter.cursor.is_none() {
                break;
            }
        }
        assert_eq!(orchestrator.len(), 7);
        filter = RunFilter {
            limit: 100,
            ..RunFilter::default()
        };
        filter.date = Some("2026-10-28".parse().unwrap());
        assert_eq!(
            list_runs(&source, &filter, &settings).unwrap().body["runs"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        filter.date = None;
        filter.trigger = Some("fill".into());
        assert_eq!(
            list_runs(&source, &filter, &settings).unwrap().body["runs"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        filter.trigger = None;
        filter.outcome = Some("abstain".into());
        assert_eq!(
            list_runs(&source, &filter, &settings).unwrap().body["runs"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        filter.outcome = None;
        filter.instrument = Some("SYNTHETIC|2".into());
        assert_eq!(
            list_runs(&source, &filter, &settings).unwrap().body["runs"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        filter.instrument = None;
        filter.failed = true;
        assert_eq!(
            list_runs(&source, &filter, &settings).unwrap().body["runs"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        filter.failed = false;
        filter.cursor = Some("bad-cursor".into());
        assert_eq!(
            list_runs(&source, &filter, &settings)
                .err()
                .unwrap()
                .to_string(),
            "run_cursor_invalid"
        );
        filter.cursor = None;
        filter.date = Some("2026-10-27".parse().unwrap());
        RUN_READS.with(|reads| reads.set(0));
        let first = list_runs(&source, &filter, &settings).unwrap();
        RUN_READS.with(|reads| assert_eq!(reads.get(), 0));
        assert_eq!(
            first.etag,
            list_runs(&source, &filter, &settings).unwrap().etag
        );
        indexed(&root.join("2026-10-27/model"), 50, "digest");
        assert_ne!(
            first.etag,
            list_runs(&source, &filter, &settings).unwrap().etag
        );
    }
    #[test]
    fn archived_and_expired_transcripts_report_interrupted_without_running_order() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("sessions");
        let current = root.join("2026-10-28/model");
        std::fs::create_dir_all(&current).unwrap();
        let source = current.join("live-stream.json");
        let settings = DashboardSettings::default();
        let now = super::super::millis();
        let start = |path: &Path, id: &str, at_ms: i64| {
            std::fs::create_dir_all(path).unwrap();
            let clock = || at_ms;
            let started = json!({"role":"digest","model":"fixture-model","trigger":{"kind":"timer"},"mode":"paper"});
            drop(Transcript::start(path, id, &settings, started, &clock).unwrap());
        };
        // An archived session's run can never still be executing (#19).
        start(
            &root.join("2026-10-26/model/harness-digest-100-archived"),
            "harness-digest-100-archived",
            now - 2 * 86_400_000,
        );
        // A current-session transcript started beyond the digest allowance is
        // interrupted even though its session is still open.
        start(
            &current.join("harness-digest-200-expired"),
            "harness-digest-200-expired",
            now - 10 * 60_000,
        );
        // Only the current run inside the allowance stays running, and only it
        // is sorted ahead of the archive rows.
        start(
            &current.join("harness-digest-300-live"),
            "harness-digest-300-live",
            now - 1_000,
        );
        let page = list_runs(
            &source,
            &RunFilter {
                limit: 100,
                ..RunFilter::default()
            },
            &settings,
        )
        .unwrap();
        let rows = page.body["runs"].as_array().unwrap();
        let row = |id: &str| rows.iter().find(|row| row["run"] == id).unwrap();
        for id in ["harness-digest-100-archived", "harness-digest-200-expired"] {
            assert_eq!(row(id)["outcome"], "interrupted", "{id}");
            assert_eq!(row(id)["running"], false, "{id}");
            // The archived transcript link stays available as display evidence.
            assert_eq!(row(id)["legacy"], false, "{id}");
        }
        assert_eq!(row("harness-digest-300-live")["outcome"], "running");
        assert_eq!(row("harness-digest-300-live")["running"], true);
        assert_eq!(rows[0]["run"], "harness-digest-300-live");
    }
    #[test]
    #[ignore = "release-mode 600-run budget measurement"]
    fn runs_list_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("sessions/2026-10-28/model");
        for index in 0..600 {
            indexed(&model, index, "digest");
        }
        let started = std::time::Instant::now();
        let page = list_runs(
            &model.join("live-stream.json"),
            &RunFilter {
                limit: 50,
                ..RunFilter::default()
            },
            &DashboardSettings::default(),
        )
        .unwrap();
        let micros = started.elapsed().as_micros();
        println!("runs_list_600_us={micros}");
        assert_eq!(page.body["runs"].as_array().unwrap().len(), 50);
        assert!(micros <= 5000, "600-run list exceeded 5 ms: {micros} us");
    }
}
