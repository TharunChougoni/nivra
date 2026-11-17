# Dashboard

The React workspace is served at `/` as committed static assets. There is no
Node, CDN or provider call on page load. Legacy `/legacy/`, `/assets/dashboard.*`
and `/api/model-*` paths are not served; unknown non-v1 paths return 404. The
updater probes `/api/v1/health`; no legacy `/api/status` alias is served.
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

The React workspace is client-rendered with Vite, TypeScript, Tailwind CSS,
Radix UI, Lucide icons and uPlot. It is a presentation layer; the Rust runtime
owns trading. The operator builds pinned npm dependencies
locally; the generated static `bundle/` is committed and embedded in the Rust
binary. There is no Node service, SSR, CDN, remote font or runtime package
installation. The bundle is served from `/`; the former `/v2/` aliases return 404.
`/SOURCES.md` and `/LICENSES.txt` carry the vendored-source and licence manifest.

The eight sections are Overview, Models, Portfolio & fills, Sessions, Market
scanner, Strategies, Backtest lab and System health. A compact sidebar, persistent
source/status bar, session selector, theme/lock controls and `Ctrl/Cmd+K`
navigation serve the workspace. The header shows the current mode and the
installation's explicitly reported data and order broker identities; missing
order broker identity stays `Not reported`. These are display fields and do not
establish live readiness. On mobile the sidebar becomes a focus-managed
Radix dialog. Typography uses the system stack; surfaces are neutral charcoal
with restrained blue accents and no green palette. The original split nivra
wordmark and standalone arrow are retained.

- **Overview** combines account figures, a price/equity chart workspace,
  recent run links and a separate model/readiness rail.
- **Models** groups exactly two live identities: Digest and the
  Orchestrator.
  A **Web tool activity** panel lists the current session's recorded
  `web_search` / `web_fetch` / `news_lookup` calls with their original query or
  URL, latency, bytes and outcome; it is display evidence only and exposes no
  provider key or order authority.
  Search/date/trigger/outcome/failure filters use the existing archive API;
  the `identity` parameter (`digest` or `orchestrator`) groups supervisor
  and scorer runs under Orchestrator before paging, so `next` never skips older rows behind
  digest volume. Supervisor and scorer calls are archived through the harness
  with their trigger reason, transcript and parsed directives. Instrument/trigger text filters apply
  on Enter or blur. A selected run replaces the archive with its full transcript,
  retaining a back path. `#/runs/<id>` links still work.
- **Portfolio & fills** separates positions, fills and plans/validation using
  tabs. Instrument-class filters derive only from recorded `instrument.kind`
  values in the loaded projection (`index`, `cash`, `etf`, `future`, `call`,
  `put`). Recorded lot metadata supports whole-lot quantities; absent or invalid
  metadata stays unavailable. Position drawers show recorded segment, expiry,
  underlying, strike and margin only when supplied; margin must match the exact
  instrument key and be marked display-only. Scanner coverage explicitly labels
  its current NSE_EQ cash scope; this is not all-market live coverage. The separate
  instrument catalog exposes captured BOD segments and contract chains (see below). Selecting a position or fill opens recorded evidence and links
  only to matching plan actions/execution verdicts by `decision_id` and
  instrument; a missing plan or strategy reference stays explicitly unavailable.
- **Sessions** merges the bounded v1 session archive with the current
  summary/portfolio identity, preserving archived checkpoints and original fill
  order. The archive shows a mode only when its report supplied explicit
  `paper_only`/`live_enabled` provenance; older rows without those fields stay
  `Not reported`. The current row uses the authenticated snapshot's mode. The
  archive currently accepts paper-only reports, so it has no mixed live history.
  Inspecting a portfolio selects the original broker/account/date without
  changing a book.
