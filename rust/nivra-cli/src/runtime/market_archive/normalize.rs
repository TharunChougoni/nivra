use super::super::{Result, data, history_download};
use chrono::DateTime;
use nivra_core::finance::{Amount, Price};
use nivra_store::market_archive::{
    CandleBar, CandleBatch, CandleUnit, QuoteBatch, QuoteObservation, QuotePurpose, Receipt,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Deserialize)]
struct ArchivedQuote {
    source: String,
    purpose: String,
    requested_ms: i64,
    received_ms: i64,
    body: String,
}

/// Keep provider decimal noise in the receipt and round archive prices to paise.
fn paise(amount: Amount) -> Result<i64> {
    if amount < Amount::ZERO {
        return Err("archive_price_negative".into());
    }
    let rounded = amount.checked_add(Amount::parse("0.005")?)?;
    Ok(i64::try_from(
        rounded.checked_floor_ratio(Amount::parse("0.01")?)?,
    )?)
}

fn price(field: &Value) -> Result<i64> {
    if !field.is_number() {
        return Err("archive_price_invalid".into());
    }
    paise(nivra_core::execution::number(field)?)
}

fn positive_paise(amount: Amount) -> Result<i64> {
    let value = paise(amount)?;
    if value == 0 {
        return Err("archive_price_below_one_paisa".into());
    }
    Ok(value)
}

fn quantity(field: &Value) -> Result<i64> {
    Ok(i64::try_from(
        field.as_u64().ok_or("archive_quantity_invalid")?,
    )?)
}

fn depth_columns(level: Option<(Price, u64)>) -> Result<(Option<i64>, Option<i64>)> {
    match level {
        Some((price, quantity)) => Ok((
            Some(positive_paise(price.amount())?),
            Some(i64::try_from(quantity)?),
        )),
        None => Ok((None, None)),
    }
}

fn validate_receipt_times(requested_ms: i64, received_ms: i64) -> Result<()> {
    if requested_ms < 0 || received_ms < requested_ms {
        return Err("archive_receipt_time_invalid".into());
    }
    Ok(())
}

/// Normalize the original JSON envelope from a wide_quotes or scanner_metadata tick.
#[cfg(test)]
fn quotes(raw: &str) -> Result<QuoteBatch<'_>> {
    quote_batch(raw, false)
}

/// Keep the exact body even when individual instruments lack normalizable
/// fields. Replay applies its own receipt checks to the original bytes.
pub(in crate::runtime) fn capture_quotes(raw: &str) -> Result<QuoteBatch<'_>> {
    quote_batch(raw, true)
}

fn quote_batch(raw: &str, allow_partial: bool) -> Result<QuoteBatch<'_>> {
    let archive: ArchivedQuote = serde_json::from_str(raw)?;
    validate_receipt_times(archive.requested_ms, archive.received_ms)?;
    if archive.source != "upstox-readonly-v1" {
        return Err("archive_quote_source_invalid".into());
    }
    let purpose = match archive.purpose.as_str() {
        "scanner" => QuotePurpose::Scanner,
        "execution_or_protection" => QuotePurpose::ExecutionOrProtection,
        "scanner_metadata" => QuotePurpose::ScannerMetadata,
        "research_observation" => QuotePurpose::ResearchObservation,
        _ => return Err("archive_quote_purpose_invalid".into()),
    };
    let body: Value = serde_json::from_str(&archive.body)?;
    if body["status"] != "success" {
        return Err("quote_status".into());
    }
    let rows = body["data"].as_object().ok_or("quote_data")?;
    let mut keys = BTreeSet::new();
    let mut observations = Vec::with_capacity(rows.len());
    for row in rows.values() {
        let key = row["instrument_token"]
            .as_str()
            .filter(|key| !key.is_empty())
            .ok_or("quote_identity")?;
        if !keys.insert(key) {
            return Err("duplicate_quote".into());
        }
        match quote_observation(key, row, archive.received_ms) {
            Ok(observation) => observations.push(observation),
            Err(_) if allow_partial => {}
            Err(error) => return Err(error),
        }
    }
    Ok(QuoteBatch {
        receipt: Receipt::new(archive.requested_ms, archive.received_ms, raw.as_bytes()),
        purpose,
        observations,
    })
}

fn quote_observation(key: &str, row: &Value, received_ms: i64) -> Result<QuoteObservation> {
    let source_ms = data::quote_source(row)?;
    let best_bid = data::quote_depth(row, "buy")?;
    let best_ask = data::quote_depth(row, "sell")?;
    if !(0..=received_ms).contains(&source_ms)
        || best_bid.zip(best_ask).is_some_and(|(b, a)| b.0 > a.0)
    {
        return Err("archive_quote_values_invalid".into());
    }
    let (bid, bid_qty) = depth_columns(best_bid)?;
    let (ask, ask_qty) = depth_columns(best_ask)?;
    let lower_circuit = price(&row["lower_circuit_limit"])?;
    let upper_circuit = price(&row["upper_circuit_limit"])?;
    if lower_circuit > upper_circuit {
        return Err("archive_quote_circuit_invalid".into());
    }
    Ok(QuoteObservation {
        instrument_key: key.to_owned(),
        source_ms,
        ltp: price(&row["last_price"])?,
        bid,
        ask,
        bid_qty,
        ask_qty,
        volume: quantity(&row["volume"])?,
        lower_circuit,
        upper_circuit,
    })
}

