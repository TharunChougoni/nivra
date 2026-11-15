import { count, datetime, numeric, words } from "./data";
import {
  ageLabel,
  configLabel,
  configReason,
  pendingKind,
  pendingSourceLabel,
  validTimestampMs,
} from "./operational";
import type {
  EpisodeMeasurement,
  EpisodeRecord,
  ObservationCapture,
  ObservationMilestone,
  ObservationStatus,
  ObservationTrialFold,
  OwnerResearchView,
  ResearchCandidate,
  ResearchContinuation,
  SupervisorResearch,
} from "./types";

// The backend projects bounded static reason codes. This client keeps its own
// allowlist so an unexpected value can never become visible operator text.
const RESEARCH_REASONS = new Set([
  "owner_research_artifact_bound",
  "owner_research_binding_invalid",
  "owner_research_clock_invalid",
  "owner_research_cutoff_invalid",
  "owner_research_expired",
  "owner_research_failed",
  "owner_research_interrupted",
  "owner_research_no_eligible_evidence",
  "owner_research_output_bound",
  "owner_research_output_citation_invalid",
  "owner_research_output_financial_refused",
  "owner_research_output_invalid",
  "owner_research_path_invalid",
  "owner_research_quote_capture_invalid",
  "owner_research_receipt_conflict",
  "owner_research_reconciliation_required",
  "owner_research_requires_decision_owner",
  "owner_research_requires_flat_or_postclose",
  "owner_research_settings_invalid",
  "owner_research_source_changed",
  "owner_research_source_invalid",
  "owner_research_source_missing",
  "owner_research_source_unverified",
  "owner_research_source_unsynced",
  "owner_research_turn_missing",
  "owner_research_workspace_changed",
  "owner_research_workspace_disabled",
  "owner_research_workspace_invalid",
  "owner_research_workspace_missing",
  "owner_research_workspace_not_file",
  "owner_research_workspace_not_text",
  "owner_research_workspace_permissions",
  "owner_research_workspace_source_invalid",
  "owner_research_workspace_source_missing",
  "owner_research_workspace_symlink",
  "research_lane_book_not_runnable",
  "research_lane_call_budget_exhausted",
  "research_lane_market_hours",
  "research_lane_log_unavailable",
  "research_lane_min_interval",
  "research_lane_required",
  "research_lane_settings_invalid",
  "research_lane_settings_missing",
  "research_lane_token_budget_exhausted",
  "research_lane_turn_budget_exhausted",
  "owner_research_workspace_too_large",
  "research_lifecycle_unavailable",
  "research_queue_unavailable",
  "research_source_changed",
  "research_source_invalid",
  "research_source_missing",
  "research_source_unverified",
  "supervisor_settings_invalid",
  "supervisor_failure_backoff",
  "planner_config_unavailable",
  "planner_usage_unavailable",
  "planner_usage_unavailable_recovery_required",
  "planner_authority_invalid",
  "planner_event_configuration_changed",
  "planner_integrity_failure",
  "controller_session_end_unavailable",
  "candidate_observation_disabled",
  "candidate_observation_measurement_unavailable",
  "candidate_observation_measurement_unbound",
  "candidate_observation_score_missing",
  "candidate_observation_target_missing",
  "candidate_task_state_unavailable",
  "candidate_lifecycle_unavailable",
  "candidate_predecessor_lifecycle_unavailable",
  "candidate_predecessor_continuation_unavailable",
  "verified_chain_unavailable",
  "trial_ledger_partial_read",
  "observation_capture_disabled",
  "observation_capture_not_configured",
  "observation_capture_journal_unavailable",
  "observation_capture_book_unavailable",
  "observation_capture_key_budget",
  "observation_capture_not_due",
  "observation_capture_refused",
  "observation_capture_session_date_mismatch",
  "observation_capture_clock_backwards",
  "observation_capture_clock_invalid",
  "observation_capture_response_invalid",
  "observation_capture_quote_invalid",
  "observation_capture_quote_stale",
  "observation_capture_quote_missing",
  "observation_capture_source_unavailable",
  "observation_capture_unavailable",
  "current_capture_not_eligible_in_source_window",
  "current_book_unavailable",
  "book_halted",
  "book_stopped",
  "book_closed",
  "market_closed",
  "not_a_trading_day",
  "calendar_unavailable",
  "quota_exhausted",
  "verified_universe_unavailable",
  "interest_key_not_in_verified_universe",
  "unsupported_session_scope",
  "unsupported_cross_session",
  "no_exact_common_cohort_baseline",
  "independent_episode_observation_not_a_funded_portfolio",
  "source_candidate_has_no_task8_execution_report",
  "source_candidate_result_not_available",
  // Canonical checked-measurement reasons from the actual d946 reader. Each is
  // a bounded static code; raw error text never reaches the panel.
  "candidate_observation_measurement_unverified",
  "candidate_observation_session_missing",
  "candidate_observation_identity_missing",
  "candidate_observation_source_unverified",
  "candidate_observation_queue_unavailable",
  "candidate_observation_task_truncated",
  "candidate_observation_task_unavailable",
  "candidate_observation_book_binding_mismatch",
  "candidate_observation_task_missing",
  "candidate_observation_identity_mismatch",
  "candidate_observation_target_invalid",
  "candidate_observation_result_unverified",
  "candidate_observation_score_mismatch",
  "candidate_observation_continuation_unverified",
]);