- **Scanner** offers original-priority/symbol/absolute-change sorting and
  prospects, missing observations and screen totals from the v1 scanner
  segments. Selecting an instrument opens its chart, original clocks, recorded
  prospect strategy reference fields and raw evidence without losing the table.
  The same view includes the paginated read-only **Verified cash instrument
  catalog**, projected only from the validated NSE cash EQ session universe. It
  shows symbol, series, tick and source-file hashes; before preparation it stays
  explicitly unavailable. F&O, option chains and margin data are not exposed.
- **Strategies** shows strategy references projected by scanner prospects
  (spec ID, version, hash, scan ID and recorded score), plus research jobs and
  reviews carried by session rows. Strategy library snapshots, active selection
  and switch history are explicitly unavailable because the v1 scanner does not
  expose them; no library record is reconstructed from prospect references.
- **Backtest lab** takes an original manifest/events pair, uses the existing
  local bounded replay endpoint and shows actual replay equity points, fees,
  fills and hash provenance. Synthetic examples are labelled; results stay in
  component memory and can be exported as JSON.
- **Health** separates transport, three model identities, original role/task
  approvals, feed/history readiness and unconnected independent telemetry. It
  also shows the controller's read-only activity attribution when supplied:
  candidate conversion, abstentions, after-cost P&L, turnover/cost drag,
  drawdown, decision latency and strategy-source experiment attribution. Missing
  reports remain unavailable; the dashboard does not calculate or fill gaps.
  Net after-cost P&L subtracts fees; slippage and combined cost drag are shown as
  diagnostics because slippage is already reflected in fill prices. Drawdown
  coverage is explicitly incomplete because historical position marks are not
  retained.

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
`check:bundle`. Commit `src/`, `package.json`, `package-lock.json` and the
generated `bundle/` together; the bundle is embedded in the Rust binary. A
deterministic rebuild must reproduce identical source and bundle hashes. Do not
install Node globally on the paper host and do not add a runtime CDN, SSR or
package-install path.

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

uPlot is loaded only when at least two actual observations exist. Charts have
an IST crosshair/readout, 5m/15m/1h/all windows, drag-to-zoom and Pause/Follow.
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
instruments. Each frame has a stream-local `batch_seq`, session date, exact
receipt times and `execution_authority:false`; it intentionally omits the `id:`
line so the model-stream cursor is independent. A reconnect or sequence reset is
not a market gap; a real batch jump is drawn as a discontinuity. A local
sequence-gap marker is discarded once an authoritative refetch covers it.

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

One SSE fetch (or held long-poll fallback) supplies model activity. SSE transport
errors fall back to held polling. A normal hold-boundary reconnect keeps the
connection live; only transport errors mark it stale. A briefly contended
in-process ring snapshot is retried within the original hold window instead of
closing the stream. The controller refreshes seven authenticated
v1 projections together — summary, portfolio, fills, plans, scanner prospects,
sessions and approvals — on summary invalidation, gap, explicit Refresh or
document visibility; health and alerts load page-locally. No status interval is
scheduled. A failed projection clears only its own field, is reported as
unavailable with the source reason, and never reuses a prior financial value.
The clock timer is display only. Source checkpoint, browser receipt and stream
connection remain separate facts. A snapshot does not establish worker liveness,
quote freshness, an ACK or a fill.

### Transcript format

Each harness run has a private, create-once `model/<run>/transcript.ndjson`
(or `model/transcripts/<run>.ndjson` after retention). It is one JSON object per
line with `v:1`, `run`, a zero-based contiguous `seq`, `at_ms` and `kind`.
Kinds are `run_started`, `input`, `system_prompt`, `assistant_text`, `tool_call`,
`tool_result`, `provider_tool`, `final_output`, `host_note`, `truncated` and
`run_ended`. `run_ended` is written after the original terminal artifacts on
success, abstention, stop or failure.

