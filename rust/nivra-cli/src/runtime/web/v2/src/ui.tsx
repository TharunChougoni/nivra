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
  Inbox,
  CloudOff,
  OctagonX,
  Info,
  TriangleAlert,
} from "lucide-react";
import { instrumentName, plain, sentenceCase, shortId } from "./labels";
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
    group: "Trade",
    items: [
      {
        id: "overview",
        label: "Overview",
        icon: LayoutDashboard,
        blurb: "Account, equity and what the models are doing",
      },
      {
        id: "portfolio",
        label: "Portfolio & fills",
        icon: Wallet,
        blurb: "Positions, orders, fills and plans",
      },
      {
        id: "instruments",
        label: "Instruments & F&O",
        icon: Layers,
        blurb: "Tradable instruments and option chains",
      },
    ],
  },
  {
    group: "Agents",
    items: [
      {
        id: "lanes",
        label: "pi lanes",
        icon: Sparkles,
        blurb: "What each model lane saw, did and decided",
      },
    ],
  },
  {
    group: "Market",
    items: [
      {
        id: "scanner",
        label: "Scanner",
        icon: ScanLine,
        blurb: "Today's picks and why others were skipped",
      },
    ],
  },
  {
    group: "Research",
    items: [
      {
        id: "strategies",
        label: "Strategies",
        icon: Workflow,
        blurb: "Strategies behind the picks, and research results",
      },
      {
        id: "backtest",
        label: "Backtest lab",
        icon: FlaskConical,
        blurb: "Replay a strategy on past data",
      },
    ],
  },
  {
    group: "System",
    items: [
      {
        id: "sessions",
        label: "Sessions",
        icon: CalendarDays,
        blurb: "Current and past paper sessions",
      },
      {
        id: "health",
        label: "Health",
        icon: HeartPulse,
        blurb: "Feed, alerts and system checks",
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
      {children || plain(value)}
    </span>
  );
}
export type EmptyTone = "empty" | "unavailable" | "failed";
const EMPTY_ICON: Record<EmptyTone, LucideIcon> = {
  empty: Inbox,
  unavailable: CloudOff,
  failed: OctagonX,
};
/** Tone from the title when a call site does not say: failures and missing
    sources must not look like an honest "nothing happened yet". */
function emptyTone(title: React.ReactNode): EmptyTone {
  const text = typeof title === "string" ? title : "";
  if (/fail|error|reject|refused|invalid/i.test(text)) return "failed";
  if (
    /unavailable|not reported|not loaded|not present|missing|no .*(source|evidence)/i.test(
      text,
    )
  )
    return "unavailable";
  return "empty";
}
export function Empty({
  title = "No records",
  children,
  icon,
  tone,
  action,
}: {
  title?: React.ReactNode;
  children?: React.ReactNode;
  icon?: LucideIcon;
  tone?: EmptyTone;
  action?: React.ReactNode;
}) {
  const kind = tone ?? emptyTone(title),
    Icon = icon ?? EMPTY_ICON[kind];
  return (
    <div className={`empty empty-${kind}`}>
      <span className="empty-icon" aria-hidden="true">
        <Icon size={16} />
      </span>
      <div>
        <strong>{title}</strong>
        {children && <p>{children}</p>}
        {action && <div className="empty-action">{action}</div>}
      </div>
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
      {sentenceCase(lane)}
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
  /** Drop columns whose every cell reads "Not reported" or the like. The first column stays. */
  hideEmpty?: boolean;
}
const EMPTY_CELLS = new Set(["Not reported", "Unknown", "Not recorded", "—"]);
function cell<T>(row: T, key: string): React.ReactNode {
  if (typeof row !== "object" || row === null || !(key in row)) return "—";
  const value: unknown = Reflect.get(row, key);
  return typeof value === "string" || typeof value === "number" ? value : "—";
}
export function Table<T>({
  columns: allColumns,
  rows,
  getKey = (_, i) => i,
  selected,
  onSelect,
  empty,
  compact = false,
  hideEmpty = false,
}: TableProps<T>) {
  if (!rows.length) return <Empty title={empty || "No matching records"} />;
  const columns = hideEmpty
    ? allColumns.filter(
        (c, j) =>
          j === 0 ||
          !rows.every((row) => {
            const shown = c.render ? c.render(row) : cell(row, c.key);
            return typeof shown === "string" && EMPTY_CELLS.has(shown);
          }),
      )
    : allColumns;
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
      <strong>{symbol || instrumentName(instrumentKey)}</strong>
      <small>
        {sub || (symbol && instrumentKey ? instrumentName(instrumentKey) : "")}
      </small>
    </span>
  );
}
/** First 8 characters of a hash, full value on hover. */
export function ShortId({ value }: { value: unknown }) {
  const { text, title } = shortId(value);
  return title ? <code title={title}>{text}</code> : <>{text}</>;
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
  tone = error ? "error" : "warn",
  title,
}: {
  children?: React.ReactNode;
  error?: boolean;
  tone?: "info" | "warn" | "error";
  title?: React.ReactNode;
}) {
  if (!children) return null;
  const Icon =
    tone === "error" ? OctagonX : tone === "warn" ? TriangleAlert : Info;
  return (
    <div
      className={`notice notice-${tone}`}
      role={tone === "error" ? "alert" : "status"}
    >
      <Icon size={14} aria-hidden="true" />
      <div>
        {title && <strong>{title}</strong>}
        <div>{children}</div>
      </div>
    </div>
  );
}
export type KpiItem = [
  label: string,
  value: React.ReactNode,
  note?: React.ReactNode,
  options?: { tone?: "pos" | "neg" | "warn"; title?: string },
];
/** One bordered row of cells. A `—` value is unknown, never zero; the note says why. */
export function KpiStrip({
  items,
  emptyTitle = "No values recorded",
}: {
  items: KpiItem[];
  emptyTitle?: string;
}) {
  if (items.length > 1 && items.every(([, value]) => value === "—"))
    return (
      <div className="kpis kpis-empty">
        <Empty title={emptyTitle} tone="unavailable">
          {items.map(([name]) => name).join(", ")}: unknown, not zero.
        </Empty>
      </div>
    );
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
          {/* Keyed on the shown value so a changed number re-runs its tick. */}
          <strong
            key={
              typeof value === "string" || typeof value === "number"
                ? value
                : undefined
            }
            className={options?.tone ?? ""}
          >
            {value}
          </strong>
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
