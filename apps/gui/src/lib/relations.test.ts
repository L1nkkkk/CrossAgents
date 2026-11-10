import assert from "node:assert/strict";
import test from "node:test";
import { isLeading, relationKind, relationText, sessionLabel, strengthsOf } from "./relations.ts";

const base = { name: "w-12ccab-misc" };

test("relationText says what the agent is doing now", () => {
  assert.equal(relationText({ ...base, now: { kind: "lead", run_id: "r", run_title: "登录页", task_key: null } }), "主导 登录页");
  assert.equal(relationText({ ...base, now: { kind: "working", run_id: "r", run_title: "登录页", task_key: "api" } }), "在做 #api");
  assert.equal(relationText({ ...base, now: { kind: "reviewing", run_id: "r", run_title: "登录页", task_key: "ui" } }), "在审 #ui");
  assert.equal(relationText({ ...base, now: null }), "空闲");
});

test("relationText degrades when the hub does not send now", () => {
  assert.equal(relationText(base), "");
  assert.equal(relationText({ ...base, now: { kind: "planner" } }), "");
  assert.equal(relationKind(base), undefined);
  assert.equal(relationKind({ ...base, now: null }), "idle");
  assert.equal(isLeading({ ...base, now: { kind: "lead", run_id: "r", run_title: "x", task_key: null } }), true);
  assert.equal(isLeading({ ...base, role: "planner" }), false);
});

test("sessionLabel names an execution session by its source agent and task", () => {
  assert.equal(sessionLabel({ ...base, session_of: { id: "a1", name: "Mira 2", task_key: "misc" } } as typeof base), "Mira 2 · misc");
  assert.equal(sessionLabel({ ...base, session_of: { id: "a1", name: "Mira 2", task_key: null } } as typeof base), "Mira 2");
  assert.equal(sessionLabel(base), "w-12ccab-misc");
  assert.equal(sessionLabel({ ...base, session_of: null } as typeof base), "w-12ccab-misc");
});

test("strengthsOf keeps non-empty strings only", () => {
  assert.deepEqual(strengthsOf({ strengths: ["Rust", " ", 3, " 前端 "] }), ["Rust", "前端"]);
  assert.deepEqual(strengthsOf(base), []);
});
