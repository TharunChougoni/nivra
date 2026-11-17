use super::*;
use serde::de::{SeqAccess, Visitor};
const BOD: &str = "https://assets.upstox.com/market-quote/instruments/exchange/NSE.json.gz";
const SUSPENDED: &str =
    "https://assets.upstox.com/market-quote/instruments/exchange/suspended-instrument.json.gz";
pub(super) use nivra_core::execution::Instrument;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Universe {
    pub instruments: Vec<Instrument>,
    pub exclusions: BTreeMap<String, usize>,
    file_date: NaiveDate,
    session: NaiveDate,
    security_sha256: String,
    broker_sha256: String,
    suspended_sha256: String,
    received_ms: i64,
}
impl Universe {
    pub(super) fn dashboard_projection(&self) -> serde_json::Value {
        let items = self
            .instruments
            .iter()
            .map(|instrument| {
                serde_json::json!({
                    "instrument_key":instrument.key,
                    "symbol":instrument.symbol,
                    "segment":"NSE_EQ",
                    "series":instrument.series,
                    "tick":instrument.tick.amount().to_string(),
                    "tick_unit":"INR"
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "source":"validated_nse_eq_universe",
            "segment":"NSE_EQ",
            "series":"EQ",
            "file_date":self.file_date,
            "session":self.session,
            "received_at_ms":self.received_ms,
            "source_hashes":{
                "nse_cm_security_sha256":self.security_sha256,
                "upstox_nse_bod_sha256":self.broker_sha256,
                "upstox_suspended_bod_sha256":self.suspended_sha256
            },
            "total":items.len(),
            "items":items
        })
    }
}
pub(super) fn number(v: &serde_json::Value) -> Result<Amount> {
    if !v.is_number() {
        return Err("number_missing".into());
    }
    Ok(Amount::parse(&v.to_string())?)
}
// Stream JSON rows: never materialize the 70k+ derivative catalog as a Value tree.
fn broker_rows(bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
    struct Rows;
    impl<'de> Visitor<'de> for Rows {
        type Value = Vec<serde_json::Value>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("instrument array")
        }
        fn visit_seq<A: SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            let mut n = 0;
            while let Some(v) = seq.next_element::<serde_json::Value>()? {
                n += 1;
                if n > 300000 {
                    return Err(serde::de::Error::custom("catalog_limit"));
                }
                if v["segment"] == "NSE_EQ" {
                    out.push(v);
                }
            }
            Ok(out)
        }
    }
    let decoder = flate2::read::GzDecoder::new(bytes).take(100_000_001);
    let mut de = serde_json::Deserializer::from_reader(decoder);
    let rows = serde::de::Deserializer::deserialize_seq(&mut de, Rows)?;
    de.end()?;
    Ok(rows)
}
fn build(c: &Config, security: &[u8], broker: &[u8], suspended: &[u8]) -> Result<Universe> {
    let blocked: BTreeSet<(String, String)> = broker_rows(suspended)?
        .iter()
        .filter_map(|r| {
            Some((
                r["instrument_key"].as_str()?.to_owned(),
                r["instrument_type"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let mut mapping = BTreeMap::<(String, String), Vec<serde_json::Value>>::new();
    for row in broker_rows(broker)? {
        if let (Some(s), Some(series)) = (
            row["trading_symbol"].as_str(),
            row["instrument_type"].as_str(),
        ) {
            mapping
                .entry((s.to_owned(), series.to_owned()))
                .or_default()
                .push(row);
        }
    }
    let plain = flate2::read::GzDecoder::new(security).take(20_000_001);
    let mut reader = csv::Reader::from_reader(plain);
    let h = reader.headers()?.clone();
    let col = |s: &str| h.iter().position(|v| v == s).ok_or("security_column");
    let symbol = col("TckrSymb")?;
    let series = col("SctySrs")?;
    let isin = col("ISIN")?;
    let lot = col("NewBrdLotQty")?;
    let normal = col("ElgbltyNrmlMkt")?;
    let mut out = Vec::new();
    let mut exclusions = BTreeMap::<String, usize>::new();
    let mut keys = BTreeSet::new();
    let mut conflicts = BTreeSet::new();
    for r in reader.records() {
        let r = r?;
        let s = r.get(symbol).unwrap_or("");
        let kind = r.get(series).unwrap_or("");
        let reason = if kind == "BE" {
            Some("BE_trade_to_trade_excluded_from_intraday")
        } else if kind != "EQ" {
            Some("outside_cash_EQ_series")
        } else if r.get(lot) != Some("1") || r.get(normal) != Some("1") {
            Some("not_normal_market_unit_lot")
        } else {
            None
        };
        if let Some(reason) = reason {
            *exclusions.entry(reason.into()).or_default() += 1;
            continue;
        }
        let rows = mapping.get(&(s.to_owned(), kind.to_owned()));
        let Some(rows) = rows.filter(|v| v.len() == 1) else {
            *exclusions
                .entry("missing_or_ambiguous_broker_mapping".into())
                .or_default() += 1;
            continue;
        };
        let b = &rows[0];
        let key = b["instrument_key"].as_str().unwrap_or("");
        let id = r.get(isin).unwrap_or("");
        if id.len() != 12
            || !(id.starts_with("INE") || id.starts_with("INF"))
            || !id.bytes().all(|b| b.is_ascii_alphanumeric())
            || b["isin"] != id
            || b["exchange"] != "NSE"
            || key != format!("NSE_EQ|{id}")
            || b["lot_size"] != 1
            || b["security_type"] != "NORMAL"
            || blocked.contains(&(key.to_owned(), kind.to_owned()))
        {
            *exclusions
                .entry("identity_kind_status_or_suspended".into())
                .or_default() += 1;
            continue;
        }
        // Upstox JSON tick_size is paise, converted explicitly once to INR.
        let tick = match number(&b["tick_size"])
            .and_then(|n| Ok(Price::try_from(n.checked_mul(a("0.01"))?)?))
        {
            Ok(p) if p.amount() >= a("0.01") && p.amount() <= a("100") => p,
            _ => {
                *exclusions.entry("unknown_tick".into()).or_default() += 1;
                continue;
            }
        };
        if !keys.insert(key.to_owned()) {
            conflicts.insert(key.to_owned());
            *exclusions.entry("duplicate_ISIN".into()).or_default() += 1;
            continue;
        }
        out.push(Instrument {
            key: key.into(),
            symbol: s.into(),
            series: kind.into(),
            tick,
        });
    }
    out.retain(|i| !conflicts.contains(&i.key));
    out.sort_by(|a, b| a.key.cmp(&b.key));
    if out.is_empty() || out.len() > 20000 {
        return Err("empty_validated_universe".into());
    }
    Ok(Universe {
        instruments: out,
        exclusions,
        file_date: c.previous_session,
        session: c.session,
        security_sha256: digest(security),
        broker_sha256: digest(broker),
        suspended_sha256: digest(suspended),
        received_ms: millis(),
    })
}
fn security_path(c: &Config) -> PathBuf {
    c.directory.join(format!(
        "NSE_CM_security_{}.csv.gz",
        c.previous_session.format("%d%m%Y")
    ))
}
pub(super) fn cached(c: &Config) -> Result<Universe> {
    let manifest: Universe =
        serde_json::from_slice(&fs::read(c.directory.join("wide-universe.json"))?)?;
    if manifest.file_date != c.previous_session || manifest.session != c.session {
        return Err(Fail::new(
            "frozen_universe_session_mismatch_use_matching_metadata_in_new_session_directory",
        )
        .into());
    }
    let security = fs::read(security_path(c))?;
    let broker = fs::read(c.directory.join("upstox-NSE.json.gz"))?;
    let suspended = fs::read(c.directory.join("upstox-suspended.json.gz"))?;
    if digest(&security) != manifest.security_sha256
        || digest(&broker) != manifest.broker_sha256
        || digest(&suspended) != manifest.suspended_sha256
    {
        return Err("universe_hash".into());
    }
    let rebuilt = build(c, &security, &broker, &suspended)?;
    if rebuilt.instruments != manifest.instruments {
        return Err("universe_mapping_changed".into());
    }
    Ok(manifest)
}
fn download(client: &data::Client, url: &str, max: u64) -> Result<Vec<u8>> {
    client.bytes(url, max)
}
pub(super) fn prepare(client: &data::Client, c: &Config) -> Result<Universe> {
    if let Ok(u) = cached(c) {
        return Ok(u);
    }
    // Preserve any existing manifest; never silently replace a frozen universe.
    if c.directory.join("wide-universe.json").exists() {
        return Err("frozen_universe_invalid".into());
    }
    let url = format!(
        "https://nsearchives.nseindia.com/content/cm/NSE_CM_security_{}.csv.gz",
        c.previous_session.format("%d%m%Y")
    );
    let security = download(client, &url, 5_000_000)?;
    let broker = download(client, BOD, 10_000_000)?;
    let suspended = download(client, SUSPENDED, 5_000_000)?;
    let u = build(c, &security, &broker, &suspended)?;
    for (path, bytes) in [
        (security_path(c), security),
        (c.directory.join("upstox-NSE.json.gz"), broker),
        (c.directory.join("upstox-suspended.json.gz"), suspended),
    ] {
        fs::write(&path, bytes)?;
        fs::File::open(path)?.sync_all()?;
    }
    let tmp = c.directory.join("wide-universe.tmp");
    fs::write(&tmp, serde_json::to_vec(&u)?)?;
    fs::File::open(&tmp)?.sync_all()?;
    fs::rename(tmp, c.directory.join("wide-universe.json"))?;
    fs::File::open(&c.directory)?.sync_all()?;
    Ok(u)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn zip(s: &str) -> Vec<u8> {
        use std::io::Write;
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(s.as_bytes()).unwrap();
        z.finish().unwrap()
    }
    #[test]
    fn mapping_status_tick_conflict_and_be() {
        let d = tempfile::tempdir().unwrap();
        let c = super::super::super::tests::config(d.path());
        let h = "TckrSymb,SctySrs,ISIN,NewBrdLotQty,ElgbltyNrmlMkt\n";
        let s = zip(&format!(
            "{h}AAA,EQ,INE000000001,1,1\nBBB,EQ,INF000000002,1,1\nCCC,BE,INE000000003,1,1\nDDD,EQ,INE000000004,1,0\n"
        ));
        let rows = serde_json::json!([{"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"AAA","instrument_type":"EQ","isin":"INE000000001","instrument_key":"NSE_EQ|INE000000001","lot_size":1,"tick_size":5,"security_type":"NORMAL"},{"segment":"NSE_EQ","exchange":"NSE","trading_symbol":"BBB","instrument_type":"EQ","isin":"INF000000002","instrument_key":"NSE_EQ|INF000000002","lot_size":1,"tick_size":1,"security_type":"NORMAL"},{"segment":"NSE_INDEX","exchange":"NSE","trading_symbol":"NIFTY","instrument_type":"INDEX","instrument_key":"NSE_INDEX|Nifty 50","lot_size":1,"tick_size":5},{"segment":"NSE_FO","exchange":"NSE","trading_symbol":"NIFTY26NOVFUT","instrument_type":"FUT","instrument_key":"NSE_FO|NIFTY26NOVFUT","lot_size":50,"tick_size":5}]);
        let broker = zip(&rows.to_string());
        let u = build(&c, &s, &broker, &zip("[]")).unwrap();
        assert_eq!(u.instruments.len(), 2);
        assert!(
            u.instruments
                .iter()
                .all(|instrument| instrument.key.starts_with("NSE_EQ|")),
            "index and F&O rows never enter the cash universe"
        );
        assert_eq!(u.instruments[0].tick, Price::parse("0.05").unwrap());
        let projection = u.dashboard_projection();
        assert_eq!(projection["source"], "validated_nse_eq_universe");
        assert_eq!(projection["segment"], "NSE_EQ");
        assert_eq!(projection["series"], "EQ");
        assert_eq!(projection["total"], 2);
        assert_eq!(projection["received_at_ms"], u.received_ms);
        assert_eq!(
            projection["source_hashes"]["nse_cm_security_sha256"],
            u.security_sha256
        );
        assert_eq!(
            projection["items"][0]["instrument_key"],
            "NSE_EQ|INE000000001"
        );
        assert_eq!(projection["items"][0]["tick"], "0.05");
        assert_eq!(projection["items"][0]["tick_unit"], "INR");
        assert!(projection.get("exclusions").is_none());
        let blocked = zip(
            &serde_json::json!([{"segment":"NSE_EQ","instrument_type":"EQ","instrument_key":"NSE_EQ|INE000000001"}])
                .to_string(),
        );
        assert_eq!(
            build(&c, &s, &broker, &blocked).unwrap().instruments.len(),
            1
        );
        let duplicate = zip(&format!(
            "{h}AAA,EQ,INE000000001,1,1\nAAA,EQ,INE000000001,1,1\n"
        ));
        assert!(build(&c, &duplicate, &broker, &zip("[]")).is_err());
    }
}
