# pi agent lanes

Paper only. This guide describes the opt-in pi agent service on the
`pi-harness` branch. [pi](https://github.com/earendil-works/pi) owns provider
calls, sessions, compaction, and usage for its three lanes. Rust owns market
data, the scanner, the book, fills, freshness, quotas, budgets, plan
validation, and the sandbox.

Add a `planner.agent` block to enable shadow turns. The existing model
harness retains paper decision authority. The pi decision lane journals
validated proposals for comparison. [Model harness](MODEL_HARNESS.md)

## How it fits together

```
nivra (Rust)                                   nivra-agent (Node 22 + pi SDK 1.5.2)
  decides when a lane turn runs        turn ->   one pi session per lane per session date
  publishes the lane's tool schemas              registers them as pi custom tools
  runs each call through the existing <- tool_call   (pi's read/bash/edit/write never load)
  validation and executors          tool_result ->
  validates `plan` with planner::validate         compaction, retries, cost by pi
  charges pi's usage to lane budgets  <- event, turn_result (text, usage, model)
```

- The agent package is `agent/`: TypeScript strict, exact-pinned
  `@earendil-works/pi-ai`, `pi-agent-core` and `pi-coding-agent` 1.5.2, no fork.
- Rust code is under `rust/nivra-cli/src/runtime/agent/`. The tool executors
  are the harness's own (`harness::Bridge` calls the extracted `execute_tool`).
- One process serves all lanes, one turn at a time.

## Protocol (version 1)

One JSON object per line on the agent's stdin and stdout. Every message carries
`"v":1`; an unknown version or type is refused. Library logging goes to stderr.

| Direction | Type | Fields |
| --- | --- | --- |
| agent → Rust | `ready` | `pi_version`, `node`, `pid` |
| Rust → agent | `turn` | `id`, `lane` (`decision`, `research`, `review`), `session` {`date`, `dir`}, `model` {`provider`, `id`, `thinking`?}, `trigger` {`kind`, `reason`, …}, `brief`, `tools` [{`name`, `description`, `parameters`}] |
| agent → Rust | `tool_call` | `turn_id`, `call_id` (`[A-Za-z0-9_-]{1,128}`), `name`, `args` |
| Rust → agent | `tool_result` | `turn_id`, `call_id`, `ok`, `output` or `error` (a bounded reason code) |
| agent → Rust | `event` | `turn_id`, `event` (see below) |
| Rust → agent | `abort` | `turn_id`, `reason` |
| agent → Rust | `turn_result` | `id`, `status` (`ok`, `error`, `aborted`), `error`?, `output` {`text`}, `usage` {`input`, `output`, `cache_read`, `cache_write`, `cost_usd`, `calls`}, `model`, `provider`, `session_file`, `prompt_sha256` |
| Rust → agent | `shutdown` | |
| agent → Rust | `error` | `error` (a line the agent could not parse) |

Events streamed for a future session view, one per pi event:

| `kind` | Fields | pi source |
| --- | --- | --- |
| `message` | `stop_reason`, `text`, `tool_calls` [{`id`, `name`}], `provider`, `model`, `usage` {`input`, `output`, `cache_read`, `cache_write`, `reasoning`, `cost_usd`}, `error` | `message_end` (assistant) |
| `tool_start` | `pi_call_id`, `name`, `args` | `tool_execution_start` |
| `tool_end` | `pi_call_id`, `name`, `is_error`, `duration_ms` | `tool_execution_end` |
| `compaction_start` | `reason` (`threshold`, `overflow`, `manual`) | `compaction_start` |
| `compaction_end` | `reason`, `aborted`, `will_retry`, `tokens_before`, `tokens_after`, `error` | `compaction_end` |
| `retry_start` / `retry_end` | `attempt`, `max_attempts`, `delay_ms`, `error` / `success`, `attempt`, `error` | `auto_retry_*` |

pi also emits text deltas (`message_update`), queue and session-info events;
the agent does not forward them yet. Each turn's events are kept in
`model/agent/turns/<turn>/events.ndjson`.

## Tools

The bridge is generic. Rust publishes each lane's tools from one table
(`agent/tools.rs`) that maps the pi name to the existing Rust tool; the schema
and description come from the existing Rust definitions. A tool appears only
when the configuration enables it, as in the harness. Adding a Rust tool and a
table row makes it available to pi with no Node change.

| pi name | Rust executor | Lanes |
| --- | --- | --- |
| `evidence`, `snapshot`, `prospects`, `rank`, `features`, `intraday`, `portfolio`, `decisions`, `session` | `read_evidence`, `get_market_snapshot`, `get_scanner_prospects`, `rank_universe`, `get_instrument_features`, `get_intraday_context`, `get_portfolio`, `get_recent_decisions`, `get_session_status` | decision, research, review |
| `analysis` | `research_task_enqueue` (the registered-analysis runner) | decision, research, review |
| `plan` | `planner::validate` (the existing TradePlan validation) | decision |
| `research` | `request_research` | decision |
| `research_status`, `research_result` | `research_task_status`, `research_task_result` | decision, research, review |
| `research_cancel`, `quotes` | `research_task_cancel`, `research_quote_capture` | research |
| `run`, `install` | `workspace_run`, `workspace_install` | research |
| `backtest`, `backtest_status`, `backtest_result`, `backtest_cancel` | `candidate_submit`, `candidate_status`, `candidate_result`, `candidate_cancel` | research |
| `list`, `read` | `workspace_list`, `workspace_read` | decision, research, review |
| `write` | `workspace_write` | research |
| `search`, `fetch`, `news` | `web_search`, `web_fetch`, `news_lookup` | decision, research |

`research` is not offered to the research lane: a research turn may not queue
research for itself. A call to a tool outside the lane returns
`agent_tool_not_in_lane` before any executor runs.

Rust validates each tool's arguments and writes a durable claim before any
tool effect. The tool result is archived under `model/agent/turns/<turn>/`.
Registered analysis requires a reviewed analysis and dataset. Workspace runs
and installs require their corresponding allowlist entries. The research lane
can also write scores-only Python rankers under `scripts/<name>.py`. To run one,
pass its path and a bounded JSON object of per-instrument features as inline
`input`. Rust runs pi research scripts with network access disabled and returns
advisory scores with the hash of the source it ran. Web tools use the configured
web policy. Tools that depend on missing evidence or artifacts
return their executor's reason code.

`plan` returns `{status:"validated", verdicts, execution_authorized:false}`.
Each verdict carries the existing per-action reason (`accepted`,
`action_allowlist`, …). A schema failure returns an error such as
`planner_json_invalid`, and the model may submit again. A validated plan is a
proposal. The last attempt in a turn supersedes earlier attempts, including
when validation refuses it.

## Lanes and sessions

- **Decision**: proposes a plan. Its brief carries the trigger, the facts and
  `research_updates`, the research answers not yet delivered. They are marked
  delivered once the turn completes.
- **Research**: answers queued `research` requests. Rust claims each request
  in `model/agent/requests.sqlite`, runs a research turn with the question
  in its brief, stores the answer at
  `model/agent/research/<request>.json`, posts a bounded `research_result`
  message for the next decision brief and marks the request done. A failed or
  pre-empted turn fails the request; the decision lane may ask again.
- **Review.** The session seal requests one review turn. Its redacted text is
  saved as memory for that book and can trigger a research agenda. The next
  trading session reads it from the calendar's immediate predecessor. Rust
  verifies the predecessor's closed paper report and account. The decision
  brief includes the review's session, turn ID, and text so the model can
  cite the source in its plan notes.

Each lane has its own system prompt (`agent/prompts/<lane>.md`, condensed from
the orchestrator, research and review prompts) and one pi session per session
date under `model/agent/pi/<lane>/`. pi persists the session as JSONL,
continues it across turns and agent restarts, and compacts it when the context
fills. A changed tool set reopens the same session file.

The pi queue uses the same message format as the existing lane queue and a
separate database. The `owner_tasks` scheduler continues to serve the
existing harness.

## Budgets and scheduling

- `daily_calls` and `daily_cost_usd` per lane apply per session date, from
  pi's usage reports. Rust refuses a turn once either is spent and aborts a
  running turn as soon as streamed usage reaches the allowance. Usage is kept in
  `model/agent/usage.json`, including the usage of a turn whose agent exited.
- `turn_timeout_ms` aborts a turn; `max_tool_calls` refuses calls beyond it.
- One turn runs at a time. A decision trigger sets the runner's `preempt`
  flag, which aborts a running research or review turn.
- Cost is pi's figure: the provider's billed cost when reported, otherwise
  pi's catalog estimate. A subscription such as Codex reports an estimate,
  not money spent.
- Every turn appends a line to `model/agent/turns.ndjson`: trigger, status,
  model, provider, usage, tool calls, the validated plan and the pi session
  file.

The controller records fired, dispatched, deferred, and skipped triggers in
`model/agent/triggers.ndjson`. Decision triggers include position risk,
fill batches, candidates that persist across distinct ranking passes, and
scheduled checks. Rust merges queued triggers and gives decision turns
priority. The research lane can wake for a request, quiet period, or review
agenda. Ranker drift requires paired live and backtest measurements from the
promoted-ranker registry, which is pending. Shadow proposals are in
`model/agent/plans.ndjson`.

## Configuration

`planner.agent` (absent by default):

```json
{
  "node": "/opt/nivra/node-22/bin/node",
  "entry": "/opt/nivra/agent/dist/src/main.js",
  "agent_dir": "/var/lib/nivra/pi",
  "lanes": {
    "decision": { "model": { "provider": "openai-codex", "id": "gpt-6.1-sol", "thinking": "medium" },
                  "daily_calls": 300, "daily_cost_usd": 10 },
    "research": { "model": { "provider": "deepseek", "id": "deepseek-v4-pro" },
                  "daily_calls": 200, "daily_cost_usd": 3, "turn_timeout_ms": 600000 },
    "review":   { "model": { "provider": "deepseek", "id": "deepseek-v4-pro" },
                  "daily_calls": 20, "daily_cost_usd": 1 }
  }
}
```

All paths are absolute. `agent_dir` holds pi's `auth.json` (subscription
OAuth tokens, API keys) and optional `models.json`; only the agent process
reads it. The `prompts` setting can point to a directory with `decision.md`,
`research.md`, and `review.md`. Use `pass_env` to allowlist environment
variables that pi needs for a configured provider. Each lane can override
`turn_timeout_ms` (default 180000) and `max_tool_calls` (default 40).
The optional `triggers` object has `decision`, `research`, and `review`
settings. Decision defaults include a top-five shortlist, two distinct
ranking passes, and a 15-minute cooldown.

