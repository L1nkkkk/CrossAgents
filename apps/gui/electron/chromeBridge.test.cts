import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { ChromeBridgeCore, ensureChromeExtensionFiles, parseChromeTabId } from "./chromeBridge.cjs";

test("parseChromeTabId recognizes chrome:<id> and rejects other tab ids", () => {
  assert.equal(parseChromeTabId("chrome:42"), 42);
  assert.equal(parseChromeTabId("chrome:0"), null);
  assert.equal(parseChromeTabId("web:42"), null);
  assert.equal(parseChromeTabId("42"), null);
});

test("heartbeat updates connected status, filters internal chrome:// tabs, and dispatches commands", async () => {
  const core = new ChromeBridgeCore("/tmp/ext", 7421, 2000);
  assert.equal(core.isConnected(), false);

  core.noteHeartbeat(
    [
      { id: 10, url: "chrome://extensions/", title: "Extensions", active: false },
      { id: 12, url: "https://github.com/openai/codex", title: "openai/codex", active: true },
    ],
    [],
  );
  assert.equal(core.isConnected(), true);
  assert.deepEqual(core.status().tabs, [
    { tab: "chrome:12", rawId: 12, url: "https://github.com/openai/codex", title: "openai/codex", active: true },
  ]);

  // Enqueue a snapshot action and simulate the extension's poll + reply cycle.
  const snapPromise = core.runAction({ tool: "browser_snapshot", tab: "chrome:12" });
  for (let i = 0; i < 4; i++) {
    const cmds = await core.waitForCommands(50);
    for (const cmd of cmds) {
      if (cmd.kind === "cdp" && cmd.method === "Accessibility.getFullAXTree") {
        core.noteReply({
          id: cmd.id,
          ok: true,
          value: {
            nodes: [
              {
                nodeId: "1",
                role: { type: "string", value: "button" },
                name: { type: "string", value: "Star" },
                backendDOMNodeId: 88,
              },
            ],
          },
        });
      } else {
        core.noteReply({ id: cmd.id, ok: true, value: {} });
      }
    }
  }
  const snap = (await snapPromise) as { text: string; url: string; title: string };
  assert.equal(snap.text, '- button "Star" [ref=1]');
  assert.equal(snap.url, "https://github.com/openai/codex");
});

test("ensureChromeExtensionFiles writes manifest.json, background.js, and popup.html", () => {
  const dir = mkdtempSync(join(tmpdir(), "ca-chrome-ext-"));
  try {
    ensureChromeExtensionFiles(dir, 7421);
    assert.equal(existsSync(join(dir, "manifest.json")), true);
    assert.equal(existsSync(join(dir, "background.js")), true);
    assert.equal(existsSync(join(dir, "popup.html")), true);
    const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8"));
    assert.equal(manifest.manifest_version, 3);
    assert.ok(manifest.permissions.includes("debugger"));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
