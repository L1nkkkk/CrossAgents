import assert from "node:assert/strict";
import { test } from "node:test";
import { restoreDocks } from "../lib/docks.ts";
import { extTabOf, extensionOfTab, filterDockTabs, isExtTab, parseExtTab } from "./tabs.ts";

// panes.ts reads __BUILD__/__MOCK__ and localStorage when it loads.
(globalThis as unknown as Record<string, unknown>).__BUILD__ = { dev: false, version: "test" };
(globalThis as unknown as Record<string, unknown>).__MOCK__ = false;
const saved = new Map<string, string>();
(globalThis as unknown as Record<string, unknown>).localStorage = {
  getItem: (key: string) => saved.get(key) ?? null,
  setItem: (key: string, value: string) => saved.set(key, value),
  removeItem: (key: string) => saved.delete(key),
  clear: () => saved.clear(),
  key: (index: number) => [...saved.keys()][index] ?? null,
  get length() {
    return saved.size;
  },
};
const panes = await import("../panes.ts");

test("an extension tab is ext:<name>/<id>", () => {
  assert.equal(extTabOf("standup", "report"), "ext:standup/report");
  assert.deepEqual(parseExtTab(extTabOf("standup", "report")), { name: "standup", id: "report" });
  assert.equal(extensionOfTab(extTabOf("standup", "report")), "standup");
  assert.ok(isExtTab("ext:standup/report"));
  assert.equal(isExtTab("ext:Standup/report"), false, "extension names are lowercase");
  assert.equal(isExtTab("ext:standup"), false, "needs an id");
  assert.equal(isExtTab("ext:standup/a/b"), false, "one id segment");
  assert.equal(isExtTab("term:abc"), false);
});

test("a saved extension tab survives load before the extension list is known", () => {
  // panes.load keeps every well-formed tab, extension tabs included; only syncExtensionTabs
  // removes one, once the hub has said the extension is disabled.
  const docks = restoreDocks(
    {
      right: { tabs: ["changes", "ext:standup/report"], active: "ext:standup/report", open: true },
      bottom: { tabs: [], active: null, open: false },
    },
    panes.isTab,
    ["changes"],
  );
  assert.deepEqual(docks.right.tabs, ["changes", "ext:standup/report"]);
  assert.equal(docks.right.active, "ext:standup/report");
});

test("only a disabled extension's tabs are removed, and a removed active tab hands over", () => {
  const dock = { tabs: ["changes", "ext:standup/report", "ext:other/x"], active: "ext:standup/report", open: true };
  const kept = filterDockTabs(dock, (tab) => !isExtTab(tab) || extensionOfTab(tab) === "standup");
  assert.deepEqual(kept.tabs, ["changes", "ext:standup/report"]);
  assert.equal(kept.active, "ext:standup/report");
  assert.equal(kept.open, true);

  // The active tab's extension went away: the first remaining one shows.
  const removed = filterDockTabs(dock, (tab) => !isExtTab(tab) || extensionOfTab(tab) !== "standup");
  assert.deepEqual(removed.tabs, ["changes", "ext:other/x"]);
  assert.equal(removed.active, "changes");

  // Nothing left: the dock folds away.
  assert.deepEqual(filterDockTabs({ tabs: ["ext:standup/report"], active: "ext:standup/report", open: true }, (tab) => !isExtTab(tab)), {
    tabs: [],
    active: null,
    open: false,
  });

  // Unchanged: the same object, so callers can skip a write.
  assert.equal(filterDockTabs(dock, () => true), dock);
});

test("a registered tab opens on the right, stays closed once closed, and reopens from the + menu", () => {
  const tab = extTabOf("standup", "report");
  panes.setExtTabLabels([{ tab, label: "站会" }]);
  assert.deepEqual(
    panes.closedExtTabs().map((entry) => entry.tab),
    [tab],
    "registered but not docked yet",
  );

  panes.offerExtTab(tab);
  assert.deepEqual(panes.closedExtTabs(), [], "the host put it in the right dock");
  assert.equal(panes.paneLabel(tab), "站会");

  panes.shutTab(tab);
  assert.deepEqual(
    panes.closedExtTabs().map((entry) => entry.tab),
    [tab],
    "closed by the person: offered in the + menu",
  );

  panes.offerExtTab(tab);
  assert.deepEqual(
    panes.closedExtTabs().map((entry) => entry.tab),
    [tab],
    "a host update must not reopen what the person closed",
  );

  panes.openExtTab(tab, "right");
  assert.deepEqual(panes.closedExtTabs(), [], "reopened from the + menu");

  // Disabling the extension takes the tab out of the menu too.
  panes.setExtTabLabels([]);
  assert.deepEqual(panes.closedExtTabs(), []);
});
