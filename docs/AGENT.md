# Model lanes

Nivra runs models through pi in four lanes. Pi owns providers, conversation
sessions, tool calls, compaction and usage reports. Rust owns everything else:
it chooses when a turn runs, builds the brief, publishes and executes the
tools, enforces budgets and validates plans. Paper only. No lane can place an
order or read a broker credential.

With `strategy_execution` enabled and an active strategy, code executes cash
entries while the decision lane supervises strategy selection, exceptions and
options. Without an active strategy, the decision lane chooses each paper trade
([DIRECTION](DIRECTION.md)). The gate lane screens Nifty options only and
hands the decision lane ranked F&O prospects.

| Lane | Runs when | Output |
| --- | --- | --- |
| Decision | A decision trigger fires during the session | A `plan` of paper actions |
| Gate | Optional. A decision batch holds only F&O signals | A `verdict`: escalate with ranked F&O prospects, or drop |
| Research | Once after the close, and when the decision lane asks, the session is quiet or the review leaves an agenda | A text answer and workspace files |
| Review | Once, after the session seal | A review carried into the next session |

## Configure the lanes

The lanes live in the planner's `agent` block. The paper controller needs
`"mode": "paper"` and a `decision` lane. A `gate` lane needs a `decision` lane.
Start from [agent-settings.example.json](../packaging/rust/agent-settings.example.json)
and the [configuration guide](../packaging/rust/CONFIGURATION.md).

