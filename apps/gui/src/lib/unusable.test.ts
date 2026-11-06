import assert from "node:assert/strict";
import test from "node:test";
import { refusedTasks, unusableOf } from "./unusable.ts";

test("unusableOf reads the hub's optional reason and nothing else", () => {
  assert.equal(unusableOf({ unusable: "no model" }), "no model");
  assert.equal(unusableOf({ unusable: "  " }), undefined);
  assert.equal(unusableOf({}), undefined);
  assert.equal(unusableOf(null), undefined);
  assert.equal(unusableOf("agent"), undefined);
});

test("refusedTasks puts each task's reason on its own key", () => {
  const message =
    'the plan is invalid:\ntask "a": Cursor: Cursor cannot use this permission on Windows: set this agent\'s permission to bypass, or set the run\'s approvals to off (全托管).\ntask "c": Cursor: Cursor cannot use this permission on Windows.';
  const got = refusedTasks(message, ["a", "b", "c"]);
  assert.deepEqual(Object.keys(got), ["a", "c"]);
  assert.match(got.a, /Cursor cannot use this permission/);
  assert.match(got.c, /^Cursor: /);
});

test("refusedTasks ignores lines about tasks the plan does not have", () => {
  const got = refusedTasks('the plan is invalid:\ntask "z": no session named "x"', ["a"]);
  assert.deepEqual(got, {});
});

test("refusedTasks leaves an unattributed refusal empty", () => {
  assert.deepEqual(refusedTasks("run is Running, not awaiting approval", ["a"]), {});
});
