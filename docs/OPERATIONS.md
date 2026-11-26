# Operations

This guide covers the procedures you run on a paper host. Nivra trades on
paper only, and nothing here enables live orders.

Today a pi decision lane chooses paper actions on each trigger, and Rust
validates and fills them. The project is moving to model-written strategies
that code executes. See [direction](DIRECTION.md).

Commands use placeholders: `PAPER_CONFIG` is the runtime paper config,
`UPDATER_CONFIG` is the updater config, and `NEW_PLANNER` is a planner file.
Every command that changes a book needs `--confirm-paper-review`.

## Rules for every procedure

- Use the application's own credentials. Never print tokens, `auth.json`,
  account payloads or service environments. Share redacted outcomes only.
- Never edit `paper.sqlite`, `current.json`, seals, receipts or usage journals
  by hand. Never create a replacement journal or reset a quota.
- A restart never clears a stop or a halt. Only the commands in this guide do.
- A refused command leaves the original state in place. Keep its output and
  any files it wrote, and inspect them before you try again.

## Services and files

| Item | Purpose |
| --- | --- |
| `nivra-paper.service` | Runs `nivra preflight`, then `nivra paper --config PAPER_CONFIG` |
| `nivra-update.timer` | Starts `nivra-update.service` (`nivra update`) at :00 and :30 |
| `nivra-dashboard.service` | Optional research-only dashboard for when the paper service is stopped |
| `~/nivra-current` | Symlink to the installed release; only the updater moves it |
| `SESSIONS_ROOT/current.json` | Pointer to the current dated session |
| `<session>/paper.sqlite` | Ledger, control state and planner journals |
| `<session>/diagnostics.ndjson` | Redacted startup, health, failure and shutdown evidence for the day |
| `<session>/model/` | Model inputs, results, usage receipts and pi turn journals |
| `<session>/report.json`, `review.json` | Sealed close report and review |
| `~/.local/state/nivra/` | Updater status, failure and recovery records |

The unit files are in `packaging/rust/`. A host first installed from the
retired `ind-sadr` repository keeps its old data directories, because the book
binding hashes paths inside them. The paper unit sets memory, task and file
limits. A drop-in survives upgrades and can override them, so after each
install check what systemd loaded:

```sh
systemctl --user show nivra-paper.service \
	--property=MemoryHigh,MemoryMax,MemorySwapMax,TasksMax,LimitNOFILE
```

The unit can write only to `~/nivra-data`, `~/.config/nivra` and
`~/.local/state/nivra`. Keep the pi `agent_dir` under `~/.local/state/nivra`.
A custom path needs a `ReadWritePaths` drop-in and `systemctl --user
daemon-reload`.

## Install a fresh paper account

A fresh account needs an explicit decision by the repository owner. Follow
[configuration](../packaging/rust/CONFIGURATION.md).

## Daily schedule

Times are IST on NSE trading days from the compiled 2026 calendar. A date
outside that calendar refuses at startup with `calendar_coverage_missing`.

| Time | What the controller does |
| --- | --- |
| 08:00–09:10 | Prepares prior-session daily history, if enabled in [configuration](../packaging/rust/CONFIGURATION.md) |
| Before 15:00 | Rolls over to the next eligible session and protects carried holdings |
| 09:15–15:30 | Protects holdings, scans, runs pi decision turns on triggers and validates paper fills |
| Close | Seals the report with actual cash, fees and carried holdings |
| After the seal | Runs the pi review lane once |

A controller that first starts after 09:15 still adopts the day's session
until 15:00. Carried holdings get protection at once, and new entries wait for
history. After 15:00 the day is `missed_preopen_idle` and the next trading date
rolls over normally.
No eligible evidence means no model call.

## Check readiness

Run these before a start, after an install and after a config change:

```sh
nivra self-check --expected-version vX.Y.Z
nivra preflight --config PAPER_CONFIG
nivra readiness --config PAPER_CONFIG
nivra rollover --config PAPER_CONFIG --now RFC3339_TIME --dry-run
```

`preflight` checks writable paths and the pi agent files, then runs the agent
with `--check` to confirm that pi knows each lane model and has a credential
for each provider. It never reads `auth.json`. Under systemd it refuses a path
the unit does not grant (`preflight_unit_path_uncovered`). From a shell, set
`INVOCATION_ID=manual` or pass `--unit-file FILE` to run that check. A changed
registered research file reports `research_registered_bytes_changed`. See
[research](RESEARCH.md).

`readiness` is read-only and calls no provider. It reports models, history and
rollover separately. Each lane in `models.roles` takes its status from local
evidence only:

