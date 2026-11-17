import type { SeriesResponse } from "./types";
export type BrowserWindow = Pick<
  Window,
  "sessionStorage" | "crypto" | "indexedDB" | "document" | "fetch"
>;
export interface CachedEvidence {
  body: SeriesResponse;
}
interface CacheRecord {
  key: string;
  tab: string;
  payload: CachedEvidence;
  saved_at: number;
}
export interface EvidenceCache {
  get(key: string): Promise<CachedEvidence | null>;
  put(
    key: string,
    payload: CachedEvidence,
    metadata?: { binding?: string; kind?: string; instrument?: string },
  ): Promise<boolean>;
  clear(): Promise<unknown>;
  close?(): void;
}
// Bounded, tab-local chart evidence cache. Records never contain credentials,
// account payloads or trading controls; every cache hit must be shown as stale.
const DATABASE = "nivra-evidence-v1";
const STORE = "series";

function tabId(win: BrowserWindow) {
  const key = "nivraEvidenceTab";
  try {
    let value = win.sessionStorage.getItem(key);
    if (!value) {
      value =
        win.crypto?.randomUUID?.() ||
        `tab-${Date.now()}-${Math.random().toString(16).slice(2)}`;
      win.sessionStorage.setItem(key, value);
    }
    return value;
  } catch {
    return "memory";
  }
}

function request<T>(value: IDBRequest<T>): Promise<T | null> {
  return new Promise((resolve) => {
    value.onsuccess = () => resolve(value.result);
    value.onerror = () => resolve(null);
  });
}

function complete(transaction: IDBTransaction): Promise<boolean> {
  return new Promise((resolve) => {
    transaction.oncomplete = () => resolve(true);
    transaction.onerror = () => resolve(false);
    transaction.onabort = () => resolve(false);
  });
}

export function createEvidenceCache(win: BrowserWindow, { limit = 24 } = {}) {
  const id = tabId(win);
  let database: Promise<IDBDatabase | null> | null;

  function open(): Promise<IDBDatabase | null> {
    if (!win.indexedDB) return Promise.resolve(null);
    if (database) return database;
    database = new Promise((resolve) => {
      const value = win.indexedDB.open(DATABASE, 1);
      value.onupgradeneeded = () => {
        const store = value.result.createObjectStore(STORE, {
          keyPath: "key",
        });
        store.createIndex("tab", "tab", { unique: false });
      };
      value.onsuccess = () => {
        value.result.onversionchange = () => value.result.close();
        resolve(value.result);
      };
      value.onerror = () => resolve(null);
      value.onblocked = () => resolve(null);
    });
    return database;
  }

  return {
    tab: id,
    async get(key: string): Promise<CachedEvidence | null> {
      const db = await open();
      if (!db) return null;
      try {
        const store = db.transaction(STORE, "readonly").objectStore(STORE);
        const record = await request<CacheRecord | undefined>(
          store.get(`${id}|${key}`),
        );
        return record?.tab === id ? (record.payload ?? null) : null;
      } catch {
        return null;
      }
    },
    async put(
      key: string,
      payload: CachedEvidence,
      { binding = "", kind = "", instrument = "" } = {},
    ) {
      const db = await open();
      if (!db) return false;
      try {
        const transaction = db.transaction(STORE, "readwrite");
        const store = transaction.objectStore(STORE);
        store.put({
          key: `${id}|${key}`,
          tab: id,
          binding,
          kind,
          instrument,
          saved_at: Date.now(),
          payload,
        });
        if (!(await complete(transaction))) return false;
        await this.prune();
        return true;
      } catch {
        return false;
      }
    },
    async prune() {
      const db = await open();
      if (!db) return false;
      try {
        const transaction = db.transaction(STORE, "readwrite");
        const store = transaction.objectStore(STORE);
        const rows =
          (await request<CacheRecord[]>(store.index("tab").getAll(id))) || [];
        rows
          .sort((a, b) => Number(b.saved_at) - Number(a.saved_at))
          .slice(Math.max(0, limit))
          .forEach((row) => store.delete(row.key));
        return complete(transaction);
      } catch {
        return false;
      }
    },
    async clear() {
      const db = await open();
      if (!db) return false;
      try {
        const transaction = db.transaction(STORE, "readwrite");
        const store = transaction.objectStore(STORE);
        const rows =
          (await request<CacheRecord[]>(store.index("tab").getAll(id))) || [];
        rows.forEach((row) => store.delete(row.key));
        return complete(transaction);
      } catch {
        return false;
      }
    },
    close() {
      database?.then((db) => db?.close()).catch(() => {});
      database = null;
    },
  };
}
