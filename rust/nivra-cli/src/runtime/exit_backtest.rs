//! Exploratory daily exit sensitivity, isolated from trading and credentials.
use super::{Result, digest, history_download};
use chrono::NaiveDate;
use nivra_core::{
    execution::{Instrument, Portfolio},
    finance::{Amount, FeeArithmetic, Money, Price, Side},
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, path::Path, process::ExitCode};

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub allocation_inr: Amount,
    pub stop_multiplier: Amount,
    pub target_multiplier: Amount,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            allocation_inr: Amount::from_integer(10000),
            stop_multiplier: Amount::parse("0.98").expect("fixed decimal"),
            target_multiplier: Amount::parse("1.02").expect("fixed decimal"),
        }
    }
}

fn read(path: &Path) -> Result<Vec<u8>> {
    crate::read_bounded(path, 32 * 1024 * 1024).map_err(|_| "exit_evidence_read".into())
}
fn amount(v: &Value) -> Result<Amount> {
    Ok(Amount::parse(&v.to_string())?)
}
fn number(a: Amount) -> Result<f64> {
    Ok(a.to_string().parse()?)
}
fn net(
    open: Amount,
    exit: Amount,
    tick: Price,
    settings: &Settings,
    costs: &FeeArithmetic,
) -> Result<Option<f64>> {
    let buy = Portfolio::execution_with_costs(Price::try_from(open)?, tick, true, costs)?;
    let units = costs.affordable_cash_lots(buy, settings.allocation_inr, i64::MAX, false, 0)?;
    if units == 0 {
        return Ok(None);
    }
    let n = Amount::from_unsigned(units);
    let entry = buy.amount().checked_mul(n)?;
    let sell = Portfolio::execution_with_costs(Price::try_from(exit)?, tick, false, costs)?
        .amount()
        .checked_mul(n)?;
    let pnl = sell
        .checked_sub(entry)?
        .checked_sub(costs.charge(Side::Buy, Money::try_from(entry)?)?.amount())?
        .checked_sub(costs.charge(Side::Sell, Money::try_from(sell)?)?.amount())?;
    Ok(Some(
        number(pnl)? / number(settings.allocation_inr)? * 100.0,
    ))
}
fn stats(db: &Connection, month: Option<&str>) -> Result<Value> {
    let filter = "(?1 IS NULL OR month=?1)";
    let n: i64 = db.query_row(
        &format!("SELECT count(*) FROM trades WHERE {filter}"),
        [month],
        |r| r.get(0),
    )?;
    let mut out = json!({"trades": n});
    for rule in ["stop", "tp", "close", "ambiguous"] {
        let count: i64 = db.query_row(
            &format!("SELECT count(*) FROM trades WHERE {filter} AND rule='{rule}'"),
            [month],
            |r| r.get(0),
        )?;
        out[rule] = json!({"count":count,"rate":if n == 0 {0.0} else {count as f64 / n as f64}});
    }
    for ordering in ["conservative", "optimistic"] {
        let mean: Option<f64> = db.query_row(
            &format!("SELECT avg({ordering}) FROM trades WHERE {filter}"),
            [month],
            |r| r.get(0),
        )?;
        let median: Option<f64> = db.query_row(&format!("SELECT avg(value) FROM (SELECT {ordering} AS value FROM trades WHERE {filter} ORDER BY {ordering} LIMIT ?2 OFFSET ?3)"), params![month, if n == 0 {0} else {2 - n % 2}, (n - 1).max(0) / 2], |r| r.get(0))?;
        out[ordering] = json!({"average_net_return_pct_of_allocation":mean,"median_net_return_pct_of_allocation":median});
    }
    Ok(out)
}
fn report(
    history: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    settings: &Settings,
    costs: &FeeArithmetic,
) -> Result<Value> {
    if start.zip(end).is_some_and(|(s, e)| s > e) {
        return Err("exit_date_range".into());
    }
    // Empty filename is a private temporary on-disk DB, removed on close.
    let db = Connection::open("")?;
    db.execute_batch("PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; CREATE TABLE trades(key TEXT, day TEXT, month TEXT, rule TEXT, conservative REAL, optimistic REAL, UNIQUE(key,day)); CREATE TABLE seen(key TEXT, day TEXT, PRIMARY KEY(key,day));")?;
    let mut receipts = 0;
    let mut skipped = 0;
    for item in fs::read_dir(history)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".receipt.json") {
            continue;
        }
        let bytes = read(&item.path())?;
        let r: history_download::Receipt = serde_json::from_slice(&bytes)?;
        if r.unit != "days" {
            continue;
        }
        let id = history_download::range_id(&r.key, &r.unit, r.start, r.end);
        if r.start > r.end || name != format!("{id}.receipt.json") {
            return Err("exit_receipt_binding".into());
        }
        let rows = history_download::candles(&r)?;
        receipts += 1;
        for (day, row) in rows {
            let prefix = format!("{id}-{day}");
            let shard: Value =
                serde_json::from_slice(&read(&history.join(format!("{prefix}.candles.json")))?)?;
            if shard["receipt"] != name
                || shard["receipt_sha256"] != digest(&bytes)
                || shard["candles"] != json!([row])
            {
                return Err("exit_shard_binding".into());
            }
            let manifest: Value =
                serde_json::from_slice(&read(&history.join(format!("{prefix}.manifest.json")))?)?;
            let instruments: Vec<Instrument> =
                serde_json::from_value(manifest["universe"].clone())?;
            if instruments.len() != 1
                || instruments[0].key != r.key
                || manifest["session"] != day.to_string()
            {
                return Err("exit_instrument_binding".into());
            }
            // Reject overlapping evidence instead of silently double counting.
            db.execute(
                "INSERT INTO seen VALUES (?1,?2)",
                params![r.key, day.to_string()],
            )?;
            if start.is_some_and(|s| day < s) || end.is_some_and(|e| day > e) {
                continue;
            }
            let open = amount(&row[1])?;
            let stop = open.checked_mul(settings.stop_multiplier)?;
            let tp = open.checked_mul(settings.target_multiplier)?;
            let (rule, low, high) = match (amount(&row[3])? <= stop, amount(&row[2])? >= tp) {
                (true, true) => ("ambiguous", stop, tp),
                (true, false) => ("stop", stop, stop),
                (false, true) => ("tp", tp, tp),
                _ => ("close", amount(&row[4])?, amount(&row[4])?),
            };
            let Some(conservative) = net(open, low, instruments[0].tick, settings, costs)? else {
                skipped += 1;
                continue;
            };
            let optimistic =
                net(open, high, instruments[0].tick, settings, costs)?.ok_or("exit_size")?;
            db.execute(
                "INSERT INTO trades VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    r.key,
                    day.to_string(),
                    day.format("%Y-%m").to_string(),
                    rule,
                    conservative,
                    optimistic
                ],
            )?;
        }
    }
    if receipts == 0 {
        return Err("exit_no_daily_receipts".into());
    }
    let mut months = serde_json::Map::new();
    let mut stmt = db.prepare("SELECT DISTINCT month FROM trades ORDER BY month")?;
    for month in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let month = month?;
        months.insert(month.clone(), stats(&db, Some(&month))?);
    }
    Ok(
        json!({"schema":1,"label":"exploratory daily exit sensitivity; no profitability claim", "start":start,"end":end,"daily_receipts_validated":receipts,"skipped_unaffordable":skipped,
        "paper_costs":costs,
        "method":format!("Independent long entry at each instrument-day open; thresholds relative to raw open. Whole shares within INR {} including entry fees; no compounding or portfolio competition. Configured paper costs and adverse tick rounding apply. Returns divide net P&L by INR {}, including idle allocation. Both-touched days are conservative stop-first and optimistic TP-first. Rule categories are exclusive; rates divide by simulated trades. Close means daily close proxy for 15:15 flatten.", settings.allocation_inr, settings.allocation_inr),
        "limitations":["Daily resolution cannot establish intraday ordering or 15:15 prices; threshold execution and close flatten are hypothetical proxies.","No liquidity, bid/ask spread, depth, circuit or executable-fill evidence.","Contemporary frozen universe, not historical membership; survivorship bias.","Entry-at-open proxy is not the strategy momentum signal or eligible candidate selection.","Historical calendars, completeness, corporate actions and provider adjustments remain unverified.","No portfolio risk, allocation competition, model selection or profitability certification."],
        "overall":stats(&db,None)?,"by_month":months}),
    )
}
pub fn entry(
    history: &Path,
    output: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
    config: Option<&Path>,
) -> ExitCode {
    let result = (|| -> Result<()> {
        let config = config.ok_or("paper_costs_required")?;
        let config: super::Config = serde_json::from_slice(&read(config)?)?;
        let costs = config.paper_costs.as_ref().ok_or("paper_costs_required")?;
        costs.validate()?;
        let settings = config.exit_backtest;
        if settings.allocation_inr <= Amount::ZERO
            || settings.stop_multiplier <= Amount::ZERO
            || settings.target_multiplier <= Amount::ZERO
        {
            return Err("exit_settings_invalid".into());
        }
        let value = report(history, start, end, &settings, costs)?;
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        if parent.canonicalize()?.starts_with(history.canonicalize()?) {
            return Err("exit_output_inside_history".into());
        }
        let bytes = serde_json::to_vec_pretty(&value)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(output)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            println!("exit_backtest_complete: exploratory report");
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!(
                "exit_backtest_failed: invalid evidence, range or output; originals preserved"
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn daily_exits_costs_ambiguity_and_evidence_refusal() {
        let dir = tempfile::tempdir().unwrap();
        let start: NaiveDate = "2026-01-01".parse().unwrap();
        let end: NaiveDate = "2026-01-04".parse().unwrap();
        let key = "NSE_EQ|SYNTHETIC";
        let id = history_download::range_id(key, "days", start, end);
        let rows = json!([
            ["2026-01-01T00:00:00+05:30", 100, 103, 97, 101, 1000, 0],
            ["2026-01-02T00:00:00+05:30", 100, 101, 97, 100, 1000, 0],
            ["2026-01-03T00:00:00+05:30", 100, 103, 99, 100, 1000, 0],
            ["2026-01-04T00:00:00+05:30", 100, 101, 99, 100, 1000, 0]
        ]);
        let body = json!({"status":"success","data":{"candles":rows}}).to_string();
        let receipt = serde_json::to_vec(&json!({"key":key,"unit":"days","start":start,"end":end,"requested_ms":1800000000000_i64,"received_ms":1800000000001_i64,"body":body,"body_sha256":digest(body.as_bytes())})).unwrap();
        let name = format!("{id}.receipt.json");
        fs::write(dir.path().join(&name), &receipt).unwrap();
        for row in rows.as_array().unwrap() {
            let day = &row[0].as_str().unwrap()[..10];
            let prefix = format!("{id}-{day}");
            fs::write(
                dir.path().join(format!("{prefix}.candles.json")),
                serde_json::to_vec(
                    &json!({"receipt":name,"receipt_sha256":digest(&receipt),"candles":[row]}),
                )
                .unwrap(),
            )
            .unwrap();
            fs::write(dir.path().join(format!("{prefix}.manifest.json")),serde_json::to_vec(&json!({"session":day,"universe":[{"key":key,"symbol":"SYNTHETIC","series":"EQ","tick":"0.05"}]})).unwrap()).unwrap();
        }
        let r = report(
            dir.path(),
            None,
            None,
            &Settings::default(),
            &crate::runtime::tests::test_costs(),
        )
        .unwrap();
        assert_eq!(r["overall"]["trades"], 4);
        for rule in ["stop", "tp", "close", "ambiguous"] {
            assert_eq!(r["overall"][rule]["rate"], 0.25);
        }
        // 99 shares: buy 100.05, stop sell 97.95, TP sell 101.90,
        // close sell 99.95. Entry fee 34.91; exit fees 34.70/35.09/34.90.
        let stop = (-207.90 - 34.91 - 34.70) / 100.0;
        let tp = (183.15 - 34.91 - 35.09) / 100.0;
        let close = (-9.90 - 34.91 - 34.90) / 100.0;
        for (order, mean, median) in [
            (
                "conservative",
                (stop * 2.0 + tp + close) / 4.0,
                (stop + close) / 2.0,
            ),
            (
                "optimistic",
                (stop + tp * 2.0 + close) / 4.0,
                (tp + close) / 2.0,
            ),
        ] {
            assert!(
                (r["overall"][order]["average_net_return_pct_of_allocation"]
                    .as_f64()
                    .unwrap()
                    - mean)
                    .abs()
                    < 1e-9
            );
            assert!(
                (r["overall"][order]["median_net_return_pct_of_allocation"]
                    .as_f64()
                    .unwrap()
                    - median)
                    .abs()
                    < 1e-9
            );
        }
        assert_eq!(r["by_month"]["2026-01"]["trades"], 4);
        assert_eq!(
            report(
                dir.path(),
                Some(end),
                Some(end),
                &Settings::default(),
                &crate::runtime::tests::test_costs()
            )
            .unwrap()["overall"]["trades"],
            1
        );
        fs::write(
            dir.path().join(format!("{id}-2026-01-01.candles.json")),
            b"{}",
        )
        .unwrap();
        assert!(
            report(
                dir.path(),
                None,
                None,
                &Settings::default(),
                &crate::runtime::tests::test_costs()
            )
            .is_err()
        );
        assert!(dir.path().join(name).exists());
    }
}

#[cfg(test)]
mod settings_defaults {
    use super::*;
    #[test]
    fn omitted_settings_preserve_original_constants() {
        let s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.allocation_inr, Amount::from_integer(10000));
        assert_eq!(s.stop_multiplier, Amount::parse("0.98").unwrap());
        assert_eq!(s.target_multiplier, Amount::parse("1.02").unwrap());
    }
}
