# Paper configuration

These files are templates for a paper installation. Replace each placeholder
with an installed path. Keep credentials in the application's private files,
never in a template.

| File | Contents |
| --- | --- |
| `paper-current.example.json` | Runtime paper config: account, session, paths, quota journal, capital and `paper_costs` |
| `agent-settings.example.json` | The pi `agent` block: `node`, `entry`, `agent_dir` and one model and budget per lane |
| `updater.example.json` | Updater remote and paper config path |
| `*.fragment.json` | Optional blocks to merge into a reviewed config |

See [agent](../../docs/AGENT.md) for the lane keys and
[operations](../../docs/OPERATIONS.md) for how to apply a config change.

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
		--capital PAPER_CAPITAL --confirm-fresh-paper
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
| `agent.triggers.decision.pre_open_minute` | 540 (09:00 IST) | Minute of the pre-open strategy choice, after history preparation |
| `research_tools.candidate_compute.strategy.recent_archive_sessions` | 3 | Closed sessions in the rolling `recent_archive` replay dataset (3 to 20) |

```json
{
	"strategy_execution": {"enabled": true, "fallback": ["vwap_trend"]}
}
```

`packaging/rust/strategy-execution.fragment.json` holds the block with an
empty fallback list. A fallback name that is not a shipped
`strategy_actions_v1` script refuses with `strategy_fallback_invalid`.

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
