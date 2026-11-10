import assert from "node:assert/strict";
import { test } from "node:test";
import { ROOTS_LIMIT, createFileRegistry, decodeRoots, encodeRoots, isWithin, mimeFor, rootIdOf, type RootEntry } from "./browserFiles.cjs";

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

test("saved folders read back only when their id matches and the folder still exists", () => {
  const site = "/home/u/site";
  const other = "/home/u/other";
  const entries: RootEntry[] = [
    [rootIdOf("linux", site), site],
    [rootIdOf("linux", other), other],
  ];
  const text = encodeRoots(entries);
  // Both folders are restored, in the order they were saved.
  assert.deepEqual(decodeRoots("linux", text, () => true), entries);
  // A folder that is gone is dropped.
  assert.deepEqual(decodeRoots("linux", text, (dir) => dir !== other), [entries[0]]);
  // An id that is not the hash of its folder is dropped.
  const forged = JSON.stringify({ version: 1, roots: [{ id: "0".repeat(32), dir: site }] });
  assert.deepEqual(decodeRoots("linux", forged, () => true), []);
  // A relative folder, or junk, is dropped; no file means nothing restored.
  assert.deepEqual(decodeRoots("linux", JSON.stringify({ roots: [{ id: rootIdOf("linux", "site"), dir: "site" }] }), () => true), []);
  assert.deepEqual(decodeRoots("linux", "{not json", () => true), []);
  assert.deepEqual(decodeRoots("linux", JSON.stringify({ roots: "x" }), () => true), []);
  assert.deepEqual(decodeRoots("linux", null, () => true), []);
});

test("saved folders are capped at the newest 200, the oldest go first", () => {
  const many: RootEntry[] = Array.from({ length: ROOTS_LIMIT + 5 }, (_, i) => {
    const dir = `/home/u/site${i}`;
    return [rootIdOf("linux", dir), dir];
  });
  const kept = decodeRoots("linux", encodeRoots(many), () => true);
  assert.equal(kept.length, ROOTS_LIMIT);
  assert.deepEqual(kept[0], many[5]);
  assert.deepEqual(kept[ROOTS_LIMIT - 1], many[ROOTS_LIMIT + 4]);
});

test("a restored registry serves the files of its folders again", () => {
  const dir = "/home/u/site";
  const saved = encodeRoots([[rootIdOf("linux", dir), dir]]);
  const reg = createFileRegistry("linux", decodeRoots("linux", saved, () => true));
  const root = rootIdOf("linux", dir);
  assert.equal(reg.resolve(`crossagents-file://${root}/css/a.css`).path, "/home/u/site/css/a.css");
});

test("the registry reports its folders after each registration and drops the oldest past the cap", () => {
  const seen: RootEntry[][] = [];
  const reg = createFileRegistry("linux", [], (entries) => seen.push(entries));
  reg.register("/home/u/site/index.html");
  reg.register("/home/u/other/a.html");
  assert.equal(seen.length, 2);
  assert.deepEqual(seen[1].map(([, dir]) => dir), ["/home/u/site", "/home/u/other"]);

  const capped = createFileRegistry("linux", []);
  for (let i = 0; i < ROOTS_LIMIT + 3; i++) capped.register(`/home/u/d${i}/a.html`);
  // The first folders are no longer registered; the newest still are.
  assert.throws(() => capped.resolve(`crossagents-file://${rootIdOf("linux", "/home/u/d0")}/a.html`), /没有登记/);
  assert.equal(capped.resolve(`crossagents-file://${rootIdOf("linux", `/home/u/d${ROOTS_LIMIT + 2}`)}/a.html`).root, `/home/u/d${ROOTS_LIMIT + 2}`);
});
