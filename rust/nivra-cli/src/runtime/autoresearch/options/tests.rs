use super::*;
use crate::runtime::{
    market_archive::{capture::Writer, options as capture, test_settings},
    options::chain::tests::catalog,
};
use nivra_core::{
    execution::Quote,
    finance::{Amount, Price},
};
use nivra_store::market_archive::{
    init, ist_midnight,
    options::{
        ContractMaster, GateCandidate, GateObservation, OptionEvidence, OptionPoll, OptionSpot,
        write_option_evidence,
    },
};
use std::time::Instant;

#[test]
#[ignore = "requires TimescaleDB at NIVRA_ARCHIVE_TEST_URL"]
fn option_stage_compares_archive_observations_without_opening_a_paper_book() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut c = crate::runtime::tests::config(dir.path());
    let session = c.session;
    let archive_settings = test_settings(dir.path())?;
    c.market_archive = Some(archive_settings.clone());
    c.nifty_options.enabled = true;
    c.paper_costs_by_segment.insert(
        Segment::NseFo,
        serde_json::from_value(json!({
            "per_order":"20","buy_bps":"0","sell_bps":"0",
            "slippage_bps":"0","rounding_increment":null,
        }))?,
    );
    c.sleeves.options.risk_per_trade_bps = 3000;
    c.sleeves.options.max_position_bps = 10000;
    c.execution.session_start_minute = 555;
    c.execution.session_end_minute = 930;

    let mut db = market_archive::open(&c)?;
    init(&mut db)?;
    // Exercise the version-one upgrade in an isolated fixture database.
    db.batch_execute("DROP TABLE option_evidence; UPDATE schema_version SET version=1")?;
    init(&mut db)?;
    init(&mut db)?;
    let store = Store::open(&dir.path().join("research"))?;
    let at = ist_midnight(session) + 10 * 3_600_000;
    let args = CompareArgs {
        ranking: Ranking::Gate,
        hypothesis: "Frozen picks match the control".into(),
    };
    refresh(&c, &store, session, at, &|| false)?;
    assert_eq!(status(&c, &store)?["reason"], TAPE_UNAVAILABLE);
    assert_eq!(
        compare(&c, &store, &args, at, &|| false)?["reason"],
        TAPE_UNAVAILABLE
    );

    let catalog = catalog("26000");
    let mut contracts = catalog.contracts.values().cloned().collect::<Vec<_>>();
    let price = |v: &str| Price::parse(v).unwrap();
    let expiry = nivra_core::time::ExchangeDate::parse(&session.succ_opt().unwrap().to_string())?;
    for contract in &mut contracts {
        contract.expiry = expiry;
    }
    // The current Nifty shortlist can be 80 CE/PE rows.
    let contracts = (0..8)
        .flat_map(|index| {
            contracts.iter().cloned().map(move |mut contract| {
                contract.key.push_str(&format!("-{index}"));
                contract.strike = Price::try_from(
                    contract
                        .strike
                        .amount()
                        .checked_add(Amount::from_integer(index * 1_000))
                        .unwrap(),
                )
                .unwrap();
                contract
            })
        })
        .collect::<Vec<_>>();
    let instrument_key = contracts[0].key.clone();
    let spot = |receipt| OptionSpot {
        key: catalog.underlying_key.clone(),
        price: price("26000"),
        source_ms: receipt - 10,
        receipt_ms: receipt,
    };
    let master_at = at - 10_000;
    let master = OptionEvidence::Master(ContractMaster {
        contracts: contracts.clone(),
        spot: spot(master_at),
        contract_sha256: "a".repeat(64),
        bod_sha256: "b".repeat(64),
    });
    let writer_dir = dir.path().join(session.to_string());
    std::fs::create_dir(&writer_dir)?;
    let mut worker = Writer::start(&writer_dir, dir.path(), archive_settings.clone())?;
    worker.options.offer(session, master_at, master);
    worker.stop();
    assert_eq!(option_coverage(&mut db, session)?.masters, 1);

    let relation_bytes = |db: &mut nivra_store::market_archive::ArchiveClient| -> Result<i64> {
        Ok(db
            .query_one("SELECT pg_total_relation_size('option_evidence')", &[])?
            .get(0))
    };
    let before = relation_bytes(&mut db)?;
    let build_poll = |step: i64, receipt| {
        let rows = contracts
            .iter()
            .enumerate()
            .map(|(i, contract)| {
                let base = 50 + ((step / 2 + i as i64) % 50);
                let ask = if i == 0 {
                    format!("{base}.10")
                } else {
                    format!("{base}.05")
                };
                let key = &contract.key;
                let bid = base.to_string();
                let row = json!({
                    "instrument_token":key, "timestamp":receipt - 5, "last_price":base,
                    "oi":1_000_000 + step * 17 + i as i64,
                    "depth":{"buy":[{"price":base,"quantity":65}],
                        "sell":[{"price":ask.parse::<f64>().unwrap(),"quantity":65}]},
                });
                let quote = Quote {
                    bid: price(&bid),
                    ask: price(&ask),
                    bid_units: 65,
                    ask_units: 65,
                    source: receipt - 5,
                    receipt,
                };
                capture::observation(key, Some(&row), Some(&quote))
            })
            .collect();
        OptionPoll {
            requested_ms: receipt - 20,
            spot: Some(spot(receipt)),
            rows,
        }
    };
    let polls = 240_i64;
    let start = Instant::now();
    for step in 0..polls {
        let receipt = at + 2_000 * step;
        let poll = build_poll(step, receipt);
        write_option_evidence(&mut db, session, receipt, &OptionEvidence::Poll(poll))?;
    }
    let elapsed = start.elapsed();
    let bytes = relation_bytes(&mut db)? - before;
    println!(
        "option fixture: {polls} polls × {} rows, {bytes} relation bytes, {:.1} ms/poll, {:.1} MiB per 6h15m at a 2 s interval",
        contracts.len(),
        elapsed.as_secs_f64() * 1000.0 / polls as f64,
        bytes as f64 / polls as f64 * 11250.0 / 1048576.0
    );

    let poll = build_poll(0, at);
    let first = &poll.rows[0];
    assert_eq!(first.oi, Some(1_000_000));
    assert_eq!(first.ltp, Some(price("50")));
    assert_eq!(first.source_ms, Some(at - 5));
    assert_eq!(
        capture::observation(&instrument_key, None, None)
            .gap
            .as_deref(),
        Some("no_quote_returned")
    );
    let gate = OptionEvidence::Gate(GateObservation {
        spot: poll.spot,
        candidates: poll.rows[..2]
            .iter()
            .enumerate()
            .map(|(i, row)| GateCandidate {
                instrument_key: row.instrument_key.clone(),
                offset: 0,
                spread_ticks: if i == 0 { 2 } else { 1 },
                depth_lots: 1,
                round_trip_cost_bps: 100.0,
                tradable: true,
                quote: row.quote.clone().unwrap(),
            })
            .collect(),
        input: json!({"triggers":["premium_move"]}),
        verdict: Some(
            json!({"escalate":true,"prospects":[{"instrument_key":instrument_key}],
            "dropped":[{"instrument_key":"NSE_FO|missing","reason":"prospect_not_in_option_universe"}]}),
        ),
    });
    write_option_evidence(&mut db, session, at, &gate)?;
    write_option_evidence(&mut db, session, at, &gate)?;
    let watermark = option_watermark(&mut db, session)?;
    let (gate_at, gate) =
        nivra_store::market_archive::options::option_gates(&mut db, session, watermark)?
            .pop()
            .unwrap();
    assert_eq!(gate_at, at);
    assert_eq!(
        gate.verdict.unwrap()["dropped"][0]["reason"],
        "prospect_not_in_option_universe"
    );
    refresh(&c, &store, session, at + 600_000, &|| false)?;
    assert_eq!(status(&c, &store)?["status"], "ready");
    // An observation that arrived after sealing cannot change which tape either arm sees.
    write_option_evidence(
        &mut db,
        session,
        at + 1_000,
        &capture::gap("late_fixture_gap"),
    )?;
    let newest = option_watermark(&mut db, session)?;
    let revised = replay::read(&mut db, session, newest, &|| false)?;
    assert_eq!(
        replay::compare(&policy(&c)?, &revised, Ranking::Gate)?.reason,
        Some(TAPE_UNAVAILABLE)
    );
    let result = compare(&c, &store, &args, at + 600_010, &|| false)?;
    assert_eq!(result["status"], "compared");
    assert_eq!(result["proposed"], result["frozen_gate"]);
    assert_eq!(result["proposed"]["trades"], 1);
    assert_eq!(result["proposed"]["equity_sleeve_cash"], "0");
    assert_eq!(result["execution_authorized"], false);
    assert_eq!(compare(&c, &store, &args, at + 600_020, &|| false)?, result);
    let alternative = CompareArgs {
        ranking: Ranking::TightestSpread,
        hypothesis: "The tighter spread improves the hypothetical comparison".into(),
    };
    let proposed = compare(&c, &store, &alternative, at + 600_030, &|| false)?;
    assert_eq!(proposed["frozen_gate"], result["frozen_gate"]);
    assert_eq!(proposed["cohort_sha256"], result["cohort_sha256"]);
    assert_eq!(
        proposed["proposed"]["outcomes"][0]["instrument_key"],
        poll.rows[1].instrument_key
    );
    assert_eq!(proposed["hypothetical_difference"], "3.25");
    assert!(!c.directory.join("paper.sqlite").exists());
    let coverage = option_coverage(&mut db, session)?;
    assert_eq!(coverage.polls, polls);
    assert_eq!(coverage.quotes, polls * 80);
    assert_eq!(coverage.gaps, 1);
    assert_eq!(
        status(&c, &store)?["latest_comparison"]["status"],
        "compared"
    );
    Ok(())
}