Switching models is a config change: edit a lane's `provider` and `id`.
To switch to DeepSeek, change the lane's model and reload the controller
through its normal session procedure. Pi resumes the lane's session.

## Offline check

`nivra agent-check` runs the real agent process against pi's scripted fake
provider: a decision turn that calls five read tools, `research` and `plan`;
the research turn that answers it; and the next decision turn, whose brief
carries the answer. It writes only a fresh directory under the system temp
directory, prints a JSON report and exits non-zero on failure.

```sh
npm ci --prefix agent && npm run build --prefix agent && npm test --prefix agent
target/debug/nivra agent-check --config AGENT_SETTINGS.json --fixture fixtures/agent-check.json
```

`AGENT_SETTINGS.json` is the `planner.agent` block on its own. The check
replaces every lane's model with the fake provider, so it needs no login.

## Measurements

Measured on the development host on 2026-11-18 with Node 26.13 (Node 22 was
not installed there), five `agent-check` runs, the fake provider and no model
network:

| Measure | Value |
| --- | --- |
| Startup to `ready` | 383–442 ms warm, 750 ms cold |
| RSS idle after `ready` | 142–144 MB |
| RSS and peak (VmHWM) after three turns | 145–146 MB |
| First decision turn (session create, 4 model steps, 7 tool calls) | 26–34 ms |
| Later turns (agent plus bridge overhead) | 2–4 ms |

