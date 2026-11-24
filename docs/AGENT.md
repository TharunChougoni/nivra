# pi lanes

Nivra uses pi for the decision, research, review, and optional gate lanes. Pi owns providers,
conversation state, tool calls, compaction, and usage reports. Rust builds the
briefs, publishes tools per lane, schedules turns, and enforces lane budgets.

The controller requires an enabled planner with `agent.mode: "paper"` and a
decision lane. Give each enabled lane a reviewed provider, model, call budget,
and cost budget. Pi reads the application's own `auth.json` and `models.json`
from its configured agent directory. Rust starts the agent as a subprocess with
an explicit environment allowlist.

## Agent directory

The agent directory holds the application's own pi `auth.json` and optional
`models.json`. Do not point it at an interactive pi directory such as
`~/.pi/agent`. The paper unit runs with `ProtectHome=read-only`, so keep the
directory under the granted state root, for example
`~/.local/state/nivra/pi`. Create it with mode `0700` and log in to each
configured provider with pi's own command:

```sh
install -d -m 0700 ~/.local/state/nivra/pi
PI_CODING_AGENT_DIR=~/.local/state/nivra/pi pi auth login openai-codex
PI_CODING_AGENT_DIR=~/.local/state/nivra/pi pi auth login mistral --api-key
```

`nivra preflight` checks that pi knows every lane model and fallback and that
each provider has a credential, without reading it.

## Fallback

A lane may name a fallback model. pi's catalog already includes Mistral's
`deepseek-v4-1-flash-1017`, so it needs no `models.json` entry. pi maps the
`medium` thinking level to `high` for that model.

```json
"decision": {
  "model": { "provider": "openai-codex", "id": "gpt-6.1-sol", "thinking": "medium" },
  "fallback": { "provider": "mistral", "id": "deepseek-v4-1-flash-1017", "thinking": "high" },
  "daily_calls": 300,
  "daily_cost_usd": 10.0
}
```

A turn moves to the fallback only when the primary fails before any model
output or tool call:

| Primary failure | Fallback |
| --- | --- |
| Usage, quota or rate limit still failing after pi's retries (`quota`) | Yes |
| Authentication or entitlement failure, such as 401, 403 or a missing credential (`auth`) | Yes |
| Another provider rejection that pi does not retry (`rejected`) | Yes |
| Timeout, 5xx, network error or context overflow | No |
| Abort, preemption, budget or turn deadline | No |
| Any model output or tool call earlier in the turn | No |

The agent rewinds the lane session to the state before the brief and runs the
brief once on the fallback. The failed attempt stays in the session file. Rust
accepts the switch only if it served no tool call in the turn; otherwise it
aborts the turn with `agent_fallback_refused`. The turn journal records
`fallback` with `from`, `to`, `reason` and the bounded error. The turn events
and the log carry the same switch. Lane budgets count the fallback's usage at
the cost pi reports for it. A decision claim accepts a plan from the primary or
the configured fallback. The fallback is part of the planner policy, so set it
before the session starts. A book already bound to the earlier policy needs the
reviewed config-apply transition.

## Gate

The optional gate lane is a cheap, fast screen for entry-candidate triggers, so
frequent market coverage does not spend the decision model's quota. Use a
long-context, low-cache-cost model with a small tool cap and a compaction
threshold, for example:

```json
"gate": {
  "model": { "provider": "mistral", "id": "deepseek-v4-1-flash-1017", "thinking": "low" },
  "daily_calls": 2000,
  "daily_cost_usd": 3.0,
  "turn_timeout_ms": 60000,
  "max_tool_calls": 8,
  "compact_at_tokens": 300000
}
```

