# Dashboard

The React workspace is served at `/` as committed static assets. There is no
Node, CDN or provider call on page load. The older `/legacy/`, `/v2/`,
`/assets/dashboard.*`, `/api/model-*` and `/api/status` paths are not served;
unknown non-v1 paths return 404. The updater probes `/api/v1/health`.
Unfinished work and telemetry qualification are tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

## Sources and authentication

The paper worker serves its own bound current snapshot on `127.0.0.1:8771`.
For a separate research-only workspace (Backtest lab and verification, no
controller state), use an existing private token file:

```sh
nivra dashboard --token-file /private/dashboard-token [--model-config PAPER_CONFIG]
```

Do not compete with the worker's listener. Open the loopback URL and enter the
token in the UI. Rotate the token file with `nivra dashboard-token rotate [--config PAPER_CONFIG] [--token TOKEN]`,
which generates 32 bytes of secure random hex entropy (or writes the provided token) with mode 0600;
a service restart is required. Static login/assets contain no account data; all data/status/model
streams/examples require bearer auth. Exact loopback/configured Funnel Hosts,
origin checks, no-store/CSP/frame/referrer headers and bounded requests apply.
The standalone dashboard also reads `dashboard_public_host` from its optional
`--model-config`; only that exact HTTPS Funnel authority is admitted in addition
to loopback. Any other Host or origin remains refused, and without a configured
host the server stays local-only. Public trading controls are absent; unknown non-v1 paths return 404.

Financial views distinguish continuing cash, dated P&L, marks, fees and unresolved
holdings; never add daily allocations as wealth. Scanner shows fresh/stale/unseen
coverage and timestamped prospects, not a fabricated expected-profit ranking.
When discovery publishes `strategy_selection`, the scanner also shows the selector
mode, chosen identity, live-ranking/fallback state and a bounded recent
switch/fallback log. Absent evidence is shown as not configured, never inferred.

## React workspace at `/`

The React workspace is client-rendered with Vite, strict TypeScript, React,
Radix Dialog, Lucide icons and uPlot, styled with plain CSS custom properties
(no CSS framework). It is a presentation layer; the Rust runtime owns trading.
The operator builds pinned npm dependencies locally; the generated static
`bundle/` is committed and embedded in the Rust binary. There is no Node
service, SSR, CDN, remote font or runtime package installation. The bundle is
served from `/`. `/SOURCES.md` and `/LICENSES.txt` carry the vendored-source
and licence manifest.

### Design system

The workspace is dark only. Colours are tokens in `src/tokens.css`:
near-black surfaces (`--bg-0`…`--bg-3`), three text weights, one desaturated
steel accent, and low-chroma semantic colours for gain (`--pos`), loss
(`--neg`) and warning (`--warn`). Each pi lane has its own muted colour
(`--lane-decision`, `--lane-gate`, `--lane-research`, `--lane-review`).
`--live` (amber) is reserved for live-mode chrome and is used nowhere else, so
a live session can never look like paper. Numbers, times and identifiers use
the system monospace stack with tabular figures and are right-aligned in
tables. Stylesheets are split by concern: `tokens`, `base`, `layout`,
`components`, `charts` and `pages`. Components read colours from tokens, never
from literals.

### Shell

A status bar shows the brand, the mode pill, the data and order brokers, the
account alias, the session date and phase, then feed age, stream state, alert
count, the IST display clock, Refresh and Lock. Under 600 px the labels
collapse to status dots. A ticker of recently observed instruments, from
`price_batch` receipts, sits below it. The navigation groups the sections as
Trade, Agents, Market, Research and System, shows counts only when they are
known, and collapses to icons (remembered per browser). Under 900 px it
becomes a focus-managed Radix dialog. `Ctrl/Cmd+K` opens section navigation.

Mode chrome comes from one derived value, `modeOf(meta, summary)` in
`src/model.ts`: `paper`, `live` (with order broker and account) or `unknown`.
Paper is neutral. Live adds a 2 px `--live` rule above the shell and a `LIVE`
pill. A missing or contradictory report shows `MODE UNKNOWN`. The broker
fields are display evidence and do not establish live readiness.

