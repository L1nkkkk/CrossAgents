import assert from "node:assert/strict";
import { test } from "node:test";
import type { GraphCommit } from "@bindings/GraphCommit";
import type { GraphRef } from "@bindings/GraphRef";
import { layoutGraph, owners, refColors } from "./graphlayout.ts";

const c = (sha: string, parents: string[], refs: string[] = []): GraphCommit => ({ sha, parents, subject: sha, author: "a", time: "2026-10-27T00:00:00Z", refs });

test("a linear history stays in one lane", () => {
  const refs: GraphRef[] = [{ name: "main", kind: "base", sha: "c3" }];
  const layout = layoutGraph({ refs, commits: [c("c3", ["c2"], ["main"]), c("c2", ["c1"]), c("c1", [])] });
  assert.equal(layout.lanes, 1);
  assert.deepEqual(layout.rows.map((r) => r.lane), [0, 0, 0]);
  assert.deepEqual(layout.rows.map((r) => r.color), [0, 0, 0]);
  assert.deepEqual(layout.rows[0].top, []);
  assert.deepEqual(layout.rows[1].top, [{ from: 0, to: 0, color: 0 }]);
  assert.deepEqual(layout.rows[2].bottom, []);
  assert.equal(layout.rows[0].refs[0].name, "main");
  assert.ok(layout.rows.every((r) => !r.merge));
});

test("a fork puts the branch in its own lane and joins at the common parent", () => {
  // run: r2 - r1 \
  //                b0
  // task:   t1   /
  const refs: GraphRef[] = [
    { name: "run", kind: "run", sha: "r2" },
    { name: "task/a", kind: "task", sha: "t1", task_key: "a" },
  ];
  const layout = layoutGraph({ refs, commits: [c("t1", ["b0"], ["task/a"]), c("r2", ["r1"], ["run"]), c("r1", ["b0"]), c("b0", [])] });
  assert.equal(layout.lanes, 2);
  assert.deepEqual(layout.rows.map((r) => r.lane), [0, 1, 1, 0]);
  const colors = refColors(refs);
  assert.equal(colors.get("run"), 1);
  assert.equal(colors.get("task/a"), 2);
  assert.equal(layout.rows[0].color, 2);
  assert.equal(layout.rows[1].color, 1);
  // b0 belongs to the run (first-parent of run reaches it first).
  assert.equal(owners({ refs, commits: layout.rows.map((r) => r.commit) }).get("b0"), "run");
  // Both lanes end in b0.
  assert.deepEqual(layout.rows[3].top, [
    { from: 0, to: 0, color: 2 },
    { from: 1, to: 0, color: 1 },
  ]);
});

test("a merge commit opens a lane for its second parent", () => {
  // m (run) merges t1 (task) into r1; both come from b0.
  const refs: GraphRef[] = [
    { name: "main", kind: "base", sha: "b0" },
    { name: "run", kind: "run", sha: "m" },
    { name: "task/a", kind: "task", sha: "t1", task_key: "a" },
  ];
  const layout = layoutGraph({ refs, commits: [c("m", ["r1", "t1"], ["run"]), c("t1", ["b0"], ["task/a"]), c("r1", ["b0"]), c("b0", [], ["main"])] });
  const [m, t1, r1, b0] = layout.rows;
  assert.ok(m.merge);
  assert.ok(!t1.merge);
  assert.equal(m.lane, 0);
  assert.deepEqual(m.bottom, [
    { from: 0, to: 0, color: 1 },
    { from: 0, to: 1, color: 2 },
  ]);
  assert.equal(t1.lane, 1);
  assert.equal(r1.lane, 0);
  assert.equal(b0.lane, 0);
  assert.equal(b0.color, 0);
  assert.equal(layout.lanes, 2);
});
