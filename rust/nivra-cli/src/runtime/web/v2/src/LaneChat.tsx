import type {
  TranscriptExchange,
  TranscriptItem,
  TranscriptSession,
  TranscriptText,
} from "./types";
import React, { useLayoutEffect, useRef, useState } from "react";
import { Brain, ChevronRight, FileText } from "lucide-react";
import { clock, datetime, duration, numeric, usd } from "./data";
import {
  TOOL_LABELS,
  TRIGGER_LABELS,
  TURN_ERROR_LABELS,
  instrumentName,
  plain,
  sentenceCase,
  spoken,
  tradesProposed,
} from "./labels";
import { turnFailed } from "./model";
import type { InstrumentRecord } from "./model";
import { Tag } from "./ui";

type Index = Map<string, InstrumentRecord>;

/** Tool calls shown before the middle of a long exchange folds away. */
const TOOL_HEAD = 3,
  TOOL_TAIL = 3;
const firstLine = (text: string) =>
  text.trim().split("\n", 1)[0] || "(no text)";
const preview = (text: string, max = 60) => {
  const flat = text.replace(/\s+/g, " ").trim();
  return flat.length > max ? flat.slice(0, max) + "…" : flat;
};
/** Pretty JSON when the text is complete JSON, the raw text otherwise. */
function pretty(t: TranscriptText) {
  if (t.truncated) return t.text;
  try {
    return JSON.stringify(JSON.parse(t.text), null, 2);
  } catch {
    return t.text;
  }
}
/** 5600 → "5.6k"; small counts stay whole. */
function compact(n: number) {
  if (n < 1000) return String(Math.round(n));
  const [value, unit] = n < 1e6 ? [n / 1e3, "k"] : [n / 1e6, "M"];
  return value.toFixed(value < 10 ? 1 : 0).replace(/\.0$/, "") + unit;
}
/** Tool arguments as short plain values: "INE397D01024" or "limit 20 · sort scanner". */
function argsPreview(args: TranscriptText, index?: Index) {
  if (args.truncated) return "";
  let parsed: unknown;
  try {
    parsed = JSON.parse(args.text);
  } catch {
    return "";
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed))
    return "";
  const value = (v: unknown): string | null => {
    if (typeof v === "string")
      return v.includes("|") ? instrumentName(v, index) : v;
    if (typeof v === "number") return String(v);
    if (typeof v === "boolean") return v ? "yes" : "no";
    if (Array.isArray(v)) {
      const items = v.map(value).filter((x): x is string => x !== null);
      return items.length
        ? items.slice(0, 3).join(", ") +
            (items.length > 3 ? ` +${items.length - 3}` : "")
        : null;
    }
    return null;
  };
  const entries = Object.entries(parsed).flatMap(([key, v]) => {
    const shown = value(v);
    return shown === null ? [] : [[key, shown] as const];
  });
  if (entries.length === 1) return entries[0][1];
  return entries
    .map(([key, shown]) => `${plain(key).toLowerCase()} ${shown}`)
    .join(" · ");
}

function Block({ label, value }: { label: string; value: TranscriptText }) {
  return (
    <div className="chat-block">
      <span className="chat-block-label">
        {label}
        {value.truncated && <Tag tone="warn">Cut short</Tag>}
      </span>
      <pre>{value.text ? pretty(value) : "(empty)"}</pre>
    </div>
  );
}

function ThinkingLine({
  text,
  truncated,
}: {
  text: string;
  truncated?: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="chat-item">
      <button
        className="chat-line"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        <ChevronRight size={12} className="chat-chevron" aria-hidden="true" />
        <Brain size={12} aria-hidden="true" />
        <span className="chat-line-text muted">{spoken(firstLine(text))}</span>
      </button>
      {open && (
        <div className="chat-detail">
          <pre>{text}</pre>
          {truncated && <Tag tone="warn">Cut short</Tag>}
        </div>
      )}
    </div>
  );
}

function ToolLine({
  tool,
  live,
  index,
}: {
  tool: Extract<TranscriptItem, { kind: "tool" }>;
  live: boolean;
  index?: Index;
}) {
  const [open, setOpen] = useState(false);
  const status: { label: string; tone?: "pos" | "neg" | "warn" } | null =
    tool.result
      ? tool.is_error
        ? { label: "Failed", tone: "neg" }
        : null
      : live
        ? { label: "Running" }
        : { label: "No result", tone: "warn" };
  return (
    <div className="chat-item">
      <button
        className="chat-line"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        <ChevronRight size={12} className="chat-chevron" aria-hidden="true" />
        <strong title={tool.name}>{plain(tool.name, TOOL_LABELS)}</strong>
        <span className="chat-line-text muted">
          {preview(argsPreview(tool.args, index))}
        </span>
        {status && <Tag tone={status.tone}>{status.label}</Tag>}
        <span className="chat-duration num muted">
          {duration(tool.duration_ms)}
        </span>
      </button>
      {open && (
        <div className="chat-detail">
          <Block label="Arguments" value={tool.args} />
          {tool.result ? (
            <Block label="Result" value={tool.result} />
          ) : (
            <p className="muted">No result yet.</p>
          )}
        </div>
      )}
    </div>
  );
}

