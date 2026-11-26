import React, { useEffect, useMemo, useState } from "react";
import { ApiError } from "./api";
import Chart from "./Chart";
import type { FillMarker } from "./Chart";
import {
  istDateString,
  istDayEndMs,
  istDayStartMs,
  numeric,
  projectSeries,
  shiftIstDate,
} from "./data";
import {
  ARCHIVE_LABELS,
  ARCHIVE_RANGE_LABELS,
  instrumentName,
  plain,
} from "./labels";
import { liveSeriesEmpty, priceSeriesSourceLabel } from "./LivePriceChart";
import { instrumentLabel } from "./model";
import type { InstrumentRecord } from "./model";
import type {
  Fill,
  Point,
  ScannerRow,
  SeriesCoverage,
  WorkspaceState,
} from "./types";
import { Button, Panel, Segments, useWorkspace } from "./ui";

type HistoryPreset = "today" | "5d" | "7d" | "30d" | "90d" | "custom";
type ArchiveKind = "archive-minute-bar" | "archive-daily-bar";

interface ArchiveSeriesState {
  status: "idle" | "loading" | "ready" | "unavailable" | "error";
  points: Point[];
  coverage: SeriesCoverage | null;
  reason: string | null;
  kind: ArchiveKind;
}

const emptyArchive = (
  kind: ArchiveKind = "archive-minute-bar",
): ArchiveSeriesState => ({
  status: "idle",
  points: [],
  coverage: null,
  reason: null,
  kind,
});

function mergeArchiveCoverages(
  coverages: (SeriesCoverage | null)[],
): SeriesCoverage | null {
  const valid = coverages.filter((c): c is SeriesCoverage => c !== null);
  if (!valid.length) return null;
  const fromValues = valid
    .map((c) => numeric(c.from_ms))
    .filter((v): v is number => v !== null);
  const toValues = valid
    .map((c) => numeric(c.to_ms))
    .filter((v): v is number => v !== null);
  return {
    ...valid[0],
    from_ms: fromValues.length ? Math.min(...fromValues) : null,
    to_ms: toValues.length ? Math.max(...toValues) : null,
    observed: valid.reduce((sum, c) => sum + (numeric(c.observed) ?? 0), 0),
    expected: valid.reduce((sum, c) => sum + (numeric(c.expected) ?? 0), 0),
    gap_count: valid.reduce((sum, c) => sum + (numeric(c.gap_count) ?? 0), 0),
    gaps: valid.flatMap((c) => c.gaps ?? []),
  };
}

