import assert from "node:assert/strict";
import { test } from "node:test";
import { bookmarkUrlAllowed, candidateBookmarkPaths, parseChromeBookmarks } from "./browserBookmarks.cjs";

test("parses bookmark_bar links and folders, skipping non-http schemes and empty folders", () => {
  const raw = JSON.stringify({
    roots: {
      bookmark_bar: {
        children: [
          { id: "1", type: "url", name: "GitHub", url: "https://github.com/" },
          { id: "2", type: "url", name: "Bad", url: "javascript:alert(1)" },
          { id: "3", type: "url", name: "", url: "https://docs.rs/tokio" },
          {
            id: "4",
            type: "folder",
            name: "工作",
            children: [
              { id: "5", type: "url", name: "iWiki", url: "https://iwiki.woa.com/" },
              {
                id: "6",
                type: "folder",
                name: "子目录",
                children: [{ id: "7", type: "url", name: "TAPD", url: "https://tapd.woa.com/" }],
              },
            ],
          },
          { id: "8", type: "folder", name: "空文件夹", children: [] },
        ],
      },
    },
  });
  const got = parseChromeBookmarks(raw);
  assert.deepEqual(got, [
    { kind: "url", id: "1", name: "GitHub", url: "https://github.com/" },
    { kind: "url", id: "3", name: "docs.rs", url: "https://docs.rs/tokio" },
    {
      kind: "folder",
      id: "4",
      name: "工作",
      children: [
        { kind: "url", id: "5", name: "iWiki", url: "https://iwiki.woa.com/" },
        { kind: "url", id: "7", name: "TAPD", url: "https://tapd.woa.com/" },
      ],
    },
  ]);
});

test("falls back to roots.other when bookmark_bar is empty", () => {
  const raw = JSON.stringify({
    roots: {
      bookmark_bar: { children: [] },
      other: {
        children: [{ id: "9", type: "url", name: "MDN", url: "https://developer.mozilla.org/" }],
      },
    },
  });
  assert.deepEqual(parseChromeBookmarks(raw), [
    { kind: "url", id: "9", name: "MDN", url: "https://developer.mozilla.org/" },
  ]);
  assert.deepEqual(parseChromeBookmarks("not json"), []);
});

test("only http and https bookmark URLs are allowed", () => {
  assert.equal(bookmarkUrlAllowed("https://example.com"), true);
  assert.equal(bookmarkUrlAllowed("http://localhost:3000"), true);
  assert.equal(bookmarkUrlAllowed("file:///C:/secret.txt"), false);
  assert.equal(bookmarkUrlAllowed("javascript:void(0)"), false);
});

test("candidateBookmarkPaths lists Chrome AccountBookmarks and Bookmarks before Edge on Windows and macOS", () => {
  const win = candidateBookmarkPaths("win32", { LOCALAPPDATA: "C:/Users/me/AppData/Local" }, "C:/Users/me");
  assert.ok(win[0].includes("Google") && win[0].endsWith("AccountBookmarks"));
  assert.ok(win[1].includes("Google") && win[1].endsWith("Bookmarks"));
  assert.ok(win.some((p) => p.includes("Edge")));
  const mac = candidateBookmarkPaths("darwin", {}, "/Users/me");
  assert.ok(mac[0].replace(/\\/g, "/").includes("Google/Chrome/Default/AccountBookmarks"));
});
