import assert from "node:assert/strict";
import { test } from "node:test";
import type { Agent } from "@bindings/Agent";
import type { Task } from "@bindings/Task";
import type { RunDetail } from "@bindings/RunDetail";
import { executorOf, projectMembers, standaloneSessions } from "./projectView.ts";

const agent = (id: string, home = "/repo", created_at = id): Agent => ({ id, name: id, home, cwd: home, created_at, status: "idle", profile: { title: `Profile ${id}` } } as Agent);
const task = (fields: Partial<Task>): Task => ({ id: "task", key: "A", title: "Build", created_at: "1", assignee: null, reviewer: null, worker_template: null, reviewer_template: null, worker_fresh: false, reviewer_fresh: false, ...fields } as Task);
const detail = (tasks: Task[]): RunDetail => ({ run: { id: "run", repo: "/repo", planner: "lead", lead_note: null } as RunDetail["run"], tasks });

test("members deduplicate stable IDs and prefer assigned responsibility over review", () => {
  const leo = agent("leo");
  const rows = projectMembers([leo], [detail([task({ id: "review", key: "B", reviewer: "leo" }), task({ id: "work", key: "A", assignee: "leo" })])]).get("/repo")!;
  assert.equal(rows.length, 1);
  assert.equal(rows[0].summary, "A Build");
  assert.equal(rows[0].status, "idle");
  assert.equal(rows[0].project, "/repo");
  assert.equal(projectMembers([leo], []).get("/repo")![0].summary, "Profile leo");
});

test("actual execution and configuration templates remain distinct", () => {
  const leo = agent("leo"), fresh = agent("fresh");
  assert.deepEqual(executorOf(task({ assignee: "leo", worker_template: "leo" }), [leo, fresh]), { executor: leo, template: leo, fresh: false });
  assert.deepEqual(executorOf(task({ assignee: "fresh", worker_template: "leo", worker_fresh: true }), [leo, fresh]), { executor: fresh, template: leo, fresh: true });
  assert.deepEqual(executorOf(task({ reviewer: "fresh", reviewer_template: "leo", reviewer_fresh: true }), [leo, fresh], "reviewer"), { executor: fresh, template: leo, fresh: true });
  assert.deepEqual(executorOf({ ...detail([]).run, planner: "fresh", lead_note: "copied" }, [leo, fresh]), { executor: fresh, template: undefined, fresh: true });
});

test("standalone sessions use the established home and chat rules", () => {
  const alone = agent("alone", "/home/.crossagents/chats/alone");
  const projectChat = { ...agent("chat"), topic: "Hello" };
  assert.deepEqual(standaloneSessions([projectChat, alone]), [alone]);
  assert.equal(projectMembers([projectChat, alone], []).get("/repo")![0].agent.id, "chat");
});

test("member ordering uses pins and stable creation keys, not status or input order", () => {
  const a = agent("a", "/repo", "1"), b = agent("b", "/repo", "2");
  const ids = (agents: Agent[]) => projectMembers(agents, [], new Set(["b"])).get("/repo")!.map((row) => row.agent.id);
  assert.deepEqual(ids([a, b]), ["b", "a"]);
  assert.deepEqual(ids([{ ...b, status: "running" }, a]), ["b", "a"]);
});
