//! Upstox V3 screening prices and executable touch for promoted keys.
mod wire;

use super::subscription_plan::{self, Plan};
use super::*;
use nivra_core::{
    execution::{Instrument, Scanner},
    finance::CashOrderGrid,
    market::MAX_FINANCIAL_AGE_MS,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::{TcpStream, ToSocketAddrs},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tungstenite::{Message as WsMessage, WebSocket, stream::MaybeTlsStream};
pub(super) use wire::Point;
use wire::{Decoded, decode};

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
    pub fn full_depth(&self) -> bool {
        self.depth_mode == "full"
    }

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
            && (self.depth_mode == "rest" || self.full_depth() && self.mode == Mode::Ltpc)
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
enum Poll {
    Data(Vec<u8>, i64),
    Heartbeat,
    Idle,
}
trait Transport {
    fn revision(&self) -> std::result::Result<String, &'static str>;
    fn connect(&mut self, keys: &[String]) -> std::result::Result<(), &'static str>;
    fn subscription(
        &mut self,
        method: &str,
        mode: &str,
        keys: &[String],
    ) -> std::result::Result<(), &'static str>;
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
        self.socket = Some(socket);
        if !keys.is_empty() {
            self.subscription("sub", "ltpc", keys)?;
        }
        Ok(())
    }
    fn subscription(
        &mut self,
        method: &str,
        mode: &str,
        keys: &[String],
    ) -> std::result::Result<(), &'static str> {
        static REQUEST_SEQUENCE: AtomicUsize = AtomicUsize::new(0);
        let sequence = REQUEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let message = json!({
            "guid": format!("nivra-{}-{sequence}", millis()),
            "method": method,
            "data": {"mode": mode, "instrumentKeys": keys},
        });
        self.socket
            .as_mut()
            .ok_or("feed_disconnected")?
            .send(WsMessage::Binary(
                serde_json::to_vec(&message)
                    .map_err(|_| "feed_subscription_invalid")?
                    .into(),
            ))
            .map_err(|_| "feed_subscribe_failed")
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
/// Prospects can coalesce to the latest receipt. Held depth preserves
/// crosses that would disappear if the book only saw the latest touch.
#[derive(Default)]
pub(super) struct DepthUpdates {
    pub prospects: BTreeMap<String, Point>,
    pub held: VecDeque<(String, Point)>,
    pub held_gap: bool,
    pub reconnected: bool,
}

const HELD_QUEUE_LIMIT: usize = 256;

#[derive(Default)]
struct Cache {
    points: BTreeMap<String, Point>,
    held: BTreeSet<String>,
    strategy_keys: BTreeSet<String>,
    depth_updates: DepthUpdates,
    /// Cash screening keys; index-only points must not mask a cash data gap.
    screening_keys: BTreeSet<String>,
    connected: bool,
    full_keys: usize,
    last_tick: Option<i64>,
    last_activity: Option<i64>,
    last_error: Option<&'static str>,
    reconnects: u64,
    next_retry: i64,
    connected_since: Option<i64>,
    invalid_ltpc: u64,
    ticks_total: u64,
    tick_buckets: VecDeque<(i64, u64)>,
    // Notify the book writer after a decoded batch or connection transition.
    controller_notify: Option<super::controller_events::Notify>,
}
impl Cache {
    fn queue_depth(&mut self, key: &str, point: &Point) -> bool {
        if self.held.contains(key) {
            self.queue_held(key.to_owned(), point.clone());
        } else if self.strategy_keys.contains(key) {
            self.depth_updates
                .prospects
                .insert(key.to_owned(), point.clone());
        } else {
            return false;
        }
        true
    }

    fn queue_held(&mut self, key: String, point: Point) {
        if self.depth_updates.held.len() == HELD_QUEUE_LIMIT {
            self.depth_updates.held.pop_back();
            self.depth_updates.held_gap = true;
        }
        self.depth_updates.held.push_back((key, point));
    }

    fn record_ticks(&mut self, receipt: i64, count: u64, window: i64) {
        self.ticks_total = self.ticks_total.saturating_add(count);
        while self
            .tick_buckets
            .front()
            .is_some_and(|(at, _)| receipt.saturating_sub(*at) >= window)
        {
            self.tick_buckets.pop_front();
        }
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
        let fresh_full_keys = self
            .points
            .values()
            .filter(|p| {
                p.depth.as_ref().is_some_and(|depth| {
                    depth.touch.is_some() && depth.fresh(now, MAX_FINANCIAL_AGE_MS)
                })
            })
            .count();
        json!({"mode":"ltpc","depth_mode":settings.depth_mode,
            "full_subscribed_keys":self.full_keys,"full_fresh_keys":fresh_full_keys,"connections":usize::from(self.connected),"connection_limit":settings.max_connections,
            "connected":self.connected,"fallback":fallback,"state":if fallback {"rest_fallback"} else {"streaming"},
            "last_tick_age_ms":self.last_tick.and_then(|at|now.checked_sub(at)),"last_heartbeat_age_ms":self.last_activity.and_then(|at|now.checked_sub(at)),
            "ticks_total":self.ticks_total,"tick_rate_per_s":tick_rate,"cached_keys":self.points.len(),"oldest_last_trade_ms":self.points.values().filter_map(|p|(p.last_trade>0).then_some(p.last_trade)).min(),"close_price_coverage":self.points.values().filter(|p|p.prior_close.is_some()).count(),"reconnects":self.reconnects,"invalid_ltpc":self.invalid_ltpc,"next_retry_ms":self.next_retry,"reason":self.last_error.or_else(||(fallback && self.connected_since.is_some_and(|at|now-at>settings.scan_activity_timeout_ms)).then_some("feed_stale_or_no_ltpc")),
            "screening_only":!settings.full_depth()})
    }
}
struct Mixed {
    core: BTreeSet<String>,
    requested: Arc<Mutex<BTreeSet<String>>>,
    current: Plan,
}

impl Mixed {
    fn reconcile(
        &mut self,
        transport: &mut impl Transport,
    ) -> std::result::Result<bool, &'static str> {
        let requested = self
            .requested
            .lock()
            .map_err(|_| "feed_promotions_unavailable")?;
        if self.current.full == *requested
            && (!self.current.ltpc.is_empty() || !self.current.full.is_empty())
        {
            return Ok(false);
        }
        let next = Plan::mixed(&self.core, requested.clone());
        drop(requested);
        for (method, mode, keys) in self.current.changes(&next) {
            transport.subscription(method, mode, &keys)?;
        }
        self.current = next;
        Ok(true)
    }
}

