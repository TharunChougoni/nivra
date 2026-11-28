# Dashboard

The dashboard is a read-only React workspace for watching a paper session. The
`nivra` binary serves it from committed static assets at `/` on
`127.0.0.1:8771`. Page load makes no Node, CDN or provider call. The dashboard
has no trading controls.

## Open the dashboard

The paper worker serves the dashboard for its running session. Open
`http://127.0.0.1:8771/` and enter the dashboard token.

To use the Backtest lab without a session, start the research-only dashboard.
It shows no controller state. Stop the paper worker first, because both bind
the same port.

```sh
nivra dashboard --token-file TOKEN_FILE [--config PAPER_CONFIG]
```

The token file must hold at least 32 characters. With `--config`, the
dashboard uses that config's `dashboard` settings and `dashboard_public_host`.

To replace the token, run the command below and restart the service. It writes
32 random bytes as hex, or the token you pass, to the token file with mode
`0600`.

```sh
nivra dashboard-token rotate --config PAPER_CONFIG [--token TOKEN]
```

## Authentication and request rules

- Every `/api/v1` route needs `Authorization: Bearer <token>`. The browser
  keeps the token in session storage for the tab. **Lock** clears the token,
  the loaded data and the chart cache.
- The server accepts the `Host` `127.0.0.1:8771`. If the config sets
  `dashboard_public_host`, it also accepts that host over HTTPS. It refuses
  any other `Host` or `Origin`.
- Failed authentication draws from a token bucket of
  `dashboard.failed_auth_per_minute` attempts (default 30). An empty bucket
  returns `429` with `Retry-After`.
- Query strings are limited to 2 KiB, strict UTF-8 percent decoding, known
  keys and no duplicate keys. Errors are `{"v":1,"error":"<code>"}`.
- Only `POST /api/v1/backtest` accepts a method other than `GET`. Unknown
  paths return `404`.
- A pool of `dashboard.workers` threads (default 4) serves a queue of
  `dashboard.queue` requests (default 16). A full queue returns `503 busy`
  with `Retry-After: 1`.

The static login page and assets carry no account data. `/SOURCES.md` and
`/LICENSES.txt` list the vendored sources and licences.

## What you see

A status bar shows the mode, the data and order brokers, the account alias,
the session phase, feed age, stream state, alert count and the IST clock.
**Ctrl+K** or **Cmd+K** opens section navigation.

The mode pill shows **Paper**, **Live** or **Mode unknown**. The dashboard
shows **Live** only when the meta and summary responses agree on live mode.
A missing or contradictory report shows **Mode unknown**.

| Group | Section | Route | Shows |
|---|---|---|---|
| Trade | Overview | `#/overview` | Equity, day P&L net of fees, fees, cash, exposure, drawdown, fills and model spend. Equity chart, lane activity, positions and alerts. |
| Trade | Portfolio & fills | `#/portfolio` | Positions, order lifecycle, fills and plans with their validation verdicts. |
| Trade | Instruments & F&O | `#/instruments` | The verified cash catalog, captured instrument metadata and option chains. |
| Agents | pi lanes | `#/lanes` | Each model lane as a chat: the brief, the thinking, the tool calls and the answer. Also lane stats and charts. |
| Market | Scanner | `#/scanner` | The current prospects, missing or stale instruments and screen totals for NSE cash equities. |
| Research | Strategies | `#/strategies` | The strategy run on each scan: its author, accepted result, recent actions, gate refusals and fills. A forward test beside it, with coverage, skipped scans and the two books' outcomes side by side. Also strategy references from the scanner and the registered research library. |
| Research | Backtest lab | `#/backtest` | Offline replay of a manifest and its quote events. |
| System | Sessions | `#/sessions` | Current and archived paper sessions. |
| System | Health | `#/health` | Feed, history, controller, research, calendar and alert status. |

The old routes `#/models` and `#/runs/*` open `#/lanes`. The pi lanes are
decision, gate, research and review. See [agent lanes](AGENT.md) and
[direction](DIRECTION.md).

How the dashboard treats evidence:

- It shows a value only when a report recorded it. A missing value reads
  **Not reported**. The dashboard never fills a gap with zero or a guess.
- An order shows five stages: intent, validated, submitted, acknowledged and
  filled. Only a fill record marks it filled. An acknowledgement is never
  shown as a fill.
