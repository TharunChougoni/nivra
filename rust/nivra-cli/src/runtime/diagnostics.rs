//! Day-level diagnostics kept in the session's own directory, so a closed day
//! can be reconstructed after the host's service journal has rotated. One
//! bounded, append-only `diagnostics.ndjson` per session: startup, health
//! transitions, failures, shutdown, secret-guard findings and research
//! preemptions. Lines carry codes, counts and flags only. They never hold
//! tokens, account payloads, host paths, quotes or model text, and every line
//! passes the transcript redaction before it is written.
use serde_json::{Value, json};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(crate) const FILE: &str = "diagnostics.ndjson";
/// The file stops growing here; one capped marker replaces later lines.
const MAX_BYTES: u64 = 1_000_000;
const CAPPED: &[u8] = b"{\"event\":\"diagnostics_capped\"}\n";

/// Append one event. Diagnostics never stop the controller: a failed write
/// is reported once on stderr with a static code and otherwise ignored.
pub(crate) fn note(directory: &Path, at_ms: i64, event: &str, detail: Value) {
    if write(directory, at_ms, event, detail).is_err() {
        eprintln!("diagnostics_write_failed event={event}");
    }
}

/// Where the secret guard matched at `site` (`decision_plan`,
/// `research_request`, ...), when it did: the pattern classes, field paths,
/// lengths and offsets, never the matched text. `action` is `blocked` when the
/// guard refused the input and `redacted` when it was stored redacted.
pub(crate) fn secret_guard(
    directory: &Path,
    at_ms: i64,
    site: &str,
    action: &str,
    turn_id: Option<&str>,
    findings: &super::transcript::Findings,
) {
    if findings.count > 0 {
        note(
            directory,
            at_ms,
            "secret_guard",
            json!({"site":site,"action":action,"turn_id":turn_id,
                "count":findings.count,"matches":findings.matches}),
        );
    }
}

/// A host error as diagnostics store it: a static snake_case code, or a
/// placeholder for anything else.
pub(crate) fn code(text: &str) -> &str {
    if !text.is_empty()
        && text.len() <= 96
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        text
    } else {
        "error_redacted"
    }
}

fn write(directory: &Path, at_ms: i64, event: &str, detail: Value) -> std::io::Result<()> {
    let mut line = json!({"at_ms":at_ms,"event":event});
    if let (Some(line), Value::Object(detail)) = (line.as_object_mut(), detail) {
        line.extend(detail);
    }
    let mut bytes = serde_json::to_vec(&super::transcript::redact(&line))?;
    bytes.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .mode(0o600)
        .open(directory.join(FILE))?;
    let len = file.metadata()?.len();
    if len + bytes.len() as u64 <= MAX_BYTES {
        // One write per line, so lines from the controller and the lane
        // worker never interleave.
        return file.write_all(&bytes);
    }
    let mut tail = vec![0; CAPPED.len()];
    if len >= CAPPED.len() as u64 {
        file.seek(SeekFrom::Start(len - CAPPED.len() as u64))?;
        file.read_exact(&mut tail)?;
    }
    if tail != CAPPED {
        file.write_all(CAPPED)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostics_are_redacted_appended_and_capped_once() {
        let dir = tempfile::tempdir().unwrap();
        note(
            dir.path(),
            1,
            "failure",
            json!({"reason":"feed_down","token":"abc","detail":"Bearer xyz"}),
        );
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        let line: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(
            line,
            json!({"at_ms":1,"event":"failure","reason":"feed_down","detail":"[redacted]"})
        );
        let filler = "x".repeat(10_000);
        for at in 0..120 {
            note(dir.path(), at, "health", json!({"health":filler}));
        }
        let text = std::fs::read(dir.path().join(FILE)).unwrap();
        assert!(text.len() as u64 <= MAX_BYTES + CAPPED.len() as u64);
        assert!(text.ends_with(CAPPED));
        assert_eq!(
            text.windows(CAPPED.len()).filter(|w| *w == CAPPED).count(),
            1
        );
    }
}
