# Development and releases

## Client workspace build

The workspace uses React/Vite/TypeScript with Tailwind, Radix UI, Lucide and
uPlot. Node 24 is the build/CI environment; Rust serves the committed static
output. Node is not a runtime dependency of the paper controller. After client
source/dependency changes:

```sh
npm ci --prefix rust/nivra-cli/src/runtime/web/v2
npm run build --prefix rust/nivra-cli/src/runtime/web/v2
```

Commit the v2 source, lockfile and generated `bundle/` together. The bundle
manifest verifies source and artifact hashes in the JS gate, so a stale build
fails. Build locally before compiling Rust's `include_bytes!` assets. The source
HTML is a Vite input; `bundle/index.html` is the served shell. A dev server is
optional for isolated fixtures, never a paper/live session.

## Local gate

```sh
cargo fmt --all --check
cargo build --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings -D clippy::all
NIVRA_REQUIRE_SCORER_SANDBOX=1 cargo test --workspace --locked
target/debug/nivra-tools test-budget
target/debug/nivra-tools check-release
target/debug/nivra-tools golden-check
npm test --prefix rust/nivra-cli/src/runtime/web/v2
```

Build every target once with `--all-targets`, then run the tools binary directly;
`cargo run -p nivra-tools` rebuilds shared dependencies and is not part of the
gate. Do not hardcode `CARGO_BUILD_JOBS=2` on a normal development host. See
[build performance](BUILD_PERFORMANCE.md) for portable settings and for
memory-constrained or different setups. Run one heavy build at a time.

The workspace tests use Node 24, jsdom, the Node test runner and `tsc --noEmit`.
They need no browser or sockets. Build and test on a development host, not the
paper host.

## Branches

Commit and push directly to `main` once the local gate passes. A separate branch
is optional when isolation helps; delete it once its work is included in `main`.
Update documentation alongside behavior changes and resolve conflicting guidance.
Release tags still require an explicit request.

## Cutting a release

Nivra is the only deployment source; never install from the retired `ind-sadr`
repository. Releases are published from this repository when explicitly requested
(`v0.9.26` is the first), and an installed host bound to the retired repository is
moved once through the reviewed
[distribution migration](OPERATIONS.md#one-time-distribution-migration). The
workflow below runs when a release is explicitly requested and its annotated
version tag is created.

CI builds and publishes a release in the **same run** as its tests when an
annotated stable version tag points at that exact main-push commit. Its release
job waits for all three checks, reuses their `release-tools` artifact, builds only
the two musl binaries, runs `self-check` and publishes. It does not repeat
Clippy, Rust tests, frontend tests or a GNU build. Never build or test on the
paper host.

1. Include the workspace version, lockfile and `## X.Y.Z — date` changelog heading
   in the implementation commit before running the local gate. Avoid a separate
   version-only commit and its extra CI run.
2. When a release is explicitly requested and the local gate passes, commit the
   final source, create its annotated tag, and push main and tag atomically:
   `git tag -a vX.Y.Z -m vX.Y.Z && git push --atomic origin main vX.Y.Z`.
   The main-push CI selector matches the tag to its own SHA and workspace version;
   an absent tag or an older release tag skips publication. A matching lightweight
   tag refuses. Tags alone do not trigger another CI run.
3. CI runs the three required jobs once. If they pass, the release job verifies
   those completed jobs in its own still-running CI run and downloads that run's
   tested assembler. A failed, missing or skipped prerequisite prevents release.
   Version/tag/changelog/source checks and immutable publication guards still apply.
4. For a tag added **after** its main CI run finished, or to finish a failed
   publication, use **Fast musl release** on `main`:
   `gh workflow run rust-release.yml --ref main -f tag=vX.Y.Z`.
   A manual retry may reuse the latest completed exact-SHA push-main CI run when
   the three required named jobs passed, including when the aggregate run failed
   or was cancelled after those jobs. It rejects an in-progress, older, wrong-SHA
   or PR run, and any missing, failed or skipped required job. It never reruns
   tests. If the immutable asset ref does not exist, the `release-tools` artifact
   (retained 30 days) is required; missing or expired artifacts fail closed and
   require rerunning exact-commit CI. If the asset ref exists, recovery verifies
   it instead. Never move a tag to repair a failed run.
5. Verify the seven published files, including `default-prompts.json` and its
   checksum, immutable `refs/release-assets/vX.Y.Z`,
   `release.json` source SHA/target and all three SHA-256 files. The updater installs
   at its next eligible window. Research registrations separately pin their
   runtime hash and never silently follow a new engine.

Once `refs/release-assets/vX.Y.Z` exists, its assets are immutable and the
publication is resumable. The asset ref becomes available to the installed updater
before the GitHub release attachment step completes, so a failed release step
can leave an installable partial publication. Rerunning the failed release job
or dispatching **Fast musl release** verifies the annotated tag, exact source
commit, exact five legacy or seven current files, `release.json`, checksums and
the binary self-check, then creates the missing GitHub release or uploads only missing
byte-identical attachments. It never moves a tag or the asset ref, overwrites an
existing attachment, rebuilds over an existing asset ref, or accepts mismatched
bytes. An existing GitHub release without its immutable asset ref refuses repair
by rebuild. No workflow step connects to the paper host or activates a session.
For an off-GitHub fallback, use the manual
[build/publish steps](OPERATIONS.md#release-build-and-publish-manual-fallback),
never race two publishers for one tag.

Published tags never move. A fix is the next patch version. A tag does not
authorize a paper or live session.

## Tools

| Command | Purpose |
| --- | --- |
| `nivra-tools check-release` | Changelog/version and Markdown link check |
| `nivra-tools release-artifact` | Assemble the release assets on a development host |
| `nivra-tools golden-check` | Verify frozen fixture bytes |
| `nivra-tools replay` / `replay-fingerprint` | Offline shared replay ([shared replay](reference/SHARED_REPLAY.md)) |
| `nivra-tools watch-progress` | Summarize roadmap link and recent commits |
| `nivra self-check --expected-version vX.Y.Z` | Quick offline binary check |
| `nivra setup --help` | Explicit fresh current-policy paper preparation; never activates |
| `nivra backtest` | Offline bounded quote backtest |
| `nivra history-download` | Resumable read-only candle evidence ([usage](HISTORY_DOWNLOAD.md)) |
