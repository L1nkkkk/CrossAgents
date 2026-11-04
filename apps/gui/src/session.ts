// A session's conversation: `GET /v1/agents/{key}/transcript`, then the agent's live events
// folded in the same way the hub does (crates/hub/src/transcript.rs). Re-fetched when the
// event stream reconnects, so a gap heals itself.

import { useEffect, useRef, useState } from "react";
import type { Transcript } from "@bindings/Transcript";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import type { TranscriptTurn } from "@bindings/TranscriptTurn";
import { api, errorText } from "./api";
import { onHubEvent, useConnectionStatus, type HubEvent } from "./hub";
import { turnStat } from "./lib/diff";

export type { Transcript, TranscriptItem, TranscriptTurn };

export type SessionState = { transcript?: Transcript; error?: string; reload: () => void };

export function useSession(key: string): SessionState {
  const [transcript, setTranscript] = useState<Transcript>();
  const [error, setError] = useState<string>();
  const current = useRef<Transcript | undefined>(undefined);
  const generation = useRef(0);
  const applied = useRef(0);
  const status = useConnectionStatus();

  const reload = () => {
    const mine = ++generation.current;
    api.transcript(key).then(
      (t) => {
        // Keep what live events have already folded in beyond this response.
        if (mine <= applied.current || (current.current && t.last_seq < current.current.last_seq)) return;
        applied.current = mine;
        current.current = t;
        setTranscript(t);
        setError(undefined);
      },
      (err) => mine > applied.current && setError(errorText(err)),
    );
  };

  useEffect(() => {
    applied.current = generation.current;
    current.current = undefined;
    setTranscript(undefined);
    setError(undefined);
    reload();
    // Events published while the transcript loads are dropped by `seq` once it arrives;
    // ones before it are already in it.
    const off = onHubEvent((event) => {
      const t = current.current;
      if (!t || event.seq <= t.last_seq) return;
      const next = applyEvent(t, event);
      if (next !== t) {
        current.current = next;
        setTranscript(next);
      } else {
        t.last_seq = event.seq;
      }
    });
    return off;
  }, [key]);

  useEffect(() => {
    if (status === "connected" && current.current) reload();
  }, [status]);

  return { transcript, error, reload };
}

