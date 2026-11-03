import assert from "node:assert/strict";
import { test } from "node:test";
import type { CliInfo } from "@bindings/CliInfo";
import { installingLabel, installingOf, installRefusedText } from "./installState.ts";

const cli = (installing?: string): CliInfo =>
  ({ installed: false, probing: false, can_upgrade: false, installing }) as CliInfo;

test("an install the hub reports disables the row until it ends", () => {
  assert.equal(installingOf(cli("install")), "install");
  assert.equal(installingOf(cli("upgrade")), "upgrade");
  // Nothing running: `installing` is absent from the hub's answer.
  assert.equal(installingOf(cli()), undefined);
  assert.equal(installingOf(undefined), undefined);
  // Anything else the hub might one day say is not taken for a running job.
  assert.equal(installingOf(cli("uninstall")), undefined);
});

test("this window's own job wins while it runs, and its end hands back to the hub", () => {
  assert.equal(installingOf(cli("install"), { action: "upgrade" }), "upgrade");
  // Finished here, still reported by the hub: the hub's word is the newer one.
  assert.equal(installingOf(cli("install"), { action: "install", done: { ok: true } }), "install");
  assert.equal(installingOf(cli(), { action: "install", done: { ok: false } }), undefined);
});

test("the chip says which one is running", () => {
  assert.equal(installingLabel("install"), "安装中");
  assert.equal(installingLabel("upgrade"), "升级中");
});

test("a second click while it runs is answered in words, not the hub's sentence", () => {
  const conflict = Object.assign(new Error("cursor is already being installed; wait for it to finish"), {
    status: 409,
  });
  assert.equal(installRefusedText(conflict, "install"), "正在安装，请等待完成");
  const upgrading = Object.assign(new Error("codex is already being upgraded; wait for it to finish"), {
    status: 409,
  });
  assert.equal(installRefusedText(upgrading, "upgrade"), "正在升级，请等待完成");
});

test("other refusals keep what the hub said", () => {
  const notUpgradable = Object.assign(new Error("cursor cannot be upgraded here: it is the copy the desktop app ships"), {
    status: 409,
  });
  assert.equal(installRefusedText(notUpgradable, "upgrade"), "cursor cannot be upgraded here: it is the copy the desktop app ships");
  assert.equal(installRefusedText(new Error("boom\nsecond line"), "install"), "boom");
});
