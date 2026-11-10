import assert from "node:assert/strict";
import { test } from "node:test";
import type { LinkStatus } from "@bindings/LinkStatus";
import { CODE_STEPS, codeFailure, codeStepIndex, countdown, entranceWords, pathWords, previewCode } from "./link.ts";

const now = new Date("2026-11-10T10:00:00Z");
const code = (fields: Record<string, unknown> = {}) =>
  `ca1-${Buffer.from(JSON.stringify({ e: "ab".repeat(32), r: "https://relay.example./", a: ["192.168.1.20:1"], s: "s", h: "DESKTOP-CENTRE", x: now.getTime() / 1000 + 1800, ...fields })).toString("base64url")}`;

test("a pasted code is previewed before anything runs", () => {
  assert.equal(previewCode("   ", now), undefined);
  const ok = previewCode(`${code().slice(0, 20)}\n${code().slice(20)}`, now);
  assert.ok(ok?.ok);
  assert.equal(ok.host, "DESKTOP-CENTRE");
  assert.equal(ok.relay, true);
  assert.equal(ok.lan, 1);
  assert.deepEqual(previewCode("hello", now), { ok: false, error: "这不是配对码：配对码以 ca1- 开头。" });
  assert.match((previewCode("ca1-@@@", now) as { error: string }).error, /不完整/);
  assert.match((previewCode(code({ x: 1 }), now) as { error: string }).error, /过期/);
  const lanOnly = previewCode(code({ r: undefined }), now);
  assert.ok(lanOnly?.ok && !lanOnly.relay);
});

test("countdown and steps", () => {
  assert.equal(countdown(new Date(now.getTime() + 125_000), now), "2:05");
  assert.equal(countdown(new Date(now.getTime() - 1), now), "");
  assert.deepEqual(CODE_STEPS.map((s) => s.key), ["redeem", "forward", "check"]);
  assert.equal(codeStepIndex("forward"), 1);
  assert.equal(codeStepIndex(undefined), -1);
});

test("the entrance says what to do in each state", () => {
  const base: LinkStatus = { enabled: true, state: "off", relay_connected: false, pending_invites: 0 };
  assert.equal(entranceWords(undefined).word, "读取中");
  assert.equal(entranceWords({ ...base, enabled: false }).word, "未开启");
  assert.match(entranceWords({ ...base, state: "starting" }).detail, /代理/);
  assert.match(entranceWords({ ...base, state: "starting", proxy: "http://127.0.0.1:7890" }).detail, /127\.0\.0\.1:7890/);
  const online = entranceWords({ ...base, state: "online", relay: "https://aps1-1.relay.example./", relay_connected: true });
  assert.equal(online.tone, "ok");
  assert.match(online.detail, /aps1-1\.relay\.example/);
  assert.deepEqual(entranceWords({ ...base, state: "failed", error: "boom" }), { word: "没有开起来", tone: "danger", detail: "boom" });
});

test("path words and what a failure means", () => {
  assert.equal(pathWords({ status: "connected", path: "direct" })?.word, "直连");
  assert.equal(pathWords({ status: "connected", path: "relay" })?.word, "经中继");
  assert.equal(pathWords({ status: "connecting", path: "relay" }), undefined);
  assert.equal(codeFailure("这个配对码已经用过、过期，或中心的 hub 重启过").title, "配对码不能用了");
  assert.equal(codeFailure("这不是 CrossAgents 的配对码（配对码以 ca1- 开头）").title, "配对码不对");
  assert.equal(codeFailure("找不到 crossagents-link（x）：这个安装包里缺少打洞程序").title, "这个安装包缺少打洞程序");
  assert.equal(codeFailure("找不到中心：中心不在线\ncannot reach the hub: No addressing information available").title, "连不上中心");
  assert.equal(codeFailure("something else").title, "配对没有完成");
});
