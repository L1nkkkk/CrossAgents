// Live updates from `GET /v1/events`. Events trigger a debounced re-fetch of the resource
// they concern (and session views fold their agent's events, see session.ts); REST
// responses stay the source of truth. Unknown event types are ignored.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { Event as HubEvent } from "@bindings/Event";
import { bridge, errorText } from "./api";

export type { HubEvent };

/** "connecting", "connected" or "offline: <reason>". */
export type ConnectionStatus = string;

const eventListeners = new Set<(event: HubEvent) => void>();
const statusListeners = new Set<() => void>();
let status: ConnectionStatus = "connecting";
let started = false;

function setStatus(next: ConnectionStatus) {
  status = next;
  statusListeners.forEach((l) => l());
}

function dispatch(data: string) {
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
  if (bridge) {
    // Subscribe before starting the relay so no event is missed.
    bridge.onStatus(setStatus);
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
    setStatus("connecting");
    try {
      // First connection: live events only (views load state over REST); reconnects resume.
      const response = await fetch(last ? "/v1/events" : "/v1/events?live=true", {
        headers: last ? { "last-event-id": last, accept: "text/event-stream" } : { accept: "text/event-stream" },
      });
      if (!response.ok || !response.body) throw new Error(`hub answered ${response.status}`);
      setStatus("connected");
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
      setStatus("offline: event stream ended");
    } catch (err) {
      setStatus(`offline: ${errorText(err)}`);
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
