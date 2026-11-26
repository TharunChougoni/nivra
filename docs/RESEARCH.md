# Research

Research writes and tests strategies away from the paper book. Nothing here
places an order or changes a running session.

A strategy is one of two kinds of Python script:

- A **ranker** scores scanner prospects. The top prospects become the
  candidates the decision lane sees. It cannot buy, sell, size or exit.
- An **action strategy** (`strategy_actions_v1`) returns buys, sells and exit
  revisions. Today it runs in an after-cost replay, and an accepted version is
  registered in the action-strategy library. A paper executor is the next
  step. See [direction](DIRECTION.md).

| Stage | What runs today | Where |
| --- | --- | --- |
| Write | The research lane writes `scripts/<name>.py` in the workspace | [Workspace](#workspace) |
| Test | The sandbox runs the ranker on inline feature rows | [Rankers](#rankers) |
| Evaluate | Recorded replay with forward marks and declared costs | [Replay and evaluation](#replay-and-evaluation) |
| Replay actions | An action strategy trades a recorded quote tape after costs | [Action strategies](#action-strategies) |
| Accept | An accepted replay registers the action strategy with no operator step | [Acceptance and the library](#acceptance-and-the-library) |
| Register | An operator copies a reviewed script into the strategy library | [Registering a ranker](#registering-a-ranker) |
| Select | The scanner ranks with a library version, or falls back to the built-in ranking | [Live ranking selection](#live-ranking-selection) |
| Analyse | Pre-registered analyses run in a contained worker | [Registered research jobs](#registered-research-jobs) |

## Workspace

The planner config's `workspace` block enables a private directory inside the
paper book (`root`, default `workspace`). Files in it are untrusted evidence.
They are never prices, quotes, fills or trade authority. The trading book and
its journal are outside the workspace.

| Directory | Contents |
| --- | --- |
| `scanner/` | Seeded `cost_aware_momentum.py` and its params file |
| `scripts/` | Python rankers written by the research lane |
| `notes/`, `skills/` | Notes and standing procedures the lanes keep across turns |
| `data/`, `reports/`, `journal/` | Inputs and kept output |
| `tools/` | Files installed only when name and SHA-256 match `install_allowlist` |

The pi lanes use these workspace tools:

| Tool | Lanes | Effect |
| --- | --- | --- |
| `list`, `read` | decision, research, review | Read workspace files |
| `write` | research | Write under `notes/`, `skills/` or `scripts/<name>.py` |
| `run` | research | Run a ranker in the sandbox |
| `install` | research | Install an allowlisted file into `tools/` |

Defaults: `max_write_bytes` 65,536, `max_file_bytes` 1 MiB, `cpu_seconds` 30,
`memory_mb` 512 (256 for `scripts/` rankers) and `wall_ms` 30,000.

## Rankers

A ranker reads one JSON object from the file named by `sys.argv[1]`. Each key is
an instrument key, and each value is that instrument's feature object. The
ranker prints one JSON object to stdout. The output keys must be a subset of
the input keys, and each value must be a finite number. A higher score ranks
higher. To reject an instrument, leave it out. An empty object is valid.

Feature names are the core scanner features: `price`, `move_bps`,
`absolute_move_bps`, `market_relative_bps`, `absolute_market_relative_bps`,
`traded_value`, `circuit_distance_bps`, `volatility_bps`, `range_position`,
`best_bid`, `best_ask`, `best_bid_qty`, `best_ask_qty`, `spread_fraction`,
`book_imbalance`, `session_vwap`, `minutes_since_open` and `rel_volume_tod`. A
feature with no evidence is absent, never zero.

The key `params` is reserved. When present, it holds the strategy's parameter
object, not an instrument.

### Sandbox

Every ranker runs as `python3 -I -S -B` under `bwrap --unshare-all` with a
cleared environment. It sees only read-only mounts of the system Python, the
script and the input. It has no network, home, credentials or book. A
seccomp filter denies `fork`, `vfork`, `clone` and `clone3`. CPU, memory, open
files, output and wall time are limited. Missing `bwrap` or Python is a
failure, never a fallback to an unconfined process. A failure returns a typed
error and no scores.

### Test a ranker

The research lane calls `run` with `scripts/<name>.py` and inline feature rows
of at most 64 KiB. The result has the scores and the `source_sha256` of the
exact bytes that ran. Use that hash when you register the ranker.

Every session, the research lane also gets one `after_close` turn after the
review (see [AGENT](AGENT.md#triggers)). It writes or revises strategies from
the day's quote tape and keeps a note per strategy under `notes/`.

To ask for a ranker by hand, stop the controller and run one research turn:

```sh
nivra agent-turn --config PAPER_CONFIG --lane research \
  --research-question 'Write and sandbox-test a Python prospect ranker in scripts/. Report its source hash and any replay evidence.'
```

The turn uses the research lane's budget.

## Action strategies

An action strategy reads one observation from the JSON file named by
`sys.argv[1]` and prints `{"actions":[...]}`. It runs in the ranker
[sandbox](#sandbox), with the same limits, once per observation. It keeps no
state between calls.

The observation holds:

| Field | Contents |
| --- | --- |
| `abi`, `at_ms` | `strategy_actions_v1` and the observation time |
| `params` | The strategy's parameter object |
| `prospects` | Up to 20 ranked prospects, best first: `key`, `rank`, `score` and `features` |
| `quotes` | Depth quotes no more than two seconds old: `bid`, `ask`, `bid_units`, `ask_units`, `source`, `receipt` |
| `positions` | Held `units`, `entry`, `mark`, `marked_at_ms` and `exit_intent` |
| `instruments` | Each key above with its `symbol` and `tick`. Only these keys may be traded |
| `cash`, `costs` | Cash after fees, and the `paper_costs` fee schedule |

Each action is a planner action, the same object a decision-lane plan holds:

```json
{"instrument_key":"NSE_EQ|INE000000001","side":"buy","units":10,"rupees":null,
 "max_price":"100.50","rationale":"breakout",
 "exit_intent":{"target":"102.00","stop":"99.50","time":1795579200000}}
```

- `buy` needs exactly one of `units` or `rupees`, a `max_price`, and an exit
  intent. A fill adds the cost policy's slippage, so leave headroom above
  the ask.
- `sell` needs `units` and `max_price`.
- `revise_exit` needs only the new `exit_intent`.

Prices and rupees are decimal strings. `time` is absolute epoch milliseconds.
At most 32 actions are allowed per call.

Deterministic code runs each action through the planner checks and then the
shared execution gate, the same path as a model plan. A failed call produces
no actions and one reason: `strategy_exit` (a crash or a blocked network
call), `strategy_wall_timeout`, `strategy_output_limit`,
`strategy_output_invalid`, `strategy_output_non_finite`,
`strategy_output_schema`, `strategy_too_many_actions` or
`strategy_unknown_instrument`.

### Strategy replay

The replay treats each depth receipt on the tape as one scan, in the paper
controller's order:

1. Held stops, targets and time exits fill on the receipt, as in the paper
   book.
2. The strategy runs on the observation.
3. Its actions are validated, filled or refused. A fill uses up depth on that
   receipt.

Screening receipts update the features and the built-in prospect ranking.
The report is canonical JSON with no floats, so its SHA-256 identifies it. It
holds the input hashes, the tape dates, every fill, every trade with its exit
reason and net P&L, and every refusal code. Its `summary` gives the trade
count, the win rate, the net P&L after costs, the median trade, the worst
drawdown and the fees. Net P&L counts closed trades only, and
`open_positions` lists what is still held at the end of the tape.

Offline, replay a tape from `scanner-archive --tape`:

```sh
nivra strategy-lab replay-strategy --source strategy.py --parameters parameters.json \
  --manifest manifest.json --events events.jsonl --output new-report.json
```

The parameters are `{"version":1}` plus these optional fields:

| Field | Default |
| --- | --- |
| `params` | `{}` |
| `capital`, `costs` | The manifest's `capital` and `paper_costs` |
| `exit_policy` | None: every held stop, target and time exit executes |
| `top_n` | 20 prospects (at most 50) |
| `from_ms`, `to_ms` | The whole tape. The window limits strategy calls, not held exits |

The command prints the summary and the `report_sha256`, which is the SHA-256
of the report file. [`fixtures/strategy/`](../fixtures/strategy/strategy.py) has an
example strategy, tape and parameters.

In the research lane, enable `candidate_compute.strategy`:

```json
"strategy": {"enabled": true, "abi": "strategy_actions_v1", "max_wall_ms": 20000,
  "max_quote_events": 10000, "max_observations": 100000, "max_tape_bytes": 16777216}
```

Then `backtest` takes `strategy: true`. It replays `source` with `params` over
the selection window of the current session's tape, with the book's capital,
costs and exit policy. It returns the summary and the report hash at once.
The report is kept as `model/strategy-replays/<sha256>.json` in the book,
and the result's `acceptance` gives the verdict described below.

The replay runs on the lane worker that also runs decision turns, so it is
bounded. It stops before the next strategy call when it passes `max_wall_ms`
(default 20,000, at most 120,000) with `strategy_replay_wall_budget`, or when
a decision turn is waiting, with `strategy_replay_preempted`. A stopped replay
keeps no report. It returns `strategy_calls` and `last_at_ms`, so retry with a
narrower `selection_from_ms` and `selection_to_ms`.

### Acceptance and the library

When the research `backtest` finishes a strategy replay, deterministic code
checks the stored report. It refuses a report whose bytes do not hash to
`report_sha256` (`report_hash_mismatch`), that cannot be read
(`report_invalid`), that is not `strategy_actions_v1` version 1
(`report_abi_unsupported`), that is not `paper_only`
(`report_not_paper_only`), or whose source does not hash to `source_sha256`
(`source_hash_mismatch`). It then lists every bar the report misses. The
planner config's optional `strategy_acceptance` block sets the bars:

| Field | Default | Rejected with |
| --- | --- | --- |
| `min_trades` | 10 closed trades | `too_few_trades` |
| `min_net_pnl` | `"0"`. Net P&L after costs must be above it | `net_pnl_too_low` |
| `max_drawdown_bps` | 300 bps of the replay capital | `drawdown_too_deep` |
| `min_tape_dates` | 1 session date | `too_few_tape_dates` |
| `max_failure_bps` | 100 bps of strategy calls | `too_many_strategy_failures` |

A report that still holds a position at the end of the tape is rejected with
`open_positions_at_end`, because its net P&L counts closed trades only. The
drawdown bar is a fraction of capital, so it keeps its meaning when the paper
capital changes. `min_net_pnl` cannot be below zero.

The result returns `{"verdict":"rejected","reasons":[...]}` or
`{"verdict":"accepted","strategy_id":...,"registration":"registered"}`, with
the thresholds used. An accepted strategy is registered with no operator step.
The library sits beside the dated session books, at
`<sessions_root>/strategy-library/`, so no session archive holds it:

| Path | Contents |
| --- | --- |
| `entries/<strategy_id>.json` | Source and report hashes, `params`, author lane, model and turn, summary, tape dates, verdict and thresholds |
| `sources/<source_sha256>.py` | The exact source bytes |
| `reports/<report_sha256>.json` | The accepted replay report |
| `controls.json` | The owner's pin, disable, enable and rollback log |

`strategy_id` is the SHA-256 of the canonical `{abi, source_sha256, params}`.
Entries are never rewritten: registering the same source and `params` again
returns the first entry unchanged. Each read verifies every hash. The active
version is the pinned one when it is eligible, else the newest eligible one.
To pin, disable or roll back a version, see
[operations](OPERATIONS.md#manage-action-strategies).

This library is separate from the ranker library below. The ranker selector
does not read it.

### In the paper controller

With `strategy_execution` enabled in the planner config (see
[operations](OPERATIONS.md#manage-action-strategies)), the paper controller
runs the active version on each rank pass:

1. Held stops, targets and time exits run on every quote read, as before.
2. The controller freezes one observation: the built-in prospects (the
   report's `top_n`), the depth quotes still within two seconds of their
   original receipt, held positions with their exit intents, cash and costs.
3. The strategy runs on a worker thread, with the default sandbox limits and
   outside the book transaction. While a call runs, later scans start none.
4. When the call finishes, the controller takes the book transaction and runs
   its actions through the planner checks and the execution gate against the
   book at that moment. A fill records the `strategy_id` where a model fill
   records a decision ID.

The controller runs a version only when its report names this build's
`engine_sha256` and `adapter_sha256`. Otherwise it runs nothing and reports
`strategy_engine_changed` or `strategy_adapter_changed`. It reads the library
again every 30 seconds, so a pin or disable takes effect without a restart.

A failed call, or a failure to read the library, trades nothing and never
halts the book. Each change of failure reason adds one `strategy_failure` row
to the book's `ticks` and one `failure` line with stage `strategy` to
`diagnostics.ndjson`. Every action's outcome is kept in the book's
`strategy_action` table. Gate refusals also go to `execution_refusal` (see
[agent](AGENT.md#execution-refusals)).

## Replay and evaluation

Replay measures what a ranking would have earned on recorded quotes. Every
replay uses original quote receipts. It never interpolates a missing price or
fills a gap with a later quote.

### In the research lane

With `research_tools.candidate_compute.enabled`, the research lane has
`backtest`, `backtest_status`, `backtest_result` and `backtest_cancel`.
`backtest` queues an immutable ranker version: source, params, a hypothesis,
training and selection windows, and a horizon in minutes. It runs over a
registered dataset in `candidate_compute.datasets`. A dataset is either the
current session's features or an archived feature-snapshot file pinned by
SHA-256.

When `candidate_compute.execution.enabled` is also set, `backtest` accepts an
`execution` argument. It requests an after-cost cash replay over the frozen
session quote tape. The model declares research capital, sizes, holding period
and costs. The replay uses the shared fill engine, so it enforces the
two-second quote gate, cash with fees, depth, ticks and whole shares.

`backtest` returns when the job is queued, not when it finishes. Poll with
`backtest_status` and read with `backtest_result`.

### Offline commands

To export a closed session's quote tape as a capture, stop the controller and
run:

```sh
nivra scanner-archive --config PAPER_CONFIG --tape --output NEW_DIRECTORY \
  [--key INSTRUMENT_KEY ...]
```

It writes `manifest.json` and `events.jsonl` from the book's original quote
receipts, with their original receipt times. Each `--key` keeps only that
instrument's observations. A full day can exceed the replay input limits, so
pass the keys you test. The manifest counts compacted receipts it skipped and
the `quote_tape_gap` ticks by reason. The output directory must not exist.

Export features from a capture, then replay a declarative ranking spec:

```sh
nivra strategy-lab init --database results.sqlite
nivra strategy-lab export-features --manifest manifest.json --events events.jsonl \
  --output features.json
nivra strategy-lab replay --spec spec.json --input features.json \
  --run replay-1 --database results.sqlite --config replay.json
```

`export-features` defaults to `--max-age-ms 2000` and `--lookback-samples 20`.
The replay config sets the forward-mark horizon, the allowed mark delay and the
costs:

```json
{"horizon_ms":900000,"max_mark_delay_ms":2000,
 "costs":{"notional":"5000","round_trip_spread_bps":10,
          "fees":{"per_order":"0.01","buy_bps":"5","sell_bps":"7","rounding_increment":"0.01"}}}
```

| Field | Default | Range |
| --- | --- | --- |
| `horizon_ms` | 300,000 | 1 to 86,400,000 |
| `max_mark_delay_ms` | 2,000 | 0 to 60,000 |
| `selection_from_ms`, `selection_to_ms` | none | Inclusive window for new selections |
| `costs` | none | Without it, replay reports gross returns only |

A forward mark is the first recorded price at or after the horizon, within the
allowed delay. With `costs`, each selection is an independent round trip of
`notional` rupees. Replay applies the shared exact-decimal fees and subtracts
the declared spread. The report then includes net expectancy, hit rate,
turnover, fees and drawdown. These are hypothetical marks, not fills.

Each missing mark gets one reason in `missing_mark_reasons`, such as
`recording_ends_before_horizon` or `instrument_absent_in_window`.

To simulate an explicit list of entries with real fills, use `replay-actions`:

```sh
nivra strategy-lab replay-actions --source entries.json --parameters parameters.json \
  --manifest capture-manifest.json --events capture-events.jsonl --output new-report.json
```

`entries.json` is an ordered array of `{at_ms, key, units, max_price}`. The
parameters set the windows, `holding_ms`, `max_exit_delay_ms`, `capital` and
`costs` in the `paper_costs` schema. Each exit fills only on a real executable
quote after the holding period. The report gives net P&L only when the
simulation ends flat.

Reports bind the exact input hashes and are always new files. Run
`nivra strategy-lab --help` for the other subcommands.

## Registering a ranker

The strategy library is a separate, append-only SQLite file. It stores each
script version by SHA-256.

1. Read the script and its sandbox and replay evidence.
2. Register the exact bytes you reviewed:

   ```sh
   nivra planner register-ranker --planner PLANNER_CONFIG --session YYYY-MM-DD \
     --book DATED_SOURCE_BOOK --script scripts/ranker.py \
     --expected-sha256 REVIEWED_SHA256 --author-model PROVIDER/MODEL
   ```

The command reads the workspace settings from the planner config. It needs an
enabled `discovery.strategy_selection` block, and it writes to that block's
`library_path`, which should be a private path. It refuses a script outside
`scripts/` or bytes whose hash differs from `--expected-sha256`. Success prints
`ranker_registered` with the name, hash and version.

Registration does not change what the scanner uses. Selection does.

## Live ranking selection

`discovery.strategy_selection` in the planner config controls which ranker the
scanner uses:

```json
"strategy_selection": {
  "enabled": true,
  "mode": "live",
  "library_path": "STRATEGY_LIBRARY",
  "paper_trial_sha256": "REVIEWED_SHA256",
  "evaluate_ms": 60000,
  "regime": {},
  "selector": {}
}
```

| Field | Default | Meaning |
| --- | --- | --- |
| `enabled` | `false` | Off means no library reads and no selection |
| `mode` | `shadow` | `shadow` only records a choice. `live` ranks with it |
| `library_path` | empty | Required when enabled |
| `paper_trial_sha256` | none | Live only. Use this exact version, skipping the selector |
| `evaluate_ms` | 60,000 | How often the selection runs |
| `regime`, `selector` | defaults | Market regime and selector thresholds |

Live mode refuses to load next to an enabled `discovery.strategy` champion. It
is part of the trading-policy hash, so turning it on or off goes through the
reviewed config-apply flow in [operations](OPERATIONS.md).

### How a ranker is chosen

On each evaluation, Nivra classifies the market regime from the scanner's
recent minute returns. The key has four parts, for example
`up_trend/high_vol/strong/gap`. With fewer than 200 instruments or 15 minutes of
evidence the regime is `unknown`.

With `paper_trial_sha256` set, that version is active unless the data is stale.
Otherwise the selector reads the library and picks the version with the best
shadow expectancy for the exact current regime. A version qualifies with these
defaults:

| Setting | Default |
| --- | --- |
| Replay sessions | 5 |
| Shadow sessions | 5 |
| Trades | 30 |
| Expectancy | Above 0 |
| Last 3 paper sessions | Each at least 0 |
| Cooldown between switches | 1,800,000 ms |
| Required improvement to switch | 0.0005 |

No command writes the session results that the selector reads, so the selector
always keeps the built-in ranking today. Use `paper_trial_sha256` to rank with a
registered version.

### What live ranking does

In live mode, the rank step loads the active version by hash from the library,
read-only. It sends the current fresh feature snapshot to the sandbox. The
scores order the prospects, highest first, with ties broken by instrument key.
Unscored prospects drop out. The result feeds the same candidate, trigger and
shortlist paths as the built-in ranking. The two-second quote gate and all
execution checks are unchanged.

The scanner falls back to the built-in ranking and logs
`strategy_selection_fallback` when any of these happen:

- The regime is unknown, the data is stale, or no version qualifies.
- The library or source is missing, too large, or fails its hash check.
- The sandbox fails, or the output is invalid or scores no prospect.

Recovery logs `strategy_selection_recovered`. The discovery status shows the
active hash under `live_ranking.sha256` and the selection under
`strategy_selection`, including the fallback reason.

### Declarative specs

`discovery.strategy` is a separate ranking path. It holds up to 16 JSON specs of
filters and weighted feature sums, with no code. One `champion` ranks for the
decision lane. The others record shadow scans. A champion needs an approval in
the results database set by `discovery.strategy_results_database`, recorded
with `strategy-lab qualify` and `strategy-lab approve`, and at least
`strategy_min_shadow_days` (default 5). `nivra strategy-lab default-spec` prints
the built-in composition.

## Registered research jobs

A registered job runs a fixed analysis that an operator reviewed in advance.
Each entry in `research_tools.registrations` pins an `analysis_id`, a
`dataset_id`, the code, runtime and input files by path and SHA-256, and
resource limits. A model picks an analysis and dataset by ID. It cannot supply
code, paths, limits or credentials.

| Tool | Lanes | Effect |
| --- | --- | --- |
| `analysis` | decision, research, review | Queue a registered analysis. Returns a task and revision, not a result |
| `research_status` | decision, research, review | Read the task state |
| `research_result` | decision, research, review | Read the bounded, verified result |
| `research_cancel` | research | Request cancellation of that revision |

The controller runs queued work at a lower priority than decision turns, so a
long analysis never holds a decision turn. A finished task produces one
research event. That event is evidence only. It changes no ranking and
authorizes no fill.

`research_tools.task_ttl_ms` (default six hours, including queue time) limits a
task's life. Each revision is immutable. Nivra never relaunches an interrupted
or ambiguous attempt, and never refunds a quota.

### Worker containment

The separate `nivra-research-worker` claims jobs from the queue. Each job runs
in Bubblewrap inside a transient systemd user unit, with fixed arguments and no
shell, network, home, credentials or trading files. Inputs are read-only, and
`/workspace` is a size-capped tmpfs. CPU, memory, tasks, output and run time
are limited. The queue records job, lease and attempt before launch. Output is
imported only after the unit's cgroup is empty, and only for the exact job and
input hashes. Failed jobs keep their artifacts. Missing containment makes the
worker unavailable, never a plain subprocess.

`nivra-analysis` supports two analyses. `shared_core_backtest_v1` replays an
exact manifest and quote events. `daily_history_summary_v1` gives descriptive,
unadjusted statistics from receipt-bound daily bars, not fills.

### Commands

```sh
nivra research-enqueue --queue QUEUE --catalog CATALOG --request REQUEST
nivra research-accept --result RESULT --code CODE --runtime RUNTIME \
  --input INPUT --split SPLIT --observation OBSERVATION
```

`research-enqueue` only queues a job. `research-accept` judges a finished
result. Neither one activates a strategy.

### Re-register a stale runtime

A registration pins its runtime file by path. If you prune the release
directory that held `nivra-analysis`, the registration fails with
`research_runtime_unreadable`. To fix it, edit the config while no session is
running:

1. Choose a retained path that holds `nivra-analysis`.
2. Update `runtime_file` and `runtime_sha256` in that registration of the
   reviewed planner config. Keep the original config for rollback.
3. Apply it through the reviewed config-apply flow in
   [operations](OPERATIONS.md).
4. Run `nivra preflight --config PAPER_CONFIG` and check for `preflight_ok`.
5. Run `nivra readiness --config PAPER_CONFIG` and check that
   `research.status` is `ready`.

## Experiment records

`nivra experiment-lifecycle --book BOOK --request request.json --output new-receipt.json`
records a strategy specification and its transitions, from `admit` through
`evaluate`, `select_paper` and `retire`. The training window must end before
selection begins. A record never activates a strategy.
