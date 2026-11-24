# Strategy sandbox

The strategy laboratory is paper-only and opt-in. It evaluates rankings, recorded
marks and explicit simulated action schedules; it does not place orders, acquire data, change a running configuration,
restart a book or promote a strategy automatically. Existing configurations retain
their scanner behaviour when strategy settings are absent or disabled.

## Executable action-tape replay

`nivra strategy-lab replay-actions --source entries.json --parameters parameters.json
--manifest capture-manifest.json --events capture-events.jsonl --output new-report.json`
simulates an explicit schedule independently of the trading book. The source is an
ordered JSON array of `{at_ms, key, units, max_price}` entries: integer whole-share
sizes and exact decimal price strings, using identifiers from the bound capture.
Each accepted entry has the holding period selected in the parameters and attempts
its exit only on a real executable quote at or after that horizon, within the
declared exit delay. Overlapping entries share cash and available recorded depth.
Repeating a quote receipt does not replenish depth. Entry/protection observations
can supply quotes; screening observations cannot acquire execution authority.

The parameter object requires `version:1`, `parent_candidate_id` (hash or null),
`hypothesis`, `training_from_ms`, `training_to_ms`, `selection_from_ms`,
`selection_to_ms`, `holding_ms`, `max_exit_delay_ms`, `capital`, and `costs`.
Times are Unix milliseconds. The training window must precede the selection window.
Costs use the shared exact-decimal paper policy (`per_order`, `buy_bps`, `sell_bps`,
`slippage_bps`, `rounding_increment`); none are inferred from the capture or a broker.
All timestamps and original receipt/source times retain their archived values.
Shared execution enforces fresh quotes within two seconds, cash including fees,
depth, whole shares, ticks and the normal session execution window.

The new report binds exact source/parameter/manifest/event bytes, the action ABI,
engine and replay adapter. It retains fills, cash, residual holdings, fees, turnover,
entry rejections and unmatured/missing exits. Net P&L is reported only if flat;
marked equity is unknown when any held mark is stale or absent. No later quote
fills a missing horizon gap. Existing report files are never replaced.

This is an executable simulation of the declared schedule, rather than a ranking
return. It does not establish when the author selected that schedule, prospective
no-lookahead authorship, independent holdout, walk-forward or multiple-testing
qualification. Binding the report into `nivra experiment-lifecycle` records that
scientific path separately; a replay report alone remains `insufficient_evidence`
and cannot promote or activate a strategy.

## Experiment lifecycle

`nivra experiment-lifecycle --book <paper-book> --request request.json --output new-receipt.json`
records an immutable strategy specification and explicit transitions. The request
names one action: `admit`, `ingest_workspace`, `record_abi`, `record_replay`,
`open_shadow`, `mature`, `evaluate`, `select_paper`, `retire` or `show`.
Admission requires source and parameter hashes, feature ABI, hypothesis,
training and selection windows and a holding horizon. The training window must
end before selection begins. Known ABIs are `cost_aware_momentum_v1`,
`cash_quote_action_tape_v1` and `scorer_replay_v1`.

Forward marks mature only on original quote clocks whose receipt falls in
`[due_ms, due_ms + max_exit_delay_ms]`. Interpolated samples are refused.
`end_of_evidence_ms` can mark remaining holes as `unknown_gap`; a later clock
does not fill them. When every mark is matured or unknown, the host runs
leakage, out-of-sample completeness and unadjusted multiple-testing checks
(same source, parameters, training window and session). `eligible` is not an
edge claim; `paper_selected` records selection intent only.
`activation_authorized` stays false.

`ingest_workspace` reads a workspace scanner report through the same path
policy as `workspace_read`. The report must be advisory (`status: ok`,
`authority: advisory_scores_not_orders`, `execution_authorized` not true) and
its scores must be numeric. The seeded `scanner/cost_aware_momentum.py` report
enters as `cost_aware_momentum_v1` at `sandbox_abi`. Those candidates appear on
the dashboard Library projection without becoming trade authority.

Existing report and receipt files are never replaced.

## Tier 1: deterministic composition

`strategy-lab default-spec` prints the versioned `legacy_scanner` composition:
absolute sampled movement ranking, ₹10 minimum price, ₹10 lakh traded value,
100 bps circuit distance, 10 names and an independent 200 bps wake threshold
with 900,000 ms cooldown. These are the recommended legacy settings; compare
the exported values with the actual discovery configuration before proposing a
replacement. Relative-market wake mode uses `absolute_market_relative_bps`.
Ranking membership and wake eligibility are retained separately. Quiet shortlisted
names do not wake the pi decision lane; missing trigger evidence cannot rearm a
signal.

Each spec has `lookback_samples` (1–20, default 20), computed independently from
the same original cache without changing it. These are irregular retained quote
samples, not a fabricated N-minute window. Exported feature snapshots carry the
same field; replay rejects a window mismatch. `export-features --lookback-samples`
selects that window explicitly. Streaming price and metadata use their separately
configured expiry gates and preserve the original receipts.

`discovery.strategy` selects up to 16 specs evaluated against the same scanner
cache. One configured `champion` feeds Luna; other specs retain shadow snapshots.
An enabled champion that has no eligible evidence does not silently use a legacy
ranking. Existing freshness and deterministic execution gates still apply.
Enabling a champion also requires `discovery.strategy_results_database` pointing
to its separate approved results ledger. Runtime approval checks open that ledger read-only and
require an approved exact spec hash, successful replay and at least
`discovery.strategy_min_shadow_days` (default 5, allowed 1–365). A Tier 2 source
approval cannot authorize the Tier 1 runtime champion.

```json
{
  "enabled": false,
  "champion": "example",
  "specs": [{
    "id": "example", "version": 1,
    "filters": [{"feature": "price", "op": "greater_equal", "threshold": 10}],
    "weights": [{"feature": "move_bps", "weight": 1}],
    "normalization": "none", "shortlist_size": 10,
    "trigger_score": 0, "cooldown_ms": 60000
  }]
}
```

