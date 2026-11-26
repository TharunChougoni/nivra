// Wire projections from ../v1.rs: envelope(), meta(), summary(), portfolio(),
// fills(), plans(), scanner(), instruments(), sessions(), health(), alerts(),
// series(), agent() and poll_source(). Decimal amounts stay strings on
// the wire.
// Optional/null fields preserve unavailable and older retained projections.
export type Decimal = string | number;
export type Json =
  null | boolean | number | string | Json[] | { [key: string]: Json };
export interface Availability {
  unavailable?: boolean;
  reason?: string | null;
}
export interface Envelope extends Availability {
  v?: number;
  mode?: string;
  paper_only?: boolean;
  live_enabled?: boolean;
  broker?: { data?: string | null; orders?: string | null };
  account?: string | null;
  session?: string | null;
  binding?: string;
  as_of_ms?: number | null;
  execution_authority?: false;
}
export interface MetaResponse extends Envelope {
  version?: string;
  roles?: string[];
  capabilities?: string[];
  limits?: {
    sse_max_streams?: number;
    sse_heartbeat_ms?: number;
    longpoll_hold_ms?: number;
    series_max_points?: number;
    series_max_range_ms?: number;
    equity_max_points?: number;
    equity_sample_interval_ms?: number;
    price_batch_instruments?: number;
    price_batch_interval_ms?: number;
  };
}
export interface Instrument {
  key?: string;
  symbol?: string;
  kind?: string;
  tick?: Decimal;
  lot?: number;
  lot_size?: number;
  segment?: string | null;
  underlying?: string | null;
  expiry?: string | null;
  strike?: Decimal | null;
}
export interface InstrumentMargin {
  instrument_key: string;
  span_margin?: Decimal | null;
  exposure_margin?: Decimal | null;
  total_margin?: Decimal | null;
  display_only: true;
}
export interface Fill {
  instrument?: Instrument;
  buy?: boolean;
  units?: Decimal;
  price?: Decimal;
  fee?: Decimal | null;
  at?: number | string;
  decision_id?: string | null;
  expected_price?: Decimal;
  slippage_bps?: Decimal;
}
export interface Greeks {
  delta?: Decimal | null;
  gamma?: Decimal | null;
  theta?: Decimal | null;
  vega?: Decimal | null;
}
export interface Position {
  instrument?: Instrument;
  /** Only when the source recorded them; never computed here. */
  greeks?: Greeks | null;
  margin?: InstrumentMargin | null;
  units?: Decimal;
  entry?: Decimal;
  mark?: Decimal | null;
  marked_at?: number | string;
  decision_id?: string | null;
  exit_intent?: { target?: Decimal; stop?: Decimal };
}
export interface PortfolioResponse extends Envelope {
  capital?: Decimal | null;
  cash?: Decimal | null;
  equity?: Decimal | null;
  initial_allocation?: Decimal;
  high_water?: Decimal;
  stopped?: boolean | null;
  halted?: boolean | null;
  closed?: boolean | null;
  health?: string | null;
  positions?: Position[] | Record<string, Position>;
  fills?: Fill[];
  fills_count?: number;
  recorded_fees?: Decimal | null;
  model_led?: boolean | null;
  policy?: Json;
  costs?: Json;
  realised_net_pnl_by_fill?: Json;
  fee_policy?: Json;
}
export interface HistoryPreparation {
  status?: string;
  ready_instruments?: number;
  instruments?: number;
  target_session?: string;
  failure?: string | null;
}
/** One pi lane's readiness and daily budget, from `readiness.models.roles`. */
export interface LaneBudget {
  role: string;
  enabled?: boolean;
  model?: string | null;
  provider?: string | null;
  transport?: string | null;
  status?: string | null;
  reason?: string | null;
  daily_call_budget?: number | null;
  budget_unit?: string | null;
  daily_cost_usd?: number | null;
  current_usage?: number | null;
  current_cost_usd?: number | null;
  remaining_current?: number | null;
  remaining_cost_usd?: number | null;
}
export interface Readiness {
  status?: string | null;
  reason?: string | null;
  history?: HistoryPreparation | null;
  lifecycle?: { status?: string; reason?: string };
  checked_at_ms?: number;
  models?: { roles?: LaneBudget[] };
}
export interface MarketFeed {
  state?: string;
  mode?: string;
  last_tick_age_ms?: number;
  cached_keys?: number;
  reconnects?: number;
  fallback?: boolean | string;
  reason?: string | null;
}
export interface ModelAction {
  instrument_key?: string;
  side?: string;
  units?: Decimal;
  max_price?: Decimal;
  rationale?: string;
}
export interface Execution {
  decision_id?: string;
  index?: number;
  verdict?: string;
  reason?: string | null;
}
export interface PlanAction {
  index?: number;
  action?: ModelAction;
  units?: Decimal;
  reason?: string;
  execution?: Execution | null;
}
export interface Planner {
  status?: string | null;
  decision_id?: string | null;
  pending_attempt?: Json;
  pending_events?: number;
  budget_used?: Json;
  budgets?: Json;
}
export interface ResearchCandidate {
  candidate_id: string;
  parent_candidate_id?: string | null;
  session?: string;
  source_sha256?: string | null;
  input_sha256?: string | null;
  hypothesis?: string | null;
  state?: string;
  result_sha256?: string | null;
  scientific_state?: string;
  origin_session?: string | null;
  continued?: boolean | null;
  feature_abi?: string | null;
  strategy_abi?: string | null;
  report_kind?: string | null;
  observation_score?: Json;
  observation_measurement?: EpisodeMeasurement | null;
  /// Canonical checked state for the effective measurement: `verified`,
  /// `not_matured` or `unavailable`. `observation_measurement_present` is the
  /// storage-presence scalar and is authoritative over a null body.
  observation_measurement_present?: boolean | null;
  measurement_status?: string | null;
  measurement_reason?: string | null;
  /// The ORIGINAL stored body/status before continuation precedence. It is
  /// preserved separately and never relabelled as the effective state.
  original_observation_measurement?: EpisodeMeasurement | null;
  original_measurement_status?: string | null;
  original_measurement_reason?: string | null;
  continuation_status?: string | null;
  continuation_reason?: string | null;
  source_observation_ref?: Json;
  continuation?: ResearchContinuation | null;
  projection_sha256?: string | null;
}
export interface ResearchContinuation {
  version?: number | null;
  continuation_id?: string | null;
  state?: string | null;
  source_session?: string | null;
  source_task_id?: string | null;
  source_revision?: number | null;
  source_candidate_id?: string | null;
  source_lifecycle_candidate_id?: string | null;
  source_result_sha256?: string | null;
  source_score_sha256?: string | null;
  source_observation_sha256?: string | null;
  current_scope?: {
    version?: number | null;
    current_session?: string | null;
    account?: string | null;
    cohort_key?: string | null;
    runtime_hash?: string | null;
    source_ledger_spec_sha256?: string | null;
  } | null;
  exit_status?: string | null;
  exit_gap?: Json;
  exit_due_ms?: number | null;
  exit_deadline_ms?: number | null;
  exit_at_ms?: number | null;
  processed_at_ms?: number | null;
  recorded_at_ms?: number | null;
  measurement_sha256?: string | null;
  source_scope?: string | null;
  capital_assumption?: boolean | null;
  capital?: Decimal | null;
  entry?: boolean | null;
  entry_quote?: FullQuoteRef | null;
  exit_quote?: FullQuoteRef | null;
  gaps?: Json;
}
export interface FullQuoteRef {
  key?: string | null;
  source_ms?: number | null;
  receipt_ms?: number | null;
  event_at_ms?: number | null;
  available_at_ms?: number | null;
  bid?: Decimal | null;
  ask?: Decimal | null;
  bid_units?: number | null;
  ask_units?: number | null;
  lower_circuit_limit?: Decimal | null;
  upper_circuit_limit?: Decimal | null;
  volume?: number | null;
  purpose?: string | null;
  source_age_ms?: number | null;
  receipt_age_ms?: number | null;
  evidence_status?: string | null;
  archive_sha256?: string | null;
  symbol?: string | null;
  series?: string | null;
  tick?: string | null;
  universe_sha256?: string | null;
}
export interface EpisodeRecord {
  candidate_id?: string | null;
  key?: string | null;
  decision_at_ms?: number | null;
  exit_due_ms?: number | null;
  status?: string | null;
  entry?: boolean | null;
  entry_quote_sha256?: string | null;
  exit_quote_sha256?: string | null;
  entry_quote?: FullQuoteRef | null;
  exit_quote?: FullQuoteRef | null;
  exit_status?: string | null;
  after_cost?: Json;
  maturation_gaps?: Json;
  residual_positions?: Json;
  missing_marks?: Json;
  original_report_sha256?: string | null;
}
export interface EpisodeMeasurement {
  version?: number | null;
  kind?: string | null;
  candidate_id?: string | null;
  task_id?: string | null;
  revision?: number | null;
  evaluation_scope?: string | null;
  strategy_portfolio_claimed?: boolean | null;
  portfolio_return?: Decimal | null;
  aggregate_net_pnl?: Decimal | null;
  episode_count?: number | null;
  episodes?: EpisodeRecord[] | null;
  original_refs?: Json;
  authorship?: Json;
  replay?: Json;
  actual_scoring?: Json;
  counts?: Json;
  capital?: {
    research_capital_assumption?: boolean | null;
    account_funds_claimed?: boolean | null;
    amount?: Decimal | null;
    source?: string | null;
  } | null;
  qualification?: Json;
  recorded_measurement_at_ms?: number | null;
  projection_sha256?: string | null;
  projection_scope?: string | null;
  live_context_sha256?: string | null;
  cohort?: Json;
  trials?: ObservationTrialFold | null;
  baseline?: Json;
  live_context?: Json;
  as_of_ms?: number | null;
  source_scope?: string | null;
  continuation_id?: string | null;
  state?: string | null;
  source_session?: string | null;
  current_scope?: {
    version?: number | null;
    current_session?: string | null;
    account?: string | null;
    cohort_key?: string | null;
    runtime_hash?: string | null;
    source_ledger_spec_sha256?: string | null;
  } | null;
  capital_assumption?: boolean | null;
  entry?: boolean | Json | null;
  exit_status?: string | null;
  exit_due_ms?: number | null;
  exit_deadline_ms?: number | null;
  exit_at_ms?: number | null;
  processed_at_ms?: number | null;
  recorded_at_ms?: number | null;
  flat?: boolean | null;
  accounting_complete?: boolean | null;
  net_pnl_if_flat?: Decimal | null;
  measurement_sha256?: string | null;
  entry_quote?: FullQuoteRef | null;
  exit_quote?: FullQuoteRef | null;
  gaps?: Json;
}
export interface ObservationQueue {
  pending?: number | null;
  running?: number | null;
  unknown?: number | null;
  cancel_requested?: number | null;
  succeeded?: number | null;
  total?: number | null;
  truncated?: boolean | null;
  oldest_created_ms?: number | null;
  oldest_age_ms?: number | null;
  next_deadline_ms?: number | null;
  deadline_remaining_ms?: number | null;
}
export interface ObservationMilestone {
  candidate_id?: string | null;
  state?: string | null;
  original_recorded_state?: string | null;
  effective_state?: string | null;
  effective_scientific_state?: string | null;
  origin_session?: string | null;
  continued?: boolean | null;
  observation_scope?: string | null;
  status?: string | null;
  gaps?: Json;
  measurement_scope?: string | null;
  exit_status?: string | null;
  measurement_gaps?: Json;
  measurement_sha256?: string | null;
  measurement_status?: string | null;
  measurement_reason?: string | null;
  original_measurement_sha256?: string | null;
  original_exit_status?: string | null;
  original_measurement_status?: string | null;
  original_measurement_reason?: string | null;
  continuation_status?: string | null;
  continuation_reason?: string | null;
  source_observation_ref_status?: string | null;
  source_observation_ref_reason?: string | null;
  continuation_state?: string | null;
  continuation_recorded_in_session?: string | null;
  processed_at_ms?: number | null;
  exit_at_ms?: number | null;
}
export interface ObservationTrialFold {
  status?: string | null;
  reason?: string | null;
  counts_lower_bound?: boolean | null;
  scope?: string | null;
  gaps?: string[] | null;
  known_lower_bound_counts?: Record<string, number | null> | null;
  global_tested_trial_count?: number | null;
  admitted_trial_count?: number | null;
  executed_trial_count?: number | null;
  terminal_trial_count?: number | null;
  unknown_exposure_count?: number | null;
  distinct_terminal_tasks?: number | null;
  distinct_lifecycle_candidates?: number | null;
  distinct_sessions?: number | null;
  distinct_sources?: number | null;
  sessions?: string[] | null;
  family_key_is_label_only?: boolean | null;
  family_count_scope?: string | null;
  multiple_testing?: string | null;
  reset?: boolean | null;
}
export interface ObservationCapture {
  enabled?: boolean | null;
  status?: string | null;
  reason?: string | null;
  coverage?: string | null;
  coverage_label?: string | null;
  current_eligibility?: string | null;
  eligible_for_attempt?: boolean | null;
  receipts?: string | null;
  used?: number | null;
  remaining?: number | null;
  max_calls_per_session?: number | null;
  max_keys?: number | null;
  max_rows?: number | null;
  min_interval_ms?: number | null;
  last_capture_ms?: number | null;
  last_capture_age_ms?: number | null;
  provider_called?: boolean | null;
  state_written?: boolean | null;
  as_of_ms?: number | null;
  timeout_ms?: number | null;
}
export interface ObservationStatus {
  version?: number | null;
  status?: string | null;
  source?: string | null;
  as_of_ms?: number | null;
  queue?: ObservationQueue | null;
  lifecycle?: {
    status?: string | null;
    recorded?: number | null;
    matured?: number | null;
    truncated?: boolean | null;
    milestones?: ObservationMilestone[] | null;
  } | null;
  cohort?: Json;
  trials?: ObservationTrialFold | null;
  capture?: ObservationCapture | null;
  /// Native source-observation enumeration. The status/truncated/partial/
  /// scanned/limit/source-error flags are structured by the backend; the
  /// panel only labels them and never promotes a raw score+job hash to a ref.
  observation_refs?: Json[] | null;
  observation_refs_status?: string | null;
  observation_refs_truncated?: boolean | null;
  observation_refs_partial?: boolean | null;
  observation_refs_scanned?: number | null;
  observation_refs_limit?: number | null;
  observation_refs_reason?: string | null;
  observation_refs_source_error_reason?: string | null;
  measurement?: Json;
  financial_profit_claim?: boolean | null;
  activation_authorized?: false;
}
export interface ResearchTasksStatus {
  status?: string | null;
  observation?: ObservationStatus | null;
  observation_capture?: ObservationCapture | null;
  lifecycle?: Json;
}
export type RevisionName =
  | "portfolio"
  | "fills"
  | "plans"
  | "scanner"
  | "sessions"
  | "health"
  | "alerts"
  | "agent"
  | "agent_turns";
