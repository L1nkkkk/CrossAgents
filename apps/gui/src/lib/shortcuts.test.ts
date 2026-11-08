import assert from "node:assert/strict";
import { test } from "node:test";
import { shortcut } from "./shortcuts.ts";

test("shortcut formatting is platform-dependent", () => {
  assert.equal(shortcut("mac", "Ctrl+K"), "⌘K");
  assert.equal(shortcut("mac", "Ctrl+Shift+Enter"), "⇧⌘Enter");
  assert.equal(shortcut("mac", "Ctrl+Alt+I"), "⌥⌘I");
  assert.equal(shortcut("mac", "Ctrl+Alt+Shift+X", true), "⌃⌥⇧X");
  assert.equal(shortcut("mac", "Ctrl+`", true), "⌃`");
  assert.equal(shortcut("mac", "Ctrl+= / Ctrl+- / Ctrl+0"), "⌘= / ⌘- / ⌘0");
  for (const keys of ["Ctrl+Shift+Enter", "Alt+← / Alt+→", "Ctrl+= / Ctrl+- / Ctrl+0"]) assert.equal(shortcut("other", keys), keys);
});
