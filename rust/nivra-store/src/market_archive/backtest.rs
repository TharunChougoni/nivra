//! Read timestamp-ordered cash bars from one read-only archive snapshot.
use super::{Client, Result};
use chrono::NaiveDate;
use postgres::IsolationLevel;
use serde::{Deserialize, Serialize};
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
