# Registered research worker

Trusted registrations pin analysis/dataset aliases, runtime/code/input hashes and
resource limits. Models can select aliases; they cannot register code, supply
shell/paths/limits, access credentials or promote a strategy. A measured result
is not scientific acceptance.

`research-enqueue` queues only; the separate non-root `nivra-research-worker` claims
jobs. The model harness can also run an already registered analysis through the
same original queue/runner. `research-accept` evaluates retained evidence, not trade
activation. Use command `--help` for explicit arguments.

## Decision-owner tasks

With the reviewed `decision_owner` policy and registered research configured,
actual owner decision turns have four scoped tools:

| Tool | Arguments | Result |
| --- | --- | --- |
| `research_task_enqueue` | `task_key`, `goal`, `analysis_id`, `dataset_id` | A durable task/revision handle; acknowledgement is not execution. |
| `research_task_status` | `task_id`, `revision` | Recorded task and original job disposition. |
| `research_task_result` | `task_id`, `revision` | Bounded, receipt-verified successful result or its explicit non-success state. |
| `research_task_cancel` | `task_id`, `revision` | Cancellation intent/receipt for that exact revision. |

The host derives identities, scope, model revision, registered input hashes,
resource limits and expiry. The model supplies no paths, shell, credentials,
limits or executable code. Each task revision is immutable; cancellation or
termination never makes a prior revision eligible to run again. Existing legacy
review tools retain their synchronous contract. Lifecycle and aside contexts
do not receive the owner task tools.

Owner enqueue returns without running the computation. The controller drives
registered work separately at lower priority than decision processing. A long
analysis does not occupy the decision model turn or financial lock. Successful,
failed and cancelled terminal work generates a deduplicated event with exact
task/revision/result references and the original terminal clock. This event is
research evidence; it neither installs a scorer nor authorizes a fill.

`research_tools.task_ttl_ms` defaults to six hours and includes queue time. A
launch retains the registered execution `wall_millis` separately. A queued task
whose original registration or scope is no longer valid is refused explicitly.
An interrupted or ambiguous running attempt retains its original lease and
occupied slot; the controller never launches it again automatically. Cancelling
a running job is not terminal until the existing worker contract has a trusted
whole-sandbox termination receipt. Original queue identities, attempts, failed
artifacts and charges remain intact.

This path runs pre-registered computation. It does not give a worker a separate
model context to write new strategy code or establish an edge.

A successor session resolves carried task/revision refs against the original
queue and records a read-only adoption receipt. That receipt does not copy the
queue, create a second authority pin, refuse a predecessor row, or launch a
worker. A task still running under the predecessor keeps its original lease.
A conflicting adoption list is refused rather than replaced. A blocked or
failing task, and a queued operator question, do not stop a later owner
decision; the controller does not restart that blocked task to make room.

## Containment and durability

- Bubblewrap inside a transient systemd user unit; fixed arguments, no shell/network,
  home, credentials, trading files or sockets. Reviewed inputs are read-only;
  `/workspace` is size-capped tmpfs. CPU/RAM/tasks/output/runtime are bounded.
- Missing containment is unavailable, never an ordinary-subprocess fallback.
  Private staging under the hardened service is a service-user mode-0700
  directory; PrivateTmp makes `/tmp` unsuitable for shared staging.
- The original pinned queue records job/lease/attempt before launch. No copied
  queue, automatic execution retry or quota refund. Cancellation/ambiguous launch
  retains ownership until the entire sandbox is proven terminated.
- Import only bounded structured output for the exact job/lease/input hashes, after
  the unit is inactive and cgroup empty. Reject symlinks, oversize/malicious output
  and late results. Preserve failed jobs and artifacts.

## Registered analysis

`nivra-analysis` supports reviewed `shared_core_backtest_v1` inputs (exact manifest
and quote events) and `daily_history_summary_v1` inputs (receipt-bound daily bars).
The latter gives descriptive unadjusted historical statistics, not executable
fills or certified profit. Runtime/engine changes never silently rewrite older
registered fingerprints. Historical preparation is described in [history](HISTORY_DOWNLOAD.md).

Containment behavior is exercised by the test suite. A passing test does not
qualify a host after kernel, sandbox or unit changes, and no containment result
establishes strategy quality. Genuine research and recurring-operation work is
Open work and qualification are tracked in
[GitHub Issues](https://github.com/TharunChougoni/nivra/issues).

## Re-registering a stale runtime

A registration pins all three input files by path and SHA-256. When a release
directory is pruned, a `runtime_file` that pointed at its `nivra-analysis` becomes
unreadable even though the code and dataset files still exist. The harness now
reports that as `research_runtime_unreadable` (separate from
`research_code_unreadable` and `research_input_unreadable`), and preflight and
readiness check every registered file before a post-close review call.

Re-register outside a session, as a reviewed configuration change:

1. Choose a retained release or stable host path containing `nivra-analysis`; do
   not point at a mutable or soon-pruned directory.
2. Compute its SHA-256 and update only the reviewed planner config's
   `research_tools.registrations` entry `runtime_file` and `runtime_sha256`.
   Keep the original config for rollback and use the normal reviewed
   `config-apply`/restart procedure; never edit a sealed book or a running
   session's state.
3. Run `nivra preflight --config PAPER_CONFIG` (must print `preflight_ok`) and
   `nivra readiness --config PAPER_CONFIG` (inspect `research.status: "ready"`).
   Do not start or resume a session merely to test the registration.
4. The current `shared_core_backtest` / `synthetic_no_quotes_fixture` entry is a
   fixture dataset. A successful registered run measures that fixture, not a real
   strategy; genuine-data qualification remains open.
