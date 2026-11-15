//! Bounded Nifty weekly option universe from the contract API plus a BOD join.
//!
//! This is deliberately not `wide::universe` and never streams the 41,220-row
//! NSE_FO catalog into the cash builder: the contract API already names the
//! current-week chain, the strike window bounds it to roughly 40 strikes x
//! CE/PE, and the BOD is streamed only to join the kept keys' lot, tick,
//! freeze and weekly fields. Every joined identity is cross-checked; a
//! disagreement refuses the row instead of inventing contract terms.
use super::*;
use sadr_core::finance::{Amount, Price};
use sadr_core::market::{OptionCatalog, OptionContract, Segment};
use sadr_core::time::ExchangeDate;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const CONTRACT_URL: &str = "https://api.upstox.com/v2/option/contract";
const BOD_URL: &str = "https://assets.upstox.com/market-quote/instruments/exchange/NSE.json.gz";
const MAX_CONTRACT_BYTES: u64 = 2_000_000;
const MAX_BOD_BYTES: u64 = 10_000_000;
const MAX_BOD_DECODED: u64 = 100_000_000;
const MAX_CONTRACT_ROWS: usize = 5_000;
const MAX_BOD_ROWS: usize = 300_000;
/// Fresh spot observation read from the existing quote map. Screening price
/// only; it selects the strike window and never authorizes a fill.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpotQuote {
    pub key: String,
    pub price: Price,
    pub source_at: i64,
    pub received_at: i64,
}

impl SpotQuote {
    /// The original receipt is the financial freshness clock. The exchange
    /// source stamp must not be later than that receipt; a small negative
    /// source/receipt skew is the feed's own tolerated clock skew, not a
    /// reason to invent a price.
    fn fresh_at(&self, now: i64) -> bool {
        self.source_at <= self.received_at
            && self.received_at <= now
            && now
                .checked_sub(self.received_at)
                .is_some_and(|age| (0..=sadr_core::market::MAX_FINANCIAL_AGE_MS).contains(&age))
    }
}

/// One session's bounded option universe. Runtime-only; the cash book never
/// stores these rows.
#[derive(Clone, Debug)]
pub(crate) struct OptionUniverse {
    pub session: ExchangeDate,
    pub underlying_key: String,
    pub underlying_symbol: String,
    pub spot: Price,
    pub spot_source_at: i64,
    pub spot_received_at: i64,
    pub expiry: ExchangeDate,
    pub weekly: bool,
    pub contracts: Vec<OptionContract>,
    pub received_ms: i64,
    pub contract_sha256: String,
    pub bod_sha256: String,
    pub exclusions: BTreeMap<String, usize>,
}

impl OptionUniverse {
    #[cfg(test)]
    pub(crate) fn contract(&self, key: &str) -> Option<&OptionContract> {
        self.contracts.iter().find(|contract| contract.key == key)
    }

    /// Runtime catalog used by admission, the planner and the fill path.
    pub(crate) fn catalog(&self) -> Result<OptionCatalog> {
        Ok(OptionCatalog::try_new(
            self.session,
            self.underlying_key.clone(),
            self.underlying_symbol.clone(),
            self.expiry,
            self.spot,
            self.contracts.clone(),
        )?)
    }

    /// Bounded quote-subscription and REST rows for the option shortlist.
    pub(crate) fn instruments(&self) -> Result<Vec<sadr_core::execution::Instrument>> {
        self.contracts
            .iter()
            .map(|contract| {
                Ok(sadr_core::execution::Instrument {
                    key: contract.key.clone(),
                    symbol: contract.symbol.clone(),
                    series: match contract.option {
                        sadr_core::market::InstrumentKind::Call => "CE".to_owned(),
                        sadr_core::market::InstrumentKind::Put => "PE".to_owned(),
                        _ => return Err("option_kind_invalid".into()),
                    },
                    tick: contract.tick,
                })
            })
            .collect()
    }