const RESEARCH_REASON_LABELS: Record<string, string> = {
  research_lane_book_not_runnable: "Book halted or stopped",
  research_lane_call_budget_exhausted: "Research call budget used up",
  research_lane_market_hours: "Waiting for market close",
  research_lane_log_unavailable: "Research log unreadable",
  research_lane_min_interval: "Waiting between research turns",
  research_lane_required: "Research lane profile required",
  research_lane_settings_invalid: "Research lane settings invalid",
  research_lane_settings_missing: "Research lane settings missing",
  research_lane_token_budget_exhausted: "Research token budget used up",
  research_lane_turn_budget_exhausted: "Research turn budget used up",
  owner_research_no_eligible_evidence: "No eligible evidence",
  owner_research_requires_flat_or_postclose:
    "Requires a flat book or post-close window",
  owner_research_settings_invalid: "Research settings invalid",
  owner_research_source_invalid: "Research source invalid",
  owner_research_source_unverified: "Research source unverified",
  owner_research_source_changed: "Research source changed",
  owner_research_workspace_invalid: "Research workspace invalid",
  owner_research_workspace_changed: "Research workspace changed",
  supervisor_failure_backoff: "Owner failure backoff",
  planner_config_unavailable: "Planner config unavailable",
  controller_session_end_unavailable: "Controller session end unavailable",
  candidate_observation_disabled: "Candidate observation disabled",
  candidate_observation_measurement_unavailable:
    "Observation measurement unavailable",
  candidate_observation_measurement_unbound: "Observation measurement unbound",
  candidate_observation_measurement_unverified:
    "Observation measurement unverified",
  candidate_observation_session_missing: "Observation session missing",
  candidate_observation_identity_missing: "Observation identity missing",
  candidate_observation_source_unverified: "Observation source unverified",
  candidate_observation_queue_unavailable: "Observation queue unavailable",
  candidate_observation_task_truncated: "Observation task scan truncated",
  candidate_observation_task_unavailable: "Observation task unavailable",
  candidate_observation_book_binding_mismatch:
    "Observation book binding mismatch",
  candidate_observation_task_missing: "Observation task missing",
  candidate_observation_identity_mismatch: "Observation identity mismatch",
  candidate_observation_target_invalid: "Observation target invalid",
  candidate_observation_result_unverified: "Observation result unverified",
  candidate_observation_score_mismatch: "Observation score mismatch",
  candidate_observation_continuation_unverified:
    "Observation continuation unverified",
  candidate_observation_score_missing: "Observation score missing",
  candidate_observation_target_missing: "Observation target missing",
  candidate_task_state_unavailable: "Candidate task state unavailable",
  candidate_lifecycle_unavailable: "Candidate lifecycle unavailable",
  candidate_predecessor_lifecycle_unavailable:
    "Predecessor lifecycle unavailable",
  candidate_predecessor_continuation_unavailable:
    "Predecessor continuation unavailable",
  verified_chain_unavailable: "Verified predecessor chain unavailable",
  trial_ledger_partial_read: "Trial ledger partial read",
  observation_capture_disabled: "Observation capture disabled",
  observation_capture_not_configured: "Observation capture not configured",
  observation_capture_journal_unavailable:
    "Observation capture journal unavailable",
  observation_capture_key_budget: "Observation capture key budget",
  current_capture_not_eligible_in_source_window:
    "Current capture is not eligible in the source window",
  current_book_unavailable: "Current book unavailable",
  book_halted: "Book halted",
  book_stopped: "Book stopped",
  book_closed: "Book closed",
  market_closed: "Market closed",
  not_a_trading_day: "Not a trading day",
  calendar_unavailable: "Calendar unavailable",
  quota_exhausted: "Quota exhausted",
  verified_universe_unavailable: "Verified universe unavailable",
  interest_key_not_in_verified_universe:
    "Interest key not in the verified universe",
  unsupported_session_scope: "Unsupported session scope",
  unsupported_cross_session: "Unsupported cross-session scope",
  no_exact_common_cohort_baseline: "No exact common cohort baseline",
  independent_episode_observation_not_a_funded_portfolio:
    "Independent episode observation; not a funded portfolio",
  source_candidate_has_no_task8_execution_report:
    "Source has no Task 8 execution report",
  source_candidate_result_not_available: "Source candidate result unavailable",
};

const EPISODE_GAP_LABELS: Record<string, string> = {
  entry_quote_stale: "Entry source quote was stale",
  entry_quote_missing: "Entry source quote missing",
  entry_gap: "No entry due to a source quote gap",
  no_entry: "No entry was recorded",
  exit_quote_missing: "Exit source quote missing",
  exit_quote_stale: "Exit source quote was stale",
  exit_gap: "Exit source quote gap",
  unsupported_cross_session: "Unsupported cross-session exit scope",
  unsupported_session_scope: "Unsupported session scope",
  coverage_truncated: "Coverage truncated",
  result_tampered: "Result binding could not be verified",
  measurement_unbound: "Measurement is not bound to this episode",
  measurement_unavailable: "Measurement unavailable",
  unexecuted: "Not executed",
  residual_positions: "Residual positions remain",
  incomplete_accounting: "Accounting incomplete",
  missing_marks: "Marks missing",
  no_fabricated_exit_price: "No fabricated exit price",
  sha_only_gap: "SHA-only gap; no fabricated price",
  continuation_processed_after_deadline:
    "Continuation processed after its deadline",
  observation_capture_disabled: "Observation capture disabled",
  candidate_observation_disabled: "Candidate observation disabled",
  observation_quote_missing: "Observation source quote missing",
  observation_coverage_budget_truncated:
    "Observation coverage budget truncated",
  scorer_score_missing_or_nonfinite: "Scorer score missing or non-finite",
  decision_after_declared_window: "Decision after the declared window",
  no_ranked_key: "No ranked key",
  missing_mark: "Mark missing",
  exit_measurement_unavailable: "Exit measurement unavailable",
  continuation_source_session_scope: "Continuation source session scope",
  continuation_source_tape_unavailable: "Continuation source tape unavailable",
  continuation_source_quote_missing: "Continuation source quote missing",
  continuation_source_quote_stale: "Continuation source quote stale",
  continuation_source_quote_gap: "Continuation source quote gap",
};

const TRIAL_GAP_LABELS: Record<string, string> = {
  trial_queue_read_unavailable: "Trial queue read unavailable",
  trial_queue_row_invalid: "Trial queue row invalid",
  trial_queue_body_invalid: "Trial queue body invalid",
  trial_candidate_schema_missing: "Trial candidate schema missing",
  trial_queue_schema_unreadable: "Trial queue schema unreadable",
  trial_queue_missing: "Trial queue missing",
  trial_queue_unreadable: "Trial queue unreadable",
  trial_source_policy_unavailable: "Trial source policy unavailable",
  trial_prefix_lifecycle_missing: "Trial prefix lifecycle missing",
  trial_prefix_lifecycle_unavailable: "Trial prefix lifecycle unavailable",
  trial_prefix_lifecycle_unknown: "Trial prefix lifecycle unknown",
  trial_prefix_unavailable: "Trial prefix unavailable",
  trial_lifecycle_missing: "Trial lifecycle missing",
  trial_lifecycle_legacy_ambiguous: "Trial lifecycle legacy ambiguous",
  trial_lifecycle_read_unavailable: "Trial lifecycle read unavailable",
  trial_lifecycle_row_invalid: "Trial lifecycle row invalid",
  trial_lifecycle_schema_unreadable: "Trial lifecycle schema unreadable",
  trial_lifecycle_unreadable: "Trial lifecycle unreadable",
};