/** The lane's own trigger as a short sentence: named instruments, else why the turn ran. */
function triggerBody(ex: TranscriptExchange, index?: Index) {
  const trigger = ex.turn?.trigger;
  const names = (trigger?.instruments ?? []).map((k) =>
    instrumentName(k, index),
  );
  if (!names.length) {
    const why = trigger?.reason || trigger?.kind;
    return why
      ? sentenceCase(plain(why, TRIGGER_LABELS))
      : "No trigger recorded";
  }
  const shown = names.slice(0, 6).join(", ");
  return `Instruments: ${shown}${names.length > 6 ? ` and ${names.length - 6} more` : ""}`;
}

function UserBubble({ ex, index }: { ex: TranscriptExchange; index?: Index }) {
  const [open, setOpen] = useState(false);
  const trigger = ex.turn?.trigger;
  const meta = [
    clock(ex.at_ms) + " IST",
    trigger?.kind ? plain(trigger.kind, TRIGGER_LABELS) : null,
    trigger?.reason && trigger.instruments?.length
      ? plain(trigger.reason, TRIGGER_LABELS)
      : null,
  ].filter(Boolean);
  return (
    <div className={open ? "chat-user open" : "chat-user"}>
      <p className="chat-bubble">{triggerBody(ex, index)}</p>
      <div className="chat-user-meta">
        <span>{meta.join(" · ")}</span>
        <button
          className="chat-icon"
          aria-expanded={open}
          aria-label={open ? "Hide full brief" : "Show full brief"}
          title={open ? "Hide full brief" : "Show full brief"}
          onClick={() => setOpen(!open)}
        >
          <FileText size={13} aria-hidden="true" />
        </button>
      </div>
      {open && (
        <>
          <pre>{ex.brief.text || "(empty)"}</pre>
          {ex.brief.truncated && (
            <p className="chat-note">Brief cut short. Showing the start.</p>
          )}
        </>
      )}
    </div>
  );
}

/** The chips that close an exchange; the turn row decides the outcome. */
function Outcome({
  ex,
  hasLater,
}: {
  ex: TranscriptExchange;
  hasLater: boolean;
}) {
  const turn = ex.turn,
    answered = ex.items.some((i) => i.kind === "text");
  const chips: React.ReactNode[] = [];
  if (turn?.error)
    chips.push(
      <Tag key="error" tone="neg" title={turn.error}>
        {plain(turn.error, TURN_ERROR_LABELS)}
      </Tag>,
    );
  else if (turn && turnFailed(turn))
    chips.push(
      <Tag key="status" tone="neg">
        {plain(turn.status)}
      </Tag>,
    );
  else if (!ex.complete)
    chips.push(
      <Tag key="open" tone={hasLater ? "warn" : undefined}>
        {hasLater ? "Interrupted" : "In progress"}
      </Tag>,
    );
  if (turn?.verdict) {
    chips.push(
      <Tag key="verdict" tone={turn.verdict.escalate ? "pos" : "warn"}>
        {turn.verdict.escalate ? "Escalated" : "Declined"}
      </Tag>,
    );
    if (!answered) chips.push(<span key="reason">{turn.verdict.reason}</span>);
  }
  if (turn?.plan) {
    const plan = turn.plan,
      validated = plan.status === "validated";
    chips.push(
      <Tag key="plan" tone={validated ? "pos" : "warn"}>
        {validated
          ? (tradesProposed(plan.actions) ?? "Validated")
          : plain(plan.status)}
      </Tag>,
    );
    const detail = [!answered && plan.stance && plain(plan.stance), plan.error]
      .filter(Boolean)
      .join(" · ");
    if (detail) chips.push(<span key="stance">{detail}</span>);
  }
  return chips.length ? <div className="chat-chips">{chips}</div> : null;
}

function Footer({ ex }: { ex: TranscriptExchange }) {
  const turn = ex.turn,
    elapsed =
      numeric(turn?.elapsed_ms) ??
      (ex.ended_ms !== null ? ex.ended_ms - ex.at_ms : null),
    usage = turn?.usage;
  const parts = [
    elapsed !== null && duration(elapsed),
    usage &&
      numeric(usage.input) !== null &&
      numeric(usage.output) !== null &&
      `${compact(Number(usage.input) + Number(usage.output))} tokens`,
    usage && numeric(usage.cost_usd) !== null && usd(usage.cost_usd),
  ].filter(Boolean);
  if (!parts.length && !turn?.fallback) return null;
  return (
    <div className="chat-meta">
      {parts.join(" · ")}
      {turn?.fallback && (
        <>
          {parts.length > 0 && " · "}
          <Tag tone="warn" title={turn.fallback.reason}>
            Fallback to {turn.fallback.to?.id ?? "unknown"}
          </Tag>
        </>
      )}
    </div>
  );
}

