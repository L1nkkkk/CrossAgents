import assert from "node:assert/strict";
import { test } from "node:test";
import { createFileRegistry, isWithin, mimeFor } from "./browserFiles.cjs";

const posixReg = () => createFileRegistry("linux");

test("a file's URL names its folder, and relative files in that folder load", () => {
  const reg = posixReg();
  const url = reg.register("/home/u/site/index.html");
  assert.match(url, /^crossagents-file:\/\/[0-9a-f]{32}\/index\.html$/);
  const root = url.slice("crossagents-file://".length, "crossagents-file://".length + 32);
  const css = reg.resolve(`crossagents-file://${root}/css/site%20main.css?v=2#x`);
  assert.equal(css.root, "/home/u/site");
  assert.equal(css.path, "/home/u/site/css/site main.css");
  assert.equal(reg.resolve(`crossagents-file://${root}/img/a.png`).path, "/home/u/site/img/a.png");
  // The same folder gives the same root, whichever file of it was registered.
  assert.equal(reg.register("/home/u/site/other.html").split("/")[2], root);
});

test("traversal and encoded traversal are refused", () => {
  const reg = posixReg();
  const root = reg.register("/home/u/site/index.html").split("/")[2];
  // The URL parser folds dot segments itself, so these land inside the root instead of escaping it.
  for (const path of ["../secret", "css/../../secret", "%2e%2e/secret"]) {
    assert.ok(reg.resolve(`crossagents-file://${root}/${path}`).path.startsWith("/home/u/site/"), path);
  }
  // Encoded separators and the rest are refused outright.
  for (const path of ["%2E%2E%2Fsecret", "a%5c..%5cb", "a%2Fb", "%00", "%zz", "", "/"]) {
    assert.throws(() => reg.resolve(`crossagents-file://${root}/${path}`), path);
  }
  assert.throws(() => reg.resolve(`crossagents-file://${root}`));
  assert.throws(() => reg.resolve(`crossagents-file://${root}//etc/passwd`));
  // A sibling folder that shares the prefix is not inside the root.
  assert.equal(isWithin("linux", "/home/u/site", "/home/u/site-evil/x"), false);
});

test("unknown roots, other schemes and relative paths are refused", () => {
  const reg = posixReg();
  reg.register("/home/u/site/index.html");
  assert.throws(() => reg.resolve("crossagents-file://00000000000000000000000000000000/a.png"), /没有登记/);
  assert.throws(() => reg.resolve("file:///home/u/site/index.html"));
  assert.throws(() => reg.resolve("crossagents-ext://x/y"));
  assert.throws(() => reg.register("site/index.html"));
  assert.throws(() => reg.register("/"));
});

test("Windows: drive letters, separators and case", () => {
  const reg = createFileRegistry("win32");
  const url = reg.register("C:\\Users\\u\\Site\\index.html");
  const root = url.split("/")[2];
  // Case folding gives the same root for the same folder.
  assert.equal(reg.register("c:\\users\\u\\site\\other.html").split("/")[2], root);
  assert.equal(reg.resolve(`crossagents-file://${root}/CSS/A.css`).path, "C:\\Users\\u\\Site\\CSS\\A.css");
  for (const path of ["..%5C..%5Cwindows", "D%3A%5Csecret", "C%3Afoo", "..%2fsecret", "a%5Cb"]) {
    assert.throws(() => reg.resolve(`crossagents-file://${root}/${path}`), path);
  }
  assert.equal(isWithin("win32", "C:\\a", "c:\\A\\b.png"), true);
  assert.equal(isWithin("win32", "C:\\a", "C:\\ab\\x.png"), false);
  assert.equal(isWithin("win32", "C:\\a", "D:\\a\\x.png"), false);
  assert.equal(isWithin("win32", "C:\\a", "C:\\a"), false);
});

test("a served file's type follows its extension", () => {
  assert.equal(mimeFor("/x/a.html"), "text/html; charset=utf-8");
  assert.equal(mimeFor("/x/a.CSS"), "text/css; charset=utf-8");
  assert.equal(mimeFor("/x/app.js"), "text/javascript; charset=utf-8");
  assert.equal(mimeFor("/x/logo.png"), "image/png");
  assert.equal(mimeFor("/x.d/logo"), "application/octet-stream");
  assert.equal(mimeFor("C:\\x.d\\logo.webp"), "image/webp");
  assert.equal(mimeFor("/x/data.bin"), "application/octet-stream");
});
