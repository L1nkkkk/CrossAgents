import assert from "node:assert/strict";
import { test } from "node:test";
import type { ExtensionCallResponse } from "@bindings/ExtensionCallResponse";
import type { ExtensionCallStatus } from "@bindings/ExtensionCallStatus";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionVersion } from "@bindings/ExtensionVersion";
import type { HubEvent } from "../hub";
import { createExtensionHost, extensionEntryUrl, type ExtensionHostOptions, type ExtensionModule } from "./host.ts";

const version = (fingerprint: string): ExtensionVersion => ({
  fingerprint,
  api: 1,
  description: "",
  access: "read",
  tools: [],
  prompts: [],
  gui_entry: "gui/index.js",
  files: 1,
  bytes: 1,
  taken_at: "2026-01-01T00:00:00Z",
});

const info = (name: string, fingerprint: string, enabled = true): ExtensionInfo => ({
  name,
  source: `/home/extensions/${name}`,
  enabled,
  auto_trust: false,
  current: enabled ? version(fingerprint) : undefined,
});

function harness(initial: ExtensionInfo[], modules: Map<string, ExtensionModule>) {
  let list = initial;
  let callStatus: ExtensionCallStatus = "running";
  let listCalls = 0;
  const events = new Set<(event: HubEvent) => void>();
  const imported: string[] = [];
  const counters = { calls: 0, cancels: 0 };
  const response = (status: ExtensionCallStatus): ExtensionCallResponse => ({
    call_id: "c1",
    extension: "a",
    tool: "report",
    fingerprint: "fp1",
    status,
    started_at: "2026-01-01T00:00:00Z",
  });

  const options: ExtensionHostOptions = {
    list: async () => {
      listCalls++;
      return list;
    },
    importModule: async (url) => {
      imported.push(url);
      const module = modules.get(url);
      if (!module) throw new Error(`no module at ${url}`);
      return module;
    },
    request: async () => ({}),
    callTool: async () => {
      counters.calls++;
      return { call_id: "c1", extension: "a", tool: "report", fingerprint: "fp1", started_at: "2026-01-01T00:00:00Z", timeout_secs: 60 };
    },
    getCall: async () => response(callStatus),
    cancelCall: async () => {
      counters.cancels++;
      callStatus = "cancelled";
      return response("cancelled");
    },
    disable: async () => ({}),
    events: (listener) => {
      events.add(listener);
      return () => events.delete(listener);
    },
    timeoutMs: 60,
    pollMs: 5,
    log: () => {},
  };

  const host = createExtensionHost(options);
  return {
    host,
    events,
    imported,
    counters,
    listCalls: () => listCalls,
    setList: (next: ExtensionInfo[]) => {
      list = next;
    },
    setCallStatus: (next: ExtensionCallStatus) => {
      callStatus = next;
    },
    emitChanged: (name: string) => {
      const event = { type: "extension.changed", data: { name } } as unknown as HubEvent;
      for (const listener of events) listener(event);
    },
  };
}

