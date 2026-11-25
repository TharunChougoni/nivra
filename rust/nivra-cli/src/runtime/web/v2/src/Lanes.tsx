import type {
  AgentTurn,
  LaneBudget,
  TranscriptExchange,
  TranscriptSession,
} from "./types";
import type { LaneStats } from "./model";
import React, { useEffect, useMemo, useState } from "react";
import { Columns2 } from "lucide-react";
import { clock, count, duration, numeric, usd, words } from "./data";
import {
  LANE_ORDER,
  journalCoverage,
  laneStats,
  turnFailed,
  turnOutcome,
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
  review: "Runs after the close seal.",
  research: "Runs on research requests.",
};

/** Daily budget rows; absent budget fields stay unknown, never zero. */
function laneBudgetRows(b: LaneBudget | undefined): [string, string][] {
  if (!b) return [["budget", "not reported"]];
  const of = (used: string, total: string) => `${used} of ${total}`;
  return [
    ["model", [b.provider, b.model].filter(Boolean).join(" · ") || "—"],
    ["calls today", of(count(b.current_usage), count(b.daily_call_budget))],
    ["spend today", of(usd(b.current_cost_usd), usd(b.daily_cost_usd))],
    ["status", words(b.status)],
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
    ["gate turns", gate.length || null],
    ["escalated", escalated],
    ["decision turns", decisions.length || null],
    [
      "validated plans",
      plans.length
        ? plans.filter((t) => t.plan?.status === "validated").length
        : null,
    ],
    [
      "actions",
      plans.length
        ? plans.reduce((n, t) => n + (t.plan?.actions ?? 0), 0)
        : null,
    ],
    ["fills", numeric(state.fills?.total ?? state.fills?.items?.length)],
  ];
  const known = stages.filter((s): s is [string, number] => s[1] !== null);
  return (
    <Panel
      title="Gate funnel"
      note={
        state.agent?.truncated
          ? "recorded counts · loaded turns only"
          : "recorded counts only"
      }
    >
      {known.length ? (
        <div className="panel-body">
          <Bars items={known.map(([label, value]) => ({ label, value }))} />
        </div>
      ) : (
        <Empty title="No funnel counts recorded" />
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
            label: lane,
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
      <Empty title="Not enough priced turns">
        Cumulative cost needs at least two turns with recorded cost.
      </Empty>
    );
  return (
    <TimeChart
      x={x}
      panes={panes}
      ariaLabel="Cumulative pi cost by lane"
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
    ["turns · errors", `${count(stat.turns)} · ${count(stat.failed)}`],
    ["fallbacks", count(stat.fallbacks)],
    [
      "latency p50 / p95",
      `${duration(stat.p50_ms)} / ${duration(stat.p95_ms)}`,
    ],
    ["tokens in / out", `${count(stat.input)} / ${count(stat.output)}`],
    ["cache read", count(stat.cache_read)],
    ["cost", usd(stat.cost_usd)],
    ["calls", count(stat.calls)],
    ...laneBudgetRows(budget),
    [
      "last turn",
      stat.last ? `${clock(stat.last.at_ms)} · ${turnOutcome(stat.last)}` : "—",
    ],
  ];
  return (
    <dl className="lane-stats" aria-label={`${stat.lane} stats`}>
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
  const [, app] = useWorkspace(),
    revision = useRevision("agent"),
    // Reloads the newest page whenever the agent revision changes.
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
      <Empty tone="failed" title="The lane transcript could not be read">
        This lane shows no history until the transcript loads again. Other lanes
        and the stats below are unaffected.
      </Empty>
    );
  else if (first.state === "unavailable")
    body = (
      <Empty tone="unavailable" title="The lane transcript is not reported">
        The controller has no pi session transcripts to show for this lane.
      </Empty>
    );
  else if (!first.value)
    body = <p className="panel-note">Loading lane transcript…</p>;
  else if (!exchanges.length)
    body = (
      <Empty title="No turns yet">
        {EMPTY_HINT[lane] ?? "Turns appear after an eligible pi lane runs."}
      </Empty>
    );
  else
    body = (
      <LaneThread
        exchanges={exchanges}
        sessions={sessions}
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
          Lane stats are unknown until the pi lane journal loads.
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
            note="x = session time IST · height = log latency · hollow = failed · ◆ gate escalated"
            actions={
              <Segments
                value={laneFilter}
                onChange={setLaneFilter}
                label="Lane filter"
                items={[["all", "all"], ...lanes.map((l) => [l, l])]}
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
              <Empty title="No lane turns recorded">
                Turns appear after an eligible pi lane runs.
              </Empty>
            )}
          </Panel>
          <div className="cols-2">
            <GateFunnel turns={turns} />
            <Panel
              title="Latency distribution"
              note="turns per log-spaced latency bucket (ms)"
            >
              <div className="panel-body">
                <LatencyHistograms turns={turns} lanes={active} />
              </div>
            </Panel>
          </div>
          <Panel
            title="Cumulative cost by lane"
            note="stepped · USD · from recorded per-turn cost"
          >
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
          The pi lane journal is{" "}
          {state.agentState === "error" ? "unavailable" : "not reported"}.
          {journalMissing &&
            " Turn counts, latency, tokens and cost stay unknown, and no lane is shown as idle."}
        </Notice>
      )}
      {coverage.label && (
        <Notice>
          Lane journal: {coverage.label}. Counts below cover the loaded turns
          only; API usage totals are shown as reported.
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
                {l}
                <span className="num muted">
                  {journalMissing
                    ? "—"
                    : count(s.turns) + (s.failed ? ` · ${s.failed} err` : "")}
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
                    {l}
                  </option>
                ))}
            </select>,
          )}
      </div>
      <SessionCharts turns={turns} lanes={lanes} />
    </>
  );
}
