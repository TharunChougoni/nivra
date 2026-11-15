use sadr_core::{
    paper_bundle::{PaperBundle, PaperMandate},
    paper_source::{PaperSourceBinding, ReaderDeclaration, Source},
};
use serde_json::{Value, json};
fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/rust/paper_contract_vectors.json"
    ))
    .unwrap()
}
#[test]
fn original_bundle_mandate_source_and_evidence_declarations_match_frozen_cases() {
    let fixture = vectors();
    for case in fixture["cases"].as_array().unwrap() {
        let raw = serde_json::to_vec(&case["input"]).unwrap();
        let accepted = match case["kind"].as_str().unwrap() {
            "bundle" => PaperBundle::parse(&raw).is_ok(),
            "mandate" => PaperMandate::parse(&raw).is_ok(),
            "source" => PaperSourceBinding::parse(&raw).is_ok(),
            _ => panic!("invalid fixture kind"),
        };
        assert_eq!(
            accepted,
            case["rust_accepted"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if case["kind"] == "source" && accepted {
            let binding = PaperSourceBinding::parse(&raw).unwrap();
            assert_eq!(
                json!(binding.quote_params()),
                case["quote_params"],
                "{}",
                case["name"]
            );
            assert!(!binding.review_ref().is_empty());
            if binding.source() == Source::Kotak {
                let mapping = binding.native_mapping().unwrap();
                assert!(!mapping.segment().is_empty());
                assert!(!mapping.token().is_empty());
                assert!(!mapping.master_version().is_empty());
                assert!(binding.timestamp_reference().is_some());
            }
        }
    }
}
#[test]
fn declared_reader_compatibility_preserves_exact_origins_type_and_single_attempt() {
    for case in vectors()["readers"].as_array().unwrap() {
        let binding =
            PaperSourceBinding::parse(&serde_json::to_vec(&case["binding"]).unwrap()).unwrap();
        let actual = case["actual"].as_str().unwrap();
        let reader = match case["kind"].as_str().unwrap() {
            "upstox" => ReaderDeclaration::Upstox {
                base_url: actual,
                max_attempts: case["attempts"].as_u64().unwrap() as u32,
            },
            "kotak" => ReaderDeclaration::Kotak { origin: actual },
            _ => ReaderDeclaration::Other,
        };
        assert_eq!(
            binding.reader_matches(reader, case["origin"].as_str().unwrap()),
            case["expected"].as_bool().unwrap()
        );
    }
}

#[test]
fn paper_and_replay_fills_retain_side_reference_and_partial_exit_slippage() {
    use sadr_core::execution::{ExitIntent, Instrument, Portfolio, Quote, a};
    let instrument: Instrument = serde_json::from_value(json!({
        "key":"fixture", "symbol":"FIXTURE", "series":"EQ", "tick":"0.05"
    }))
    .unwrap();
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-26T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let quote: Quote = serde_json::from_value(json!({
        "bid":"100", "ask":"100", "bid_units":10, "ask_units":10,
        "source":now, "receipt":now
    }))
    .unwrap();
    let mut paper = Portfolio::new();
    let mut replay = Portfolio::new();
    let fixture = serde_json::from_str(include_str!("../../../fixtures/paper-costs.json")).unwrap();
    paper.costs = Some(fixture);
    replay.costs = paper.costs.clone();
    for (buy, units) in [(true, 4), (false, 1), (false, 3)] {
        for book in [&mut paper, &mut replay] {
            assert!(
                book.execute_plan(
                    now,
                    &instrument,
                    &quote,
                    buy,
                    units,
                    serde_json::from_value(json!("101")).unwrap(),
                    ExitIntent::default(),
                    "fixture"
                )
                .unwrap()
            );
            let fill = book.fills.last().unwrap();
            assert_eq!(fill.expected_price, Some(quote.bid));
            assert_eq!(fill.slippage_bps, Some(a("5")));
            book.reconcile().unwrap();
        }
        assert_eq!(paper, replay);
    }
    assert!(paper.positions.is_empty());
    let mut legacy = serde_json::to_value(&paper).unwrap();
    for fill in legacy["fills"].as_array_mut().unwrap() {
        fill.as_object_mut().unwrap().remove("expected_price");
        fill.as_object_mut().unwrap().remove("slippage_bps");
    }
    let original_legacy = serde_json::to_vec(&legacy).unwrap();
    let legacy: Portfolio = serde_json::from_value(legacy).unwrap();
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        serde_json::from_slice::<Value>(&original_legacy).unwrap()
    );
    legacy.reconcile().unwrap();
    assert!(
        legacy
            .fills
            .iter()
            .all(|f| f.expected_price.is_none() && f.slippage_bps.is_none())
    );

    // Phase 2: long option premium uses the same executable fill path with the
    // configured F&O slippage and tick rounding. Cash falls by premium x units
    // plus the F&O fee, never by the index notional, and there is no margin.
    use sadr_core::finance::{FeeSchedule, Price};
    use sadr_core::market::{InstrumentKind, OptionCatalog, OptionContract, Segment};
    use sadr_core::time::ExchangeDate;
    let contract = OptionContract {
        key: "NSE_FO|26000CE".into(),
        symbol: "NIFTY 26000 CE 20 OCT 26".into(),
        underlying_key: "NSE_INDEX|Nifty 50".into(),
        underlying_symbol: "NIFTY".into(),
        expiry: ExchangeDate::parse("2026-10-20").unwrap(),
        strike: Price::parse("26000").unwrap(),
        option: InstrumentKind::Call,
        lot_size: 65,
        tick: Price::parse("0.05").unwrap(),
        freeze_quantity: Some(1800),
        weekly: true,
    };
    let option_instrument = Instrument {
        key: contract.key.clone(),
        symbol: contract.symbol.clone(),
        series: "CE".into(),
        tick: contract.tick,
    };
    let fo: sadr_core::finance::FeeArithmetic = serde_json::from_value(json!({
        "per_order":"0.01","buy_bps":"0","sell_bps":"0",
        "slippage_bps":"10","rounding_increment":"0.05"
    }))
    .unwrap();
    let cash_costs = paper.costs.clone().unwrap();
    let mut options = Portfolio::with_capital(a("5000"));
    options.costs = Some(cash_costs.clone());
    options.fee_schedule =
        Some(FeeSchedule::new(cash_costs).with_segment(Segment::NseFo, fo.clone()));
    options.options = Some(
        OptionCatalog::try_new(
            ExchangeDate::parse("2026-10-16").unwrap(),
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            contract.expiry,
            Price::parse("26000").unwrap(),
            [contract.clone()],
        )
        .unwrap(),
    );
    let option_quote = |bid: &str, ask: &str, receipt: i64| Quote {
        bid: Price::parse(bid).unwrap(),
        ask: Price::parse(ask).unwrap(),
        bid_units: 130,
        ask_units: 130,
        source: receipt,
        receipt,
    };
    // Buy at ask 50.05 + 10 bps slippage, rounded up to the 0.05 tick: 50.15.
    let at = now;
    assert!(
        options
            .execute_plan(
                at,
                &option_instrument,
                &option_quote("50.00", "50.05", at),
                true,
                65,
                Price::parse("50.15").unwrap(),
                ExitIntent::default(),
                "option-buy",
            )
            .unwrap()
    );
    let fill = options.fills.last().unwrap();
    assert_eq!(fill.price, Price::parse("50.15").unwrap());
    assert_eq!(fill.expected_price, Some(Price::parse("50.05").unwrap()));
    assert_eq!(fill.fee, a("0.05"));
    assert_eq!(fill.costs.as_ref(), Some(&fo));
    assert_eq!(
        options.cash,
        a("5000")
            .checked_sub(a("50.15").checked_mul(a("65")).unwrap())
            .unwrap()
            .checked_sub(a("0.05"))
            .unwrap()
    );
    assert!(options.cash > a("1700"));
    options.reconcile().unwrap();
    // Sell at bid 50.00 - 10 bps, rounded down to the 0.05 tick: 49.95.
    assert!(
        options
            .execute_plan(
                at,
                &option_instrument,
                &option_quote("50.00", "50.05", at),
                false,
                65,
                Price::parse("49.95").unwrap(),
                ExitIntent::default(),
                "option-sell",
            )
            .unwrap()
    );
    let fill = options.fills.last().unwrap();
    assert_eq!(fill.price, Price::parse("49.95").unwrap());
    assert_eq!(fill.expected_price, Some(Price::parse("50.00").unwrap()));
    options.reconcile().unwrap();
    assert!(options.positions.is_empty());
    // The two-second original-receipt gate still applies to option fills.
    assert!(
        !options
            .execute_plan(
                at,
                &option_instrument,
                &option_quote("50.00", "50.05", at - 2_001),
                true,
                65,
                Price::parse("60").unwrap(),
                ExitIntent::default(),
                "option-stale",
            )
            .unwrap()
    );
    // Unknown NSE_FO keys and non-lot units are never filled as cash.
    let unknown = Instrument {
        key: "NSE_FO|26000PE".into(),
        symbol: "NIFTY 26000 PE".into(),
        series: "PE".into(),
        tick: Price::parse("0.05").unwrap(),
    };
    assert!(
        !options
            .execute_plan(
                at,
                &unknown,
                &option_quote("50.00", "50.05", at),
                true,
                65,
                Price::parse("60").unwrap(),
                ExitIntent::default(),
                "option-unknown",
            )
            .unwrap()
    );
    assert!(
        !options
            .execute_plan(
                at,
                &option_instrument,
                &option_quote("50.00", "50.05", at),
                true,
                64,
                Price::parse("60").unwrap(),
                ExitIntent::default(),
                "option-lot",
            )
            .unwrap()
    );
}

#[test]
fn configured_costs_fifo_partial_exits_and_reporting_failure_preserve_execution() {
    use sadr_core::execution::{ExitIntent, Instrument, Portfolio, Quote, a};
    use sadr_core::finance::{FeeArithmetic, Price};
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-26T10:00:00+05:30")
        .unwrap()
        .timestamp_millis();
    let mut i: Instrument = serde_json::from_value(json!({
        "key":"fixture", "symbol":"FIXTURE", "series":"EQ", "tick":"0.01"
    }))
    .unwrap();
    let mut q: Quote = serde_json::from_value(json!({
        "bid":"100", "ask":"100", "bid_units":10, "ask_units":10,
        "source":now, "receipt":now
    }))
    .unwrap();
    let costs: FeeArithmetic = serde_json::from_value(json!({
        "per_order":"1", "buy_bps":"0", "sell_bps":"0",
        "slippage_bps":"0", "rounding_increment":"0.01"
    }))
    .unwrap();
    costs.validate().unwrap();
    for invalid in [
        json!({"per_order":"-1", "buy_bps":"0", "sell_bps":"0", "slippage_bps":"0"}),
        json!({"per_order":"0", "buy_bps":"10001", "sell_bps":"0", "slippage_bps":"0"}),
        json!({"per_order":"0", "buy_bps":"0", "sell_bps":"10001", "slippage_bps":"0"}),
        json!({"per_order":"0", "buy_bps":"0", "sell_bps":"0", "slippage_bps":"10000"}),
        json!({"per_order":"0", "buy_bps":"0", "sell_bps":"0", "slippage_bps":"0", "rounding_increment":"0"}),
    ] {
        assert!(
            serde_json::from_value::<FeeArithmetic>(invalid)
                .map_or(true, |v| v.validate().is_err())
        );
    }
    let mut p = Portfolio::new();
    p.costs = Some(costs.clone());
    for (buy, units, price) in [
        (true, 3, "100"),
        (true, 2, "110"),
        (false, 1, "120"),
        (false, 4, "120"),
    ] {
        q.bid = Price::parse(price).unwrap();
        q.ask = q.bid;
        assert!(
            p.execute_plan(
                now,
                &i,
                &q,
                buy,
                units,
                Price::parse("200").unwrap(),
                ExitIntent::default(),
                "fixture"
            )
            .unwrap()
        );
    }
    let net = p.realised_net_pnl();
    assert_eq!(
        net,
        vec![None, None, Some(a("18.66666667")), Some(a("57.33333333"))]
    );
    assert_eq!(
        net.into_iter()
            .flatten()
            .try_fold(a("0"), |sum, v| sum.checked_add(v))
            .unwrap(),
        p.cash.checked_sub(a("5000")).unwrap()
    );
    let restored: Portfolio = serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
    restored.reconcile().unwrap();
    assert_eq!(restored.realised_net_pnl(), p.realised_net_pnl());
    // Each fill's own fee snapshot reconciles, independent of a subsequent policy.
    p.costs = Some(
        serde_json::from_str(include_str!(
            "../../../fixtures/test-legacy-paper-costs.json"
        ))
        .unwrap(),
    );
    p.reconcile().unwrap();
    // Overflow in bps reporting is possible even when the actual fill is valid.
    let mut large = Portfolio::with_capital(a("50000000000000000000000000"));
    large.costs = Some(costs);
    i.tick = Price::parse("20000000000000000000000000").unwrap();
    q.bid = Price::parse("10000000000000000000000000").unwrap();
    q.ask = q.bid;
    assert!(
        large
            .execute_plan(
                now,
                &i,
                &q,
                true,
                1,
                i.tick,
                ExitIntent::default(),
                "reporting-overflow"
            )
            .unwrap()
    );
    assert_eq!(large.fills[0].slippage_bps, None);
    assert_eq!(large.fills[0].expected_price, Some(q.ask));
    large.reconcile().unwrap();
}
