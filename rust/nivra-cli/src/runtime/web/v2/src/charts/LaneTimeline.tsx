import React, { useMemo, useState } from "react";
import type { AgentTurn } from "../types";
import { clock, duration, numeric } from "../data";
import { turnFailed } from "../model";
import { useWidth } from "./useWidth";

const OPEN_MS = (9 * 60 + 15) * 60_000,
  CLOSE_MS = (15 * 60 + 30) * 60_000,
  IST_MS = 5.5 * 3_600_000,
  DAY_MS = 86_400_000;
/** 09:15–15:30 IST of the session date, widened to include every observed turn. */
export function sessionRange(
  date: string | null | undefined,
  turns: AgentTurn[],
): [number, number] {
  const times = turns
    .map((t) => numeric(t.at_ms))
    .filter((v): v is number => v !== null);
  const iso = /^\d{4}-\d{2}-\d{2}$/.test(date ?? "")
    ? Date.parse(`${date}T00:00:00Z`)
    : NaN;
  const midnight = Number.isFinite(iso)
    ? iso - IST_MS
    : times.length
      ? Math.floor((Math.min(...times) + IST_MS) / DAY_MS) * DAY_MS - IST_MS
      : null;
  if (midnight === null) return [0, 1];
  return [
    Math.min(midnight + OPEN_MS, ...times),
    Math.max(midnight + CLOSE_MS, ...times),
  ];
}
const LANE_COLOR: Record<string, string> = {
  decision: "--lane-decision",
  gate: "--lane-gate",
  research: "--lane-research",
  review: "--lane-review",
};
export const laneColor = (lane: string) => LANE_COLOR[lane] ?? "--fg-1";

/** One row per lane; each turn is a bar whose height follows log(elapsed). */
export default function LaneTimeline({
  turns,
  lanes,
  range,
  now,
  rowHeight = 36,
}: {
  turns: AgentTurn[];
  lanes: string[];
  range: [number, number];
  /** Source checkpoint time; never the browser clock. */
  now?: number | null;
  rowHeight?: number;
}) {
  const [ref, width] = useWidth<HTMLDivElement>(800);
  const [hover, setHover] = useState<{
    turn: AgentTurn;
    x: number;
    y: number;
  } | null>(null);
  const left = 74,
    right = 8,
    axis = 16;
  const [from, to] = range;
  const x = (t: number) =>
    left + ((t - from) / Math.max(1, to - from)) * (width - left - right);
  const maxElapsed = useMemo(
    () => Math.max(1, ...turns.map((t) => numeric(t.elapsed_ms) ?? 0)),
    [turns],
  );
  const height = lanes.length * rowHeight + axis + 4;
  // Hour-ish ticks aligned to quarter hours that fit the width.
  const ticks = useMemo(() => {
    const step = (to - from) / Math.max(2, Math.floor((width - left) / 90));
    const quant =
      [15, 30, 60, 120].map((m) => m * 60_000).find((q) => q >= step) ??
      7_200_000;
    const out: number[] = [];
    for (
      let t = Math.ceil((from + IST_MS) / quant) * quant - IST_MS;
      t <= to;
      t += quant
    )
      out.push(t);
    return out;
  }, [from, to, width]);
  return (
    <div ref={ref} style={{ position: "relative" }}>
      <svg
        className="lane-timeline"
        width={width}
        height={height}
        role="img"
        aria-label="pi lane timeline"
      >
        {lanes.map((lane, r) => {
          const y0 = 4 + r * rowHeight;
          return (
            <g key={lane}>
              <text x={4} y={y0 + rowHeight / 2 + 3}>
                {lane}
              </text>
              <line
                className="grid-line"
                x1={left}
                x2={width - right}
                y1={y0 + rowHeight - 2}
                y2={y0 + rowHeight - 2}
              />
              {turns
                .filter((t) => t.lane === lane && numeric(t.at_ms) !== null)
                .map((t) => {
                  const ms = numeric(t.elapsed_ms) ?? 0;
                  const h =
                    4 +
                    (Math.log1p(ms) / Math.log1p(maxElapsed)) *
                      (rowHeight - 14);
                  const px = x(t.at_ms!);
                  const failed = turnFailed(t);
                  const color = `var(${failed ? "--neg" : laneColor(lane)})`;
                  return (
                    <g key={t.turn_id}>
                      {t.verdict?.escalate && (
                        <path
                          d={`M${px} ${y0 + rowHeight - 6 - h - 7}l3 3l-3 3l-3 -3z`}
                          style={{ fill: `var(${laneColor(lane)})` }}
                        />
                      )}
                      <rect
                        className="turn"
                        x={px - 1.5}
                        y={y0 + rowHeight - 3 - h}
                        width={3}
                        height={h}
                        style={{
                          fill: failed ? "none" : color,
                          stroke: color,
                          opacity: failed ? 1 : 0.8,
                        }}
                        onMouseEnter={(e) =>
                          setHover({ turn: t, x: e.clientX, y: e.clientY })
                        }
                        onMouseMove={(e) =>
                          setHover({ turn: t, x: e.clientX, y: e.clientY })
                        }
                        onMouseLeave={() => setHover(null)}
                      />
                    </g>
                  );
                })}
            </g>
          );
        })}
        {ticks.map((t) => (
          <text
            key={t}
            x={Math.min(x(t), width - 14)}
            y={height - 3}
            textAnchor="middle"
          >
            {clock(t)}
          </text>
        ))}
        {now != null && now >= from && now <= to && (
          <g>
            <line
              className="now-line"
              x1={x(now)}
              x2={x(now)}
              y1={2}
              y2={height - axis}
            />
            <text x={x(now) + 4} y={10}>
              now
            </text>
          </g>
        )}
      </svg>
      {hover && (
        <div
          className="chart-tip"
          style={{ left: hover.x + 12, top: hover.y + 12 }}
        >
          <div>
            <span>{clock(hover.turn.at_ms)} IST</span>
            <span>{hover.turn.lane}</span>
          </div>
          <div>
            <span>model</span>
            <span>{hover.turn.model ?? "—"}</span>
          </div>
          <div>
            <span>trigger</span>
            <span>
              {hover.turn.trigger?.reason || hover.turn.trigger?.kind || "—"}
            </span>
          </div>
          <div>
            <span>outcome</span>
            <span>
              {hover.turn.verdict
                ? hover.turn.verdict.escalate
                  ? "escalate"
                  : "pass"
                : hover.turn.error || hover.turn.status || "—"}
            </span>
          </div>
          <div>
            <span>latency</span>
            <span>{duration(hover.turn.elapsed_ms)}</span>
          </div>
          <div>
            <span>tokens</span>
            <span>
              {hover.turn.usage
                ? `${hover.turn.usage.input ?? "—"} / ${hover.turn.usage.output ?? "—"}`
                : "—"}
            </span>
          </div>
        </div>
      )}
    </div>
  );
}