async function until(check: () => boolean, label: string, ms = 1000) {
  const start = Date.now();
  while (!check()) {
    if (Date.now() - start > ms) throw new Error(`timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}

test("an activate that throws halfway leaves nothing behind, and other extensions load", async () => {
  const a = extensionEntryUrl("a", "fp1", "gui/index.js");
  const b = extensionEntryUrl("b", "fp1", "gui/index.js");
  const modules = new Map<string, ExtensionModule>([
    [
      a,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "面板", render: () => null });
          ca.toolCards.register("report", () => null);
          ca.events.on("run.finished", () => {});
          ca.styles.add("./panel.css");
          throw new Error("halfway");
        },
      },
    ],
    [
      b,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "B 的面板", render: () => null });
        },
      },
    ],
  ]);
  const run = harness([info("a", "fp1"), info("b", "fp1")], modules);
  await run.host.start();

  const snapshot = run.host.snapshot();
  const failed = snapshot.records.get("a")!;
  assert.equal(failed.status, "failed");
  assert.match(failed.error ?? "", /halfway/);
  assert.ok(failed.logs.some((line) => line.includes("加载失败")));
  assert.deepEqual([...snapshot.tabs.keys()], ["ext:b/panel"]);
  assert.equal(snapshot.cards.size, 0);
  assert.equal(snapshot.records.get("b")!.status, "ready");
  // The host's own event subscription is the only one left.
  assert.equal(run.events.size, 1);
});

test("a version switch cleans the old registrations first, then imports the new entry", async () => {
  const fp1 = extensionEntryUrl("a", "fp1", "gui/index.js");
  const fp2 = extensionEntryUrl("a", "fp2", "gui/index.js");
  let ownCleanups = 0;
  const modules = new Map<string, ExtensionModule>([
    [
      fp1,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "v1", render: () => null });
          ca.toolCards.register("report", () => null);
          return () => {
            ownCleanups++;
          };
        },
      },
    ],
    [
      fp2,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "v2", render: () => null });
        },
      },
    ],
  ]);
  const run = harness([info("a", "fp1")], modules);
  await run.host.start();
  assert.equal(run.host.snapshot().tabs.get("ext:a/panel")!.label, "v1");
  assert.equal(run.host.snapshot().cards.size, 2, "bare name and qualified alias");

  run.setList([info("a", "fp2")]);
  run.emitChanged("a");
  await until(() => run.host.snapshot().records.get("a")?.status === "ready" && run.host.snapshot().records.get("a")?.fingerprint === "fp2", "the new version");

  const snapshot = run.host.snapshot();
  assert.equal(snapshot.tabs.get("ext:a/panel")!.label, "v2");
  assert.equal(snapshot.tabs.size, 1, "the old tab was removed, not duplicated");
  assert.equal(snapshot.cards.size, 0, "a card only the old version registered is gone");
  assert.equal(ownCleanups, 1, "the extension's own cleanup ran");
  assert.deepEqual(run.imported, [fp1, fp2]);
});

test("an activate that never settles times out and is cleaned up", async () => {
  const url = extensionEntryUrl("a", "fp1", "gui/index.js");
  const modules = new Map<string, ExtensionModule>([
    [
      url,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "面板", render: () => null });
          return new Promise(() => {});
        },
      },
    ],
  ]);
  const run = harness([info("a", "fp1")], modules);
  await run.host.start();
  const record = run.host.snapshot().records.get("a")!;
  assert.equal(record.status, "failed");
  assert.match(record.error ?? "", /超时/);
  assert.equal(run.host.snapshot().tabs.size, 0);
});

test("safe mode loads nothing at all", async () => {
  let listCalls = 0;
  let loading = 0;
  let ready = 0;
  const host = createExtensionHost({
    list: async () => {
      listCalls++;
      return [info("a", "fp1")];
    },
    importModule: async () => {
      throw new Error("must not import in safe mode");
    },
    request: async () => ({}),
    callTool: async () => {
      throw new Error("unused");
    },
    getCall: async () => {
      throw new Error("unused");
    },
    cancelCall: async () => ({}),
    disable: async () => ({}),
    safeMode: () => true,
    onLoading: () => loading++,
    onReady: () => ready++,
    log: () => {},
  });
  await host.start();
  assert.equal(listCalls, 0);
  assert.equal(loading, 0);
  assert.equal(ready, 0);
  assert.equal(host.snapshot().safeMode, true);
  assert.equal(host.snapshot().tabs.size, 0);
});

test("ca.tools.call can be cancelled and is not retried", async () => {
  const url = extensionEntryUrl("a", "fp1", "gui/index.js");
  let ca!: Parameters<NonNullable<ExtensionModule["activate"]>>[0];
  const modules = new Map<string, ExtensionModule>([
    [
      url,
      {
        activate: (given) => {
          ca = given;
        },
      },
    ],
  ]);
  const run = harness([info("a", "fp1")], modules);
  await run.host.start();

  const call = ca.tools.call("report", { days: 1 });
  call.cancel();
  await assert.rejects(call, (error: unknown) => error instanceof Error && /取消/.test(error.message));
  assert.equal(run.counters.calls, 1, "the POST happened once");
  assert.equal(run.counters.cancels, 1, "the DELETE happened once");
});

test("ca.tools.call resolves with the result the hub stored", async () => {
  const url = extensionEntryUrl("a", "fp1", "gui/index.js");
  let ca!: Parameters<NonNullable<ExtensionModule["activate"]>>[0];
  const modules = new Map<string, ExtensionModule>([
    [
      url,
      {
        activate: (given) => {
          ca = given;
        },
      },
    ],
  ]);
  const run = harness([info("a", "fp1")], modules);
  await run.host.start();
  run.setCallStatus("ok");

  const result = await ca.tools.call("report");
  assert.equal(result, null);
  assert.equal(run.counters.calls, 1);
});
