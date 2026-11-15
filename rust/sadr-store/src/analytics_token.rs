//! Explicit analytics-read credential selection. Never discovers or evaluates dotenv files.
use std::{
    fmt, fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const MAX_FILE_BYTES: u64 = 32_768;
const MAX_TOKEN_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error;
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Analytics token file unavailable; require private owned 0600 file with one literal UPSTOX_ANALYTICS_TOKEN assignment")
    }
}
impl std::error::Error for Error {}

/// Opaque credential. It cannot be serialized and Debug/Display never disclose it.
/// The callback is the deliberate credential handoff to a trusted read transport.
pub struct AnalyticsToken(String);
impl AnalyticsToken {
    pub fn with_secret<R>(&self, use_token: impl FnOnce(&str) -> R) -> R {
        use_token(&self.0)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl fmt::Debug for AnalyticsToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AnalyticsToken([REDACTED])")
    }
}
impl fmt::Display for AnalyticsToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// Explicit file errors are terminal for this selection; the environment callback
/// is evaluated only when no file was selected. An absent environment value is empty,
/// matching historical ReadCredentials. Environment values are literal, unvalidated,
/// and never copied into the process environment or looked up in a dotenv file.
pub fn analytics_token(
    token_file: Option<&Path>,
    process_environment: impl FnOnce() -> Result<Option<String>, Error>,
) -> Result<AnalyticsToken, Error> {
    match token_file {
        Some(path) => load_analytics_token(path),
        None => Ok(AnalyticsToken(process_environment()?.unwrap_or_default())),
    }
}

/// Select from a host-supplied process-environment snapshot, preserving Python's
/// case-insensitive key matching and last matching key behavior. The host need
/// only supply analytics-token entries; no unrelated credentials are needed.
pub fn environment_token<'a>(
    entries: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<String> {
    entries
        .into_iter()
        .filter(|(key, _)| key.eq_ignore_ascii_case("UPSTOX_ANALYTICS_TOKEN"))
        .map(|(_, value)| value.to_owned())
        .last()
}

fn absolute(path: &Path) -> Result<PathBuf, Error> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(Error);
    }
    if path.is_absolute() {
        Ok(path.into())
    } else {
        Ok(std::env::current_dir().map_err(|_| Error)?.join(path))
    }
}
fn open(path: &Path) -> Result<fs::File, Error> {
    let directory_flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::DIRECTORY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::CLOEXEC;
    let mut parent = fs::File::from(
        rustix::fs::open("/", directory_flags, rustix::fs::Mode::empty()).map_err(|_| Error)?,
    );
    let names: Vec<_> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n),
            _ => None,
        })
        .collect();
    let (name, directories) = names.split_last().ok_or(Error)?;
    for directory in directories {
        parent = fs::File::from(
            rustix::fs::openat(
                &parent,
                *directory,
                directory_flags,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| Error)?,
        );
    }
    let flags = rustix::fs::OFlags::RDONLY
        | rustix::fs::OFlags::NOFOLLOW
        | rustix::fs::OFlags::NONBLOCK
        | rustix::fs::OFlags::CLOEXEC;
    Ok(fs::File::from(
        rustix::fs::openat(&parent, *name, flags, rustix::fs::Mode::empty()).map_err(|_| Error)?,
    ))
}
fn identity(m: &fs::Metadata) -> (u64, u64, u32, u32, u64, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.mode(),
        m.uid(),
        m.nlink(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
fn private(m: &fs::Metadata) -> bool {
    m.is_file()
        && fs::metadata("/proc/self").is_ok_and(|owner| m.uid() == owner.uid())
        && m.mode() & 0o7777 == 0o600
        && m.nlink() == 1
        && m.len() <= MAX_FILE_BYTES
}

/// Reads only the selected file through no-follow directory descriptors. Metadata,
/// byte count and a fresh path open must still agree after the bounded read. This
/// detects ordinary replacements/edits; it is not hostile same-user containment.
pub fn load_analytics_token(path: &Path) -> Result<AnalyticsToken, Error> {
    let path = absolute(path)?;
    let file = open(&path)?;
    let before = file.metadata().map_err(|_| Error)?;
    if !private(&before) {
        return Err(Error);
    }
    let mut raw = Vec::new();
    (&file)
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut raw)
        .map_err(|_| Error)?;
    stable(&file, &before, &path, raw.len() as u64)?;
    parse(&raw)
}
fn stable(
    file: &fs::File,
    before: &fs::Metadata,
    path: &Path,
    read_bytes: u64,
) -> Result<(), Error> {
    let after = file.metadata().map_err(|_| Error)?;
    let named = open(path)?.metadata().map_err(|_| Error)?;
    if read_bytes != before.len()
        || !private(&after)
        || !private(&named)
        || identity(before) != identity(&after)
        || identity(before) != identity(&named)
    {
        return Err(Error);
    }
    Ok(())
}

fn python_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
fn parse(raw: &[u8]) -> Result<AnalyticsToken, Error> {
    let text = std::str::from_utf8(raw).map_err(|_| Error)?;
    let mut token = None;
    // Python str.splitlines includes these separators beyond Rust str.lines.
    for line in text.split([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ]) {
        let mut line = line.trim_matches(python_space);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if token.is_some() {
            return Err(Error);
        }
        if let Some(rest) = line.strip_prefix("export") {
            if !rest.starts_with([' ', '\t']) {
                return Err(Error);
            }
            line = rest.trim_start_matches([' ', '\t']);
        }
        let rest = line
            .strip_prefix("UPSTOX_ANALYTICS_TOKEN")
            .ok_or(Error)?
            .trim_start_matches([' ', '\t']);
        let mut value = rest
            .strip_prefix('=')
            .ok_or(Error)?
            .trim_matches(python_space);
        if value.starts_with(['\'', '"']) {
            let quote = value.as_bytes()[0];
            if value.len() < 2 || value.as_bytes().last() != Some(&quote) {
                return Err(Error);
            }
            value = &value[1..value.len() - 1];
        }
        if value.is_empty()
            || value.len() > MAX_TOKEN_BYTES
            || value
                .bytes()
                .any(|b| !(b'!'..=b'~').contains(&b) || b"\"'$`\\".contains(&b))
        {
            return Err(Error);
        }
        token = Some(AnalyticsToken(value.into()));
    }
    token.ok_or(Error)
}
