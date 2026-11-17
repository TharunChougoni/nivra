//! Offline measurement of captured catalog timings against a cadence (#202).
//! This compares already recorded samples; it does not capture or call a vendor.
use serde_json::{Value, json};

fn hash_ok(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn sample_ok(sample: &Value) -> Result<(i64, i64, i64, u64, bool), String> {
    let started = sample["started_at_ms"]
        .as_i64()
        .ok_or("catalog_measure_sample_invalid")?;
    let finished = sample["finished_at_ms"]
        .as_i64()
        .ok_or("catalog_measure_sample_invalid")?;
    if started < 0 || finished < started {
        return Err("catalog_measure_sample_invalid".into());
    }
    let cpu = sample["cpu_user_ms"].as_i64().unwrap_or(0);
    let sys = sample["cpu_system_ms"].as_i64().unwrap_or(0);
    let rss = sample["rss_bytes"].as_u64().unwrap_or(0);
    if cpu < 0 || sys < 0 {
        return Err("catalog_measure_sample_invalid".into());
    }
    if let Some(hash) = sample["source_sha256"].as_str()
        && !hash_ok(hash)
    {
        return Err("catalog_measure_sample_invalid".into());
    }
    match sample["outcome"].as_str() {
        Some("ok") | Some("failed") | Some("incomplete") | None => {}
        Some(_) => return Err("catalog_measure_sample_invalid".into()),
    }
    Ok((
        started,
        finished.saturating_sub(started),
        cpu.saturating_add(sys),
        rss,
        sample["outcome"].as_str() == Some("ok") || sample.get("outcome").is_none(),
    ))
}

fn metrics_of(report: &Value) -> Result<(u64, u64, i64, u64), String> {
    Ok((
        report["intervals_exceeding_cadence"]
            .as_u64()
            .ok_or("catalog_measure_previous_invalid")?,
        report["failed_samples"]
            .as_u64()
            .ok_or("catalog_measure_previous_invalid")?,
        report["max_duration_ms"]
            .as_i64()
            .ok_or("catalog_measure_previous_invalid")?,
        report["max_rss_bytes"].as_u64().unwrap_or(0),
    ))
}

/// Measure captured catalog timings. The stored path is kept unless the candidate
/// is strictly better; replacement is never authorized here.
pub fn evaluate(current: &Value, previous: Option<&Value>) -> Result<Value, String> {
    if current["version"].as_u64() != Some(1) {
        return Err("catalog_measure_invalid".into());
    }
    let cadence = current["cadence_ms"]
        .as_i64()
        .ok_or("catalog_measure_invalid")?;
    if !(1..=300_000).contains(&cadence) {
        return Err("catalog_measure_invalid".into());
    }
    let samples = current["samples"]
        .as_array()
        .ok_or("catalog_measure_invalid")?;
    if samples.is_empty() || samples.len() > 10_000 {
        return Err("catalog_measure_invalid".into());
    }
    let mut parsed = Vec::with_capacity(samples.len());
    let mut previous_started = None;
    for sample in samples {
        let (started, duration, cpu, rss, ok) = sample_ok(sample)?;
        if previous_started.is_some_and(|last| started < last) {
            return Err("catalog_measure_sample_order".into());
        }
        previous_started = Some(started);
        parsed.push((started, duration, cpu, rss, ok));
    }
    let mut intervals = Vec::new();
    let mut exceeding = 0u64;
    for window in parsed.windows(2) {
        let interval = window[1].0.saturating_sub(window[0].0);
        let over = interval > cadence;
        if over {
            exceeding += 1;
        }
        intervals.push(json!({"from_ms":window[0].0,"to_ms":window[1].0,"interval_ms":interval,"exceeds_cadence":over}));
    }
    let max_duration = parsed.iter().map(|row| row.1).max().unwrap_or(0);
    let duration_sum: i64 = parsed.iter().map(|row| row.1).sum();
    let mean_duration = duration_sum / i64::try_from(parsed.len()).unwrap_or(1);
    let max_rss = parsed.iter().map(|row| row.3).max().unwrap_or(0);
    let cpu_total: i64 = parsed.iter().map(|row| row.2).sum();
    let failed = parsed.iter().filter(|row| !row.4).count() as u64;
    let report = json!({
        "schema": 1,
        "cadence_ms": cadence,
        "sample_count": parsed.len(),
        "intervals": intervals,
        "intervals_exceeding_cadence": exceeding,
        "max_duration_ms": max_duration,
        "mean_duration_ms": mean_duration,
        "max_rss_bytes": max_rss,
        "cpu_ms_total": cpu_total,
        "failed_samples": failed,
        "within_cadence": exceeding == 0,
        "activation_authorized": false,
        "replace_authorized": false
    });
    let comparison = if let Some(previous) = previous {
        let prior = if previous.get("schema") == Some(&json!(1)) {
            previous.clone()
        } else {
            evaluate(previous, None)?
        };
        let candidate = metrics_of(&report)?;
        let stored = metrics_of(&prior)?;
        let prefer = if candidate < stored {
            "candidate"
        } else {
            "stored"
        };
        json!({
            "prefer": prefer,
            "reason": if prefer == "stored" { "stored_better_or_equal" } else { "candidate_strictly_better" },
            "stored": {"intervals_exceeding_cadence":stored.0,"failed_samples":stored.1,"max_duration_ms":stored.2,"max_rss_bytes":stored.3},
            "candidate": {"intervals_exceeding_cadence":candidate.0,"failed_samples":candidate.1,"max_duration_ms":candidate.2,"max_rss_bytes":candidate.3},
            "replace_authorized": false
        })
    } else {
        json!({
            "prefer":"unknown",
            "reason":"no_previous_measurement",
            "replace_authorized": false
        })
    };
    let mut report = report;
    report["comparison"] = comparison;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(spacing: i64, duration: i64) -> Value {
        json!({
            "version":1,
            "cadence_ms":30_000,
            "samples":[
                {"started_at_ms":0,"finished_at_ms":duration,"cpu_user_ms":10,"cpu_system_ms":2,"rss_bytes":1000,"outcome":"ok"},
                {"started_at_ms":spacing,"finished_at_ms":spacing+duration,"cpu_user_ms":11,"cpu_system_ms":2,"rss_bytes":1100,"outcome":"ok"}
            ]
        })
    }

    #[test]
    fn catalog_measure_prefers_stored_when_not_strictly_better() {
        let inside = evaluate(&samples(20_000, 800), None).unwrap();
        assert_eq!(inside["within_cadence"], true);
        assert_eq!(inside["intervals_exceeding_cadence"], 0);
        assert_eq!(inside["max_duration_ms"], 800);
        assert_eq!(inside["comparison"]["prefer"], "unknown");
        assert_eq!(inside["replace_authorized"], false);
        let late = evaluate(&samples(40_000, 800), None).unwrap();
        assert_eq!(late["within_cadence"], false);
        assert_eq!(late["intervals_exceeding_cadence"], 1);
        let vs_worse = evaluate(&samples(20_000, 800), Some(&late)).unwrap();
        assert_eq!(vs_worse["comparison"]["prefer"], "candidate");
        assert_eq!(vs_worse["comparison"]["replace_authorized"], false);
        let vs_equal = evaluate(&samples(20_000, 800), Some(&inside)).unwrap();
        assert_eq!(vs_equal["comparison"]["prefer"], "stored");
        let vs_better = evaluate(&samples(20_000, 1200), Some(&inside)).unwrap();
        assert_eq!(vs_better["comparison"]["prefer"], "stored");
        assert_eq!(vs_better["activation_authorized"], false);
    }
}
