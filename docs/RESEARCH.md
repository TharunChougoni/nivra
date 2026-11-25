# Research

Research writes and tests strategies away from the paper book. Nothing here
places an order or changes a running session.

Today a strategy is a **ranker**, a Python script that scores scanner
prospects. The top prospects become the candidates the decision lane sees. A
ranker cannot buy, sell, size a position or set an exit. The model in the
decision lane still chooses every action. Strategies with entry and exit rules,
accepted automatically by an after-cost replay and run by code, are the planned
next step. See [direction](DIRECTION.md).

| Stage | What runs today | Where |
| --- | --- | --- |
| Write | The research lane writes `scripts/<name>.py` in the workspace | [Workspace](#workspace) |
| Test | The sandbox runs the ranker on inline feature rows | [Rankers](#rankers) |
| Evaluate | Recorded replay with forward marks and declared costs | [Replay and evaluation](#replay-and-evaluation) |
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
