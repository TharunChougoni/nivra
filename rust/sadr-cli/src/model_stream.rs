//! Bounded, read-only model telemetry. Never an execution or validation authority.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
};
const CAP: usize = 128;
pub const KIND_SYSTEM: &str = "system";
pub const KIND_TOOL_CALL: &str = "tool_call";
pub const KIND_TOOL_RESULT: &str = "tool_result";
pub const KIND_TRIGGER: &str = "trigger";
#[derive(Default)]
struct Ring {
    next: u64,
    events: VecDeque<Arc<Value>>,
    text_bytes: usize,
    dropped: u64,
}
impl Ring {
    fn push(&mut self, mut event: Value) {
        self.next += 1;
        event["seq"] = json!(self.next);
        self.text_bytes += event["text"].as_str().map_or(0, str::len);
        self.events.push_back(Arc::new(event));
        while self.events.len() > CAP || self.text_bytes > 262144 {
            if let Some(event) = self.events.pop_front() {
                self.text_bytes -= event["text"].as_str().map_or(0, str::len);
                self.dropped += 1;
            }
        }
    }
}
static FEED: OnceLock<Mutex<Ring>> = OnceLock::new();
static CHANGED: Condvar = Condvar::new();
static EPOCH: OnceLock<String> = OnceLock::new();
type Destination = (std::path::PathBuf, String, u128, Option<String>);
thread_local! {static DESTINATION:std::cell::RefCell<Option<Destination>>=const {std::cell::RefCell::new(None)};}
pub struct Scope(Option<Destination>);
impl Scope {
    pub fn new(path: std::path::PathBuf, binding: String, archive_run: Option<String>) -> Self {
        Self(DESTINATION.with(|slot| {
            let archive_run = archive_run.or_else(|| {
                let current = slot.borrow();
                current
                    .as_ref()
                    .filter(|v| v.0 == path && v.1 == binding)
                    .and_then(|v| v.3.clone())
            });
            slot.replace(Some((path, binding, 0, archive_run)))
        }))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        DESTINATION.with(|slot| slot.replace(self.0.take()));
    }
}
fn feed() -> &'static Mutex<Ring> {
    FEED.get_or_init(|| Mutex::new(Ring::default()))
}
/// Condvar waiting releases the ring mutex; no reader owns it while asleep.
pub fn wait_after(seq: u64, timeout: std::time::Duration) {
    if let Ok(state) = feed().lock() {
        drop(CHANGED.wait_timeout_while(state, timeout, |state| state.next == seq));
    }
}
fn push(event: Value) {
    if let Ok(mut state) = feed().lock() {
        state.push(event);
    }
    CHANGED.notify_all();
}
pub fn summary_changed(etag: &str) {
    if etag.len() != 18
        || !etag.starts_with('"')
        || !etag.ends_with('"')
        || !etag.as_bytes()[1..17].iter().all(u8::is_ascii_hexdigit)
    {
        return;
    }
    push(
        json!({"kind":"summary_changed","etag":etag,"archive_run":null,"execution_authority":false}),
    );
}
pub fn publish(role: &str, model: &str, run: &str, kind: &str, text: &str) {
    let tool = matches!(kind, "tool_requested" | "tool_running").then_some(text);
    publish_inner(role, model, run, kind, tool, text);
}
pub fn publish_tool(role: &str, model: &str, run: &str, kind: &str, tool: &str, text: &str) {
    publish_inner(role, model, run, kind, Some(tool), text);
}
fn publish_inner(role: &str, model: &str, run: &str, kind: &str, tool: Option<&str>, text: &str) {
    if !["digest", "decide", "orchestrator"].contains(&role) {
        return;
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let (binding, archive_run) = DESTINATION.with(|slot| {
        let slot = slot.borrow();
        (
            slot.as_ref().map(|v| v.1.clone()),
            slot.as_ref().and_then(|v| v.3.clone()),
        )
    });
    let filtered = text
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect::<String>();
    let mut end = filtered
        .len()
        .min(if kind == "final_output" { 65536 } else { 8192 });
    while !filtered.is_char_boundary(end) {
        end -= 1;
    }
    let event = json!({"at_ms":at,"role":role,"model":model,"run":run,"archive_run":archive_run,"tool":tool,"session_binding":binding,"kind":kind,"text":&filtered[..end],"truncated":end<filtered.len(),"execution_authority":false});
    // Only append/copy Arc pointers under this lock; readers build JSON after
    // releasing it. Waiting for this bounded critical section avoids telemetry loss.
    push(event);
    // A disposable redacted projection for a separate dashboard process. Never
    // fsync or hold the telemetry lock across disk I/O; failure affects display only.
    let destination = DESTINATION.with(|slot| {
        let mut slot = slot.borrow_mut();
        let (path, binding, last, _) = slot.as_mut()?;
        if at.saturating_sub(*last) < 500
            && !matches!(
                kind,
                "request_failed" | "response_complete_unvalidated" | "host_validated" | "run_ended"
            )
        {
            return None;
        }
        *last = at;
        Some((path.clone(), binding.clone()))
    });
    if let Some((path, binding)) = destination {
        let _ = persist(&path, &binding, at);
    }
}
fn persist(path: &std::path::Path, binding: &str, at: u128) -> Result<(), ()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    sadr_store::native_grant::file_path_ok(path).map_err(|_| ())?;
    let mut data = snapshot(0);
    // A contended in-process lock yields a placeholder; never replace the last
    // good projection with it, or readers flicker to "Unavailable" (#64).
    if data["unavailable"] == true {
        return Err(());
    }
    data["source_binding"] = json!(binding);
    data["observed_at_ms"] = json!(at);
    if let Some(events) = data["events"].as_array_mut() {
        events.retain(|v| v["session_binding"] == binding);
    }
    let bytes = serde_json::to_vec(&data).map_err(|_| ())?;
    if bytes.len() > 1_048_576 {
        return Err(());
    }
    let temp = path.with_file_name(format!("model-stream-{}-{at}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temp)
        .map_err(|_| ())?;
    file.write_all(&bytes).map_err(|_| ())?;
    std::fs::rename(temp, path).map_err(|_| ())
}
pub fn snapshot(after: u64) -> Value {
    snapshot_from(feed(), after)
}
fn snapshot_from(ring: &Mutex<Ring>, after: u64) -> Value {
    let (events, next, oldest, dropped) = {
        let Ok(state) = ring.try_lock() else {
            return json!({"unavailable":true,"events":[]});
        };
        let oldest = state
            .events
            .front()
            .and_then(|v| v["seq"].as_u64())
            .unwrap_or(state.next + 1);
        (
            state
                .events
                .iter()
                .filter(|v| v["seq"].as_u64().unwrap_or(0) > after)
                .cloned()
                .collect::<Vec<_>>(),
            state.next,
            oldest,
            state.dropped,
        )
    };
    let epoch = EPOCH.get_or_init(|| {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        )
    });
    let events: Vec<_> = events.into_iter().map(|v| v.as_ref().clone()).collect();
    json!({"epoch":epoch,"events":events,"next":next,"oldest":oldest,"gap":after>next||(after>0&&after+1<oldest),"dropped":dropped,"partial_output_is_unvalidated":true})
}
#[derive(Debug)]
pub struct FeedUnavailable;
pub fn read_snapshot(
    path: &std::path::Path,
    binding: &str,
    after: u64,
) -> Result<Value, FeedUnavailable> {
    read_snapshot_inner(path, binding, after).map_err(|_| FeedUnavailable)
}
fn read_snapshot_inner(path: &std::path::Path, binding: &str, after: u64) -> Result<Value, ()> {
    use std::{
        io::Read,
        os::unix::fs::{MetadataExt, OpenOptionsExt},
    };
    sadr_store::native_grant::file_path_ok(path).map_err(|_| ())?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ())?;
    let meta = file.metadata().map_err(|_| ())?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.mode() & 0o777 != 0o600
        || meta.uid() != std::fs::metadata("/proc/self").map_err(|_| ())?.uid()
        || meta.len() > 1_048_576
    {
        return Err(());
    }
    let mut bytes = vec![];
    file.take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    let raw: Value = serde_json::from_slice(&bytes).map_err(|_| ())?;
    if raw["source_binding"] != binding || raw["partial_output_is_unvalidated"] != true {
        return Err(());
    }
    let mut events = vec![];
    for item in raw["events"]
        .as_array()
        .filter(|v| v.len() <= 128)
        .ok_or(())?
    {
        if item["session_binding"] != binding || item["execution_authority"] != false {
            return Err(());
        }
        if item["seq"].as_u64().ok_or(())? <= after {
            continue;
        }
        let mut event = json!({});
        for key in [
            "seq",
            "at_ms",
            "role",
            "model",
            "run",
            "archive_run",
            "tool",
            "session_binding",
            "kind",
            "text",
            "execution_authority",
            "truncated",
        ] {
            event[key] = item[key].clone();
        }
        if event["text"].as_str().is_none_or(|s| s.len() > 65536) {
            return Err(());
        }
        events.push(event);
    }
    let next = raw["next"].as_u64().ok_or(())?;
    let oldest = raw["oldest"].as_u64().ok_or(())?;
    Ok(
        json!({"events":events,"next":next,"oldest":oldest,"gap":after>next||(after>0&&after+1<oldest),"epoch":raw["epoch"],"observed_at_ms":raw["observed_at_ms"],"dropped":raw["dropped"],"partial_output_is_unvalidated":true}),
    )
}