The idle RSS is mostly module loading: bare Node is 44 MB, importing `pi-ai`
reaches 57 MB, and importing `pi-coding-agent` (sessions and compaction live
there) reaches 140 MB in 370 ms. A real provider adds its HTTP client and
response buffers, so treat these as lower bounds. The fake provider does not
compute cost, so cost and cached-token share need a live turn.

## Install on the VM (owner steps)

The service needs Node 22.19 or later. These steps do not start trading.

1. Install a pinned Node 22 from nodejs.org and verify its checksum:

   ```sh
   V=22.X.Y   # the newest 22.x on https://nodejs.org/dist/latest-v22.x/
   curl -fsSLO https://nodejs.org/dist/v$V/node-v$V-linux-x64.tar.xz
   curl -fsSL https://nodejs.org/dist/v$V/SHASUMS256.txt | grep "node-v$V-linux-x64.tar.xz" | sha256sum -c -
   sudo mkdir -p /opt/nivra/node-22 && sudo tar -xJf node-v$V-linux-x64.tar.xz -C /opt/nivra/node-22 --strip-components=1
   /opt/nivra/node-22/bin/node --version
   ```

2. Build the agent bundle on a Linux x86_64 build host with Node 22 and copy
   it over (the dependencies are JavaScript and WebAssembly):

   ```sh
   npm ci --prefix agent && npm run build --prefix agent && npm test --prefix agent
   npm ci --prefix agent --omit=dev
   tar -czf nivra-agent.tgz -C agent package.json package-lock.json dist prompts node_modules
   # on the VM
   sudo mkdir -p /opt/nivra/agent && sudo tar -xzf nivra-agent.tgz -C /opt/nivra/agent
   ```

