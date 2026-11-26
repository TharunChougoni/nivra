//! Exploratory daily exit sensitivity, isolated from trading and credentials.
use super::{Result, market_archive, wide};
use chrono::NaiveDate;
use nivra_core::{
    execution::{Instrument, Portfolio},
    finance::{Amount, FeeArithmetic, Money, Price, Side},
};
use nivra_store::market_archive::{DailyBar, daily_bars};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
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
    batches: impl Iterator<Item = Result<Vec<DailyBar>>>,
    instruments: &[Instrument],
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
    db.execute_batch("PRAGMA cache_size=-2048; PRAGMA temp_store=FILE; CREATE TABLE trades(key TEXT, day TEXT, month TEXT, rule TEXT, conservative REAL, optimistic REAL, UNIQUE(key,day));")?;
    let ticks: BTreeMap<_, _> = instruments
        .iter()
        .map(|i| (i.key.as_str(), i.tick))
        .collect();
    let mut bars = 0;
    let mut skipped = 0;
    for batch in batches {
        for bar in batch? {
            let tick = *ticks
                .get(bar.key.as_str())
                .ok_or("exit_instrument_binding")?;
            bars += 1;
            let open = market_archive::from_paise(bar.open_paise)?;
            let stop = open.checked_mul(settings.stop_multiplier)?;
            let tp = open.checked_mul(settings.target_multiplier)?;
            let (rule, low, high) = match (
                market_archive::from_paise(bar.low_paise)? <= stop,
                market_archive::from_paise(bar.high_paise)? >= tp,
            ) {
                (true, true) => ("ambiguous", stop, tp),
                (true, false) => ("stop", stop, stop),
                (false, true) => ("tp", tp, tp),
                _ => {
                    let close = market_archive::from_paise(bar.close_paise)?;
                    ("close", close, close)
                }
            };
            let Some(conservative) = net(open, low, tick, settings, costs)? else {
                skipped += 1;
                continue;
            };
            let optimistic = net(open, high, tick, settings, costs)?.ok_or("exit_size")?;
            db.execute(
                "INSERT INTO trades VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    bar.key,
                    bar.day.to_string(),
                    bar.day.format("%Y-%m").to_string(),
                    rule,
                    conservative,
                    optimistic
                ],
            )?;
        }
    }
    if bars == 0 {
        return Err("exit_no_daily_bars".into());
    }
    let mut months = serde_json::Map::new();
    let mut stmt = db.prepare("SELECT DISTINCT month FROM trades ORDER BY month")?;
    for month in stmt.query_map([], |r| r.get::<_, String>(0))? {
        let month = month?;
        months.insert(month.clone(), stats(&db, Some(&month))?);
    }
    Ok(
        json!({"schema":2,"label":"exploratory daily exit sensitivity; no profitability claim", "start":start,"end":end,"daily_bars":bars,"skipped_unaffordable":skipped,
        "paper_costs":costs,
        "method":format!("Independent long entry at each instrument-day open; thresholds relative to raw open. Whole shares within INR {} including entry fees; no compounding or portfolio competition. Configured paper costs and adverse tick rounding apply. Returns divide net P&L by INR {}, including idle allocation. Both-touched days are conservative stop-first and optimistic TP-first. Rule categories are exclusive; rates divide by simulated trades. Close means daily close proxy for 15:15 flatten.", settings.allocation_inr, settings.allocation_inr),
        "limitations":["Daily resolution cannot establish intraday ordering or 15:15 prices; threshold execution and close flatten are hypothetical proxies.","No liquidity, bid/ask spread, depth, circuit or executable-fill evidence.","Contemporary frozen universe, not historical membership; survivorship bias.","Entry-at-open proxy is not the strategy momentum signal or eligible candidate selection.","Historical calendars, completeness, corporate actions and provider adjustments remain unverified.","No portfolio risk, allocation competition, model selection or profitability certification."],
        "overall":stats(&db,None)?,"by_month":months}),
    )
}
pub fn entry(
    config: &Path,
    output: &Path,
    start: Option<NaiveDate>,
    end: Option<NaiveDate>,
) -> ExitCode {
    let result = (|| -> Result<()> {
        let config: super::Config = serde_json::from_slice(&read(config)?)?;
        let costs = config.paper_costs.as_ref().ok_or("paper_costs_required")?;
        costs.validate()?;
        let settings = &config.exit_backtest;
        if settings.allocation_inr <= Amount::ZERO
            || settings.stop_multiplier <= Amount::ZERO
            || settings.target_multiplier <= Amount::ZERO
        {
            return Err("exit_settings_invalid".into());
        }
        let instruments = wide::history_instruments(&config)?;
        let mut archive = market_archive::open(&config)?;
        let batches = instruments.chunks(40).map(|chunk| {
            let keys: Vec<_> = chunk
                .iter()
                .map(|instrument| instrument.key.as_str())
                .collect();
            daily_bars(&mut archive, &keys, start, end)
        });
        let value = report(batches, &instruments, start, end, settings, costs)?;
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
    fn daily_exits_costs_and_ambiguity() {
        let key = "NSE_EQ|SYNTHETIC";
        let instruments = [Instrument {
            key: key.into(),
            symbol: "SYNTHETIC".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        }];
        let rows = [
            (10300, 9700, 10100),
            (10100, 9700, 10000),
            (10300, 9900, 10000),
            (10100, 9900, 10000),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (high, low, close))| DailyBar {
            key: key.into(),
            day: NaiveDate::from_ymd_opt(2026, 1, index as u32 + 1).unwrap(),
            open_paise: 10000,
            high_paise: high,
            low_paise: low,
            close_paise: close,
            volume: 1000,
        })
        .collect();
        let r = report(
            std::iter::once(Ok(rows)),
            &instruments,
            None,
            None,
            &Settings::default(),
            &crate::runtime::tests::test_costs(),
        )
        .unwrap();
        assert_eq!(r["schema"], 2);
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
