import assert from "node:assert/strict";
import { test } from "node:test";
import type { Agent } from "@bindings/Agent";
import type { RunDetail } from "@bindings/RunDetail";
import type { Task } from "@bindings/Task";
import { capped, sidebarGroups, searchGroups, groupClosed, summaryText } from "./sidebarTree.ts";
import { todoItems, visibleTodos } from "./todos.ts";
import type { HumanItem } from "@bindings/HumanItem";

const agent = (id: string, fields: Partial<Agent> = {}): Agent =>
  ({
    id,
    name: id,
    home: "/repo",
    cwd: "/repo",
    created_at: id,
    status: "idle",
    profile: { title: "Profile" },
    ...fields,
  }) as Agent;
const task = (id: string, fields: Partial<Task>): Task =>
  ({
    id,
    key: "G1-3",
    title: "实现长中文任务标题",
    created_at: id,
    assignee: null,
    reviewer: null,
    ...fields,
  }) as Task;
const run = (tasks: Task[], fields = {}): RunDetail =>
  ({
    run: { id: "run", repo: "/repo", planner: "a", ...fields },
    tasks,
  }) as RunDetail;
const projects = [
  { key: "/repo", name: "Project" },
  { key: "/other", name: "Other" },
];
const groups = (agents: Agent[], runs: RunDetail[] = [], pins = new Set<string>(), order: string[] = []) =>
  sidebarGroups(agents, runs, projects, pins, order);

test("one canonical member per project despite multiple responsibilities, no origin tree", () => {
  const a = agent("a"),
    b = agent("b");
  const view = groups(
    [a, b],
    [
      run([task("one", { assignee: "b" }), task("two", { reviewer: "b" })], {
        origin_chat_id: "a",
      }),
    ],
  );
  assert.deepEqual(
    view.find((g) => g.key === "/repo")!.members.map((m) => m.agent.id),
    ["a", "b"],
  );
  assert.equal(view.find((g) => g.key === "/repo")!.members[1].summary, "实现长中文任务标题");
});

test("project-linked chats are not duplicated as standalone; archived members leave empty projects reachable", () => {
  const chat = agent("chat", { home: "/home/.crossagents/chats/chat" });
  const alone = agent("alone", { home: "/home/.crossagents/chats/alone" });
  const archived = agent("archived", { archived_at: "today" });
  const view = groups([chat, alone, archived], [run([], { planner: "chat" })]);
  assert.deepEqual(
    view.find((g) => g.key === "__chats")!.members.map((m) => m.agent.id),
    ["alone"],
  );
  assert.deepEqual(
    view.find((g) => g.key === "/repo")!.members.map((m) => m.agent.id),
    ["chat"],
  );
  assert.ok(groups([archived]).some((g) => g.key === "/repo" && !g.members.length));
});

test("pins move members within their group without copying or archiving; status does not reorder", () => {
  const a = agent("a"),
    b = agent("b");
  const ids = (rows: Agent[], pins = new Set<string>()) =>
    groups(rows, [], pins)
      .find((g) => g.key === "/repo")!
      .members.map((m) => m.agent.id);
  assert.deepEqual(ids([b, a]), ["a", "b"]);
  assert.deepEqual(ids([{ ...b, status: "running" }, a]), ["a", "b"]);
  assert.deepEqual(ids([a, b], new Set(["s:b"])), ["b", "a"]);
  assert.deepEqual(ids([a, b]), ["a", "b"]);
  assert.deepEqual(
    groups([a], [], new Set(), ["/repo", "/other"]).map((g) => g.key),
    ["/repo", "/other"],
  );
});

test("search matches names, topics, current tasks and preserves project titles and fold preferences", () => {
  const view = groups([agent("Leo", { topic: "断线重连" }), agent("Iris")], [run([task("one", { assignee: "Iris" })])]);
  const folded = new Set(["/repo"]);
  for (const query of ["leo", "断线", "长中文"]) {
    const result = searchGroups(view, query);
    assert.equal(result.length, 1);
    assert.equal(result[0].name, "Project");
    assert.equal(result[0].members.length, 1);
    assert.equal(groupClosed(folded, result[0].key, true), false);
  }
  assert.deepEqual(searchGroups(view, " "), view);
  assert.equal(groupClosed(folded, "/repo", false), true);
  assert.deepEqual([...folded], ["/repo"]);
});

test("cap keeps selected member in place and search expansion is reversible", () => {
  const rows = ["a", "b", "c", "d", "e"];
  const selected = (id: string) => id === "e";
  assert.deepEqual(capped(rows, selected, 2, false), ["a", "b", "e"]);
  assert.deepEqual(capped(rows, selected, 2, true), rows);
  assert.deepEqual(capped(rows, selected, 2, false), ["a", "b", "e"]);
});

test("paths and identity labels are not shown as task summaries", () => {
  for (const value of ["C:\\repo\\work", "/repo/work", "\\\\host\\share", "worker", "主导"]) assert.equal(summaryText(value), "");
  assert.equal(summaryText("修复断线重连"), "修复断线重连");
});

test("todos survive hidden/archived/search-filtered members, and only server answers remove them", () => {
  const items = ["1", "2", "3", "4"].map(
    (id) =>
      ({
        id,
        from: "a",
        status: "open",
        kind: "question",
        text: "回答问题",
        created_at: "2026-01-0" + id,
      }) as HumanItem,
  );
  const todos = todoItems({ items, runs: [], now: 0 });
  assert.equal(todos.length, 4);
  assert.equal(visibleTodos(todos).remaining, 1);
  assert.equal(visibleTodos(todos, { expanded: true }).items.length, 4);
  assert.deepEqual(searchGroups(groups([agent("a", { archived_at: "today" })]), "missing"), []);
  assert.deepEqual(todoItems({ items, runs: [], now: 10 }), todos);
  assert.equal(
    todoItems({
      items: items.map((i) => (i.id === "1" ? { ...i, status: "answered" } : i)),
      runs: [],
      now: 10,
    }).length,
    3,
  );
});
