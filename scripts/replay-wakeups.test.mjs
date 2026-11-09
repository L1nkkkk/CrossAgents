import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import {
  classifyMessage,
  classifyNoticeLine,
  grow,
  parseDigest,
  simulate,
} from "./replay-wakeups.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const script = path.join(here, "replay-wakeups.mjs");

const LEAD = "01a10000-0000-7000-8000-000000000001";
const WORKER = "01a10000-0000-7000-8000-000000000002";
const T0 = Date.parse("2026-11-09T10:00:00.000Z");

let seq = 0;
function ev(type, at, data) {
  return {
    seq: ++seq,
    id: `01a1ffff-0000-7000-8000-${String(seq).padStart(12, "0")}`,
    ts: new Date(T0 + at * 1000).toISOString(),
    schema: 1,
    type,
    data,
  };
}

function writeLog(name, events) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "replay-wakeups-"));
  const file = path.join(dir, name);
  fs.writeFileSync(file, events.map((e) => JSON.stringify(e)).join("\n") + "\n");
  return file;
}

function run(args) {
  return execFileSync(process.execPath, [script, ...args], { encoding: "utf8" });
}

function runJSON(args) {
  return JSON.parse(run([...args, "--json"]));
}

// 手写的一份旧式日志：人的消息、silent 通知、两条 batch（合并）、队友交付、stalled、run 结束。
const DIGEST = "CrossAgents notices (things you asked to hear about):\n";
const REPORT =
  "改完了。\n\n---\ntask #a (01a10000-0000-7000-8000-0000000000aa) · branch crossagents/x-a\n" +
  "worktree: /tmp/a\nchanges:\n docs/a.md | 2 +-\nchecks: 1/1 passed";

function legacyLog() {
  const start = 0;
  return [
    ev("agent.created", start, {
      agent: { id: LEAD, name: "Lead One", role: "planner", model: "test" },
    }),
    // 人的消息；同一个回合作者写了笔记（算做了决定）。
    ev("agent.input", 1, {
      id: LEAD,
      message_id: "m1",
      text: "# 目标\n做一件事",
      queued: false,
      from: { kind: "human" },
    }),
    ev("agent.turn_started", 1.2, { id: LEAD, message_id: "m1" }),
    ev("note.written", 2, { scope: "run", key: "plan/x", author: LEAD, text: "记一笔" }),
    ev("agent.usage", 2.5, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 1000, cached_input: 900, output: 10, reasoning_output: 0, total: 1010 },
    }),
    ev("agent.turn_completed", 3, { id: LEAD, message_id: "m1", stop_reason: "end_turn" }),
    // 只有 silent 的通知：实际叫醒了一次，新规则下不该叫醒。
    ev("agent.input", 10, {
      id: LEAD,
      message_id: "m2",
      text: DIGEST + "- #a 已派给 w-a（来自模板 X），给它发消息用这个名字\n- Progress from w-a on #a (t): working\n",
      queued: false,
      from: { kind: "notice" },
    }),
    ev("agent.turn_started", 10.1, { id: LEAD, message_id: "m2" }),
    ev("agent.usage", 11, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 2000, cached_input: 1800, output: 20, reasoning_output: 0, total: 2020 },
    }),
    ev("agent.turn_completed", 12, { id: LEAD, message_id: "m2", stop_reason: "end_turn" }),
    // 两条「等接受」在排队时先后到达：input 一条，input_updated 再加一条。
    ev("agent.input", 20, {
      id: LEAD,
      message_id: "m3",
      text: DIGEST + "- Task #a is now passed.",
      queued: false,
      from: { kind: "notice" },
    }),
    ev("agent.input_updated", 25, {
      id: LEAD,
      message_id: "m3",
      text: DIGEST + "- Task #a is now passed.\n- Task #b is now passed.",
    }),
    ev("agent.turn_started", 25.1, { id: LEAD, message_id: "m3" }),
    ev("agent.usage", 26, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 3000, cached_input: 2700, output: 30, reasoning_output: 0, total: 3030 },
    }),
    ev("agent.turn_completed", 30, { id: LEAD, message_id: "m3", stop_reason: "end_turn" }),
    // 队友的交付报告（旧式消息）。
    ev("agent.input", 40, {
      id: LEAD,
      message_id: "m4",
      text: REPORT,
      queued: false,
      from: { kind: "agent", agent_id: WORKER },
    }),
    ev("agent.turn_started", 40.1, { id: LEAD, message_id: "m4" }),
    ev("agent.usage", 41, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 4000, cached_input: 3600, output: 40, reasoning_output: 0, total: 4040 },
    }),
    ev("agent.turn_completed", 45, { id: LEAD, message_id: "m4", stop_reason: "end_turn" }),
    // 要决定的通知：立刻叫醒，并带上所有待带的通知。
    ev("agent.input", 50, {
      id: LEAD,
      message_id: "m5",
      text: DIGEST + "- Task #b (01a10000-0000-7000-8000-0000000000bb) is stalled: w-b ended a turn without reporting.",
      queued: false,
      from: { kind: "notice" },
    }),
    ev("agent.turn_started", 50.1, { id: LEAD, message_id: "m5" }),
    ev("agent.usage", 51, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 5000, cached_input: 4500, output: 50, reasoning_output: 0, total: 5050 },
    }),
    ev("agent.turn_completed", 52, { id: LEAD, message_id: "m5", stop_reason: "end_turn" }),
    // 全部结束。
    ev("agent.input", 60, {
      id: LEAD,
      message_id: "m6",
      text: DIGEST + "- All 2 task(s) of run 01a10000-0000-7000-8000-0000000000ff have ended: 2 merged into main.",
      queued: false,
      from: { kind: "notice" },
    }),
    ev("agent.turn_started", 60.1, { id: LEAD, message_id: "m6" }),
    ev("agent.usage", 61, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 6000, cached_input: 5400, output: 60, reasoning_output: 0, total: 6060 },
    }),
    ev("agent.turn_completed", 62, { id: LEAD, message_id: "m6", stop_reason: "end_turn" }),
  ];
}

