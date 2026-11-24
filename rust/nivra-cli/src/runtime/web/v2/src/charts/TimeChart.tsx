import type uPlot from "uplot";
import React, { useEffect, useMemo, useRef, useState } from "react";
import { clock } from "../data";
import { readTokens, withAlpha } from "./tokens";

export interface TimeSeries {
  label: string;
  /** A CSS custom property such as `--accent`. */
  color: string;
  /** One value per x; `null` leaves a gap. */
  data: (number | null)[];
  /** 7% area fill under the line. */
  area?: boolean;
  dash?: number[];
  step?: boolean;
  width?: number;
  /** Mark observations that stand alone between gaps. */
  isolatedPoints?: boolean;
}
export interface Marker {
  at: number;
  value: number;
  side: "buy" | "sell";
}
export interface Pane {
  height: number;
  series: TimeSeries[];
  /** Dashed horizontal reference, drawn under the series. */
  baseline?: number | null;
  markers?: Marker[];
  yRange?: (min: number, max: number) => [number, number];
  format?: (value: number) => string;
  /** Short label drawn in the pane's top-left corner. */
  caption?: string;
}
export interface Band {
  from: number;
  to: number;
  label?: string;
}
export interface Tip {
  title: string;
  rows: [string, string][];
}
export interface TimeChartProps {
  /** Seconds since epoch, shared by every pane. */
  x: number[];
  panes: Pane[];
  /** Unobserved intervals in epoch milliseconds. */
  bands?: Band[];
  tooltip?: (index: number) => Tip | null;
  ariaLabel: string;
  /** Keep the current zoom instead of resetting on new data. */
  frozen?: boolean;
  onSelect?: () => void;
  /** Called when uPlot cannot be loaded. */
  onError?: () => void;
}
const TOKENS: string[] = [
  "--fg-1",
  "--fg-2",
  "--fg-3",
  "--line",
  "--line-2",
  "--pos",
  "--neg",
  "--bg-3",
];
let instances = 0;
const FONT = "10px ui-monospace, Menlo, Consolas, monospace";

function tooltipPlugin(read: () => TimeChartProps["tooltip"]): uPlot.Plugin {
  let tip: HTMLDivElement;
  return {
    hooks: {
      init: (u) => {
        tip = document.createElement("div");
        tip.className = "u-tip";
        u.over.appendChild(tip);
      },
      setCursor: (u) => {
        const idx = u.cursor.idx,
          left = u.cursor.left ?? -1;
        const body = idx == null || left < 0 ? null : read()?.(idx);
        if (!body) {
          tip.style.display = "none";
          return;
        }
        tip.replaceChildren();
        const head = document.createElement("div");
        head.textContent = body.title;
        tip.appendChild(head);
        for (const [label, value] of body.rows) {
          const row = document.createElement("div");
          const a = document.createElement("span"),
            b = document.createElement("span");
          a.textContent = label;
          b.textContent = value;
          row.append(a, b);
          tip.appendChild(row);
        }
        tip.style.display = "block";
        const flip = left > u.over.clientWidth - 190;
        tip.style.left =
          (flip ? left - tip.offsetWidth - 12 : left + 12) + "px";
        tip.style.top = "6px";
      },
    },
  };
}

