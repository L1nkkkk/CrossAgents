import assert from "node:assert/strict";
import { test } from "node:test";
import type { WorkDetail } from "@bindings/WorkDetail";
import { WorkDetails } from "./workDetails.ts";

const detail = (id: string): WorkDetail => ({
  work: { id, title: id, repo: "/repo", lead: "lead", created_at: "1", updated_at: "1" },
  runs: [],
  history: [],
});

/** A loader that remembers which work groups it was asked for. */
function counter() {
  const asked: string[] = [];
  return {
    asked,
    load: async (id: string) => {
      asked.push(id);
      return detail(id);
    },
  };
}

test("the detail requests do not grow with the number of work groups", async () => {
  const counts: number[] = [];
  for (const n of [1, 40]) {
    const details = new WorkDetails();
    const { asked, load } = counter();
    const works = Array.from({ length: n }, (_, i) => `w${i}`);
    // The list comes from one `listWorks`; opening one row is the only thing that reads a
    // detail, whatever else sits in the list (behind 更多, or under a folded project).
    await details.expand(works[0], load);
    counts.push(asked.length);
  }
  assert.deepEqual(counts, [1, 1]);
});

test("a run event refreshes only the open rows, and only stales what was folded", async () => {
  const details = new WorkDetails();
  const { asked, load } = counter();
  await details.expand("a", load);
  await details.expand("b", load);
  assert.deepEqual(asked, ["a", "b"]);

  // Both open: each event refreshes both and nothing else.
  details.collapse("b");
  details.invalidate();
  await details.refresh(load);
  assert.deepEqual(asked, ["a", "b", "a"]);

  // "a" was refreshed at the current version: closing and reopening it asks nothing.
  details.collapse("a");
  await details.expand("a", load);
  assert.deepEqual(asked, ["a", "b", "a"]);

  // Folded while things changed: it is marked stale, so reopening rereads it once.
  details.collapse("a");
  details.invalidate();
  const cached = details.get("a");
  assert.equal(cached?.work.id, "a");
  await details.expand("a", load);
  assert.deepEqual(asked, ["a", "b", "a", "a"]);
  await details.expand("a", load);
  assert.deepEqual(asked, ["a", "b", "a", "a"]);
});

test("a row opened while its read is in flight asks once", async () => {
  const details = new WorkDetails();
  const { asked, load } = counter();
  await Promise.all([details.expand("a", load), details.expand("a", load)]);
  await details.refresh(load);
  assert.deepEqual(asked, ["a", "a"]);
});
