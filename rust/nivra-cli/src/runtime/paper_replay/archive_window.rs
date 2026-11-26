use super::*;
use nivra_core::time::ExchangeDate;
use nivra_store::market_archive::{quote_receipt_bodies, quote_receipt_ids, quote_watermark};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DailyWindow {
    pub from_minute: u32,
    pub to_minute: u32,
}

impl DailyWindow {
    pub(crate) fn validate(self) -> bool {
        self.from_minute < self.to_minute && self.to_minute <= 1440
    }
}

pub(crate) struct ArchiveWindow {
    pub max_days: i64,
    pub from_session: NaiveDate,
    pub to_session: NaiveDate,
    pub from_ms: i64,
    pub to_ms: i64,
    pub daily: Option<DailyWindow>,
    pub receipt_id: Option<i64>,
}

#[derive(Serialize)]
pub(crate) struct ArchiveWindowReport {
    pub as_of_receipt_id: i64,
    pub sessions: Vec<NaiveDate>,
    pub missing_sessions: Vec<NaiveDate>,
}

impl ArchiveWindow {
    fn sessions(&self, c: &Config) -> std::result::Result<Vec<(NaiveDate, i64, i64)>, String> {
        if !(0..self.max_days).contains(&(self.to_session - self.from_session).num_days())
            || self.from_ms < 0
            || self.from_ms > self.to_ms
            || self.daily.is_some_and(|daily| !daily.validate())
        {
            return Err("candidate_archive_window_invalid".into());
        }
        let calendar = c
            .calendar_at(self.from_session)
            .map_err(|_| "candidate_archive_calendar_unavailable")?;
        let ist = chrono::FixedOffset::east_opt(19_800).expect("IST");
        let daily = self.daily.unwrap_or(DailyWindow {
            from_minute: 0,
            to_minute: 1440,
        });
        let mut sessions = Vec::new();
        for day in self
            .from_session
            .iter_days()
            .take_while(|d| *d <= self.to_session)
        {
            let date = ExchangeDate::parse(&day.to_string())
                .map_err(|_| "candidate_archive_calendar_unavailable")?;
            if calendar
                .hours(date)
                .map_err(|_| "candidate_archive_calendar_unavailable")?
                .is_none()
            {
                continue;
            }
            let midnight = day
                .and_hms_opt(0, 0, 0)
                .expect("midnight")
                .and_local_timezone(ist)
                .single()
                .expect("IST")
                .timestamp_millis();
            let from = self
                .from_ms
                .max(midnight + i64::from(daily.from_minute) * 60_000);
            let to = self
                .to_ms
                .min(midnight + i64::from(daily.to_minute) * 60_000 - 1);
            if from <= to {
                sessions.push((day, from, to));
            }
        }
        if sessions.is_empty() {
            return Err("candidate_archive_window_no_sessions".into());
        }
        Ok(sessions)
    }
}

fn retain_pinned_receipts(ids: &mut Vec<(i64, i64)>, from: i64, to: i64, watermark: i64) {
    ids.retain(|&(received, id)| (from..=to).contains(&received) && id <= watermark);
}

