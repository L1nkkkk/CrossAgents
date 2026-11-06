import assert from "node:assert/strict";
import { test } from "node:test";
import type { Agent } from "@bindings/Agent";
import type { HubEvent } from "../hub.ts";
import { buildFeed, descendantsOf, feedSummary, filterFeed, scopeAgentIds, senderName, type FeedEntry } from "./activityFeed.ts";

const at = (n: number) => new Date(Date.UTC(2026, 10, 6, 12, 0, n)).toISOString();
const event = (seq: number, type: string, data: unknown): HubEvent => ({ seq, id: `e${seq}`, ts: at(seq), schema: 1, type, data }) as unknown as HubEvent;

const agent = (id: string, spawnedBy?: string): Agent =>
  ({ id, name: id, home: "/repo", cwd: "/repo", created_at: at(0), status: "idle", config: spawnedBy ? { spawned_by_agent: spawnedBy } : null, profile: { title: null } }) as Agent;

const names = new Map([
  ["lead", "Ada"],
  ["worker", "Rex"],
  ["grand", "Kai"],
  ["mia", "Mia"],
]);
const nameOf = (id: string) => names.get(id);

const entries = (events: HubEvent[]) => buildFeed(events);
const find = (list: FeedEntry[], type: FeedEntry["type"], text?: string) => list.find((e) => e.type === type && (text === undefined || e.text.includes(text)));

test("replies, tool calls, errors and messages become entries; reasoning does not", () => {
  const feed = entries([
    event(1, "agent.input", { id: "lead", message_id: "m1", text: "看一下登录", queued: false, from: { kind: "human" } }),
    event(2, "agent.turn_started", { id: "lead", message_id: "m1" }),
    event(3, "agent.output", { id: "lead", stream: "thought", text: "先看路由" }),
    event(4, "agent.output", { id: "lead", stream: "text", text: "我先看登录路由。" }),
    event(5, "agent.tool_call", { id: "lead", tool_call_id: "t1", title: "Read src/auth.rs", kind: "read", status: "in_progress" }),
    event(6, "agent.error", { id: "lead", message: "读取失败" }),
  ]);
  assert.equal(feed.length, 4);
  assert.equal(find(feed, "message")?.text, "看一下登录");
  assert.equal(find(feed, "reply")?.text, "我先看登录路由。");
  assert.equal(find(feed, "tool")?.text, "Read src/auth.rs");
  assert.equal(find(feed, "error")?.text, "读取失败");
});

test("consecutive text chunks are one reply; another agent's event keeps it open", () => {
  const feed = entries([
    event(1, "agent.turn_started", { id: "lead", message_id: "m1" }),
    event(2, "agent.output", { id: "lead", stream: "text", text: "第一段，" }),
    event(3, "agent.output", { id: "lead", stream: "text", text: "第二段。" }),
    event(4, "agent.output", { id: "worker", stream: "text", text: "另一个人。" }),
    event(5, "agent.output", { id: "lead", stream: "text", text: "接着写。" }),
  ]);
  const replies = feed.filter((e) => e.type === "reply");
  assert.deepEqual(
    replies.map((e) => e.text),
    ["第一段，第二段。接着写。", "另一个人。"],
  );
  assert.equal(replies[0].seq, 5);
  assert.equal(replies[0].turn, "m1");
  assert.equal(replies[1].turn, undefined);
});

test("a failed tool update marks the same entry and keeps its turn", () => {
  const feed = entries([
    event(1, "agent.turn_started", { id: "worker", message_id: "m2" }),
    event(2, "agent.tool_call", { id: "worker", tool_call_id: "t9", title: "Run npm test", kind: "execute", status: "in_progress" }),
    event(3, "agent.tool_call_updated", { id: "worker", tool_call_id: "t9", title: null, status: "failed" }),
  ]);
  const tool = find(feed, "tool");
  assert.ok(tool);
  assert.equal(tool.failed, true);
  assert.equal(tool.turn, "m2");
  assert.equal(feed.filter((e) => e.type === "tool").length, 1);
});

test("a turn is attached to what happens inside it and cleared when it ends", () => {
  const feed = entries([
    event(1, "agent.turn_started", { id: "lead", message_id: "m1" }),
    event(2, "agent.output", { id: "lead", stream: "text", text: "做到了。" }),
    event(3, "agent.turn_completed", { id: "lead", message_id: "m1", stop_reason: "end_turn" }),
    event(4, "agent.error", { id: "lead", message: "之后才出的错" }),
  ]);
  assert.equal(find(feed, "reply")?.turn, "m1");
  assert.equal(find(feed, "error")?.turn, undefined);
});

