# Working rules for Nivra

Read [README.md](README.md), the [documentation index](docs/README.md) and open
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues) before changing
anything. Update current guides when behaviour changes; track future work in
Issues and leave handoff detail in Git history.

## Hard boundaries

- **Paper only.** No live orders or live activation without a separate, explicit
  live mandate from the repository owner. A tag, install or passing build never
  authorizes a session.
- Models select paper actions/sizes/holding periods. Under the reviewed
  `decision_owner` policy, Luna compiles factual digests and the persistent
  orchestrator owns decisions; legacy policies remain readable for reviewed
  migration and archive access. Deterministic code owns validation, credentials,
  cash/fees, fills and protection. The reviewed model-led paper policy has no
  added trading risk caps. No eligible evidence means no model call; no silent
  fallback.
- Keep broker, account, instrument and session identities explicit. Never retry
  an ambiguous order or fail it over to another broker. An ACK is not a fill.
- Preserve stopped or halted books, original journals and failed artifacts. No
  automatic restart, unhalt, replay, quota reset or replacement journal.
- Keep the two-second financial freshness gate and the archived quote receipt
  time. Never replace receipt time with a later clock sample.
- Verify instruments, tick units, calendars and fees. Never invent identifiers,
  lots, holidays, prices or provider guarantees.

## Secrets

- Never print, log or commit tokens, `.env*`, auth state, account payloads or
  `data/`. Redact errors. Do not put host addresses or key paths in the
  repository.
- Use only the application's own credentials. Never read another application's
  or agent's auth caches.

## Code and verification

- The repository is Cargo only. Every commit must build and be formatted; before
  committing to `main`, run the full gate in
  [Fast build and test](#fast-build-and-test) below.
- Keep the test suite lean and behavioral ([testing](docs/TESTING.md)). Tests
  never open sockets; use fixtures, fakes and injectable clocks.
- Keep `sadr-rs paper`, `probe`, `resume` and the dashboard compatible with the
  installed config and on-disk state unless a migration is explicitly planned.
- New work follows open [GitHub Issues](https://github.com/TharunChougoni/nivra/issues),
  implemented in Rust.
- Commit and push directly to `main` after the required checks pass. A separate
  branch is optional when isolation helps. Delete branches after their work is
  included in `main`. Create release tags only when asked.

## Fast build and test

Use all cores; do not hardcode `CARGO_BUILD_JOBS=2`. Reduce jobs only on a
memory-constrained or shared host. See [build performance](docs/BUILD_PERFORMANCE.md)
for portable settings.

Run the gate in this order, reusing an existing `target/` directory so the build
stays warm:

```sh
cargo fmt --all --check                                       # ~2s, fails fastest
cargo build --workspace --all-targets --locked                # one compile, test features
cargo clippy --workspace --all-targets --locked -- -D warnings  # ~30s warm
SADR_REQUIRE_SCORER_SANDBOX=1 cargo test --workspace --locked  # ~15-20s, no recompiling
target/debug/sadr-tools test-budget && target/debug/sadr-tools check-release
```

`test-budget` must print `400/400`; replace or merge an existing test instead of
raising it.

- Skip a separate `cargo build --workspace`. `--all-targets` covers it, and a
  plain build after the tests rebuilds tokio and sadr-cli because the dev
  `test-util` feature differs.
- Skip `cargo run -p sadr-tools`. Building one package on its own resolves
  features differently and recompiles serde, chrono and sadr-core (12–26s).
  Run the binary that is already built.
- Build musl only for a release, once, on the tagged commit. Never run tests on
  musl. Don't rerun the gate after tagging if it already passed on that commit.
  A fresh clone or worktree means a cold build of about 3 min.
- The release profile uses `codegen-units = 16` with thin LTO. It measured about
  39s warm and 106s cold for the musl `sadr-rs`; keep it unless a benchmark shows
  a better trade-off.
- Keep `target/`. Don't `cargo clean`, and don't build a GNU `--release`; it is
  never shipped.

## Dashboard frontend gate

The React workspace is strict TypeScript. After frontend changes, build the
committed assets and verify types, behavior and source/bundle hashes:

```sh
npm ci --prefix rust/sadr-cli/src/runtime/web/v2
npm run build --prefix rust/sadr-cli/src/runtime/web/v2
npm test --prefix rust/sadr-cli/src/runtime/web/v2
```

Commit source, lockfile and generated bundle together. The removed Preact shell
has no separate package or test gate. The committed bundle needs no Node runtime.

## Releases

A push to `main` does not deploy. This source snapshot has no published binary
release. When a release is explicitly requested, the version in `Cargo.toml`, the
changelog heading and the annotated tag `vX.Y.Z` must match on the exact commit.
Never move a published tag; ship fixes as the next patch. See
[development](docs/DEVELOPMENT.md).
