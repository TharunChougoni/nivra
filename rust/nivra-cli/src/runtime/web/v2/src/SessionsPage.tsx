import React, { useState } from "react";
import { ArrowUpRight, Search } from "lucide-react";
import { amount, count, datetime, finance, sessionKey, sessions } from "./data";
import { plain } from "./labels";
import { yesNo } from "./operational";
import type { SessionRecord } from "./types";
import {
  Button,
  Drawer,
  Facts,
  Instrument,
  KpiStrip,
  Notice,
  Panel,
  Status,
  Table,
  useWorkspace,
} from "./ui";

function sessionMode(row: SessionRecord) {
  if (row.mode === "paper" || row.mode === "live") return plain(row.mode);
  if (row.paper_only === true && row.live_enabled === false) return "Paper";
  if (row.paper_only === false && row.live_enabled === true) return "Live";
  return "Not reported";
}

export function Sessions() {
  const [state, app] = useWorkspace(),
    [query, setQuery] = useState(""),
    [selected, setSelected] = useState<SessionRecord | null>(null);
  const records = sessions(state, state.meta),
    rows = records.filter((r) =>
      [r.id?.date, r.id?.broker, r.id?.account, r.status, r.phase]
        .join(" ")
        .toLowerCase()
        .includes(query.toLowerCase()),
    );
  const f = finance(selected);
  const archiveUnavailable =
    state.sessionsState === "error" || state.sessionsState === "unavailable";
  return (
    <>
      <Panel
        title="Sessions"
        actions={
          <label className="search-field">
            <Search size={13} />
            <input
              type="search"
              placeholder="Find session or account…"
              aria-label="Search sessions"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </label>
        }
      >
        {archiveUnavailable && (
          <Notice>
            Session archive{" "}
            {state.sessionsState === "unavailable"
              ? "unavailable"
              : "could not be read"}
            .{state.summary ? " Showing the current session only." : ""}
          </Notice>
        )}
        <Table
          getKey={sessionKey}
          rows={rows}
          onSelect={setSelected}
          columns={[
            {
              key: "id",
              label: "Session / account",
              render: (r) => (
                <Instrument symbol={r.id?.date} sub={r.id?.account} />
              ),
            },
            {
              key: "broker",
              label: "Broker",
              render: (r) => r.id?.broker || "Not reported",
            },
            {
              key: "mode",
              label: "Mode",
              render: sessionMode,
            },
            {
              key: "state",
              label: "Status",
              render: (r) => <Status value={r.status || r.phase} />,
            },
            {
              key: "evidence",
              label: "Evidence",
              render: (r) =>
                r.unavailable
                  ? "Unavailable"
                  : r.stale
                    ? "Archived"
                    : r.portfolio
                      ? "Full ledger"
                      : "Listed only",
            },
            {
              key: "net",
              label: "Net P&L",
              render: (r) => amount(finance(r).net),
            },
            {
              key: "open",
              label: "Positions",
              render: (r) =>
                finance(r).positionsKnown ? finance(r).positions.length : "—",
            },
          ]}
        />
        <div className="table-footer">
          <span>
            {count(state.sessions?.total ?? records.length)} sessions
            {state.sessions?.next != null ? " · more available" : ""}
          </span>
        </div>
      </Panel>
      <Drawer
        open={!!selected}
        onClose={() => setSelected(null)}
        title={"Session " + (selected?.id?.date || "")}
        description={[selected?.id?.broker, selected?.id?.account]
          .filter(Boolean)
          .join(" / ")}
      >
        <KpiStrip
          items={[
            ["Cash", amount(f.cash)],
            ["Net P&L", amount(f.net)],
          ]}
        />
        <Facts
          items={[
            ["Status", plain(selected?.status || selected?.phase)],
            [
              "Checkpoint",
              datetime(selected?.as_of_ms || selected?.observed_at_ms),
            ],
            [
              "Stopped",
              selected?.portfolio
                ? yesNo(selected.portfolio.stopped)
                : "Not reported",
            ],
            [
              "Halted",
              selected?.portfolio
                ? yesNo(selected.portfolio.halted)
                : "Not reported",
            ],
            [
              "Closed",
              selected?.portfolio
                ? yesNo(selected.portfolio.closed)
                : "Not reported",
            ],
            ["Evidence", selected?.portfolio ? "Full ledger" : "Listed only"],
          ]}
        />
        <Button
          className="button-primary button-block"
          icon={ArrowUpRight}
          onClick={() => {
            app.selectSession(sessionKey(selected));
            location.hash = "#/portfolio";
          }}
        >
          Open portfolio
        </Button>
      </Drawer>
    </>
  );
}
