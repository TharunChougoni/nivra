use super::*;
use chrono::{DateTime, NaiveDate};
use nivra_core::time::{ExchangeDate, LocalTime, Timestamp};
use serde::Serialize;

pub(crate) const RECENT_ARCHIVE_DATASET_ID: &str = "recent_archive";

#[derive(Clone, Debug, Serialize)]
pub(crate) struct RecentArchiveWindow {
    dataset_id: &'static str,
    pub(crate) sessions: Vec<NaiveDate>,
    pub selection_from_ms: i64,
    pub selection_to_ms: i64,
}

impl RecentArchiveWindow {
    pub(crate) fn resolve(
        c: &Config,
        settings: &CandidateStrategySettings,
        as_of_ms: i64,
    ) -> std::result::Result<Self, &'static str> {
        const CALENDAR: &str = "candidate_archive_calendar_unavailable";
        let as_of = Timestamp::from_epoch_millis(as_of_ms).map_err(|_| CALENDAR)?;
        let today: NaiveDate = as_of.ist_date().to_string().parse().map_err(|_| CALENDAR)?;
        let calendar = c.calendar_at(today.min(c.session)).map_err(|_| CALENDAR)?;
        let epoch = |day, time: LocalTime| {
            DateTime::parse_from_rfc3339(&format!("{day}T{time}+05:30"))
                .map(|t| t.timestamp_millis())
                .map_err(|_| CALENDAR)
        };
        let mut sessions = Vec::new();
        let mut selection_to_ms = None;
        for day in std::iter::successors(Some(today.min(c.session)), |day| day.pred_opt()) {
            let date = ExchangeDate::parse(&day.to_string()).map_err(|_| CALENDAR)?;
            let Some((open, close)) = calendar.hours(date).map_err(|_| CALENDAR)? else {
                continue;
            };
            let end = epoch(day, close)?;
            if end > as_of_ms {
                continue;
            }
            let latest = selection_to_ms.get_or_insert((day, end - 1));
            if (latest.0 - day).num_days() >= settings.max_archive_window_days {
                break;
            }
            sessions.push(day);
            if sessions.len() == settings.recent_archive_sessions {
                sessions.reverse();
                return Ok(Self {
                    dataset_id: RECENT_ARCHIVE_DATASET_ID,
                    sessions,
                    selection_from_ms: epoch(day, open)?,
                    selection_to_ms: latest.1,
                });
            }
        }
        Err("candidate_recent_archive_sessions_unavailable")
    }

    /// First complete exchange sessions after an executable was frozen.
    pub(crate) fn after(
        c: &Config,
        settings: &CandidateStrategySettings,
        frozen: NaiveDate,
        now: i64,
    ) -> Result<Option<Self>> {
        let today = nivra_core::autoresearch::ist_day(now);
        let through = today.min(c.session);
        let calendar = c
            .calendar_at(through)
            .map_err(|_| "candidate_archive_calendar_unavailable")?;
        let mut sessions = Vec::new();
        let mut first = None;
        let epoch = |day, time: LocalTime| {
            DateTime::parse_from_rfc3339(&format!("{day}T{time}+05:30"))
                .map(|t| t.timestamp_millis())
        };
        for day in frozen.iter_days().skip(1).take_while(|day| *day <= through) {
            let date = ExchangeDate::parse(&day.to_string())?;
            let Some((open, close)) = calendar.hours(date)? else {
                continue;
            };
            let close = epoch(day, close)?;
            if close > now {
                break;
            }
            let (first_day, from) = *first.get_or_insert((day, epoch(day, open)?));
            if (day - first_day).num_days() >= settings.max_archive_window_days {
                return Err("candidate_archive_window_invalid".into());
            }
            sessions.push(day);
            if sessions.len() == settings.recent_archive_sessions {
                return Ok(Some(Self {
                    dataset_id: RECENT_ARCHIVE_DATASET_ID,
                    sessions,
                    selection_from_ms: from,
                    selection_to_ms: close - 1,
                }));
            }
        }
        Ok(None)
    }

    pub(super) fn session_bounds(&self) -> (NaiveDate, NaiveDate) {
        (self.sessions[0], self.sessions[self.sessions.len() - 1])
    }
}
