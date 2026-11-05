import assert from "node:assert/strict";
import { test } from "node:test";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import { QUIET_AFTER, actionOf, duration, liveActivity, liveKey, liveNow, liveProgress, quietLabel, quietSeconds, relativeTo, stepLine, summarize, workGroups, type Step } from "./workGroups.ts";

const tool = (kind: string, status: "in_progress" | "completed" | "failed" = "completed", title = kind): TranscriptItem => ({
  type: "tool_call",
  tool_call_id: Math.random().toString(36),
  title,
  kind,
  status,
});
const text = (t: string): TranscriptItem => ({ type: "output", stream: "text", text: t });
const thought = (t: string): TranscriptItem => ({ type: "output", stream: "thought", text: t });

test("a run of steps folds once text follows it, and stays open at the end", () => {
  const blocks = workGroups([tool("read"), tool("execute"), text("found it"), tool("edit"), tool("execute")]);
  assert.deepEqual(
    blocks.map((b) => (b.kind === "steps" ? `steps:${b.steps.length}:${b.closed}` : "item")),
    ["steps:2:true", "item", "steps:2:false"],
  );
});

test("a single step is never folded", () => {
  const blocks = workGroups([tool("execute"), text("ok")]);
  assert.equal(blocks[0].kind === "steps" && blocks[0].closed, false);
});

test("reasoning folds with the tools; errors close the run", () => {
  const blocks = workGroups([thought("hmm"), tool("read"), { type: "error", message: "boom" }]);
  assert.equal(blocks.length, 2);
  assert.ok(blocks[0].kind === "steps" && blocks[0].closed && blocks[0].steps.length === 2);
});

test("the summary counts by kind, names reasoning only when alone, and counts failures", () => {
  const steps = [tool("execute"), tool("execute", "failed"), tool("edit"), thought("x"), tool("mcp")] as Step[];
  assert.equal(summarize(steps), "2 个命令 · 1 次编辑 · 1 次工具调用 · 1 个失败");
  assert.equal(summarize([thought("a"), thought("b")] as Step[]), "推理 2 段");
});

test("the live line says what the turn is doing now", () => {
  assert.equal(liveActivity([]), "思考中");
  assert.equal(liveActivity([tool("execute", "in_progress")]), "运行命令");
  assert.equal(liveActivity([tool("edit", "in_progress")]), "修改文件");
  assert.equal(liveActivity([thought("…")]), "推理中");
  assert.equal(liveActivity([text("…")]), "回复中");
  assert.equal(liveActivity([tool("execute")]), "工作中");
});

test("step lines and durations", () => {
  assert.equal(stepLine(tool("execute", "completed", "cargo test") as never), "$ cargo test");
  assert.equal(stepLine(tool("edit", "completed", "Edit src/a.rs") as never), "✎ src/a.rs");
  assert.equal(duration(42_000), "42s");
  assert.equal(duration(185_000), "3m 05s");
  assert.equal(duration(3_720_000), "1h 02m");
});

test("paths inside the working directory are shortened", () => {
  const cwd = "C:\\Users\\x\\repo";
  assert.equal(relativeTo("Read C:\\Users\\x\\repo\\README.md", cwd), "Read README.md");
  assert.equal(relativeTo('git -C "c:/users/x/repo/sub" diff', cwd), 'git -C "sub" diff');
  assert.equal(relativeTo("Read /w/a/b.rs", "/w/a/"), "Read b.rs");
  assert.equal(relativeTo("Read /w/ab/c.rs", "/w/a"), "Read /w/ab/c.rs");
  assert.equal(relativeTo('git -C "C:/Users/x/repo" diff', cwd), 'git -C "." diff');
  assert.equal(stepLine(tool("edit", "completed", "Write /w/a/n.txt") as never, "/w/a"), "✎ n.txt");
});

test("the shortened path uses /, and only inside the path", () => {
  const cwd = "C:\\Users\\x\\repo";
  // The bug from run-cleanup: the rest of the path kept Windows separators.
  assert.equal(relativeTo("Read C:\\Users\\x\\repo\\crates\\orchestrator\\src", cwd), "Read crates/orchestrator/src");
  assert.equal(stepLine(tool("read", "completed", "C:\\Users\\x\\repo\\crates\\orchestrator\\src\\main.rs") as never, cwd), "crates/orchestrator/src/main.rs");
  // A command's own backslashes are not path separators to rewrite.
  assert.equal(relativeTo("cd C:\\Users\\x\\repo && echo a\\b", cwd), "cd . && echo a\\b");
  assert.equal(relativeTo("echo C:\\Users\\x\\note.txt", cwd), "echo C:\\Users\\x\\note.txt");
  // Paths joined in one word are each shortened.
  assert.equal(relativeTo("C:\\Users\\x\\repo\\a\\b,C:\\Users\\x\\repo\\c;C:\\Users\\x\\repo", cwd), "a/b,c;.");
  // A trailing separator is dropped, so the file name reads as itself.
  assert.equal(relativeTo("Read C:\\Users\\x\\repo\\sub\\", cwd), "Read sub");
});

test("calls that change the team or the code are cards, not folded steps", () => {
  const spawn = tool("mcp", "completed", "crossagents.agent_spawn");
  const claudeSpawn = tool("mcp", "completed", "mcp__crossagents__agent_spawn");
  assert.equal(actionOf(spawn), "agent_spawn");
  assert.equal(actionOf(claudeSpawn), "agent_spawn");
  assert.equal(actionOf(tool("mcp", "completed", "crossagents.agent_list")), undefined);
  assert.equal(actionOf(tool("mcp", "completed", "agent_spawn")), undefined, "only the hub tool");
  const blocks = workGroups([tool("read"), spawn, tool("read")]);
  assert.deepEqual(
    blocks.map((b) => b.kind),
    ["steps", "item", "steps"],
  );
});

