# Architecture

Cargo-only workspace; `nivra` owns the paper controller and embedded dashboard.
No live order transport is enabled.

| Crate | Ownership |
| --- | --- |
| `nivra-core` | Decimal finance, fees, instruments/calendar, quotes, paper execution and replay |
| `nivra-store` | SQLite ledger, durable control/read journals, writer bindings |
| `nivra-cli` | Controller, scanner, model harness, history, dashboard, recovery and updater |
| `nivra-research` | Registered jobs, leases, sandbox worker and result contracts |
| `nivra-sandbox` | CLI-free scorer/replay containment API |
| `nivra-tools` | Release assembly/checks, streaming replay and progress tooling |

The shared financial engine is `nivra-core`; the paper controller and offline
replay both call it. The research lane is separate from the trading lane and
cannot place, size or promote a trade.

See [strategy sandbox](STRATEGY_SANDBOX.md) for its containment behavior. Supervisor durable state and load/save/park helpers are in `runtime/supervisor/state.rs`.
Wake configuration and pure scheduling precedence are in
`runtime/supervisor/wake.rs`; `supervisor.rs` retains lifecycle workers and
reexports the existing `Wake` and `Inputs` paths.

## Runtime flow

1. Resolve the committed session pointer and original book/journal identities;
   fence competing writers and retain stopped/halted state. Root controller guards and the resume/config-apply/planner-recovery
   book guards explicitly unlock on scope exit, so a concurrent fork awaiting
   exec cannot extend a completed operation’s lock lifetime.
2. Prepare dated NSE/Upstox universe and previous-session history. Coverage gaps
   remain explicit. Protect held positions before lower-priority scanning.
3. Rotate configured quote batches. Project timestamped scanner prospects and full
   historical features; observations are not executable fill authority.
4. Luna compiles a factual digest from eligible evidence. The durable owner queue
   feeds one persistent orchestrator that selects paper action, size, holding
   period and exit; Luna has no handoff veto. Legacy policies decode for reviewed
   migration and archive access, but cannot schedule independent Decide calls.
5. Deterministic execution rechecks identity, held units, available cash, fees and
   the original ≤2-second quote receipt under the financial lock. Accepting a plan
   does not create a fill. Outcome/holdings feed the next fresh Luna review.
6. Close/seal the dated book without forced liquidation of model-led holdings.
   Separate advisory jobs review sealed evidence; normal rollover verifies carry.

Model/network work runs outside financial transactions. Protection continues while
models run. Ordinary provider/schema/timeout failures end the event with no new
trade; corruption or lost authority is a distinct integrity incident.

## Durable state

SQLite uses WAL/FULL sync. The original book owns fills, scanner checkpoints,
planner events/attempts/plans, halt history and review jobs. Hashed artifacts retain
inputs, outputs and receipts; namespace/device/inode pins prevent journal resets.
`sessions_root/current.json` selects the committed dated successor. Credentials
stay in private application files outside Git/model evidence.

Research workers receive registered hash-pinned inputs, never financial writer
ownership or credentials. [Paper replay](PAPER_REPLAY.md) replaces clocks/data/model
adapters with explicit offline evidence while using the same paper flow.

See [operations](OPERATIONS.md) and [harness](MODEL_HARNESS.md). Future work is
tracked in [GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

The financial freshness gate derives both clock units from
`nivra_core::market::MAX_FINANCIAL_AGE: Duration` (two seconds). `_MS` is used
for execution clocks and `_MICROS` for market receipts; receipt timestamps are
still archived unchanged.

`runtime/model_admission.rs` binds real provider calls to the exact configured
role/profile and application credential path before reserving usage. Owner calls
use the active admitted selection, never the pending revision. Catalog membership
alone remains insufficient; provider approval and exact response checks still run.

`runtime/instrument_display.rs` owns background public BOD capture, bounded
metadata projection and immutable source receipts. It joins only already-verified
cash metadata for qualified tick display and never extends `Universe::instruments`.
Authenticated catalog/chain reads stay in `web/v1.rs`; summary readiness uses a
nonblocking status read so catalog work cannot hold up the financial loop.

`runtime/planner/config.rs` owns `ModelRole`, its defaults, invocation/output
bounds and role validation. `planner.rs` reexports the existing paths;
`planner/turn.rs` owns turn execution. Serialized role configuration and budget
semantics are unchanged.

`runtime/web_tools/` owns the opt-in orchestrator web search/fetch/news tools:
configuration, public-HTTPS URL policy, bounded provider reads, per-run
budget and the session `model/web-tools.ndjson` call record. It has no broker
credential, order path or sandbox environment access. The owner workspace
sandbox keeps network disabled unless the reviewed `workspace.network` setting
explicitly enables it; the post-close scorer sandbox refuses network.

`runtime/wide/projection.rs` owns bounded execution/usage dashboard reads and
their existing incident handling; the controller retains the original call order.

`runtime/wide/controller.rs` owns the dated paper loop and worker orchestration.
`wide.rs` reexports the original `run` path and retains book/history/recovery helpers.

`runtime/planner/turn.rs` owns digest turn production, cached-digest admission,
trigger envelopes and result assembly; `planner.rs` reexports existing entry paths.

`runtime/supervisor/consider.rs` owns owner/supervisor wake admission and task
precedence; `supervisor.rs` reexports `Tick` and `consider_owner` unchanged.

`runtime/supervisor/decision.rs` owns persistent owner decision start, completion
and interruption recovery. Durable state, wake rules and admission remain in
`supervisor/state.rs`, `supervisor/wake.rs` and `supervisor/consider.rs`.
