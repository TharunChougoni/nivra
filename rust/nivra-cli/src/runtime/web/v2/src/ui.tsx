import type { Controller } from "./controller";
import type { LucideIcon } from "lucide-react";
import React, { createContext, useContext, useSyncExternalStore } from "react";
import * as Dialog from "@radix-ui/react-dialog";
import * as Tabs from "@radix-ui/react-tabs";
import {
  ArrowUpRight,
  PanelLeft,
  Sparkles,
  Columns3,
  CalendarDays,
  Search,
  Workflow,
  FlaskConical,
  CircleDot,
  X,
  ChevronRight,
  Activity,
  RefreshCw,
  MoreHorizontal,
  ChevronsUpDown,
  Command,
  ArrowDown,
  FileText,
  Clock,
  ExternalLink,
} from "lucide-react";
import { words } from "./data";
export const Context = createContext<Controller | null>(null);
export function useWorkspace() {
  const app = useContext(Context);
  if (!app) throw new Error("Workspace provider missing");
  return [useSyncExternalStore(app.subscribe, app.getSnapshot), app] as const;
}
export const NAV: [string, string, LucideIcon][] = [
  ["overview", "Overview", PanelLeft],
  ["portfolio", "Portfolio & fills", Columns3],
  ["lanes", "pi lanes", Sparkles],
  ["sessions", "Sessions", CalendarDays],
  ["scanner", "Market scanner", Search],
  ["strategies", "Strategies", Workflow],
  ["backtest", "Backtest lab", FlaskConical],
  ["health", "System health", CircleDot],
];
export {
  ArrowUpRight,
  X,
  ChevronRight,
  Activity,
  RefreshCw,
  MoreHorizontal,
  ChevronsUpDown,
  Command,
  ArrowDown,
  FileText,
  Clock,
  ExternalLink,
  Search,
};
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
  icon: Icon = FileText,
}: {
  title?: React.ReactNode;
  children?: React.ReactNode;
  icon?: LucideIcon;
}) {
  return (
    <div className="empty">
      <span className="empty-icon">
        <Icon size={19} />
      </span>
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
    <section className={`surface ${className}`}>
      <header className="surface-head">
        <div>
          <h2>{title}</h2>
          {note && <p>{note}</p>}
        </div>
        {actions && <div className="surface-actions">{actions}</div>}
      </header>
      {children}
    </section>
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
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  label: string;
  icon: LucideIcon;
}) {
  return (
    <button className="icon-button" aria-label={label} title={label} {...props}>
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
              <th key={c.key} scope="col">
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
                <td key={c.key}>
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
export function Facts({ items }: { items: [string, React.ReactNode][] }) {
  return (
    <dl className="fact-list">
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
export function MetricStrip({
  items,
}: {
  items: [string, React.ReactNode, React.ReactNode?][];
}) {
  return (
    <div className="metric-strip">
      {items.map(([name, value, note]) => (
        <div key={name}>
          <span>{name}</span>
          <strong>{value}</strong>
          <small>{note}</small>
        </div>
      ))}
    </div>
  );
}