const LIFECYCLE_STATES = new Set([
  "proposed",
  "sandbox_abi",
  "replayed",
  "forward_shadow",
  "evaluated",
  "eligible",
  "rejected",
  "insufficient",
  "paper_selected",
  "retired",
]);

const CONTINUATION_STATES = new Set([
  "evaluated",
  "unknown",
  "waiting",
  "pending_capture",
  "unavailable",
]);

// The scientific vocabulary is a SEPARATE projection from the recorded
// lifecycle state. The backend maps every continuation state to one of these
// four values; `continued_evaluated` is not a real source value and is never
// invented here.
const SCIENTIFIC_STATES: Record<string, string> = {
  insufficient_evidence: "Insufficient evidence",
  observation_measured: "Observation measured (checks not run)",
  eligible: "Eligible",
  rejected: "Rejected",
  retired: "Retired",
};

const MEASUREMENT_STATUS_LABELS: Record<string, string> = {
  verified: "Verified",
  not_matured: "Not matured",
  unavailable: "Unavailable",
};

const ORIGINAL_MEASUREMENT_STATUS_LABELS: Record<string, string> = {
  verified: "Original record verified",
  not_matured: "No original measurement",
  unavailable: "Original record unavailable",
};

const SOURCE_REF_STATUS_LABELS: Record<string, string> = {
  verified: "Verified source ref",
  not_matured: "Not matured",
  unavailable: "Unavailable",
};

const CAPTURE_COVERAGE: Record<string, string> = {
  journal_readable: "Journal readable",
  journal_readability_only: "Journal readability only",
  book_unavailable: "Book unavailable",
  journal_unavailable: "Journal unavailable",
  disabled: "Disabled",
};

const CAPTURE_ELIGIBILITY: Record<string, string> = {
  not_evaluated: "Not evaluated",
  eligible: "Reported eligible for attempt",
  not_eligible: "Not eligible",
  unavailable: "Unavailable",
};

const EXIT_STATUS: Record<string, string> = {
  executed: "Executed",
  no_entry: "No entry",
  entry_gap: "No entry (source quote gap)",
  exit_gap: "Exit gap",
  not_matured: "Not matured",
  unsupported_cross_session: "Unsupported cross-session exit scope",
  exit_scope_unsupported: "Exit scope unsupported",
  unknown: "Unknown",
};

const RESEARCH_STATES: Record<string, string> = {
  due: "Due",
  eligible: "Eligible",
  awaiting_evidence: "No eligible evidence",
  no_evidence: "No eligible evidence",
  deferred: "Deferred (backoff)",
  backoff: "Backoff",
  parked: "Parked",
  pending: "Pending",
  failed: "Failed",
  invalid: "Unavailable",
  unavailable: "Unavailable",
  expired: "Expired (cutoff passed)",
  busy: "Owner turn active",
  portfolio_open: "Not eligible (portfolio open)",
  disabled: "Disabled",
  attempted: "Attempted",
  unknown: "Unknown",
};

const TRIAL_STATUS: Record<string, string> = {
  available: "Available",
  partial: "Partial",
  unavailable: "Unavailable",
  disabled: "Disabled",
};

const CAPTURE_STATUS: Record<string, string> = {
  disabled: "Disabled",
  unknown: "Unknown",
  journal_readable: "Journal readable",
  captured: "Captured",
  timeout: "Timeout",
  not_eligible: "Not eligible",
  quota_exhausted: "Quota exhausted",
  not_due: "Not due",
  refused: "Refused",
  unavailable: "Unavailable",
  backoff: "Backoff",
  unsupported_session_scope: "Unsupported session scope",
  not_configured: "Not configured",
};

