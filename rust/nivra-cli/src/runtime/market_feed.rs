//! Upstox V3 screening prices. LTPC never becomes an executable depth quote.
use super::*;
use nivra_core::{
    execution::{Instrument, Scanner},
    finance::Price,
};
use prost::Message;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::{TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tungstenite::{Message as WsMessage, WebSocket, stream::MaybeTlsStream};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mode {
    Rest,
    Ltpc,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub mode: Mode,
    pub depth_mode: String,
    pub max_connections: usize,
    #[serde(skip_serializing_if = "is_default_max_connect_addresses")]
    pub max_connect_addresses: usize,
    #[serde(skip_serializing_if = "is_default_connect_timeout_ms")]
    pub connect_timeout_ms: u64,
    #[serde(skip_serializing_if = "is_default_handshake_io_timeout_ms")]
    pub handshake_io_timeout_ms: u64,
    #[serde(skip_serializing_if = "is_default_read_poll_timeout_ms")]
    pub read_poll_timeout_ms: u64,
    #[serde(skip_serializing_if = "is_default_worker_idle_sleep_ms")]
    pub worker_idle_sleep_ms: u64,
    pub shortlist_size: usize,
    pub tick_ms: i64,
    pub stale_age_ms: i64,
    #[serde(skip_serializing_if = "is_default_scan_activity_timeout_ms")]
    pub scan_activity_timeout_ms: i64,
    #[serde(skip_serializing_if = "is_default_fresh_drop_alert_fraction")]
    pub fresh_drop_alert_fraction: f64,
    #[serde(skip_serializing_if = "is_default_telemetry_window_ms")]
    pub telemetry_window_ms: i64,
    pub metadata_poll_ms: i64,
    pub metadata_stale_ms: i64,
    #[serde(skip_serializing_if = "is_default_metadata_preload_max_age_ms")]
    pub metadata_preload_max_age_ms: i64,
    pub depth_poll_ms: i64,
    pub fallback_poll_ms: i64,
    pub rest_batch_size: usize,
    pub reconnect_initial_ms: i64,
    pub reconnect_max_ms: i64,
    pub heartbeat_timeout_ms: i64,
    pub token_check_ms: i64,
    /// Tolerated lead of provider/exchange timestamps over the local receipt clock.
    pub source_clock_skew_ms: i64,
}
/// Settings added after sealed policies existed are omitted at their defaults so
/// planner policy hashes stay stable across upgrades (#66).
fn is_default_max_connect_addresses(value: &usize) -> bool {
    *value == 2
}
fn is_default_connect_timeout_ms(value: &u64) -> bool {
    *value == 3000
}
fn is_default_handshake_io_timeout_ms(value: &u64) -> bool {
    *value == 3000
}
fn is_default_read_poll_timeout_ms(value: &u64) -> bool {
    *value == 250
}
fn is_default_worker_idle_sleep_ms(value: &u64) -> bool {
    *value == 25
}
fn is_default_scan_activity_timeout_ms(value: &i64) -> bool {
    *value == 30000
}
fn is_default_fresh_drop_alert_fraction(value: &f64) -> bool {
    *value == 0.5
}
fn is_default_telemetry_window_ms(value: &i64) -> bool {
    *value == 60000
}
fn is_default_metadata_preload_max_age_ms(value: &i64) -> bool {
    *value == 900000
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Rest,
            depth_mode: "rest".into(),
            max_connections: 1,
            max_connect_addresses: 2,
            connect_timeout_ms: 3000,
            handshake_io_timeout_ms: 3000,
            read_poll_timeout_ms: 250,
            worker_idle_sleep_ms: 25,
            shortlist_size: 40,
            tick_ms: 2000,
            stale_age_ms: 15000,
            scan_activity_timeout_ms: 30000,
            fresh_drop_alert_fraction: 0.5,
            telemetry_window_ms: 60000,
            metadata_poll_ms: 8000,
            metadata_stale_ms: 300000,
            metadata_preload_max_age_ms: 900000,
            depth_poll_ms: 8000,
            fallback_poll_ms: 8000,
            rest_batch_size: 200,
            reconnect_initial_ms: 1000,
            reconnect_max_ms: 60000,
            heartbeat_timeout_ms: 30000,
            token_check_ms: 1000,
            source_clock_skew_ms: 1000,
        }
    }
}
impl Settings {
    pub fn rest_interval_ms(&self, fallback: bool) -> i64 {
        if fallback {
            self.fallback_poll_ms
        } else {
            self.metadata_poll_ms
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !(self.fresh_drop_alert_fraction > 0.0
            && self.fresh_drop_alert_fraction <= 1.0
            && self.depth_mode == "rest"
            && (1..=2).contains(&self.max_connections)
            && (1..=64).contains(&self.max_connect_addresses))
            || [
                self.connect_timeout_ms,
                self.handshake_io_timeout_ms,
                self.read_poll_timeout_ms,
                self.worker_idle_sleep_ms,
            ]
            .into_iter()
            .any(|n| !(1..=86_400_000).contains(&n))
            || self.shortlist_size == 0
            || self.shortlist_size > 200
            || self.rest_batch_size == 0
            || self.rest_batch_size > 200
            || [
                self.tick_ms,
                self.stale_age_ms,
                self.scan_activity_timeout_ms,
                self.telemetry_window_ms,
                self.metadata_poll_ms,
                self.metadata_stale_ms,
                self.depth_poll_ms,
                self.fallback_poll_ms,
                self.reconnect_initial_ms,
                self.reconnect_max_ms,
                self.heartbeat_timeout_ms,
                self.token_check_ms,
            ]
            .into_iter()
            .any(|n| !(100..=86_400_000).contains(&n))
            || self.reconnect_max_ms < self.reconnect_initial_ms
            || !(0..=86_400_000).contains(&self.metadata_preload_max_age_ms)
            || self.metadata_stale_ms < self.metadata_poll_ms
            || !(0..=5000).contains(&self.source_clock_skew_ms)
        {
            return Err("market_feed_settings_invalid".into());
        }
        Ok(())
    }
}
// Wire tags copied from the adjacent reviewed provider proto. Unknown fields
// (including auctions/full feeds) are skipped; no provider-side symbols exist.
#[derive(Clone, PartialEq, Message)]
struct Ltpc {
    #[prost(double, tag = "1")]
    ltp: f64,
    #[prost(int64, tag = "2")]
    ltt: i64,
    #[prost(int64, tag = "3")]
    ltq: i64,
    #[prost(double, tag = "4")]
    cp: f64,
}
#[derive(Clone, PartialEq, Message)]
struct Feed {
    #[prost(message, optional, tag = "1")]
    ltpc: Option<Ltpc>,
}
#[derive(Clone, PartialEq, Message)]
struct Response {
    #[prost(int32, tag = "1")]
    kind: i32,
    #[prost(btree_map = "string, message", tag = "2")]
    feeds: BTreeMap<String, Feed>,
    #[prost(int64, tag = "3")]
    current_ts: i64,
}
#[derive(Clone)]
pub(super) struct Point {
    pub price: Price,
    pub source: i64,
    pub receipt: i64,
    pub last_trade: i64,
    pub prior_close: Option<Price>,
}
/// Decoded frame: accepted points plus instruments skipped for an invalid LTPC.
/// One bad instrument (e.g. no trade yet at the open) never rejects the frame.
struct Decoded {
    points: BTreeMap<String, Point>,
    invalid: u64,
}
fn decode(
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
        let Some(ltpc) = feed.ltpc else {
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
            },
        );
    }
    Ok(Decoded { points, invalid })
}

