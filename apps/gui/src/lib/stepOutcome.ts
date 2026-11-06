// Naming a folded run of steps by what it produced (docs/research/magpie-lancer.md §2.7, after
// Lancer): 「改了 3 个文件 · 跑了测试：通过 · 5 步」 rather than a count per kind. The artifacts it
// left — images, changed files — come with it, so a folded line can show thumbnails under
// itself. Pure, tested in stepOutcome.test.ts.

import { toolChanges, toolFailed } from "./toolcall.ts";
import { extractToolImagePaths, extractToolImages } from "./toolimages.ts";
import { summarize, type Step } from "./workGroups.ts";

type ToolCall = Extract<Step, { type: "tool_call" }>;

const isCall = (step: Step): step is ToolCall => step.type === "tool_call";

/** What a call is, in the terms the summary speaks: a command, an edit, a read, a search. */
function kindOf(call: ToolCall): "command" | "edit" | "read" | "search" | "tool" {
  switch (call.kind) {
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
    default:
      return "tool";
  }
}

/** A command that runs tests: what it says about the work is "pass" or "fail", not "ran". */
const TEST = /(^|[\s/\\])(cargo test|npm test|npm run test|pnpm test|pnpm run test|yarn test|vitest|jest|pytest|go test|ctest|dotnet test|make test)(\s|$)/i;
export const isTestCommand = (text: string) => TEST.test(text);

/** The files a call changed; a failed call changed nothing, so it names none. */
function changedFiles(call: ToolCall): string[] {
  const fromDiff = toolChanges(call).map((c) => c.path);
  if (fromDiff.length || toolFailed(call)) return fromDiff;
  // A vendor that reports no diff still names its file in the title: "Edit src/auth.rs".
  if (kindOf(call) !== "edit") return [];
  const title = call.title.replace(/^(Edit|Write|Apply|Delete|Move)\s+/i, "").trim();
  return /[./\\]/.test(title) ? [title] : [];
}

/** What a run of steps left behind: images it produced, and the files it changed. */
export type StepArtifacts = {
  /** Image data URLs, ready to show. */
  images: string[];
  /** Images named by path on an agent's machine: loaded by the view, which knows the agent. */
  imagePaths: string[];
  /** Files changed, in the order they were first touched. */
  files: string[];
};

export type StepOutcome = {
  /** The folded line: what the run produced, then how many calls it took. */
  title: string;
  /** Calls that failed (a test that failed counts here too). */
  failed: number;
  artifacts: StepArtifacts;
};

/**
 * One line for a run of steps, by what it produced: files changed, tests passed or failed,
 * images made, commands run, files read. Nothing that produced anything falls back to
 * `summarize`, which counts by kind.
 */
export function outcomeOf(steps: Step[]): StepOutcome {
  const artifacts: StepArtifacts = { images: [], imagePaths: [], files: [] };
  const seen = new Set<string>();
  let tests = 0;
  let testsFailed = 0;
  let commands = 0;
  let reads = 0;
  let searches = 0;
  let images = 0;
  let failed = 0;
  let calls = 0;

  for (const step of steps) {
    if (!isCall(step)) continue;
    calls += 1;
    if (toolFailed(step)) failed += 1;
    const kind = kindOf(step);
    if (kind === "command") {
      const text = step.detail?.command ?? step.title;
      if (isTestCommand(text)) {
        tests += 1;
        if (toolFailed(step) || (step.detail?.exit_code ?? 0) !== 0) testsFailed += 1;
      } else {
        commands += 1;
      }
      continue;
    }
    if (kind === "edit") {
      for (const path of changedFiles(step)) {
        if (seen.has(path)) continue;
        seen.add(path);
        artifacts.files.push(path);
      }
      continue;
    }
    if (kind === "read") {
      reads += 1;
      continue;
    }
    if (kind === "search") {
      searches += 1;
      continue;
    }
    // Anything else may have made a picture (an MCP tool, a screenshot).
    const fromResult = step.detail?.result ? extractToolImages(step.detail.result).images : [];
    const fromArgs = step.detail?.arguments ? extractToolImagePaths(step.detail.arguments) : [];
    artifacts.images.push(...fromResult);
    for (const path of fromArgs) if (!artifacts.imagePaths.includes(path)) artifacts.imagePaths.push(path);
    images += fromResult.length + fromArgs.length;
  }

  const parts: string[] = [];
  if (artifacts.files.length) parts.push(`改了 ${artifacts.files.length} 个文件`);
  if (tests === 1) parts.push(testsFailed ? "跑了测试：✕ 失败" : "跑了测试：通过");
  else if (tests > 1) parts.push(`跑了 ${tests} 个测试命令：${testsFailed ? "有失败" : "都通过"}`);
  if (images) parts.push(`出了 ${images} 张图`);
  if (commands) parts.push(`跑了 ${commands} 个命令`);
  if (reads) parts.push(`读了 ${reads} 个文件`);
  if (searches) parts.push(`搜了 ${searches} 次`);
  if (failed && !testsFailed) parts.push(`${failed} 个失败`);
  if (calls) parts.push(`${calls} 步`);
  return { title: parts.length ? parts.join(" · ") : summarize(steps), failed, artifacts };
}
