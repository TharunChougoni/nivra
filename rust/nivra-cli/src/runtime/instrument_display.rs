//! Public BOD metadata for authenticated display, isolated from execution admission.
use super::{Config, Result, data, digest, millis};
use nivra_core::finance::Amount;
use serde::{
    Deserialize, Serialize,
    de::{SeqAccess, Visitor},
};
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const COMPLETE: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/complete.json.gz";
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    #[default]
    SessionNse,
    Complete,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub source: Source,
    pub max_source_bytes: u64,
    pub max_decoded_bytes: u64,
    pub max_rows: usize,
    pub max_text_bytes: usize,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            source: Source::SessionNse,
            max_source_bytes: data::HISTORY_RESPONSE_BYTES,
            max_decoded_bytes: 100_000_000,
            max_rows: 300_000,
            max_text_bytes: 256,
        }
    }
}
impl Settings {
    pub(super) fn validate(&self) -> Result<()> {
        if self.max_source_bytes == 0
            || self.max_decoded_bytes == 0
            || self.max_rows == 0
            || self.max_text_bytes == 0
        {
            return Err("instrument_display_limits_invalid".into());
        }
        Ok(())
    }
}

/// The dashboard catalog: the validated cash universe and the display
/// projection the worker publishes for that same session and BOD.
pub(crate) struct Catalog {
    pub(crate) cash: Value,
    /// `None` while loading.
    pub(crate) display: Option<Display>,
}
/// `Err` carries the unavailable reason code.
pub(crate) type Display = std::result::Result<Projection, String>;
impl Catalog {
    pub(crate) fn new(cash: Value) -> Self {
        Self {
            cash,
            display: None,
        }
    }
}

/// Retained for the whole session beside ~86k BOD rows, so each row keeps
/// its projected JSON serialized plus only the fields the dashboard filters,
/// pages and groups on.
pub(crate) struct Projection {
    source: &'static str,
    received_at_ms: i64,
    bod_sha256: String,
    cash_source_hashes: Value,
    segments: BTreeMap<String, usize>,
    kinds: BTreeMap<String, usize>,
    rows_seen: usize,
    rejected_rows: usize,
    pub(crate) rows: Vec<Row>,
}
pub(crate) struct Row {
    pub(crate) instrument_key: Box<str>,
    pub(crate) symbol: Option<Box<str>>,
    pub(crate) segment: Box<str>,
    pub(crate) kind: Option<&'static str>,
    pub(crate) underlying_key: Option<Box<str>>,
    pub(crate) underlying_symbol: Option<Box<str>>,
    pub(crate) expiry_ms: Option<i64>,
    pub(crate) strike: Option<Amount>,
    json: Box<RawValue>,
}
impl Row {
    pub(crate) fn value(&self) -> serde_json::Result<Value> {
        serde_json::from_str(self.json.get())
    }
}
impl Projection {
    pub(crate) fn summary(&self) -> Value {
        json!({"source":self.source,"received_at_ms":self.received_at_ms,"source_hashes":{"bod_sha256":self.bod_sha256},
        "cash_source_hashes":self.cash_source_hashes,"segments":self.segments,"kinds":self.kinds,"rows_seen":self.rows_seen,
        "rejected_rows":self.rejected_rows,"total":self.rows.len(),"display_only":true,"quotes_available":false,
        "margin_available":false,"coverage_kind":"contract_metadata_only"})
    }
}

/// One worker per session. Public metadata I/O never delays a financial transaction.
pub(super) fn start(c: &Config, catalog: &Arc<Mutex<Option<Catalog>>>, started: &AtomicBool) {
    if started.load(Ordering::Acquire) {
        return;
    }
    let cash = match catalog.lock() {
        Ok(value) => value.as_ref().map(|catalog| catalog.cash.clone()),
        Err(_) => None,
    };
    let Some(cash) = cash else { return };
    if started.swap(true, Ordering::AcqRel) {
        return;
    }
    let c = c.clone();
    let catalog = Arc::clone(catalog);
    let worker_catalog = Arc::clone(&catalog);
    let session = c.session.to_string();
    let expected_hash = cash["source_hashes"]["upstox_nse_bod_sha256"].clone();
    let worker_session = session.clone();
    let worker_hash = expected_hash.clone();
    let work = std::thread::Builder::new()
        .name("instrument-display".into())
        .spawn(move || {
            let display = load(&c, &cash).map_err(|error| {
                let code = super::failure_code(error.as_ref());
                if code.starts_with("instrument_display_") {
                    code
                } else {
                    "instrument_display_source_invalid_or_unavailable".into()
                }
            });
            publish(&worker_catalog, &worker_session, &worker_hash, display);
        });
    if work.is_err() {
        publish(
            &catalog,
            &session,
            &expected_hash,
            Err("instrument_display_worker_unavailable".into()),
        );
    }
}
/// Nonblocking summary status; a catalog reader must not stall the paper loop.
pub(super) fn status(catalog: &Arc<Mutex<Option<Catalog>>>) -> Option<Value> {
    let catalog = catalog.try_lock().ok()?;
    Some(match catalog.as_ref().map(|catalog| &catalog.display) {
        None => json!({"status":"not_prepared"}),
        Some(None) => json!({"status":"loading"}),
        Some(Some(Err(_))) => {
            json!({"status":"unavailable","source_sha256":null,"received_at_ms":null})
        }
        Some(Some(Ok(display))) => {
            json!({"status":"ready","source_sha256":display.bod_sha256,"received_at_ms":display.received_at_ms})
        }
    })
}