test("an extension tool is never mistaken for a built-in action", () => {
  // `standup__run_land` ends in an action name but belongs to the extension, not the hub.
  for (const title of [
    "mcp__crossagents__standup__run_land",
    "crossagents.standup__run_land",
    "mcp__crossagents__standup__agent_spawn",
    "crossagents.standup__task_accept",
  ]) {
    assert.equal(actionOf(tool("mcp", "completed", title)), undefined, title);
  }
  assert.equal(actionOf(tool("mcp", "completed", "mcp__crossagents__extension_call")), undefined);
});

test("a tool call an extension draws itself is a card, not a folded step", () => {
  const own = tool("mcp", "completed", "mcp__crossagents__standup__report");
  const blocks = workGroups([tool("read"), own, tool("read")], (item) => item === own);
  assert.deepEqual(
    blocks.map((b) => b.kind),
    ["steps", "item", "steps"],
  );
});

type Call = Extract<TranscriptItem, { type: "tool_call" }>;
const running = (title: string, kind: string, output?: string): Call => ({ type: "tool_call", tool_call_id: "run-1", title, kind, status: "in_progress", ...(output !== undefined ? { output } : {}) });

test("the live line shows the newest step: a stream's last line, or a running command's tail", () => {
  assert.equal(liveNow([]), undefined);
  // A stream is one line: the last non-empty one (the whole text is in the steps above).
  assert.deepEqual(liveNow([text("先看 README\n\n再改 cli.md")]), { title: "再改 cli.md" });
  assert.deepEqual(liveNow([thought("想一下")]), { title: "想一下" });
  assert.equal(liveNow([text("\n\n")]), undefined, "nothing to show yet");
  // A step is its line, with paths shortened.
  assert.deepEqual(liveNow([tool("read", "in_progress", "Read /w/a/README.md")], "/w/a"), { title: "Read README.md" });
  // A running command also shows its newest output lines, and how many it left out.
  const tail = liveNow([running("cargo test", "execute", "1\n2\n3\n4\n5\n")]);
  assert.equal(tail?.title, "$ cargo test");
  assert.deepEqual(tail?.output, { lines: ["3", "4", "5"], hidden: 2 });
  // Finished, its output belongs to the step, not to the live line.
  assert.deepEqual(liveNow([{ ...running("cargo test", "execute", "done"), status: "completed" }]), { title: "$ cargo test" });
  assert.deepEqual(liveNow([running("Edit /w/a/b.ts", "edit")], "/w/a"), { title: "✎ b.ts" });
});

test("time without new output counts up, and never goes back", () => {
  const at = 1_000_000;
  assert.equal(quietSeconds(at, at), 0);
  assert.equal(quietSeconds(at, at + 45_900), 45);
  assert.equal(quietSeconds(at, at - 5_000), 0);
});

test("a quiet turn says how long, in the unit that reads best", () => {
  assert.equal(quietLabel(0), "0 秒没动静");
  assert.equal(quietLabel(45), "45 秒没动静");
  assert.equal(quietLabel(59), "59 秒没动静");
  assert.equal(quietLabel(60), "1 分钟没动静");
  assert.equal(quietLabel(299), "4 分钟没动静");
  assert.equal(quietLabel(3_600), "1 小时没动静");
  assert.equal(quietLabel(3_900), "1 小时 5 分钟没动静");
  assert.equal(quietLabel(-3), "0 秒没动静");
});

test("a running turn reads as elapsed, activity, and how long since it moved", () => {
  const t0 = 1_000_000;
  // Both times known: "1m 05s · 运行命令", quiet only past the threshold.
  const moving = liveProgress([tool("execute", "in_progress")], { since: t0, at: t0 + 63_000, now: t0 + 65_000 });
  assert.deepEqual(moving, { elapsed: "1m 05s", activity: "运行命令", quiet: 2 });
  const quiet = liveProgress([tool("read", "completed")], { since: t0, at: t0, now: t0 + 90_000 });
  assert.equal(quiet.elapsed, "1m 30s");
  assert.equal(quiet.activity, "工作中");
  assert.equal(quiet.quiet, 90);
  assert.equal(quiet.quietText, "1 分钟没动静");
  // Under the threshold it is still moving: no quiet line at all.
  assert.equal(liveProgress([text("…")], { since: t0, at: t0, now: t0 + (QUIET_AFTER - 1) * 1000 }).quietText, undefined);
  // An unknown start is left out rather than guessed.
  const fresh = liveProgress([text("…")], { now: t0 });
  assert.equal(fresh.elapsed, undefined);
  assert.equal(fresh.activity, "回复中");
  assert.equal(fresh.quiet, 0);
  assert.equal(fresh.quietText, undefined);
});

test("the live key changes only when the turn produced something", () => {
  const call = running("cargo test", "execute", "a\n");
  assert.equal(liveKey([call]), liveKey([call]));
  assert.notEqual(liveKey([call]), liveKey([{ ...call, output: "a\nb\n" }]));
  assert.notEqual(liveKey([call]), liveKey([call, { ...call, tool_call_id: "run-2" }]));
  assert.notEqual(liveKey([text("ab")]), liveKey([text("abc")]));
  assert.notEqual(liveKey([text("ab")]), liveKey([thought("ab")]));
});
