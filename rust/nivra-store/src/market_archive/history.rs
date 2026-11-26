//! Bounded reads for history planning, preparation and research.
use super::{CandleUnit, Client, Result};
use chrono::NaiveDate;
use std::collections::{BTreeMap, BTreeSet};

/// Serialize fetch passes sharing one archive. The connection owns the lock.
pub fn lock_history_download(db: &mut Client) -> Result<()> {
    if !db
        .query_one("SELECT pg_try_advisory_lock(6749301730906321427)", &[])?
        .get::<_, bool>(0)
    {
        return Err("history_download_locked".into());
    }
    Ok(())
}

pub struct CoverageWindow<'a> {
    pub keys: &'a [&'a str],
    pub unit: CandleUnit,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub sessions: Option<&'a [NaiveDate]>,
    pub skip_unavailable: bool,
}

#[derive(Default)]
pub struct CoveredDays {
    pub answered: BTreeSet<NaiveDate>,
    pub excluded: BTreeSet<NaiveDate>,
}

/// Days a download can skip. Daily answers may leave an incomplete tail.
pub fn covered_days(
    db: &mut Client,
    window: &CoverageWindow<'_>,
) -> Result<BTreeMap<String, CoveredDays>> {
    let rows = db.query(
        "WITH evidence AS (
           SELECT i.key, c.day, c.status,
                  (c.status <> 'observed' AND (c.status = 'unavailable' OR (c.unavailable_ms IS NOT NULL
                       AND (c.receipt_id IS NULL OR c.unavailable_ms > c.attempted_ms)))) AS excluded,
                  max(c.day) FILTER (WHERE c.status = 'observed'
                                     AND ($5::date[] IS NULL OR c.day = ANY($5)))
                      OVER (PARTITION BY c.instrument_id) AS last_bar
           FROM instruments i JOIN coverage c ON c.instrument_id = i.id
           WHERE i.key = ANY($1) AND c.unit = $2 AND c.day BETWEEN $3 AND $4
         )
         SELECT key,
                coalesce(array_agg(day) FILTER (
                    WHERE ($5::date[] IS NULL OR day = ANY($5))
                      AND (status = 'observed'
                       OR (status = 'empty' AND ($2 = 3 OR last_bar IS NULL OR day < last_bar)))
                ), '{}'),
                coalesce(array_agg(day) FILTER (WHERE $6 AND excluded), '{}')
         FROM evidence GROUP BY key",
        &[
            &window.keys,
            &(window.unit as i16),
            &window.start,
            &window.end,
            &window.sessions,
            &window.skip_unavailable,
        ],
    )?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let answered: Vec<NaiveDate> = row.get(1);
            let excluded: Vec<NaiveDate> = row.get(2);
            (
                row.get(0),
                CoveredDays {
                    answered: answered.into_iter().collect(),
                    excluded: excluded.into_iter().collect(),
                },
            )
        })
        .collect())
}

pub struct DailyCoverage {
    pub key: String,
    pub observed: u32,
    pub average_volume: Option<f64>,
    pub covered_empty: usize,
    pub missing: Vec<NaiveDate>,
}

/// Count genuine bars and classify missing sessions over the exact requested window.
pub fn daily_coverage(
    db: &mut Client,
    keys: &[&str],
    sessions: &[NaiveDate],
) -> Result<Vec<DailyCoverage>> {
    let rows = db.query(
        "WITH evidence AS (
           SELECT requested.key, dates.day, b.volume, c.status,
                  max(b.day) OVER (PARTITION BY requested.key) AS last_bar
           FROM unnest($1::text[]) AS requested(key)
           CROSS JOIN unnest($2::date[]) AS dates(day)
           LEFT JOIN instruments i ON i.key = requested.key
           LEFT JOIN daily_bars b ON b.instrument_id = i.id AND b.day = dates.day AND b.day = ANY($2)
           LEFT JOIN coverage c ON c.instrument_id = i.id AND c.unit = 2 AND c.day = dates.day AND c.day = ANY($2)
         )
         SELECT key, count(volume)::int, avg(volume::double precision),
                count(*) FILTER (WHERE volume IS NULL AND status = 'empty'
                                 AND (last_bar IS NULL OR day < last_bar))::int,
                coalesce(array_agg(day ORDER BY day) FILTER (WHERE volume IS NULL), '{}')
         FROM evidence GROUP BY key",
        &[&keys, &sessions],
    )?;
    Ok(rows
        .into_iter()
        .map(|row| DailyCoverage {
            key: row.get(0),
            observed: row.get::<_, i32>(1) as u32,
            average_volume: row.get(2),
            covered_empty: row.get::<_, i32>(3) as usize,
            missing: row.get(4),
        })
        .collect())
}

pub struct DailyBar {
    pub key: String,
    pub day: NaiveDate,
    pub open_paise: i64,
    pub high_paise: i64,
    pub low_paise: i64,
    pub close_paise: i64,
    pub volume: i64,
}

impl DailyBar {
    fn from_row(row: postgres::Row) -> Self {
        Self {
            key: row.get(0),
            day: row.get(1),
            open_paise: row.get(2),
            high_paise: row.get(3),
            low_paise: row.get(4),
            close_paise: row.get(5),
            volume: row.get(6),
        }
    }
}

pub fn latest_daily_bars(
    db: &mut Client,
    keys: &[&str],
    before: NaiveDate,
    count: i64,
) -> Result<Vec<DailyBar>> {
    Ok(db
        .query(
            "SELECT i.key, b.day, b.o, b.h, b.l, b.c, b.volume
             FROM instruments i CROSS JOIN LATERAL (
                 SELECT day, o, h, l, c, volume FROM daily_bars
                 WHERE instrument_id = i.id AND day < $2 ORDER BY day DESC LIMIT $3
             ) b
             WHERE i.key = ANY($1) ORDER BY i.key, b.day",
            &[&keys, &before, &count],
        )?
        .into_iter()
        .map(DailyBar::from_row)
        .collect())
}

pub fn daily_bars(
    db: &mut Client,
    keys: &[&str],
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> Result<Vec<DailyBar>> {
    Ok(db
        .query(
            "SELECT i.key, b.day, b.o, b.h, b.l, b.c, b.volume
             FROM instruments i JOIN daily_bars b ON b.instrument_id = i.id
             WHERE i.key = ANY($1)
               AND b.day BETWEEN coalesce($2::date, '-infinity') AND coalesce($3::date, 'infinity')
             ORDER BY i.key, b.day",
            &[&keys, &start, &end],
        )?
        .into_iter()
        .map(DailyBar::from_row)
        .collect())
}

pub fn daily_instrument_keys(
    db: &mut Client,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<String>> {
    Ok(db
        .query(
            "SELECT i.key FROM instruments i WHERE EXISTS (
                 SELECT 1 FROM daily_bars b WHERE b.instrument_id = i.id AND b.day BETWEEN $1 AND $2
             ) ORDER BY i.key",
            &[&start, &end],
        )?
        .into_iter()
        .map(|row| row.get(0))
        .collect())
}