fn publish(catalog: &Arc<Mutex<Option<Catalog>>>, session: &str, hash: &Value, display: Display) {
    if let Ok(mut catalog) = catalog.lock()
        && let Some(current) = catalog.as_mut()
        && current.cash["session"] == session
        && current.cash["source_hashes"]["upstox_nse_bod_sha256"] == *hash
    {
        current.display = Some(display);
    }
}

fn load(c: &Config, cash: &Value) -> Result<Projection> {
    if cash["session"] != c.session.to_string() {
        return Err("instrument_display_session_mismatch".into());
    }
    let settings = c.dashboard.instrument_catalog.clone().unwrap_or_default();
    settings.validate()?;
    let (bytes, received, hash, source) = match settings.source {
        Source::SessionNse => {
            let bytes = read_source(
                &c.directory.join("upstox-NSE.json.gz"),
                settings.max_source_bytes,
            )?;
            let hash = digest(&bytes);
            if cash["source_hashes"]["upstox_nse_bod_sha256"] != hash {
                return Err("instrument_display_source_hash".into());
            }
            let received = cash["received_at_ms"]
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or("instrument_display_receipt_missing")?;
            (bytes, received, hash, "session_nse_bod")
        }
        Source::Complete => {
            let archive = c.directory.join("upstox-display-complete.json.gz");
            let receipt = c.directory.join("instrument-display-source.json");
            // Never replace an original or incomplete/failed capture.
            if !archive.exists() && !receipt.exists() {
                let client = data::Client::new(&c.market_data_transport)?;
                let bytes = client.bytes(COMPLETE, settings.max_source_bytes)?;
                let received = millis();
                let hash = digest(&bytes);
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&archive)?;
                file.write_all(&bytes)?;
                file.sync_all()?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&receipt)?;
                file.write_all(&serde_json::to_vec(&json!({"source":"upstox_complete_bod","received_at_ms":received,"sha256":hash,"session":c.session}))?)?;
                file.sync_all()?;
                fs::File::open(&c.directory)?.sync_all()?;
            }
            let bytes = read_source(&archive, settings.max_source_bytes)?;
            let receipt: Value = serde_json::from_slice(&read_source(
                &receipt,
                c.dashboard.history_read_max_bytes as u64,
            )?)?;
            let hash = digest(&bytes);
            if receipt["sha256"] != hash
                || receipt["source"] != "upstox_complete_bod"
                || receipt["session"] != c.session.to_string()
            {
                return Err("instrument_display_source_hash".into());
            }
            let received = receipt["received_at_ms"]
                .as_i64()
                .filter(|n| *n > 0)
                .ok_or("instrument_display_receipt_missing")?;
            (bytes, received, hash, "upstox_complete_bod")
        }
    };
    project(&bytes, &settings, cash, received, &hash, source)
}

fn read_source(path: &std::path::Path, max: u64) -> Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "instrument_display_source_unreadable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "instrument_display_source_unreadable")?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > max {
        return Err("instrument_display_source_invalid".into());
    }
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err("instrument_display_source_limit".into());
    }
    Ok(bytes)
}

