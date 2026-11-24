import React, { useId } from "react";

/* The Nivra mark: an "N" drawn as one evidence trace that rises from a
   recorded observation (the dot) to a decision. Shared by the brand, the
   unlock page and the favicon in index.html (keep the paths in sync). */
const TRACE = "M11 23V9.5L21 22.5V9";
export function LogoMark({
  size = 18,
  animated = false,
  className = "",
}: {
  size?: number;
  animated?: boolean;
  className?: string;
}) {
  const gradient = useId();
  return (
    <svg
      className={`logo-mark ${animated ? "draw" : ""} ${className}`}
      width={size}
      height={size}
      viewBox="0 0 32 32"
      aria-hidden="true"
      focusable="false"
    >
      <defs>
        <linearGradient id={gradient} x1="0" y1="1" x2="1" y2="0">
          <stop offset="0" stopColor="#8db89a" />
          <stop offset="1" stopColor="#8fa6c0" />
        </linearGradient>
      </defs>
      <rect
        className="logo-tile"
        x="1"
        y="1"
        width="30"
        height="30"
        rx="8"
        fill="#14181d"
        stroke="#2d333c"
      />
      <path
        className="logo-trace"
        d={TRACE}
        fill="none"
        stroke={`url(#${gradient})`}
        strokeWidth="3"
        strokeLinecap="round"
        strokeLinejoin="round"
        pathLength={1}
      />
      <circle className="logo-dot" cx="21" cy="9" r="2.6" fill="#e3e6ea" />
    </svg>
  );
}
