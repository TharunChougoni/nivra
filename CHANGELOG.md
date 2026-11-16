# Source snapshot

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
