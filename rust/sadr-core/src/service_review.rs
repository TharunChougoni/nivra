//! Inert unit-text diagnostics. No filesystem, service commands, credentials,
//! network or installed/host/live/start acceptance authority.
use serde::Serialize;
use std::{collections::BTreeMap, fmt};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Service review unavailable")
    }
}
impl std::error::Error for Error {}
#[derive(Serialize)]
pub struct UnitReview {
    version: &'static str,
    user: String,
    restart: String,
    credential_assignment: bool,
    broad_write: bool,
    native_refresh_narrow: bool,
    installed: bool,
    host_verified: bool,
    live_enabled: bool,
    would_start: bool,
}
impl fmt::Debug for UnitReview {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UnitReview(redacted, not installed, no authority)")
    }
}
impl UnitReview {
    pub fn user(&self) -> &str {
        &self.user
    }
    pub fn restart(&self) -> &str {
        &self.restart
    }
    pub fn credential_assignment(&self) -> bool {
        self.credential_assignment
    }
    pub fn broad_write(&self) -> bool {
        self.broad_write
    }
    pub fn native_refresh_narrow(&self) -> bool {
        self.native_refresh_narrow
    }
}
fn whitespace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
fn safe_user(user: &str) -> bool {
    if user == "UNSPECIFIED" {
        return true;
    }
    if user.is_empty() || user.len() > 32 || !user.is_ascii() {
        return false;
    }
    let lower = user.to_ascii_lowercase();
    if [
        "token",
        "secret",
        "password",
        "bearer",
        "credential",
        "api_key",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return false;
    }
    if user.bytes().all(|b| b.is_ascii_digit()) {
        return user.len() <= 10;
    }
    let user = user.strip_suffix('$').unwrap_or(user);
    user.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_lowercase() || *b == b'_')
        && user
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_.-".contains(&b))
}
/// Normalize only lexical systemd path modifiers, slashes and dot components.
/// No lookup/canonicalization, namespace acceptance or write permission follows.
fn broad_alias(path: &str) -> bool {
    let path = path.trim_start_matches(['-', '+', '!']);
    if !path.starts_with('/') {
        return false;
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => (),
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    matches!(
        parts.as_slice(),
        [] | ["home"] | ["home", "ubuntu"] | ["root"]
    )
}
/// Retains first-directive semantics. Unlike the historical parser, arbitrary
/// credential-ish User/Restart values refuse before any summary can echo them.
pub fn review_unit(text: &str) -> Result<UnitReview, Error> {
    if text.len() > 80_000
        || text.chars().count() > 20_000
        || text.trim_matches(whitespace).is_empty()
        || text.contains('\0')
    {
        return Err(Error);
    }
    let mut values = BTreeMap::new();
    for line in text.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ]) {
        let line = line.trim_matches(whitespace);
        if line.is_empty() || line.starts_with(['#', '[']) {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            values
                .entry(key.trim_matches(whitespace))
                .or_insert(value.trim_matches(whitespace));
        }
    }
    let user = values
        .get("User")
        .copied()
        .filter(|s| !s.is_empty())
        .unwrap_or("UNSPECIFIED");
    let restart = values
        .get("Restart")
        .copied()
        .filter(|s| !s.is_empty())
        .unwrap_or("UNSPECIFIED");
    if !safe_user(user)
        || ![
            "UNSPECIFIED",
            "no",
            "on-success",
            "on-failure",
            "on-abnormal",
            "on-watchdog",
            "on-abort",
            "always",
        ]
        .contains(&restart)
    {
        return Err(Error);
    }
    let read_write = values.get("ReadWritePaths").copied().unwrap_or("");
    // This reviewer does not interpret systemd quoting, escapes or specifiers.
    // Refuse rather than turning an unknown expansion into a narrow-write claim.
    if read_write.contains(['\'', '"', '\\', '%']) {
        return Err(Error);
    }
    let parts: Vec<_> = read_write
        .split(whitespace)
        .filter(|s| !s.is_empty())
        .collect();
    let broad_write = parts.iter().any(|path| broad_alias(path));
    let subscription = parts
        .iter()
        .filter(|path| {
            path.trim_end_matches('/')
                .ends_with("ind-sadr-subscription")
        })
        .count();
    let credential_assignment = ["TOKEN=", "SECRET=", "PASSWORD=", "BEGIN "]
        .iter()
        .any(|marker| text.contains(marker));
    Ok(UnitReview {
        version: "unit-review-v1",
        user: user.into(),
        restart: restart.into(),
        credential_assignment,
        broad_write,
        native_refresh_narrow: subscription == 1 && !broad_write && !credential_assignment,
        installed: false,
        host_verified: false,
        live_enabled: false,
        would_start: false,
    })
}
/// Renders review-only text. The hostname is bounded ASCII DNS syntax, with
/// reserved local labels refused; the binary/service is never invoked here.
pub fn render_loopback_tunnel(port: i64, hostname: &str) -> Result<String, Error> {
    if !(1024..=65535).contains(&port) || hostname.len() > 253 || !hostname.is_ascii() {
        return Err(Error);
    }
    let labels: Vec<_> = hostname.split('.').collect();
    fn alphanumeric(b: u8) -> bool {
        b.is_ascii_lowercase() || b.is_ascii_digit()
    }
    if labels.len() < 2
        || labels.iter().any(|label| {
            label.is_empty()
                || label.len() > 63
                || ["localhost", "local", "internal"].contains(label)
                || !label.bytes().all(|b| alphanumeric(b) || b == b'-')
                || !alphanumeric(label.as_bytes()[0])
                || !alphanumeric(label.as_bytes()[label.len() - 1])
        })
    {
        return Err(Error);
    }
    Ok(format!(
        "# NOT_INSTALLED. Review only; this text does not start a tunnel.\n# Dashboard listen address stays loopback. No credential is stored here.\n[Unit]\nDescription=SADR loopback tunnel forwarder\n[Service]\nType=exec\nExecStart=/usr/bin/cloudflared tunnel --no-autoupdate --url http://127.0.0.1:{port} --hostname {hostname}\nRestart=no\nNoNewPrivileges=yes\n"
    ))
}