The writer clips text and arguments using `dashboard.transcript_text_max_bytes`
and `dashboard.transcript_args_max_bytes`, keeps at most
`dashboard.transcript_max_bytes`, and emits one `truncated` marker before the
retained terminal event. Credential keys and credential-shaped strings are
redacted on write and again on read. A torn final line is ignored; a corrupt
complete line makes the transcript unavailable rather than inventing content.
Usage, cost, duration and validation fields that the archive did not record stay
unknown. The detail API replaces inline fields larger than 4 KiB with a `blob`
pointer, and the workspace fetches prompts, inputs, arguments and results lazily in
slices capped by `dashboard.blob_slice_max_bytes`.

The new transcript is a reading view with original task/model metadata,
unknown-safe usage/cost totals, lazy input/prompt/result folds, tool arguments
and terminal tool evidence, provider-tool lines, model output and host notes.
Model actions can be inspected as a table or raw JSON; intent is explicitly
separate from validation/fill records. Safe React text rendering never injects
archived HTML. The first 200 events render before Show next; full results load
in bounded server slices. Failed/interrupted/legacy evidence stays visible.

### Live updates

A running selected transcript appends only matching `archive_run` hints from the
shared stream and refetches authoritative events at the terminal event. Live
text/tool activity remains unvalidated display evidence. Gaps are explicit and
never invent a tool terminal state, accepted plan or fill.

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
| `GET /api/v1/summary` | Compact session phase, mode/identities, readiness, equity/cash, counts, feed and alert summary. Precomputed and cached on trading loop view updates. |
| `GET /api/v1/portfolio` | Cash, capital/policy, valuation marks, exact position records, fees and book state. |
| `GET /api/v1/positions?cursor=&limit=` | Dedicated position records projected from the current portfolio with total count and next cursor. |
| `GET /api/v1/orders?date=&cursor=&limit=` | Planner validation verdict rows with total count and next cursor; each row contains a nested validated `action` or `null` when rejected. Separate execution records are included, and verdicts are never fills. Restricted to the currently loaded session. |
| `GET /api/v1/fills?cursor=&limit=` | Newest-first immutable simulated fills with `total` and `next`; ACKs are never exposed as fills. |
| `GET /api/v1/plans?date=` | Recorded planner actions and host execution verdicts for the loaded session only. |
| `GET /api/v1/scanner?segment=prospects\|missing\|screens&cursor=&limit=` | Bounded scanner rows, coverage and explicit missing/unseen reasons. |
| `GET /api/v1/instruments?cursor=&limit=` | Read-only validated NSE cash EQ catalog with `total`/`next`, validated session, source-file hashes and an explicit unavailable envelope before preparation; no derivative, option-chain or margin data. |
| `GET /api/v1/sessions?cursor=&limit=` | Bounded current/archive session checkpoints and provenance. |
| `GET /api/v1/session?date=` | Read-only persistent-session turns, recordings, compaction/handoff status and recorded operator notes/aside requests. |
| `GET /api/v1/tools?date=&cursor=&limit=` | Bounded orchestrator web-tool call record: original query or URL, provider, latency, bytes, result count and outcome. Missing records are an empty list; a corrupt complete row is unavailable. Read-only, no provider call. |
| `GET /api/v1/health` | Readiness, feed/history, book safeguards, retention, planner budgets, read-only `activity_attribution` when present, derived alerts and the bounded `updater` status (last preflight failure reason/version/since, or null). |
| `GET /api/v1/approvals` | Separate Digest and Orchestrator identities with original trigger-task allowances. |
| `GET /api/v1/alerts?cursor=&limit=` | Stable derived alert rows with original evidence and `next`. |
| `GET /api/v1/series?kind=receipt-minute-bar&instrument=&from=&to=&limit=` | Bounded receipt-derived minute bars with classified coverage gaps (`no_trade`/`unobserved`), store/session-start clocks and `watermark_ms`. |
| `GET /api/v1/series?kind=account-equity&from=&to=&limit=` | Bounded recorded portfolio checkpoints with cash, equity and mark basis. |
| `GET /api/v1/runs` | Newest-day-first transcript index with `cursor`, `limit` (1–100, default 50), `role`, `identity` (`digest`, `decide` or `orchestrator`), `date`, `trigger`, `outcome`, `instrument` and `failed=1`. |
| `GET /api/v1/runs/{id}` | Transcript detail with `legacy`, `running` and `events`; large fields carry lazy `blob` pointers. |
| `GET /api/v1/runs/{id}/blobs/{name}?offset=&len=` | Bounded redacted byte slice with `X-Blob-Size`; only recorded pointers and numbered request/response artifacts are allowed. |
| `GET /api/v1/prompts/{sha}` | Content-addressed, redacted `text/plain` prompt after SHA-256 validation. |
| `GET /api/v1/events` | SSE `run_event` / `run_started` / `run_ended` / `summary_changed` / `price_batch` frames with heartbeats; `?poll=1&after=N` is the held long-poll fallback. |

