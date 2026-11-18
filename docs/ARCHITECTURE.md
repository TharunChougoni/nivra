# Architecture

The application is a Cargo workspace; `nivra` owns the paper controller and
embedded dashboard. The pi agent is a separate TypeScript package under `agent/`.
No live order transport is enabled.

| Crate | Ownership |
| --- | --- |
| `nivra-core` | Decimal finance, fees, instruments/calendar, quotes, paper execution and replay |
| `nivra-store` | SQLite ledger, durable control/read journals, writer bindings |
| `nivra-cli` | Controller, scanner, pi agent lanes, history, dashboard, recovery and updater |
| `nivra-research` | Registered jobs, leases, sandbox worker and result contracts |
| `nivra-sandbox` | CLI-free scorer/replay containment API |
| `nivra-tools` | Release assembly/checks, streaming replay and progress tooling |

The shared financial engine is `nivra-core`; the paper controller and offline
replay both call it. The research lane is separate from the trading lane and
cannot place, size or promote a trade.

See [strategy sandbox](STRATEGY_SANDBOX.md) for its containment behavior. The
earlier persistent supervisor's durable state and load/save helpers remain in
`runtime/supervisor/state.rs` for archived-books and the read-only dashboard
projection; the live controller starts no supervisor turn. Wake configuration and
pure scheduling precedence in `runtime/supervisor/wake.rs` are retained for
archived-state handling and offline replay.

## Runtime flow

1. Resolve the committed session pointer and original book/journal identities;
   fence competing writers and retain stopped/halted state. Root controller guards and the resume/config-apply/planner-recovery
   book guards explicitly unlock on scope exit, so a concurrent fork awaiting
   exec cannot extend a completed operation’s lock lifetime.
2. Prepare dated NSE/Upstox universe and previous-session history. Coverage gaps
   remain explicit. Protect held positions before lower-priority scanning.
3. Rotate configured quote batches. Project timestamped scanner prospects and full
   historical features; observations are not executable fill authority.
4. Rust's deterministic trigger engine wakes a pi decision lane turn from eligible
   evidence: a fill, position risk, a persistent top-ranked candidate or a
   scheduled/pre-close turn. Rust builds the bounded brief and publishes the
   lane's tools; pi owns the provider call, conversation state, tool loop,
   compaction and usage report. Only a completed turn with a final validated
   proposal can reach the paper plan head. There is no factual digest or
   persistent orchestrator on the live path; older journals and transcripts keep
   their archive identities.
5. Deterministic execution rechecks identity, held units, available cash, fees and
   the original ≤2-second quote receipt under the financial lock. Accepting a plan
   does not create a fill. Outcome/holdings feed the next decision turn and the
   research/review lanes.
6. Close/seal the dated book without forced liquidation of model-led holdings.
   The review lane runs once after the seal and its redacted text becomes the
   review memory the next session carries. Normal rollover verifies carry and
   requires the successor's pi paper planner.

Model/network work runs outside financial transactions. Protection continues while
models run. Ordinary provider/schema/timeout failures end the event with no new
trade; corruption or lost authority is a distinct integrity incident.

## Durable state

SQLite uses WAL/FULL sync. The original book owns fills, scanner checkpoints,
planner events/attempts/plans, halt history and the review memory. Hashed artifacts retain
inputs, outputs and receipts; namespace/device/inode pins prevent journal resets.
`sessions_root/current.json` selects the committed dated successor. Credentials
stay in private application files outside Git/model evidence.

Research workers receive registered hash-pinned inputs, never financial writer
ownership or credentials. [Paper replay](PAPER_REPLAY.md) replaces market/clock
inputs with explicit offline evidence and replays recorded model replies through
the retained planner backend while using the same paper flow; it never starts pi
or the lane service.

See [operations](OPERATIONS.md) and [harness](MODEL_HARNESS.md). Future work is
tracked in [GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

The financial freshness gate derives both clock units from
`nivra_core::market::MAX_FINANCIAL_AGE: Duration` (two seconds). `_MS` is used
for execution clocks and `_MICROS` for market receipts; receipt timestamps are
still archived unchanged.

`runtime/model_admission.rs` retains the earlier planner's provider-call admission
checks (exact configured role/profile and application credential path before
reserving usage). It belongs to the retained legacy planner path used by offline
replay and archived-policy validation, not to pi lane turns.

`runtime/instrument_display.rs` owns background public BOD capture, bounded
metadata projection and immutable source receipts. It joins only already-verified
cash metadata for qualified tick display and never extends `Universe::instruments`.
Authenticated catalog/chain reads stay in `web/v1.rs`; summary readiness uses a
nonblocking status read so catalog work cannot hold up the financial loop.

`runtime/planner/config.rs` owns `ModelRole`, its defaults, invocation/output
bounds and role validation for the retained legacy planner. Serialized legacy
role configuration and budget semantics are unchanged.

`runtime/agent/` owns the pi agent service the paper controller requires: the
JSONL client, the lane runner and budgets, the deterministic trigger engine and
the tool bridge that serves pi's calls through the harness executors. The book's
`model/agent/` directory holds trigger batches, turn receipts, plan outcomes,
usage and lane sessions. See [pi agent lanes](AGENT.md).

`runtime/web_tools/` owns the opt-in web search/fetch/news tools exposed to the
pi decision and research lanes: configuration, public-HTTPS URL policy, bounded
provider reads, per-run budget and the session `model/web-tools.ndjson` call
record. It has no broker credential, order path or sandbox environment access.
The owner workspace sandbox keeps network disabled unless the reviewed
`workspace.network` setting explicitly enables it; the post-close scorer sandbox
refuses network.

`runtime/wide/projection.rs` owns bounded execution/usage dashboard reads and
their existing incident handling; the controller retains the original call order.

`runtime/wide/controller.rs` owns the dated paper loop and worker orchestration.
`wide.rs` reexports the original `run` path and retains book/history/recovery helpers.

`runtime/planner/turn.rs` retains digest-turn production, cached-digest admission,
trigger envelopes and result assembly for offline recorded-reply replay; the live
controller reaches the plan head only through `runtime/agent/paper.rs`.

`runtime/supervisor/consider.rs` and `runtime/supervisor/decision.rs` retain the
earlier owner/supervisor wake admission and persistent owner-decision mechanics as
test-only code. `supervisor.rs` keeps the read-only dashboard projections and
archived-state readers the running controller still uses.