function Answer({
  items,
  live,
  index,
}: {
  items: TranscriptItem[];
  live: boolean;
  index?: Index;
}) {
  const [all, setAll] = useState(false);
  const tools = items.filter((i) => i.kind === "tool");
  // Fold the middle of a long run of tool calls; a failed call always stays visible.
  const folded = new Set<TranscriptItem>(
    all || tools.length <= TOOL_HEAD + TOOL_TAIL
      ? []
      : tools
          .slice(TOOL_HEAD, tools.length - TOOL_TAIL)
          .filter((t) => !(t.kind === "tool" && t.is_error)),
  );
  const out: React.ReactNode[] = [];
  let hidden = 0;
  const flush = (key: number) => {
    if (!hidden) return;
    out.push(
      <button
        key={`more-${key}`}
        className="chat-line chat-more"
        onClick={() => setAll(true)}
      >
        <ChevronRight size={12} className="chat-chevron" aria-hidden="true" />
        {hidden} more tool calls
      </button>,
    );
    hidden = 0;
  };
  items.forEach((item, i) => {
    if (folded.has(item)) {
      hidden++;
      return;
    }
    flush(i);
    switch (item.kind) {
      case "thinking":
        out.push(<ThinkingLine key={i} {...item} />);
        break;
      case "tool":
        out.push(<ToolLine key={i} tool={item} live={live} index={index} />);
        break;
      case "text":
        out.push(
          <p
            key={i}
            className="chat-answer"
            title={spoken(item.text) === item.text ? undefined : item.text}
          >
            {spoken(item.text)}
            {item.truncated && <Tag tone="warn">Cut short</Tag>}
          </p>,
        );
        break;
      case "error":
        out.push(
          <p key={i} className="chat-error" role="alert">
            {spoken(item.text)}
          </p>,
        );
        break;
      case "note":
        out.push(
          <p key={i} className="chat-note">
            {spoken(item.text)}
          </p>,
        );
    }
  });
  flush(items.length);
  return <>{out}</>;
}

function Exchange({
  ex,
  hasLater,
  index,
}: {
  ex: TranscriptExchange;
  hasLater: boolean;
  index?: Index;
}) {
  return (
    <article className="chat-exchange">
      <UserBubble ex={ex} index={index} />
      <div className="chat-assistant">
        <Answer
          items={ex.items}
          live={!ex.complete && !hasLater}
          index={index}
        />
        <Outcome ex={ex} hasLater={hasLater} />
        <Footer ex={ex} />
      </div>
    </article>
  );
}

function SessionDivider({ session }: { session?: TranscriptSession }) {
  const bits = [
    "New session",
    session?.model,
    session?.thinking && `${session.thinking} thinking`,
    session?.started_ms != null && datetime(session.started_ms),
  ].filter(Boolean);
  return (
    <div className="chat-session" role="separator">
      <span>{bits.join(" · ")}</span>
    </div>
  );
}

/** A lane's exchanges as a chat. `exchanges` arrive newest first and read oldest first. */
export function LaneThread({
  exchanges,
  sessions,
  index,
  hasMore,
  loadingMore,
  moreFailed,
  onLoadMore,
}: {
  exchanges: TranscriptExchange[];
  sessions: Map<string, TranscriptSession>;
  index?: Index;
  hasMore: boolean;
  loadingMore: boolean;
  moreFailed: boolean;
  onLoadMore: () => void;
}) {
  const box = useRef<HTMLDivElement>(null),
    stick = useRef(true),
    before = useRef<{ height: number; top: number } | null>(null);
  const ordered = [...exchanges].reverse();
  // Newest sits at the bottom. Keep following it unless the reader scrolled up,
  // and hold the reader's place when older exchanges are prepended.
  useLayoutEffect(() => {
    const el = box.current;
    if (!el) return;
    if (before.current) {
      el.scrollTop =
        el.scrollHeight - before.current.height + before.current.top;
      before.current = null;
    } else if (stick.current) el.scrollTop = el.scrollHeight;
  }, [exchanges.length]);
  return (
    <div
      className="chat-thread"
      ref={box}
      onScroll={(e) => {
        const el = e.currentTarget;
        stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
      }}
    >
      {hasMore && (
        <div className="chat-earlier">
          <button
            className="button"
            disabled={loadingMore}
            onClick={() => {
              const el = box.current;
              if (el)
                before.current = { height: el.scrollHeight, top: el.scrollTop };
              onLoadMore();
            }}
          >
            {loadingMore ? "Loading…" : "Load earlier"}
          </button>
          {moreFailed && (
            <span className="chat-error" role="alert">
              Earlier exchanges could not be loaded.
            </span>
          )}
        </div>
      )}
      {ordered.map((ex, i) => (
        <React.Fragment key={`${ex.session}:${ex.at_ms}`}>
          {(i === 0 || ordered[i - 1].session !== ex.session) && (
            <SessionDivider session={sessions.get(ex.session)} />
          )}
          <Exchange ex={ex} hasLater={i < ordered.length - 1} index={index} />
        </React.Fragment>
      ))}
    </div>
  );
}