export type Revisions = Partial<Record<RevisionName, string>>;
export interface SummaryResponse extends Envelope {
  revisions?: Revisions;
  research_library?: {
    status?: string;
    items?: ResearchCandidate[];
    truncated?: boolean;
    activation_authorized?: false;
  } | null;

  operational_health?: OperationalHealth | null;
  research_tasks?: ResearchTasksStatus | null;
  instrument_display?: {
    status?: string;
    source_sha256?: string | null;
    received_at_ms?: number | null;
  } | null;
  status?: string | null;
  model_status?: string | null;
  entry_status?: string | null;
  model_led?: boolean | null;
  capital?: Decimal | null;
  cash?: Decimal | null;
  equity?: Decimal | null;
  counts?: {
    positions?: number;
    fills?: number;
    universe?: number;
    scanned?: number;
    pending?: number;
  };
  state?: {
    stopped?: boolean | null;
    halted?: boolean | null;
    closed?: boolean | null;
    health?: string | null;
    phase?: string | null;
    stop_classification?: string | null;
  };
  market_feed?: MarketFeed | null;
  readiness?: Readiness | null;
  planner?: Planner | null;
  oldest_scan_age_ms?: number | null;
  alerts?: Alert[];
  alert_count?: number;
  history_preparation?: HistoryPreparation | null;
}
export interface Page<T> extends Envelope {
  items?: T[];
  total?: number;
  next?: number | null;
}
export interface FillsResponse extends Page<Fill> {}
export interface PlansResponse extends Envelope {
  planner?: Planner | null;
  actions?: PlanAction[] | null;
  execution?: Execution[] | null;
  note?: string;
}
export interface StrategyFields {
  strategy_spec_id?: string | null;
  strategy_spec_version?: string | number | null;
  strategy_spec_hash?: string | null;
  strategy_scan_id?: string | null;
  strategy_score?: Decimal | null;
}
export interface ScannerRow extends StrategyFields {
  kind?: string | null;
  instrument?: Instrument;
  segment?: string | null;
  instrument_key?: string;
  symbol?: string;
  reason?: string;
  total?: number;
  diagnostic?: string;
  last_observed_ms?: number;
  latest_screening_price?: Decimal;
  latest_sampled_bid?: Decimal;
  sampled_price_ms?: number;
  sampled_bid_ms?: number;
  source_ms?: number;
  price_kind?: string;
  sample_change_bps?: Decimal;
  sample_count?: number;
  observed_ms?: number;
  screening_reason?: string;
}
export interface ScannerCoverage {
  market_segment?: string | null;
  instrument_kind?: string | null;
  display_only?: boolean;
  fresh?: number;
  stale?: number;
  unseen?: number;
  eligible?: number;
  presented?: number;
  universe?: number;
  oldest_fresh_age_ms?: number;
  max_scan_age_ms?: number;
  unavailable_count?: number;
  unavailable_truncated?: boolean;
}
export interface ScannerResponse extends Page<ScannerRow> {
  segment?: string;
  coverage?: ScannerCoverage;
  screen_reasons?: Record<string, number>;
  ranking?: Json;
  settings?: {
    series_max_points?: number;
    price_batch_instruments?: number;
    price_batch_interval_ms?: number;
  };
  selection?: Json;
  limitations?: Json;
}
export interface CatalogInstrument {
  instrument_key: string;
  symbol?: string | null;
  segment: string;
  series?: string | null;
  exchange?: string | null;
  kind?: string | null;
  instrument_type?: string | null;
  underlying_key?: string | null;
  underlying_symbol?: string | null;
  expiry_ms?: number | null;
  strike?: Decimal | null;
  lot_size?: Decimal | null;
  tick?: Decimal | null;
  tick_unit?: string | null;
  provider_tick?: Decimal | null;
  cash_execution_verified?: boolean;
  margin?: InstrumentMargin | null;
  margin_reason?: string | null;
  display_only?: boolean;
}
export interface InstrumentCatalogResponse extends Page<CatalogInstrument> {
  source?: string;
  scope?: string;
  segment?: string;
  series?: string;
  file_date?: string;
  received_at_ms?: number;
  source_hashes?: Record<string, string>;
  cash_source_hashes?: Record<string, string>;
  segments?: Record<string, number>;
  kinds?: Record<string, number>;
  rows_seen?: number;
  rejected_rows?: number;
  display_only?: boolean;
  coverage_kind?: string;
}
export interface ContractStrike {
  strike: Decimal;
  call?: CatalogInstrument | null;
  put?: CatalogInstrument | null;
  ambiguous_call?: boolean;
  ambiguous_put?: boolean;
}
export interface ContractChainResponse extends Page<ContractStrike> {
  source?: string;
  received_at_ms?: number;
  source_hashes?: Record<string, string>;
  underlying_key?: string;
  expiry_ms?: number;
  futures?: CatalogInstrument[];
  futures_total?: number;
  futures_truncated?: boolean;
  unavailable_strikes?: number;
  display_only?: boolean;
  note?: string;
}
export interface SessionRecord extends Availability {
  mode?: string | null;
  paper_only?: boolean | null;
  live_enabled?: boolean | null;
  id?: {
    broker?: string | null;
    account?: string | null;
    date?: string | null;
  };
  current?: boolean;
  status?: string | null;
  phase?: string | null;
  as_of_ms?: number | null;
  observed_at_ms?: number;
  capital?: Decimal | null;
  cash?: Decimal | null;
  portfolio?: PortfolioResponse;
  positions?: number;
  fills?: number;
  provenance?: string;
  trading_controls?: false;
  stale?: boolean;
}
export type SessionsResponse = Page<SessionRecord>;
export interface Alert {
  id?: string;
  severity?: string;
  message?: string;
  source?: string;
  observed_at_ms?: number;
  delivery?: string;
  execution_authority?: false;
}
export interface AlertsResponse extends Page<Alert> {}
export type LivenessStatus =
  "healthy" | "unknown" | "stale" | "future" | "malformed";
