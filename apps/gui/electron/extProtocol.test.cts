import assert from "node:assert/strict";
import { test } from "node:test";
import { assetPath } from "./extProtocol.cjs";
const url = (path: string) => `crossagents-ext://example/abc123/${path}`;
test("builds hub asset URL without query or fragment", () => {
  assert.equal(assetPath(url("gui/a%20b.js?v=1#x")), "/v1/extensions/example/assets/abc123/gui/a%20b.js");
  assert.equal(assetPath(url("%E4%B8%AD.js")), "/v1/extensions/example/assets/abc123/%E4%B8%AD.js");
});
test("refuses invalid URLs and traversal before normalization", () => {
  for (const path of ["../secret", "gui/%2E%2E/secret", "%2e%2e%2fsecret", "%2e%2e%5csecret", "/etc", "%2fetc", "C:secret", "gui/a:stream", "%00", "%zz", "a//b", ". /../x"]) assert.throws(() => assetPath(url(path)), path);
  for (const value of ["file:///secret", "crossagents-ext://../abc/file", "crossagents-ext://example/%2e%2e/file", "crossagents-ext://example:90/abc/file", "crossagents-ext://user@example/abc/file"]) assert.throws(() => assetPath(value), value);
});
