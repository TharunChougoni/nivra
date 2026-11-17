import { test } from "node:test";
import assert from "node:assert/strict";
import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { JSDOM } from "jsdom";
import { OperationalPanel } from "./Operational";
import type {
  HealthResponse,
  OperationalHealth,
  PortfolioResponse,
  SummaryResponse,
} from "./types";

const now = Date.now();
const envelope = {
  v: 1,
  mode: "paper",
  paper_only: true,
  broker: { data: "fixture-broker" },
  account: "private-fixture",
  session: "2026-10-29",
  as_of_ms: now - 5_000,
};

function summary(extra: Record<string, unknown> = {}): SummaryResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "5000",
    equity: "5000",
    status: "open",
    state: { stopped: false, halted: false, closed: false, health: "ok" },
    research_library: {
      status: "ready",
      items: [],
      truncated: false,
      activation_authorized: false,
    },
    ...extra,
  } as unknown as SummaryResponse;
}

function portfolio(
  overrides: Partial<PortfolioResponse> = {},
): PortfolioResponse {
  return {
    ...envelope,
    capital: "5000",
    cash: "5000",
    equity: "5000",
    positions: [],
    fills: [],
    stopped: false,
    halted: false,
    closed: false,
    ...overrides,
  };
}

function health(operational?: OperationalHealth | null): HealthResponse {
  return {
    ...envelope,
    ...(operational ? { operational_health: operational } : {}),
  } as unknown as HealthResponse;
}

function text(node: React.ReactElement) {
  const dom = new JSDOM(renderToStaticMarkup(node));
  return dom.window.document.body.textContent || "";
}

function markup(node: React.ReactElement) {
  return renderToStaticMarkup(node);
}

function researchOperational(
  research: Record<string, unknown>,
  overrides: Record<string, unknown> = {},
): OperationalHealth {
  return {
    version: 1,
    captured_at_ms: now - 5_000,
    supervisor: {
      status: "idle",
      config: "configured",
      config_reason: null,
      config_scope: "loaded_policy_or_static_failure_code",
      state: "present",
      enabled: true,
      parked: null,
      budget: {
        configured_daily_calls: 20,
        charged_calls: 1,
        parked_at_calls: null,
        exhausted: false,
      },
      context: null,
      compaction: null,
      backoff: {
        configured_ms: 30_000,
        retry_limit: 2,
        enforced: true,
        active: false,
        earliest_retry_at_ms: null,
      },
      last_completed_run: null,
      pending_source: "no_pending",
      pending_scope: "checked_bounded_session_log",
      pending: null,
      research,
      ...overrides,
    },
  } as unknown as OperationalHealth;
}

