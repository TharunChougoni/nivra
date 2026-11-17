# Historical candles

`history-download` reads Upstox v3 candles using the application's config/token,
validated frozen universe and **original shared read journal**. It never starts
paper trading or sends orders. Historical membership is not inferred from today's
universe. Unfinished work is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

```sh
nivra history-download --config PAPER_CONFIG --output HISTORY_DIR \
  --start 2025-10-17 --end 2026-10-16 --interval days
```

For operational catch-up without generating descriptive replay shards, add
`--receipts-only`. This validates/reuses original receipts and appends missing
provider observations plus a separately identified `artifact_mode:receipts_only`
index. It does not rebuild old manifests/events/candle shards. A conflicting
derived artifact remains preserved and cannot block this raw acquisition mode;
a corrupt raw receipt still refuses. Ordinary downloads retain their existing
replay-shard export behavior.

For the running config's exact required daily sessions, use
`nivra history-backfill --config PAPER_CONFIG`. This explicit manual command
uses receipts-only acquisition, the frozen validated universe, reviewed calendar
and configured history root. It coalesces missing trading-session spans instead
of spending requests on weekend-only holes. It preserves the same shared quota,
reserved read headroom and original receipts; it neither starts a session nor
changes the automatic worker's pre-open window/deadline. Inspect coverage after
completion: newly listed/unavailable instruments or empty responses may still
lack the required bars.

Intervals: `minutes`, `days`, `both` (default). Omitted dates select yesterday in
IST and the preceding twelve calendar months; use explicit inclusive dates for
reproducibility. The runtime resolves the current dated book even if config still
names a predecessor. Use a separate private output outside Git.

## Receipts and reruns

Per-range receipts retain raw successful body/request/receipt times and range
identity. Per returned candle date, manifest/events/candle shards retain bindings;
a plan index lists outputs/limitations. Daily candles intentionally have per-date
shards too. Same-directory reruns reuse validated receipts across interval choices
and request only uncovered spans. Changing layout creates a new index; originals
and failed/staging artifacts remain. Corruption/conflicting engine bindings refuse.
Do not overwrite receipts or reset journal quota to force a rerun. Older descriptive shard manifests retain their engine fingerprints; they are
not admitted to a newer engine without an explicit derived import.

[Upstox v3](https://upstox.com/developer/api-documentation/v3/get-historical-candle-data/)
was the implementation reference for one-minute/one-day intervals and request
windows (one month / ten years), with historical availability from January 2022 /
January 2000 respectively. These are recorded adapter assumptions, not a fresh
provider guarantee. Original shared-journal limits remain stricter than the dated
[provider rate documentation](https://upstox.com/developer/api-documentation/rate-limiting/).
Failures consume reservations; bounded quota waits do not reset them.

The controller's opt-in pre-open preparation acquires missing prior-session daily
spans in receipts-only mode and publishes READY/PARTIAL/BLOCKED coverage. Its
coverage pass reads the complete receipt inventory, but it never scans an
unbounded corpus: receipts that predate the required window are skipped by their
write-once mtime before being decoded (a provider row can never postdate the
receipt that carries it, with a two-day clock/timezone margin), and explicit
entry/decode bounds fail closed with `history_receipt_inventory_limit` or
`history_receipt_decode_limit`. Skipping a receipt only understates coverage for
that pass; it never substitutes, edits or deletes evidence. Reaching either bound
is an operator-review condition, not a reason to prune or overwrite receipts. Its
cutoff/deadline, cancellation and original quota remain unchanged; a manual
catch-up is separate from pre-open admission. Preparation skips uncovered
ranges with no session in the reviewed NSE calendar and coalesces split daily gaps
per instrument into one first-missing through last-missing session range (subject
to provider window bounds). It can overlap validated receipts; originals remain
immutable. Dates outside the reviewed calendar fail closed. The explicit historical
`history-download` command retains its calendar-day range behavior for older data.
Neither empty provider responses nor successful requests certify bar completeness;
shared read quotas and the pre-open deadline still apply. Twenty-one reviewed session bars
are needed per eligible feature row. `history_skip_unavailable` explicitly excludes
specific unavailable ranges; missing values are never fabricated. See
[operations](OPERATIONS.md#event-and-history-behaviour).

## What candle evidence supports

Daily/minute OHLCV is not bid/ask/depth/circuit or original two-second executable
quote evidence. Candle-only backtests cannot establish exact intrabar fills or
corporate-action/calendar completeness. Preserve engine fingerprints; never edit
old manifests to pretend compatibility.

For bounded descriptive research, the registered runtime can prepare immutable
shards without touching originals:

```sh
nivra-analysis --prepare-daily-history SOURCE_HISTORY NEW_PRIVATE_DIRECTORY
```

It validates receipt hashes/times/duplicates and produces pinned daily datasets
(up to 200 instruments per shard), inventory and compact receipt index. Register
reviewed runtime/code/input hashes before exposing aliases to models. See
[worker](RESEARCH_WORKER.md).

`exit-backtest --config PAPER_CONFIG` requires the explicit paper-cost policy and exposes the legacy exploratory daily exit study: hypothetical
open entries, 2% stop/target, both-touch orderings, close proxy and shared simulation
costs with independent ₹10,000 allocations. Those assumptions are not current
model-led strategy policy. Missing spread/depth/intrabar order and contemporary
universe bias prevent treating this as a certified intraday strategy backtest.

The exit study accepts optional `--config <runtime-config.json>` and reads
`exit_backtest.allocation_inr`, `stop_multiplier` and `target_multiplier` (exact
decimal strings; defaults `"10000"`, `"0.98"`, `"1.02"`). Without `--config`,
these same defaults apply. This advisory setting is excluded from runtime
identity serialization; existing sealed and hashed formats are unchanged.

Historical requests leave `history_download.reserve_half_hour_reads` (default 300)
unused by history within the shared 1,500-read rolling 30-minute journal window.
Other consumers can use that headroom; it is not a guarantee of availability,
and the shared per-second and per-minute caps still apply. Denied historical
reservations wait under the existing bounded retry and deadline settings.

History also leaves `reserve_second_reads` (default 2, range 0–4) and
`reserve_minute_reads` (default 30, range 0–119) in the corresponding shared
windows. Ordinary operational reads retain the full existing 5/second,
120/minute and 1,500/half-hour caps. These reserves apply only to history
admission, are omitted from serialized configuration at their defaults, and
never erase reservations or guarantee availability against other readers.
