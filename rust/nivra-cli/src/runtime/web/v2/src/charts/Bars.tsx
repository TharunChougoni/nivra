import React from "react";
import { useWidth } from "./useWidth";
export interface Bar {
  label: string;
  value: number;
  /** CSS custom property; defaults to `--accent`. */
  color?: string;
  title?: string;
}
/** SVG bars. Horizontal rows carry a label and count; vertical bars form a histogram. */
export default function Bars({
  items,
  orientation = "horizontal",
  height,
  format = (v) => String(v),
  color = "--accent",
}: {
  items: Bar[];
  orientation?: "horizontal" | "vertical";
  height?: number;
  format?: (value: number) => string;
  color?: string;
}) {
  const [ref, width] = useWidth<HTMLDivElement>(320);
  const max = Math.max(1, ...items.map((i) => i.value));
  if (orientation === "horizontal") {
    const row = 18,
      label = 118,
      count = 52;
    const track = Math.max(10, width - label - count - 12);
    return (
      <div ref={ref}>
        <svg
          className="bars-chart"
          width={width}
          height={items.length * row + 2}
          role="img"
        >
          {items.map((item, i) => (
            <g key={item.label} transform={`translate(0 ${i * row})`}>
              <title>
                {item.title ?? `${item.label}: ${format(item.value)}`}
              </title>
              <text y={12}>{item.label}</text>
              <rect
                x={label}
                y={4}
                width={track}
                height={8}
                style={{ fill: "var(--bg-2)" }}
              />
              <rect
                x={label}
                y={4}
                width={Math.max(item.value ? 1 : 0, (item.value / max) * track)}
                height={8}
                style={{ fill: `var(${item.color ?? color})`, opacity: 0.75 }}
              />
              <text
                x={width - 2}
                y={12}
                textAnchor="end"
                style={{ fill: "var(--fg-1)" }}
              >
                {format(item.value)}
              </text>
            </g>
          ))}
        </svg>
      </div>
    );
  }
  const h = height ?? 64,
    axis = 12,
    slot = width / Math.max(1, items.length);
  return (
    <div ref={ref}>
      <svg className="bars-chart" width={width} height={h + axis} role="img">
        {items.map((item, i) => {
          const bar = (item.value / max) * (h - 4);
          return (
            <g key={item.label}>
              <title>
                {item.title ?? `${item.label}: ${format(item.value)}`}
              </title>
              <rect
                x={i * slot + 1}
                y={h - bar}
                width={Math.max(1, slot - 2)}
                height={bar}
                style={{ fill: `var(${item.color ?? color})`, opacity: 0.75 }}
              />
              {(items.length <= 8 || i % 2 === 0) && slot > 18 && (
                <text
                  x={i * slot + slot / 2}
                  y={h + axis - 2}
                  textAnchor="middle"
                >
                  {item.label}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}
