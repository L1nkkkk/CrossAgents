import assert from "node:assert/strict";
import { test } from "node:test";
import { isTestCommand, outcomeOf } from "./stepOutcome.ts";
import type { Step } from "./workGroups.ts";

let n = 0;
const call = (
  kind: string,
  title: string,
  detail?: Record<string, unknown>,
  status: "completed" | "failed" = "completed",
): Step =>
  ({
    type: "tool_call",
    tool_call_id: `c${(n += 1)}`,
    title,
    kind,
    status,
    ...(detail ? { detail: { changes: [], ...detail } } : {}),
  }) as unknown as Step;

const thought = (text: string): Step => ({ type: "output", stream: "thought", text }) as Step;

test("an edit run is named by the files it changed", () => {
  const outcome = outcomeOf([call("edit", "Edit src/auth.rs"), call("edit", "Write web/login.tsx")]);
  assert.equal(outcome.title, "改了 2 个文件 · 2 步");
  assert.deepEqual(outcome.artifacts.files, ["src/auth.rs", "web/login.tsx"]);
});

test("one file touched twice is one file", () => {
  assert.equal(outcomeOf([call("edit", "Edit src/auth.rs"), call("edit", "Edit src/auth.rs")]).title, "改了 1 个文件 · 2 步");
});

test("a test command is named by how it went", () => {
  assert.equal(outcomeOf([call("execute", "cargo test -p auth", { command: "cargo test -p auth", exit_code: 0 })]).title, "跑了测试：通过 · 1 步");
  assert.equal(outcomeOf([call("execute", "npm test", { command: "npm test", exit_code: 1 })]).title, "跑了测试：✕ 失败 · 1 步");
  assert.equal(outcomeOf([call("execute", "cargo test -p auth", { command: "cargo test -p auth" }, "failed")]).title, "跑了测试：✕ 失败 · 1 步");
});

test("a plain command is a command, not a test", () => {
  assert.equal(isTestCommand("cargo build"), false);
  assert.equal(isTestCommand("pnpm run test --watch"), true);
  assert.equal(outcomeOf([call("execute", "cargo build", { command: "cargo build", exit_code: 0 })]).title, "跑了 1 个命令 · 1 步");
});

test("a failed edit claims no file, and the failure is on the line", () => {
  const outcome = outcomeOf([call("edit", "Edit src/auth.rs", undefined, "failed")]);
  assert.deepEqual(outcome.artifacts.files, []);
  assert.equal(outcome.title, "1 个失败 · 1 步");
});

test("an image a step produced comes with its data URL", () => {
  const result = JSON.stringify({ image: "data:image/png;base64,iVBORw0KGgo=" });
  const outcome = outcomeOf([call("other", "screenshot", { result })]);
  assert.equal(outcome.title, "出了 1 张图 · 1 步");
  assert.deepEqual(outcome.artifacts.images, ["data:image/png;base64,iVBORw0KGgo="]);
});

test("reasoning alone keeps the old wording", () => {
  assert.equal(outcomeOf([thought("先看看登录流程")]).title, "推理 1 段");
});
