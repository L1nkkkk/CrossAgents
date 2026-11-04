// What the 后台 tab (ToolColumn.tsx) shows: the agents' reminders, their watches and the tasks
// being verified (`GET /v1/background`), split into what concerns the conversation or run in
// front and the rest, which the tab folds away. Pure; tested in background.test.ts.

import type { BackgroundReminder } from "@bindings/BackgroundReminder";
import type { BackgroundResponse } from "@bindings/BackgroundResponse";
import type { BackgroundWatch } from "@bindings/BackgroundWatch";
import type { VerifyingTask } from "@bindings/VerifyingTask";
import type { WatchOn } from "@bindings/WatchOn";

/** What the tab is in front of: the followed conversation's agent id, the routed run. */
export type Focus = { agent?: string; run?: string };

export type Split<T> = { near: T[]; other: T[] };
export type Board = { reminders: Split<BackgroundReminder>; watches: Split<BackgroundWatch>; verifying: Split<VerifyingTask> };

const watchedAgent = (w: BackgroundWatch) => (w.watch.target.kind === "agent" ? w.watch.target.agent_id : undefined);

/**
 * The runs the focus touches: the routed run, and the runs of whatever the followed agent set,
 * watches, is watched by or has in verification.
 */
export function focusRuns(bg: BackgroundResponse, focus: Focus): Set<string> {
  const runs = new Set<string>();
  if (focus.run) runs.add(focus.run);
  if (!focus.agent) return runs;
  for (const r of bg.reminders) if (r.reminder.owner === focus.agent && r.run_id) runs.add(r.run_id);
  for (const w of bg.watches) if ((w.watch.owner === focus.agent || watchedAgent(w) === focus.agent) && w.run_id) runs.add(w.run_id);
  for (const v of bg.verifying) if (v.assignee === focus.agent) runs.add(v.run_id);
  return runs;
}

function split<T>(items: T[], near: (item: T) => boolean): Split<T> {
  const out: Split<T> = { near: [], other: [] };
  for (const item of items) (near(item) ? out.near : out.other).push(item);
  return out;
}

/** Sorts the board into near and other. With no focus at all, everything is near. */
export function board(bg: BackgroundResponse, focus: Focus): Board {
  const any = !!(focus.agent || focus.run);
  const runs = focusRuns(bg, focus);
  const inRun = (run: string | null) => !!run && runs.has(run);
  const mine = (id: string | null | undefined) => !!id && id === focus.agent;
  return {
    reminders: split(bg.reminders, (r) => !any || mine(r.reminder.owner) || inRun(r.run_id)),
    watches: split(bg.watches, (w) => !any || mine(w.watch.owner) || mine(watchedAgent(w)) || inRun(w.run_id)),
    verifying: split(bg.verifying, (v) => !any || mine(v.assignee) || inRun(v.run_id)),
  };
}

/** Removes what an event says is over: a reminder that fired. Watches and tasks are re-read. */
export function withoutReminder(bg: BackgroundResponse, id: string): BackgroundResponse {
  return { ...bg, reminders: bg.reminders.filter((r) => r.reminder.id !== id) };
}

/** "还有 12 分 05 秒", "还有 2 小时 3 分", "还有 3 天 4 小时", or "已到期" once it is due. */
export function countdown(due: string | Date, now: Date = new Date()): string {
  const at = due instanceof Date ? due : new Date(due);
  if (Number.isNaN(at.getTime())) return String(due);
  const s = Math.floor((at.getTime() - now.getTime()) / 1000);
  if (s <= 0) return "已到期";
  const d = Math.floor(s / 86_400);
  const h = Math.floor((s % 86_400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d > 0) return `还有 ${d} 天 ${h} 小时`;
  if (h > 0) return `还有 ${h} 小时 ${m} 分`;
  if (m > 0) return `还有 ${m} 分 ${String(sec).padStart(2, "0")} 秒`;
  return `还有 ${sec} 秒`;
}

/** How long something has been going: "3 分 12 秒", "1 小时 5 分". */
export function elapsed(since: string | Date, now: Date = new Date()): string {
  const at = since instanceof Date ? since : new Date(since);
  if (Number.isNaN(at.getTime())) return String(since);
  const s = Math.max(0, Math.floor((now.getTime() - at.getTime()) / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${String(s % 60).padStart(2, "0")} 秒`;
  return `${s} 秒`;
}

const ON: Record<WatchOn, string> = { done: "完成", failed: "失败", asked: "提问", stalled: "卡住", idle: "空闲", any: "任何变化" };

/** The events a watch reports, as the tab reads them: "完成、失败". */
export const watchOn = (on: WatchOn[]) => (on.length ? on.map((o) => ON[o] ?? o).join("、") : ON.any);
