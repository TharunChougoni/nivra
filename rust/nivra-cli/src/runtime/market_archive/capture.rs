use super::{ArchiveClient, MarketArchiveSettings, Result, connect, normalize};
use crate::runtime::millis;
use chrono::NaiveDate;
use nivra_store::market_archive::write_quotes;
use rusqlite::{Connection, OptionalExtension};
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SPOOL_NAME: &str = "archive-spool.sqlite";
const PAGE_BYTES: u32 = 4096;
const SPOOL_PAGES: u32 = 64 * 1024 * 1024 / PAGE_BYTES;
const RETRY_MS: i64 = 5_000;

pub(in crate::runtime) struct Writer {
    notify: Option<SyncSender<()>>,
    spool: Arc<Mutex<Spool>>,
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
        let path = directory.join(SPOOL_NAME);
        let mut backlog = prior_spools(root);
        backlog.retain(|prior| prior != &path);
        let spool = Arc::new(Mutex::new(Spool::open(&path, SPOOL_PAGES)?));
        fs::File::open(directory)?.sync_all()?;
        let worker_spool = spool.clone();
        let (notify, notified) = mpsc::sync_channel(1);
        let (report_prior, prior_gaps) = mpsc::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let worker_dropped = dropped.clone();
        let worker = thread::Builder::new()
            .name("market-archive".into())
            .spawn(move || {
                let mut sink = ArchiveSink { settings, db: None };
                let start = Instant::now();
                let mut retry_at = 0;
                let mut capture = Capture {
                    spool: worker_spool,
                    prior: None,
                    backlog,
                    report_prior,
                };
                loop {
                    let now = start.elapsed().as_millis() as i64;
                    if now >= retry_at {
                        match capture.replay(&mut sink, &worker_dropped) {
                            Ok(Replay::Progress) => continue,
                            Ok(Replay::Empty) => {}
                            Ok(Replay::Unavailable) | Err(_) => retry_at = now + RETRY_MS,
                        }
                    }
                    if let Err(mpsc::RecvTimeoutError::Disconnected) =
                        notified.recv_timeout(Duration::from_millis(250))
                    {
                        break;
                    }
                }
            })?;
        Ok(Self {
            notify: Some(notify),
            spool,
            worker: Some(worker),
            dropped,
            prior_gaps,
        })
    }

    /// Commit the original bytes before the ledger can publish a reference.
    /// The worker holds the spool lock only while it reads or deletes a row.
    pub(in crate::runtime) fn stage(&self, raw: &str) -> Result<()> {
        let Some(notify) = &self.notify else {
            return Err("archive_capture_stopped".into());
        };
        let spool = self.spool.lock().map_err(|_| "archive_spool_unavailable")?;
        if spool.push(raw).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return Err("archive_spool_unavailable".into());
        }
        if matches!(
            notify.try_send(()),
            Err(mpsc::TrySendError::Disconnected(()))
        ) {
            return Err("archive_capture_stopped".into());
        }
        Ok(())
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
        self.notify.take();
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

enum Replay {
    Progress,
    Empty,
    Unavailable,
}

struct Capture {
    spool: Arc<Mutex<Spool>>,
    prior: Option<Spool>,
    backlog: VecDeque<PathBuf>,
    report_prior: Sender<NaiveDate>,
}