| Status | When | `reason` |
| --- | --- | --- |
| `blocked` | The pi runtime is missing, usage is unreadable or the daily budget is spent | For example `agent_node_unavailable` or `daily_budget_exhausted` |
| `unknown` | The lane ran no turn this session, or `turns.ndjson` is unreadable | `agent_provider_not_checked` or `agent_turns_unreadable` |
| `ready` | The lane's latest turn this session finished `ok` on its primary model | `agent_turn_ok` |
| `degraded` | The latest turn failed, was refused or aborted, or fell back | The turn's error, or `agent_fallback_used` |

`last_turn` names the evidence: turn id, time, status, error and any fallback
(from, to and reason). Configuration alone never makes a lane ready.
`credential_check` stays `not_checked` because readiness never reads
credentials; `preflight` checks credential presence when the service starts.

The decision lane sets `models.status` (`model_roles_ready`,
`model_roles_degraded`, `model_roles_unknown` or `model_roles_blocked`). The
command exits zero only for `model_roles_ready`. Before the session's first
decision turn, and after the close when the target is the next session, a
healthy pi planner reports `model_roles_unknown`.

To catch up missing daily history by hand, run
`nivra history-backfill --config PAPER_CONFIG`. Add `--check-only` to report
coverage without calling the provider. See [market data](MARKET_DATA.md).

## Start and stop

Start with `systemctl --user start nivra-paper.service`. The controller treats
each stop signal as one of two kinds.

| Stop | How | After restart |
| --- | --- | --- |
| Service stop | `systemctl --user stop` or `restart`, the updater, a host shutdown or a crash | The same session continues |
| Operator stop | `nivra stop`, then a stop signal | The book stays stopped until `nivra resume` |

To stop the book for review:

```sh
nivra stop --config PAPER_CONFIG --confirm-paper-review
systemctl --user stop nivra-paper.service
```

With the controller running, `nivra stop` writes `stop-request.json` next to
`current.json`, valid for ten minutes. The controller reads it on the next stop
signal, blocks any in-flight planner attempt and marks the book stopped. With
no controller running, `nivra stop` stops the open book directly. A closed
book refuses with `stop_session_closed`.

To resume an operator-stopped open book after review:

```sh
nivra resume --config PAPER_CONFIG --confirm-paper-review
nivra readiness --config PAPER_CONFIG
systemctl --user start nivra-paper.service
```

`resume` refuses a closed book (`resume_session_closed`) and an edited planner
that is not applied yet (`resume_planner_config_not_applied`). A model-led book
resumes with its holdings.

## Change the planner policy

The planner file sets `enabled`, `model_led` and the `agent` block with the pi
lanes, their models, budgets and timeouts. See [agent](AGENT.md) for the keys.
Any other key refuses with `planner_config_invalid`.

1. Write the new policy to a new file. Check it offline. The check calls no
   model and writes nothing.

	```sh
	nivra planner config-check --planner NEW_PLANNER --session SESSION
	```

2. Point `planner_config` in `PAPER_CONFIG` at the new file.
3. Apply it in the way that matches the book.

**Open book.** Stop, apply, then resume:

```sh
nivra stop --config PAPER_CONFIG --confirm-paper-review
systemctl --user stop nivra-paper.service
nivra config-apply --config PAPER_CONFIG --confirm-paper-review
nivra resume --config PAPER_CONFIG --confirm-paper-review
systemctl --user start nivra-paper.service
```

`config-apply` records the old and new policy hashes and leaves the book
stopped. It refuses a halted book
(`config_apply_halted_requires_planner_recover`) and pending orders. It refuses
positions unless the book is model-led. Switching `model_led` needs a book with
no fills.

**Closed book.** `config-apply` refuses with `config_apply_book_closed`. Leave
the book alone. The next rollover binds the edited planner.

When the running controller sees an edited planner or prompt, it suspends the
planner with `planner_configuration_changed_apply_required`. It does not halt
the book. Run the open-book sequence.

To schedule a change, add `effective_from` (a date) and `previous_config` (an
absolute path) to the new file. Sessions before the date use the previous file.
Run `config-check` for a session on each side of the date.

To switch a lane model, change `provider` and `id` in the lane's `model` block
and sign in to the provider in the application's pi directory. To roll back,
point `planner_config` at the previous file. Apply either change as above.

## Manage action strategies

