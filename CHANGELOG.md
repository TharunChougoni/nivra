# Source snapshot

## 0.16.0 — 2026-11-27

### Upgrade an installed paper host (breaking)

1. Install the release and deploy the rebuilt `nivra` binary and updated `agent/` runtime (`npm run build --prefix agent`).
2. **Owner override of sandbox guard:** `strategy_actions_v1` scripts now
   require a top-level `on_observation(obs)` entry point (adapter digest changed)
   and run on a persistent hardened Python worker (`python3 -I -S -B`, rlimits,
   Landlock, seccomp, no bubblewrap) instead of per-call `bwrap`. Remove old
   v1 entries under `<sessions_root>/strategy-library/entries/` (or re-backtest
   strategies so `entry_version: 2` entries with passing `reviews/` records are
   written).
3. Ensure `strategy_review` or `agent.lanes.gate` is configured in the planner
   so the review gate can approve new candidate strategies. Optionally tune
   `strategy_execution.call_deadline_ms` (default `250`).

### Changed

- **Persistent hardened strategy worker & `StrategyHost` pool.** Replaced per-call
  bubblewrap spawning for `strategy_actions_v1` with persistent `python3 -I -S -B`
  workers (`on_observation(obs)`), lockstep sequence-checked NDJSON framing,
  one-time OS hardening in `pre_exec` (closed fds `>= 3`, `setpgid`, memory/fd/fsize/core
  rlimits without lifetime `RLIMIT_CPU`, `PR_SET_NO_NEW_PRIVS`, Landlock, and
  seccomp), monotonic per-call deadlines, exponential backoff/quarantine, and
  a `StrategyHost` pool with non-blocking hot swap and in-transaction
  `activation_generation` + `choice::choose` verification.
- **Two-stage strategy review gate.** Every `strategy_actions_v1` script must
  pass a deterministic Python AST import/attribute check (`ast_check.py`) plus
  an ephemeral tool-less model security review (`strategy_review` / `gate` lane),
  bound to exact `source_sha256`, `runner_sha256` and `review_policy_sha256`
  digests (`entry_version: 2`).
- **Removed** `nivra_sandbox::run_document` (one-shot action-strategy bubblewrap helper).

## 0.15.1 — 2026-11-27

**Fixed:** a decision-lane strategy choice now counts only under the planner
policy it was made under. A stand-down recorded before fallbacks were
configured kept blocking them after `config-apply`. Each `lane_select` records
the policy digest; older records count under no policy. No upgrade step.

## 0.15.0 — 2026-11-27

A strategy runs from the open, and the gate screens F&O.

### Upgrade an installed paper host

1. Install the release (stop, `update --operator-stopped`, or the updater
   after the close). No book or data migration.
2. In the planner, set `strategy_execution.fallback` to the shipped action
   strategies you want as fallbacks, best first (for example
   `["bollinger_reversion"]`), run `nivra planner config-check`, then apply it
   with `config-apply` and keep a `*.pre-fallback` backup.
3. Keep `nifty_options.enabled: true` with the `NSE_FO` fee schedule in the
   paper config; without it the gate lane never runs.
4. Refresh the pi agent runtime on the run host so the new prompts and the
   `strategy-library` skill load.

### Added

- **09:00 strategy choice.** At `agent.triggers.decision.pre_open_minute`
  (default 540, 09:00 IST) the decision lane picks today's strategy: an
  accepted library version, a configured fallback, or none. When nothing
  accepted can run, research gets a `strategy_search` task that replays the
  shipped strategies over the last `recent_archive_sessions` (default 3)
  closed sessions and registers what passes acceptance.
- **Fallback strategies.** `strategy_execution.fallback` lists shipped action
  strategies to run while no accepted version can. A fallback is labelled
  `fallback:<name>` on actions, fills, refusals, briefs and the dashboard and
  goes through the same execution gate. An accepted version, an owner pin or a
  lane stand-down stops it.
