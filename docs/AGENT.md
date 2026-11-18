# pi lanes

Nivra uses pi for the decision, research, and review lanes. Pi owns providers,
conversation state, tool calls, compaction, and usage reports. Rust builds the
briefs, publishes tools per lane, schedules turns, and enforces lane budgets.

The controller requires an enabled planner with `agent.mode: "paper"` and a
decision lane. Give each enabled lane a reviewed provider, model, call budget,
and cost budget. Pi reads the application's own `auth.json` and `models.json`
from its configured agent directory. Rust starts the agent as a subprocess with
an explicit environment allowlist.

## Decision

Rust wakes the decision lane when eligible evidence supports a trigger, such as
a fill, position risk, a persistent candidate, or a scheduled scan. The decision
lane can inspect the book and market and submit a `plan`. Only a completed turn
with a final validated proposal can reach the paper plan head. Rust checks the
book, policy, model, instrument allowlist, turn deadline, and exact claim before
committing the plan. Rust uses original quote receipts and the two-second gate
for paper execution. The lane has no order or broker credential access.

An interrupted decision is recorded and is never replayed. A book already
bound to another authority requires the reviewed config-apply transition.

## Research and review

The decision lane can post `research` questions. Rust delivers each completed
answer in a subsequent decision brief. The research lane can write
`scripts/<name>.py` and test the source with bounded inline feature rows. The
scores-only sandbox has no network access. The response includes the executed
source hash. Configured analysis and backtest tools use the existing research
runner and evidence policy.

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