function observation(overrides: Record<string, unknown> = {}) {
  return {
    status: "available",
    source: "original_queue_and_lifecycle_read_only",
    as_of_ms: now - 5_000,
    queue: {
      pending: 2,
      running: 1,
      unknown: 0,
      cancel_requested: 1,
      succeeded: 4,
      total: 8,
      truncated: true,
      oldest_created_ms: now - 90_000,
      oldest_age_ms: 90_000,
      next_deadline_ms: now + 30_000,
      deadline_remaining_ms: 30_000,
    },
    lifecycle: {
      status: "available",
      recorded: 3,
      matured: 1,
      truncated: false,
      milestones: [
        {
          candidate_id: "a".repeat(64),
          state: "replayed",
          original_recorded_state: "replayed",
          effective_state: "evaluated",
          effective_scientific_state: "insufficient_evidence",
          origin_session: "2026-10-27",
          continued: true,
          observation_scope: "episode_key_only",
          status: "scored",
          gaps: [],
          measurement_scope: "continuation_original_episode",
          exit_status: "executed",
          measurement_gaps: [],
          measurement_sha256: "b".repeat(64),
          measurement_status: "verified",
          measurement_reason: null,
          original_measurement_sha256: "e".repeat(64),
          original_exit_status: "exit_quote_missing",
          original_measurement_status: "unavailable",
          original_measurement_reason:
            "candidate_observation_measurement_unverified",
          continuation_status: "verified",
          continuation_reason: null,
          source_observation_ref_status: "verified",
          source_observation_ref_reason: null,
          continuation_state: "evaluated",
          continuation_recorded_in_session: "2026-10-28",
          processed_at_ms: now - 60_000,
          exit_at_ms: now - 61_000,
        },
      ],
    },
    cohort: {
      account: "private-fixture",
      cohort_key: "c".repeat(64),
      feed: "wide_quotes",
      dataset_abi: "scorer_replay_v1",
    },
    trials: {
      status: "partial",
      reason: "trial_ledger_partial_read",
      counts_lower_bound: true,
      scope: "verified_prefix_lower_bound",
      global_tested_trial_count: null,
      admitted_trial_count: null,
      executed_trial_count: null,
      terminal_trial_count: null,
      unknown_exposure_count: null,
      distinct_terminal_tasks: null,
      distinct_lifecycle_candidates: null,
      distinct_sessions: null,
      distinct_sources: null,
      known_lower_bound_counts: {
        global_tested_trial_count: 3,
        admitted_trial_count: 5,
        executed_trial_count: 4,
        terminal_trial_count: 3,
        unknown_exposure_count: 1,
        distinct_terminal_tasks: 3,
        distinct_lifecycle_candidates: 5,
        distinct_sessions: 2,
        distinct_sources: 2,
      },
      family_key_is_label_only: true,
      reset: false,
    },
    capture: {
      enabled: true,
      status: "journal_readable",
      coverage: "journal_readable",
      coverage_label: "journal_readability_only",
      current_eligibility: "not_evaluated",
      eligible_for_attempt: false,
      used: 2,
      remaining: 2,
      max_calls_per_session: 4,
      max_keys: 8,
      max_rows: 64,
      min_interval_ms: 60_000,
      receipts: "present",
      provider_called: false,
      state_written: false,
      as_of_ms: now - 5_000,
    },
    measurement: {
      evaluation_scope: "independent_episodes",
      strategy_portfolio_claimed: false,
      portfolio_return: null,
      aggregate_net_pnl: null,
    },
    financial_profit_claim: false,
    activation_authorized: false,
    ...overrides,
  };
}

