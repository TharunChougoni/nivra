# Source snapshot

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
