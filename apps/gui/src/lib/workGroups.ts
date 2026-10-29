// Folding a turn's steps (docs/tasks/V4-GUI.md §3, after Cutex DESIGN §4.14). Pure, tested in
// workGroups.test.ts.
//
// Tool calls and reasoning are *steps* and fold; agent text and errors do not. A run of steps
// stays open (newest few shown) until something that does not fold follows it; then it folds
// to one summary line, e.g. "4 个命令 · 2 次编辑". Failed steps stay visible under the line.
// A run of a single step is never folded.
//
// Calls that change the team or the code (recruit, retire, switch a model, merge, land, finish)
// are not steps: each is a card of its own with its outcome, so what really happened is never
// folded away under a summary line (the agent's prose may say otherwise).

import type { TranscriptItem } from "@bindings/TranscriptItem";
import { tailLines } from "./diff.ts";
import { toolFailed } from "./toolcall.ts";

export type Step =
  | Extract<TranscriptItem, { type: "tool_call" }>
  | Extract<TranscriptItem, { type: "output" }>
  | Extract<TranscriptItem, { type: "permission" }>;

export type Block =
  | { kind: "item"; item: TranscriptItem }
  /** `closed`: something that does not fold came after, so the run folds to its summary. */
  | { kind: "steps"; steps: Step[]; closed: boolean };

/** The CrossAgents tools whose calls change something: shown as cards, never folded. */
export const ACTION_TOOLS = ["agent_spawn", "agent_retire", "agent_set_model", "task_accept", "task_drop", "run_land", "run_finish", "plan_submit"] as const;
export type ActionTool = (typeof ACTION_TOOLS)[number];

/** "crossagents.agent_spawn" or "mcp__crossagents__agent_spawn" → "agent_spawn"; other tools → undefined. */
export function actionOf(item: TranscriptItem): ActionTool | undefined {
  if (item.type !== "tool_call" || !/crossagents/i.test(item.title)) return undefined;
  const name = item.title.split(/\.|__/).pop() ?? "";
  return (ACTION_TOOLS as readonly string[]).includes(name) ? (name as ActionTool) : undefined;
}

export const isStep = (item: TranscriptItem): item is Step =>
  (item.type === "tool_call" && !actionOf(item)) || item.type === "permission" || (item.type === "output" && item.stream === "thought");

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

// A failure is what toolcall.ts says it is: the status, or an error recorded with the call.
export const failed = (step: Step) => step.type === "tool_call" && toolFailed(step);

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

/** After this long with nothing new, a running turn is called quiet: it may be waiting, not working. */
export const QUIET_AFTER = 30;

/** Seconds since the turn last produced anything, for "已 N 秒没有新输出" (never negative). */
export function quietSeconds(changedAt: number, now: number): number {
  return Math.max(0, Math.floor((now - changedAt) / 1000));
}

/** The size of one item's content: equal strings mean it produced nothing new. */
function produced(item: TranscriptItem): string {
  switch (item.type) {
    case "output":
      return `${item.stream}:${item.text.length}`;
    case "tool_call":
      return `${item.tool_call_id}:${item.status}:${item.output?.length ?? 0}:${item.detail?.arguments?.length ?? 0}`;
    default:
      return item.type;
  }
}

/** A key that changes exactly when the turn produced something (a step, more output, more text). */
export function liveKey(items: TranscriptItem[]): string {
  return `${items.length}|${items.slice(-3).map(produced).join("|")}`;
}

export type LiveNow = {
  /** The newest thing in one line: a step as `stepLine` writes it, or the last line of a stream. */
  title: string;
  /** A running command's newest output lines; `hidden` older ones were left out above them. */
  output?: { lines: string[]; hidden: number };
};

/**
 * What a running turn is producing right now: the last step's line and, while a command still
 * runs, the tail of its output (`maxLines`, as the step itself shows while running). A stream of
 * reasoning or text is its last line only: the whole thing is in the steps above.
 */
export function liveNow(items: TranscriptItem[], cwd?: string, maxLines = 3): LiveNow | undefined {
  const last = items[items.length - 1];
  if (!last) return undefined;
  if (last.type === "output") {
    const line = last.text
      .split("\n")
      .map((l) => l.trimEnd())
      .filter((l) => l.length > 0)
      .pop();
    return line === undefined ? undefined : { title: line };
  }
  if (last.type !== "tool_call") return undefined;
  const title = stepLine(last, cwd);
  const live = last.status === "in_progress" || last.status === "pending";
  if (!live || !last.output) return { title };
  const tail = tailLines(last.output, maxLines);
  const lines = tail.text.split("\n").filter((l) => l.length > 0);
  return lines.length ? { title, output: { lines, hidden: tail.hidden } } : { title };
}

/** `text` with the working directory left out of the paths in it: `C:\w\src\a.rs` → `src\a.rs`,
 * and the directory itself as `.`. */
export function relativeTo(text: string, cwd?: string): string {
  const dir = cwd?.replace(/[\\/]+$/, "");
  if (!dir) return text;
  // Either slash, any case on Windows paths; then a separator, or the end of the path.
  const pattern = dir
    .split(/[\\/]/)
    .map((part) => part.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"))
    .join("[\\\\/]");
  const flags = /^[a-z]:/i.test(dir) ? "gi" : "g";
  return text.replace(new RegExp(`${pattern}(?:[\\\\/]|(?=["'\\s]|$))`, flags), (m) => (/[\\/]$/.test(m) ? "" : "."));
}

/** A tool step as one short line: `$ cmd`, `✎ file`, else its title; paths inside `cwd` are shortened. */
export function stepLine(step: Extract<TranscriptItem, { type: "tool_call" }>, cwd?: string): string {
  const title = relativeTo(step.title, cwd);
  switch (category(step)) {
    case "command":
      return `$ ${title}`;
    case "edit":
      return `✎ ${title.replace(/^(Edit|Write) /, "")}`;
    default:
      return title;
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
