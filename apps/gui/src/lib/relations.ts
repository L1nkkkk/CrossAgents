// What an agent is doing right now, in words. `GET /v1/agents` carries, on each agent, `now`
// (the run it leads, or the task it works on or reviews; null when idle), `session_of` (for an
// execution session: the agent it runs for) and `strengths`. Older hubs send none of them, so
// every field is read structurally and is optional: no relation, no strengths, the raw name.

export type RelationKind = "lead" | "working" | "reviewing";

export type AgentNow = { kind: RelationKind; run_id: string; run_title: string; task_key: string | null };
export type SessionOf = { id: string; name: string; task_key: string | null };

/** The fields the GUI reads beside the generated `Agent` binding. */
export type AgentRelations = {
  now?: AgentNow | null;
  session_of?: SessionOf | null;
  strengths?: string[];
};

type Named = { name: string };

const KINDS: readonly RelationKind[] = ["lead", "working", "reviewing"];

/** The agent's `now`, when the hub sent a well-formed one; null for idle, undefined when absent. */
export function nowOf(agent: unknown): AgentNow | null | undefined {
  const raw = (agent as { now?: unknown } | null | undefined)?.now;
  if (raw === null) return null;
  if (!raw || typeof raw !== "object") return undefined;
  const n = raw as Partial<AgentNow>;
  if (!n.kind || !KINDS.includes(n.kind)) return undefined;
  return {
    kind: n.kind,
    run_id: typeof n.run_id === "string" ? n.run_id : "",
    run_title: typeof n.run_title === "string" ? n.run_title : "",
    task_key: typeof n.task_key === "string" && n.task_key ? n.task_key : null,
  };
}

/** The relation kind, or "idle"; undefined when the hub says nothing (an older hub). */
export function relationKind(agent: unknown): RelationKind | "idle" | undefined {
  const now = nowOf(agent);
  if (now === undefined) return undefined;
  return now ? now.kind : "idle";
}

/** 「主导 <run>」/「在做 #key」/「在审 #key」/「空闲」; "" when the hub does not say. */
export function relationText(agent: unknown): string {
  const now = nowOf(agent);
  if (now === undefined) return "";
  if (now === null) return "空闲";
  const key = now.task_key ? ` #${now.task_key}` : "";
  switch (now.kind) {
    case "lead":
      return `主导 ${now.run_title || "一个 run"}`.trim();
    case "working":
      return key ? `在做${key}` : `在做 ${now.run_title}`.trim();
    case "reviewing":
      return key ? `在审${key}` : `在审 ${now.run_title}`.trim();
  }
}

/** The agent an execution session runs for, when it is one. */
export function sessionOfOf(agent: unknown): SessionOf | undefined {
  const raw = (agent as { session_of?: unknown } | null | undefined)?.session_of;
  if (!raw || typeof raw !== "object") return undefined;
  const s = raw as Partial<SessionOf>;
  if (typeof s.name !== "string" || !s.name.trim()) return undefined;
  return { id: typeof s.id === "string" ? s.id : "", name: s.name, task_key: typeof s.task_key === "string" && s.task_key ? s.task_key : null };
}

/** 「Mira 2 · misc」 for an execution session (its source agent · task key), else the name. */
export function sessionLabel(agent: Named): string {
  const of = sessionOfOf(agent);
  if (!of) return agent.name;
  return of.task_key ? `${of.name} · ${of.task_key}` : of.name;
}

/** What the agent is good at, as the hub lists it; empty when absent. */
export function strengthsOf(agent: unknown): string[] {
  const raw = (agent as { strengths?: unknown } | null | undefined)?.strengths;
  if (!Array.isArray(raw)) return [];
  return raw.filter((s): s is string => typeof s === "string" && !!s.trim()).map((s) => s.trim());
}

/** Whether the agent leads a run right now (the flag on its avatar). */
export const isLeading = (agent: unknown) => nowOf(agent)?.kind === "lead";
