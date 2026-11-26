use super::{ArchiveClient, MarketArchiveSettings, Result, connect, normalize};
use crate::runtime::millis;
use chrono::NaiveDate;
use nivra_store::market_archive::write_quotes;
use rusqlite::{Connection, OptionalExtension};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SPOOL_NAME: &str = "archive-spool.sqlite";
const PAGE_BYTES: u32 = 4096;
const SPOOL_PAGES: u32 = 64 * 1024 * 1024 / PAGE_BYTES;
const QUEUE_CAPACITY: usize = 16;
const RETRY_MS: i64 = 5_000;

pub(in crate::runtime) struct Writer {
    sender: Option<SyncSender<String>>,
    worker: Option<JoinHandle<()>>,
    dropped: Arc<AtomicU64>,
    prior_gaps: Receiver<NaiveDate>,
}

impl Writer {
    pub(in crate::runtime) fn start(
        directory: &Path,
        root: &Path,
        settings: MarketArchiveSettings,
    ) -> Result<Self> {
        let spool = directory.join(SPOOL_NAME);
        let root = root.to_owned();
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (report_prior, prior_gaps) = mpsc::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let worker_dropped = dropped.clone();
        let worker = thread::Builder::new()
            .name("market-archive".into())
            .spawn(move || {
                let mut sink = ArchiveSink { settings, db: None };
                let start = Instant::now();
                let clock = || start.elapsed().as_millis() as i64;
                let mut capture =
                    Capture::new(spool, prior_spools(&root), SPOOL_PAGES, report_prior);
                loop {
                    let now = clock();
                    // Limit each catch-up turn so the producer can keep sending
                    // while a large spool is being replayed.
                    capture.retry(now, &mut sink, &worker_dropped);
                    match receiver.try_recv() {
                        Ok(raw) => capture.submit(raw, now, &mut sink, &worker_dropped),
                        Err(mpsc::TryRecvError::Disconnected) => break,
                        Err(mpsc::TryRecvError::Empty) if capture.can_retry(now) => continue,
                        Err(mpsc::TryRecvError::Empty) => {
                            match receiver.recv_timeout(Duration::from_millis(250)) {
                                Ok(raw) => capture.submit(raw, clock(), &mut sink, &worker_dropped),
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
            dropped,
            prior_gaps,
        })
    }

    pub(in crate::runtime) fn offer(&self, raw: String) {
        if self
            .sender
            .as_ref()
            .is_none_or(|sender| sender.try_send(raw).is_err())
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(in crate::runtime) fn take_dropped(&self) -> u64 {
        self.dropped.swap(0, Ordering::Relaxed)
    }

    pub(in crate::runtime) fn restore_dropped(&self, dropped: u64) {
        self.dropped.fetch_add(dropped, Ordering::Relaxed);
    }

    pub(in crate::runtime) fn prior_gaps(&self) -> impl Iterator<Item = NaiveDate> + '_ {
        self.prior_gaps.try_iter()
    }

    pub(in crate::runtime) fn stop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn prior_spools(root: &Path) -> VecDeque<PathBuf> {
    let Ok(dirs) = fs::read_dir(root) else {
        return VecDeque::new();
    };
    let mut paths = dirs
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.parse::<chrono::NaiveDate>().is_ok())
        })
        .map(|entry| entry.path().join(SPOOL_NAME))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    paths.sort();
    paths.into()
}

struct Spool(Connection);

impl Spool {
    fn open(path: &Path, pages: u32) -> Result<Self> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.execute_batch(
            "PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS batches(id INTEGER PRIMARY KEY, raw TEXT NOT NULL)",
        )?;
        db.pragma_update(None, "max_page_count", pages)?;
        Ok(Self(db))
    }

    fn push(&self, raw: &str) -> Result<()> {
        self.0
            .execute("INSERT INTO batches(raw) VALUES(?1)", [raw])?;
        Ok(())
    }

    fn first(&self) -> Result<Option<(i64, String)>> {
        Ok(self
            .0
            .query_row(
                "SELECT id,raw FROM batches ORDER BY id LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    fn remove(&self, id: i64) -> Result<()> {
        self.0.execute("DELETE FROM batches WHERE id=?1", [id])?;
        Ok(())
    }
}

enum WriteResult {
    Stored,
    Invalid,
    Unavailable,
}

trait Sink {
    fn write(&mut self, raw: &str) -> WriteResult;
}

struct ArchiveSink {
    settings: MarketArchiveSettings,
    db: Option<ArchiveClient>,
}

impl Sink for ArchiveSink {
    fn write(&mut self, raw: &str) -> WriteResult {
        let Ok(batch) = normalize::capture_quotes(raw) else {
            return WriteResult::Invalid;
        };
        if self.db.is_none() {
            let Ok(db) = connect(&self.settings) else {
                return WriteResult::Unavailable;
            };
            self.db = Some(db);
        }
        let db = self.db.as_mut().expect("archive connection");
        match write_quotes(db, &batch) {
            Ok(_) => WriteResult::Stored,
            Err(error) if nivra_store::market_archive::invalid_quote_write(error.as_ref()) => {
                WriteResult::Invalid
            }
            Err(_) => {
                self.db = None;
                WriteResult::Unavailable
            }
        }
    }
}

struct Capture {
    path: PathBuf,
    spool: Option<Spool>,
    prior: Option<Spool>,
    backlog: VecDeque<PathBuf>,
    pages: u32,
    retry_at: i64,
    report_prior: Sender<NaiveDate>,
}

impl Capture {
    fn new(
        path: PathBuf,
        mut backlog: VecDeque<PathBuf>,
        pages: u32,
        report_prior: Sender<NaiveDate>,
    ) -> Self {
        backlog.retain(|prior| prior != &path);
        if path.exists() {
            backlog.push_back(path.clone());
        }
        Self {
            path,
            spool: None,
            prior: None,
            backlog,
            pages,
            retry_at: 0,
            report_prior,
        }
    }

    fn can_retry(&self, now: i64) -> bool {
        !self.backlog.is_empty() && now >= self.retry_at
    }

    fn local(&mut self) -> Result<&Spool> {
        if self.spool.is_none() {
            self.spool = Some(Spool::open(&self.path, self.pages)?);
        }
        Ok(self.spool.as_ref().expect("local archive spool"))
    }

    fn submit(&mut self, raw: String, now: i64, sink: &mut impl Sink, dropped: &AtomicU64) {
        if self.backlog.is_empty() && now >= self.retry_at {
            match sink.write(&raw) {
                WriteResult::Stored => return,
                WriteResult::Invalid => {
                    dropped.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                WriteResult::Unavailable => self.retry_at = now + RETRY_MS,
            }
        }
        if self.local().and_then(|spool| spool.push(&raw)).is_err() {
            dropped.fetch_add(1, Ordering::Relaxed);
        } else if self.backlog.back() != Some(&self.path) {
            self.backlog.push_back(self.path.clone());
        }
    }

    fn retry(&mut self, now: i64, sink: &mut impl Sink, dropped: &AtomicU64) {
        if !self.can_retry(now) {
            return;
        }
        let result = self.replay(sink, dropped);
        match result {
            Ok(true) => {}
            Ok(false) | Err(_) => self.retry_at = now + RETRY_MS,
        }
    }

    fn replay(&mut self, sink: &mut impl Sink, dropped: &AtomicU64) -> Result<bool> {
        let path = self.backlog.front().expect("pending archive spool").clone();
        let local = path == self.path;
        let spool = if local {
            self.local()?
        } else {
            if self.prior.is_none() {
                self.prior = Some(Spool::open(&path, self.pages)?);
            }
            self.prior.as_ref().expect("prior archive spool")
        };
        let Some((id, raw)) = spool.first()? else {
            self.backlog.pop_front();
            if local {
                self.spool = None;
            } else {
                self.prior = None;
            }
            fs::remove_file(path)?;
            return Ok(true);
        };
        let invalid = match sink.write(&raw) {
            WriteResult::Unavailable => return Ok(false),
            WriteResult::Invalid => true,
            WriteResult::Stored => false,
        };
        if invalid && !local {
            record_prior_gap(&path)?;
        }
        spool.remove(id)?;
        if invalid {
            if local {
                dropped.fetch_add(1, Ordering::Relaxed);
            } else if let Some(session) = path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse().ok())
            {
                let _ = self.report_prior.send(session);
            }
        }
        Ok(true)
    }
}

fn record_prior_gap(spool: &Path) -> Result<()> {
    let db = Connection::open_with_flags(
        spool.with_file_name("paper.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    record_gap(&db, 1)
}

pub(in crate::runtime) fn record_gap(db: &Connection, dropped: u64) -> Result<()> {
    db.execute(
        "INSERT INTO ticks(at,receipt,body,event) VALUES(?1,NULL,?2,'archive_gap')",
        rusqlite::params![
            millis(),
            serde_json::json!({"dropped_batches":dropped}).to_string()
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_recovers_fifo_and_refuses_work_past_its_disk_bound() {
        #[derive(Default)]
        struct FakeSink {
            online: bool,
            committed: Vec<String>,
        }
        impl Sink for FakeSink {
            fn write(&mut self, raw: &str) -> WriteResult {
                if !self.online {
                    return WriteResult::Unavailable;
                }
                self.committed.push(raw.to_owned());
                WriteResult::Stored
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SPOOL_NAME);
        let batches = ["a".repeat(3500), "b".repeat(3500), "c".repeat(9000)];
        let mut sink = FakeSink::default();
        let dropped = AtomicU64::new(0);
        let mut now = 100;
        let (report_prior, _) = mpsc::channel();
        let mut capture = Capture::new(path.clone(), VecDeque::new(), 4, report_prior);
        for batch in &batches {
            capture.submit(batch.clone(), now, &mut sink, &dropped);
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(fs::metadata(&path).unwrap().len() <= 4 * u64::from(PAGE_BYTES));
        sink.online = true;
        now += RETRY_MS - 1;
        capture.retry(now, &mut sink, &dropped);
        assert!(sink.committed.is_empty());

        // Restart with the same durable FIFO, then add another batch.
        drop(capture);
        let (report_prior, _) = mpsc::channel();
        let mut capture = Capture::new(path.clone(), VecDeque::new(), 4, report_prior);
        now += 1;
        capture.retry(now, &mut sink, &dropped);
        capture.submit("later".into(), now, &mut sink, &dropped);
        while capture.can_retry(now) {
            capture.retry(now, &mut sink, &dropped);
        }
        assert_eq!(sink.committed, [&batches[0], &batches[1], "later"]);
        assert!(!path.exists());
    }
}
