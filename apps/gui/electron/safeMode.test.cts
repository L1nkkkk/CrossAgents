import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { decideSafeMode, loadingMarker } from "./safeMode.cjs";

test("normal and explicitly requested safe mode", () => {
  assert.deepEqual(decideSafeMode([], false), { enabled: false, reason: null });
  assert.equal(decideSafeMode(["--safe-mode"], false).enabled, true);
  assert.match(decideSafeMode(["--safe-mode"], true).reason!, /--safe-mode/);
  assert.equal(decideSafeMode(["--safe-mode=false"], false).enabled, false);
});

test("only an unfinished extension load triggers automatic safe mode", () => {
  const home = mkdtempSync(join(tmpdir(), "ca-safe-mode-"));
  try {
    const file = join(home, "state", "loading");
    const first = loadingMarker(file);
    assert.equal(decideSafeMode([], first.unfinished()).enabled, false);
    first.start();
    const nextLaunch = loadingMarker(file);
    const state = decideSafeMode([], nextLaunch.unfinished());
    assert.equal(state.enabled, true);
    assert.match(state.reason!, /上次启动.*未能就绪/);
    // A safe-mode launch may report ready without reporting a new load.
    nextLaunch.ready();
    nextLaunch.ready();
    assert.equal(decideSafeMode([], loadingMarker(file).unfinished()).enabled, false);
    first.start();
    first.ready();
    assert.equal(decideSafeMode([], loadingMarker(file).unfinished()).enabled, false);
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});
