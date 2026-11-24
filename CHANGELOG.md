# Source snapshot

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