test("agent-to-agent sends and questions read as the person would say them", () => {
  const feed = entries([
    event(1, "agent.input", { id: "worker", message_id: "m2", text: "先把筛选补上", queued: false, from: { kind: "agent", agent_id: "lead" } }),
    event(2, "agent.steered", { id: "worker", message_id: "m3", text: "顺便加个测试" }),
    event(4, "agent.input", { id: "worker", message_id: "m4", text: "旧事件里没有说话人", queued: true }),
    event(3, "human.asked", {
      item: {
        id: "q1", kind: "question", from: "lead", run_id: null, task_id: null, text: "要不要默认只看错误？", detail: null, options: [],
        default: null, timeout_at: null, status: "open", answer: null, answered_at: null, action: null, created_at: at(1),
      },
    }),
  ]);
  const showed = feed.map((e) => feedSummary(e, nameOf));
  assert.ok(showed.includes("Ada → Rex：先把筛选补上"));
  assert.ok(showed.includes("你 → Rex：顺便加个测试"));
  assert.ok(showed.includes("你 → Rex：旧事件里没有说话人"));
  assert.ok(showed.includes("Ada → 你：要不要默认只看错误？"));
});

test("newest entries come first, and a streamed reply moves up as it grows", () => {
  const feed = entries([
    event(1, "agent.output", { id: "lead", stream: "text", text: "a" }),
    event(2, "agent.tool_call", { id: "worker", tool_call_id: "t1", title: "Bash", kind: "execute", status: "completed" }),
    event(3, "agent.output", { id: "lead", stream: "text", text: "b" }),
  ]);
  assert.deepEqual(
    feed.map((e) => e.type),
    ["reply", "tool"],
  );
  assert.equal(feed[0].seq, 3);
});

test("subordinates are the spawned agents and theirs, even through bad config", () => {
  const agents = [agent("lead"), agent("worker", "lead"), agent("grand", "worker"), agent("mia"), { ...agent("odd"), config: { spawned_by_agent: 7 } } as Agent];
  assert.deepEqual([...descendantsOf(agents, "lead")].sort(), ["grand", "lead", "worker"]);
  assert.deepEqual([...descendantsOf(agents, "mia")], ["mia"]);
  assert.equal(descendantsOf(agents, "missing").size, 1);
});

test("scope keeps all, one agent, or an agent and its subordinates", () => {
  const agents = [agent("lead"), agent("worker", "lead"), agent("grand", "worker"), agent("mia")];
  const feed = entries([
    event(1, "agent.output", { id: "lead", stream: "text", text: "a" }),
    event(2, "agent.output", { id: "worker", stream: "text", text: "b" }),
    event(3, "agent.output", { id: "grand", stream: "text", text: "c" }),
    event(4, "agent.output", { id: "mia", stream: "text", text: "d" }),
  ]);
  const ids = (scope: Parameters<typeof filterFeed>[1]["scope"]) =>
    filterFeed(feed, { types: new Set(["reply"]), scope, agents }).map((e) => e.agentId).sort();
  assert.deepEqual(ids({ kind: "all" }), ["grand", "lead", "mia", "worker"]);
  assert.deepEqual(ids({ kind: "agent", agent: "lead" }), ["lead"]);
  assert.deepEqual(ids({ kind: "team", agent: "lead" }), ["grand", "lead", "worker"]);
  assert.equal(scopeAgentIds(agents, { kind: "all" }), null);
});

test("type filters combine, and asking for none leaves nothing", () => {
  const agents = [agent("lead")];
  const feed = entries([
    event(1, "agent.output", { id: "lead", stream: "text", text: "回复" }),
    event(2, "agent.tool_call", { id: "lead", tool_call_id: "t1", title: "Bash", kind: "execute", status: "completed" }),
    event(3, "agent.error", { id: "lead", message: "出错" }),
  ]);
  const types = new Set(["reply", "error"] as const);
  assert.deepEqual(
    filterFeed(feed, { types, scope: { kind: "all" }, agents }).map((e) => e.type).sort(),
    ["error", "reply"],
  );
  assert.equal(filterFeed(feed, { types: new Set(), scope: { kind: "all" }, agents }).length, 0);
  assert.equal(senderName({ kind: "orchestrator", run_id: "r1" }, nameOf), "调度");
});
