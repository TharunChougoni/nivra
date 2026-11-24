import type { Controller } from "./controller";
import type { LucideIcon } from "lucide-react";
import React, { createContext, useContext, useSyncExternalStore } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import * as Tabs from "@radix-ui/react-tabs";
import {
  LayoutDashboard,
  Wallet,
  Layers,
  Sparkles,
  Workflow,
  FlaskConical,
  CalendarDays,
  HeartPulse,
  ScanLine,
  X,
} from "lucide-react";
import { words } from "./data";
import { STAGES, instrumentLabel } from "./model";
import type { InstrumentRecord, OrderTrack } from "./model";
import type { RouteName } from "./data";
export const Context = createContext<Controller | null>(null);
export function useWorkspace() {
  const app = useContext(Context);
  if (!app) throw new Error("Workspace provider missing");
  return [useSyncExternalStore(app.subscribe, app.getSnapshot), app] as const;
}
export interface NavItem {
  id: RouteName;
  label: string;
  icon: LucideIcon;
  /** One line under the page title. */
  blurb: string;
}
export const NAV_GROUPS: { group: string; items: NavItem[] }[] = [
  {
    group: "TRADE",
    items: [
      {
        id: "overview",
        label: "Overview",
        icon: LayoutDashboard,
        blurb: "Book, equity, pi lanes and order flow at a glance",
      },
      {
        id: "portfolio",
        label: "Portfolio & fills",
        icon: Wallet,
        blurb: "Positions, order lifecycle, recorded fills and plans",
      },
      {
        id: "instruments",
        label: "Instruments & F&O",
        icon: Layers,
        blurb:
          "Verified cash catalog, derivatives metadata and contract chains",
      },
    ],
  },
  {
    group: "AGENTS",
    items: [
      {
        id: "lanes",
        label: "pi lanes",
        icon: Sparkles,
        blurb:
          "pi decision, gate, research and review turns · display evidence only",
      },
    ],
  },
  {
    group: "MARKET",
    items: [
      {
        id: "scanner",
        label: "Scanner",
        icon: ScanLine,
        blurb: "Fresh prospects, missing coverage and screening reasons",
      },
    ],
  },
  {
    group: "RESEARCH",
    items: [
      {
        id: "strategies",
        label: "Strategies",
        icon: Workflow,
        blurb: "Recorded strategy references and the research library",
      },
      {
        id: "backtest",
        label: "Backtest lab",
        icon: FlaskConical,
        blurb: "Deterministic replay of a supplied strategy artifact",
      },
    ],
  },
  {
    group: "SYSTEM",
    items: [
      {
        id: "sessions",
        label: "Sessions",
        icon: CalendarDays,
        blurb: "Archived and current paper sessions",
      },
      {
        id: "health",
        label: "Health",
        icon: HeartPulse,
        blurb: "Transport, feed, protection and live readiness",
      },
    ],
  },
];
export const NAV: NavItem[] = NAV_GROUPS.flatMap((g) => g.items);
export function Status({
  value,
  children,
}: {
  value?: string | null;
  children?: React.ReactNode;
}) {
  const kind = /^(ready|READY|ok|accepted|completed|open|live)$/.test(
    value || "",
  )
    ? "positive"
    : /failed|failure|halted|blocked|stopped|error/i.test(value || "")
      ? "negative"
      : /partial|stale|waiting|pending|polling/i.test(value || "")
        ? "warning"
        : "muted";
  return (
    <span className={`status ${kind}`}>
      <i />
      {children || words(value)}
    </span>
  );
}
export function Empty({
  title = "No records",
  children,
}: {
  title?: React.ReactNode;
  children?: React.ReactNode;
  /** Accepted for call-site compatibility; empty states carry no icon. */
  icon?: LucideIcon;
}) {
  return (
    <div className="empty">
      <strong>{title}</strong>
      <p>{children || "No evidence has been supplied for this view."}</p>
    </div>
  );
}
export function Panel({
  title,
  note,
  actions,
  children,
  className = "",
}: {
  title: React.ReactNode;
  note?: React.ReactNode;
  actions?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
}) {
  return (
    <section className={`panel ${className}`}>
      <header className="panel-head">
        <h2>{title}</h2>
        {note && <span className="note">{note}</span>}
        {actions && <div className="panel-actions">{actions}</div>}
      </header>
      {children}
    </section>
  );
}
export function Tag({
  children,
  tone,
  title,
}: {
  children: React.ReactNode;
  tone?: "pos" | "neg" | "warn";
  title?: string;
}) {
  return (
    <span className={`tag ${tone ?? ""}`} title={title}>
      {children}
    </span>
  );
}
export function LaneSwatch({ lane }: { lane: string }) {
  const known = ["decision", "gate", "research", "review"].includes(lane);
  return (
    <span className={`lane-swatch ${known ? "lane-" + lane : ""}`}>
      <i />
      {lane}
    </span>
  );
}
export function Button({
  children,
  icon: Icon,
  className = "",
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & { icon?: LucideIcon }) {
  return (
    <button className={`button ${className}`} {...props}>
      {Icon && <Icon size={14} />}
      {children}
    </button>
  );
}
export function IconButton({
  label,
  icon: Icon,
  className = "",
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  label: string;
  icon: LucideIcon;
}) {
  return (
    <button
      className={`icon-button ${className}`}
      aria-label={label}
      title={label}
      {...props}
    >
      <Icon size={16} />
    </button>
  );
}
export function Segments({
  value,
  onChange,
  items,
  label = "View",
}: {
  value: string;
  onChange: (value: string) => void;
  items: string[][];
  label?: string;
}) {
  return (
    <Tabs.Root value={value} onValueChange={onChange}>
      <Tabs.List className="segments" aria-label={label}>
        {items.map(([id, text]) => (
          <Tabs.Trigger key={id} value={id}>
            {text}
          </Tabs.Trigger>
        ))}
      </Tabs.List>
    </Tabs.Root>
  );
}
export interface Column<T> {
  key: string;
  label: React.ReactNode;
  render?: (row: T) => React.ReactNode;
  /** Numeric column: right-aligned, monospaced, tabular. */
  num?: boolean;
}
export interface TableProps<T> {
  columns: Column<T>[];
  rows: T[];
  getKey?: (row: T, index: number) => React.Key | undefined;
  selected?: unknown;
  onSelect?: (row: T) => void;
  empty?: string;
  compact?: boolean;
}
function cell<T>(row: T, key: string): React.ReactNode {
  if (typeof row !== "object" || row === null || !(key in row)) return "—";
  const value: unknown = Reflect.get(row, key);
  return typeof value === "string" || typeof value === "number" ? value : "—";
}
export function Table<T>({
  columns,
  rows,
  getKey = (_, i) => i,
  selected,
  onSelect,
  empty,
  compact = false,
}: TableProps<T>) {
  if (!rows.length) return <Empty title={empty || "No matching records"} />;
  return (
    <div className={`table-scroll ${compact ? "compact" : ""}`}>
      <table>
        <thead>
          <tr>
            {columns.map((c) => (
              <th key={c.key} scope="col" className={c.num ? "r" : ""}>
                {c.label}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr
              key={getKey(row, i)}
              className={selected === getKey(row, i) ? "selected" : ""}
            >
              {columns.map((c, j) => (
                <td key={c.key} className={c.num ? "r num" : ""}>
                  {onSelect && j === 0 ? (
                    <button className="row-link" onClick={() => onSelect(row)}>
                      {c.render ? c.render(row) : cell(row, c.key)}
                    </button>
                  ) : c.render ? (
                    c.render(row)
                  ) : (
                    (cell(row, c.key) ?? "—")
                  )}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
export function Instrument({
  symbol,
  instrumentKey,
  sub,
}: {
  symbol?: string | null;
  instrumentKey?: string | null;
  sub?: React.ReactNode;
}) {
  return (
    <span className="instrument">
      <strong>{symbol || instrumentKey || "Unknown instrument"}</strong>
      <small>{sub || instrumentKey}</small>
    </span>
  );
}
export function Facts({
  items,
  className = "",
}: {
  items: [string, React.ReactNode][];
  className?: string;
}) {
  return (
    <dl className={`fact-list ${className}`}>
      {items.map(([label, value]) => (
        <div key={label}>
          <dt>{label}</dt>
          <dd>{value ?? "Not reported"}</dd>
        </div>
      ))}
    </dl>
  );
}
export function Drawer({
  open,
  onClose,
  title,
  description,
  children,
  className = "",
}: {
  open: boolean;
  onClose: () => void;
  title: React.ReactNode;
  description?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
}) {
  return (
    <Dialog.Root open={open} onOpenChange={(v) => !v && onClose()}>
      <Dialog.Portal>
        <div className="dialog-overlay" aria-hidden="true" />
        <Dialog.Content className={`drawer ${className}`}>
          <div className="drawer-title">
            <div>
              <Dialog.Title>{title}</Dialog.Title>
              <Dialog.Description>{description}</Dialog.Description>
            </div>
            <Dialog.Close asChild>
              <IconButton icon={X} label="Close detail" />
            </Dialog.Close>
          </div>
          <div className="drawer-body">{children}</div>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}
export function Notice({
  children,
  error,
}: {
  children?: React.ReactNode;
  error?: boolean;
}) {
  return children ? (
    <div
      className={`notice ${error ? "notice-error" : ""}`}
      role={error ? "alert" : "status"}
    >
      {children}
    </div>
  ) : null;
}
export type KpiItem = [
  label: string,
  value: React.ReactNode,
  note?: React.ReactNode,
  options?: { tone?: "pos" | "neg" | "warn"; title?: string },
];
/** One bordered row of cells. A `—` value is unknown, never zero; the note says why. */
export function KpiStrip({ items }: { items: KpiItem[] }) {
  return (
    <div className="kpis">
      {items.map(([name, value, note, options]) => (
        <div
          key={name}
          className={`kpi ${value === "—" ? "unknown" : ""}`}
          title={
            options?.title ?? (value === "—" ? String(note ?? "") : undefined)
          }
        >
          <span>{name}</span>
          <strong className={options?.tone ?? ""}>{value}</strong>
          <small>{note}</small>
        </div>
      ))}
    </div>
  );
}
/** Five-cell order lifecycle bar. Simulated stages are dashed; ACK is never a fill. */
export function Steps({ track }: { track: OrderTrack }) {
  return (
    <span
      className="steps"
      role="img"
      aria-label={STAGES.map((s) => `${s} ${track.stages[s]}`).join(", ")}
    >
      {STAGES.map((stage) => (
        <i
          key={stage}
          className={track.stages[stage]}
          title={`${stage}: ${track.stages[stage]}`}
        />
      ))}
    </span>
  );
}
/** Label from recorded fields, with a kind tag and expiry when it is a derivative. */
export function InstrumentCell({
  record,
  fallback,
}: {
  record?: InstrumentRecord | null;
  fallback?: string | null;
}) {
  const label = instrumentLabel(record ?? { key: fallback ?? undefined });
  return (
    <span className="instrument">
      <span className="row">
        <strong>{label.primary}</strong>
        {label.kind !== "unknown" && (
          <Tag>
            {label.kind === "call"
              ? "CE"
              : label.kind === "put"
                ? "PE"
                : label.kind === "future"
                  ? "FUT"
                  : label.kind.toUpperCase()}
          </Tag>
        )}
      </span>
    </span>
  );
}
