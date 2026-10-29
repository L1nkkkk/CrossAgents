// Runs compiled (`npm test` builds electron/ first): node cannot strip types from a .cts that uses `import`.

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { binRoot, cleanup, compareVersions, install, isInside, plan, reasonOf, shadowRoot } from "./localHub.cjs";

const EXE = process.platform === "win32" ? "crossagents.exe" : "crossagents";

function tempHome(): string {
  return mkdtempSync(join(tmpdir(), "crossagents-gui-"));
}

test("a failure is shown without the command line's `error: ` prefix", () => {
  // docs/TODO.md §一: the reason a hub could not start is what the offline page shows.
  assert.equal(
    reasonOf("error: hub exited during startup (exit code: 1): the database comes from a newer CrossAgents\n", "", ""),
    "hub exited during startup (exit code: 1): the database comes from a newer CrossAgents",
  );
  assert.equal(reasonOf("", "  error: boom  ", ""), "boom");
  assert.equal(reasonOf("", "", "spawn failed"), "spawn failed");
});

test("versions compare by number, a release after its pre-releases", () => {
  assert.ok(compareVersions("0.1.0", "0.2.0")! < 0);
  assert.ok(compareVersions("0.10.0", "0.9.9")! > 0);
  assert.equal(compareVersions("1.2.3", "1.2.3"), 0);
  assert.ok(compareVersions("1.0.0-beta.1", "1.0.0")! < 0);
  assert.ok(compareVersions("1.0.0", "1.0.0-rc.1")! > 0);
  assert.equal(compareVersions("dev", "1.0.0"), undefined);
});

test("only a hub running from <home>/bin is ours", () => {
  const home = join(tmpdir(), "home");
  const ours = join(binRoot(home), "0.1.0", EXE);
  assert.ok(isInside(ours, binRoot(home)));
  assert.ok(!isInside(join(home, "target", "debug", EXE), binRoot(home)));
  assert.ok(!isInside(binRoot(home), binRoot(home)));
  assert.ok(!isInside(join(home, "bin-old", EXE), binRoot(home)));
});

test("the plan starts, keeps, replaces or leaves the hub alone", () => {
  const home = join(tmpdir(), "home");
  const file = (version: string, exe?: string) => ({ pid: 1, port: 7420, version, exe });
  const at = (version: string) => join(binRoot(home), version, EXE);

  assert.deepEqual(plan(undefined, "0.2.0", home), { action: "start" });
  assert.deepEqual(plan({ file: file("0.2.0", at("0.2.0")), version: "0.2.0" }, "0.2.0", home), { action: "keep" });
  assert.deepEqual(plan({ file: file("0.1.0", at("0.1.0")), version: "0.1.0" }, "0.2.0", home), {
    action: "replace",
    from: "0.1.0",
  });
  // An older GUI never downgrades the hub.
  assert.deepEqual(plan({ file: file("0.3.0", at("0.3.0")), version: "0.3.0" }, "0.2.0", home), {
    action: "newer",
    running: "0.3.0",
  });
  // A development build runs from its copy under <home>/shadow: kept, but called out, even at
  // the same version string as the shipped hub.
  const shadow = join(shadowRoot(home), "881aadafed8a0740", EXE);
  assert.deepEqual(plan({ file: file("0.1.0", shadow), version: "0.1.0" }, "0.1.0", home), {
    action: "dev",
    exe: shadow,
    running: "0.1.0",
  });
  // Started by hand, or by a hub too old to say where it runs from: never touched.
  const dev = join(home, "Dev", "CrossAgents", "target", "debug", EXE);
  assert.deepEqual(plan({ file: file("0.1.0", dev), version: "0.1.0" }, "0.2.0", home), { action: "foreign" });
  assert.deepEqual(plan({ file: file("0.1.0"), version: "0.1.0" }, "0.2.0", home), { action: "foreign" });
});

test("install copies into a version directory once", async () => {
  const home = tempHome();
  try {
    const shipped = join(home, "shipped");
    writeFileSync(shipped, "build 1");
    const target = await install(shipped, home, "0.1.0");
    assert.equal(target, join(binRoot(home), "0.1.0", EXE));
    assert.equal(readFileSync(target, "utf8"), "build 1");
    // A different build of the same version replaces a copy nobody runs.
    writeFileSync(shipped, "build 2");
    assert.equal(await install(shipped, home, "0.1.0"), target);
    assert.equal(readFileSync(target, "utf8"), "build 2");
    assert.ok(!existsSync(`${target}.partial`));
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});

test("cleanup removes old versions but not the kept ones", async () => {
  const home = tempHome();
  try {
    for (const v of ["0.1.0", "0.2.0", "0.3.0"]) {
      mkdirSync(join(binRoot(home), v), { recursive: true });
      writeFileSync(join(binRoot(home), v, EXE), v);
    }
    const removed = await cleanup(home, [join(binRoot(home), "0.3.0"), join(binRoot(home), "0.2.0")]);
    assert.deepEqual(removed, ["0.1.0"]);
    assert.ok(existsSync(join(binRoot(home), "0.2.0", EXE)));
    assert.ok(existsSync(join(binRoot(home), "0.3.0", EXE)));
    assert.deepEqual(await cleanup(join(home, "nowhere"), []), []);
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});

test("cleanup keeps a version some process still runs from", { skip: process.platform !== "win32" }, async () => {
  const home = tempHome();
  const dir = join(binRoot(home), "0.1.0");
  mkdirSync(dir, { recursive: true });
  // Any long-running program stands in for a runtime host left by the old hub.
  copyFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "PING.EXE"), join(dir, EXE));
  const child = spawn(join(dir, EXE), ["-n", "30", "127.0.0.1"], { stdio: "ignore", windowsHide: true });
  try {
    await new Promise((r) => child.once("spawn", r));
    assert.deepEqual(await cleanup(home, []), []);
    assert.ok(existsSync(join(dir, EXE)));
    child.kill();
    await new Promise((r) => child.once("exit", r));
    assert.deepEqual(await cleanup(home, []), ["0.1.0"]);
  } finally {
    child.kill();
    rmSync(home, { recursive: true, force: true });
  }
});