- **F&O gate.** The gate lane wakes on `fo_signal` triggers (Nifty spot,
  near-money premium or open-interest moves on tradable contracts) and hands
  the decision lane up to five ranked option prospects, checked against the
  option universe, as `gate_prospects`. Thresholds are under
  `agent.triggers.fo`. Cash entry candidates go straight to the decision lane.
- Every build ships `strategies/`; lane workspaces get it read-only under
  `reference/strategies/`. `action-bench` replays action strategies over
  archived quote tapes (`strategies/BENCHMARK.md`).
- `AGENTS.md` rules: configuration follows the owner's intent; codebase
  structure and change rules.

### Changed

- `regime_changed` supervision turns fire whenever strategy execution is on.
- A replay on the current build replaces an acceptance made by an older build.

## 0.14.1 — 2026-11-27

**Paper fills must carry their fee policy and benchmark price.** Every fill
stores `costs` and `expected_price`, and reconciliation re-derives each fee
(#50). Before upgrading, check the stopped book: every fill in
`state.body.fills` must have both fields. If one does not, close the book flat
with the previous binary and start a fresh paper book with
`nivra setup --confirm-fresh-paper`. See
[configuration](packaging/rust/CONFIGURATION.md).

**Removed** virtual observation tape events, the unused `research_quotes`
paper tick, placeholder Health activity metrics, unused market and research
APIs and three Cargo dependencies. Quote staging no longer has a test-only
bypass.

**`nivra update` installs `nivra-analysis`.** The updater fetches the
published `nivra-analysis-x86_64-unknown-linux-musl` asset with its checksum,
checks it, and installs it as `nivra-analysis` beside `nivra` in the release
directory, listed in `SHA256SUMS`. Archive bar jobs on an updater-managed host
no longer fail with `archive_bars_worker_unavailable`. The controller now
looks only for `nivra-analysis`; the long asset name is no longer a fallback.
Releases installed by an older updater have no worker. Update to the next
release to get one. An older release can still be reactivated as a rollback.

## 0.14.0 — 2026-11-27

A cleanup release. Market data lives only in the TimescaleDB archive, the
research lane evaluates only strategies, and about 34,000 lines of retired
code are gone.

### Upgrade an installed paper host

1. Stop the book with the open-book sequence in
   [operations](docs/OPERATIONS.md#change-the-planner-policy) before you
   switch binaries.
2. Remove `history_directory`, `prior_config`, `keep_full_quote_bodies` and
   `max_quote_body_bytes` from the paper config. A config that still has them
   fails with `config_invalid`. Paper operation requires `market_archive`; run
   `nivra archive init --config PAPER_CONFIG` if it is new.
3. In the planner, remove `abi`, `replay`, `max_result_bytes`, `limits`,
   `helper_path`, `unit_open_files`, `execution` and `observation` from
   `research_tools.candidate_compute`, and any `archived_feature_snapshots`
   dataset. Add `candidate_compute.strategy` to keep the `backtest` tools (see
   [research](docs/RESEARCH.md#in-the-research-lane)). Check it with
   `nivra planner config-check`, then run `config-apply` and `resume`.
4. Check that the book stores `initial_allocation`:
   `sqlite3 PAPER_BOOK "SELECT json_extract(body,'$.initial_allocation') FROM state WHERE id=1"`.
   If it prints nothing, start a fresh paper book.
5. Replay registered library strategies with this build. Entries from older
   builds report `strategy_adapter_changed` until a new replay accepts them.
6. Optional: shrink closed books with
   `nivra archive import-book --rewrite --config PAPER_CONFIG BOOK_DIR`.
   Queued ranker jobs and experiment-lifecycle tables from older builds are no
   longer read; the [research guide](docs/RESEARCH.md#in-the-research-lane)
   lists the tables you can drop.

### Added

- `backtest` with an `archive_bars` dataset runs one strategy over years of
  daily or minute NSE bars as a research job, with separate training and test
  results. The archive-window replay is configurable up to 60 days
  (`max_archive_window_days`).
- A pinned, read-only Python research environment (numpy, pandas, scipy,
  statsmodels, scikit-learn) for sandboxed strategies: `nivra research-env
  build|verify` and the planner's `research_env` block.
- 15 bar strategies and 8 action strategies under `strategies/`, with
  `nivra-tools bar-bench` and a leaderboard on 13 months of NSE data in
  `strategies/BENCHMARK.md`. Most lose money after costs.
- The pi agent loads research skills from `agent/skills/` and the book
  workspace's `skills/`.
- Dashboard charts read past sessions and multi-day history from the archive.

### Fixed

- A paper buy that adds to a held position no longer drops its stop, target
  and time exit.
- A protective exit blocked by thin bid depth sets health to
  `held_exit_depth_insufficient` on every pass instead of failing silently
  (#49).
- The dashboard checks the bearer token before it reads a request body.
- Quote receipts are staged durably before the book writes their archive
  reference. Future-dated quote observations are excluded. Journal time
  bounds apply before the row limit. Download exclusions match coverage.
- Strategy replays count a tape date only when the strategy ran that day.
- A leftover `planned-restart.next` file after a crash no longer blocks the
  planned-restart notice.
- Dashboard: daily axes show IST dates, zoom resets when the instrument or
  range changes, series reads retry after an error, archived sessions no
  longer show today's realized P&L, and the response cache is bounded.

### Removed

- Ranker candidate backtests, prospective observation, cross-session
  continuation and the experiment lifecycle ledger, with the
  `candidate-supervise`, `experiment-lifecycle` and `scorer-compare` commands
  and the `cost_aware_momentum` ranker.
- The `orchestrator`, `strategy-scorer`, `protection-gap`, `service-review`,
  `loopback-tunnel-text`, `novel-evaluate`, `memory-check`,
  `protection-timings` and `protection-diagnostics` commands and their
  library code.
- Dashboard routes `/api/v1/positions`, `/orders` and `/tools`, and the
  observation, measurement and continuation panels.
- Offline quote compaction, the LTPC frame database, the original-install
  config chain and the single-instrument book format.

## 0.13.1 — 2026-11-26

Publishes the 0.13.0 changes below. The 0.13.0 tag failed the release-link
check and was never published.

## 0.13.0 — 2026-11-26

**Strategy acceptance now defaults to three tape dates.** Configure an
`archive_window` dataset and replay strategies across at least three
reviewed NSE sessions. Update all quote writers to this build before pinning
archive windows. Replay a new source revision or parameter set to register
an entry with this build's replay adapter hash. See [research](docs/RESEARCH.md#strategy-replay).

**Breaking history change.** TimescaleDB now stores daily and minute candles,
coverage, and receipts. Set `market_archive`, run
`nivra archive init --config PAPER_CONFIG`, then run
`nivra archive import-history --config PAPER_CONFIG HISTORY_DIR` against your
former history folder. Check the result with
`nivra history-backfill --config PAPER_CONFIG --check-only`. The import
preserves the source files. Paper configs that do not use history can still omit
`market_archive`.

Remove `--output` and `--receipts-only` from `history-download` and
`--history` from `exit-backtest`. Exit reports now count archived daily bars.
Use `nivra archive export-daily-history`
to build daily research datasets. See [operations](docs/OPERATIONS.md#move-an-existing-history-folder-to-timescaledb).

**Quote capture moves to TimescaleDB.** With `market_archive` configured, the
controller writes bulk REST quote bodies and normalized observations to the
archive, and the paper book keeps a reference row in their place. Executable
receipts that fills, protection and plan checks cite stay in the book. Run
`nivra archive init --config PAPER_CONFIG` before starting the controller, and
keep the archive configured when replaying books that contain references.
`nivra archive import-book --config PAPER_CONFIG BOOK_DIR` copies a closed
book's quote bodies into the archive without changing the book.

**Retention shrinks closed books after archiving.** With `market_archive`
configured, `nivra retention --apply` replaces a closed book's bulk quote
bodies with archive references once TimescaleDB holds a receipt with the same
SHA-256 and receipt time, then vacuums the book. Run `nivra archive import-book`
on older closed books first and check `kept_missing_archive_quotes` in the
`--dry-run` summary. See [operations](docs/OPERATIONS.md#shrink-closed-books-after-archiving-quotes).

**Dashboard history from the archive.** The instrument chart has a range
picker for past sessions and multi-day history. Minute charts cover up to 14
days and daily charts up to 366 days, read from TimescaleDB on request. Live
sessions still stream over SSE.

## 0.12.8 — 2026-11-26

A pre-open history pass no longer parks a session it cannot start (#27). A
`PARTIAL` pass with no ready instrument keeps its bounded attempt running
instead of ending the retry schedule, so the controller never sits out the day
waiting for a pass that will not come.

A missing day counts as an empty gap only when the provider answered past it, or
answered no bar at all for that instrument. Listing and suspension gaps still
stop retrying; the days after the last bar the provider returned stay fillable,
so a bar that is published late is never written off. `covered_empty_bars` and
`retryable_missing_bars` now mean what docs/MARKET_DATA.md says they mean.

The guides no longer describe a pending-order guard that paper never had.

## 0.12.7 — 2026-11-25

Nivra now follows an algorithms-first direction: models research and write
paper strategies, and code executes them. See docs/DIRECTION.md and #44. The
guides are rewritten, shorter and checked against the source.

- Nifty options can load. The option spot now comes from the index feed point
  for the underlying, with the original receipt as the two-second freshness
  clock (#38).
- The decision brief states the book's paper cost rates per segment, and the
  prompt warns that stops and targets inside the round-trip cost lose (#37).
- Every execution-gate refusal records an exact reason, such as a stale quote,
  a price above the cap or too little depth, in a new `planner_refusal` table.
  The dashboard shows it in plain words (#28).

## 0.12.6 — 2026-11-25

An idle dashboard tab uses far less bandwidth during market hours (#33).
Projection revisions hash content only, without per-loop clocks, ages, loop
counters or feed tick counters, so a page refetches a projection only when it
changed. `summary_changed` fires at most once a second when a revision moves,
and every 5 s otherwise so summary figures stay current; the scanner revision
moves at most every 15 s. `/api/v1/health` revalidates on content, so a repeat
read returns 304 while only ages differ. The event stream is gzipped when the
browser accepts it. The Lanes thread reloads only when a turn lands, through a
new `agent_turns` revision. The Health page ages quote and loop times locally
from the summary.

## 0.12.5 — 2026-11-25

The dashboard reads in plain language. Code names, internal field names,
`NSE_EQ|` keys and full hashes give way to short labels, symbols and
eight-character IDs with the full value on hover. Repeated disclaimers are
gone; the few safety facts an operator needs, such as "an acknowledgement is
not a fill", stay as one short sentence. Tables hide columns that are empty
for every row. Only the mode pill says Live; the stream indicator says
Streaming.

In the Lanes tab, tool calls show plain names and argument values, only
failed or unfinished calls carry a tag, and decision answers say how many
trades were proposed. The trigger message holds only the trigger, with its
time, reason and a brief icon below it.

## 0.12.4 — 2026-11-25

The Lanes tab shows each pi lane as a chat. Browser-style tabs switch between
the decision, gate, research and review lanes, and a side-by-side layout shows
two at once. Each trigger appears as a message with its time, reason and
instruments, and the full brief folds beneath it. The model's thinking and
each tool call are single lines that expand to the full arguments and result.
Answers carry the gate verdict, the validated plan or the turn error. Lane
stats sit below the thread, and the session charts fold into one section.

`GET /api/v1/agent/transcript` serves a lane's pi session transcripts
read-only, capped by the dashboard transcript limits. The Lanes tab no longer
reports a healthy journal as unreadable: it requested more turns than
`/api/v1/agent` allows.

## 0.12.3 — 2026-11-24

The dashboard explains its state. A banner at the top of each page says when
the controller is starting, the book is halted or stopped, the session has
closed, sources are missing, or the dashboard runs research only, and links
to the next useful section. A research-only Overview lists the sections that
work without a paper book instead of empty values. Empty, unavailable and
failed views have distinct boxes, and a strip with no recorded values shows
one empty state.

The unlock page names the failure: a rejected token, an unreachable dashboard
or a dashboard error. The workspace has a brand mark and favicon, page
metadata that keeps it out of search indexes, higher text contrast, and
entrance and data-arrival animations that respect reduced motion.

## 0.12.2 — 2026-11-24

The updater now leaves the paper controller stopped during a reviewed
`--operator-stopped` install. Normal updates accept authenticated health
responses up to 4 MiB so detailed history readiness can be checked.

## 0.12.1 — 2026-11-24

Paper history backfill can check exact candle coverage before startup and
prepare the dated universe for a new session. Checks for past sessions use
the original frozen universe.

The research lane can replay hash-pinned historical scanner snapshots in its
existing sandbox. A stopped controller's sealed paper book can export those
snapshots for an operator to register. Archived ranking results stay
retrospective. Current-session after-cost cash replay uses original quote
receipts.

## 0.12.0 — 2026-11-24

Pi now owns every model turn. The controller starts the pi agent subprocess,
builds bounded briefs, publishes each lane's tools and validates the returned
TradePlan before the unchanged two-second receipt gate. `planner.agent`
configures the process and the decision, research, review and optional gate
lanes: provider, model and thinking level, daily calls and spend, turn timeout,
tool cap, compaction size and a fallback that runs once after an authentication
or usage failure that produced no output. Fresh setup writes the pi paper
planner from reviewed agent settings. Preflight checks the node, entry, package
dependencies, prompts, an owner-only writable agent directory, known models and
stored credentials without reading them; release CI now requires the pi agent
build and tests before it publishes a tag. `agent-check`, `agent-turn` and
`agent-shadow-report` prove the loop offline with pi's scripted fake provider.
The legacy harness is removed: the model CLI and provider transports, the
supervisor and digest/orchestrator lanes, owner tasks, the usage journal, the
packaged prompt bundle, recorded-reply replay and `host-migrate`. Older journals
and archive projections stay readable.

Paper scanning keeps deterministic authority. Reviewed research rankers
register by exact workspace source SHA and bind to a paper trial; research
claims expire, answers publish atomically and the post-seal review reaches the
next brief. The optional gate lane screens batches made only of entry-candidate
triggers on a cheaper model, escalates only a verdict to the decision lane and
fails open; fills, position risk and scheduled turns skip it.

The dashboard is a rebuilt React/TypeScript workspace: a muted design system,
one status-bar shell, grouped navigation, a pi lanes view with turn drawer,
gate funnel and lane budgets, an order lifecycle, an instruments catalog,
synced equity and drawdown charts, a read-only readiness panel and operational
health v2. Projections publish their own revisions so a page refetches only
what changed, price frames arrive as delta batches and the lane journal is
served as a bounded cached tail. The retired Tailwind/light theme, run,
transcript and approval views are gone. The brand reads Nivra.

The controller keeps the instrument display catalog as serialized rows with a
typed index and streams the BOD gzip instead of materializing its JSON, removing
its two largest heap consumers. `nivra-tools check-release` refuses retired
harness vocabulary beyond a per-file allowance that may only shrink.

Upgrading from 0.11.x: the planner policy must be the pi lane shape, with the
agent package, its prompts and an application-owned agent directory under the
granted state root installed beside the configured node and entry; the retired
roles are refused. `nivra dashboard --model-config` is now `--config`. Release
assets no longer include `default-prompts.json`; prompts ship with the agent
package. Live orders remain disabled.

## 0.11.1 — 2026-11-18

Raise the local Upstox read caps from the retired shared-journal values
(5/s, 120/min, 1,500/30 min) to 90% of the published per-API, per-user limits
(45/s, 450/min, 1,800/30 min). The journal now serves only this application.
The half-hour cap bounds the daily prior-session history refresh, one request per
instrument, so the refresh is about 20% shorter and fits the 08:00–09:10 window
with more margin. Preflight now skips a `-`-prefixed optional `ReadWritePaths`
entry whose path is absent, as systemd does, so the packaged unit's optional
research staging path no longer fails every start on a host without it.

## 0.11.0 — 2026-11-18

Stop losing paper trading days to lifecycle edge cases. A stop signal is now a
typed shutdown kind decided once. Only an explicit `nivra stop --config
PAPER_CONFIG --confirm-paper-review` makes the next stop an operator stop. That
command writes a private request bound to the account and active session, and the
controller consumes it on SIGTERM, or it stops the book directly when no
controller is running. Every other stop is a service stop: `systemctl --user
restart`, the updater's stop, a host shutdown. A service stop cancels workers and
retires an in-flight planner attempt as failed
`service_shutdown_interrupted_no_replay` with its charges preserved and no sticky
block. It records the `service_shutdown` tick and leaves the book open, so the
same dated session continues on restart. A crash also continues the session as
before. Operator-stopped books still need a reviewed `resume`, and halted books
stay halted. Procedures that end in `resume` now start with `nivra stop`.

A controller that first comes up after 09:15 IST still starts the day's session.
An eligible predecessor seal rolls over until the 15:00 IST late-start cutoff
instead of the 09:15 gate, and a first-ever book configured for today starts the
same way. A session adopted after the history cutoff prepares history immediately with the
same quota journal, validation and bounded retries; a crash-interrupted attempt is
never replayed. Carried holdings are
protected as soon as the session is adopted. The readiness disposition and the
stale-feed anchor follow a late start.

The updater's health check now refuses an open book that comes back
operator-stopped, unless the update used `--operator-stopped`, and its guard
defers such a book. The forced `market_open_flat` restart, and every other forced
restart, now continues the same session as documented. The planned-restart
notice is written before the controller stops, so its service-stop alert is the
informational `planned_restart`; it is written only after every step that can
refuse the update. `resume` and a direct `nivra stop` archive a pending stop
request, so a later service restart cannot consume it. A service stop during pre-open preparation
records the cancelled attempt as retryable instead of leaving it interrupted.

Upgrading from 0.10.x: the installed updater still runs the old binary's stop,
which operator-stops an open book. Update outside market hours and never force the
0.10.x to 0.11.0 update while a session is open; otherwise `resume` the book after
the update.

## 0.10.4 — 2026-11-17

Fix #14. The orchestrator's `request_research` tool was declared strict but did
not require its `refs` property. Strict function schemas must require every
property, so the provider rejected every decision turn that advertised the tool
as an opaque HTTP 200 at response validation, while compaction and session-open
turns, which send no tools, were accepted. `refs` is now required and nullable,
and a null `refs` queues the question with no references. The strict-schema test
now covers every owner tool, including the research request and quote capture.

## 0.10.3 — 2026-11-17

Stop supervisor runs failing on grant contention. Luna digests and the persistent
orchestrator share one grant file and run concurrently, but the grant lock was
non-blocking and held for the whole provider call, so whichever role arrived
second failed at `grant_owner` with `grant_busy_or_unavailable`. Supervisor runs
died mid-way after already spending fallback turns. The lock now guards only the
grant read or refresh, waits within the role deadline, and is released before the
provider call. Contention still never switches to the fallback.

Failure records keep an allowlisted `local_cause` for pre-provider grant failures,
and an opaque HTTP 200 rejection keeps the provider's identifier-shaped error code
(`provider_error_code`; `primary_error_code` in fallback evidence) so #14 can be
diagnosed without provider text.

## 0.10.2 — 2026-11-17

Keep the dashboard, the reviewed fallback and the host migration honest after
the 2026-11-17 outage.

Dashboard: an archived transcript without a terminal event reports `interrupted`
instead of `Running` (only a current-session run inside its role's bounded
allowance stays running); controller startup renders a retrying loading state
instead of "Unavailable v1 evidence"; and the order authority is reported
explicitly as `paper` in paper mode, never implying a live broker.

Fallback: the reviewed explicit limit fallback now also covers entitlement/auth
failures at the catalog or inference stage and opaque HTTP 200
response-validation rejections, with stage-bounded static reason codes. Timeouts,
5xx, cancellation, malformed output from a real response, grant-refresh auth
failures and local refusals still never switch. Readiness publishes the eligible
reason codes.

Model artifacts: a failure before any provider turn keeps a bounded receipt and
at most one representative request and audit input per failure window instead of
the full multi-megabyte bundle on every retry. The first failure evidence is
preserved and linked; nothing is deleted. This removes the outage write load that
could exceed the controller's stop timeout.

Host migration: `host-migrate` rewrites every planner in the `previous_config`
chain (bounded to 32 entries with cycle detection), so a dated planner schedule
passes `preflight` after migration instead of refusing on a legacy predecessor.

## 0.10.1 — 2026-11-17

Fix the transcript index test to inspect the supervisor archive named by its
recorded run ID. Filesystem directory order could select an earlier supervisor
run and fail CI despite correct recorded output. Explicitly release stopped-
and halted-carry acceptance locks with the existing advisory guard, so concurrent
process creation cannot keep a completed operation locked. Includes the 0.10.0 cleanup
below; that source tag did not pass CI and has no binary release.

## 0.10.0 — 2026-11-17

Keep one paper decision path. Luna compiles factual digests and the persistent
orchestrator owns actions, sizes, holding periods and exits. Remove the retired
Decide role, handoff gates, legacy policy migrations and repair commands. Old
usage reservations and archived transcripts retain their original identities.
Planner configs must use the owner-only shape and pass reviewed `config-apply`
before an existing book resumes.

Rename the crates, binaries, environment variables, release assets and service
templates to Nivra. Add `nivra host-migrate` to review and apply the one-time
installation transition while preserving bound book and credential paths. The
command defaults to a dry run and requires `--during-market-hours` inside the
activation window. It never starts a service or resumes a book.

Remove the retired dashboard routes and Decide identity filter, split large
runtime modules, and delete unreachable financial and storage implementations.
The offline full-day demo now records factual digest attempts without running
the persistent owner or creating fills. Live orders remain disabled.

## 0.9.33 — 2026-11-16

Tighten the bounded prompt inputs. Replay now keeps at most 4 KB for one replayed
request, 12 KB for the whole replayed conversation and 6 KB of carried summary,
and host facts bound `model/luna-latest.json` to 8 KB. The owner prompt is built
from the brief, host facts, a compact summary and a bounded replay instead of a
growing transcript, which was still reaching about 220 KB after the earlier
changes. Compaction thresholds, decision authority, tool definitions and the
append-only log are unchanged; only prompt reconstruction is bounded.

## 0.9.32 — 2026-11-16

Bound the replayed committed conversation. The owner prompt replayed every
committed turn's request after the latest compaction summary, so old turns that
still carried Luna's raw tool observations and data rows kept the decision prompt
at about 223 KB even after the digest and the owner facts became small. Replay now
drops host envelope and legacy tool-result fields, keeps only the digest brief and
coverage, bounds one replayed request to 8 KB and the whole conversation to 32 KB
(oldest exchanges omitted first, with `omitted_exchanges`), and bounds the carried
summary to 12 KB. The append-only log, recorded requests and results are
unchanged; only the prompt reconstruction is bounded.

## 0.9.31 — 2026-11-16

Keep the owner prompt small. The decision facts embedded Luna's raw digest tool
observations, so the prompt grew with every digest tool call (about 211 KB on the
live host) even after the digest itself became a short brief. The owner prompt now
carries only the brief, the digest identity/count and host facts; the observations
stay on the durable digest plan and are read on demand through
`get_luna_tool_results`. No evidence is deleted, and the owner can still cite or
re-read any observation through its tools.

## 0.9.30 — 2026-11-16

Wait for a slow but healthy controller restart before calling an activation
failed. A restart re-derives pre-open history coverage, and on a large receipt
corpus that took about 150 seconds on the paper host while the updater allowed
only 60 seconds. The healthy candidate was reported as `update_dashboard_health_failed`
and the rollback restart failed the same way, leaving the book stopped. The
authenticated health wait is now a named 240-second bound; it is still a bound,
still requires the same healthy projection, and still rolls back on a real
failure.

## 0.9.29 — 2026-11-16

Give the decision owner a digest in words instead of a data dump. The owner
digest now carries a bounded `brief` (plain prose, at most 2000 characters) and
the Luna prompt asks for direction, focus and options rather than pasted tool
output and tables. Detailed ranking, bar, spread, recent-action and experiment
rows stay optional and empty by default; the orchestrator fetches the exact
values with its own tools, and the decision template and orchestrator system
prompt now say the brief is direction, never data. A missing brief warns rather
than refusing the factual digest, and an oversized brief fails closed.

The live decision prompt had grown to roughly 211 KB, and every provider
decision turn failed response validation (HTTP 200) before any plan was read.
This change shrinks that prompt to the brief plus host facts. Prompt bytes
participate in authority, so installing it follows the reviewed policy
transition.

## 0.9.28 — 2026-11-16

Accept the research-lane usage bucket in the usage reservation inventory. An
owner-research turn records a `research-NNNN` usage reservation (#238), but the
inventory validator only accepted the six trading role prefixes. A completed
research turn therefore made every later usage read refuse
`planner_usage_reservations_invalid`, and the controller halted on authority
initialization with no digest or owner decisions. The validator now accepts the
documented `research` bucket; unknown role prefixes still refuse, and budget
accounting, trading policy and execution are unchanged.

## 0.9.27 — 2026-11-16

Reviewed forced activation for a flat in-session book. `--force-unwatched-window`
now admits a bounded restart window when the paper book is flat and in session
(no positions, no pending orders, not halted, not an operator-stopped book) with
reason `market_open_flat`, instead of deferring until the blackout ends. The
original book is preserved and the same session resumes after the health-checked
restart; a halted or operator-stopped book still defers, and every integrity,
hold, identity, preflight and rollback guard is unchanged. The unwatched window
is recorded in the transaction receipt. No trading policy, sizing or execution
behaviour changed.

## 0.9.26 — 2026-11-16

First Nivra binary release and the one-time deployment rehome. A host installed
from the retired `ind-sadr` repository can now adopt the exact installed release
identity once, under review, and continue receiving guarded updates from this
repository through the canonical `github-nivra:` transport. The rebind is inert
without `--migrate-distribution`, refuses a mismatched or still-published legacy
identity, requires an explicit advancing target tag, and records
`distribution-migration.json`; every existing integrity, book, clock, hold,
preflight and rollback guard still applies. The retired `github-ind-sadr:`
transport remains accepted only for that migration, and the old release
directory is retained unchanged as the rollback target.

Pre-open history coverage no longer fails on an accumulated receipt corpus. The
coverage pass skips write-once receipts that predate the required window before
decoding them, and its explicit entry/decode bounds fail closed with
`history_receipt_inventory_limit`/`history_receipt_decode_limit` instead of a
fixed 50,000-file ceiling. Coverage derivation, cancellation, deadline, quota and
evidence handling are unchanged; a skipped receipt can only understate coverage
for that pass. No trading policy, sizing or execution behaviour changed.

## 0.9.25 — Initial Nivra source snapshot

This is the initial public source snapshot of Nivra, an experimental paper-only
trading and research workspace. The version is retained for CLI, configuration
and on-disk compatibility; it does not identify a published Nivra binary release.

The snapshot includes the deterministic financial engine, model-led paper
controller, sandboxed research lane, shared-engine replay and read-only dashboard.
See [architecture](docs/ARCHITECTURE.md) and [testing](docs/TESTING.md) for the
current design and verification commands.

Prior operational Git history, issues, logs, tags and releases are not included.
