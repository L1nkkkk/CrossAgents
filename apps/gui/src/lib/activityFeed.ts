// The combined activity feed (docs/TODO.md §二十五 第 7 条): every agent's replies, tool calls
// and messages (including agent-to-agent `agent_send` and the person's own) on one timeline.
//
// Everything here is pure: the hub events become entries, the filters keep what is asked for,
// and an agent's subordinates are read from the `spawned_by_agent` config the hub already
// records. Only events this page has seen can be shown; the view says so.

import type { Agent } from "@bindings/Agent";
import type { MessageSource } from "@bindings/MessageSource";
import type { HubEvent } from "../hub.ts";

export const FEED_TYPES = ["error", "reply", "tool", "message"] as const;
export type FeedType = (typeof FEED_TYPES)[number];

/** One line of the timeline. */
export type FeedEntry = {
  /** Stable within the page: a reply that keeps streaming stays one entry. */
  id: string;
  /** The newest event behind it, for ordering (newest first). */
  seq: number;
  at: string;
  agentId: string;
  type: FeedType;
  /** What happened, without the agent or sender prefix (`feedSummary` adds it). */
  text: string;
  /** The turn it belongs to, when this page saw one start; `[data-turn]` in a session. */
  turn?: string;
  /** Who sent a message to the agent (`agent.input`, `agent.steered`). */
  from?: MessageSource;
  /** The agent spoke to the person instead (`human.asked`). */
  toHuman?: boolean;
  /** A tool call the hub reported as failed. */
  failed?: boolean;
};

/** What this page watched of each agent's work, in the order the events came. */
export function buildFeed(events: readonly HubEvent[]): FeedEntry[] {
  const out: FeedEntry[] = [];
  const byId = new Map<string, FeedEntry>();
  /** The turn each agent is in, as `agent.turn_started` / `agent.turn_completed` bracket it. */
  const turnOf = new Map<string, string>();
  /** The reply still taking text, per agent: consecutive `agent.output` chunks are one line. */
  const openReply = new Map<string, FeedEntry>();

  const put = (entry: FeedEntry) => {
    out.push(entry);
    byId.set(entry.id, entry);
  };
  const closeReply = (agentId: string) => openReply.delete(agentId);

  for (const event of events) {
    switch (event.type) {
      case "agent.turn_started": {
        turnOf.set(event.data.id, event.data.message_id);
        closeReply(event.data.id);
        break;
      }
      case "agent.turn_completed": {
        const agentId = event.data.id;
        if (turnOf.get(agentId) === event.data.message_id) turnOf.delete(agentId);
        closeReply(agentId);
        break;
      }
      case "agent.output": {
        const { id, stream, text } = event.data;
        if (stream !== "text") {
          closeReply(id);
          break;
        }
        if (!text.trim()) break;
        const open = openReply.get(id);
        if (open) {
          open.text += text;
          open.seq = event.seq;
          open.at = event.ts;
          open.turn ??= turnOf.get(id);
          break;
        }
        const entry: FeedEntry = {
          id: `reply:${id}:${event.seq}`,
          seq: event.seq,
          at: event.ts,
          agentId: id,
          type: "reply",
          text,
          ...(turnOf.has(id) ? { turn: turnOf.get(id) } : {}),
        };
        openReply.set(id, entry);
        put(entry);
        break;
      }
      case "agent.tool_call": {
        const { id, tool_call_id, title, status } = event.data;
        closeReply(id);
        const key = `tool:${id}:${tool_call_id}`;
        const existing = byId.get(key);
        if (existing) {
          existing.text = title;
          existing.failed = status === "failed";
          existing.seq = event.seq;
          existing.at = event.ts;
          break;
        }
        put({
          id: key,
          seq: event.seq,
          at: event.ts,
          agentId: id,
          type: "tool",
          text: title,
          ...(turnOf.has(id) ? { turn: turnOf.get(id) } : {}),
          ...(status === "failed" ? { failed: true } : {}),
        });
        break;
      }
      case "agent.tool_call_updated": {
        const { id, tool_call_id, title, status } = event.data;
        closeReply(id);
        const entry = byId.get(`tool:${id}:${tool_call_id}`);
        if (!entry) break;
        if (title) entry.text = title;
        if (status === "failed") entry.failed = true;
        entry.seq = event.seq;
        entry.at = event.ts;
        break;
      }
      case "agent.error":
      case "agent.warning": {
        const agentId = event.data.id;
        closeReply(agentId);
        const text = event.type === "agent.warning" ? `警告：${event.data.message}` : event.data.message;
        put({
          id: `error:${agentId}:${event.seq}`,
          seq: event.seq,
          at: event.ts,
          agentId,
          type: "error",
          text,
          ...(turnOf.has(agentId) ? { turn: turnOf.get(agentId) } : {}),
        });
        break;
      }
      case "agent.input": {
        const { id, message_id, text } = event.data;
        // Before the field existed, an input event was a person's message (crates/api-types Event).
        const from: MessageSource = event.data.from ?? { kind: "human" };
        closeReply(id);
        const key = `message:${id}:${message_id}`;
        const existing = byId.get(key);
        if (existing) {
          existing.text = text;
          existing.from = from;
          existing.seq = event.seq;
          existing.at = event.ts;
          break;
        }
        put({
          id: key,
          seq: event.seq,
          at: event.ts,
          agentId: id,
          type: "message",
          text,
          // The turn that answers it has this message id (agent.turn_started).
          turn: message_id,
          from,
        });
        break;
      }
      case "agent.input_updated": {
        const { id, message_id, text } = event.data;
        const entry = byId.get(`message:${id}:${message_id}`);
        if (!entry) break;
        entry.text = text;
        entry.seq = event.seq;
        entry.at = event.ts;
        break;
      }
      case "agent.steered": {
        const { id, text } = event.data;
        closeReply(id);
        put({
          id: `steer:${id}:${event.seq}`,
          seq: event.seq,
          at: event.ts,
          agentId: id,
          type: "message",
          text,
          from: { kind: "human" },
          ...(turnOf.has(id) ? { turn: turnOf.get(id) } : {}),
        });
        break;
      }
      case "human.asked": {
        const { item } = event.data;
        closeReply(item.from);
        put({
          id: `asked:${item.id}`,
          seq: event.seq,
          at: event.ts,
          agentId: item.from,
          type: "message",
          text: item.text,
          toHuman: true,
          ...(turnOf.has(item.from) ? { turn: turnOf.get(item.from) } : {}),
        });
        break;
      }
      case "human.answered": {
        const { item } = event.data;
        const answer = item.answer?.trim();
        if (!answer) break;
        closeReply(item.from);
        put({
          id: `answered:${item.id}`,
          seq: event.seq,
          at: event.ts,
          agentId: item.from,
          type: "message",
          text: `回答：${answer}`,
          from: { kind: "human" },
          ...(turnOf.has(item.from) ? { turn: turnOf.get(item.from) } : {}),
        });
        break;
      }
      default:
        break;
    }
  }

  return out.sort((a, b) => b.seq - a.seq);
}

