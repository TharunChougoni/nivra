# Lane tools, evidence and budgets

Paper only. This guide covers the controller side of the pi lanes: how Rust
triggers a turn, which tools it publishes, how it binds and validates a plan, and
how lane budgets and failures behave. See [pi agent lanes](AGENT.md) for the
agent service, settings and verification commands.

| Lane | Model | Tools and output |
| --- | --- | --- |
| Decision | `planner.agent.lanes.decision` provider and model | Data tools and `plan`; selects paper action, size and holding period |
| Research | `planner.agent.lanes.research` provider and model | Data tools, workspace, registered analysis and ranker backtests; answers the decision lane's research requests |
| Review | `planner.agent.lanes.review` provider and model | Data tools; one redacted review after the session seal, carried as next-session memory |

## Triggers

Rust's trigger engine observes each scan cycle. A fill, position risk, a
persistent top-ranked candidate, an hourly schedule turn or the pre-close minute
triggers a decision turn. Triggers that arrive while a turn runs merge into the
next batch under the highest-priority kind. Rust builds the brief from frozen
controller evidence and publishes only the tools the lane may call. No eligible
evidence means no model call. A due turn is skipped, never invented, when the lane
is unconfigured, its budget is exhausted or the book is not trading.

The research lane serves the decision lane's `research` requests. Rust delivers
each completed answer in a later decision brief and marks it delivered once that
turn is durable. The review lane runs once after the seal.

`planner.exit_policy` is frozen trading policy and applies to plan actions.
`target` and `time` default to `review`: crossing them raises `held_exit_review`
and does not sell. `stop` defaults to `execute`. `on_halt` defaults to `freeze`,
which blocks new buys and model calls but does not liquidate. The decision lane
may submit `revise_exit` to update a held intent without a trade.

## Tools

Rust publishes every tool's JSON schema per lane and executes calls by name. The
pi-facing name comes first and the Rust name follows in parentheses.

- `evidence` (`read_evidence`): fixed bundle, portfolio, shortlist, fees and
  session-evidence sections. No paths, URLs or query language.
- `snapshot` (`get_market_snapshot`) and `features` (`get_instrument_features`):
  up to `max_instrument_keys` unique allowlisted keys. Duplicate keys fold in
  first-seen order. Quotes use the application's own data credential and quota.
- `prospects` (`get_scanner_prospects`): call-time scanner rows with sampled and
  market-relative moves, depth, ages and the scan reason.
- `rank` (`rank_universe`): compact rows over the validated allowlist with
  explicit missing or stale history and truncation flags.
- `portfolio`, `decisions` and `session`: cash, holdings and exit intents, bounded
  prior decisions, and the clock, market phase and trigger.
- `intraday` (`get_intraday_context`): the frozen minute evidence described below.
- `plan`: decision lane only. Rust validates identity, allowlist, cash, fees,
  sizes and exits and returns per-action verdicts. A validated plan is a proposal,
  never an order.
- `research`, `research_status`, `research_result` and `research_cancel`: the
  durable research request queue. Only the decision lane may queue a request.
- `analysis`, `run`, `backtest` and its status, result and cancel tools, `quotes`,
  and `list`/`read`/`write`/`install`: registered analysis, ranker backtests,
  archived quote receipts (`planner.research_tools.quote_capture`) and the
  bounded workspace. Results are advisory and never place, size or promote a
  paper trade.
- `search`, `fetch` and `news`: opt-in tools under `planner.web_tools`. Private,
  loopback, credential-bearing and redirecting URLs are refused. Each call is
  appended to the session's `model/web-tools.ndjson` record. Results are untrusted
  public evidence.

Strict argument schemas and unique call IDs apply before any dispatch. A durable
claim precedes every tool effect, and a call is never replayed. An oversized result
becomes an explicit tool failure. Every turn's calls and results are archived
under `model/agent/turns/<turn-id>/`. Per-tool bounds are
`planner.tools.scanner_limit` (40), `ranking_limit` (50), `history_limit` (5) and
`max_instrument_keys` (20); defaults are omitted from policy serialization. No tool
grants orders, credential access, service control or ranker promotion.

## Intraday decision evidence

The optional `planner.decision_evidence` object enables `intraday` with
`{"instrument_keys":[...]}`. The defaults are in the
[configuration fragment](../packaging/rust/decision-evidence.fragment.json).
The settings enter the reviewed policy.

The controller freezes context from its scanner and minute price store when it
builds a brief. It makes no extra provider request or financial write. The result
holds a bounded tail of completed receipt-derived price bars and tick counts; no
missing minute is interpolated and tick counts are not traded volume. Returns for
the configured windows need consecutive completed closes. Broad-market medians
need the configured minimum of valid contributors. Context is never a new
executable quote: `snapshot` and the two-second execution gate keep that role.

The tool gives facts only. There is no trade quota, required catalyst or added
trading risk cap, and an empty action list remains valid.

## Plan binding

A plan records the revision it was produced against: `portfolio_revision` (cash,
positions, fills and exit intents), `policy_sha256`, the decision turn ID,
provider, model and `input_hash`. The claim also binds the trigger and the
allowlist revision. Execution rechecks these before the first action. A changed
portfolio records `portfolio_changed`, a changed or unresolvable policy records
`policy_changed`, and a changed allowlist refuses the plan. None performs a fill.
The plan stays archived and a new decision supersedes it.

## Budgets and failures

Each lane needs positive `daily_calls` and `daily_cost_usd`, `turn_timeout_ms` in
1,000–3,600,000 ms and `max_tool_calls` in 1–200; an invalid lane refuses at
planner load. Pi reports usage per turn; Rust charges it to the lane's daily
allowance in `model/agent/usage.json` and journals the turn. A turn cut off by a
crash is charged once from its in-flight checkpoint. A lane allowance is
operational, never a position, loss, exposure or holding-period cap.

The lane service runs at most one turn at a time. A turn beyond its wall limit is
aborted. Provider, schema or timeout failures mean no new trade for that trigger
and never halt the book. Usage corruption, identity mismatch or a failed durable
write is a separate integrity failure. Protection and fills stay deterministic
while a lane runs.
