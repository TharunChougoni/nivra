# Market data

Nivra reads all market data from Upstox with the application's own token.

## Data sources

| Source | Used for | Can it set a fill price? |
|---|---|---|
| Upstox v3 WebSocket feed, LTPC mode | Screening prices for the whole cash universe | No |
| REST full-market quotes | Screening metadata, the REST fallback, and executable quotes with depth | Yes, if no more than 2 s old |
| v3 index keys on the same feed | Index context in the decision brief | No |
| v3 historical candles | Daily feature history and research shards | No |

## Shared market archive

TimescaleDB stores full-quote and candle receipts across sessions. Candle
acquisition, preparation, and research queries require the archive. It preserves
the original receipt envelopes, including provider bodies, as zstd-compressed
bytes. It stores prices rounded to the nearest paise
and leaves missing quote depth null. A candle attempt records coverage for each
date in its request window.

Set [`market_archive`](../packaging/rust/CONFIGURATION.md#market-archive) in
the paper configuration. Run `nivra archive init --config PAPER_JSON` to
create or upgrade the schema. Run `nivra archive check --config PAPER_JSON`
to see its version and row counts as JSON. You can repeat either command.

TimescaleDB partitions quote observations by receipt time and minute bars by
bar time. The compression policies group rows by instrument and compress
chunks older than one day. Backfill commits each successful receipt, its bars,
and its coverage in one transaction.

Use a disposable TimescaleDB database to run the archive integration tests.
They write synthetic rows.

```sh
export NIVRA_ARCHIVE_TEST_URL=postgresql://postgres:test@127.0.0.1:55434/postgres
cargo test --locked -p nivra-store --test market_archive -- --ignored --nocapture
cargo test --locked -p nivra-cli --bin nivra import_history_twice_and_measure_coverage -- --ignored --nocapture
```

## Read quota and quote batches

Every REST read and feed authorization counts in one shared read journal.
Nivra caps reads at 45 per second, 450 per minute and 1,800 per 30 minutes.
These caps are 90% of the
[published Upstox limits](https://upstox.com/developer/api-documentation/rate-limiting/).
A failed request still uses its reservation. Feed pushes use no reservation.
History starts are spaced at least 1.25 seconds apart, or at most 1,440
per 30 minutes from pacing alone. The shared read cap and configured headroom
still apply. This is Nivra's starting pace. Upstox can throttle below its
published limits.

One REST quote request carries at most `quote_keys.max_keys` keys (default and
maximum 200). `market_feed.rest_batch_size` (default and maximum 200) sets the
keys in one metadata or fallback batch. `quote_keys.prefixes` lists the key
prefixes that Nivra may request, by default `NSE_EQ|`.

## Two-second freshness gate

An executable quote must be no more than 2 seconds old by both its source time
and its receipt time. A quote with a receipt time in the future also fails.
The limit is `MAX_FINANCIAL_AGE` in `rust/nivra-core/src/market.rs`. Validation,
fills and protection check it. Nivra keeps the original receipt time and never
replaces it with a later clock reading. Feed prices, index levels and candles
never pass this gate.

## The v3 feed

To turn on the feed, merge
[the discovery fragment](../packaging/rust/scanner-discovery.fragment.json)
into the planner `discovery` settings. Without a `market_feed` block,
`mode` is `rest` and scanning uses REST quotes only.

With `mode` set to `ltpc`, the feed works as follows:

1. Nivra calls `GET https://api.upstox.com/v3/feed/market-data-feed/authorize`
   for a single-use WSS URL on every connection attempt. It never logs the URL.
2. It opens one connection and subscribes the whole cash universe in LTPC
   mode, at most 5,000 keys. It decodes frames with
   [MarketDataFeed.proto](../rust/nivra-cli/src/runtime/market_feed/MarketDataFeed.proto).
3. REST full quotes refresh screening metadata every `metadata_poll_ms`.
   Metadata includes volume and circuit bands, which LTPC lacks. REST depth
   reads cover the shortlist, held positions and outstanding model actions.
   See [Research quote tape](#research-quote-tape) for the top-K names the
   shortlist read also carries.
4. Every `tick_ms`, the scanner ranks the cached prices. It computes the market
   median from all fresh prices before it filters or truncates.

An LTPC price stays eligible for screening while the connection has had a
heartbeat or message within `scan_activity_timeout_ms`. REST samples use
`max_scan_age_ms` instead. An invalid price, such as `ltp` 0 before the first
trade, skips that instrument.

The feed worker keeps LTPC screening prices in memory. The market archive
stores REST quote receipts and their original receipt times.

Main `market_feed` settings:

| Key | Default | Effect |
|---|---:|---|
| `mode` | `rest` | `rest` or `ltpc`. |
| `tick_ms` | 2000 | Time between scanner ranks. |
| `scan_activity_timeout_ms` | 30000 | Connection activity window for price eligibility. |
| `heartbeat_timeout_ms` | 30000 | Silence before a reconnect. |
| `metadata_poll_ms` | 8000 | Time between REST metadata batches. |
| `fallback_poll_ms` | 8000 | Time between REST batches in fallback. |
| `fresh_drop_alert_fraction` | 0.5 | Drop in fresh prices that raises `scanner_fresh_drop`. |

The fragment lists the other keys with their defaults.

### Market archive

Set `market_archive.url` to a PostgreSQL URL without a password and
`market_archive.password_file` to an absolute path to a private mode-0600 file.
Run `nivra archive init --config PAPER_CONFIG` before starting the controller.

The paper controller requires the archive. It stages each REST quote receipt
in the session's `archive-spool.sqlite` before writing its SHA-256 reference
to the paper book. The book records the original event and receipt times.
A background writer stores the exact envelope compressed with Zstandard and
writes the normalized observations in the same archive transaction. A provider
row with invalid fields stays in the envelope and is skipped during
normalization.

The spool holds up to 64 MiB per session, plus a transient SQLite rollback
journal. If staging fails, the controller stops and records an `archive_gap`
when it can still write to the book. The writer retries queued receipts in
order and also drains spools under earlier dated session directories.

Research queries and tape exports use the book's time bounds and ordering to
read from the archive. A receipt that is still queued or missing causes a
read error. Use the same archive configuration when opening the book. Pass
the paper configuration to `strategy-lab source` and `strategy-lab tools`
with `--archive-config`.

To copy original quotes from a closed, checkpointed book, run
`nivra archive import-book --config PAPER_CONFIG BOOK_DIR`. The command
verifies identical receipts on repeat runs and refuses a changed receipt
for an already imported tick. Add `--rewrite` to replace verified inline
receipts with archive references and reclaim SQLite space.
The summary counts compacted legacy rows whose original body is unavailable.
With `--rewrite`, those rows become explicit gaps at their original times.

### Research quote tape

Each name that enters the top `top_k` of the live ranking (see
[AGENT](AGENT.md#triggers)) joins the shortlist depth read from that scan
until 30 minutes after it last ranked there. The names share the existing
request every `depth_poll_ms` (8 s), after the shortlist, so they use no
extra reads. Held names are skipped because the protection read covers them
every 2 s. The journal records ordinary `wide_quotes` with the original
receipt times used by the two-second gate.

With the default 40-name shortlist, 160 keys are left for watched names
before the 200-key batch is full. Nivra never adds a read to cover a gap. It
records one `quote_tape_gap` tick instead, with `reason`, `watched` and
`dropped`:

| Reason | When |
|---|---|
| `depth_batch_full` | More watched names than the batch has room for. The most recently ranked names are kept. |
| `feed_fallback` | The depth read pauses in fallback. The rotating REST scan still records each name once per pass. |

A controller restart forgets names that had already left the top K.

## REST fallback

The scanner switches to REST quotes when the feed is disconnected, has no
prices for the screening keys, or has been silent for longer than
`scan_activity_timeout_ms`.

In fallback, Nivra polls REST batches every `fallback_poll_ms` and sends an
alert. It sends a recovery notice when streaming returns. A reconnect or a
token change clears the price cache, so each instrument needs a new valid
price. Reconnects use exponential backoff and subscribe the whole universe
again.

`scanner_fresh_drop` fires when the fresh-price count falls by more than
`fresh_drop_alert_fraction` between two streaming scans. Fallback resets its
baseline.

## Index feeds

The session config can add NSE index keys to the same LTPC connection. The
feature is off by default.

```json
{
  "index_feeds": {
    "enabled": true,
    "keys": ["NSE_INDEX|Nifty 50", "NSE_INDEX|Nifty Bank", "NSE_INDEX|India VIX"]
  }
}
```

Before you enable it, add `NSE_INDEX|` to `quote_keys.prefixes`. Otherwise
config validation fails with `index_feeds_prefix_not_allowed`. The section
allows at most 64 keys.

Index keys never enter the cash universe, the scanner ranking or the screens.
A cash-equity paper mandate refuses an index instrument. The dashboard health
view and the decision brief's `index_context` show each index level. An
optional `index_feeds.regime` adds a regime label to that context.

## Historical candles

`history-download` reads v3 candles for the frozen universe of a config. It
uses the shared read journal and never starts a session.

```sh
nivra history-download --config PAPER_CONFIG \
	--start 2025-10-17 --end 2026-10-16 --interval days
```

`--interval` is `minutes`, `days` or `both` (default `both`). Without dates,
the range ends yesterday in IST and covers the twelve months before it.
Dates with observed bars or covered empty responses are reused on the next
pass. A daily answer that ends early leaves the tail eligible for retry.
Minute downloads reuse the full range of a successful response. A conflicting
bar stops the write and rolls back the request.

History reads leave headroom for other readers:
`history_download.reserve_second_reads` (default 2),
`reserve_minute_reads` (default 30) and `reserve_half_hour_reads`
(default 300).

## Backfill and pre-open preparation

The feature store needs `daily_history_sessions` (default 21) daily bars per
instrument. To fetch the missing sessions for a config, run:

```sh
nivra history-backfill --config PAPER_CONFIG
```

Backfill uses archive coverage and the reviewed NSE calendar. It requests
only missing trading-session spans. Add `--check-only` to verify every
required bar without a provider call. For a new config whose universe is
not frozen yet, add `--prepare-universe`.

After midnight IST, `history-backfill --config PAPER_CONFIG --upcoming`
can prepare the upcoming reviewed session from the current frozen universe,
even before the session pointer advances. It takes the same archive download
lock as pre-open preparation. Start it early enough to finish before that
window. A 2,760-instrument pass takes at least 57 minutes at the default pace.

With `history_preparation_enabled`, the controller runs the same acquisition
before the open. It reports coverage as `READY`, `PARTIAL` or `BLOCKED`, and
separates missing bars no further fetch can fill (`covered_empty_bars`: the
provider answered past the day, or answered no bar at all for that instrument,
as with listing or suspension gaps) from gaps an attempt may still fill
(`retryable_missing_bars`: unfetched days, and days after the last bar the
provider returned for that instrument, which may not be published yet). A
`PARTIAL` pass stops retrying for the session when it has zero
`retryable_missing_bars`, no fetch failures and at least one ready instrument. A
pass with no ready instrument cannot be admitted, so it keeps a bounded attempt
booked instead of parking the session. HTTP 429 ends the pass on its first
occurrence. A private sidecar beside the original read journal prevents
history callers from sending requests during the cooldown, including after a
restart. The cooldown is at least 10 minutes and honors longer
`Retry-After` values. After consecutive rate-limited attempts, the controller
waits at least 20 minutes. The existing cutoff and attempt limit still apply.
For other transient fetch errors, preparation skips at most
`history_preparation_max_fetch_failures` (default 25) instruments
whose fetch fails. A manual download fails on the first error.

## Coverage gaps

- A successful or empty provider response does not prove that every bar
  exists. Check coverage with `history-backfill --check-only`.
- An HTTP 400 marks the affected dates unavailable. With
  `history_skip_unavailable`, the planner skips those dates. Unavailable dates
  still count as missing in the coverage report.
- Transient provider errors leave retryable coverage. A failure cannot remove
  an observed bar or erase an earlier successful empty answer.
- Candles have no bid, ask, depth or circuit data. A candle backtest cannot
  prove intrabar fills.
- LTPC has no volume or circuit bands. Nivra never estimates volume from
  last-trade quantity.
- [Dashboard](DASHBOARD.md) minute bars mark minutes with no feed as
  `unobserved`.

## Research from archived daily bars

Run `nivra exit-backtest --config PAPER_CONFIG --output REPORT.json` to
simulate exploratory daily exits. It uses instrument ticks from the frozen
universe and accepts optional `--start` and `--end` dates.

Build bounded research datasets for the offline analysis runtime with this
command.

```sh
nivra archive export-daily-history --config PAPER_CONFIG NEW_DIRECTORY \
	--start 2025-10-17 --end 2026-10-16
```

The export writes dated closes and an inventory of shard hashes. See
[research](RESEARCH.md) for how the offline analysis runtime uses them.
