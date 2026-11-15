# Build performance

This guide describes the repository's build shape and portable settings. It does
not publish a machine benchmark or guarantee timings; measure your own host and
record the environment with any number you keep.

The goal is a short local gate and a short release build without hiding test
coverage or weakening the paper-only checks. The mandatory gate and release rules
are in [AGENTS](../AGENTS.md).

## Workspace shape

The repository is Cargo-only: one workspace, one `Cargo.lock`, six crates and a
pinned Rust toolchain (`1.95.0`, edition 2024). The controller binary is
`sadr-rs`; `sadr-tools` owns release assembly and repository checks. The release
target is `x86_64-unknown-linux-musl`.

Keep `target/` on a local SSD or NVMe volume. A fresh clone or worktree starts
cold; reusing the existing clone keeps the gate warm.

## Fast local gate

Run this sequence from the repository root:

```sh
# Do not set CARGO_BUILD_JOBS=2.
# Set CARGO_INCREMENTAL=0 for a clean gate/CI run; omit it for an edit loop.
cargo fmt --all --check
cargo build --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
SADR_REQUIRE_SCORER_SANDBOX=1 cargo test --workspace --locked
target/debug/sadr-tools test-budget
target/debug/sadr-tools check-release
npm test --prefix rust/sadr-cli/src/runtime/web/v2
```

Why this order:

- `--all-targets` builds binaries, tests and examples once with the same feature
  set. `cargo test` then reuses those artifacts instead of rebuilding `tokio`
  with the dev `test-util` feature.
- `cargo build --workspace` is redundant after `--all-targets`; it is not a
  separate gate step.
- Never run `cargo run -p sadr-tools` in the gate. It resolves features for that
  package alone and rebuilds shared dependencies. Build once, then execute
  `target/debug/sadr-tools` directly.
- Run one heavy build at a time when the desktop is also in use. The gate is
  CPU- and link-bound; parallel cargo builds compete for memory.

For iterative work, check only the crate you changed:

```sh
cargo check -p sadr-cli
cargo test -p sadr-cli --lib
```

Use the full gate before committing to `main`.

## Release build

Use the same profile that produces the release musl artifacts:

```sh
cargo build --release --locked --offline \
  --target x86_64-unknown-linux-musl \
  -p sadr-cli --bin sadr-rs --bin sadr-analysis
```

The release profile keeps thin LTO, strips debug info, and sets
`codegen-units = 16`. The paper controller is I/O-bound, so the small
binary-size increase is not worth a serial multi-minute codegen pass. Check the
result with:

```sh
target/x86_64-unknown-linux-musl/release/sadr-rs self-check \
  --expected-version vX.Y.Z
```

Build musl only for a release, once, on the tagged commit. Never run tests on
musl. Do not build a GNU `--release`; it is never shipped.

## General setups

### CPU jobs

Use the Cargo default (the logical CPU count), or set jobs to the physical core
count on a hybrid CPU. A hardcoded `2` is only appropriate on a small, shared or
memory-starved host; it is not a test-stability requirement. Do not set
`CARGO_BUILD_JOBS=2` on a normal development host.

### Memory

Reserve headroom for the OS and other work. A practical rule of thumb is roughly
0.3–0.5 GiB per rustc job, with a larger spike while linking. If available memory
is low, reduce `CARGO_BUILD_JOBS` or build one crate at a time. Avoid running two
full workspace builds concurrently.

### Disk and incremental state

Keep `target/` on a local SSD or NVMe volume. Do not put it on a network share,
a synced folder or a slow external disk. If the repository lives on a different
volume, set `CARGO_TARGET_DIR` to a local path.

Prefer a targeted clean for a single crate when you need to break a stale
artifact:

```sh
cargo clean -p sadr-cli
```

Incremental compilation is useful during an edit loop. Set
`CARGO_INCREMENTAL=0` for a clean gate or CI run; switching between the two modes
usually causes one larger rebuild.

### Optional compiler and linker acceleration

These are optional, not required:

- `sccache`: if installed, set `RUSTC_WRAPPER=sccache` and a fast local
  `SCCACHE_DIR`. The first build is cold; later clean builds reuse compiler
  output.
- `lld` or `mold`: faster linking for large binaries. Use a per-user Cargo config
  (never commit a machine-specific `.cargo/config.toml`) and re-run `self-check`
  after changing the linker.

Do not benchmark with a cache you have not verified; a linker or wrapper change
can hide a real build failure.

## CI

`.github/workflows/ci.yml` runs formatting + Clippy and the test suite in
parallel jobs, restores `Swatinem/rust-cache@v2`, sets `CARGO_INCREMENTAL=0`,
builds `--all-targets` once, and runs the two `sadr-tools` checks from
`target/debug` instead of through `cargo run`. It uploads the already-built
`sadr-tools` assembler for successful push-main runs (30-day retention).

The fast release workflow verifies the exact SHA's latest CI run and all three
required jobs, downloads that assembler, and uses a separate musl cache with
workspace crates enabled. It builds only `sadr-rs` and `sadr-analysis`,
smoke-checks and publishes; there is no repeated test gate, frontend install or
GNU build.

## Re-measuring

Record the date, CPU, available RAM, toolchain, whether the build was cold or
warm, and the exact command. Do not compare a cold build with a warm one, and do
not use `cargo clean` just to make a number look better. Any number you keep is a
snapshot, not a permanent performance guarantee.
