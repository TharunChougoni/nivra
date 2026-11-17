//! Bounded offline research preview using the same engine as quote replay/paper.
//! No files, credentials, providers, journal writes or trading authority.
use crate::{
    finance::Amount,
    historical::{Event, Manifest, Report, Runner},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;
pub const MAX_MANIFEST: usize = 512 * 1024;
pub const MAX_EVENTS: usize = 4 * 1024 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Manifest,
    EngineBinding,
    DataBinding,
    Event,
    Contract,
    Limit,
    Deadline,
    Report,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Manifest => "backtest_manifest_invalid",
            Self::EngineBinding => "backtest_engine_binding_mismatch",
            Self::DataBinding => "backtest_data_binding_mismatch",
            Self::Event => "backtest_event_invalid",
            Self::Contract => "backtest_contract_rejected",
            Self::Limit => "backtest_preview_limit",
            Self::Deadline => "backtest_deadline",
            Self::Report => "backtest_report_unavailable",
        })
    }
}
impl std::error::Error for Error {}
pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn engine_fingerprint() -> String {
    let engine = include_bytes!("execution.rs");
    let adapter = include_bytes!("historical.rs");
    let mut hash = Sha256::new();
    hash.update((engine.len() as u64).to_be_bytes());
    hash.update(engine);
    hash.update((adapter.len() as u64).to_be_bytes());
    hash.update(adapter);
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
#[derive(Debug, Serialize)]
pub struct EquityPoint {
    pub at: i64,
    pub equity: Amount,
    pub cash: Amount,
    pub exposure: Amount,
}
#[derive(Debug, Serialize)]
pub struct Preview {
    pub schema: u8,
    pub manifest_hash: String,
    pub data_hash: String,
    pub engine_hash: String,
    pub universe_size: usize,
    pub equity_curve: Vec<EquityPoint>,
    pub report: Report,
}
/// The caller supplies a deadline/cancellation predicate. No result escapes before
/// exact data/engine binding, chronological validation and reconciliation succeed.
pub fn evaluate(
    manifest: &[u8],
    events: &[u8],
    mut allowed: impl FnMut() -> bool,
) -> Result<Preview, Error> {
    if manifest.len() > MAX_MANIFEST || events.len() > MAX_EVENTS {
        return Err(Error::Limit);
    }
    if !allowed() {
        return Err(Error::Deadline);
    }
    let data_hash = digest(events);
    let engine_hash = engine_fingerprint();
    let header: Manifest =
        serde_json::from_value(crate::native::decode(manifest).map_err(|_| Error::Manifest)?)
            .map_err(|_| Error::Manifest)?;
    if header.bindings.engine != engine_hash {
        return Err(Error::EngineBinding);
    }
    if header.bindings.data != data_hash {
        return Err(Error::DataBinding);
    }
    let universe_size = header.universe.len();
    if universe_size > 500 {
        return Err(Error::Limit);
    }
    let mut runner = Runner::new(header).map_err(|_| Error::Contract)?;
    let mut curve = vec![];
    let mut observations = 0usize;
    for row in events.split_inclusive(|b| *b == b'\n') {
        if !allowed() {
            return Err(Error::Deadline);
        }
        if row.len() > 128 * 1024 || curve.len() >= 5000 {
            return Err(Error::Limit);
        }
        let event: Event =
            serde_json::from_value(crate::native::decode(row).map_err(|_| Error::Event)?)
                .map_err(|_| Error::Event)?;
        observations = observations
            .checked_add(event.observations.len())
            .ok_or(Error::Limit)?;
        if observations > 50000 {
            return Err(Error::Limit);
        }
        let at = event.at;
        runner.apply(event).map_err(|_| Error::Contract)?;
        let p = runner.portfolio();
        curve.push(EquityPoint {
            at,
            equity: p.equity().map_err(|_| Error::Report)?,
            cash: p.cash,
            exposure: p.exposure().map_err(|_| Error::Report)?,
        });
    }
    if !allowed() {
        return Err(Error::Deadline);
    }
    let report = runner.finish().map_err(|_| Error::Report)?;
    Ok(Preview {
        schema: 1,
        manifest_hash: digest(manifest),
        data_hash,
        engine_hash,
        universe_size,
        equity_curve: curve,
        report,
    })
}
/// Explicitly synthetic teaching dataset, never a default source or market evidence.
pub fn example() -> serde_json::Value {
    use serde_json::json;
    let start = chrono::DateTime::parse_from_rfc3339("2026-10-16T09:20:00+05:30")
        .expect("constant")
        .timestamp_millis();
    let mut rows = vec![];
    for n in 0..16 {
        let at = start + n * 30000;
        let bid = 10000 + n * 25;
        rows.push(json!({"at":at,"available_at":at,"kind":"screen","observations":[{"key":"SYNTHETIC|DEMO","quote":{"bid":format!("{}.{:02}",bid/100,bid%100),"ask":format!("{}.{:02}",(bid+5)/100,(bid+5)%100),"bid_units":10000,"ask_units":10000,"source":at,"receipt":at},"lower_circuit_limit":"50","upper_circuit_limit":"200","volume":1000000}]}));
        if n == 15 {
            let mut entry = rows.last().expect("row").clone();
            entry["at"] = json!(at + 1);
            entry["kind"] = json!("entry");
            rows.push(entry);
        }
    }
    let at = start + 355 * 60 * 1000;
    rows.push(json!({"at":at,"available_at":at,"kind":"protection","observations":[{"key":"SYNTHETIC|DEMO","quote":{"bid":"105","ask":"105.05","bid_units":10000,"ask_units":10000,"source":at,"receipt":at},"lower_circuit_limit":"50","upper_circuit_limit":"200","volume":1000000}]}));
    let events = rows.iter().map(|v| format!("{v}\n")).collect::<String>();
    let manifest = json!({"session":"2026-10-16","warmup_start":"2026-10-15","paper_costs":serde_json::from_str::<serde_json::Value>(include_str!("../../../fixtures/paper-costs.json")).unwrap(),"policy":crate::execution::POLICY,"split":"development","receipt_clock":"virtual_historical","seed":1,"bindings":{"engine":engine_fingerprint(),"strategy":digest(b"SYNTHETIC strategy declaration"),"data":digest(events.as_bytes()),"calendar":digest(b"SYNTHETIC calendar declaration"),"fees":digest(b"SYNTHETIC simulation costs"),"slippage":digest(b"SYNTHETIC adverse slippage")},"universe":[{"key":"SYNTHETIC|DEMO","symbol":"DEMO","series":"EQ","tick":"0.05"}]});
    json!({"synthetic":true,"label":"Synthetic demonstration — not market evidence","manifest":manifest,"events":events})
}