export interface ControllerLiveness {
  status?: LivenessStatus | string;
  last_completed_at_ms?: number | null;
  age_ms?: number | null;
  max_age_ms?: number | null;
  expected_deadline_ms?: number | null;
  loop_generation?: number | null;
}
export interface CalendarCoverage {
  source?: string | null;
  covered_from?: string | null;
  covered_to?: string | null;
  current_operational_date?: string | null;
  covered?: boolean | null;
  requested_session?: string | null;
  requested_session_covered?: boolean | null;
  historical_session?: string | null;
  historical_session_covered?: boolean | null;
  days_remaining?: number | null;
  expiring?: boolean | null;
  expired?: boolean | null;
  expiry_warning_days?: number | null;
  expiry_warning_basis?: string | null;
  update_prerequisite?: string | null;
}
export type AlertStoreStatus =
  "available" | "missing" | "malformed" | "unreadable" | "disabled";
export interface AlertScope {
  account?: string | null;
  session?: string | null;
  store_scope?: string | null;
}
export interface AlertRetainedWindow {
  start_ms?: number | null;
  end_ms?: number | null;
  scope?: string | null;
  max_retained_rows?: number | null;
  retained_delivery_rows?: number | null;
  rows_in_window?: number | null;
  truncation?: string | null;
  clock_anomaly?: boolean | null;
}
export interface AlertCounts {
  queued?: number | null;
  retrying?: number | null;
  transport_accepted?: number | null;
  transport_accepted_scope?: string | null;
  dead?: number | null;
  dead_definition?: string | null;
  dropped?: number | null;
  in_flight_or_transport_error?: number | null;
  in_flight_or_transport_error_definition?: string | null;
  outbox_scope?: string | null;
  outbox_max_rows?: number | null;
  queued_scope?: string | null;
  retrying_scope?: string | null;
  dead_scope?: string | null;
  in_flight_or_transport_error_scope?: string | null;
}
export interface AlertTransportStatus {
  status?: AlertStoreStatus | string;
  store?: string | null;
  counts_scope?: string | null;
  scope?: AlertScope | null;
  counts?: AlertCounts | null;
  retained_window?: AlertRetainedWindow | null;
  last_accepted_at_ms?: number | null;
  last_accepted_scope?: string | null;
  last_failure_at_ms?: number | null;
  last_failure_category?: string | null;
  last_failure_scope?: string | null;
  failure_marker_present?: boolean | null;
  failure_marker_definition?: string | null;
  retention_window_seconds?: number | null;
  timestamp_normalization?: string | null;
  dropped_source?: string | null;
  ack_meaning?: string | null;
  delivery_guarantee?: boolean | null;
}
export interface OperationalHealth {
  version?: number | null;
  captured_at_ms?: number | null;
  calendar?: CalendarCoverage | null;
  alert_transport?: AlertTransportStatus | null;
}
export interface HealthResponse extends SummaryResponse {
  version?: string;
  activity_attribution?: ActivityAttribution | null;
  controller_liveness?: ControllerLiveness | null;
  book?: {
    stopped?: boolean;
    halted?: boolean;
    closed?: boolean;
    health?: string;
    read_failure_reason?: string;
  };
  model_initialization?: Json;
  retention?: Json;
  independent_telemetry?: { status?: string; reason?: string };
}
export interface ActivityAttribution {
  session?: string;
  conversion?: {
    candidates_evaluated?: number;
    decisions_considered?: number;
    actions_proposed?: number;
    actions_validated?: number;
    fills_executed?: number;
    candidate_to_decision_rate?: number;
    decision_to_action_rate?: number;
    action_to_fill_rate?: number;
    overall_conversion_rate?: number;
  };
  abstentions?: {
    total_abstentions?: number;
    model_abstained?: number;
    risk_preflight_blocked?: number;
    freshness_expired?: number;
    spread_cost_excessive?: number;
    market_closed_or_halted?: number;
    universe_filter_excluded?: number;
    candidate_confidence_low?: number;
    duplicate_or_in_flight?: number;
    other?: number;
  };
  financials?: {
    realized_pnl?: Decimal;
    marked_pnl?: Decimal;
    gross_profit?: Decimal;
    total_fees?: Decimal;
    total_slippage?: Decimal;
    total_cost_drag?: Decimal;
    net_after_cost_pnl?: Decimal;
  };
  turnover?: {
    notional_traded?: Decimal;
    capital?: Decimal;
    turnover_ratio?: number;
    fee_drag_bps?: number;
    slippage_drag_bps?: number;
    total_cost_drag_bps?: number;
  };
  drawdown?: {
    high_water_mark?: Decimal;
    current_equity?: Decimal;
    current_drawdown?: Decimal;
    max_drawdown?: Decimal;
    max_drawdown_pct?: number;
    max_drawdown_duration_ms?: number;
  };
  drawdown_coverage?: {
    basis?: string;
    complete?: boolean;
    reason?: string;
  };
  latency?: {
    count?: number;
    min_ms?: number;
    max_ms?: number;
    mean_ms?: number;
    p50_ms?: number;
    p90_ms?: number;
    p95_ms?: number;
    p99_ms?: number;
  };
  experiment_attributions?: {
    source_hash?: string;
    spec_id?: string | null;
    selection_reason?: string | null;
    decisions_count?: number;
    fills_count?: number;
    notional_volume?: Decimal;
    realized_pnl?: Decimal;
    marked_pnl?: Decimal;
    fees_incurred?: Decimal;
    net_after_cost_pnl?: Decimal;
    trial_status?: string | null;
  }[];
}
export interface SeriesCoverage {
  from_ms?: number | null;
  to_ms?: number | null;
  interval_ms?: number;
  observed?: number;
  expected?: number;
  gap_count?: number;
  gaps?: {
    from_ms?: number;
    to_ms?: number;
    reason?: string;
    minutes?: number;
  }[];
  store_start_ms?: number | null;
  session_open_ms?: number | null;
  store_started_after_open?: boolean;
}
export interface SeriesRow {
  at?: number;
  equity?: Decimal | null;
  cash?: Decimal;
  mark_basis?: string;
  open?: Decimal;
  high?: Decimal;
  low?: Decimal;
  close?: Decimal;
  tick_count?: number;
  price_kind?: string;
  bar_kind?: string;
  certified_ohlcv?: boolean;
  volume?: null;
}
export type SeriesKind = "receipt-minute-bar" | "account-equity";
export interface SeriesResponse extends Envelope {
  kind?: SeriesKind;
  limit?: number;
  range?: { from?: number | null; to?: number | null };
  instrument_key?: string;
  points?: SeriesRow[];
  watermark_ms?: number | null;
  coverage?: SeriesCoverage | null;
  source?: string;
  mark_basis?: string;
  note?: string;
  next?: number | null;
  truncated?: boolean;
}
export interface Point {
  at: number;
  value: number | null;
  open?: number | null;
  high?: number | null;
  low?: number | null;
  close?: number | null;
  tickCount?: number | null;
  priceKind?: string | null;
  barKind?: string | null;
  certified?: boolean;
  volume?: null;
  source?: string | number | null;
  kind?: string;
  cash?: number | null;
  markBasis?: string | null;
  carried?: boolean;
  gap?: boolean;
  streamGap?: boolean;
  gapTo?: number;
  reason?: string;
  isolated?: boolean;
}
export interface PriceItem {
  instrument_key?: string;
  receipt_ms?: number;
  last_price?: Decimal;
  price_kind?: string;
}
export interface StreamEvent {
  kind: string;
  revisions?: Revisions;
  /** price_batch: a full snapshot (connect or missed batch) vs only moved instruments. */
  full?: boolean;
  price_kind?: string;
  event?: string;
  seq?: number;
  epoch?: string | number;
  gap?: boolean;
  next?: number;
  batch_seq?: number;
  session?: string;
  items?: PriceItem[];
  archive_run?: string;
  run?: string;
  at_ms?: number;
  text?: string;
}
export interface PollResponse {
  epoch?: string | number;
  gap?: boolean;
  next?: number;
  events?: StreamEvent[];
}
export interface ReplayInput {
  manifest: string;
  events: string;
}
export interface ReplayExample {
  manifest: { universe?: Instrument[] };
  events: string;
  label?: string;
}
export interface ReplayResponse {
  report?: {
    realized_pnl_if_flat?: Decimal;
    maximum_drawdown?: Decimal;
    fees?: Decimal;
    events?: number;
    portfolio?: PortfolioResponse;
    session?: string;
    receipt_clock?: string;
    limitations?: string[];
  };
  equity_curve?: { at: number; equity: Decimal }[];
  engine_hash?: string;
  manifest_hash?: string;
  data_hash?: string;
}
export interface ReplayPayload extends ReplayInput {
  synthetic?: boolean;
  label?: string;
}
export interface ReplayResult extends ReplayResponse {
  synthetic?: boolean;
  label?: string;
}
export type Query = Record<
  string,
  string | number | boolean | null | undefined