enum Poll {
    Data(Vec<u8>, i64),
    Heartbeat,
    Idle,
}
trait Transport {
    fn revision(&self) -> std::result::Result<String, &'static str>;
    fn connect(&mut self, keys: &[String]) -> std::result::Result<(), &'static str>;
    fn poll(&mut self) -> std::result::Result<Poll, &'static str>;
    fn close(&mut self);
}
struct Network {
    settings: Settings,
    client: data::Client,
    config: Config,
    socket: Option<WebSocket<MaybeTlsStream<TcpStream>>>,
}
impl Transport for Network {
    fn revision(&self) -> std::result::Result<String, &'static str> {
        data::application_token(&self.config)
            .map(|token| digest(token.as_bytes()))
            .map_err(|_| "feed_credential_unavailable")
    }
    fn connect(&mut self, keys: &[String]) -> std::result::Result<(), &'static str> {
        // A new single-use URL is authorized on every attempt. Never retain/log it.
        self.close();
        let token =
            data::application_token(&self.config).map_err(|_| "feed_credential_unavailable")?;
        data::reserve(&self.config, millis())
            .map_err(|_| "feed_authorization_quota_or_journal_blocked")?;
        let body = self
            .client
            .get(
                "https://api.upstox.com/v3/feed/market-data-feed/authorize",
                Some(&token),
                16384,
            )
            .map_err(|_| "feed_authorization_failed")?;
        let value: Value = serde_json::from_str(&body).map_err(|_| "feed_authorization_invalid")?;
        if value["status"] != "success" {
            return Err("feed_authorization_failed");
        }
        let url = value["data"]["authorized_redirect_uri"]
            .as_str()
            .ok_or("feed_authorization_invalid")?;
        let uri: ureq::http::Uri = url.parse().map_err(|_| "feed_authorization_invalid")?;
        let host = uri
            .host()
            .filter(|h| h.ends_with(".upstox.com"))
            .ok_or("feed_authorization_host_invalid")?;
        if uri.scheme_str() != Some("wss")
            || uri.authority().is_some_and(|a| a.as_str().contains('@'))
            || uri.port_u16().is_some_and(|p| p != 443)
        {
            return Err("feed_authorization_host_invalid");
        }
        let addresses = (host, 443)
            .to_socket_addrs()
            .map_err(|_| "feed_connect_failed")?;
        let mut stream = None;
        for address in addresses.take(self.settings.max_connect_addresses) {
            if let Ok(connection) = TcpStream::connect_timeout(
                &address,
                Duration::from_millis(self.settings.connect_timeout_ms),
            ) {
                stream = Some(connection);
                break;
            }
        }
        let stream = stream.ok_or("feed_connect_failed")?;
        stream
            .set_read_timeout(Some(Duration::from_millis(
                self.settings.handshake_io_timeout_ms,
            )))
            .map_err(|_| "feed_socket_timeout_failed")?;
        stream
            .set_write_timeout(Some(Duration::from_millis(
                self.settings.handshake_io_timeout_ms,
            )))
            .map_err(|_| "feed_socket_timeout_failed")?;
        let config = tungstenite::protocol::WebSocketConfig::default()
            .max_message_size(Some(1_000_000))
            .max_frame_size(Some(1_000_000));
        let (mut socket, _) = tungstenite::client_tls_with_config(url, stream, Some(config), None)
            .map_err(|_| "feed_handshake_failed")?;
        match socket.get_mut() {
            MaybeTlsStream::Plain(s) => s.set_read_timeout(Some(Duration::from_millis(
                self.settings.read_poll_timeout_ms,
            ))),
            MaybeTlsStream::Rustls(s) => s.sock.set_read_timeout(Some(Duration::from_millis(
                self.settings.read_poll_timeout_ms,
            ))),
            _ => return Err("feed_tls_unsupported"),
        }
        .map_err(|_| "feed_socket_timeout_failed")?;
        let message = json!({"guid":format!("nivra-{}",millis()),"method":"sub","data":{"mode":"ltpc","instrumentKeys":keys}});
        socket
            .send(WsMessage::Binary(
                serde_json::to_vec(&message)
                    .map_err(|_| "feed_subscription_invalid")?
                    .into(),
            ))
            .map_err(|_| "feed_subscribe_failed")?;
        self.socket = Some(socket);
        Ok(())
    }
    fn poll(&mut self) -> std::result::Result<Poll, &'static str> {
        let socket = self.socket.as_mut().ok_or("feed_disconnected")?;
        match socket.read() {
            Ok(WsMessage::Binary(bytes)) => Ok(Poll::Data(bytes.to_vec(), millis())),
            Ok(WsMessage::Ping(_)) => {
                socket.flush().map_err(|_| "feed_heartbeat_failed")?;
                Ok(Poll::Heartbeat)
            }
            Ok(WsMessage::Pong(_)) => Ok(Poll::Heartbeat),
            Ok(WsMessage::Close(_)) => Err("feed_closed"),
            Ok(_) => Err("feed_message_not_binary"),
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(Poll::Idle)
            }
            Err(_) => Err("feed_read_failed"),
        }
    }
    fn close(&mut self) {
        if let Some(mut socket) = self.socket.take() {
            let _ = socket.close(None);
        }
    }
}
#[derive(Default)]
struct Cache {
    points: BTreeMap<String, Point>,
    /// Cash screening keys; index-only points must not mask a cash data gap.
    screening_keys: BTreeSet<String>,
    connected: bool,
    last_tick: Option<i64>,
    last_activity: Option<i64>,
    last_error: Option<&'static str>,
    reconnects: u64,
    next_retry: i64,
    connected_since: Option<i64>,
    invalid_ltpc: u64,
    ticks_total: u64,
    tick_buckets: VecDeque<(i64, u64)>,
}
impl Cache {
    fn record_ticks(&mut self, receipt: i64, count: u64, window: i64) {
        self.ticks_total = self.ticks_total.saturating_add(count);
        self.tick_buckets
            .retain(|(at, _)| receipt.saturating_sub(*at) < window);
        if let Some((at, ticks)) = self.tick_buckets.back_mut()
            && *at == receipt
        {
            *ticks = ticks.saturating_add(count);
        } else {
            self.tick_buckets.push_back((receipt, count));
        }
    }