struct Controller {
    keys: Vec<String>,
    allowed: BTreeSet<String>,
    mixed: Option<Mixed>,
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
            cache.full_keys = 0;
            cache.depth_updates.prospects.clear();
            cache.depth_updates.reconnected = true;
            cache.last_error = Some(reason);
            cache.next_retry = now.saturating_add(self.retry_ms);
            cache.reconnects = cache.reconnects.saturating_add(1);
            if let Some(notify) = &cache.controller_notify {
                notify.signal(super::controller_events::Event::FeedState);
            }
        }
        self.retry_ms = self
            .retry_ms
            .saturating_mul(2)
            .min(self.settings.reconnect_max_ms);
    }
    fn step(&mut self, transport: &mut impl Transport, now: i64) -> bool {
        if self
            .cache
            .lock()
            .ok()
            .is_some_and(|c| !c.connected && now < c.next_retry)
        {
            return false;
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
                    return false;
                }
            }
        }
        let (connected, next_retry) = match self.cache.lock() {
            Ok(c) => (c.connected, c.next_retry),
            Err(_) => return false,
        };
        if !connected {
            if now < next_retry {
                return false;
            }
            let keys = if self.mixed.is_some() {
                &[][..]
            } else {
                &self.keys
            };
            match transport.connect(keys) {
                Ok(()) => {
                    if let Some(mixed) = &mut self.mixed {
                        mixed.current = Plan::default();
                    }
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
                    return false;
                }
            }
        }
        if let Some(mixed) = &mut self.mixed {
            match mixed.reconcile(transport) {
                Ok(true) => {
                    self.allowed = mixed.current.keys();
                    if let Ok(mut cache) = self.cache.lock() {
                        cache.full_keys = mixed.current.full.len();
                        cache.points.retain(|key, point| {
                            if !mixed.current.full.contains(key) {
                                point.depth = None;
                            }
                            self.allowed.contains(key)
                        });
                    }
                }
                Ok(false) => {}
                Err(reason) => {
                    self.failure(transport, now, reason);
                    return false;
                }
            }
        }
        // Receipt is sampled once by the worker immediately after the full frame.
        let received = match transport.poll() {
            Ok(Poll::Data(bytes, receipt)) => {
                self.ingest(transport, &bytes, receipt);
                true
            }
            Ok(Poll::Heartbeat) => {
                if let Ok(mut c) = self.cache.lock() {
                    c.last_activity = Some(now);
                }
                true
            }
            Ok(Poll::Idle) => false,
            Err(reason) => {
                self.failure(transport, now, reason);
                return false;
            }
        };
        let expired = self.cache.lock().ok().is_some_and(|c| {
            c.last_activity
                .is_some_and(|at| now - at > self.settings.heartbeat_timeout_ms)
        });
        if expired {
            self.failure(transport, now, "feed_heartbeat_timeout");
        }
        received
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
                        let mut depth_changed = false;
                        for (key, mut point) in points {
                            if let Some(mixed) = &self.mixed
                                && !mixed.current.full.contains(&key)
                            {
                                point.depth = None;
                            }
                            // Delayed messages never replace a newer source/receipt.
                            if receipt - point.source <= self.settings.stale_age_ms
                                && c.points.get(&key).is_none_or(|old| {
                                    point.source >= old.source && point.receipt >= old.receipt
                                })
                            {
                                accepted += 1;
                                if point.depth.is_some() {
                                    depth_changed |= c.queue_depth(&key, &point);
                                } else {
                                    point.depth =
                                        c.points.get_mut(&key).and_then(|prior| prior.depth.take());
                                }
                                c.points.insert(key, point);
                            }
                        }
                        if accepted > 0 {
                            c.record_ticks(receipt, accepted, self.settings.telemetry_window_ms);
                            let first = c.last_tick.is_none();
                            c.last_tick = Some(receipt);
                            c.last_error = None;
                            self.retry_ms = self.settings.reconnect_initial_ms;
                            if let Some(notify) = &c.controller_notify {
                                use super::controller_events::Event;
                                if first {
                                    notify.signal(Event::FeedState);
                                }
                                if depth_changed {
                                    notify.signal(Event::Feed);
                                }
                            }
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

/// The validated cash universe plus configured non-tradeable index keys.
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
    if keys.len() > subscription_plan::SINGLE_LTPC {
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
    promoted: Arc<Mutex<BTreeSet<String>>>,
    universe: BTreeSet<String>,
    pub settings: Settings,
}
impl Worker {
    pub fn start(
        c: &Config,
        instruments: &[Instrument],
        settings: Settings,
        notify: super::controller_events::Notify,
    ) -> Result<Self> {
        settings.validate()?;
        let keys = subscription_keys(c, instruments)?;
        let permit = ConnectionPermit::acquire(settings.max_connections)?;
        let promoted = Arc::new(Mutex::new(BTreeSet::new()));
        let universe = instruments
            .iter()
            .map(|i| i.key.clone())
            .collect::<BTreeSet<_>>();
        let mixed = settings.full_depth().then(|| Mixed {
            // Reserve the index keys before filling the rest of the LTPC pool.
            core: keys[instruments.len()..]
                .iter()
                .chain(&keys[..instruments.len()])
                .take(subscription_plan::MIXED_LTPC)
                .cloned()
                .collect(),
            requested: promoted.clone(),
            current: Plan::default(),
        });
        let cache = Arc::new(Mutex::new(Cache {
            screening_keys: instruments.iter().map(|i| i.key.clone()).collect(),
            controller_notify: Some(notify),
            ..Cache::default()
        }));
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut controller = Controller {
            allowed: keys.iter().cloned().collect(),
            keys,
            mixed,
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
                    if !controller.step(&mut network, millis()) {
                        thread::sleep(Duration::from_millis(
                            controller.settings.worker_idle_sleep_ms,
                        ));
                    }
                }
                network.close();
            })?;
        Ok(Self {
            cache,
            cancelled,
            handle: Some(handle),
            promoted,
            universe,
            settings,
        })
    }
    pub fn set_promoted_keys(
        &self,
        held: &[Instrument],
        extra_shadow_slots: usize,
        ranked: impl IntoIterator<Item = String>,
        notify_strategy: bool,
    ) -> Result<()> {
        if self.settings.full_depth() {
            let full = subscription_plan::promoted(
                held.iter().map(|instrument| instrument.key.clone()),
                ranked.into_iter().filter(|key| self.universe.contains(key)),
                self.settings
                    .shortlist_size
                    .saturating_add(extra_shadow_slots),
            )?;
            {
                let mut cache = self.cache.lock().map_err(|_| "feed_cache_unavailable")?;
                cache.held = held
                    .iter()
                    .map(|instrument| instrument.key.clone())
                    .collect();
                for instrument in held {
                    // A receipt can arrive between the entry fill and pinning.
                    if let Some(point) = cache.depth_updates.prospects.remove(&instrument.key) {
                        cache.queue_held(instrument.key.clone(), point);
                    }
                }
                cache.strategy_keys = if notify_strategy {
                    full.difference(&cache.held).cloned().collect()
                } else {
                    BTreeSet::new()
                };
                let Cache {
                    strategy_keys,
                    depth_updates,
                    ..
                } = &mut *cache;
                depth_updates
                    .prospects
                    .retain(|key, _| strategy_keys.contains(key));
            }
            *self
                .promoted
                .lock()
                .map_err(|_| "feed_promotions_unavailable")? = full;
        }
        Ok(())
    }

    pub fn drain_depth(&self) -> Result<DepthUpdates> {
        let mut cache = self.cache.lock().map_err(|_| "feed_cache_unavailable")?;
        Ok(std::mem::take(&mut cache.depth_updates))
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
                cache
                    .points
                    .iter()
                    .map(|(key, point)| {
                        let mut price = point.clone();
                        // Notifications apply these receipts in order, ahead of strategy replies.
                        if cache.held.contains(key) || cache.strategy_keys.contains(key) {
                            price.depth = None;
                        }
                        (key.clone(), price)
                    })
                    .collect()
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
            if settings.full_depth() {
                if let Some(sample) = scan.samples.get_mut(&instrument.key) {
                    sample.scan_activity = None;
                    if sample.price_kind.as_deref() == Some("ltpc") {
                        sample.fresh_for_ms = Some(MAX_FINANCIAL_AGE_MS);
                    }
                }
            } else {
                scan.missing(&instrument.key, "no_quote_returned");
            }
            continue;
        };
        if activity.is_none_or(|at| {
            now.checked_sub(at)
                .is_none_or(|age| !(0..=settings.scan_activity_timeout_ms).contains(&age))
        }) {
            scan.missing(&instrument.key, "stale");
            continue;
        }
        if let Some(store) = feature_store.as_deref_mut() {
            super::wide::observe_feature(store, &instrument.key, point.price, point.receipt);
        }
        observe_point(scan, instrument, point, settings, now, activity)?;
    }
    Ok(())
}

