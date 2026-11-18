# Model harness

Paper only. Pi owns the paper decision, research and review turns: it runs the
provider call, conversation session, tool loop, compaction and usage report. Rust
builds each factual brief, publishes the lane's tools, schedules turns, validates
the returned TradePlan and commits paper fills through the unchanged deterministic
gates. See [pi agent lanes](AGENT.md) for the service, settings and verification
commands.

| Lane | Exact model / transport | Tools and output |
| --- | --- | --- |
| Decision | Reviewed `planner.agent.lanes.decision` provider/model spec | Rust brief, read-only evidence tools and `plan`; selects paper action, size and holding period |
| Research | Reviewed `planner.agent.lanes.research` provider/model spec | Read-only evidence, workspace write/run, registered analysis and candidate tools; answers the decision lane's research requests |
| Review | Reviewed `planner.agent.lanes.review` provider/model spec | Read-only evidence; one redacted review after the session seal, carried as next-session memory |

Each configured lane needs a reviewed provider/model spec, positive daily call and
cost budgets, a turn timeout and a tool-call bound. Pi reads the application's own
`auth.json` and `models.json` from its configured agent directory; Rust starts it
as a subprocess with an explicit environment allowlist and never reads those files.

## Retained legacy planner

The earlier two-role Rust harness is not on the live path. A pi paper planner
refuses an enabled `digest`, an enabled `orchestrator`, a `model_config` or
`approval_auto_renew` with `pi_paper_legacy_model_config_refused`, and the running
controller refuses a policy without `agent.mode: "paper"` with
`pi_paper_planner_required`. The retained backend still produces digest turns for
offline `nivra paper-replay`; original archived journals, usage receipts,
transcripts, prompts and supervisor projections remain readable.

| Retained role | Original binding | Retained use |
| --- | --- | --- |
| Digest (formerly Luna) | `digest` role | Offline recorded-reply replay and archived digest evidence |
| Owner decision (formerly persistent orchestrator) | `orchestrator.model`, `model_profiles` | Archived plan-head projection and earlier admission evidence |

Historical usage journals still decode (`decide-` reservations included), and the
read-only dashboard still projects the earlier supervisor/research records. No
live model turn uses them. The retained role mechanics are described below with
their historical status made explicit.

## How the decision lane is woken

Rust's trigger engine observes each scan cycle. A fill, position risk, a
persistent top-ranked candidate, an hourly schedule turn or the pre-close minute
wakes the decision lane; triggers that arrive while a turn runs merge into the
next batch under the highest-priority kind. Rust builds the brief from frozen
controller evidence and publishes only the tools the lane may call. Only a
completed turn whose final plan passed validation can reach the paper plan head;
an abstention is an empty action list. No eligible evidence means no model call.
A due turn is skipped, never invented, when the lane is unconfigured, its budget
is exhausted or the book is not trading.

A pi research lane serves the decision lane's `research` requests; completed
answers are delivered in a later decision brief and marked delivered once that
turn is durable. A pi review lane runs once after the seal and its redacted text
becomes the review memory the next session's briefs carry. Registered analysis
jobs, the workspace and the strategy sandbox are separate research evidence; they
never place, size or promote a paper trade.

The `planner-harness-v3` audit label and older projection records remain readable.

Pi reads each lane's standing prompt from `agent/prompts/<lane>.md` in the built
agent package, or from the directory in `planner.agent.prompts`. Rust hands pi the
brief and trigger; the agent reports the prompt's SHA-256 with each turn. The
earlier planner's `prompts/` directory, content hashes and embedded default
prompts are retained only for offline replay and archived-policy evidence; a pi
paper planner does not load a sibling `prompts/` directory.

An optional `planner.experiment_profile` can replace the retained legacy prompt
text and tighten tool and role tunables without a release. It is excluded from the
trading-policy hash but is recorded in a pi decision's claim and plan head, so a
change between reservation and commit refuses that plan. An invalid profile keeps
the previous one, logs `experiment_profile_rejected`, and does not halt. A valid
change writes `experiment_profile_applied` and alerts once. Check a file with
`nivra planner profile-check`.

`planner.exit_policy` is frozen trading policy and applies to pi plan actions.
`target` and `time` default to `review`: crossing them raises `held_exit_review`
and does not sell. `stop` defaults to `execute`. `on_halt` defaults to `freeze`,
which blocks new buys and model calls but does not liquidate; an executing stop
stays armed. Absent `exit_policy` keeps the previous execute-on-intent and
liquidate-on-halt behaviour. The decision lane may submit `revise_exit` to update
a held intent without a trade.

## Historical: persistent supervisor

The earlier planner ran one persistent supervisor session per trading day with
its own session log, notes, seed, compaction, aside questions and wake triggers.
None of that runs on the live pi path: pi owns each lane's session, compaction and
turn state. The artifacts (`model/supervisor-state.json`,
`model/session/<date>/`, `seed.json`, failure records) remain readable archive
evidence and are never rewritten or replayed, and the read-only dashboard
operational-health projection still reads them without writing. The historical
sections below retain their detailed mechanics for archive interpretation; the
plan-head authority section that follows is current.

### Plan-head and ledger authority

The book has one durable plan-head authority. A pi paper planner initializes it
with the current policy: `enabled`, `model_led` not `false` and a decision lane.
A book whose durable authority was written by the earlier orchestrator refuses
with `decision_owner_migration_required` until the reviewed `config-apply`
transition records the pi owner. The planner config has no `decision_owner`,
`decide` or `handoff_gate` key, and `deny_unknown_fields` refuses a file that
still carries one.

To review a policy without applying it, run
`nivra planner config-check --planner FILE --session DATE`. It reports the policy
hash and the resolved `agent` lanes, refuses a policy with missing pi
prerequisites, and never writes a policy.

Installing a release does not rewrite a planner config or qualify host operation.
A pi decision claim binds the policy, profile, model/provider, portfolio revision,
instrument revision and input hash before the turn runs. Plan-head activation and
its SQLite receipt share one transaction, and recovery reconstructs the session
result from that receipt. A restart that finds a running decision without a
committed receipt refuses the work as `owner_decision_interrupted` and records the
session result; it never resends an ambiguous request. A committed plan is
projected from its receipt without another model call.

The controller requires durable ledger authority initialized by the exact current
policy. If the policy's config hash differs from the durable authority, startup
refuses with `decision_owner_migration_required` instead of rewriting the
authority; host protection continues. To change the policy, write the new planner
config while stopped, check it with `planner config-check`, then run
`config-apply --confirm-paper-review` and `resume`. Rollback is the same reviewed
transition using the predecessor planner file. A halted book cannot run
`config-apply` and stays archive-only. A stopped book that is not halted uses
`config-apply`.

Every new authority writes its owner marker in the same transaction. A missing,
malformed, foreign-owner (including a stored `legacy` owner) or stale marker
refuses as `planner_authority_owner_missing`, `planner_authority_owner_invalid` or
`planner_authority_owner_mismatch`. No command reconstructs it, and such a book
stays archive-only. Startup preserves the original active head and any stronger
sticky data, news or configuration block; a pending claim is marked interrupted
without replacing that block or clearing the head.

### Historical orchestrator model selections

The earlier planner used `orchestrator.model_profiles` and
`orchestrator.admitted_models` to bind reviewed profile names to model IDs,
transports, and application-owned credential paths. The `default` profile
referred to the configured `orchestrator.model`.

