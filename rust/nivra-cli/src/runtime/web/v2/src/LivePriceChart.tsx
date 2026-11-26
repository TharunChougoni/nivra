import React from "react";
import Chart from "./Chart";
import type { FillMarker } from "./Chart";
import { plain } from "./labels";
import type { ChartSeries, WorkspaceState } from "./types";
import type { EmptyTone } from "./ui";

export function priceSeriesSourceLabel(
  series:
    | Partial<Pick<ChartSeries, "price" | "priceCached" | "priceSource">>
    | undefined,
): string {
  if (series?.priceCached && series?.price?.length) {
    return "Cached minute bars, may be stale";
  }
  if (series?.price?.length) {
    return series.priceSource
      ? "Minute bars from received quotes"
      : "Live quotes, this tab";
  }
  return "Seen in this tab";
}

export function liveSeriesEmpty(
  loadState: ChartSeries["priceState"] | undefined,
  note: string | null | undefined,
  hasTarget: boolean,
  emptyKind: "equity" | "price",
): { tone?: EmptyTone; title: string; detail: string } {
  if (!hasTarget) {
    return {
      title: "No instrument selected",
      detail: "Choose an instrument from the scanner or portfolio.",
    };
  }
  if (loadState === "error") {
    return {
      tone: "failed",
      title:
        emptyKind === "equity"
          ? "Could not load equity history"
          : "Could not load price history",
      detail: "The series request failed.",
    };
  }
  if (loadState === "unavailable") {
    return {
      tone: "unavailable",
      title:
        emptyKind === "equity"
          ? "Equity history unavailable"
          : "Price history unavailable",
      detail: note ? plain(note) : "Series is not available yet.",
    };
  }
  if (loadState === "loading" || loadState === "idle") {
    return {
      title: emptyKind === "equity" ? "No equity history" : "No price history",
      detail: "Loading…",
    };
  }
  return {
    title: emptyKind === "equity" ? "No equity history" : "No price history",
    detail:
      emptyKind === "equity"
        ? "No equity checkpoints recorded yet."
        : "No minute bars yet. Missing minutes stay blank.",
  };
}

export function LivePriceChart({
  state,
  label,
  seriesKey,
  height = 240,
  historical = false,
  markers = [],
}: {
  state: Pick<WorkspaceState, "points" | "watch"> & {
    series?: Partial<ChartSeries>;
  };
  label: string;
  seriesKey?: string;
  height?: number;
  historical?: boolean;
  markers?: FillMarker[];
}) {
  const points = historical
    ? []
    : state.series?.price?.length
      ? state.series.price
      : state.points;
  const empty = historical
    ? {
        title: "No price history",
        detail: "This archive has one checkpoint only.",
      }
    : liveSeriesEmpty(
        state.series?.priceState,
        state.series?.priceNote,
        !!state.watch,
        "price",
      );
  return (
    <Chart
      label={label}
      seriesKey={seriesKey}
      points={points}
      coverage={historical ? null : state.series?.priceCoverage}
      markers={historical ? [] : markers}
      height={height}
      source={
        historical
          ? "Archive has no chart history"
          : priceSeriesSourceLabel(state.series)
      }
      emptyTone={empty.tone}
      emptyTitle={empty.title}
      emptyDetail={empty.detail}
    />
  );
}
