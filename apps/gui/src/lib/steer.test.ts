import assert from "node:assert/strict";
import { test } from "node:test";
import { canSteer, composerAction, composerHint, composerPlaceholder } from "./steer.ts";

const enter = (init: { ctrl?: boolean; shift?: boolean; running: boolean; steer?: boolean }) => ({
  key: "Enter",
  ctrl: init.ctrl ?? false,
  shift: init.shift ?? false,
  running: init.running,
  steer: init.steer ?? false,
});

test("Ctrl+Shift+Enter interrupts and sends; Ctrl+Enter steers", () => {
  // The bug this replaced: Ctrl+Shift+Enter was read as Enter (a newline) and Ctrl+Enter as
  // "interrupt and send", the reverse of the hint.
  assert.equal(composerAction(enter({ ctrl: true, shift: true, running: true, steer: true })), "interrupt-send");
  assert.equal(composerAction(enter({ ctrl: true, running: true, steer: true })), "steer");
});

test("Enter queues while a turn runs, and sends when nothing does", () => {
  assert.equal(composerAction(enter({ running: true, steer: true })), "queue");
  assert.equal(composerAction(enter({ running: false })), "send");
  // Shift+Enter is a newline: the textarea keeps it.
  assert.equal(composerAction(enter({ shift: true, running: true })), undefined);
  assert.equal(composerAction(enter({ shift: true, running: false })), undefined);
  // Another key is none of the composer's business.
  assert.equal(composerAction({ key: "a", running: true, steer: true }), undefined);
});

test("a driver that cannot steer falls back to the queue on Ctrl+Enter", () => {
  assert.equal(composerAction(enter({ ctrl: true, running: true, steer: false })), "queue");
  // Nothing runs, so there is nothing to steer: it is an ordinary send.
  assert.equal(composerAction(enter({ ctrl: true, running: false })), "send");
  assert.equal(composerAction(enter({ ctrl: true, shift: true, running: false })), "send");
});

test("canSteer reads the capability, and says no when it is unknown", () => {
  assert.equal(canSteer({ steer: true }), true);
  assert.equal(canSteer({ steer: false }), false);
  assert.equal(canSteer(null), false);
  assert.equal(canSteer(undefined), false);
});

test("the hint names each key, and explains the driver that cannot steer", () => {
  assert.equal(composerHint({ running: false, steer: true, interrupt: true }), "Enter 发送 · Shift+Enter 换行 · agent 离线时会自动启动");
  const steering = composerHint({ running: true, steer: true, interrupt: true });
  assert.match(steering, /Enter 排队/);
  assert.match(steering, /Ctrl\+Enter 插话/);
  assert.match(steering, /Ctrl\+Shift\+Enter 打断并发送/);
  assert.match(steering, /Esc 中断/);
  // The fallback stays visible and says why, rather than dropping the key.
  const queued = composerHint({ running: true, steer: false, interrupt: true });
  assert.match(queued, /Ctrl\+Enter 也排队/);
  assert.match(queued, /这个驱动不能插话/);
  // No interrupt capability: no Esc.
  assert.doesNotMatch(composerHint({ running: true, steer: true, interrupt: false }), /Esc/);
});

test("the placeholder says what Enter will do with what is typed", () => {
  assert.equal(composerPlaceholder({ name: "Ada", running: false, steer: true }), "给 Ada 发消息，@ 点名协作者或模型，/ 看指令");
  assert.match(composerPlaceholder({ name: "Ada", running: true, steer: true }), /Ctrl\+Enter 插话/);
  const queued = composerPlaceholder({ name: "Ada", running: true, steer: false });
  assert.match(queued, /这个驱动不能插话/);
  assert.match(queued, /排在本轮之后/);
});
