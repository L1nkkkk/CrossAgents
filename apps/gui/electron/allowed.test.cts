// Runs compiled (`npm test` builds electron/ first). Every call `src/api.ts` makes must be in
// ALLOWED: a call missing there works in `npm run dev` (no main process) and fails only in the
// installed app, which is how DELETE /v1/runs/{key} and the execution endpoints slipped by.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { allowed } from "./allowed.cjs";

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

test("what the GUI never calls stays refused", () => {
  assert.ok(!allowed("POST", "/v1/shutdown"));
  assert.ok(!allowed("GET", "/v1/me"));
  assert.ok(!allowed("POST", "/v1/me/tools/task_complete"));
  assert.ok(!allowed("DELETE", "/v1/runs/x/tasks/y"));
  assert.ok(!allowed("GET", "/v1/agents/x/execution/interrupt"));
});
