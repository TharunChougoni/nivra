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
  capabilities?: string[];
  archive_configured?: boolean;
  limits?: {
    sse_max_streams?: number;
    sse_heartbeat_ms?: number;
    longpoll_hold_ms?: number;
    series_max_points?: number;
    series_max_range_ms?: number;
    archive_minute_max_range_ms?: number;
    archive_daily_max_range_ms?: number;
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
/** One capital sleeve's derived state: what it was given and what it holds. */
export interface SleeveView {
  sleeve: "cash" | "options" | string;
  capital: Decimal;
  cash: Decimal;
  equity: Decimal;
  pnl: Decimal;
  open_positions: number;
}
export interface Allocation {
  at_ms: number;
  sleeve: string;
  amount: Decimal;
  reason: string;
}
export interface PortfolioResponse extends Envelope {
  capital?: Decimal | null;
  sleeves?: SleeveView[] | null;
  cash?: Decimal | null;
  equity?: Decimal | null;
  allocations?: Allocation[];
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
  feature_abi?: string | null;
  strategy_abi?: string | null;
  report_kind?: string | null;
  projection_sha256?: string | null;
}
export interface ResearchTasksStatus {
  status?: string | null;
}
/** The autonomous research loop as the controller last saw it. */
export interface ResearchLoopStatus {
  enabled?: boolean;
  state?: string | null;
  reason?: string | null;
  tape_state?: string | null;
  tape_session?: string | null;
  next_step_ms?: number | null;
  last_step_ms?: number | null;
  budget?: {
    variants_left?: number;
    turns_left?: number;
    calls_left?: number;
    usd_left?: number;
    variants?: number;
    turns?: number;
    usd?: number;
  } | null;
  campaign?: {
    name?: string;
    trials_used?: number | null;
    trial_cap?: number;
    development_from?: string;
    development_to?: string;
  } | null;
  latest?: {
    experiment_id?: number;
    ordinal?: number;
    status?: string;
    verdict?: string | null;
    reason?: string | null;
    net_pnl?: string | null;
    idea?: { family?: string; hypothesis?: string } | null;
  } | null;
  incumbents?: number;
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
/** One strategy action and what the execution gate did with it. */
export interface StrategyAction {
  at_ms?: number;
  observed_at_ms?: number;
  strategy_id?: string;
  index?: number;
  instrument_key?: string | null;
  side?: string | null;
  units?: number;
  outcome?: string;
}
export interface StrategyRefusal {
  at_ms?: number;
  action?: {
    side?: string;
    instrument_key?: string;
    units?: number;
    max_price?: string;
  } | null;
  reason?: string;
  reason_text?: string;
}
/** The paper controller's strategy executor. Absent on older controllers. */
export interface StrategyExecution {
  enabled?: boolean;
  state?: string;
  reason?: string | null;
  /** Shipped strategies configured to run while none is accepted, best first. */
  fallback?: string[];
  strategy?: {
    strategy_id?: string;
    basis?: "accepted" | "fallback";
    name?: string;
    author?: { lane?: string; model?: string | null } | null;
    summary?: {
      trades?: number;
      net_pnl?: Decimal;
      win_rate?: Decimal | null;
      worst_drawdown?: Decimal;
    } | null;
    tape_dates?: string[];
    registered_at_ms?: number;
  } | null;
  calls?: number;
  failures?: number;
  skipped?: number;
  last_call_at_ms?: number | null;
  last_failure?: string | null;
  actions?: StrategyAction[];
  refusals?: StrategyRefusal[];
  fills?: Fill[];
}
export interface SummaryResponse extends Envelope {
  revisions?: Revisions;
  strategy_execution?: StrategyExecution | null;
  research_library?: {
    status?: string;
    items?: ResearchCandidate[];
    truncated?: boolean;
    activation_authorized?: false;
  } | null;

  operational_health?: OperationalHealth | null;
  research_tasks?: ResearchTasksStatus | null;
  research_loop?: ResearchLoopStatus | null;
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
  sleeves?: SleeveView[] | null;
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
  day?: string;
  equity?: Decimal | null;
  cash?: Decimal;
  mark_basis?: string;
  open?: Decimal;
  high?: Decimal;
  low?: Decimal;
  close?: Decimal;
  tick_count?: number | null;
  price_kind?: string;
  bar_kind?: string;
  certified_ohlcv?: boolean;
  volume?: number | null;
}
export type SeriesKind =
  | "receipt-minute-bar"
  | "account-equity"
  | "archive-minute-bar"
  | "archive-daily-bar";
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
  volume?: number | null;
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
  verdict?: {
    escalate: boolean;
    reason: string;
    prospects?: GateProspect[];
  } | null;
}
/** One F&O contract the gate ranked for the decision lane; terms come from the option universe. */
export interface GateProspect {
  rank: number;
  instrument_key: string;
  idea: "buy_call" | "buy_put" | string;
  strike: string;
  expiry: string;
  lot_size: number;
  evidence: string;
  why: string;
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
