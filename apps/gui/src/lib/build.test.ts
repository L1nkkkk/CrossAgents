import assert from "node:assert/strict";
import { test } from "node:test";
import { buildTag, compareBuilds } from "./build.ts";

test("a build reads as its version and commit", () => {
  assert.equal(buildTag({ version: "0.1.0", commit: "d5011e4" }), "v0.1.0 · d5011e4");
  assert.equal(buildTag({ version: "0.1.0" }), "v0.1.0");
});

test("page and hub differ by commit at the same version, else by version", () => {
  const page = { version: "0.1.0", commit: "d5011e4", dev: false };
  assert.deepEqual(compareBuilds(page, { version: "0.1.0", commit: "d5011e4" }), { same: true, page: "d5011e4", hub: "d5011e4" });
  assert.deepEqual(compareBuilds(page, { version: "0.1.0", commit: "5996b02", debug: true }), {
    same: false,
    page: "d5011e4",
    hub: "5996b02",
  });
  assert.deepEqual(compareBuilds(page, { version: "0.2.0", commit: "abc1234" }), { same: false, page: "v0.1.0", hub: "v0.2.0" });
  // An older hub does not say its commit: the version is all there is to compare.
  assert.deepEqual(compareBuilds(page, { version: "0.1.0" }), { same: true, page: "d5011e4", hub: "v0.1.0" });
});