Finished transcripts and prompts use private immutable caching; unfinished
transcripts, the list and meta revalidate with ETags. Private responses vary by
`Authorization`. Snapshot and financial projections stay `no-store`/`private,
no-cache`. `price_batch` frames carry their own `batch_seq` and no `id:` line;
`Last-Event-ID` continues to address only the model-event ring. Untouched legacy
paths (`/api/model-*`, `/api/backtest`, `/legacy/`, `/assets/dashboard.*`) stay
404. The workspace reads the split v1 projections. Offline replay is
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
| `dashboard.transcript_max_bytes` | 2097152 | Transcript file cap; 65536–16777216. |
| `dashboard.transcript_text_max_bytes` | 16384 | Per-text clip; 1024–262144. |
| `dashboard.transcript_args_max_bytes` | 4096 | Per-arguments clip; 256–65536. |
| `dashboard.blob_slice_max_bytes` | 262144 | Blob slice cap; 4096–4194304. |
| `dashboard.series_max_points` | 2000 | Maximum points in one series response and the default page; 64–10000. |
| `dashboard.series_max_range_ms` | 86400000 | Maximum explicit `from`–`to` span; 60000–604800000. |
| `dashboard.equity_max_points` | 4096 | Bounded account-equity observation ring; 64–65536. |
| `dashboard.equity_sample_interval_ms` | 15000 | Minimum spacing between recorded equity checkpoints; 1000–300000. |
| `dashboard.price_batch_instruments` | 24 | Instruments per `price_batch` frame; 1–256. |
| `dashboard.price_batch_interval_ms` | 2000 | Display batch cadence; 500–60000. Execution and telemetry keep original precision. |
| `dashboard.retention.pack_after_days` | off | Pack eligible raw model archives after this age; 1–365. |
| `dashboard.retention.delete_after_days` | off | Delete expired raw archives and packed copies; 1–365 and greater than the pack age. |
| `dashboard.failed_auth_per_minute` | 30 | Token bucket capacity for failed auth attempts per minute; 1–600; returns 429 when exhausted. |
| `dashboard.model_prices.*` | none | Exact decimal INR per million input/output/cached tokens for cost estimates. |