    pub(crate) fn dashboard_projection(&self) -> serde_json::Value {
        let items = self
            .contracts
            .iter()
            .map(|contract| {
                serde_json::json!({
                    "instrument_key":contract.key,
                    "symbol":contract.symbol,
                    "option":match contract.option {
                        sadr_core::market::InstrumentKind::Call => "call",
                        sadr_core::market::InstrumentKind::Put => "put",
                        _ => "unknown",
                    },
                    "underlying_key":contract.underlying_key,
                    "underlying_symbol":contract.underlying_symbol,
                    "expiry":contract.expiry,
                    "strike":contract.strike,
                    "lot_size":contract.lot_size,
                    "tick":contract.tick,
                    "freeze_quantity":contract.freeze_quantity,
                    "weekly":contract.weekly,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "status":"ready",
            "source":"bounded_nifty_option_universe",
            "paper_only":true,
            "underlying_key":self.underlying_key,
            "underlying_symbol":self.underlying_symbol,
            "spot":self.spot,
            "spot_source_ms":self.spot_source_at,
            "spot_receipt_ms":self.spot_received_at,
            "expiry":self.expiry,
            "weekly":self.weekly,
            "session":self.session,
            "received_at_ms":self.received_ms,
            "source_hashes":{
                "option_contract_sha256":self.contract_sha256,
                "upstox_nse_bod_sha256":self.bod_sha256
            },
            "total":items.len(),
            "exclusions":self.exclusions,
            "cash_screens_applied":false,
            "items":items
        })
    }
}

fn number(value: &serde_json::Value) -> Option<Amount> {
    value
        .is_number()
        .then(|| Amount::parse(&value.to_string()).ok())
        .flatten()
}

fn text(value: &serde_json::Value) -> Option<&str> {
    value
        .as_str()
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
}

fn instrument_kind(value: &str) -> Option<sadr_core::market::InstrumentKind> {
    match value {
        "CE" => Some(sadr_core::market::InstrumentKind::Call),
        "PE" => Some(sadr_core::market::InstrumentKind::Put),
        _ => None,
    }
}

fn bod_expiry_date(value: &serde_json::Value) -> Option<ExchangeDate> {
    let millis = value.as_i64().filter(|millis| *millis > 0)?;
    ExchangeDate::parse(&local(millis).date_naive().to_string()).ok()
}

/// One joined contract row from the contract API payload.
struct ContractRow {
    key: String,
    symbol: String,
    expiry: ExchangeDate,
    strike: Price,
    option: sadr_core::market::InstrumentKind,
    lot_size: u64,
    tick_paise: Amount,
    freeze_quantity: u64,
    minimum_lot: Option<u64>,
    weekly: bool,
}

fn parse_contract_row(
    row: &serde_json::Value,
    settings: &NiftyOptionSettings,
    exclusions: &mut BTreeMap<String, usize>,
) -> Option<ContractRow> {
    let refuse = |exclusions: &mut BTreeMap<String, usize>, reason: &str| -> Option<ContractRow> {
        *exclusions.entry(reason.to_owned()).or_default() += 1;
        None
    };
    if row["segment"] != "NSE_FO" || row["exchange"] != "NSE" {
        return refuse(exclusions, "not_nse_fo");
    }
    let Some(option) = row["instrument_type"].as_str().and_then(instrument_kind) else {
        return refuse(exclusions, "not_call_or_put");
    };
    if row["underlying_key"].as_str() != Some(settings.underlying_key.as_str())
        || row["underlying_symbol"].as_str() != Some(settings.underlying_symbol.as_str())
    {
        return refuse(exclusions, "other_underlying");
    }
    let Some(key) = text(&row["instrument_key"]) else {
        return refuse(exclusions, "missing_identity");
    };
    if Segment::from_key(key) != Some(Segment::NseFo) || key.len() > 256 {
        return refuse(exclusions, "missing_identity");
    }
    let Some(symbol) = text(&row["trading_symbol"]) else {
        return refuse(exclusions, "missing_identity");
    };
    let Some(expiry) = row["expiry"]
        .as_str()
        .and_then(|text| ExchangeDate::parse(text).ok())
    else {
        return refuse(exclusions, "missing_expiry");
    };
    let Some(strike) = number(&row["strike_price"]).and_then(|value| Price::try_from(value).ok())
    else {
        return refuse(exclusions, "invalid_strike");
    };
    let Some(lot_size) = row["lot_size"].as_u64().filter(|lot| *lot > 0) else {
        return refuse(exclusions, "invalid_lot");
    };
    let Some(tick_paise) = number(&row["tick_size"]).filter(|tick| *tick > Amount::ZERO) else {
        return refuse(exclusions, "invalid_tick");
    };
    let Some(freeze_quantity) = row["freeze_quantity"].as_u64().filter(|freeze| *freeze > 0) else {
        return refuse(exclusions, "missing_freeze_quantity");
    };
    let minimum_lot = row["minimum_lot"].as_u64();
    if minimum_lot.is_some_and(|minimum| minimum != lot_size) {
        return refuse(exclusions, "minimum_lot_mismatch");
    }
    if row["weekly"] != true {
        return refuse(exclusions, "not_weekly");
    }
    Some(ContractRow {
        key: key.to_owned(),
        symbol: symbol.to_owned(),
        expiry,
        strike,
        option,
        lot_size,
        tick_paise,
        freeze_quantity,
        minimum_lot,
        weekly: true,
    })
}

/// Strike window around spot: at most `strike_window` strikes on each side.
/// Ties choose the lower strike first so the result is deterministic.
fn windowed_strikes(mut strikes: Vec<Price>, spot: Price, window: u32) -> Vec<Price> {
    strikes.sort();
    strikes.dedup();
    if strikes.is_empty() {
        return strikes;
    }
    let distance = |strike: Price| -> Amount {
        if strike >= spot {
            strike.amount().checked_sub(spot.amount())
        } else {
            spot.amount().checked_sub(strike.amount())
        }
        .unwrap_or(Amount::ZERO)
    };
    let nearest = strikes
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| distance(**a).cmp(&distance(**b)).then_with(|| a.cmp(b)))
        .map(|(index, _)| index)
        .unwrap_or(0);
    let window = window as usize;
    let start = nearest.saturating_sub(window);
    let end = (nearest + window + 1).min(strikes.len());
    strikes[start..end].to_vec()
}

/// Stream the BOD array and retain only the kept option keys. This is a
/// bounded join, not a catalog scan: rows outside the shortlist are dropped as
/// they are decoded and the whole file is never materialized as a Value tree.
fn join_bod(
    bod: &[u8],
    kept: &BTreeMap<String, ContractRow>,
) -> Result<BTreeMap<String, serde_json::Value>> {
    struct Rows<'a> {
        kept: &'a BTreeMap<String, ContractRow>,
        rows: BTreeMap<String, serde_json::Value>,
        seen: usize,
    }
    impl<'de> serde::de::Visitor<'de> for Rows<'_> {
        type Value = BTreeMap<String, serde_json::Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("BOD instrument array")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            mut self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            while let Some(row) = seq.next_element::<serde_json::Value>()? {
                self.seen += 1;
                if self.seen > MAX_BOD_ROWS {
                    return Err(serde::de::Error::custom("option_bod_rows"));
                }
                let Some(key) = row["instrument_key"].as_str() else {
                    continue;
                };
                if self.kept.contains_key(key) {
                    self.rows.insert(key.to_owned(), row);
                }
            }
            Ok(self.rows)
        }
    }
    // Stream the decoded array: the whole catalog is never materialized as a
    // Value tree, and only the bounded shortlist keys are retained.
    let decoder = flate2::read::MultiGzDecoder::new(bod).take(MAX_BOD_DECODED.saturating_add(1));
    let mut de = serde_json::Deserializer::from_reader(decoder);
    let rows = serde::de::Deserializer::deserialize_seq(
        &mut de,
        Rows {
            kept,
            rows: BTreeMap::new(),
            seen: 0,
        },
    )
    .map_err(|_| "option_bod_json_invalid")?;
    de.end().map_err(|_| "option_bod_json_invalid")?;
    Ok(rows)
}

