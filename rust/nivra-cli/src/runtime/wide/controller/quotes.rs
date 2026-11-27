use super::*;
use crate::runtime::controller_events::{Event, Notify};
use crate::runtime::wide::reads::{
    FetchedQuoteRows, QuoteReadError, QuoteReadTarget, apply_executable_quotes,
    apply_scanner_metadata, fetch_quote_rows, record_read_failure,
};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};

#[derive(Clone, Copy)]
pub(super) enum ReadKind {
    Held,
    Plan,
    Scanner { metadata_only: bool, end: usize },
    Options,
    Depth,
}

impl ReadKind {
    fn slot(self) -> usize {
        match self {
            Self::Held => 0,
            Self::Plan => 1,
            Self::Scanner { .. } => 2,
            Self::Options => 3,
            Self::Depth => 4,
        }
    }
}

struct Request {
    kind: ReadKind,
    instruments: Vec<Instrument>,
}

struct Completed {
    request: Request,
    rows: std::result::Result<FetchedQuoteRows, QuoteReadError>,
}

struct Reader {
    requests: SyncSender<Request>,
    completed: Receiver<Completed>,
}

impl Reader {
    fn start(c: &Config, notify: Notify, name: &str, max_pending: usize) -> Result<Self> {
        let (requests, incoming) = sync_channel::<Request>(max_pending);
        let (outgoing, completed) = sync_channel(max_pending);
        let client = data::Client::new(&c.market_data_transport)?;
        let c = c.clone();
        thread::Builder::new().name(name.into()).spawn(move || {
            while let Ok(request) = incoming.recv() {
                let rows = fetch_quote_rows(&client, &c, &request.instruments);
                if outgoing.send(Completed { request, rows }).is_err() {
                    break;
                }
                notify.signal(Event::Quotes);
            }
        })?;
        Ok(Self {
            requests,
            completed,
        })
    }

    fn take(&self) -> Result<Option<Completed>> {
        match self.completed.try_recv() {
            Ok(completed) => Ok(Some(completed)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err("quote_reader_exited".into()),
        }
    }
}

pub(super) struct QuoteReads {
    protection: Reader,
    market: Reader,
    pending: [bool; 5],
}

impl QuoteReads {
    pub(super) fn start(c: &Config, notify: Notify) -> Result<Self> {
        Ok(Self {
            protection: Reader::start(c, notify.clone(), "held-quotes", 1)?,
            market: Reader::start(c, notify, "market-quotes", 4)?,
            pending: [false; 5],
        })
    }

    pub(super) fn pending(&self, kind: ReadKind) -> bool {
        self.pending[kind.slot()]
    }

    pub(super) fn request(&mut self, kind: ReadKind, instruments: Vec<Instrument>) -> Result<()> {
        let pending = &mut self.pending[kind.slot()];
        if *pending {
            return Err("quote_read_already_pending".into());
        }
        let reader = if matches!(kind, ReadKind::Held) {
            &self.protection
        } else {
            &self.market
        };
        reader
            .requests
            .try_send(Request { kind, instruments })
            .map_err(|_| "quote_reader_unavailable")?;
        *pending = true;
        Ok(())
    }
}

impl Controller {
    pub(super) fn commit_protection(&mut self) -> Result<()> {
        if let Some(completed) = self.quote_reads.protection.take()? {
            self.commit_read(completed)?;
        }
        Ok(())
    }

    pub(super) fn commit_market_reads(&mut self) -> Result<()> {
        while let Some(completed) = self.quote_reads.market.take()? {
            self.commit_read(completed)?;
        }
        Ok(())
    }

    fn commit_read(&mut self, completed: Completed) -> Result<()> {
        let Request { kind, instruments } = completed.request;
        self.quote_reads.pending[kind.slot()] = false;
        let cfg = self.event_config.as_ref();
        self.read_failure = match completed.rows {
            Err(error) => Some(record_read_failure(
                error,
                &mut self.db,
                &self.config,
                &mut self.p,
                cfg.is_some(),
            )?),
            Ok(rows) => match kind {
                ReadKind::Scanner {
                    metadata_only: true,
                    ..
                } => apply_scanner_metadata(
                    rows,
                    &self.config,
                    &mut self.db,
                    &mut self.p,
                    &instruments,
                    &mut self.scan,
                    cfg,
                    self.archive.as_ref(),
                )?,
                _ => {
                    let target = match kind {
                        ReadKind::Held => QuoteReadTarget::Held {
                            depth_sink: self.strategy.is_some().then_some(&mut self.scan),
                        },
                        ReadKind::Plan => QuoteReadTarget::Plan,
                        ReadKind::Options | ReadKind::Depth => QuoteReadTarget::Depth {
                            scan: &mut self.scan,
                        },
                        ReadKind::Scanner { .. } => QuoteReadTarget::Scan(&mut self.scan),
                    };
                    let receipt = rows.receipt;
                    let outcome = apply_executable_quotes(
                        rows,
                        &self.config,
                        &mut self.db,
                        &mut self.p,
                        &instruments,
                        &mut self.last_fresh_receipt,
                        cfg,
                        &self.feature_store,
                        target,
                        self.archive.as_ref(),
                    )?;
                    let full = self.full_strategy_depth();
                    if outcome.is_none()
                        && !matches!(kind, ReadKind::Plan)
                        && let Some(run) = self.strategy.as_mut()
                        && self.strategy_inputs.rest_read(
                            &self.scan,
                            &instruments,
                            receipt,
                            millis(),
                        )
                        && (!full || matches!(kind, ReadKind::Held))
                    {
                        run.request_observation();
                    }
                    outcome
                }
            },
        };
        if let ReadKind::Scanner { end, .. } = kind {
            if self.read_failure.is_none()
                && let Some(universe) = &self.universe
            {
                self.scan
                    .ranking
                    .sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
                self.scan.cursor = if end >= universe.instruments.len() {
                    0
                } else {
                    end
                };
                checkpoint_scanner(&self.db, &self.scan)?;
            }
            self.last_scan = millis();
        }
        Ok(())
    }
}
