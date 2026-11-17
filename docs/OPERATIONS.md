# Operations

**Paper only.** Use application-owned credentials; never print tokens, dump service
environments or edit SQLite/hash-sealed files. Future work is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).
Commands below use placeholders or the conventional installation layout.

## Services and files

Units, the release pointer, the deploy directory and updater state use the
`nivra` names below. A host moved by the [host migration](#one-time-host-migration)
keeps its `~/ind-sadr-data` and `~/.config/ind-sadr` directories, because the book
binding hashes paths inside them.

| Item | Purpose |
| --- | --- |
| `nivra-paper.service` | `nivra paper --config PAPER_CONFIG` |
| `nivra-update.timer` / `.service` | Guarded update check every 30 minutes |
| `nivra-dashboard.service` | Optional research-only dashboard (Backtest lab) using the installed `nivra` |
| `PAPER_CONFIG` | Account/capital, original book/read journal, source/history/planner paths |
| `SESSIONS_ROOT/current.json` | Application-owned committed dated-book pointer |
| `<session>/paper.sqlite` | Original ledger, control and planner/review journals |
| `<session>/model/` | Inputs, results, usage receipts, identities and status |
| `<session>/report.json`, `review.json` | Sealed financial close/review evidence |
| `RELEASES_ROOT/vX.Y.Z-<commit>/` | Immutable releases; `CURRENT` selects one |

Inspect `systemctl --user status UNIT` and `journalctl --user -u UNIT` privately;
share only redacted outcomes. Tokens/grants/webhook settings stay private outside
Git, owned by the service user, single-link mode 0600 without symlink traversal.

For remote diagnostics, put a bounded timeout around the command on the paper
host; a local remote-command timeout can leave its child running. Prefer
authenticated status projections and bounded journal windows over recursive
model/history archive inventories during a session. After a timeout, identify and
terminate only the orphaned diagnostic process; preserve the paper controller and
existing downloads. Large archive walks can add substantial disk and swap
pressure on a small paper host.

The packaged service profile uses MemoryHigh 1G, MemoryMax 1280M,
MemorySwapMax 256M, TasksMax 32 and NOFILE 256, with read-only home, PrivateTmp,
ProtectSystem=strict and NoNewPrivileges. Shared research staging is private
mode-0700 `RESEARCH_STAGE`; service-private `/tmp` cannot serve that
purpose.

The effective limit is whatever systemd actually loads for the unit, not the
packaged profile alone. A previously installed unit or a drop-in can keep older
values, and drop-ins survive package upgrades. After installing a release,
operators must check the effective values and update or remove any stale
unit/drop-in that overrides the packaged profile. Check rather than trusting the
packaged file:

```sh
systemctl --user show nivra-paper.service \
  --property=MemoryHigh,MemoryMax,MemorySwapMax,TasksMax,LimitNOFILE
```

## Current configuration baseline

For a fresh configuration baseline, prepare new paper and planner files
outside every original session directory. Retain predecessor configs and immutable
release evidence unchanged. Installation,
authority application and a paper start are separate operator actions.

Review the complete set together:

| Input | Required current baseline |
| --- | --- |
| Paper config | Explicit broker, account, session, original read-journal and sessions/history/source paths; continuing account cash comes from its original ledger, never a replacement journal |
| Costs and instruments | Complete `paper_costs` (see packaged fragment), verified cash instrument identities/tick units, calendar and provider metadata; no derivative execution |
| Planner | Owner-only policy (no `decision_owner`, `decide` or `handoff_gate` key); factual Digest enabled; events and supervisor enabled; Operations/Strategy/post-close disabled by the two-role migration |
| Models | Digest `gpt-6-luna`; reviewed owner profile for `grok-4.7` or `gpt-6.1-sol`, with exact model/transport/application credential binding and explicit admission; catalog presence does not establish entitlement |
| Prompts | Complete revision-matched `prompts/` beside the planner JSON; reviewed prompt bytes participate in authority; see [model harness](MODEL_HARNESS.md) |
| Credentials | Application-owned private grants and a live-catalog model ID; never reuse another application's authentication or put credentials in templates/history |
| Limits | Explicit reviewed role allowances, timeouts, tool/output limits and source bounds in config; no new trading risk caps or silent provider fallback; the explicit limit fallback is separately reviewed |

Choose each existing book's disposition before applying any new policy. Archived
books remain read-only with their original config, journal, usage and failed
artifacts. Reviewed continuation uses the original book and authority transition;
it never creates a replacement ledger, resets allowances, replays an ambiguous
attempt or clears stop/halt flags. A fresh config is not a fresh account.

Use `planner config-check --planner NEW_PLANNER --session SESSION` for offline
planner validation; it reports owner prerequisites. Source/fixture
validation does not establish credential entitlement or installed-host readiness.
Before any separately authorized paper start, validate the original authority,
self-check and preflight under the reviewed configuration. Preserve stopped or
halted state; a failed check is evidence to retain, not permission to restart.

Legacy configuration decoding is retained for archive interpretation and reviewed
migration only; enabled independent legacy planning is refused at runtime.
Older updater delivery compatibility remains separate from active planner policy.
Use the [current setup templates](../packaging/rust/CONFIGURATION.md) for the
complete input set and compatibility inventory. Target-host preparation and
qualification remain separate.

## Startup and daily flow

For an explicitly requested fresh account, use the Rust
[`setup` workflow](../packaging/rust/CONFIGURATION.md#explicit-fresh-paper-installation).
It prepares the current owner policy in unused private config/state directories,
preserves original evidence and shared quotas, and does not activate a session.
Keep the old installation archived and the new paper service stopped after
installation. Future updates use the canonical updater and stable new config;
do not replay setup, reset a journal, or restore independent legacy planning to
work around `decision_owner_migration_required`.

Supply explicit account/broker/session identities, positive capital, original
read-journal binding, source credentials, sessions/history directories and planner
path in ordinary config. Legacy `directory/session/previous_session` fields remain
bound to their original book; do not rewrite them each morning. The runtime follows
its committed current pointer under a root-wide writer lock.

| Phase, IST | Behaviour |
| --- | --- |
| Pre-open | Validate calendar, predecessor/carry, dated metadata, model settings and history |
| 08:00–09:10 | Optional prior-session daily-history preparation under original read quota |
| Before 09:15 | Commit the next eligible session; missed preparation does not create a late replacement |
| 09:15–15:30 | Protect holdings, rotate scans, compile factual Luna digests and run persistent orchestrator decisions; independently validate paper fills |
| Close | Seal/report actual cash, fees and unresolved/carried holdings; no forced 15:15 liquidation for model-led holdings |
| After close, before 18:00 | Seal financial reporting; Operations/Strategy/post-close remain disabled in the current two-role baseline |

An open operator-stopped book is expected protection: startup diagnostics,
stop alerts and `/api/v1/health` book / summary state classify it as
`expected_not_failure`. It still requires explicit reviewed resume; installation
never clears the stop. Halted or incomplete/unknown stop evidence remains an
operator-review condition, separate from updater preflight or rollback failure.

A service restart preserves the same dated book, stop/halt flags and original
attempts. A normal model-led account carries actual cash/holdings across verified
successors. Closed books are never resumed. No replacement intraday book, quota
reset or automatic unhalt is a recovery procedure.

The reviewed compiled 2026 NSE calendar is bounded; holidays and previous-session
selection come from it. Unreviewed special hours/dates refuse. Calendar/source
coverage is evidence, not an inferred provider promise.

## Paper-cost configuration

Set the complete `paper_costs` object in the runtime paper config before using
this source version; see [fees](STRATEGY.md#fees) and the packaged paper-cost
fragment. Missing costs refuse validation. A same-session book retains its
recorded policy: a changed or absent stored policy refuses with
`paper_cost_policy_changed`. Review the existing book and any migration separately
before installing this change; never rewrite the original journal or silently
assign a new policy to an existing session. New verified successor books use the
configured policy, but changing the source config before rollover can also change
its book binding.

For a reviewed cost-only change to the continuing model-led account, save the
source session's complete config outside the session directory, set the new
`paper_costs` in the runtime config, and add:

```json
"paper_cost_transition": {
  "source_session": "2026-10-16",
  "prior_config": "/absolute/path/to/paper-20261016.json"
}
```

The prior config is read-only evidence: every field except `paper_costs` must
match the new config, its journal device/inode must match, and the source book
must be closed, non-stopped, non-halted and model-led. Carried positions are
allowed; an open source book is refused. The source book keeps its original
binding and same-session policy; a same-session restart with the new policy still
refuses `paper_cost_policy_changed`. At the verified rollover, the successor keeps
cash, positions and fills but uses the new policy for new fills; existing fills
retain their recorded fees. Legacy books without a stored policy can use the same
transition, but their legacy fee arithmetic remains unknown. The transition
is evidence for the named source session and is not serialized into the policy
binding. Keep it in place until the successor book has been written, then remove
it with the next reviewed config change. Do not use this to mutate a stopped,
halted or open book.

### Stopped-controller offline advance

After the reviewed source book has closed and the controller is stopped, complete
an already-validated rollover without starting the service. Preview first, then
advance explicitly:

```sh
nivra rollover --config PAPER_CONFIG --now 2026-10-28T09:00:00+05:30 --dry-run
nivra rollover --config PAPER_CONFIG --now 2026-10-28T09:00:00+05:30 --advance --confirm-paper-review
```

`--now` is mandatory; the command never samples the wall clock. `--advance`
requires `--confirm-paper-review`, takes the root controller lock before the
selected source book controller lock, validates the config (live activation
remains disabled), and requires `preview.status == rollover_ready`. Run it on the
target session date before the 09:15 IST pre-open gate; the same clock rule as
the controller applies. It advances at most one transition, from the session
committed in `current.json` to the `--now` session, and runs the same transition,
closed-book and carry validation and the single rollover commit used by the
controller, then prints JSON evidence containing `status`, source and target
sessions, bindings and migration kind. The source binding, fills and fees remain
unchanged; the successor uses the configured costs for new fills. The command
never starts the service, opens a market connection, places orders, mutates
source fills/state or replays a failed attempt.

The command resolves its source book from `current.json`, not from the config's
`session` field. Keep the reviewed config and its original identity: it may still
name the original session after later sessions started, and each verified
successor becomes the source for a later invocation. Do not edit the pointer or
rewrite the config to force a source.

If `current.json` already points at or past the `--now` session, the command
returns `status: "already_advanced"` with source/target evidence and changes
nothing. This includes a service start that already performed the reviewed
rollover and a repeated invocation. Do not point the pointer back or rerun an
older config to force a second advance. Refusals print
`rollover_advance_refused_original_state_preserved` with a stable reason code.
The normal rollover review still owns closed-book, carry, halt and transition
admission; this command never clears a source stop or halt. A halted book needs
the reviewed `--accept-halted-carry` evidence first.

## Preflight and readiness

For an explicit manual catch-up of the current config's required daily history,
run `nivra history-backfill --config PAPER_CONFIG`. It uses the original shared
quota and configured history root, validates/reuses raw receipts, and coalesces
missing reviewed trading sessions. Old descriptive replay manifests are preserved
and are not rebuilt by operational acquisition. Automatic pre-open preparation
uses the same receipt-only mode while retaining its cutoff/deadline. Empty or
unavailable observations remain gaps; inspect readiness after the pass. See
[historical candles](HISTORY_DOWNLOAD.md).

```sh
nivra self-check --expected-version vX.Y.Z
nivra preflight --config PAPER_CONFIG
nivra readiness --config PAPER_CONFIG
nivra rollover --config PAPER_CONFIG --now RFC3339_TIME --dry-run
```

Self-check accepts a version with or without `v` and prints the failed check.
Preflight checks writable paths using a private temporary probe, without credentials
or opening the read journal. Under systemd, it also checks effective writable paths;
use `INVOCATION_ID=manual` for that check before activation. `--unit-file FILE`
checks one explicit unit file instead. This is admission, not sandbox qualification.

Readiness is read-only: it never refreshes credentials, calls models, creates
namespaces or repairs state. It reports grant coverage, history and rollover
separately. Its exit code checks `models.status`: success means
`model_roles_ready`, including when history coverage is PARTIAL. It does not
certify overall history/lifecycle readiness; inspect those fields separately.
Older snapshots without `models` may use top-level `model_status`.
Future refresh/unestablished usage may be UNKNOWN;
authenticated status alone does not establish session coverage. Readiness also
reports each configured role's optional limit fallback (`fallback`) with a
static status and reason; a blocked fallback does not change the primary role's
status or the model-readiness exit code.

Preflight and readiness also read every `research_tools.registrations` file from the
resolved planner config and compare its bytes with the registered SHA-256. A
missing or unreadable file fails preflight with its own code
(`research_code_unreadable`, `research_runtime_unreadable` or
`research_input_unreadable`); a changed file reports
`research_registered_bytes_changed`. Readiness adds a path-free `research` object
with `status` and per-artifact reasons, so a stale runtime path is visible before
the post-close review call. A blocked research registration does not change the
model-readiness exit code. Re-register a stale runtime outside a session against
a retained release's `nivra-analysis` binary as described in the
[research worker](RESEARCH_WORKER.md#re-registering-a-stale-runtime), then rerun
preflight and readiness.

The reviewed decision-owner policy can queue registered research separately from
its decision turn. Its optional planner setting is:

| Setting | Default | Valid range | Meaning |
| --- | --- | --- | --- |
| `research_tools.task_ttl_ms` | 21,600,000 (6 hours) | 60,000–86,400,000 | Expiry measured from task creation, including queue time; the registered `wall_millis` separately bounds sandbox execution. |
| `research_tools.termination_grace_ms` | 30,000 | 0–300,000 | Additional time to obtain trusted whole-sandbox termination after cancellation or execution expiry; exhaustion retains an occupied, reconciliation-required attempt. |

Omitting the default retains the original serialized policy bytes. Changing this
setting follows the normal reviewed policy transition; it does not extend an
already queued task's recorded expiry. See [registered research](RESEARCH_WORKER.md).

Candidate compute additionally requires a running systemd user manager, cgroup
v2, `/usr/bin/systemd-run`, `/usr/bin/systemctl`, `/usr/bin/bwrap`,
`/usr/bin/python3`, a usable `XDG_RUNTIME_DIR` user bus, a readable machine
identity (`/etc/machine-id` or `/var/lib/dbus/machine-id`) and unprivileged user
namespaces. Readiness checks configured prerequisites only
(`readiness_scope: configured_prerequisites_only`): it verifies the executable
paths, cgroup v2 marker, user-bus path and machine identity, reports `configured`
when they pass, and never claims a live manager, user-namespace operation or
host qualification.
`runtime_verified` and `host_qualified` stay false until a separate host
qualification drill observes the actual unit/cgroup behaviour. The overall
projection reports `configured`, not `ready`, while candidate compute is
configured but runtime-unverified; a missing known prerequisite reports
`blocked`. It never advertises a weaker process-group fallback. The stage root must be private (0700), outside `/tmp`/`/var/tmp`
because the unit uses `PrivateTmp=yes`, and outside a forbidden home/root
component. Candidate units are `nivra-candidate-<lease>.service`; the controller
stops only that original unit and releases the lease only after the typed
unit/cgroup proof. A retained `candidate_recovery_required` lease is never
replayed or reset; inspect the original unit and result receipts before any
reviewed recovery. The hidden `nivra candidate-supervise` command is not an
operator workflow and accepts only the staged launch manifest; it claims a
one-run marker before reading source, so a unit restart refuses instead of
executing the payload twice. The reviewed `max_result_bytes` cap is recorded in
the task and enforced on the helper output, envelope and artifact; an oversized
result is a bounded terminal failure, not a success.

The optional `research_tools.candidate_compute.execution` block enables the
after-cost cash replay. It is absent by default; omitting it keeps the existing
policy bytes, tool schema and ranking-only behaviour unchanged. A reviewed
enablement is configuration, for example:

```json
"execution": {
  "enabled": true,
  "abi": "cash_quote_action_tape_v1",
  "max_quote_events": 2000,
  "max_observations": 20000,
  "max_tape_bytes": 4194304,
  "max_entries": 100,
  "max_capital": "100000",
  "max_cost_stress": 4
}
```

Only when it is enabled does `candidate_submit` advertise the nullable
`execution` JSON-string argument. The submit freezes the executable quote tape
from the original read-only paper quote journal at the submission clock and
stores it separately from the scorer dataset; a later exit quote is never
fetched. Scanner/OHLC/compacted evidence alone returns
`candidate_execution_quotes_unavailable` and creates no task. The fixed helper
runs the existing scorer replay, then the trusted Rust adapter over the staged
tape, and reuses the existing cash engine for base and stress costs. The result
kind is `executable_replay`; `execution_unavailable` is distinct and carries no
cash/PnL summary. All execution results are retrospective research evidence and
never authorize a broker order, promotion, activation, prospective shadow or
account-funds claim.

Rollback is configuration-only: remove the block or set `enabled: false`.
Existing tasks, results and lifecycle records remain readable from their
recorded hashes; do not reset the queue, delete artifacts or migrate the book.
The readiness projection reports the execution capability separately and keeps
`runtime_verified` and `host_qualified` false.

For #7, the running paper controller checks readiness every
`model_readiness_check_interval_ms` (default 60000; must be positive). With alerts
configured, enabled roles whose credentials fail validation or whose future
refresh is unverified emit `model_credential_unavailable` warnings throughout the
configured session date, including after market open. These warnings are
independent of usage availability and deduplicated per role and failure category
by the existing delivery policy. A blocked role keeps its real stable reason code
instead of a generic credential failure. Only static role/category details are sent.
xAI refresh-attempt failures also immediately enqueue `model_refresh_failed`
(critical), with only static provider/reason/action details. No error bodies,
account identifiers, tokens or paths enter that alert. The paper planner uses
its existing `alert_webhook_file`; a model config may also set optional
`alert_webhook_file` for standalone model commands (otherwise the existing default
alert destination applies). Delivery retains the configured timeout, dedupe and
admission limits; an unavailable receiver is not proof of delivery.

A successfully validated, same-account xAI refresh records `refresh_verified_at`
only with the durable authenticated revision. Readiness can then report validated
refreshability beyond the current access-token expiry, provided verification is
at most 36 hours old at the evaluation time, matches the revision's update time, and the grant
covers the entire requested window. This is evidence that refresh worked, not a
provider guarantee about the next attempt. Legacy records without that evidence
remain UNKNOWN beyond expiry. An in-progress/failed refresh, stale evidence or
future-dated verification cannot establish readiness. Readiness itself never
contacts the provider or attempts refresh.

The packaged unit grants stable data/config/state parents. Create intended
sessions/state directories once; custom paths need explicit `ReadWritePaths`
drop-ins and `systemctl --user daemon-reload`. Do not create replacement journals
or move their original device/inode identity to satisfy preflight.

## Planner configuration

The paper config points to ordinary planner JSON. Explicit enabled roles need
exact model IDs, application model configs and positive daily budgets. Set
`model_led:true` and `require_digest:true` for the current model-led flow.
`planner.context.mode` defaults to `tools_only`: Luna receives only a trigger
envelope and Grok receives only the trigger and Luna's validated digest/handoff.
Set `"context":{"mode":"projection",...}` only to reproduce the v2 rollback
message; projection still requires `full_universe_features:true`. Deployed
configs may retain `context` and `full_universe_features`; tools-only validates
but does not paste them.

`planner.tools` bounds call-time reads and defaults are omitted from policy bytes:

```json
"tools":{"scanner_limit":40,"ranking_limit":50,"history_limit":5,"max_instrument_keys":20}
```

`operator_context` supplies up to 8192 characters of task context; it cannot
authorise live orders, unsafe fills or credential/service tools.
`orchestrator.triggers.supervisor.failure_backoff_ms` is optional
(60,000–3,600,000 ms). It defers new owner turns with
`supervisor_failure_backoff` only for the same model-selection profile/revision
after actual supervisor/decision failures; review, compaction, aside and host
lifecycle failures do not arm it. Omitted keeps the old policy/config/state
bytes and the broad failure-streak wake behavior.
[Replay/flow](PAPER_REPLAY.md) contains the discovery/allowance example;
[harness](MODEL_HARNESS.md) explains accounting and tools.

Prefer dated configuration for the next session:

```json
{"effective_from":"2026-10-23","previous_config":"/absolute/original-planner.json"}
```

These fields belong in a complete new planner config retaining actual roles,
credentials and policy. Before the effective date, resolution uses the untouched
previous config; it detects missing pairs/cycles. Point only the ordinary paper
config at the new file after review, and compare resolved policies:

```sh
nivra planner config-check --planner NEW_PLANNER --session 2026-10-22
nivra planner config-check --planner NEW_PLANNER --session 2026-10-23
```

For a reviewed **current flat, non-halted, open book**, stop the controller, edit
only ordinary planner config, then:

```sh
nivra config-apply --confirm-paper-review --config PAPER_CONFIG
nivra resume --confirm-paper-review --config PAPER_CONFIG
systemctl --user start nivra-paper.service
```

`config-apply` takes controller/book locks, validates and records old/new policy
and complete configuration hashes, supersedes the old plan and leaves the book
stopped. It refuses closed/halted/non-flat books, pending attempts, planner blocks
or changed/missing usage authority. It also refuses an enabled model role blocked
by a missing or invalid grant, so the last valid authority and
config stay in place instead of an unauthenticated model becoming active. Model-led
conversion additionally needs a pristine flat book. An initialised namespace
cannot move. Never hand-edit a sealed policy or rely on an arbitrary restart to
adopt trading-policy changes.

The original session namespace is bootstrapped/pinned during authorised controller
initialisation before model work. `usage_namespace` is a validated 1–64 character
name under `model/`; omitted means `usage`. Never mkdir/copy/reset pins or receipts
to repair readiness. Failed provider/transport/interrupted model attempts remain
recorded in the usage journal and failure artifacts for audit but do not consume the
daily call allowance; successful model calls still count. No usage reset, refund,
deletion or journal replacement.

## Enabling the decision-evidence policy

The optional planner fragment
[`decision-evidence.fragment.json`](../packaging/rust/decision-evidence.fragment.json)
adds intraday context and the uncertainty instructions described in the
[model harness](MODEL_HARNESS.md#opt-in-intraday-decision-evidence). Defaults:
20 completed price bars, 20 retained scanner samples, return windows `[1,5,15]`,
180,000 ms price age, 300,000 ms metadata age and 200 benchmark contributors.
Bars and sample limits are 1–20; one to six unique return windows may each be
1–60 minutes; ages must be 1–86,400,000 ms and contributors positive. Volume-proxy
parameters use the existing validated strategy-feature settings. These are
screening/resource parameters and never replace the two-second financial gate.

Install a supporting annotated release through the normal updater before adding
this key: v0.8.7 and earlier cannot parse it, while v0.8.8 does. Merge the
fragment into a complete reviewed planner config, preserving identities,
approvals, allowance namespace and other policy. Use dated configuration for
the next session, or the supported stopped
controller `config-apply`/`resume` procedure for an eligible reviewed open book.
Closed books are not resumed; use the existing reviewed successor/rollover path.
The option and instruction hashes change policy bytes, so do not silently edit
an active same-session policy or add the key before its supporting binary is
installed. Keep the rollback binary's compatible config available under the
normal updater guards. No existing book, journal, usage or failed request is
replaced by enabling this policy. Source tests do not qualify a real session.

## Diagnosing a running book with no fills

Separate model choice from execution refusal. Check current book stop/halt/close
flags, model readiness and remaining allowances, feed/scanner eligibility and
history admission first. Then inspect the original ledger read-only: accepted
`planner_plan.body.parsed_plan.actions`, its `verdicts`, `planner_action` verdict
counts and failed/deferred/planned attempt counts. Share aggregates and bounded
reason codes only; never dump book bodies, account context or model requests.
Use the authenticated run detail for the corresponding retained model explanation.

An accepted empty action list is a model abstention and has no execution verdicts.
It does not establish stale-quote, affordability or price-limit rejection. A
`model_plan_unavailable_no_trades` dashboard label alone does not diagnose why:
a later deferred event can clear the active plan while preserving earlier
accepted abstentions. Distinguish current readiness from earlier failed attempts,
and do not count repeated log lines as new calls. PARTIAL history is admitted
under the documented conditions; it is not itself proof that inference is blocked.

Do not manufacture a trade to verify the pipeline. Compare a proposed prompt or
strategy against preserved genuine evidence offline, retaining costs, timestamps,
missing observations and original outcomes. Prompt changes belong to reviewed
policy/release handling; do not silently replace same-session model instructions.
Future work is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

## Live strategy-selection ranking (paper only)

Discovery normally runs the strategy selector in `shadow` mode: it records which
strategy it would choose but does not change ranking. The opt-in live mode lets
that selected strategy's scorer rank scanner prospects in paper:

```json
"strategy_selection": {
  "enabled": true,
  "mode": "live",
  "library_path": "/absolute/strategy-library.sqlite",
  "evaluate_ms": 60000,
  "regime": {},
  "selector": {}
}
```

`mode` defaults to `shadow` when omitted, so existing configs keep the old
diagnostic-only behaviour and the old trading-policy hash. Live requires an
enabled selector, a nonempty library path, no enabled Tier 1
`discovery.strategy` champion, and a valid selector/regime configuration. The
runtime reads the exact chosen source by SHA from a read-only library, scores the
current fresh scanner snapshot in the existing bwrap/seccomp sandbox, and ranks
the existing scanner prospects by descending finite score with instrument-key
tie-break. Unscored prospects are omitted before the normal shortlist limits.
This only changes the paper scanner shortlist used for candidates, triggers and
depth polling; orders, validation, risk controls and Grok's decision are
unchanged. `LIVE_ENABLED` remains false.

Live ranking fails closed to the default ranking on unknown regime, stale data,
scorer/library failure, paper-floor breach, invalid config, missing or invalid
source, sandbox failure/timeout/output-limit breach, invalid scores, no scored
prospect, or when the selector keeps the default identity. The controller logs
`strategy_selection_fallback` with a stable reason/detail, attempted identity,
previous active strategy and regime; recovery logs
`strategy_selection_recovered`. Dashboard discovery status exposes `mode`,
`chosen_id`/`chosen_name`, `previous_active`, regime, `switched`, `live_applied`,
score count, bounded fallback/recent-switch evidence, and scorer/stale flags. Do
not paste source or model output into incident notes from these logs.

Turning live mode on or off changes trading-policy bytes (shadow/absent selection
settings stay excluded). Treat it as a policy change: review the resolved config,
use `config-apply --confirm-paper-review`, and follow the normal stop/resume
procedure; never hand-edit a sealed policy. This feature is paper-only and must
never be used to enable live trading.

## Event and history behaviour

Discovery adds scanner-refresh events; configured cooldown coalesces work rather
than forcing inference. Luna chooses whether a new Grok decision is warranted.
Optional deterministic price/volume/gap/proximity/fill/news detectors retain their
own configured thresholds, with no enabled signal defaults. News requires an
explicit bounded dated/cited archive, not automatic collection.

`events.decision_timeout_ms` is 30,000–900,000 ms from admission. Queue observation
age governs admission separately; it does not consume the admitted model lifetime.
Latest covered rechecks collapse with retained rows; fills/news/initial identities
are not silently discarded. A plan needs execution-time quotes regardless of how
long inference took. Provider failure/timeout/schema errors produce event no-trades,
not book halts. Integrity failures remain distinct.

Failed provider attempts retain a bounded `reason`/`provider_reason` from the
allowlisted model validation codes (for example `model_stream_incomplete`,
`model_http_502`, `planner_response_model_mismatch`, `planner_output_cap`) and a
`provider_stage` such as `transport`, `http_response`, `response_validation` or
`output_validation` in `failure.json`, with the numeric HTTP status when present.
`planner_output_cap` bounds the serialized provider envelope (`output`, `model`,
`usage`, including encrypted reasoning) at `output_cap × 64` bytes (512 KiB for
the paper decide `output_cap` of 8192). Empty output still fails.
Non-timeout planner block status stays `planner_provider_failed`; timeouts keep
their existing status. Raw provider bodies, headers, credentials, account context
and model text are never retained; unknown errors fall back to the generic status.

Scanner samples carry optional `avg_daily_volume` and `avg_daily_volume_sessions`
from validated daily receipts within the prepared 21-session window.
`history_avg_daily_volume_min_sessions` defaults to 10; missing or below-minimum
coverage leaves both fields absent. Available sessions are averaged without
filling gaps. This plumbing does not affect ranking.

`history_preparation_enabled:true` acquires missing prior-session daily spans
pre-open without blocking protection. On market-hours restart, cached coverage is
verified read-only. At least one fully covered instrument and completed READY or
PARTIAL preparation are needed when enabled. Missing bars remain missing.
`history_skip_unavailable:true` records specific HTTP-400 ranges as exclusions,
without retrying them as a new allowance. Other per-instrument fetch failures
(timeouts, 5xx) skip that instrument and continue, up to
`history_preparation_max_fetch_failures` (default 25); the report records
`history_fetch_failures_skipped:N`. One more failure, cancellation or the 09:10
deadline stops preparation. Authentication, quota/journal integrity and malformed
response failures stop immediately; the skip allowance covers only transient
transport errors, HTTP 429 and HTTP 5xx. A completed PARTIAL pass retries only missing ranges
before 09:10, using the original shared quota and immutable receipts.
`history_preparation_max_attempts` defaults to 3 (0 disables acquisition), with
`history_preparation_retry_ms` defaulting to 60000 between PARTIAL passes.
Completed FAILED attempts also retry within the same attempt budget and preparation
window, using `history_preparation_failed_retry_ms` (default 60000, positive),
including failures that may not be transient. A cancelled or deadline-stopped
pass is FAILED with its specific code (`history_preparation_cancelled`,
`history_preparation_deadline`, or `history_preparation_cancelled_or_expired`)
so remaining budget can retry before cutoff. The configured
`history_preparation_cutoff_minute` (default 550, 09:10 IST) stops admission
and cancels active work before market open. Failed attempts and their artifacts
remain recorded; retries do not reset the shared quota or journals.
BLOCKED/UNKNOWN results and admission failures require review and are not
automatically retried.
Coverage includes `missing_sessions` per instrument. After the cutoff the dashboard
retains `last_attempt` and its failure beside current read-only coverage, plus the
durable attempt count. `history_preparation_check_interval_ms` (default 60000,
positive) controls read-only coverage refresh after the cutoff. Each session's
`history-attempts.json` uses the existing atomic durable writer and binds the
account/session; it retains all attempt start/completion times and reports.
Claims are persisted before worker admission as `IN_PROGRESS` (not BLOCKED).
An incomplete claim, admission failure, BLOCKED/UNKNOWN result or invalid state
cannot trigger automatic replay;
restarts retain the count, original backoff deadline and diagnostics. Missing state
supports older sessions; invalid/unreadable or mismatched state fails closed.
Redacted per-range `*.failure.json` artifacts retain fetch errors even when a pass
aborts. Restarting reuses archived receipts; no allowance or journal is reset.
See [history](HISTORY_DOWNLOAD.md) for explicit downloads.

### Interrupted history attempt recovery

An interrupted claim is never retried automatically. After stopping the
controller and reviewing the session, settle the current session's latest
incomplete attempt (`completed_ms:null`) whose status is `IN_PROGRESS` or,
for files written before that status existed, whose reason is exactly
`history_attempt_interrupted_no_replay`:

```sh
systemctl --user stop nivra-paper.service
nivra history-recover --config PAPER_CONFIG --confirm-paper-review
systemctl --user start nivra-paper.service
```

`--now RFC3339` is optional and defaults to the wall clock. The command validates
the paper config, takes the root controller lock before the current session
controller lock, and refuses a running controller or a live-enabled build. It
operates only on the current committed session. It requires a latest record with
`completed_ms:null` and either `IN_PROGRESS` or the legacy interrupted reason,
and refuses a completed attempt, a different incomplete reason, wrong
account/session, malformed or non-regular state, or an exhausted
`history_preparation_max_attempts` budget.
Refusals print `history_recover_refused_original_state_preserved:` with a stable
bounded code and change nothing.

Successful recovery preserves the original start, keeps the original report as
nested evidence, sets `completed_ms`, records `FAILED` /
`history_attempt_interrupted_recovered_no_replay` with a bounded operator-review
marker, and keeps the original count/sequence. It sets `retry_at` to recovery
time plus `history_preparation_failed_retry_ms`; normal admission may then start
a fresh attempt only inside the configured window and remaining budget. The
ambiguous request is not replayed. A deterministic audit receipt
(`history-recovery-<sequence>-<started_ms>.json`, mode 0600, atomic create)
records the account, session, previous and new attempt state, operator
confirmation and timestamp. A repeated invocation returns `already_recovered`
without a second state mutation. The command does not open or modify
`paper.sqlite`, read journals, quota receipts, fills or hash-sealed state, and
prints a redacted JSON summary to stdout.

This recovery path does not address memory configuration or prove that memory
pressure caused the interruption. Update any stale installed unit/drop-in
separately using the effective-limit note above.

Runtime timing keys are optional and retain the original millisecond defaults:

| Runtime config key | Default (ms) | Accepted range (ms) |
| --- | ---: | ---: |
| `universe_prepare_retry_ms` | 300000 | 1–86400000 |
| `idle_poll_interval_ms` | 15000 | 1–300000 |
| `market_data_transport.http_timeout_ms` | 7000 | 1–120000 |
| `storage.read_journal_busy_timeout_ms` | 1000 | 1–60000 |
| `dashboard.refresh_interval_ms` | 5000 | 1–300000 |
| `dashboard.model_feed_unavailable_grace_ms` | 5000 | Retired/ignored; 0–60000 validation retained for config compatibility |
| `dashboard.workers` | 4 | 1–16 threads serving dashboard connections |
| `dashboard.queue` | 16 | 1–256 queued connections before a busy refusal |
| `dashboard.static_max_age_s` | 86400 | Retired/ignored compatibility key; shell assets always revalidate (0–31536000 validation retained) |
| `dashboard.gzip_min_bytes` | 1024 | 0–1048576 bytes gzip threshold |
| `dashboard.history_read_max_bytes` | 8000000 | 1000000–64000000 bytes per model-history read |
| `dashboard.history_list_cache` | true | Retired/ignored; accepted to preserve deployed config bytes |
| `dashboard.sse_max_streams` | 3 | 0–16 concurrent SSE streams (0 disables SSE) |
| `dashboard.sse_heartbeat_ms` | 15000 | 1000–60000 |
| `dashboard.sse_write_timeout_ms` | 30000 | 1000–60000 |
| `dashboard.longpoll_hold_ms` | 25000 | 1000–60000 |
| `dashboard.transcript_max_bytes` | 2097152 | 65536–16777216 bytes per run transcript |
| `dashboard.transcript_text_max_bytes` | 16384 | 1024–262144 bytes per assistant-text event |
| `dashboard.transcript_args_max_bytes` | 4096 | 256–65536 bytes per tool-call arguments event |
| `dashboard.blob_slice_max_bytes` | 262144 | 4096–4194304 bytes per blob slice |
| `dashboard.failed_auth_per_minute` | 30 | 1–600 failed-auth token bucket per minute returning 429 when exhausted |
| `dashboard.retention.pack_after_days` | off | 1–365 days before raw run directories are packed |
| `dashboard.retention.delete_after_days` | off | 1–365 days; must exceed `pack_after_days` |
| `dashboard.model_prices.<model>` | empty | decimal strings (₹ per million tokens): `input_inr_per_mtok`, `output_inr_per_mtok` and optional `cached_inr_per_mtok` |

The worker/queue, static cache/gzip, history read/cache, transcript, model-price
and opt-in retention settings are active.
Other Phase-1 settings are validated ahead of their corresponding features.
Invalid numeric values and model-price decimals fail as
`dashboard_settings_invalid`; unknown keys fail strict config deserialization.

The last four keys live in nested `market_data_transport`, `storage` and
`dashboard` objects. The research dashboard reads dashboard refresh settings from
its optional `--model-config` runtime config; without it, defaults apply. Default
values are omitted on serialization to preserve existing sealed book bindings;
custom values participate in the binding. HTTP timeout applies to all runtime
market-data clients, including history and model snapshot reads.

## Stop, recovery and closed incidents

Stop and inspect before recovery. A halt's durable reason survives operator stop;
closed-book stop must preserve the sealed financial state. Review older binary
shutdown behaviour before maintenance: older releases may mutate a closed book.
Never use a general restart to clear stop/halt state.

```sh
systemctl --user stop nivra-paper.service
nivra planner-recover --config PAPER_CONFIG --confirm-paper-review
nivra resume --config PAPER_CONFIG --confirm-paper-review
systemctl --user start nivra-paper.service
```

Use this sequence for an eligible **flat open paper planner incident**. Recovery
validates original authority, last relevant halt history, current inputs and
usage; an older `operator_stopped` health projection alone cannot hide a planner
halt. Eligible pending claims receive audited failed/no-replay disposition.
Unrelated risk halts, missing/cross-book authority and non-flat open books refuse
with a failed-check reason. Recovery leaves the book stopped and preserves charges.
Apply reviewed config before resume if required; recovery itself makes no call/fill.
Under the decision-owner policy, recovery checks the enabled orchestrator supervisor
role rather than the disabled legacy Decide role. The same reviewed eligibility
checks apply to flat recovery and protection-only recovery. Recovery does not
migrate legacy authority or enable independent Decide inference.

Owner review artifacts written by this build are versioned canonical JSON
(`ReviewCompletedV1`). Older binaries reject them; this build still reads the
frozen legacy `Completed` shape. A failed review whose observed wall stepped
behind its start is terminalized once with clock-anomaly evidence, and the old
fully-bound failed `owner_review_clock_invalid` artifact is recovered in place
without rewriting bytes or replaying the model. Unknown or corrupt review
artifacts still fail closed.

Every recovery mode uses one shared typed block gate. A data, news or transient
read block requires the same input validation as a stopped recovery; an
unavailable or ineligible input refuses with
`planner_recover_input_unavailable` or `planner_recover_no_eligible_input` and
leaves the book, authority, attempts and usage unchanged. A configuration block
is resolved by the exact policy rebind, not by a no-op validator. Protection-only
and closed recovery retain a data, news or transient read block because the halt
or seal means no new decision can execute; they never clear an unknown sticky
block. An interrupted claim is retired only from its exact stored claim body and
preserves its charges.

Owner authority receipts are versioned separately from the planner event state.
Every new authority writes its orchestrator owner marker in the same
transaction, and `config-apply` and `planner-recover` rewrite it with the
control hash they bind. Startup preserves the original active head and any
stronger sticky data/news/configuration block; a pending claim is marked
interrupted without replacing that block or clearing the head. A journal that
predates the marker refuses as `planner_authority_owner_missing` and stays
archive-only; nothing reconstructs a marker. Do not hand-create or edit the
marker table.

Production recovery never infers the legacy Decide path. Legacy planner configs
remain readable for archives, but `planner-recover` refuses them with
`planner_recover_events_disabled`, and `config-apply` refuses a halted book. A
halted legacy book stays archive-only. The legacy recovery mechanics retained in
tests exercise archive and refusal behavior only.

A present table with a missing row also refuses as
`planner_authority_owner_missing`. A malformed receipt or one naming another
owner, including a stored `legacy` owner, refuses as
`planner_authority_owner_invalid`; a foreign binding, another version or a stale
hash refuses as `planner_authority_owner_mismatch`. These refusals leave the
book, report, charges and authority unchanged.

When the reviewed `owner_tasks.review` policy is enabled, the controller may run
one sealed owner review after the report is written. It is advisory: it cannot
place, size or block an order, change policy, or write the workspace. The
dashboard `owner_review` projection is read-only and reports `disabled`, `due`,
`busy`, `attempted`, `expired`, `parked` or
`awaiting_sealed_report`, and `invalid` with one of
`owner_tasks_settings_invalid`, `owner_review_cutoff_invalid`,
`owner_review_path_invalid`, `owner_review_binding_invalid`,
`owner_review_report_changed` or `owner_review_session_date_mismatch`. The first
sealed report owns the single attempt. Later evidence ticks do not authorize a
second call, and a changed original `report.json` refuses as
`owner_review_report_changed` instead of starting another review. A recorded
review remains valid evidence after the binary prompt changes; only a new
review call must match the current prompt and policy. A failed review keeps its
artifact and specific code; an interrupted review is never replayed. Review
does not replace the legacy post-close scorer, which remains disabled unless
separately configured.

A reviewed `owner_tasks.research` policy adds one bounded owner research turn
over terminal verified evidence. It is optional and absent by default; when
absent, old policy bytes, templates, tool frames and the legacy post-close path
are unchanged. The dashboard `owner_research` projection and the operational
health `research` block are read-only and report `disabled`, `awaiting_evidence`,
`pending`, `deferred`, `expired`, `parked`, `due` or `invalid` with a static
reason; `parked` and `deferred` carry the research lane's own cap, book or
schedule reason (see the research lane in [model harness](MODEL_HARNESS.md)). They expose the selected original evidence id
and count, cutoff, real deferred expiry, checked pending source and the captured
`as_of_ms`; they never start a turn, create a queue or renew the snapshot. Both
projections are derived from one read-only capture at the controller completion
sample, so `owner_research.as_of_ms` and
`operational_health.research.as_of_ms` are the same completion time. Every
disabled, unavailable, invalid, configured, parked and deferred branch carries
that sample, and a later health/summary GET copies the captured value instead
of renewing it with request time.
An open paper position no longer blocks research; research runs in its own lane
and never parks or delays trading. `owner_research_no_eligible_evidence` means no terminal
verified evidence exists; no provider call or usage journal is created. A
configured policy that failed to load reports `unavailable` with a bounded
static reason, never `disabled`; only a genuinely absent policy is `disabled`.
A retained turn whose owner-task binding cannot be verified reports
`owner_research_source_unverified`; malformed evidence reports
`owner_research_source_invalid`, never `owner_research_settings_invalid`.
When evidence is due, the read-only projection also verifies the pinned
workspace refs with the same bounded reader admission uses, so a changed or
missing source is reported as `invalid` before any paid call.

The lane profile is bounded: `max_tool_turns` is 1..=256 and `timeout_ms` is
10,000..=21,600,000; the defaults remain 8 tool turns and 600,000 ms. A reviewed
deployment may raise them within those bounds (for example 96 tool turns, 150
calls, 12 turns, 12,000,000 tokens and 3,600,000 ms); those are deployment
settings, not source defaults. `max_tokens_per_session` counts provider
input+output tokens from completed and incomplete calls, including result
envelopes written before completed-call usage was recorded.

Optional `owner_tasks.research.idle_trigger {enabled, quiet_minutes}` is off by
default and requires enabled research with its lane. During market hours, when
no fill has happened for `quiet_minutes` (anchored at the market open before
the first fill), the lane projects due with reason
`research_lane_idle_trigger` and posts one host-authored request into the same
durable queue. It still passes the lane's turn/call/token caps, minimum
interval, pending turn, request capacity and market-hours window, runs only in
the research slot, and never blocks or delays trading. `quiet_minutes` is
1..=375. A reviewed deployment may enable it with `quiet_minutes:30`; that is a
deployment setting, not a source default. The lane must opt into
`during_market:true` for an intraday trigger; otherwise the existing
market-hours deferral applies. The trigger adds no trade authority and no paid
call starts without an eligible request or evidence.

The operational health `owner_failure_backoff` block is separate from the
context-exhaustion `backoff` block. It reports the optional configured
`failure_backoff_ms`, `enforced_scope=owner_model_turns_only`, active state,
earliest retry, owner failure count and last failure time. Only actual
supervisor/decision provider or transport failures arm it; research lane,
review,
compaction, aside and host lifecycle failures do not. Host protection and
lifecycle work continue while it is active. An aside request that cannot start
because the supervisor is parked, owner work is pending or the owner backoff is
active is exposed as `aside` with a static `deferred_reason` and, for backoff,
the real `deferred_until_ms`; its question is never exposed and it is never
silently dropped.

The research turn writes one durable artifact and one `owner_research` action
receipt before its result. A committed artifact or action reconstructs the
missing result without another provider call; neither is an explicit
`owner_research_interrupted` terminal state and is never replayed. A failed
attempt is terminal for that original evidence id. The turn has no order,
market, financial, web, policy, `workspace_run`, `workspace_install` or
registered-analysis enqueue tools; it may submit candidates only through the
existing queue and write only `notes/`/`skills/` in the workspace. Research
does not promote, activate, place or size anything and makes no profitability
claim.

When the reviewed owner workspace policy is enabled, the successor seed may
carry a top-level `workspace_state` with bounded pinned `notes`/`skills` refs.
The host verifies the original source session, same book binding, path, size and
sha256 on a no-follow descriptor; missing or changed refs are explicit and no
file is copied. Pinning uses the reviewed `workspace.max_file_bytes` limit and
the same bounded UTF-8 reader, so a larger or non-UTF-8 file is not offered as a
readable ref. New seeds with this field are forward-only for older binaries;
restore the original seed or disable the workspace policy before rollback.

Workspace writes replace the destination with a new owner-only inode: a private
0600 temp file is created in the already-validated private parent, written,
synced and atomically renamed, then the parent is synced. An existing 0644 file
therefore becomes 0600 without truncating the old inode, so a reader holding it
keeps the old bytes and a hardlink target outside the tree is untouched. Every
current workspace directory component must already be owner-owned and
owner-only (0700); a missing component is created 0700, while a symlink, foreign
owner or wider mode fails closed without chmodding a predecessor. Pinned
predecessor reads only validate ownership and never repair permissions.

The controller samples one completion timestamp per loop and writes it as
`as_of_ms` with a `controller_heartbeat` (`version`, `completed_at_ms`,
`next_deadline_ms`, `loop_generation`). `/api/v1/health` reports
`controller_liveness` as healthy, unknown, stale, future or malformed. The
updater refuses a missing timestamp, an old completion, a future completion or a
malformed heartbeat. API request time never refreshes an old snapshot. The
two-second financial quote freshness gate remains separate and unchanged.

A **closed, flat, halted planner-authority book** is cleared with the same
reviewed command. Stop the service if it is still running, then run
`planner-recover`; no `resume` or service restart is needed. It clears only the
halt, preserves the closed/stopped flags, and records `mode=closed_flat` plus
`financial_state_unchanged=true` in the `operator_planner_recovery_confirmed_paper_review`
tick. A data, news or transient read block is retained because the seal means no
new decision can execute; a configuration block is resolved by the exact policy
rebind and an interrupted claim by exact retirement. An unknown sticky block
refuses. Cash, holdings, fills, fees, seals and report bytes are unchanged.
Restarting the controller after a config or prompt edit does not halt a closed
book; planner authority is not initialized for a closed book.

A **halted model-led book with open positions** enters reviewed protection-only
mode instead of abandoning the book:

```sh
systemctl --user stop nivra-paper.service
nivra planner-recover --config PAPER_CONFIG --protection-only --confirm-paper-review
systemctl --user start nivra-paper.service
```

Protection-only recovery rebinds the planner authority to the corrected config,
clears the operator stop, and **keeps `halted=true`**. New entries remain blocked;
held positions continue to receive fresh two-second quotes and the frozen exit
policy, including stop/backstop protection. A data, news or transient read block
is retained because the halt already blocks new decisions; a configuration block
is resolved by the exact policy rebind and an interrupted claim by exact
retirement. An unknown sticky block refuses. After the position is flat, stop the
service and run the normal `planner-recover` above to clear the halt, then
`resume`. The audit tick is
`operator_planner_protection_only_confirmed_paper_review` with
`mode=protection_only`, `entries_blocked=true`, `halted=true` and
`financial_state_unchanged=true`. A halt carried from a closed predecessor is
cleared by the same reviewed command once its carried position is flat; that
recovery records `mode=flat_carried_protective` instead.

A **closed, halted, model-led book still holding positions** cannot use
protection-only recovery: the book is sealed, so in-session quotes and exits are
already over, and rollover refuses halted books. Carry it into the next session
with the reviewed halted-carry acceptance instead. Run it after the source close
and before 09:15 IST on the next trading date, with the controller stopped:

```sh
systemctl --user stop nivra-paper.service
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --dry-run
nivra rollover --config PAPER_CONFIG --accept-halted-carry --confirm-paper-review
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --dry-run
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --advance --confirm-paper-review
systemctl --user start nivra-paper.service
```

The first preview shows the blocked book (`rollover_blocked`, `halted: true`,
`carry_admitted: false`). Acceptance creates a private, append-only
`halted-carry-acceptance.json` audit receipt binding account/session, config,
report/state/review hashes, the recorded halt reason and source, the held-position
count and settled planner attempt evidence. It preserves the original SQLite
book, report and review seal, and it never clears the halt. Repeating acceptance
returns the original receipt and timestamp; changed evidence refuses during
preview, advance and successor carry verification. A partial or invalid receipt
is retained for inspection, never replaced.

The second preview must show `rollover_ready`, `model_led: true`, `halted: true`,
`halted_carry_admitted: true`, `carry_admitted: true` and a non-zero
`carried_positions`. The successor receives the same cash, positions, fills and
fees and starts `halted=true`, `stopped=false`, `closed=false`, health
`risk_halted_protective_exits_only` with entries blocked. Its carry-over records
`halted_carry` with the source halt reason, acceptance hash,
`entries_blocked=true`; the successor book records one
`operator_protective_carry_adopted` tick. No model claim is reserved, and the
held position continues under the frozen exit policy with fresh two-second
quotes. Nothing unhalts automatically, and the source is never reopened,
unstopped or replayed. If the controller is started instead of advancing, its
first observation of the next trading date before 09:15 performs the same
reviewed rollover; the `--advance` step then returns `already_advanced` and
changes nothing.

When the carried position is flat, stop the service and clear only the halt with
the reviewed recovery, then resume:

```sh
systemctl --user stop nivra-paper.service
nivra planner-recover --config PAPER_CONFIG --confirm-paper-review
nivra resume --config PAPER_CONFIG --confirm-paper-review
systemctl --user start nivra-paper.service
```

That recovery refuses while any carried position remains, requires the recorded
protective halt plus the verified carry and adoption evidence, and records
`mode=flat_carried_protective` with `entries_blocked_before=true`. It leaves the
book stopped; only `resume` clears the stop. If the position goes flat only at
the close before you clear it, the same reviewed command works on the closed
flat book and records `mode=closed_flat` with `carried_protective=true`; the
book stays closed and the normal closed flat rollover then applies. Acceptance,
advance and recovery never start the service, place an order or replay a call.

Non-model-led, stopped, open, flat, unreconciled, missing-halt-history,
pending-order, interrupted or integrity-blocked and unknown-state books refuse.
Only a non-stopped halted hold qualifies. A halted flat book keeps its
`planner-recover` path; a stopped non-halted book
keeps `--accept-stopped-carry` or `--acknowledge-closed-book`. The receipt binds
one source close, so a multi-day hold repeats the reviewed sequence for each
closed halted predecessor; the successor's own halt is carried again only after
a fresh acceptance in that session directory.

A live config or prompt edit detected by the running controller is suspended as
`planner_configuration_changed_apply_required`; it does not halt the book. Stop
the service, run `config-apply --confirm-paper-review`, then `resume`. Starting
an open book after a config edit without applying it leaves the planner
suspended on `planner_event_configuration_changed`; it does not halt the book,
and the same `config-apply` then `resume` sequence applies. Authority incident
ticks (`planner_incident_check`) record the exact failed check (`config_hash` or
`prompt_sha256`), prior/current config and prompt hashes, and the supported
recovery command. `planner_configuration_changed_apply_required` records the
`config-apply` and `resume` commands.

Planner event blocks separate read availability from data integrity.
`planner_read_unavailable_no_replay` covers transport, HTTP, local quota-admission
and response-encoding failures, which produce no market data. The controller clears
it automatically only after a fully validated fresh REST quote and only when no
attempt is pending or unsettled. An attempt that overlapped the outage is settled as
failed/no-replay with its evidence and charges preserved; it is never replayed.
Malformed or identity-invalid quote data (`planner_data_failed_no_replay`), news,
configuration and interrupted-attempt blocks remain sticky and require explicit
review and `planner-recover`.

Under the `decision_owner` policy only sticky blocks refuse an owner decision.
A recoverable digest failure (provider, budget, deadline, invalid plan) is one
bad attempt: it keeps its own reason on the attempt, appears as
`last_digest_failure` in the planner status, and does not stop the orchestrator
from committing a decision built from the earlier valid digest. The transient
quote suspension is not sticky either: the decision commits and stays suspended
until a validated fresh quote clears the block, then the committed plan becomes
active. The commit is still refused with `planner_decision_owner_not_ready`
while a sticky block is set.

Nothing to buy and nothing to manage is a normal no-op, reported as
`model_plan_no_prospects_no_positions`. It is not a failed attempt, a block or an
alert, and it needs no recovery.

A decision skipped in a cycle that already compacted the session history is
recorded as `supervisor_context_limit_compacted`. The decision itself waits for
the next cycle: replaying history with omitted exchanges would record a turn
that differs from the one rendered, and a mid-cycle compaction summary does not
exist until the lifecycle turn completes.

For an old reconciled closed/flat/non-halted stopped book lacking its review seal:

```sh
nivra rollover --config PAPER_CONFIG --acknowledge-closed-book --confirm-paper-review
```

This binds the original report/state and writes only acknowledgment evidence. It
never reopens/unstops the book.

For a **closed, operator-stopped, non-halted model-led book with carried positions**
stop the controller and inspect its original report, review and recorded
`operator_stopped` event. Keep the original config and seals. After installing a
release containing this recovery, run the following on the next trading date,
before 09:15 IST (the clock arguments sample the actual command time):

```sh
systemctl --user stop nivra-paper.service
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --dry-run
nivra rollover --config PAPER_CONFIG --accept-stopped-carry --confirm-paper-review
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --dry-run
nivra rollover --config PAPER_CONFIG --now "$(date --iso-8601=seconds)" --advance --confirm-paper-review
```

The first preview shows the blocked carried book. Acceptance creates a private,
append-only `stopped-carry-acceptance.json` audit receipt, binding account/session,
config, report/state hashes and exact planner control/settled attempt evidence.
It preserves SQLite, the source stop flag and original report/review. Repeating
acceptance returns the original receipt and timestamp; changed evidence refuses,
including during preview, advance and successor carry verification. A partial or
invalid receipt is retained for inspection, never replaced automatically.
The second preview must show `rollover_ready`, `model_led: true` and
`carry_admitted: true`; inspect it before advancing. The successor receives the
same cash, holdings, fills and fees through normal carry validation; only the
successor copy clears stopped/closed. Advance remains idempotent.

Keep the controller stopped from the first preview through the advance. If it is
started first, startup performs the same reviewed rollover; the `--advance` step
then returns `already_advanced` and changes nothing. Treat that as an
already-completed transition, not a failure: do not reset `current.json`, restart
the source session or rerun an older config to force a second commit. The
advance uses the committed pointer as its source, so a later stopped-controller
advance works from the successor even while the config still names the original
session.

Live or non-paper reports, non-planner strategies, non-model-led/open/halted/flat
books, financial reconciliation failures, pending orders/attempts, missing stop
history, mismatched seals and unresolved planner blocks refuse. Only an unset
planner block or the operator-stop admission block is eligible; no integrity
block or interrupted attempt is silently retired. A halted book uses the
reviewed `--accept-halted-carry` route above, never this acceptance. Flat books
keep the existing `--acknowledge-closed-book` path. Do not run `resume` on the
closed source.
Run readiness for the successor and follow the separately authorized paper-service
startup procedure. Acceptance and advance themselves never start the service.

Review output now separates the account fact `model_led` from `carry_admitted`;
older `model_led_account` verdicts remain readable. A restart of a stopped, still-open
book logs `stopped_open_session_restart_requires_review` with explicit resume
instructions. The book remains stopped; a restart is never a resume.

Planner authority changes on a closed sealed book apply to the next session.

Legacy migrations (`prior_config`) admit only exact documented original
bindings. Keep original configs
and predecessor seals; do not generalise compatibility flags to new successors.

## Grants and model admission

Use `model login/list/status/probe/select --help` for the application's independent
login, exact-model diagnostic and reviewed role-model switch. Probes consume requests; status is read-only. Luna
uses its own OpenAI grant, all Grok roles can share one refreshed xAI model config.
The legacy credential-config `enabled` field is not planner role enablement.
Production uses Luna's factual digest and one persistent orchestrator, with
independent credentials and original charge identities. Existing configs with no
owner policy preserve their legacy hash for migration and archive access; they
cannot run independent Decide calls in this build. Current readiness and the
dashboard expose Digest and Orchestrator only. Original Decide reservations
remain separate evidence and are never refunded or merged.

Stop the paper controller and back up reviewed configs. Write the owner policy
to a new planner file and check it with `planner config-check`, which refuses a
missing digest, supervisor trigger or events block and an enabled Decide block.
Install the exact revision's prompt files and bind application-owned credentials.
Confirm the digest and owner grants, run self-check and preflight, then reviewed
`config-apply --confirm-paper-review` and audited `resume` before an authorized
paper start. On failure preserve the failed artifacts and restore the reviewed
predecessor config and compatible release through the updater guards.

The supervisor runs before Luna's first digest at session start or resume when
`supervisor.run_first.on_session_start`/`on_resume` is enabled in the experiment
profile. It also wakes once per applied `(version, profile, prompts, config)`
tuple when `on_change` is enabled; `change_min_gap_ms` coalesces a burst and the
supervisor's own writes do not retrigger it. Luna waits only while that run is
in flight, at most `hold_luna_ms` (default 120 s); a failed, slow or skipped
supervisor fails open and Luna proceeds. If the supervisor's own daily allowance
is exhausted, it parks and records the reason, time and allowance in
`supervisor-state.json`; the controller writes at most one deduplicated
`supervisor_parked_budget` skip per minute. Recovery requires a reviewed
`daily_call_budget` increase and intact usage evidence showing that the allowance
now exceeds already charged supervisor usage. Recovery clears only the park flag;
it does not reset cooldowns or refund, delete or rewrite reservations. An
unavailable/corrupt journal or insufficient allowance keeps the supervisor parked.
Host protection, execution and the trading loop continue; terminal books are not
resumed or unhalted. Existing parked records without an allowance baseline remain
parked for that session; the next trading session starts fresh. See the
[model harness](MODEL_HARNESS.md#identity-archives-and-reviews) for the full contract.

### Switching the orchestrator model

The reviewed policy must define the exact binding and admit it before a switch:

```json
"orchestrator": {
  "model": {"model_config": "/absolute/application-model.json", "model_id": "<default>", "...": "..."},
  "model_profiles": {
    "PROFILE_NAME": {
      "model_id": "<exact provider model id>",
      "transport": "direct_oauth",
      "model_config": "/absolute/application-model.json"
    }
  },
  "admitted_models": ["PROFILE_NAME"]
}
```

Apply that policy through the normal reviewed `config-apply`; the profile is
rejected at load when the name, exact model ID, absolute credential path or
admission list is invalid. Then inspect and request the switch:

```sh
nivra agent model show --config PAPER_CONFIG --json
nivra agent model set --config PAPER_CONFIG --model PROFILE_NAME --json
nivra agent model show --config PAPER_CONFIG --json
```

For a config-driven request, keep the runtime selection in a separate JSON file:

```json
{"version":1,"profile":"PROFILE_NAME"}
```

Reload that file with the same optional revision and request-ID guards as `set`:

```sh
nivra agent model reload --config PAPER_CONFIG --selection-config SELECTION_CONFIG --json
```

The selection file names a reviewed profile; it cannot add a model binding or
admission. Reload uses the same validator, lock and pending/active receipts as
`set`. Editing this file takes effect only after an explicit reload. Keep the
paper and planner policy files unchanged for a model-only switch.

`set` records a durable pending revision; it does not edit the policy or restart
the service. The controller applies it at the next idle turn boundary after the
current turn and its action receipts settle. `show` reports configured, active
and pending models. A stale `--expect-revision` is refused, and a pending profile
the current policy no longer admits or binds exactly is refused while the last
valid active model is retained. Provider entitlement and structured
output are rechecked by the existing per-call admission; a catalog listing alone
is not authorization. Selection never resets the session, notes, receipts or
usage; a new trading session starts with the configured `default` model. This is
a per-turn runtime selection; for a persistent policy-level switch that survives
the next session, use `model select` below. See the
[model harness](MODEL_HARNESS.md#orchestrator-model-selection).

### Selecting a role model from the CLI

`nivra model select` changes the model one planner role uses, without a hand
edit or a manual migration. It only chooses between bindings the reviewed
planner policy already declares: the target must be an admitted profile in
`orchestrator.model_profiles`/`admitted_models`, or the role's configured
`default`. It never adds a model, credential, admission or budget, and settings
stay in the planner config.

Preview first; `--dry-run` writes no planner, paper-config, book or audit state:

```sh
nivra model select --config PAPER_CONFIG --role orchestrator --profile PROFILE_NAME --dry-run --json
```

The preview reports the planner-config diff (only the role's `model_id`,
`transport` and `model_config` move), the target transport's grant status, live
catalog membership, whether a paper session is open, and the refusal a real run
would return. Catalog membership is read-only: it uses the target's own
application grant without refreshing it and never prints token content; an
unavailable grant or catalog is reported as `unavailable` with its reason.

Apply it with the same reviewed confirmation as `config-apply`:

```sh
nivra model select --config PAPER_CONFIG --role orchestrator --profile PROFILE_NAME --confirm-paper-review --json
```

The command writes a new planner file beside the live one (same `prompts/`
directory, so prompt hashes are unchanged), points the paper config's
`planner_config` at it atomically, and runs the reviewed
`config-apply --confirm-paper-review` transition: preflight, controller lock,
book and flatness checks, then the audited config transition. The same book
eligibility as `config-apply` applies, so a closed, halted or non-flat book is
refused with the printed `config-apply` reason. The switch is recorded on the
`operator_config_apply_confirmed_paper_review` tick under `operator_command`.
The book stops with `operator_stopped`; `resume` starts the next session on the
new model. Rollback is the same command with the previous
profile (for example `--profile grok-owner` after switching to `sol-owner`); the
predecessor planner file is kept, so a binding that is not declared as a profile
can be restored by pointing `planner_config` back at it and running the reviewed
`config-apply`. The first reviewed use is the packaged owner profiles:
orchestrator `grok-4.7` (`direct_oauth`, `grok-owner`) to `gpt-6.1-sol`
(`openai_codex`, `sol-owner`).

Refusals, all with a printed reason code:

- `model_select_session_open`: a paper session is running or its controller lock
  is held. Wait for the session to close; the change applies to the next session.
- `model_select_grant_required`: the target transport has no authenticated grant.
  Run `nivra model login` for that transport, then retry.
- `model_select_model_config_invalid`: the target credential config is missing,
  invalid or declares another transport.
- `model_select_profile_unknown` / `model_select_model_not_declared`: the profile
  or exact model ID is not declared by the current planner policy.
- `model_select_transport_incompatible`: the declared profile's transport cannot
  use the role's configured tools.
- `model_select_ambiguous_target`, `model_select_transport_required`,
  `model_select_profile_required`, `model_select_same_path`: give exactly one
  target form, and never point the paper config at the file it already uses.
- `model_select_shared_credential`: the role's effective credential comes from
  the shared top-level `model_config`, so a role-only switch cannot be expressed
  without changing another role. Bind the role's own `model_config` first.
- `model_select_policy_not_effective`: the live planner file is a future-dated
  schedule; switch the policy in force instead of deferring the change.
- `model_select_unchanged`: the role already uses that exact binding, so no
  book stop is needed.
- `model_select_confirm_paper_review_required`: review the book, then pass
  `--confirm-paper-review`.
- `model_select_apply_refused`: the reviewed apply refused and printed the
  `config-apply` reason; the new planner file was removed and nothing changed.
- `model_select_config_unwritable`: the book transition was applied but the
  paper config could not be rewritten. The printed planner path is the reviewed
  file to point `planner_config` at before `resume`.
- `model_select_config_changed` / `model_select_config_unreadable` /
  `model_select_config_invalid`: the paper config changed or could not be
  read/parsed; nothing was applied.
- `model_select_unwritable` / `model_select_invalid` / `model_select_policy_invalid`:
  the new planner file or policy could not be written or revalidated; nothing
  was applied.

`--model-id ID --transport TRANSPORT` selects by exact binding instead of by
profile name; the pair must match a declared profile or the current default.
The current model per role is reported by the readiness report and the dashboard
roster; `model status` remains the grant-state check.

### Explicit limit fallback

A reviewed role may configure one explicit fallback for a single call whose
primary provider returns a usage, rate or quota limit. It is never a silent
fallback and never replaces the primary for another failure. It applies to
`digest.fallback` and `orchestrator.model.fallback`; omit it to keep existing
policy bytes and hashes unchanged.

```json
"digest": {
  "fallback": {
    "model_id": "deepseek-v4-1-flash-1017",
    "transport": "mistral_chat",
    "model_config": "/absolute/path/mistral-fallback.json",
    "output_cap": 32000,
    "reasoning_effort": "max"
  }
}
```

The same object goes under `orchestrator.model.fallback`. `output_cap` is
optional and, when present, is 64..=131072. `reasoning_effort` is optional and
one of `low`, `medium`, `high` or `max`. `model_config` is an absolute path to
the application-owned fallback credential config, not the key:

```json
{"transport":"mistral_chat","api_key_file":"/absolute/path/mistral-api-key"}
```

The key file must be a regular, owner-owned, single-link file with mode 0600,
outside the repository and foreign agent auth caches. Create it without putting
the key in shell history, a config file, a log or the repository; the runtime
reads and trims it and never prints it. A group/world-readable key, a symlink, a
foreign owner or a transport mismatch blocks fallback readiness without changing
the primary role's status.

Only HTTP 429 after the bounded retries, or a provider error carrying
`usage_limit_reached`, `rate_limit_exceeded` or `insufficient_quota`, may switch
that one call to the fallback. Expired grants, authentication failures,
timeouts, invalid output, cancelled turns, ambiguous paper actions and other
errors never switch. The fallback has no separate call budget; the call keeps
the role's existing reservation and accounting. The request uses Mistral chat
completions with a system role, `max_tokens`, no `store` and no tool-action
replay; the reply is translated back to the same harness envelope. Each switch
is recorded in transport evidence (`fallback` with from/to model, transport,
reason, primary status/stage and outcome), logged, and alerted as
`model_fallback` at most once per 30 minutes per role. Readiness reports each
role's `fallback` status and reason. No key, grant, token or auth cache enters
the evidence, alert or report.

Probe the configured key and model with one minimal request (it never starts a
paper session or touches a journal):

```sh
nivra model probe-fallback --config /absolute/path/mistral-fallback.json --model deepseek-v4-1-flash-1017 --reasoning-effort max
```

The JSON report includes the HTTP status, transport, requested/received model,
latency and token counts; `billing_verified_by_probe` is always false. A fixture
or probe does not establish provider entitlement, host qualification or future
availability.

Adding a fallback is a policy change: it changes the serialized policy bytes and
hash. With the old config still in place, install the release that supports the
key first, then back up the reviewed config, add the fallback and key reference,
and run `planner config-check` and preflight before the normal reviewed
transition. Do not add the key to config or change policy before the supporting
binary is installed; the updater's rollback preflight must still parse the old
config. The normal [release transition](#releases-and-updates) and reviewed
`config-apply` path apply; there is no bypass.

### Reviewing the decision-owner migration

The owner policy is reviewed without applying it. Write the planner config
without the removed `decision_owner`, `decide` and `handoff_gate` keys to a new
file and validate it offline:

```sh
nivra planner config-check --planner NEW_PLANNER.json --session SESSION
```

The check refuses any missing owner prerequisite and writes nothing. Point the
paper config's `planner_config` at the new file, run
`config-apply --confirm-paper-review`, then `resume`. Rollback uses the
predecessor planner file and the same reviewed transition.

A two-role policy whose durable authority was initialized under a different
policy is refused at controller startup with `decision_owner_migration_required`
(or `decision_owner_legacy_plan_requires_migration` for a legacy active plan);
host protection continues until a reviewed `config-apply` rebinds it. See the
[model harness](MODEL_HARNESS.md#decision-owner-migration-policy).

### Personal-agent status and requests

The local agent CLI is machine-readable and works over the operator's existing
authenticated remote access. It adds no HTTP control endpoint and no dashboard
editor.
Every command accepts `--config PAPER_CONFIG --json`; findings are JSON, never
an order, fill or application acknowledgement.

```sh
nivra agent status --config PAPER_CONFIG --json
nivra agent ask --config PAPER_CONFIG --question 'Why was the entry skipped?' \
  --request-id ask-1 --idempotency-key ask-key-1 --json
nivra agent result --config PAPER_CONFIG --request-id REQUEST_ID --json
nivra agent instruct --config PAPER_CONFIG \
  --expect-session 2026-11-04 --expect-revision 0 \
  --scope decision_context --text 'Prefer cash when the quote is stale.' \
  --request-id instruct-1 --idempotency-key instruct-key-1 --json
nivra agent cancel --config PAPER_CONFIG --request-id REQUEST_ID --json
```

`status` reads the resolved session, paper-book flags, controller state, model
selection revision, pending aside slot and owner-directive ids. It makes no
model call and writes nothing. `ask` uses the controller's existing single
read-only `aside-request.json`/`aside-reply.json` slot; it never writes the
book, journal, notes, directives or trading session and never wakes the
controller. Only one question may be outstanding. `result` polls by request id
without resubmitting and reports `queued`, `running`, `answered`, `failed`,
`expired`, `applied`, `refused` or `cancelled` as applicable. `cancel` removes
only that question or supersedes only that owner directive; the trading
instance is untouched.

`instruct` is an explicitly requested, typed, expiring directive for the one
decision owner. It requires the exact session and instruction revision reported
by `status`. A stale session or revision creates a durable `refused` receipt and
writes no directive. Successful instructions use only the `review_only` or
`decision_context` authorization scope and are delivered to the decision owner
(the legacy `decide` envelope or the two-role owner turn) on a later controller
turn, never as an order or an execution authority. Request ids, idempotency
keys, expiry and receipts live under `model/agent-requests.json`; repeating the
same idempotency key returns the same receipt, and the per-session instruction
revision stays monotonic when old receipts are pruned. Instructions live in the
existing `model/directives.ndjson` store with target `owner`; model output may
not nominate that target.

The orchestrator scorer trigger is post-close only. Configure
`orchestrator.triggers.scorer.after_minute` (931–1439 IST, or the top-level
`scorer_after_minute` fallback), its prompt/tools, and
`orchestrator.triggers.scorer.replay` (`max_snapshots` 60,
`min_valid_snapshots` 3, `horizons_minutes` `[5,15,30]`, `top_n` 10,
`max_replays` 3, `max_age_ms` 2000, `lookback_samples` 20,
`max_feedback_chars` 4096, `notional` 5000). An enabled scorer trigger without
either due minute is rejected at planner-config load with
`planner_scorer_after_minute_required`. A runtime `after_minute_unset`
configuration error is reported as `configuration_error` and is not persisted as
a terminal skip; a skip written by an older release is re-admittable after the
corrected config is applied. The trigger extracts the session's
real feature snapshots, runs the written Python scorer through the sandbox and
feeds an unqualified report back for a bounded fix/replay loop. With
`discovery.shadow_scorer.enabled`, only a report with `status=ok`, enough valid
snapshots and zero failed snapshots publishes the written scorer and parameter
files to the configured `scorer_path`/`params_path`, plus a
`<scorer>.qualification.json` hash sidecar; no strategy is promoted. A restart
leaves an admitted run `interrupted` and never replays it automatically. See the
[model harness](MODEL_HARNESS.md#post-close-scorer-trigger).

After same-account re-login, inference uses the authenticated application grant
and the live provider catalog. There is no separate approval file, rebind,
renewal command or scheduled auto-renew. Optional leftover keys
`direct_oauth_approval_file` and `approval_auto_renew` are accepted in existing
config for parse compatibility and ignored at runtime.

`nivra model login|list|status|probe` remain the grant and catalog tools, and
`model select` is the reviewed role-model switch. A model ID must appear in the
live catalog; `grok-4.7-build-fast` is still refused.
Catalog presence is not entitlement, billing or tool-quality proof. Credential
failures emit `model_credential_unavailable` notices. See
[transport](XAI_SUBSCRIPTION_TRANSPORT.md).

## Releases and updates

Build and test on a development host using [development](DEVELOPMENT.md). The
main CI run builds and publishes musl after its required checks when an annotated
version tag matches that commit. The **Fast musl release** workflow reuses passed
exact-commit CI for a tag added later, or resumes a publication whose immutable
asset ref already exists, without repeating tests. A failed release step is
resumable from that verified ref; it never overwrites or moves published assets.
Annotated stable tags publish musl binary, SHA-256 and `release.json` both as
attachments and in immutable `refs/release-assets/<tag>`. Verify tag, exact source
commit, target and SHA-256; a tag is not session authority. Published tags never
move.

Nivra is the only deployment source. Releases are published from
`github.com/TharunChougoni/nivra` when explicitly requested by the repository
owner; the first is `v0.9.26`. The predecessor `ind-sadr` repository is retired:
its release assets are no longer installed, and new work, tags and deployment
instructions live here. An installed host bound to the retired repository is moved
once through the reviewed [host migration](#one-time-host-migration).

Updater config selects the configured Git remote and absolute `paper_config` path.
It never reads keys itself or needs a GitHub token. Remote transport is a
deploy-key SSH host alias, and only `github-nivra:` is accepted; the host
migration is the one path that rewrites a retired `github-ind-sadr:` remote
([example config](../packaging/rust/updater.example.json)):

```json
{
  "remote": "github-nivra:TharunChougoni/nivra.git",
  "paper_config": "/absolute/paper.json"
}
```

The host's private alias/key details and the concrete host addresses stay outside
Git, in the git-ignored private host inventory described in
[AGENTS.md](../AGENTS.md). Preview/install:

```sh
nivra update --check --config UPDATER_CONFIG
nivra update --dry-run --config UPDATER_CONFIG
systemctl --user start nivra-update.service
```

`--check` lists only; dry-run fetches/verifies but never activates. Both write status.
Manual deploys go through `update --tag` or `update --sha`. Never flip the
symlink by hand. Explicit selection does not bypass hold, halt or integrity guards.
Run as the service/home owner, using the installed binary and absolute updater
configuration path (replace `UPDATER_CONFIG`):

```sh
nivra update --tag vX.Y.Z --config UPDATER_CONFIG
```

Operator-stopped deploy (inside the blackout or with carried holdings): stop the paper
service, then

```
nivra update --tag vX.Y.Z --operator-stopped --confirm-paper-review --config UPDATER_CONFIG
```

It requires an inactive service and a book with no pending orders that is either
operator-stopped and non-halted, or closed and flat (a closed flat book may still
carry a planner halt). It smokes the new binary and leaves the service stopped.
For a closed flat halted book, install first and then run the new release's own
`planner-recover --confirm-paper-review`; do not rebind authority with a candidate
binary before the swap, because an older installed release will re-halt on that
authority. Then run `config-apply --confirm-paper-review` (model-led carried
books keep their holdings), `resume --confirm-paper-review`, readiness, and start
the service:

```sh
nivra config-apply --confirm-paper-review --config PAPER_CONFIG
nivra resume --confirm-paper-review --config PAPER_CONFIG
nivra readiness --config PAPER_CONFIG
systemctl --user start nivra-paper.service
```

Run these only for the reviewed stopped book; a failed check requires inspection.
Deployment alone never resumes the book.

`--tag vX.Y.Z` also accepts the same tag without its leading `v`. The diagnostic
`--sha <commit>` selector accepts only an unambiguous commit of a published
annotated stable release, never an arbitrary source build. Without a selector the
timer chooses the newest newer stable tag.
All invocations take the same updater and deployment locks; contention reports
`skipped_lock_busy` without activation. Git fetches use a private bare cache,
independent of the caller's checkout, with bounded redacted failure diagnostics.

Manual and automatic activation require a reconciled closed non-halted bound
book, NTP-synchronised clock, no `DEPLOY_ROOT/HOLD`, and time outside the
weekday **09:00–15:45 inclusive IST** blackout. A closed flat non-halted book
is admitted by the original rule. The deploy guard also reads the real book: while the session is
open and any position or pending order exists, activation defers with an
explicit `deferred_*` reason instead of silently reporting `deferred`. An
explicit forced deploy (`--force-unwatched-window`) accepts a bounded restart
window for that exposure, but never bypasses a risk halt, hold, identity,
integrity or preflight check. The same explicit flag also admits a **flat**
in-session book (no positions and no pending orders, not halted and not an
operator-stopped book) with reason `market_open_flat`: the original book is
preserved and the same session resumes after the health-checked restart. A
halted or operator-stopped book still defers. A forced activation records
`unwatched` (`started_at`, `stopped_at`, `duration_ms`, `reason`, `forced:true`)
in the transaction receipt and logs the same record; the window is never hidden.
`--operator-stopped` remains the separate path that leaves the controller
stopped and is mutually exclusive with the forced window.

A closed model-led book that is already carrying positions overnight is
admitted only outside market hours (the NSE calendar is closed at the sampled
clock), and only when a read-only adoption check passes: the bound book is
readable and reconciled, closed, model-led, non-stopped, non-halted, with no
pending orders and no pending or
reserved planner attempt. The check never rewrites, clears or replays the book,
and the successor session still starts only through the reviewed
rollover/carry path. An open in-session book, a halted or stopped book, an
unknown/unreconciled state and any unsettled claim or attempt defer activation
to their reviewed paths. Carried holdings therefore no longer defer updates
indefinitely: post-close, weekend and holiday windows admit them. Manual
selection does not otherwise bypass session admission or authorize a session.
The updater verifies annotated tag, source commit, target and SHA-256, then runs
binary version, self-check and deployed-config preflight checks. It detects the
installed release from the current symlink target and binary, including installs
without VERSION/SOURCE_COMMIT files. Release directories are owner-only 0700,
binaries 0755 and metadata 0600, matching bootstrap installation.

The updater stops the service, atomically flips current, repeats preflight,
restarts and checks unit/authenticated status. A stop failure, a swap/activation
failure and a failed health check take the same recovery path: stop the
candidate, restore the previous pointer if it was swapped, restart and probe the
previous release, then finalise `pending.json` as the release's
`failed-update.json` receipt with bounded recovery evidence. If the previous
release cannot be restored or restarted, the updater writes
`~/.local/state/nivra/update-recovery.json` (`kind`, `cause`, timestamp and
pointer/restart verdicts) and emits a critical `updater_failure` alert naming
`update_rollback_restart_failed` or `update_rollback_pointer_restore_failed`;
it never returns success. `failed-update.json` blocks retry of that release.

A stale `pending.json` is never replayed. If the pointer still matches
`previous`, the original evidence is preserved as
`interrupted-update.<millis>.json` and a fresh, fully checked transaction runs.
If the pointer already reached `new`, or evidence is invalid or unrecognised,
the updater keeps `pending.json` untouched, writes a bounded
`pending-review.json` naming the pointer state and returns a specific
`interrupted_update_*_requires_review` reason for operator inspection.
Previews (`--check`/`--dry-run`) report these conditions without moving evidence.
Preserve pending/recovery files, all releases, previous pointer and original
journals. Status is in `~/.local/state/nivra/update.json`; Git failure details
are in the adjacent `git-diagnostic.json` with secrets redacted.

Before writing transaction state, stopping the controller or flipping the pointer,
the updater also runs `preflight --config PAPER_CONFIG` with the currently
installed binary. That binary is the rollback target, so it must still parse the
current reviewed config. If that preflight fails for any reason, including a key
the old binary cannot parse, activation refuses with
`update_rollback_config_incompatible`; the pointer, service state, config and
pending state are left untouched. The check applies to automatic and manual
activation, explicit rollback and the `--operator-stopped` path. It never repairs
or rewrites reviewed config.

Candidate and rollback preflight output is discarded except for a bounded reason
code and timestamp in `~/.local/state/nivra/preflight-failure.json`; never
config bytes, paths, tokens or account data. A failed candidate preflight is
retried by the timer, and the record appears in `update.json`, the CLI status
line (`updater_preflight_failed: version=… reason=… phase=… since=…`), the
`updater_failure` alert and the bounded `updater` object in
`GET /api/v1/health`. A later successful preflight clears it, so fixing the
configuration lets the update pass without review.

Config-schema transitions therefore require an orderly sequence:

1. With the old config still in place, install the release that supports the new
   keys through the normal update path (or a reviewed operator-stopped deploy).
2. Stop the controller, apply the reviewed config transition, and run the new
   binary's `preflight --config PAPER_CONFIG` before resuming or starting.
3. For a closed source book, complete the documented reviewed rollover/advance
   path; never restart a same-session closed book to adopt a changed policy.

Do not introduce config keys that the currently installed binary cannot parse
before installing the supporting binary. The updater's rollback target is the
previously installed release, and a failed activation must be able to restore a
runnable controller.

If the config was already changed and the installed binary cannot parse it, the
same guard also refuses the forward install. Stop the controller, restore the
last reviewed compatible config, install the supporting release, then reapply the
reviewed transition and preflight it. Never bypass the guard by editing the
pointer or pending transaction.

To roll back a successfully installed release, identify the previous verified
published tag from the preserved release manifest/history, then run the same
command with that tag, for example:

```sh
nivra update --tag vX.Y.Z --config UPDATER_CONFIG
```

Rollback runs all the same checks and guards. If blocked by holdings, halt,
blackout, interrupted transaction or failed-release evidence, preserve the state
for operator review; never delete evidence or bypass guards to force activation.
Verify state/seal/usage continuity and actual installed version afterward.

Use the current Rust `setup` workflow for fresh paper preparation; the retired
dated shell installer is not shipped. During updates, the verified prompt bundle
and checksum stay in the immutable release directory. For an existing book,
review and extract prompt files beside the planner JSON, then use suspend and
reviewed `config-apply`; the updater never edits these policy inputs. Custom
writable paths require preflight. Resume only an eligible reviewed stopped open
book, never a closed book.

### One-time host migration

A host installed before the rename runs `sadr-rs` from `~/ind-sadr-rust-current`
under the `ind-sadr-rust-*` units, and its updater cannot fetch the renamed
`nivra-x86_64-unknown-linux-musl` asset. The renamed updater looks for
`~/nivra-current/nivra`, `~/nivra-deploy` and `~/.local/state/nivra` and accepts
only the `github-nivra:` remote. `nivra host-migrate` moves such a host once,
off-hours, with the controller stopped. It is a dry run unless `--apply` is
given. It never starts, stops, enables or resumes a unit or a book, and never
reads or prints a credential.

```sh
nivra host-migrate --release STAGE [--home HOME] [--apply]
```

**What moves and what stays.**

| Location | Migration | Reason |
| --- | --- | --- |
| `~/ind-sadr-deploy` | Renamed to `~/nivra-deploy`; a `HOLD` file carries over | The updater hard-codes the deploy lock and hold |
| `~/.local/state/ind-sadr` | Renamed to `~/.local/state/nivra` | Hard-coded updater state, alert outbox and evidence |
| `~/nivra-releases/<tag>-<commit>/` | Created from the staged release | Updater release root |
| `~/nivra-current`, `~/nivra-previous` | Created; they point at the new release and at the old current release | Updater pointers |
| `~/.config/systemd/user/nivra-paper.service`, `nivra-update.service`, `nivra-update.timer`, `nivra-alert@.service` | Rendered from `packaging/rust` with the kept paths below | The old `ind-sadr-*` units stay installed for rollback |
| `~/ind-sadr-data` | Kept | The book binding hashes the session directory path |
| `~/.config/ind-sadr` | Kept; reviewed configs are rewritten in place | The book binding hashes `token_file` and `dashboard_token_file`; grants and model-selection receipts name files here |
| `~/ind-sadr-history`, `~/ind-sadr-backups`, `~/ind-sadr-diagnostics`, `~/ind-sadr-monitor` | Kept | Not hard-coded by the binary; history may hold the bound read journal |
| `~/ind-sadr-rust-releases`, `~/ind-sadr-rust-current`, `~/ind-sadr-rust-previous` | Kept | Prior releases and the rollback pointer of the old units |
| `/var/lib/ind-sadr-research` | Kept | Root-owned research stage that the paper unit may write |

**Why data and config keep their names.** A paper book's identity is the
SHA-256 of the serialized paper config (`book_binding` in
`runtime/mod.rs`). The serialized fields include `directory`, `token_file`,
`dashboard_token_file` and `read_journal` as absolute path text, plus the read
journal's device and inode; `sessions_root`, `planner_config`,
`alert_webhook_file`, `prior_config` and `history_directory` are not serialized.
A dated successor's `directory` is `sessions_root/<date>`. `book-identity.json`
stores the `paper.sqlite` device, inode and this binding, and the same binding is
repeated by the ledger `state` row, the planner authority, the usage pin
`model/usage-identity.json`, `model/model-selection.json` and the scorer
admission. A same-filesystem rename keeps inodes but changes the path text, so the
live book would refuse with `original_paper_book_changed`, and so would every
predecessor the next rollover verifies. There is no reviewed rebind, and a fresh
book would be a replacement journal. A compatibility symlink does not help either:
token reads refuse a symlinked ancestor. Paths that bind nothing need no move:
grant files record version, client, grant ID and account hash but no path;
prompts are hashed by content; history receipts bind data and engine hashes.
Renaming the data or config directories later needs a reviewed rebind feature
first.

**What the command checks and rewrites.** Before writing anything it refuses:

| Refusal | Condition |
| --- | --- |
| `host_migrate_activation_blackout` | Weekday 09:00–15:45:59 IST (the updater's clock window; holidays are not exempt) |
| `host_migrate_lock_held` | Another process holds the sessions-root or live-session `controller.lock`, `updater.lock` or the deploy `.lock` |
| `host_migrate_update_transaction_requires_review` | `pending.json`, `pending-review.json` or `update-recovery.json` is present |
| `host_migrate_book_binding_mismatch` | The rewritten paper config would not reproduce the live book's binding |
| `host_migrate_release_version_mismatch` | The staged release is not the running binary's own version |
| `host_migrate_*_config_invalid` | A rewritten config does not parse with the new types |
| `host_migrate_*_conflict` | A target path already exists with different content |

It reads `~/.config/ind-sadr/updater.json`, the paper config it names, that
config's planner, the planner's experiment profile and every `model_config` the
planner names. It then rewrites:

- Paper config: drops `allow_legacy_model_led_default`. It writes `sessions_root`
  explicitly as `~/ind-sadr-data/sessions` when the key is absent, because the
  default moved to `~/nivra-data/sessions`.
- Planner: drops `decision_owner`, `decide`, `handoff_gate`, `post_close`,
  `orchestrator.triggers.operations|strategy`, the trigger keys
  `after_decide`, `luna_no_decide_streak`, `minimum_turn_ms` and
  `max_failed_tools`, the same two limits on `digest` and `orchestrator.model`,
  and the `orchestrator.trigger_prompts` keys `post_decide` and
  `luna_no_decide_streak`.
- Experiment profile: drops `handoff_gate`, `prompts.decide`,
  `prompts.decision_evidence`, `prompts.decision_evidence_digest` and
  `roles.decide`.
- Model configs: writes `grant_file` explicitly as
  `~/.config/ind-sadr/xai-grant` when a non-Codex config omits it, because the
  default moved to `~/.config/nivra/xai-grant`. Fallback configs are only
  validated.
- Paper, model and updater configs: write `alert_webhook_file` explicitly when it
  is absent and `~/.config/ind-sadr/alert-webhook.json` exists, because the
  default destination moved.
- `updater.json`: written fresh with `remote`
  `github-nivra:TharunChougoni/nivra.git`, the same `paper_config` and the same
  alert file. `updater-nivra.json` and its staged `distribution_migration` block
  are left as an unused archive; the new updater no longer parses that block.
- Any string under a renamed root (`~/ind-sadr-deploy`, `~/.local/state/ind-sadr`)
  moves with it.

Each rewritten file is validated with the new types first. The original stays
beside it as `FILE.pre-nivra` (0600) and the new bytes are staged as
`FILE.nivra-next` (0600). The release install, directory renames, links and
units run next, and only then is each staged file renamed over its original. The
command holds the controller and updater locks throughout. It prints one JSON
report with the steps, the removed and added key names, the kept roots, the
book's binding check and `operator_next`. It prints no config values. A rerun
after success reports `already_migrated` and changes nothing; rerunning after an
interrupted apply completes it.

**Runbook.** The owner runs every step on the host; nothing here runs
automatically.

1. Choose an off-hours window after the close. Check the book with the
   installed binary and confirm that no research sandbox units are running,
   because the renamed binary names them `nivra-rw-*`/`nivra-candidate-*` and
   would not see `sadr-*` ones:

   ```sh
   ~/ind-sadr-rust-current/sadr-rs readiness --config ~/.config/ind-sadr/rust-paper.json
   systemctl --user list-units 'sadr-*'
   ```

2. Stop the old updater and the controller, and confirm they are inactive:

   ```sh
   systemctl --user disable --now ind-sadr-rust-update.timer
   systemctl --user stop ind-sadr-rust-update.service ind-sadr-rust-paper.service
   systemctl --user is-active ind-sadr-rust-paper.service ind-sadr-rust-update.service
   ```

3. Fetch the release assets with the existing deploy-key alias into a private
   staging directory, and verify the checksum and the tag's commit:

   ```sh
   TAG=vX.Y.Z
   STAGE=$(mktemp -d ~/nivra-stage.XXXXXX)
   git init --bare -q "$STAGE/git"
   git --git-dir "$STAGE/git" fetch -q github-nivra:TharunChougoni/nivra.git \
     "refs/release-assets/$TAG:refs/assets/$TAG" "refs/tags/$TAG:refs/tags/$TAG"
   for f in release.json nivra-x86_64-unknown-linux-musl nivra-x86_64-unknown-linux-musl.sha256; do
     git --git-dir "$STAGE/git" show "refs/assets/$TAG:$f" > "$STAGE/$f"
   done
   (cd "$STAGE" && sha256sum -c nivra-x86_64-unknown-linux-musl.sha256)
   git --git-dir "$STAGE/git" rev-parse "$TAG^{commit}"   # equals source_commit in release.json
   chmod 0700 "$STAGE/nivra-x86_64-unknown-linux-musl"
   ```

4. Preview, review the report, then apply:

   ```sh
   "$STAGE/nivra-x86_64-unknown-linux-musl" host-migrate --release "$STAGE"
   "$STAGE/nivra-x86_64-unknown-linux-musl" host-migrate --release "$STAGE" --apply
   ```

   The report must show `"binding": "matches"`. Keep the applied report; its
   `rewrite_config` steps list the files rollback restores.

5. Verify the installed release with its own binary. `update --check` proves the
   installed tag and commit against the published annotated tag and prints
   `already_current`, or `available` when a newer tag exists:

   ```sh
   ~/nivra-current/nivra self-check --expected-version "$TAG"
   ~/nivra-current/nivra planner config-check --planner ~/.config/ind-sadr/planner.json --session NEXT_SESSION
   ~/nivra-current/nivra preflight --config ~/.config/ind-sadr/rust-paper.json
   ~/nivra-current/nivra update --check --config ~/.config/ind-sadr/updater.json
   systemctl --user daemon-reload
   systemctl --user cat nivra-paper.service
   ```

6. Apply the policy change that `operator_next` names. The planner policy hash
   changes because `decide` and `decision_owner` are gone.
   - `start_service_successor_binds_new_policy`: the book is closed, which is the
     normal off-hours state. `config-apply` refuses a closed book
     (`config_apply_book_closed`) and none is needed. The next rollover
     initializes the successor book's planner authority from the rewritten
     policy. Start `nivra-paper.service` the way the old service was started.
   - `config_apply_then_resume`: the book is open. Run
     `~/nivra-current/nivra config-apply --confirm-paper-review --config ~/.config/ind-sadr/rust-paper.json`,
     then `resume --confirm-paper-review`, then `readiness`, then start
     `nivra-paper.service`. If `agent model show` reports an active non-default
     profile, it was recorded under the old policy hash and refuses with
     `model_selection_policy_changed` until the owner requests it again with
     `agent model set`.
   - `planner_recover_required`: follow
     [Stop, recovery and closed incidents](#stop-recovery-and-closed-incidents)
     before anything else.
   - `inspect_book_readiness`: the book state could not be read without touching
     its WAL; inspect it with `readiness` before starting.

   Keep `nivra-paper.service` disabled at boot, as the old service was.

7. Re-enable updates and remove the staging copy:

   ```sh
   systemctl --user enable --now nivra-update.timer
   rm -rf "$STAGE"
   ```

**Rollback.** Before the renamed controller has written to a book, rollback is
exact. Stop the new units, restore every file the applied report rewrote, move the
two directories back, remove the new links and units, and start the old units:

```sh
systemctl --user disable --now nivra-update.timer
systemctl --user stop nivra-update.service nivra-paper.service
for f in FILES_FROM_THE_REPORT; do mv "$f.pre-nivra" "$f"; done
mv ~/nivra-deploy ~/ind-sadr-deploy
mv ~/.local/state/nivra ~/.local/state/ind-sadr
rm ~/nivra-current ~/nivra-previous
rm ~/.config/systemd/user/nivra-paper.service ~/.config/systemd/user/nivra-update.service \
  ~/.config/systemd/user/nivra-update.timer ~/.config/systemd/user/nivra-alert@.service
systemctl --user daemon-reload
systemctl --user enable --now ind-sadr-rust-update.timer
```

Then start `ind-sadr-rust-paper.service` as before. Keep `~/nivra-releases` as
evidence. After the renamed controller has run a rollover, `config-apply` or
`resume`, the book's planner authority is bound to the new policy hash and the old
binary with the restored planner refuses it. Fix forward from that point instead
of rolling back.

## Dashboard and alerts

The paper worker serves loopback `127.0.0.1:8771`; enter the application dashboard
token through the login UI, never a URL/command. Data APIs/model streams require
bearer authentication. There are no trading control endpoints. Avoid a competing
standalone server on the same port. See [dashboard](DASHBOARD.md).

The paper worker's embedded dashboard is canonical. Enable the standalone
`nivra-dashboard.service` only when the research-only Backtest lab is
needed while the paper worker is stopped. Its unit uses
the installed `nivra`, the same release pointer as paper and updater;
do not run both dashboard servers on port 8771. Host unit enablement and symlink
targets still need verification.

Rotate the dashboard token using `nivra dashboard-token rotate [--config PAPER_CONFIG] [--token TOKEN]`.
When `--token` is omitted, 32 bytes of secure random hex entropy are generated.
Missing parent directories are created without following symlinks; existing path
components must not be symlinks. Existing token files must be regular, owned by
the service user, and single-link. Rotation sets mode `0600` (`-rw-------`)
before writing the new token. Service restart is required to load it.

The workspace is `/`. Legacy `/legacy/` and `/api/model-*` paths are not served.
The updater probes `/api/v1/health`; no legacy `/api/status` alias is served.
`/v2/` is not served.
Its static shell and vendored modules load before login, but every `/api/v1/*`
read still needs the bearer token. **Lock** clears the token, ETag cache and page
state. The workspace holds one SSE stream while live (or one held long-poll per
`longpoll_hold_ms` after fallback) and does not use 500 ms model polling. Running
transcripts append matched `archive_run` display hints and refetch the archived
transcript once on `run_ended`. Live events are hints only and carry
`execution_authority:false`; they never imply an order, fill or validation fact.
The workspace remains read-only and exposes no controls.

The Orchestrator tab lists digest and orchestrator runs alongside operations, strategy
and scorer runs, with their individual model identities and outcomes. System
health lists each configured role's own status and grant expiry; missing
evidence stays unknown.

An optional authenticated tunnel uses the exact `dashboard_public_host` hostname
(no scheme/path) and proxies to loopback. Enable it only through the operator's
reviewed tunnel. The standalone `nivra dashboard` reads the same key from its
optional `--model-config`; without that config only loopback is admitted. Exact
Host/origin and authentication checks still apply. Tunnel reachability is not
trading health.

Optional `alert_webhook_file` points to private HTTPS receiver settings; absence
disables delivery. `nivra alert --test --config PAPER_CONFIG` sends an actual
notification, so use only for an authorised destination. Events include halt/stop,
restart loop, rollover blocked, stale feed, start/end, model fallback and updater
failure. A clean
operator/updater signal stop is informational and reports that explicit resume is
required. A halt or runtime failure remains critical for an open book, including
when an operator also stops it; closed-book notices remain informational. Persisted
dedupe/admission bounds attempts; `suppressed_count` is per dedupe key since that
key's last successful delivery, so unrelated event types do not inflate it. With a
valid configured receiver, the controller's background alert events enter a private
SQLite outbox before the sender is woken; `nivra alert --test` remains synchronous.
At most 64 messages wait in the outbox; transport failures, 429s and server
errors remain queued with bounded backoff and resume after process
restart. Other 4xx responses are terminal under the existing delivery policy. A
crash after receiver acceptance but before the local receipt commits can resend that
alert after restart; retries can deliver duplicates and do not guarantee delivery.
The payload includes its dedupe key, but receiver-side deduplication is not guaranteed.
An absent or invalid receiver disables sending, and a full or unwritable outbox
leaves the existing redacted failure marker for operator review. A dead host still
needs an independent observer. Never share webhook URLs/headers.

### Operational health projection (backend foundation)

The controller captures one read-only `operational_health` snapshot per loop after
its work and exposes it as `view.operational_health`. `/api/v1/health` and
`/api/v1/summary` copy that snapshot only when present; they never generate a new
one. `captured_at_ms` is the loop's single completion sample, so an API request
clock never renews the projection, the controller heartbeat, `as_of_ms` or a
quote receipt. There is no UI for this projection yet.

The snapshot has `version`, `captured_at_ms`, `supervisor`, `calendar` and
`alert_transport`. New fields are nullable and mean unknown, not zero:

- `supervisor` reads the bounded current-session `model/supervisor-state.json`
  (64 KiB limit, no follow) and the pending session-log turn with a checked,
  bounded reader (64 MiB cap, no follow, no repair/truncate on read) without
  creating a journal. `config` is `configured`, `unavailable` or `not_configured`
  and `config_reason` is a static code (for example
  `planner_usage_unavailable_recovery_required`, matching the planner status).
  A genuinely absent/disabled policy stays `disabled`; a configured policy that
  failed to load, or whose usage/authority projection failed, is `unknown`
  (never `disabled`), and its static caps remain visible when the original
  policy is available. `pending_source` distinguishes
  the known absences `missing`/`no_pending` from the unavailable sources
  `corrupt`/`incomplete`/`unreadable`/`oversize`/`non_regular`/`symlink`; a real
  checked pending turn is still exposed separately as evidence. Status is
  `unknown` when the state source is not present or the pending source is
  unavailable, never `idle`; a missing log with present state can be `idle`.
  Missing, malformed, oversized, unreadable or other-session state is reported
  as `unknown` with its `state` source; `budget.exhausted` and `backoff.active`
  are `null` unless the present current-session state proves the fact.
  `charged_calls` is the existing provider-usage projection, not a turn-run
  count. `backoff` reports the actual current-session context-exhaustion path
  only; general failure backoff is not implemented here. No next-retry time,
  progress percentage or model-hang conclusion is guessed from an old
  `last_run`. Only static reason/kind codes and bounded counters are exposed; raw
  prompts, transcripts, provider errors, profile/auth data and paths are not.
- `calendar` reports the verified embedded `NSE/CMTR/71775` coverage
  (`2026-01-01`..`2026-12-31`), the current operational date, the requested
  session and the recorded book session separately, plus `days_remaining`.
  `expiring` uses an operator-chosen 30-day warning; it is not an exchange
  guarantee or a new trading risk cap. A current date outside verified coverage
  refuses at startup as `calendar_coverage_missing` before broker/model work; no
  2027 holiday or weekday fallback is invented.
- `alert_transport` is a read-only projection of the host alert outbox. It never
  creates the database, schema or rows, never updates a row and never reads
  payload, token, destination or webhook fields. A live WAL database may still
  create or maintain `delivery.sqlite-shm` lock metadata on a read-only
  connection; `immutable=1` is not used. Missing or malformed stores report
  `missing`/`malformed` with null counts. `delivery.at` is Unix seconds and is
  converted with a checked multiply to milliseconds. Two scopes are separate:
  `queued`/`retrying`/`dead` are current host-outbox rows, capped at 64
  (`outbox_scope`/`outbox_max_rows`/`queued_scope`/`retrying_scope`/
  `dead_scope`), while `transport_accepted` and the last-accepted/last-failure
  timestamps cover the retained delivery window
  `[captured_now_s - 3600, captured_now_s]` with at most 20 retained rows
  (`retained_window`). The window counts are a lower bound, not lifetime delivery
  history, and truncation is unknown. A future timestamp sets `clock_anomaly`.
  Status 0 (or NULL) is `in_flight_or_transport_error`, not a confirmed failure;
  the `last-failure` marker proves presence only, not that a particular attempt
  failed. `dropped` stays null because the store does not record it; `dead` is
  quarantined outbox rows only. A sender 2xx is an ACK, not a human read or a
  delivery guarantee.

## Release build and publish (manual fallback)

This is the off-GitHub fallback. Normally push main and the explicitly requested
annotated stable tag atomically
after the local gate; the same CI run then tests, builds musl and publishes.
Use **Fast musl release** on `main` for a tag added after its CI run completed,
or to finish a failed publication. Pushing a tag alone does not publish. See [development](DEVELOPMENT.md#cutting-a-release) for CI/artifact checks
and retry rules. Never build or test on the paper host. On a development host,
run this only after the full gate passes on the exact commit (`fmt --check`,
strict Clippy, `cargo test --workspace`, `check-release`).

1. Run `sudo apt-get install -y musl-tools` and `rustup target add x86_64-unknown-linux-musl`.
2. Push the annotated tag `vX.Y.Z`, then check out that tag with a clean tree.
3. Run `git rev-parse HEAD` and confirm it is the annotated tag's source commit.
4. Run `target/debug/nivra-tools check-release`; the full gate above already built
   this binary with the same feature set.
5. Run `cargo build --release --locked --offline --target x86_64-unknown-linux-musl -p nivra-cli`.
6. Run `target/x86_64-unknown-linux-musl/release/nivra self-check --expected-version vX.Y.Z`.
7. Run `target/debug/nivra-tools release-artifact --tag vX.Y.Z --binary target/x86_64-unknown-linux-musl/release/nivra --output dist`.
8. Install `nivra-analysis` as `dist/nivra-analysis-x86_64-unknown-linux-musl` and write its `.sha256`.
9. Publish the seven files locally as the tree of `refs/release-assets/vX.Y.Z`;
   do not also dispatch the GitHub workflow for that tag. The empty-ref check and
   `--force-with-lease` make a duplicate publish fail without replacing assets:

   ```sh
   asset_ref=refs/release-assets/vX.Y.Z
   test -z "$(git ls-remote origin "$asset_ref")"
   index=$(mktemp)
   rm "$index"
   export GIT_INDEX_FILE="$index"
   git read-tree --empty
   for file in nivra-x86_64-unknown-linux-musl \
       nivra-x86_64-unknown-linux-musl.sha256 release.json \
       default-prompts.json default-prompts.sha256 \
       nivra-analysis-x86_64-unknown-linux-musl \
       nivra-analysis-x86_64-unknown-linux-musl.sha256; do
     blob=$(git hash-object -w "dist/$file")
     git update-index --add --cacheinfo "100644,$blob,$file"
   done
   tree=$(git write-tree)
   export GIT_AUTHOR_NAME=nivra-release GIT_COMMITTER_NAME=nivra-release
   export GIT_AUTHOR_EMAIL=release@users.noreply.github.com
   export GIT_COMMITTER_EMAIL=release@users.noreply.github.com
   commit=$(git commit-tree "$tree" -m "Assets for vX.Y.Z")
   git push --force-with-lease="$asset_ref:" origin "$commit:$asset_ref"
   rm "$index"
   ```

10. Run `gh release create vX.Y.Z dist/* --verify-tag`.
11. Verify the release attachments, immutable asset ref, `release.json` source
    commit and all SHA-256 files before allowing the updater to install.

Each tag gets one publisher. The empty-ref check makes a duplicate publish fail
without replacing assets. Manually built and GitHub-built binaries are not
bit-identical. The updater verifies against the published `.sha256`, so either
artifact installs through `nivra update --tag` unchanged.

Scanner screens and optional streaming settings are documented in
[Upstox V3 market feed](UPSTOX_MARKET_FEED.md). Enabling the feed requires a reviewed
planner config change; installing source alone keeps REST-only deployments on REST.

### Configuration bounds

Omitted keys preserve the previous values. Default-valued additions are omitted
from serialized configuration so existing sealed configuration hashes remain stable.

| Configuration file / key | Default | Valid range |
| --- | --- | --- |
| Planner: `digest.max_analysis_tools`, `decide.max_analysis_tools` (also each post-close role) | 4 | 0–64 calls per invocation; 0 disables analysis calls |
| Planner: `max_handoff_instruments` | 20 | 1–20 keys per Luna handoff or snapshot/feature tool request |
| Runtime: `history_download.quota_wait_max_attempts` | 1800 | 0–1800 retries after the initial shared-quota reservation attempt |
| Runtime: `history_download.quota_wait_poll_ms` | 1000 | 1–60000 ms between denied reservations |
| Runtime: `history_download.reserve_half_hour_reads` | 300 | 0–1499 shared reads held back from history requests in each rolling 30-minute window; other reads may use them |
| Runtime: `history_download.reserve_second_reads` | 2 | 0–4 reads held back from history in the rolling second window |
| Runtime: `history_download.reserve_minute_reads` | 30 | 0–119 reads held back from history in the rolling minute window |
| Runtime: `dashboard.backtest_timeout_ms` | 3000 | 1–3000 ms |
| Runtime: `dashboard.history_max_files` | 256 | 1–256 response files per archived model run |
| Runtime: `dashboard.workers` / `dashboard.queue` | 4 / 16 | 1–16 workers; 1–256 queued requests; a full queue returns 503 `busy` |
| Runtime: `dashboard.static_max_age_s` | 86400 | Retired/ignored compatibility key; the React shell and its stable-named assets always revalidate with ETags |
| Runtime: `dashboard.gzip_min_bytes` | 1024 | 0–1048576 bytes before gzip is attempted |
| Runtime: `dashboard.history_read_max_bytes` | 8000000 | 1000000–64000000 bytes per history file |
| Runtime: `dashboard.history_list_cache` | true | Retired/ignored compatibility key; serialization unchanged |
| Runtime: `dashboard.model_feed_unavailable_grace_ms` | 5000 | Retired/ignored compatibility key; original 0–60000 ms validation remains |
| Runtime: `dashboard.sse_max_streams` | 3 | 0–16 concurrent SSE readers; over the cap clients fall back to long-poll |
| Runtime: `dashboard.sse_heartbeat_ms` | 15000 | 1000–60000 ms |
| Runtime: `dashboard.sse_write_timeout_ms` | 30000 | 1000–60000 ms |
| Runtime: `dashboard.longpoll_hold_ms` | 25000 | 1000–60000 ms |
| Runtime: `dashboard.transcript_max_bytes` | 2097152 | 65536–16777216 bytes per transcript |
| Runtime: `dashboard.transcript_text_max_bytes` | 16384 | 1024–262144 bytes per clipped text |
| Runtime: `dashboard.transcript_args_max_bytes` | 4096 | 256–65536 bytes per clipped tool arguments |
| Runtime: `dashboard.blob_slice_max_bytes` | 262144 | 4096–4194304 bytes per blob slice |
| Runtime: `dashboard.series_max_points` | 2000 | 64–10000 points per series response |
| Runtime: `dashboard.series_max_range_ms` | 86400000 | 60000–604800000 ms explicit series range |
| Runtime: `dashboard.equity_max_points` | 4096 | 64–65536 recorded equity checkpoints |
| Runtime: `dashboard.equity_sample_interval_ms` | 15000 | 1000–300000 ms minimum checkpoint spacing |
| Runtime: `dashboard.price_batch_instruments` | 24 | 1–256 instruments per display batch |
| Runtime: `dashboard.price_batch_interval_ms` | 2000 | 500–60000 ms display batch cadence |
| Runtime: `dashboard.failed_auth_per_minute` | 30 | 1–600 failed-auth token bucket per minute returning 429 when exhausted |
| Runtime: `dashboard.retention.pack_after_days` / `delete_after_days` | off | 1–365 days; delete must be greater than pack |

Instrument parsing retains an independent 20-key ceiling. Dashboard evaluation
and archive response scans retain absolute ceilings of 3000 ms and 256 files.
Quota waiting preserves cancellation and preparation deadlines; HTTP errors are
not retried, and shared quotas are never reset. A zero retry setting keeps the
initial reservation attempt and the existing final-denial sleep semantics.

Runtime config accepts `execution` with defaults `history_sample_interval_ms: 30000`,
`history_max_samples: 20`, `legacy_entry_cutoff_minute: 915`,
`legacy_liquidation_minute: 915`, `legacy_stop_multiplier: "0.98"`,
`legacy_target_multiplier: "1.02"`, `session_start_minute: 555` and
`session_end_minute: 930` (IST minutes). These settings are excluded from book and
scanner state. Model-led protection still uses model exit intents. Financial
freshness remains two seconds. Session start must precede end; history length
must be positive. Runtime `daily_history_sessions` defaults to 21 and must be at
least 21; planner feature
lookbacks retain their existing schema and need at least 21 bars. Preparation uses
`history_preparation_start_minute: 480` and
`history_preparation_cutoff_minute: 550`, including its deadline/cancellation;
start must precede cutoff and cutoff must be before 555 (09:15 IST).

### Model archive retention

Archive pack and delete are off unless `dashboard.retention.pack_after_days`
and/or `delete_after_days` is configured. The paper runtime runs retention
after persisting post-close; it never activates a session. Manual preview is
the default:

```sh
nivra retention --config /private/session-config.json --dry-run
nivra retention --config /private/session-config.json --apply
```

Only dated sibling sessions older than the configured age are eligible. They
need a matching paper report with `portfolio.closed=true`, `stopped=false` and
`halted=false`; current/future dates, open/stopped/halted or unknown reports are
skipped. Pack and delete do not open a book, journal or credential. Eligible model run directories
are compressed into private `model-archive/<date>.tar.gz` using ustar, published
atomically before raw directories are removed. Their transcripts are retained
under `model/transcripts/`; `runs.ndjson`, `prompts/`, `live-stream.json` and all
non-model files are preserved. Delete retention removes expired raw runs and the
compressed archive while preserving those display files. The JSON result/log and
status `retention` field report completed/planned dates and logical bytes freed.

A failed pack keeps the raw artifacts and its temporary file; the next apply
cleans that exact temp path. An existing published archive with remaining raw
directories is a conflict: both are preserved for inspection, never overwritten
or assumed equivalent. Conflicting transcript copies are also preserved. No pack or delete settings are active unless configured.

Closed sibling sessions are size-bounded by default, including when pack and
delete are off. The bound does not rewrite the configured session directory,
so the running paper book stays untouched across midnight. It keeps the newest
`dashboard.retention.keep_full_quote_bodies` `wide_quotes` bodies (default 32)
and replaces older bodies over `max_quote_body_bytes` (default 4,096) with a
`compacted_quote_receipt` that records sha256 and original length. `plan.json`,
`output.json` and `input.json` over `max_model_file_bytes` (default 65,536)
become `compacted_model_artifact` receipts. The `state` book row, `failure.json`,
transcripts and shared journals stay byte-for-byte. A `paper.sqlite-wal` or
`paper.sqlite-shm` file skips that database. After vacuum the book-identity file keeps its binding and
records the new device and inode. A compacted quote row fails paper replay with
`paper_replay_compacted_quote_body` instead of being treated as a live quote.

Retained transcripts remain available to the workspace and the v1 API after the raw
run directory is packed or deleted. The list index, transcript detail and
content-addressed prompts stay readable; a blob whose original artifact is gone
returns 404, so the transcript is shown without inventing the missing value.

### Default prompt files

For file-based prompts, copy the exact revision's `packaging/rust/prompts/`
directory beside the planner JSON as `prompts/`. It must contain `objective.md`,
`data_owner.md`, `supervisor.md`, `orchestrator.md` and `scorer.md`; the other
packaged files are shipped for the fixed release list and are not read. The directory is optional for existing configurations; when absent,
embedded defaults preserve their existing policy bytes. Once present, missing,
empty, unreadable, oversized or invalid UTF-8 files refuse configuration loading.
There is no partial-directory fallback.

The owner policy selects `data_owner.md` and `orchestrator.md`; legacy books
retain `data.md` and `supervisor.md`. Explicit orchestrator system prompts and
experiment profile overrides retain their precedence. Default file contents are
part of the sealed trading-policy hash; their location is not. Edit these files
only with the existing suspend → `config-apply --confirm-paper-review` → audited
resume workflow. The controller never reloads these default files in flight;
a changed file requires reviewed apply before further model work. Keep a backup
of the previously reviewed files for rollback. Installing files or applying a
config does not authorize starting a session.

### Owner-only planner migration

The persistent orchestrator is the only decision owner. Planner JSON that still
carries `decision_owner`, `decide` or `handoff_gate` refuses with
`planner_config_invalid`, and a book bound to an older policy hash refuses with
`decision_owner_migration_required`. On a stopped book, write the owner policy
to a new planner config, check it with `planner config-check`, review its exact
prompt and credential bindings, then use `config-apply --confirm-paper-review`
and audited resume. The authority checks preserve original journals and separate
role charges. Installing a build does not migrate a book.
Rollback means restoring the reviewed predecessor config and compatible binary
through the documented updater guards; never edit seals or replay pending work.
The current-market `planner live-preview` compiles only a factual digest; use
read-only `agent ask` for owner-session inspection.

Model profiles use the existing inline `digest`, orchestrator `model`,
`model_profiles` and `admitted_models` configuration. Review their exact model,
transport and application credential bindings before applying policy. A profile
must be admitted and promoted at the normal turn boundary before owner calls can
use it. Native xAI tool flags are incompatible with OpenAI Codex profiles; refusal
keeps the active selection and never chooses another model. Configuration/catalog
presence does not replace a valid application grant and live-catalog listing.

### Display instrument sources

Existing configs read the frozen session's `upstox-NSE.json.gz` for catalog display
without an extra provider request. To capture complete-market public BOD metadata,
set the following dashboard option through the normal reviewed config workflow:

```json
{"instrument_catalog":{"source":"complete","max_source_bytes":20000000,"max_decoded_bytes":100000000,"max_rows":300000,"max_text_bytes":256}}
```

The object is inside `dashboard`. `source:"session_nse"` explicitly selects the
legacy source; absence has the same behavior and preserves serialized config
compatibility. All size/row/text limits must be positive resource bounds, not
trading risk caps. A source exceeding them becomes visibly unavailable; there is
no automatic limit increase or fallback to another source/broker.

Capture runs once in a background worker after cash-universe preparation. The
complete source is stored as `upstox-display-complete.json.gz` with original
`instrument-display-source.json` hash/receipt/session binding. Existing complete,
incomplete and failed captures are preserved; they are never downloaded over.
Nonregular, linked, changed or mismatched sources refuse. No metadata worker
starts/resumes a session or alters financial journals. Confirm source receipt,
observed segments, rejected/ambiguous rows and memory/bandwidth on the target host
before treating display coverage as qualified. BOD contract chains do not provide
current prices or margin; the UI reports those as uncaptured.

### Prospective observation status

When the reviewed observation capability is enabled, the controller's
`research_tasks` projection carries an `observation` block derived read-only
from the original candidate queue and lifecycle ledgers:

- `queue`: pending/running/unknown/cancel-requested/succeeded counts, total,
  oldest age, next deadline, remaining time and an explicit `truncated` flag;
- `lifecycle`: recorded/matured counts and the actual milestone, origin
  session/continuation, score status, gaps, `source_scope`, exit status,
  measurement gaps and measurement SHA per candidate; a broken predecessor
  ledger makes the block `unavailable`, never zero;
- `cohort`: account, stable cohort key, feed and dataset ABI;
- `trials`: the global fold of admissions, executions, terminal
  failures/cancellations and unknown exposure across every verified lineage;
  counts are null or explicitly lower-bound when the fold is partial;
- `measurement`: `independent_episodes`, no portfolio claim and no aggregate
  PnL;
- `capture`: the distinct controller capture grant, used/remaining calls, last
  receipt age, timeout/row/key budgets and journal coverage
  (`journal_readable` / `journal_unavailable`). `coverage_label` says the label
  is journal readability only and `current_eligibility` is `not_evaluated`;
  `status: journal_readable` is not capture eligibility. It never calls the
  provider.

`as_of_ms` is the controller sample; a later GET never renews it. Missing or
malformed source state is `unavailable`, never a fake zero, idle or progress
claim. The projection is operational metadata, not a financial-profit or
activation claim. A disabled capability reports `disabled`; a stopped or
halted book reports `not_eligible` for capture and is never restarted.