fn join_contract(
    row: &ContractRow,
    bod: Option<&serde_json::Value>,
    settings: &NiftyOptionSettings,
    exclusions: &mut BTreeMap<String, usize>,
) -> Option<OptionContract> {
    let refuse =
        |exclusions: &mut BTreeMap<String, usize>, reason: &str| -> Option<OptionContract> {
            *exclusions.entry(reason.to_owned()).or_default() += 1;
            None
        };
    let Some(bod) = bod else {
        return refuse(exclusions, "bod_missing");
    };
    if bod["segment"] != "NSE_FO"
        || bod["exchange"] != "NSE"
        || bod["instrument_type"].as_str().and_then(instrument_kind) != Some(row.option)
        || bod["underlying_key"].as_str() != Some(settings.underlying_key.as_str())
        || bod["underlying_symbol"].as_str() != Some(settings.underlying_symbol.as_str())
        || bod_expiry_date(&bod["expiry"]) != Some(row.expiry)
        || number(&bod["strike_price"]).and_then(|value| Price::try_from(value).ok())
            != Some(row.strike)
        || bod["lot_size"].as_u64() != Some(row.lot_size)
        || number(&bod["tick_size"]) != Some(row.tick_paise)
        || bod["freeze_quantity"].as_u64() != Some(row.freeze_quantity)
        || bod["weekly"] != true
        || bod["minimum_lot"].as_u64() != row.minimum_lot
    {
        return refuse(exclusions, "bod_contract_mismatch");
    }
    if let Some(expected) = settings.expected_lot_size
        && row.lot_size != expected
    {
        return refuse(exclusions, "expected_lot_size_mismatch");
    }
    // Provider masters store tick sizes in paise; convert once to INR.
    let tick = row
        .tick_paise
        .checked_mul(Amount::parse("0.01").expect("fixed decimal"))
        .ok()
        .and_then(|value| Price::try_from(value).ok())?;
    let contract = OptionContract {
        key: row.key.clone(),
        symbol: row.symbol.clone(),
        underlying_key: settings.underlying_key.clone(),
        underlying_symbol: settings.underlying_symbol.clone(),
        expiry: row.expiry,
        strike: row.strike,
        option: row.option,
        lot_size: row.lot_size,
        tick,
        freeze_quantity: Some(row.freeze_quantity),
        weekly: row.weekly,
    };
    contract.validate().ok()?;
    Some(contract)
}

