import assert from "node:assert/strict";
import { test } from "node:test";
import { applicationMenu, shortcut, trayHint, trayIcon } from "./platform.cjs";

test("macOS gets native menus and a template tray icon", () => {
  const menu = applicationMenu("darwin")!;
  assert.deepEqual(menu.map((item) => item.role), ["appMenu", "editMenu", "windowMenu"]);
  assert.deepEqual((menu[1].submenu as { role?: string }[]).filter((item) => item.role).map((item) => item.role), ["undo", "redo", "cut", "copy", "paste", "selectAll"]);
  assert.equal(applicationMenu("win32"), null);
  assert.equal(trayIcon("darwin"), "iconTemplate.png");
  assert.equal(trayIcon("win32"), "icon.ico");
  assert.match(trayHint("darwin", false).content, /菜单栏/);
  assert.match(trayHint("win32", true).content, /任务栏右下角/);
});

test("shortcuts use the platform modifier and development gate", () => {
  const base = { type: "keyDown", key: "=", code: "Equal", control: false, meta: true, alt: false, shift: false };
  assert.equal(shortcut("darwin", base, false), "zoomIn");
  assert.equal(shortcut("darwin", { ...base, key: "-" }, false), "zoomOut");
  assert.equal(shortcut("darwin", { ...base, key: "0" }, false), "zoomReset");
  assert.equal(shortcut("win32", base, false), undefined);
  assert.equal(shortcut("win32", { ...base, meta: false, control: true }, false), "zoomIn");
  assert.equal(shortcut("darwin", { ...base, key: "ˆ", code: "KeyI", alt: true }, true), "devtools");
  assert.equal(shortcut("darwin", { ...base, key: "i", code: "KeyI", alt: true }, false), undefined);
  assert.equal(shortcut("darwin", { ...base, key: "r", code: "KeyR" }, true), "reload");
  assert.equal(shortcut("win32", { ...base, key: "i", control: true, meta: false, shift: true }, true), "devtools");
  assert.equal(shortcut("win32", { ...base, key: "F12", meta: false }, true), "devtools");
});
