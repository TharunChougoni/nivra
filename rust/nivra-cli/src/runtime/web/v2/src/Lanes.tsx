import type {
  AgentTurn,
  LaneBudget,
  TranscriptExchange,
  TranscriptSession,
} from "./types";
import type { LaneStats } from "./model";
import React, { useEffect, useMemo, useState } from "react";
import { Columns2 } from "lucide-react";
import {
  clock,
  count,
  duration,
  fillsFor,
  finance,
  numeric,
  usd,
} from "./data";
import { plain, sentenceCase, turnOutcome } from "./labels";
import {
  LANE_ORDER,
  instrumentIndex,
  journalCoverage,
  laneStats,
  turnFailed,
} from "./model";
import { Empty, LaneSwatch, Notice, Panel, Segments, useWorkspace } from "./ui";
import Bars from "./charts/Bars";
import LaneTimeline, { laneColor, sessionRange } from "./charts/LaneTimeline";
import { TimeChart } from "./charts/TimeChart";
import { useRead, useRevision } from "./reads";
import { LaneThread } from "./LaneChat";

const PAGE = 20;
const VIEW_KEY = "nivra.lanes.view";
const EMPTY_HINT: Record<string, string> = {
  review: "Runs after the market closes.",
  research: "Runs when requested.",
};

/** Daily budget rows; absent budget fields stay unknown, never zero. */
function laneBudgetRows(b: LaneBudget | undefined): [string, string][] {
  if (!b) return [["Budget", "Not reported"]];
  const of = (used: string, total: string) => `${used} of ${total}`;
  return [
    ["Model", [b.provider, b.model].filter(Boolean).join(" · ") || "—"],
    ["Calls today", of(count(b.current_usage), count(b.daily_call_budget))],
    ["Spend today", of(usd(b.current_cost_usd), usd(b.daily_cost_usd))],
    ...(b.status && b.status.toLowerCase() !== "unknown"
      ? [["Status", plain(b.status)] as [string, string]]
      : []),
  ];
}
/** Gate turns → escalated → decision turns → validated plans → actions → fills; recorded counts only. */
function GateFunnel({ turns }: { turns: AgentTurn[] }) {
  const [state] = useWorkspace();
  const gate = turns.filter((t) => t.lane === "gate"),
    decisions = turns.filter((t) => t.lane === "decision");
  const escalated =
    numeric(state.agent?.triggers?.gate_escalated) ??
    (gate.length ? gate.filter((t) => t.verdict?.escalate).length : null);
  const plans = decisions.filter((t) => t.plan);
  const stages: [string, number | null][] = [
    ["Gate turns", gate.length || null],
    ["Escalated", escalated],
    ["Decision turns", decisions.length || null],
    [
      "Validated plans",
      plans.length
        ? plans.filter((t) => t.plan?.status === "validated").length
        : null,
    ],
    [
      "Actions",
      plans.length
        ? plans.reduce((n, t) => n + (t.plan?.actions ?? 0), 0)
        : null,
    ],
    ["Fills", numeric(state.fills?.total ?? state.fills?.items?.length)],
  ];
  const known = stages.filter((s): s is [string, number] => s[1] !== null);
  return (
    <Panel
      title="Gate funnel"
      note={state.agent?.truncated ? "Loaded turns only" : undefined}
    >
      {known.length ? (
        <div className="panel-body">
          <Bars items={known.map(([label, value]) => ({ label, value }))} />
        </div>
      ) : (
        <Empty title="No counts yet" />
      )}
    </Panel>
  );
}
function CostChart({ turns, lanes }: { turns: AgentTurn[]; lanes: string[] }) {
  const priced = useMemo(
    () =>
      turns
        .filter(
          (t) =>
            numeric(t.at_ms) !== null && numeric(t.usage?.cost_usd) !== null,
        )
        .sort((a, b) => a.at_ms! - b.at_ms!),
    [turns],
  );
  const x = useMemo(() => priced.map((t) => t.at_ms! / 1000), [priced]);
  const panes = useMemo(
    () => [
      {
        height: 170,
        format: (v: number) => "$" + v.toFixed(2),
        yRange: (_min: number, max: number): [number, number] => [
          0,
          max * 1.1 || 1,
        ],
        series: lanes.map((lane) => {
          let total = 0;
          return {
            label: sentenceCase(lane),
            color: laneColor(lane),
            step: true,
            width: 1.4,
            data: priced.map((t) => {
              if (t.lane === lane) total += Number(t.usage!.cost_usd);
              return total;
            }),
          };
        }),
      },
    ],
    [priced, lanes],
  );
  if (priced.length < 2)
    return (
      <Empty title="Not enough cost data">
        Needs at least two turns with a cost.
      </Empty>
    );
  return (
    <TimeChart
      x={x}
      panes={panes}
      ariaLabel="Cumulative cost by lane"
      tooltip={(i) => ({
        title: clock(x[i] * 1000) + " IST",
        rows: panes[0].series.map(
          (s) => [s.label, usd(s.data[i])] as [string, string],
        ),
      })}
    />
  );
}
const EDGES = [100, 250, 500, 1000, 2500, 5000, 10000, 30000, 100000];
const edgeLabel = (ms: number) => (ms < 1000 ? `${ms}` : `${ms / 1000}s`);
function LatencyHistograms({
  turns,
  lanes,
}: {
  turns: AgentTurn[];
  lanes: string[];
}) {
  return (
    <div className="latency-grid">
      {lanes.map((lane) => {
        const counts = new Array(EDGES.length + 1).fill(0);
        for (const t of turns) {
          const ms = numeric(t.elapsed_ms);
          if (t.lane !== lane || ms === null) continue;
          const i = EDGES.findIndex((e) => ms < e);
          counts[i < 0 ? EDGES.length : i]++;
        }
        return (
          <div key={lane}>
            <LaneSwatch lane={lane} />
            <Bars
              orientation="vertical"
              color={laneColor(lane)}
              height={54}
              items={counts.map((value, i) => ({
                label: i < EDGES.length ? edgeLabel(EDGES[i]) : "∞",
                value,
                title: `${i ? edgeLabel(EDGES[i - 1]) : "0"} – ${i < EDGES.length ? edgeLabel(EDGES[i]) : "∞"}: ${value} turns`,
              }))}
            />
          </div>
        );
      })}
    </div>
  );
}

