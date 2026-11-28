# Research

Research writes and tests strategies away from the paper book. Nothing here
places an order or changes a running session.

A strategy is one of two kinds of Python script:

- A **ranker** scores scanner prospects. The top prospects become the
  candidates the decision lane sees. It cannot buy, sell, size or exit.
- An **action strategy** (`strategy_actions_v1`) returns buys, sells and exit
  revisions. It runs in an after-cost replay, an accepted version is
  registered in the action-strategy library, and when `strategy_execution` is
  enabled the paper controller runs the active version on every scan. See
  [direction](DIRECTION.md).

| Stage | What runs today | Where |
| --- | --- | --- |
| Write | The research lane writes `scripts/<name>.py` in the workspace | [Workspace](#workspace) |
| Test | The sandbox runs the ranker on inline feature rows | [Rankers](#rankers) |
| Backtest | An action strategy trades a recorded quote tape, or archived bars, after costs | [Action strategies](#action-strategies) |
| Accept | An accepted replay registers the action strategy with no operator step | [Acceptance and the library](#acceptance-and-the-library) |
| Register | An operator copies a reviewed script into the strategy library | [Registering a ranker](#registering-a-ranker) |
| Select | The scanner ranks with a library version, or falls back to the built-in ranking | [Live ranking selection](#live-ranking-selection) |
| Analyse | Pre-registered analyses run in a contained worker | [Registered research jobs](#registered-research-jobs) |

## Workspace

The planner config's `workspace` block enables two private workspaces. Files
in them are untrusted evidence. They are never prices, quotes, fills or trade
authority. The trading book and its journal are outside both.

The research lane works in one persistent workspace under the sessions root,
`<sessions_root>/research/workspace/`, so its scripts, notes and learned skills
survive the daily book:

| Directory | Contents |
| --- | --- |
| `scripts/` | Python strategies and rankers the research lane writes and runs |
| `params/` | JSON parameter files |
| `notes/` | Short notes that cite experiment and finding ids; `notes/imported/<date>/` holds notes copied from old books |
| `skills-proposed/<name>/SKILL.md` | Learned procedures waiting for review |
| `skills/` | Reviewed procedures, written only by Rust |
| `reference/strategies/` | Shipped strategies, read-only, refreshed on upgrade |

The decision, review and gate lanes keep the book's own workspace
(`<book>/<root>`, default `workspace`) with `notes/`, `skills/`, `data/`,
`reports/`, `journal/` and `tools/` (files installed only when name and SHA-256
match `install_allowlist`).

On its first step the research loop copies every dated book's
`workspace/scripts/`, `workspace/notes/` and `workspace/skills/` once into the
research workspace, deduplicated by SHA-256. Book skills become proposed
drafts. Old books are never changed.

The pi lanes use these workspace tools:

| Tool | Lanes | Effect |
| --- | --- | --- |
| `list`, `read` | decision, research, review | Read the lane's workspace |
| `write` | research | Write under `notes/`, `params/<name>.json`, `skills-proposed/<name>/SKILL.md` or `scripts/<name>.py` |
| `run` | research | Run a ranker in the sandbox |
| `install` | research | Install an allowlisted file into `tools/` |

The research lane can run its own rankers under `scripts/`. To run other
workspace scripts, list their paths in `workspace.run_allowlist`. The list
starts empty.

Defaults: `max_write_bytes` 65,536, `max_file_bytes` 1 MiB, `cpu_seconds` 30,
`memory_mb` 512 (256 for `scripts/` rankers without `research_env`, 512 when
`research_env` or `ranker_memory_mb` is configured) and `wall_ms` 30,000.

## The autonomous research loop

With `planner.autoresearch.enabled`, Rust runs a bounded research loop. Rust
keeps the experiment log and the clock; pi gets one falsifiable hypothesis and
one editable strategy per turn. A recorded rejection is a good outcome.

The record lives in `<sessions_root>/research/research.sqlite` (WAL, foreign
keys) beside immutable blobs in `research/objects/sha256/<digest>`: campaigns,
ideas, experiments with their attempts, findings, skill revisions, cell
incumbents, leases and IST-day budget reservations. Promotion and shadow
tables exist for the forward stage and stay empty for now. Bar jobs run in the
loop's own queue, `research/jobs.sqlite`.

**When it runs.** From the seal until 08:30 IST (`stop_minute`), and all day on
weekends and exchange holidays, one step after another while there is work.
The paper controller steps it on its lane worker after lane turns, and yields
to decision work and memory pressure. When no controller runs,
`nivra-research-loop.timer` starts `nivra research-loop tick` every 30 minutes;
it exits with `controller_running` while the controller is up. Both entry
points take the same SQLite lease, and only the lease's current generation may
publish a result.

**One step.** Each step makes one durable transition:

1. A new campaign freezes the archive content, calendar, universe, costs and
   `sleeves.cash.capital` once the archive's daily bars cover the last sealed
   session, or else the newest trading session within the last five sessions
   that has full daily-bar coverage (reported as `using_previous_tape` with
   `tape_session`; if none within five sessions qualifies, the loop reports
   `waiting_for_tape`). The most recent 11 complete calendar months contain
   eight months of warm-up and three chronological monthly folds. Every
   scheduled exchange date in the covered calendar must have bars. Full
   historical membership and corporate-action adjustments are unavailable.
   Revised inputs receive a new content address, and superseded scores stay
   searchable as unreproducible.
2. An unfinished experiment runs its frozen job. A restart adopts the job the
   queue already holds; a preemption or an infrastructure failure starts a new
   attempt of the same frozen experiment without spending another trial.
3. A verified report is scored and the verdict is committed before any model
   reads it.
4. An interpretation turn records findings. An invalid source gets one repair.
5. Proposed skill drafts go to a review-lane turn.
6. Otherwise a proposal turn records one idea, writes one strategy and submits
   it. The slot cycles through mutating an incumbent, revisiting a failure and
   a new family.

**The keep rule.** Each fold starts with independent cash and fills whole
shares at the next observed open in that fold. Pending orders expire at its
boundary. A bar book marks an open terminal position at the last observed
close and keeps the count in its report; there is no executable exit to
charge at that close. A quote replay must still end flat. Net P&L must be
positive across the three folds. The candidate must beat cash and each
available required comparator on at least two folds, with a positive median
paired after-cost difference and drawdown inside the configured limit. Ties
keep the incumbent. Each report includes daily equity, turnover, costs, worst
paired date, and a stress run with up to 10 additional bps of slippage. For
longer samples it also shows a weekly-block resampling interval. The daily
Deflated Sharpe estimate uses the raw trial count and reports when its
sample or comparability requirements are unmet.

Declare `required_warmup_sessions` and `label_horizon_sessions` in the
parameter object when the strategy requires them. The embargo withholds the
configured number of exchange sessions of strategy inputs before each fold.
The same compute lock covers both research lanes and quote replay.

**Confirmation.** The loop reserves the last trial for quote replay when
possible. It freezes at most one discovery finalist and its comparator, then
waits for the first two full months after the freeze month. Only the host can
open this window, once. The report records an aggregate verdict and evidence
digest. An interrupted read retires the confirmation as inconclusive. A
passing confirmation still requires separate action evidence.

**Counting.** The experiment key hashes the protocol, evaluator, stage, ABI,
exact source, canonical params, frozen manifest and runner image. A duplicate
submission returns the earlier record and spends nothing. Every reserved
experiment counts as a trial against the campaign's `trial_cap` (60), failed
and abandoned ones included, and the count never resets when a book rolls
over. Each turn reserves its worst case (`turn.calls`, `turn.usd`) against the
IST day's ceilings before the provider is asked and settles to the reported
usage once. A turn that fails keeps its worst case. On the book's own day the
research lane's daily limits also cap each turn.

**Registration.** A quote draft freezes the exact action source and
parameters before its first eligible tape session. The host uses at least
three unseen completed exchange sessions and pins the receipt high-water
mark. The candidate and its action comparator share the same tape, sizing,
costs and capital. A bar translation is a separate action experiment with
its own source identity. The current adapter checks close-price feature
and buy/sell decision parity on common completed minute bars and tape
observations. Daily bars and strategies requiring OHLCV fields the action
observation lacks need a different adapter. A mutation also needs a frozen
action version of its parent for comparison.

Passing quote coverage, paired after-cost P&L, existing acceptance thresholds
and exact-source review permits direct paper registration. The decision lane
still chooses which registered version runs.

**Cost.** The owner measured about USD 0.08 for a six-call research turn on
the paper host, so the default day of 60 turns and 200 calls stays inside
USD 8. Each turn still reserves its configured worst case first.

Inspect the loop with `nivra research-loop --config PAPER_CONFIG status`, or in
`/api/v1/summary` under `research_loop`. Back up the record with
`nivra research-loop --config PAPER_CONFIG backup --to NEW_DIR`; it takes a
consistent SQLite copy, copies every referenced object and verifies them.
`verify --root DIR` checks a backup or the live research root.

### Compare option prospect rankings

The research loop checks the newest sealed session for option evidence under
`research_loop.options`. It reports `option_tape_unavailable` until the
archive contains the dated master, gate inputs and verdicts, and the option
touches needed for replay. Once ready, the loop freezes the archive watermark,
the cohort digest, and the fee and sizing policies. It schedules one ranking
proposal turn per sealed session within the existing research turn budget.

The research-only `option_compare` tool takes a hypothesis and a ranking.
Choose `tightest_spread`, `lowest_cost`, or `deepest_touch` to rank candidates
that passed the frozen screen, or `gate` to repeat the control. The host stores
a pending hypothesis when the tape is unavailable. Repeat the call for the
same session and ranking to retrieve its recorded comparison. Each comparison
reports hypothetical proposed and frozen-gate arms, their individual
refusals, and their paired difference after costs if the tape supports one.

Each arm starts with virtual options cash of ₹5,000. At each non-overlapping
gate observation, take the first candidate from the arm's ranking. Size whole
lots with `sleeves.options` using a stop at 80% of the observed ask rounded
down to the tick. Enter against the archived ask. Exit against the first
scheduled poll whose bid reaches the stop or whose receipt reaches ten seconds
after entry. The replay applies `paper_costs_by_segment.NSE_FO` fees,
slippage, tick rounding, original quote freshness of at most two seconds,
and displayed depth on each leg. Expiry-day and expired entries are refused.
Both legs must fit in the exchange session. Missing, stale, or shallow exit
touches withhold the paired score. The report carries no paper authority.
Actual decision-lane choices and equity cash stay in the paper book.

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

### Sandbox and action worker

Rankers and research bar scripts continue to run as one-shot `python3 -I -S -B`
processes under `bwrap --unshare-all` with a cleared environment, read-only
mounts of the system Python, script, input and optional research environment,
no network, seccomp denying `fork`/`vfork`/`clone`/`clone3`, and bounded CPU,
memory, open files, output and wall time. Missing `bwrap`, Python or a
configured environment fails closed with a typed error.

**Owner override for action strategies (`strategy_actions_v1`):** by owner
decision, action strategies no longer run under per-call bubblewrap (`bwrap`
user-namespace `clone` failed with `EPERM` when launched by the controller on
the paper host, and added ~30–130 ms of startup overhead per scan). Instead,
they run in a **persistent hardened Python worker** (`python3 -I -S -B` with an
empty env, private `0700` working directory, closed file descriptors `>= 3`,
dedicated process group, `PR_SET_NO_NEW_PRIVS`, rlimits for `RLIMIT_AS`,
`RLIMIT_FSIZE`, `RLIMIT_NOFILE`, `RLIMIT_CORE` — no `RLIMIT_CPU` on the
persistent process — Landlock read-only FS + TCP bind/connect denial when
supported, seccomp blocking sockets, `fork`/`clone` and `ptrace`, monotonic
Rust per-call deadlines (`strategy_execution.call_deadline_ms`, default 250 ms),
bounded NDJSON framing, and backoff/quarantine on repeated failures). Safety
also moves up-front to a **two-stage review gate**:

1. **Deterministic Python AST check** (`ast_check.py`): import allowlist
   (standard numeric/collection modules plus `numpy`/`pandas` only when
   `research_env` is active; `sys` restricted to `argv`/`stdin`/`stdout`), and
   rejection of `open`, `exec`, `eval`, `compile`, `__import__`, `globals`,
   `locals`, dunder attribute escapes and format-string attribute traversal.
2. **Model security review** (`strategy_review`, or fallback to
   `agent.lanes.gate`): an ephemeral tool-less review checking for escapes or
   unbounded loops, returning `{verdict, reasons}` bound to the exact
   `source_sha256`, `runner_sha256` and `review_policy_sha256` in library
   entries (`entry_version: 2`). Both static and review currency are re-verified
   before replay and execution.

Without `research_env` (or `env_dir`), the sandbox runs standard library only
with the historical defaults (`memory_mb` 128, `cpu_seconds` 1, `wall_ms` 2,000
in `SandboxConfig`, and 256 MiB for workspace `scripts/` rankers).

#### Research Python environment

Configure `research_env` in `planner.json` (or `env_dir` / `env_sha256` on
`workspace` or `SandboxConfig`) to mount a pinned read-only analysis
environment at `/research-env`:

```json
"research_env": {
  "env_dir": "/opt/nivra/research-env",
  "env_sha256": "<64-hex-sha256>",
  "memory_mb": 512,
  "cpu_seconds": 10,
  "wall_ms": 15000
}
```

To keep `-I -S -B` intact, the sandbox invokes a tiny `-c` wrapper that appends
`/research-env` to `sys.path` after the standard library and sets
`OPENBLAS_NUM_THREADS=1`, `OMP_NUM_THREADS=1`, `MKL_NUM_THREADS=1`,
`NUMEXPR_NUM_THREADS=1`, `VECLIB_MAXIMUM_THREADS=1` and `BLIS_NUM_THREADS=1` so
BLAS/OpenMP libraries stay single-threaded inside the seccomp policy and avoid
multi-thread virtual memory bloat under `RLIMIT_AS`. Network remains disabled for
all rankers and strategies.

Pinned packages in
[`packaging/research/requirements.txt`](../packaging/research/requirements.txt)
(Python 3.14, Linux x86_64 binary wheels, ~293 MiB unpacked):

| Package | Version | Notes |
| --- | --- | --- |
| `numpy` | `2.5.5` | Array math (`numpy.libs` OpenBLAS) |
| `pandas` | `3.1.1` | DataFrames and rolling window statistics |
| `scipy` | `1.18.1` | Hypothesis tests, distributions, optimization |
| `statsmodels` | `0.15.0` | OLS, time-series and regression diagnostics |
| `scikit-learn` | `1.9.1` | Linear models and cross-validation utilities (`joblib` serial mode) |

Excluded from the candidate list: `polars` (~170 MiB unpacked; panics on import
by spawning a background thread via `clone`/`clone3` which seccomp denies, and
breaks `statsmodels` when co-installed), `numba` (~188 MiB unpacked with
`llvmlite` LLVM JIT footprint) and `ta` (sdist-only on PyPI with no Linux x86_64
wheel for `--only-binary=:all:`).

Measured limits: importing `numpy` + `pandas` takes ~1.1–1.3 s CPU and ~90 MiB
RSS; importing all five libraries and fitting models requires `RLIMIT_AS` >=
512 MiB (~206 MiB peak RSS; 256 MiB fails mapping `scipy` shared objects and
384 MiB fails OpenBLAS allocation in `statsmodels`) and ~4.0 s CPU / ~4.2 s wall
time. The defaults for `research_env` are `memory_mb` 512 (`32..=2048`),
`cpu_seconds` 10 (`1..=30`) and `wall_ms` 15,000 (`1..=30,000`).

To build or rebuild the environment from the committed hash-pinned lockfile
(`pip install --require-hashes --only-binary=:all: --no-compile`), and verify
its content hash:

```sh
nivra research-env build --config CFG
nivra research-env verify --config CFG
```

`CFG` can be a paper config (following `planner_config`), a planner config with
`research_env`, or a standalone JSON containing `{"env_dir": "..."}`. Copy the
printed `env_sha256` into `research_env.env_sha256` (or `sandbox.env_sha256`) to
pin the exact installed bytes. Strategy replay reports (`sandbox.env_sha256`)
and workspace `run` responses (`env_sha256`) record this hash on every run.

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

An action strategy defines a top-level `on_observation(obs)` function that
receives the decoded observation dict and returns `{"actions":[...]}`. It runs
inside the persistent hardened [action worker](#sandbox-and-action-worker)
(reused across scans/steps per `(source_sha256, params_sha256)`), after
passing the AST check and model review gate. Strategies must treat each
`on_observation(obs)` call as deterministic and stateless.

The observation holds:

| Field | Contents |
| --- | --- |
| `abi`, `at_ms` | `strategy_actions_v1` and the observation time |
| `params` | The strategy's parameter object |
| `prospects` | Up to 20 ranked prospects, best first: `key`, `rank`, `score` and `features` |
| `quotes` | Depth quotes no more than two seconds old: `bid`, `ask`, `bid_units`, `ask_units`, `source`, `receipt` |
| `positions` | Held `units`, `entry`, `mark`, `marked_at_ms` and `exit_intent` |
| `instruments` | Each key above with its `symbol` and `tick`. Only these keys may be traded |
| `book` | The cash sleeve: `sleeve`, `equity`, `cash`, `open_positions`, `max_positions`, `realised_today` (net realised P&L on this IST date) and `drawdown_bps` (equity below the sleeve's allocated capital) |
| `sizing` | The cash sleeve's policy: `risk_per_trade_bps`, `max_position_bps`, `max_positions`, `min_order_rupees` |
| `costs` | The `paper_costs` fee schedule |

Each action is a planner action, the same object a decision-lane plan holds:

```json
{"instrument_key":"NSE_EQ|INE000000001","side":"buy","max_price":"100.50",
 "conviction":"0.8","rationale":"breakout",
 "exit_intent":{"target":"102.00","stop":"99.50","time":1795579200000}}
```

- `buy` needs a `max_price` and an exit intent with a `stop` below it. A
  strategy never names units. The runtime sizes the buy from the sleeve
  policy: the most shares whose loss to the stop, with both fees and exit
  slippage, fits `risk_per_trade_bps` of sleeve equity, within
  `max_position_bps`, the sleeve's cash and the displayed ask depth. A wider
  stop buys fewer shares. The optional `conviction`, a decimal in (0, 1],
  scales the risk budget. A fill adds the cost policy's slippage, so leave
  headroom above the ask. See
  [capital sleeves](../packaging/rust/CONFIGURATION.md#capital-sleeves-and-sizing).
- `sell` needs `units` and `max_price`.
- `revise_exit` needs only the new `exit_intent`.

Prices and rupee amounts are decimal strings. `time` is absolute epoch milliseconds.
At most 32 actions are allowed per call.

Deterministic code runs each action through the planner checks and then the
shared execution gate, the same path as a model plan. A failed call produces
no actions and one reason: `strategy_exit` (a crash or a blocked network
call), `strategy_wall_timeout`, `strategy_output_limit`,
`strategy_output_invalid`, `strategy_output_non_finite`,
`strategy_output_schema`, `strategy_too_many_actions`,
`strategy_unknown_instrument`, `strategy_missing_on_observation`,
`strategy_review_unsafe` or `strategy_worker_quarantined`.

Use the optional `params.promotion` object to nominate cash candidates before
they have depth. Set `order` to `rising`, `falling`, or `largest_move`.
`min_move_bps` defaults to zero and sets the minimum move in that direction
over the retained price lookback. Nivra evaluates the rule once a second
and on a strategy change. A nomination keeps its slot for at least ten
seconds during the same activation. Holdings always get depth, and the
ranker fills remaining slots. Omit `promotion` to use the ranker alone.

### Strategy replay

The replay processes original, validated, two-sided quotes from archived REST
reads. This includes archived scanner metadata reads with executable depth.
A screen-only legacy tape event updates the scanner.

1. Feed the quoted touch, VWAP, volume, and book totals through the same
   per-instrument reducer as the paper scanner.
2. Run held stops, targets, and time exits on the receipt.
3. Call the strategy when the receipt updates a selected prospect or a holding.
   Replay uses archived executable receipts as coverage for the selected keys.
4. Validate and apply the actions. Each fill consumes the displayed depth
   available on that receipt.

Archive-window tapes carry `avg_daily_volumes` from the 35 calendar days
before the first session. A key needs at least 10 sessions to produce
`rel_volume_tod`. The reducer uses the quote's source minute to estimate the
expected cumulative volume. Quotes must be within two seconds of both their
source and receipt times. The report includes the number of prospect rows
with each feature and a fresh quote.

The report is canonical JSON with no floats, so its SHA-256 identifies it. It
holds the input hashes, the session dates the strategy ran on, every fill, every trade with its exit
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
| `sizing` | The shipped cash-sleeve policy: 50 bps risk, 4000 bps position cap, 3 positions, ₹200 minimum |
| `exit_policy` | None: every held stop, target and time exit executes |
| `top_n` | 20 prospects by default. A call is capped at 20. |
| `from_ms`, `to_ms` | The whole tape. The window limits strategy calls, not held exits |

The command prints the summary and the `report_sha256`, which is the SHA-256
of the report file. [`fixtures/strategy/`](../fixtures/strategy/strategy.py) has an
example strategy, tape and parameters.

In the research lane, enable `candidate_compute.strategy`:

```json
"strategy": {"enabled": true, "abi": "strategy_actions_v1", "max_wall_ms": 20000,
  "max_quote_events": 10000, "max_observations": 100000, "max_tape_bytes": 33554432}
```

Register an archive window in `candidate_compute.datasets`. This example spans
five trading sessions in the reviewed NSE calendar.

```json
{"dataset_id":"november_window","kind":{"archive_window":{
  "from_session":"2026-11-23","to_session":"2026-11-30"
}}}
```

Use an archive window for recent intraday fill realism. Set
`candidate_compute.strategy.max_archive_window_days` to change the maximum
inclusive span. The default is 14 calendar days and the allowed range is 1
through 60. In the local TimescaleDB fixture, tape construction took 1.22 s
across five one-hour slices with 720 receipts per session, or 0.24 s per
session. A 60-calendar-day cap leaves room for sparse selections across more
sessions. Receipt, observation, byte, and wall limits still apply.

The paper config must set `market_archive`. Keep each dated book's `wide-universe.json` for its
frozen cash instrument metadata. The replay uses original receipt times and
refuses a key whose instrument metadata changes within the tape.

Call `backtest` with the archive dataset ID. Set
`selection_from_ms` and `selection_to_ms` to cover at least three trading
sessions. Use an optional `archive_daily_window` to replay the same slice on
each date. For 09:30 to 10:30 IST, pass
`{"from_minute":570,"to_minute":630}`. The start is inclusive and the end is
exclusive. The tape contains the receipts inside both windows, including the
receipts used for held exits. Without a daily slice, the full selected part of
each date is eligible. Use a configured `current_session_features` dataset ID for
a current-session diagnostic.

The result reports the selected trading dates, dates with no archived screen
or protection quotes, and the tape's `events_sha256`. The
archive reader pins the latest committed quote receipt ID before it reads the
window. Pass `tape.archive_window.as_of_receipt_id` back as `as_of_receipt_id`
on the same request to reproduce the tape after new batches arrive. Keep the
same dated instrument metadata when reproducing a tape.

The result includes the after-cost summary and acceptance verdict. The report
is kept as `model/strategy-replays/<sha256>.json` in the book.

The replay shares a worker with decision turns. Tape construction and
strategy calls share `max_wall_ms` (default 20,000, maximum 120,000). A budget
stop returns `strategy_replay_wall_budget`. A waiting decision turn returns
`strategy_replay_preempted`. Both return progress without a report. If the
request exceeds the quote-event, observation or byte cap, narrow the daily
slice or selection window and retry.

### Backtest years of archived bars

Use `archive_bars` to compare a strategy across years and market regimes. Pick
`day` for long ranges or `minute` for intraday patterns. The archive must
already contain the bars. The request reads the dates you choose, within the
research job's CPU, wall time, and memory limits.

Enable `candidate_compute.strategy` and configure `market_archive` and
`paper_costs`. The controller finds `nivra-analysis` beside `nivra`; the updater
installs it there. Install the runtime from the same build as the controller.

Call `backtest` with `dataset_id: "archive_bars"` and a caller-selected
`archive_bars` window. Dates are inclusive IST dates before today. Pass
`instruments: null` to read all archived NSE cash equities in the window.

```json
{"unit":"day","from":"2020-01-01","to":"2024-12-31","instruments":["NSE_EQ|INE000000001"]}
```

You can also register a named dataset with
`"kind":{"archive_bars":{"unit":"day","from":"2020-01-01","to":"2024-12-31"}}`.
Pass that dataset ID and `archive_bars: null`.

Set the inclusive `training_from_ms` and `training_to_ms` to the training
range. Set `selection_from_ms` and `selection_to_ms` to the later test range.
Each split needs data. For daily bars, the timestamp is midnight IST on the
bar's date. For minute bars, it is the archived minute start. Exits come
from the strategy's signals. Pass `params` as a JSON-object string.

The worker keeps one sandboxed Python process for the job. Implement
`on_bars(at_ms, split, bars, params, state)`. Each call receives the complete
batch of observed instruments at that timestamp in key order. Each row is
`[instrument_key, open_paise, high_paise, low_paise, close_paise, volume]`.
The `split` is `"training"` or `"test"`. Use `state` to carry fitted
parameters or rolling features forward. Return an empty list to hold.

```python
def on_bars(at_ms, split, bars, params, state):
    for i, bar in enumerate(bars):
        if bar[0] != params["instrument"]:
            continue
        if split == "training":
            state["prior_close"] = bar[4]
            return []
        if "prior_close" in state:
            return [[i, int(bar[4] > state["prior_close"])]]
    return []
```

Each action is `[row_index, units]`. Positive whole shares enter a long
position when flat. Zero closes the held position. Rust validates the actions,
checks available cash, and fills at the instrument's next observed open
within the same split. The worker releases the next batch after it collects
the previous actions. Each split starts with the book's capital and an empty
portfolio. The test can reuse Python state fitted during training.

The fill model applies the paper schedule's adverse slippage, rounded away
from the open to whole paise with a minimum allowance of one paise. It charges
paper fees on every fill. Trades at the same timestamp compete for cash in
instrument key order. A gap can delay a fill until a later observed bar. Open
positions at the end are marked to their last observed close and reported
separately from realized P&L. The report gives bar and fill counts, fees, and
P&L for each split. It records the source, parameters, and input bar hashes.
Use deterministic source to reproduce the report hash on the same archive
data. The worker reads one archive snapshot per job.

`backtest` returns a task ID and revision. Poll `backtest_status` and read
the verified report with `backtest_result`. Its evidence is `bar_level` and
its acceptance verdict is `research_only`. The report assumes the modeled
fill at open. Spread, queue priority, liquidity, historical universe
membership, and corporate actions are outside the model. Use quote-tape replay to qualify a strategy for the
library. Keep the test split fixed when comparing strategies. If you tune
using the test result, choose a later untouched split for a new assessment.

To exercise the worker with a small synthetic archive, build all targets,
set `NIVRA_ARCHIVE_TEST_URL` to a local TimescaleDB, and run:

```sh
cargo test -p nivra-cli --bin nivra --locked archive_bar_job_replays_closed_splits -- --ignored --test-threads=1
```

The fixture needs Bubblewrap and a systemd user bus. To time the large
fixtures, build `nivra-analysis` with `cargo build --release --locked -p
nivra-cli --bin nivra-analysis`. Set `NIVRA_ARCHIVE_BAR_PERF=day` or
`NIVRA_ARCHIVE_BAR_PERF=minute` on the test command and add `--nocapture`.
The timer starts after the synthetic rows are committed and covers submission
through verified report retrieval.

### Benchmark the bar strategy library

`nivra-tools bar-bench` runs every bar strategy in a directory through the
same `archive_bars` engine, against an archive URL and a password file you
name. A bar strategy is a `<name>.py` file with a `<name>.params.json`
file that holds `{"abi":"archive_bars_v1","unit":"day","params":{...}}`.
Use `"minute"` as the unit for a minute strategy. The bench calls the
engine directly, with no research job queue, and runs strategies in
parallel. It writes a Markdown leaderboard of return, Sharpe, maximum
drawdown, turnover, trades and fees for each split, and with `--json` all
metrics and the report hashes. Training runs from `--from` to the day
before `--test-from`, and the test runs from `--test-from` to `--to`. See
[the library benchmark](../strategies/BENCHMARK.md) for the commands and
the results on the local archive copy.

To check the bench itself, point `NIVRA_ARCHIVE_TEST_URL` at a Postgres
server with a password in the URL. The test creates and drops its own
database:

```sh
NIVRA_ARCHIVE_TEST_URL=postgresql://postgres:PASSWORD@127.0.0.1:5432/postgres \
  cargo test -p nivra-tools --locked bench_ranks -- --ignored
```

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
| `min_tape_dates` | 3 distinct session dates with a strategy call | `too_few_tape_dates` |
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
| `entries/<strategy_id>.json` | Schema v2: source, report, review hashes & verdict, `params`, author lane, model and turn, summary, tape dates, verdict and thresholds |
| `sources/<source_sha256>.py` | The exact source bytes |
| `reports/<report_sha256>.json` | The accepted replay report |
| `reviews/<source_sha256>.json` | Cached passing model review bound to source, runner and review-policy digests |
| `controls.json` | The owner's pin, disable, enable and rollback log, plus decision-lane `lane_select` entries |

`strategy_id` is the SHA-256 of the canonical `{abi, source_sha256, params}`.
Entries are never rewritten: registering the same source and `params` again
returns the first entry unchanged. Each read verifies every hash. The active
version is the pinned one when it is eligible, else the decision lane's last
`lane_select` choice (which may be `null` to stand down), else the newest
eligible one. Each `lane_select` records the planner policy digest it was
made under and counts only under that policy, so applying a new planner policy
clears an earlier lane choice or stand-down. Owner pins, rollbacks and
disables always override lane selections. To pin, disable or roll back a version, see
[operations](OPERATIONS.md#manage-action-strategies).

This library is separate from the ranker library below. The ranker selector
does not read it.

### In the paper controller

With `strategy_execution` enabled in the planner config (see
[operations](OPERATIONS.md#manage-action-strategies)), the paper controller
runs the active version on executable updates for covered prospects and
holdings.

1. The controller processes pushed held depth and completed held reads before
   it applies returned strategy actions. A dedicated REST reader supplies
   held depth on its own two-second deadline.
2. The controller looks up at most the report's `top_n` covered prospects,
   capped at 20. It includes the current holdings, their exit intents, fresh
   quotes, cash, and costs in the same observation.
3. The controller dispatches the observation non-blockingly to a `StrategyHost`
   pool holding the active live worker (with non-blocking hot swap on strategy
   changes and immediate revocation on stand-down or disable), keeping the main
   controller loop unblocked and bounded per call by
   `strategy_execution.call_deadline_ms` (default 250 ms). While a call is in
   flight, the controller keeps one pending observation request and takes a
   fresh snapshot when the worker can accept it.
4. A worker reply notifies the controller immediately. The controller takes
   the book transaction, rechecks the current choice and the worker generation,
   and applies actions against the current book. The gate checks the original
   source and receipt ages after the lock. An action can use only the remaining
   units on its original receipt. A fill records the `strategy_id` where a
   model fill records a decision ID.

`/api/v1/summary` reports `strategy_execution.coverage`. `nominated` counts
strategy nominees and `covered` counts nominees with fresh, valid pushed
depth. `fresh_prospects` includes ranker picks and follows the reported
`source` of `full` or `rest`.

The summary API reports the last 256 completed calls under
`strategy_execution.latency`. Each hop reports its sample count and p50, p95,
and p99 in milliseconds. `switch_to_first_call` measures the time from
controller receipt of a changed choice to the first submission for that choice.
Calls without an action contribute observations and worker timings.
Gate and end-to-end timings use the exact quote for each
action that reaches the execution gate, including refused actions.

The controller runs a version only when its report names this build's
`engine_sha256` and `adapter_sha256` AND its review record matches the current
`source_sha256`, `runner_sha256` and `review_policy_sha256` (plus passes the
static AST check). Otherwise it reports `strategy_engine_changed`,
`strategy_adapter_changed`, `strategy_review_changed` or
`strategy_review_unsafe`. Published decisions and library changes notify the
controller to resolve the selection again. A 30-second refresh covers missed
notifications.

#### Fallback strategies

`strategy_execution.fallback` lists shipped action strategies from
`strategies/`, best first. While no accepted version can run on this build,
the controller runs one of them: the decision lane's choice
(`fallback:<name>`) when it is still listed, else the first. A fallback has
not passed acceptance, so it is labelled everywhere: its actions, fills and
refusals carry `fallback:<name>` in place of a `strategy_id`, the
`strategy_actions` tick records `"basis":"fallback"`, the decision brief
shows `active_basis: "fallback"` with the reason no accepted version runs, and
the dashboard says so. It goes through the same planner checks, execution
gate, costs and freshness checks as an accepted version.

The fallback stops at the next library read once an accepted version can run.
An owner pin still wins; a pin on a version that cannot run here runs
nothing. A lane stand-down (`null`) runs nothing, fallback included. An empty
list, the default, runs nothing until a version is accepted. The planner
refuses a name that is not a shipped `strategy_actions_v1` script with
`strategy_fallback_invalid`.

Every build embeds `strategies/`. The lane workspace holds a read-only copy
under `reference/strategies/`, rewritten when the shipped bytes change.

A failed call, or a failure to read the library, trades nothing and never
halts the book. Each change of failure reason adds one `strategy_failure` row
to the book's `ticks` and one `failure` line with stage `strategy` to
`diagnostics.ndjson`. Every action's outcome is kept in the book's
`strategy_action` table. Gate refusals also go to `execution_refusal` (see
[agent](AGENT.md#execution-refusals)).

## Replay and evaluation

Replay measures what a strategy would have earned on recorded quotes. Every
replay uses original quote receipts. It never interpolates a missing price or
fills a gap with a later quote.

### In the research lane

With `research_tools.candidate_compute.enabled` and
`candidate_compute.strategy.enabled`, the research lane has `backtest`,
`backtest_status`, `backtest_result` and `backtest_cancel`. `backtest` takes a
strategy source, params, a hypothesis, training and selection windows, and a
`dataset_id`. Pass `null` for unused fields that the tool schema allows to be
null.

- A `current_session_features` or `archive_window` dataset runs a
  [strategy replay](#strategy-replay) inside the call and returns the report.
- `recent_archive` is a rolling archive window over the most recent
  `candidate_compute.strategy.recent_archive_sessions` (3, at most 20) closed
  trading sessions within `max_archive_window_days`. It needs no dated
  dataset. The selection window must fall inside those sessions; the
  `strategy_search` task gives the exact `selection_from_ms` and
  `selection_to_ms`. A replay on the current build replaces an acceptance
  made on an older engine or adapter under the same `strategy_id`.
- `archive_bars` queues a [bar job](#backtest-years-of-archived-bars). Poll
  it with `backtest_status` and read it with `backtest_result`.

When upgrading the planner config, remove the retired keys from
`research_tools.candidate_compute`. Remove `abi`, `replay`, `max_result_bytes`,
`limits`, `helper_path`, `unit_open_files`, `execution`, and `observation`.
Keep `candidate_compute.strategy.abi` for strategy replays. Existing paper
strategy entries carry the previous build's adapter hash. Run a new replay
with this build and use the accepted entry for paper execution.

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
     --sessions-root SESSIONS_ROOT --script scripts/ranker.py \
     --expected-sha256 REVIEWED_SHA256 --author-model PROVIDER/MODEL
   ```

The command reads the script from the research workspace under
`SESSIONS_ROOT/research/workspace/` and the workspace settings from the
planner config. It needs an
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

Archive bar jobs use the same queue, lease, and systemd limits. The trusted
`nivra-analysis` worker reads the archive outside Bubblewrap. Only the Python
strategy runs inside Bubblewrap. It receives bar batches over a pipe and has
no archive connection or credentials.

The `nivra-analysis` runtime supports two registered analyses. `shared_core_backtest_v1` replays an
exact manifest and quote events. `daily_history_summary_v1` gives descriptive,
unadjusted statistics from daily closes exported by
`nivra archive export-daily-history`. See [market data](MARKET_DATA.md#research-from-archived-daily-bars).

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