function measurement(overrides: Record<string, unknown> = {}) {
  return {
    version: 1,
    kind: "forward_observation_measurement",
    candidate_id: "candidate-1",
    task_id: "task-1",
    revision: 1,
    evaluation_scope: "independent_episodes",
    strategy_portfolio_claimed: false,
    portfolio_return: null,
    aggregate_net_pnl: null,
    episode_count: 1,
    episodes: [
      {
        candidate_id: "candidate-1",
        key: "TEST|1",
        decision_at_ms: now - 120_000,
        exit_due_ms: now - 60_000,
        status: "scored",
        entry: true,
        entry_quote_sha256: "d".repeat(64),
        exit_quote_sha256: "e".repeat(64),
        exit_status: "executed",
        after_cost: {
          net_pnl_if_flat: "-12.34",
          fees: "1.50",
          turnover: "2400.00",
          flat: true,
          accounting_complete: true,
          exit_status: "executed",
          gaps: [],
        },
        maturation_gaps: [],
        residual_positions: [],
        missing_marks: [],
        original_report_sha256: "f".repeat(64),
      },
    ],
    original_refs: {
      source_task_id: "source-task-1",
      source_revision: 1,
      source_candidate_id: "source-candidate-1",
      source_lifecycle_candidate_id: "lifecycle-1",
      source_result_sha256: "1".repeat(64),
      source_runtime_hash: "2".repeat(64),
      source_sha256: "3".repeat(64),
      params_sha256: "4".repeat(64),
      dataset_hash: "5".repeat(64),
      observation_sha256: "6".repeat(64),
    },
    authorship: {
      authoring_at_ms: now - 300_000,
      target_frozen_at_ms: now - 240_000,
      score_produced_at_ms: now - 180_000,
      decision_at_ms: now - 120_000,
      clock_source: "recorded_helper_completion_clock",
      original_strategy_measurement: false,
      observation_projection: "single_entry",
      study_projection: {
        selection_top_n: 1,
        max_entries: 1,
        entry_price_offset_ticks: 0,
        original_multi_entry_strategy: false,
        study_only: true,
      },
    },
    replay: {
      status: "not_present",
      copied_unchanged: true,
      reason: "source_candidate_has_no_task8_execution_report",
    },
    actual_scoring: {
      result_kind: "observation_score",
      status: "scored",
      score: "0.42",
      score_metric: "fixture_metric",
      score_sha256: "7".repeat(64),
      evidence_scope: "fixture",
      partial_evidence: false,
      qualified: false,
      gaps: [],
    },
    counts: {
      episodes: 1,
      sessions: 1,
      signals: 1,
      quote_receipts: 2,
      raw_republications: null,
      distinct_quotes: 2,
      source_scope: "episode_key_only",
      all_tape_quotes_counted: false,
    },
    capital: {
      research_capital_assumption: true,
      account_funds_claimed: false,
      amount: "2500.00",
      source: "observation_spec_parameters_research_capital_assumption",
    },
    qualification: {
      qualified: false,
      promotion_authorized: false,
      activation_authorized: false,
      broker_funds_claimed: false,
      return_claimed: false,
      reason: "independent_episode_observation_not_a_funded_portfolio",
    },
    recorded_measurement_at_ms: now - 30_000,
    projection_sha256: "8".repeat(64),
    live_context_sha256: "9".repeat(64),
    projection_scope: "immutable_episode_evidence",
    cohort: {
      account: "private-fixture",
      cohort_key: "c".repeat(64),
      source_cohort_key: null,
      source_cohort_key_reason: "source_cohort_not_recorded_in_target",
      status: "runtime_matched_cohort_unknown",
      runtime_matched: true,
      cohort_matched: null,
      feed: "wide_quotes",
      dataset_abi: "scorer_replay_v1",
      lineage: "owner_candidate_v1",
    },
    trials: {
      status: "available",
      counts_lower_bound: false,
      scope: "verified_prefix",
      known_lower_bound_counts: null,
      global_tested_trial_count: 3,
      admitted_trial_count: 5,
      executed_trial_count: 4,
      terminal_trial_count: 3,
      unknown_exposure_count: 1,
      distinct_terminal_tasks: 3,
      distinct_lifecycle_candidates: 5,
      distinct_sessions: 2,
      distinct_sources: 2,
      family_key: {
        source_sha256: "3".repeat(64),
        params_sha256: "4".repeat(64),
        training_from_ms: now - 1_000_000,
        training_to_ms: now - 500_000,
        feature_abi: "observation_single_entry_v1",
      },
      family_key_is_label_only: true,
      family_count_scope: "session_local_not_global",
      multiple_testing: "unadjusted",
      reset: false,
    },
    baseline: {
      matched: false,
      delta: null,
      reason: "no_exact_common_cohort_baseline",
    },
    live_context: {
      cohort: {
        status: "runtime_matched_cohort_unknown",
        runtime_matched: true,
        cohort_matched: null,
      },
      trials: { status: "available" },
    },
    ...overrides,
  };
}

function libraryItem(measurementValue: Record<string, unknown>) {
  return {
    candidate_id: "candidate-1",
    session: "2026-10-27",
    origin_session: "2026-10-27",
    continued: true,
    state: "replayed",
    feature_abi: "observation_single_entry_v1",
    observation_measurement: measurementValue,
    observation_measurement_present: true,
    measurement_status: "verified",
    measurement_reason: null,
    original_measurement_status: "verified",
    original_measurement_reason: null,
    continuation_status: "verified",
    continuation_reason: null,
    continuation: {
      state: "evaluated",
      current_scope: { current_session: "2026-10-29" },
      processed_at_ms: now - 60_000,
      exit_at_ms: now - 61_000,
    },
  };
}

