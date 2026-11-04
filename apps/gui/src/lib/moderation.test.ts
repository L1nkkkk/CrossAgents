import assert from "node:assert/strict";
import { test } from "node:test";
import { MODERATION_RULES, isModerationError } from "./moderation.ts";

test("a gateway moderation refusal is recognized", () => {
  assert.equal(isModerationError("content_filter: the request was filtered"), true);
  assert.equal(isModerationError('{"error":{"code":"content_filter","message":"blocked"}}'), true);
  assert.equal(isModerationError("content_policy_violation"), true);
  assert.equal(isModerationError("The response was filtered due to the content management policy"), true);
  assert.equal(isModerationError("The request was rejected as a result of our safety system."), true);
  assert.equal(isModerationError("data_inspection_failed: Input data may contain inappropriate content."), true);
  assert.equal(isModerationError("Output blocked by content filtering policy"), true);
  assert.equal(isModerationError("moderation flagged the request"), true);
  assert.equal(isModerationError("the content filter rejected this prompt"), true);
  assert.equal(isModerationError("内容审核未通过：输入包含敏感内容"), true);
  assert.equal(isModerationError("请求被内容安全拦截"), true);
  assert.equal(isModerationError("输入内容不合规"), true);
  assert.equal(isModerationError("内容包含敏感词，已拦截"), true);
});

test("an ordinary error is not read as moderation", () => {
  assert.equal(isModerationError(undefined), false);
  assert.equal(isModerationError(""), false);
  assert.equal(isModerationError("cannot start with-cli: program not found"), false);
  assert.equal(isModerationError("Not logged in"), false);
  assert.equal(isModerationError("moderation service timed out"), false);
  assert.equal(isModerationError("the content filter tool failed"), false);
  assert.equal(isModerationError("content policy"), false);
  assert.equal(isModerationError("the safety system is unavailable"), false);
  assert.equal(isModerationError("代码审查未通过"), false);
  assert.equal(isModerationError("内容安全服务超时"), false);
  assert.equal(isModerationError("请审查这段内容"), false);
  assert.equal(isModerationError("敏感词列表已更新"), false);
});

test("every rule says where it came from and none claims confirmation", () => {
  for (const rule of MODERATION_RULES) {
    assert.ok(rule.source.length > 10, `${rule.id} needs a source`);
    assert.equal(rule.confirmed, false, `${rule.id} is not confirmed by a real error`);
    assert.ok(!rule.pattern.global, `${rule.id} must not carry a stateful flag`);
  }
});
