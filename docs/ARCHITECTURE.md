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

See [strategy sandbox](STRATEGY_SANDBOX.md) for its containment behavior.

## Runtime flow

1. Resolve the committed session pointer and original book/journal identities;
   fence competing writers and retain stopped/halted state. Root controller guards and the resume/config-apply/planner-recovery
   book guards explicitly unlock on scope exit, so a concurrent fork awaiting
   exec cannot extend a completed operation’s lock lifetime.
2. Prepare dated NSE/Upstox universe and previous-session history. Coverage gaps
   remain explicit. Protect held positions before lower-priority scanning.
3. Rotate configured quote batches. Project timestamped scanner prospects and full
   historical features; observations are not executable fill authority.
4. Rust's deterministic trigger engine starts a pi decision lane turn from eligible
   evidence: a fill, position risk, a persistent top-ranked candidate or a
   scheduled/pre-close turn. Rust builds the bounded brief and publishes the
   lane's tools; pi owns the provider call, conversation state, tool loop,
   compaction and usage report. Only a completed turn with a final validated
   proposal can reach the paper plan head.
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
ownership or credentials.

See [operations](OPERATIONS.md) and [lane tools](LANE_TOOLS.md). Future work is
tracked in [GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

The financial freshness gate derives both clock units from
`nivra_core::market::MAX_FINANCIAL_AGE: Duration` (two seconds). `_MS` is used
for execution clocks and `_MICROS` for market receipts; receipt timestamps are
still archived unchanged.

`runtime/instrument_display.rs` owns background public BOD capture, bounded
metadata projection and immutable source receipts. It joins only already-verified
cash metadata for qualified tick display and never extends `Universe::instruments`.
Authenticated catalog/chain reads stay in `web/v1.rs`; summary readiness uses a
nonblocking status read so catalog work cannot hold up the financial loop.

`runtime/planner.rs` owns the pi paper policy shape (`PlannerConfig`), its
validation and policy hash, the TradePlan validator (`planner/plan.rs`), the
brief's call-time evidence (`planner/turn.rs`) and the lane call guard
(`planner/guard.rs`). `runtime/redecision.rs` owns the durable plan-head
authority: pi claims, settlement, recovery and config rebinding.

`runtime/agent/` owns the pi agent service the paper controller requires: the
JSONL client, the lane runner and budgets, the deterministic trigger engine and
the tool bridge. `runtime/harness/` owns the lane tool schemas, argument
validation, the data-tool executor and the research task and ranker runner. The book's
`model/agent/` directory holds trigger batches, turn receipts, plan outcomes,
usage and lane sessions. See [pi agent lanes](AGENT.md).

`runtime/web_tools/` owns the opt-in web search/fetch/news tools exposed to the
pi decision and research lanes: configuration, public-HTTPS URL policy, bounded
provider reads, per-run budget and the session `model/web-tools.ndjson` call
record. It has no broker credential, order path or sandbox environment access.
The research workspace sandbox keeps network disabled unless the reviewed
`workspace.network` setting explicitly enables it; the ranker sandbox refuses
network.

`runtime/wide/projection.rs` owns the bounded execution dashboard read and
their existing incident handling; the controller retains the original call order.

`runtime/wide/controller.rs` owns the dated paper loop and worker orchestration.
`wide.rs` reexports the original `run` path and retains book/history/recovery helpers.
