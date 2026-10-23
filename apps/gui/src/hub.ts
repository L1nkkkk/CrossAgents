// Live updates from `GET /v1/events`. Events trigger a debounced re-fetch of the resource
// they concern (and session views fold their agent's events, see session.ts); REST
// responses stay the source of truth. Unknown event types are ignored.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Event as HubEvent } from "@bindings/Event";
import { api, errorText, getBridge, refuseRequests } from "./api";

export type { HubEvent };

/** The hub API this GUI speaks: `API_VERSION` in crates/api-types. */
export const API_VERSION = "v1";

/** "connecting", "connected" or "offline: <reason>". */
export type ConnectionStatus = string;

/** The hub speaks another API version: nothing is asked of it or taken from it. */
export type VersionMismatch = { hub: string; gui: string };

const eventListeners = new Set<(event: HubEvent) => void>();
const statusListeners = new Set<() => void>();
let status: ConnectionStatus = "connecting";
let mismatch: VersionMismatch | undefined;
let started = false;
/** What the event stream says, before the hub's version is checked. */
let transport: ConnectionStatus = "connecting";
/** Numbers each check, so one for a stream that has since dropped is ignored. */
let checks = 0;

function setStatus(next: ConnectionStatus) {
  status = next;
  statusListeners.forEach((l) => l());
}

/**
 * The event stream's own status. "connected" becomes ours only once the hub's API version
 * matches; a hub that speaks another one is refused, never half-used.
 */
function setTransport(next: ConnectionStatus) {
  transport = next;
  const mine = ++checks;
  if (next !== "connected") return setStatus(next);
  setStatus("connecting");
  const check = () =>
    api.info().then(
      (info) => {
        if (mine !== checks) return;
        if (info.api === API_VERSION) {
          mismatch = undefined;
          refuseRequests(undefined);
          return setStatus("connected");
        }
        mismatch = { hub: info.api, gui: API_VERSION };
        const reason = `hub API ${info.api}, GUI API ${API_VERSION}: update both to the same version`;
        refuseRequests(reason);
        setStatus(`offline: ${reason}`);
      },
      (err) => {
        if (mine !== checks) return;
        // The stream is up but the hub did not answer: try again while it stays up.
        setStatus(`offline: ${errorText(err)}`);
        setTimeout(() => mine === checks && transport === "connected" && void check(), 2000);
      },
    );
  void check();
}

function dispatch(data: string) {
  if (mismatch) return;
  let event: HubEvent;
  try {
    event = JSON.parse(data);
  } catch {
    return;
  }
  if (!event || typeof event !== "object" || typeof event.type !== "string") return;
  eventListeners.forEach((l) => l(event));
}

/** Opens the event stream once per page load. */
export function startEvents() {
  if (started) return;
  started = true;
  const bridge = getBridge();
  if (bridge) {
    // Subscribe before starting the relay so no event is missed.
    bridge.onStatus(setTransport);
    bridge.onEvent(dispatch);
    bridge.startEvents();
  } else {
    void webEvents();
  }
}

/** Reads the SSE stream with `fetch` (EventSource drops named events it does not know). */
async function webEvents() {
  let last: string | undefined;
  for (;;) {
    setTransport("connecting");
    try {
      // First connection: live events only (views load state over REST); reconnects resume.
      const response = await fetch(last ? "/v1/events" : "/v1/events?live=true", {
        headers: last ? { "last-event-id": last, accept: "text/event-stream" } : { accept: "text/event-stream" },
      });
      if (!response.ok || !response.body) throw new Error(`hub answered ${response.status}`);
      setTransport("connected");
      const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += value.replace(/\r/g, "");
        let end;
        while ((end = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, end);
          buf = buf.slice(end + 2);
          const data: string[] = [];
          for (const line of block.split("\n")) {
            const i = line.indexOf(":");
            const field = i < 0 ? line : line.slice(0, i);
            const value = i < 0 ? "" : line.slice(i + 1).replace(/^ /, "");
            if (field === "id") last = value;
            else if (field === "data") data.push(value);
          }
          if (data.length) dispatch(data.join("\n"));
        }
      }
      setTransport("offline: event stream ended");
    } catch (err) {
      setTransport(`offline: ${errorText(err)}`);
    }
    await new Promise((r) => setTimeout(r, 2000));
  }
}

export function onHubEvent(listener: (event: HubEvent) => void): () => void {
  eventListeners.add(listener);
  return () => eventListeners.delete(listener);
}

export function useConnectionStatus(): ConnectionStatus {
  return useSyncExternalStore(
    (l) => {
      statusListeners.add(l);
      return () => statusListeners.delete(l);
    },
    () => status,
  );
}

/** Set while the hub speaks another API version than this GUI. */
export function useVersionMismatch(): VersionMismatch | undefined {
  return useSyncExternalStore(
    (l) => {
      statusListeners.add(l);
      return () => statusListeners.delete(l);
    },
    () => mismatch,
  );
}


export type Loaded<T> = { data?: T; error?: string; loading: boolean; reload: () => void };

/**
 * Loads a resource, then re-loads it (debounced) whenever `relevant` accepts an event and
 * whenever the event stream (re)connects.
 */
export function useHubData<T>(
  load: () => Promise<T>,
  deps: unknown[],
  relevant: (event: HubEvent) => boolean,
): Loaded<T> {
  const [state, setState] = useState<{ data?: T; error?: string; loading: boolean }>({ loading: true });
  const relevantRef = useRef(relevant);
  relevantRef.current = relevant;
  // Requests are numbered; a response is applied unless a newer one already was. (Dropping
  // every response but the latest starves the view while events keep arriving.)
  const generation = useRef(0);
  const applied = useRef(0);

  const reload = useCallback(() => {
    const mine = ++generation.current;
    const fresh = () => mine > applied.current && ((applied.current = mine), true);
    load().then(
      (data) => fresh() && setState({ data, loading: false }),
      (err) => fresh() && setState((s) => ({ data: s.data, error: errorText(err), loading: false })),
    );
  }, deps);

  useEffect(() => {
    // Responses to requests made for the previous `deps` no longer apply.
    applied.current = generation.current;
    setState({ loading: true });
    reload();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = () => {
      clearTimeout(timer);
      timer = setTimeout(reload, 250);
    };
    const offEvent = onHubEvent((e) => relevantRef.current(e) && schedule());
    let prev = status;
    const onStatus = () => {
      if (status === "connected" && prev !== "connected") schedule();
      prev = status;
    };
    statusListeners.add(onStatus);
    return () => {
      clearTimeout(timer);
      offEvent();
      statusListeners.delete(onStatus);
    };
  }, [reload]);

  return { ...state, reload };
}