test("owner research is absent-safe and never invents disabled or zero", () => {
  const textValue = text(
    <OperationalPanel summary={summary()} portfolio={portfolio()} />,
  );
  assert.match(textValue, /Owner research/);
  assert.match(textValue, /Not present in this snapshot/i);
  assert.doesNotMatch(textValue, /Evidence count\s*0/);
  assert.doesNotMatch(textValue, /Research state\s*Disabled/);
});

test("owner research config unavailable is not disabled", () => {
  const operational = researchOperational({
    config: "unavailable",
    config_reason: "planner_config_unavailable",
    enabled: null,
    due: null,
    state: null,
    reason: "planner_config_unavailable",
    evidence_count: null,
    cutoff_ms: null,
    deferred_until_ms: null,
    pending_source: "not_checked",
    pending: null,
    as_of_ms: now - 5_000,
  });
  const textValue = text(
    <OperationalPanel
      summary={summary({ operational_health: operational })}
      portfolio={portfolio()}
      health={health(operational)}
    />,
  );
  assert.match(textValue, /Config unavailable/);
  assert.match(textValue, /Planner config unavailable/);
  assert.match(textValue, /not Disabled/i);
});

test("owner research invalid version is not interpreted", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        owner_research: {
          version: 2,
          status: "due",
          evidence_count: 3,
          as_of_ms: now - 5_000,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /version 2 is not supported/i);
  assert.match(textValue, /not interpreted/i);
  assert.doesNotMatch(textValue, /Research state\s*Due/);
});

test("owner research future as-of is a clock anomaly", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        owner_research: {
          version: 1,
          status: "awaiting_evidence",
          reason: "owner_research_no_eligible_evidence",
          evidence_count: 0,
          as_of_ms: now + 120_000,
          cutoff_ms: now + 600_000,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Clock anomaly/i);
  assert.match(textValue, /not trusted/i);
  assert.doesNotMatch(textValue, /Recent capture/);
});

