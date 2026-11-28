# Paper configuration

These files are templates for a paper installation. Replace each placeholder
with an installed path. Keep credentials in the application's private files,
never in a template.

| File | Contents |
| --- | --- |
| `paper-current.example.json` | Runtime paper config: account, session, paths, quota journal, capital, `sleeves`, `paper_costs` and the `NSE_FO` costs from [F&O paper costs](../../docs/reference/NSE_FO_PAPER_COSTS.md) |
| `agent-settings.example.json` | The pi `agent` block: `node`, `entry`, `agent_dir` and one model and budget per lane |
| `updater.example.json` | Updater remote and paper config path |
| `*.fragment.json` | Optional blocks to merge into a reviewed config |

See [agent](../../docs/AGENT.md) for the lane keys and
[operations](../../docs/OPERATIONS.md) for how to apply a config change.

## Capital sleeves and sizing

The book splits its capital into two sleeves. The cash sleeve funds the
cash-equity action strategies and cash plans. The options sleeve funds Nifty
option plans. A buy spends only its own sleeve's cash, and the book refuses
it with `insufficient_cash` when that sleeve cannot pay. `capital` must equal
the sum of the sleeve capitals, or the config fails with
`sleeve_capital_sum_mismatch`. A config without `sleeves` is invalid.

```json
"capital": "10000",
"sleeves": {
  "cash":    {"capital": "5000", "risk_per_trade_bps": 50,   "max_position_bps": 4000,
              "max_positions": 3, "min_order_rupees": "200"},
  "options": {"capital": "5000", "risk_per_trade_bps": 3000, "max_position_bps": 10000,
              "max_positions": 1, "min_order_rupees": "200"}
}
```

| Key | Range | Meaning |
| --- | --- | --- |
| `capital` | >= 0 | Rupees allocated to the sleeve when a fresh book opens. 0 leaves it unfunded |
| `risk_per_trade_bps` | 1 to 10000 | Sleeve equity one buy may lose if its stop fills, fees and exit slippage included |
| `max_position_bps` | 1 to 10000 | Largest single position, in bps of sleeve equity |
| `max_positions` | >= 1 | Open positions the sleeve may hold |
| `min_order_rupees` | >= 0 | A sized buy below this notional is refused with `size_below_min_order` |

The runtime sizes every buy from these numbers and the live book. A buy must
carry a stop below its `max_price`. Its units are the most whole lots that
the risk budget, the position cap, the sleeve's cash and the displayed depth
allow. A Nifty option uses the contract lot; cash equity uses one share. A
strategy buy never names units. A decision-lane plan may name units; the
runtime refuses units above the policy with `sizing_policy_exceeded`. Other
refusals are `sleeve_positions_full`, `size_below_lot`,
`sleeve_cash_insufficient` and `size_stop_not_below_price`.

The sleeve policies are runtime config, like `execution`. The allocation
ledger is book state: a fresh book records one entry per funded sleeve, and
each sleeve's cash is its allocation plus its sells, minus its buys and fees.
Reconciliation refuses a buy that overdrew its sleeve
(`sleeve_cash_overdrawn`). Drawdown halts stay book-level.

## Stream executable depth

Set `discovery.market_feed.mode` to `"ltpc"` and
`discovery.market_feed.depth_mode` to `"full"` in the planner file.
The [discovery fragment](scanner-discovery.fragment.json) uses one socket
for both modes. `shortlist_size` defaults to 40 and accepts 1 to 200.
Holdings take priority, followed by the ranker's candidates.