// ---------------------------------------------------------------------------
// 归类

test("通知行按新分类归类", () => {
  const cases = [
    ["#ext-api2 已派给 w-x（来自模板 Suki 5），给它发消息用这个名字", "silent"],
    ["#rounds (01a10000-0000-7000-8000-000000000002): given to Cleo 2 instead: account cursor", "silent"],
    ["Progress from w-x on #a (t): 正在读文件", "silent"],
    ["Task #a is now passed.", "batch"],
    ["Task #a is now merged.", "silent"],
    ["Review of #a (01a10000-0000-7000-8000-0000000000aa): approved\n可以合并。", "batch"],
    ["Review of #a (01a10000-0000-7000-8000-0000000000aa): changes needed", "silent"],
    ["Task #a (01a10000-0000-7000-8000-0000000000aa) is stalled: w ended a turn without reporting.", "decide"],
    ["Task #a (01a10000-0000-7000-8000-0000000000aa) failed: tests failed", "decide"],
    ["r-x ended its turn without a verdict on #a (01a10000-0000-7000-8000-0000000000aa) (still reviewing).", "decide"],
    ["Reminder (r-1): 提交计划", "decide"],
    ["All 4 task(s) of run 01a10000-0000-7000-8000-0000000000ff have ended: 4 merged into main.", "run_done"],
    ["The plan of run 01a10000-0000-7000-8000-0000000000ff was approved: 4 task(s) start.", "decide"],
    ["codex account codex has 50% of its primary window left and it resets in 0h38m: dispatch more work there while it lasts.", "batch"],
    ["The hub restarted; task #a is stalled until you decide.", "decide"],
    ["完全认不出来的一行", "decide"],
  ];
  for (const [line, want] of cases) {
    assert.equal(classifyNoticeLine(line).class, want, line);
  }
});