| Field | Rule |
| --- | --- |
| `node`, `entry`, `agent_dir`, `prompts` | Absolute paths. `entry` is `agent/dist/src/main.js`. `prompts` is optional; the agent reads `<lane>.md` from it, or from `agent/prompts/`. |
| `pass_env` | Up to 16 variable names passed to the agent. Only `PATH` and these are inherited; `HOME` is set to `agent_dir`. |
| `lanes.<lane>.model` | `provider`, `id` and optional `thinking`: `off`, `minimal`, `low`, `medium`, `high` or `xhigh`. |
| `lanes.<lane>.fallback` | Optional second model. It must differ from `model`. |
| `daily_calls`, `daily_cost_usd` | Required, positive. See [Budgets](#budgets). |
| `turn_timeout_ms` | 1,000 to 3,600,000. Default 180,000. |
| `max_tool_calls` | 1 to 200. Default 40. |
| `compact_at_tokens` | Optional, 32,000 to 2,000,000. See [Compaction](#compaction). |
| `triggers` | Optional trigger thresholds. See [Triggers](#triggers). |

### Agent directory

`agent_dir` holds the application's own pi `auth.json` and optional
`models.json`. Rust never reads it. Never use an interactive pi directory.
The paper unit runs with `ProtectHome=read-only`, so keep the directory under
the state root the unit can write:

```sh
install -d -m 0700 AGENT_DIR
PI_CODING_AGENT_DIR=AGENT_DIR pi auth login openai-codex
PI_CODING_AGENT_DIR=AGENT_DIR pi auth login mistral --api-key
```

## Triggers

Rust's trigger engine
([triggers.rs](../rust/nivra-cli/src/runtime/agent/triggers.rs)) reads one
observation per scan cycle. Each trigger fires once. Triggers that arrive while
a turn runs merge into the next batch for their lane. The worker runs one turn
at a time, decision first, then research, then review. A waiting decision
trigger aborts a running research or review turn with `agent_preempted`.

| Trigger | Lane | Fires when |
| --- | --- | --- |
| `position_risk` | Decision | A held position comes within `proximity_bps` (50) of its stop or target, falls `adverse_move_bps` (150) since the last decision turn, or ranks below `exit_score` (off by default) |
| `fill` | Decision | New fills appear |
| `entry_candidate` | Decision | An unheld instrument stays in the top `top_k` (5) with score at least `entry_score` (10) for `persist_cycles` (2) scans; then `cooldown_ms` (15 min) per instrument (suppressed while an active strategy owns cash entries) |
| `pre_open` | Decision | Once per session at `pre_open_minute` (09:00 IST) before the open, with `strategy_execution` enabled, once history, universe and book are ready. The turn chooses today's strategy (`strategy_select`) or asks research for one; it holds no paper claim and cannot trade |
| `fo_signal` | Decision, screened by the gate | The Nifty option chain moves: `spot_move`, `premium_move` or `oi_shift` past its threshold while a near-money contract is tradable. See [F&O triggers](#fo-triggers) |
| `schedule` | Decision | Within 30 minutes of the open, at `pre_close_minute` (15:00), on `regime_changed` when `strategy_execution` is enabled (`regime_cooldown_ms`, 5 min), and on the hour when `material_change` (2) new names entered the top K (skipped while a strategy is active) |
| `request` | Research | The decision lane queued a research request |
| `after_close` | Research | Once per session, after the seal, when the review turn has finished, failed or been skipped |
| `quiet` | Research | No decision turn for `quiet_ms` (30 min) during the session, with research budget left |
| `strategy_search` | Research | Once per session, with the `pre_open` turn or the open, when no accepted strategy runs and research budget is left. The task carries the `recent_archive` window; research replays the shipped references and registers what passes (skill `strategy-library`) |
| `agenda` | Research | The review finished and left text |
| `seal` | Review | The session is reported and sealed |

As the decision lane spends its budget, the entry bar rises by
`entry_score_rise` (40) times the spent share, and the top K shrinks in
proportion.

### F&O triggers

With `nifty_options.enabled` and the option universe ready, each scan builds
the near-the-money chain: the contracts within `strikes_near` strikes of the
strike closest to the fresh Nifty spot, with a depth read no older than 5 s.
Each row carries the spread in ticks, the depth in lots on the thinner side,
the open interest from the same REST read, the cash one lot needs at the ask
with the buy fee, the lots the book's cash affords and the round-trip cost
(fees and slippage on both legs plus the spread, in bps of the mid premium).

A contract is tradable when its spread is at most `max_spread_ticks`, its
depth at least `min_depth_lots`, one lot is affordable and the round-trip
cost is at most `max_cost_bps`. Each move is measured from a reference: the
first value seen, the value the last trigger fired on, or the value at the
last decision turn.

| Reason | Fires when |
| --- | --- |
| `spot_move` | Spot moved `spot_move_bps` (30) and some near-money contract is tradable |
| `premium_move` | A tradable contract's mid premium moved `premium_move_bps` (1,500) |
| `oi_shift` | A tradable contract's open interest changed `oi_shift_pct` (10 %) |

Each instrument and reason then waits `cooldown_ms` (10 min). F&O triggers
fire only in the open phase, and on expiry day not from
`expiry_day_last_minute` (13:30). The thresholds scale by
`1 + threshold_rise × spent` (`threshold_rise` 1), so they double once the
decision lane's budget is spent. Index futures are not in the runtime
universe, so the gate screens options only.

The `after_close` turn (reason `post_close`) does not depend on review text.
When the review leaves an agenda, both triggers run as one turn, and the task
carries the review. The task asks the lane to write or revise strategies from
the day's quote tape and backtest them after costs. Rust creates
`model/agent/research-after-close.json` before the turn starts, so a restart
never runs it twice. A spent research budget skips it with
`agent_budget_exhausted`, like any other turn.

A due decision or gate turn is skipped, never invented, when the book is not
trading, or when there is no ranking, no held position and no fill. Any turn
is skipped when its lane is not configured or its budget is spent. Each
outcome goes to `triggers.ndjson` as `fired`, `dispatched`, `skipped`,
`deferred`, `gated` or `gate_bypassed`.

### Gate routing

With a gate lane, a decision batch made only of `fo_signal` triggers runs on
the gate first. The gate brief carries the chain as `fo` and the thresholds in
force as `budget.fo_bar`. The gate calls `verdict` with `escalate`, a reason
and up to five `prospects`, best first. Each prospect names an `NSE_FO` key,
an idea (`buy_call` or `buy_put`), the evidence and why it qualifies.

The host keeps a prospect only when its key is in the current option universe
and its idea matches the contract, drops duplicates and anything past five,
and takes the strike, expiry and lot size from the universe. The tool result
lists each dropped key with its reason (`prospect_not_in_option_universe`,
`prospect_idea_mismatch`, `prospect_duplicate`, `prospect_limit` or
`prospect_invalid`).

On `true`, the same batch runs next on the decision lane, and its brief lists
the kept prospects as `gate_prospects` next to the same `fo` chain. The
decision lane alone decides whether to buy, how many lots and with which
exits, through the normal `plan` and execution gate. On `false`, the batch
ends. A gate turn that fails or gives no verdict escalates without prospects.
The `gated` line in `triggers.ndjson` records the verdict and its prospects.

Cash `entry_candidate` triggers, and any batch with a fill, position risk or
schedule trigger, go straight to the decision lane. When the gate budget is
spent, F&O batches bypass the gate. When the decision budget is spent, the
gate does not run.

While `strategy_execution` is enabled and a strategy runs (an accepted
`entry_version: 2` library version that has passed the AST + `strategy_review`
gate and runs in the persistent hardened Python worker without bubblewrap — an
owner override documented in [research](RESEARCH.md#sandbox-and-action-worker)
— or a configured fallback), the strategy owns cash entries: cash
`entry_candidate` and `hourly_top_k_changed` triggers do not fire, and strategy
fills do not trigger `fill` decision turns. The decision lane keeps `open`,
`regime_changed`, `position_risk`, gated `fo_signal` batches, non-strategy
fills and `pre_close`. Without an active strategy, cash entry candidates go to
the decision lane directly.

## Tools

Rust publishes a JSON schema per tool and lane, and only for tools the planner
config enables. A call to a tool outside the lane fails with
`agent_tool_not_in_lane`.

| Tool | Lanes | Purpose |
| --- | --- | --- |
| `evidence`, `snapshot`, `prospects`, `rank`, `features`, `portfolio`, `session` | All | Frozen evidence, fresh quotes, scanner rows, ranking, features, cash and holdings, clock and trigger |
| `intraday` | All | Minute bars from the scanner store; only with `planner.decision_evidence` |
| `decisions` | Decision, research, review | Recent decisions |
| `analysis`, `research_status`, `research_result` | Decision, research, review | Queue and read registered analysis tasks and research answers |
| `list`, `read` | Decision, research, review | Read the workspace |
| `plan` | Decision | Submit the paper plan (cash buys refused with `agent_paper_strategy_owns_cash_entries` while an active strategy runs) |
| `strategy_select` | Decision | Pick an eligible action strategy from the library, a configured fallback as `fallback:<name>`, or `null` to stand down, with a reason; only with `strategy_execution.enabled`. Owner pins and disables always win, and a runnable accepted version wins over a fallback |
| `research` | Decision | Queue a research request with up to eight `refs` |
| `verdict` | Gate | Escalate the F&O batch with up to five ranked prospects, or drop it |
| `write`, `install`, `run` | Research | Write workspace files, install an allowlisted, hash-matched file into `tools/`, run a `scripts/<name>.py` ranker in the no-network sandbox |
| `backtest`, `backtest_status`, `backtest_result`, `backtest_cancel` | Research | Strategy backtests: a quote-tape replay answers inside the call, an `archive_bars` run is a queued job |
| `research_history`, `idea_record`, `finding_record` | Research | Read the persistent research record, record an idea, record a finding; only with `planner.autoresearch.enabled` |
| `quotes`, `research_cancel` | Research | Archived quote receipts, cancel a request |
| `search`, `fetch`, `news` | Decision, research | Opt-in web tools under `planner.web_tools`; each call is logged in `model/web-tools.ndjson` |

An autonomous research-loop turn publishes only the tools of its step (see
[the research loop](RESEARCH.md#the-autonomous-research-loop)). Its `backtest`
takes an idea id, a workspace script, params and a stage; the host freezes
everything else. A skill review turn on the review lane gets `skill_verdict`.

Per-tool row limits live under `planner.tools`. A `plan` action is `buy`,
`sell` or `revise_exit`. A buy's time exit is a future Unix epoch millisecond.
Research results are advisory and never place, size or promote a trade.

### Skills

The agent loads pi `SKILL.md` procedures from two locations: committed defaults
under `agent/skills/` and the workspace `skills/` directory Rust names in each
turn's `session.skills_dir`. Research reads the persistent research workspace
(`<sessions_root>/research/workspace/skills/`); the other lanes read their
book's workspace (`<book>/workspace/skills/`). Shipped skills win a name
collision. Each skill uses YAML frontmatter with a lowercase-hyphen `name` and
a non-empty `description`.

- Lanes that have the `read` tool (`decision`, `research`, `review`) receive
  `<available_skills>` in their system prompt and load matching skills with
  `read` when needed. Default skills cover `autoresearch`, `strategy-abi`,
  `backtest-methodology`, `regime-check`, `strategy-library` and
  `strategy-review`.
- The research lane drafts learned procedures at
  `skills-proposed/<name>/SKILL.md`. A draft never reaches discovery on its
  own: a review-lane turn approves or rejects it and Rust writes approved
  revisions into `skills/`. The agent reloads skills at the start of each
  turn.
- Invalid or colliding skill files emit `skill_diagnostic` turn events and
  stderr warnings and never abort or block a turn.

### Research requests

Requests sit in `model/agent/requests.sqlite`. At most `max_open_requests` (4)
are open, and each expires after `request_ttl_ms` (2 hours). One research pass
answers up to four requests, one turn each. A finished answer goes into the
next decision brief. A failed or preempted turn fails its request, which never
runs again. The queue event records the turn ID, and the session's
`diagnostics.ndjson` gets a `research_request_failed` line with the turn ID,
request ID, turn error (`agent_preempted` for a preemption) and the queue
outcome. If publishing an answer fails, the turn reports
`agent_research_answer_failed` and the artifact stays for audit.

The review lane runs once per session, after the seal. Rust saves its redacted
text to `model/agent/memory.json`. The next session's decision brief carries
it after Rust checks the previous session's closed paper report.

## Budgets

Each lane has a daily allowance per session date in `model/agent/usage.json`.

- `daily_calls` counts provider calls, not turns. Pi reports one call per model
  response, so every tool round in a turn adds a call.
- `daily_cost_usd` counts the cost pi reports, fallback usage included.
- Rust aborts a running turn with `agent_budget_exhausted` as soon as either
  limit is reached. The lane then runs no more turns that day.
- `max_tool_calls` caps tool calls per turn. Further calls return
  `agent_tool_budget_exhausted`.
- A turn cut off by a crash is charged once from its last usage checkpoint.

A lane budget limits model spend only, never positions, losses or exposure.

Under active strategy supervision, cash entry turns stop and the decision
budget goes to supervision: the open selection, `regime_changed` reviews,
position-risk exceptions, Nifty options and the pre-close turn.

## Compaction

`compact_at_tokens` sets the context size at which pi compacts a lane session,
for the primary and fallback models alike. Pi keeps the most recent fifth of
the threshold, at least 20,000 tokens, and summarizes the rest. Without it, pi
compacts 16,384 tokens short of the model's context window, so a long-context
model pays for a nearly full context on every call.

## Fallback

A turn moves to the fallback model only when the primary fails before any
model output or tool call.

| Primary failure | Fallback |
| --- | --- |
| Quota, usage or rate limit still failing after pi's retries (`quota`) | Yes |
| Authentication or entitlement failure, such as 401 or 403 (`auth`) | Yes |
| Another provider rejection that pi does not retry (`rejected`) | Yes |
| Timeout, 5xx, network error or context overflow | No |
| Abort, preemption, budget or turn deadline | No |

The agent rewinds the session to before the brief and runs the brief once on
the fallback. If Rust already served a tool call in the turn, it aborts with
`agent_fallback_refused`. The turn journal records `fallback` with `from`,
`to`, `reason` and the bounded error. The fallback is part of the planner
policy, so set it before the session starts.

## Plan binding

Before a decision turn, Rust reserves a claim that binds the portfolio hash,
policy hash, input hash, trigger, model and fallback, and a deadline of
`turn_timeout_ms`. When the turn completes, Rust commits the plan only if all
of these still hold, and refuses it otherwise:

| Refusal | Cause |
| --- | --- |
| `agent_paper_decision_expired` | The claim deadline passed |
| `agent_paper_portfolio_changed` | Cash, positions, fills or exit intents changed |
| `agent_paper_policy_changed` | The planner policy on disk changed |
| `agent_paper_turn_binding_invalid` | Turn ID, model, input or policy hash differs |
| `agent_paper_instrument_revision_changed` | The allowlist changed |
| `agent_paper_outside_session` | The book stopped, halted, closed or left the session window |
| `agent_paper_output_secret_refused` | The plan held credential-shaped text (see [Diagnostics](#diagnostics)) |

A committed plan is validated again at execution. A changed portfolio records
`portfolio_changed` and a changed policy records `policy_changed`; neither
fills. Execution uses the original quote receipt and the two-second freshness
gate. An interrupted decision is recorded and never replayed.

### Execution refusals

The execution gate can refuse a validated action, for example on a stale
quote or an ask above `max_price`. The book stores each refusal in the
`execution_refusal` table, keyed by its source, with the action and the
numbers the check used. The source is `decision` with the decision ID, or
`strategy` with `<strategy_id>@<observation ms>` for an action from the
[strategy executor](RESEARCH.md#in-the-paper-controller). A turn's
`execution.json` carries the same numbers as `facts`.

The next decision brief lists the five latest refusals in
`execution_refusals`, newest first:

```json
{"at_ms":1760600002001,
 "source":{"kind":"decision","id":"owner-decision-1","index":0},
 "action":{"side":"buy","instrument_key":"NSE_EQ|INE000000001","units":2,"max_price":"101"},
 "reason":"stale_quote","reason_text":"the quote was older than the freshness bound",
 "facts":{"quote_age_ms":2001,"receipt_age_ms":2001,"max_age_ms":2000}}
```

`facts` holds only the numbers the failed check compared: quote, receipt or
mark age against `max_age_ms`; `ask` and `price_with_costs` against
`max_price`; `units` against `depth_units`, `affordable_units` (with `cash`)
or `held_units`; the minute against the session window. It never holds a
quote payload, a credential or model text.

## Failures

A provider, schema or timeout failure means no trade for that trigger and
never halts the book. A corrupt `usage.json`, identity mismatch or failed
durable write is an integrity failure. Protection and fills stay deterministic.

## Diagnostics

The secret guard refuses a plan or research request that holds
credential-shaped text, and redacts research answers, review memory and turn
journal lines. Each time it matches, the session's `diagnostics.ndjson` gets a
`secret_guard` line naming the site (`decision_plan`, `research_request`,
`research_answer`, `review_memory` or `turn_journal`), `blocked` or
`redacted`, and the turn ID. Each match records the pattern class, the JSON
field path, the string's length and the match offset in bytes, never the
matched text:

| Class | Matches |
| --- | --- |
| `secret_key_field` | A credential key such as `token` or `api_key`; `field` is the object that held it |
| `bearer_prefix`, `sk_prefix` | `Bearer ` or `sk-` anywhere in a string |
| `oauth_access_word`, `oauth_refresh_word` | `access_token` or `refresh_token` in a string |
| `private_key_block` | A PEM private key header |
| `credential_assignment` | A credential key followed by `=` or `:`; the offset is the separator |

A field path such as `$.actions[0].rationale` locates the match. Model keys
that are not short plain identifiers appear as `?`.

## Check an agent

Build and test the agent with the [pi agent gate](../AGENTS.md). Then:

| Command | What it does |
| --- | --- |
| `nivra preflight --config PAPER_CONFIG` | Checks node, entry, modules, prompts and the agent directory, and that pi knows every lane model and fallback and has a credential for each. It does not read credentials. |
| `nivra agent-check --config AGENT_SETTINGS --fixture fixtures/agent-check.json` | Runs a decision turn, its research request and the next decision turn through the real agent against pi's scripted fake provider. The fixture's sleeves fund a scratch book and its optional `options` catalog lets the plan buy a Nifty option; the report shows the units the host sized for each buy. No network, no stored book. |
| `nivra agent-turn --config PAPER_CONFIG --lane decision --dry` | Prints the brief and published tools. Starts no agent. |
| `nivra agent-turn --config PAPER_CONFIG --lane research --research-question 'QUESTION'` | Runs one real research turn on that question against the current book. Needs the controller stopped. Uses the lane budget. Without `--dry`, a decision turn prints its plan and never executes it. |
| `nivra agent-shadow-report --config PAPER_CONFIG` | Reports trigger coverage and gaps from the journals. |
| `nivra probe --config PAPER_CONFIG` | Checks the broker read path. |

To register a ranker that a research turn wrote, take the source hash from the
sandbox run and use `nivra planner register-ranker` with `--planner`,
`--session`, `--sessions-root`, `--script`, `--expected-sha256` and `--author-model`.
See [STRATEGY](STRATEGY.md) and [RESEARCH](RESEARCH.md).

## Files in the book's `model/agent/` directory

| File | Contents |
| --- | --- |
| `triggers.ndjson` | Fired, dispatched, skipped and gated triggers |
| `turns/<turn-id>/` | The turn's archived tool calls and events |
| `plans.ndjson` | Plan outcomes |
| `usage.json` | Daily spend per lane |
| `requests.sqlite` | The research request queue |
| `memory.json` | The review text |