- Charts use the recorded receipt times. The browser's clock never replaces a
  quote time.
- Minute bars come from quote receipts, not exchange OHLCV. A `no_trade`
  minute draws as a flat step. An `unobserved` minute, such as during a feed
  outage, breaks the line and is shaded.
- Past sessions and multi-day instrument history load on demand from the
  TimescaleDB market archive when `market_archive` is configured, without
  holding historical series in controller memory. Today's live view keeps
  streaming from controller memory over SSE.
- A failed read clears that section's financial values instead of showing
  stale ones.

## Refresh

The browser opens one server-sent event stream at `/api/v1/events`. If the
stream fails, it falls back to a held long poll. The stream is gzipped when
the browser accepts gzip.

The stream carries two frame types:

- `summary_changed` carries the summary ETag and `revisions`, a content hash
  for each projection: `portfolio`, `fills`, `plans`, `scanner`, `sessions`,
  `health`, `alerts`, `agent` and `agent_turns`. The browser refetches only
  the projections whose hash changed. The server sends a changed revision at
  most once a second, and `scanner` at most once every 15 seconds. With no
  change, it still sends the summary every 5 seconds.
- `price_batch` carries the latest receipt price of recently observed
  instruments, every `dashboard.price_batch_interval_ms` (default 2000). A
  new stream gets a full frame. Later frames carry only instruments whose
  receipt moved.

**Refresh**, a stream gap and returning to the tab refetch every projection.

Only the controller loop sends `summary_changed`. Stream heartbeats and
`price_batch` frames keep flowing when the loop stalls. If no
`summary_changed` arrives for 15 seconds (three summary refreshes), the browser
reads the summary and health directly. When health reports the controller
stale, the state banner says the controller has stopped updating and names its
last update time. The browser clock never decides this. The next `summary_changed` clears the banner.

## API routes

All routes live in `rust/nivra-cli/src/runtime/web/` and return JSON with
`"v":1`. List routes take `cursor` and `limit` and return `next`.

| Route | Returns |
|---|---|
| `GET /api/v1/meta` | Version, paper or live mode, account alias, broker roles, dashboard limits and capability names. |
| `GET /api/v1/summary` | Session phase, identities, readiness, equity, cash, `sleeves`, counts, feed and alerts, plus `revisions`. Built once per trading loop. |
| `GET /api/v1/portfolio` | Cash, capital, the allocation ledger, `sleeves`, marks, positions, fees and book state. |
| `GET /api/v1/fills` | Simulated fills, newest first. |
| `GET /api/v1/plans?date=` | Recorded plan actions, execution verdicts and planner status for the loaded session. |
| `GET /api/v1/scanner?segment=prospects\|missing\|screens` | Scanner rows, coverage and reasons for missing instruments. |
| `GET /api/v1/instruments` | The verified NSE cash catalog. Add `scope=all` for captured metadata, filtered by `market_segment`, `kind` and `q`. |
| `GET /api/v1/instruments/chain?underlying_key=&expiry_ms=` | The call, strike and put chain and matching futures for one underlying and expiry. Optional `market_segment`. |
| `GET /api/v1/sessions` | Current and archived session checkpoints. |
| `GET /api/v1/health` | Readiness, feed and history state, controller liveness, retention, model budgets, alerts and updater status. |
| `GET /api/v1/alerts` | Alerts with their original evidence. |
| `GET /api/v1/series?kind=receipt-minute-bar&instrument=&from=&to=` | Minute bars from quote receipts, with classified gaps and `watermark_ms`. |
| `GET /api/v1/series?kind=archive-minute-bar&instrument=&from=&to=&limit=` | Paged minute bars from the market archive (`minute_bars` where present or `quote_obs` bucketed by minute). Max range 14 days; max `limit` is `series_max_points`. |
| `GET /api/v1/series?kind=archive-daily-bar&instrument=&from=&to=&limit=` | Paged daily bars from `daily_bars` in the market archive. Max range 366 days; max `limit` is `series_max_points`. |
| `GET /api/v1/series?kind=account-equity&from=&to=` | Recorded portfolio checkpoints: cash, equity and mark basis. |
| `GET /api/v1/agent` | pi lane turns, newest first, with tool calls, usage, trigger counts and per-lane totals. `limit` is 1 to 200, default 50. |
| `GET /api/v1/agent/transcript?lane=` | One lane's newest eight session files as chat exchanges. `limit` is 1 to 50, default 20. System prompts are not served. |
| `GET /api/v1/events` | The event stream. `?poll=1&after=N` is the long-poll fallback. |
| `GET /api/v1/backtest/example` | A labelled synthetic backtest input. |
| `POST /api/v1/backtest` | Runs an offline replay. Returns `409 backtest_paused_for_trading` unless the research-only dashboard serves it. |