test("owner research awaiting evidence states no model call and bounds unknown reasons", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        owner_research: {
          version: 1,
          status: "awaiting_evidence",
          reason: "provider said /tmp/private-token",
          evidence_count: 0,
          as_of_ms: now - 5_000,
          cutoff_ms: now + 600_000,
          workspace_refs: ["notes/private.md", "skills/private.md"],
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /No eligible evidence/i);
  assert.match(textValue, /no model call/i);
  assert.match(textValue, /Unknown reason/);
  assert.doesNotMatch(textValue, /tmp\/private-token/);
  assert.doesNotMatch(textValue, /notes\/private\.md/);
  // A raw ref array is not the public contract: only the sanitized scalar
  // count is rendered, and its absence is explicit "Not reported", not 2.
  assert.match(textValue, /Workspace refs\s*Not reported/);
  assert.match(textValue, /Workspace snapshot\s*Not reported/);

  const known = text(
    <OperationalPanel
      summary={summary({
        owner_research: {
          version: 1,
          status: "awaiting_evidence",
          reason: "owner_research_no_eligible_evidence",
          evidence_count: 0,
          as_of_ms: now - 5_000,
          cutoff_ms: now + 600_000,
          workspace_ref_count: 2,
          workspace_snapshot_sha256: "a".repeat(64),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(known, /Workspace refs\s*2/);
  assert.match(known, /aaaaaaaaaaaa…/);
});

test("observation queue separates counts and trial debt is a lower bound", () => {
  const observationValue = observation();
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: observationValue,
          observation_capture: { status: "disabled" },
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Prospective observation/);
  assert.match(textValue, /Queue pending/);
  assert.match(textValue, /Queue running/);
  assert.match(textValue, /Queue cancel requested/);
  assert.match(textValue, />= 3 known; incomplete history/);
  assert.match(textValue, /main counts stay Not reported/i);
  assert.match(textValue, /2026-10-28/);
  assert.match(textValue, /does not claim the financial fill/i);
  assert.match(textValue, /Not validated/i);
  assert.match(textValue, /truncated/i);
});

test("measurement is independent episodes and unavailable PnL is not zero", () => {
  const noEntry = measurement({
    episodes: [
      {
        ...measurement().episodes[0],
        entry: false,
        exit_status: "entry_gap",
        after_cost: null,
        maturation_gaps: ["entry_quote_stale"],
      },
    ],
  });
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [
            libraryItem(noEntry),
            {
              ...libraryItem(measurement()),
              candidate_id: "candidate-2",
              observation_measurement: null,
              observation_measurement_present: true,
              measurement_status: "unavailable",
              measurement_reason:
                "candidate_observation_measurement_unverified",
              original_measurement_status: "unavailable",
              original_measurement_reason:
                "candidate_observation_measurement_unverified",
            },
          ],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(
    textValue,
    /Independent virtual episodes — not portfolio returns/,
  );
  assert.match(textValue, /No entry/i);
  assert.match(textValue, /Unavailable/);
  assert.match(textValue, /not zero/i);
  assert.doesNotMatch(textValue, /Episode PnL\s*₹0/);
  assert.match(textValue, /₹2,500\.00/);
  assert.match(textValue, /declared research capital assumption/i);
  assert.match(textValue, /not account funds/i);
  // An unavailable checked measurement is an explicit entry even when the
  // effective body is null; the original status is preserved separately.
  assert.match(
    textValue,
    /Measurement unavailable: Observation measurement unverified/,
  );
  assert.match(
    textValue,
    /Original record unavailable: Observation measurement unverified/,
  );
});

test("measurement shows immutable episode hash separately from live context", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [libraryItem(measurement())],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Immutable episode hash/);
  assert.match(textValue, /Live context hash/);
  assert.match(textValue, /does not rewrite/i);
  assert.match(textValue, /-₹12\.34/);
  assert.match(textValue, /Cohort match unknown/i);
  assert.match(textValue, /does not claim the financial fill/i);
  assert.doesNotMatch(textValue, /validated/i);
  // The checked status and original record are shown separately from the
  // effective immutable episode evidence.
  assert.match(textValue, /Measurement state\s*Verified/);
  assert.match(textValue, /Original record\s*Original record verified/);
});

test("cross-session continuity uses observation milestones, not library continuation", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [libraryItem(measurement())],
          activation_authorized: false,
        },
        research_tasks: { status: "idle", observation: observation() },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Cross-session continuity/);
  assert.match(textValue, /Original recorded/);
  assert.match(textValue, /Effective scientific/);
  assert.match(textValue, /2026-10-27/);
  assert.match(textValue, /2026-10-28/);
  assert.doesNotMatch(textValue, /2026-10-29/);
  assert.match(textValue, /Milestones are the only cross-session source/i);
  // Actual d946 vocabulary: the scientific map is separate from the recorded
  // lifecycle state, and the original record is preserved separately.
  assert.match(textValue, /Insufficient evidence/);
  assert.match(
    textValue,
    /Original record unavailable: Observation measurement unverified/,
  );
  assert.match(textValue, /Verified source ref/);
  assert.doesNotMatch(textValue, /continued_evaluated|Continued evaluated/);

  const truncated = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: observation({
            lifecycle: {
              ...observation().lifecycle,
              truncated: true,
            },
          }),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(truncated, /Milestone source is truncated/i);
  assert.match(truncated, /not a complete total/i);
});

test("three-session milestone keeps original, recording and current read separate", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: observation({
            lifecycle: {
              status: "available",
              recorded: 1,
              matured: 1,
              truncated: false,
              milestones: [
                {
                  ...observation().lifecycle.milestones[0],
                  origin_session: "2026-10-26",
                  continuation_recorded_in_session: "2026-10-28",
                },
              ],
            },
          }),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /2026-10-26/);
  assert.match(textValue, /2026-10-28/);
  assert.doesNotMatch(textValue, /2026-10-29/);
  assert.match(textValue, /current session does not claim the financial fill/i);
});

test("library continued flags alone are not cross-session evidence", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [libraryItem(measurement())],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /No cross-session continuity/);
  assert.doesNotMatch(textValue, /2026-10-29/);
});

