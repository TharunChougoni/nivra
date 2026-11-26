import React from "react";
import { Radio } from "lucide-react";
import {
  amount,
  count,
  datetime,
  duration,
  percent,
  percentPoints,
} from "./data";
import { plain } from "./labels";
import { modeOf } from "./model";
import { yesNo } from "./operational";
import { OperationalPanel } from "./Operational";
import { useRead, useRevision } from "./reads";
import type { HealthResponse } from "./types";
import {
  Empty,
  Facts,
  Instrument,
  Notice,
  Panel,
  ShortId,
  Status,
  Table,
  useWorkspace,
} from "./ui";

function LiveReadiness({ health }: { health: HealthResponse | null }) {
  const [state] = useWorkspace();
  const mode = modeOf(state.meta, state.summary);
  const liveEnabled =
    health?.live_enabled ??
    state.summary?.live_enabled ??
    state.meta?.live_enabled;
  return (
    <Panel title="Live readiness">
      <Facts
        items={[
          [
            "Mode",
            mode.kind === "paper"
              ? "Paper"
              : mode.kind === "live"
                ? "Live"
                : "Unknown",
          ],
          [
            "Order broker",
            mode.kind === "live"
              ? mode.orderBroker
              : state.meta?.broker?.orders || "Not reported",
          ],
          ["Data broker", state.meta?.broker?.data || "Not reported"],
          [
            "Account",
            (mode.kind === "live" ? mode.account : state.meta?.account) ||
              "Not reported",
          ],
          [
            "Live trading",
            liveEnabled === undefined || liveEnabled === null
              ? "Not reported"
              : liveEnabled
                ? "On"
                : "Off",
          ],
          ["Paper only", yesNo(state.meta?.paper_only)],
          ["This dashboard", "Read only"],
        ]}
      />
    </Panel>
  );
}