### Sections

| Group | Section | Route |
|---|---|---|
| Trade | Overview | `#/overview` |
| Trade | Portfolio & fills | `#/portfolio` |
| Trade | Instruments & F&O | `#/instruments` |
| Agents | pi lanes | `#/lanes` (`#/models` and `#/runs/*` land here) |
| Market | Scanner | `#/scanner` |
| Research | Strategies, Backtest lab | `#/strategies`, `#/backtest` |
| System | Sessions, Health | `#/sessions`, `#/health` |

- **Overview** shows a ten-cell figure strip: equity, day P&L net of fees,
  realized, unrealized, fees, cash, gross exposure, maximum drawdown, fills and
  pi spend. Day P&L and drawdown are computed only from recorded equity
  checkpoints, and the cell names that basis; otherwise it shows `—` with the
  reason. Below are the account-equity chart (fill markers, a session-start
  baseline and a synced drawdown pane, or a selected instrument's price), a
  "pi lanes now" rail, the lane timeline, positions, the latest order tracks,
  recent alerts and the latest lane turns.
- **Portfolio & fills** has positions, order lifecycle, fills and
  plans/validation tabs. Instrument-class filters derive only from recorded
  `instrument.kind` values (`index`, `cash`, `etf`, `future`, `call`, `put`).
  Derivative labels such as `NIFTY 25NOV 24500 CE` are built only from recorded
  underlying, expiry, strike and kind; missing parts are left out, never
  guessed. Expiry and margin show only when recorded; margin must match the
  exact instrument key and be marked display-only. A Greeks column appears only
  when a position carries recorded Greeks. Selecting a position or fill opens
  its recorded evidence and links only to plan actions and execution verdicts
  that match by `decision_id` and instrument.
- **Order lifecycle** shows each recorded plan action as five stages: intent,
  validated, submitted, acknowledged and filled. Validation is `done` only for
  the validator's `accepted` verdict and `failed` for any rejection code.
  `filled` comes only from a fill record. In paper, submitted and acknowledged
  are drawn as simulated (dashed) when a simulated fill exists and are absent
  otherwise. An ACK is never shown as a fill. Live order records can fill these
  stages later without a layout change.
- **Instruments & F&O** holds the verified cash catalog, the captured
  instrument-metadata catalog and the contract chain described in
  [Instrument metadata](#instrument-metadata-and-fo-contract-chains).
- **pi lanes** is described in [pi lanes view](#pi-lanes-view).
- **Scanner** offers original-priority, symbol and absolute-change sorting of
  prospects, missing observations and screen totals from the v1 scanner
  segments. Selecting an instrument opens its chart, original clocks, recorded
  strategy reference fields and raw evidence without losing the table. Its
  coverage is labelled as the current NSE_EQ cash scope, not all-market
  coverage.
- **Strategies** has two tabs. **Strategy references** shows references
  projected by scanner prospects (spec ID, version, hash, scan ID and recorded
  score); no library record is reconstructed from them. **Library & switches**
  shows the registered-research library described below.
- **Backtest lab** takes an original manifest/events pair, uses the local
  bounded replay endpoint and shows replay equity points, fees, fills and hash
  provenance. Synthetic examples are labelled; results stay in component memory
  and can be exported as JSON.
- **Sessions** merges the bounded v1 session archive with the current
  summary and portfolio identity, preserving archived checkpoints and original
  fill order. A mode shows only when the report supplied explicit
  `paper_only`/`live_enabled` provenance; older rows stay `Not reported`.
- **Health** separates transport, feed and history readiness, controller
  liveness, the read-only activity attribution and unconnected independent
  telemetry. A **Live readiness** panel shows the mode, the order broker
  identity and `live_enabled` as reported; it has no controls. Missing reports
  remain unavailable; the dashboard does not calculate or fill gaps. Net
  after-cost P&L subtracts fees; slippage is a diagnostic because it is already
  in fill prices. Drawdown coverage is incomplete because historical position
  marks are not retained.

### Frontend build gate

Node is a frontend build/CI tool, not a runtime dependency of the paper host. The
controller serves the committed static bundle; it does not run npm, Vite or a
Node service. After any frontend change, run from the repository root:

```sh
npm ci --prefix rust/nivra-cli/src/runtime/web/v2
npm run build --prefix rust/nivra-cli/src/runtime/web/v2
npm test --prefix rust/nivra-cli/src/runtime/web/v2
```

`npm test` runs the strict TypeScript check, the Node test files and
`check:bundle`. Commit `src/`, `scripts/`, `package.json`, `package-lock.json`
and the generated `bundle/` together; the bundle is embedded in the Rust binary.
A deterministic rebuild must reproduce identical source and bundle hashes. Do
not install Node globally on the paper host and do not add a runtime CDN, SSR or
package-install path.

To work on the UI with a full synthetic trading day, run:

```sh
npm run dev:mock --prefix rust/nivra-cli/src/runtime/web/v2
```

It serves the workspace on `127.0.0.1:5173` against a seeded fixture API
(`scripts/mock-api.ts`, `scripts/mock-data.ts`) that emits the real v1 wire
shapes, including revisions, delta `price_batch` frames and pi lane turns.
Unlock it with any 64-hex token. The mock registers only for `vite serve` with
`NIVRA_MOCK=1`; it never enters the built bundle. Its values are synthetic and
are not market or model evidence.

### Read-only research projections

The operational panel may additionally render owner research, prospective
observation, cross-session continuity, observation capture and independent
virtual episode measurements. These are optional, read-only projections: the
panel renders them only when the authenticated v1 summary/health snapshot
carries the captured fields. Missing fields stay `Not reported`; they are never
inferred as zero, idle, available or disabled.

The v1 `summary` and `health` responses copy the controller's already-captured
`owner_research` and `research_tasks` objects verbatim. The GET never samples a
new clock, reads the queue/state, recomputes the research status or renews
freshness; an absent captured field stays absent.

- Cross-session continuity reads `research_tasks.observation.lifecycle.milestones`.
  `research_library.items[].continued` / `continuation` is not used as
  continuation evidence. Original recorded state, effective state, effective
  scientific state, source session and recording session remain separate. The
  scientific vocabulary (`insufficient_evidence`, `eligible`, `rejected`,
  `retired`) is a separate map from recorded lifecycle states; the removed
  `continued_evaluated` value is never invented. A source-truncated milestone
  list is disclosed as truncated and never shown as a complete total.
- Workspace refs are shown only as a public scalar `workspace_ref_count` and
  `workspace_snapshot_sha256` when the captured projection supplies them. The
  count and the canonical hash of the pinned snapshot are captured with the
  selected evidence at the producer completion sample; a captured empty
  workspace is count `0` with a valid empty-array hash, while an uncaptured
  source stays `Not reported`. Raw workspace refs, paths, file contents and
  private filesystem reads are never exposed or recomputed on GET.
- Observation capture distinguishes journal readability
  (`coverage_label: journal_readability_only`) from current eligibility
  (`current_eligibility: not_evaluated`, `eligible_for_attempt:false`). It never
  claims a provider guarantee; its caps are resource limits, not trading risk
  caps.
- Native source-observation enumeration is rendered from the structured
  `observation_refs_status` / `truncated` / `partial` / `scanned` / `limit` /
  source-error fields; a raw score plus job hash is never promoted to a
  verified ref.
- Continuation receipts use typed `entry_quote` / `exit_quote` objects and their
  `archive_sha256`; `measurement_sha256` is a continuation receipt hash, not the
  immutable episode `projection_sha256`. The continuation id, source session and
  recording session are separate from the current read. Original exit, processed
  and recorded clocks are separate.
- Independent virtual episodes are not portfolio returns. The panel does not sum
  episode PnL, draw an equity curve, compute Sharpe or profit percentage, or
  claim promotion/activation. The canonical checked `measurement_status`
  (`verified`, `not_matured`, `unavailable`) is authoritative: an unavailable
  record is an explicit entry even when the effective body is null, the original
  stored status is preserved separately, and a missing or unsupported
  measurement version is never zero or green. A legacy protection replay source that is
  explicitly unavailable is shown as unavailable, never as copied unchanged.
- Trial debt from a partial read is a lower bound. The panel shows bounded gap
  reasons and keeps main counts `Not reported`; it does not extrapolate a total.
- Display limits are explicit: at most 5 measurement entries, 20 milestones and
  10 continued cross-session rows, with `Showing N of M` or a source-truncation
  note when the total is unknown. A source-truncated list is never presented as
  a complete total.

These panels add no route, request, trading control, provider fallback,
restart/unhalt, promotion or activation authority. This documentation does not
claim the panels are deployed, that a browser/HMR session was exercised, or that
the Source API is activated.

### Charts and available data

Time-series charts use one uPlot wrapper (`src/charts/TimeChart.tsx`) with
synced panes, markers, a token-coloured theme and an IST tooltip. Sparklines,
the lane timeline and bar charts are small SVG components in `src/charts/`.
uPlot is loaded only when at least two actual observations exist. Charts have
an IST crosshair/readout, 5m/15m/1h/day windows, drag-to-zoom and Pause/Follow.
Replay curves use the report's original times and values. Scanner prices use
`latest_screening_price` and original `sampled_price_ms` (legacy sampled-bid
fields remain supported); browser response time never replaces quote time.

The workspace loads a selected instrument's durable receipt-minute bars from
`GET /api/v1/series?kind=receipt-minute-bar` and the recorded account-equity
checkpoints from `kind=account-equity`. Points keep original receipt or
portfolio-checkpoint times. Receipt-minute bars are display evidence, not
certified exchange OHLCV; `tick_count` is never labelled or treated as volume.
Missing minutes are classified by the backend: `no_trade` means the instrument
was observed by a healthy feed, the store was live and no tick arrived;
`unobserved` means feed fallback/outage, restart or reset, or a minute before the
store started. The workspace carries `no_trade` minutes forward as a muted
dashed step at the previous receipt close, and breaks only on `unobserved`
intervals, which are shaded with a reason hint. Isolated receipt observations
remain visible as points. When the in-memory store started after session open,
the chart shows a history-start note instead of implying earlier coverage.
The event stream also carries bounded `price_batch` frames for recently observed
instruments. A frame is `{v, batch_seq, as_of_ms, session, full, items,
price_kind, source, note, execution_authority:false}` with rows
`{instrument_key, last_price, receipt_ms}`. The server builds one frame per
`dashboard.price_batch_interval_ms` and shares it across streams. A new stream,
or one that missed a batch, receives `full:true`; otherwise a frame carries only
instruments whose receipt moved, and no frame is sent when nothing moved. The
client merges rows per instrument. `batch_seq` advances only when a receipt
moved, so a quiet interval is not a gap. Frames omit the `id:` line so the
event cursor is independent. A reconnect or sequence reset is not a market gap;
a real batch jump is drawn as a discontinuity, and a local gap marker is
discarded once an authoritative refetch covers it.

Authenticated series responses are cached in a bounded, tab-local IndexedDB
store (24 records per tab, each bounded to 2,048 display points). Cached
financial points are shown immediately only with an explicit stale marker while
the authoritative read runs; a successful read replaces them. The cache is
cleared on lock and whenever broker/account/session identity changes. If the
authoritative read fails, cached points remain visibly stale; they are never
presented as fresh. Hidden tabs defer batch/chart work and reconcile against the
series watermark when visible. Tab-local snapshot observations remain a fallback
when no durable series is available; they are still labelled as this-tab
evidence. The client never reconstructs candles, volume or prices from sampled
returns.

### Authentication and refresh

Every API read requires the installation's bearer token. It stays in this tab's
session storage. Lock aborts the stream and clears account/session data, chart
points and API caches; request generations prevent late responses repopulating
them even if the same token is later reused. Failed reads remove financial
state instead of treating old values as current. Unknown marks, fees or capital
remain unknown; a flat-book net total is withheld while holdings remain.

One SSE fetch (or a held long-poll fallback) carries `summary_changed` and
`price_batch` frames. SSE transport errors fall back to held polling. A normal
hold-boundary reconnect keeps the connection live; only transport errors mark it
stale.

The summary carries `revisions`, a map of projection name to a 16-hex content
hash for `portfolio`, `fills`, `plans`, `scanner`, `sessions`, `health`,
`alerts` and `agent`. Each hash covers the view fields that projection reads,
excluding per-loop clocks (`as_of_ms`, the controller heartbeat and liveness,
`captured_at_ms` and alert `observed_at_ms`). `agent` is derived from the
length, inode and mtime of the lane journals and `usage.json`. The controller
computes them once per loop and publishes `summary_changed` with
`{etag, revisions}` whenever the summary ETag changes. The summary still changes
every loop because it carries `as_of_ms`, so the saving is on the client: it
refetches only the projections whose revision differs from the one it last
read. Without `revisions` (an older backend) it refetches every projection.
Explicit Refresh, a stream gap and document visibility refetch everything.
Page-local reads (health, alerts, tools, lanes) reload when their revision
changes; no status interval is scheduled.

A failed projection clears only its own field, is reported as unavailable with
the source reason, and never reuses a prior financial value. The clock timer is
display only. Source checkpoint, browser receipt and stream connection remain
separate facts. A snapshot does not establish worker liveness, quote freshness,
an ACK or a fill.

### v1 JSON API

All routes require `Authorization: Bearer <dashboard token>`. Query strings are
limited to 2 KiB, strict UTF-8 percent decoding, no duplicate keys and no
unknown keys; errors are `{"v":1,"error":"<code>"}`.
Failed authentication attempts are rate-limited via a token bucket (`dashboard.failed_auth_per_minute`,
default 30, range 1–600). When exhausted, requests return HTTP 429 Too Many Requests with
`Retry-After` set to the rounded-up time until the next failed-auth attempt is accepted. This
header is present on JSON, long-poll and SSE authentication responses.

| Route | Purpose |
|---|---|
| `GET /api/v1/meta` | Version, explicit paper/live mode, account alias, broker roles and dashboard limits. ETag revalidation. |
| `GET /api/v1/summary` | Compact session phase, mode/identities, readiness, equity/cash, counts, feed and alert summary, plus per-projection `revisions`. Precomputed once per trading loop. |
| `GET /api/v1/portfolio` | Cash, capital/policy, valuation marks, exact position records, fees and book state. |
| `GET /api/v1/positions?cursor=&limit=` | Dedicated position records projected from the current portfolio with total count and next cursor. |
| `GET /api/v1/orders?date=&cursor=&limit=` | Planner validation verdict rows under `items` with total count and next cursor; each row contains a nested validated `action` or `null` when rejected. Separate execution records are included, and verdicts are never fills. Restricted to the currently loaded session. |
| `GET /api/v1/fills?cursor=&limit=` | Newest-first immutable simulated fills with `total` and `next`; ACKs are never exposed as fills. |
| `GET /api/v1/plans?date=` | Recorded planner `actions` and host `execution` verdicts for the loaded session only, plus the `planner` status without those two arrays. |
| `GET /api/v1/scanner?segment=prospects\|missing\|screens&cursor=&limit=` | Bounded scanner rows, coverage and explicit missing/unseen reasons. |
| `GET /api/v1/instruments?cursor=&limit=` | Read-only validated NSE cash EQ catalog with `total`/`next`, validated session, source-file hashes and an explicit unavailable envelope before preparation; no derivative, option-chain or margin data. |
| `GET /api/v1/sessions?cursor=&limit=` | Bounded current/archive session checkpoints and provenance. |
| `GET /api/v1/tools?date=&cursor=&limit=` | Bounded pi lane web-tool call record: original query or URL, provider, latency, bytes, result count and outcome. Missing records are an empty list; a corrupt complete row is unavailable. Read-only, no provider call. |
| `GET /api/v1/health` | Readiness, feed/history, book safeguards, retention, planner budgets, read-only `activity_attribution` when present, derived alerts and the bounded `updater` status (last preflight failure reason/version/since, or null). |
| `GET /api/v1/alerts?cursor=&limit=` | Stable derived alert rows with original evidence and `next`. |
| `GET /api/v1/series?kind=receipt-minute-bar&instrument=&from=&to=&limit=` | Bounded receipt-derived minute bars with classified coverage gaps (`no_trade`/`unobserved`), store/session-start clocks and `watermark_ms`. |
| `GET /api/v1/series?kind=account-equity&from=&to=&limit=` | Bounded recorded portfolio checkpoints with cash, equity and mark basis. |
| `GET /api/v1/agent?cursor=&limit=` | Newest-first pi lane turns with tool calls, plan stance, gate verdict, usage and fallback; trigger counts; per-lane usage totals. Served from a process-local journal cache that parses only appended bytes. When a journal exceeds the read window it returns the newest rows with `truncated:true` and `rows_not_loaded:{turns,triggers}`. ETag revalidation. |
| `GET /api/v1/events` | SSE `summary_changed` / `price_batch` frames with heartbeats; `?poll=1&after=N` is the held long-poll fallback. |

Meta and the lane journal revalidate with ETags. Private responses vary by
`Authorization`. Snapshot and financial projections stay `no-store`/`private,
no-cache`. `price_batch` frames carry their own `batch_seq` and no `id:` line;
`Last-Event-ID` continues to address only the event ring. The older
`/api/model-*`, `/api/backtest`, `/legacy/` and `/assets/dashboard.*` paths
and the removed `/api/v1/runs*`, `/prompts`, `/session` and `/approvals` routes
return 404. The workspace reads the split v1 projections. Offline replay is
`GET /api/v1/backtest/example` and `POST /api/v1/backtest`; it still refuses
while a session can trade.

### Settings and retention

Defaults are omitted from serialized configuration to preserve sealed
configuration hashes. Validation ranges are enforced on load; changing any value
requires a reviewed configuration change.

| Key | Default | Effect |
|---|---:|---|
| `dashboard.workers` | 4 | HTTP worker threads; 1–16. |
| `dashboard.queue` | 16 | Accepted-but-unserved requests; 1–256; full queue returns 503 `busy` with `Retry-After: 1`. |
| `dashboard.static_max_age_s` | 86400 | Retired/ignored compatibility key; the shell and its assets always revalidate with ETags. Validation at most 31536000. |
| `dashboard.gzip_min_bytes` | 1024 | Gzip threshold for assets and JSON; at most 1048576. |
| `dashboard.history_read_max_bytes` | 8000000 | Maximum per-file history read; 1000000–64000000. |
| `dashboard.history_list_cache` | true | Retired, ignored compatibility key; preserves existing config serialization. |
| `dashboard.history_max_files` | 256 | Response files per legacy run; 1–256. |
| `dashboard.backtest_timeout_ms` | 3000 | Offline backtest compute deadline; 1–3000. |
| `dashboard.model_feed_unavailable_grace_ms` | 5000 | Retired, ignored compatibility key; original 0–60000 validation remains. |
| `dashboard.sse_max_streams` | 3 | Concurrent SSE readers; 0–16; beyond the cap clients fall back to long-poll. |
| `dashboard.sse_heartbeat_ms` | 15000 | SSE comment heartbeat; 1000–60000. |
| `dashboard.sse_write_timeout_ms` | 30000 | Per-write SSE timeout; 1000–60000. |
| `dashboard.longpoll_hold_ms` | 25000 | Long-poll hold; 1000–60000. |
| `dashboard.transcript_max_bytes` | 2097152 | No reader since the transcript viewer was removed; still validated and reported in meta `limits` for configuration compatibility. Range 65536–16777216. |
| `dashboard.transcript_text_max_bytes` | 16384 | No reader since the transcript viewer was removed; still validated and reported in meta `limits` for configuration compatibility. Range 1024–262144. |
| `dashboard.transcript_args_max_bytes` | 4096 | No reader since the transcript viewer was removed; still validated and reported in meta `limits` for configuration compatibility. Range 256–65536. |
| `dashboard.blob_slice_max_bytes` | 262144 | No reader since the transcript viewer was removed; still validated and reported in meta `limits` for configuration compatibility. Range 4096–4194304. |
| `dashboard.series_max_points` | 2000 | Maximum points in one series response and the default page; 64–10000. |
| `dashboard.series_max_range_ms` | 86400000 | Maximum explicit `from`–`to` span; 60000–604800000. |
| `dashboard.equity_max_points` | 4096 | Bounded account-equity observation ring; 64–65536. |
| `dashboard.equity_sample_interval_ms` | 15000 | Minimum spacing between recorded equity checkpoints; 1000–300000. |
| `dashboard.price_batch_instruments` | 24 | Instruments in a full `price_batch` frame; 1–256. |
| `dashboard.price_batch_interval_ms` | 2000 | Display batch cadence; 500–60000. Execution and telemetry keep original precision. |
| `dashboard.retention.pack_after_days` | off | Pack eligible raw model archives after this age; 1–365. |
| `dashboard.retention.delete_after_days` | off | Delete expired raw archives and packed copies; 1–365 and greater than the pack age. |
| `dashboard.failed_auth_per_minute` | 30 | Token bucket capacity for failed auth attempts per minute; 1–600; returns 429 when exhausted. |
| `dashboard.model_prices.*` | none | Exact decimal INR per million input/output/cached tokens for cost estimates. |

Opt-in retention keeps `model/transcripts/<run>.ndjson`, `model/runs.ndjson`,
prompts and the live stream while raw run directories and blob artifacts are
packed or deleted. The workspace no longer renders these archives. See
[retention operations](OPERATIONS.md#model-archive-retention) for eligibility,
conflict handling and dry-run/apply commands.

## pi lanes view

`#/lanes` reads `GET /api/v1/agent` and reloads it when `revisions.agent`
changes. It shows:

- **Lane cards** for decision, gate, research and review, then any other lane:
  turns, failed turns, fallbacks, p50/p95 latency, tokens in/out, cache reads,
  cost, provider calls, the configured model, calls and spend against the daily
  budget (from `readiness.models.roles` in `/api/v1/health`), readiness status
  and the last turn. Usage totals come from the API's `usage.lanes`; latency and
  failure figures come from the loaded turns and say so.
- **Timeline**: one row per lane over the session. Each turn is a bar whose
  height is log latency; a failed turn is a hollow outline and a gate
  escalation adds a diamond. Hover shows the turn; click opens it.
- **Gate funnel**: gate turns, escalations, decision turns, validated plans,
  actions and fills, with only recorded counts.
- **Latency distribution** per lane and **cumulative cost by lane**.
- **Turn table** with model and fallback, trigger and instruments, the tool
  chain (failed tools marked with their error), outcome, latency, tokens, cost
  and status. A row opens a drawer with the full recorded turn.
- **Web tool activity**: recorded `web_search`, `web_fetch` and `news_lookup`
  calls with their original query or URL, latency, bytes and outcome.

Turns, queries and URLs are untrusted model text rendered as text. The view is
display evidence only and carries no order authority. A truncated journal is
labelled as showing the newest rows, never as a complete total.

## Offline backtest lab

Supply an explicit manifest and its exact quote NDJSON. The shared engine checks
engine/data hashes, chronology, identities, fees, slippage and residual exposure.
No candle-to-quote conversion or invented closing fill. The optional synthetic
example is labelled. Limits: 512 KiB manifest, 4 MiB events, 500 symbols, 5,000 events,
50,000 observations, three-second HTTP compute deadline (five seconds for CLI).

Stateless requests accept content, never host paths; they change no trading state.
The paper-worker view refuses compute; only the research-only dashboard admits it. Host/origin checks still apply; this is not resource qualification.
At most ten reports are stored in the browser without silent eviction; export
preserves a convenience copy, not an authoritative experiment record.

Use `nivra backtest-fingerprint` / `nivra backtest --manifest FILE --events FILE`
for matching CLI replay. [Shared replay](reference/SHARED_REPLAY.md) explains formats.

Static assets have SHA-256 ETags and configurable cache lifetimes; the HTML shell
revalidates with `no-cache`. Gzip is negotiated for assets and JSON; API responses
retain `no-store`. No font download is needed.

Connections use a bounded `dashboard.workers` pool and `dashboard.queue`.
When the queue is full, the server returns 503 `busy` with `Retry-After: 1`.
A slow request occupies one worker rather than the accept loop.

`GET /api/v1/meta` is an authenticated, versioned projection of the binary version,
explicit paper/live status, account alias, broker roles, dashboard limits and the
capability names; every advertised capability has a route. It uses ETags and
`Cache-Control: private, no-cache`. V1 queries have a fixed 2 KiB wire bound,
strict UTF-8 percent decoding, and reject duplicate/unknown keys. V1 errors
include `v:1`; unknown v1 paths return 404.

The pi lane roster and per-lane budgets come from `readiness.models.roles`.

## Instrument metadata and F&O contract chains

The scanner's Instrument catalog reads captured BOD metadata separately from the
verified cash execution catalog. Filters use recorded segment, class, symbol/key
and underlying; counts describe the captured metadata, not fresh quote coverage.
Selecting a derivative opens a read-only call/strike/put chain and matching
futures for the recorded underlying key, expiry milliseconds and segment.
Missing legs remain unknown; colliding contracts leave an ambiguous leg rather
than selecting one. Strike and lot values retain their source precision.

`GET /api/v1/instruments` retains its original verified-cash default. Add
`scope=all` for display metadata, with `market_segment`, `kind`, `q`, `cursor`
and `limit` filters. `GET /api/v1/instruments/chain` requires `underlying_key` and
`expiry_ms`; optional `market_segment`, `cursor` and `limit` bound the chain.
Both routes retain the authenticated paper envelope and no execution authority.
The summary's `instrument_display` status/hash triggers one UI refresh when the
background catalog finishes; metadata parsing cannot block the paper loop.

An absent `dashboard.instrument_catalog` uses the original hash-bound NSE BOD
archive and its retained receipt time. Configuring `source:"complete"` enables a
separate public complete-market BOD capture. Its original bytes/hash/receipt stay
in the session directory; incomplete or changed captures refuse without overwrite
or source fallback. Source provenance and rejected-row counts stay visible.
The schema follows the [Upstox instrument reference](https://upstox.com/developer/api-documentation/instruments/).

Only a matching existing cash identity, symbol, unit lot and qualified tick can
carry the verified cash tick in INR; its separate source hashes remain inspectable.
Other tick values stay in raw provider units. `EQ` rows are not guessed to be ETFs
from names or ISIN prefixes; explicit ETF metadata is rendered when supplied.
BOD data supplies no current option quote, Greek, spot or margin: those remain
uncaptured, never zero. Existing recorded position margin fields still require
matching instrument identity and display-only provenance. The verified cash
allowlist and financial freshness/fee/protection gates are unchanged; derivatives
remain excluded from paper execution. Target-host resource/source qualification
and additional quote/margin evidence remain tracked in GitHub Issues.

The Strategies Library tab projects bounded original registered-research versions
and terminal outcomes from the current controller's dedicated experiment ledger.
It exposes hashes and provenance, not accepted strategies or switch qualification.
Reads never create, migrate or repair the ledger. Missing/corrupt/busy originals
stay unavailable; output is redacted and no promotion or trading control exists.
`research_tools.library_max_candidates` (default 32, maximum 256) and
`library_max_bytes` (default 262144, maximum 1048576) configure the read budget.
