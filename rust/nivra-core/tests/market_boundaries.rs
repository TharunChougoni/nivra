//! Market record boundaries: quote domains and the archived receipt-time binding.

use nivra_core::{
    finance::{FinancialError, Price},
    market::{
        AccountSource, ArchivedQuoteReceipt, Instrument, MarketError, PaperMandate, Quote, Segment,
        Snapshot,
    },
    time::Timestamp,
};
use serde_json::{Value, json};

const RECEIVED: &str = "2026-10-15T04:00:00Z";

fn timestamp(value: &str) -> Timestamp {
    Timestamp::parse(value).unwrap()
}

fn instrument_json() -> Value {
    json!({
        "key": "NSE_EQ|FIXTURE", "symbol": "FIXTURE", "kind": "cash",
        "lot_size": 25, "tick_size": "0.05", "version": "fixture-v1",
        "cas_eligible": false, "security_type": "NORMAL"
    })
}

fn quote_json() -> Value {
    json!({
        "instrument": instrument_json(), "bid": "100", "ask": "100.05",
        "bid_units": 100, "ask_units": 100,
        "source_at": RECEIVED, "received_at": RECEIVED
    })
}

fn snapshot_json() -> Value {
    json!({
        "snapshot_id": "fixture-snapshot-v1", "captured_at": RECEIVED,
        "payload_hash": "a".repeat(64), "source": "upstox-full-quotes-v2",
        "quotes": [quote_json()]
    })
}

fn identity_json() -> Value {
    json!({
        "broker": "upstox", "account_alias": "fixture-paper",
        "archive_source": "upstox-readonly-v1", "quote_source": "upstox-full-quotes-v2"
    })
}

fn receipt_json() -> Value {
    json!({
        "capture_id": "b".repeat(64), "identity": identity_json(),
        "requested_at": "2026-10-15T03:59:59.999999Z", "received_at": RECEIVED,
        "normalized_snapshot": snapshot_json()
    })
}