/** Who a message came from, as a person reads it; undefined when the event did not say. */
export function senderName(from: MessageSource | undefined, nameOf: (id: string) => string | undefined): string | undefined {
  if (!from) return undefined;
  if (from.kind === "human") return "你";
  if (from.kind === "agent") return nameOf(from.agent_id) ?? "某个 agent";
  if (from.kind === "orchestrator") return "调度";
  return "通知";
}

/** The one-line summary the row shows: the entry's text, with who said what for a message. */
export function feedSummary(entry: FeedEntry, nameOf: (id: string) => string | undefined): string {
  const agent = nameOf(entry.agentId) ?? "某个会话";
  if (entry.type === "message") {
    if (entry.toHuman) return `${agent} → 你：${entry.text}`;
    const from = senderName(entry.from, nameOf);
    return from ? `${from} → ${agent}：${entry.text}` : `${agent}：${entry.text}`;
  }
  return entry.failed ? `${entry.text}（失败）` : entry.text;
}

export type FeedScope = { kind: "all" } | { kind: "agent"; agent: string } | { kind: "team"; agent: string };

/** The agents `root` spawned, and theirs, from the `spawned_by_agent` config the hub keeps. */
export function descendantsOf(agents: readonly Agent[], root: string): Set<string> {
  const children = new Map<string, string[]>();
  for (const agent of agents) {
    const config = agent.config as Record<string, unknown> | null | undefined;
    const by = config?.spawned_by_agent;
    if (typeof by !== "string" || !by) continue;
    const list = children.get(by);
    if (list) list.push(agent.id);
    else children.set(by, [agent.id]);
  }
  const seen = new Set<string>([root]);
  const queue = [root];
  while (queue.length) {
    const id = queue.shift()!;
    for (const child of children.get(id) ?? []) {
      if (seen.has(child)) continue;
      seen.add(child);
      queue.push(child);
    }
  }
  return seen;
}

/** The agents in scope, or null for every agent. */
export function scopeAgentIds(agents: readonly Agent[], scope: FeedScope): Set<string> | null {
  if (scope.kind === "all") return null;
  if (scope.kind === "agent") return new Set([scope.agent]);
  return descendantsOf(agents, scope.agent);
}

/** The entries the filter leaves: one of `types`, and in `scope`. */
export function filterFeed(
  entries: readonly FeedEntry[],
  { types, scope, agents }: { types: ReadonlySet<FeedType>; scope: FeedScope; agents: readonly Agent[] },
): FeedEntry[] {
  const ids = scopeAgentIds(agents, scope);
  return entries.filter((entry) => types.has(entry.type) && (!ids || ids.has(entry.agentId)));
}