    fn fallback(&self, now: i64, settings: &Settings) -> bool {
        !self.connected
            || self.points.is_empty()
            || (!self.screening_keys.is_empty()
                && !self
                    .points
                    .keys()
                    .any(|key| self.screening_keys.contains(key)))
            || self.last_activity.is_none_or(|at| {
                now.checked_sub(at)
                    .is_none_or(|age| !(0..=settings.scan_activity_timeout_ms).contains(&age))
            })
    }
    fn health(&self, now: i64, settings: &Settings) -> Value {
        let fallback = self.fallback(now, settings);
        let window_ticks: u64 = self
            .tick_buckets
            .iter()
            .filter(|(at, _)| {
                now.checked_sub(*at)
                    .is_some_and(|age| (0..settings.telemetry_window_ms).contains(&age))
            })
            .map(|(_, count)| count)
            .sum();
        let tick_rate = window_ticks as f64 * 1000.0 / settings.telemetry_window_ms as f64;
        json!({"mode":"ltpc","depth_mode":"rest","connections":usize::from(self.connected),"connection_limit":settings.max_connections,
            "connected":self.connected,"fallback":fallback,"state":if fallback {"rest_fallback"} else {"streaming"},
            "last_tick_age_ms":self.last_tick.and_then(|at|now.checked_sub(at)),"last_heartbeat_age_ms":self.last_activity.and_then(|at|now.checked_sub(at)),
            "ticks_total":self.ticks_total,"tick_rate_per_s":tick_rate,"cached_keys":self.points.len(),"oldest_last_trade_ms":self.points.values().filter_map(|p|(p.last_trade>0).then_some(p.last_trade)).min(),"close_price_coverage":self.points.values().filter(|p|p.prior_close.is_some()).count(),"reconnects":self.reconnects,"invalid_ltpc":self.invalid_ltpc,"next_retry_ms":self.next_retry,"reason":self.last_error.or_else(||(fallback && self.connected_since.is_some_and(|at|now-at>settings.scan_activity_timeout_ms)).then_some("feed_stale_or_no_ltpc")),
            "screening_only":true})
    }
}
struct Controller {
    keys: Vec<String>,
    allowed: BTreeSet<String>,
    revision: Option<String>,
    token_checked: i64,
    retry_ms: i64,
    settings: Settings,
    cache: Arc<Mutex<Cache>>,
}
impl Controller {
    fn failure(&mut self, transport: &mut impl Transport, now: i64, reason: &'static str) {
        transport.close();
        if let Ok(mut cache) = self.cache.lock() {
            cache.connected = false;
            cache.last_error = Some(reason);
            cache.next_retry = now.saturating_add(self.retry_ms);
            cache.reconnects = cache.reconnects.saturating_add(1);
        }
        self.retry_ms = self
            .retry_ms
            .saturating_mul(2)
            .min(self.settings.reconnect_max_ms);
    }
    fn step(&mut self, transport: &mut impl Transport, now: i64) {
        if self
            .cache
            .lock()
            .ok()
            .is_some_and(|c| !c.connected && now < c.next_retry)
        {
            return;
        }
        if now - self.token_checked >= self.settings.token_check_ms || self.revision.is_none() {
            self.token_checked = now;
            match transport.revision() {
                Ok(revision) => {
                    if self.revision.as_ref().is_some_and(|old| old != &revision) {
                        self.failure(transport, now, "feed_token_refreshed_resubscribe");
                        if let Ok(mut cache) = self.cache.lock() {
                            cache.next_retry = now;
                        }
                    }
                    self.revision = Some(revision);
                }
                Err(reason) => {
                    self.failure(transport, now, reason);
                    return;
                }
            }
        }
        let (connected, next_retry) = match self.cache.lock() {
            Ok(c) => (c.connected, c.next_retry),
            Err(_) => return,
        };
        if !connected {
            if now < next_retry {
                return;
            }
            match transport.connect(&self.keys) {
                Ok(()) => {
                    if let Ok(mut c) = self.cache.lock() {
                        c.connected = true;
                        c.connected_since = Some(now);
                        c.last_activity = Some(now);
                        c.last_tick = None;
                        c.points.clear();
                    }
                }
                Err(reason) => {
                    self.failure(transport, now, reason);
                    return;
                }
            }
        }
        // Receipt is sampled once by the worker immediately after the full frame.
        match transport.poll() {
            Ok(Poll::Data(bytes, receipt)) => {
                self.ingest(transport, &bytes, receipt);
            }
            Ok(Poll::Heartbeat) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.last_activity = Some(now);
                }
            }
            Ok(Poll::Idle) => {}
            Err(reason) => {
                self.failure(transport, now, reason);
                return;
            }
        }
        let expired = self.cache.lock().ok().is_some_and(|c| {
            c.last_activity
                .is_some_and(|at| now - at > self.settings.heartbeat_timeout_ms)
        });
        if expired {
            self.failure(transport, now, "feed_heartbeat_timeout");
        }
    }
    fn ingest(&mut self, transport: &mut impl Transport, bytes: &[u8], receipt: i64) {
        match decode(
            bytes,
            receipt,
            self.settings.source_clock_skew_ms,
            &self.allowed,
        ) {
            Ok(Decoded { points, invalid }) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.last_activity = Some(receipt);
                    c.invalid_ltpc = c.invalid_ltpc.saturating_add(invalid);
                    if !points.is_empty() {
                        let mut accepted = 0;
                        for (key, point) in points {
                            // Delayed messages never replace a newer source/receipt.
                            if receipt - point.source <= self.settings.stale_age_ms
                                && c.points.get(&key).is_none_or(|old| {
                                    point.source >= old.source && point.receipt >= old.receipt
                                })
                            {
                                accepted += 1;
                                c.points.insert(key, point);
                            }
                        }
                        if accepted > 0 {
                            c.record_ticks(receipt, accepted, self.settings.telemetry_window_ms);
                            c.last_tick = Some(receipt);
                            c.last_error = None;
                            self.retry_ms = self.settings.reconnect_initial_ms;
                        }
                    }
                }
            }
            Err(reason) => self.failure(transport, receipt, reason),
        }
    }
}
// Count workers through asynchronous teardown, including an in-flight authorize
// or TLS handshake. A replacement can never exceed the configured 1–2 slots.
static FEED_SLOTS: AtomicUsize = AtomicUsize::new(0);
struct ConnectionPermit;
impl ConnectionPermit {
    fn acquire(limit: usize) -> Result<Self> {
        FEED_SLOTS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < limit.min(2)).then_some(n + 1)
            })
            .map_err(|_| "feed_connection_budget_busy")?;
        Ok(Self)
    }
}
impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        FEED_SLOTS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) type WorkerSnapshot = (BTreeMap<String, Point>, Value, bool, i64, Option<i64>);

