import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { dictionaries, hasKey, missingKeys, setLocale, t } from "./i18n.ts";

/** Files that take their strings from the dictionary. */
const MIGRATED = ["../Sidebar.tsx", "../views/Home.tsx", "../strips.tsx"];

/** Every literal key passed to `t(...)` in `file`. */
function keysIn(file: string): string[] {
  const src = readFileSync(new URL(file, import.meta.url), "utf8");
  return [...src.matchAll(/\bt\(\s*"([^"]+)"/g)].map((m) => m[1]);
}

test("every key the migrated files use is in the dictionary", () => {
  for (const file of MIGRATED) {
    const keys = keysIn(file);
    assert.ok(keys.length > 0, `${file} uses no keys`);
    const missing = keys.filter((k) => !hasKey(k));
    assert.deepEqual(missing, [], `${file} uses keys the dictionary lacks`);
  }
});

test("every locale has every key", () => {
  for (const [name, dict] of Object.entries(dictionaries)) {
    assert.deepEqual(missingKeys(dict), [], `${name} is missing keys`);
  }
  assert.deepEqual(missingKeys({ "common.clear": "Clear" }).includes("common.more"), true);
});

test("placeholders are filled, unknown ones are left alone", () => {
  assert.equal(t("common.more", { n: 3 }), "更多 (3)");
  assert.equal(t("sidebar.merged", { merged: 1, total: 4 }), "1/4 已合并");
  assert.equal(t("hub.offline", {}), "未连接：{reason}");
  assert.equal(t("nav.team"), "队友");
});

test("an unknown locale falls back to zh-CN", () => {
  setLocale("xx");
  assert.equal(t("nav.usage"), "用量");
  setLocale("zh-CN");
});

test("terms stay untranslated", () => {
  assert.match(t("home.start"), /run/);
  assert.match(t("home.worker"), /Worker$/);
});
