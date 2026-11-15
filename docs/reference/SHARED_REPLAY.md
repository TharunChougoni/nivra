# Shared quote replay

`sadr-core::historical` streams explicit quote events through the shared paper
execution/fee engine. `sadr-rs` owns data collection, original journals and ledger
commit; neither models nor network calls belong inside the financial engine.

```sh
target/debug/sadr-tools replay-fingerprint
target/debug/sadr-tools replay --manifest FILE --events FILE
```

Manifest: session/warmup, policy, development/training split, virtual historical
clock, seed, dated universe and exact engine/strategy/data/calendar/fee/slippage
bindings. Event NDJSON: chronological `at`, `available_at`, purpose
`protection|screen|entry`, and bounded quote observations with original source/
receipt times, depth, circuit limits and volume. Unknown identities, future
availability, invalid grids and mismatched exact hashes refuse.

The tool hashes while streaming and withholds the report on mismatch. Output
includes fills/fees/turnover, cash, flat realised P&L, drawdown and open exposure;
no missing closing quote is replaced by an invented fill. Engine fingerprints
are exact; old manifests are preserved, not silently rebound to a newer build.

This lower-level replay is distinct from [full-day model-led paper replay](../PAPER_REPLAY.md),
which also drives scanner/Luna/Grok orchestration using an explicit transcript.
Neither synthetic replay nor a declared provenance hash certifies a historical
strategy. Virtual receipts cannot measure real financial latency. All remaining
data/methodology qualification is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).