/// Build the bounded universe from fixture-injectable payloads. Pure: no
/// sockets, no clock reads.
pub(crate) fn build(
    c: &Config,
    spot: SpotQuote,
    contract_body: &str,
    bod: &[u8],
    now: i64,
) -> Result<OptionUniverse> {
    let settings = &c.nifty_options;
    if !settings.enabled {
        return Err("nifty_options_disabled".into());
    }
    settings.validate()?;
    if spot.key != settings.underlying_key {
        return Err("option_spot_identity".into());
    }
    if !spot.fresh_at(now) {
        return Err("option_spot_stale".into());
    }
    if contract_body.len() as u64 > MAX_CONTRACT_BYTES {
        return Err("option_contract_size".into());
    }
    if bod.len() as u64 > MAX_BOD_BYTES {
        return Err("option_bod_size".into());
    }
    let session = ExchangeDate::parse(&c.session.to_string())?;
    let body: serde_json::Value =
        serde_json::from_str(contract_body).map_err(|_| "option_contract_json_invalid")?;
    if body["status"] != "success" {
        return Err("option_contract_status".into());
    }
    let rows = body["data"]
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= MAX_CONTRACT_ROWS)
        .ok_or("option_contract_rows")?;
    let mut exclusions = BTreeMap::<String, usize>::new();
    let mut parsed = Vec::new();
    let mut expiries = BTreeSet::new();
    for row in rows {
        if let Some(row) = parse_contract_row(row, settings, &mut exclusions) {
            expiries.insert(row.expiry);
            parsed.push(row);
        }
    }
    if parsed.is_empty() {
        return Err("option_contracts_empty".into());
    }
    if expiries.len() != 1 {
        return Err("option_expiry_ambiguous".into());
    }
    let expiry = *expiries.iter().next().expect("one expiry");
    if expiry < session {
        return Err("option_expiry_past".into());
    }
    // Bound the chain before joining the BOD: roughly 40 strikes x CE/PE.
    let strikes = windowed_strikes(
        parsed.iter().map(|row| row.strike).collect(),
        spot.price,
        settings.strike_window,
    );
    let window = strikes.iter().copied().collect::<BTreeSet<_>>();
    let mut kept = BTreeMap::new();
    for row in parsed {
        if window.contains(&row.strike) {
            kept.insert(row.key.clone(), row);
        }
    }
    if kept.is_empty() {
        return Err("option_universe_empty".into());
    }
    let joined = join_bod(bod, &kept)?;
    let mut contracts = Vec::new();
    for (key, row) in &kept {
        if let Some(contract) = join_contract(row, joined.get(key), settings, &mut exclusions) {
            contracts.push(contract);
        }
    }
    if contracts.is_empty() {
        return Err(if exclusions.contains_key("expected_lot_size_mismatch") {
            "option_lot_size_mismatch".into()
        } else {
            "option_universe_empty".into()
        });
    }
    contracts.sort_by(|a, b| a.strike.cmp(&b.strike).then_with(|| a.key.cmp(&b.key)));
    Ok(OptionUniverse {
        session,
        underlying_key: settings.underlying_key.clone(),
        underlying_symbol: settings.underlying_symbol.clone(),
        spot: spot.price,
        spot_source_at: spot.source_at,
        spot_received_at: spot.received_at,
        expiry,
        weekly: true,
        contracts,
        received_ms: now,
        contract_sha256: digest(contract_body.as_bytes()),
        bod_sha256: digest(bod),
        exclusions,
    })
}

