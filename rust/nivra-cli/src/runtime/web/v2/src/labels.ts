// Plain-language labels for values the backend records as code names. One
// lookup table plus a humanizing fallback, so a page never formats a code
// itself and an unknown code still reads as a sentence.
import { datetime, instrumentClassLabels, refusalLabels } from "./data";
import { instrumentLabel } from "./model";
import type { InstrumentRecord } from "./model";
import type { AgentTurn } from "./types";

/** Known codes → plain labels. Anything missing falls back to `plain`'s sentence case. */
export const LABELS: Record<string, string> = {
  paper_only: "Paper only",
  live_enabled: "Live trading",
  session_nse_bod: "NSE start-of-day file",
  nse_cm_security_sha256: "NSE security file",
  upstox_nse_bod_sha256: "Upstox NSE file",
  upstox_suspended_bod_sha256: "Upstox suspended list",
  NSE_EQ: "NSE cash",
  NSE_FO: "NSE F&O",
  NSE_INDEX: "NSE index",
  BSE_EQ: "BSE cash",
  BSE_FO: "BSE F&O",
  ok: "OK",
  sse: "SSE",
  http_503: "HTTP 503",
  read_failure: "Read failed",
  partial_read: "Partly read",
  insufficient_evidence: "Not enough evidence",
  gate_escalated: "Escalated",
  gate_declined: "Declined",
};

/** Lane tool names. */
export const TOOL_LABELS: Record<string, string> = {
  session: "Session",
  prospects: "Scanner picks",
  rank: "Rank universe",
  intraday: "Intraday bars",
  snapshot: "Quotes",
  quotes: "Quotes",
  verdict: "Gate verdict",
  portfolio: "Portfolio",
  features: "Features",
  decisions: "Past decisions",
  analysis: "Queue analysis",
  plan: "Plan",
  strategy_select: "Select strategy",
  research: "Ask research",
  research_status: "Research status",
  research_result: "Research result",
  research_cancel: "Cancel research",
  run: "Run script",
  backtest: "Run backtest",
  backtest_status: "Backtest status",
  backtest_result: "Backtest result",
  backtest_cancel: "Cancel backtest",
  list: "List files",
  read: "Read file",
  write: "Write file",
  install: "Install package",
  search: "Web search",
  fetch: "Read page",
  news: "News",
  web_search: "Web search",
  web_fetch: "Read page",
  news_lookup: "News",
  evidence: "Evidence",
  ranking: "Ranking",
};

/** Why a lane turn started: kinds read as titles, reasons as phrases. */
export const TRIGGER_LABELS: Record<string, string> = {
  entry_candidate: "New pick",
  entered_top_k: "entered the top picks",
  schedule: "Scheduled",
  scheduled: "Scheduled",
  open: "market open",
  pre_close: "before close",
  after_close: "After close",
  post_close: "after close",
  hourly_top_k_changed: "top picks changed",
  regime_changed: "regime changed",
  pre_open: "Before open",
  choose_strategy: "choosing today's strategy",
  strategy_search: "Strategy search",
  no_accepted_strategy: "no accepted strategy",
  fill: "Fill",
  fill_batch: "fills arrived",
  position_risk: "Position risk",
  near_stop: "near stop",
  near_target: "near target",
  score_below_exit: "score below exit",
  adverse_move: "adverse move",
  quiet: "Quiet",
  no_decision_turn: "no decision yet",
  drift: "Drift",
  hit_rate_drift: "hit rate drifting",
  seal: "Close",
  session_sealed: "session sealed",
  request: "Request",
  decision_request: "decision request",
  agenda: "Agenda",
};

/** Why a lane turn failed. */
export const TURN_ERROR_LABELS: Record<string, string> = {
  agent_exited: "Stopped by restart",
  response_validation: "Invalid model reply",
  turn_timeout: "Timed out",
};

/** The shipped strategies the executor can run as a fallback. */
export const REFERENCE_STRATEGY_LABELS: Record<string, string> = {
  opening_range_breakout: "Opening range breakout",
  vwap_reversion: "VWAP reversion",
  relative_strength: "Relative strength",
  bollinger_reversion: "Bollinger reversion",
  order_book_imbalance: "Order book imbalance",
  circuit_magnet: "Circuit magnet",
  volume_climax_reversal: "Volume climax reversal",
  vwap_trend: "VWAP trend",
};

/** The strategy executor's states, reasons and action outcomes. */
export const STRATEGY_LABELS: Record<string, string> = {
  ready: "Running on each scan",
  calling: "Running a call",
  blocked: "Not running",
  no_active_strategy: "No accepted strategy yet",
  strategy_lane_stood_down: "The decision lane chose to run no strategy",
  accepted: "Accepted by research",
  fallback: "Fallback: a shipped strategy that has not passed acceptance",
  strategy_engine_changed: "Accepted on a different engine build",
  strategy_adapter_changed: "Accepted with different execution code",
  strategy_library_unavailable: "The strategy library could not be read",
  strategy_exit: "The strategy crashed",
  strategy_wall_timeout: "The strategy ran past its time limit",
  strategy_output_limit: "The strategy printed too much",
  strategy_output_invalid: "The strategy printed something unreadable",
  strategy_output_non_finite:
    "The strategy printed a number that is not finite",
  strategy_output_schema: "The strategy's reply had the wrong shape",
  strategy_too_many_actions: "The strategy asked for too many actions",
  strategy_unknown_instrument:
    "The strategy named an instrument it was not shown",
  strategy_call_panicked: "The strategy call failed",
  strategy_sandbox_unavailable: "The sandbox was unavailable",
  filled: "Filled",
  exit_revised: "Exit updated",
  exit_revision_rejected: "Exit update refused",
  quote_unavailable: "Refused: no fresh quote",
  price_above_max: "Refused: the price was above the strategy's limit",
  action_cash: "Refused: not enough cash",
  action_sell_unheld: "Refused: nothing held to sell",
  action_sell_units: "Refused: more units than held",
  action_allowlist: "Refused: instrument not allowed",
  action_bounds: "Refused: the action was malformed",
  action_schema: "Refused: the action was malformed",
  action_zero_units: "Refused: the size came to zero units",
  action_time_exit_expired: "Refused: the exit time had passed",
  action_revise_unheld: "Refused: nothing held to update",
  action_strategy_owns_cash_entries:
    "Refused: active strategy owns cash entries",
  agent_paper_strategy_owns_cash_entries:
    "Refused: active strategy owns cash entries",
  strategy_owns_cash_entries: "Skipped: active strategy owns cash entries",
};

