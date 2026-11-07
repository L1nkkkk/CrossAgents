import assert from "node:assert/strict";
import { test } from "node:test";
import { PERMISSION_OPTIONS, configWithPermission, cursorCannotGuard, permissionOf } from "./permission.ts";

test("permissionOf reads the three stored states and treats anything else as follow-the-run", () => {
  assert.equal(permissionOf({ config: { permission: "guard" } }), "guard");
  assert.equal(permissionOf({ config: { permission: "bypass" } }), "bypass");
  assert.equal(permissionOf({ config: { permission: "deny" } }), "deny");
  assert.equal(permissionOf({ config: {} }), null);
  assert.equal(permissionOf({ config: { permission: "whatever" } }), null);
  assert.equal(permissionOf({ config: null }), null);
  assert.equal(permissionOf({}), null);
});

test("the form offers four choices, each with its line, starting at follow-the-run", () => {
  assert.deepEqual(
    PERMISSION_OPTIONS.map((o) => o.value),
    [null, "guard", "bypass", "deny"],
  );
  for (const option of PERMISSION_OPTIONS) assert.ok(option.label && option.hint && option.short);
});

test("configWithPermission writes the choice into a createAgent body and 跟随 removes it", () => {
  assert.deepEqual(configWithPermission({ account: "a" }, "bypass"), { account: "a", permission: "bypass" });
  assert.deepEqual(configWithPermission({ account: "a", permission: "deny" }, "guard"), { account: "a", permission: "guard" });
  // 跟随 stores nothing, so a template's own permission is not handed to the new agent.
  assert.deepEqual(configWithPermission({ account: "a", permission: "deny" }, null), { account: "a" });
  assert.deepEqual(configWithPermission(null, "bypass"), { permission: "bypass" });
  assert.deepEqual(configWithPermission({}, null), {});
});

test("Cursor cannot guard on Windows, and only there", () => {
  assert.equal(cursorCannotGuard({ plugin: "cursor" }, true), true);
  assert.equal(cursorCannotGuard({ plugin: "cursor" }, false), false);
  assert.equal(cursorCannotGuard({ plugin: "claude" }, true), false);
});