/// LTPC subscription keys: the validated cash universe plus configured,
/// non-tradeable index keys. One connection; no second websocket.
pub(super) fn subscription_keys(c: &Config, instruments: &[Instrument]) -> Result<Vec<String>> {
    if instruments.is_empty() {
        return Err("feed_ltpc_subscription_limit".into());
    }
    let mut keys = instruments
        .iter()
        .map(|i| i.key.clone())
        .collect::<Vec<_>>();
    if c.index_feeds.enabled {
        keys.extend(c.index_feeds.keys.iter().cloned());
    }
    if keys.len() > 5000 {
        return Err("feed_ltpc_subscription_limit".into());
    }
    if keys.iter().collect::<BTreeSet<_>>().len() != keys.len() {
        return Err("feed_duplicate_instruments".into());
    }
    Ok(keys)
}

pub(super) struct Worker {
    cache: Arc<Mutex<Cache>>,
    cancelled: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
    pub settings: Settings,
}
impl Worker {
    pub fn start(c: &Config, instruments: &[Instrument], settings: Settings) -> Result<Self> {
        settings.validate()?;
        let keys = subscription_keys(c, instruments)?;
        let permit = ConnectionPermit::acquire(settings.max_connections)?;
        let cache = Arc::new(Mutex::new(Cache {
            screening_keys: instruments.iter().map(|i| i.key.clone()).collect(),
            ..Cache::default()
        }));
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut controller = Controller {
            allowed: keys.iter().cloned().collect(),
            keys,
            revision: None,
            token_checked: 0,
            retry_ms: settings.reconnect_initial_ms,
            settings: settings.clone(),
            cache: cache.clone(),
        };
        let mut network = Network {
            settings: settings.clone(),
            client: data::Client::new(&c.market_data_transport)?,
            config: c.clone(),
            socket: None,
        };
        let cancel = cancelled.clone();
        let handle = thread::Builder::new()
            .name("upstox-ltpc".into())
            .spawn(move || {
                let _permit = permit;
                while !cancel.load(Ordering::Acquire) {
                    controller.step(&mut network, millis());
                    thread::sleep(Duration::from_millis(
                        controller.settings.worker_idle_sleep_ms,
                    ));
                }
                network.close();
            })?;
        Ok(Self {
            cache,
            cancelled,
            handle: Some(handle),
            settings,
        })
    }
    /// Health is judged against a clock read under the cache lock, never a
    /// loop-start sample: a tick received after that sample has a negative age
    /// and was misread as stale once per slow (history/readiness) loop (#34).
    pub fn snapshot(&self) -> Result<WorkerSnapshot> {
        let cache = self.cache.lock().map_err(|_| "feed_cache_unavailable")?;
        let now = millis();
        Ok((
            if cache.fallback(now, &self.settings) {
                BTreeMap::new()
            } else {
                cache.points.clone()
            },
            cache.health(now, &self.settings),
            cache.fallback(now, &self.settings),
            now,
            cache.last_activity,
        ))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        // Cancellation is prompt for the controller even if DNS/TLS is still
        // completing on the reader. Its permit remains held until the socket closes.
        drop(self.handle.take());
    }
}