3. Create the application's pi directory, owned by the service user, mode 0700:

   ```sh
   sudo install -d -m 0700 -o nivra -g nivra /var/lib/nivra/pi
   ```

4. Log in to the Codex subscription once, headless, as the service user. Use
   an interactive SSH session (`ssh -t`). `--no-browser` uses the provider's
   device-code or paste-the-redirect flow:

   ```sh
   sudo -u nivra env PI_CODING_AGENT_DIR=/var/lib/nivra/pi \
     /opt/nivra/node-22/bin/node /opt/nivra/agent/node_modules/.bin/pi auth login openai-codex --no-browser
   sudo -u nivra env PI_CODING_AGENT_DIR=/var/lib/nivra/pi \
     /opt/nivra/node-22/bin/node /opt/nivra/agent/node_modules/.bin/pi auth check --provider openai-codex --json
   ```

   The token lands in `/var/lib/nivra/pi/auth.json`. pi refreshes it.

5. Add DeepSeek as the alternate model. `deepseek` is a built-in pi provider:

   ```sh
   sudo -u nivra env PI_CODING_AGENT_DIR=/var/lib/nivra/pi \
     /opt/nivra/node-22/bin/node /opt/nivra/agent/node_modules/.bin/pi auth login deepseek --api-key
   ```

   Any other OpenAI-compatible endpoint goes in `/var/lib/nivra/pi/models.json`;
   Set its key in the service environment and add the variable name to
   `planner.agent.pass_env`.

   ```json
   { "providers": { "my-endpoint": {
       "baseUrl": "https://api.example.com/v1", "api": "openai-completions",
       "apiKey": "$MY_ENDPOINT_API_KEY", "authHeader": true,
       "models": [ { "id": "model-id", "contextWindow": 128000, "maxTokens": 8192,
                     "cost": { "input": 0.3, "output": 1.2, "cacheRead": 0.03, "cacheWrite": 0 } } ] } } }
   ```

6. Prove the install offline, then switch models by editing the config only:

   ```sh
   /opt/nivra/current/nivra agent-check --config /etc/nivra/agent.json --fixture /opt/nivra/agent-check.json
   ```

   Change `lanes.decision.model` from `openai-codex`/`gpt-6.1-sol` to
   `deepseek`/`deepseek-v4-pro` to switch; nothing else changes.

## Inspect or run one shadow turn

Use `--dry` to inspect the brief and published tools from the current book.

```sh
nivra agent-turn --config PAPER_CONFIG.json --lane decision --dry
```

With the controller stopped, omit `--dry` to call the configured provider
once. The command acquires the controller lock, charges the lane's daily
budget, and records any decision plan as a shadow proposal. Decision turns
require current-session evidence during the configured trading window.
Review turns require a sealed report and share the controller's one-attempt
claim for that session.

## Compare a shadow session

Run the evidence report against the paper runtime configuration after the
session closes.

```sh
nivra agent-shadow-report --config PAPER_CONFIG.json
```

The report matches controller dispatches, decision turns, and settled plans.
It shows observed open and pre-close coverage, rejected actions, missing
receipts, and the matched session seal. It excludes manual turns from
controller coverage. The legacy plan counts require a checkpointed book.
Pi's recorded usage is in USD and the legacy estimate is in INR.

Use the report to inspect the session exit criteria. It cannot establish
continuous controller or feed availability between recorded events.
