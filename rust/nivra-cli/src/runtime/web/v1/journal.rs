//! Process-local cache of the append-only lane journals `/api/v1/agent`
//! reads. A request parses only the bytes appended since the last one and
//! holds at most the newest `max_bytes` of complete lines.
use super::*;
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};
use std::io::Seek;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

/// Identifies one version of a file: an in-place rewrite or a replacement
/// changes it even when the length does not.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
}

/// Reduces one parsed journal line to the row the route serves.
pub(super) type Project = fn(Value) -> Value;

#[derive(Default)]
pub(super) struct Journal {
    stamp: Option<Stamp>,
    /// File offset just past the last complete line.
    committed: u64,
    /// Complete lines in file order: the projected row (None when the line
    /// is not JSON) and the line's byte length.
    lines: VecDeque<(Option<Value>, u64)>,
    held_bytes: u64,
    /// A final line without its newline yet; it parses only once complete.
    tail: Option<Value>,
    /// Complete lines older than the held window.
    pub(super) not_loaded: u64,
}

impl Journal {
    /// Rows oldest first, including a complete but unterminated final line.
    pub(super) fn rows(&self) -> impl DoubleEndedIterator<Item = &Value> {
        self.lines
            .iter()
            .filter_map(|(row, _)| row.as_ref())
            .chain(self.tail.as_ref())
    }

    fn refresh(&mut self, path: &Path, max_bytes: u64, project: Project) -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(std::io::ErrorKind::InvalidInput.into());
        }
        let stamp = Stamp {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
        };
        let appended = self.stamp.is_some_and(|old| {
            old.dev == stamp.dev
                && old.ino == stamp.ino
                && stamp.len > old.len
                && stamp.len - self.committed <= max_bytes
        });
        if self.stamp == Some(stamp) {
            return Ok(());
        }
        let mut mid_line = false;
        if !appended {
            *self = Self::default();
            let start = stamp.len.saturating_sub(max_bytes);
            let complete;
            (complete, mid_line) = count_lines(&mut file, start)?;
            self.not_loaded = complete + u64::from(mid_line);
            self.committed = start;
        }
        file.seek(std::io::SeekFrom::Start(self.committed))?;
        let mut bytes = Vec::new();
        file.take(stamp.len - self.committed)
            .read_to_end(&mut bytes)?;
        let mut rest = bytes.as_slice();
        if mid_line {
            // The window starts mid-line; that line is counted as not loaded.
            let skip = rest
                .iter()
                .position(|b| *b == b'\n')
                .map_or(rest.len(), |i| i + 1);
            self.committed += skip as u64;
            rest = &rest[skip..];
        }
        while let Some(end) = rest.iter().position(|b| *b == b'\n') {
            let row = serde_json::from_slice(&rest[..end]).ok().map(project);
            self.lines.push_back((row, end as u64 + 1));
            self.held_bytes += end as u64 + 1;
            self.committed += end as u64 + 1;
            rest = &rest[end + 1..];
        }
        self.tail = serde_json::from_slice(rest).ok().map(project);
        while self.held_bytes > max_bytes {
            let Some((_, len)) = self.lines.pop_front() else {
                break;
            };
            self.held_bytes -= len;
            self.not_loaded += 1;
        }
        self.stamp = Some(stamp);
        Ok(())
    }
}

/// Lines that end before `end`, and whether `end` falls inside a line.
fn count_lines(file: &mut std::fs::File, end: u64) -> std::io::Result<(u64, bool)> {
    let mut count = 0;
    let mut last = b'\n';
    let mut buffer = vec![0u8; 64 * 1024];
    let mut reader = Read::take(&mut *file, end);
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        count += buffer[..n].iter().filter(|b| **b == b'\n').count() as u64;
        last = buffer[n - 1];
    }
    Ok((count, last != b'\n'))
}

static JOURNALS: Mutex<BTreeMap<PathBuf, Journal>> = Mutex::new(BTreeMap::new());

/// Refreshes the named journals and hands them to `read` under one lock.
/// A missing file reads as an empty journal.
pub(super) fn with_journals<const N: usize, R>(
    files: [(PathBuf, Project); N],
    max_bytes: u64,
    read: impl FnOnce([&Journal; N]) -> R,
) -> std::result::Result<R, &'static str> {
    let mut cache = JOURNALS.lock().map_err(|_| "agent_journal_unavailable")?;
    for (path, project) in &files {
        match cache
            .entry(path.clone())
            .or_default()
            .refresh(path, max_bytes, *project)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                cache.remove(path);
            }
            Err(_) => {
                cache.remove(path);
                return Err("agent_journal_unavailable");
            }
        }
    }
    let empty = Journal::default();
    Ok(read(
        files
            .each_ref()
            .map(|(path, _)| cache.get(path).unwrap_or(&empty)),
    ))
}