This object is the value of `discovery.strategy`, not a complete application
configuration. Available features are `price`, `move_bps`, `market_relative_bps`,
`traded_value`, `circuit_distance_bps`, `volatility_bps`, `range_position`,
`absolute_move_bps`, `absolute_market_relative_bps`, `session_vwap` and
`minutes_since_open`, `best_bid`, `best_ask`, `best_bid_qty`, `best_ask_qty`,
`spread_fraction`, `book_imbalance` and `rel_volume_tod`.
They use retained scanner evidence; unavailable features exclude a row rather
than becoming zero. The market baseline uses the whole eligible fresh cache.
Session VWAP uses the optional positive finite Upstox full-quote `average_price`
with metadata freshness; minutes since open uses the original price receipt and
the existing IST session-open boundary (pre-open is missing). Both are omitted
from the default discovery compatibility output to preserve its exact ranking
contract. Optional full-quote top levels require positive price and quantity;
zero or missing levels stay absent. Spread is `(ask-bid)/mid` for uncrossed books;
book imbalance is `total_buy_quantity/(total_buy_quantity+total_sell_quantity)`
when both totals exist and their sum is positive. These features use metadata
freshness and are excluded from default discovery output. They supply screening
evidence only; executable quotes and their two-second gate are unchanged.
`rel_volume_tod` is a time-of-day proxy, with no per-minute history:
`volume / (avg_daily_volume * min(1, (minutes_since_open / session_minutes)^rel_volume_curve_exponent))`.
Configure `discovery.strategy.feature_settings` with serde defaults
`session_minutes: 375`, `rel_volume_curve_exponent: 0.7`, and
`rel_volume_min_minutes: 5`. Session length and exponent must be finite and
positive; the finite minimum must be between zero and session length. Elapsed
time uses the original price receipt. Missing inputs, invalid/nonpositive daily
average, stale/missing volume metadata, times below the minimum, or an invalid
denominator leave the feature absent; no zero-fill is applied. A recorded zero
volume can produce zero. The fraction caps at one at/after the configured close.
This optional feature is excluded from default discovery compatibility output.
Previous-close gap features are not yet implemented.

Filters support `greater_equal` and `less_equal`; scores are weighted sums with
`none` or `cross_sectional_rank` normalization. No expression evaluator or I/O
exists in Tier 1. Specs reject unknown fields/features, non-finite numbers,
invalid identities and out-of-bound parameters. Limits include 32 filters,
32 weights, shortlist 1–200, absolute weight at most 1e6, absolute threshold at
most 1e15, absolute trigger score at most 1e12, and cooldown 0–86,400,000 ms.

`discovery.strategy.admission_bounds` can tighten admission ceilings. Omitted
fields use these explicit defaults: `max_specs: 16`, `max_features: 9`,
`max_filters: 32`, `max_weights: 32`, `max_lookback_samples: 20`,
`max_shortlist_size: 200`, `max_cooldown_ms: 86400000`, `max_abs_weight: 1000000`,
`max_abs_filter_threshold: 1000000000000000`, `max_abs_trigger_score: 1000000000000`,
and `max_trigger_threshold: 1000000000000`. `max_features` counts distinct features
across filters, weights and the independent trigger. Counts must be positive;
cooldown and numeric magnitudes may be zero. Non-finite, negative, unknown or
above-supported-ceiling settings fail closed, including when strategies are
disabled. Every configured spec must fit the bounds. Default bounds are omitted
when serializing settings, preserving existing config hashes. These ceilings do
not add trading risk caps.

The library also provides `evaluate_default_scanner` for offline exact legacy
compatibility, with explicit universe keys and the serialized default composition
(shortlist, cooldown and screen thresholds may be configured). It retains
unavailable universe rows, ranks empty histories at zero, preserves legacy cached
metadata-reason and diagnostic-row admission, and applies the shared exact decimal
screen policy before floating-point ranking. It leaves cache receipts unchanged.
Other compositions fail closed. This API is not wired into runtime discovery;
the existing feature extraction APIs still enforce separate metadata expiry.

Every retained scan includes features, scores, selection and rejection reasons,
spec version/hash and a content-derived scan ID. Discovery retains unavailable
universe rows too. The champion attribution follows digest, decision and resulting
paper evidence; missing older attribution remains missing, not reconstructed by
inference. This does not rewrite original journals or sealed state.

## Tier 2: scores-only source

`strategy-scorer scores` evaluates one snapshot; `strategy-scorer replay-scorer`
evaluates timestamped snapshots. Both require an explicit SandboxConfig JSON file:

```json
{
  "enabled": false,
  "wall_millis": 1000,
  "cpu_seconds": 1,
  "memory_bytes": 134217728,
  "max_source_bytes": 65536,
  "max_input_bytes": 1048576,
  "max_output_bytes": 65536,
  "wasm_fuel": 1000000
}
```

Each flag maps to a bounded code-validated setting. Wall time is 1–30,000 ms,
CPU 1–30 seconds, memory 32–512 MiB, source/output at most 256 KiB, input at most
4 MiB, and fuel 1–100,000,000. Missing containment/runtime support fails closed.
Execution errors, timeouts and invalid output are failed ticks without fallback.

Example CLI shape (use operator-created, non-secret files):

```sh
nivra strategy-scorer scores --config limits.json --source scorer.py \
  --language python --input snapshot.json --scorer-id candidate --version 1 \
  --run trial-1 --observed-ms 1000 --database results.sqlite
```

A snapshot is a JSON object mapping instrument keys to finite numeric arrays;
array ordering is fixed by the caller's declared feature/history schema. Source
sees numeric snapshots, never provider clients or credentials. Inputs allow up
to 4,096 keys and 128 numbers per key, subject to byte limits. Output must contain
exactly the same keys and finite scores. It cannot emit orders. This ordered
numeric-row ABI has no params object, so `strategy-scorer` does not accept
`--paper-costs` and never infers a fee policy; use `scorer-compare` for
parameterized Python candidates.

Python is a restricted mathematical source subset with functions, arithmetic,
loops and approved pure builtins, not an unrestricted Python environment. A
Cargo-owned embedded worker validates its AST and installs a kernel syscall
allowlist before invoking untrusted code. Imports, attributes, arbitrary builtins,
filesystem/network access, clocks and randomness are unavailable. The launcher
clears the environment, isolates Python initialization, applies process resource
limits and enforces bounded pipes and wall timeout. The repository remains a
Cargo workspace; the worker is not a standalone Python project or toolchain setup.

WASM modules must have no imports, including WASI. The fixed `score` export
receives the requested key index, key count and all numeric rows in lexicographic
key order, each prefixed by its length; it returns f64. Thus cross-sectional
maths is possible without host callbacks. Fuel, linear memory, disabled threads,
process limits and a whole-batch deadline apply. The local runtime is
`/usr/bin/wasmtime`; absence is an unavailable failed tick. Host qualification of
the installed runtime and containment is separate from source tests.

Tier 2 is an offline scoring/replay path. It does not automatically become the
runtime champion. Source, language, hashes, configured limits, scores and failures
can be retained in the separate results database. The effective scorer definition
hash binds identity/version, language, executable source hash, limits and ABI.
Full source is stored as indexed hex chunks plus a manifest, including sources
whose escaped JSON or WASM hex would exceed one evidence row. Numeric score replay does not
establish fills, fees or trading profitability.

## Recorded-data replay and results

```sh
nivra strategy-lab init --database results.sqlite
nivra strategy-lab export-features --manifest manifest.json --events events.jsonl \
  --output features.json
nivra strategy-lab replay --spec spec.json --input features.json \
  --run replay-1 --database results.sqlite --config replay.json
```

