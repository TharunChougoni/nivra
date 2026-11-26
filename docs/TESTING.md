# Testing

Keep the suite lean and behavioral. Test what a caller can observe, inside an
existing scenario when you can, and avoid duplicate micro-tests.

## Rules

- `nivra-tools test-budget` caps the `#[test]` and `#[tokio::test]` count,
  ignored ones included. Raise the limit when a change needs more tests.
- Default tests never use credentials or call a real provider or model. Use
  fakes, fixture transports and injectable clocks.
- Integration tests may talk to a local service, such as a Postgres container.
  Mark them `#[ignore]` so `cargo test` stays self-contained, and run them with
  `cargo test -- --ignored`.
- Each Rust fixture owns a unique `tempfile::tempdir()`. Never coordinate tests
  through a fixed `/tmp` path, a process-wide environment change or a shared
  lock file.
- Take advisory locks in fixtures with `nivra_cli::lock::AdvisoryLock`. If a
  test returns an unexplained `*_locked` result, find the leak. Do not retry or
  serialize the suite to hide it.
- Do not refresh the frozen golden vectors in `fixtures/rust/` to hide a
  changed result. `nivra-tools golden-check` verifies their bytes.

Run the whole [local gate](DEVELOPMENT.md) before you commit. Fixture sources
are in [fixtures](../fixtures/README.md).

## Dashboard client

The React tests run under Node 24 with jsdom, the Node test runner and
`tsc --noEmit`. They need no browser:

```sh
npm ci --prefix rust/nivra-cli/src/runtime/web/v2
npm test --prefix rust/nivra-cli/src/runtime/web/v2
```

`npm test` typechecks, runs the suite and checks that the committed bundle
matches its source. Rust asset tests check MIME types, caching, gzip and
asset hashes.

## Coverage map

| Area | What the tests prove | Where |
| --- | --- | --- |
| Finance and execution | Exact costs and rounding, cash and held units, tick and lot identity, atomic fills, 2-second receipts | `nivra-core/tests`, runtime execution tests |
| Storage and recovery | Writer fencing, crash boundaries, stopped and halted state, dated pointers, carry seals | `nivra-store`, runtime controller tests |
| pi lanes and planner | Strict JSON and tools, evidence checks, failed calls, cancellation, plan supersession | `runtime/agent`, `runtime/planner`, `runtime/harness` |
| Data and history | Source times, malformed rows, immutable receipts, missing bars, partial coverage | Parser, download and preparation fixtures |
| Dashboard | Bearer, Host, method and origin checks, bounded inputs, redaction, no trading controls | Runtime web tests and `web/v2/src` |
| Delivery | Immutable tags and checksums, update admission, rollback on failed health | Updater and preflight tests |
| Research | Registration hashes, leases, hostile output, cancellation, no unsandboxed fallback | `nivra-research` |

## Limits

`nivra agent-check` runs a scripted pi decision and research round trip. See
[pi agent lanes](AGENT.md). Scripted turns test the lane plumbing, not
reasoning quality. Daily bars do not show executable depth or intrabar order.
Real provider calls, sandbox containment on a host and market execution need
separately recorded evidence.

### Market archive integration tests

Start a disposable TimescaleDB container and run both ignored tests. The
URL and password in these commands are for the disposable container.

```sh
docker run -d --name nivra-archive-d -p 55433:5432 -e POSTGRES_PASSWORD=test timescale/timescaledb:latest-pg17
docker exec nivra-archive-d pg_isready
export NIVRA_ARCHIVE_TEST_URL=postgresql://postgres:test@127.0.0.1:55433/postgres
cargo test -p nivra-store --test market_archive --locked -- --ignored --nocapture
cargo test -p nivra-cli --bin nivra --locked archive_capture_replays_the_same_session_as_sqlite -- --ignored --nocapture
docker rm -f nivra-archive-d
```

Set `NIVRA_ARCHIVE_SIZE_REPLAY=1` for the CLI test to replay 4,377 synthetic
batches and print the two checkpointed book sizes.
