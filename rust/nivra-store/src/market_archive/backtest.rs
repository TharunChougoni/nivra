//! Read timestamp-ordered cash bars from one read-only archive snapshot.
use super::{Client, Result};
use chrono::NaiveDate;
use postgres::IsolationLevel;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarUnit {
    Day,
    Minute,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarWindow {
    pub unit: BarUnit,
    pub from: NaiveDate,
    pub to: NaiveDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruments: Option<Vec<String>>,
}

impl BarWindow {
    pub fn validate(&self) -> bool {
        self.from <= self.to
            && ist_midnight(self.from) >= 0
            && self.to.succ_opt().is_some()
            && self.instruments.as_ref().is_none_or(|keys| {
                !keys.is_empty()
                    && keys.iter().all(|key| {
                        key.starts_with("NSE_EQ|")
                            && key.len() > "NSE_EQ|".len()
                            && key.len() <= 128
                            && !key.chars().any(char::is_control)
                    })
            })
    }
}

pub fn ist_midnight(day: NaiveDate) -> i64 {
    day.and_time(chrono::NaiveTime::MIN)
        .and_utc()
        .timestamp_millis()
        - 19_800_000
}

#[derive(Clone, Copy)]
pub struct Bar {
    pub instrument: usize,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
}

/// Identity and observed dates of one ordered archive read. Identical content
/// yields the same address across repeatable-read transactions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarSnapshot {
    pub ordered_sha256: String,
    pub dates: Vec<NaiveDate>,
    pub bars: usize,
}

#[derive(Default)]
pub struct BarSnapshotBuilder {
    hash: Sha256,
    dates: Vec<NaiveDate>,
    bars: usize,
}

impl BarSnapshotBuilder {
    pub fn push(&mut self, at: i64, keys: &[String], bars: &[Bar]) {
        let day = nivra_core::autoresearch::ist_day(at);
        if self.dates.last() != Some(&day) {
            self.dates.push(day);
        }
        self.bars += bars.len();
        self.hash.update(at.to_be_bytes());
        self.hash.update((bars.len() as u64).to_be_bytes());
        for bar in bars {
            let key = &keys[bar.instrument];
            self.hash.update((key.len() as u64).to_be_bytes());
            self.hash.update(key.as_bytes());
            for value in [bar.open, bar.high, bar.low, bar.close, bar.volume] {
                self.hash.update(value.to_be_bytes());
            }
        }
    }

    pub fn finish(self) -> BarSnapshot {
        BarSnapshot {
            ordered_sha256: self
                .hash
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            dates: self.dates,
            bars: self.bars,
        }
    }
}

pub fn bar_snapshot(db: &mut Client, window: &BarWindow, budget: Duration) -> Result<BarSnapshot> {
    Ok(bar_snapshot_with_prefix(db, window, None, budget)?.0)
}

/// Read the expanded window and its discovery prefix from the same transaction.
pub fn bar_snapshot_with_prefix(
    db: &mut Client,
    window: &BarWindow,
    prefix_to: Option<NaiveDate>,
    budget: Duration,
) -> Result<(BarSnapshot, Option<BarSnapshot>)> {
    let deadline = std::time::Instant::now() + budget;
    let mut snapshot = BarSnapshotBuilder::default();
    let mut prefix = prefix_to.map(|_| BarSnapshotBuilder::default());
    read_bar_batches(db, window, budget, |at, keys, bars| {
        if std::time::Instant::now() >= deadline {
            return Err("archive_bars_wall_budget".into());
        }
        snapshot.push(at, keys, bars);
        if let Some((to, prefix)) = prefix_to.zip(prefix.as_mut())
            && nivra_core::autoresearch::ist_day(at) <= to
        {
            prefix.push(at, keys, bars);
        }
        Ok(())
    })?;
    Ok((snapshot.finish(), prefix.map(BarSnapshotBuilder::finish)))
}

pub fn read_bar_batches(
    db: &mut Client,
    window: &BarWindow,
    statement_budget: Duration,
    mut on_batch: impl FnMut(i64, &[String], &[Bar]) -> Result<()>,
) -> Result<()> {
    if !window.validate() {
        return Err("archive_bars_window_invalid".into());
    }
    let mut tx = db
        .build_transaction()
        .isolation_level(IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()?;
    // Keep each fetch inside the research job's overall systemd wall limit.
    let timeout = format!("{}ms", statement_budget.as_millis().clamp(1, 600_000));
    tx.query_one(
        "SELECT set_config('statement_timeout', $1, true)",
        &[&timeout],
    )?;
    let instruments = tx.query(
        "SELECT id, key FROM instruments
         WHERE starts_with(key, 'NSE_EQ|') AND ($1::text[] IS NULL OR key = ANY($1))
         ORDER BY key COLLATE \"C\"",
        &[&window.instruments],
    )?;
    let mut keys = Vec::with_capacity(instruments.len());
    let mut index = BTreeMap::new();
    for row in instruments {
        index.insert(row.get::<_, i32>(0), keys.len());
        keys.push(row.get::<_, String>(1));
    }
    let ids = index.keys().copied().collect::<Vec<_>>();
    match window.unit {
        BarUnit::Day => tx.execute(
            "DECLARE research_bars NO SCROLL CURSOR FOR
             SELECT day, instrument_id, o, h, l, c, volume FROM daily_bars
             WHERE day BETWEEN $1 AND $2 AND instrument_id = ANY($3)
             ORDER BY day, instrument_id",
            &[&window.from, &window.to, &ids],
        )?,
        BarUnit::Minute => tx.execute(
            "DECLARE research_bars NO SCROLL CURSOR FOR
             SELECT bar_ms, instrument_id, o, h, l, c, volume FROM minute_bars
             WHERE bar_ms >= $1 AND bar_ms < $2 AND instrument_id = ANY($3)
             ORDER BY bar_ms, instrument_id",
            &[
                &ist_midnight(window.from),
                &ist_midnight(window.to.succ_opt().expect("validated window")),
                &ids,
            ],
        )?,
    };
    let mut bars = Vec::new();
    let mut timestamp = None;
    let mut dispatch = |at, bars: &mut Vec<Bar>| {
        bars.sort_unstable_by_key(|b| b.instrument);
        on_batch(at, &keys, bars)?;
        bars.clear();
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    loop {
        let rows = tx.query("FETCH FORWARD 16384 FROM research_bars", &[])?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let at = match window.unit {
                BarUnit::Day => ist_midnight(row.get(0)),
                BarUnit::Minute => row.get(0),
            };
            if let Some(prior) = timestamp.filter(|prior| *prior != at) {
                dispatch(prior, &mut bars)?;
            }
            timestamp = Some(at);
            bars.push(Bar {
                instrument: index[&row.get::<_, i32>(1)],
                open: row.get(2),
                high: row.get(3),
                low: row.get(4),
                close: row.get(5),
                volume: row.get(6),
            });
        }
    }
    if let Some(at) = timestamp {
        dispatch(at, &mut bars)?;
    }
    tx.commit()?;
    Ok(())
}
