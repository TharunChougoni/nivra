# Development and releases

The repository has three build units:

| Unit | Path | Toolchain |
| --- | --- | --- |
| Rust workspace, six crates | `rust/` | Rust `1.95.0` from `rust-toolchain.toml`, edition 2024 |
| Dashboard client | `rust/nivra-cli/src/runtime/web/v2` | Node 24, React, Vite and TypeScript |
| pi agent | `agent/` | Node 22.19 or later, TypeScript |

Build and test on a development host, never on the paper host.

## Local gate

Run the full gate before you commit to `main`. Run it from the repository root,
in this order:

```sh
cargo fmt --all --check
cargo build --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
NIVRA_REQUIRE_SCORER_SANDBOX=1 cargo test --workspace --locked
target/debug/nivra-tools test-budget
target/debug/nivra-tools check-release
target/debug/nivra-tools golden-check
npm test --prefix rust/nivra-cli/src/runtime/web/v2
```

The test environment variable makes sandbox tests fail instead of skip when
`python3` or bubblewrap is missing. [Testing](TESTING.md) has the test rules.

If you changed files under `agent/`, also run the pi agent gate:

```sh
npm ci --prefix agent
npm run build --prefix agent
npm test --prefix agent
```

Commit `agent/src`, `agent/test` and the lockfile, but not `agent/dist`.

## Dashboard client

Rust embeds the committed `bundle/` with `include_bytes!`, so Node is not a
runtime dependency. After you change client source or dependencies, rebuild
before you compile Rust:

```sh
npm ci --prefix rust/nivra-cli/src/runtime/web/v2
npm run build --prefix rust/nivra-cli/src/runtime/web/v2
```

Commit the source, the lockfile and the generated `bundle/` together. `npm test`
checks that the bundle manifest matches the source, so a stale build fails.

## Keep builds fast

- Reuse one clone and its `target/` directory. A fresh clone or worktree
  starts cold, at about 3 minutes. Do not run `cargo clean`.
- Use all cores. Do not set `CARGO_BUILD_JOBS=2`. Lower the job count only on
  a small, shared or memory-starved host. Allow about 0.3 to 0.5 GiB per rustc
  job, plus more while linking.
- Run one heavy build at a time.
- Build every target once with `--all-targets`. A separate
  `cargo build --workspace` is redundant, and a plain build after the tests
  rebuilds `tokio` because the dev `test-util` feature differs.
- Run `target/debug/nivra-tools` directly. `cargo run -p nivra-tools` resolves
  features for one package and recompiles shared dependencies.
- Keep `target/` on a local SSD, using `CARGO_TARGET_DIR` if needed. Set
  `CARGO_INCREMENTAL=0` for a clean gate run, and leave it unset while you edit.

While you edit, check only the crate you changed:

```sh
cargo check -p nivra-cli
cargo test -p nivra-cli --lib
```

Build musl only for a release, once, on the tagged commit. Never run tests on
musl, and never build a GNU `--release`, because it is never shipped. The
release profile uses thin LTO and `codegen-units = 16`.

## Branches

Push directly to `main` after the gate passes. Delete a branch once its work is
in `main`. Update the guides in the same commit as the behavior they describe.

## Cutting a release

Publish releases from this repository only, and only when a release is
requested. The version in `Cargo.toml`, the `## X.Y.Z` changelog heading and
the annotated tag `vX.Y.Z` must match on the same commit.

1. Put the workspace version, `Cargo.lock` and the changelog heading in the
   implementation commit, then run the local gate.
2. Create the annotated tag and push `main` and the tag together:

   ```sh
   git tag -a vX.Y.Z -m vX.Y.Z && git push --atomic origin main vX.Y.Z
   ```

3. CI runs once on the push. The release job starts only after the four
   required jobs pass: **Dashboard JS tests**, **pi agent tests**,
   **Rust fmt + clippy** and **Rust tests**. It reuses the tested
   `release-tools` artifact, builds the musl `nivra` and `nivra-analysis`,
   runs `self-check` and publishes. It does not rerun tests. A missing or older
   tag skips publication. A matching lightweight tag fails the run.
4. If the tag was added after CI finished, or publication failed, run
   **Fast musl release** on `main`:

   ```sh
   gh workflow run rust-release.yml --ref main -f tag=vX.Y.Z
   ```

   It reuses the latest push-main CI run for that exact commit if the four
   required jobs passed. The `release-tools` artifact expires after 30 days.
   After that, rerun CI on the commit.
5. Check the five published files: the musl `nivra` and `nivra-analysis`,
   `release.json` and both SHA-256 files. Check that `refs/release-assets/vX.Y.Z`
   points at the tagged source.

### Immutable tags and assets

- Never move a published tag. Ship a fix as the next patch version.
- After `refs/release-assets/vX.Y.Z` exists, its assets never change. A rerun
  verifies them and only creates a missing GitHub release or uploads missing,
  byte-identical attachments. It never rebuilds over an existing asset ref or
  overwrites an attachment.
- The installed updater can see the asset ref before the GitHub release exists.
  Finish a partial publication with a rerun, not a new build.
- Never run two publishers for one tag. For the manual fallback, see
  [operations](OPERATIONS.md). A tag never starts a paper or live session.

## Tools

| Command | Purpose |
| --- | --- |
| `nivra-tools check-release` | Changelog and version, Markdown links and retired words |
| `nivra-tools test-budget` | Count Rust tests against the 400 limit |
| `nivra-tools golden-check` | Verify frozen fixture bytes |
| `nivra-tools release-artifact` | Assemble release assets |
| `nivra-tools replay`, `replay-fingerprint` | Offline shared replay ([reference](reference/SHARED_REPLAY.md)) |
| `nivra self-check --expected-version vX.Y.Z` | Offline check of a built binary |
| `nivra backtest` | Offline quote backtest |

## Retired words

`check-release` fails when a file under `rust/`, `docs/` or `packaging/` holds
more retired words than its line in `rust/nivra-tools/retired-names.txt` allows.
The word list is in `rust/nivra-tools/src/retired.rs`. An allowance only
shrinks. When a file loses occurrences, lower its line in the same commit, or
regenerate the file with `nivra-tools retired-names`.
