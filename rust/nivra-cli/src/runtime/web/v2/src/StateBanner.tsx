import type { LucideIcon } from "lucide-react";
import {
  ArrowUpRight,
  CircleStop,
  CloudOff,
  FlaskConical,
  Loader,
  MoonStar,
  OctagonX,
  RefreshCw,
} from "lucide-react";
import React from "react";
import type { Condition } from "./model";
import { Button } from "./ui";

type Tone = "info" | "warn" | "error" | "busy";
interface View {
  tone: Tone;
  icon: LucideIcon;
  eyebrow: string;
  title: string;
  body: React.ReactNode;
  chips?: { label: string; note: string }[];
  link?: [href: string, label: string];
  refresh?: boolean;
}

function view(condition: Condition): View | null {
  switch (condition.kind) {
    case "starting":
      return {
        tone: "busy",
        icon: Loader,
        eyebrow: "Starting",
        title: "Controller starting",
        body: "The v1 evidence view is not published yet. Missing values stay unknown; this page retries automatically.",
      };
    case "halted":
      return {
        tone: "error",
        icon: condition.stopped ? CircleStop : OctagonX,
        eyebrow: condition.stopped ? "Book stopped" : "Book halted",
        title: condition.stopped
          ? "The paper book is stopped"
          : "The paper book is halted",
        body: (
          <>
            The book and its journals are preserved as recorded. Nothing
            restarts, unhalts or replays automatically.
            {condition.reason && (
              <>
                {" "}
                Classification: <code>{condition.reason}</code>.
              </>
            )}
          </>
        ),
        link: ["#/health", "Open health"],
      };
    case "research":
      return {
        tone: "info",
        icon: FlaskConical,
        eyebrow: "Research only",
        title: "No paper book is attached to this dashboard",
        body: "This standalone dashboard has no account, fills or live scanner. Strategies, the backtest lab and archived sessions remain available.",
        chips: condition.detached.map((i) => ({
          label: i.source,
          note: "not attached",
        })),
        link: ["#/backtest", "Open backtest lab"],
      };
    case "closed":
      return {
        tone: "info",
        icon: MoonStar,
        eyebrow: "Session closed",
        title: "The paper session has closed",
        body: "Values below are the final recorded state of the session. No further fills or marks arrive until the next session.",
        link: ["#/sessions", "Browse sessions"],
      };
    case "degraded":
      return {
        tone: "warn",
        icon: CloudOff,
        eyebrow: "Partial evidence",
        title: "Unavailable v1 evidence",
        body: "Some sources did not report. Missing values stay unknown; no zero, healthy or complete state is inferred.",
        chips: condition.missing.length
          ? condition.missing.map((i) => ({
              label: i.source,
              note: i.reason,
            }))
          : [{ label: "unknown source", note: "unavailable" }],
        refresh: true,
      };
    case "nominal":
      return null;
  }
}

export function StateBanner({
  condition,
  onRefresh,
  refreshing,
}: {
  condition: Condition;
  onRefresh: () => void;
  refreshing: boolean;
}) {
  const v = view(condition);
  if (!v) return null;
  const Icon = v.icon;
  return (
    <section
      className={`state-banner tone-${v.tone}`}
      role={v.tone === "error" ? "alert" : "status"}
      aria-label={v.title}
    >
      <span className="state-icon" aria-hidden="true">
        <Icon size={18} />
      </span>
      <div className="state-text">
        <span className="state-eyebrow">{v.eyebrow}</span>
        <strong>{v.title}</strong>
        <p>{v.body}</p>
        {v.chips && (
          <ul className="state-chips">
            {v.chips.map((c) => (
              <li key={c.label} title={c.note}>
                <b>{c.label}</b>
                <span>{c.note}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
      <div className="state-actions">
        {v.refresh && (
          <Button icon={RefreshCw} onClick={onRefresh} disabled={refreshing}>
            {refreshing ? "Refreshing…" : "Retry"}
          </Button>
        )}
        {v.link && (
          <a className="button" href={v.link[0]}>
            {v.link[1]} <ArrowUpRight size={13} />
          </a>
        )}
      </div>
    </section>
  );
}
