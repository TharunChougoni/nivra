import React from "react";
/** Counts per equal-width bucket over [from, to]; times outside the range are ignored. */
export function bucketize(
  times: number[],
  from: number,
  to: number,
  buckets = 60,
) {
  const out = new Array<number>(buckets).fill(0);
  const span = to - from;
  if (!(span > 0)) return out;
  for (const t of times) {
    if (t < from || t > to) continue;
    out[Math.min(buckets - 1, Math.floor(((t - from) / span) * buckets))]++;
  }
  return out;
}
/** Tiny SVG activity bars. `color` is a CSS custom property such as `--lane-gate`. */
export default function Sparkline({
  values,
  color = "--accent",
  height = 18,
  label,
}: {
  values: number[];
  color?: string;
  height?: number;
  label?: string;
}) {
  const max = Math.max(1, ...values);
  const step = 100 / Math.max(1, values.length);
  return (
    <svg
      className="spark"
      width="100%"
      height={height}
      viewBox={`0 0 100 ${height}`}
      preserveAspectRatio="none"
      role="img"
      aria-label={label}
    >
      {values.map((v, i) =>
        v ? (
          <rect
            key={i}
            x={i * step}
            width={Math.max(step * 0.7, 0.3)}
            y={height - (v / max) * (height - 2) - 1}
            height={(v / max) * (height - 2) + 1}
            style={{ fill: `var(${color})`, opacity: 0.75 }}
          />
        ) : null,
      )}
    </svg>
  );
}
