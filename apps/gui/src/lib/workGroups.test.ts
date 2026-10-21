import assert from "node:assert/strict";
import { test } from "node:test";
import type { TranscriptItem } from "@bindings/TranscriptItem";
import { duration, liveActivity, stepLine, summarize, workGroups, type Step } from "./workGroups.ts";

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