test("watch 行只在内容认不出来时兜底 decide", () => {
  const lines = new Set(["Task #a is now passed.", "Some odd line about #a"]);
  assert.equal(classifyNoticeLine("Task #a is now passed.", { watchLines: lines }).class, "batch");
  assert.deepEqual(classifyNoticeLine("Some odd line about #a", { watchLines: lines }), {
    class: "decide",
    why: "watch",
  });
});

test("消息来源和正文决定消息本身怎么记", () => {
  assert.equal(classifyMessage("hi", { kind: "human" }).class, "human");
  assert.equal(classifyMessage("hi", { kind: "orchestrator" }).class, "orchestrator");
  assert.equal(classifyMessage("普通回复", { kind: "agent", agent_id: WORKER }).class, "teammate");
  assert.equal(classifyMessage(REPORT, { kind: "agent", agent_id: WORKER }).class, "batch");
  assert.equal(
    classifyMessage("Review of #a (x): approved\n可以合并。", { kind: "agent", agent_id: WORKER }).class,
    "batch",
  );
  assert.equal(
    classifyMessage("Review of #a (x): changes needed\n补测试。", { kind: "agent", agent_id: WORKER }).class,
    "silent",
  );
});

test("摘要按条目切开，续行归上一条", () => {
  const { isDigest, items } = parseDigest(
    DIGEST + "- The plan of run r was approved: 2 task(s)\nWho does each task: a, b\n- Task #a is now passed.",
  );
  assert.equal(isDigest, true);
  assert.equal(items.length, 2);
  assert.match(items[0].line, /Who does each task/);
});

// ---------------------------------------------------------------------------
// 合并

const at = (sec) => T0 + sec * 1000;

test("batch 静默 60 秒合并，最多等 180 秒", () => {
  // 30 秒来一条：从最后一条起 60 秒。
  let sim = simulate([
    { ms: at(0), class: "batch" },
    { ms: at(30), class: "batch" },
    { ms: at(50), class: "batch" },
  ], []);
  assert.equal(sim.wakes.length, 1);
  assert.equal(sim.wakes[0].at, at(110));
  assert.equal(sim.wakes[0].notices, 3);

  // 隔得够久就是两次。
  sim = simulate([
    { ms: at(0), class: "batch" },
    { ms: at(70), class: "batch" },
  ], []);
  assert.deepEqual(sim.wakes.map((w) => w.at), [at(60), at(130)]);

  // 第一条起 180 秒封顶：每 55 秒一条、间隔都不到 60，0/55/110/165 一起走到 180；
  // 185 是下一批。
  sim = simulate([
    { ms: at(0), class: "batch" },
    { ms: at(55), class: "batch" },
    { ms: at(110), class: "batch" },
    { ms: at(165), class: "batch" },
    { ms: at(185), class: "batch" },
  ], []);
  assert.deepEqual(sim.wakes.map((w) => w.notices), [4, 1]);
  assert.equal(sim.wakes[0].at, at(180));
  assert.equal(sim.wakes[1].at, at(245));
});

test("silent 不叫醒，跟着下一次唤醒走", () => {
  let sim = simulate([
    { ms: at(0), class: "silent" },
    { ms: at(10), class: "silent" },
  ], []);
  assert.equal(sim.wakes.length, 0);
  assert.equal(sim.silentLeft, 2);

  sim = simulate([
    { ms: at(0), class: "silent" },
    { ms: at(10), class: "decide" },
  ], []);
  assert.equal(sim.wakes.length, 1);
  assert.equal(sim.wakes[0].class, "decide");
  assert.equal(sim.wakes[0].silent, 1);
  assert.equal(sim.silentRode, 1);
});

test("消息立刻叫醒，并带上待带的通知；正在回合里就等回合结束", () => {
  let sim = simulate([
    { ms: at(0), class: "silent" },
    { ms: at(5), class: "batch" },
    { ms: at(10), class: "human" },
  ], []);
  assert.equal(sim.wakes.length, 1);
  assert.equal(sim.wakes[0].class, "human");
  assert.equal(sim.wakes[0].at, at(10));

  // 回合 0–200 秒：10 秒到的决定和 100 秒到的通知都并进回合结束后的那一次唤醒。
  sim = simulate(
    [
      { ms: at(10), class: "decide" },
      { ms: at(100), class: "batch" },
    ],
    [{ start: at(0), end: at(200) }],
  );
  assert.equal(sim.wakes.length, 1);
  assert.equal(sim.wakes[0].at, at(200));
  assert.equal(sim.wakes[0].notices, 2);
});