Feature export accepts both the shared historical manifest (including its
engine/data bindings) and the application's actual paper-export schema (including
its original event hash and explicit universe). It validates chronology and
original receipts. Paper exports declaring unknown legacy screening purpose
refuse; no engine/calendar binding is fabricated for them. Only screening-purpose observations enter reconstructed scans.
`max_age_ms` defaults to 2,000; a new output file is required. Optional
`export-features --config archive.json` settings are `max_archive_bytes` (default
32 MiB, max 128 MiB), `max_observations` (default 100,000, max 1,000,000) and
nullable inclusive `from_ms`/`to_ms`. The full archive hash/chronology/receipts and
observation allowance are checked even for a window; earlier observations warm
the cache, while only selected-window screening snapshots are emitted. Replay accepts
1–10,000 strictly increasing snapshots and applies trigger/cooldown rules.
Optional ReplayConfig JSON defaults to `horizon_ms: 300000` and
`max_mark_delay_ms: 2000`, bounded to 1–86,400,000 and 0–60,000 respectively.
Forward returns use the first available recorded price for each selected key
at/after the horizon within the allowed delay, using only valid snapshots.
Optional `selection_from_ms` and `selection_to_ms` are inclusive bounds on which
snapshots may create selections. Omit both for the existing full-input behavior.
All input snapshots still evaluate and remain available for forward marks;
feature export retains original warmup history. Snapshots outside the selection
window do not fire or rearm signals. This lets an
opening-window experiment include a real recorded tail covering its horizon and
mark delay, without adding later picks. A selection window does not authorize
interpolation, stale prices or additional acquisition.

Each missing mark has exactly one reason in `missing_mark_reasons`. The sum equals
`missing_forward_marks`; older scorecards without this field remain unknown.
The CLI report, results scorecard, read-only HTML report and orchestrator replay
summary retain these counts:

| Reason | What the supplied input establishes |
| --- | --- |
| `initial_price_unavailable` | Selected row has no positive initial price |
| `horizon_overflow` | Target timestamp cannot be represented |
| `recording_ends_before_horizon` | Input ends before the selected row's horizon |
| `no_snapshot_in_window` | No recorded snapshot falls in the horizon/delay window |
| `only_failed_snapshots_in_window` | Recorded snapshots exist there but none evaluated successfully |
| `instrument_absent_in_window` | Valid snapshots exist there but none contains the selected key |
| `price_unavailable_in_window` | The key occurs there but has no positive price |
| `nonfinite_return` | The first available price gives an unrepresentable return |

Reasons distinguish recording coverage from evaluation failure. They do not
certify provider outages or recover missing quotes.
These are descriptive recorded marks, without executable fills. Invalid snapshots
cannot supply forward marks. Set `horizon_ms` to 900000 or 3600000 for separate
15-minute or one-hour experiments; each run retains its horizon and delay.
An optional `costs` object enables explicit independent fixed-notional analysis:

```json
{"horizon_ms":900000,"max_mark_delay_ms":2000,
 "costs":{"notional":"5000","round_trip_spread_bps":10,
          "fees":{"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"}}}
```

Replay uses the shared exact-decimal fee arithmetic on buy/sell notional and
subtracts the declared round-trip spread. `costs.fees` must be explicit. To stress
other assumptions, supply all rates as decimal strings:

```json
{"horizon_ms":900000,"max_mark_delay_ms":60000,
 "costs":{"notional":"5000","round_trip_spread_bps":10,
          "fees":{"per_order":"0.01","buy_bps":"5","sell_bps":"7",
                  "rounding_increment":"0.01"}}}
```

`rounding_increment: null` explicitly disables fee rounding. Negative fees,
floating-point financial values, nonpositive rounding increments, unknown fields
and arithmetic overflow refuse. Spread/price impact remains the separate
`round_trip_spread_bps` assumption; `fees` does not accept a second slippage field.
These settings affect offline marks only; they do not modify the paper account's
financial fee policy. Reports retain measured sample count, net expectancy,
hit rate, turnover, fees and cumulative return drawdown. These independent marks
are not an allocation simulation, executable fills or broker-certified costs.
Omitted costs remain explicitly absent. Exact specs are registered by canonical
hash and included in replay evidence, alongside exact dataset hashes.

Replay reports bind canonical spec and dataset hashes. Persistence atomically
records summary, individual scans, individual forward marks and a scorecard.
Duplicate IDs or invalid rows roll back the batch. Existing evidence is immutable.
The results DB is created/migrated by code with its own application identity;
foreign databases, including paper books, are rejected before schema changes.
No hand-edited SQLite or replacement journal is needed.

`strategy-lab import` is a trusted producer command for attributed evidence JSON,
not a model tool. Evidence kinds cover quotes/history, scans, digests, handoffs,
gates, decisions/failures, orders/fills, positions/trades, forward returns,
scorecards and replays. Order/fill/trade imports require scan, digest and decide
IDs. Recording a claim does not certify the underlying source.

## Orchestrator capabilities and lifecycle

The typed `PipelineTools` API takes operator-selected source/session paths and
preloaded recorded snapshots. Model requests contain no paths or SQL. LabConfig
JSON defaults are:

```json
{"enabled": false, "token_budget": 4096, "min_shadow_days": 5, "read_sources": false}
```

Budget is bounded to 1–1,000,000 and minimum shadow days to 1–365. The enabled
flag authorizes laboratory tools, never a trading session. `read_sources` must
also be enabled for source journals. The gateway supports only `read_results`, `read_registry`,
`read_pipeline`, `replay_spec`, `propose` and `note`, encoded as
`{"tool":"note","input":"experiment rationale"}`. It has no approval,
qualification, order, configuration, SQL or general file-write tools. It is a
local tool harness; no autonomous orchestrator model/provider is started here.

Results queries require `kind`, `from_ms`, `to_ms`, `after_id` (nullable) and
`limit` (1–100); optional `spec_id` and `instrument_key` narrow evidence.
Registry queries use `kind` (`specs`, `proposals` or `notes`), nullable `after_id` and
`limit` (1–100). Proposal reads retain source, lineage and qualification evidence;
notes are restricted to the gateway's own run. They share the output budget.
Source queries use `kind`, `from_ms`, `to_ms`, `after_rowid` (default 0), `limit`
and optional `instrument_key`/`audit_id`. Categories include quotes, scanner,
portfolio, events, attempts, plans, actions and allowlisted model-audit evidence.
Reads open original sources read-only, use fixed bounded queries and paginate.
Absent/legacy evidence is labeled; the gateway does not invent a complete quote
history or exact attribution for older sessions. Fill records preserve fees and
decision IDs, but do not independently bind an exact quote archive receipt. The
source reader therefore does not invent per-fill slippage or per-lot realized P&L
by joining timestamps or applying an undeclared lot-allocation policy.