Opt-in retention keeps `model/transcripts/<run>.ndjson`, `model/runs.ndjson`,
prompts and the live stream while raw run directories and blob artifacts are
packed or deleted. The workspace continues to list and render retained
transcripts; blobs that no longer exist return 404 and the transcript stays
readable. See
[retention operations](OPERATIONS.md#model-archive-retention) for eligibility,
conflict handling and dry-run/apply commands.

## Orchestrator run view

The Models view groups the live Digest and Orchestrator identities. Archived
Decide runs retain their original archive label and are never merged into owner
usage. Operations, strategy and scorer names describe original trigger tasks,
not additional live decision peers. Credentials and allowances stay independent.

The left column lists runs. Each entry shows a trigger badge, start time in IST,
duration in milliseconds, outcome and total tokens. The list filters by trigger
and outcome, and older pages load on demand. Missing fields are shown as not
recorded rather than guessed.

The Models section reads `GET /api/v1/runs` and
`GET /api/v1/runs/{id}`. Activity arrives through `/api/v1/events` with a held
long-poll fallback; there is no 500 ms model polling. Hidden pages defer
refreshes. A timeout means service state is unknown, not stopped.

The right column shows the selected run as a conversation:

- **Trigger header**: the trigger name (which selects the prompt template), the
  first 12 characters of its SHA-256 prompt hash, and the model id.
- **System messages**: centred, muted and width-limited. They cover run start,
  run end, failures and other host events such as budget or tool status.
- **Assistant text**: left-aligned archived response output. A retained final
  response is not proof of host acceptance; inspect the failure, plan-validation
  and execution evidence. Internal reasoning is never displayed.
- **Tool-call cards**: collapsed by default. The header shows the tool name and a
  status pill: Running while the feed awaits a result, OK, Refused or Failed.
  Missing, unknown or legacy statuses show Unknown rather than running or
  success. Expanding shows the arguments and result as pretty JSON; results
  longer than 2,000 characters have a "Show more" control. Failed cards use a red
  border and refusals an amber border.
- **Footer**: tokens in/out, cost estimate and final outcome from the run audit.

Live activity uses `/api/v1/events` and shows gaps when the bounded feed drops
events. Archive IDs from `/api/v1/runs` resolve through `/api/v1/runs/{id}` to
the same archive. Exact lookup refuses symlinked run directories and ambiguous
IDs; missing evidence stays unavailable. Locking clears the list and view, and
late responses cannot restore private data.

Digest and Orchestrator keep separate approvals. The health view lists
each live role's own status and expiry when supplied; absent role or approval evidence
is unknown. Audit capture and the configured post-close scorer caller exist;
target-host qualification remains in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues). Token/cost/tool
fields missing from an original run are not evidence of zero usage or successful
execution. Remaining producer, contract and browser acceptance work is tracked in
GitHub Issues.

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
for matching CLI replay, or [full-day paper replay](PAPER_REPLAY.md) for the
scanner/Luna/Grok loop. [Shared replay](reference/SHARED_REPLAY.md) explains formats.

Static assets have SHA-256 ETags and configurable cache lifetimes; the HTML shell
revalidates with `no-cache`. Gzip is negotiated for assets and JSON; API responses
retain `no-store`. No font download is needed.

Connections use a bounded `dashboard.workers` pool and `dashboard.queue`.
When the queue is full, the server returns 503 `busy` with `Retry-After: 1`.
A slow request occupies one worker rather than the accept loop.

History summary lists read run metadata, terminal outputs and bounded responses,
without tool-result or account-input reads. Legacy metadata absent from these
files stays unknown in the list; selecting the run retains the detailed legacy
lookup. `dashboard.history_list_cache` is accepted only for configuration
compatibility. Per-file history reads
use `dashboard.history_read_max_bytes` (8 MB by default).

Live stream events include the originating harness directory as `archive_run`
(or null outside a harness), and tool events include `tool` plus measured
completion time or a redacted failure reason. Readers copy shared event pointers
under the ring lock and construct JSON after releasing it; the dropped counter
counts bounded-ring evictions. These events retain `execution_authority:false`.

The transcript module provides private, create-once `transcript.ndjson` files
with versioned sequential events and injected observation clocks. It redacts
credential keys and credential-shaped text both when writing and reading, clips
text/arguments using dashboard settings, and emits one truncation marker while
retaining the terminal event. Readers ignore a torn final line and refuse corrupt
complete lines. Harness runs now capture these events alongside the original artifacts. Capture
failures disable the writer with one fixed diagnostic and preserve the model
result. Usage remains unknown when a provider omits it; accepted capture outcomes
describe harness completion, not downstream order validation or fills.

Each provider response records its prose, tool calls and usage before local tool
execution, followed by tool results with measured durations and blob references.
`run_ended` is emitted after original terminal artifacts on success, abstention,
stop or failure. System-prompt events carry hashes, lengths and write-once paths under the
session's `model/prompts/` directory; identical instructions share one private
file. Tools-disabled final instructions have their own hash/file. Existing
conflicting files are preserved and marked unavailable; prompt-store failure
does not change the model result. Raw inputs/results remain lazy references to
their original files.

