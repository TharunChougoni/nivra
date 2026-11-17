//! Socket-free response stream boundary. No HTTP client, credentials, cookies or retry.
use crate::native::{self, ParsedResponse, ResponseKind};
use std::fmt;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Native(native::Error),
    Deadline,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Native(e) => e.fmt(f),
            Self::Deadline => f.write_str("deadline"),
        }
    }
}
impl std::error::Error for Error {}
impl From<native::Error> for Error {
    fn from(e: native::Error) -> Self {
        Self::Native(e)
    }
}
pub struct Headers<'a> {
    pub status: u16,
    pub content_type: Option<&'a str>,
    pub encoding: Option<&'a str>,
}
/// Inject monotonic milliseconds. Once expired/reversed, this gate stays rejected.
#[derive(Debug)]
pub struct Deadline {
    last: u64,
    until: u64,
    failed: bool,
}
impl Deadline {
    pub fn new(now: u64, timeout_ms: u64) -> Result<Self, Error> {
        if !(1..=30000).contains(&timeout_ms) {
            return Err(Error::Deadline);
        }
        Ok(Self {
            last: now,
            until: now.checked_add(timeout_ms).ok_or(Error::Deadline)?,
            failed: false,
        })
    }
    pub fn check(&mut self, now: u64) -> Result<(), Error> {
        if self.failed || now < self.last || now >= self.until {
            self.failed = true;
            return Err(Error::Deadline);
        }
        self.last = now;
        Ok(())
    }
}
/// Guard must check current deadline and original grant/account/journal/lease.
/// Called before header admission, on each consumed chunk, and before parsing.
/// The future HTTP adapter must separately deny redirects, env auth and cookies;
/// this function cannot establish those transport properties or authorize a call.
pub fn read_response<I, G>(
    headers: Headers<'_>,
    kind: ResponseKind<'_>,
    chunks: I,
    mut guard: G,
) -> Result<ParsedResponse, Error>
where
    I: IntoIterator<Item = Result<Vec<u8>, native::Error>>,
    G: FnMut() -> Result<(), Error>,
{
    guard()?;
    native::validate_http_headers(headers.status, headers.content_type, headers.encoding)?;
    let limit = match kind {
        ResponseKind::Catalog => 262144,
        ResponseKind::Trading { .. } => 1000000,
    };
    let mut raw = Vec::new();
    for chunk in chunks {
        let chunk = chunk?;
        guard()?;
        if chunk.len() > limit - raw.len() {
            return Err(native::Error::OutputInvalid.into());
        }
        raw.extend_from_slice(&chunk);
    }
    guard()?;
    native::parse_http(
        headers.status,
        headers.content_type,
        headers.encoding,
        &raw,
        kind,
    )
    .map_err(Error::from)
}
