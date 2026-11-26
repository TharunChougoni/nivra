//! Read-only daily-bar research. No execution, quote synthesis or strategy promotion.
use chrono::{DateTime, NaiveDate, Timelike};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub max_ranked_rows: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_ranked_rows: 20,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    key: String,
    unit: String,
    start: NaiveDate,
    end: NaiveDate,
    requested_ms: i64,
    received_ms: i64,
    body: String,
    body_sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dataset {
    pub schema: u8,
    pub receipts: Vec<serde_json::Value>,
}
#[derive(Serialize)]
pub struct Summary {
    pub instruments: usize,
    pub bars: usize,
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub ranked_close_returns: Vec<serde_json::Value>,
}
/// Verify original provider receipt hashes, dates, OHLC units and duplicate bars.
/// Returns are descriptive unadjusted close returns, never simulated profit.
pub fn evaluate(dataset: &Dataset) -> Result<Summary, &'static str> {
    evaluate_with_settings(dataset, &Settings::default())
}

pub fn evaluate_with_settings(
    dataset: &Dataset,
    settings: &Settings,
) -> Result<Summary, &'static str> {
    if dataset.schema != 1 || dataset.receipts.is_empty() || dataset.receipts.len() > 10000 {
        return Err("daily_dataset_invalid");
    }
    let mut series = BTreeMap::<String, BTreeMap<NaiveDate, Vec<serde_json::Value>>>::new();
    for value in &dataset.receipts {
        let r: Receipt =
            serde_json::from_value(value.clone()).map_err(|_| "daily_receipt_schema")?;
        if r.unit != "days"
            || r.key.is_empty()
            || r.key.len() > 128
            || r.start > r.end
            || r.requested_ms > r.received_ms
            || crate::replay::digest(r.body.as_bytes()) != r.body_sha256
        {
            return Err("daily_receipt_binding");
        }
        let body: serde_json::Value = serde_json::from_str(&r.body).map_err(|_| "daily_body")?;
        if body["status"] != "success" {
            return Err("daily_provider_status");
        }
        let rows = body["data"]["candles"].as_array().ok_or("daily_candles")?;
        for row in rows {
            let fields = row.as_array().ok_or("daily_row")?;
            if fields.len() != 7 {
                return Err("daily_row");
            }
            let t = DateTime::parse_from_rfc3339(fields[0].as_str().ok_or("daily_time")?)
                .map_err(|_| "daily_time")?;
            let day = t.date_naive();
            if t.offset().local_minus_utc() != 19800
                || t.hour() != 0
                || t.minute() != 0
                || t.second() != 0
                || t.nanosecond() != 0
                || day < r.start
                || day > r.end
                || t.timestamp_millis() > r.received_ms
            {
                return Err("daily_time");
            }
            let mut prices = vec![];
            for v in &fields[1..5] {
                let p = v.as_f64().ok_or("daily_price")?;
                if !p.is_finite() || p <= 0.0 {
                    return Err("daily_price");
                }
                prices.push(p);
            }
            if prices[2] > prices[0]
                || prices[2] > prices[3]
                || prices[1] < prices[0]
                || prices[1] < prices[3]
                || prices[2] > prices[1]
                || fields[5].as_u64().is_none()
                || fields[6].as_u64().is_none()
            {
                return Err("daily_ohlcv");
            }
            let bars = series.entry(r.key.clone()).or_default();
            if let Some(old) = bars.insert(day, fields.clone())
                && old != *fields
            {
                return Err("daily_conflict");
            }
        }
    }
    let series = series
        .into_iter()
        .map(|(key, bars)| {
            let closes = bars
                .into_iter()
                .map(|(day, fields)| (day, fields[4].as_f64().expect("validated close")))
                .collect();
            (key, closes)
        })
        .collect();
    summarize(&series, settings)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveDataset {
    pub schema: u8,
    pub bars: Vec<ArchiveBar>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveBar {
    pub key: String,
    pub day: NaiveDate,
    pub close_paise: i64,
}

pub fn evaluate_archive(dataset: &ArchiveDataset) -> Result<Summary, &'static str> {
    if dataset.schema != 2 || dataset.bars.is_empty() {
        return Err("daily_dataset_invalid");
    }
    let mut series = BTreeMap::<String, BTreeMap<NaiveDate, f64>>::new();
    for bar in &dataset.bars {
        if bar.key.is_empty() || bar.key.len() > 128 || bar.close_paise <= 0 {
            return Err("daily_row");
        }
        if series
            .entry(bar.key.clone())
            .or_default()
            .insert(bar.day, bar.close_paise as f64)
            .is_some()
        {
            return Err("daily_conflict");
        }
    }
    summarize(&series, &Settings::default())
}

fn summarize(
    series: &BTreeMap<String, BTreeMap<NaiveDate, f64>>,
    settings: &Settings,
) -> Result<Summary, &'static str> {
    let mut ranked = vec![];
    let mut first = None;
    let mut last = None;
    let mut count = 0;
    for (key, bars) in series {
        let (Some((start, a)), Some((end, b))) = (bars.first_key_value(), bars.last_key_value())
        else {
            continue;
        };
        first = Some(first.map_or(*start, |d: NaiveDate| d.min(*start)));
        last = Some(last.map_or(*end, |d: NaiveDate| d.max(*end)));
        count += bars.len();
        let ret = b / a - 1.0;
        if !ret.is_finite() {
            return Err("daily_return");
        }
        ranked.push(serde_json::json!({"instrument_key":key,"bars":bars.len(),"first_day":start,"last_day":end,"unadjusted_close_return":ret}));
    }
    ranked.sort_by(|a, b| {
        b["unadjusted_close_return"]
            .as_f64()
            .partial_cmp(&a["unadjusted_close_return"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                a["instrument_key"]
                    .as_str()
                    .cmp(&b["instrument_key"].as_str())
            })
    });
    ranked.truncate(settings.max_ranked_rows);
    Ok(Summary {
        instruments: series.len(),
        bars: count,
        first_day: first.ok_or("daily_empty")?,
        last_day: last.ok_or("daily_empty")?,
        ranked_close_returns: ranked,
    })
}

#[cfg(test)]
mod settings_defaults {
    use super::*;
    #[test]
    fn omitted_settings_preserve_original_constants() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.max_ranked_rows, 20);
    }
}