test("unavailable observation without source keeps reason, queue and trials", () => {
  const unavailable = {
    status: "unavailable",
    reason: "candidate_task_state_unavailable",
    queue: {
      pending: null,
      running: null,
      unknown: null,
      cancel_requested: null,
      succeeded: null,
      total: null,
      truncated: null,
    },
    lifecycle: {
      status: "unavailable",
      recorded: null,
      matured: null,
      milestones: [],
    },
    trials: {
      status: "unavailable",
      reason: "verified_chain_unavailable",
      counts_lower_bound: false,
    },
    as_of_ms: now - 5_000,
  };
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: { status: "idle", observation: unavailable },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Observation projection is unavailable/i);
  assert.match(textValue, /Candidate task state unavailable/i);
  assert.match(textValue, /Queue pending/);
  assert.match(textValue, /Trial debt status/);
  assert.doesNotMatch(textValue, /Queue pending\s*0/);
});

test("unknown observation version is unsupported, never available or idle", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: {
            ...observation(),
            version: 2,
          },
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /version 2 is not supported/i);
  assert.match(textValue, /not interpreted/i);
  assert.doesNotMatch(textValue, /Queue pending\s*2/);
});

test("capture journal readability is separate from current eligibility", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: { status: "idle", observation: observation() },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Journal readable/);
  assert.match(textValue, /Journal readability only/);
  assert.match(textValue, /Current eligibility/);
  assert.match(textValue, /Not evaluated/);
  assert.match(textValue, /Eligible for attempt\s*No/);
  assert.match(textValue, /8 keys · 64 rows · 60,000 ms interval/);
  assert.match(textValue, /not trading risk caps/i);
  assert.match(textValue, /makes no provider guarantee/i);
  assert.doesNotMatch(
    textValue,
    /provider guarantee\s*(?:Yes|True|Confirmed)/i,
  );
  assert.doesNotMatch(textValue, /new model call/i);
});

test("continuation quote refs use typed archive hashes and receipt clocks", () => {
  const continuation = {
    version: 1,
    source_scope: "continuation_original_episode",
    entry: true,
    exit_status: "executed",
    flat: true,
    accounting_complete: true,
    net_pnl_if_flat: "5.00",
    capital_assumption: true,
    capital: "2500.00",
    entry_quote: {
      archive_sha256: "1".repeat(64),
      purpose: "research_observation",
      source_ms: now - 200_000,
      receipt_ms: now - 199_000,
    },
    exit_quote: {
      archive_sha256: "2".repeat(64),
      purpose: "research_observation",
      source_ms: now - 100_000,
      receipt_ms: now - 99_000,
    },
    exit_due_ms: now - 100_000,
    exit_deadline_ms: now - 90_000,
    exit_at_ms: now - 95_000,
    processed_at_ms: now - 85_000,
    recorded_at_ms: now - 80_000,
    measurement_sha256: "3".repeat(64),
    gaps: [],
  };
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [
            libraryItem(continuation as unknown as Record<string, unknown>),
          ],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Entry quote archive hash/);
  assert.match(textValue, /111111111111…/);
  assert.match(textValue, /Exit quote archive hash/);
  assert.match(textValue, /222222222222…/);
  assert.match(textValue, /Continuation receipt hash/);
  assert.match(textValue, /333333333333…/);
  assert.match(textValue, /Continuation receipt scope/);
  assert.match(textValue, /continuation_original_episode/);
  assert.match(textValue, /Recorded source clock/);
  assert.match(textValue, /Processed source clock/);
  assert.match(textValue, /Exit source clock/);
  assert.match(textValue, /Immutable episode hash\s*Not reported/);
});

test("partial trial debt renders bounded gap reasons, not raw JSON", () => {
  const partial = observation({
    trials: {
      status: "partial",
      reason: "trial_ledger_partial_read",
      counts_lower_bound: true,
      scope: "verified_prefix_lower_bound",
      gaps: ["trial_queue_body_invalid", "trial_lifecycle_missing"],
      known_lower_bound_counts: { global_tested_trial_count: 3 },
      global_tested_trial_count: null,
    },
  });
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: { status: "idle", observation: partial },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Trial gaps/);
  assert.match(textValue, /Trial queue body invalid/);
  assert.match(textValue, /Trial lifecycle missing/);
  assert.doesNotMatch(textValue, /trial_queue_body_invalid/);
});