`model/model-selection.json` preserves the session's active and pending
revisions. Its receipts record the policy hash, model identity, and credential
path hash. A new session started with the configured default. For the pi
model bindings, see the [pi lane configuration](AGENT.md#prepare-and-verify).

### Historical operator questions and owner instructions

The earlier planner used `model/agent-requests.json` to record request IDs,
expiry, and outcomes. Its aside replies are archived in `aside-reply.json`
under the session. Owner instructions remain in `model/directives.ndjson`
with their original session, revision, and expiry.

That planner's `orchestrator.system_prompt` supplied the standing instructions actually sent
to supervisor and lifecycle model calls, with the host-owned objective prepended.
Its precedence is explicit `orchestrator.system_prompt`, then the experiment
profile's supervisor prompt, then the compiled supervisor default. If the
explicit field is absent, the existing profile/default behavior is unchanged.
`orchestrator.trigger_prompts.<trigger>` overrides the short user-turn template
for one of `session_open`, `resume`,
`change`, `pre_close`, `post_fill`, `failure_streak`, `idle`, `compaction` or `handoff`. Templates may use only the
placeholders documented for that trigger (for example `{{session}}`,
`{{notes}}`, `{{recent_turns}}`, `{{summary}}` and `{{change}}`); an unknown or
misspelled placeholder is a configuration error, never an empty render. The
standing prompt is outside the compactable log, and a turn records the prompt
version it was built from. Configuring either field binds the resolved content
in the planner policy hash, so a live edit follows the existing suspend and
`config-apply` path instead of halting the book. Absent fields are omitted from
serialization, so existing configuration bytes and hashes are unchanged.

### Historical: supervisor session, owner tasks and notes

The following described the earlier persistent Rust supervisor and its owner
tasks. None of it runs on the live pi path; original artifacts remain readable and
are never rewritten or replayed.

The supervisor ran as one persistent session per trading day, not unrelated
one-shot calls. `model/session/<date>/log.bin` is an append-only, length-framed
and SHA-256-checked log: a turn record is durable before the provider call and
exactly one result follows it. The reader stops at the last complete frame, so a
chat reader or a restarted controller never sees a partial record. Append repairs
only an incomplete final header or JSON body. A complete frame with invalid
checksum, JSON, length or non-increasing sequence refuses append and preserves
the original bytes. Unreadable, non-regular or oversized logs also refuse rather
than starting a replacement log. The legacy frame length is not checksummed, so
a simultaneous length and incomplete-body corruption cannot be distinguished
from an interrupted final write. A durable cursor ahead of the readable prefix
remains an integrity incident, never an automatic reset or re-anchor. On resume, a
committed action receipt completes the pending turn without a second call;
otherwise the turn is replayed with the same id, and per-turn action receipts
refuse a duplicate commit. A corrupt or unreadable state file is refused rather
than replaced.

A reviewed `owner_tasks.review` policy adds one sealed post-close owner review.
The field is optional: absence keeps existing policy bytes and behavior, and the
post-close scorer trigger is never silently enabled or disabled. When enabled,
the policy must enable the same supervisor trigger. The review turn uses
that admitted supervisor role, model, persistent session and daily usage budget.
It has no trading, market, workspace, web or research tools. The host seals the
closed report and current book, sends the bounded sealed evidence inline with
the compiled review prompt, and requires exactly one JSON review with cited
observations and hypotheses, explicit uncertainties, and memory copied from a
cited claim. An uncertainties-only review with empty observations, hypotheses
and memory is valid when it cites no claim; the host never invents evidence to
fill those arrays. Foreign citations, paraphrased memory, a missing binding and
an action or TradePlan body are refused.

One attempt is allowed per original sealed report. The turn is durable before
the call; the original artifact `model/reviews/<review-id>.json` is written once
with a 0600 file under a 0700 directory, no-follow exclusive create and parent
sync. It is bounded and never overwritten. A committed source reconstructs the
missing result and action receipt without another model call. A missing source
is recorded as `owner_review_interrupted` and is never replayed, including after
a restart. An unreadable, symlinked, oversized or malformed source is retained
and refused as `owner_review_source_invalid`; an action receipt without its
source is `owner_review_source_missing`.

The review path opens `model/reviews` from the data directory without following
a symlinked ancestor. An existing directory must already be 0700 and owned by
the current uid; the host does not chmod an existing directory. A symlinked
ancestor, a foreign owner or a wider mode refuses as `owner_review_path_invalid`.
The artifact must be a regular 0600 file with one link, the current uid and the
size bound; otherwise it refuses as `owner_review_source_invalid`. The host
revalidates durability by syncing the same pinned file and directory descriptors
before it appends the session action or result. A failed sync appends nothing
and preserves the partial artifact for recovery.

A recorded review keeps the prompt and policy hashes from its original turn, so
it remains valid evidence after a binary prompt change and can still seed the
successor `review_state`. Only a new provider invocation must match the current
compiled prompt and policy. A changed original `report.json` is an integrity
conflict (`owner_review_report_changed`), not a new attempt. A failed attempt
records its specific code in the artifact and `model/supervisor-failure.json`.
The dashboard `owner_review` projection and `/api/v1/health` expose only a
bounded read-only schedule/status; they add no review control and no writer
authority.

A reviewed `owner_tasks.research` policy adds one bounded research turn over
existing terminal evidence. It is optional and absent by default: omission
keeps every existing policy byte, compiled template binding and tool frame
unchanged. When enabled, the policy must keep the orchestrator decision owner,
enable the same supervisor trigger, and enable either the reviewed candidate
compute capability or the owner workspace. The new `owner_tasks.research_prompt_sha256`
binding is added only when enabled; old policies never gain it. The compiled
`SessionTrigger::Research` template is not included in an old policy's prompt
hashes because only explicitly configured trigger prompts are bound.

An optional **session agenda** (`owner_tasks.agenda {pre_open, pre_close}`,
both `false` by default) gives the decision owner an explicit pre-open
turn at the supervisor `session_open_minute` and a pre-close turn at its
`pre_close_minute`. Each runs at most once per session as an ordinary
supervisor turn (notes and digest directives only, no order tools), only when
no owner decision is queued, the book is not halted or stopped, the supervisor
is cooled down and has allowance. A failed agenda turn is retried through the
usual failure streak; a successful one is terminal for the session. A missed
pre-open turn is skipped once pre-close is due. The post-close sealed review is
separate and unchanged. Absent `agenda` keeps every existing policy byte.
It requires the orchestrator decision owner and an enabled supervisor trigger.

Decision prompts reconstruct committed review and research turns only as short
references (`owner_task` kind and identity, outcome, reason, action refs and
the reply's SHA-256 and length), never their sealed evidence, notes block,
evidence payload or full reply. The session log itself is unchanged.

Research runs in its own **research lane**. It shares the
orchestrator's reviewed model selection and harness, but it has its own worker
slot, its own session log (`model/session/<session>/research-lane/`), its own
`research` usage bucket and its own failure record
(`model/research-lane-failure.json`). The usage inventory accepts the
`research-NNNN` reservation prefix alongside the trading role buckets; a
completed research turn is a valid journal record and never invalidates later
usage reads. Trading is never preempted: a research
turn in flight never holds a decision, a queued paper decision, handoff or
compaction never waits for research, and research failures, budget exhaustion
or a stuck research turn never park trading, arm the owner failure streak or its
backoff, or charge the `supervisor` allowance. Enabling research requires the
capped `owner_tasks.research.lane` profile (`research_lane_required` otherwise):

| Key | Default | Meaning |
| --- | --- | --- |
| `model_id` | absent | Optional provider-listed model. Omit to use the orchestrator's active profile. |
| `max_tool_turns` | 8 | Tool turns inside one research call |
| `timeout_ms` | 600000 | Wall limit for one research turn (also bounded by the cutoff) |
| `max_turns_per_session` | 6 | Research turns per session |
| `max_calls_per_session` | 48 | Provider calls charged to the `research` bucket |
| `max_tokens_per_session` | 2000000 | Provider input+output tokens charged to the `research` bucket |
| `min_interval_ms` | 900000 | Minimum time between two research turn starts |
| `during_market` | false | `false` admits research only outside market hours |
| `max_open_requests` | 4 | Queued plus running trading requests |
| `request_ttl_ms` | 21600000 | Queued request expiry |

An exhausted cap parks only the lane (`research_lane_turn_budget_exhausted`,
`research_lane_call_budget_exhausted`, `research_lane_token_budget_exhausted`);
a halted, stopped or disposed book parks it with
`research_lane_book_not_runnable`; spacing and market hours defer it
(`research_lane_min_interval`, `research_lane_market_hours`).

The lanes talk only through a durable typed queue in the experiment ledger
(`lane_messages` plus an audit `lane_message_events` table). A decision turn may
call `request_research` (offered only to a bound decision turn when the lane is
enabled) to post a bounded `research_request`; it returns at once and never
waits. Requests are idempotent per decision turn and question, capped by
`max_open_requests`, expire, and move `queued → running → done|failed` or
`queued → cancelled` only through compare-and-set transitions. The research lane
serves the oldest queued request before terminal evidence. A finished turn posts
a `research_result` with a short redacted summary (≤600 characters) and the
artifact reference; the next decision turn receives at most four undelivered
results as `research_updates` and they are marked delivered once that turn is
durable. Trading never sees the research trace. Settlement is re-derived from
the lane log, so a crash between the lane log and the ledger is repaired on the
next tick; a request claimed by a turn that never became durable, or by an
interrupted turn, fails as `interrupted` and is never re-served.

Admission is a read-only projection over queued requests and terminal verified
evidence: a verified candidate
result, a verified registered-analysis result, a terminal `CandidateStopped`
failure, or terminal lifecycle maturity. Evidence ids already present in a
research turn are excluded, so one attempt per original evidence id survives
compaction and recovery. Every projected record is validated before admission:
a malformed or missing result hash, session, revision or workspace ref is an
explicit static unknown and never becomes a readable evidence id or a paid call.
Terminal lifecycle maturity must carry its original replay-report hash; a
missing or non-hash value is refused rather than replaced by a derived hash. A
running candidate is not maturity evidence. An open paper position no longer
blocks research. No eligible evidence and no queued request means no provider
call and no usage-journal initialization.

The durable turn is appended before the provider call. The host writes one
bounded `model/research/<research-id>.json` artifact (0600 file in a 0700
directory, no-follow exclusive create, canonical JSON, parent sync) and the
`owner_research` action receipt before the result. A committed artifact or
action reconstructs the missing result without another model call; neither
artifact nor action is an explicit `owner_research_interrupted` terminal state
and is never replayed. A recovered artifact result is
`recovered_research_receipt`. Research results live only in the lane log and
never touch trading supervisor state, its failure counters or applied hashes. A changed or unverifiable source refuses with
`owner_research_source_changed`; a changed pinned workspace file refuses with
`owner_research_workspace_changed` (and the other bounded
`owner_research_workspace_*` codes). The typed `ResearchOutputV1` is bounded,
rejects
financial keys and TradePlan bodies, and accepts only the supplied evidence id
or pinned workspace paths as citations. Research output is advisory evidence,
never an order, price, fill, cash, fee, P&L, promotion or activation claim. The
read-only dashboard `owner_research` view and operational-health `research`
block are derived from one capture at the controller completion sample, so both
report the same `as_of_ms` in every branch; a later health/summary GET copies
it. A retained turn with an unverifiable owner-task binding reports
`owner_research_source_unverified`, and malformed evidence reports
`owner_research_source_invalid`, never a settings-invalid code.

A research turn receives only `candidate_submit`/`candidate_status`/
`candidate_result`/`candidate_cancel` and `workspace_list`/`workspace_read`/
`workspace_write`. It never receives TradePlan, order, market, portfolio,
financial, web, policy, `workspace_run`, `workspace_install` or
registered-analysis enqueue tools. Workspace writes are restricted to `notes/`
and `skills/`. Candidate submits pass no decision evidence and reuse the
existing candidate queue, guard, helper and cgroup; the optional reviewed
`candidate_execution` argument remains available only when that capability is
enabled. Review remains tool-less. An optional reviewed
`owner_tasks.research.quote_capture` exposes `research_quote_capture`, which
reads only original archived current-session receipt clocks and keys; it never
calls a provider, reads OHLC, interpolates or renews freshness. Dynamic
observation is separate work and is not claimed here.

Pinned workspace text is advisory evidence, never a system instruction or native
execution. A `WorkspaceRefV1` names the original source session, workspace-
relative path, sha256, capture time and `notes`/`skills` kind. The host resolves
the current or a known predecessor session under `sessions_root`, requires the
same original book binding, normalizes the path, refuses symlinks and escapes,
opens a bounded no-follow regular file, and verifies its size and hash. Pinning
uses the reviewed `workspace.max_file_bytes` bound and the same bounded UTF-8
reader as a pinned read: a larger, non-UTF-8, foreign-owned or world-writable
file is not emitted as a readable ref. Missing or changed files fail explicitly
with a bounded static code; text is never copied, and skills are procedures, not
executable code. The current session's seed may carry at most 16 such refs; no
predecessor file is updated or repaired. A seed ref path is validated by
workspace-relative components, so a valid name such as `notes/v1..2.md` is
allowed while a real `..` parent, absolute path or escape is refused.

Compaction and handoff turns also carry typed, version 1 `host_lifecycle`
metadata in their original turn request before inference. Compaction records the
exact `up_to_seq` prefix to summarize. Handoff records the calendar-verified
successor session and host-built seed. Resume restores the recorded lifecycle
kind and trigger template; a pending legacy lifecycle turn without this metadata
is explicitly refused as `supervisor_legacy_lifecycle_pending`. If the summary
or handoff receipt was committed before a crash interrupted the result append,
the host reconstructs the result from that receipt without another model call.
Lifecycle metadata also retains the applied revisions. A durable state cursor
reconciles a recorded result's original clock and revisions exactly once if a
crash interrupted the state update, preserving run counts and cooldowns.
New stored replies use the transcript secret redactor before byte bounding.
Plain text credential assignments use the same denied keys as JSON fields.

The successor date comes from the application's exchange calendar. If the date
is outside verified calendar coverage, handoff refuses as
`supervisor_successor_calendar_unavailable`; it does not guess a weekday. The
host writes `seed.json` before a handoff model call. Seed creation is immutable:
a retry with identical valid JSON succeeds, while a differing seed is refused
as `session_seed_conflict` and an unreadable, malformed or symlink artifact as
`session_seed_invalid`; existing artifacts are never replaced. Creation uses a
0600 file, no-follow exclusive create, file sync and parent-directory sync. If
the handoff model fails, the seed remains available and the log records the
handoff failure; non-keep notes are not expired as though handoff succeeded.
When allowance exhaustion parks model calls, the host can still make this
seed-only handoff and record `supervisor_parked_budget`, without model or
credential lookup.

New daily seeds use schema version 2. The shared host handoff builder records the
source handoff turn ID and, when available, the preceding completed turn's exact
log sequences, result hash and action receipt references. It carries the standing
instructions, keep notes and descriptive holdings with original mark times;
executable exit intents and decision IDs are excluded. Nonexpired queued/running
operator requests contribute separate ID/state/expiry references, without their
question or instruction payloads. These references never restart or reapply work.
Configured registered-research tasks contribute bounded, path-free references
from the original queue; disabled or unavailable research stays explicit. A
successor resolves those refs on the original queue and may record a read-only
adoption. That receipt does not restart, refuse, or reauthorize the predecessor
worker. Legacy seeds remain readable as recorded evidence. New turn construction
distinguishes an absent seed from an unreadable, malformed or unsupported seed;
the latter refuses model work instead of silently discarding the carry. Recovery
checks the versioned carry before persisting it.

When the reviewed owner review policy is enabled, the successor seed also
carries an optional `review_state` object with path-free hash references
(`review_id`, `sealed_sha256`, `artifact_sha256`, `status`, `completed_ms`) to
original review artifacts, bounded to eight references. The host validates each
original artifact before writing the seed. The field is omitted when review is
absent, so existing seeds remain valid. Older binaries reject the new version 2
field: before rolling back to a release without owner review, restore the
original seed or disable the policy before handoff. Existing seeds and journals
are never rewritten.

When the reviewed owner workspace policy is enabled, the successor seed also
carries an optional top-level `workspace_state` object (`version`, bounded
`refs`, `available`, optional static `reason`). It is separate from
`research_state` and `review_state`; refs are host-computed from files actually
written under `notes/` and `skills/`, never from model-supplied hashes. The field
is omitted when the workspace policy is absent, so old seeds remain
byte-identical and old readers accept them. New seeds with the field are
forward-only: an older binary rejects the unknown version 2 field, so restore
the original seed or disable the workspace policy before rollback. Existing
files and journals are never rewritten.

Operator notes live in `model/session/<date>/notes.json`, outside the compactable
log. States are waiting, pinned, done, superseded, cleared (and expired at close
for non-keep notes). A turn records the note-set version it was built from; any
add, clear or keep edit during the turn refuses that commit (`stale_notes`) and
the next turn sees the new set. The orchestrator may mark only the note revision
it saw (`notes_done`/`notes_superseded`), so an edit is never clobbered. Each note records its last actor (`operator`,
`orchestrator` or `host`), and a note set that keeps changing mid-turn re-runs
the turn a bounded three times before it is dropped with a
`session_note_rerun_cap` alert. Keep
notes are copied into tomorrow's `seed.json` by the host before non-keep notes
expire; that seed does not depend on the handoff model reply. Compaction asks for
a summary after 12 turns since the previous summary by default; the experiment
profile's `supervisor.compaction` can lower `max_turns` (1–256) and add a
measured `max_context_bytes` ceiling (4 KiB–8 MiB) with `reserve_bytes` (default
16 KiB). The measured context is the serialized summary and completed
turn/result exchanges after that summary, including replies and action receipt
references; pending turns are excluded until their result is durable. This is a
byte proxy, not a provider token count. The host adds this versioned history to
each actual model prompt independently of trigger-template placeholders, along
with current host facts. Reused old requests have their embedded
`session_context`, `recent_turns`, `summary` and `seed` fields stripped to avoid
recursive growth. Resume refuses a pending turn if its recorded prompt version
changed; it does not render that old request under a new prompt.

`supervisor.compaction.max_request_bytes` bounds the full provider request —
the rendered model prompt, standing instructions, tool definitions and request
envelope — plus the existing reserve, default 1 MiB and valid from 4 KiB
through 8 MiB. It must exceed `reserve_bytes`. The host measures the exact
request body at each provider call and retains the envelope beyond the
JSON-escaped rendered prompt (standing instructions, tool definitions and
request fields) in supervisor state, so later renders bound the whole request
rather than only the prompt; before the first measurement the bound covers the
JSON-escaped prompt only. Crossing this bound also triggers earlier compaction.
A request that still exceeds the bound is refused as `supervisor_context_limit`
before a model call; the original log and host protection state remain intact.
An oversized compaction preflight is recorded as `compaction_failed`, without
appending a summary. Other failed compactions keep the last valid log and record
their bounded reason until a later compaction succeeds. The log is never
rewritten. Chat/aside instances are built with write-capable tools removed by
the host.

A provider-incomplete response (`model_response_incomplete`, HTTP 200) is
context exhaustion when the measured request is at or above
`supervisor.compaction.near_cap_percent` (default 90, range 50–100) of
`max_request_bytes`. The host then compacts with the measured full-request
envelope and retries once in the same cycle (`context_retry_limit`, default 1,
range 1–3). If that retry fails, it records the bounded reason and backs off for
`context_backoff_ms` (default 15 minutes, range 1–60 minutes) instead of
repeating the same oversized request every cycle; one fresh bounded attempt is
allowed after the backoff. A `content_filter` incomplete cause and an
incomplete response on a normal-size request keep the ordinary failure path.
The provider-reported `incomplete_details.reason` (allowlisted as
`max_output_tokens`, `content_filter` or `other`) and the measured
request/prompt/token sizes are retained in `failure.json`, the planner attempt
evidence, `supervisor-failure.json` and the run transcript end event; provider
text is never persisted.

New turns store only a `resume_binding_sha256`, derived from resolved config,
effective role/profile and prompt hashes. Resume refuses changed bindings as
`supervisor_config_revision_changed` and changed prompt versions as
`supervisor_prompt_revision_changed`. Older pending turns without a binding
retain the existing model-selection and prompt-version guards and their original
request rendering; they are not rejected solely for lacking the new hash.

`full_universe_features:true` remains required only for projection mode, where it
includes a compact column-labelled matrix. `shortlist_n` is navigation/ranking,
not the allowlist. Warmup requires 21 daily closes ending at the
configured previous session. Features include 1/5/20-day returns, volatility,
volume z-score and the last archived opening gap. Coverage/as-of dates stay
explicit; daily bars never become live quotes.

## Tool boundary

Pi lane tools are published from the existing Rust executors in
`runtime/agent/tools.rs`; the pi-facing name is given first.

- `evidence({"evidence_id":...})` (Rust `read_evidence`): fixed
  bundle/portfolio/shortlist/fees/session_evidence sections, no paths/URLs/query
  language. Bundle and shortlist reads expose compact shortlist rows plus original
  count/hash and explicit detail/row omissions. Fetch full feature details by
  instrument key.
- `snapshot({"instrument_keys":[...]})` and `features({"instrument_keys":[...]})`:
  up to 20 unique allowlisted keys, including names outside the shortlist.
  Duplicate keys are folded in first-seen order; empty and over-max lists stay
  invalid. Provider strict schemas omit `uniqueItems`. Quotes use the
  application's own data credential and original shared quota; feature reads
  preserve dates/gaps.
- `prospects({"limit":...,"sort":...})`: call-time scanner rows with sampled and
  market-relative moves, bid/ask depth, ages and trigger reason.
- `rank({"sort_by":...,"order":...,"limit":...,"min_liquidity":...})`: compact rows
  over the full validated allowlist with explicit missing/stale history. The
  requested row limit also bounds the missing-history diagnostic sample. Exact
  `unavailable_count` and `unavailable_reasons` cover the full universe;
  `unavailable_truncated` and `rows_truncated` identify omitted rows. The response
  remains within the existing 64,000-byte tool limit, including when a frozen
  universe has thousands of missing histories. A truncated list is not complete
  evidence; request features for specific keys when needed.
- `portfolio`, `decisions` and `session`: current cash/holdings/exit intents,
  bounded prior decision memory and clock/phase/budget/trigger facts.
- `plan`: decision lane only. Submits this turn's paper TradePlan; the host
  validates identity, allowlist, cash, fees, sizes and exits and returns per-action
  verdicts. It never places an order, and a validated plan is a proposal.
- `research`, `research_status`, `research_result` and `research_cancel`: the
  durable research request queue. Only the decision lane may queue a research
  request; a research turn may not queue research for itself.
- `analysis`, `run`, `backtest`/`backtest_status`/`backtest_result`/
  `backtest_cancel`, `quotes`, `list`/`read`/`write`/`install`: registered
  analysis, the bounded workspace sandbox and retained research evidence. All
  results are advisory and never place, size or promote a paper trade.
- `search`, `fetch` and `news`: opt-in host tools configured under
  `planner.web_tools`, never compiled in. `search` calls the reviewed provider
  endpoint (`searxng` or `brave`), `fetch` reads one public HTTPS URL as bounded
  text, and `news` reads the reviewed free RSS/Atom announcement feeds. Private,
  loopback, credential-bearing and redirecting URLs are refused; no local shell,
  browser, broker credential or order path is reachable. Calls are bounded by the
  configured per-run budget, minimum spacing, timeout and byte caps, and each call
  is appended to the session's `model/web-tools.ndjson` record with its query or
  URL, latency, bytes and outcome. Results are untrusted public evidence, never
  official filings, quotes, fills or execution authority.

Strict argument schemas, unique call IDs and whole-batch validation apply before
any local dispatch.

An oversized result becomes an explicit `unavailable_no_retry` tool failure with
original byte count/hash and the existing output limit. It contributes no
successful observation. Read requests may be narrowed using a new call within the
same turn, read and allowance bounds; effectful calls must not be replayed.
Original full input/quote artifacts and existing failed records remain preserved.

Every lane turn's tool calls and results are archived under
`model/agent/turns/<turn-id>/`, and a failed tool call keeps its bounded reason.
Per-tool bounds are `planner.tools.scanner_limit` (40), `ranking_limit` (50),
`history_limit` (5) and `max_instrument_keys` (20); defaults are omitted from
policy serialization. No tool grants orders, credential access, service control
or strategy promotion.

Archived digest results and earlier owner plans remain attempt evidence; only a
plan activated through the current plan-head authority is executable.

## Opt-in intraday decision evidence

The planner's optional `decision_evidence` object enables the read-only
`intraday` (`get_intraday_context`) tool for lane turns with
`{"instrument_keys":[...]}`. The
complete defaults are in the
[configuration fragment](../packaging/rust/decision-evidence.fragment.json).
Omitting the object keeps the tool definitions and policy bytes unchanged. The
settings enter the reviewed policy; installing the binary alone does not enable
this change on an existing book.

The controller freezes context from its existing scanner and minute price store
when it builds a lane brief. It makes no additional provider
request, read-journal reservation or financial write. It supports any validated
allowlisted key, including names outside the ranked shortlist, under the same
configured key/read/turn/output limits as other local reads. Tool results are
archived with their original as-of, last price receipt, scanner source and
metadata receipt. Metadata's independent provider source clock is unavailable
and explicitly null. A standalone diagnostic/replay backend without controller
context returns an explicit unavailable result rather than fetching substitute
data or fabricating bars.

The result includes a bounded tail of completed receipt-derived price OHLC bars
and tick counts; no missing minute is interpolated and tick counts are not traded
volume. Returns for configured 1/5/15-minute windows require consecutive completed
minute closes with identical endpoints. Broad-market medians require the configured
minimum number of valid, non-stale contributors; relative return is absent when
the instrument window or benchmark is missing. These are observed screening
comparisons, not executable performance or exchange-certified candles.

Optional scanner fields include VWAP, cumulative volume, the prepared average
daily volume and its session count, relative-volume proxy, depth quantities and
spread/imbalance when actually available. Metadata freshness is checked separately;
stale optional fields are withheld. Proxy parameters are returned explicitly.
`features.move_bps`/`market_relative_bps` use irregular retained samples; only the
`returns` array denotes aligned fixed-minute windows. Context is frozen at brief
build and never presented as a new executable quote; `get_market_snapshot`
and the unchanged two-second execution gate retain that responsibility.

The tool gives the lane facts only. The decision lane still chooses actions,
sizes and holding periods; there is no trade quota, required catalyst, fabricated
probability, lower fee assumption or new trading risk cap. An empty action list
remains valid.

### Plan binding and execution refusal

A pi paper plan records the revision the decision was produced against:
`portfolio_revision` (cash, positions, fills and exit intents; marks, health and
high water are excluded), `policy_sha256`, optional `profile_sha256`, the decision
turn ID, provider/model and `input_hash`. The claim also binds the trigger that
woke the lane and the instrument allowlist revision it used. Execution revalidates
these before the first action: a changed portfolio records `portfolio_changed`, a
changed or unresolvable policy `policy_changed` and a changed profile
`profile_changed`; a changed allowlist refuses the plan. None performs a fill. The
plan remains archived and a new decision supersedes it. Archived plans without
these optional bindings remain readable, and runtime admission requires the
current pi plan-head authority.

## What model observations establish

The `features` tool (Rust `get_instrument_features`) reads frozen dated historical
features and matching scanner prospects; it is not a general historical-bar query. A default scanner
prospect includes irregular sample movement and retained top bid/ask/age fields.
Optional VWAP/relative-volume/strategy features existing elsewhere in the runtime
must not be assumed present in this tool. The opt-in context tool above exposes receipt-based minute price windows;
its tick counts are not traded volume. Inspect actual returned fields and their
coverage.

The `snapshot` tool (Rust `get_market_snapshot`) performs a new application-owned
quote read, validates identity and the original source/receipt freshness, and
archives the original receipt. It grants no execution authority; model reasoning time does not extend
freshness. Screening LTPC activity does not replace executable depth. Native
Web observations require dates, source attribution and uncertainty; they are not
automatically official filings or independent price verification. Registered
analysis availability depends on actual pinned registrations and configured
roles; a configured catalog does not establish that a model used it. A missing or
unreadable registered file is reported as `research_code_unreadable`,
`research_runtime_unreadable` or `research_input_unreadable`, and preflight and
readiness check the pinned bytes before a research call.

When `planner.workspace` is present and `enabled`, decision lane turns also
receive bounded workspace tools: `workspace_list`, `workspace_read`,
`workspace_write`, `workspace_run` and `workspace_install`. The host seeds
`<book>/workspace` (or the configured relative root) with `README.md`, the
`cost_aware_momentum` scanner script and its params, plus empty `notes/`,
`skills/`, `tools/`, `data/`, `reports/` and `journal/` directories. Existing
files are not overwritten. Writes are limited to `notes/`, `skills/`,
`reports/`, `data/` and `journal/`; path escapes, symlinks and the trading book
are refused. `workspace_run` executes only `run_allowlist` scripts through the
resource-limited scorer sandbox and returns advisory scores, never orders. The
sandbox network stays disabled by default; the reviewed `workspace.network`
setting is an explicit opt-in for scanner scripts, mounts only read-only host
resolver/CA material and never exposes the host root, credentials or the trading
book. The post-close scorer sandbox refuses network unconditionally.
`nivra experiment-lifecycle` can ingest a stored advisory report into the
A4 lifecycle; scores still do not become orders.
`workspace_run.input` is a nullable JSON-encoded object string (validated by the
host); `input_path` is a nullable workspace path. Supply at most one, or nulls
for the sibling params file. Legacy archived object inputs remain readable.
Lane workspace/research schemas share one call-time builder with provider
preflight; harness preflight failure codes remain in failure evidence.

`workspace_install` writes under `tools/` only when the reviewed allowlist
matches both the file name and the content SHA-256; an empty allowlist refuses
every install. Omitting `planner.workspace` leaves sealed policy hashes
unchanged and exposes no workspace tools. Enabling or changing the block
requires reviewed `config-apply`.

`planner.web_tools` is a separate optional block; omitting it exposes no web
tools and makes no outbound call. An enabled block requires an explicit provider
and endpoint (or free news feeds), and a paid provider additionally requires an
absolute application-owned key file read only at call time. The key never enters
a result, transcript, session record or sandbox environment. Web tools are
advertised only on decision and research lane turns, and the dashboard's Models view lists
the recorded calls. Enabling or changing the block requires reviewed
`config-apply`.

Registered research revisions admitted from a lane turn also bind an immutable
candidate in
the session's existing `model/experiment-trials.sqlite`. Task summaries expose
`experiment_candidate_id`; later revisions link their predecessor without
replacing it. The original scoped task binds the hypothesis, analysis/input/runtime
hashes, policy, authoring turn, session and resource limits. Admission and reconciliation
repair an interrupted ledger write from the original queue, without rerunning work.
Terminal outcomes retain their original timestamp and result hash; analysis success
requires verified worker receipts. Candidate/outcome identity changes are refused.
Strategy ABI, parameters, training window and holding horizon stay explicitly
unknown: a registered analysis does not supply those strategy specifications.
Research admission records the authoring turn identity, policy and resource
limits in the immutable task. An order plan does not exist at enqueue time and is
never fabricated or used as research approval.

Lane research enqueue requires explicit `research_tools.admission_limits` (see
`packaging/rust/research-admission.example.json`). `max_trials_per_input` counts
all admitted versions for identical code/runtime/input bytes in the same book and
session, across aliases, task keys, goals, turns, failures and cancellations.
`max_session_trials` counts all admitted compute trials in that book/session.
These are research compute limits, not paper sizing or trading risk caps. Checks,
deferral receipts and enqueue share an immediate queue transaction. Idempotent
reads of an existing revision spend nothing. An exhausted input returns
`research_unchanged_input_budget_exhausted` and requires new registered content or
reviewed compute policy; a session exhaustion returns
`research_session_compute_budget_exhausted` and requires the next authorized
session. Neither restarts a job. Existing immutable tasks remain readable without
new evidence fields. A missing compute policy returns `research_compute_policy_required`
without creating a task; the tool transcript retains that reason.

### Candidate compute supervision

The optional `research_tools.candidate_compute` block admits score-only source
through the existing owned systemd user unit/cgroup supervisor. The controller
captures the host machine identity before claiming the shared queue slot; an
unavailable identity refuses the still-Pending task with
`machine_identity_unavailable` and no launch receipt. It then writes an immutable
`candidate_launch` receipt and starts exactly one
`nivra-candidate-<lease>.service` unit. The unit runs the fixed,
content-addressed `nivra candidate-supervise` helper with a minimal
environment; the helper re-verifies the staged source/params/dataset, helper and
interpreter/bwrap hashes, and executes the existing no-network
`nivra_sandbox::score_source` bwrap path. The controller never starts a process
group fallback and never signals a raw PID or PGID.

The only release proof is a typed `CandidateUnitProof` read from the original
unit's `systemctl --user show` state and the kernel-tracked
`cgroup.events populated 0` value after `systemctl --user stop`. A truthful
absent-cgroup variant is accepted when systemd has unloaded the unit; a
non-empty control group with a missing or unreadable events file is ambiguous
and retains the lease. A loaded control group must carry the complete actual
bound property surface (`MemoryMax`, `MemorySwapMax`, `CPUQuotaPerSecUSec`,
`TasksMax`, `RuntimeMaxUSec`, `LimitNOFILE`, `KillMode`, `NoNewPrivileges`,
`TimeoutStopUSec`, `Restart=no`); a fast unloaded unit may retain an empty
observation without inventing values. A fast normal exit and a wall-timeout exit
both require the same owned-unit proof. `CandidateStopped` version 2 records the
proof, a typed `cause` (`succeeded`, `operator_cancel`, `deadline_expired` or
`failure`), a closed `outcome`, the helper's actual `result_produced_at_ms` and
the controller's separate `stopped_at_ms`; the result envelope binds the proof
hash. Recorded reads use the recorded policy, helper and executable hashes,
never the current host bytes, so a later upgrade cannot invalidate an authentic
result. Execution-aware content/input identities additionally bind the recorded
runtime binding and the actual Python/bwrap content hashes and every sandbox
cap. A Python/bwrap or containment-cap change under the same helper is therefore
a new execution version and a new input allowance, while an old result remains
authentic under its recorded runtime. Ranking-only task bytes are unchanged.

An operator cancel is a distinct cause from task TTL or unit wall expiry. The
in-process signal carries operator intent only; the driver derives expiry from
the immutable launch deadline, so an expiry is never labelled a human
cancellation. An authentic helper result produced after the recorded compute
deadline is classified as `deadline_expired`/`result_after_deadline` with the
bounded code `candidate_result_after_deadline`; a late payload with a mismatched
identity, hash or proof remains `invalid_result`, never trusted late output. A
genuine operator cancel recorded before the completion transaction always wins
and the result artifact is preserved. A timely result
with a durable stop proof may still reconcile after a later deadline/failure
termination; a later expiry never erases authentic finished work. The result cap
recorded in the task's unit policy is enforced on the helper output, the accepted
envelope and the stored artifact; an oversized payload leaves a bounded failure
code, hash and byte count instead of a false success. The helper claims a
one-run marker with `O_EXCL` and fsync before reading untrusted source, so a
unit restart or manual start refuses rather than replaying. Staged inputs and
the pinned helper are opened through one `O_NOFOLLOW` fd and checked for
regular-file type, owner, mode and byte bounds; the private 0700 stage and
0500 content-addressed helper cache remain the containment boundary. A
same-UID replacement between the helper's byte check and bwrap's exec is a
residual path-to-exec risk; the code does not claim a generic memfd runner or
an OS guarantee beyond that.

Crash recovery (`reconcile_candidate_attempt`) never relaunches source. A durable
valid stop plus result completes idempotently only after the exact stored
payload is re-verified against the recorded task, lease, runtime, policy and
proof; for an executable task the stored execution spec/manifest/events are also
re-verified against the recorded report, so a tampered execution artifact cannot
become a `Succeeded` row that can never verify. Corrupt, missing or mismatched
bytes become a trusted terminal failure with the artifact preserved. A durable stop without a result is a trusted
terminal failure or cancellation, and its recorded cause/outcome is honored
instead of the mutable job row. A live attempt without a stop proof is stopped
only through its original unit and then proven absent. Ambiguous scope retains
the lease and reports `candidate_recovery_required`. Candidate results remain
`ranking_diagnostic` partial current-session evidence with no P&L, cash, fees,
edge, promotion or activation claim; no executable adapter or cross-session
parent is implemented here. A lifecycle conclusion that was committed but not
transitioned is retried mechanically from the original ledger, including an
`evaluated` half-state, without replaying the model or the candidate.

### Optional after-cost cash replay

`research_tools.candidate_compute.execution` is an optional, default-absent
capability block. When it is omitted or `enabled: false`, every existing
candidate policy byte, registered specification, candidate task and tool schema
remains unchanged and no execution argument is advertised. Enabling it is a
reviewed host policy decision, not a per-candidate human approval; the model
still declares the research sizing, holding period, capital and costs.

When enabled, `candidate_submit` advertises one additional nullable JSON-string
argument, `execution`. The strict provider schema requires it (as a nullable
property) whenever it is advertised. The JSON object is a
`CandidateExecutionSpecV1`:

```json
{
  "version": 1,
  "segment": "cash",
  "parameters": {
    "version": 1,
    "parent_candidate_id": null,
    "hypothesis": "same text as the submit hypothesis",
    "training_from_ms": 0,
    "training_to_ms": 0,
    "selection_from_ms": 0,
    "selection_to_ms": 0,
    "holding_ms": 60000,
    "max_exit_delay_ms": 60000,
    "capital": "100000",
    "costs": {
      "per_order": "1",
      "buy_bps": "0",
      "sell_bps": "0",
      "slippage_bps": "0",
      "rounding_increment": "0.01"
    }
  },
  "units": 10,
  "max_entries": 1,
  "selection_top_n": 1,
  "entry_price_offset_ticks": 0,
  "entry_at": "signal",
  "exit": "time",
  "first_signal_per_key": true,
  "cost_stress": []
}
```

The declared windows must exactly match the submit windows, `holding_ms` must
equal `horizon_minutes * 60000`, `max_exit_delay_ms` is `60000`, capital must be
positive and no larger than the reviewed `max_capital`, `max_entries` no larger
than the reviewed cap, and `selection_top_n` no larger than the scorer's actual
ranking width. `cost_stress` is capped at four entries and changes only the fee
arithmetic. `segment` must be `cash`, `entry_at` must be `signal` and `exit`
must be `time`; NFO/index keys are refused explicitly rather than synthesized.

At submit the controller freezes the full executable quote tape from the same
read-only original-receipt reader as the scorer dataset. Only receipts whose
original source observation is at or before the submission clock enter the
tape; a later exit quote is never fetched, so an incomplete exit is honestly
unknown. The tape requires verified instrument/tick/session identity, circuits,
depth, volume and source/receipt clocks. Screen-only, OHLC-only or compacted
evidence cannot fill: submission returns `candidate_execution_quotes_unavailable`
and creates no task row. The scorer dataset and the execution tape are separate
stored artifacts; the Python score sandbox receives only source, params and
frozen feature snapshots, never the execution manifest/events, so a malicious
score source cannot read exit or future quotes. The contract exercises this with
a source that probes the actual bwrap mount table, open file descriptors,
cwd/root and a bounded traversal of the real stage root for an execution-only
sentinel; an unexpected mount point or a sentinel hit suppresses the score map
and fails the contract. The helper itself reads only the declared execution
policy before untrusted scoring and reads the full frozen tape only after the
scorer replay completes. The tape's manifest/events are hashed over the exact
bytes and the canonical execution spec is content addressed.

The fixed helper runs the existing scorer replay first, then the trusted Rust
adapter over the staged execution artifacts. The adapter derives scored signals
only from selection-window ticks, folds duplicate horizon ticks for one
snapshot, never uses training entries and refuses a requested top-N that does
not fit the actual ranking output. It then calls the existing
`strategy_actions::evaluate` cash engine exactly once for the base costs and
once per declared stress cost with only `Parameters.costs` changed. There is no
second fill loop, fee engine or portfolio journal writer. The same owned unit,
resource caps, deadline, one-run marker, cancellation and typed proof cover the
base and all stress work; `max_result_bytes` remains authoritative for the
combined output. An oversized result is a bounded terminal failure with its
hash and byte count, never an unverified success.

The native result kind is `executable_replay`, distinct from the ranking-only
`ranking_diagnostic`. The pure verifier binds the outer engine/adapter hashes to
the embedded shared report, checks unavailable candidate identity/time/
truncation/rejections, recomputes expected rejections when the manifest is
available, and enforces the same P&L/equity nullability as the shared engine:
residual positions or missing marks can never be turned into a fabricated flat
profit or marked equity. Raw executable observations and unique first captures
are labelled separately so identical republications are not counted as
independent samples. The adapter report is structurally either `executed`
(measured cash/fees/turnover/fills/PnL/equity, with `net_pnl_if_flat` and
`marked_equity` null when a residual position or missing mark leaves accounting
incomplete) or `execution_unavailable` (no summary, no report, no fake zero, no
engine claim). Every replay is retrospective: `retrospective: true`,
`prospective`/`prospective_authoring`/`no_lookahead_qualified`/
`holdout_qualified`/`edge_qualified`/`promotion_authorized`/
`activation_authorized` stay false and `scientific_state` is
`insufficient_evidence`. The lifecycle records the full wrapper and reaches
`replayed`; it never opens a prospective forward shadow, marks eligible or
promotes from an execution replay. Cash and costs are declared research
assumptions, not account funds, broker fills or qualified alpha.

Rollback is configuration-only: remove the `execution` block or set
`enabled: false`. Existing execution tasks and verified retained results stay
readable from their recorded task, launch, proof and artifact hashes; no queue
reset, artifact deletion or schema migration is required. Continuous-forward,
cross-session, model-research-turn and clock-repair work is not implemented by
this capability.

Owner-research-task candidates remain `insufficient_evidence` with no edge
qualification until a strategy specification, replay and forward evidence are
recorded separately. `nivra experiment-lifecycle` persists that path, including
ingestion of workspace scanner reports
Eligible is not an edge and does not authorize activation.

[GitHub Issues](https://github.com/TharunChougoni/nivra/issues) tracks
qualification and evidence-tool work. Successful tools establish software
operation, not profitable decisions or complete/independently certified data.

## Budgets and failure semantics

Each configured pi lane needs positive `daily_calls` and `daily_cost_usd`,
`turn_timeout_ms` in 1,000–3,600,000 ms and `max_tool_calls` in 1–200; an invalid
lane refuses at planner load. Pi reports usage per turn; Rust charges it to that
lane's daily allowance and journals the turn. A turn cut off by a crash is charged
once from its in-flight checkpoint. A lane allowance is operational, never a
position, loss, exposure, holding-period or trading-risk cap.

The lane service dispatches at most one turn at a time and merges triggers that
arrive while it runs. A turn beyond its wall limit is aborted; provider, schema or
timeout failures mean no new trade for that trigger and never halt the book.
Missing history or absence of eligible evidence produces no model call. Usage
corruption, identity mismatch or failed durable writes are separate integrity
failures. Protection and fills stay deterministic while a lane runs.

The retained legacy planner's `usage_namespace`, reservation-journal capacity
bounds and per-role `daily_call_budget` remain archive evidence. Original inode
pins and consumed charges are never reset, refunded or rewritten.

### Historical: owner-turn clock and failure backoff

The earlier supervisor's model-visible observation time was the actual wall clock
sampled when the turn was admitted or observed; it was never
`turn start + elapsed`. A separate monotonic `Instant` measured duration budgets,
deadline admission and transcript `duration_ms`, so a frozen or stepped-back wall
could not freeze a deadline. A wall backstep or negative monotonic reading refused
the turn with `planner_clock_anomaly`; the two-second quote-receipt gate was
unchanged. `orchestrator.triggers.supervisor.failure_backoff_ms` deferred new
owner turns after configured supervisor/decision failures under the same
model-selection profile.

## Historical: post-close scorer trigger

In the earlier planner, when `orchestrator.triggers.scorer.enabled` was true and
`orchestrator.triggers.scorer.after_minute` (or the top-level
`scorer_after_minute`) is set, the runtime may run one scorer admission after the
session is closed and `report.json` exists. The due minute must be in
`931..=1439` IST. The trigger is disabled unless the planner and orchestrator are
enabled, the scorer role is enabled, the daily allowance remains and its prompt
loads. A disabled, missing or unset trigger records a bounded skip reason; it
does not invent a default time.

Admission is durable in the book's `scorer_control` row. A restart preserves an
in-flight reservation as `interrupted` with `process_restart_no_replay`; it never
reruns or silently skips. The worker may write only workspace-relative paths
through the policy-gated orchestrator. It reads the session's own
`paper.sqlite`/`wide-universe.json` feature snapshots with the same export path
as `strategy-lab export-features`; no manifest, journal or runtime state is
written by that read.

A snapshot extraction failure ends as `unavailable` with the bounded specific
code (for example `strategy_no_screening_evidence` or
`scorer_snapshots_receipt_binding`) and a non-secret `detail` object: the
queried window size, scanner/metadata/protection/unknown counts, oldest/newest
times and the replay bounds. This distinguishes "no scan ran" from a malformed
receipt or a window containing only protection polls; a generic
`scorer_snapshots_unavailable` is not a dead end. The extractor builds feature
screens from full `purpose:"scanner"` receipts and from the streamed session's
`scanner_metadata` polls, whose body is the same full quote payload.
`execution_or_protection` polls remain protection-only evidence and are not
promoted to screening. Original rows and the reservation are preserved.

The model writes a Python scorer and parameters, then replays them through the
sandbox over those snapshots. The report records per-snapshot status/failure
codes, top-N overlap, Kendall/Spearman rank metrics, forward returns at the
configured horizons and cost-adjusted marks using the paper-cost policy. An
unqualified report is fed back, so the model can fix a broken script; turns and
replays are bounded by the scorer role's `max_turns` (10) and
`orchestrator.triggers.scorer.replay.max_replays` (3). All new numbers live in
`orchestrator.triggers.scorer.replay` (`max_snapshots`, `min_valid_snapshots`,
`horizons_minutes`, `top_n`, `max_replays`, `max_age_ms`, `lookback_samples`,
`max_feedback_chars`, `notional`) with validated defaults. The scorer role's
model calls are archived through the harness as `harness-scorer-*` runs.

Publication requires `status=ok`, at least `min_valid_snapshots` valid
snapshots and zero failed snapshots. A qualified replay copies only the written
scorer/parameter paths to the configured `scorer_path` and `params_path`, and
writes `<scorer>.qualification.json` with the report, dataset, scorer and params
hashes. An unqualified or broken script is never published. Publication is
diagnostic; it does not promote a strategy or create an order. Status is exposed
as `scorer_trigger status=<...> reason=<...>` and in the dashboard
`scorer_trigger` projection.

## Identity, archives and reviews

A pi lane's provider credentials belong to the application's own pi directory
(`auth.json`, `models.json`) and are read only by the agent process. Rust starts it
with an explicit environment allowlist and never reads those files. The retained
legacy planner's per-role `model_config` grant bindings remain archive evidence,
not a live credential source.

The original book owns events, attempts, plans and fills. A pi paper decision
writes `model/agent/triggers.ndjson`, `model/agent/turns.ndjson`, the per-turn
archive under `model/agent/turns/<turn-id>/`, the plan-outcome journal
`model/agent/plans.ndjson`, lane sessions under `model/agent/pi/<lane>/` and the
daily usage ledger `model/agent/usage.json`. A sealed review writes
`model/agent/memory.json` and claims its single attempt with
`review-claimed.json`. Archived legacy digest/owner artifacts under
`model/<decision-id>/` and `model/usage/` stay readable and are never rewritten.
SQLite per-action rows commit with paper fills; JSON status/execution projections
cannot revive a plan.

The planner config has no `post_close` block and no `operations` or `strategy`
entry under `orchestrator.triggers`; a config that still carries one is refused at
load. The earlier owner review and research artifacts (`model/reviews/`,
`model/research/`, `ReviewCompletedV1` bytes) remain frozen archive evidence.

The protected model stream exposes redacted output/tool progress with gaps,
truncation and host-validation labels. Hidden reasoning is excluded; displayed
text is never interpreted as HTML. Opening the dashboard invokes no model.

## Diagnostics

| Command | Scope |
| --- | --- |
| `planner config-check --planner FILE --session DATE` | Offline dated-policy validation and resolved pi lanes; no credentials or calls |
| `planner profile-check --planner FILE --profile FILE` | Offline experiment-profile validation; no calls |
| `agent-check --config AGENT_SETTINGS --fixture FIXTURE` | Offline decision → research → decision round trip through the real pi subprocess and its scripted fake provider; no book, network or trading |
| `agent-turn --config PAPER_CONFIG --lane LANE [--dry]` | `--dry` prints the brief and published tools without starting pi; otherwise one shadow lane turn charged to the lane allowance and never executed |
| `agent-shadow-report --config PAPER_CONFIG` | Read-only summary of recorded pi shadow decisions and the sealed paper session |
| `readiness --config PAPER_CONFIG` | Read-only local pi runtime, lane-budget and history projection for the next session |

Use `--help` for required options. Diagnostic namespaces cannot replenish an
original book's allowance. Operational qualification is tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues);
[transport](XAI_SUBSCRIPTION_TRANSPORT.md) covers the earlier planner's grant
records.

Archived digest citation diagnostics (`digest_citation_warning` and a retained
`rejected-digest.json`) remain readable for earlier runs; no live lane turn
produces a digest.

The authenticated dashboard’s Model run history reads dated session harnesses and
planner audit files directly, including failed and interrupted runs. It presents
20 runs per page, newest first, with older cards collapsed. Requests are never
served wholesale: only tool arguments, bounded result summaries and assistant
output are projected. Symlink archives are excluded. New harnesses additionally
retain `run.json`, `output.json` or `failure.json` for run identity, trigger,
timing and terminal outcome. Old runs show unavailable metadata explicitly;
legacy durations use retained archive write times where possible. This view does
not start models or change any book, journal or sealed evidence.

A provider failure that happens before any provider turn (for example a
`catalog`, `grant_owner`, `application_config` or `tool_definitions` failure) is
archived as a bounded failure record, not as a full request per retry. The
record keeps the reason, stage, HTTP status when present, request and prompt
sizes, and the request, tool and instruction hashes; the run keeps its
`transcript.ndjson`, `run.json` and `failure.json`, so it stays visible in the
run list with role, model, start, outcome and reason. The first failure of a
failure window (same archive kind/role/model/reason/stage, one hour) also
retains one representative full request and later retries link to it; a provider
turn closes the window so a new outage keeps its own first evidence. The
transcript input event links the representative request or the bounded
`turn-NN-request-receipt.json`. The planner audit directory follows the same
rule with its own window: the full `input.json` is the representative, later
retries keep `input-receipt.json` with the input hash, size and stage, and the
audit `failure.json` links the representative. No original journal, book or
failed artifact is deleted.

## Strategy laboratory

The opt-in [strategy sandbox](STRATEGY_SANDBOX.md) documents spec-driven shadow
rankings, scores-only Python/WASM execution, recorded replay and typed read-only
pipeline tools. Its proposals and approval records never activate a session.

Archived digest failures stay diagnosable from their retained records
(`harness_digest_json_invalid`, `harness_digest_schema_invalid`,
`harness_digest_bounds_invalid`, duplicate changed prospects, or a bounded
credential-redacted `rejected-digest.json`); no live lane turn produces a digest.

## Retained legacy admission

The earlier planner's owner prerequisites still validate a retained legacy planner
used for offline replay, and original usage journals keep their `decide-`
reservation names for archive validation. Installing a build migrates no book and
starts no session, and no live dashboard identity is Digest or Orchestrator: the
configured model identities are the pi lanes.

### Historical: configured model admission

The inline `digest` role block is its named profile. The orchestrator `model`
block is `orchestrator.default`; `model_profiles` plus `admitted_models` define
operator-selectable named profiles. Role admission uses configured bindings rather than compiled Luna/Grok
model-ID pairs. Real backend calls match role, exact model ID, transport,
application credential path and curated-model flag before reserving usage. The
owner must match the currently active selection; a pending switch is not admitted
until the existing idle-turn promotion boundary.

OpenAI Codex transport profiles cannot carry the xAI native `web_search` or
`x_search` tool flags. Incompatible switches refuse before changing the active
selection; tools are never silently substituted. An admitted profile does not
prove entitlement: the transport still verifies the application's own grant,
exact-model approval, provider admission and exact response model. Invalid or
policy-unadmitted switches retain the last valid selection and durable refusal receipt.
The `LUNA` catalog label remains a display suggestion, not an inference allowlist.

No provider performance benchmark is claimed. Tokens/cost and genuine session
qualification remain unknown.

Registered owner-task versions and terminal outcomes are ingested into the
append-only experiment ledger and exposed read-only in the current-session
Library. Unknown strategy ABI, parameters, regime and holding horizon remain
unknown. A successful analysis is insufficient evidence, and Library ingestion
never qualifies or activates a candidate. There is no automatic promotion from
selector cooldown or fallback; configured scanner selection and its execution
validation are unchanged.

Compaction and handoff are single-request lifecycle turns: their request uses
`orchestrator.model.run_timeout_ms` when set, otherwise `timeout_ms`, bounded by
the remaining invocation time. Decision tool turns retain the per-request
`timeout_ms` bound. The original request deadline also covers response reads;
expiry is `model_timeout`, including generic IO/EOF errors at the deadline.

If the full provider request implied by the retained context exceeds
`compaction.max_request_bytes` minus `reserve_bytes`, compaction/handoff omit
the oldest exchanges first, then shorten retained summary text if necessary.
The fitted turn records `context_fit` omissions and is replayable byte for byte.
Original session logs, current notes, positions, seed and lifecycle bindings
remain intact. Decisions continue to refuse oversized requests; a successful
compaction supplies bounded context for later decisions. Invalid context
versions still refuse. A decision refused with `supervisor_context_limit` starts
that compaction when uncompacted history exists, because the trigger measures
only the committed conversation, not current decision facts. A provider-incomplete
response on a near-cap request starts the same bounded compaction retry, and a
failed retry backs off with an explicit `supervisor_context_exhausted_backoff`
skip instead of looping.

A local harness preflight error records `tool_failure` with its reason, time and
applied configuration hashes. Identical decision contracts skip with
`supervisor_tool_contract_blocked`; a reviewed configuration/profile/prompt or
version change admits the new contract. Lifecycle work and host protection
continue. Timeouts and other provider failures do not trip this local circuit,
and no fallback model is selected. The newest 32 skips are retained. Successful
compaction, aside and EOD handoff turns preserve the decision failure streak;
a successful decision/review clears it.

## Prospective observation (opt-in, default off)

`candidate_submit` accepts an optional `observation` JSON-object string only
when `research_tools.candidate_compute.observation.enabled` is reviewed true.
The host freezes three immutable artifacts at the submitted clock T0:

- `dataset`: feature snapshots observed at or before T0 inside the declared
  observation window;
- `observation_quotes`: one bounded virtual-observation tape of original
  `research_observation` receipts for the snapshot keys;
- `observation_spec`: the new `observation_single_entry_v1` study ABI.

The original candidate must be a verified terminal `Succeeded` authoring task
on the same session/book and the same recorded runtime/helper/unit policy. The
helper runs the existing no-network scorer over the frozen snapshots, samples
the real completion clock after the scorer returns, and only then chooses
`ordered_keys[0]`. A missing or stale (>2 s) quote yields a valid dynamic score
with no entry and explicit gaps; no later quote, OHLC, depth refresh or
backdated fill is used. `authoring_at_ms` is the original candidate admission
time and `target_frozen_at_ms` is the separate observation freeze.

The virtual tape is authority-separated. `research_observation` receipts are
never relabelled `protection` or `screen`. The default paper authority
(`strategy_actions::evaluate`, `candidate_execution::execute`,
`paper_replay::apply_event` and `historical::Runner`) refuses
`EventKind::VirtualObservation`; only
`strategy_actions::evaluate_virtual_observation` accepts it. The same cash,
fee, fill, depth, tick and two-second freshness engine runs; there is no second
fill or PnL loop.

Maturation is lifecycle-bound. A pinned decision is `waiting` before
`decision+holding`; no terminal measurement or lifecycle row exists yet. Inside
`[due, due+max_exit_delay]` a missing eligible quote is `pending_capture`, not a
terminal zero. Only after the deadline, or on a confirmed irrecoverable data
gap (for example a retention-compacted receipt), is the exit an explicit
unknown. The FIRST eligible original exit receipt wins and later captures never
replace it.

A flat virtual book has no real position, so no protection receipt is
guaranteed. The virtual authority therefore accepts an approved
`research_observation` receipt for BOTH the virtual entry and the virtual exit,
and still accepts an original protection receipt as an exit when one exists.
The original receipt's archive SHA, event/available clocks, depth, tick and
purpose are retained; a compacted receipt is a typed `first_receipt_unknown`
gap, never silently replaced by the next readable receipt. After-cost fields
are present only when the episode is executed, flat and accounting-complete;
otherwise they are null with explicit exit status, residual positions, missing
marks and gaps. The measurement scope is `independent_episodes`: each episode
has its own declared research capital, and no episode PnL is summed, reused as
account cash or presented as a portfolio return.

A durable measurement is bound to task id, revision, observation SHA, source
and runtime refs and its canonical `measurement_sha256`. A stale or tampered
measurement is `unavailable`, never valid PnL. The measurement content hash
covers immutable source refs and the recorded measurement timestamp only; the
read `as_of_ms` is outer, unhashed metadata, so two reads of the same durable
measurement have the same hash. A required post-score target must carry a
finite `f64`; NaN/Infinity is refused by typed validation before serialization,
not converted to a quiet null. A legacy protection source's nested `execution` axes are
copied exactly; a ranking-only source reports `not_present` rather than a row
of null axes.

Configuration (all fields optional; omission preserves existing policy bytes):

```json
"research_tools": {
  "candidate_compute": {
    "observation": {
      "enabled": true,
      "abi": "observation_single_entry_v1",
      "max_quotes": 64,
      "max_quote_events": 100,
      "max_observations": 1000,
      "max_tape_bytes": 1048576,
      "max_capital": "10000",
      "max_units": 100,
      "max_holding_ms": 3600000,
      "max_decision_delay_ms": 5000
    }
  }
}
```

The distinct controller-only capture grant is:

```json
"owner_tasks": {
  "research": {
    "enabled": true,
    "observation_capture": {
      "enabled": true,
      "max_keys": 64,
      "max_calls_per_session": 64,
      "min_interval_ms": 60000,
      "timeout_ms": 5000,
      "max_rows": 512
    }
  }
}
```

`observation_capture` is not the archive-only `quote_capture` reader. When
enabled, the controller may use the existing application quote client for the
current verified universe and append one original `wide_quotes` receipt with
purpose `research_observation`; it writes no portfolio, state, plan, order or
new financial book. The grant's `timeout_ms` is applied to the actual HTTP
request, the `max_rows` budget is enforced before commit, and `max_keys` is a
hard key budget (an over-budget verified universe is refused, not silently
truncated). The request start and the ACTUAL post-response completion are both
rechecked against the verified `c.session` trading day and cash-session window
before the insert; a request admitted before the close that completes after it
is refused and writes no tick. The original receipt/source timestamps are kept
unchanged. The universe reader binds the declared session/file date and the
security/broker/suspended hashes; a wrong-session or hash-less universe never
reaches the provider. A stopped/halted book, closed market, missing calendar,
missing universe, exhausted quota or provider refusal is an explicit static
status and never unhalts, restarts or fails over. On a provider error the
projection distinguishes `request_attempted` from `provider_called` (the latter
is null when a pre-network token/key validation may have failed).

Cross-session continuation uses the optional typed `parent_ref` argument. A
read-only `candidate_status`/`candidate_result` response exposes a host-built,
verified, opaque `parent_ref.ref` for an eligible terminal source; the model
echoes that object and never invents hashes. The resolver opens the original
queue read-only, requires a terminal verified `Succeeded` result and every
`carry-over.json`/account/state hop to verify, and compares a stable cohort
identity derived from the SOURCE account/feed/instruments/runtime, not a caller
claim. The parent's original source/params hashes remain provenance; a revised
source/params is a versioned continuation and is admitted as new input debt,
while a prospective observation still reuses the original algorithm exactly.
Running/Pending/Unknown/CancelRequested predecessors are ineligible; their
leases are retained and never launched, reconciled, cancelled or replayed. A
broken, cyclic, future or cross-account chain is an explicit unavailable/deny,
never "no data".

The global trial fold counts distinct original admissions, attempts, terminal
budget-charged failures/cancellations and unknown exposure across every
verified carry-lineage queue and lifecycle ledger. It does not count only
`Succeeded` tasks or only the nearest predecessor, and a new session, source
comment or family label never resets that debt. A partial/unavailable read
reports `partial`/`unavailable` with null or lower-bound counts, never a fake
zero. `family_key` is a label only.