Apply the planner file through the reviewed procedure. During the next open
session, verify that `market_feed.depth_mode` is `full` and
`market_feed.full_fresh_keys` reports keys with fresh depth in
`/api/v1/summary`. [Planner policy changes](../../docs/OPERATIONS.md#change-the-planner-policy)

## F&O gate triggers

The gate lane screens Nifty options. Its thresholds live in the pi `agent`
block under `triggers.fo`. Every key is optional, and a block left at its
defaults is not serialized, so the policy hash does not change. The
[F&O triggers](../../docs/AGENT.md#fo-triggers) section describes how they
fire.

| Key | Default | Range | Meaning |
| --- | --- | --- | --- |
| `spot_move_bps` | 30 | > 0 | Nifty spot move since the reference |
| `premium_move_bps` | 1500 | > 0 | Mid-premium move of a near-money contract |
| `oi_shift_pct` | 10 | > 0 | Open-interest change of a near-money contract, in % |
| `strikes_near` | 2 | 1 to 10 | Strikes either side of the at-the-money strike in the chain |
| `max_spread_ticks` | 4 | 1 to 1000 | Widest spread a tradable contract may show |
| `min_depth_lots` | 2 | 1 to 1000 | Lots the thinner side of the best level must hold |
| `max_cost_bps` | 300 | > 0 | Highest round-trip fees, slippage and spread, in bps of the premium |
| `cooldown_ms` | 600000 | 0 to 86400000 | Quiet time per contract and reason after a trigger |
| `expiry_day_last_minute` | 810 | 0 to 1439 | Exchange-local minute (13:30) after which expiry day raises no trigger |
| `threshold_rise` | 1 | > 0 | Thresholds scale by `1 + threshold_rise × spent` of the decision budget |

F&O triggers need `nifty_options.enabled` in the paper config and a `gate`
lane; without a gate lane they go to the decision lane directly.

## Install a fresh paper account

A fresh account needs an explicit decision by the repository owner and a
starting capital. Keep the original account and its evidence as an archive.

1. Prepare the pi settings JSON with `mode` set to `paper` and a decision
   lane. Confirm that each lane's provider account can use its model.
2. Prepare a private paper base config with an exchange-verified session and
   previous session, the account, fee, market and dashboard credential paths,
   the market archive for pre-open preparation, and the read quota journal
   with its device and inode.
3. Choose two new absolute paths whose parent directories exist, outside the
   original config and session directories.
4. Run setup:

	```sh
	nivra setup --paper PAPER_BASE_CONFIG \
		--agent-settings AGENT_SETTINGS_JSON \
		--config-directory NEW_CONFIG_DIRECTORY \
		--state-directory NEW_STATE_DIRECTORY \
		--cash-capital CASH_SLEEVE_CAPITAL \
		--options-capital OPTIONS_SLEEVE_CAPITAL --confirm-fresh-paper
	```

Setup writes private `paper.json`, `planner.json` and `setup.json` with
`activated: false`, and creates empty state directories. It does not touch the
original book or quota journal, open credential files or start a session.
Before the first start, sign in to each provider in the application's pi
directory, then run `preflight` and `readiness`.

## History preparation settings

These optional runtime keys control pre-open history preparation.

| Key | Default | Meaning |
| --- | --- | --- |
| `history_preparation_enabled` | `false` | Prepare missing daily history before the open |
| `history_preparation_start_minute` | 480 (08:00 IST) | Start of the window |
| `history_preparation_cutoff_minute` | 550 (09:10 IST) | End of the window; must be before 555 |
| `history_preparation_max_attempts` | 3 | Attempts per session; 0 disables preparation |
| `history_preparation_retry_ms` | 60000 | Wait between partial passes |
| `history_preparation_failed_retry_ms` | 60000 | Wait after a failed pass |
| `history_preparation_max_fetch_failures` | 25 | Transient per-instrument failures skipped before a pass stops |
| `history_skip_unavailable` | `false` | Record HTTP 400 ranges as exclusions instead of failures |
| `daily_history_sessions` | 21 | Sessions of daily history; at least 21 |

Authentication, quota and malformed-response failures stop a pass at once.

## Strategy execution settings

These planner keys run an action strategy on each scan and choose it before
the open.

| Key | Default | Meaning |
| --- | --- | --- |
| `strategy_execution.enabled` | absent (off) | Run the library's active strategy on each scan |
| `strategy_execution.fallback` | `[]` | Shipped action strategies (`strategies/<name>.py`), best first, that run labelled as fallback while no accepted version can run |
| `strategy_execution.call_deadline_ms` | `250` | Per-call monotonic deadline (ms, `10..=30000`) enforced on the persistent hardened Python strategy worker (no bubblewrap) |
| `strategy_review.model` | absent (falls back to `agent.lanes.gate.model`) | Model spec (`provider`, `id`, optional `thinking`) used for the security review gate before a candidate `strategy_actions_v1` script is registered |
| `strategy_review.fallback` | absent (falls back to `agent.lanes.gate.fallback`) | Optional fallback model for strategy review on pre-output quota/auth/rejected errors |
| `strategy_review.timeout_ms` | `30000` (or `gate.turn_timeout_ms.min(60000)`) | Per-review timeout in milliseconds (`1000..=120000`) |
| `agent.triggers.decision.pre_open_minute` | 540 (09:00 IST) | Minute of the pre-open strategy choice, after history preparation |
| `research_tools.candidate_compute.strategy.recent_archive_sessions` | 3 | Closed sessions in the rolling `recent_archive` replay dataset (3 to 20) |

```json
{
	"strategy_execution": {"enabled": true, "fallback": ["vwap_trend"], "call_deadline_ms": 250}
}
```

The keys below go under `agent.triggers.decision.strategy_health`. Existing
planner files use the defaults with their current policy hash.

| Key | Default | Meaning |
| --- | --- | --- |
| `idle_minutes` | 15 | Minutes with fresh prospects and no applied action |
| `loss_bps` | 100 | Strategy session net loss in basis points of cash sleeve capital |
| `drawdown_bps` | 150 | Cash sleeve peak-to-current drawdown in basis points of its capital |
| `replay_gap_bps` | 75 | Shortfall against the time-scaled replay return in basis points |
| `min_calls` | 5 | Minimum completed calls for the failure check (1 to 20) |
| `failure_rate_pct` | 50 | Percentage of completed calls that failed |
| `min_actions` | 5 | Minimum evaluated actions for the refusal check |
| `refusal_rate_pct` | 50 | Percentage of evaluated actions that were refused |
| `cooldown_ms` | 900000 | Minimum time between triggers for the same reason |

Use `strategy-health.fragment.json` as a reference. Merge custom values
into the existing planner's `agent` block. See [strategy health](../../docs/AGENT.md#strategy-health)
for the measurement windows.

> **Owner override of sandbox guard:** `strategy_actions_v1` strategies no longer run under per-call bubblewrap (`bwrap`); they run inside a persistent hardened Python worker (`python3 -I -S -B`, rlimits, Landlock, seccomp) after passing a static Python AST check and `strategy_review` model review. See [research](../../docs/RESEARCH.md#sandbox-and-action-worker).

`packaging/rust/strategy-execution.fragment.json` holds the block with an
empty fallback list. A fallback name that is not a shipped
`strategy_actions_v1` script refuses with `strategy_fallback_invalid`.

## Autonomous research loop

The planner key `autoresearch` runs the research loop described in
[research](../../docs/RESEARCH.md#the-autonomous-research-loop).
[`autoresearch.fragment.json`](autoresearch.fragment.json) turns it on with
the defaults below. It needs an enabled `workspace` block and
`research_tools.candidate_compute`, refusing with
`autoresearch_requires_workspace_and_candidate_compute` otherwise, plus a
`market_archive` in the paper config for bar jobs and a `review` lane for skill
review. The block is operational: it never enters the planner policy hash.

| Key | Default | Range | Meaning |
| --- | --- | --- | --- |
| `enabled` | `false` | | Run the loop |
| `campaign.name` | `bars-v1` | slug | A new name starts a new campaign with fresh frozen dates and a fresh trial count |
| `campaign.unit` | `day` | `day`, `minute` | Bar unit of the campaign window |
| `campaign.lookback_days` | 365 | 30 to 3650 | Calendar days of history ending on the last sealed session |
| `campaign.selection_days` | 90 | 5 to below lookback | Trailing days that are scored |
| `campaign.instruments` | absent | up to 500 `NSE_EQ` keys | Universe; absent uses the archived cash universe |
| `campaign.trial_cap` | 60 | 1 to 10000 | Scored variants the campaign may spend, every trial counted |
| `day.variants` | 30 | 1 to 1000 | New variants per IST research day |
| `day.turns` | 60 | 1 to 1000 | Autonomous turns per IST day, reviews included |
| `day.calls` | 200 | 1 to 100000 | Billed model calls per IST day |
| `day.usd` | 8.0 | above 0 to 1000 | Model cost per IST day in USD |
| `turn.calls` | 8 | 1 to 200 | Worst-case calls reserved before each turn |
| `turn.usd` | 0.4 | above 0 to `day.usd` | Worst-case cost reserved before each turn |
| `turn.max_tool_calls` | 8 | 1 to 40 | Tool calls per autonomous turn |
| `turn.timeout_ms` | 120000 | 10000 to 900000 | Wall time per autonomous turn |
| `stop_minute` | 510 (08:30 IST) | 0 to 1439 | On a trading day the loop stops here and waits for the seal |
| `max_drawdown_bps` | absent | 1 to 10000 | Keep-rule drawdown limit; absent uses `strategy_acceptance.max_drawdown_bps` |
| `max_consecutive_failures` | 2 | 1 to 10 | Infrastructure failures in an IST day that pause the loop |
| `max_attempts` | 3 | 1 to 10 | Attempts of one frozen experiment before it is abandoned |
| `family_stall` | 3 | 1 to 20 | Scored non-improvements that close a family |
| `idle_poll_ms` | 300000 | 10000 to 3600000 | How long the controller waits before asking a waiting loop again |
| `skill_review` | `true` | | Review learned skill drafts with a review-lane turn |
| `forward_sessions_paper` | 1 | 1 to 60 | Forward shadow sessions a paper candidate will need (forward stage) |
| `forward_sessions_live` | 10 | 1 to 60 | Forward shadow sessions a live candidate would need; live needs a separate owner mandate |

The research lane's own `daily_calls` and `daily_cost_usd` also cap each
autonomous turn on the book's day. Install `nivra-research-loop.service` and
`nivra-research-loop.timer` from this directory so the loop also runs when the
controller is stopped.

## Market archive

Set this block before starting the paper controller or running archive-backed
commands. Use a dedicated PostgreSQL database with TimescaleDB available.

```json
{
	"market_archive": {
		"url": "postgresql://nivra@127.0.0.1/nivra_market",
		"password_file": "/absolute/path/to/archive-password"
	}
}
```

Include the database user, host, and database name in `url`. Put the password
on one line in the owner-readable `0600` file at `password_file`. The client
uses PostgreSQL without TLS. Use a local socket, loopback, or an encrypted
tunnel for the connection.

Run `nivra archive init --config PAPER_JSON` as a role that can create the
schema objects. Install the TimescaleDB extension in the database first if
the role lacks permission to create it. Run
`nivra archive check --config PAPER_JSON` to print the schema version and
counts of instruments, receipts, observations, bars, and coverage.

## Compatibility

Remove `history_directory`, `dashboard.retention.keep_full_quote_bodies`, and
`dashboard.retention.max_quote_body_bytes` from installed configs. Import
legacy history with `nivra archive import-history --config PAPER_JSON HISTORY_DIR`.
If removing custom retention settings changes a sealed book's config binding,
keep that book for history and prepare a fresh paper config and book.

Every paper fill now requires its original executable quote price
(`expected_price`) and the fee policy used at execution (`costs`). Check a
stopped book with a read-only query. A nonzero result identifies a book
that cannot load with the new format.

```sh
sqlite3 -readonly PAPER_BOOK "SELECT count(*) FROM state,
    json_each(state.body, '$.fills') AS fill
    WHERE json_type(fill.value, '$.costs') IS NOT 'object'
       OR json_type(fill.value, '$.expected_price') IS NOT 'text';"
```

Close the old book flat with the prior binary before replacing it. Keep its
config and state directory for history. With the owner's approval for a fresh
paper account, follow [fresh paper setup](#install-a-fresh-paper-account) with new config
and state directories.

A planner key outside the pi policy shape refuses with `planner_config_invalid`.
Optional keys at their defaults are not serialized, so they leave a sealed
policy hash unchanged. Without a `workspace` or `web_tools` block, those lane
tools are off.