All tool output shares one per-run conservative byte-based token allowance,
including summarized replay results and proposal/note receipts. This is an upper
bound accounting convention, not provider tokenizer measurement. Exhaustion
rejects further results rather than silently extending the allowance.

A local transcript exercises the same shared-budget gateway without a provider:

```sh
nivra strategy-lab tools --config lab.json --database results.sqlite \
  --run research-1 --requests requests.json --session archived-session \
  --recorded features.json --replay-config replay.json
```

`requests.json` is a JSON array of 1–100 typed tool requests. Session, recorded
data and replay settings are operator-selected optional inputs. Replay tools
return summaries rather than expanding all scans into the model budget. The
standalone `query`, `source`, `propose` and `note` CLI commands are manual offline
utilities; they do not create an autonomous model loop.

Proposals store a validated spec, canonical source hash, rationale, parent ID
and optional Python/WASM source/hash. Python source is UTF-8 text; WASM source is canonical lowercase hex of a binary module beginning with the WebAssembly v1 header. Scorer SHA-256 always hashes the decoded executable bytes, matching scorer CLI provenance. Parent lineage must refer to an existing
proposal. Notes append under the gateway's run ID. Trusted qualification APIs
record replay success and measured shadow days. Approval requires the named owner
approver (`Tharun` is the literal value recorded by the current ledger schema),
an explicit approval reference, between-session confirmation, a successful replay
and sufficient configured shadow days. Runtime validation checks that ledger
before accepting an enabled Tier 1 champion configuration. The API records
approval evidence only;
it does not activate a session, edit deployment config or promote automatically.
Source tests do not constitute owner approval or measured live shadow days.

## Trusted qualification and read-only reports

`strategy-lab report --database results.sqlite --output strategy-report.html`
exports a bounded read-only leaderboard, scan report cards and approval history.
The file has no promotion or trading controls. Its data remains a projection of
retained evidence, including missing marks and declared cost assumptions.

Model tools cannot qualify or approve. Explicit operator commands are:

```sh
nivra strategy-lab qualify --database results.sqlite --input qualification.json
nivra strategy-lab approve --database results.sqlite --proposal candidate-1 \
  --approval-ref owner-review-reference --between-sessions --config lab.json
```

The approval command records the owner's supplied approval reference; invoking it
requires that real approval. It does not obtain approval, edit configuration or
activate a champion. `LabConfig.promotion` defaults to:

```json
{"min_samples":100,"min_out_of_sample":30,
 "min_improvement_bps":5,"max_parameter_change":0.25}
```

Qualification input contains `proposal_id`, `replay_passed`, `shadow_days` and a
`qualification` object with `id`, `replay_id`, full `champion` spec,
`sample_count`, `out_of_sample_count`, `training_end_ms`, `holdout_start_ms`,
`holdout_end_ms`, `candidate_return_bps` and `champion_return_bps`. Training must
end before holdout starts. The replay reference must bind the exact candidate
spec hash with zero failed ticks. Parameter change compares numeric fields using
`abs(a-b)/max(abs(a),abs(b),1)`; a structural change counts as 1. ID/version do not
count as parameter changes. The chosen bounded policy is stored with approval
and rechecked at runtime.

These metrics and measured shadow days are trusted evaluator attestations, not
values the model may write or independently certified measurements. Approval and
qualification history append; schema v2 preserves v1 evidence and approvals.
Legacy approvals without guardrail policy evidence cannot admit a new champion.
Original runtime journals remain the source of truth. With strategy settings
enabled, a deterministic producer records every rank tick before model admission:
champion and shadow snapshots include rejected/unavailable rows, exact spec hashes,
ordered hashed chunks and the full reconstructed scan hash. It also stores sampled
quote-cache history with original price/metadata receipts. This is a rank-tick
cache history, not a reconstruction of every intervening WebSocket message or
executable depth. Disabled strategy settings perform no producer I/O.

`discovery.strategy_results.limits` defaults to `max_scan_rows: 20000`,
`chunk_bytes: 196608`, `max_batch_bytes: 67108864`; validated maxima are 20,000
rows, 256 KiB per chunk and 128 MiB per batch. Oversize input or a persistence
failure fails closed before another scan-trigger model enqueue. Trusted explicit
imports remain available for other archived evidence.

The runtime producer also appends exact frozen model-input scan manifests/chunks,
verified digest artifact references, owner decision references, attributed fills
and changing position snapshots. Historical handoff and gate artifacts retain
their original evidence kinds when read from an archive. Identical already-recorded facts are verified;
conflicting IDs fail closed without overwriting evidence. A post-execution ledger
failure preserves the committed financial fill and prevents subsequent model work.
Older or carried evidence without the required lineage is explicitly unknown.
Raw provider/configuration/account envelopes are excluded. Detailed digest/decision
output remains in original audit artifacts, accessible through read-only tools.

Default runtime discovery uses the built-in strategy spec and the feature library's
compatibility evaluator, preserving the existing scanner JSON and ranking. Runtime
screen thresholds retain their exact decimal policy values; metadata receipts and
diagnostic handling retain scanner compatibility semantics. Invalid configured
spec admission falls back to the built-in default. Golden fixtures compare the
complete serialized runtime projection against a test-only legacy oracle.

## Python file sandbox (offline library)

`nivra-sandbox` is a CLI-free, Python-only API for replay/evaluation. It does not
change the older Tier 2 commands above; runtime selection is owned by the
config-gated discovery path below. `score(config, scorer_path, features)` accepts
a JSON object keyed by instrument and returns a subset of those keys with finite
f64 scores. `score_source(config, source_bytes, features)` runs the same sandbox
from bytes already read by the caller (the live-ranking path after SHA lookup),
while `score` keeps the existing file/path ABI. The scorer
runs as a file with `-I -S -B`; `sys.argv[1]` names `/input.json`. It must print
one strict JSON object to stdout. Duplicate/unknown keys, NaN/infinity, nonnumeric
values, trailing JSON and excessive output are rejected. An empty subset is valid.

`score_or_default(config, scorer_path, feature_snapshot)` validates the snapshot
with the core built-in spec and computes its default ranking. On any scorer
failure it returns `Ranking::Default` and a typed `Failure` reason (`config`,
`input`, `setup`, `unavailable`, `exit`, `wall_timeout`, `output_limit`, or
`output_invalid`). Successful candidates return `Ranking::Candidate` scores;
callers retain their own selection policy. Invalid default inputs return an
explicit error rather than manufacturing a ranking. Exit covers sandbox setup
refusal and resource termination; raw stderr and host paths are never returned.

The standalone `ScorerSandboxConfig` section has these explicit defaults:

