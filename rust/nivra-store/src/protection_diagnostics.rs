//! Read-only association of explicitly selected timing streams. No session authority.
use serde_json::{Value, json};
use std::{
    fmt,
    path::{Component, Path, PathBuf},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Protection diagnostics unavailable")
    }
}
impl std::error::Error for Error {}
fn original_path(path: &Path, role: &str) -> Result<PathBuf, Error> {
    let path = std::path::absolute(path).map_err(|_| Error)?;
    if path.file_name().and_then(|s| s.to_str())
        != Some(format!("ledger.db.{role}-timings.jsonl").as_str())
        || path.components().any(|p| matches!(p, Component::ParentDir))
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(|c| (c as u32) < 32 || c == '\u{7f}'))
    {
        return Err(Error);
    }
    Ok(path)
}
/// Reads sequentially, never claims an atomic snapshot or authenticated session.
pub fn build(protection: &Path, supervision: Option<&Path>) -> Result<Value, Error> {
    build_with(protection, supervision, |path| {
        crate::timing_journal::summarize_timings(path).map_err(|_| Error)
    })
}
fn build_with(
    protection: &Path,
    supervision: Option<&Path>,
    mut summarize: impl FnMut(&Path) -> Result<Value, Error>,
) -> Result<Value, Error> {
    let protection = original_path(protection, "protection")?;
    let supervision = supervision
        .map(|p| original_path(p, "supervision"))
        .transpose()?;
    if supervision
        .as_ref()
        .is_some_and(|s| s.parent() != protection.parent())
    {
        return Err(Error);
    }
    let p = summarize(&protection)?;
    let s = supervision
        .as_ref()
        .map(|path| summarize(path))
        .transpose()?;
    let complete = p["state"] == "COMPLETE" && s.as_ref().is_none_or(|v| v["state"] == "COMPLETE");
    Ok(json!({
        "version":"protection-diagnostics-v1", "session_directory":protection.parent().ok_or(Error)?.to_str().ok_or(Error)?,
        "session_association":"DECLARED_NOT_AUTHENTICATED", "protection":p, "supervision":s,
        "supervision_selection": if supervision.is_some() {"SELECTED"} else {"NOT_SELECTED"},
        "selected_streams_state":if complete {"COMPLETE"} else {"INCOMPLETE"},
        "atomic_snapshot":false, "historical_causality":"UNKNOWN", "process_health":"UNVERIFIED",
        "financial_freshness":"UNVERIFIED", "reliability_acceptance":"UNVERIFIED", "activation":"NOT_AUTHORIZED",
        "live_enabled":false, "would_start":false,
        "limitations":["Selected timing files do not authenticate a session or original journal continuity.",
        "Absent or dropped measurements cannot be reconstructed.",
        "Nested span maxima can come from different ticks and must not be added.",
        "Measurements alone do not establish the cause of historical protection gaps."]
    }))
}
