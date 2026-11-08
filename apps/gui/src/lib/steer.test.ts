import assert from "node:assert/strict";
import { test } from "node:test";
import { canSteer, composerAction, composerHint, composerPlaceholder } from "./steer.ts";

test("composer hints use Mac symbols", () => {
  assert.match(composerHint({ platform: "mac", state: "idle", steer: true, interrupt: true }), /⇧Enter/);
  assert.match(composerHint({ platform: "mac", state: "turn", steer: true, interrupt: true }), /⌘Enter 插话 · ⇧⌘Enter/);
  assert.match(composerPlaceholder({ platform: "mac", name: "Ada", state: "turn", steer: true }), /⌘Enter 插话/);
});

const enter = (init: { ctrl?: boolean; shift?: boolean; busy: boolean; steer?: boolean }) => ({
  key: "Enter",
  ctrl: init.ctrl ?? false,
  shift: init.shift ?? false,
  busy: init.busy,
  steer: init.steer ?? false,
});

test("Ctrl+Shift+Enter interrupts and sends; Ctrl+Enter steers", () => {
  // The bug this replaced: Ctrl+Shift+Enter was read as Enter (a newline) and Ctrl+Enter as
  // "interrupt and send", the reverse of the hint.
  assert.equal(composerAction(enter({ ctrl: true, shift: true, busy: true, steer: true })), "interrupt-send");
  assert.equal(composerAction(enter({ ctrl: true, busy: true, steer: true })), "steer");
});

test("Enter queues while messages would wait, and sends when nothing does", () => {
  assert.equal(composerAction(enter({ busy: true, steer: true })), "queue");
  assert.equal(composerAction(enter({ busy: false })), "send");
  // Shift+Enter is a newline: the textarea keeps it.
  assert.equal(composerAction(enter({ shift: true, busy: true })), undefined);
  assert.equal(composerAction(enter({ shift: true, busy: false })), undefined);
  // Another key is none of the composer's business.
  assert.equal(composerAction({ key: "a", busy: true, steer: true }), undefined);
});

test("a driver that cannot steer falls back to the queue on Ctrl+Enter", () => {
  assert.equal(composerAction(enter({ ctrl: true, busy: true, steer: false })), "queue");
  // Nothing runs, so there is nothing to steer: it is an ordinary send.
  assert.equal(composerAction(enter({ ctrl: true, busy: false })), "send");
  assert.equal(composerAction(enter({ ctrl: true, shift: true, busy: false })), "send");
});

test("busy covers every non-idle hub state, not only a running turn", () => {
  // P1-4: a message waiting behind another, a session starting or one outside the hub all mean
  // Enter would queue; the key must say "queue" in all of them, as the hint does.
  for (const state of ["starting", "turn", "held"] as const) {
    assert.equal(composerHint({ platform: "other", state, steer: true, interrupt: true }).startsWith("Enter 排队"), true, state);
  }
  assert.equal(composerHint({ platform: "other", state: "idle", steer: true, interrupt: true }).startsWith("Enter 发送"), true);
});

test("canSteer reads the capability, and says no when it is unknown", () => {
  assert.equal(canSteer({ steer: true }), true);
  assert.equal(canSteer({ steer: false }), false);
  assert.equal(canSteer(null), false);
  assert.equal(canSteer(undefined), false);
});

test("the hint names each key, and explains the driver that cannot steer", () => {
  assert.equal(composerHint({ platform: "other", state: "idle", steer: true, interrupt: true }), "Enter 发送 · Shift+Enter 换行 · agent 离线时会自动启动");
  const steering = composerHint({ platform: "other", state: "turn", steer: true, interrupt: true });
  assert.match(steering, /Enter 排队/);
  assert.match(steering, /Ctrl\+Enter 插话/);
  assert.match(steering, /Ctrl\+Shift\+Enter 打断并发送/);
  assert.match(steering, /Esc 中断/);
  // The fallback stays visible and says why, rather than dropping the key.
  const queued = composerHint({ platform: "other", state: "turn", steer: false, interrupt: true });
  assert.match(queued, /Ctrl\+Enter 也排队/);
  assert.match(queued, /这个驱动不能插话/);
  // No interrupt capability: no Esc.
  assert.doesNotMatch(composerHint({ platform: "other", state: "turn", steer: true, interrupt: false }), /Esc/);
});

test("a session that runs outside the hub is not promised a start it will never do", () => {
  const held = composerHint({ platform: "other", state: "held", steer: false, interrupt: false });
  assert.match(held, /不在 hub 里跑/);
  assert.match(held, /自己来取/);
  assert.doesNotMatch(held, /自动启动/);
  const starting = composerHint({ platform: "other", state: "starting", steer: false, interrupt: false });
  assert.match(starting, /正在启动/);
});

test("the placeholder says what Enter will do with what is typed", () => {
  assert.equal(composerPlaceholder({ platform: "other", name: "Ada", state: "idle", steer: true }), "给 Ada 发消息，@ 点名协作者或模型，/ 看指令");
  assert.match(composerPlaceholder({ platform: "other", name: "Ada", state: "turn", steer: true }), /Ctrl\+Enter 插话/);
  const queued = composerPlaceholder({ platform: "other", name: "Ada", state: "turn", steer: false });
  assert.match(queued, /这个驱动不能插话/);
  assert.match(queued, /排在本轮之后/);
  // The external agent: the message waits for it, it does not get started for it.
  const held = composerPlaceholder({ platform: "other", name: "Ada", state: "held", steer: false });
  assert.match(held, /不在 hub 里运行/);
  assert.match(held, /自己来取/);
  assert.match(composerPlaceholder({ platform: "other", name: "Ada", state: "starting", steer: false }), /正在启动/);
});