>;
export type Fetch = (input: string, init?: RequestInit) => Promise<Response>;
export type LoadState =
  "idle" | "loading" | "ready" | "error" | "unavailable" | "cached";
export type Connection = "locked" | "live" | "polling" | "stale";
export interface ChartSeries {
  price: Point[];
  equity: Point[];
  priceState: LoadState;
  equityState: LoadState;
  priceCached: boolean;
  equityCached: boolean;
  priceCoverage: SeriesCoverage | null;
  equityCoverage: SeriesCoverage | null;
  priceSource: string | null;
  equitySource: string | null;
  priceNote: string | null;
  equityNote: string | null;
  priceWatermark: number | null;
  equityWatermark: number | null;
}
export interface ProjectionData {
  summary: SummaryResponse | null;
  portfolio: PortfolioResponse | null;
  fills: FillsResponse | null;
  plans: PlansResponse | null;
  scanner: ScannerResponse | null;
  sessions: SessionsResponse | null;
}
export type ProjectionName = keyof ProjectionData;
/** Every v1 projection the workspace reads, in display order. */
export const PROJECTION_NAMES = [
  "summary",
  "portfolio",
  "fills",
  "plans",
  "scanner",
  "sessions",
] as const satisfies readonly ProjectionName[];
type ListedProjection = (typeof PROJECTION_NAMES)[number];
/** Fails the build when `ProjectionData` gains a name this list does not cover. */
const projectionsCovered: Exclude<
  ProjectionName,
  ListedProjection