pub(super) fn observe_point(
    scan: &mut Scanner,
    instrument: &Instrument,
    point: &Point,
    settings: &Settings,
    now: i64,
    activity: Option<i64>,
) -> Result<bool> {
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
    let (price, source, receipt) = retained.unwrap_or((point.price, point.source, point.receipt));
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
    observe_depth(sample, instrument, point)
}

/// Pushed depth uses the instrument's tick grid and keeps the receipt clock.
pub(super) fn observe_depth(
    sample: &mut nivra_core::execution::Sample,
    instrument: &Instrument,
    point: &Point,
) -> Result<bool> {
    let Some(mut data) = point.depth.clone() else {
        return Ok(false);
    };
    let clock = data.clock;
    if sample
        .intraday
        .as_ref()
        .is_some_and(|old| clock.source < old.clock.source || clock.receipt <= old.clock.receipt)
    {
        return Ok(false);
    }
    if let Some(touch) = &data.touch {
        let grid = CashOrderGrid::new(1, instrument.tick)?;
        if grid.validate(1, touch.bid).is_err() || grid.validate(1, touch.ask).is_err() {
            data.touch = None;
        }
    }
    sample.observe_intraday(data);
    if sample
        .intraday
        .as_ref()
        .is_some_and(|depth| depth.clock == clock)
    {
        // Pushed touch carries no REST open-interest receipt.
        sample.open_interest = None;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