interface View {
  lane: string;
  split: boolean;
  right: string;
}
/** The remembered tab and split mode; storage may be blocked or hold junk. */
function readView(): Partial<View> {
  try {
    const v = JSON.parse(localStorage.getItem(VIEW_KEY) || "null");
    return {
      lane: typeof v?.lane === "string" ? v.lane : undefined,
      split: v?.split === true,
      right: typeof v?.right === "string" ? v.right : undefined,
    };
  } catch {
    return {};
  }
}
function saveView(view: View) {
  try {
    localStorage.setItem(VIEW_KEY, JSON.stringify(view));
  } catch {
    /* the page works without storage */
  }
}

/** One lane's numbers: loaded turns, API usage totals and the daily budget. */
function LaneStatsStrip({
  stat,
  budget,
}: {
  stat: LaneStats;
  budget?: LaneBudget;
}) {
  const cells: [string, string][] = [
    ["Turns / errors", `${count(stat.turns)} / ${count(stat.failed)}`],
    ["Fallbacks", count(stat.fallbacks)],
    [
      "Reply time (typical / slow)",
      `${duration(stat.p50_ms)} / ${duration(stat.p95_ms)}`,
    ],
    ["Tokens in / out", `${count(stat.input)} / ${count(stat.output)}`],
    ["Cost", usd(stat.cost_usd)],
    ["Calls", count(stat.calls)],
    ...laneBudgetRows(budget),
    [
      "Last turn",
      stat.last ? `${clock(stat.last.at_ms)} · ${turnOutcome(stat.last)}` : "—",
    ],
  ];
  return (
    <dl className="lane-stats" aria-label={`${sentenceCase(stat.lane)} stats`}>
      {cells.map(([k, v]) => (
        <div key={k}>
          <dt>{k}</dt>
          <dd title={v}>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

/** One lane as a chat thread over its stats strip. Remount per lane to reset paging. */
function LaneColumn({
  lane,
  stat,
  budget,
  journalMissing,
  picker,
}: {
  lane: string;
  stat: LaneStats;
  budget?: LaneBudget;
  journalMissing: boolean;
  picker?: React.ReactNode;
}) {
  const [state, app] = useWorkspace(),
    revision = useRevision("agent_turns"),
    // Reloads the newest page only when a turn row lands.
    first = useRead(
      (api) => api.agentTranscript({ lane, limit: PAGE }),
      lane + "|" + revision,
    ),
    [older, setOlder] = useState<{
      exchanges: TranscriptExchange[];
      sessions: TranscriptSession[];
      next: string | null | undefined;
      busy: boolean;
      failed: boolean;
    }>({
      exchanges: [],
      sessions: [],
      next: undefined,
      busy: false,
      failed: false,
    });
  const exchanges = useMemo(() => {
    const seen = new Set<string>();
    return [...(first.value?.exchanges ?? []), ...older.exchanges].filter(
      (e) => {
        const key = `${e.session}:${e.at_ms}`;
        return !seen.has(key) && !!seen.add(key);
      },
    );
  }, [first.value, older.exchanges]);
  const sessions = useMemo(
    () =>
      new Map(
        [...(first.value?.sessions ?? []), ...older.sessions].map((s) => [
          s.id,
          s,
        ]),
      ),
    [first.value, older.sessions],
  );
  const record = app.record(),
    index = useMemo(
      () =>
        instrumentIndex({
          positions: finance(record).positions,
          fills: fillsFor(state, record),
          scanner: state.scanner?.items,
        }),
      [record, state],
    );
  const cursor = older.next !== undefined ? older.next : first.value?.next;
  const loadMore = async () => {
    if (!cursor) return;
    setOlder((o) => ({ ...o, busy: true, failed: false }));
    try {
      const page = await app.api.agentTranscript({
        lane,
        cursor,
        limit: PAGE,
      });
      setOlder((o) => ({
        exchanges: [...o.exchanges, ...(page.exchanges ?? [])],
        sessions: [...o.sessions, ...(page.sessions ?? [])],
        next: page.next ?? null,
        busy: false,
        failed: false,
      }));
    } catch {
      setOlder((o) => ({ ...o, busy: false, failed: true }));
    }
  };
  let body: React.ReactNode;
  if (first.state === "error")
    body = (
      <Empty tone="failed" title="Could not load this lane">
        Other lanes are unaffected.
      </Empty>
    );
  else if (first.state === "unavailable")
    body = <Empty tone="unavailable" title="No transcript for this lane" />;
  else if (!first.value) body = <p className="panel-note">Loading…</p>;
  else if (!exchanges.length)
    body = <Empty title="No turns yet">{EMPTY_HINT[lane]}</Empty>;
  else
    body = (
      <LaneThread
        exchanges={exchanges}
        sessions={sessions}
        index={index}
        hasMore={!!cursor}
        loadingMore={older.busy}
        moreFailed={older.failed}
        onLoadMore={() => void loadMore()}
      />
    );
  return (
    <section className="lane-col" aria-label={`${lane} lane`}>
      {picker && (
        <header className="lane-col-head">
          <LaneSwatch lane={lane} />
          {picker}
        </header>
      )}
      {body}
      {journalMissing ? (
        <p className="panel-note">
          Stats are unknown until lane history loads.
        </p>
      ) : (
        <LaneStatsStrip stat={stat} budget={budget} />
      )}
    </section>
  );
}

function SessionCharts({
  turns,
  lanes,
}: {
  turns: AgentTurn[];
  lanes: string[];
}) {
  const [state] = useWorkspace(),
    [open, setOpen] = useState(false),
    [laneFilter, setLaneFilter] = useState("all");
  const range = useMemo(
    () => sessionRange(state.summary?.session, turns),
    [state.summary?.session, turns],
  );
  const shownLanes = lanes.filter(
    (l) => laneFilter === "all" || l === laneFilter,
  );
  const active = lanes.filter((l) => turns.some((t) => t.lane === l));
  return (
    <details
      className="session-charts"
      onToggle={(e) => setOpen(e.currentTarget.open)}
    >
      <summary>Session charts</summary>
      {open && (
        <div className="session-charts-body">
          <Panel
            title="Timeline"
            note="Taller = slower · hollow = failed · ◆ gate escalated"
            actions={
              <Segments
                value={laneFilter}
                onChange={setLaneFilter}
                label="Lane filter"
                items={[
                  ["all", "All"],
                  ...lanes.map((l) => [l, sentenceCase(l)]),
                ]}
              />
            }
          >
            {turns.length ? (
              <div className="panel-body">
                <LaneTimeline
                  turns={turns}
                  lanes={shownLanes}
                  range={range}
                  now={numeric(state.summary?.as_of_ms)}
                  rowHeight={laneFilter === "all" ? 40 : 90}
                />
              </div>
            ) : (
              <Empty title="No turns recorded" />
            )}
          </Panel>
          <div className="cols-2">
            <GateFunnel turns={turns} />
            <Panel title="Reply times" note="Turns by reply time (ms)">
              <div className="panel-body">
                <LatencyHistograms turns={turns} lanes={active} />
              </div>
            </Panel>
          </div>
          <Panel title="Cumulative cost by lane" note="USD, running total">
            <CostChart turns={turns} lanes={active} />
          </Panel>
        </div>
      )}
    </details>
  );
}

export default function Lanes() {
  const [state, app] = useWorkspace(),
    [view, setView] = useState<Partial<View>>(readView);
  const turns = state.agent?.turns ?? [],
    coverage = journalCoverage(state.agent);
  // The controller reloads the journal when `revisions.agent` changes, on
  // run_ended and on visibility; this page only asks once if it never loaded.
  useEffect(() => {
    if (!state.agent && state.agentState === "idle") void app.loadAgent();
  }, [app]);
  const lanes = useMemo(
      () => [...new Set([...LANE_ORDER, ...turns.map((t) => t.lane)])],
      [turns],
    ),
    stats = useMemo(
      () => laneStats(turns, state.agent?.usage),
      [turns, state.agent],
    );
  // Budgets move only with lane usage, which the agent revision covers.
  const health = useRead((api) => api.health(), useRevision("agent")),
    budgets = new Map<string, LaneBudget>(
      (health.value?.readiness?.models?.roles ?? []).map((b) => [b.role, b]),
    );
  const lane = view.lane && lanes.includes(view.lane) ? view.lane : lanes[0],
    split = view.split === true,
    other = lane === "gate" ? "decision" : "gate",
    right =
      view.right && view.right !== lane && lanes.includes(view.right)
        ? view.right
        : other;
  useEffect(() => saveView({ lane, split, right }), [lane, split, right]);
  const journalMissing =
    (state.agentState === "error" || state.agentState === "unavailable") &&
    !turns.length;
  const statFor = (l: string) =>
    stats.find((s) => s.lane === l) ?? laneStats([], null)[0];
  const column = (l: string, picker?: React.ReactNode) => (
    <LaneColumn
      key={l}
      lane={l}
      stat={statFor(l)}
      budget={budgets.get(l)}
      journalMissing={journalMissing}
      picker={picker}
    />
  );
  return (
    <>
      {(state.agentState === "error" || state.agentState === "unavailable") && (
        <Notice error>
          Lane history is{" "}
          {state.agentState === "error" ? "unavailable" : "not reported"}.
          {journalMissing && " Counts stay unknown."}
        </Notice>
      )}
      {coverage.label && (
        <Notice>
          {sentenceCase(coverage.label)}. Counts cover loaded turns only.
        </Notice>
      )}
      <div className="lane-tabbar">
        <div className="lane-tabs" role="tablist" aria-label="Lane">
          {lanes.map((l) => {
            const s = statFor(l),
              tone = journalMissing
                ? "muted"
                : s.last && turnFailed(s.last)
                  ? "neg"
                  : s.turns
                    ? "pos"
                    : "muted";
            return (
              <button
                key={l}
                role="tab"
                aria-selected={l === lane}
                className="lane-tab"
                onClick={() => setView({ ...view, lane: l })}
              >
                <i className={`lane-dot ${tone}`} aria-hidden="true" />
                {sentenceCase(l)}
                <span className="num muted">
                  {journalMissing
                    ? "—"
                    : count(s.turns) +
                      (s.failed ? ` · ${s.failed} failed` : "")}
                </span>
              </button>
            );
          })}
        </div>
        <button
          className="lane-split"
          aria-pressed={split}
          onClick={() => setView({ ...view, split: !split })}
        >
          <Columns2 size={14} aria-hidden="true" />
          Side by side
        </button>
      </div>
      <div className={split ? "lane-view lane-view-split" : "lane-view"}>
        {column(lane)}
        {split &&
          column(
            right,
            <select
              aria-label="Right lane"
              value={right}
              onChange={(e) => setView({ ...view, right: e.target.value })}
            >
              {lanes
                .filter((l) => l !== lane)
                .map((l) => (
                  <option key={l} value={l}>
                    {sentenceCase(l)}
                  </option>
                ))}
            </select>,
          )}
      </div>
      <SessionCharts turns={turns} lanes={lanes} />
    </>
  );
}