fn text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && !s.chars().any(char::is_control))
}
fn decimal(value: &Value) -> Option<String> {
    if !value.is_number() {
        return None;
    }
    let value = Amount::parse(&value.to_string()).ok()?;
    (value >= Amount::ZERO).then(|| value.to_string())
}
fn item(row: &Value, cash: &BTreeMap<String, &Value>, settings: &Settings) -> Option<Row> {
    for field in [
        "instrument_key",
        "segment",
        "exchange",
        "trading_symbol",
        "instrument_type",
        "underlying_key",
        "underlying_symbol",
    ] {
        if row[field]
            .as_str()
            .is_some_and(|s| s.len() > settings.max_text_bytes)
        {
            return None;
        }
    }
    let key = text(&row["instrument_key"])?;
    let segment = text(&row["segment"])?;
    let (prefix, token) = key.split_once('|')?;
    if prefix != segment || token.is_empty() {
        return None;
    }
    let verified = cash.get(key).filter(|v| {
        row["instrument_type"] == "EQ"
            && row["segment"] == "NSE_EQ"
            && row["exchange"] == "NSE"
            && v["symbol"] == row["trading_symbol"]
            && row["lot_size"].as_u64() == Some(1)
            // Only the existing NSE cash verifier qualifies paise-to-INR units.
            && decimal(&row["tick_size"]).and_then(|n| Amount::parse(&n).ok())
                .and_then(|n| n.checked_div(Amount::from_integer(100)).ok())
                .zip(v["tick"].as_str().and_then(|n| Amount::parse(n).ok()))
                .is_some_and(|(native,verified)| native == verified)
    });
    let kind = match row["instrument_type"].as_str() {
        Some("FUT") => Some("future"),
        Some("CE") => Some("call"),
        Some("PE") => Some("put"),
        Some("ETF") => Some("etf"),
        Some("INDEX") => Some("index"),
        Some("EQ") if verified.is_some() => Some("cash"),
        Some("EQ") => Some("equity"),
        _ => None,
    };
    let symbol = text(&row["trading_symbol"]);
    let underlying_key = text(&row["underlying_key"]);
    let underlying_symbol = text(&row["underlying_symbol"]);
    let expiry_ms = row["expiry"].as_i64().filter(|n| *n > 0);
    let strike = decimal(&row["strike_price"]);
    let json = serde_json::value::to_raw_value(&json!({
        "instrument_key":key,"symbol":symbol,"segment":segment,"exchange":text(&row["exchange"]),
        "kind":kind,"instrument_type":text(&row["instrument_type"]),"underlying_key":underlying_key,
        "underlying_symbol":underlying_symbol,"expiry_ms":expiry_ms,
        "strike":strike,"lot_size":row["lot_size"].as_u64().filter(|n|*n>0).map(|n| n.to_string()),
        "provider_tick":decimal(&row["tick_size"]),"tick":verified.map(|v|v["tick"].clone()),
        "tick_unit":verified.map(|v|v["tick_unit"].clone()),"cash_execution_verified":verified.is_some(),
        "margin":null,"margin_reason":"margin_snapshot_not_captured","display_only":true
    }))
    .ok()?;
    Some(Row {
        instrument_key: key.into(),
        symbol: symbol.map(Into::into),
        segment: segment.into(),
        kind,
        underlying_key: underlying_key.map(Into::into),
        underlying_symbol: underlying_symbol.map(Into::into),
        expiry_ms,
        strike: strike.and_then(|n| Amount::parse(&n).ok()),
        json,
    })
}

