//! Bounded reads for history planning, preparation and research.
use super::{CandleUnit, Client, Result};
use chrono::NaiveDate;
use std::collections::{BTreeMap, BTreeSet};

/// Serialize fetch passes sharing one archive. The connection owns the lock.
pub fn lock_history_download(db: &mut Client) -> Result<()> {
    if !db
        .query_one(
            "SELECT pg_try_advisory_lock($1)",
            &[&super::HISTORY_DOWNLOAD_LOCK],
        )?
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

const EXCLUDED_DAY: &str = "c.status <> 'observed' AND (c.status = 'unavailable' OR
     (c.unavailable_ms IS NOT NULL AND (c.receipt_id IS NULL OR c.unavailable_ms > c.attempted_ms)))";
const ANSWERED_EMPTY_DAY: &str = "status = 'empty' AND (last_bar IS NULL OR day < last_bar)";

/// Days a download can skip. Daily answers may leave an incomplete tail.
pub fn covered_days(
    db: &mut Client,
    window: &CoverageWindow<'_>,
) -> Result<BTreeMap<String, CoveredDays>> {
    let rows = db.query(
        &format!(
            "WITH evidence AS (
           SELECT i.key, c.day, c.status,
                  ({EXCLUDED_DAY}) AS excluded,
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
                       OR ({ANSWERED_EMPTY_DAY}) OR ($2 = 3 AND status = 'empty'))
                ), ARRAY[]::date[]),
                coalesce(array_agg(day) FILTER (WHERE $6 AND excluded), ARRAY[]::date[])
         FROM evidence GROUP BY key"
        ),
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
    pub excluded: usize,
    pub missing: Vec<NaiveDate>,
}

