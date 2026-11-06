import assert from "node:assert/strict";
import { test } from "node:test";
import { permissionAllowed } from "./permissions.cjs";

test("the page may write to the clipboard and nothing else", () => {
  assert.equal(permissionAllowed("clipboard-sanitized-write"), true);
  for (const permission of ["clipboard-read", "media", "geolocation", "notifications", "fullscreen", "openExternal", ""]) {
    assert.equal(permissionAllowed(permission), false, permission);
  }
});