test("milestone observation and measurement gaps render known reasons separately", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: observation({
            lifecycle: {
              status: "available",
              recorded: 1,
              matured: 1,
              truncated: false,
              milestones: [
                {
                  ...observation().lifecycle.milestones[0],
                  gaps: ["entry_quote_stale"],
                  measurement_gaps: ["exit_quote_missing"],
                },
              ],
            },
          }),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Observation gaps/);
  assert.match(textValue, /Entry source quote was stale/);
  assert.match(textValue, /Measurement gaps/);
  assert.match(textValue, /Exit source quote missing/);
  assert.doesNotMatch(textValue, /entry_quote_stale/);
});

test("unsupported measurement version is an explicit entry, not no evidence", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [libraryItem({ version: 2, net_pnl_if_flat: "999.00" })],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Measurement version 2 is not supported/i);
  assert.match(textValue, /No PnL, return or progress is inferred/i);
  assert.doesNotMatch(textValue, /No measurement evidence/);
  assert.doesNotMatch(textValue, /₹999/);
});

test("display caps state showing N of M and never extrapolate a total", () => {
  const milestones = Array.from({ length: 21 }, (_, index) => ({
    ...observation().lifecycle.milestones[0],
    candidate_id: `${index}`.padStart(64, "a"),
  }));
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: observation({
            lifecycle: {
              status: "available",
              recorded: 21,
              matured: 1,
              truncated: false,
              milestones,
            },
          }),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Showing 20 of 21 milestones/);
  assert.match(textValue, /display limit/i);
});

test("source-truncated milestones do not claim a total", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_tasks: {
          status: "idle",
          observation: observation({
            lifecycle: {
              status: "available",
              recorded: 3,
              matured: 1,
              truncated: true,
              milestones: observation().lifecycle.milestones,
            },
          }),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Milestone source is truncated/i);
  assert.match(textValue, /total count not reported/i);
});

