import { test } from "node:test";
import assert from "node:assert/strict";
import {
  captureStatusLabel,
  cohortStatus,
  episodePnl,
  liveContextHash,
  measurementHash,
  measurementState,
  measurementTimeline,
  money,
  observationState,
  qualificationLabel,
  replayAxes,
  researchReason,
  safeHash,
  trialLowerBound,
  trialMainCount,
} from "./research";
import type { EpisodeMeasurement, ObservationTrialFold } from "./types";

const partialTrials: ObservationTrialFold = {
  status: "partial",
  counts_lower_bound: true,
  known_lower_bound_counts: { global_tested_trial_count: 3 },
  global_tested_trial_count: null,
};
const availableTrials: ObservationTrialFold = {
  status: "available",
  counts_lower_bound: false,
  global_tested_trial_count: 0,
};

test("trial debt keeps null separate from an authoritative zero", () => {
  assert.equal(
    trialMainCount(partialTrials, "global_tested_trial_count"),
    "Not reported",
  );
  assert.equal(
    trialMainCount(availableTrials, "global_tested_trial_count"),
    "0",
  );
  assert.equal(
    trialMainCount(null, "global_tested_trial_count"),
    "Not reported",
  );
  assert.equal(
    trialLowerBound(partialTrials),
    ">= 3 known; incomplete history",
  );
  assert.equal(trialLowerBound(availableTrials), "Available");
  assert.equal(trialLowerBound({ status: "unavailable" }), "Unavailable");
});

test("episode hash is immutable while live context hash may change", () => {
  const episode = "a".repeat(64);
  const first = {
    projection_sha256: episode,
    live_context_sha256: "b".repeat(64),
  };
  const second = {
    projection_sha256: episode,
    live_context_sha256: "c".repeat(64),
  };
  assert.equal(measurementHash(first), measurementHash(second));
  assert.notEqual(liveContextHash(first), liveContextHash(second));
  assert.equal(
    measurementHash({ projection_sha256: "not-a-hash" }),
    "Not reported",
  );
});

test("unknown measurement states never become zero PnL", () => {
  const states: Record<string, unknown>[] = [
    { entry: false, exit_status: "entry_gap" },
    { entry: true, exit_status: "unknown" },
    { entry: true, exit_status: "executed", after_cost: null },
    {
      entry: true,
      exit_status: "executed",
      after_cost: {
        flat: false,
        accounting_complete: true,
        net_pnl_if_flat: "1",
      },
    },
    {
      entry: true,
      exit_status: "executed",
      after_cost: {
        flat: true,
        accounting_complete: false,
        net_pnl_if_flat: "1",
      },
    },
    {
      entry: true,
      exit_status: "executed",
      after_cost: {
        flat: true,
        accounting_complete: true,
        net_pnl_if_flat: "NaN",
      },
    },
  ];
  for (const episode of states) {
    const measurement = {
      version: 1,
      kind: "forward_observation_measurement",
      evaluation_scope: "independent_episodes",
      episodes: [episode],
    } as unknown as EpisodeMeasurement;
    const pnl = episodePnl(measurement);
    assert.equal(pnl.available, false);
    assert.equal(pnl.value, "Unavailable");
    assert.notEqual(pnl.value, "₹0.00");
    assert.ok(pnl.reason.length > 0);
  }
  // The actual replay projection distinguishes an explicit unavailable source
  // from the authoritative no-report absence; an unknown status string is never
  // relabelled as "Copied unchanged".
  assert.equal(
    replayAxes({
      replay: { status: "unavailable", reason: "source_unavailable" },
    } as unknown as EpisodeMeasurement).status,
    "Unavailable",
  );
  assert.equal(
    replayAxes({
      replay: { status: "not_present" },
    } as unknown as EpisodeMeasurement).status,
    "No Task 8 execution report",
  );
  assert.equal(
    replayAxes({
      replay: { copied_unchanged: true, source: "task8_execution_report" },
    } as unknown as EpisodeMeasurement).status,
    "Copied unchanged from Task 8 report",
  );
  assert.equal(
    replayAxes({
      replay: { status: "unexpected" },
    } as unknown as EpisodeMeasurement).status,
    "Unsupported source status",
  );
  const good = {
    version: 1,
    kind: "forward_observation_measurement",
    evaluation_scope: "independent_episodes",
    episodes: [
      {
        entry: true,
        exit_status: "executed",
        after_cost: {
          flat: true,
          accounting_complete: true,
          net_pnl_if_flat: "12.50",
        },
      },
    ],
  } as unknown as EpisodeMeasurement;
  assert.deepEqual(episodePnl(good), {
    available: true,
    value: "₹12.50",
    reason: "",
  });
});