pub(crate) fn archive_window_tape(
    c: &Config,
    window: &ArchiveWindow,
    capital: Amount,
    limits: TapeLimits,
    stop: &dyn Fn() -> Option<&'static str>,
) -> std::result::Result<(Option<ExecutionTape>, ArchiveWindowReport), String> {
    check_tape_stop(stop)?;
    let settings = c.market_archive.as_ref().ok_or("archive_config_missing")?;
    let mut db =
        super::super::market_archive::connect(settings).map_err(|_| "archive_connection_failed")?;
    check_tape_stop(stop)?;
    let latest = quote_watermark(&mut db).map_err(|_| "archive_quote_read_failed")?;
    let watermark = window.receipt_id.unwrap_or(latest);
    if !(0..=latest).contains(&watermark) {
        return Err("candidate_archive_watermark_invalid".into());
    }
    let sessions = window.sessions(c)?;
    let mut report = ArchiveWindowReport {
        as_of_receipt_id: watermark,
        sessions: sessions.iter().map(|&(day, _, _)| day).collect(),
        missing_sessions: Vec::new(),
    };
    let root =
        super::super::rollover::root(c).map_err(|_| "candidate_archive_universe_unavailable")?;
    let mut universe = BTreeMap::<String, Instrument>::new();
    let mut builder = ExecutionTapeBuilder::new(capital, limits)?;
    let mut remaining = limits.max_quote_events;
    let mut has_observations = false;
    for (day, from, to) in sessions {
        check_tape_stop(stop)?;
        let mut ids = quote_receipt_ids(&mut db, from, to, watermark, remaining as i64 + 1)
            .map_err(|_| "archive_quote_read_failed")?;
        // Keep the replay boundary local as well as in the archive query.
        retain_pinned_receipts(&mut ids, from, to, watermark);
        if ids.len() > remaining {
            return Err("candidate_execution_tape_too_large".into());
        }
        remaining -= ids.len();
        if ids.is_empty() {
            report.missing_sessions.push(day);
            continue;
        }
        let directory = if day == c.session {
            c.directory.clone()
        } else {
            root.join(day.to_string())
        };
        let instruments = session_universe(&directory, day)
            .map_err(|_| "candidate_archive_universe_unavailable")?;
        let index = instruments.iter().map(|i| (i.key.as_str(), i)).collect();
        for page in ids.chunks(8) {
            check_tape_stop(stop)?;
            let bodies =
                quote_receipt_bodies(&mut db, page).map_err(|_| "archive_quote_read_failed")?;
            for ((received, _), raw) in page.iter().zip(bodies) {
                check_tape_stop(stop)?;
                let mut receipt = archived_receipt(&index, *received, Some(*received), &raw, to)?;
                if receipt.kind == QuotePurposeV1::ResearchObservation {
                    continue;
                }
                receipt.observations.retain(|o| {
                    o["quote"]["source"]
                        .as_i64()
                        .is_some_and(|source| source <= *received)
                });
                for observation in &receipt.observations {
                    let key = observation["key"].as_str().expect("normalized quote key");
                    let instrument = index[key];
                    let prior = universe
                        .entry(key.to_owned())
                        .or_insert_with(|| instrument.clone());
                    if prior != instrument {
                        return Err("candidate_archive_instrument_changed".into());
                    }
                }
                if !receipt.observations.is_empty() {
                    has_observations = true;
                    builder.push(receipt)?;
                }
            }
        }
    }
    if !has_observations {
        return Ok((None, report));
    }
    check_tape_stop(stop)?;
    let first = report.sessions[0];
    let last = *report.sessions.last().expect("nonempty sessions");
    // Paper manifests use previous_session as the lower bound for event dates.
    let tape = builder.finish(
        last,
        (first < last).then_some(first),
        universe.into_values().collect(),
        json!({
            "source":"market_archive_read_only",
            "archive_window":report,
        }),
        false,
    )?;
    check_tape_stop(stop)?;
    Ok((Some(tape), report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_receipts_reproduce_the_tape_after_appends() {
        let fixture = include_str!("../../../../../fixtures/strategy/events.jsonl");
        let rows = fixture
            .lines()
            .enumerate()
            .map(|(i, line)| {
                let event: Value = serde_json::from_str(line).unwrap();
                ((event["at"].as_i64().unwrap(), i as i64 + 1), event)
            })
            .collect::<BTreeMap<_, _>>();
        let from = rows.keys().next().unwrap().0;
        let tape_hash = |mut ids| {
            retain_pinned_receipts(&mut ids, from, from + 3_000, 3);
            let mut tape = ExecutionTapeBuilder::new(
                Amount::from_integer(5000),
                TapeLimits {
                    max_quote_events: 5,
                    max_observations: 100,
                    max_tape_bytes: 64_000,
                },
            )
            .unwrap();
            for id in ids {
                let event = &rows[&id];
                tape.push(QuoteReceipt {
                    at_ms: id.0,
                    receipt_ms: event["available_at"].as_i64().unwrap(),
                    kind: QuotePurposeV1::Protection,
                    observations: event["observations"].as_array().unwrap().clone(),
                })
                .unwrap();
            }
            let manifest: Value = serde_json::from_str(include_str!(
                "../../../../../fixtures/strategy/manifest.json"
            ))
            .unwrap();
            let universe = serde_json::from_value(manifest["universe"].clone()).unwrap();
            let day = manifest["session"].as_str().unwrap().parse().unwrap();
            tape.finish(day, None, universe, json!({}), true)
                .unwrap()
                .events_sha256
        };
        let initial: Vec<_> = rows.keys().take(3).copied().collect();
        let hash = tape_hash(initial.clone());
        assert_ne!(hash, tape_hash(initial[..2].to_vec()));
        // The fourth receipt is inside the time window but its ID follows the pin.
        assert_eq!(hash, tape_hash(rows.keys().copied().collect()));
    }
}