```json
{"python_path":"/usr/bin/python3","bwrap_path":"/usr/bin/bwrap",
 "cpu_seconds":1,"memory_mb":128,"wall_ms":2000,
 "max_output_bytes":65536,"max_keys":4096,
 "max_processes":64,"max_open_files":32}
```

Paths must be absolute. Bounds are CPU 1–30 seconds, memory 32–512 MiB,
wall 1–30000 ms, output 1–262144 bytes, keys 1–4096, processes 8–256 and
open descriptors 16–128. Unknown fields refuse. The process ceiling is currently
enforced more strictly: the payload cannot create any process or thread. This
single-process bound is independent of the host user's existing processes.
These settings are separate from deployed session config, so no
on-disk migration or runtime enablement is implied. Source is snapshotted up to
256 KiB and input up to 4 MiB; keys are nonempty and at most 512 bytes.

Linux x86_64 containment uses bwrap `--unshare-all` (including network),
`--clearenv`, `--die-with-parent`, `--new-session` and dropped capabilities.
The network namespace stays unshared unless the caller sets the explicit
`network` opt-in on the sandbox configuration; only the reviewed orchestrator
workspace setting does so, and the post-close scorer trigger refuses it.
That opt-in adds `--share-net` plus read-only host resolver/CA mounts and never
mounts the host root, home, credentials or the trading book.
Only fixed PATH/LANG survive; on merged-library hosts a fixed `LD_LIBRARY_PATH`
points to the existing read-only `/usr/lib64` mount. Read-only mounts contain the selected interpreter,
installed `/usr/lib/python3.*` stdlibs, system dynamic-library directories,
snapshotted scorer and input. `/proc` and the root mount are read-only; there is
no home, host root, host temporary directory, device mount or writable workspace.
Use an operator-trusted system Python installation; custom layouts lacking these
libraries fail closed. A payload seccomp filter additionally denies fork, vfork,
clone and clone3 (including alternate syscall ABI refusal). Resource limits are
installed in the child `pre_exec` before bwrap: CPU, address space,
file size zero and descriptor count. RLIMIT_NPROC is deliberately omitted: its
per-real-UID accounting can prevent bwrap setup forks on a busy host, and does
not reliably constrain root. The seccomp filter is installed after setup forks.
This small launcher crate contains the
reviewable libc unsafe boundary; the remaining workspace keeps its unsafe ban.
Nonblocking bounded stdout and a wall deadline kill the process group and
namespace supervisor, then reap it. Stderr is discarded. Missing bwrap, missing
Python, unsupported architecture or failed setup never invokes Python directly.

The behavioral tests exercise real denied network attempts (external and local),
parent fake-secret removal, hidden home/parent temporary files, writes to cwd,
`/tmp`, source and input, configured limits, bounded fork attempts, CPU exhaustion,
memory exhaustion, wall timeout, and rejected outputs with default-ranking parity.
Containment tests require installed, usable bwrap and fail if setup is broken.
The missing-bwrap fallback test always runs.
These isolated network-denial probes are the explicit sandbox exception to the usual
no-socket fixture rule; no providers or credentials are involved.

## Offline strategy source library

`nivra_sandbox::library::Library` stores source files and evaluation results in a
separate SQLite file, never the trading book or existing approval/results ledger.
It has opt-in shadow diagnostics and config-gated live paper ranking (see below). `open(path)` uses default `LibraryLimits`;
`open_with_limits(path, limits)` accepts a serde config object with
`max_body_bytes` (262,144 bytes) and `max_metrics_bytes` (65,536 bytes).
Missing fields use these defaults; zero limits are rejected.

`add_version(NewVersion)` hashes exact UTF-8 file bytes with SHA-256 and appends
per-name versions starting at 1, including model, intent, JSON inputs and a
caller-supplied creation timestamp in milliseconds. Repeating a hash returns the
original version and metadata; reusing it under another name is rejected.
`versions(name)` returns version order. `version_by_sha(sha)` reads exactly one
version, including its strategy name, under the body/metadata limits; missing,
duplicate/ambiguous, non-UTF-8, oversized or hash-mismatched rows are rejected.
`open_read_only` never initializes or migrates and is the live-ranking read path.
`record_result(RunResult)` appends replay,
shadow or paper metrics for a known hash and a valid YYYY-MM-DD session date,
optional regime and caller-supplied recording timestamp. Metrics must contain
nonnegative integer `trades` and finite numeric `net_pnl_after_costs`, `expectancy`
and `max_drawdown`. `results(sha, run_kind, regime)` returns insertion order with
optional exact filters (`None` means no filter, including null regimes).

`record_regime_result(RegimeResult)` separately appends exact strategy id/version,
canonical `nivra_core::regime::Regime::label()`, replay/shadow/paper source, positive
sample count, the same metrics object and recording timestamp. The existing
`max_metrics_bytes` limit also applies. `regime_performance()` groups by strategy
id/version, regime and source: it sums samples, trades and net P&L, weights
expectancy by sample count, and takes the largest recorded batch drawdown (not
a reconstructed equity-curve drawdown). This API has no runtime wiring.

Schema migration 2 is tracked with `PRAGMA user_version` and a library-specific
application ID. Unrecognized, nonempty or newer databases are refused. Foreign
keys bind results to versions; update/delete triggers enforce append-only tables.
`open()` migrates v1 libraries forward without changing existing rows.
`open_read_only()` accepts v1 and v2 without migration; v1 regime queries return
no evidence. Unknown versions are rejected by both openers.
There is no update/delete API, strategy activation, model call or promotion.

## Offline evidence selector

`nivra_sandbox::selector::select` is a pure function over library results, regime,
health and caller-owned state. It never activates a strategy. IDs are strategy
hashes, with `default` reserved for fallback. Only exact regime matches count;
malformed metrics are ignored. Distinct dates count as sessions; expectancy is
the unweighted mean of session means, and the recent paper floor checks each
of the latest session means. Ranking uses shadow expectancy, total session count,
then ascending hash. Inputs may be supplied in any order.

Serde-defaulted `SelectorConfig` requires validation: defaults are 5 replay and
5 shadow sessions, 30 trades, strictly positive overall expectancy, a zero floor
for each of the last 3 paper sessions, 1,800,000 ms cooldown and 0.0005 switch
margin. Stale data, scorer failure and active paper floor breaches immediately
choose the default. Unknown regimes and an active non-default strategy lacking
eligible evidence for the current regime also immediately choose the default,
even when another candidate is eligible. Default fallback always bypasses cooldown
and hysteresis. Switching to a non-default strategy obeys cooldown and must exceed
the active shadow expectancy by more than the margin when that score exists.
The returned state records only actual switches; callers retain it explicitly.

## Strategy selection: shadow diagnostics and live paper ranking

Optional discovery settings (also shown in the scanner discovery fragment):

