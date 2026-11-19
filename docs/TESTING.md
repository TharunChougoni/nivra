# Testing

Keep the suite lean and behavioral. Prefer scenarios inside existing contracts
over duplicate micro-tests; fixed historical test counts are not a quality gate.
The suite has a hard budget of **400 Rust `#[test]` functions** (including
ignored measurements), enforced by `nivra-tools test-budget` in the local gate.
Merge or remove tests before adding one; do not raise the limit to land a test.
Tests never open sockets, use credentials or perform real provider/model calls.
Use fakes, fixture transports and injectable clocks. Heavy checks run on a
development host, one at a time; see [build performance](BUILD_PERFORMANCE.md)
for the fast local gate and memory guidance.

The local gate also checks **2** frozen golden files
(`nivra-tools golden-check`) and the dashboard suite. `nivra-tools test-budget`
reports the current Rust count and the ceiling separately. A lower count passes;
do not add tests to reach the ceiling.

## Parallel isolation

Every Rust fixture owns a unique `tempfile::tempdir()` (or an explicitly unique
per-process path); never use a fixed `/tmp` or `/var/tmp` name, a process-global
environment change or a shared lock file as test coordination. Advisory locks in
fixture paths use `nivra_cli::lock::AdvisoryLock`: it unlocks
explicitly before closing, so a child forked by an unrelated test cannot keep
the lock held after the owner drops it. A test that needs contention holds its
own guard and then proves the lock is released, including while a cloned
descriptor models the inherited-description window; do not retry around an
unexplained `*_locked` result or serialize the suite to hide a lock leak.
The application grant's `FileOwner` also explicitly unlocks on drop; its
regression fixture retains a cloned description while reacquiring ownership.

## Dashboard workspace

The React workspace tests in `web/v2/src/` cover financial unknowns, original
chart clocks/gaps, receipt-series gaps and tick-count/volume separation,
price-batch sequence resets and gap flags, watermark reconciliation,
extrema-preserving downsampling, stale cache fallback, Digest and Orchestrator
identities, hash routing, authentication, late-response lock privacy, eight-section navigation
and archive selection. jsdom, the Node runner and `tsc --noEmit` need no
browser, credentials or real sockets.
Use Node 24 and the pinned lockfile:

```sh
npm ci --prefix rust/nivra-cli/src/runtime/web/v2
npm run build --prefix rust/nivra-cli/src/runtime/web/v2
npm test --prefix rust/nivra-cli/src/runtime/web/v2
```

The command typechecks, runs the suite and verifies the committed bundle matches
its sources. Rust asset tests verify MIME, cache/ETag, gzip and generated asset
hashes; application-source tests reject raw HTML injection/evaluation. React's
internal DOM implementation is not mistaken for application HTML injection.
Manual browser fixtures qualify layout/interaction only, not market acceptance.

## Coverage map

| Area | Behaviour and source |
| --- | --- |
| Finance/execution | `nivra-core/tests`, runtime execution contracts: exact costs/rounding, cash/held units, tick/lot identity, atomic fills and ≤2-second original receipts |
| Recovery/storage | Store journals and runtime controller/binding contracts: fencing, crash boundaries, stopped/halted persistence, dated pointers and carry seals |
| Models/planner | Existing planner/harness contracts: strict JSON/tools, full evidence, event allowance accounting, failed charges, missing/replaced namespaces, cancellation and plan supersession |
| Continuous discovery | Planner/replay scenarios: fresh factual digests, durable owner work, invalid evidence refusal, configured allowances and dated config hashes |
| Failure/recovery | Event no-trade on timeout; durable halt reason survives stop; reviewed flat-book recovery, unrelated halt refusal, no ambiguous replay/refund |
| Grants | Model fixtures: exact identity/origin, fenced refresh, expired/mismatched refusal, live-catalog admission |
| Data/history | Parser/download/preparation fixtures: source times, malformed rows, immutable receipts, missing bars, original shared quota and partial coverage |
| Dashboard | Runtime web/history tests: bearer/Host/method/origin checks, bounded inputs, redaction, identity/hash failures and no trading controls |
| Delivery | Updater/preflight tests: immutable tags/checksums, HOLD/blackout/book admission, failed health rollback and interrupted transaction |
| Research | `nivra-research` contracts: registration hashes, leases, malicious output, cancellation/uncertain-worker recovery; no unsandboxed fallback |
| Full-day replay | Virtual 09:15–15:30 digest flow, no owner inference or fills, cash reconciliation, timeout no-halt, future receipt refusal, original inputs unchanged |

Frozen golden vectors remain for finance/market boundaries; do not
refresh them to hide a changed result.

## Run and interpret

Run the full [local gate](DEVELOPMENT.md#local-gate), including release link checks.
Some private-file fixtures need writable `/var/tmp`; a sandbox denial is an
environment failure, not grounds to relax file ownership/privacy validation.
Fixture provenance is in [fixtures](../fixtures/README.md).

For the scripted pi decision and research round trip, use
`nivra agent-check` ([pi agent lanes](AGENT.md#prepare-and-verify)). Scripted
model turns test the lane plumbing; they do not measure reasoning quality.
Daily bars do not establish executable quote depth or intrabar order.

Provider inference/search/refresh, actual sandbox containment, market execution,
resource latency and recovery drills require separately recorded genuine evidence.
The suite cannot certify profitability or unattended operation. Future work and
qualification are tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).
