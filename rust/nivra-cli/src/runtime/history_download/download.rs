use super::*;
use archive::{CandleUnit, Client, CoverageWindow, CoveredDays, FailedAttempt};

pub(super) struct DownloadWindow<'a> {
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
    pub interval: Interval,
    pub sessions: Option<&'a [NaiveDate]>,
}

/// Coalesce daily preparation gaps over reviewed sessions. Explicit downloads
/// keep disjoint gaps and split them at the provider's request limit.
pub(super) fn gaps(
    start: NaiveDate,
    end: NaiveDate,
    unit: CandleUnit,
    sessions: Option<&[NaiveDate]>,
    covered: &CoveredDays,
) -> Result<Vec<(NaiveDate, NaiveDate)>> {
    let mut gaps = Vec::new();
    if let Some(sessions) = sessions {
        // Coalesce across answered sessions, but never include an excluded day
        // in the provider request.
        let mut pending: Option<(NaiveDate, NaiveDate)> = None;
        let mut exclusions = covered.excluded.iter().copied().peekable();
        for &day in sessions {
            while exclusions.next_if(|&excluded| excluded <= day).is_some() {
                gaps.extend(pending.take());
            }
            if !covered.answered.contains(&day) && !covered.excluded.contains(&day) {
                let first = pending.map_or(day, |(first, _)| first);
                pending = Some((first, day));
            }
        }
        gaps.extend(pending);
        return Ok(gaps);
    }

    let mut from = start;
    for to in covered
        .answered
        .union(&covered.excluded)
        .copied()
        .filter(|day| (start..=end).contains(day))
        .chain(end.succ_opt())
    {
        while from < to {
            let last = window_end(from, to.pred_opt().ok_or("history_date")?, unit_name(unit))?;
            gaps.push((from, last));
            from = last.succ_opt().ok_or("history_date")?;
        }
        from = to.succ_opt().ok_or("history_date")?;
    }
    Ok(gaps)
}

pub(super) fn download(
    mut db: Client,
    transport: &mut impl Transport,
    instruments: &[Instrument],
    window: DownloadWindow<'_>,
    control: Option<&PreparationControl>,
) -> Result<usize> {
    let today = local(millis()).date_naive();
    let end = window
        .end
        .unwrap_or(today.pred_opt().ok_or("history_date")?);
    let start = match window.start {
        Some(start) => start,
        None => end
            .succ_opt()
            .and_then(|day| day.checked_sub_months(Months::new(12)))
            .ok_or("history_date")?,
    };
    let first_supported = NaiveDate::from_ymd_opt(
        if window.interval == Interval::Days {
            2000
        } else {
            2022
        },
        1,
        1,
    )
    .ok_or("history_date")?;
    if start > end || end >= today || start < first_supported || instruments.is_empty() {
        return Err("history_range".into());
    }

    if let Some(control) = control {
        control.check()?;
    }
    archive::lock_history_download(&mut db)?;
    let keys: Vec<_> = instruments
        .iter()
        .map(|instrument| instrument.key.as_str())
        .collect();
    let mut plans = Vec::new();
    for &unit in window.interval.units() {
        let covered = archive::covered_days(
            &mut db,
            &CoverageWindow {
                keys: &keys,
                unit,
                start,
                end,
                sessions: window.sessions,
                skip_unavailable: transport.skip_unavailable(),
            },
        )?;
        plans.push((unit, covered));
    }

    let mut failures = 0;
    let empty = CoveredDays::default();
    'instrument: for key in keys {
        for (unit, covered) in &plans {
            for (from, to) in gaps(
                start,
                end,
                *unit,
                window.sessions,
                covered.get(key).unwrap_or(&empty),
            )? {
                if let Some(control) = control {
                    control.check()?;
                }
                let receipt = match transport.fetch(key, unit_name(*unit), from, to) {
                    Ok((requested_ms, received_ms, body)) => Receipt {
                        key: key.into(),
                        unit: unit_name(*unit).into(),
                        start: from,
                        end: to,
                        requested_ms,
                        received_ms,
                        body_sha256: digest(body.as_bytes()),
                        body,
                    },
                    Err(error) => {
                        let code = failure_code(error.as_ref());
                        let range = (key, *unit, from, to);
                        if code == "http_400" {
                            archive::write_failed_range(
                                &mut db,
                                range,
                                millis(),
                                FailedAttempt::Unavailable,
                            )?;
                            if transport.skip_unavailable() {
                                continue;
                            }
                        } else if transient_fetch_failure(&code) {
                            archive::write_failed_range(
                                &mut db,
                                range,
                                millis(),
                                FailedAttempt::Retryable,
                            )?;
                            if failures < transport.max_fetch_failures() {
                                failures += 1;
                                continue 'instrument;
                            }
                        }
                        return Err(error);
                    }
                };
                let raw = serde_json::to_string(&receipt)?;
                archive::write_candles(
                    &mut db,
                    &super::super::market_archive::normalize::candles(&raw)?,
                )?;
            }
        }
    }
    Ok(failures)
}

pub(crate) fn prepare_daily(
    config: &Config,
    instruments: &[Instrument],
    sessions: &[NaiveDate],
    control: &PreparationControl,
) -> Result<usize> {
    let db = super::super::market_archive::open(config)?;
    let mut provider = Provider::new(config, Some(control.clone()))?;
    download(
        db,
        &mut provider,
        instruments,
        DownloadWindow {
            start: sessions.first().copied(),
            end: sessions.last().copied(),
            interval: Interval::Days,
            sessions: Some(sessions),
        },
        Some(control),
    )
}