```json
"strategy_selection": {
  "enabled": false,
  "mode": "shadow",
  "library_path": "",
  "evaluate_ms": 60000,
  "regime": {},
  "selector": {}
}
```

An omitted block or `enabled: false` performs no library I/O or selection.
`mode` is `shadow` (default, and what old configs that omit the field get) or
`live`. Enabling requires a nonempty library path; the interval must be positive
and both nested configurations must pass their documented validation. Empty nested
objects use the classifier and selector defaults above. Live mode additionally
requires `enabled: true` and refuses to load with an enabled Tier 1
`discovery.strategy` champion, because that champion already owns ranking.

At most once per interval during the existing rank step, the observer classifies
available scanner samples and reads all library versions/results. REST OHLC
close/open references retain their original receipt; missing references,
previous-day references and stale current samples are skipped. Discovery's optional `feature_store` block uses module defaults when omitted:
`{"max_bars":375,"min_instruments":200}`. `max_bars` must be at least two and
`min_instruments` positive. One in-memory store resets with each session; accepted
LTPC points and REST scan prices retain their original price receipts, never the
later processing clock. It retains receipt-minute IST OHLC bars, including the
current bar within the cap; missing minutes remain missing. Completed consecutive
minute returns supply cross-sectional medians to shadow regime inputs, requiring
at least `min_instruments` contributors per minute. Thin evidence still yields
Unknown under the classifier's configured minimums. No opening price or minute
data is invented.