function sentence(value: string) {
  const text = value.replaceAll("_", " ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

export function researchReason(value: unknown, known?: boolean): string {
  if (known === false || typeof value !== "string") return "Unknown reason";
  if (!RESEARCH_REASONS.has(value)) return "Unknown reason";
  return RESEARCH_REASON_LABELS[value] || sentence(value);
}

export function researchConfig(value: unknown): string {
  return configLabel(value);
}

export function researchConfigReason(value: unknown): string {
  return configReason(value);
}

export function researchStateLabel(value: unknown): string {
  if (typeof value !== "string") return "Unknown";
  return RESEARCH_STATES[value] || "Unknown";
}

export function researchModelCall(
  state: unknown,
  evidenceCount: unknown,
): string {
  const evidence = numeric(evidenceCount);
  if (state === "awaiting_evidence" || evidence === 0)
    return "No eligible evidence; no model call was made.";
  switch (state) {
    case "due":
    case "eligible":
      return "Evidence eligible; no model call is claimed by this projection.";
    case "pending":
      return "Owner turn pending; no completion is claimed.";
    case "deferred":
    case "backoff":
      return "Deferred by owner failure backoff; no model call is claimed.";
    case "parked":
      return "Parked; no model call is claimed.";
    case "busy":
      return "Owner turn active; no completion is claimed.";
    case "portfolio_open":
      return "Portfolio open; research is not eligible.";
    case "expired":
      return "Cutoff passed; no model call is claimed.";
    case "disabled":
      return "Disabled by policy; no model call is claimed.";
    case "failed":
    case "invalid":
    case "unavailable":
      return "Unavailable; no model call is claimed.";
    default:
      return "Not reported.";
  }
}

export function ownerResearchFrom(
  summary?: { owner_research?: unknown } | null,
  supervisor?: { research?: SupervisorResearch | null } | null,
): OwnerResearchView | null {
  const raw = summary?.owner_research;
  if (raw && typeof raw === "object" && !Array.isArray(raw)) {
    const value = raw as OwnerResearchView;
    if (value.version === 1) return value;
    return null;
  }
  const research = supervisor?.research;
  if (!research || typeof research !== "object") return null;
  return {
    version: 1,
    status: research.state ?? null,
    research_due: research.due ?? null,
    as_of_ms: research.as_of_ms ?? null,
    cutoff_ms: research.cutoff_ms ?? null,
    evidence_count: research.evidence_count ?? null,
    state_available: null,
    reason: research.reason ?? null,
    evidence_id: research.evidence_id ?? null,
    deferred_until_ms: research.deferred_until_ms ?? null,
  };
}

export function ownerResearchVersionState(
  value: unknown,
): "missing" | "unsupported" | "present" {
  if (value === null || value === undefined) return "missing";
  if (typeof value !== "object" || Array.isArray(value)) return "unsupported";
  return (value as { version?: unknown }).version === 1
    ? "present"
    : "unsupported";
}

export function workspaceRefCount(value: unknown): number | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const parsed = numeric(
    (value as { workspace_ref_count?: unknown }).workspace_ref_count,
  );
  return parsed !== null && Number.isInteger(parsed) && parsed >= 0
    ? parsed
    : null;
}

export function safeHash(value: unknown): string {
  return typeof value === "string" && /^[0-9a-f]{64}$/.test(value)
    ? `${value.slice(0, 12)}…`
    : "Not reported";
}

export function safeSession(value: unknown): string {
  return typeof value === "string" && /^\d{4}-\d{2}-\d{2}$/.test(value)
    ? value
    : "Not reported";
}

export function observationState(
  value: unknown,
): "missing" | "unsupported" | "present" | "disabled" {
  if (value === null || value === undefined) return "missing";
  if (typeof value !== "object" || Array.isArray(value)) return "unsupported";
  const status = value as ObservationStatus;
  if (status.version != null && status.version !== 1) return "unsupported";
  if (status.status === "disabled") return "disabled";
  // Actual UNAVAILABLE branches carry a bounded reason, queue and trial fold
  // but no source. They stay interpretable; only AVAILABLE needs the source
  // guard.
  if (status.status === "unavailable") return "present";
  if (status.status !== "available") return "unsupported";
  if (status.source !== "original_queue_and_lifecycle_read_only")
    return "unsupported";
  if (
    status.measurement &&
    typeof status.measurement === "object" &&
    !Array.isArray(status.measurement) &&
    (status.measurement as { evaluation_scope?: unknown }).evaluation_scope !==
      "independent_episodes"
  )
    return "unsupported";
  return "present";
}

export function observationStatusLabel(value: unknown): string {
  return value === "available"
    ? "Available"
    : value === "unavailable"
      ? "Unavailable"
      : value === "disabled"
        ? "Disabled"
        : "Unknown";
}

export function observationSourceLabel(value: unknown): string {
  return value === "original_queue_and_lifecycle_read_only"
    ? "Original queue and lifecycle ledgers (read-only)"
    : "Not reported";
}

const OBSERVATION_REFS_STATUS_LABELS: Record<string, string> = {
  available: "Available",
  truncated: "Truncated",
  partial: "Partial",
  unavailable: "Unavailable",
};

export function observationRefsStatusLabel(value: unknown): string {
  return typeof value === "string"
    ? OBSERVATION_REFS_STATUS_LABELS[value] || "Unknown"
    : "Not reported";
}

export function trialStatusLabel(value: unknown): string {
  return typeof value === "string"
    ? TRIAL_STATUS[value] || "Unknown"
    : "Not reported";
}

export function trialCount(value: unknown): string {
  return numeric(value) === null ? "Not reported" : count(value);
}

export function trialGapReasons(
  trials?: ObservationTrialFold | null,
): string[] {
  if (!trials || !Array.isArray(trials.gaps)) return [];
  const seen = new Set<string>();
  const labels: string[] = [];
  for (const gap of trials.gaps) {
    if (typeof gap !== "string" || !TRIAL_GAP_LABELS[gap]) continue;
    const label = TRIAL_GAP_LABELS[gap];
    if (seen.has(label)) continue;
    seen.add(label);
    labels.push(label);
  }
  return labels;
}

export function trialGapReason(trials?: ObservationTrialFold | null): string {
  if (!trials) return "Not reported";
  if (trials.status === "partial" || trials.counts_lower_bound === true) {
    const reasons = trialGapReasons(trials);
    return reasons.length ? reasons.join("; ") : "Reason not reported";
  }
  return "Not applicable";
}

export function trialLowerBound(trials?: ObservationTrialFold | null): string {
  if (!trials) return "Not reported";
  if (trials.status === "partial" || trials.counts_lower_bound === true) {
    const tested = trials.known_lower_bound_counts?.global_tested_trial_count;
    return numeric(tested) === null
      ? "Known lower bound; incomplete history"
      : `>= ${count(tested)} known; incomplete history`;
  }
  if (trials.status === "unavailable") return "Unavailable";
  if (trials.status === "disabled") return "Disabled";
  if (trials.status === "available") return "Available";
  return "Not reported";
}

export function trialMainCount(
  trials: ObservationTrialFold | null | undefined,
  key:
    | "global_tested_trial_count"
    | "admitted_trial_count"
    | "executed_trial_count"
    | "terminal_trial_count"
    | "unknown_exposure_count"
    | "distinct_terminal_tasks"
    | "distinct_lifecycle_candidates"
    | "distinct_sessions"
    | "distinct_sources",
): string {
  if (!trials) return "Not reported";
  if (trials.status === "partial" || trials.counts_lower_bound === true)
    return "Not reported";
  return trialCount(trials[key]);
}

export function trialFamilyKeyNote(
  trials?: ObservationTrialFold | null,
): string {
  return trials?.family_key_is_label_only === true
    ? "Family key is a label only; not a multiple-testing adjustment."
    : "Family key scope not reported.";
}

export function trialReset(trials?: ObservationTrialFold | null): string {
  return trials?.reset === false
    ? "No"
    : trials?.reset === true
      ? "Reported reset"
      : "Not reported";
}

export function cohortStatus(value: unknown): string {
  const status =
    typeof value === "string"
      ? value
      : value && typeof value === "object" && !Array.isArray(value)
        ? (value as { status?: unknown }).status
        : null;
  switch (status) {
    case "runtime_matched_cohort_unknown":
      return "Runtime matched; cohort match unknown";
    case "runtime_changed":
      return "Runtime changed; cohort not proven";
    case "runtime_unknown":
      return "Runtime unknown; cohort not proven";
    default:
      return "Cohort unknown; not proven";
  }
}

export function money(value: unknown): string | null {
  if (typeof value === "number") {
    if (!Number.isFinite(value) || Math.abs(value) > 1e15) return null;
  } else if (typeof value === "string") {
    if (!/^-?\d{1,18}(?:\.\d{1,6})?$/.test(value)) return null;
    if (!Number.isFinite(Number(value))) return null;
  } else {
    return null;
  }
  return new Intl.NumberFormat("en-IN", {
    style: "currency",
    currency: "INR",
    maximumFractionDigits: 2,
  }).format(Number(value));
}

export function exitStatusLabel(value: unknown): string {
  return typeof value === "string"
    ? EXIT_STATUS[value] || "Not reported"
    : "Not reported";
}

function gapLabel(value: unknown): string | null {
  return typeof value === "string" && EPISODE_GAP_LABELS[value]
    ? EPISODE_GAP_LABELS[value]
    : null;
}

const CRITICAL_EPISODE_GAPS = new Set([
  "coverage_truncated",
  "result_tampered",
  "measurement_unbound",
  "measurement_unavailable",
  "unsupported_cross_session",
  "unsupported_session_scope",
  "incomplete_accounting",
  "residual_positions",
  "missing_marks",
  "unexecuted",
  "exit_gap",
  "entry_gap",
  "no_entry",
  "entry_quote_stale",
  "entry_quote_missing",
  "exit_quote_stale",
  "exit_quote_missing",
  "no_fabricated_exit_price",
  "sha_only_gap",
]);

function criticalEpisodeGap(
  measurement: EpisodeMeasurement | null | undefined,
  episode?: EpisodeRecord | null,
): string | null {
  const gaps: unknown[] = [];
  if (Array.isArray(episode?.maturation_gaps))
    gaps.push(...episode.maturation_gaps);
  if (Array.isArray(measurement?.gaps)) gaps.push(...measurement.gaps);
  const after = episode?.after_cost;
  if (after && typeof after === "object" && !Array.isArray(after)) {
    const afterGaps = (after as { gaps?: unknown }).gaps;
    if (Array.isArray(afterGaps)) gaps.push(...afterGaps);
  }
  for (const gap of gaps) {
    if (typeof gap === "string" && CRITICAL_EPISODE_GAPS.has(gap)) {
      return gapLabel(gap) || "Unresolved episode gap";
    }
  }
  return null;
}

export function episodeGapReason(
  measurement: EpisodeMeasurement | null | undefined,
  episode?: EpisodeRecord | null,
): string {
  const gaps: unknown[] = [];
  if (Array.isArray(episode?.maturation_gaps))
    gaps.push(...episode.maturation_gaps);
  const after = episode?.after_cost;
  if (after && typeof after === "object" && !Array.isArray(after)) {
    const afterGaps = (after as { gaps?: unknown }).gaps;
    if (Array.isArray(afterGaps)) gaps.push(...afterGaps);
  }
  for (const gap of gaps) {
    const label = gapLabel(gap);
    if (label) return label;
  }
  const exit = exitStatusLabel(episode?.exit_status);
  if (exit !== "Not reported" && exit !== "Executed") return exit;
  return "Reason not reported";
}

export function episodeEntryLabel(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  if (!measurement) return "Not reported";
  if (measurement.source_scope === "continuation_original_episode")
    return measurement.entry
      ? "Entry recorded in the original episode"
      : "No entry recorded in the original episode";
  const episode = measurement.episodes?.[0];
  if (!episode) return "Not reported";
  return episode.entry === true
    ? "Entry recorded"
    : episode.entry === false
      ? "No entry"
      : "Not reported";
}

export function episodePnl(
  measurement: EpisodeMeasurement | null | undefined,
): {
  available: boolean;
  value: string;
  reason: string;
} {
  if (!measurement)
    return {
      available: false,
      value: "Unavailable",
      reason: "Measurement not present",
    };
  if (measurement.source_scope === "continuation_original_episode") {
    if (!measurement.entry)
      return {
        available: false,
        value: "Unavailable",
        reason: "No entry recorded in the original episode",
      };
    if (measurement.exit_status !== "executed")
      return {
        available: false,
        value: "Unavailable",
        reason: exitStatusLabel(measurement.exit_status),
      };
    if (measurement.flat !== true)
      return {
        available: false,
        value: "Unavailable",
        reason: "Residual positions remain",
      };
    if (measurement.accounting_complete !== true)
      return {
        available: false,
        value: "Unavailable",
        reason: "Accounting incomplete",
      };
    const critical = criticalEpisodeGap(measurement, null);
    if (critical)
      return { available: false, value: "Unavailable", reason: critical };
    const pnl = money(measurement.net_pnl_if_flat);
    return pnl === null
      ? {
          available: false,
          value: "Unavailable",
          reason: "Net after-cost value not reported",
        }
      : { available: true, value: pnl, reason: "" };
  }
  const episode = measurement.episodes?.[0];
  if (!episode)
    return {
      available: false,
      value: "Unavailable",
      reason: "Episode record not reported",
    };
  if (episode.entry !== true)
    return {
      available: false,
      value: "Unavailable",
      reason: episodeGapReason(measurement, episode),
    };
  if (episode.exit_status !== "executed")
    return {
      available: false,
      value: "Unavailable",
      reason: exitStatusLabel(episode.exit_status),
    };
  const after = episode.after_cost;
  if (!after || typeof after !== "object" || Array.isArray(after))
    return {
      available: false,
      value: "Unavailable",
      reason: "After-cost accounting not reported",
    };
  const accounting = after as {
    flat?: unknown;
    accounting_complete?: unknown;
    net_pnl_if_flat?: unknown;
  };
  if (accounting.flat !== true)
    return {
      available: false,
      value: "Unavailable",
      reason: "Residual positions remain",
    };
  if (accounting.accounting_complete !== true)
    return {
      available: false,
      value: "Unavailable",
      reason: "Accounting incomplete",
    };
  const critical = criticalEpisodeGap(measurement, episode);
  if (critical)
    return { available: false, value: "Unavailable", reason: critical };
  const pnl = money(accounting.net_pnl_if_flat);
  return pnl === null
    ? {
        available: false,
        value: "Unavailable",
        reason: "Net after-cost value not reported",
      }
    : { available: true, value: pnl, reason: "" };
}

export function episodeCapital(
  measurement: EpisodeMeasurement | null | undefined,
): { value: string | null; assumption: boolean } {
  if (!measurement) return { value: null, assumption: false };
  if (measurement.source_scope === "continuation_original_episode") {
    return measurement.capital_assumption === true
      ? { value: money(measurement.capital), assumption: true }
      : { value: null, assumption: false };
  }
  const capital = measurement.capital;
  return capital?.research_capital_assumption === true &&
    capital.account_funds_claimed === false
    ? { value: money(capital.amount), assumption: true }
    : { value: null, assumption: false };
}

export function measurementState(
  value: unknown,
): "missing" | "unsupported" | "present" {
  if (value === null || value === undefined) return "missing";
  if (typeof value !== "object" || Array.isArray(value)) return "unsupported";
  const measurement = value as EpisodeMeasurement;
  if (measurement.version !== 1) return "unsupported";
  if (
    measurement.kind === "forward_observation_measurement" &&
    measurement.evaluation_scope === "independent_episodes"
  )
    return "present";
  if (measurement.source_scope === "continuation_original_episode")
    return "present";
  return "unsupported";
}

export function measurementScope(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  if (!measurement) return "Not reported";
  if (measurement.source_scope === "continuation_original_episode")
    return "Independent virtual episode continuation — not portfolio returns";
  return measurement.evaluation_scope === "independent_episodes"
    ? "Independent virtual episodes — not portfolio returns"
    : "Unsupported measurement scope";
}

export function measurementEpisodeCount(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  if (!measurement) return "Not reported";
  if (measurement.source_scope === "continuation_original_episode")
    return "1 original episode continuation";
  const countValue = numeric(measurement.episode_count);
  if (countValue === null) return "Not reported";
  return countValue === 1
    ? "1 independent virtual episode"
    : `${count(countValue)} episodes; this panel does not sum them`;
}

export function measurementHash(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  if (!measurement) return "Not reported";
  // The immutable episode projection hash only. A continuation receipt hash is
  // a different scope and must never be presented as this hash.
  return safeHash(measurement.projection_sha256);
}

export function continuationReceiptHash(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  return safeHash(measurement?.measurement_sha256);
}

export function continuationId(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  return safeHash(measurement?.continuation_id);
}

export function measurementOriginalStatus(
  item?: ResearchCandidate | null,
): string {
  return originalMeasurementStatusLabel(
    item?.original_measurement_status,
    item?.original_measurement_reason,
  );
}

export function liveContextHash(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  return safeHash(measurement?.live_context_sha256);
}

export function measurementTimeline(
  measurement: EpisodeMeasurement | null | undefined,
): [string, string][] {
  if (!measurement) return [];
  if (measurement.source_scope === "continuation_original_episode") {
    // Continuation records its own source clocks. Do not substitute the live
    // read time or an invented recorded_measurement_at field.
    return [
      [
        "Exit due source clock",
        datetime(validTimestampMs(measurement.exit_due_ms)),
      ],
      [
        "Exit deadline source clock",
        datetime(validTimestampMs(measurement.exit_deadline_ms)),
      ],
      ["Exit source clock", datetime(validTimestampMs(measurement.exit_at_ms))],
      [
        "Processed source clock",
        datetime(validTimestampMs(measurement.processed_at_ms)),
      ],
      [
        "Recorded source clock",
        datetime(validTimestampMs(measurement.recorded_at_ms)),
      ],
    ];
  }
  const authorship =
    measurement.authorship &&
    typeof measurement.authorship === "object" &&
    !Array.isArray(measurement.authorship)
      ? (measurement.authorship as Record<string, unknown>)
      : {};
  const episode = measurement.episodes?.[0];
  const items: [string, string][] = [
    [
      "Authoring source clock",
      datetime(validTimestampMs(authorship.authoring_at_ms)),
    ],
    [
      "Target frozen source clock",
      datetime(validTimestampMs(authorship.target_frozen_at_ms)),
    ],
    [
      "Score produced source clock",
      datetime(validTimestampMs(authorship.score_produced_at_ms)),
    ],
    [
      "Decision source clock",
      datetime(
        validTimestampMs(episode?.decision_at_ms ?? authorship.decision_at_ms),
      ),
    ],
    [
      "Exit due source clock",
      datetime(
        validTimestampMs(episode?.exit_due_ms ?? measurement.exit_due_ms),
      ),
    ],
    [
      "Exit processed source clock",
      datetime(validTimestampMs(measurement.exit_at_ms)),
    ],
    [
      "Measurement recorded",
      datetime(validTimestampMs(measurement.recorded_measurement_at_ms)),
    ],
    ["Live context read", datetime(validTimestampMs(measurement.as_of_ms))],
  ];
  return items;
}

export function replayAxes(
  measurement: EpisodeMeasurement | null | undefined,
): {
  status: string;
  copied: string;
  retrospective: string;
  prospective: string;
  noLookahead: string;
  holdout: string;
  edge: string;
  promotion: string;
} {
  const replay =
    measurement?.replay &&
    typeof measurement.replay === "object" &&
    !Array.isArray(measurement.replay)
      ? (measurement.replay as Record<string, unknown>)
      : null;
  const axis = (value: unknown) =>
    value === true ? "Yes" : value === false ? "No" : "Not reported";
  // The actual d946 source projection has three shapes: an explicit
  // `unavailable` reason, an authoritative `not_present` absence, or the
  // copied Task 8 axes object (`copied_unchanged:true`, no status). An unknown
  // status string is never relabelled as "Copied unchanged".
  const status =
    replay?.status === "unavailable"
      ? "Unavailable"
      : replay?.status === "not_present"
        ? "No Task 8 execution report"
        : replay?.copied_unchanged === true &&
            replay?.source === "task8_execution_report"
          ? "Copied unchanged from Task 8 report"
          : typeof replay?.status === "string"
            ? "Unsupported source status"
            : "Not reported";
  return {
    status,
    copied: replay?.copied_unchanged === true ? "Yes" : "Not reported",
    retrospective: axis(replay?.retrospective),
    prospective: axis(replay?.prospective),
    noLookahead: axis(replay?.no_lookahead_qualified),
    holdout: axis(replay?.holdout_qualified),
    edge: axis(replay?.edge_qualified),
    promotion: axis(replay?.promotion_authorized),
  };
}

export function actualScoring(
  measurement: EpisodeMeasurement | null | undefined,
): {
  status: string;
  qualified: string;
  score: string;
} {
  const actual =
    measurement?.actual_scoring &&
    typeof measurement.actual_scoring === "object" &&
    !Array.isArray(measurement.actual_scoring)
      ? (measurement.actual_scoring as Record<string, unknown>)
      : null;
  return {
    status:
      actual?.status === "scored"
        ? "Scored"
        : actual?.status === "not_scored"
          ? "Not scored"
          : actual?.status === "unavailable"
            ? "Unavailable"
            : actual?.status === "unknown"
              ? "Unknown"
              : "Not reported",
    qualified:
      actual?.qualified === true ? "Reported qualified" : "Not qualified",
    score:
      numeric(actual?.score) === null ? "Not reported" : count(actual?.score),
  };
}

export function qualificationLabel(
  measurement: EpisodeMeasurement | null | undefined,
): string {
  const qualification =
    measurement?.qualification &&
    typeof measurement.qualification === "object" &&
    !Array.isArray(measurement.qualification)
      ? (measurement.qualification as Record<string, unknown>)
      : null;
  if (!qualification) return "Not reported";
  if (
    qualification.qualified === true ||
    qualification.promotion_authorized === true ||
    qualification.activation_authorized === true
  )
    return "Reported qualified by policy; no operating qualification claim";
  if (
    qualification.qualified === false ||
    qualification.promotion_authorized === false ||
    qualification.activation_authorized === false
  )
    return "Not qualified; no promotion or activation";
  return "Not reported";
}

export function captureStatusLabel(value: unknown): string {
  return typeof value === "string"
    ? CAPTURE_STATUS[value] || "Not reported"
    : "Not reported";
}

export function captureCoverageLabel(value: unknown): string {
  return typeof value === "string"
    ? CAPTURE_COVERAGE[value] || "Not reported"
    : "Not reported";
}

export function captureEligibilityLabel(value: unknown): string {
  return typeof value === "string"
    ? CAPTURE_ELIGIBILITY[value] || "Not reported"
    : "Not reported";
}

export function captureEligibleForAttempt(value: unknown): string {
  return value === true ? "Yes" : value === false ? "No" : "Not reported";
}

export function captureReason(capture?: ObservationCapture | null): string {
  if (!capture) return "Not reported";
  if (capture.reason == null) {
    return capture.status === "journal_readable"
      ? "Not applicable"
      : "Reason not reported";
  }
  return researchReason(capture.reason, true);
}

export function captureBudget(capture?: ObservationCapture | null): string {
  if (!capture) return "Not reported";
  const used = numeric(capture.used);
  const max = numeric(capture.max_calls_per_session);
  if (used === null || max === null) return "Not reported";
  return `${count(used)} of ${count(max)} calls used`;
}

export function captureOperationalCaps(
  capture?: ObservationCapture | null,
): string {
  if (!capture) return "Not reported";
  const parts = [
    numeric(capture.max_keys) === null
      ? null
      : `${count(capture.max_keys)} keys`,
    numeric(capture.max_rows) === null
      ? null
      : `${count(capture.max_rows)} rows`,
    numeric(capture.min_interval_ms) === null
      ? null
      : `${count(capture.min_interval_ms)} ms interval`,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "Not reported";
}

export function observationAge(value: unknown): string {
  return ageLabel(value);
}

export function observationEvaluationScope(value: unknown): string {
  const measurement =
    value && typeof value === "object" && !Array.isArray(value)
      ? (value as { evaluation_scope?: unknown })
      : null;
  return measurement?.evaluation_scope === "independent_episodes"
    ? "Independent virtual episodes — not portfolio returns"
    : "Not reported";
}

export function observationCohortKey(value: unknown): string {
  const cohort =
    value && typeof value === "object" && !Array.isArray(value)
      ? (value as { cohort_key?: unknown })
      : null;
  return safeHash(cohort?.cohort_key);
}

export function validObservationTimestamp(value: unknown): number | null {
  return validTimestampMs(value);
}

export function researchPendingKind(
  value: unknown,
  pendingSource?: unknown,
): string {
  if (value) return pendingKind(value);
  return pendingSource === "missing" || pendingSource === "no_pending"
    ? "No pending turn reported"
    : "Unknown (source not authoritative)";
}

export function researchPendingSource(value: unknown): string {
  return pendingSourceLabel(value);
}

export function researchEvidenceId(value: unknown): string {
  return safeHash(value);
}

export function researchWorkspaceSnapshot(value: unknown): string {
  return safeHash(value);
}

export type MeasurementEntryState = "present" | "unsupported" | "unavailable";

export interface MeasurementEntry {
  item: ResearchCandidate;
  measurement: EpisodeMeasurement | null;
  state: MeasurementEntryState;
  reason: string;
}

export interface MeasurementWindow {
  entries: MeasurementEntry[];
  shown: number;
  total: number;
  displayLimit: number;
  truncatedByDisplay: boolean;
  sourceTruncated: boolean;
}

export function measurementEntryState(value: unknown): {
  state: MeasurementEntryState;
  reason: string;
} {
  if (value === null || value === undefined)
    return { state: "unsupported", reason: "Measurement not present" };
  if (typeof value !== "object" || Array.isArray(value))
    return { state: "unsupported", reason: "Measurement is not an object" };
  const measurement = value as EpisodeMeasurement;
  if (measurement.version !== 1) {
    const version =
      typeof measurement.version === "number" ||
      typeof measurement.version === "string"
        ? String(measurement.version)
        : "not reported";
    return {
      state: "unsupported",
      reason: `Measurement version ${version} is not supported`,
    };
  }
  if (measurement.source_scope === "continuation_original_episode")
    return { state: "present", reason: "" };
  if (
    measurement.kind === "forward_observation_measurement" &&
    measurement.evaluation_scope === "independent_episodes"
  )
    return { state: "present", reason: "" };
  return {
    state: "unavailable",
    reason: "Measurement shape is not recognized",
  };
}

export function measurementWindow(
  library?: {
    items?: ResearchCandidate[] | null;
    truncated?: boolean | null;
  } | null,
  displayLimit = 5,
): MeasurementWindow {
  const entries: MeasurementEntry[] = [];
  for (const item of library?.items || []) {
    // The canonical checked status is authoritative when present. A
    // `not_matured` item is the genuine "no measurement yet" and is not an
    // entry; an `unavailable` item is an explicit entry even when the
    // effective body is null, so a corrupt/stale record is never hidden.
    const status =
      typeof item.measurement_status === "string"
        ? item.measurement_status
        : null;
    if (status === "not_matured") continue;
    if (status === "unavailable") {
      entries.push({
        item,
        measurement: null,
        state: "unavailable",
        reason: researchReason(item.measurement_reason, true),
      });
      continue;
    }
    const value = item.observation_measurement;
    if (value == null) {
      if (item.observation_measurement_present === true) {
        entries.push({
          item,
          measurement: null,
          state: "unavailable",
          reason: "Measurement body is present but not readable",
        });
      } else if (status === "verified") {
        entries.push({
          item,
          measurement: null,
          state: "unavailable",
          reason: "Verified measurement body not present",
        });
      }
      continue;
    }
    const result = measurementEntryState(value);
    entries.push({
      item,
      measurement:
        result.state === "present" ? (value as EpisodeMeasurement) : null,
      state: result.state,
      reason: result.reason,
    });
  }
  const total = entries.length;
  return {
    entries: entries.slice(0, displayLimit),
    shown: Math.min(total, displayLimit),
    total,
    displayLimit,
    truncatedByDisplay: total > displayLimit,
    sourceTruncated: library?.truncated === true,
  };
}

export function measurementItems(
  library?: {
    items?: ResearchCandidate[] | null;
    truncated?: boolean | null;
  } | null,
): MeasurementEntry[] {
  return measurementWindow(library).entries;
}

export function milestoneWindow(
  lifecycle?: ObservationStatus["lifecycle"],
  displayLimit = 20,
): {
  rows: ObservationMilestone[];
  shown: number;
  total: number;
  displayLimit: number;
  truncatedByDisplay: boolean;
  sourceTruncated: boolean;
} {
  const rows = Array.isArray(lifecycle?.milestones) ? lifecycle.milestones : [];
  const total = rows.length;
  return {
    rows: rows.slice(0, displayLimit),
    shown: Math.min(total, displayLimit),
    total,
    displayLimit,
    truncatedByDisplay: total > displayLimit,
    sourceTruncated: lifecycle?.truncated === true,
  };
}

export function lifecycleStateLabel(value: unknown): string {
  return typeof value === "string" && LIFECYCLE_STATES.has(value)
    ? words(value)
    : "Unknown";
}

export function scientificStateLabel(value: unknown): string {
  return typeof value === "string"
    ? SCIENTIFIC_STATES[value] || "Unknown"
    : "Unknown";
}

export function measurementStatusLabel(value: unknown): string {
  return typeof value === "string"
    ? MEASUREMENT_STATUS_LABELS[value] || "Unknown"
    : "Not reported";
}

export function originalMeasurementStatusLabel(
  value: unknown,
  reason?: unknown,
): string {
  if (typeof value !== "string") return "Not reported";
  const label = ORIGINAL_MEASUREMENT_STATUS_LABELS[value];
  if (!label) return "Unknown";
  return value === "unavailable"
    ? `${label}: ${researchReason(reason, true)}`
    : label;
}

export function sourceRefStatusLabel(value: unknown): string {
  return typeof value === "string"
    ? SOURCE_REF_STATUS_LABELS[value] || "Unknown"
    : "Not reported";
}

export function continuationStateLabel(value: unknown): string {
  return typeof value === "string" && CONTINUATION_STATES.has(value)
    ? words(value)
    : "Unknown";
}

function gapLabels(source: unknown): string[] {
  if (!Array.isArray(source)) return [];
  const seen = new Set<string>();
  const labels: string[] = [];
  for (const gap of source) {
    if (typeof gap !== "string") continue;
    const label = EPISODE_GAP_LABELS[gap];
    if (!label || seen.has(label)) continue;
    seen.add(label);
    labels.push(label);
  }
  return labels;
}

export function milestoneGapReasons(
  milestone?: ObservationMilestone | null,
): string[] {
  return gapLabels(milestone?.gaps);
}

export function milestoneMeasurementGapReasons(
  milestone?: ObservationMilestone | null,
): string[] {
  return gapLabels(milestone?.measurement_gaps);
}

export function continuationQuoteHash(
  continuation: ResearchContinuation | EpisodeMeasurement | null | undefined,
  side: "entry" | "exit",
): string {
  const quote =
    side === "entry" ? continuation?.entry_quote : continuation?.exit_quote;
  return safeHash(quote?.archive_sha256);
}

export function continuationQuotePurpose(
  continuation: ResearchContinuation | EpisodeMeasurement | null | undefined,
  side: "entry" | "exit",
): string {
  const quote =
    side === "entry" ? continuation?.entry_quote : continuation?.exit_quote;
  return quote?.purpose === "research_observation"
    ? "Research observation"
    : "Not reported";
}

export function continuationQuoteClock(
  continuation: ResearchContinuation | EpisodeMeasurement | null | undefined,
  side: "entry" | "exit",
  clock: "source" | "receipt",
): string {
  const quote =
    side === "entry" ? continuation?.entry_quote : continuation?.exit_quote;
  return datetime(
    validTimestampMs(clock === "source" ? quote?.source_ms : quote?.receipt_ms),
  );
}

export function continuationLabel(
  continuation?: ResearchContinuation | null,
): string {
  if (!continuation) return "Not recorded";
  switch (continuation.state) {
    case "evaluated":
      return "Evaluated";
    case "unknown":
      return "Unknown";
    case "waiting":
      return "Waiting";
    case "pending_capture":
      return "Pending capture";
    case "unavailable":
      return "Unavailable";
    default:
      return "Not reported";
  }
}

export function continuationRecordingSession(
  continuation?: ResearchContinuation | null,
): string {
  return safeSession(continuation?.current_scope?.current_session);
}

export function continuationSourceSession(
  continuation?: ResearchContinuation | null,
): string {
  return safeSession(continuation?.source_session);
}

export function researchLibraryCount(
  library?: { items?: ResearchCandidate[] | null } | null,
): string {
  return Array.isArray(library?.items)
    ? count(library.items.length)
    : "Not reported";
}
