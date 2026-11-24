import type { Api } from "./api";
import type { Availability, LoadState, RevisionName } from "./types";
import { useEffect, useState } from "react";
import { useWorkspace } from "./ui";

/**
 * Reload key for a page-local read. With a revision-aware backend this changes
 * only when that projection changed; otherwise it changes on every refresh.
 */
export function useRevision(name: RevisionName) {
  const [state] = useWorkspace();
  return state.summary?.revisions?.[name] ?? String(state.received ?? "");
}
/** A page-local read of one authenticated projection, reloaded when `key` changes. */
export function useRead<T extends Availability>(
  load: (api: Api) => Promise<T>,
  key = "",
) {
  const [state, app] = useWorkspace(),
    [result, setResult] = useState<{ value: T | null; state: LoadState }>({
      value: null,
      state: "idle",
    }),
    token = state.token;
  useEffect(() => {
    let active = true;
    if (!token) {
      setResult({ value: null, state: "idle" });
      return;
    }
    // Keep the previous value while a revision-driven reload is in flight.
    setResult((previous) => ({ ...previous, state: "loading" }));
    Promise.resolve(load(app.api))
      .then((value) => {
        if (!active) return;
        setResult({
          value,
          state: value?.unavailable ? "unavailable" : "ready",
        });
      })
      .catch(() => {
        if (active) setResult({ value: null, state: "error" });
      });
    return () => {
      active = false;
    };
  }, [token, key]);
  return result;
}
