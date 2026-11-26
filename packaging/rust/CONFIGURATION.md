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

## Market archive

Add this block to use history commands, pre-open preparation, or daily research.
Use a dedicated PostgreSQL database with TimescaleDB available.

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

A planner key outside the pi policy shape refuses with `planner_config_invalid`.
Optional keys at their defaults are not serialized, so they leave a sealed
policy hash unchanged. Without a `workspace` or `web_tools` block, those lane
tools are off.
