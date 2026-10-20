// Live updates from `GET /v1/events`. Events only (a) trigger a debounced re-fetch of the
// resource they concern and (b) build each agent's transcript; REST responses stay the
// source of truth. Unknown event types are ignored.

import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Event as HubEvent } from "@bindings/Event";
import { errorText, inTauri } from "./api";

export type { HubEvent };

/** "connecting", "connected" or "offline: <reason>". */
export type ConnectionStatus = string;

export type LineKind = "input" | "text" | "thought" | "tool" | "turn" | "error" | "status";
export type Line = { kind: LineKind; text: string; toolCallId?: string };

/** Characters of transcript kept per agent. */
const TRANSCRIPT_LIMIT = 200_000;

type Transcript = { lines: Line[]; size: number };

const eventListeners = new Set<(event: HubEvent) => void>();
const statusListeners = new Set<() => void>();
const transcriptListeners = new Set<() => void>();
const transcripts = new Map<string, Transcript>();
const EMPTY: Line[] = [];
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
  record(event);
  eventListeners.forEach((l) => l(event));
}

function push(id: string, line: Line) {
  const t = transcripts.get(id) ?? { lines: [], size: 0 };
  const last = t.lines[t.lines.length - 1];
  let lines: Line[];
  // Streamed text arrives in chunks: extend the current paragraph.
  if (last && (line.kind === "text" || line.kind === "thought") && last.kind === line.kind) {
    lines = [...t.lines.slice(0, -1), { ...last, text: last.text + line.text }];
  } else {
    lines = [...t.lines, line];
  }
  let size = t.size + line.text.length;
  while (size > TRANSCRIPT_LIMIT && lines.length > 1) {
    size -= lines[0].text.length;
    lines = lines.slice(1);
  }
  transcripts.set(id, { lines, size });
  transcriptListeners.forEach((l) => l());
}

function record(event: HubEvent) {
  switch (event.type) {
    case "agent.input":
      push(event.data.id, { kind: "input", text: event.data.text + (event.data.queued ? "  (queued)" : "") });
      break;
    case "agent.output":
      push(event.data.id, { kind: event.data.stream === "thought" ? "thought" : "text", text: event.data.text });
      break;
    case "agent.tool_call":
      push(event.data.id, {
        kind: "tool",
        text: `${event.data.title} · ${event.data.status}`,
        toolCallId: event.data.tool_call_id,
      });
      break;
    case "agent.tool_call_updated": {
      const t = transcripts.get(event.data.id);
      const i = t ? t.lines.findLastIndex((l) => l.toolCallId === event.data.tool_call_id) : -1;
      if (!t || i < 0) break;
      const [title, oldStatus] = t.lines[i].text.split(" · ");
      const text = `${event.data.title ?? title} · ${event.data.status ?? oldStatus}`;
      const lines = t.lines.slice();
      lines[i] = { ...lines[i], text };
      transcripts.set(event.data.id, { ...t, lines });
      transcriptListeners.forEach((l) => l());
      break;
    }
    case "agent.turn_completed":
      push(event.data.id, { kind: "turn", text: `turn ended: ${event.data.stop_reason}` });
      break;
    case "agent.error":
      push(event.data.id, { kind: "error", text: event.data.message });
      break;
    case "agent.status_changed":
      if (event.data.to === "starting" || event.data.to === "stopped" || event.data.to === "error") {
        push(event.data.id, { kind: "status", text: `status: ${event.data.to}` });
      }
      break;
    default:
      break;
  }
}

/** Opens the event stream once per page load. */
export function startEvents() {
  if (started) return;
  started = true;
  if (inTauri) {
    void (async () => {
      // Subscribe before starting the relay so no event is missed.
      await listen<string>("hub://status", (e) => setStatus(e.payload));
      await listen<string>("hub://event", (e) => dispatch(e.payload));
      await invoke("hub_events_start");
    })();
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
      const response = await fetch("/v1/events", {
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

export function useTranscript(agentId: string | undefined): Line[] {
  return useSyncExternalStore(
    (l) => {
      transcriptListeners.add(l);
      return () => transcriptListeners.delete(l);
    },
    () => (agentId ? (transcripts.get(agentId)?.lines ?? EMPTY) : EMPTY),
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
  const generation = useRef(0);

  const reload = useCallback(() => {
    const mine = ++generation.current;
    load().then(
      (data) => mine === generation.current && setState({ data, loading: false }),
      (err) => mine === generation.current && setState((s) => ({ data: s.data, error: errorText(err), loading: false })),
    );
  }, deps);

  useEffect(() => {
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
