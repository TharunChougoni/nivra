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
  intraday: "Intraday bars",
  snapshot: "Quotes",
  quotes: "Quotes",
  bars: "Bars",
  verdict: "Gate verdict",
  portfolio: "Portfolio",
  positions: "Positions",
  features: "Features",
  decisions: "Past decisions",
  propose_plan: "Propose plan",
  plan: "Plan",
  web_search: "Web search",
  web_fetch: "Read page",
  news_lookup: "News",
  evidence: "Evidence",
  ranking: "Ranking",
  history: "History",
  scanner: "Scanner",
};

/** Why a lane turn started: kinds read as titles, reasons as phrases. */
export const TRIGGER_LABELS: Record<string, string> = {
  entry_candidate: "New pick",
  entered_top_k: "entered the top picks",
  schedule: "Scheduled",
  scheduled: "Scheduled",
  open: "market open",
  pre_close: "before close",
  hourly_top_k_changed: "top picks changed",
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
  agenda: "Agenda",
};

/** Why a lane turn failed. */
export const TURN_ERROR_LABELS: Record<string, string> = {
  agent_exited: "Stopped by restart",
  response_validation: "Invalid model reply",
  turn_timeout: "Timed out",
};

/** The strategy executor's states, reasons and action outcomes. */
export const STRATEGY_LABELS: Record<string, string> = {
  ready: "Running on each scan",
  calling: "Running a call",
  blocked: "Not running",
  no_active_strategy: "No accepted strategy is active",
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