#[test]
fn instrument_identity_covers_lot_tick_version_and_validates_orders() {
    let original: Instrument = serde_json::from_value(instrument_json()).unwrap();
    assert_eq!(original.key(), "NSE_EQ|FIXTURE");
    assert_eq!(original.version(), "fixture-v1");
    // Historic rows omit the new fields: the segment is inferred from the key
    // prefix and the quantity multiplier defaults to 1, so old JSON still parses
    // and re-serializes without sprouting keys an older binary would reject.
    assert_eq!(original.segment(), Some(Segment::NseEq));
    assert_eq!(original.qty_multiplier(), 1);
    let spec = original.contract_spec();
    assert_eq!(spec.lot_size, 25);
    assert_eq!(spec.qty_multiplier, 1);
    assert_eq!(spec.tick, Price::parse("0.05").unwrap());
    assert_eq!(spec.expiry, None);
    assert_eq!(spec.strike, None);
    assert_eq!(spec.option, None);
    assert_eq!(spec.freeze_quantity, None);
    let serialized = serde_json::to_value(&original).unwrap();
    assert!(serialized.get("segment").is_none());
    assert!(serialized.get("qty_multiplier").is_none());
    assert_eq!(
        original.validate_order(25, Price::parse("100.05").unwrap()),
        Ok(())
    );
    assert_eq!(
        original.validate_order(24, Price::parse("100.05").unwrap()),
        Err(FinancialError::InvalidOrderGrid)
    );
    assert_eq!(
        original.validate_order(25, Price::parse("100.01").unwrap()),
        Err(FinancialError::InvalidOrderGrid)
    );
    for (field, replacement) in [
        ("version", json!("fixture-v2")),
        ("lot_size", json!(1)),
        ("tick_size", json!("0.01")),
    ] {
        let mut changed = instrument_json();
        changed[field] = replacement;
        let changed: Instrument = serde_json::from_value(changed).unwrap();
        assert_eq!(changed.key(), original.key());
        assert_ne!(changed, original, "{field} must remain part of identity");
    }
    for (field, replacement) in [
        ("lot_size", json!(0)),
        ("tick_size", json!("0")),
        ("security_type", json!("unknown")),
        // New fields fail closed too: a zero multiplier or a segment that
        // contradicts the key prefix is not a valid identity.
        ("qty_multiplier", json!(0)),
        ("segment", json!("NSE_FO")),
    ] {
        let mut value = instrument_json();
        value[field] = replacement;
        assert!(
            serde_json::from_value::<Instrument>(value).is_err(),
            "{field}"
        );
    }

    // D5: Indian instruments expansion (Future, Etf), display models and derivatives lockout.
    use nivra_core::market::{
        FoChainStrike, FoChainView, InstrumentKind, InstrumentMargin, InstrumentRef, LotQuantity,
    };
    assert!(!original.is_derivative());
    assert!(!original.kind().display_only());
    // One admission helper: paper execution admits Cash lot-1 only.
    assert_eq!(
        original.validate_paper_execution(),
        Err(FinancialError::InvalidOrderGrid)
    );
    assert_eq!(
        PaperMandate::CashEquity.admit(&original),
        Err(FinancialError::InvalidOrderGrid)
    );
    let mut cash_lot_one = instrument_json();
    cash_lot_one["lot_size"] = json!(1);
    let cash_lot_one: Instrument = serde_json::from_value(cash_lot_one).unwrap();
    assert_eq!(cash_lot_one.validate_paper_execution(), Ok(()));
    assert_eq!(PaperMandate::CashEquity.admit(&cash_lot_one), Ok(()));

    let mut etf_json = instrument_json();
    etf_json["key"] = json!("NSE_EQ|NIFTYBEES");
    etf_json["symbol"] = json!("NIFTYBEES");
    etf_json["kind"] = json!("etf");
    let etf: Instrument = serde_json::from_value(etf_json).unwrap();
    assert_eq!(etf.kind(), InstrumentKind::Etf);
    assert!(!etf.is_derivative());
    assert!(etf.kind().display_only());
    assert_eq!(
        etf.validate_paper_execution(),
        Err(FinancialError::InvalidOrderGrid)
    );

    let mut fut_json = instrument_json();
    fut_json["key"] = json!("NSE_FO|NIFTY26NOVFUT");
    fut_json["symbol"] = json!("NIFTY26NOVFUT");
    fut_json["kind"] = json!("future");
    fut_json["expiry"] = json!("2026-11-26");
    fut_json["underlying"] = json!("NIFTY");
    fut_json["lot_size"] = json!(50);
    fut_json["cas_eligible"] = json!(null);
    fut_json["security_type"] = json!(null);
    // Explicit segment and multiplier are the F&O shape (NSE_FO lot 50 here).
    fut_json["segment"] = json!("NSE_FO");
    fut_json["qty_multiplier"] = json!(1);
    let fut: Instrument = serde_json::from_value(fut_json.clone()).unwrap();
    assert_eq!(fut.kind(), InstrumentKind::Future);
    assert_eq!(fut.segment(), Some(Segment::NseFo));
    assert_eq!(fut.qty_multiplier(), 1);
    assert_eq!(fut.contract_spec().expiry, fut.expiry());
    assert!(fut.kind().display_only());
    assert!(fut.is_derivative());
    assert_eq!(
        fut.validate_paper_execution(),
        Err(FinancialError::InvalidOrderGrid)
    );

    // An index key keeps the space in the instrument id and carries no contract.
    let index: Instrument = serde_json::from_value(json!({
        "key": "NSE_INDEX|Nifty 50", "symbol": "NIFTY", "kind": "index",
        "lot_size": 1, "tick_size": "0.05", "version": "fixture-v1",
        "segment": "NSE_INDEX"
    }))
    .unwrap();
    assert_eq!(index.segment(), Some(Segment::NseIndex));
    assert_eq!(index.qty_multiplier(), 1);
    assert_eq!(index.contract_spec().option, None);
    assert!(index.kind().display_only());
    // Index data is non-tradeable: paper execution refuses it.
    assert_eq!(
        index.validate_paper_execution(),
        Err(FinancialError::InvalidOrderGrid)
    );
    assert_eq!(
        PaperMandate::CashEquity.admit(&index),
        Err(FinancialError::InvalidOrderGrid)
    );
    let mut bad_index = json!({
        "key": "NSE_INDEX|Nifty 50", "symbol": "NIFTY", "kind": "cash",
        "lot_size": 1, "tick_size": "0.05", "version": "fixture-v1"
    });
    assert!(serde_json::from_value::<Instrument>(bad_index.clone()).is_err());
    bad_index["kind"] = json!("index");
    bad_index["segment"] = json!("NSE_EQ");
    assert!(serde_json::from_value::<Instrument>(bad_index).is_err());

    // Future cannot have a strike
    fut_json["strike"] = json!("24000");
    assert!(serde_json::from_value::<Instrument>(fut_json).is_err());

    // InstrumentRef display row
    let iref = InstrumentRef::from_instrument(&fut, "NSE", "NFO");
    assert_eq!(iref.key, "NSE_FO|NIFTY26NOVFUT");
    assert_eq!(iref.exchange, "NSE");
    assert_eq!(iref.segment, "NFO");
    assert_eq!(iref.strike, None);
    let iref_val = serde_json::to_value(&iref).unwrap();
    assert_eq!(iref_val["strike"], serde_json::Value::Null);

    let lots = LotQuantity::from_lots(2, 50).unwrap();
    assert_eq!(lots.lots, 2);
    assert_eq!(lots.total_units, 100);
    let units = LotQuantity::from_units(150, 50).unwrap();
    assert_eq!(units.lots, 3);
    assert!(LotQuantity::from_units(55, 50).is_err());

    let margin = InstrumentMargin::new_display(
        "NSE_FO|NIFTY26NOVFUT",
        Some(nivra_core::finance::Amount::from_integer(100_000)),
        Some(nivra_core::finance::Amount::from_integer(20_000)),
        nivra_core::finance::Amount::from_integer(120_000),
    );
    assert!(margin.display_only);
    assert_eq!(
        margin.total_margin,
        nivra_core::finance::Amount::from_integer(120_000)
    );

    let chain = FoChainView::new(
        "NIFTY",
        serde_json::from_value(json!("2026-11-26")).unwrap(),
        Some(Price::parse("24500").unwrap()),
        vec![iref],
        vec![FoChainStrike {
            strike: Price::parse("24500").unwrap(),
            call: None,
            put: None,
        }],
    );
    assert!(chain.display_only);
    assert_eq!(chain.strikes.len(), 1);

    // Named session calendars: the default cash session is the NSE cash name;
    // CDS and MCX exist only as documented names and are not activated.
    use nivra_core::market::{Calendar, SessionCalendarId};
    use nivra_core::time::{ExchangeDate, LocalTime};
    assert_eq!(SessionCalendarId::NseCash.as_str(), "nse_cash");
    assert_eq!(SessionCalendarId::NseFo.as_str(), "nse_fo");
    assert_eq!(SessionCalendarId::Cds.as_str(), "cds");
    assert_eq!(SessionCalendarId::McxNonAgri.as_str(), "mcx_non_agri");
    for (calendar, opening, closing) in [
        (SessionCalendarId::NseCash, "09:15:00", "15:30:00"),
        (SessionCalendarId::NseFo, "09:15:00", "15:30:00"),
        (SessionCalendarId::Cds, "09:00:00", "17:00:00"),
        (SessionCalendarId::McxNonAgri, "09:00:00", "23:55:00"),
    ] {
        assert_eq!(
            calendar.hours(),
            (
                LocalTime::parse(opening).unwrap(),
                LocalTime::parse(closing).unwrap()
            ),
            "{}",
            calendar.as_str()
        );
    }
    let calendar: Calendar = serde_json::from_value(json!({
        "source": "fixture-calendar", "valid_from": "2026-10-15", "valid_to": "2026-10-31"
    }))
    .unwrap();
    let weekday: ExchangeDate = serde_json::from_value(json!("2026-10-16")).unwrap();
    let weekend: ExchangeDate = serde_json::from_value(json!("2026-10-17")).unwrap();
    assert_eq!(
        calendar.hours(weekday).unwrap(),
        Some(SessionCalendarId::NseCash.hours())
    );
    assert_eq!(calendar.hours(weekend).unwrap(), None);

    // Phase 2: buy-only long Nifty option admission. Structural identity,
    // lot grid, buy-to-open, sell-against-holding and no hold past expiry.
    use nivra_core::market::{NiftyOptionMandate, OptionCatalog, OptionContract, OptionOrder};
    let option: OptionContract = serde_json::from_value(json!({
        "key": "NSE_FO|26000CE", "symbol": "NIFTY 26000 CE 20 OCT 26",
        "underlying_key": "NSE_INDEX|Nifty 50", "underlying_symbol": "NIFTY",
        "expiry": "2026-10-20", "strike": "26000", "option": "call",
        "lot_size": 65, "tick": "0.05", "freeze_quantity": 1800, "weekly": true
    }))
    .unwrap();
    option.validate().unwrap();
    let option_instrument = option.instrument().unwrap();
    assert_eq!(option_instrument.kind(), InstrumentKind::Call);
    assert_eq!(option_instrument.segment(), Some(Segment::NseFo));
    assert_eq!(option_instrument.lot_size(), 65);
    assert_eq!(option_instrument.tick_size(), Price::parse("0.05").unwrap());
    assert_eq!(
        option_instrument.expiry(),
        Some(serde_json::from_value(json!("2026-10-20")).unwrap())
    );
    assert_eq!(option_instrument.underlying(), Some("NIFTY"));
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit(&option_instrument),
        Ok(())
    );
    assert_eq!(
        PaperMandate::CashEquity.admit(&option_instrument),
        Err(FinancialError::InvalidOrderGrid)
    );
    // Futures, index keys and cash are not long options.
    for wrong in [&fut, &index, &cash_lot_one] {
        assert_eq!(
            PaperMandate::NiftyOptionLong.admit(wrong),
            Err(FinancialError::InvalidOptionMandate)
        );
    }
    let mandate = NiftyOptionMandate {
        underlying_key: "NSE_INDEX|Nifty 50",
        underlying_symbol: "NIFTY",
    };
    let order = |buy, units, held_units, today| OptionOrder {
        contract: &option,
        buy,
        units,
        held_units,
        today: serde_json::from_value(json!(today)).unwrap(),
    };
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(true, 65, 0, "2026-10-16"), &mandate),
        Ok(())
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(true, 130, 0, "2026-10-16"), &mandate),
        Ok(())
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(true, 64, 0, "2026-10-16"), &mandate),
        Err(FinancialError::OptionQuantityGrid)
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(true, 0, 0, "2026-10-16"), &mandate),
        Err(FinancialError::OptionQuantityGrid)
    );
    // Opening on expiry day is refused; a same-day sell is the flatten path.
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(true, 65, 0, "2026-10-20"), &mandate),
        Err(FinancialError::OptionExpiry)
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(false, 65, 65, "2026-10-20"), &mandate),
        Ok(())
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(false, 65, 0, "2026-10-16"), &mandate),
        Err(FinancialError::OptionShortOpen)
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(false, 130, 65, "2026-10-16"), &mandate),
        Err(FinancialError::OptionShortOpen)
    );
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(false, 65, 65, "2026-10-21"), &mandate),
        Err(FinancialError::OptionExpiry)
    );
    let other = NiftyOptionMandate {
        underlying_key: "NSE_INDEX|Nifty Bank",
        underlying_symbol: "BANKNIFTY",
    };
    assert_eq!(
        PaperMandate::NiftyOptionLong.admit_option(&order(true, 65, 0, "2026-10-16"), &other),
        Err(FinancialError::OptionUnderlyingMismatch)
    );
    // The bounded catalog refuses duplicate identities and a past expiry.
    let catalog = OptionCatalog::try_new(
        weekday,
        "NSE_INDEX|Nifty 50",
        "NIFTY",
        option.expiry,
        Price::parse("26000").unwrap(),
        [option.clone()],
    )
    .unwrap();
    assert_eq!(catalog.contracts.len(), 1);
    assert_eq!(catalog.mandate().underlying_symbol, "NIFTY");
    assert!(
        OptionCatalog::try_new(
            weekday,
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            option.expiry,
            Price::parse("26000").unwrap(),
            [option.clone(), option.clone()],
        )
        .is_err()
    );
    assert!(
        OptionCatalog::try_new(
            weekday,
            "NSE_INDEX|Nifty 50",
            "NIFTY",
            weekday.previous_day().unwrap(),
            Price::parse("26000").unwrap(),
            [option.clone()],
        )
        .is_err()
    );
}