export function TimeChart(props: TimeChartProps) {
  const host = useRef<HTMLDivElement>(null),
    plots = useRef<uPlot[]>([]),
    latest = useRef(props);
  const [ready, setReady] = useState(0),
    [failed, setFailed] = useState(false);
  latest.current = props;
  // Rebuild only when the pane structure changes, not on every data tick.
  const shape = props.panes
    .map(
      (p) =>
        `${p.height}|${p.caption ?? ""}|` +
        p.series
          .map((s) =>
            [s.label, s.color, s.area, s.step, s.dash?.join()].join(":"),
          )
          .join(","),
    )
    .join("/");
  useEffect(() => {
    const root = host.current;
    if (!root) return;
    let disposed = false;
    let resize: ResizeObserver | undefined;
    import("uplot")
      .then(({ default: UPlot }) => {
        if (disposed || !root) return;
        const { panes, x } = latest.current;
        const c = readTokens([
          ...TOKENS,
          ...new Set(panes.flatMap((p) => p.series.map((s) => s.color))),
        ]);
        const sync = `nivra-time-${++instances}`;
        const width = () => Math.max(200, root.clientWidth);
        const made = panes.map((pane, index) => {
          const last = index === panes.length - 1;
          const mount = document.createElement("div");
          mount.className = "chart-pane";
          root.appendChild(mount);
          const opts: uPlot.Options = {
            width: width(),
            height: pane.height,
            padding: [10, 8, 0, 0],
            legend: { show: false },
            tzDate: (ts) => UPlot.tzDate(new Date(ts * 1000), "Asia/Kolkata"),
            cursor: {
              x: true,
              y: false,
              sync: { key: sync },
              drag: { x: true, y: false },
              points: { size: 5 },
            },
            scales: {
              x: { time: true },
              y: {
                range: (_u, min, max) => {
                  if (pane.yRange) return pane.yRange(min, max);
                  const pad = (max - min || Math.abs(max) * 0.002 || 1) * 0.08;
                  return [min - pad, max + pad];
                },
              },
            },
            series: [
              {},
              ...pane.series.map((s): uPlot.Series => ({
                label: s.label,
                stroke: c[s.color],
                width: s.width ?? 1.4,
                dash: s.dash,
                fill: s.area ? withAlpha(c[s.color], 0.07) : undefined,
                paths: s.step ? UPlot.paths.stepped!({ align: 1 }) : undefined,
                spanGaps: false,
                points: s.isolatedPoints
                  ? {
                      show: true,
                      size: 5,
                      filter: (u, i) => {
                        const values = u.data[i];
                        return Array.from(values).flatMap((value, idx) =>
                          value != null &&
                          (idx === 0 || values[idx - 1] == null) &&
                          (idx === values.length - 1 || values[idx + 1] == null)
                            ? [idx]
                            : [],
                        );
                      },
                    }
                  : { show: false },
              })),
            ],
            axes: [
              {
                show: last,
                stroke: c["--fg-2"],
                font: FONT,
                grid: { stroke: c["--line"], width: 0.5 },
                ticks: { show: false },
                values: (_, ticks) => ticks.map((t) => clock(t * 1000)),
              },
              {
                side: 1,
                stroke: c["--fg-2"],
                font: FONT,
                size: 64,
                grid: { stroke: c["--line"], width: 0.5 },
                ticks: { show: false },
                values: (_, ticks) =>
                  ticks.map((v) =>
                    (latest.current.panes[index]?.format ?? compact)(v),
                  ),
              },
            ],
            plugins:
              index === 0 ? [tooltipPlugin(() => latest.current.tooltip)] : [],
            hooks: {
              setSelect: [() => latest.current.onSelect?.()],
              drawClear: [
                (u) => {
                  const p = latest.current.panes[index];
                  const ctx = u.ctx;
                  ctx.save();
                  const { left, top, width: w, height: h } = u.bbox;
                  for (const band of latest.current.bands ?? []) {
                    const a = u.valToPos(band.from / 1000, "x", true),
                      b = u.valToPos(band.to / 1000, "x", true);
                    const x0 = Math.max(left, Math.min(a, b)),
                      x1 = Math.min(left + w, Math.max(a, b));
                    if (x1 - x0 < 1) continue;
                    ctx.globalAlpha = 0.14;
                    ctx.fillStyle = c["--fg-2"];
                    ctx.fillRect(x0, top, x1 - x0, h);
                    if (index === 0 && x1 - x0 > 60 && band.label) {
                      ctx.globalAlpha = 0.7;
                      ctx.font = FONT;
                      ctx.fillText(band.label, x0 + 5, top + 12);
                    }
                  }
                  ctx.globalAlpha = 1;
                  if (p?.baseline != null) {
                    const y = u.valToPos(p.baseline, "y", true);
                    if (y >= top && y <= top + h) {
                      ctx.setLineDash([
                        4 * devicePixelRatio,
                        4 * devicePixelRatio,
                      ]);
                      ctx.strokeStyle = c["--fg-3"];
                      ctx.lineWidth = devicePixelRatio;
                      ctx.beginPath();
                      ctx.moveTo(left, y);
                      ctx.lineTo(left + w, y);
                      ctx.stroke();
                    }
                  }
                  ctx.restore();
                },
              ],
              draw: [
                (u) => {
                  const p = latest.current.panes[index];
                  const ctx = u.ctx,
                    r = 4 * devicePixelRatio;
                  ctx.save();
                  if (p?.caption) {
                    ctx.fillStyle = c["--fg-2"];
                    ctx.font = FONT;
                    ctx.fillText(
                      p.caption,
                      u.bbox.left + 6,
                      u.bbox.top + 12 * devicePixelRatio,
                    );
                  }
                  for (const m of p?.markers ?? []) {
                    const px = u.valToPos(m.at / 1000, "x", true),
                      py = u.valToPos(m.value, "y", true);
                    if (px < u.bbox.left || px > u.bbox.left + u.bbox.width)
                      continue;
                    const dir = m.side === "buy" ? 1 : -1;
                    ctx.fillStyle = m.side === "buy" ? c["--pos"] : c["--neg"];
                    ctx.globalAlpha = 0.85;
                    ctx.beginPath();
                    const y0 = py + dir * r * 1.6;
                    ctx.moveTo(px, y0 - dir * r * 1.2);
                    ctx.lineTo(px - r, y0 + dir * r * 0.6);
                    ctx.lineTo(px + r, y0 + dir * r * 0.6);
                    ctx.closePath();
                    ctx.fill();
                  }
                  ctx.restore();
                },
              ],
            },
          };
          return new UPlot(
            opts,
            [x, ...pane.series.map((s) => s.data)] as uPlot.AlignedData,
            mount,
          );
        });
        plots.current = made;
        if (typeof ResizeObserver !== "undefined") {
          resize = new ResizeObserver(() =>
            made.forEach((u, i) =>
              u.setSize({
                width: width(),
                height: latest.current.panes[i]?.height ?? u.height,
              }),
            ),
          );
          resize.observe(root);
        }
        setReady((v) => v + 1);
      })
      .catch(() => {
        setFailed(true);
        latest.current.onError?.();
      });
    return () => {
      disposed = true;
      resize?.disconnect();
      plots.current.forEach((u) => u.destroy());
      plots.current = [];
      root.replaceChildren();
    };
  }, [shape]);
  const fingerprint = useMemo(() => props.x, [props.x]);
  useEffect(() => {
    if (props.frozen) return;
    plots.current.forEach((u, i) =>
      u.setData([
        fingerprint,
        ...(props.panes[i]?.series.map((s) => s.data) ?? []),
      ] as uPlot.AlignedData),
    );
  }, [fingerprint, props.panes, props.frozen, ready]);
  useEffect(() => {
    // Markers and bands are drawn from props; repaint without touching scales.
    plots.current.forEach((u) => u.redraw(false));
  }, [props.bands, props.panes, ready]);
  return failed ? null : (
    <div
      ref={host}
      className="chart-canvas"
      role="img"
      aria-label={props.ariaLabel}
    />
  );
}
const compact = (v: number) =>
  v.toLocaleString("en-IN", { maximumFractionDigits: 2 });