> extends never
  ? true
  : never = true;
export type ProjectionStates = {
  [K in ProjectionName as `${K}State`]: LoadState;
};
export interface WorkspaceState extends ProjectionData, ProjectionStates {
  token: string;
  /** pi lane journal: display evidence, never an order authority. */
  agent: AgentResponse | null;
  agentState: LoadState;
  /** Latest receipt price per instrument from `price_batch` frames. */
  quotes: Record<string, { at: number; price: number }>;
  meta: MetaResponse | null;
  snapshotState: LoadState;
  issues: { source: string; state: LoadState; reason: string }[];
  received: number | null;
  connection: Connection;
  error: string;
  session: string;
  points: Point[];
  equityPoints: Point[];
  series: ChartSeries;
  priceBatchSeq: number;
  watch: string;
  events: StreamEvent[];
  revision: number;
  /**
   * Set when the summary announcements stopped and a direct read found the
   * controller loop older than its liveness bound; holds its last update time.
   */
  stalled: { lastUpdateMs: number | null } | null;
}
export interface ProjectionSource {
  summary?: SummaryResponse | null;
  portfolio?: PortfolioResponse | null;
  fills?: FillsResponse | null;
  fillsState?: LoadState;
  plans?: PlansResponse | null;
  plansState?: LoadState;
  scanner?: ScannerResponse | null;
  sessions?: SessionsResponse | SessionRecord[] | null;
}
export interface StrategyReference extends StrategyFields {
  key: string;
  instruments: (string | undefined)[];
  scores: Decimal[];
}
export interface ToolCallRecord {
  v?: number;
  at_ms?: number;
  session?: string;
  run?: string;
  turn?: number;
  call?: string;
  tool?: string;
  provider?: string;
  query?: string | null;
  url?: string | null;
  status?: string;
  reason?: string | null;
  latency_ms?: number;
  bytes?: number;
  results?: number;
  execution_authorized?: false;
}
export interface ToolsResponse extends Availability {
  v?: number;
  session?: string;
  calls?: ToolCallRecord[];
  total?: number;
  next?: string | null;
  execution_authorized?: false;
}
export interface AgentUsage {
  input?: number;
  output?: number;
  cache_read?: number;
  cost_usd?: number;
  calls?: number;
}
export interface AgentTurn {
  turn_id: string;
  lane: "decision" | "research" | "review" | "gate" | string;
  at_ms?: number;
  status?: string;
  error?: string | null;
  provider?: string | null;
  model?: string | null;
  elapsed_ms?: number;
  usage?: AgentUsage | null;
  fallback?: { to?: { id?: string }; reason?: string } | null;
  trigger?: {
    kind?: string | null;
    reason?: string | null;
    instruments?: string[];
  };
  tool_calls?: { name: string; ok: boolean; error?: string | null }[] | null;
  plan?: {
    status?: string;
    stance?: string | null;
    actions?: number;
    error?: string | null;
  } | null;
  verdict?: { escalate: boolean; reason: string } | null;
}
export interface AgentResponse extends Availability {
  v?: number;
  turns?: AgentTurn[];
  total?: number;
  next?: string | null;
  triggers?: Record<string, number>;
  /** The journal is larger than the page returned; totals are then not complete. */
  truncated?: boolean;
  rows_not_loaded?: { turns?: number; triggers?: number } | null;
  usage?: { date?: string; lanes?: Record<string, AgentUsage> } | null;
  execution_authorized?: false;
}
/** Text the API may have cut at its byte cap; `truncated` says it did. */
export interface TranscriptText {
  text: string;
  truncated: boolean;
}
export interface TranscriptSession {
  id: string;
  file?: string;
  started_ms?: number;
  provider?: string | null;
  model?: string | null;
  thinking?: string | null;
}
export type TranscriptItem =
  | { kind: "thinking"; text: string; truncated?: boolean }
  | { kind: "text"; text: string; truncated?: boolean }
  | {
      kind: "tool";
      id: string;
      name: string;
      args: TranscriptText;
      /** Null until the tool result arrives. */
      result: TranscriptText | null;
      is_error: boolean | null;
      duration_ms: number | null;
    }
  | { kind: "error"; text: string }
  | { kind: "note"; text: string };
export interface TranscriptExchange {
  session: string;
  at_ms: number;
  ended_ms: number | null;
  /** False while the model is still working, or when the exchange was cut off. */
  complete: boolean;
  brief: { text: string; bytes: number; truncated: boolean };
  items: TranscriptItem[];
  turn: AgentTurn | null;
}
/** `GET /api/v1/agent/transcript`: one lane's pi exchanges, newest first. */
export interface AgentTranscript extends Availability {
  v?: number;
  lane?: string;
  /** Oldest first, one per pi session file read. */
  sessions?: TranscriptSession[];
  exchanges?: TranscriptExchange[];
  total?: number;
  next?: string | null;
  truncated?: boolean;
  execution_authorized?: false;
}
export function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : "request_failed";
}