#[test]
fn quote_structural_validity_is_separate_from_execution_and_freshness() {
    let mut locked = quote_json();
    locked["ask"] = json!("100");
    locked["bid_units"] = json!(0);
    locked["ask_units"] = json!(0);
    let quote: Quote = serde_json::from_value(locked).unwrap();
    assert_eq!(quote.bid(), quote.ask());
    assert_eq!(quote.bid_units(), 0);
    assert_eq!(quote.ask_units(), 0);
    assert!(quote.fresh(timestamp(RECEIVED)));
    assert!(quote.fresh(timestamp("2026-10-15T04:00:02Z")));
    assert!(!quote.fresh(timestamp("2026-10-15T04:00:02.000001Z")));
    assert!(!quote.fresh(timestamp("2026-10-15T03:59:59.999999Z")));
    for (field, bad) in [
        ("bid", json!("100.10")),
        ("ask", json!("0")),
        ("ask_units", json!(true)),
        ("source_at", json!("2026-10-15T04:00:00.000001Z")),
    ] {
        let mut value = quote_json();
        value[field] = bad;
        assert!(serde_json::from_value::<Quote>(value).is_err(), "{field}");
    }
}

#[test]
fn receipt_binding_compares_versions_prices_snapshot_and_identity() {
    let original: Snapshot = serde_json::from_value(snapshot_json()).unwrap();
    let receipt: ArchivedQuoteReceipt = serde_json::from_value(receipt_json()).unwrap();
    let expected: AccountSource = serde_json::from_value(identity_json()).unwrap();
    let bound = original.bind_receipt(&receipt, &expected).unwrap();
    assert_eq!(bound.snapshot(), &original);
    assert_eq!(bound.capture_id(), "b".repeat(64));
    assert_eq!(bound.received_at(), timestamp(RECEIVED));
    for (path, replacement) in [
        ("/quotes/0/bid", json!("99.95")),
        ("/quotes/0/instrument/version", json!("fixture-v2")),
        ("/quotes/0/instrument/tick_size", json!("0.01")),
        ("/payload_hash", json!("c".repeat(64))),
        ("/source", json!("another-quote-source")),
    ] {
        let mut changed = snapshot_json();
        *changed.pointer_mut(path).unwrap() = replacement;
        let changed: Snapshot = serde_json::from_value(changed).unwrap();
        assert_eq!(
            changed.bind_receipt(&receipt, &expected).unwrap_err(),
            MarketError::ReceiptMismatch,
            "{path}"
        );
    }
    for field in ["broker", "account_alias", "quote_source"] {
        let mut other = identity_json();
        other[field] = json!("other");
        let other: AccountSource = serde_json::from_value(other).unwrap();
        assert_eq!(
            original.bind_receipt(&receipt, &other).unwrap_err(),
            MarketError::ReceiptMismatch,
            "{field}"
        );
    }
}