/// Count genuine bars and classify missing sessions over the exact requested window.
pub fn daily_coverage(
    db: &mut Client,
    keys: &[&str],
    sessions: &[NaiveDate],
    skip_unavailable: bool,
) -> Result<Vec<DailyCoverage>> {
    let rows = db.query(
        &format!("WITH evidence AS (
           SELECT requested.key, dates.day, b.volume, c.status,
                  ({EXCLUDED_DAY}) AS excluded,
                  max(b.day) OVER (PARTITION BY requested.key) AS last_bar
           FROM unnest($1::text[]) AS requested(key)
           CROSS JOIN unnest($2::date[]) AS dates(day)
           LEFT JOIN instruments i ON i.key = requested.key
           LEFT JOIN daily_bars b ON b.instrument_id = i.id AND b.day = dates.day AND b.day = ANY($2)
           LEFT JOIN coverage c ON c.instrument_id = i.id AND c.unit = 2 AND c.day = dates.day AND c.day = ANY($2)
         )
         SELECT key, count(volume)::int, avg(volume::double precision),
                count(*) FILTER (WHERE volume IS NULL AND ({ANSWERED_EMPTY_DAY}))::int,
                count(*) FILTER (WHERE $3 AND volume IS NULL AND excluded
                                 AND NOT ({ANSWERED_EMPTY_DAY}))::int,
                coalesce(array_agg(day ORDER BY day) FILTER (WHERE volume IS NULL), ARRAY[]::date[])
         FROM evidence GROUP BY key"),
        &[&keys, &sessions, &skip_unavailable],
    )?;
    Ok(rows
        .into_iter()
        .map(|row| DailyCoverage {
            key: row.get(0),
            observed: row.get::<_, i32>(1) as u32,
            average_volume: row.get(2),
            covered_empty: row.get::<_, i32>(3) as usize,
            excluded: row.get::<_, i32>(4) as usize,
            missing: row.get(5),
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

/// The newest session with any archived daily bar, the archive's bar
/// watermark. `None` for an empty archive.
pub fn last_daily_bar_day(db: &mut Client) -> Result<Option<NaiveDate>> {
    Ok(db.query_one("SELECT max(day) FROM daily_bars", &[])?.get(0))
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

#[derive(Debug, PartialEq, Eq)]
pub struct ArchiveMinuteBar {
    pub bar_ms: i64,
    pub open_paise: i64,
    pub high_paise: i64,
    pub low_paise: i64,
    pub close_paise: i64,
    pub tick_count: Option<i64>,
    pub volume: Option<i64>,
    pub from_quote: bool,
}

/// Bounded minute bars for one instrument from `minute_bars` where present or
/// `quote_obs` aggregated per minute with Timescale `time_bucket`.
/// Returns chronological bars and whether older bars were truncated by `limit`.
pub fn archive_minute_bars(
    db: &mut Client,
    key: &str,
    from_ms: i64,
    to_ms: i64,
    limit: usize,
) -> Result<(Vec<ArchiveMinuteBar>, bool)> {
    let fetch_limit = i64::try_from(limit.max(1).saturating_add(1))?;
    let rows = db.query(
        "WITH inst AS MATERIALIZED (
             SELECT id FROM instruments WHERE key = $1
         ),
         quotes AS (
             SELECT
                 time_bucket(60000::bigint, q.receipt_ms) AS bar_ms,
                 first(q.ltp, q.receipt_ms) AS o,
                 max(q.ltp) AS h,
                 min(q.ltp) AS l,
                 last(q.ltp, q.receipt_ms) AS c,
                 count(*)::bigint AS tick_count
             FROM quote_obs q
             WHERE q.instrument_id = (SELECT id FROM inst)
               AND q.receipt_ms BETWEEN $2 AND $3
             GROUP BY 1
         ),
         bars AS (
             SELECT m.bar_ms, m.o, m.h, m.l, m.c, m.volume
             FROM minute_bars m
             WHERE m.instrument_id = (SELECT id FROM inst)
               AND m.bar_ms BETWEEN $2 AND $3
         )
         SELECT
             coalesce(b.bar_ms, q.bar_ms) AS bar_ms,
             coalesce(b.o, q.o) AS o,
             coalesce(b.h, q.h) AS h,
             coalesce(b.l, q.l) AS l,
             coalesce(b.c, q.c) AS c,
             q.tick_count,
             b.volume,
             (b.bar_ms IS NULL) AS from_quote
         FROM quotes q
         FULL OUTER JOIN bars b ON b.bar_ms = q.bar_ms
         ORDER BY bar_ms DESC
         LIMIT $4",
        &[&key, &from_ms, &to_ms, &fetch_limit],
    )?;
    let truncated = rows.len() > limit;
    let mut bars: Vec<ArchiveMinuteBar> = rows
        .into_iter()
        .take(limit)
        .map(|row| ArchiveMinuteBar {
            bar_ms: row.get(0),
            open_paise: row.get(1),
            high_paise: row.get(2),
            low_paise: row.get(3),
            close_paise: row.get(4),
            tick_count: row.get(5),
            volume: row.get(6),
            from_quote: row.get(7),
        })
        .collect();
    bars.reverse();
    Ok((bars, truncated))
}

/// Bounded daily bars for one instrument from `daily_bars`.
/// Returns chronological bars and whether older bars were truncated by `limit`.
pub fn archive_daily_bars(
    db: &mut Client,
    key: &str,
    start: NaiveDate,
    end: NaiveDate,
    limit: usize,
) -> Result<(Vec<DailyBar>, bool)> {
    let fetch_limit = i64::try_from(limit.max(1).saturating_add(1))?;
    let rows = db.query(
        "SELECT $1::text, b.day, b.o, b.h, b.l, b.c, b.volume
         FROM daily_bars b
         WHERE b.instrument_id = (SELECT id FROM instruments WHERE key = $1)
           AND b.day BETWEEN $2 AND $3
         ORDER BY b.day DESC
         LIMIT $4",
        &[&key, &start, &end, &fetch_limit],
    )?;
    let truncated = rows.len() > limit;
    let mut bars: Vec<DailyBar> = rows
        .into_iter()
        .take(limit)
        .map(DailyBar::from_row)
        .collect();
    bars.reverse();
    Ok((bars, truncated))
}
