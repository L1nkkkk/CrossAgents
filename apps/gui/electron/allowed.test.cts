// Runs compiled (`npm test` builds electron/ first). Every call `src/api.ts` makes must be in
// ALLOWED: a call missing there works in `npm run dev` (no main process) and fails only in the
// installed app, which is how DELETE /v1/runs/{key} and the execution endpoints slipped by.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { ALLOWED, allowed, COMPACT_TIMEOUT_MS, REQUEST_TIMEOUT_MS, timeoutFor } from "./allowed.cjs";

/** `src/api.ts`, next to the compiled test in `dist-electron/`. */
const API_TS = join(__dirname, "..", "src", "api.ts");

/**
 * The string literal or template starting at `at`, with each `${…}` (nested templates and all)
 * replaced: by `x` where it fills a path segment (right after a `/`), by nothing elsewhere, where
 * it adds a query string or nothing: `/v1/runs/${enc(key)}?force=${f}` → `/v1/runs/x?force=`,
 * `/v1/questions${all ? "?all=true" : ""}` → `/v1/questions`.
 */
function literalAt(src: string, at: number): string {
  const quote = src[at];
  if (quote === '"' || quote === "'") {
    const end = src.indexOf(quote, at + 1);
    assert.ok(end > at, `unterminated string at ${at}`);
    return src.slice(at + 1, end);
  }
  assert.equal(quote, "`", `a path that is not a literal at ${at}: ${src.slice(at, at + 40)}`);
  let out = "";
  let i = at + 1;
  while (i < src.length && src[i] !== "`") {
    if (src[i] === "$" && src[i + 1] === "{") {
      i = skipExpression(src, i + 2);
      if (out.endsWith("/")) out += "x";
    } else {
      out += src[i++];
    }
  }
  return out;
}

/** The index just past the `}` closing an expression that starts at `i`. */
function skipExpression(src: string, i: number): number {
  let depth = 1;
  while (depth > 0) {
    assert.ok(i < src.length, "unterminated ${");
    const c = src[i];
    if (c === "{") depth++;
    else if (c === "}") depth--;
    else if (c === "`") {
      // A template inside the expression: skip it, with its own expressions.
      i++;
      while (src[i] !== "`") i = src[i] === "$" && src[i + 1] === "{" ? skipExpression(src, i + 2) : i + 1;
    } else if (c === '"' || c === "'") {
      i = src.indexOf(c, i + 1);
    }
    i++;
  }
  return i;
}

