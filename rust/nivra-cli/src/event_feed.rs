//! Bounded in-process change feed for the dashboard event stream. It carries
//! summary changes only and is never an execution or validation authority.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, OnceLock},
};
const CAP: usize = 128;
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_releases_the_ring_and_wakes_on_a_summary_change() {
        let before = snapshot(0)["next"].as_u64().unwrap();
        let waiter = std::thread::spawn(move || {
            wait_after(before, std::time::Duration::from_secs(5));
            snapshot(before)
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        summary_changed("\"0123456789abcdef\"");
        summary_changed("not-an-etag");
        let seen = waiter.join().unwrap();
        let events = seen["events"].as_array().unwrap();
        assert!(events.iter().any(|e| e["kind"] == "summary_changed"));
        assert!(events.iter().all(|e| e["etag"] != "not-an-etag"));
    }
}
