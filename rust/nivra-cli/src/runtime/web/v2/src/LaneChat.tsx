import type {
  TranscriptExchange,
  TranscriptItem,
  TranscriptSession,
  TranscriptText,
} from "./types";
import React, { useLayoutEffect, useRef, useState } from "react";
import { Brain, ChevronRight } from "lucide-react";
import { clock, count, datetime, duration, numeric, usd, words } from "./data";
import { turnFailed } from "./model";
import { Tag } from "./ui";

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
const kib = (bytes: number) =>
  bytes < 1024 ? `${bytes} B` : `${(bytes / 1024).toFixed(1)} KB`;

function Block({ label, value }: { label: string; value: TranscriptText }) {
  return (
    <div className="chat-block">
      <span className="chat-block-label">
        {label}
        {value.truncated && <Tag tone="warn">truncated</Tag>}
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
        <span className="chat-line-text muted">{firstLine(text)}</span>
      </button>
      {open && (
        <div className="chat-detail">
          <pre>{text}</pre>
          {truncated && <Tag tone="warn">truncated</Tag>}
        </div>
      )}
    </div>
  );
}

function ToolLine({
  tool,
  live,
}: {
  tool: Extract<TranscriptItem, { kind: "tool" }>;
  live: boolean;
}) {
  const [open, setOpen] = useState(false);
  const status: { label: string; tone?: "pos" | "neg" | "warn" } = tool.result
    ? tool.is_error
      ? { label: "failed", tone: "neg" }
      : { label: "ok", tone: "pos" }
    : live
      ? { label: "running" }
      : { label: "no result", tone: "warn" };
  return (
    <div className="chat-item">
      <button
        className="chat-line"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        <ChevronRight size={12} className="chat-chevron" aria-hidden="true" />
        <strong>{tool.name}</strong>
        <span className="chat-line-text muted">{preview(tool.args.text)}</span>
        <Tag tone={status.tone}>{status.label}</Tag>
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
            <p className="muted">No result recorded yet.</p>
          )}
        </div>
      )}
    </div>
  );
}

/** The lane's own trigger as a short sentence: named instruments, else the brief size. */
function triggerBody(ex: TranscriptExchange) {
  const names = (ex.turn?.trigger?.instruments ?? []).map(
    (k) => k.split("|")[1] || k,
  );
  if (!names.length) return `Brief · ${kib(ex.brief.bytes)}`;
  const shown = names.slice(0, 6).join(", ");
  return `Instruments: ${shown}${names.length > 6 ? ` and ${names.length - 6} more` : ""}`;
}

function UserBubble({ ex }: { ex: TranscriptExchange }) {
  const [open, setOpen] = useState(false);
  const trigger = ex.turn?.trigger;
  const meta = [
    clock(ex.at_ms) + " IST",
    trigger?.kind ? words(trigger.kind) : null,
    trigger?.reason ? words(trigger.reason) : null,
  ].filter(Boolean);
  return (
    <div className="chat-user">
      <div className="chat-meta">{meta.join(" · ")}</div>
      <p>{triggerBody(ex)}</p>
      <button
        className="chat-link"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        {open ? "Hide full brief" : "Show full brief"}
      </button>
      {open && (
        <>
          <pre>{ex.brief.text || "(empty)"}</pre>
          {ex.brief.truncated && (
            <p className="chat-note">
              Brief truncated: showing the first part of {kib(ex.brief.bytes)}.
            </p>
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
  const turn = ex.turn;
  const chips: React.ReactNode[] = [];
  if (turn?.error)
    chips.push(
      <Tag key="error" tone="neg">
        {turn.error}
      </Tag>,
    );
  else if (turn && turnFailed(turn))
    chips.push(
      <Tag key="status" tone="neg">
        {words(turn.status)}
      </Tag>,
    );
  else if (!ex.complete)
    chips.push(
      <Tag key="open" tone={hasLater ? "warn" : undefined}>
        {hasLater ? "interrupted" : "in progress"}
      </Tag>,
    );
  if (turn?.verdict)
    chips.push(
      <Tag key="verdict" tone={turn.verdict.escalate ? "pos" : "warn"}>
        {turn.verdict.escalate ? "escalated" : "declined"}
      </Tag>,
      <span key="reason">{turn.verdict.reason}</span>,
    );
  if (turn?.plan) {
    const plan = turn.plan,
      validated = plan.status === "validated";
    chips.push(
      <Tag key="plan" tone={validated ? "pos" : "warn"}>
        {validated
          ? `plan validated · ${count(plan.actions ?? 0)} actions`
          : `plan ${words(plan.status)}`}
      </Tag>,
    );
    const detail = [plan.stance && words(plan.stance), plan.error]
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
    usage && `${count(usage.input)} in / ${count(usage.output)} out`,
    usage && numeric(usage.cache_read) !== null
      ? `${count(usage.cache_read)} cached`
      : null,
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
            fallback {turn.fallback.to?.id ?? "unknown"}
          </Tag>
        </>
      )}
    </div>
  );
}

function Answer({ items, live }: { items: TranscriptItem[]; live: boolean }) {
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
        out.push(<ToolLine key={i} tool={item} live={live} />);
        break;
      case "text":
        out.push(
          <p key={i} className="chat-answer">
            {item.text}
            {item.truncated && <Tag tone="warn">truncated</Tag>}
          </p>,
        );
        break;
      case "error":
        out.push(
          <p key={i} className="chat-error" role="alert">
            {item.text}
          </p>,
        );
        break;
      case "note":
        out.push(
          <p key={i} className="chat-note">
            {item.text}
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
}: {
  ex: TranscriptExchange;
  hasLater: boolean;
}) {
  return (
    <article className="chat-exchange">
      <UserBubble ex={ex} />
      <div className="chat-assistant">
        <Answer items={ex.items} live={!ex.complete && !hasLater} />
        <Outcome ex={ex} hasLater={hasLater} />
        <Footer ex={ex} />
      </div>
    </article>
  );
}

function SessionDivider({
  id,
  session,
}: {
  id: string;
  session?: TranscriptSession;
}) {
  const bits = [
    `session ${id.slice(0, 8)}`,
    session?.model,
    session?.thinking && `thinking ${session.thinking}`,
    session?.started_ms != null && `started ${datetime(session.started_ms)}`,
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
  hasMore,
  loadingMore,
  moreFailed,
  onLoadMore,
}: {
  exchanges: TranscriptExchange[];
  sessions: Map<string, TranscriptSession>;
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
            <SessionDivider
              id={ex.session}
              session={sessions.get(ex.session)}
            />
          )}
          <Exchange ex={ex} hasLater={i < ordered.length - 1} />
        </React.Fragment>
      ))}
    </div>
  );
}
