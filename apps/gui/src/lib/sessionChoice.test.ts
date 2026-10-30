import assert from "node:assert/strict";
import { test } from "node:test";
import type { Agent } from "@bindings/Agent";
import type { SessionCandidate } from "@bindings/SessionCandidate";
import { hasKey, t } from "./i18n.ts";
import { chatBody, newChatRequest, runSessionFields, sessionStateKey, usabilityOf } from "./sessionChoice.ts";

const agent = { id: "a1", name: "Ada", plugin: "claude", model: "claude-opus-4-1" } as unknown as Agent;

const candidate = (over: Partial<SessionCandidate>): SessionCandidate => ({
  agent,
  state: "idle",
  busy_with: null,
  last_execution: null,
  can_use: true,
  reason: null,
  ...over,
});

test("a new session adds nothing to the Work body; an existing one says fresh: false", () => {
  const body = { request: "把登录页改成 OAuth", planner: "p1", worker: "w1" };
  const fresh = { ...body, ...runSessionFields(undefined) };
  assert.deepEqual(fresh, body);
  assert.equal("fresh" in fresh, false);
  assert.equal("restore" in fresh, false);

  assert.deepEqual(runSessionFields(candidate({})), { fresh: false });
});

test("an archived session also asks the hub to restore it", () => {
  const archived = candidate({ state: "archived", reason: "archived" });
  assert.deepEqual(runSessionFields(archived), { fresh: false, restore: true });
  // Chat's own body: restore is always stated there.
  assert.deepEqual(chatBody("接着改", archived), { text: "接着改", restore: true });
  assert.deepEqual(chatBody("开始", candidate({})), { text: "开始", restore: false });
});

test("a new chat is made as the same agent request as before", () => {
  assert.deepEqual(newChatRequest({ plugin: "claude", model: "claude-opus-4-1" }, "修一下登录页\n细节如下", "", undefined), {
    topic: "修一下登录页",
    plugin: "claude",
    driver: null,
    model: "claude-opus-4-1",
    role: "worker",
    cwd: "",
    config: {},
    profile: { display_name: null, title: null, avatar: null, color: null, persona: null },
  });
  assert.deepEqual(newChatRequest({ plugin: "claude", model: "claude-opus-4-1", account: "claude-max" }, "hi", "C:/work/shop", "high").config, {
    account: "claude-max",
    effort: "high",
  });
});

test("busy candidates show the hub's sentence as it is, and cannot be picked", () => {
  const said = 'Rex is busy: it works on #auth of run "Login" (01a1)';
  const busy = usabilityOf(candidate({ state: "busy", reason: "busy", busy_with: said, can_use: false }));
  assert.equal(busy.selectable, false);
  assert.equal(t(busy.reason!.key, busy.reason!.vars), said);

  const held = usabilityOf(candidate({ state: "busy", reason: "busy", busy_with: null, can_use: false }));
  assert.equal(held.selectable, false);
  assert.equal(t(held.reason!.key), "正被别的 run 或人占用");
});

test("cannot_move is refused wherever can_use says so", () => {
  const immovable = usabilityOf(candidate({ reason: "cannot_move", can_use: false }));
  assert.equal(immovable.selectable, false);
  assert.equal(t(immovable.reason!.key), "它的驱动不能在别的目录里接着原来的会话");
  // can_use is the hub's answer even when a reason is present: a stale snapshot is not a veto.
  assert.equal(usabilityOf(candidate({ reason: "cannot_move", can_use: true })).selectable, true);
});

test("archived candidates stay selectable with a restore hint", () => {
  const archived = usabilityOf(candidate({ state: "archived", reason: "archived", can_use: true }));
  assert.equal(archived.selectable, true);
  assert.equal(archived.restore, true);
  assert.equal(t(archived.hint!.key), "使用前会先恢复");
  // An archived candidate the hub says cannot be used is still disabled: can_use governs.
  assert.equal(usabilityOf(candidate({ reason: "archived", can_use: false })).selectable, false);
});

test("any other refusal keeps the hub's reason visible", () => {
  const odd = usabilityOf(candidate({ reason: "not_here", can_use: false }));
  assert.equal(odd.selectable, false);
  assert.equal(t(odd.reason!.key, odd.reason!.vars), "现在不能用：not_here");
});

test("nothing is disabled without saying why", () => {
  const refused = [
    usabilityOf(candidate({ reason: "busy", can_use: false })),
    usabilityOf(candidate({ reason: "cannot_move", can_use: false })),
    usabilityOf(candidate({ reason: "not_here", can_use: false })),
    usabilityOf(candidate({ can_use: false })),
  ];
  for (const use of refused) {
    assert.equal(use.selectable, false);
    assert.ok(use.reason, "a disabled candidate must carry a reason");
  }
});

test("states have a readable name", () => {
  assert.equal(t(sessionStateKey("idle")), "空闲");
  assert.equal(t(sessionStateKey("busy")), "占用中");
  assert.equal(t(sessionStateKey("archived")), "已归档");
  assert.equal(t(sessionStateKey("something-new")), "状态未知");
});

test("every key this module can return is in the dictionary", () => {
  const keys = [
    "session.state.idle",
    "session.state.busy",
    "session.state.archived",
    "session.state.other",
    "session.reason.busy",
    "session.reason.busyFallback",
    "session.reason.cannotMove",
    "session.reason.unavailable",
    "session.hint.archived",
  ];
  assert.deepEqual(keys.filter((k) => !hasKey(k)), []);
});
