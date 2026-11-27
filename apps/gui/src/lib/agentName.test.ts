import assert from "node:assert/strict";
import { test } from "node:test";
import { isBareGreeting, parseScopedName, semanticTopic, shortScopedLabel } from "./agentName.ts";

test("parseScopedName splits CA-Nyx-12 and leaves legacy/custom names untouched", () => {
  assert.deepEqual(parseScopedName("CA-Nyx-12"), { code: "CA", word: "Nyx", seq: "12" });
  assert.deepEqual(parseScopedName("NS-Altair-1"), { code: "NS", word: "Altair", seq: "1" });
  assert.deepEqual(parseScopedName("笔记-Thoth-3"), { code: "笔记", word: "Thoth", seq: "3" });
  assert.equal(parseScopedName("Nova 4"), null);
  assert.equal(parseScopedName("Ada"), null);
  assert.equal(parseScopedName("w-fix-1"), null);
});

test("shortScopedLabel omits redundant project prefix inside a project group", () => {
  assert.equal(shortScopedLabel("CA-Nyx-12", true), "Nyx #12");
  assert.equal(shortScopedLabel("CA-Nyx-12", false), "CA-Nyx-12");
  assert.equal(shortScopedLabel("Nova 4", true), "Nova 4");
});

test("semanticTopic skips bare greetings and strips conversational fillers", () => {
  assert.equal(isBareGreeting("你好！"), true);
  assert.equal(semanticTopic("你好"), undefined);
  assert.equal(semanticTopic("hello"), undefined);
  assert.equal(semanticTopic("你看看你能不能解决一下Pia 5会话遇到的问题"), "解决一下Pia 5会话遇到的问题");
  assert.equal(semanticTopic("please fix proxy timeout"), "fix proxy timeout");
  assert.equal(semanticTopic("history view layout"), "history view layout");
  assert.equal(semanticTopic("修一下登录页\n细节如下"), "修一下登录页");
});