/// Normalize an immutable history-download receipt without reserializing it.
pub(crate) fn candles(raw: &str) -> Result<CandleBatch<'_>> {
    let receipt: history_download::Receipt = serde_json::from_str(raw)?;
    validate_receipt_times(receipt.requested_ms, receipt.received_ms)?;
    let unit = match receipt.unit.as_str() {
        "days" => CandleUnit::Daily,
        "minutes" => CandleUnit::Minute,
        _ => return Err("archive_candle_unit_invalid".into()),
    };
    if receipt.start > receipt.end
        || receipt.key.is_empty()
        || history_download::window_end(receipt.start, receipt.end, &receipt.unit)? != receipt.end
    {
        return Err("archive_candle_range_invalid".into());
    }
    let bars = history_download::candles(&receipt)?
        .into_iter()
        .map(|(day, row)| -> Result<CandleBar> {
            let price = |index: usize| positive_paise(nivra_core::execution::number(&row[index])?);
            Ok(CandleBar {
                day,
                bar_ms: DateTime::parse_from_rfc3339(row[0].as_str().ok_or("history_timestamp")?)?
                    .timestamp_millis(),
                o: price(1)?,
                h: price(2)?,
                l: price(3)?,
                c: price(4)?,
                volume: quantity(&row[5])?,
                oi: quantity(&row[6])?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(CandleBatch {
        receipt: Receipt::new(receipt.requested_ms, receipt.received_ms, raw.as_bytes()),
        instrument_key: receipt.key,
        unit,
        start: receipt.start,
        end: receipt.end,
        bars,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quote_receipt_preserves_clock_purpose_and_empty_depth() {
        let mut archive = json!({
            "source": "upstox-readonly-v1",
            "purpose": "scanner_metadata",
            "requested_ms": 1697673120000_i64,
            "received_ms": 1697673120010_i64,
            "body": include_str!("../../../../../fixtures/upstox_full_quote_doc_example.json"),
        });
        let raw = archive.to_string();
        let batch = quotes(&raw).unwrap();
        assert_eq!(batch.purpose, QuotePurpose::ScannerMetadata);
        assert_eq!(batch.receipt.requested_ms, 1697673120000);
        assert_eq!(batch.receipt.received_ms, 1697673120010);
        assert_eq!(batch.receipt.body, raw.as_bytes());
        assert_eq!(
            batch.observations,
            [QuoteObservation {
                instrument_key: "NSE_EQ|INE848E01016".into(),
                source_ms: 1697673111099,
                ltp: 5205,
                bid: Some(5205),
                ask: None,
                bid_qty: Some(6917),
                ask_qty: None,
                volume: 24123697,
                lower_circuit: 4250,
                upper_circuit: 6370,
            }]
        );

        let mut body: Value = serde_json::from_str(archive["body"].as_str().unwrap()).unwrap();
        body["data"]["NSE_EQ:NHPC"]["depth"]["sell"] = json!([{"quantity": 1, "price": 51.90}]);
        archive["body"] = json!(body.to_string());
        let invalid = archive.to_string();
        assert!(quotes(&invalid).is_err());
        let partial = capture_quotes(&invalid).unwrap();
        assert!(partial.observations.is_empty());
        assert_eq!(partial.receipt.body, invalid.as_bytes());
        archive["received_ms"] = json!(0);
        assert!(quotes(&archive.to_string()).is_err());
    }

    #[test]
    fn capture_keeps_future_dated_provider_bytes_without_normalizing_the_observation() {
        let mut body: Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/upstox_full_quote_doc_example.json"
        ))
        .unwrap();
        body["data"]["NSE_EQ:NHPC"]["timestamp"] = json!(1697673120011_i64);
        let raw = json!({
            "source": "upstox-readonly-v1",
            "purpose": "scanner_metadata",
            "requested_ms": 1697673120000_i64,
            "received_ms": 1697673120010_i64,
            "body": body.to_string(),
        })
        .to_string();
        let batch = capture_quotes(&raw).unwrap();
        assert_eq!(batch.receipt.body, raw.as_bytes());
        assert_eq!(batch.observations, []);
    }

    #[test]
    fn candle_receipts_reuse_history_validation() {
        let raw = include_str!("../../../../../fixtures/market-archive-candle-receipt.json");
        let batch = candles(raw).unwrap();
        assert_eq!(batch.instrument_key, "NSE_EQ|INF204KB14I2");
        assert_eq!(batch.unit, CandleUnit::Minute);
        assert_eq!(batch.receipt.body, raw.as_bytes());
        let expected = CandleBar {
            day: "2026-10-16".parse().unwrap(),
            bar_ms: 1792122300000,
            o: 30010,
            h: 30325,
            l: 29995,
            c: 30100,
            volume: 1588,
            oi: 0,
        };
        assert_eq!(batch.bars, std::slice::from_ref(&expected));

        let mut daily: Value = serde_json::from_str(raw).unwrap();
        daily["unit"] = json!("days");
        let body = daily["body"]
            .as_str()
            .unwrap()
            .replace("09:15:00", "00:00:00");
        daily["body"] = json!(body);
        assert!(candles(&daily.to_string()).is_err());
        daily["body_sha256"] = json!(super::super::super::digest(body.as_bytes()));
        let daily_raw = daily.to_string();
        let batch = candles(&daily_raw).unwrap();
        assert_eq!(batch.unit, CandleUnit::Daily);
        assert_eq!(
            batch.bars,
            [CandleBar {
                bar_ms: 1792089000000,
                ..expected
            }]
        );
        daily["end"] = json!("2026-10-15");
        assert!(candles(&daily.to_string()).is_err());
    }
}