Discovery status includes `feature_store` with `instruments`, `bars_total`,
`ignored_ticks` (older-bar observations), `duplicate_ticks` (receipts equal to or
older than that instrument's last observed receipt, ignored without changing bars),
and `completed_minutes` (retained market
return minutes meeting the contributor minimum). No store history is persisted
or reconstructed on restart.
`Library::open` can create/initialize the library schema for shadow diagnostics;
the observer never adds versions or results. Any open/read error sets
`scorer_failure` and selects default.

In **shadow** mode the observer only records selection. Switches and reason
changes emit a `strategy_selection` log line. It never executes a scorer, changes
ranking, candidates, model evidence or orders. Selector state persists in memory
between evaluations and resets on process restart/session rollover.

In **live** mode (paper only), when the selector holds a non-default active
candidate (a `BestEvidence` choice, or cooldown/hysteresis retaining that
candidate), the rank step reads the exact library source by SHA with
`Library::open_read_only` and `version_by_sha`. A missing, oversized, non-UTF-8,
ambiguous, non-library or hash-mismatched source fails closed; live mode never
creates or migrates the library. The current fresh scanner feature snapshot is
sent to the existing bwrap/seccomp scorer sandbox from those source bytes, with
library `inputs` supplied under the reserved top-level `params` key. Callers
cannot substitute a scorer path between lookup and execution.

A live score output must be a nonempty subset of the input identities with finite
values. Scores rank the scanner prospects descending, with ascending instrument
key as the deterministic tie-break. Unscored or rejected prospects are omitted,
then the existing presentation/shortlist limits apply. The resulting prospects
feed the same scanner candidates, trigger and depth-shortlist paths as the default
ranking. They do **not** create orders, bypass validation or risk controls, or
replace the owner's decision. `LIVE_ENABLED` remains false; this is paper ranking only.

For an explicitly reviewed paper trial, set `paper_trial_sha256` in the enabled
live selection block. The operator chooses that exact library version through
the reviewed planner policy. This path tests a single ranker without waiting
for the evidence-based selector to pick a candidate. It uses the same sandbox,
fresh scanner inputs, score validation and fallback. Registry writes alone do
not select a new version. The [research ranker workflow](AGENT.md#review-a-research-ranker-for-paper-scanning)
describes how to register a script and bind its hash.

Every live path falls back to the default ranking and logs
`strategy_selection_fallback` with a stable reason/detail when the selector
returns `UnknownRegime`, `StaleData`, `ScorerFailure`, `NoEligibleCandidates`,
`PaperFloorBreach`, `InvalidConfig` or the default identity, and when live
evidence is unavailable: missing library or source, source too large/invalid,
any sandbox failure (config, input, setup, unavailable, exit, wall timeout,
output limit, invalid output), invalid scores, or no scored prospect. Recovery
emits `strategy_selection_recovered`. Fallback lines include the attempted
identity (SHA/name), previous active, regime and reason, but never source, model
text or secrets.

The discovery status `strategy_selection` object contains the evaluation time,
`mode`, `shadow_only`, regime and sample counts, `chosen_id`, bounded
`chosen_name`, `previous_active`, `active`, selector `reason`, `switched`,
`live_applied`, `live_scored_count`, `fallback`, `fallback_count`, a bounded
`recent` switch/fallback list (at most eight events), `scorer_failure`,
`stale_data` and selector state. Shadow status remains additive-compatible with
the original fields.

Shadow and absent selector settings remain excluded from the trading-policy hash
exactly as before; an existing legacy hash stays valid for those configurations.
Live mode is included in the policy hash, including the serialized `mode`, so
enabling or disabling it changes policy bytes and requires the normal paper
policy review/`config-apply` path. Sealed receipts are not rewritten.

## Offline cost-aware momentum file

`strategies/cost_aware_momentum.py` is a stdlib-only candidate for the file
sandbox above; it is available through the opt-in shadow observer and live paper
ranking below. Supply a
reserved top-level `params` object alongside instrument-keyed feature objects.
Load all policy defaults from `strategies/cost_aware_momentum.params.json` into
that object before calling `score`; the sandbox does not mount adjacent files.
The `params` key cannot be an instrument identity. Output contains only eligible
instrument keys and finite scores, higher first; rejection is omission, not a
large negative score. Missing, null, nonfinite or invalid required features omit
that instrument, with no zero-fill or optional-feature bypass.

Required feature keys are `price`, `move_bps`, `market_relative_bps`,
`traded_value`, `circuit_distance_bps`, `volatility_bps`, `range_position`,
`best_bid`, `best_ask`, `best_bid_qty`, `best_ask_qty`, `spread_fraction`,
`book_imbalance`, `session_vwap`, `rel_volume_tod`, `minutes_since_open`.
These are the serialized core Feature names. Both top quantities and imbalance
are required depth evidence; only ask notional gates depth in the research
formula. Relative volume is the prepared time-of-day proxy. Spread uses the
larger of the supplied fraction and the midpoint spread derived from top prices.
Market regime uses the median move of complete valid rows, including rows later
rejected by policy gates. Costs and expected movement are research assumptions,
not executable fills or a profitability claim.

The params JSON documents the complete default policy: price/value/circuit,
spread/depth, time/relative-volume/extension gates; order notional, per-order and
side fees, per-side slippage, edge margin, volatility capture and horizon;
component weights; weak-tape volatility multiple; normalization floor and term
caps. All fields are required, numeric, finite and nonnegative; notional,
normalization floor and session length must be positive, and opening plus closing
exclusions cannot exceed session length. Unknown params refuse. Unit constants
(10,000 bps per fraction, 100 bps per percentage point, two sides) and validity
bounds are mathematical definitions rather than policy tunables. The checked-in
fee and slippage values match the agreed paper policy: `per_order_fee=0.01`,
`buy_bps=5`, `sell_bps=7`, `slippage_bps=5`. `scorer-compare --paper-costs` can
replace or confirm those four values from an explicit runtime-schema file. The
synthetic Rust fixture exercises actual sandbox execution, leader ordering,
down-mover, wide-spread and missing-feature rejection, and an input-parameter
change.

## Shadow scorer diagnostics

Optional discovery configuration:

```json
"shadow_scorer": {
  "enabled": false,
  "shortlist_only": false,
  "scorer_path": "",
  "params_path": "",
  "evaluate_ms": 60000,
  "top_n": 10
}
```

Omission or disabled configuration performs no scorer/params reads or execution.
Enabled settings require nonempty paths, a positive interval and top_n from 1
through 4096. `discovery.scorer_sandbox` accepts the validated
`ScorerSandboxConfig` limits documented above; omission uses those defaults.

At most once per interval in the rank step, the observer freezes the same dated
scanner cache and timestamp as live ranking, restricted to that universe. It
extracts core features using configured metadata age and feature settings (defaults
when absent), and supplies the params JSON object under reserved key `params`.
`shortlist_only` defaults to false. When true and the enabled prefilter produced
this rank step's shortlist, only those feature rows enter the sandbox (an empty
shortlist sends zero instrument rows). Otherwise the full snapshot is used.
Filtering follows full-universe feature extraction; reserved `params` is separate.
Params reads are bounded to 256 KiB; malformed input fails closed. Only the
existing sandbox runner executes the scorer, with configured limits, on a background
thread. At most one evaluation is in flight; rank steps skip launches while it
runs. Each runtime loop polls for completion without waiting.

Dashboard discovery status gains `shadow_scorer` with `as_of_ms`, descending
`top` entries (`instrument_key`, `score`, ties by key), and `ineligible_count`
(universe size minus returned scores, before top_n truncation). Sandbox/input
failure adds a typed `fallback_reason`, empty top and all instruments ineligible.
The `comparison` block contains `default_top` (up to `top_n` keys from the
built-in live default ranking at evaluation time, before shadow input filtering),
`overlap_top_n` (keys shared by shadow top and default top), and `input_rows`
(instrument feature rows supplied, excluding `params`; zero if extraction fails).
Failures retain this comparison with zero overlap and their `fallback_reason`.
One `shadow_scorer` log line records each completed evaluation, including top_n
keys; `as_of_ms` retains the input snapshot time. In-flight results are ignored
on session reset.
The last diagnostic persists between evaluations and resets on session rollover
or restart. Status attaches after model projections; scores never change live
ranking, candidates, model inputs or orders. Failures never replace live ranking.

## Shadow prefilter

`discovery.prefilter` defaults to disabled; the scanner packaging fragment lists
all defaults. When enabled, each rank step computes a separate in-memory shortlist
from universe-filtered strategy feature rows. No live ranking, candidate, model
input or order consumes it. Discovery status adds `prefilter` with `size`,
`entered`/`exited` counts and ordered instrument keys in `top10`; membership resets
on session rollover and is not persisted.

The score is `market_relative_weight * MarketRelativeBps +
rel_volume_tod_weight * RelVolumeTod + vwap_distance_weight * (Price / SessionVwap
- 1) - spread_weight * SpreadFraction`. All weights default to 1, must be finite
and nonnegative, and retain the feature units (bps for market-relative returns,
fractions for VWAP distance/spread). All inputs are required even at zero weight;
missing or nonfinite inputs and nonpositive price/VWAP are excluded, never filled
with zero. Relative volume remains the documented time-of-day proxy.

Ranks are one-based with inclusive boundaries: retain previous members through
`exit_rank` (150), then admit new members through `enter_rank` (100), up to
`shortlist_size` (100). Retained members take capacity first; both groups use
descending score and ascending key for ties. Ranks/size must be positive and
exit rank must be at least enter rank. These are shadow diagnostics, not
performance or operating evidence.

The offline `nivra_cli::orchestrator::Orchestrator` API inspects
strategy files and an optional existing source library using a read-only opener.
`OrchestratorConfig` uses serde defaults: the existing policy roots
(`strategies`, `proposals`, `orchestrator_notes`), no library path, and default
`strategy_lab::ReplayConfig` settings. Relative inputs and library paths resolve
against its workspace; absolute read paths are allowed. Replay reports and notes
require policy admission before creating parent directories or writing. Existing
symlink write components are rejected; the workspace must not be modified
concurrently during writes. This API has no runtime activation.

`list_proposals()` and `pipeline_view().proposals` recursively inspect
regular files under the configured proposals root without writing. Entries contain
workspace-relative paths, byte sizes, optional Unix modification milliseconds and,
for JSON, top-level `kind`/`strategy` values. Redacted JSON/read/size errors stay
local to each entry. Missing roots are empty; directory errors propagate. Symlinks
are skipped. `max_proposals` (100) stops traversal and the returned subset is sorted;
`max_proposal_bytes` (1,048,576) caps each JSON read. Both have serde defaults;
zero count disables listing. Concurrent filesystem mutation is unsupported.

`run_replay_recorded` optionally appends source-library v2 regime results.
`ReplayRecording` supplies an exact library path, strategy id/version, recording
timestamp and one `RegimeInputs` per snapshot (constructible with the core
`compute_inputs` aligned-price slice helper). Classification uses `RegimeConfig`
defaults. Marks are grouped by their selection snapshot's regime from the full
replay report, preserving cooldown and forward-mark coverage. Each nonempty group
records sample/trade count, after-cost P&L and expectancy in rupees, and cumulative
independent-mark drawdown in rupees; these remain hypothetical round trips.
Explicit replay costs are required for measured groups; missing marks produce no
result rows. The library must be under a write-policy root or exactly match
`OrchestratorConfig.library_path`. Both destinations are checked, including
symlinks, before any write. Library appends and report writes are separate operations,
not an atomic transaction; failed artifacts remain available for inspection.


The offline CLI group requires an explicit workspace root:

```sh
nivra orchestrator --workspace /tmp/lab view
nivra orchestrator --workspace /tmp/lab --config offline.json replay --spec spec.json --snapshots snapshots.json --out proposals/replay.json
nivra orchestrator --workspace /tmp/lab note --path orchestrator_notes/review.txt --text 'Reviewed offline replay'
```

`view` prints pipeline JSON including proposals. `replay` prints the report and
writes it to the policy-admitted `--out` path. To append library results, supply
all three flags: `--library proposals/library.sqlite --strategy-id 1 --version 1`.
Optionally add `--regime-inputs regimes.json` (requires `--library`) to load a
JSON array of `RegimeInputs`, ordered one-to-one with `--snapshots`. Relative paths
resolve against the workspace; absolute paths are allowed. Each object contains
`session_return`, `opening_gap`, `realised_volatility`, `breadth`,
`instrument_count` and `minute_count`; returns and volatility are decimal fractions.
A length mismatch returns a typed `RegimeInputsLength` error before writing the
report or changing the library. Without this flag, recorded regimes remain
`unknown`. Inputs with insufficient evidence still classify as `unknown`.

IDs and versions must be positive. Feature snapshots lack aligned regime evidence,
so this CLI records measured groups as `unknown`; it does not infer market regimes.
Recording uses the current UTC timestamp and the API's explicit-cost requirement.

`--config` selects a bounded (1 MiB) offline `OrchestratorConfig` JSON file relative
to the invoking directory, with serde defaults and unknown-field rejection. For
example, `{"policy":{"proposals_dir":"ideas"},"max_proposals":25,
"max_proposal_bytes":65536}` changes policy and inspection limits. Its `replay`
field supplies replay limits/costs; `library_path` selects the read-only view's
existing library and authorizes that exact recording destination. This is separate
from runtime configuration. Inputs resolve against `--workspace` (absolute reads
are allowed); output and note paths must obey the configured write policy.
These commands use local files only: they do not start runtime, network or broker
clients, discover credentials, or activate a session. Failures print a fixed,
redacted diagnostic and return nonzero.

## Offline scorer comparison

```sh
nivra scorer-compare --snapshots features.json \
  --candidate strategies/cost_aware_momentum.py \
  --params strategies/cost_aware_momentum.params.json \
  --paper-costs paper-costs.json \
  --config comparison.json --out comparison-report.json
```

All arguments are local file paths. `--params` and `--config` are optional JSON
files; params must contain an object. `--paper-costs` is an optional explicit
policy using the runtime `paper_costs` schema:

```json
{"per_order":"0.01","buy_bps":"5","sell_bps":"7",
 "slippage_bps":"5","rounding_increment":"0.01"}
```

It is never read from runtime configuration or the environment. The four
turnover assumptions map exactly into the scorer's params object:
`per_order` -> `per_order_fee`, `buy_bps` -> `buy_bps`, `sell_bps` ->
`sell_bps`, `slippage_bps` -> `slippage_bps`; `rounding_increment` remains
policy evidence because the scorer has no rounding parameter. Missing keys are
added. Existing keys must already have exactly matching numeric values and are
left unchanged. A conflicting value refuses with
`scorer_compare_paper_costs_conflict`. Runtime-only or evidence names
(`params`, `per_order`, `rounding_increment`, `paper_costs`,
`paper_costs_source`) in the params object refuse with
`scorer_compare_paper_costs_reserved` rather than being overwritten. Supplying
`--paper-costs` without `--params` starts an empty params object containing the
four mapped keys.

Unknown fields, malformed JSON, floating-point financial values, negative
amounts, side fees above 10,000 bps, slippage at/above 10,000 bps or a
nonpositive rounding increment refuse with
`scorer_compare_paper_costs_invalid`; an unreadable file refuses with
`scorer_compare_paper_costs_missing`. Reports retain `paper_costs_source`
(`"explicit_file"`, `"params_file"` when only the params file supplies cost
keys, or `"absent"`), the exact `paper_costs` policy when supplied, and the
effective `scorer_cost_params`, so a report cannot be mistaken for one that used
the stale checked-in defaults. The checked-in
`strategies/cost_aware_momentum.params.json` matches the agreed paper policy
(0.01/5/7/5).

Snapshots use the JSON array of
`FeatureSnapshot` objects produced by `strategy-lab export-features`, with strictly
increasing original `observed_ms` values and the default 20-sample lookback.
Input files are bounded to 16 MiB and replay to 10,000 snapshots. The output must
be a new file. This command never constructs providers or changes runtime state.

Omitting config uses K=10, the existing `ScorerSandboxConfig` defaults (bubblewrap,
1 CPU second, 128 MiB, 2-second wall deadline and 65,536 output bytes), and replay
with a 300,000 ms horizon and 2,000 ms maximum mark delay; after-cost metrics are
disabled unless costs are explicitly supplied. `--paper-costs` affects only the
scorer parameters; after-cost replay metrics remain controlled by `replay.costs`
in `--config`, which keeps its separate fee schema without a slippage field.
Example config:

```json
{
  "k": 10,
  "sandbox": {"wall_ms": 2000},
  "replay": {
    "horizon_ms": 300000,
    "max_mark_delay_ms": 2000,
    "costs": {"notional": "5000", "round_trip_spread_bps": 10,
              "fees": {"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"}}
  }
}
```

K is bounded to 1–200. Config and sandbox fields use serde defaults. An explicitly
supplied `replay` object uses the existing laboratory replay defaults for missing
fields; omitting its `costs` (or setting it to null) disables after-cost metrics.
Fees are required within a costs object and use the laboratory `ReplayFees` schema.
This offline schema is separate from the runtime policy and must be supplied explicitly.

Each snapshot reports the core default ranking, candidate ranking, typed sandbox
failure if any, top-K common count and overlap fraction, Kendall tau and Spearman
rho. Scores sort descending with instrument-key ascending tie breaking for both
rankings. Correlations compare the intersection of ranked instruments, compressing
ranks within that intersection; fewer than two common instruments yields null.
Thus these are correlations of deterministic total orders, including tie breaks.
Top-K overlap divides the common count by the larger available top-K list size;
both empty yields null, one empty yields zero. Omitted candidate keys remain
unranked. Failures count toward `failed_ticks` and use the exact core default
ranking, so fallback ticks can have perfect agreement; inspect failures alongside
metrics.

The two replay reports reuse laboratory forward marks, cooldown/rearm, selection
windows, missing-mark reasons and exact-decimal `cost_adjusted` fee arithmetic.
The default retains its original filters, shortlist and wake condition. Candidate
scores select the first K scored rows without a separate wake threshold; fallback
uses the original default selection. Both use the default cooldown/rearm policy.
Forward returns are measured only when recorded future snapshot prices cover the
configured horizon and delay. Missing prices remain missing; there is no
interpolation. With marks and costs present, reports include net expectancy,
hit rate, cumulative drawdown, fees and turnover. These are independent
hypothetical marks, not executable fills, portfolio returns or promotion evidence.
The candidate replay's embedded `spec` identifies the shared default replay
policy (especially cooldown/rearm), not a Python strategy definition; its
`spec_hash` is not a candidate code hash. Use the per-tick candidate rankings and
failure fields to identify which scoring path supplied each selection.