export function Health() {
  const [state] = useWorkspace(),
    // A stalled controller publishes no new revision; reread its liveness.
    health = useRead(
      (api) => api.health(),
      useRevision("health") + (state.stalled ? "|stalled" : ""),
    ),
    alerts = useRead(
      (api) => api.alerts({ limit: 100 }),
      useRevision("alerts"),
    ),
    s = state.summary,
    h =
      health.value?.history_preparation ||
      s?.readiness?.history ||
      s?.history_preparation,
    // The summary's feed block refreshes at least every 5 s; health's only
    // when its content changes, so its ages would hold still.
    feed = s?.market_feed || health.value?.market_feed,
    readiness = health.value?.readiness || s?.readiness,
    attribution = health.value?.activity_attribution;
  return (
    <>
      <div className="health-summary">
        <div className="health-symbol">
          <Radio size={22} />
        </div>
        <div>
          <h2>System health</h2>
        </div>
        <Status value={readiness?.status} />
      </div>
      <OperationalPanel
        summary={s}
        portfolio={state.portfolio}
        health={health.value}
      />
      {(health.state === "error" || health.state === "unavailable") && (
        <Notice>
          Health data{" "}
          {health.state === "unavailable" ? "unavailable" : "could not be read"}
          .
        </Notice>
      )}
      <div className="cols-2">
        <Panel title="Market data">
          <Facts
            items={[
              ["Mode", plain(feed?.mode)],
              ["Connection", plain(feed?.state)],
              ["Last quote age", duration(feed?.last_tick_age_ms)],
              ["Cached instruments", count(feed?.cached_keys)],
              ["Reconnects", count(feed?.reconnects)],
              ["Fallback", feed ? yesNo(feed.fallback) : "Not reported"],
              ["Reason", plain(feed?.reason)],
            ]}
          />
        </Panel>
        <Panel title="History">
          <Facts
            items={[
              ["Status", plain(h?.status)],
              [
                "Ready / universe",
                count(h?.ready_instruments) + " / " + count(h?.instruments),
              ],
              ["Target session", h?.target_session],
              ["Lifecycle", plain(readiness?.lifecycle?.status)],
              ["Checked", datetime(readiness?.checked_at_ms)],
              ["Failure", plain(h?.failure)],
            ]}
          />
        </Panel>
      </div>
      <Panel
        title="Activity"
        note={
          attribution?.session ? `Session ${attribution.session}` : undefined
        }
      >
        {attribution ? (
          <>
            <div className="cols-2">
              <Facts
                items={[
                  [
                    "Candidates evaluated",
                    count(attribution.conversion?.candidates_evaluated),
                  ],
                  [
                    "Decisions considered",
                    count(attribution.conversion?.decisions_considered),
                  ],
                  [
                    "Actions proposed / validated",
                    `${count(attribution.conversion?.actions_proposed)} / ${count(attribution.conversion?.actions_validated)}`,
                  ],
                  [
                    "Simulated fills",
                    count(attribution.conversion?.fills_executed),
                  ],
                  [
                    "Candidate → decision",
                    percent(attribution.conversion?.candidate_to_decision_rate),
                  ],
                  [
                    "Decision → action",
                    percent(attribution.conversion?.decision_to_action_rate),
                  ],
                  [
                    "Action → fill",
                    percent(attribution.conversion?.action_to_fill_rate),
                  ],
                  [
                    "Overall conversion",
                    percent(attribution.conversion?.overall_conversion_rate),
                  ],
                  [
                    "Abstentions",
                    count(attribution.abstentions?.total_abstentions),
                  ],
                  [
                    "Decision time (typical / slow)",
                    `${count(attribution.latency?.p50_ms)} / ${count(attribution.latency?.p95_ms)} ms`,
                  ],
                ]}
              />
              <Facts
                items={[
                  [
                    "Realized P&L",
                    amount(attribution.financials?.realized_pnl),
                  ],
                  ["Marked P&L", amount(attribution.financials?.marked_pnl)],
                  ["Fees deducted", amount(attribution.financials?.total_fees)],
                  ["Slippage", amount(attribution.financials?.total_slippage)],
                  [
                    "Cost drag",
                    amount(attribution.financials?.total_cost_drag),
                  ],
                  [
                    "Net after cost",
                    amount(attribution.financials?.net_after_cost_pnl),
                  ],
                  [
                    "Traded notional",
                    amount(attribution.turnover?.notional_traded),
                  ],
                  [
                    "Cost drag (bps)",
                    count(attribution.turnover?.total_cost_drag_bps),
                  ],
                  [
                    "Current / max drawdown",
                    `${amount(attribution.drawdown?.current_drawdown)} / ${amount(attribution.drawdown?.max_drawdown)}`,
                  ],
                  [
                    "Max drawdown (%)",
                    percentPoints(attribution.drawdown?.max_drawdown_pct),
                  ],
                  ...(attribution.drawdown_coverage?.reason
                    ? [
                        [
                          "Drawdown coverage",
                          plain(attribution.drawdown_coverage.reason),
                        ] as [string, string],
                      ]
                    : []),
                ]}
              />
            </div>
            <Panel title="By strategy" className="stack-top">
              <Table
                rows={attribution.experiment_attributions || []}
                columns={[
                  {
                    key: "spec",
                    label: "Strategy",
                    render: (row) => (
                      <Instrument
                        symbol={
                          row.spec_id ? plain(row.spec_id) : "Not recorded"
                        }
                        sub={<ShortId value={row.source_hash} />}
                      />
                    ),
                  },
                  {
                    key: "decisions",
                    label: "Decisions",
                    render: (row) => count(row.decisions_count),
                  },
                  {
                    key: "fills",
                    label: "Fills",
                    render: (row) => count(row.fills_count),
                  },
                  {
                    key: "net",
                    label: "Net P&L",
                    render: (row) => amount(row.net_after_cost_pnl),
                  },
                  {
                    key: "trial",
                    label: "Trial",
                    render: (row) => plain(row.trial_status),
                  },
                ]}
                empty="No strategy attribution recorded"
              />
            </Panel>
          </>
        ) : (
          <Empty title="Activity unavailable" />
        )}
      </Panel>
      <Panel title="Alerts">
        {alerts.state === "error" || alerts.state === "unavailable" ? (
          <Empty title="Alerts unavailable">
            {alerts.state === "unavailable"
              ? plain(alerts.value?.reason)
              : "Could not read alerts."}
          </Empty>
        ) : (
          <Table
            rows={Array.isArray(alerts.value?.items) ? alerts.value.items : []}
            columns={[
              {
                key: "severity",
                label: "Severity",
                render: (v) => <Status value={v.severity} />,
              },
              { key: "message", label: "Alert" },
              {
                key: "source",
                label: "Source",
                render: (v) => plain(v.source),
              },
              {
                key: "observed_at_ms",
                label: "Observed",
                render: (v) => datetime(v.observed_at_ms),
              },
            ]}
            empty="No alerts"
          />
        )}
      </Panel>
      <LiveReadiness health={health.value} />
      <div className="cols-2">
        <Panel title="Service details">
          <Facts
            items={[
              ["Quote freshness limit", "2 seconds from receipt"],
              ["Market archive", yesNo(state.meta?.archive_configured)],
              [
                "Health data",
                health.state === "ready"
                  ? `${count(health.value?.alert_count)} alerts`
                  : health.state === "unavailable"
                    ? `Unavailable · ${plain(health.value?.reason)}`
                    : "Not loaded",
              ],
              [
                "Alert data",
                alerts.state === "ready"
                  ? `${count(alerts.value?.total ?? alerts.value?.items?.length)} alerts`
                  : alerts.state === "unavailable"
                    ? `Unavailable · ${plain(alerts.value?.reason)}`
                    : "Not loaded",
              ],
            ]}
          />
        </Panel>
      </div>
    </>
  );
}
