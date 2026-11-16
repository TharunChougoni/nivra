//! Frozen, read-only decision context from the controller's existing observations.
//! No provider calls, financial writes, inferred minutes or volume synthesis.
use super::*;
use sadr_core::{
    execution::{Instrument, Scanner},
    feature_store::{FeatureStore, MinuteBar},
    strategy::{Feature, FeatureSettings},
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub max_bars: usize,
    pub lookback_samples: usize,
    pub return_windows_minutes: Vec<u32>,
    pub max_age_ms: i64,
    pub metadata_max_age_ms: i64,
    pub benchmark_min_instruments: usize,
    pub volume_proxy: FeatureSettings,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_bars: 20,
            lookback_samples: 20,
            return_windows_minutes: vec![1, 5, 15],
            max_age_ms: 180_000,
            metadata_max_age_ms: 300_000,
            benchmark_min_instruments: 200,
            volume_proxy: FeatureSettings::default(),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if !(1..=20).contains(&self.max_bars)
            || !(1..=20).contains(&self.lookback_samples)
            || self.return_windows_minutes.is_empty()
            || self.return_windows_minutes.len() > 6
            || self
                .return_windows_minutes
                .iter()
                .any(|n| !(1..=60).contains(n))
            || self
                .return_windows_minutes
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.return_windows_minutes.len()
            || !(1..=86_400_000).contains(&self.max_age_ms)
            || !(1..=86_400_000).contains(&self.metadata_max_age_ms)
            || self.benchmark_min_instruments == 0
        {
            return Err("decision_evidence_settings_invalid".into());
        }
        self.volume_proxy.validate()?;
        Ok(())
    }
}
struct Row {
    receipt: Option<i64>,
    source: Option<i64>,
    metadata_receipt: Option<i64>,
    volume: Option<u64>,
    average_volume: Option<f64>,
    average_volume_sessions: Option<u32>,
    bars: Vec<MinuteBar>,
    features: BTreeMap<Feature, f64>,
    returns: BTreeMap<u32, Option<f64>>,
}
pub(crate) struct Snapshot {
    session: NaiveDate,
    at: i64,
    settings: Settings,
    rows: BTreeMap<String, Row>,
    /// Additive, non-tradeable index context; absent unless configured.
    index_context: Option<Value>,
}
fn age(now: i64, receipt: Option<i64>, max: i64) -> Option<i64> {
    now.checked_sub(receipt?).filter(|n| (0..=max).contains(n))
}
fn window(bars: &std::collections::VecDeque<MinuteBar>, end: u32, minutes: u32) -> Option<f64> {
    let start = end.checked_sub(minutes)?;
    let rows = bars
        .iter()
        .filter(|b| (start..=end).contains(&b.start_minute))
        .collect::<Vec<_>>();
    if rows.len() != minutes as usize + 1
        || rows.first()?.start_minute != start
        || rows.last()?.start_minute != end
        || rows
            .windows(2)
            .any(|w| w[1].start_minute != w[0].start_minute + 1)
    {
        return None;
    }
    let value = (rows.last()?.close / rows.first()?.close - 1.0) * 10_000.0;
    value.is_finite().then_some(value)
}
impl Snapshot {
    /// Frozen context plus an additive, non-tradeable index projection.
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        settings: &Settings,
        session: NaiveDate,
        now: i64,
        instruments: &[Instrument],
        scanner: &Scanner,
        store: &FeatureStore,
        index_context: Option<Value>,
    ) -> Result<Self> {
        settings.validate()?;
        let features = sadr_core::strategy::extract_features_with_settings(
            scanner,
            now,
            settings.max_age_ms,
            settings.metadata_max_age_ms,
            settings.lookback_samples,
            &settings.volume_proxy,
        )?;
        let mut features = features
            .rows
            .into_iter()
            .map(|r| (r.key, r.features))
            .collect::<BTreeMap<_, _>>();
        let end = minute(now).saturating_sub(1);
        let same_session = store.session() == Some(session) && local(now).date_naive() == session;
        let mut rows = BTreeMap::new();
        for i in instruments {
            let sample = scanner.samples.get(&i.key);
            let receipt = if same_session {
                store.last_receipt(&i.key)
            } else {
                None
            };
            let bars = if same_session {
                store.bars(&i.key)
            } else {
                None
            };
            let returns = settings
                .return_windows_minutes
                .iter()
                .map(|n| {
                    (
                        *n,
                        if age(now, receipt, settings.max_age_ms).is_some() {
                            bars.and_then(|b| window(b, end, *n))
                        } else {
                            None
                        },
                    )
                })
                .collect();
            let retained = bars
                .into_iter()
                .flat_map(|b| b.iter())
                .filter(|b| b.start_minute <= end)
                .collect::<Vec<_>>();
            let retained = retained
                .iter()
                .skip(retained.len().saturating_sub(settings.max_bars))
                .map(|b| (*b).clone())
                .collect();
            rows.insert(
                i.key.clone(),
                Row {
                    receipt,
                    source: sample.and_then(|s| s.source),
                    metadata_receipt: sample.and_then(|s| s.market_fields_receipt),
                    volume: sample
                        .and_then(|s| s.market_fields.as_ref())
                        .map(|f| f.volume),
                    average_volume: sample.and_then(|s| s.avg_daily_volume),
                    average_volume_sessions: sample.and_then(|s| s.avg_daily_volume_sessions),
                    bars: retained,
                    features: features.remove(&i.key).unwrap_or_default(),
                    returns,
                },
            );
        }
        Ok(Self {
            session,
            at: now,
            settings: settings.clone(),
            rows,
            index_context,
        })
    }
    pub fn read(&self, keys: &[String], now: i64) -> Result<Value> {
        if keys.is_empty()
            || keys.len() > super::harness::MAX_INSTRUMENT_KEYS
            || keys.iter().collect::<BTreeSet<_>>().len() != keys.len()
            || keys.iter().any(|k| !self.rows.contains_key(k))
        {
            return Err("decision_evidence_keys_invalid".into());
        }
        if now < self.at || local(now).date_naive() != self.session {
            return Err("decision_evidence_clock_or_session".into());
        }
        let market: BTreeMap<u32, (usize, Option<f64>)> = self
            .settings
            .return_windows_minutes
            .iter()
            .map(|n| {
                let mut values = self
                    .rows
                    .values()
                    .filter(|r| age(now, r.receipt, self.settings.max_age_ms).is_some())
                    .filter_map(|r| r.returns[n])
                    .collect::<Vec<_>>();
                let count = values.len();
                values.sort_by(f64::total_cmp);
                let median = (count >= self.settings.benchmark_min_instruments).then(|| {
                    if count % 2 == 0 {
                        values[count / 2 - 1] / 2.0 + values[count / 2] / 2.0
                    } else {
                        values[count / 2]
                    }
                });
                (*n, (count, median))
            })
            .collect();
        let rows=keys.iter().map(|key| {
            let row=&self.rows[key];
            let fresh=age(now,row.receipt,self.settings.max_age_ms).is_some();
            let metadata_fresh=age(now,row.metadata_receipt,self.settings.metadata_max_age_ms).is_some();
            let features=row.features.iter().filter(|(f,_)|fresh && (metadata_fresh || matches!(f,Feature::Price|Feature::MoveBps|Feature::AbsoluteMoveBps|Feature::MinutesSinceOpen|Feature::RangePosition|Feature::VolatilityBps))).collect::<BTreeMap<_,_>>();
            let windows=self.settings.return_windows_minutes.iter().map(|n| {
                let own=row.returns[n].filter(|_|fresh);let (count,market)=market[n];let market=market.filter(|_|fresh);
                json!({"minutes":n,"end_minute":minute(self.at).saturating_sub(1),"return_bps":own,"market_median_bps":market,"market_contributors":count,
                    "relative_bps":own.zip(market).map(|(a,b)|a-b),"status":if !fresh {"stale_or_unavailable"} else if own.is_none(){"missing_consecutive_minutes"}else if market.is_none(){"benchmark_unavailable"}else{"available"}})
            }).collect::<Vec<_>>();
            json!({"instrument_key":key,"status":if fresh {"available"}else if row.receipt.is_none(){"unavailable"}else{"stale"},
                "last_price_receipt_ms":row.receipt,"scanner_source_ms":row.source,"metadata_receipt_ms":row.metadata_receipt,"metadata_status":if metadata_fresh {"available"}else{"stale_or_unavailable"},
                "metadata_source_ms":Value::Null,"cumulative_volume":row.volume.filter(|_|metadata_fresh),"avg_daily_volume":row.average_volume,"avg_daily_volume_sessions":row.average_volume_sessions,
                "features":features,"returns":windows,"bars":row.bars.iter().map(|b|json!([b.start_minute,b.open,b.high,b.low,b.close,b.tick_count])).collect::<Vec<_>>()})
        }).collect::<Vec<_>>();
        Ok(
            json!({"schema":1,"session":self.session,"as_of_ms":self.at,"read_at_ms":now,"execution_authorized":false,"source":"receipt_observed_prices_and_scanner_metadata",
            "sample_lookback":self.settings.lookback_samples,"volume_proxy_parameters":self.settings.volume_proxy,"bar_columns":["ist_minute","open","high","low","close","tick_count"],"rows":rows,
            "index_context":self.index_context.clone().unwrap_or(Value::Null),
            "limitations":["Frozen screening evidence; request get_market_snapshot for new executable observations","features move_bps/market_relative_bps use irregular retained samples; only returns uses aligned consecutive completed minutes","Bars are receipt-derived price OHLC, not exchange-certified OHLCV; tick_count is not volume; missing minutes stay missing","session_vwap uses the provider average-price field; it is not independently recomputed from individual trades","rel_volume_tod is a daily-volume time-of-day proxy, not measured per-minute relative volume","Search/news claims require dated primary-source verification; absence of news is not itself a trading prohibition"]}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sadr_core::{
        execution::{MarketFields, Sample, a},
        finance::Price,
    };
    fn at(minute: u32) -> i64 {
        DateTime::parse_from_rfc3339("2026-10-16T00:00:00+05:30")
            .unwrap()
            .timestamp_millis()
            + i64::from(minute) * 60_000
    }
    fn fixture() -> (Settings, Vec<Instrument>, Scanner, FeatureStore) {
        let settings = Settings {
            benchmark_min_instruments: 2,
            ..Settings::default()
        };
        let instruments = ["A", "B"]
            .into_iter()
            .map(|key| Instrument {
                key: key.into(),
                symbol: key.into(),
                series: "EQ".into(),
                tick: Price::parse("0.05").unwrap(),
            })
            .collect::<Vec<_>>();
        let mut scan = Scanner::default();
        let mut store = FeatureStore::new(Default::default()).unwrap();
        store.reset_for_session(local(at(562)).date_naive());
        for (n, i) in instruments.iter().enumerate() {
            for m in 555..=562 {
                store
                    .observe(&i.key, 100.0 + n as f64 * 100.0 + (m - 555) as f64, at(m))
                    .unwrap();
            }
            scan.samples.insert(
                i.key.clone(),
                Sample {
                    seen: at(562),
                    source: Some(at(562) - 10),
                    current: Some(Price::parse("107").unwrap()),
                    history: vec![Price::parse("100").unwrap()],
                    reason: "eligible_no_signal".into(),
                    market_fields_receipt: Some(at(562)),
                    avg_daily_volume: Some(10_000.0),
                    avg_daily_volume_sessions: Some(21),
                    market_fields: Some(MarketFields {
                        session_vwap: Some(103.0),
                        best_bid: Some(106.95),
                        best_ask: Some(107.0),
                        best_bid_qty: Some(10),
                        best_ask_qty: Some(20),
                        total_buy_quantity: Some(30),
                        total_sell_quantity: Some(60),
                        lower_circuit_limit: a("90"),
                        upper_circuit_limit: a("120"),
                        volume: 1000,
                    }),
                    ..Sample::default()
                },
            );
        }
        (settings, instruments, scan, store)
    }
    #[test]
    fn context_preserves_receipts_gaps_metadata_and_aligned_benchmark_without_execution() {
        let (settings, instruments, scan, mut store) = fixture();
        let now = at(562);
        let snapshot = Snapshot::capture(
            &settings,
            local(now).date_naive(),
            now,
            &instruments,
            &scan,
            &store,
            None,
        )
        .unwrap();
        let value = snapshot.read(&["A".into()], now + 50).unwrap();
        let row = &value["rows"][0];
        assert_eq!(row["last_price_receipt_ms"], now);
        assert_eq!(row["metadata_receipt_ms"], now);
        assert_eq!(row["scanner_source_ms"], now - 10);
        assert_eq!(row["features"]["session_vwap"], 103.0);
        assert!(row["features"]["rel_volume_tod"].as_f64().unwrap() > 0.0);
        assert_eq!(row["bars"].as_array().unwrap().len(), 7);
        assert_eq!(row["bars"][6][0], 561);
        let expected = (106.0 / 105.0 - 1.0) * 10_000.0;
        assert!((row["returns"][0]["return_bps"].as_f64().unwrap() - expected).abs() < 1e-9);
        assert_eq!(row["returns"][0]["market_contributors"], 2);
        assert_eq!(row["returns"][2]["status"], "missing_consecutive_minutes");
        assert_eq!(value["execution_authorized"], false);
        assert_eq!(value["bar_columns"][5], "tick_count");
        let stale = snapshot
            .read(&["A".into()], now + settings.metadata_max_age_ms + 1)
            .unwrap();
        assert_eq!(stale["rows"][0]["status"], "stale");
        assert_eq!(stale["rows"][0]["features"], json!({}));
        assert_eq!(stale["rows"][0]["last_price_receipt_ms"], now);
        assert!(snapshot.read(&["unknown".into()], now).is_err());
        assert!(snapshot.read(&["A".into(), "A".into()], now).is_err());
        assert!(snapshot.read(&["A".into()], now - 1).is_err());
        // Missing one middle minute must not become a five-minute return or a fabricated flat bar.
        let mut missing = FeatureStore::new(Default::default()).unwrap();
        missing.reset_for_session(local(now).date_naive());
        for m in [555, 556, 558, 559, 560, 561, 562] {
            missing.observe("A", 100.0, at(m)).unwrap();
        }
        let gap = Snapshot::capture(
            &settings,
            local(now).date_naive(),
            now,
            &instruments,
            &scan,
            &missing,
            None,
        )
        .unwrap()
        .read(&["A".into(), "B".into()], now)
        .unwrap();
        assert_eq!(gap["rows"][0]["returns"][1]["return_bps"], Value::Null);
        assert_eq!(
            gap["rows"][0]["returns"][0]["market_median_bps"],
            Value::Null
        );
        assert_eq!(gap["rows"][1]["status"], "unavailable");
        store.reset_for_session(local(now + 86_400_000).date_naive());
        let wrong = Snapshot::capture(
            &settings,
            local(now).date_naive(),
            now,
            &instruments,
            &scan,
            &store,
            None,
        )
        .unwrap()
        .read(&["A".into()], now)
        .unwrap();
        assert_eq!(wrong["rows"][0]["status"], "unavailable");
        let mut metadata_settings = settings.clone();
        metadata_settings.metadata_max_age_ms = 1_000;
        let aged = Snapshot::capture(
            &metadata_settings,
            local(now).date_naive(),
            now,
            &instruments,
            &scan,
            &missing,
            None,
        )
        .unwrap()
        .read(&["A".into()], now + 1_001)
        .unwrap();
        assert_eq!(aged["rows"][0]["status"], "available");
        assert_eq!(aged["rows"][0]["metadata_status"], "stale_or_unavailable");
        assert!(aged["rows"][0]["features"].get("session_vwap").is_none());
        assert_eq!(aged["rows"][0]["cumulative_volume"], Value::Null);
        // Index context is additive digest evidence and never execution authority.
        let with_index = Snapshot::capture(
            &settings,
            local(now).date_naive(),
            now,
            &instruments,
            &scan,
            &store,
            Some(serde_json::json!({
                "paper_only": true, "tradeable": false,
                "observations": [{"instrument_key": "NSE_INDEX|Nifty 50", "price": "24500"}]
            })),
        )
        .unwrap()
        .read(&["A".into()], now)
        .unwrap();
        assert_eq!(with_index["execution_authorized"], false);
        assert_eq!(with_index["index_context"]["tradeable"], false);
        assert_eq!(
            with_index["index_context"]["observations"][0]["instrument_key"],
            "NSE_INDEX|Nifty 50"
        );
        assert!(
            Snapshot::capture(
                &settings,
                local(now).date_naive(),
                now,
                &instruments,
                &scan,
                &store,
                None,
            )
            .unwrap()
            .read(&["A".into()], now)
            .unwrap()["index_context"]
                .is_null(),
            "cash-only sessions carry no index context"
        );
        let mut invalid = settings;
        invalid.return_windows_minutes = vec![1, 1];
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn configured_context_reaches_archived_tool_and_policy_without_changing_default_contracts() {
        use super::super::{harness, planner};
        let (settings, instruments, scan, store) = fixture();
        let now = at(562);
        let snapshot = Snapshot::capture(
            &settings,
            local(now).date_naive(),
            now,
            &instruments,
            &scan,
            &store,
            None,
        )
        .unwrap();
        let default_cfg = planner::PlannerConfig::default();
        let default_bytes = planner::policy_bytes(&default_cfg).unwrap();
        assert!(
            !String::from_utf8(default_bytes.clone())
                .unwrap()
                .contains("decision_evidence")
        );
        let fragment: Value = serde_json::from_str(include_str!(
            "../../../../packaging/rust/decision-evidence.fragment.json"
        ))
        .unwrap();
        let fragment: Settings =
            serde_json::from_value(fragment["decision_evidence"].clone()).unwrap();
        fragment.validate().unwrap();
        let cfg = planner::PlannerConfig {
            decision_evidence: Some(settings),
            max_handoff_instruments: 1,
            ..default_cfg.clone()
        };
        let policy = planner::policy_bytes(&cfg).unwrap();
        assert_ne!(policy, default_bytes);
        assert!(
            serde_json::from_slice::<Value>(&policy).unwrap()["decision_evidence"]
                .get("prompt_sha256")
                .is_none()
        );
        let dir = tempfile::tempdir().unwrap();
        let c = super::super::tests::config(dir.path());
        fs::write(
            dir.path().join("planner.json"),
            serde_json::to_vec(&cfg).unwrap(),
        )
        .unwrap();
        assert!(planner::PlannerConfig::load(&dir.path().join("planner.json")).is_ok());
        struct Fake<'a> {
            context: &'a Snapshot,
            requests: Vec<Value>,
        }
        impl planner::Backend for Fake<'_> {
            fn call(&mut self, _: &planner::ModelRole, prompt: &str) -> Result<String> {
                self.requests.push(serde_json::from_str(prompt)?);
                Ok(if self.requests.len()==1 {json!({"output":[{"type":"function_call","name":"get_intraday_context","call_id":"context-one","arguments":"{\"instrument_keys\":[\"A\"]}"}]})} else {json!({"complete":true,"as_of_ms":0,"coverage":{"universe":1,"scanned":1,"quoted":1,"featured":1,"experiments":0},"gaps":[],"changed_prospects":[],"active_rankings":[],"candidate_rankings":[],"bar_context":[],"spread_depth":[],"holdings":[],"cash":{"available":"0"},"fees":{"per_order":"0","buy_bps":"0","sell_bps":"0"},"recent_actions":[],"experiments":[],"evidence_ids":["bundle"]})}.to_string())
            }
            fn intraday_context(&mut self, keys: &[String], at: i64) -> Option<Result<Value>> {
                Some(self.context.read(keys, at))
            }
        }
        let mut backend = Fake {
            context: &snapshot,
            requests: vec![],
        };
        let clock = || now;
        let mut guard = planner::preview_guard();
        guard.clock = &clock;
        let role = planner::ModelRole {
            enabled: true,
            model_id: "gpt-6-luna".into(),
            daily_call_budget: 8,
            ..Default::default()
        };
        let answer = harness::run(
            &c,
            &cfg,
            "digest",
            &role,
            "fixture",
            &json!({"allowlist":instruments}),
            &mut backend,
            &guard,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&answer).unwrap()["complete"],
            true
        );
        assert_eq!(backend.requests.len(), 2);
        let frame: harness::Frame = serde_json::from_value(backend.requests[0].clone()).unwrap();
        assert_eq!(
            frame.tools,
            harness::configured_definitions("digest", &role.tools, false, true, 1).unwrap()
        );
        assert!(
            !harness::definitions("digest", &[], false)
                .unwrap()
                .iter()
                .any(|t| t["name"] == "get_intraday_context")
        );
        let result = backend.requests[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "function_call_output")
            .unwrap();
        let result: Value = serde_json::from_str(result["output"].as_str().unwrap()).unwrap();
        assert_eq!(result["rows"][0]["last_price_receipt_ms"], now);
        assert_eq!(result["execution_authorized"], false);
        let run = fs::read_dir(c.directory.join("model"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("harness-digest-")
            })
            .unwrap();
        let archive = fs::read_dir(run)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .ends_with(".result.json")
            })
            .unwrap();
        let archived: Value = serde_json::from_slice(&fs::read(archive).unwrap()).unwrap();
        assert_eq!(archived, result);
        assert!(!c.directory.join("paper.sqlite").exists());
        assert!(!c.token_file.exists());
        let owner_cfg = planner::PlannerConfig::default();
        let digest_frame = harness::Frame {
            role: "digest".into(),
            input: vec![],
            tools: vec![],
            citation_ids: vec![],
            final_answer_only: false,
            review_only: false,
            owner_tools: Default::default(),
            decision_evidence: true,
            max_instrument_keys: None,
            instructions: None,
            tool_limits: None,
        };
        let digest_text = harness::frame_instructions_for(&owner_cfg, &digest_frame).unwrap();
        assert!(digest_text.contains("owner-facts compiler"));
        assert!(!digest_text.contains("Grok handoff"));
        assert!(!digest_text.contains("DECISION EVIDENCE POLICY v1"));
        let owner_frame = harness::Frame {
            role: "supervisor".into(),
            decision_evidence: false,
            ..digest_frame
        };
        let owner_text = harness::frame_instructions_for(&owner_cfg, &owner_frame).unwrap();
        assert!(owner_text.contains("persistent paper decision owner"));
        assert!(!owner_text.contains("legacy supervisor turns"));
        let decision = super::super::orchestrator_model::SessionTrigger::Decision.default_prompt();
        assert!(decision.contains("Factual digest"));
        assert!(!decision.contains("decision_evidence"));
    }
}
