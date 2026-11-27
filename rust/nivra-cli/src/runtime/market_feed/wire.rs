use nivra_core::{
    execution::Quote,
    finance::Price,
    intraday::{Clock, Features, Levels, Receipt, integral_quantity},
};
use prost::Message;
use std::collections::{BTreeMap, BTreeSet};

// Wire tags copied from the adjacent reviewed provider proto. Unknown fields
// are skipped. A feed key is the identity; the wire carries no stock symbol.
#[derive(Clone, PartialEq, Message)]
pub(super) struct Ltpc {
    #[prost(double, tag = "1")]
    pub(super) ltp: f64,
    #[prost(int64, tag = "2")]
    pub(super) ltt: i64,
    #[prost(int64, tag = "3")]
    pub(super) ltq: i64,
    #[prost(double, tag = "4")]
    pub(super) cp: f64,
}
#[derive(Clone, PartialEq, Message)]
pub(super) struct Feed {
    #[prost(oneof = "FeedKind", tags = "1, 2, 3")]
    pub(super) kind: Option<FeedKind>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub(super) enum FeedKind {
    #[prost(message, tag = "1")]
    Ltpc(Ltpc),
    #[prost(message, tag = "2")]
    Full(FullFeed),
    #[prost(message, tag = "3")]
    FirstLevel(FirstLevel),
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct FullFeed {
    // IndexFullFeed (tag 2) carries no executable depth.
    #[prost(message, optional, tag = "1")]
    market: Option<MarketFullFeed>,
}

#[derive(Clone, PartialEq, Message)]
struct MarketFullFeed {
    #[prost(message, optional, tag = "1")]
    ltpc: Option<Ltpc>,
    #[prost(message, optional, tag = "2")]
    levels: Option<MarketLevel>,
    #[prost(double, tag = "5")]
    atp: f64,
    #[prost(int64, tag = "6")]
    vtt: i64,
    #[prost(double, tag = "9")]
    tbq: f64,
    #[prost(double, tag = "10")]
    tsq: f64,
}

#[derive(Clone, PartialEq, Message)]
struct MarketLevel {
    #[prost(message, repeated, tag = "1")]
    levels: Vec<BookLevel>,
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct FirstLevel {
    #[prost(message, optional, tag = "1")]
    ltpc: Option<Ltpc>,
    #[prost(message, optional, tag = "2")]
    depth: Option<BookLevel>,
    #[prost(int64, tag = "4")]
    vtt: i64,
}

#[derive(Clone, PartialEq, Message)]
struct BookLevel {
    #[prost(int64, tag = "1")]
    bid_units: i64,
    #[prost(double, tag = "2")]
    bid: f64,
    #[prost(int64, tag = "3")]
    ask_units: i64,
    #[prost(double, tag = "4")]
    ask: f64,
}

impl BookLevel {
    fn levels(&self) -> Levels {
        Levels {
            bid: self
                .bid_units
                .try_into()
                .ok()
                .map(|units| (self.bid, units)),
            ask: self
                .ask_units
                .try_into()
                .ok()
                .map(|units| (self.ask, units)),
        }
    }

    fn quote(&self, clock: Clock) -> Option<Quote> {
        Some(Quote {
            bid: Price::parse(&self.bid.to_string()).ok()?,
            ask: Price::parse(&self.ask.to_string()).ok()?,
            bid_units: self.bid_units.try_into().ok()?,
            ask_units: self.ask_units.try_into().ok()?,
            source: clock.source,
            receipt: clock.receipt,
        })
    }
}

#[derive(Clone, PartialEq, Message)]
pub(super) struct Response {
    #[prost(int32, tag = "1")]
    pub(super) kind: i32,
    #[prost(btree_map = "string, message", tag = "2")]
    pub(super) feeds: BTreeMap<String, Feed>,
    #[prost(int64, tag = "3")]
    pub(super) current_ts: i64,
}
#[derive(Clone)]
pub(in crate::runtime) struct Point {
    pub price: Price,
    pub source: i64,
    pub receipt: i64,
    pub last_trade: i64,
    pub prior_close: Option<Price>,
    pub depth: Option<Features>,
}
/// Decoded frame: accepted points plus instruments skipped for an invalid LTPC.
/// One bad instrument (e.g. no trade yet at the open) never rejects the frame.
pub(super) struct Decoded {
    pub(super) points: BTreeMap<String, Point>,
    pub(super) invalid: u64,
}
pub(super) fn decode(
    bytes: &[u8],
    receipt: i64,
    skew_ms: i64,
    allowed: &BTreeSet<String>,
) -> std::result::Result<Decoded, &'static str> {
    let limit = receipt.saturating_add(skew_ms);
    if bytes.len() > 1_000_000 {
        return Err("feed_message_too_large");
    }
    let response = Response::decode(bytes).map_err(|_| "feed_protobuf_invalid")?;
    if !matches!(response.kind, 0..=2) {
        return Err("feed_message_type_invalid");
    }
    if response.kind == 2 {
        return Ok(Decoded {
            points: BTreeMap::new(),
            invalid: 0,
        });
    }
    if response.current_ts <= 0 || response.current_ts > limit {
        return Err("feed_source_time_invalid");
    }
    let mut points = BTreeMap::new();
    let mut invalid = 0u64;
    for (key, feed) in response.feeds {
        if !allowed.contains(&key) {
            continue;
        }
        let clock = Clock {
            source: response.current_ts,
            receipt,
        };
        let (ltpc, depth) = match feed.kind {
            Some(FeedKind::Ltpc(ltpc)) => (Some(ltpc), None),
            Some(FeedKind::Full(FullFeed {
                market: Some(market),
            })) => {
                let level = market.levels.as_ref().and_then(|book| book.levels.first());
                (
                    market.ltpc,
                    Some(Receipt {
                        clock,
                        vwap: Some(market.atp),
                        volume: market.vtt.try_into().ok(),
                        buy_quantity: integral_quantity(market.tbq),
                        sell_quantity: integral_quantity(market.tsq),
                        touch: level.and_then(|level| level.quote(clock)),
                        levels: level.map(BookLevel::levels).unwrap_or_default(),
                    }),
                )
            }
            Some(FeedKind::FirstLevel(level)) => (
                level.ltpc,
                Some(Receipt {
                    clock,
                    vwap: None,
                    volume: level.vtt.try_into().ok(),
                    buy_quantity: None,
                    sell_quantity: None,
                    touch: level.depth.as_ref().and_then(|level| level.quote(clock)),
                    levels: level
                        .depth
                        .as_ref()
                        .map(BookLevel::levels)
                        .unwrap_or_default(),
                }),
            ),
            _ => continue,
        };
        let Some(ltpc) = ltpc else {
            invalid += 1;
            continue;
        };
        if !ltpc.ltp.is_finite()
            || ltpc.ltp <= 0.0
            || ltpc.ltt < 0
            || ltpc.ltt > limit
            || ltpc.ltq < 0
        {
            invalid += 1;
            continue;
        }
        let Ok(price) = Price::parse(&ltpc.ltp.to_string()) else {
            invalid += 1;
            continue;
        };
        let prior_close = if ltpc.cp.is_finite() && ltpc.cp > 0.0 {
            Price::parse(&ltpc.cp.to_string()).ok()
        } else {
            None
        };
        points.insert(
            key,
            Point {
                price,
                // Never let a skewed source time sit ahead of its own receipt.
                source: response.current_ts.min(receipt),
                receipt,
                last_trade: ltpc.ltt,
                prior_close,
                depth: depth
                    .filter(|_| clock.source <= clock.receipt)
                    .map(Features::reduce),
            },
        );
    }
    Ok(Decoded { points, invalid })
}