`sleeves` lists each capital sleeve (`cash`, `options`) with its allocated
`capital`, `cash`, marked `equity`, `pnl` and `open_positions`. The Paper
account panel on the overview shows them as "Stock strategies" and "Nifty
options".

`meta`, `health` and the lane routes support ETag revalidation. Financial
projections are `no-store` or `private, no-cache`.

## Backtest lab

Paste a manifest and its quote events in NDJSON. The shared replay engine
checks the engine and data hashes, the order of events, identities, fees,
slippage and open exposure. It does not turn candles into quotes and does not
invent a closing fill. The manifest is limited to 512 KiB and the events to
4 MiB. The compute deadline is `dashboard.backtest_timeout_ms` (default 3000).

The browser keeps at most ten reports and can export each one as JSON. For the
same replay on the command line, run:

```sh
nivra backtest-fingerprint
nivra backtest --manifest MANIFEST --events EVENTS
```

[Shared replay](reference/SHARED_REPLAY.md) describes the formats.

## Settings

The `dashboard` block of the paper config holds these keys. A key at its
default is left out of the serialized config. The config loader validates the
range of each key.

| Key | Default | Effect |
|---|---:|---|
| `workers` | 4 | HTTP worker threads. |
| `queue` | 16 | Requests waiting for a worker. |
| `gzip_min_bytes` | 1024 | Smallest response that is gzipped. |
| `backtest_timeout_ms` | 3000 | Backtest compute deadline. |
| `sse_max_streams` | 3 | Concurrent event streams. Extra clients use long poll. |
| `sse_heartbeat_ms` | 15000 | Event stream heartbeat. |
| `sse_write_timeout_ms` | 30000 | Timeout for one stream write. |
| `longpoll_hold_ms` | 25000 | Long-poll hold time. |
| `transcript_text_max_bytes` | 16384 | Cap on transcript text per field. |
| `transcript_args_max_bytes` | 4096 | Cap on tool arguments per call. |
| `series_max_points` | 2000 | Points in one series response. |
| `series_max_range_ms` | 86400000 | Longest `from` to `to` span. |
| `equity_max_points` | 4096 | Account-equity checkpoints kept in memory. |
| `equity_sample_interval_ms` | 15000 | Least time between equity checkpoints. |
| `price_batch_instruments` | 24 | Instruments in a full `price_batch` frame. |
| `price_batch_interval_ms` | 2000 | Time between `price_batch` frames. |
| `failed_auth_per_minute` | 30 | Failed authentication attempts allowed per minute. |
| `retention.pack_after_days` | off | Pack raw model archives after this many days. |
| `retention.delete_after_days` | off | Delete raw model archives after this many days. |
| `model_prices.*` | none | INR per million input, output and cached tokens, for cost figures. |
| `instrument_catalog` | none | Leave unset for the NSE BOD archive. Set `source` to `"complete"` for the complete-market BOD capture. |

The config still accepts `static_max_age_s`, `history_read_max_bytes`,
`history_list_cache`, `history_max_files`, `model_feed_unavailable_grace_ms`,
`transcript_max_bytes` and `blob_slice_max_bytes` so that existing config
hashes stay the same. The workspace does not use them.

[Operations](OPERATIONS.md) describes archive retention.

## Change the frontend

The source is in `rust/nivra-cli/src/runtime/web/v2`: Vite, React and strict
TypeScript. The built `bundle/` is committed and embedded in the binary.
After a frontend change, run the
[dashboard frontend gate](../AGENTS.md) and commit the source, the lockfile
and the bundle together.

To work against a synthetic trading day, run the command below. It serves the
workspace on `127.0.0.1:5173` with a fixture API. Unlock it with any 64-hex
token. The fixture API never enters the built bundle.

```sh
npm run dev:mock --prefix rust/nivra-cli/src/runtime/web/v2
```