The gate session keeps its earlier verdicts, so it does not rescreen unchanged
evidence. See [lane tools](LANE_TOOLS.md#triggers) for how verdicts route
batches.

## Decision

Rust wakes the decision lane when eligible evidence supports a trigger, such as
a fill, position risk, a persistent candidate, or a scheduled scan. The decision
lane can inspect the book and market and submit a `plan`. Only a completed turn
with a final validated proposal can reach the paper plan head. Rust checks the
book, policy, model, instrument allowlist, turn deadline, and exact claim before
committing the plan. Rust uses original quote receipts and the two-second gate
for paper execution. A buy's time exit uses absolute Unix epoch milliseconds.
If supplied, the deadline must still be in the future when Rust validates and
fills the buy. The lane has no order or broker credential access.

An interrupted decision is recorded and is never replayed. A book already
bound to another authority requires the reviewed config-apply transition.

## Research and review

The decision lane can post `research` questions. Rust delivers each completed
answer in a subsequent decision brief. Rust checks the request deadline when it
claims the question, including questions selected earlier in the same batch.
After saving the answer artifact, Rust publishes the result and completes the
request in one SQLite transaction. If publication fails, the turn reports
`agent_research_answer_failed` and the artifact stays available for audit. The research lane can write
`scripts/<name>.py` and test the source with bounded inline feature rows. The
scores-only sandbox has no network access. The response includes the executed
source hash. Configured analysis and backtest tools use the existing research
runner and evidence policy.

### Review a research ranker for paper scanning

Use the decision lane's research request or run a specific research task with
the controller stopped. The manual turn uses the research budget and can write
workspace artifacts.

```sh
nivra agent-turn --config PAPER_CONFIG --lane research \
  --research-question 'Write and sandbox-test a Python prospect ranker in scripts/. Report its source hash and any replay evidence.'
```

Inspect the script and its sandbox and replay evidence. Use the hash from the
successful workspace run to register the exact source in the configured
`discovery.strategy_selection.library_path`:

```sh
nivra planner register-ranker --planner PLANNER_CONFIG --session YYYY-MM-DD \
  --book DATED_SOURCE_BOOK --script scripts/ranker.py \
  --expected-sha256 REVIEWED_SHA256 --author-model PROVIDER/MODEL
```

The CLI reads the workspace settings from the supplied dated planner config.
Use a private path for the strategy library. Registration stores an immutable
library version and leaves the scanner policy alone. To trial it, set
`paper_trial_sha256` to the registered hash in the reviewed planner's enabled
`discovery.strategy_selection` block with `mode: "live"`. Apply the normal
reviewed configuration transition for the target paper session. Check the
scanner's `live_ranking.sha256` and `strategy_selection` status for the applied
hash or the reason it fell back to built-in ranking. See
[Strategy sandbox](STRATEGY_SANDBOX.md#strategy-selection-shadow-diagnostics-and-live-paper-ranking).

### Post-close review

The review lane runs once after the session seal. Rust saves its redacted
review and checks the previous session's closed report and account before
including the review's date, turn ID, and text in a successor brief.

## Prepare and verify

Build the application and agent:

```sh
cargo build --workspace --all-targets --locked
npm ci --prefix agent
npm run build --prefix agent
npm test --prefix agent
```

Use the [configuration guide](../packaging/rust/CONFIGURATION.md) for the
application-owned market-data binding, pi settings, and fresh paper workspace.
Provider account authorization is separate from offline verification. Use the
configured pi directory for login. Use `nivra probe --config PAPER_CONFIG.json`
to check the broker read path. A ready binary and valid config do not confirm
provider access.

For the scripted decision and research round trip, provide agent settings with
both lanes and run:

```sh
target/debug/nivra agent-check --config AGENT_SETTINGS.json --fixture fixtures/agent-check.json
```

`nivra agent-turn --config PAPER_CONFIG.json --lane decision --dry` prints a
brief from the existing book without calling pi. A manual decision proposal
has no execution authority. `nivra agent-shadow-report` reads the historical
shadow journals and reports their observed coverage and gaps.

## Evidence

The book's `model/agent` directory holds trigger batches, turn receipts, plan
outcomes, usage, and lane sessions. Original model-harness journals remain
readable for audit. The paper controller uses pi for provider calls.
