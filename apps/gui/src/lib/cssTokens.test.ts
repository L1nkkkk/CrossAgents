import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const SRC_ROOT = fileURLToPath(new URL("..", import.meta.url));

/** Layout custom properties injected dynamically via inline styles rather than `:root`. */
const DYNAMIC_VARS = new Set([
  "--sidebar-width",
  "--tools-width",
  "--bottom-height",
  "--cols",
]);

function walkCss(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const full = join(dir, name);
    const stat = statSync(full);
    if (stat.isDirectory()) walkCss(full, out);
    else if (name.endsWith(".css")) out.push(full);
  }
  return out;
}

test("every CSS var(--token) referenced without fallback is defined in the stylesheet tokens", () => {
  const files = walkCss(SRC_ROOT);
  const defined = new Set<string>(DYNAMIC_VARS);
  for (const file of files) {
    const css = readFileSync(file, "utf8");
    for (const m of css.matchAll(/(--[a-zA-Z0-9_-]+)\s*:/g)) {
      defined.add(m[1]);
    }
  }

  const missing: string[] = [];
  for (const file of files) {
    const rel = file.slice(SRC_ROOT.length + 1).replace(/\\/g, "/");
    const lines = readFileSync(file, "utf8").split(/\r?\n/);
    lines.forEach((line, idx) => {
      for (const m of line.matchAll(/var\(\s*(--[a-zA-Z0-9_-]+)\s*(,)?/g)) {
        const token = m[1];
        const hasFallback = Boolean(m[2]);
        if (!defined.has(token) && !hasFallback) {
          missing.push(`${rel}:${idx + 1} references undefined ${token}`);
        }
      }
    });
  }

  assert.deepEqual(
    missing,
    [],
    `Found undefined CSS tokens without fallback (can cause transparent modals/broken borders):\n${missing.join("\n")}`,
  );
});