/// Redact complete credentials and credential prefixes crossing text deltas.
pub struct Decoder {
    role: String,
    model: String,
    run: String,
    secrets: Vec<String>,
    wire: Vec<u8>,
    pending: String,
    seen_text: bool,
    observer: Option<EventObserver>,
}
pub type EventObserver = Box<dyn FnMut(&str, &str)>;
impl Decoder {
    pub fn new(role: &str, model: &str, run: &str, secrets: Vec<String>) -> Self {
        Self {
            role: role.into(),
            model: model.into(),
            run: run.into(),
            secrets: secrets.into_iter().filter(|s| !s.is_empty()).collect(),
            wire: vec![],
            pending: String::new(),
            seen_text: false,
            observer: None,
        }
    }
    pub fn with_observer(mut self, observer: EventObserver) -> Self {
        self.observer = Some(observer);
        self
    }
    pub fn bytes(&mut self, chunk: &[u8]) {
        if self.wire.len() + chunk.len() > 1_000_000 {
            self.wire.clear();
            return;
        }
        self.wire.extend_from_slice(chunk);
        while let Some(end) = self.wire.iter().position(|b| *b == b'\n') {
            let line = self.wire.drain(..=end).collect::<Vec<_>>();
            let Ok(line) = std::str::from_utf8(&line) else {
                continue;
            };
            let Some(raw) = line.trim().strip_prefix("data:") else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<Value>(raw.trim()) else {
                continue;
            };
            match v["type"].as_str() {
                Some("response.output_text.delta") => {
                    if let Some(t) = v["delta"].as_str() {
                        self.seen_text = true;
                        self.text(t, false);
                    }
                }
                Some("response.web_search_call.in_progress") => {
                    self.event("tool_running", "web_search")
                }
                Some("response.x_search_call.in_progress") => {
                    self.event("tool_running", "x_search")
                }
                Some("response.output_item.added") => {
                    let item = &v["item"];
                    if item["type"] == "function_call"
                        && let Some(name) = item["name"].as_str().filter(|s| {
                            [
                                "read_evidence",
                                "get_market_snapshot",
                                "get_instrument_features",
                                "get_intraday_context",
                            ]
                            .contains(s)
                        })
                    {
                        self.event("tool_requested", name);
                    }
                }
                _ => {}
            }
        }
    }
    fn event(&mut self, kind: &str, text: &str) {
        if let Some(observer) = &mut self.observer {
            observer(kind, text);
        }
        publish(&self.role, &self.model, &self.run, kind, text);
    }
    fn text(&mut self, text: &str, finish: bool) {
        self.pending.push_str(text);
        for secret in &self.secrets {
            self.pending = self.pending.replace(secret, "[redacted]");
        }
        let mut keep = 0;
        if !finish {
            for secret in &self.secrets {
                for n in 1..secret.len().min(self.pending.len() + 1) {
                    if secret.is_char_boundary(n) && self.pending.ends_with(&secret[..n]) {
                        keep = keep.max(n);
                    }
                }
            }
        }
        let end = self.pending.len() - keep;
        if end > 0 {
            let safe = self.pending[..end].to_owned();
            self.pending.drain(..end);
            for part in safe.chars().collect::<Vec<_>>().chunks(2048) {
                self.event("text_delta", &part.iter().collect::<String>());
            }
        }
    }
    pub fn final_output(&mut self, value: &Value) {
        let messages = value["output"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|i| i["type"] == "message" && i["role"] == "assistant")
            .collect::<Vec<_>>();
        let has_final = messages.iter().any(|i| i["phase"] == "final_answer");
        let mut final_text = messages
            .iter()
            .filter(|i| !has_final || i["phase"] == "final_answer")
            .filter_map(|i| i["content"].as_array())
            .flatten()
            .filter(|p| p["type"] == "output_text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        for secret in &self.secrets {
            final_text = final_text.replace(secret, "[redacted]");
        }
        let keep = self
            .secrets
            .iter()
            .flat_map(|secret| {
                (1..secret.len().min(final_text.len() + 1))
                    .filter(|n| secret.is_char_boundary(*n) && final_text.ends_with(&secret[..*n]))
            })
            .max()
            .unwrap_or_default();
        if keep > 0 {
            final_text.truncate(final_text.len() - keep);
            final_text.push_str("[redacted credential prefix]");
        }
        if !self.seen_text {
            for item in value["output"].as_array().into_iter().flatten() {
                if item["type"] == "message" && item["role"] == "assistant" {
                    for part in item["content"].as_array().into_iter().flatten() {
                        if part["type"] == "output_text"
                            && let Some(text) = part["text"].as_str()
                        {
                            self.text(text, false);
                        }
                    }
                }
            }
        }
        self.finish();
        if !final_text.is_empty() {
            self.event("final_output", &final_text);
        }
    }
    pub fn finish(&mut self) {
        // An unfinished prefix may be a truncated credential; never flush it raw.
        if !self.pending.is_empty() {
            self.pending.clear();
            self.event("text_delta", "[redacted incomplete credential prefix]");
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    static SERIAL: Mutex<()> = Mutex::new(());
    #[test]
    fn condvar_wait_releases_the_ring_and_wakes_on_publish() {
        let _serial = SERIAL.lock().unwrap();
        let after = snapshot(0)["next"].as_u64().unwrap();
        let barrier = std::sync::Barrier::new(2);
        let started = std::time::Instant::now();
        std::thread::scope(|scope| {
            let waiting = scope.spawn(|| {
                barrier.wait();
                wait_after(after, std::time::Duration::from_secs(1));
                snapshot(0)
            });
            barrier.wait();
            publish("digest", "fixture", "wait-fixture", "text_delta", "fixture");
            assert!(waiting.join().unwrap()["next"].as_u64().unwrap() > after);
        });
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
        let after = snapshot(0)["next"].as_u64().unwrap();
        let started = std::time::Instant::now();
        wait_after(after, std::time::Duration::from_millis(5));
        assert!(started.elapsed() >= std::time::Duration::from_millis(5));
    }
    #[test]
    fn concurrent_readers_only_lose_overflow_evictions_and_scopes_retain_identity() {
        let _serial = SERIAL.lock().unwrap();
        let ring = Arc::new(Mutex::new(Ring::default()));
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let ring = ring.clone();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        let _ = snapshot_from(&ring, 0);
                    }
                });
            }
            for _ in 0..10_000 {
                ring.lock().unwrap().push(json!({"text":"fixture"}));
            }
        });
        let data = snapshot_from(&ring, 0);
        assert_eq!(data["next"], 10_000);
        assert_eq!(data["dropped"], 10_000 - CAP);
        assert_eq!(data["events"].as_array().unwrap().len(), CAP);
        assert_eq!(ring.lock().unwrap().text_bytes, CAP * "fixture".len());
        let dir = tempfile::tempdir_in("/var/tmp").unwrap();
        let path = dir.path().join("model-stream.json");
        let _scope = Scope::new(
            path.clone(),
            "scope-fixture".into(),
            Some("harness-digest-1-fixture".into()),
        );
        publish_tool(
            "digest",
            "fixture",
            "scope-run",
            "tool_completed",
            "read_evidence",
            "read_evidence completed in 1 ms",
        );
        persist(&path, "scope-fixture", 1).unwrap();
        let data = read_snapshot(&path, "scope-fixture", 0).unwrap();
        let event = data["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["run"] == "scope-run")
            .unwrap();
        assert_eq!(event["archive_run"], "harness-digest-1-fixture");
        assert_eq!(event["tool"], "read_evidence");
        assert_eq!(event["execution_authority"], false);
    }
    #[test]
    fn contended_lock_never_overwrites_last_good_projection() {
        let _serial = SERIAL.lock().unwrap();
        let dir = tempfile::tempdir_in("/var/tmp").unwrap();
        let path = dir.path().join("model-stream.json");
        let good = (0..1000)
            .find_map(|_| persist(&path, "b", 1).ok())
            .is_some();
        assert!(good);
        let before = std::fs::read(&path).unwrap();
        assert!(read_snapshot(&path, "b", 0).is_ok());
        let held = loop {
            if let Ok(guard) = feed().try_lock() {
                break guard;
            }
        };
        assert_eq!(snapshot(0)["unavailable"], true);
        assert!(persist(&path, "b", 2).is_err());
        drop(held);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(read_snapshot(&path, "b", 0).is_ok());
    }
    fn events_after(after: u64) -> Vec<Value> {
        for _ in 0..100 {
            let snapshot = snapshot(after);
            if snapshot["unavailable"] != true {
                return snapshot["events"].as_array().cloned().unwrap_or_default();
            }
            std::thread::yield_now();
        }
        panic!("model stream unavailable");
    }
    #[test]
    fn orchestrator_tool_call_publishes_but_unknown_role_does_not() {
        let _serial = SERIAL.lock().unwrap();
        let mut published = false;
        for attempt in 0..1000 {
            let run = format!("a9-orch-{attempt}");
            publish("orchestrator", "model", &run, KIND_TOOL_CALL, "tool call");
            let snapshot = snapshot(0);
            if snapshot["unavailable"] != true
                && snapshot["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|event| {
                        event["run"] == run.as_str()
                            && event["role"] == "orchestrator"
                            && event["kind"] == KIND_TOOL_CALL
                    })
            {
                published = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(published, "orchestrator tool call was never published");
        let run = "a9-rogue";
        publish("rogue", "model", run, KIND_TOOL_CALL, "tool call");
        assert!(
            !events_after(0)
                .iter()
                .any(|event| event["run"] == run && event["role"] == "rogue")
        );
    }
}
