import type uPlot from "uplot";
import type { Point, SeriesCoverage } from "./types";
import React, { useEffect, useRef, useState } from "react";
import { Crosshair, Maximize2, Pause, Play, TrendingUp } from "lucide-react";
import { amount, clock, downsamplePoints, words } from "./data";
import { Button, Empty, Segments } from "./ui";
export function chartData(
  points: Point[],
  range: string,
  maxPoints = 1200,
): uPlot.AlignedData {
  const tail = points.at(-1)?.at;
  const windowMs = (
    { "5m": 300000, "15m": 900000, "1h": 3600000 } as Record<string, number>
  )[range];
  const visible = windowMs
    ? points.filter((p) => p.at >= (tail ?? 0) - windowMs)
    : points;
  const sampled = downsamplePoints(visible, maxPoints);
  const values = sampled.map((p) =>
    p.value !== null && !p.carried ? p.value : null,
  );
  const carried = sampled.map((p) => (p.carried ? p.value : null));
  // Bridge each carried run to its neighbouring receipts so the muted dashed
  // step starts and ends at the original observations.
  for (let index = 0; index < sampled.length; index++) {
    if (carried[index] === null) continue;
    if (index > 0 && values[index - 1] !== null)
      carried[index - 1] = values[index - 1];
    if (index + 1 < sampled.length && values[index + 1] !== null)
      carried[index + 1] = values[index + 1];
  }
  return [sampled.map((p) => p.at / 1000), values, carried];
}
export default function Chart({
  points = [],
  label = "Price",
  source = "Recorded observations",
  coverage = null,
  height = 280,
  emptyTitle = "Waiting for recorded chart evidence",
  emptyDetail = "The authenticated series read is unavailable or still loading. No synthetic history is drawn.",
}: {
  points?: Point[];
  label?: string;
  source?: string;
  coverage?: SeriesCoverage | null;
  height?: number;
  emptyTitle?: string;
  emptyDetail?: string;
}) {
  const host = useRef<HTMLDivElement>(null),
    plot = useRef<uPlot | null>(null),
    latest = useRef(points);
  const [range, setRange] = useState("all"),
    [follow, setFollow] = useState(true),
    [hover, setHover] = useState<Point | null>(null),
    [gapHint, setGapHint] = useState<{ reason?: string; left: number } | null>(
      null,
    ),
    [failed, setFailed] = useState(false);
  latest.current = points;
  const valid = points.filter((p) => p.value !== null && !p.carried),
    enough = valid.length >= 2,
    plotted = chartData(points, range)[0].length,
    historyStart =
      coverage?.store_started_after_open &&
      Number.isFinite(Number(coverage?.store_start_ms))
        ? Number(coverage.store_start_ms)
        : null;
  const [version, setVersion] = useState(0),
    [theme, setTheme] = useState(() => document.documentElement.dataset.theme);
  useEffect(() => {
    const observer = new MutationObserver(() =>
      setTheme(document.documentElement.dataset.theme),
    );
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    if (!enough || !host.current) return;
    let disposed = false;
    let resize: ResizeObserver | undefined;
    import("uplot")
      .then(({ default: uPlot }) => {
        if (disposed || !host.current) return;
        const styles = getComputedStyle(document.documentElement);
        const muted = styles.getPropertyValue("--muted").trim(),
          line = styles.getPropertyValue("--line").trim(),
          accent = styles.getPropertyValue("--accent").trim();
        const current = new uPlot(
          {
            width: Math.max(200, host.current.clientWidth),
            height,
            padding: [18, 18, 0, 0],
            legend: { show: false },
            cursor: { drag: { x: true, y: false }, points: { size: 5 } },
            scales: { x: { time: true } },
            series: [
              {},
              {
                label,
                stroke: accent,
                width: 1.6,
                fill: accent + "0a",
                spanGaps: false,
                points: {
                  show: true,
                  filter: (u) => {
                    const values = u.data[1];
                    return Array.from(values).flatMap((value, idx) =>
                      value != null &&
                      (idx === 0 || values[idx - 1] == null) &&
                      (idx === values.length - 1 || values[idx + 1] == null)
                        ? [idx]
                        : [],
                    );
                  },
                  size: 5,
                },
              },
              {
                label: `${label} carried`,
                stroke: muted,
                width: 1.2,
                dash: [4, 4],
                spanGaps: false,
                points: { show: false },
              },
            ],
            axes: [
              {
                stroke: muted,
                font: "10px system-ui",
                grid: { show: false },
                ticks: { show: false },
                values: (_, ticks) => ticks.map((t) => clock(t * 1000)),
              },
              {
                side: 1,
                stroke: muted,
                font: "10px system-ui",
                size: 62,
                grid: { stroke: line, width: 0.5 },
                ticks: { show: false },
                values: (_, ticks) =>
                  ticks.map((v) =>
                    v.toLocaleString("en-IN", { maximumFractionDigits: 2 }),
                  ),
              },
            ],
            hooks: {
              setCursor: [
                (u) => {
                  const pointer = u.cursor.left;
                  const at =
                    pointer == null ? null : u.posToVal(pointer, "x") * 1000;
                  const gap =
                    at == null
                      ? null
                      : latest.current.find(
                          (point) =>
                            point.gap &&
                            point.value === null &&
                            at >= point.at &&
                            at <= (point.gapTo ?? point.at) + 59_999,
                        );
                  setGapHint(
                    gap && pointer != null
                      ? {
                          reason: gap.reason,
                          left: u.valToPos(gap.at / 1000, "x", true),
                        }
                      : null,
                  );
                  if (gap) {
                    setHover(null);
                    return;
                  }
                  const i = u.cursor.idx;
                  const value =
                    i == null ? null : (u.data[1][i] ?? u.data[2][i]);
                  setHover(
                    value == null
                      ? null
                      : {
                          at: u.data[0][i!] * 1000,
                          value,
                          carried: u.data[1][i!] == null,
                        },
                  );
                },
              ],
              setSelect: [() => setFollow(false)],
              draw: [
                (u) => {
                  const gaps = latest.current.filter(
                    (point) => point.gap && point.value === null,
                  );
                  if (!gaps.length) return;
                  const ctx = u.ctx,
                    left = u.bbox.left,
                    right = left + u.bbox.width;
                  ctx.save();
                  for (const gap of gaps) {
                    const start = u.valToPos(gap.at / 1000, "x", true);
                    const end = u.valToPos(
                      ((gap.gapTo ?? gap.at) + 60_000) / 1000,
                      "x",
                      true,
                    );
                    const x0 = Math.max(left, Math.min(start, end)),
                      x1 = Math.min(right, Math.max(start, end));
                    if (x1 - x0 < 1) continue;
                    ctx.fillStyle = muted;
                    ctx.globalAlpha = 0.12;
                    ctx.fillRect(x0, u.bbox.top, x1 - x0, u.bbox.height);
                    if (x1 - x0 > 55) {
                      ctx.globalAlpha = 0.65;
                      ctx.font = "9px system-ui";
                      ctx.fillText("not observed", x0 + 5, u.bbox.top + 13);
                    }
                  }
                  ctx.restore();
                },
              ],
            },
          },
          chartData(latest.current, range),
          host.current,
        );
        plot.current = current;
        if (typeof ResizeObserver !== "undefined") {
          resize = new ResizeObserver(() =>
            current.setSize({
              width: Math.max(200, host.current?.clientWidth || 200),
              height,
            }),
          );
          resize.observe(host.current);
        }
        setVersion((v) => v + 1);
      })
      .catch(() => setFailed(true));
    return () => {
      disposed = true;
      resize?.disconnect();
      plot.current?.destroy();
      plot.current = null;
    };
  }, [enough, height, label, theme]);
  useEffect(() => {
    if (follow && plot.current) plot.current.setData(chartData(points, range));
  }, [points, range, follow, version]);
  const current = hover || valid.at(-1);
  return (
    <div className="chart-module">
      <div className="chart-toolbar">
        <div className="chart-readout">
          <span className="chart-series-dot" />
          {label}
          <strong>{current ? amount(current.value) : "—"}</strong>
          <time>{current ? clock(current.at) + " IST" : ""}</time>
          {current?.carried ? (
            <span className="chart-carry-tag">carried · no trade</span>
          ) : null}
        </div>
        <div className="chart-tools">
          <Segments
            value={range}
            onChange={(v) => {
              setRange(v);
              setFollow(true);
            }}
            items={["5m", "15m", "1h", "all"].map((v) => [
              v,
              v === "all" ? "All" : v,
            ])}
            label="Chart time range"
          />
          <Button
            icon={follow ? Pause : Play}
            onClick={() => setFollow(!follow)}
          >
            {follow ? "Pause" : "Follow"}
          </Button>
        </div>
      </div>
      {enough && !failed ? (
        <div className="chart-plot">
          <div
            ref={host}
            className="chart-canvas"
            role="img"
            aria-label={`${label}, ${valid.length} observations${plotted < points.length ? `, ${plotted} plotted with extrema preserved` : ""}. Drag to zoom; use Follow to reset.`}
          />
          {gapHint ? (
            <span className="chart-gap-hint" style={{ left: gapHint.left }}>
              Unobserved · {words(gapHint.reason)}
            </span>
          ) : null}
        </div>
      ) : (
        <div className="chart-unavailable" style={{ minHeight: height }}>
          <Empty
            icon={TrendingUp}
            title={failed ? "Chart could not be loaded" : emptyTitle}
          >
            {emptyDetail}
            {valid.length === 1
              ? ` One observation: ${amount(valid[0].value)} at ${clock(valid[0].at)} IST.`
              : ""}
          </Empty>
        </div>
      )}
      {historyStart !== null ? (
        <div className="chart-history-note">
          History since {clock(historyStart)} IST · the in-memory minute store
          restarted after session open, so earlier minutes are not shown.
        </div>
      ) : null}
      <div className="chart-foot">
        <span>
          <Crosshair size={12} /> {source}
        </span>
        <span>
          {enough
            ? plotted < points.length
              ? `Extrema-preserving view · ${plotted} of ${points.length} points · no-trade minutes carried; unobserved intervals stay open`
              : "Drag to zoom · hover to inspect · no-trade minutes carried; unobserved intervals stay open"
            : "No synthetic history or interpolated prices"}
        </span>
      </div>
    </div>
  );
}
