/**
 * Human-work derivation, independent of streamed activity. Run approval waiting
 * times are approximate: the backend does not record when a run entered that
 * state, so without an explicit pending_since we can only use run.created_at.
 * Tasks instead fall back to their latest persisted handoff, then created_at.
 */
import type { AgentId } from "@bindings/AgentId";
import type { HumanItem } from "@bindings/HumanItem";
import type { Run } from "@bindings/Run";
import type { Task } from "@bindings/Task";
import { planWaitsForPerson } from "./policy.ts";
import { runState } from "./taskState.ts";

export type TodoKind = "question" | "notice" | "approval" | "acceptance" | "failure";
export type TodoItem = {
  /** The source's persistent HumanItem.id, RunId or TaskId. */
  id: string;
  agent: AgentId;
  kind: TodoKind;
  summary: string;
  action: "answer" | "view" | "confirm";
  href: string | undefined;
  /** Explanation to display when the processing destination is unavailable. */
  reason?: string;
  since: string;
};

/**
 * Supply the persisted state-transition time when available. Current bindings have no
 * dedicated field for it; old records use the stable fallbacks above, never updated_at
 * (progress, policy edits and other activity can change that without a new wait).
 */
type Waiting = { pending_since?: string | null };
export type TodoRun = { run: Run & Waiting; tasks: readonly (Task & Waiting)[] };
export type TodoInput = { items: readonly HumanItem[]; runs: readonly TodoRun[]; now: number };

const path = (...parts: string[]) => "#/" + parts.map(encodeURIComponent).join("/");

/**
 * A pure snapshot of server-confirmed human work. Viewing, unread marks, archived
 * agents and sidebar filters are deliberately not inputs. Pass the complete run list.
 * Question routes carry the HumanItem ID for the consumer to focus its question card.
 */
export function todoItems({ items, runs, now }: TodoInput): TodoItem[] {
  const out = new Map<string, TodoItem>();
  const byRun = new Map(runs.map((detail) => [detail.run.id, detail]));
  const add = (source: string, item: TodoItem) => {
    if (!out.has(source)) out.set(source, item);
  };
  for (const item of items) {
    // Unlike pending()'s recent defaults, these still require a human response.
    if (item.status !== "open") continue;
    const approval = item.kind !== "notice" && item.action === "approval";
    const detail = item.run_id ? byRun.get(item.run_id) : undefined;
    let reason: string | undefined;
    if (item.run_id && !detail) reason = "关联的工作不可用，可能已被删除。";
    else if (item.task_id && !detail?.tasks.some((task) => task.id === item.task_id)) reason = "关联的任务找不到，暂时无法打开处理位置。";
    const href = reason ? undefined : approval && item.run_id
      ? item.task_id ? path("runs", item.run_id, "tasks", item.task_id) : path("runs", item.run_id)
      : path("s", item.from, "questions", item.id);
    add(`human:${item.id}`, {
      id: item.id, agent: item.from,
      kind: item.kind === "notice" ? "notice" : approval ? "approval" : "question",
      summary: item.text, action: item.kind === "notice" ? "view" : approval ? "confirm" : "answer",
      href, ...(reason ? { reason } : {}), since: item.created_at,
    });
  }
  for (const { run, tasks } of runs) {
    // An actual open HumanItem remains actionable under any policy (e.g. a tool
    // approval). Only synthetic plan approvals are gated by the plan policy.
    const hasApproval = items.some((item) => item.status === "open" && item.action === "approval" && item.run_id === run.id && !item.task_id);
    if (run.status === "awaiting_approval" && runState(run.status).bucket === "needs_you"
      && planWaitsForPerson(run) && !hasApproval) {
      add(`run:${run.id}`, { id: run.id, agent: run.planner, kind: "approval", summary: run.title,
        action: "confirm", href: path("runs", run.id), since: run.pending_since ?? run.created_at });
    }
    // A failed task in a running run can still be recovered by its issuer. A
    // paused/failed run is the durable signal that the person must intervene.
    if (run.status === "failed" || run.status === "paused") {
      add(`run:${run.id}`, { id: run.id, agent: run.planner, kind: "failure", summary: run.note ?? run.title,
        action: "view", href: path("runs", run.id), since: run.pending_since ?? run.created_at });
    }
    if (run.status === "completed" || run.status === "cancelled" || run.status === "failed") continue;
    for (const task of tasks) {
      if (task.status !== "passed") continue;
      add(`task:${task.id}`, { id: task.id, agent: task.assignee ?? task.issuer ?? run.planner, kind: "acceptance",
        summary: task.title, action: "confirm", href: path("runs", run.id, "tasks", task.id),
        since: task.pending_since ?? task.handoffs?.at(-1)?.at ?? task.created_at });
    }
  }
  // Invalid legacy timestamps sort last; now is only a deterministic fallback,
  // never a timeout that silently removes an open item.
  const time = (item: TodoItem) => Number.isFinite(Date.parse(item.since)) ? Date.parse(item.since) : now;
  return [...out.values()].sort((a, b) => time(a) - time(b));
}

/** remaining is the N in the consumer's “还有 N 项” expansion control. */
export function visibleTodos<T>(items: readonly T[], { cap = 3, expanded = false }: { cap?: number; expanded?: boolean } = {}): { items: T[]; remaining: number } {
  const limit = Number.isFinite(cap) ? Math.max(0, Math.floor(cap)) : 3;
  const shown = expanded ? [...items] : items.slice(0, limit);
  return { items: shown, remaining: items.length - shown.length };
}

/** Counts work items, including multiple items from the same agent. */
export function todoCount(items: readonly TodoItem[]): number {
  return items.length;
}