#[test]
fn bound_freshness_uses_inclusive_two_seconds_and_rejects_clock_reversal() {
    let snapshot: Snapshot = serde_json::from_value(snapshot_json()).unwrap();
    let receipt: ArchivedQuoteReceipt = serde_json::from_value(receipt_json()).unwrap();
    let expected: AccountSource = serde_json::from_value(identity_json()).unwrap();
    let bound = snapshot.bind_receipt(&receipt, &expected).unwrap();
    assert_eq!(bound.check_fresh(timestamp(RECEIVED)), Ok(()));
    assert_eq!(bound.check_fresh(timestamp("2026-10-15T04:00:02Z")), Ok(()));
    for now in ["2026-10-15T04:00:02.000001Z", "2026-10-15T03:59:59.999999Z"] {
        assert_eq!(
            bound.check_fresh(timestamp(now)),
            Err(MarketError::FutureOrStaleReceipt)
        );
    }
    let mut stale = receipt_json();
    stale["normalized_snapshot"]["quotes"][0]["source_at"] = json!("2026-10-15T03:59:57.999999Z");
    let snapshot: Snapshot = serde_json::from_value(stale["normalized_snapshot"].clone()).unwrap();
    let receipt: ArchivedQuoteReceipt = serde_json::from_value(stale).unwrap();
    let bound = snapshot.bind_receipt(&receipt, &expected).unwrap();
    assert_eq!(
        bound.check_fresh(timestamp(RECEIVED)),
        Err(MarketError::FutureOrStaleQuote)
    );
}

#[test]
fn later_clock_or_quote_receipt_cannot_renew_original_archive_projection() {
    let receipt: ArchivedQuoteReceipt = serde_json::from_value(receipt_json()).unwrap();
    let expected: AccountSource = serde_json::from_value(identity_json()).unwrap();
    for renew_quotes in [false, true] {
        let mut changed = snapshot_json();
        changed["captured_at"] = json!("2026-10-15T04:00:00.000001Z");
        if renew_quotes {
            changed["quotes"][0]["received_at"] = json!("2026-10-15T04:00:00.000001Z");
        }
        let changed: Snapshot = serde_json::from_value(changed).unwrap();
        assert_eq!(
            changed.bind_receipt(&receipt, &expected).unwrap_err(),
            MarketError::ReceiptMismatch
        );
    }
}
