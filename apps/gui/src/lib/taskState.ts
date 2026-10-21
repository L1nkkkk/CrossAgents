// How a task or run reads everywhere (docs/tasks/V4-GUI.md §2): a glyph plus one word, never
// colour alone, and the bucket that decides ordering and the "needs you" count. Pure, so it is
// tested (taskState.test.ts) and shared by the sidebar, cards, boards and strips.

import type { RunStatus } from "@bindings/RunStatus";
import type { Task } from "@bindings/Task";
import type { TaskStatus } from "@bindings/TaskStatus";

/** ○ waits on someone · ● moving · ✓ done · ✕ ended without a result. */
export type Glyph = "○" | "●" | "✓" | "✕";
export type StateTone = "accent" | "ok" | "warn" | "danger" | "muted";
/** In display order: what you must act on, what waits, what moves, what is over. */
export type Bucket = "needs_you" | "waiting" | "active" | "ended";

export type State = { glyph: Glyph; word: string; tone: StateTone; bucket: Bucket; live: boolean };

export const BUCKETS: Bucket[] = ["needs_you", "waiting", "active", "ended"];
export const BUCKET_LABEL: Record<Bucket, string> = { needs_you: "需要你", waiting: "等待", active: "进行中", ended: "已结束" };

const s = (glyph: Glyph, word: string, tone: StateTone, bucket: Bucket, live = false): State => ({ glyph, word, tone, bucket, live });

type Sibling = Pick<Task, "key" | "status">;

/** `siblings` (the run's tasks) lets a pending task say which dependencies it still waits for. */
export function taskState(task: Pick<Task, "status" | "attempts" | "depends_on">, siblings?: Sibling[]): State {
  const status: TaskStatus = task.status;
  switch (status) {
    case "pending": {
      const open = task.depends_on.filter((key) => !siblings || siblings.find((t) => t.key === key)?.status !== "merged");
      return s("○", open.length ? `等 ${open.join("、")}` : "排队中", "muted", "waiting");
    }
    case "running":
      return s("●", task.attempts > 1 ? `第 ${task.attempts} 次尝试` : "运行中", "accent", "active", true);
    case "waiting_answer":
      return s("○", "等 Planner 回答", "warn", "waiting");
    case "verifying":
      return s("●", "验收中", "accent", "active", true);
    case "retrying":
      return s("●", "重试中", "accent", "active", true);
    case "escalated":
      return s("●", "已升级", "warn", "active", true);
    case "reviewing":
      return s("●", "审查中", "accent", "active", true);
    case "passed":
      return s("○", "待合并", "ok", "waiting");
    case "merged":
      return s("✓", "已合并", "ok", "ended");
    case "failed":
      return s("✕", "失败", "danger", "ended");
    case "cancelled":
      return s("✕", "已取消", "muted", "ended");
  }
}

export function runState(status: RunStatus): State {
  switch (status) {
    case "planning":
      return s("●", "拆解中", "accent", "active", true);
    case "awaiting_approval":
      return s("○", "待审批", "warn", "needs_you");
    case "running":
      return s("●", "进行中", "accent", "active", true);
    case "paused":
      return s("○", "已暂停", "warn", "needs_you");
    case "completed":
      return s("✓", "已完成", "ok", "ended");
    case "failed":
      return s("✕", "失败", "danger", "ended");
    case "cancelled":
      return s("✕", "已取消", "muted", "ended");
  }
}

/** Tasks grouped by bucket in display order, empty groups left out; plan order inside. */
export function groupTasks<T extends Pick<Task, "status" | "attempts" | "depends_on">>(tasks: T[]): { bucket: Bucket; tasks: T[] }[] {
  return BUCKETS.map((bucket) => ({ bucket, tasks: tasks.filter((t) => taskState(t).bucket === bucket) })).filter((g) => g.tasks.length);
}

/** "3/5 已合并 · 2 进行中", the run's progress in one phrase. */
export function progress(tasks: Pick<Task, "status" | "attempts" | "depends_on">[]): string {
  if (!tasks.length) return "";
  const merged = tasks.filter((t) => t.status === "merged").length;
  const active = tasks.filter((t) => taskState(t).bucket === "active").length;
  const failed = tasks.filter((t) => t.status === "failed").length;
  return [`${merged}/${tasks.length} 已合并`, active && `${active} 进行中`, failed && `${failed} 失败`].filter(Boolean).join(" · ");
}
