import type uPlot from "uplot";
import type { Point, SeriesCoverage } from "./types";
import React, { useMemo, useState } from "react";
import { Crosshair, Pause, Play } from "lucide-react";
import { amount, clock, downsamplePoints, words } from "./data";
import { Button, Empty, Segments } from "./ui";
import { TimeChart } from "./charts/TimeChart";
import type { Band, Marker } from "./charts/TimeChart";
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
export interface FillMarker {
  at: number;
  side: "buy" | "sell";
}
const RANGES: [string, string][] = [
  ["5m", "5m"],
  ["15m", "15m"],
  ["1h", "1h"],
  ["all", "day"],
];
/** Value of the last non-null observation at or before `at`. */
function valueAt(x: number[], values: (number | null)[], at: number) {
  let lo = 0,
    hi = x.length - 1,
    found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (x[mid] * 1000 <= at) {
      found = mid;
      lo = mid + 1;
    } else hi = mid - 1;
  }
  for (let i = found; i >= 0; i--) if (values[i] !== null) return values[i];
  return null;
}
/** Price or equity series over TimeChart: gaps, carried minutes and fills. */
export default function Chart({
  points = [],
  label = "Price",
  source = "Recorded observations",
  coverage = null,
  height = 280,
  emptyTitle = "Waiting for recorded chart evidence",
  emptyDetail = "The authenticated series read is unavailable or still loading. No synthetic history is drawn.",
  baseline = null,
  drawdown = false,
  markers = [],
}: {
  points?: Point[];
  label?: string;
  source?: string;
  coverage?: SeriesCoverage | null;
  height?: number;
  emptyTitle?: string;
  emptyDetail?: string;
  /** Session-start reference drawn dashed; also feeds the tooltip's day net. */
  baseline?: number | null;
  /** Add a synced drawdown pane computed from the plotted values. */
  drawdown?: boolean;
  markers?: FillMarker[];
}) {
  const [range, setRange] = useState("all"),
    [follow, setFollow] = useState(true),
    [failed, setFailed] = useState(false);
  const valid = points.filter((p) => p.value !== null && !p.carried),
    enough = valid.length >= 2,
    data = useMemo(() => chartData(points, range), [points, range]),
    plotted = data[0].length,
    historyStart =
      coverage?.store_started_after_open &&
      Number.isFinite(Number(coverage?.store_start_ms))
        ? Number(coverage.store_start_ms)
        : null;
  const [x, values, carried] = data as [
    number[],
    (number | null)[],
    (number | null)[],
  ];
  const peaks = useMemo(() => {
    let peak = -Infinity;
    return x.map((_, i) => {
      const v = values[i] ?? carried[i];
      if (v === null) return null;
      peak = Math.max(peak, v);
      return peak > 0 ? ((v - peak) / peak) * 100 : 0;
    });
  }, [x, values, carried]);
  const bands = useMemo<Band[]>(
    () =>
      points
        .filter((p) => p.gap && p.value === null)
        .map((p) => ({
          from: p.at,
          to: (p.gapTo ?? p.at) + 60_000,
          label: "not observed",
        })),
    [points],
  );
  const placed = useMemo<Marker[]>(
    () =>
      markers.flatMap((m) => {
        const value = valueAt(x, values, m.at);
        return value === null ? [] : [{ ...m, value }];
      }),
    [markers, x, values],
  );
  const panes = useMemo(
    () => [
      {
        height: drawdown ? Math.round(height * 0.72) : height,
        baseline,
        markers: placed,
        series: [
          {
            label,
            color: "--accent",
            data: values,
            area: true,
            isolatedPoints: true,
          },
          {
            label: `${label} carried`,
            color: "--fg-2",
            data: carried,
            dash: [4, 4],
            width: 1.2,
          },
        ],
      },
      ...(drawdown
        ? [
            {
              height: Math.round(height * 0.28),
              caption: "drawdown %",
              format: (v: number) => v.toFixed(2),
              yRange: (min: number): [number, number] => [
                Math.min(min * 1.15, -0.05),
                0,
              ],
              series: [
                {
                  label: "Drawdown",
                  color: "--neg",
                  data: peaks,
                  area: true,
                  width: 1,
                },
              ],
            },
          ]
        : []),
    ],
    [values, carried, peaks, drawdown, height, baseline, label, placed],
  );
  const tooltip = (i: number) => {
    const at = x[i] * 1000;
    const gap = points.find(
      (p) =>
        p.gap &&
        p.value === null &&
        at >= p.at &&
        at <= (p.gapTo ?? p.at) + 59_999,
    );
    if (gap)
      return {
        title: `${clock(at)} IST`,
        rows: [["unobserved", words(gap.reason)]] as [string, string][],
      };
    const value = values[i] ?? carried[i];
    if (value === null || value === undefined) return null;
    const rows: [string, string][] = [
      [values[i] === null ? "carried · no trade" : label, amount(value)],
    ];
    if (baseline !== null) rows.push(["day net", amount(value - baseline)]);
    if (drawdown && peaks[i] !== null)
      rows.push(["drawdown", `${peaks[i]!.toFixed(2)}%`]);
    return { title: `${clock(at)} IST`, rows };
  };
  const current = valid.at(-1);
  return (
    <div className="chart-module">
      <div className="chart-toolbar">
        <div className="chart-readout">
          <span className="chart-series-dot" />
          {label}
          <strong>{current ? amount(current.value) : "—"}</strong>
          <time>{current ? clock(current.at) + " IST" : ""}</time>
        </div>
        <div className="chart-tools">
          <Segments
            value={range}
            onChange={(v) => {
              setRange(v);
              setFollow(true);
            }}
            items={RANGES}
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
          <TimeChart
            x={x}
            panes={panes}
            bands={bands}
            tooltip={tooltip}
            frozen={!follow}
            onSelect={() => setFollow(false)}
            onError={() => setFailed(true)}
            ariaLabel={`${label}, ${valid.length} observations${plotted < points.length ? `, ${plotted} plotted with extrema preserved` : ""}. Drag to zoom; use Follow to reset.`}
          />
        </div>
      ) : (
        <div className="chart-unavailable" style={{ minHeight: height }}>
          <Empty title={failed ? "Chart could not be loaded" : emptyTitle}>
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
