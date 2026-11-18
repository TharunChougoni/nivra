# Upstox V3 screening feed

Reviewed against official documentation. Paper only. Tests use an in-memory
mock transport and do not contact a live feed.

## Provider limits and protocol

[Standard REST limits](https://upstox.com/developer/api-documentation/rate-limiting/)
are **50 requests/second, 500/minute and 2,000/30 minutes**, per API/user. The
[full quote endpoint](https://upstox.com/developer/api-documentation/get-full-market-quote/)
accepts up to **500 keys/request**. Nivra counts every read in its own journal
scope against local caps of **45/s, 450/min, 1,800/30-minute**, 90% of the
published limits. Because the provider enforces limits per API, pooling all reads
under one cap keeps each API below its own limit. Nivra keeps its 200-key request
ceiling. One prior-session daily refresh of the 2,746-instrument universe is one
request per instrument, so the half-hour cap bounds it at roughly 46 minutes. Failed reservations/requests are never refunded.

The [normal V3 feed limits](https://upstox.com/developer/api-documentation/v3/get-market-data-feed/)
are **2 connections/user**. Single-category subscription ceilings: LTPC **5,000**,
full **2,000**, option Greeks **3,000**. Mixed categories reduce LTPC/Greeks to
**2,000** each and full to **1,500**, across the user rather than independently
per connection. No Plus entitlement is assumed. Consequently this adapter opens
one LTPC-only connection; shortlist depth uses REST, avoiding mixed-mode ceilings.
WebSocket pushes consume no REST quote reservations; authorization and REST reads
still use the original shared journal. This is not an unlimited-throughput promise.

[Feed authorization](https://upstox.com/developer/api-documentation/get-market-data-feed-authorize-v3/)
uses `GET https://api.upstox.com/v3/feed/market-data-feed/authorize` with the
application's existing access token. The returned `authorized_redirect_uri` is a
single-use WSS URL; every connection attempt obtains a new one. URLs, codes,
tokens and provider error bodies are never logged. Only TLS Upstox hosts are
accepted. Subscribe requests are binary JSON; responses are V3 protobuf. Ping
frames receive pong replies. The checked-in schema is
[MarketDataFeed.proto](../rust/nivra-cli/src/runtime/market_feed/MarketDataFeed.proto),
from [Upstox's schema](https://assets.upstox.com/feed/market-data-feed/v3/MarketDataFeed.proto).

## Configuration

Merge [this config fragment](../packaging/rust/scanner-discovery.fragment.json)
into the current dated planner `discovery` settings; retain
its existing scan/trigger settings. Existing configs without `market_feed` keep
REST scanning. Apply planner changes through the documented reviewed config flow.

```json
{
  "screens": {
    "min_price": "10",
    "min_traded_value": "1000000",
    "min_circuit_distance_bps": "100"
  },
  "market_feed": {
    "mode": "ltpc",
    "depth_mode": "rest",
    "max_connections": 1,
    "max_connect_addresses": 2,
    "connect_timeout_ms": 3000,
    "handshake_io_timeout_ms": 3000,
    "read_poll_timeout_ms": 250,
    "worker_idle_sleep_ms": 25,
    "shortlist_size": 40,
    "tick_ms": 2000,
    "stale_age_ms": 15000,
    "scan_activity_timeout_ms": 30000,
    "fresh_drop_alert_fraction": 0.5,
    "telemetry_window_ms": 60000,
    "metadata_poll_ms": 8000,
    "metadata_stale_ms": 300000,
    "metadata_preload_max_age_ms": 900000,
    "depth_poll_ms": 8000,
    "fallback_poll_ms": 8000,
    "rest_batch_size": 200,
    "reconnect_initial_ms": 1000,
    "reconnect_max_ms": 60000,
    "heartbeat_timeout_ms": 30000,
    "token_check_ms": 1000,
    "source_clock_skew_ms": 1000
  }
}
```

These are adapter defaults except `mode`, which defaults to `rest`.
`max_connect_addresses` bounds DNS address attempts to 1–64 (default 2).
`connect_timeout_ms` (default 3000) applies per TCP address;
`handshake_io_timeout_ms` (default 3000) sets socket read/write timeouts for
the handshake, with the write timeout retained after connection.
`read_poll_timeout_ms` (default 250) replaces the read timeout after the handshake.
`worker_idle_sleep_ms` (default 25) sleeps after every worker step. All four
durations accept 1–86400000 ms. Omitted fields retain the previous timings.
`source_clock_skew_ms` (0–5000) tolerates provider/exchange timestamps slightly
ahead of the local receipt clock; the stored source time is capped at receipt.
An invalid LTPC for one instrument (e.g. `ltp` 0 before its first trade) skips
that instrument and is counted in `invalid_ltpc`; it no longer drops the frame
and reconnects. Frames containing only invalid prices count as transport activity
but cannot supply a first valid price for an instrument. Scanner/discovery eligibility
uses `scan_activity_timeout_ms` (default 30000 ms, validated 100–86400000 ms):
a heartbeat or decoded message within that window keeps prices received during
this connection eligible, even when a quiet instrument has not traded for minutes.
`max_scan_age_ms` continues to govern REST samples; LTPC samples use the connection
window, including strategy feature extraction. Disconnects fail closed and every
reconnect/token rotation clears the price cache, requiring a new valid price per
instrument. `heartbeat_timeout_ms` still controls transport reconnect timing;
`stale_age_ms` still rejects delayed incoming frames, rather than expiring quiet
cached instruments. Metadata retains its separate `metadata_stale_ms` expiry.
Original source, price receipt and metadata receipt times are never refreshed by
heartbeats. This is screening only: executable REST quotes and the two-second
financial gate for orders/fills are unchanged. A frame-wide source timestamp beyond the
configured skew tolerance still fails the connection and uses configured retry
backoff. Offline controller fixtures cover these feed transitions; whole-session
feed stability still requires genuine operating evidence.
Each discovery scan compares its fresh count with the previous scan.
`fresh_drop_alert_fraction` defaults to 0.5 and must satisfy 0 < value ≤ 1.
A drop strictly greater than that fraction emits one `scanner_fresh_drop` warning
through the existing alert delivery/deduplication path and one log line with
previous/current fresh counts, stale/unseen counts, the five most common sample
reasons, and feed state/reason/connected/heartbeat age. The first scan and a zero
previous count do not alert; a value of 1 cannot trigger a strictly greater drop.
Feed health is judged on a clock read at snapshot time. Scanner screen
defaults are the operator-selected ₹10, ₹10 lakh and 100 bps. Amounts use decimal
strings. Distance is `(price − lower)/price` and `(upper − price)/price; equality
at the configured minimum passes, but an actual circuit touch always fails.
Traded value means reported cumulative volume × current screening price, an
estimate rather than certified exchange turnover. Invalid/missing fields fail
closed. Rejections distinguish price, traded value, lower circuit, upper circuit
and invalid circuit bands. Screens restrict prospects/new buys, never protective
exits or model-selected sells on held positions.

LTPC supplies prices and previous close, not cumulative volume/circuit bands.
Rotating REST metadata snapshots therefore continue at `metadata_poll_ms` per
batch; these full-quote responses inherently include depth, which is not used to
rank LTPC prices. Additional depth reads are limited to the shortlisted prospects,
held positions and outstanding model actions. Metadata has its own original
receipt and expiry; LTPC updates cannot freshen it. A zero/missing volume is not
estimated from last-trade quantity. Requests share the original quota, including
protection/history/model tools; a limit can defer optional reads.

Every tick ranks the whole cache by retained price movement, filters candidates,
and computes the market median from **all fresh price samples before filtering
or truncation**. Trigger journals/logs include that baseline and population size.
Receipts are sampled once after complete frames; provider time and last-trade time
are distinct. LTPC points never fabricate bid/ask depth or authorize fills.
Execution/protection retains fresh REST depth and the two-second source/receipt gate.

Disconnects use bounded exponential backoff and re-subscribe the complete universe.
A changed application token closes the old connection and reauthorizes with a new
URL. Stale per-key prices become unavailable; connection/data loss activates
configured REST fallback and alerts, with a recovery notice when streaming returns.
Transitions also appear in the controller log and original book journal, including
when a webhook is not configured.
The dashboard reports connection/fallback state, last-tick age, reconnects,
screen counts and missing/stale/symbol-mismatch diagnostics. Fresh scan age excludes
missing/stale keys; missing responses never overwrite the last valid observation.
Only the first 100 unavailable rows are displayed, with the total/truncation explicit.
A symbol mismatch is accepted by the frozen instrument key and shown as a diagnostic;
account, broker, universe and session identities are unchanged.

Feed health includes process-lifetime `ticks_total` (accepted LTPC points) and
`tick_rate_per_s`: accepted points in the trailing `telemetry_window_ms` divided
by the full window in seconds, including during startup. The default is 60000 ms;
valid values are 100–86400000 ms. Points exactly one window old are excluded.
Reconnects retain these counters. The existing scanner health record includes
`scan_latency_ms` (monotonic ranking/projection and result-recording time),
`instruments_scanned` (whole dated universe), `eligible` and `stale` from the latest
scan; no additional telemetry files or sockets are opened.

At startup, `metadata_preload_max_age_ms` admits persisted per-instrument screening
metadata whose original receipt is within that age (default 900000 ms; validated
range 0–86400000; 0 disables). Preloaded fields retain their original receipt and
expire at this limit; fresh REST metadata returns to `metadata_stale_ms` expiry.
Incomplete REST metadata does not erase retained fields or renew their receipt.
Discovery health exposes `metadata_preloaded_count` and per-instrument
`metadata_preloaded` entries with `metadata_age_ms`. New connection prices are
still required. Executable quotes and their two-second freshness gate are unchanged.

## Non-tradeable index feeds

The session config (not the planner `discovery` block) accepts an optional
`index_feeds` section, disabled by default:

```json
{
  "index_feeds": {
    "enabled": true,
    "keys": ["NSE_INDEX|Nifty 50", "NSE_INDEX|Nifty Bank", "NSE_INDEX|India VIX"]
  }
}
```

The three keys are the documented Upstox NSE index identifiers; they are not
inferred from symbols. `enabled: false` is the default and keeps the section out
of the serialized config. Enabling requires the operator to also add `NSE_INDEX|`
to `quote_keys.prefixes`; otherwise config validation fails closed with
`index_feeds_prefix_not_allowed`. The existing 5,000-key LTPC ceiling still
applies to the combined subscription.

Configured keys join the **existing single LTPC connection** — no second
websocket, no paid feed, no mixed-mode ceilings. The decode allowlist includes
them, so their LTPC points (price, source/receipt, prior close) are cached with
the cash universe. They are never added to `Universe::instruments`, never enter
the cash scanner ranking or screens (no ₹10 / ₹10 lakh / circuit gates on index
levels), and `PaperMandate::CashEquity` refuses an index instrument, so a paper
order on an index key cannot execute. The cash market median and feature store
are unchanged: index minute returns use a separate feature store.

Index context is additive and read-only: the dashboard status carries an
`index_feeds` block (`observations` with price/source/receipt/prior close/change
bps/fresh, and `tradeable: false`), and the model digest carries `index_context`
in the `get_market_snapshot` output. An optional `index_feeds.regime`
(`RegimeConfig`) classifies index session return/gap and index minute returns
through `nivra_core::regime`; the label is advisory digest context only and grants
no trade authority. A configured regime requires `min_instruments` not to exceed
the configured index key count. Once `NSE_INDEX|` is in the quote-key prefix
allowlist, the v3 history path accepts the same keys and percent-encodes the
space (`NSE_INDEX%7CNifty%2050`). Tests use fixtures only; no index socket or
provider call is made by the suite.

`scanner_fresh_drop` compares consecutive connected `streaming` projections.
Disconnected feed and every `rest_fallback` state (including connection warm-up
or stale stream activity) suppress the alert and clear its baseline. The first
streaming projection after fallback seeds a new baseline; subsequent drops use
`fresh_drop_alert_fraction` unchanged. This avoids comparing opening REST/history
coverage to the first sparse live projection; other feed-loss alerts remain active.