Finished harness runs append one private, redacted row (at most 2 KiB) to
`model/runs.ndjson` in completion order, with usage, outcome, tool counts,
mentioned instrument keys and archive size. Legacy `run-audit.json` files remain
readable; no new builder writes them. Configured model prices produce
exact decimal INR estimates using the plan formula (input plus output plus an
optional cached-token component, each per million tokens). Missing prices or
usage, invalid/negative rates and decimal overflow leave cost/basis unknown;
these estimates do not change trading fees, cash or model allowances.

Request archives retain the full first frame and store appended input items on
subsequent turns in `turn-NN-request-delta.json`. Unchanged tools are referenced
by hash; changed final-answer tools are stored. Reconstruction verifies exact
serialized provider-frame, tool and instruction hashes and refuses tampering.
Legacy full-turn request files remain readable; list/detail views recognize
both layouts. Provider request bytes and stream identifiers are unchanged.

Opt-in retention preserves per-run transcripts under `model/transcripts/`, daily
indexes and prompts after packing/deleting expired raw model archives. The status
`retention` projection reports its last post-close result. See
[retention operations](OPERATIONS.md#model-archive-retention) for CLI previews,
eligibility and conflict handling.

`GET /api/v1/meta` is an authenticated, versioned projection of the binary version,
explicit paper/live status, account alias, broker roles and dashboard limits. It
uses ETags and `Cache-Control: private, no-cache`. V1 queries have a fixed 2 KiB
wire bound, strict UTF-8 percent decoding, and reject duplicate/unknown keys.
V1 errors include `v:1`; unknown v1 paths return 404. Legacy endpoints and assets
retain their existing routing/authentication behavior.

`GET /api/v1/runs` lists daily indexes newest day first with opaque `cursor`,
`limit` (1–100; default 50), `role`, `date`, `trigger`, `outcome`, `instrument`
and `failed=1` filters. It returns `v`, `runs` and `next`, supports private ETag
revalidation, and places unfinished transcripts first. Indexed finished runs need
no reads inside their archive directories. Runs without index rows use their
transcript or a lightweight legacy summary marked `legacy:true`; unavailable
legacy usage, instruments, costs and tool-failure counts stay unknown. A terminal
transcript whose index write failed keeps its recorded outcome rather than being
mislabelled running. Corrupt indexes are unavailable; torn final appends are
ignored until complete.

`GET /api/v1/runs/{id}` returns a transcript with explicit legacy/running flags.
Fields over 4 KiB carry lazy blob references. Finished transcripts and prompts
use private immutable caching; unfinished transcripts revalidate. Private
responses vary by Authorization, and every v1 route remains authenticated.
Retained `model/transcripts/<id>.ndjson` files remain readable after retention.

`GET /api/v1/runs/{id}/blobs/{name}?offset=&len=` serves a bounded byte slice of
the redacted JSON representation with `X-Blob-Size`. Allowed names come from
recorded event pointers, terminal artifacts and numbered response/rebuilt request
files. Encode JSON-pointer suffixes in the path (for example
`turn-00-request.json%23%2Frequest%2Finput`). Inputs and results keep non-secret
fields; credential keys are removed even inside encoded JSON strings. Traversal,
symlinks, linked blob files and unknown blobs are refused. Raw blobs absent after
retention return 404; the retained transcript remains available.

`GET /api/v1/prompts/{sha}` accepts a lowercase 64-digit SHA-256, verifies the
stored bytes and serves redacted plain text. Missing or conflicting artifacts
remain unavailable. Legacy adapters retain unknown durations, usage and tool
statuses; intermediate prose alone never establishes terminal completion.

The live model roster and approval identity list contain Digest and Orchestrator
only.

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