/// Merge prices from a healthy connection; original price/metadata receipts stay intact.
pub(super) fn observe_cache(
    scan: &mut Scanner,
    universe: &[Instrument],
    points: &BTreeMap<String, Point>,
    settings: &Settings,
    now: i64,
    activity: Option<i64>,
    mut feature_store: Option<&mut nivra_core::feature_store::FeatureStore>,
) -> Result<()> {
    for instrument in universe {
        let Some(point) = points.get(&instrument.key) else {
            scan.missing(&instrument.key, "no_quote_returned");
            continue;
        };
        if activity.is_none_or(|at| {
            now.checked_sub(at)
                .is_none_or(|age| !(0..=settings.scan_activity_timeout_ms).contains(&age))
        }) {
            scan.missing(&instrument.key, "stale");
            continue;
        }
        let fields = scan
            .samples
            .get(&instrument.key)
            .and_then(|s| {
                s.market_fields_receipt
                    .filter(|at| {
                        now.checked_sub(*at).is_some_and(|age| {
                            (0..=if s.metadata_preloaded {
                                settings.metadata_preload_max_age_ms
                            } else {
                                settings.metadata_stale_ms
                            })
                                .contains(&age)
                        })
                    })
                    .and(s.market_fields.as_ref())
            })
            .cloned();
        // Shortlist REST depth may have supplied a newer screening observation.
        // Retain its actual price clocks while the connection supplies eligibility.
        let retained = scan
            .samples
            .get(&instrument.key)
            .filter(|s| s.seen > point.receipt && s.source.is_some_and(|at| at >= point.source))
            .and_then(|s| Some((s.current?, s.source?, s.seen)));
        let (price, source, receipt) =
            retained.unwrap_or((point.price, point.source, point.receipt));
        if let Some(store) = feature_store.as_deref_mut() {
            super::wide::observe_feature(store, &instrument.key, point.price, point.receipt);
        }
        scan.observe_price(
            now,
            &instrument.key,
            price,
            source,
            receipt,
            // Price age does not expire a price learned on this healthy connection.
            i64::MAX,
            fields.as_ref(),
        )?;
        let sample = scan.samples.get_mut(&instrument.key).unwrap();
        sample.price_kind = Some("ltpc".into());
        sample.scan_activity = activity;
        sample.fresh_for_ms = Some(settings.scan_activity_timeout_ms);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    struct Mock {
        revision: String,
        polls: VecDeque<std::result::Result<Poll, &'static str>>,
        subscriptions: Vec<Vec<String>>,
        active: bool,
    }
    impl Transport for Mock {
        fn revision(&self) -> std::result::Result<String, &'static str> {
            Ok(self.revision.clone())
        }
        fn connect(&mut self, keys: &[String]) -> std::result::Result<(), &'static str> {
            assert!(
                !self.active,
                "never open a second connection before closing the first"
            );
            self.active = true;
            self.subscriptions.push(keys.to_vec());
            Ok(())
        }
        fn poll(&mut self) -> std::result::Result<Poll, &'static str> {
            self.polls.pop_front().unwrap_or(Ok(Poll::Idle))
        }
        fn close(&mut self) {
            self.active = false;
        }
    }
    fn frame(source: i64, price: f64) -> Vec<u8> {
        Response {
            kind: 1,
            current_ts: source,
            feeds: BTreeMap::from([(
                "NSE_EQ|A".into(),
                Feed {
                    ltpc: Some(Ltpc {
                        ltp: price,
                        ltt: source,
                        ltq: 2,
                        cp: 100.0,
                    }),
                },
            )]),
        }
        .encode_to_vec()
    }
    #[test]
    fn mock_v3_feed_decoding_reconnect_rotation_staleness_and_fallback() {
        let first = ConnectionPermit::acquire(2).unwrap();
        let second = ConnectionPermit::acquire(2).unwrap();
        assert!(ConnectionPermit::acquire(2).is_err());
        drop((first, second));
        let keys = vec!["NSE_EQ|A".to_owned()];
        let allowed = keys.iter().cloned().collect();
        // Independent provider-wire fixture (not a round-trip of our decoder).
        let hex = "080112250a084e53455f45517c4112190a1709000000000000594010e807180521000000000000594018e807";
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|n| u8::from_str_radix(&hex[n..n + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let points = decode(&bytes, 1001, 0, &allowed).unwrap().points;
        assert_eq!(points[&keys[0]].price, Price::parse("100").unwrap());
        assert_eq!(
            (points[&keys[0]].source, points[&keys[0]].receipt),
            (1000, 1001)
        );
        assert!(decode(&bytes, 999, 0, &allowed).is_err());
        assert!(decode(&[0xff], 1001, 0, &allowed).is_err());
        assert!(
            decode(&bytes, 1001, 0, &BTreeSet::new())
                .unwrap()
                .points
                .is_empty()
        );
        // Configured NSE_INDEX keys join the same subscription list; disabled
        // feeds add nothing and a duplicate key is refused, not folded.
        let index_key = "NSE_INDEX|Nifty 50".to_owned();
        let universe = vec![Instrument {
            key: keys[0].clone(),
            symbol: "A".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        }];
        let mut c = super::super::tests::config(Path::new("fixture"));
        assert_eq!(subscription_keys(&c, &universe).unwrap(), keys);
        c.index_feeds.enabled = true;
        c.quote_keys.prefixes.push("NSE_INDEX|".into());
        c.validate().unwrap();
        let subscribed = subscription_keys(&c, &universe).unwrap();
        assert_eq!(subscribed.len(), keys.len() + 3);
        assert_eq!(&subscribed[keys.len()..], c.index_feeds.keys.as_slice());
        assert!(subscription_keys(&c, &[]).is_err());
        let mut duplicate = c.clone();
        duplicate.index_feeds.keys = vec![keys[0].clone()];
        assert!(subscription_keys(&duplicate, &universe).is_err());
        // Index prices decode only for the configured allowlist; the LTPC point
        // keeps its prior close for read-only context.
        let index_frame = Response {
            kind: 1,
            current_ts: 1000,
            feeds: BTreeMap::from([(
                index_key.clone(),
                Feed {
                    ltpc: Some(Ltpc {
                        ltp: 24_500.0,
                        ltt: 1000,
                        ltq: 0,
                        cp: 24_400.0,
                    }),
                },
            )]),
        }
        .encode_to_vec();
        let index_allowed = BTreeSet::from([index_key.clone()]);
        let index_points = decode(&index_frame, 1001, 0, &index_allowed)
            .unwrap()
            .points;
        assert_eq!(
            index_points[&index_key].price,
            Price::parse("24500").unwrap()
        );
        assert_eq!(
            index_points[&index_key].prior_close,
            Some(Price::parse("24400").unwrap())
        );
        assert!(
            decode(&index_frame, 1001, 0, &allowed)
                .unwrap()
                .points
                .is_empty(),
            "cash subscription must not accept index keys"
        );
        let settings = Settings {
            tick_ms: 100,
            stale_age_ms: 500,
            metadata_poll_ms: 100,
            metadata_stale_ms: 500,
            reconnect_initial_ms: 100,
            reconnect_max_ms: 400,
            token_check_ms: 100,
            heartbeat_timeout_ms: 1000,
            scan_activity_timeout_ms: 500,
            source_clock_skew_ms: 0,
            ..Settings::default()
        };
        settings.validate().unwrap();
        assert_eq!(settings.rest_interval_ms(true), settings.fallback_poll_ms);
        assert_eq!(settings.rest_interval_ms(false), settings.metadata_poll_ms);
        let cache = Arc::new(Mutex::new(Cache::default()));
        let mut controller = Controller {
            keys: keys.clone(),
            allowed,
            revision: None,
            token_checked: 0,
            retry_ms: 100,
            settings: settings.clone(),
            cache: cache.clone(),
        };
        let mut mock = Mock {
            revision: "day-one".into(),
            polls: VecDeque::from([Ok(Poll::Data(bytes, 1001))]),
            subscriptions: vec![],
            active: false,
        };
        controller.step(&mut mock, 1001);
        assert!(!cache.lock().unwrap().fallback(1001, &settings));
        assert_eq!(
            cache.lock().unwrap().health(1001, &settings)["ticks_total"],
            1
        );
        controller.ingest(&mut mock, &frame(1100, 101.0), 1100);
        // Invalid and delayed points must not inflate accepted tick telemetry.
        controller.ingest(&mut mock, &frame(1100, 0.0), 1100);
        controller.ingest(&mut mock, &frame(1000, 100.0), 1100);
        let health = cache.lock().unwrap().health(1100, &settings);
        assert_eq!(health["ticks_total"], 2);
        assert_eq!(health["tick_rate_per_s"], 2.0 / 60.0);
        assert_eq!(
            cache.lock().unwrap().health(61001, &settings)["tick_rate_per_s"],
            1.0 / 60.0
        );
        assert_eq!(
            cache.lock().unwrap().health(61100, &settings)["tick_rate_per_s"],
            0.0
        );
        // Index-only points never mask a missing cash screening price.
        let mut index_only = Cache {
            screening_keys: BTreeSet::from([keys[0].clone()]),
            connected: true,
            last_activity: Some(1001),
            ..Cache::default()
        };
        index_only.points.insert(
            index_key.clone(),
            Point {
                price: Price::parse("24500").unwrap(),
                source: 1000,
                receipt: 1001,
                last_trade: 1000,
                prior_close: None,
            },
        );
        assert!(index_only.fallback(1001, &settings));
        index_only.points.insert(
            keys[0].clone(),
            Point {
                price: Price::parse("100").unwrap(),
                source: 1000,
                receipt: 1001,
                last_trade: 1000,
                prior_close: None,
            },
        );
        assert!(!index_only.fallback(1001, &settings));

        mock.polls.push_back(Err("feed_closed"));
        controller.step(&mut mock, 1101);
        assert_eq!(
            cache.lock().unwrap().health(1101, &settings)["reconnects"],
            1
        );
        assert!(cache.lock().unwrap().fallback(1101, &settings));
        controller.step(&mut mock, 1200);
        assert_eq!(mock.subscriptions.len(), 1);
        controller.step(&mut mock, 1201);
        assert_eq!(mock.subscriptions.len(), 2);
        assert!(
            cache.lock().unwrap().points.is_empty(),
            "reconnect waits for a new snapshot"
        );
        mock.polls
            .push_back(Ok(Poll::Data(frame(1202, 101.0), 1203)));
        controller.step(&mut mock, 1203);
        assert!(!cache.lock().unwrap().fallback(1203, &settings));
        // Daily application-token renewal reauthorizes/resubscribes the same universe.
        mock.revision = "day-two".into();
        controller.step(&mut mock, 1304);
        assert_eq!(mock.subscriptions.len(), 3);
        assert!(mock.subscriptions.iter().all(|sub| sub == &keys));
        assert!(cache.lock().unwrap().fallback(1304, &settings));
        mock.polls
            .push_back(Ok(Poll::Data(frame(1400, 102.0), 1401)));
        controller.step(&mut mock, 1401);
        let instrument = Instrument {
            key: keys[0].clone(),
            symbol: "A".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        };
        let mut scan = Scanner {
            screens: Some(nivra_core::execution::ScreenPolicy::recommended()),
            ..Scanner::default()
        };
        let sample = scan.samples.entry(keys[0].clone()).or_default();
        sample.market_fields = Some(nivra_core::execution::MarketFields {
            session_vwap: None,
            best_bid: None,
            best_bid_qty: None,
            best_ask: None,
            best_ask_qty: None,
            total_buy_quantity: None,
            total_sell_quantity: None,
            lower_circuit_limit: nivra_core::execution::a("80"),
            upper_circuit_limit: nivra_core::execution::a("120"),
            volume: 20000,
        });
        sample.market_fields_receipt = Some(1000);
        let points = cache.lock().unwrap().points.clone();
        observe_cache(
            &mut scan,
            std::slice::from_ref(&instrument),
            &points,
            &settings,
            1402,
            Some(1401),
            None,
        )
        .unwrap();
        assert_eq!(scan.samples[&keys[0]].reason, "eligible_no_signal");
        assert_eq!(scan.samples[&keys[0]].market_fields_receipt, Some(1000));
        observe_cache(
            &mut scan,
            std::slice::from_ref(&instrument),
            &points,
            &settings,
            1902,
            Some(1401),
            None,
        )
        .unwrap();
        assert_eq!(scan.samples[&keys[0]].reason, "stale");
        assert_eq!(
            scan.samples[&keys[0]].seen, 1401,
            "clock samples never renew quote receipt"
        );
        mock.polls
            .push_back(Ok(Poll::Data(frame(1950, 103.0), 1951)));
        controller.step(&mut mock, 1951);
        observe_cache(
            &mut scan,
            std::slice::from_ref(&instrument),
            &cache.lock().unwrap().points,
            &settings,
            1952,
            Some(1951),
            None,
        )
        .unwrap();
        assert_eq!(
            scan.samples[&keys[0]].reason, "missing_market_fields",
            "LTPC cannot renew volume/band metadata"
        );
        assert!(cache.lock().unwrap().fallback(2452, &settings));
        assert_eq!(
            cache.lock().unwrap().health(2452, &settings)["reason"],
            "feed_stale_or_no_ltpc"
        );
        // Heartbeats keep the transport alive but do not refresh price timestamps.
        mock.polls.push_back(Ok(Poll::Heartbeat));
        controller.step(&mut mock, 2452);
        assert!(!cache.lock().unwrap().fallback(2452, &settings));
        controller.step(&mut mock, 3453);
        assert_eq!(
            cache.lock().unwrap().last_error,
            Some("feed_heartbeat_timeout")
        );
        let retry = cache.lock().unwrap().next_retry;
        mock.polls.push_back(Err("feed_read_failed"));
        controller.step(&mut mock, retry);
        assert_eq!(
            controller.retry_ms, 400,
            "bounded exponential reconnect backoff"
        );
        assert!(cache.lock().unwrap().fallback(retry, &settings));
        assert!(!mock.active);
        assert!(
            Settings {
                max_connections: 3,
                ..settings
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn transport_settings_preserve_defaults_and_reject_invalid_bounds() {
        let settings: Settings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(settings.max_connect_addresses, 2);
        assert_eq!(settings.connect_timeout_ms, 3000);
        assert_eq!(settings.handshake_io_timeout_ms, 3000);
        assert_eq!(settings.read_poll_timeout_ms, 250);
        assert_eq!(settings.worker_idle_sleep_ms, 25);
        settings.validate().unwrap();
        let defaults = serde_json::to_value(&settings).unwrap();
        for (field, maximum) in [
            ("max_connect_addresses", 64),
            ("connect_timeout_ms", 86_400_000),
            ("handshake_io_timeout_ms", 86_400_000),
            ("read_poll_timeout_ms", 86_400_000),
            ("worker_idle_sleep_ms", 86_400_000),
        ] {
            for value in [0, maximum + 1] {
                let mut config = defaults.clone();
                config[field] = json!(value);
                let invalid: Settings = serde_json::from_value(config).unwrap();
                assert!(invalid.validate().is_err(), "{field}={value}");
            }
            for value in [1, maximum] {
                let mut config = defaults.clone();
                config[field] = json!(value);
                let valid: Settings = serde_json::from_value(config).unwrap();
                assert!(valid.validate().is_ok(), "{field}={value}");
            }
        }
    }

    #[test]
    fn fresh_drop_fraction_defaults_and_bounds() {
        let settings: Settings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(settings.fresh_drop_alert_fraction, 0.5);
        for fraction in [0.0, -0.1, 1.01, f64::NAN, f64::INFINITY] {
            assert!(
                Settings {
                    fresh_drop_alert_fraction: fraction,
                    ..settings.clone()
                }
                .validate()
                .is_err()
            );
        }
        for fraction in [0.01, 0.5, 1.0] {
            assert!(
                Settings {
                    fresh_drop_alert_fraction: fraction,
                    ..settings.clone()
                }
                .validate()
                .is_ok()
            );
        }
    }

    #[test]
    fn quiet_prices_follow_connection_liveness_without_renewing_financial_receipts() {
        let settings: Settings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(settings.scan_activity_timeout_ms, 30_000);
        assert_eq!(settings.telemetry_window_ms, 60_000);
        for window in [0, 99, 86_400_001] {
            assert!(
                Settings {
                    telemetry_window_ms: window,
                    ..settings.clone()
                }
                .validate()
                .is_err()
            );
        }
        for timeout in [0, 99, 86_400_001] {
            assert!(
                Settings {
                    scan_activity_timeout_ms: timeout,
                    ..settings.clone()
                }
                .validate()
                .is_err()
            );
        }
        let key = "NSE_EQ|A".to_owned();
        let instrument = Instrument {
            key: key.clone(),
            symbol: "A".into(),
            series: "EQ".into(),
            tick: Price::parse("0.05").unwrap(),
        };
        let cache = Arc::new(Mutex::new(Cache::default()));
        let mut controller = Controller {
            keys: vec![key.clone()],
            allowed: BTreeSet::from([key.clone()]),
            revision: None,
            token_checked: 0,
            retry_ms: settings.reconnect_initial_ms,
            settings: settings.clone(),
            cache: cache.clone(),
        };
        let mut mock = Mock {
            revision: "fixture".into(),
            polls: VecDeque::from([Ok(Poll::Heartbeat)]),
            subscriptions: vec![],
            active: false,
        };
        controller.step(&mut mock, 1000);
        assert!(
            cache.lock().unwrap().fallback(1000, &settings),
            "heartbeat alone supplies no valid price"
        );
        mock.polls
            .push_back(Ok(Poll::Data(frame(1001, 100.0), 1001)));
        controller.step(&mut mock, 1001);
        mock.polls.push_back(Ok(Poll::Heartbeat));
        controller.step(&mut mock, 25_000);
        let mut scan = Scanner::default();
        let sample = scan.samples.entry(key.clone()).or_default();
        sample.market_fields = Some(nivra_core::execution::MarketFields {
            session_vwap: None,
            best_bid: None,
            best_bid_qty: None,
            best_ask: None,
            best_ask_qty: None,
            total_buy_quantity: None,
            total_sell_quantity: None,
            lower_circuit_limit: nivra_core::execution::a("80"),
            upper_circuit_limit: nivra_core::execution::a("120"),
            volume: 20_000,
        });
        sample.market_fields_receipt = Some(1000);
        let c = cache.lock().unwrap();
        assert!(!c.fallback(55_000, &settings));
        assert!(c.fallback(55_001, &settings));
        observe_cache(
            &mut scan,
            std::slice::from_ref(&instrument),
            &c.points,
            &settings,
            25_000,
            c.last_activity,
            None,
        )
        .unwrap();
        assert_eq!(scan.samples[&key].seen, 1001);
        assert_eq!(scan.samples[&key].source, Some(1001));
        assert!(scan.samples[&key].executable.is_none());
        let discovery: super::super::discovery::Settings = serde_json::from_value(json!({
            "max_scan_age_ms": 2000, "max_prospects": 10, "market_feed": { "mode": "ltpc" }
        }))
        .unwrap();
        for now in [25_000, 55_000] {
            let projection = super::super::discovery::project(
                &scan,
                std::slice::from_ref(&instrument),
                &discovery,
                now,
            );
            assert_eq!(projection["fresh_count"], 1);
            assert_eq!(projection["eligible_count"], 1);
            assert_eq!(
                nivra_core::strategy::extract_features(&scan, now, 2000)
                    .rows
                    .len(),
                1
            );
        }
        let projection = super::super::discovery::project(
            &scan,
            std::slice::from_ref(&instrument),
            &discovery,
            55_001,
        );
        assert_eq!(projection["fresh_count"], 0);
        drop(c);
        controller.step(&mut mock, 55_001);
        assert!(!cache.lock().unwrap().connected);
        let retry = cache.lock().unwrap().next_retry;
        mock.polls.push_back(Ok(Poll::Heartbeat));
        controller.step(&mut mock, retry);
        let c = cache.lock().unwrap();
        assert!(c.points.is_empty(), "new connection requires its own price");
        assert!(c.fallback(retry, &settings));
        observe_cache(
            &mut scan,
            &[instrument],
            &c.points,
            &settings,
            retry,
            c.last_activity,
            None,
        )
        .unwrap();
        assert_eq!(scan.samples[&key].reason, "no_quote_returned");
    }

    fn two(source: i64, good: Ltpc, bad: Ltpc) -> Vec<u8> {
        Response {
            kind: 1,
            current_ts: source,
            feeds: BTreeMap::from([
                ("NSE_EQ|A".into(), Feed { ltpc: Some(good) }),
                ("NSE_EQ|B".into(), Feed { ltpc: Some(bad) }),
            ]),
        }
        .encode_to_vec()
    }
    fn ltpc(ltp: f64, ltt: i64) -> Ltpc {
        Ltpc {
            ltp,
            ltt,
            ltq: 1,
            cp: 100.0,
        }
    }
    #[test]
    fn one_invalid_instrument_or_small_clock_skew_never_drops_the_frame() {
        let allowed: BTreeSet<String> = ["NSE_EQ|A".into(), "NSE_EQ|B".into()].into();
        // Untraded at the open (ltp 0) or NaN: skip that key, keep the rest.
        for bad in [ltpc(0.0, 0), ltpc(f64::NAN, 900), ltpc(10.0, -1)] {
            let d = decode(&two(1000, ltpc(100.0, 990), bad), 1001, 0, &allowed).unwrap();
            assert_eq!((d.points.len(), d.invalid), (1, 1));
            assert!(d.points.contains_key("NSE_EQ|A"));
        }
        // Exchange/provider clocks a little ahead of ours are tolerated within config.
        let ahead = two(1500, ltpc(100.0, 1400), ltpc(101.0, 1500));
        assert!(decode(&ahead, 1001, 0, &allowed).is_err());
        let d = decode(&ahead, 1001, 500, &allowed).unwrap();
        assert_eq!((d.points.len(), d.invalid), (2, 0));
        assert_eq!(
            d.points["NSE_EQ|A"].source, 1001,
            "source never leads receipt"
        );
        assert!(decode(&ahead, 1001, 400, &allowed).is_err());
        assert!(
            Settings {
                source_clock_skew_ms: 5001,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn invalid_opening_ticks_preserve_connection_but_cannot_hide_staleness() {
        let settings = Settings {
            stale_age_ms: 500,
            source_clock_skew_ms: 100,
            reconnect_initial_ms: 100,
            ..Settings::default()
        };
        settings.validate().unwrap();
        let keys = vec!["NSE_EQ|A".into(), "NSE_EQ|B".into()];
        let cache = Arc::new(Mutex::new(Cache::default()));
        let mut controller = Controller {
            allowed: keys.iter().cloned().collect(),
            keys: keys.clone(),
            revision: None,
            token_checked: 0,
            retry_ms: settings.reconnect_initial_ms,
            settings: settings.clone(),
            cache: cache.clone(),
        };
        let mut mock = Mock {
            revision: "fixture".into(),
            polls: VecDeque::new(),
            subscriptions: vec![],
            active: false,
        };
        // A valid key at the exact configured skew boundary survives an invalid peer.
        mock.polls.push_back(Ok(Poll::Data(
            two(1100, ltpc(100.0, 1100), ltpc(0.0, 0)),
            1000,
        )));
        controller.step(&mut mock, 1000);
        {
            let c = cache.lock().unwrap();
            assert!(!c.fallback(1000, &settings));
            assert_eq!(c.invalid_ltpc, 1);
            assert_eq!(
                (c.points[&keys[0]].source, c.points[&keys[0]].receipt),
                (1000, 1000)
            );
        }
        // All-invalid frames are transport activity, never new price evidence.
        mock.polls.push_back(Ok(Poll::Data(
            two(1500, ltpc(0.0, 0), ltpc(101.0, 1601)),
            1500,
        )));
        controller.step(&mut mock, 1500);
        {
            let c = cache.lock().unwrap();
            assert!(!c.fallback(1500, &settings));
            assert!(!c.fallback(1501, &settings));
            assert_eq!(c.invalid_ltpc, 3);
            assert_eq!(c.last_activity, Some(1500));
            assert_eq!(c.last_tick, Some(1000));
            assert_eq!(c.points[&keys[0]].receipt, 1000);
            assert_eq!(c.reconnects, 0);
        }
        assert!(mock.active);
        assert_eq!(mock.subscriptions.len(), 1);
        // A delayed frame keeps transport alive but cannot replace retained price evidence.
        mock.polls.push_back(Ok(Poll::Data(frame(999, 99.0), 1600)));
        controller.step(&mut mock, 1600);
        assert!(!cache.lock().unwrap().fallback(1600, &settings));
        assert_eq!(cache.lock().unwrap().points[&keys[0]].receipt, 1000);
        mock.polls
            .push_back(Ok(Poll::Data(frame(1700, 102.0), 1700)));
        controller.step(&mut mock, 1700);
        assert!(!cache.lock().unwrap().fallback(1700, &settings));
        assert_eq!(mock.subscriptions.len(), 1, "recovery needs no reconnect");
        // Frame-wide invalid source time remains a real failure, beyond tolerance.
        mock.polls
            .push_back(Ok(Poll::Data(frame(1901, 103.0), 1800)));
        controller.step(&mut mock, 1800);
        {
            let c = cache.lock().unwrap();
            assert_eq!(c.last_error, Some("feed_source_time_invalid"));
            assert!(c.fallback(1800, &settings));
            assert_eq!(c.next_retry, 1900);
            assert_eq!(c.points[&keys[0]].receipt, 1700);
        }
        assert!(!mock.active);
        controller.step(&mut mock, 1899);
        assert_eq!(mock.subscriptions.len(), 1);
        mock.polls
            .push_back(Ok(Poll::Data(frame(1900, 104.0), 1900)));
        controller.step(&mut mock, 1900);
        assert_eq!(mock.subscriptions, vec![keys.clone(), keys]);
        assert!(!cache.lock().unwrap().fallback(1900, &settings));
        assert_eq!(cache.lock().unwrap().last_error, None);
    }

    #[test]
    fn ticks_newer_than_a_stale_loop_clock_are_not_reported_as_fallback() {
        let settings = Settings::default();
        let cache = Cache {
            connected: true,
            connected_since: Some(0),
            last_tick: Some(100_000),
            last_activity: Some(100_000),
            points: decode(
                &frame(100_000, 100.0),
                100_000,
                0,
                &BTreeSet::from(["NSE_EQ|A".into()]),
            )
            .unwrap()
            .points,
            ..Cache::default()
        };
        // A loop-start clock sampled before the tick sees a negative age (the #34 flap).
        assert!(cache.fallback(99_000, &settings));
        // The worker snapshot reads its own clock, which is never behind its receipts.
        let worker = Worker {
            cache: Arc::new(Mutex::new(Cache {
                last_tick: Some(millis()),
                last_activity: Some(millis()),
                ..cache
            })),
            cancelled: Arc::new(AtomicBool::new(false)),
            handle: None,
            settings,
        };
        let (_, health, fallback, now, _) = worker.snapshot().unwrap();
        assert!(!fallback);
        assert_eq!(health["state"], "streaming");
        assert!(now >= millis() - 1000);
    }
}
