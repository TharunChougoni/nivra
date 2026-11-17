# Rust workspace

Members: `nivra-core`, `nivra-cli` (binary `nivra`), `nivra-store`, `nivra-tools`,
`nivra-research`, and `nivra-sandbox` (CLI-free scorer/replay containment API).
Toolchain pinned in `rust-toolchain.toml`; dependencies pinned in `Cargo.lock`.
Version comes from the root `Cargo.toml`.

See [architecture](../docs/ARCHITECTURE.md) and
[development](../docs/DEVELOPMENT.md).
See [strategy sandbox](../docs/STRATEGY_SANDBOX.md) for containment behavior.

`nivra memory-check` reads one explicit non-secret JSON file (≤16 KiB) and
reports whether declared peaks fit a budget. It doesn't measure a host or
authorize installation.

`runtime/supervisor/wake.rs` owns wake defaults, trigger/profile overlays and
pure scheduling precedence. `supervisor.rs` reexports `Wake` and `Inputs` and
retains lifecycle workers; durable park/load/save remains in `supervisor/state.rs`.

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
