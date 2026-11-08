import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { trayIcon } from "../dist-electron/platform.cjs";

const { build } = JSON.parse(readFileSync(new URL("../package.json", import.meta.url), "utf8"));
const hub = { from: "../../target/release/crossagents.exe", to: "bin/crossagents.exe" };
const winIcon = { from: "build/icon.ico", to: trayIcon("win32") };
const macIcon = trayIcon("darwin");
const macRetina = macIcon.replace(/\.png$/, "@2x.png");

// electron-builder's getFileMatchers combines top-level and platform resources.
test("platform resources and ad-hoc signing", () => {
  assert.deepEqual(build.win.extraResources, [hub, winIcon]);
  assert.deepEqual(build.mac.extraResources, [
    { from: `build/${macIcon}`, to: macIcon },
    { from: `build/${macRetina}`, to: macRetina },
  ]);
  assert.equal(build.extraResources, undefined);
  assert.equal(build.mac.identity, "-");
  assert.equal(build.mac.hardenedRuntime, false);
});