/** Returns a new transcript with `event` folded in, or `t` itself if it is unrelated. */
export function applyEvent(t: Transcript, event: HubEvent): Transcript {
  const id = t.agent.id;
  const seq = event.seq;
  const turns = t.turns;
  switch (event.type) {
    case "agent.updated":
      // Profile, model or effort changed: the conversation shows the new card.
      if (event.data.agent.id !== id) return t;
      return { ...t, agent: { ...event.data.agent, status: t.agent.status }, last_seq: seq };
    case "agent.model_changed": {
      // Shown as a note between the turns.
      if (event.data.id !== id) return t;
      const { model, effort, from_model, from_effort } = event.data;
      return { ...t, switches: [...(t.switches ?? []), { at: event.ts, model, effort, from_model, from_effort }], last_seq: seq };
    }
    case "agent.status_changed":
      if (event.data.id !== id) return t;
      return { ...t, agent: { ...t.agent, status: event.data.to }, last_seq: seq };
    case "agent.input": {
      if (event.data.id !== id) return t;
      const turn: TranscriptTurn = {
        message_id: event.data.message_id,
        input: event.data.text,
        from: event.data.from ?? null,
        reply_to: event.data.reply_to,
        thread: event.data.thread,
        reply_excerpt: event.data.reply_excerpt,
        sent_at: event.ts,
        started_at: null,
        completed_at: null,
        stop_reason: null,
        items: [],
        ...(event.data.attachments?.length ? { attachments: event.data.attachments } : {}),
      };
      return { ...t, turns: [...turns, turn], last_seq: seq };
    }
    case "agent.input_updated": {
      if (event.data.id !== id) return t;
      const { message_id, text } = event.data;
      return { ...t, turns: turns.map((x) => (x.message_id === message_id && !x.started_at ? { ...x, input: text } : x)), last_seq: seq };
    }
    case "agent.input_withdrawn": {
      if (event.data.id !== id) return t;
      const mid = event.data.message_id;
      return { ...t, turns: turns.filter((x) => x.message_id !== mid || !!x.started_at), last_seq: seq };
    }
    case "agent.steered": {
      // Handed to the turn running now: it reads this at its next tool boundary. Shown inside
      // that turn, dashed until `agent.steer_read` says the model has it.
      if (event.data.id !== id) return t;
      const { message_id, text, attachments } = event.data;
      const item: TranscriptItem = { type: "steer", message_id, text, read: false, ...(attachments?.length ? { attachments } : {}) };
      return withActive(t, seq, (items) => [...items, item]);
    }
    case "agent.steer_read": {
      if (event.data.id !== id) return t;
      const mid = event.data.message_id;
      const read = (items: TranscriptItem[]): TranscriptItem[] =>
        items.map((x) => (x.type === "steer" && x.message_id === mid && !x.read ? { ...x, read: true } : x));
      return { ...t, turns: turns.map((x) => ({ ...x, items: read(x.items) })), last_seq: seq };
    }
    case "agent.turn_started": {
      if (event.data.id !== id) return t;
      const mid = event.data.message_id;
      return { ...t, turns: turns.map((x) => (x.message_id === mid ? { ...x, started_at: event.ts } : x)), last_seq: seq };
    }
    case "agent.turn_completed": {
      if (event.data.id !== id) return t;
      const { message_id, stop_reason } = event.data;
      return {
        ...t,
        turns: turns.map((x) => (x.message_id === message_id ? { ...x, completed_at: event.ts, stop_reason } : x)),
        last_seq: seq,
      };
    }
    case "agent.output": {
      if (event.data.id !== id) return t;
      const { stream, text } = event.data;
      return withActive(t, seq, (items) => {
        const last = items[items.length - 1];
        if (last?.type === "output" && last.stream === stream) {
          return [...items.slice(0, -1), { ...last, text: last.text + text }];
        }
        return [...items, { type: "output", stream, text }];
      });
    }
    case "agent.tool_call": {
      if (event.data.id !== id) return t;
      const { tool_call_id, title, kind, status } = event.data;
      return withActive(t, seq, (items) => [...items, { type: "tool_call", tool_call_id, title, kind, status }]);
    }
    case "agent.tool_call_updated": {
      if (event.data.id !== id) return t;
      const { tool_call_id, title, status } = event.data;
      const update = (item: TranscriptItem): TranscriptItem =>
        item.type === "tool_call" && item.tool_call_id === tool_call_id
          ? { ...item, title: title ?? item.title, status: status ?? item.status }
          : item;
      return { ...t, turns: turns.map((x) => ({ ...x, items: x.items.map(update) })), last_seq: seq };
    }
    case "agent.tool_output": {
      if (event.data.id !== id) return t;
      const { tool_call_id, text } = event.data;
      return withCall(t, seq, tool_call_id, (item) => ({ ...item, output: (item.output ?? "") + text }));
    }
    case "agent.tool_detail": {
      if (event.data.id !== id) return t;
      const { tool_call_id, detail } = event.data;
      return withCall(t, seq, tool_call_id, (item) => ({ ...item, detail }));
    }
    case "agent.turn_diff": {
      if (event.data.id !== id) return t;
      const { message_id, diff, source } = event.data;
      const index = message_id ? t.turns.findIndex((x) => x.message_id === message_id) : activeIndex(t);
      if (index < 0) return { ...t, last_seq: seq };
      const turns = t.turns.slice();
      turns[index] = { ...turns[index], diff: turnStat(diff, source) };
      return { ...t, turns, last_seq: seq };
    }
    case "agent.permission": {
      if (event.data.id !== id) return t;
      const { kind, title, decision } = event.data;
      return withActive(t, seq, (items) => [...items, { type: "permission", kind, title, decision }]);
    }
    case "agent.warning": {
      if (event.data.id !== id) return t;
      const message = event.data.message;
      return withActive(t, seq, (items) => [...items, { type: "warning", message }]);
    }
    case "agent.adopted": {
      if (event.data.id !== id) return t;
      if (!event.data.message_id) return { ...t, last_seq: seq };
      const message = "hub 已重启，这一轮由新的 hub 接管，继续运行";
      return withActive(t, seq, (items) => [...items, { type: "warning", message }]);
    }
    case "agent.exited": {
      // The `agent.error` the hub sends next carries the same message; `error` is set now so
      // that one keeps `exit` (crates/hub/src/transcript.rs does the same).
      if (event.data.id !== id) return t;
      const { cause, code, message } = event.data;
      return { ...t, exit: { cause, code }, error: message, last_seq: seq };
    }
    case "agent.error": {
      if (event.data.id !== id) return t;
      const message = event.data.message;
      const exit = message === t.error ? t.exit : undefined;
      return { ...withActive(t, seq, (items) => [...items, { type: "error", message }]), error: message, exit };
    }
    case "agent.usage": {
      if (event.data.id !== id) return t;
      const { used, size, cost_usd, tokens } = event.data;
      // The breakdown arrives after the turn; keep the last one until then.
      return { ...t, usage: { used, size, cost_usd, tokens, parts: t.usage?.parts }, last_seq: seq };
    }
    case "agent.context": {
      if (event.data.id !== id || !t.usage) return t;
      return { ...t, usage: { ...t.usage, parts: event.data.parts }, last_seq: seq };
    }
    default:
      return t;
  }
}

type ToolCall = Extract<TranscriptItem, { type: "tool_call" }>;

/** Changes one tool call, searching back from the newest turn. */
function withCall(t: Transcript, seq: number, callId: string, change: (item: ToolCall) => ToolCall): Transcript {
  for (let i = t.turns.length - 1; i >= 0; i--) {
    const at = t.turns[i].items.findIndex((x) => x.type === "tool_call" && x.tool_call_id === callId);
    if (at < 0) continue;
    const items = t.turns[i].items.slice();
    items[at] = change(items[at] as ToolCall);
    const turns = t.turns.slice();
    turns[i] = { ...turns[i], items };
    return { ...t, turns, last_seq: seq };
  }
  return { ...t, last_seq: seq };
}

/** The running turn, else the last turn that started; -1 if none. */
function activeIndex(t: Transcript): number {
  let index = -1;
  for (let i = t.turns.length - 1; i >= 0; i--) {
    const turn = t.turns[i];
    if (turn.started_at && !turn.completed_at) return i;
    if (index < 0 && turn.started_at) index = i;
  }
  return index;
}

/** Changes the items of the running turn, else of the last turn that started. */
function withActive(t: Transcript, seq: number, change: (items: TranscriptItem[]) => TranscriptItem[]): Transcript {
  const index = activeIndex(t);
  if (index < 0) return { ...t, last_seq: seq };
  const turns = t.turns.slice();
  turns[index] = { ...turns[index], items: change(turns[index].items) };
  return { ...t, turns, last_seq: seq };
}