// ---------------------------------------------------------------------------
// 用量

test("用量按会话累计值做差，数字变小算新会话", () => {
  const first = grow(null, { input: 100, cached: 80, cacheWrite: 0, output: 10, reasoning: 0, requests: 1 });
  assert.equal(first.fresh, true);
  assert.deepEqual(first.delta.input, 100);

  const second = grow({ input: 100, cached: 80, cacheWrite: 0, output: 10, reasoning: 0, requests: 1 }, {
    input: 250,
    cached: 200,
    cacheWrite: 0,
    output: 40,
    reasoning: 0,
    requests: 3,
  });
  assert.equal(second.fresh, false);
  assert.equal(second.delta.input, 150);
  assert.equal(second.delta.cached, 120);
  assert.equal(second.delta.output, 30);
  assert.equal(second.delta.requests, 2);

  const reset = grow({ input: 250, cached: 200, cacheWrite: 0, output: 40, reasoning: 0, requests: 3 }, {
    input: 20,
    cached: 0,
    cacheWrite: 0,
    output: 5,
    reasoning: 0,
    requests: 1,
  });
  assert.equal(reset.fresh, true);
  assert.equal(reset.delta.input, 20);
});

// ---------------------------------------------------------------------------
// 整份样例日志

test("样例日志：重建回合、归类、合并和输出", () => {
  const file = writeLog("events-2026-11-09.jsonl", legacyLog());
  const out = run(["--agent", "Lead One", file]);

  // 抬头和标注。
  assert.match(out, /旧日志回放/);
  assert.match(out, /归类: 推断/);
  assert.match(out, /估算/);

  // 实际 6 次唤醒；新规则 3 次（人、decide、run_done），两条 batch 合并进 decide。
  assert.match(out, /回合: 6/);
  assert.match(out, /唤醒: 3 次（实际 6 次，少 3 次/);
  assert.match(out, /人 1/);
  assert.match(out, /decide 1/);
  assert.match(out, /run_done 1/);
  assert.match(out, /silent: 2 条没有单独叫醒/);

  // 输入 token 按分类：增量 1000 / 1000 / 1000 / 1000 / 1000 / 1000。
  assert.match(out, /合计\s+6\s+100%\s+6K/);

  // 没做决定的回合：m2、m3、m4、m5、m6 五个（m1 写了笔记）。
  assert.match(out, /合计: 5 \/ 6 回合/);

  // 时间戳来自 input_updated：第二条 passed 在 25 秒，batch 的静默期从它算起。
  assert.match(out, /Task #a is now passed/);
});

test("样例日志：--json 给出同样的数", () => {
  const file = writeLog("events-2026-11-09.jsonl", legacyLog());
  const data = runJSON(["--agent", LEAD, file]);
  assert.equal(data.agent.name, "Lead One");
  assert.equal(data.rounds, 6);
  assert.equal(data.actual.total.input, 6000);
  assert.equal(data.actual.byClass.silent.wakes, 1);
  assert.equal(data.actual.byClass.batch.wakes, 2);
  assert.equal(data.estimated.wakes, 3);
  assert.equal(data.estimated.byTrigger.human, 1);
  assert.equal(data.estimated.byTrigger.decide, 1);
  assert.equal(data.undecided.count, 5);
  assert.equal(data.inferred, true);
});

test("时间段过滤，以及找不到 agent 的报错", () => {
  const file = writeLog("events-2026-11-09.jsonl", legacyLog());
  const data = runJSON([
    "--agent",
    "Lead One",
    "--from",
    "2026-11-09T10:00:45.000Z",
    file,
  ]);
  assert.equal(data.rounds, 2); // m5、m6
  assert.equal(data.undecided.count, 2);

  assert.throws(() => run(["--agent", "Nobody", file]), /找不到 agent/);
});

// ---------------------------------------------------------------------------
// 新格式：agent.round 和 notice.recorded 直接用，不推断

test("有新事件时直接用分类和 decided", () => {
  const events = [
    ev("agent.created", 0, { agent: { id: LEAD, name: "Lead One", role: "planner", model: "test" } }),
    ev("notice.recorded", 1, { id: LEAD, topic: "task:a", line: "Task #a is now passed.", class: "batch" }),
    ev("notice.recorded", 2, { id: LEAD, topic: "progress", line: "Progress from w-a on #a (t): x", class: "silent" }),
    ev("agent.input", 3, {
      id: LEAD,
      message_id: "m1",
      text: DIGEST + "- [batch] Task #a is now passed.",
      queued: false,
      from: { kind: "notice" },
    }),
    ev("agent.turn_started", 3.1, { id: LEAD, message_id: "m1" }),
    ev("agent.usage", 4, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 500, cached_input: 400, output: 5, reasoning_output: 0, total: 505 },
    }),
    ev("agent.turn_completed", 5, { id: LEAD, message_id: "m1", stop_reason: "end_turn" }),
    ev("agent.round", 5.1, {
      id: LEAD,
      message_ids: ["m1"],
      sources: [{ kind: "notice" }],
      notices: [
        { line: "Task #a is now passed.", class: "batch" },
        { line: "Progress from w-a on #a (t): x", class: "silent" },
      ],
      tokens: { input: 500, cached_input: 400, cache_write_input: 0, output: 5, reasoning_output: 0, total: 505, requests: 1 },
      tools: ["task_accept"],
      decided: true,
      prompt_via_file: false,
    }),
    ev("notice.recorded", 60, { id: LEAD, topic: "run", line: "All 2 task(s) of run r have ended: 2 merged into main.", class: "run_done" }),
    ev("agent.input", 61, {
      id: LEAD,
      message_id: "m2",
      text: DIGEST + "- [run_done] All 2 task(s) of run r have ended: 2 merged into main.",
      queued: false,
      from: { kind: "notice" },
    }),
    ev("agent.turn_started", 61.1, { id: LEAD, message_id: "m2" }),
    ev("agent.usage", 62, {
      id: LEAD,
      used: 0,
      size: 0,
      cost_usd: null,
      tokens: { input: 900, cached_input: 800, output: 9, reasoning_output: 0, total: 909 },
    }),
    ev("agent.turn_completed", 63, { id: LEAD, message_id: "m2", stop_reason: "end_turn" }),
    ev("agent.round", 63.1, {
      id: LEAD,
      message_ids: ["m2"],
      sources: [{ kind: "notice" }],
      notices: [{ line: "All 2 task(s) of run r have ended: 2 merged into main.", class: "run_done" }],
      tokens: { input: 400, cached_input: 300, cache_write_input: 0, output: 4, reasoning_output: 0, total: 404, requests: 1 },
      tools: [],
      decided: false,
      prompt_via_file: false,
    }),
  ];
  const file = writeLog("events-2026-11-09.jsonl", events);
  const out = run(["--agent", LEAD, file]);
  assert.match(out, /归类: 直接/);
  assert.match(out, /通知时间: notice\.recorded/);
  // batch 在 1 秒、silent 在 2 秒、run_done 在 60 秒：run_done 的 60 秒早于 batch 的 61 秒，
  // 一次唤醒把它们都带上。
  assert.match(out, /唤醒: 1 次（实际 2 次，少 1 次/);
  assert.match(out, /合计: 1 \/ 2 回合/); // 第二个回合 agent.round 说 decided=false

  const data = runJSON(["--agent", LEAD, file]);
  assert.equal(data.inferred, false);
  assert.equal(data.undecided.count, 1);
  assert.equal(data.estimated.wakes, 1);
});
