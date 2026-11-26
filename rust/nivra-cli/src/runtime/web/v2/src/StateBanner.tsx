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
  TimerOff,
} from "lucide-react";
import React from "react";
import type { Condition } from "./model";
import { plain, stalledText } from "./labels";
import { Button } from "./ui";

type Tone = "info" | "warn" | "error" | "busy";
interface View {
  tone: Tone;
  icon: LucideIcon;
  title: string;
  body: React.ReactNode;
  chips?: { label: string; note: string }[];
  link?: [href: string, label: string];
  refresh?: boolean;
}

function view(condition: Condition): View | null {
  switch (condition.kind) {
    case "stalled":
      return {
        tone: "error",
        icon: TimerOff,
        ...stalledText(condition.lastUpdateMs),
        link: ["#/health", "Open health"],
      };
    case "starting":
      return {
        tone: "busy",
        icon: Loader,
        title: "Controller starting",
        body: "Values appear shortly. This page retries on its own.",
      };
    case "halted":
      return {
        tone: "error",
        icon: condition.stopped ? CircleStop : OctagonX,
        title: condition.stopped
          ? "The paper book is stopped"
          : "The paper book is halted",
        body: (
          <>
            Nothing restarts on its own.
            {condition.reason && <> Reason: {plain(condition.reason)}.</>}
          </>
        ),
        link: ["#/health", "Open health"],
      };
    case "research":
      return {
        tone: "info",
        icon: FlaskConical,
        title: "Research only. No paper account is attached.",
        body: "Strategies, backtest and past sessions still work.",
        chips: condition.detached.map((i) => ({
          label: plain(i.source),
          note: "not attached",
        })),
        link: ["#/backtest", "Open backtest lab"],
      };
    case "closed":
      return {
        tone: "info",
        icon: MoonStar,
        title: "The paper session has closed",
        body: "These are the final values for the session.",
        link: ["#/sessions", "Browse sessions"],
      };
    case "degraded":
      return {
        tone: "warn",
        icon: CloudOff,
        title: "Some data is unavailable",
        body: "Missing values stay unknown.",
        chips: condition.missing.length
          ? condition.missing.map((i) => ({
              label: plain(i.source),
              note: plain(i.reason).toLowerCase(),
            }))
          : [{ label: "Unknown source", note: "unavailable" }],
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
