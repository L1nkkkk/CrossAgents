// Folding a turn's steps (docs/tasks/V4-GUI.md §3, after Cutex DESIGN §4.14). Pure, tested in
// workGroups.test.ts.
//
// Tool calls and reasoning are *steps* and fold; agent text and errors do not. A run of steps
// stays open (newest few shown) until something that does not fold follows it; then it folds
// to one summary line, e.g. "4 个命令 · 2 次编辑". Failed steps stay visible under the line.
// A run of a single step is never folded.

import type { TranscriptItem } from "@bindings/TranscriptItem";

export type Step =
  | Extract<TranscriptItem, { type: "tool_call" }>
  | Extract<TranscriptItem, { type: "output" }>
  | Extract<TranscriptItem, { type: "permission" }>;

export type Block =
  | { kind: "item"; item: TranscriptItem }
  /** `closed`: something that does not fold came after, so the run folds to its summary. */
  | { kind: "steps"; steps: Step[]; closed: boolean };

export const isStep = (item: TranscriptItem): item is Step =>
  item.type === "tool_call" || item.type === "permission" || (item.type === "output" && item.stream === "thought");

export function workGroups(items: TranscriptItem[]): Block[] {
  const blocks: Block[] = [];
  let run: Step[] = [];
  const flush = (closed: boolean) => {
    if (run.length) blocks.push({ kind: "steps", steps: run, closed: closed && run.length > 1 });
    run = [];
  };
  for (const item of items) {
    if (isStep(item)) {
      run.push(item);
    } else {
      flush(true);
      blocks.push({ kind: "item", item });
    }
  }
  flush(false);
  return blocks;
}

type Category = "command" | "edit" | "read" | "search" | "thought" | "tool" | "permission";

function category(step: Step): Category {
  if (step.type === "output") return "thought";
  if (step.type === "permission") return "permission";
  switch (step.kind) {
    case "execute":
      return "command";
    case "edit":
    case "delete":
    case "move":
      return "edit";
    case "read":
      return "read";
    case "search":
    case "fetch":
      return "search";
    case "think":
      return "thought";
    default:
      return "tool";
  }
}

const PHRASE: Record<Category, (n: number) => string> = {
  command: (n) => `${n} 个命令`,
  edit: (n) => `${n} 次编辑`,
  read: (n) => `读了 ${n} 个文件`,
  search: (n) => `${n} 次搜索`,
  tool: (n) => `${n} 次工具调用`,
  thought: (n) => `推理 ${n} 段`,
  permission: (n) => `${n} 次授权`,
};
const ORDER: Category[] = ["command", "edit", "read", "search", "tool"];

export const failed = (step: Step) => step.type === "tool_call" && step.status === "failed";

/** One line for a folded run: what it did, by kind; reasoning is named only when alone. */
export function summarize(steps: Step[]): string {
  const counts = new Map<Category, number>();
  for (const step of steps) counts.set(category(step), (counts.get(category(step)) ?? 0) + 1);
  const parts = ORDER.filter((c) => counts.has(c)).map((c) => PHRASE[c](counts.get(c)!));
  if (!parts.length && counts.has("thought")) parts.push(PHRASE.thought(counts.get("thought")!));
  const bad = steps.filter(failed).length;
  if (bad) parts.push(`${bad} 个失败`);
  return parts.join(" · ");
}

/** What a running turn is doing now, for its live status line. */
export function liveActivity(items: TranscriptItem[]): string {
  const last = items[items.length - 1];
  if (!last) return "思考中";
  if (last.type === "output") return last.stream === "thought" ? "推理中" : "回复中";
  if (last.type === "error" || last.type === "warning" || last.type === "permission") return "工作中";
  if (last.status === "completed" || last.status === "failed") return "工作中";
  switch (category(last)) {
    case "command":
      return "运行命令";
    case "edit":
      return "修改文件";
    case "read":
      return "读取文件";
    case "search":
      return "搜索";
    case "thought":
      return "推理中";
    default:
      return "调用工具";
  }
}

/** A tool step as one short line: `$ cmd`, `✎ file`, else its title. */
export function stepLine(step: Extract<TranscriptItem, { type: "tool_call" }>): string {
  switch (category(step)) {
    case "command":
      return `$ ${step.title}`;
    case "edit":
      return `✎ ${step.title.replace(/^Edit /, "")}`;
    default:
      return step.title;
  }
}

/** "42s", "3m 05s", "1h 02m". */
export function duration(ms: number): string {
  const secs = Math.max(0, Math.round(ms / 1000));
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m ${String(secs % 60).padStart(2, "0")}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
}