/// Read-only preparation: contract API for the current week plus the session
/// BOD join. Callers fail closed on any error; nothing is persisted here.
pub(crate) fn prepare(
    client: &data::Client,
    c: &Config,
    spot: SpotQuote,
    now: i64,
) -> Result<OptionUniverse> {
    if !c.nifty_options.enabled {
        return Err("nifty_options_disabled".into());
    }
    let token = data::application_token(c)?;
    // The authenticated contract read shares the existing market-read journal.
    data::reserve(c, millis())?;
    let url = format!(
        "{}?instrument_key={}&expiry_date=current_week",
        CONTRACT_URL,
        data::encode_key_path(&c.nifty_options.underlying_key)
    );
    let contract_body = client.get(&url, Some(&token), MAX_CONTRACT_BYTES)?;
    let bod = match fs::read(c.directory.join("upstox-NSE.json.gz")) {
        Ok(bytes) if bytes.len() as u64 <= MAX_BOD_BYTES => bytes,
        _ => client.bytes(BOD_URL, MAX_BOD_BYTES)?,
    };
    build(c, spot, &contract_body, &bod, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zip(text: &str) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    fn contract_row(
        strike: u64,
        kind: &str,
        lot: u64,
        tick_paise: u64,
        weekly: bool,
    ) -> serde_json::Value {
        let key = format!("NSE_FO|{strike}{kind}");
        serde_json::json!({
            "name":"NIFTY","segment":"NSE_FO","exchange":"NSE","expiry":"2026-10-20",
            "instrument_key":key,"exchange_token":format!("{strike}{kind}"),
            "trading_symbol":format!("NIFTY {strike} {kind} 20 OCT 26"),
            "tick_size":tick_paise,"lot_size":lot,"instrument_type":kind,
            "freeze_quantity":1800,"underlying_key":"NSE_INDEX|Nifty 50",
            "underlying_type":"INDEX","underlying_symbol":"NIFTY",
            "strike_price":strike,"minimum_lot":lot,"weekly":weekly
        })
    }

    fn bod_row(strike: u64, kind: &str, lot: u64, tick_paise: u64) -> serde_json::Value {
        let expiry_ms = local(1_792_454_400_000).timestamp_millis();
        let _ = expiry_ms;
        serde_json::json!({
            "segment":"NSE_FO","name":"NIFTY","exchange":"NSE",
            "expiry":1_792_454_400_000_i64,"instrument_type":kind,
            "underlying_symbol":"NIFTY","instrument_key":format!("NSE_FO|{strike}{kind}"),
            "lot_size":lot,"freeze_quantity":1800,"exchange_token":format!("{strike}{kind}"),
            "minimum_lot":lot,"underlying_key":"NSE_INDEX|Nifty 50","tick_size":tick_paise,
            "underlying_type":"INDEX","trading_symbol":format!("NIFTY {strike} {kind} 20 OCT 26"),
            "strike_price":strike,"weekly":true
        })
    }

    fn config() -> Config {
        let dir = tempfile::tempdir().unwrap();
        let mut c = super::super::super::tests::config(dir.path());
        c.session = "2026-10-16".parse().unwrap();
        c.nifty_options.enabled = true;
        c.nifty_options.expected_lot_size = Some(65);
        c.paper_costs_by_segment.insert(
            Segment::NseFo,
            serde_json::from_value(serde_json::json!({
                "per_order":"0.01","buy_bps":"0","sell_bps":"0",
                "slippage_bps":"10","rounding_increment":"0.05"
            }))
            .unwrap(),
        );
        c.quote_keys.prefixes.push("NSE_FO|".into());
        c
    }

    fn spot(now: i64) -> SpotQuote {
        SpotQuote {
            key: "NSE_INDEX|Nifty 50".into(),
            price: Price::parse("26000").unwrap(),
            source_at: now,
            received_at: now,
        }
    }

    #[test]
    fn bounded_current_week_universe_joins_bod_and_refuses_mismatch() {
        let now = 1_792_122_300_000_i64;
        let c = config();
        let fixture_bod: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/options/nse-bod-options.json"
        ))
        .unwrap();
        let fixture_bod = zip(&fixture_bod.to_string());
        // Fixture payloads: the bounded builder joins the BOD and refuses a
        // future, another underlying and a past expiry instead of inventing
        // membership.
        let universe = build(
            &c,
            spot(now),
            include_str!("../../../../../fixtures/options/contract-current-week.json"),
            &fixture_bod,
            now,
        )
        .unwrap();
        assert_eq!(universe.expiry, ExchangeDate::parse("2026-10-20").unwrap());
        assert_eq!(universe.spot, Price::parse("26000").unwrap());
        assert_eq!(universe.contracts.len(), 10);
        assert_eq!(universe.exclusions["not_call_or_put"], 1);
        assert_eq!(universe.exclusions["other_underlying"], 1);
        assert!(
            universe
                .contracts
                .iter()
                .all(|contract| contract.lot_size == 65
                    && contract.tick == Price::parse("0.05").unwrap()
                    && contract.freeze_quantity == Some(1800)
                    && contract.weekly)
        );
        assert!(universe.contract("NSE_FO|26000CE").is_some());
        let projection = universe.dashboard_projection();
        assert_eq!(projection["source"], "bounded_nifty_option_universe");
        assert_eq!(projection["cash_screens_applied"], false);
        assert_eq!(projection["total"], 10);
        assert_eq!(projection["items"][0]["lot_size"], 65);
        let instruments = universe.instruments().unwrap();
        assert_eq!(instruments.len(), 10);
        assert_eq!(instruments[0].series, "CE");
        let catalog = universe.catalog().unwrap();
        assert_eq!(catalog.contracts.len(), 10);
        assert_eq!(catalog.underlying_key, "NSE_INDEX|Nifty 50");
        let expired: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../fixtures/options/expired-contracts.json"
        ))
        .unwrap();
        assert_eq!(
            build(&c, spot(now), &expired.to_string(), &fixture_bod, now)
                .unwrap_err()
                .to_string(),
            "option_expiry_past"
        );

        // 50 strikes x CE/PE on a 50-point grid; the window keeps 41 strikes
        // around the nearest strike to spot (~40 strikes x CE/PE, never a
        // full chain).
        let mut rows = Vec::new();
        let mut bod = Vec::new();
        for step in 0..50u64 {
            let strike = 25_000 + step * 50;
            for kind in ["CE", "PE"] {
                rows.push(contract_row(strike, kind, 65, 5, true));
                bod.push(bod_row(strike, kind, 65, 5));
            }
        }
        let body = serde_json::json!({"status":"success","data":rows}).to_string();
        let bod_gz = zip(&serde_json::Value::Array(bod.clone()).to_string());
        let universe = build(&c, spot(now), &body, &bod_gz, now).unwrap();
        assert_eq!(universe.contracts.len(), 41 * 2);
        assert!(universe.contract("NSE_FO|26000CE").is_some());
        assert_eq!(universe.dashboard_projection()["total"], 82);

        // A lot disagreement against the expected lot refuses every row.
        let mut wrong = config();
        wrong.nifty_options.expected_lot_size = Some(75);
        assert_eq!(
            build(&wrong, spot(now), &body, &bod_gz, now)
                .unwrap_err()
                .to_string(),
            "option_lot_size_mismatch"
        );
        // BOD lot disagreement is a row refusal, not a silent overwrite.
        let mut bod_mismatch = bod.clone();
        bod_mismatch[0]["lot_size"] = serde_json::json!(75);
        let universe = build(
            &c,
            spot(now),
            &body,
            &zip(&serde_json::Value::Array(bod_mismatch).to_string()),
            now,
        )
        .unwrap();
        assert_eq!(universe.contracts.len(), 81);
        assert_eq!(universe.exclusions["bod_contract_mismatch"], 1);
        // Non-weekly, other-underlying and future rows never enter the universe.
        let mut mixed = Vec::new();
        mixed.push(contract_row(26_000, "CE", 65, 5, false));
        let mut other = contract_row(26_000, "PE", 65, 5, true);
        other["underlying_key"] = serde_json::json!("NSE_INDEX|Nifty Bank");
        mixed.push(other);
        let mut future = contract_row(26_000, "CE", 65, 5, true);
        future["instrument_type"] = serde_json::json!("FUT");
        mixed.push(future);
        let body = serde_json::json!({"status":"success","data":mixed}).to_string();
        assert_eq!(
            build(
                &c,
                spot(now),
                &body,
                &zip(&serde_json::Value::Array(bod).to_string()),
                now
            )
            .unwrap_err()
            .to_string(),
            "option_contracts_empty"
        );
        // A stale spot fails closed with an explicit reason.
        let stale = SpotQuote {
            received_at: now - 2_001,
            ..spot(now)
        };
        assert_eq!(
            build(&c, stale, &serde_json::json!({"status":"success","data":[contract_row(26_000,"CE",65,5,true)]}).to_string(), &zip("[]"), now)
                .unwrap_err()
                .to_string(),
            "option_spot_stale"
        );
    }
}
