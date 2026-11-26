use super::*;
mod commit;
mod history_attempts;
mod quote_reads;
mod recovery;
mod resume;
mod selection;
mod shadow_scoring;
mod stop_requests;
mod strategy_cycle;

fn start() -> i64 {
    DateTime::parse_from_rfc3339("2026-10-16T09:15:00+05:30")
        .unwrap()
        .timestamp_millis()
}
fn instrument(n: usize) -> Instrument {
    Instrument {
        key: format!("NSE_EQ|INE{n:09}"),
        symbol: format!("S{n}"),
        series: "EQ".into(),
        tick: Price::parse("0.05").unwrap(),
    }
}
fn quote(at: i64, price: i64) -> Quote {
    Quote {
        bid: Price::try_from(Amount::from_integer(price)).unwrap(),
        ask: Price::try_from(Amount::from_integer(price).checked_add(a("0.05")).unwrap()).unwrap(),
        bid_units: 10000,
        ask_units: 10000,
        source: at,
        receipt: at,
    }
}