Accepted research strategies register themselves (see
[research](RESEARCH.md#acceptance-and-the-library)). These commands read the
runtime config only to find the library. They never open a book, a broker or a
credential, and they work with the controller running:

```sh
nivra strategy --config PAPER_CONFIG list
nivra strategy --config PAPER_CONFIG show STRATEGY_ID
nivra strategy --config PAPER_CONFIG pin STRATEGY_ID
nivra strategy --config PAPER_CONFIG unpin
nivra strategy --config PAPER_CONFIG disable STRATEGY_ID
nivra strategy --config PAPER_CONFIG enable STRATEGY_ID
nivra strategy --config PAPER_CONFIG rollback [--to STRATEGY_ID]
```

An id can be a unique prefix of at least 8 characters. Each change appends to
`controls.json` and prints the new active version; entries never change.
`rollback` disables the active version and pins the eligible version
registered just before it, or `--to`. A disabled version cannot be pinned.

## Change paper costs

The runtime config needs a complete `paper_costs` object. See
[strategy](STRATEGY.md) and `packaging/rust/paper-costs.fragment.json`. A
session keeps the cost policy it started with, and a change on the same
session refuses with `paper_cost_policy_changed`.

To change costs for a continuing model-led account, save the current config
outside the session directory, set the new `paper_costs`, and add:

```json
"paper_cost_transition": {"source_session": "2026-10-16", "prior_config": "/absolute/paper-20261016.json"}
```

Every field except `paper_costs` must match the prior config. The source book
must be closed, not stopped and not halted. The successor keeps cash, positions
and fills, and uses the new costs for new fills. Remove the block with the
next config change.

## Rollover

The controller rolls over by itself before 15:00 when the source book is sealed
and eligible. With the controller stopped, you can preview and advance one
rollover yourself:

```sh
nivra rollover --config PAPER_CONFIG --now RFC3339_TIME --dry-run
nivra rollover --config PAPER_CONFIG --now RFC3339_TIME --advance --confirm-paper-review
```

`--now` is required. Run it on the target date before 15:00. `--advance` needs
the preview status `rollover_ready` and takes its source from `current.json`.
If the target is already current, it returns `already_advanced` and changes
nothing. Rollover never starts the service or places an order.

Some closed books need an acceptance first. Each acceptance writes an
append-only receipt in the session directory, and a repeat returns the same
receipt.

| Closed source book | Flag before the advance |
| --- | --- |
| Flat, stopped, not halted, missing its review seal | `--acknowledge-closed-book` |
| Model-led, operator-stopped, not halted, holding positions | `--accept-stopped-carry` |
| Model-led, halted, not stopped, holding positions | `--accept-halted-carry` |

For a carried book, with the controller stopped:

```sh
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --dry-run
nivra rollover --config PAPER_CONFIG --accept-stopped-carry --confirm-paper-review
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --dry-run
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --advance --confirm-paper-review
systemctl --user start nivra-paper.service
```

Use `--accept-halted-carry` for a halted book. The second preview must show
`rollover_ready` and `carry_admitted: true`. The successor gets the same cash,
positions, fills and fees. After a halted carry it starts halted, with entries
blocked and exits protected. A multi-day halted hold needs a new acceptance
for each closed session. If you start the controller instead of advancing, it
performs the same rollover.

## Recover from a halt

A halt blocks new entries. Stop and inspect the book first. `planner-recover`
clears planner halts only. It refuses risk halts, non-flat open books and
missing authority, and changes nothing when it refuses.

| Book | Procedure |
| --- | --- |
| Open, flat | Stop, `planner-recover`, `resume`, start |
| Open, model-led, holding positions | Stop the service, `planner-recover --protection-only`, start; when flat, run the open flat procedure |
| Closed, flat | Stop the service, `planner-recover`; no resume |
| Closed, holding positions | `--accept-halted-carry` rollover; when the successor is flat, run the open flat procedure |

Open flat book:

```sh
nivra stop --config PAPER_CONFIG --confirm-paper-review
systemctl --user stop nivra-paper.service
nivra planner-recover --config PAPER_CONFIG --confirm-paper-review
nivra resume --config PAPER_CONFIG --confirm-paper-review
systemctl --user start nivra-paper.service
```

If the halt came from the policy, run `config-apply` before `resume`.
Recovery makes no model call and no fill, and leaves the book stopped.

`--protection-only` clears the operator stop and keeps the halt. Held positions
keep fresh two-second quotes and their exit policy.

On a closed flat book, `planner-recover` clears only the halt. The book stays
closed, and its cash, fills and report do not change.

Some failures block the planner without a halt.
`planner_read_unavailable_no_replay` (transport, HTTP or quota failure) clears
by itself after a validated fresh quote. Data, news, configuration and
interrupted-attempt blocks stay until `planner-recover` or `config-apply`. A
failed pi turn, such as a provider error or an invalid plan, does not block the
next turn.

### Interrupted history attempt

The controller never retries an interrupted history preparation. With the
controller stopped, settle it as failed:

```sh
systemctl --user stop nivra-paper.service
nivra history-recover --config PAPER_CONFIG --confirm-paper-review
systemctl --user start nivra-paper.service
```

It settles the current session's latest incomplete attempt and writes an audit
receipt. The controller may then start a fresh attempt inside the window.

## Diagnose a book with no fills

Check the book's stop, halt and closed flags, then `nivra readiness` and the
lane budgets, then history coverage. Then read `model/agent/triggers.ndjson`,
`turns.ndjson` and `plans.ndjson` to see why a turn did not run or which plan
was refused.

The `execution_refusal` table in `paper.sqlite`, and each turn's
`model/<decision>/execution.json`, say why the execution gate refused an
action and which numbers it compared. An accepted empty plan is a model
abstention.
`nivra agent-turn --config PAPER_CONFIG --lane decision --dry` prints the
current brief and tools without calling a model.

## Reconstruct a closed day

The service journal can rotate before a closed day is reviewed. Each session
directory keeps its own `diagnostics.ndjson`, append-only and capped at 1 MB;
past the cap one `diagnostics_capped` line ends it. Each line has `at_ms` and
`event`:

| Event | Written when | Fields |
| --- | --- | --- |
| `startup`, `session_adopted` | The controller starts, or adopts the next session at rollover | Release version, session, process ID, book flags and health, position and fill counts, whether pi lanes run |
| `health_transition` | Book health, halt, stop or close flags, the read failure or the feed mode change | The new values |
| `failure` | The pi lanes fail a pass, or the paper runtime exits on an error | `stage` and a static `reason` code |
| `shutdown` | The controller settles a stop | Stop kind, book flags, whether a decision was in flight |
| `secret_guard`, `research_request_failed` | See [model lanes](AGENT.md#diagnostics) | |

Lines hold codes, counts and flags only, never tokens, account payloads, host
paths, quotes or model text, and each passes the transcript redaction. Writing
the file never stops the controller. A failed write prints
`diagnostics_write_failed` to the service journal. The file is new beside the
book; the book, its journals and stopped or halted state are not changed.

## Releases and updates

Releases come from this repository only, and a push to `main` does not deploy.
See [development](DEVELOPMENT.md) for tagging. The updater config names the
paper config and the Git remote, which must use the `github-nivra:` SSH host
alias. See `packaging/rust/updater.example.json`. Preview and install:

```sh
nivra update --check --config UPDATER_CONFIG
nivra update --dry-run --config UPDATER_CONFIG
nivra update --tag vX.Y.Z --config UPDATER_CONFIG
```

`--check` lists releases. `--dry-run` fetches and verifies without activating.
Without `--tag` or `--sha`, the updater picks the newest stable tag. An older
`--tag` is a rollback with the same checks. Never move `~/nivra-current` by
hand.

The updater verifies the annotated tag, source commit, target and SHA-256. It
runs the new binary's `self-check` and `preflight`, stops the service, swaps
the pointer, restarts and checks `/api/v1/health`. On a failure it restores the
previous release and writes `failed-update.json`, which stops that release from
being retried.

Activation needs all of these:

- No `~/nivra-deploy/HOLD` file, and a clock that reports NTP synchronization.
- A time outside the weekday 09:00–15:45 IST blackout, unless the NSE calendar
  says the market is closed.
- A closed, flat book that is not halted. A closed model-led book that carries
  positions overnight also qualifies, if nothing is pending.
- The installed binary passes `preflight` on the current config, because it is
  the rollback target. If not, the update refuses with
  `update_rollback_config_incompatible`.

Otherwise the updater defers with a reason such as `deferred_market_open` or
`deferred_open_positions`. A flat book that is not closed yet, while the NSE
calendar says the session is not trading, defers as
`deferred_open_book_outside_session`. This happens in the 09:00–09:15 IST
pre-open and after 15:30.

`--force-unwatched-window` accepts a short unwatched restart of an open book
and records the reason. The same session continues. A flat open book is
admitted anywhere in the blackout: as `market_open_flat` while the session
trades, and as `outside_session_flat` in the pre-open or after 15:30. A book
with positions or pending orders is admitted only as before. Force never
bypasses a halt, a hold or an integrity check.

To deploy to an operator-stopped book or inside the blackout:

```sh
nivra stop --config PAPER_CONFIG --confirm-paper-review
systemctl --user stop nivra-paper.service
nivra update --tag vX.Y.Z --operator-stopped --confirm-paper-review --config UPDATER_CONFIG
```

The book must be operator-stopped and not halted, or closed and flat. The
service stays stopped. Recover or resume afterward with the new release.

| File in `~/.local/state/nivra/` | Meaning |
| --- | --- |
| `update.json` | Last updater status |
| `preflight-failure.json` | Last failed candidate preflight; a later pass clears it |
| `update-recovery.json` | The previous release could not be restored; a critical alert is sent |
| `pending-review.json` | An interrupted update needs inspection |
| `git-diagnostic.json` | Redacted Git failure details |

Keep these files and every release directory.

To add config keys, install the release that understands them first, with the
old config. Then stop the controller, apply the change, and run the new
binary's `preflight`. If the config already has keys the installed binary
cannot parse, restore the last compatible config, install, then reapply.

## Publish a release by hand

Use this only when GitHub Actions cannot publish. Run it on a development host
after the full gate in [AGENTS.md](../AGENTS.md) passes on the tagged commit.
Push the annotated tag `vX.Y.Z` and check it out with a clean tree. Install
`musl-tools`. Build with the commands below, then copy `nivra-analysis` to
`dist/nivra-analysis-x86_64-unknown-linux-musl` with its `.sha256`. The
empty-ref check and `--force-with-lease` make a second publish fail.

```sh
rustup target add x86_64-unknown-linux-musl
target/debug/nivra-tools check-release
cargo build --release --locked --offline --target x86_64-unknown-linux-musl -p nivra-cli
target/x86_64-unknown-linux-musl/release/nivra self-check --expected-version vX.Y.Z
target/debug/nivra-tools release-artifact --tag vX.Y.Z \
	--binary target/x86_64-unknown-linux-musl/release/nivra --output dist
# Add nivra-analysis and its .sha256 to dist/, then publish the asset ref:
asset_ref=refs/release-assets/vX.Y.Z
test -z "$(git ls-remote origin "$asset_ref")"
index=$(mktemp); rm "$index"; export GIT_INDEX_FILE="$index"
git read-tree --empty
for file in nivra-x86_64-unknown-linux-musl nivra-x86_64-unknown-linux-musl.sha256 \
	release.json nivra-analysis-x86_64-unknown-linux-musl \
	nivra-analysis-x86_64-unknown-linux-musl.sha256; do
	git update-index --add --cacheinfo "100644,$(git hash-object -w "dist/$file"),$file"
done
export GIT_AUTHOR_NAME=nivra-release GIT_COMMITTER_NAME=nivra-release
export GIT_AUTHOR_EMAIL=release@users.noreply.github.com
export GIT_COMMITTER_EMAIL=release@users.noreply.github.com
commit=$(git commit-tree "$(git write-tree)" -m "Assets for vX.Y.Z")
git push --force-with-lease="$asset_ref:" origin "$commit:$asset_ref"
rm "$index"
gh release create vX.Y.Z dist/* --verify-tag
```

Check the attachments, the asset ref, the source commit in `release.json` and
every SHA-256 before the updater installs it. Do not also run the GitHub
workflow for that tag. Published tags never move.

## Dashboard and alerts

The paper controller serves a read-only dashboard on `127.0.0.1:8771`. Enter
the token on the login page, never in a URL. It has no trading controls. See
[dashboard](DASHBOARD.md). Run `nivra-dashboard.service` only while the paper
service is stopped, because both use the same port.

To rotate the token, run `nivra dashboard-token rotate --config PAPER_CONFIG`
and restart the service. Without `--token`, it generates a random token.

Alerts go to the HTTPS receiver in the private file that `alert_webhook_file`
names. Without the key, no alert is sent. Events include halt, stop, restart
loop, rollover blocked, missed pre-open, stale feed, readiness blocked, model
credential unavailable and updater failure. Alerts wait in a local outbox of up
to 64 messages and retry with backoff, so a receiver can get a duplicate.
`nivra alert --test --config PAPER_CONFIG` sends a real notification. A dead
host sends nothing, so watch it from outside as well.

## Model archive retention

Retention packs or deletes old model run directories. It is off until you set
`dashboard.retention.pack_after_days` or `delete_after_days`, each 1 to 365,
with delete greater than pack. The controller runs it after the close. To run
it by hand:

```sh
nivra retention --config PAPER_CONFIG --dry-run
nivra retention --config PAPER_CONFIG --apply
```

Only past sessions whose report is closed, not stopped and not halted qualify.
Packing writes `model-archive/<date>.tar.gz`. Transcripts stay readable on the
dashboard.
