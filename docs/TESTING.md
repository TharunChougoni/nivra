# Testing

Keep the suite lean and behavioral. Test what a caller can observe, inside an
existing scenario when you can, and avoid duplicate micro-tests.

## Rules

- The Rust suite has a ceiling of 400 `#[test]` and `#[tokio::test]` functions,
  ignored ones included. `nivra-tools test-budget` enforces it. To add a test at
  the ceiling, merge or remove one first. Never raise the limit.
- Tests never open sockets, use credentials or call a real provider or model.
  Use fakes, fixture transports and injectable clocks.
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
