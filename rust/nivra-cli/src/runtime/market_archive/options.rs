use super::capture::ArchiveSink;
use crate::runtime::{data, failure_code, options::SpotQuote};
use chrono::NaiveDate;
use nivra_core::{execution::number, finance::Price};
use nivra_store::market_archive::{
    invalid_quote_write, is_connection_error,
    options::{OptionEvidence, OptionGap, OptionObservation, OptionSpot, write_option_evidence},
};
use serde_json::Value;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering::Relaxed},
    mpsc::{self, Receiver, SyncSender},
};

struct Event {
    session: NaiveDate,
    at_ms: i64,
    evidence: OptionEvidence,
}

/// The controller only moves bounded evidence into this queue. Encoding and
/// database writes share the existing archive worker and connection.
pub(in crate::runtime) struct OptionWriter {
    send: SyncSender<Event>,
    notify: Option<SyncSender<()>>,
    first_drop: Arc<AtomicI64>,
}

impl OptionWriter {
    pub(in crate::runtime) fn offer(
        &self,
        session: NaiveDate,
        at_ms: i64,
        evidence: OptionEvidence,
    ) {
        if self
            .send
            .try_send(Event {
                session,
                at_ms,
                evidence,
            })
            .is_err()
        {
            let _ = self.first_drop.compare_exchange(0, at_ms, Relaxed, Relaxed);
        }
        if let Some(notify) = &self.notify {
            let _ = notify.try_send(());
        }
    }

    pub(super) fn stop(&mut self) {
        self.notify.take();
    }
}

pub(super) struct OptionQueue {
    receive: Receiver<Event>,
    first_drop: Arc<AtomicI64>,
    pending: Option<Event>,
    retry_at: i64,
}

impl OptionQueue {
    pub(super) fn channel(notify: SyncSender<()>) -> (OptionWriter, Self) {
        let (send, receive) = mpsc::sync_channel(16);
        let first_drop = Arc::new(AtomicI64::new(0));
        (
            OptionWriter {
                send,
                notify: Some(notify),
                first_drop: first_drop.clone(),
            },
            Self {
                receive,
                first_drop,
                pending: None,
                retry_at: 0,
            },
        )
    }

    pub(super) fn flush(&mut self, sink: &mut ArchiveSink, now: i64) -> bool {
        if now < self.retry_at {
            return false;
        }
        let Some(event) = self.pending.take().or_else(|| self.receive.try_recv().ok()) else {
            return false;
        };
        let dropped = self.first_drop.swap(0, Relaxed);
        let mut write = || {
            let db = sink.connection()?;
            if dropped > 0 {
                write_option_evidence(
                    db,
                    event.session,
                    dropped,
                    &OptionEvidence::CaptureGap(OptionGap {
                        reason: "option_capture_gap".into(),
                    }),
                )?;
            }
            write_option_evidence(db, event.session, event.at_ms, &event.evidence)
        };
        match write() {
            Ok(()) => true,
            Err(error) => {
                let code = failure_code(error.as_ref());
                eprintln!("option_archive_write_failed reason={code}");
                if is_connection_error(error.as_ref()) {
                    sink.disconnect();
                }
                let permanent = invalid_quote_write(error.as_ref())
                    || code == "archive_option_evidence_conflict";
                if permanent {
                    let _ = self
                        .first_drop
                        .compare_exchange(0, event.at_ms, Relaxed, Relaxed);
                } else {
                    self.pending = Some(event);
                }
                if dropped > 0 {
                    let _ = self.first_drop.fetch_update(Relaxed, Relaxed, |current| {
                        Some(if current == 0 {
                            dropped
                        } else {
                            current.min(dropped)
                        })
                    });
                }
                self.retry_at = now + 5_000;
                false
            }
        }
    }
}

pub(in crate::runtime) fn gap(reason: &str) -> OptionEvidence {
    OptionEvidence::Gap(OptionGap {
        reason: reason.into(),
    })
}

pub(in crate::runtime) fn spot(value: &SpotQuote) -> OptionSpot {
    OptionSpot {
        key: value.key.clone(),
        price: value.price,
        source_ms: value.source_at,
        receipt_ms: value.received_at,
    }
}

pub(in crate::runtime) fn observation(
    key: &str,
    row: Option<&Value>,
    quote: Option<&nivra_core::execution::Quote>,
) -> OptionObservation {
    OptionObservation {
        instrument_key: key.into(),
        source_ms: row.and_then(|row| data::quote_source(row).ok()),
        bid: quote
            .map(|q| (q.bid, q.bid_units))
            .or_else(|| row.and_then(|row| data::quote_depth(row, "buy").ok().flatten())),
        ask: quote
            .map(|q| (q.ask, q.ask_units))
            .or_else(|| row.and_then(|row| data::quote_depth(row, "sell").ok().flatten())),
        ltp: row.and_then(|row| Price::try_from(number(&row["last_price"]).ok()?).ok()),
        oi: row.and_then(|row| {
            row["oi"].as_u64().or_else(|| {
                let oi = row["oi"].as_f64()?;
                (oi >= 0.0 && oi.fract() == 0.0 && oi < u64::MAX as f64).then_some(oi as u64)
            })
        }),
        quote: quote.cloned(),
        gap: if row.is_none() {
            Some("no_quote_returned".into())
        } else if quote.is_none() {
            Some("option_depth_unavailable".into())
        } else {
            None
        },
    }
}