impl Capture {
    fn replay(&mut self, sink: &mut impl Sink, dropped: &AtomicU64) -> Result<Replay> {
        if let Some(path) = self.backlog.front() {
            if self.prior.is_none() {
                self.prior = Some(Spool::open(path, SPOOL_PAGES)?);
            }
            let spool = self.prior.as_ref().expect("open prior archive spool");
            let Some((id, raw)) = spool.first()? else {
                self.prior = None;
                fs::remove_file(path)?;
                self.backlog.pop_front();
                return Ok(Replay::Progress);
            };
            match sink.write(&raw) {
                WriteResult::Unavailable => return Ok(Replay::Unavailable),
                WriteResult::Stored => {}
                WriteResult::Invalid => {
                    record_prior_gap(path)?;
                    if let Some(session) = path
                        .parent()
                        .and_then(Path::file_name)
                        .and_then(|name| name.to_str())
                        .and_then(|name| name.parse().ok())
                    {
                        let _ = self.report_prior.send(session);
                    }
                }
            }
            spool.remove(id)?;
            return Ok(Replay::Progress);
        }

        let first = self
            .spool
            .lock()
            .map_err(|_| "archive_spool_unavailable")?
            .first()?;
        let Some((id, raw)) = first else {
            return Ok(Replay::Empty);
        };
        match sink.write(&raw) {
            WriteResult::Unavailable => return Ok(Replay::Unavailable),
            WriteResult::Stored => {}
            WriteResult::Invalid => {
                dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.spool
            .lock()
            .map_err(|_| "archive_spool_unavailable")?
            .remove(id)?;
        Ok(Replay::Progress)
    }
}

fn record_prior_gap(spool: &Path) -> Result<()> {
    let book = spool.with_file_name("paper.sqlite");
    if fs::symlink_metadata(&book).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(());
    }
    let db = Connection::open_with_flags(
        book,
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
        let spool = Spool::open(&path, 4).unwrap();
        spool.push(&batches[0]).unwrap();
        spool.push(&batches[1]).unwrap();
        assert!(spool.push(&batches[2]).is_err());
        assert!(fs::metadata(&path).unwrap().len() <= 4 * u64::from(PAGE_BYTES));
        drop(spool);

        let (report_prior, _) = mpsc::channel();
        let spool = Arc::new(Mutex::new(Spool::open(&path, 4).unwrap()));
        let mut capture = Capture {
            spool: spool.clone(),
            prior: None,
            backlog: VecDeque::new(),
            report_prior,
        };
        assert!(matches!(
            capture.replay(&mut sink, &dropped).unwrap(),
            Replay::Unavailable
        ));
        sink.online = true;
        assert!(matches!(
            capture.replay(&mut sink, &dropped).unwrap(),
            Replay::Progress
        ));
        spool.lock().unwrap().push("later").unwrap();
        while matches!(
            capture.replay(&mut sink, &dropped).unwrap(),
            Replay::Progress
        ) {}
        assert_eq!(sink.committed, [&batches[0], &batches[1], "later"]);
        assert!(spool.lock().unwrap().first().unwrap().is_none());
    }

    #[test]
    fn missing_historical_book_does_not_block_capture_after_an_invalid_batch() -> Result<()> {
        struct FakeSink;
        impl Sink for FakeSink {
            fn write(&mut self, raw: &str) -> WriteResult {
                if raw == "invalid" {
                    WriteResult::Invalid
                } else {
                    WriteResult::Stored
                }
            }
        }

        let directory = tempfile::tempdir()?;
        let earlier = directory.path().join("2026-10-16");
        fs::create_dir(&earlier)?;
        let path = earlier.join(SPOOL_NAME);
        Spool::open(&path, SPOOL_PAGES)?.push("invalid")?;
        let current = Spool::open(&directory.path().join(SPOOL_NAME), SPOOL_PAGES)?;
        current.push("current")?;

        let (report_prior, prior_gaps) = mpsc::channel();
        let spool = Arc::new(Mutex::new(current));
        let mut capture = Capture {
            spool: spool.clone(),
            prior: None,
            backlog: VecDeque::from([path.clone()]),
            report_prior,
        };
        let mut sink = FakeSink;
        let dropped = AtomicU64::new(0);
        for _ in 0..3 {
            assert!(matches!(
                capture.replay(&mut sink, &dropped)?,
                Replay::Progress
            ));
        }
        assert_eq!(prior_gaps.try_recv()?, "2026-10-16".parse()?);
        assert!(!path.exists());
        assert!(spool.lock().unwrap().first()?.is_none());
        Ok(())
    }
}