/** Every `request<T>("METHOD", path…)` in `src/api.ts`, as method and path. */
function apiCalls(): { method: string; path: string }[] {
  const src = readFileSync(API_TS, "utf8");
  const calls: { method: string; path: string }[] = [];
  const pattern = /request<[^(]*?>\(\s*"(GET|POST|DELETE)",\s*/g;
  for (let m; (m = pattern.exec(src)); ) {
    calls.push({ method: m[1], path: literalAt(src, m.index + m[0].length) });
  }
  // Every call site, so a call written in a shape the pattern misses fails here instead of
  // passing unchecked. The declaration `request<T>(method: …)` is not a call.
  const sites = (src.match(/request<[^(]*?>\(\s*"/g) ?? []).length;
  assert.equal(calls.length, sites, "a call in src/api.ts was not read");
  return calls;
}

test("every hub call src/api.ts makes is in ALLOWED", () => {
  const calls = apiCalls();
  assert.ok(calls.length >= 40, `only ${calls.length} calls found in src/api.ts`);
  const missing = calls.filter((c) => !allowed(c.method, c.path)).map((c) => `${c.method} ${c.path}`);
  assert.deepEqual(missing, [], "add these to ALLOWED in electron/allowed.cts");
});

test("the S1 endpoints pass, query strings included", () => {
  assert.ok(allowed("DELETE", "/v1/runs/0193-abc?branches=true&force=false"));
  assert.ok(allowed("GET", "/v1/agents/Rex/execution"));
  assert.ok(allowed("POST", "/v1/agents/Rex/execution/interrupt"));
  assert.ok(allowed("GET", "/v1/agents/Rex/resume-command"));
});

test("the files and graph endpoints pass for every scope", () => {
  assert.ok(allowed("GET", "/v1/agents/Rex/files/tree?path=src&all=true"));
  assert.ok(allowed("GET", "/v1/runs/r1/files/content?path=a.rs"));
  assert.ok(allowed("GET", "/v1/runs/r1/tasks/auth/files/tree?path="));
  assert.ok(allowed("POST", "/v1/runs/r1/tasks/auth/files/resolve"));
  assert.ok(allowed("POST", "/v1/agents/Rex/files/resolve"));
  assert.ok(!allowed("POST", "/v1/agents/Rex/files/content"));
  assert.ok(!allowed("GET", "/v1/files/tree"));
  assert.ok(allowed("GET", "/v1/runs/r1/graph?limit=50"));
  assert.ok(allowed("GET", "/v1/agents/Rex/graph"));
  assert.ok(allowed("GET", "/v1/runs/r1/commits/abc123/diff"));
  assert.ok(allowed("GET", "/v1/agents/Rex/commits/abc123/diff"));
});

test("the extension endpoints the page uses pass, and nothing wider", () => {
  assert.ok(allowed("GET", "/v1/extensions"));
  assert.ok(allowed("GET", "/v1/extensions/standup/logs"));
  assert.ok(allowed("GET", "/v1/extensions/standup/pending/diff"));
  assert.ok(allowed("POST", "/v1/extensions/standup/enable"));
  assert.ok(allowed("POST", "/v1/extensions/standup/disable"));
  assert.ok(allowed("POST", "/v1/extensions/standup/auto-trust"));
  assert.ok(allowed("POST", "/v1/extensions/standup/tools/report"));
  assert.ok(allowed("GET", "/v1/extensions/calls/abc"));
  // Logging is read-only, and no route under /v1/extensions takes anything else.
  assert.ok(!allowed("POST", "/v1/extensions/standup/logs"));
  assert.ok(!allowed("DELETE", "/v1/extensions/standup/logs"));
  assert.ok(!allowed("GET", "/v1/extensions/standup/logs/old"));
  assert.ok(!allowed("GET", "/v1/extensions/standup/enable"));
});

test("the device endpoints the page uses pass, and issuing stays refused", () => {
  assert.ok(allowed("GET", "/v1/devices"));
  assert.ok(allowed("DELETE", "/v1/devices/0193-abc"));
  // This round the GUI never issues: adding a device is `crossagents device add` (docs/API-AUTH.md §4).
  assert.ok(!allowed("POST", "/v1/devices"));
  assert.ok(!allowed("DELETE", "/v1/devices"));
  assert.ok(!allowed("PATCH", "/v1/devices/0193-abc"));
});

test("what the GUI never calls stays refused", () => {
  assert.ok(!allowed("POST", "/v1/shutdown"));
  assert.ok(!allowed("GET", "/v1/me"));
  assert.ok(!allowed("POST", "/v1/me/tools/task_complete"));
  assert.ok(!allowed("DELETE", "/v1/runs/x/tasks/y"));
  assert.ok(!allowed("GET", "/v1/agents/x/execution/interrupt"));
});

test("compacting gets minutes, everything else the usual timeout", () => {
  // A 600k-token history took 148 s to summarise; at 30 s the GUI gave up halfway.
  assert.equal(timeoutFor("POST", "/v1/agents/Ravi/compact"), COMPACT_TIMEOUT_MS);
  assert.equal(timeoutFor("POST", "/v1/agents/Ravi/messages"), REQUEST_TIMEOUT_MS);
  assert.equal(timeoutFor("GET", "/v1/agents/Ravi/compact"), REQUEST_TIMEOUT_MS);
});

test("settings and repo initialization require admin while runs remain write", () => {
  for (const [path, level] of [["/v1/agents/Rex/settings", "admin"], ["/v1/repos/init", "admin"], ["/v1/runs", "write"], ["/v1/agents/Rex/connect", "write"]]) {
    const entries = ALLOWED.filter((r) => r.method === "POST" && r.path.test(path));
    assert.equal(entries.length, 1, path);
    assert.equal(entries[0].level, level, path);
    assert.ok(allowed("POST", path));
  }
});