export function OverviewChartsPanel({
  historical,
  todaySession,
  prospects,
  index,
  baseline,
  fills,
}: {
  historical: boolean;
  todaySession: string | null | undefined;
  prospects: ScannerRow[];
  index: Map<string, InstrumentRecord>;
  baseline: number | null;
  fills: Fill[];
}) {
  const [state, app] = useWorkspace(),
    [view, setView] = useState("equity"),
    [historyPreset, setHistoryPreset] = useState<HistoryPreset>("today"),
    [draftFrom, setDraftFrom] = useState(""),
    [draftTo, setDraftTo] = useState(""),
    [draftKind, setDraftKind] = useState<ArchiveKind>("archive-minute-bar"),
    [customRequest, setCustomRequest] = useState<{
      fromDate: string;
      toDate: string;
      kind: ArchiveKind;
      seq: number;
    } | null>(null),
    [archive, setArchive] = useState<ArchiveSeriesState>(() => emptyArchive());

  const asOf = numeric(state.summary?.as_of_ms);
  const todayIst = todaySession || istDateString(asOf ?? Date.now());

  const archiveRequest = useMemo(() => {
    if (view === "equity" || historyPreset === "today" || !state.watch)
      return null;
    if (historyPreset === "custom") {
      if (!customRequest) return null;
      return {
        instrument: state.watch,
        kind: customRequest.kind,
        fromDate: customRequest.fromDate,
        toDate: customRequest.toDate,
        seq: customRequest.seq,
      };
    }
    const lookbackDays =
      historyPreset === "5d"
        ? 4
        : historyPreset === "7d"
          ? 6
          : historyPreset === "30d"
            ? 29
            : 89;
    const reqKind: ArchiveKind =
      historyPreset === "5d" || historyPreset === "7d"
        ? "archive-minute-bar"
        : "archive-daily-bar";
    return {
      instrument: state.watch,
      kind: reqKind,
      fromDate: shiftIstDate(todayIst, -lookbackDays),
      toDate: todayIst,
      seq: 0,
    };
  }, [view, historyPreset, state.watch, todayIst, customRequest]);

  useEffect(() => {
    if (!archiveRequest) {
      setArchive(emptyArchive());
      return;
    }
    const from = istDayStartMs(archiveRequest.fromDate);
    const to = istDayEndMs(archiveRequest.toDate);
    if (from === null || to === null || to < from) {
      setArchive({
        status: "error",
        points: [],
        coverage: null,
        reason: "series_range_invalid",
        kind: archiveRequest.kind,
      });
      return;
    }
    let cancelled = false;
    setArchive({
      status: "loading",
      points: [],
      coverage: null,
      reason: null,
      kind: archiveRequest.kind,
    });
    (async () => {
      try {
        const collected: Point[] = [];
        const coverages: (SeriesCoverage | null)[] = [];
        let cursorTo = to;
        for (let page = 0; page < 5; page++) {
          const body = await app.api.series({
            kind: archiveRequest.kind,
            instrument: archiveRequest.instrument,
            from,
            to: cursorTo,
          });
          if (cancelled) return;
          if (body?.unavailable) {
            setArchive({
              status: "unavailable",
              points: [],
              coverage: null,
              reason: body.reason || "archive_unavailable",
              kind: archiveRequest.kind,
            });
            return;
          }
          const projected = projectSeries(body, archiveRequest.kind);
          coverages.push(projected.coverage);
          collected.push(...projected.points);
          if (!body?.truncated || body.next == null || body.next < from) break;
          cursorTo = body.next;
        }
        collected.sort((a, b) => a.at - b.at);
        const deduped: Point[] = [];
        for (const p of collected) {
          const prev = deduped.at(-1);
          if (prev && prev.at === p.at) deduped[deduped.length - 1] = p;
          else deduped.push(p);
        }
        if (!cancelled)
          setArchive({
            status: "ready",
            points: deduped,
            coverage: mergeArchiveCoverages(coverages),
            reason: null,
            kind: archiveRequest.kind,
          });
      } catch (e) {
        if (!cancelled)
          setArchive({
            status: "error",
            points: [],
            coverage: null,
            reason: e instanceof ApiError ? e.code : "archive_unavailable",
            kind: archiveRequest.kind,
          });
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [archiveRequest, app]);

  const selectHistoryPreset = (next: HistoryPreset) => {
    setHistoryPreset(next);
    if (next === "custom") {
      const fromDate = draftFrom || shiftIstDate(todayIst, -4);
      const toDate = draftTo || todayIst;
      setDraftFrom(fromDate);
      setDraftTo(toDate);
      setCustomRequest((prev) => ({
        fromDate,
        toDate,
        kind: draftKind,
        seq: (prev?.seq ?? 0) + 1,
      }));
    }
  };

  const applyCustomRange = () => {
    const fromDate = draftFrom || shiftIstDate(todayIst, -4);
    const toDate = draftTo || todayIst;
    setCustomRequest((prev) => ({
      fromDate,
      toDate,
      kind: draftKind,
      seq: (prev?.seq ?? 0) + 1,
    }));
  };

  const series = state.series || {};
  const equitySeries = series.equity?.length
    ? series.equity
    : state.equityPoints;
  const priceSeries = series.price?.length ? series.price : state.points;
  const selected = prospects.find((v) => v.instrument_key === state.watch);
  const watchInProspects = !!selected;
  const watchLabel = state.watch
    ? selected?.symbol || instrumentName(state.watch, index)
    : "Selected instrument";
  const showEquity = view === "equity";
  const showArchive = !showEquity && historyPreset !== "today";
  const markers: FillMarker[] = fills.flatMap((fill) => {
    const at = numeric(fill.at);
    return at === null || (!showEquity && fill.instrument?.key !== state.watch)
      ? []
      : [{ at, side: fill.buy ? ("buy" as const) : ("sell" as const) }];
  });
  const points = showArchive
    ? archive.points
    : historical
      ? []
      : showEquity
        ? equitySeries
        : priceSeries;
  const active = showEquity ? series.equity : series.price;
  const cached =
    !showArchive &&
    !!active?.length &&
    (showEquity ? series.equityCached : series.priceCached);

  const liveEmpty = historical
    ? {
        title: showEquity ? "No equity history" : "No price history",
        detail: "This archive has one checkpoint only.",
      }
    : liveSeriesEmpty(
        showEquity ? series.equityState : series.priceState,
        showEquity ? series.equityNote : series.priceNote,
        showEquity || !!state.watch,
        showEquity ? "equity" : "price",
      );

  const archiveEmpty = !state.watch
    ? {
        tone: undefined,
        title: "No instrument selected",
        detail: "Choose an instrument from the scanner or portfolio.",
      }
    : {
        tone: (archive.status === "unavailable"
          ? "unavailable"
          : archive.status === "error"
            ? "failed"
            : "empty") as "unavailable" | "failed" | "empty",
        title:
          archive.status === "unavailable"
            ? "Archive unavailable"
            : archive.status === "error"
              ? "Could not load archive history"
              : "No archive history",
        detail:
          archive.status === "loading" || archive.status === "idle"
            ? "Loading…"
            : archive.status === "unavailable" || archive.status === "error"
              ? plain(archive.reason, ARCHIVE_LABELS)
              : "No archived bars for this range.",
      };

  const chartSource = showArchive
    ? plain("market_archive", ARCHIVE_LABELS)
    : historical
      ? "Archive has no chart history"
      : showEquity
        ? cached
          ? "Cached, may be stale"
          : active?.length
            ? series.equitySource
              ? "Recorded checkpoints"
              : "Seen in this tab"
            : "Seen in this tab"
        : priceSeriesSourceLabel(series);

  return (
    <Panel
      className="span-8"
      title={showEquity ? "Account equity" : "Instrument price"}
      note={
        showArchive
          ? ARCHIVE_RANGE_LABELS[historyPreset]
          : historical
            ? "Archived session"
            : showEquity
              ? "Fills marked, drawdown below"
              : "Last received price"
      }
      actions={
        <>
          {!showEquity && (
            <>
              <select
                aria-label="Chart instrument"
                value={state.watch}
                onChange={(e) => app.watch(e.target.value)}
              >
                {!prospects.length && !state.watch && (
                  <option value="">No instruments</option>
                )}
                {state.watch && !watchInProspects && (
                  <option value={state.watch}>{watchLabel}</option>
                )}
                {prospects.map((p) => (
                  <option key={p.instrument_key} value={p.instrument_key}>
                    {
                      instrumentLabel({ ...p.instrument, symbol: p.symbol })
                        .primary
                    }
                  </option>
                ))}
              </select>
              <select
                aria-label="History range"
                value={historyPreset}
                onChange={(e) =>
                  selectHistoryPreset(e.target.value as HistoryPreset)
                }
              >
                {Object.entries(ARCHIVE_RANGE_LABELS).map(([id, text]) => (
                  <option key={id} value={id}>
                    {text}
                  </option>
                ))}
              </select>
            </>
          )}
          <Segments
            value={view}
            onChange={setView}
            items={[
              ["equity", "Equity"],
              ["price", "Instrument price"],
            ]}
            label="Chart series"
          />
        </>
      }
    >
      {!showEquity && historyPreset === "custom" && (
        <div className="filter-bar">
          <div className="filter-fields">
            <label>
              From{" "}
              <input
                type="date"
                aria-label="History start date"
                value={draftFrom}
                onChange={(e) => setDraftFrom(e.target.value)}
              />
            </label>
            <label>
              To{" "}
              <input
                type="date"
                aria-label="History end date"
                value={draftTo}
                onChange={(e) => setDraftTo(e.target.value)}
              />
            </label>
            <label>
              Interval{" "}
              <select
                aria-label="History interval"
                value={draftKind}
                onChange={(e) => setDraftKind(e.target.value as ArchiveKind)}
              >
                <option value="archive-minute-bar">1 min</option>
                <option value="archive-daily-bar">Daily</option>
              </select>
            </label>
            <Button type="button" onClick={applyCustomRange}>
              Load
            </Button>
          </div>
        </div>
      )}
      <Chart
        points={points}
        seriesKey={
          showEquity
            ? `equity:${historical ? "hist" : "live"}`
            : `price:${state.watch}:${historyPreset}:${customRequest?.seq ?? 0}:${customRequest?.kind ?? ""}`
        }
        coverage={
          showArchive
            ? archive.coverage
            : showEquity
              ? series.equityCoverage
              : series.priceCoverage
        }
        label={showEquity ? "Account equity" : watchLabel}
        baseline={showEquity ? baseline : null}
        drawdown={showEquity}
        markers={showArchive ? [] : markers}
        multiDay={showArchive}
        daily={showArchive && archive.kind === "archive-daily-bar"}
        source={chartSource}
        height={showEquity ? 300 : 280}
        emptyTone={showArchive ? archiveEmpty.tone : liveEmpty.tone}
        emptyTitle={showArchive ? archiveEmpty.title : liveEmpty.title}
        emptyDetail={showArchive ? archiveEmpty.detail : liveEmpty.detail}
      />
    </Panel>
  );
}