test("money accepts only explicit finite values", () => {
  assert.equal(money("2500.00"), "₹2,500.00");
  assert.equal(money("-12.34"), "-₹12.34");
  assert.equal(money(0), "₹0.00");
  assert.equal(money("NaN"), null);
  assert.equal(money("Infinity"), null);
  assert.equal(money("1e9"), null);
  assert.equal(money(Number.POSITIVE_INFINITY), null);
  assert.equal(money("/home/private/account.json"), null);
});

test("observation source and version guards stay explicit", () => {
  assert.equal(observationState(undefined), "missing");
  assert.equal(observationState({ status: "available" }), "unsupported");
  assert.equal(
    observationState({
      status: "available",
      source: "original_queue_and_lifecycle_read_only",
      measurement: { evaluation_scope: "portfolio" },
    }),
    "unsupported",
  );
  assert.equal(
    observationState({
      status: "available",
      source: "original_queue_and_lifecycle_read_only",
      measurement: { evaluation_scope: "independent_episodes" },
    }),
    "present",
  );
  assert.equal(observationState({ status: "disabled" }), "disabled");
  assert.equal(
    observationState({
      status: "unavailable",
      reason: "candidate_task_state_unavailable",
      queue: { pending: null },
      trials: { status: "unavailable" },
    }),
    "present",
  );
  assert.equal(
    observationState({
      status: "available",
      version: 2,
      source: "original_queue_and_lifecycle_read_only",
    }),
    "unsupported",
  );
  assert.equal(observationState({ status: "mystery" }), "unsupported");
  assert.equal(observationState("not-json"), "unsupported");
});

test("hashes are shortened and anything that is not a hash is not reported", () => {
  assert.equal(safeHash("a".repeat(64)), `${"a".repeat(12)}…`);
  assert.equal(safeHash("/home/private/account.json"), "Not reported");
  assert.equal(safeHash("<img src=x>"), "Not reported");
});

test("cohort and reason labels are bounded and never claim validation", () => {
  assert.equal(
    cohortStatus("runtime_changed"),
    "Runtime changed; cohort not proven",
  );
  assert.equal(
    cohortStatus("runtime_unknown"),
    "Runtime unknown; cohort not proven",
  );
  assert.equal(
    cohortStatus("runtime_matched_cohort_unknown"),
    "Runtime matched; cohort match unknown",
  );
  assert.doesNotMatch(cohortStatus("anything"), /validated/i);
  assert.equal(
    researchReason("research_lane_min_interval", true),
    "Waiting between research turns",
  );
  assert.equal(
    researchReason("provider said /tmp/private", true),
    "Unknown reason",
  );
  assert.equal(
    researchReason("research_lane_min_interval", false),
    "Unknown reason",
  );
});

test("continuation measurement is a present independent episode", () => {
  assert.equal(
    measurementState({
      version: 1,
      source_scope: "continuation_original_episode",
    }),
    "present",
  );
  assert.equal(measurementState({ version: 2 }), "unsupported");
  assert.equal(measurementState(null), "missing");
});

test("continuation receipt hash is not the immutable episode projection", () => {
  const receipt = "a".repeat(64);
  assert.equal(
    measurementHash({ measurement_sha256: receipt }),
    "Not reported",
  );
  assert.equal(
    measurementHash({
      projection_sha256: receipt,
      measurement_sha256: "b".repeat(64),
    }),
    `${receipt.slice(0, 12)}…`,
  );
  const timeline = measurementTimeline({
    version: 1,
    source_scope: "continuation_original_episode",
    exit_due_ms: 1_000,
    exit_at_ms: 2_000,
    processed_at_ms: 3_000,
    recorded_at_ms: 4_000,
    recorded_measurement_at_ms: 9_000,
    as_of_ms: 9_500,
  } as unknown as EpisodeMeasurement);
  assert.ok(timeline.some(([label]) => label === "Recorded source clock"));
  assert.ok(
    timeline.some(
      ([label, value]) =>
        label === "Recorded source clock" && value !== "Not reported",
    ),
  );
  assert.ok(!timeline.some(([label]) => label === "Measurement recorded"));
  assert.ok(!timeline.some(([label]) => label === "Live context read"));
});

test("qualification is not reported unless the policy object is explicit", () => {
  const label = (value: unknown) =>
    qualificationLabel(value as unknown as EpisodeMeasurement);
  assert.equal(label({}), "Not reported");
  assert.equal(label({ version: 1 }), "Not reported");
  assert.equal(
    label({
      qualification: {
        qualified: false,
        promotion_authorized: false,
        activation_authorized: false,
      },
    }),
    "Not qualified; no promotion or activation",
  );
  assert.equal(
    label({
      qualification: { qualified: true, promotion_authorized: false },
    }),
    "Reported qualified by policy; no operating qualification claim",
  );
});

test("capture labels describe journal readability, not current eligibility", () => {
  assert.equal(captureStatusLabel("journal_readable"), "Journal readable");
  assert.equal(captureStatusLabel("unavailable"), "Unavailable");
  assert.equal(captureStatusLabel("<img src=x>"), "Not reported");
});
