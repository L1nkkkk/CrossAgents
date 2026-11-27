import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { test } from "node:test";

const guiDir = new URL("..", import.meta.url);
const repoDir = new URL("../../..", import.meta.url);
const pkg = JSON.parse(readFileSync(new URL("package.json", guiDir), "utf8"));
const tauriConf = JSON.parse(readFileSync(new URL("src-tauri/tauri.conf.json", guiDir), "utf8"));
const tauriBundleConf = JSON.parse(readFileSync(new URL("src-tauri/tauri.bundle.conf.json", guiDir), "utf8"));
const routes = JSON.parse(readFileSync(new URL("shared/hub-routes.json", guiDir), "utf8"));
const cargoToml = readFileSync(new URL("Cargo.toml", repoDir), "utf8");

test("Tauri is the sole desktop shell and Electron is removed", () => {
  assert.equal(existsSync(new URL("electron", guiDir)), false);
  assert.equal(pkg.main, undefined);
  assert.equal(pkg.build, undefined);
  for (const dep of ["electron", "electron-builder", "electron-updater"]) {
    assert.equal(pkg.dependencies?.[dep], undefined, `unexpected dependency: ${dep}`);
    assert.equal(pkg.devDependencies?.[dep], undefined, `unexpected devDependency: ${dep}`);
  }
  assert.equal(tauriConf.version, "../package.json");
  assert.equal(tauriConf.build.frontendDist, "../dist-tauri");
  assert.equal(tauriBundleConf.bundle.active, true);
  assert.deepEqual(tauriBundleConf.bundle.targets, ["nsis", "app", "dmg"]);
  assert.deepEqual(tauriBundleConf.bundle.externalBin, ["binaries/crossagents", "binaries/crossagents-link"]);
});

test("GUI and Cargo workspace versions stay aligned", () => {
  const cargoVersion = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(cargoToml)?.[1];
  assert.ok(cargoVersion, "missing [workspace.package].version in Cargo.toml");
  assert.equal(pkg.version, cargoVersion);
});

test("shared desktop Hub route allowlist validates cleanly and blocks sensitive routes", () => {
  assert.ok(Array.isArray(routes) && routes.length > 0);
  const compiled = routes.map((r) => {
    assert.ok(typeof r.method === "string" && r.method.length > 0);
    assert.ok(typeof r.path === "string" && r.path.startsWith("^/v1/"));
    return { method: r.method, re: new RegExp(r.path) };
  });
  const allowed = (method, path) => compiled.some((r) => r.method === method && r.re.test(path));

  assert.ok(allowed("POST", "/v1/agents"));
  assert.ok(allowed("POST", "/v1/browser/requests/abc/claim"));
  assert.ok(allowed("POST", "/v1/browser/requests/abc/result"));
  assert.ok(!allowed("GET", "/v1/browser/requests/abc/claim"));
  assert.ok(!allowed("POST", "/v1/browser/requests"));
  assert.ok(!allowed("POST", "/v1/shutdown"));
  assert.ok(!allowed("GET", "/v1/events"));
  assert.ok(!allowed("GET", "/v1/agents/one/token"));
});