pub(super) fn project(
    bytes: &[u8],
    settings: &Settings,
    cash: &Value,
    received: i64,
    hash: &str,
    source: &'static str,
) -> Result<Projection> {
    settings.validate()?;
    struct Rows<'a> {
        settings: &'a Settings,
        cash: BTreeMap<String, &'a Value>,
    }
    impl<'de> Visitor<'de> for Rows<'_> {
        type Value = (Vec<Row>, usize, usize);
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("BOD instrument array")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut rows = BTreeMap::new();
            let mut duplicate = BTreeSet::new();
            let mut seen = 0;
            let mut rejected = 0;
            while let Some(raw) = seq.next_element::<Value>()? {
                seen += 1;
                if seen > self.settings.max_rows {
                    return Err(serde::de::Error::custom("instrument_display_row_limit"));
                }
                let Some(row) = item(&raw, &self.cash, self.settings) else {
                    rejected += 1;
                    continue;
                };
                let key = row.instrument_key.clone();
                if duplicate.contains(&key) {
                    rejected += 1;
                    continue;
                }
                if rows.remove(&key).is_some() {
                    duplicate.insert(key);
                    rejected += 2;
                } else {
                    rows.insert(key, row);
                }
            }
            Ok((rows.into_values().collect(), seen, rejected))
        }
    }
    if digest(bytes) != hash {
        return Err("instrument_display_source_hash".into());
    }
    if bytes.len() as u64 > settings.max_source_bytes {
        return Err("instrument_display_source_limit".into());
    }
    let cash_source_hashes = cash["source_hashes"].clone();
    let cash = cash["items"]
        .as_array()
        .ok_or("instrument_display_cash_catalog_invalid")?
        .iter()
        .filter_map(|row| Some((row["instrument_key"].as_str()?.to_owned(), row)))
        .collect();
    let decoder =
        flate2::read::MultiGzDecoder::new(bytes).take(settings.max_decoded_bytes.saturating_add(1));
    // A bounded decoded buffer makes limit exhaustion explicit, not a truncated successful parse.
    let mut decoded = Vec::new();
    decoder
        .take(settings.max_decoded_bytes.saturating_add(1))
        .read_to_end(&mut decoded)
        .map_err(|_| "instrument_display_gzip_invalid")?;
    if decoded.len() as u64 > settings.max_decoded_bytes {
        return Err("instrument_display_decoded_limit".into());
    }
    let mut de = serde_json::Deserializer::from_slice(&decoded);
    let (rows, seen, rejected) =
        serde::de::Deserializer::deserialize_seq(&mut de, Rows { settings, cash }).map_err(
            |error| {
                if error
                    .to_string()
                    .starts_with("instrument_display_row_limit")
                {
                    "instrument_display_row_limit"
                } else {
                    "instrument_display_json_invalid"
                }
            },
        )?;
    de.end().map_err(|_| "instrument_display_json_invalid")?;
    let mut segments = BTreeMap::<String, usize>::new();
    let mut kinds = BTreeMap::<String, usize>::new();
    for row in &rows {
        *segments.entry(row.segment.to_string()).or_default() += 1;
        *kinds
            .entry(row.kind.unwrap_or("unknown").to_owned())
            .or_default() += 1;
    }
    Ok(Projection {
        source,
        received_at_ms: received,
        bod_sha256: hash.to_owned(),
        cash_source_hashes,
        segments,
        kinds,
        rows_seen: seen,
        rejected_rows: rejected,
        rows,
    })
}

#[cfg(test)]
pub(super) fn storage_fixture(bytes: &[u8], cash: &Value) {
    let dir = tempfile::tempdir().unwrap();
    let mut c = super::tests::config(dir.path());
    c.session =
        chrono::NaiveDate::parse_from_str(cash["session"].as_str().unwrap(), "%Y-%m-%d").unwrap();
    c.dashboard.instrument_catalog = Some(Settings {
        source: Source::Complete,
        ..Default::default()
    });
    let archive = dir.path().join("upstox-display-complete.json.gz");
    let receipt = dir.path().join("instrument-display-source.json");
    fs::write(&archive, bytes).unwrap();
    let identity = json!({"source":"upstox_complete_bod","received_at_ms":123456,"sha256":digest(bytes),"session":c.session});
    fs::write(&receipt, serde_json::to_vec(&identity).unwrap()).unwrap();
    let restored = load(&c, cash).unwrap();
    assert_eq!(restored.received_at_ms, 123456);
    let mut changed = identity;
    changed["sha256"] = json!("mismatch");
    fs::write(&receipt, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(load(&c, cash).is_err());
    assert_eq!(fs::read(&archive).unwrap(), bytes);
    fs::remove_file(&receipt).unwrap();
    assert!(load(&c, cash).is_err()); // incomplete capture is preserved, never downloaded over
    assert_eq!(fs::read(&archive).unwrap(), bytes);
    let target = dir.path().join("original.gz");
    fs::rename(&archive, &target).unwrap();
    std::os::unix::fs::symlink(&target, &archive).unwrap();
    assert!(read_source(&archive, Settings::default().max_source_bytes).is_err());
    assert_eq!(fs::read(&target).unwrap(), bytes);
    let catalog = Arc::new(Mutex::new(Some(Catalog::new(cash.clone()))));
    publish(
        &catalog,
        "wrong-session",
        &cash["source_hashes"]["upstox_nse_bod_sha256"],
        Err("instrument_display_wrong_session".into()),
    );
    assert_eq!(status(&catalog).unwrap()["status"], "loading");
    {
        let _held = catalog.lock().unwrap();
        assert!(status(&catalog).is_none());
    }
    publish(
        &catalog,
        c.session.to_string().as_str(),
        &cash["source_hashes"]["upstox_nse_bod_sha256"],
        Ok(restored),
    );
    let ready = status(&catalog).unwrap();
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["source_sha256"], digest(bytes));
}