/** An action outcome in plain words; other gate refusals read "Refused: …". */
export function strategyOutcome(code: unknown): string {
  const text = String(code ?? "");
  if (Object.hasOwn(STRATEGY_LABELS, text)) return STRATEGY_LABELS[text];
  if (Object.hasOwn(refusalLabels, text))
    return `Refused: ${refusalLabels[text]}`;
  return plain(code);
}

export const sentenceCase = (text: string) =>
  text.charAt(0).toUpperCase() + text.slice(1);

/** The plain label for a code. Null and empty read "Not reported"; unknown codes get sentence case. */
export function plain(
  code: unknown,
  table: Record<string, string> = LABELS,
): string {
  if (code == null || code === "") return "Not reported";
  const text = String(code).trim();
  if (Object.hasOwn(table, text)) return table[text];
  // "PARTIAL" reads as "Partial"; "opening-range-breakout" as "Opening range breakout".
  const words = /^[A-Z]{5,}$/.test(text)
    ? text.toLowerCase()
    : /^[a-z0-9]+(-[a-z0-9]+)+$/.test(text)
      ? text.replaceAll("-", " ")
      : text;
  return sentenceCase(words.replaceAll("_", " ").replace(/\s+/g, " "));
}

/** Instrument class ("cash", "future") as a title; unknown classes read as sentence case. */
export function classLabel(kind?: string | null) {
  return kind ? plain(kind, instrumentClassLabels) : "Class not reported";
}

/** First 8 characters of a hash or id, with the full value kept for a tooltip. */
export function shortId(hash: unknown): { text: string; title?: string } {
  if (hash == null || hash === "") return { text: "Not reported" };
  const full = String(hash);
  return full.length > 8
    ? { text: full.slice(0, 8), title: full }
    : { text: full };
}

/** Model prose with snake_case code names spoken as words. Still plain text. */
export const spoken = (text: string) =>
  text.replace(/\b[a-z][a-z0-9]*(?:_[a-z0-9]+)+\b/g, (m) =>
    m.replaceAll("_", " "),
  );

/** A person-readable instrument name: the recorded record when known, else the key without its segment prefix. */
export function instrumentName(
  key: string | null | undefined,
  index?: Map<string, InstrumentRecord>,
): string {
  if (!key) return "Unknown instrument";
  const record = index?.get(key);
  if (record) return instrumentLabel(record).primary;
  const cut = key.indexOf("|");
  return cut >= 0 && cut < key.length - 1 ? key.slice(cut + 1) : key;
}

/** "No trade" or "2 trades proposed"; null when the plan does not say. */
export function tradesProposed(actions?: number | null): string | null {
  if (actions == null) return null;
  return actions
    ? `${actions} ${actions === 1 ? "trade" : "trades"} proposed`
    : "No trade";
}

/** One line for a lane turn: gate verdict, plan outcome or error. */
export function turnOutcome(t: AgentTurn): string {
  if (t.verdict)
    return (
      (t.verdict.escalate ? "Escalated" : "Declined") + " · " + t.verdict.reason
    );
  if (t.plan) {
    const { status, stance, actions } = t.plan;
    return [
      status === "validated" ? null : plain(status),
      stance && plain(stance),
      tradesProposed(actions),
    ]
      .filter(Boolean)
      .join(" · ");
  }
  return t.error
    ? plain(t.error, TURN_ERROR_LABELS)
    : t.status
      ? plain(t.status)
      : "—";
}

/** Market archive series labels, errors and range choices. */
export const ARCHIVE_LABELS: Record<string, string> = {
  archive_not_configured: "Market archive is not configured",
  archive_unavailable: "Market archive is unavailable",
  market_archive: "Market archive",
  archive_quote_minute: "Archive minute bars from quotes",
  archive_minute: "Archive minute bars",
  archive_daily: "Archive daily bars",
  archive_candle: "Archive candle",
  series_range_invalid: "Choose a valid date range",
  series_range_required: "Choose both start and end dates",
  series_range_too_large: "Selected date range is too wide",
  series_instrument_required: "Choose an instrument first",
  series_instrument_invalid: "Instrument selection is not valid",
};

export const ARCHIVE_RANGE_LABELS: Record<string, string> = {
  today: "Today (live)",
  "5d": "Past 5 days (1 min)",
  "7d": "Past 7 days (1 min)",
  "30d": "Past 30 days (daily)",
  "90d": "Past 90 days (daily)",
  custom: "Custom range",
};

/** State banner text for a controller whose loop stopped updating. */
export function stalledText(lastUpdateMs: number | null) {
  return {
    title: "The controller has stopped updating",
    body:
      lastUpdateMs === null
        ? "Its last update time is not reported."
        : `Its last update was at ${datetime(lastUpdateMs)}. The values below are from then.`,
  };
}