test("measurement list display cap states showing N of M", () => {
  const items = Array.from({ length: 6 }, (_, index) => ({
    ...libraryItem(measurement()),
    candidate_id: `${index}`.padStart(64, "b"),
  }));
  const textValue = text(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items,
          truncated: false,
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(textValue, /Showing 5 of 6 measurement entries/);
  assert.match(textValue, /display limit/i);
});

test("rendered untrusted fields stay bounded and never become private text", () => {
  const unsafeObservation = observation({
    lifecycle: {
      status: "available",
      recorded: 1,
      matured: 1,
      truncated: false,
      milestones: [
        {
          ...observation().lifecycle.milestones[0],
          candidate_id: '<img src=x onerror="alert(1)">',
          origin_session: "/home/private/session",
          original_recorded_state: "/home/private/state",
          effective_state: "<script>alert(2)</script>",
          continuation_state: "javascript:alert(3)",
          continuation_recorded_in_session: "../../private",
          measurement_sha256: "/home/private/hash",
          gaps: ["/home/private/gap"],
        },
      ],
    },
    capture: {
      ...observation().capture,
      status: "/home/private/status",
      coverage: "/home/private/coverage",
      coverage_label: "/home/private/label",
      current_eligibility: "/home/private/eligibility",
      reason: "/home/private/reason",
    },
  });
  const unsafeSummary = summary({
    owner_research: {
      version: 1,
      status: "awaiting_evidence",
      reason: "/home/private/reason",
      evidence_count: 0,
      as_of_ms: now - 5_000,
      workspace_refs: ["/home/private/workspace.md"],
      workspace_ref_count: null,
    },
    research_tasks: { status: "idle", observation: unsafeObservation },
    research_library: {
      status: "ready",
      items: [
        libraryItem(
          measurement({
            candidate_id: '<img src=x onerror="alert(4)">',
            original_refs: {
              ...measurement().original_refs,
              private_path: "/home/private/account.json",
            },
          }),
        ),
      ],
      activation_authorized: false,
    },
  });
  const html = markup(
    <OperationalPanel summary={unsafeSummary} portfolio={portfolio()} />,
  );
  const textValue = new JSDOM(html).window.document.body.textContent || "";
  assert.doesNotMatch(html, /<img/i);
  assert.doesNotMatch(html, /<script/i);
  assert.doesNotMatch(textValue, /\/home\/private/);
  assert.doesNotMatch(textValue, /alert\(/);
  assert.doesNotMatch(textValue, /javascript:/);
  assert.match(textValue, /Unknown reason/);
  assert.match(textValue, /Not reported/);
});

test("workspace count and snapshot come only from public scalars", () => {
  const absent = text(
    <OperationalPanel
      summary={summary({
        owner_research: {
          version: 1,
          status: "awaiting_evidence",
          reason: "owner_research_no_eligible_evidence",
          evidence_count: 0,
          as_of_ms: now - 5_000,
          workspace_refs: ["/home/private/notes.md", "/home/private/skills.md"],
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(absent, /Workspace refs\s*Not reported/);
  assert.doesNotMatch(absent, /\/home\/private/);

  const present = text(
    <OperationalPanel
      summary={summary({
        owner_research: {
          version: 1,
          status: "awaiting_evidence",
          reason: "owner_research_no_eligible_evidence",
          evidence_count: 0,
          as_of_ms: now - 5_000,
          workspace_ref_count: 3,
          workspace_snapshot_sha256: "c".repeat(64),
        },
      })}
      portfolio={portfolio()}
    />,
  );
  assert.match(present, /Workspace refs\s*3/);
  assert.match(present, /cccccccccccc…/);
  assert.match(present, /raw workspace paths are never read/i);
});

test("measurement never renders private refs or untrusted HTML", () => {
  const unsafe = measurement({
    candidate_id: '<img src=x onerror="alert(1)">',
    original_refs: {
      ...measurement().original_refs,
      private_path: "/home/private/account.json",
    },
  });
  const html = markup(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [libraryItem(unsafe)],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  const textValue = new JSDOM(html).window.document.body.textContent || "";
  assert.doesNotMatch(html, /<img/i);
  assert.doesNotMatch(textValue, /\/home\/private/);
  assert.doesNotMatch(textValue, /alert\(1\)/);
});

test("measurement never aggregates PnL or draws an equity chart", () => {
  const html = markup(
    <OperationalPanel
      summary={summary({
        research_library: {
          status: "ready",
          items: [libraryItem(measurement())],
          activation_authorized: false,
        },
      })}
      portfolio={portfolio()}
    />,
  );
  const textValue = new JSDOM(html).window.document.body.textContent || "";
  assert.doesNotMatch(textValue, /Sharpe\s*[:=]/i);
  assert.doesNotMatch(textValue, /Portfolio return\s*₹/);
  assert.doesNotMatch(textValue, /equity curve\s*[:=]/i);
  assert.doesNotMatch(textValue, /profit percentage\s*[:=]/i);
  assert.doesNotMatch(html, /<canvas/i);
});

test("book continuity keeps stopped and halted books explicit without controls", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        state: {
          stopped: true,
          halted: false,
          closed: false,
          health: "stopped",
        },
      })}
      portfolio={portfolio({ stopped: true })}
    />,
  );
  assert.match(textValue, /Paper book continuity/);
  assert.match(textValue, /Stopped/);
  assert.match(textValue, /₹5,000\.00/);
  assert.match(textValue, /no restart, unhalt or activation control/i);
});

test("halted book stays explicit and observation disabled is not zero", () => {
  const textValue = text(
    <OperationalPanel
      summary={summary({
        state: {
          stopped: false,
          halted: true,
          closed: false,
          health: "halted",
        },
        research_tasks: { observation: { status: "disabled" } },
      })}
      portfolio={portfolio({ halted: true })}
    />,
  );
  assert.match(textValue, /Halted/);
  assert.match(textValue, /Observation is disabled by policy/i);
  assert.doesNotMatch(textValue, /Queue pending\s*0/);
  assert.doesNotMatch(textValue, /Queue running\s*0/);
});
