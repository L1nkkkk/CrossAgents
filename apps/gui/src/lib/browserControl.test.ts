import assert from "node:assert/strict";
import { test } from "node:test";
import { AGENT_HOLD_MS, holdAfterRequest, holdLapsed, isHashOnlyChange, runBrowserWait, type Hold } from "./browserControl.ts";

test("an agent's bar stays for 15 seconds from its last request", () => {
  const held = holdAfterRequest(null, "Nova", 1_000);
  assert.deepEqual(held, { kind: "agent", agent: "Nova", until: 1_000 + AGENT_HOLD_MS });
  assert.equal(holdLapsed(held, 1_000), false);
  assert.equal(holdLapsed(held, 1_000 + AGENT_HOLD_MS - 1), false);
  assert.equal(holdLapsed(held, 1_000 + AGENT_HOLD_MS), true);
});

test("a second request from the same agent pushes the deadline out", () => {
  const first = holdAfterRequest(null, "Nova", 0);
  const second = holdAfterRequest(first, "Nova", 10_000);
  assert.deepEqual(second, { kind: "agent", agent: "Nova", until: 10_000 + AGENT_HOLD_MS });
  // The first deadline has come and gone, and the bar is still up.
  assert.equal(holdLapsed(second, 15_000), false);
  assert.equal(holdLapsed(second, AGENT_HOLD_MS * 2), true);
});

test("a person who took the tab over keeps it, and its bar never lapses", () => {
  const human: Hold = { kind: "human" };
  assert.deepEqual(holdAfterRequest(human, "Nova", 5_000), human);
  assert.equal(holdLapsed(human, 10_000_000), false);
  assert.equal(holdLapsed(null, 10_000_000), false);
});

test("another agent may hold the tab after the first one's bar went", () => {
  const nova = holdAfterRequest(null, "Nova", 0);
  const rex = holdAfterRequest(nova, "Rex", AGENT_HOLD_MS);
  assert.deepEqual(rex, { kind: "agent", agent: "Rex", until: AGENT_HOLD_MS * 2 });
});

test("isHashOnlyChange detects fragment-only navigations on the same document", () => {
  assert.equal(isHashOnlyChange("https://example.com/app", "https://example.com/app#tab2"), true);
  assert.equal(isHashOnlyChange("https://example.com/app#tab1", "https://example.com/app#tab2"), true);
  assert.equal(isHashOnlyChange("https://example.com/app#tab1", "https://example.com/app#tab1"), false);
  assert.equal(isHashOnlyChange("https://example.com/a", "https://example.com/b"), false);
  assert.equal(isHashOnlyChange("https://example.com/a?x=1", "https://example.com/a?x=2"), false);
});

test("runBrowserWait retries across transient navigation errors until condition matches", async () => {
  let clock = 1000;
  let calls = 0;
  const result = await runBrowserWait({
    input: { text: "Ready", timeout_ms: 2000 },
    now: () => clock,
    sleep: async (ms) => { clock += ms; },
    pollMs: 100,
    probe: async () => {
      calls++;
      if (calls === 1) throw new Error("浏览器页面没有回应");
      if (calls === 2) return { matched: false, url: "https://example.com/loading", title: "Loading" };
      return { matched: true, url: "https://example.com/done", title: "Done" };
    },
  });
  assert.equal(calls, 3);
  assert.equal(result.ok, true);
  assert.equal(result.url, "https://example.com/done");
  assert.equal(result.title, "Done");
  assert.equal(result.waited_ms, 200);
});

test("runBrowserWait fails fast on invalid selector or human takeover", async () => {
  let calls = 0;
  await assert.rejects(
    () =>
      runBrowserWait({
        input: { selector: "div[broken", timeout_ms: 5000 },
        sleep: async () => {},
        probe: async () => {
          calls++;
          throw new Error("无效的 CSS 选择器：div[broken");
        },
      }),
    /无效的 CSS 选择器/,
  );
  assert.equal(calls, 1);

  await assert.rejects(
    () =>
      runBrowserWait({
        input: { text: "Never", timeout_ms: 5000 },
        onTick: () => {
          throw new Error("human_control: 这个标签正由人接管");
        },
        sleep: async () => {},
        probe: async () => ({ matched: false, url: "", title: "" }),
      }),
    /human_control:/,
  );
});

test("runBrowserWait times out with condition summary and supports simple ms delay", async () => {
  let clock = 0;
  await assert.rejects(
    () =>
      runBrowserWait({
        input: { text: "Loaded", timeout_ms: 300 },
        now: () => clock,
        sleep: async (ms) => { clock += ms; },
        pollMs: 100,
        probe: async () => ({ matched: false, url: "https://example.com", title: "Wait" }),
      }),
    /等待超时（300ms）.*text="Loaded"/,
  );

  clock = 0;
  const settled = await runBrowserWait({
    input: { ms: 250 },
    now: () => clock,
    sleep: async (ms) => { clock += ms; },
    pollMs: 100,
    probe: async () => ({ matched: true, url: "https://example.com/ok", title: "OK" }),
  });
  assert.equal(settled.waited_ms, 250);
  assert.equal(settled.title, "OK");
});
