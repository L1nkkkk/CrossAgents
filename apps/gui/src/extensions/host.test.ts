import assert from "node:assert/strict";
import { test } from "node:test";
import type { ExtensionCallResponse } from "@bindings/ExtensionCallResponse";
import type { ExtensionCallStatus } from "@bindings/ExtensionCallStatus";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionVersion } from "@bindings/ExtensionVersion";
import type { HubEvent } from "../hub";
import { createExtensionHost, detectSafeMode, extensionEntryUrl, readyTabs,type ExtensionHostOptions, type ExtensionModule } from "./host.ts";

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
  let callError: number | undefined;
  let listCalls = 0;
  const logs: string[] = [];
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
    getCall: async () => {
      if (callError !== undefined) throw Object.assign(new Error(`HTTP ${callError}`), { status: callError });
      return response(callStatus);
    },
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
    retryMs: 5,
    log: (line) => logs.push(line),
  };

  const host = createExtensionHost(options);
  return {
    host,
    events,
    imported,
    counters,
    logs,
    listCalls: () => listCalls,
    setList: (next: ExtensionInfo[]) => {
      list = next;
    },
    setCallStatus: (next: ExtensionCallStatus) => {
      callStatus = next;
    },
    setCallError: (status?: number) => {
      callError = status;
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
  let duringActivate: string[] | undefined;
  const modules = new Map<string, ExtensionModule>([
    [
      a,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "面板", render: () => null });
          ca.toolCards.register("report", () => null);
          ca.events.on("run.finished", () => {});
          ca.styles.add("./panel.css");
          duringActivate = readyTabs(run.host.snapshot());
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
  // A tab registered before the throw is never offered to a dock, which would outlive the load.
  assert.deepEqual(duringActivate, []);
  assert.deepEqual(readyTabs(snapshot), ["ext:b/panel"]);
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
  assert.equal(run.host.snapshot().cards.size, 1, "the bare name is stored qualified");

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
    onLoading: () => {
      loading++;
    },
    onReady: () => {
      ready++;
    },
    log: () => {},
  });
  await host.start();
  assert.equal(listCalls, 0);
  assert.equal(loading, 0);
  assert.equal(ready, 1, "safe mode still clears the last run's crash marker");
  assert.equal(host.snapshot().safeMode, true);
  assert.equal(host.snapshot().tabs.size, 0);
});

test("the shell's loading marker is awaited before anything is imported", async () => {
  let release!: () => void;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let imports = 0;
  const host = createExtensionHost({
    list: async () => [info("a", "fp1")],
    importModule: async () => {
      imports++;
      return { activate: () => {} };
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
    onLoading: () => gate,
    log: () => {},
  });
  const started = host.start();
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(imports, 0, "nothing was imported while the marker was being set");
  release();
  await started;
  assert.equal(imports, 1);
  assert.equal(host.snapshot().records.get("a")!.status, "ready");
});

test("safe mode is read from the shell's awaited answer", async () => {
  const globals = globalThis as unknown as Record<string, unknown>;
  globals.window = {
    location: { search: "" },
    crossagents: { extensions: { safeMode: async () => ({ enabled: true, reason: "上次崩溃" }) } },
  };
  assert.equal(await detectSafeMode(), true);
  globals.window = {
    location: { search: "" },
    crossagents: { extensions: { safeMode: async () => ({ enabled: false, reason: null }) } },
  };
  assert.equal(await detectSafeMode(), false);
  delete globals.window;
});

test("a list that fails at first is retried until the hub answers, then loading is reported", async () => {
  let listCalls = 0;
  const order: string[] = [];
  const host = createExtensionHost({
    list: async () => {
      listCalls++;
      order.push("list");
      if (listCalls < 3) throw new Error("hub down");
      return [info("a", "fp1")];
    },
    importModule: async () => {
      order.push("import");
      return { activate: () => { order.push("activate"); } };
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
    retryMs: 2,
    onLoading: () => {
      order.push("loading");
    },
    onReady: () => {
      order.push("ready");
    },
    log: () => {},
  });
  await host.start();
  assert.equal(listCalls, 3);
  assert.deepEqual(order, ["list", "list", "list", "loading", "import", "activate", "ready"]);
  assert.equal(host.snapshot().records.get("a")!.status, "ready");
  host.dispose();
});

test("nothing with a gui_entry: ready without a loading marker", async () => {
  const headless: ExtensionInfo = { ...info("a", "fp1"), current: { ...version("fp1"), gui_entry: undefined } };
  let loading = 0;
  let ready = 0;
  let imports = 0;
  const host = createExtensionHost({
    list: async () => [headless],
    importModule: async () => {
      imports++;
      return {};
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
    onLoading: () => {
      loading++;
    },
    onReady: () => {
      ready++;
    },
    log: () => {},
  });
  await host.start();
  assert.equal(imports, 0);
  assert.equal(loading, 0);
  assert.equal(ready, 1, "the last run's marker is still cleared");
  host.dispose();
});

/** A host that records the shell's marker calls in `order`, with an optional loading gate. */
function marked(initial: ExtensionInfo[], modules: Map<string, ExtensionModule>, timeoutMs = 60) {
  let list = initial;
  const order: string[] = [];
  let gate: Promise<void> | undefined;
  const host = createExtensionHost({
    list: async () => list,
    importModule: async (url) => {
      order.push(`import ${url}${host.snapshot().loading ? " with marker" : ""}`);
      const module = modules.get(url);
      if (!module) throw new Error(`no module at ${url}`);
      return module;
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
    timeoutMs,
    onLoading: () => {
      order.push("loading");
      return gate;
    },
    onReady: () => {
      order.push("ready");
    },
    log: () => {},
  });
  return {
    host,
    order,
    setList: (next: ExtensionInfo[]) => {
      list = next;
    },
    clearOrder: () => {
      order.length = 0;
    },
    /** Holds the next `onLoading` until the returned function is called. */
    holdLoading: () => {
      let release!: () => void;
      gate = new Promise<void>((resolve) => {
        release = resolve;
      });
      return () => release();
    },
  };
}

test("a version switch after startup marks the load, then clears the marker", async () => {
  const fp1 = extensionEntryUrl("a", "fp1", "gui/index.js");
  const fp2 = extensionEntryUrl("a", "fp2", "gui/index.js");
  const modules = new Map<string, ExtensionModule>([
    [fp1, { activate: () => {} }],
    [fp2, { activate: () => {} }],
  ]);
  const run = marked([info("a", "fp1")], modules);
  await run.host.start();
  assert.deepEqual(run.order, ["loading", `import ${fp1} with marker`, "ready"]);

  run.clearOrder();
  run.setList([info("a", "fp2")]);
  const release = run.holdLoading();
  const hot = run.host.refresh();
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(run.order, ["loading"], "the new entry is not imported while the marker is being set");

  release();
  await hot;
  assert.deepEqual(run.order, ["loading", `import ${fp2} with marker`, "ready"]);
  const snapshot = run.host.snapshot();
  assert.equal(snapshot.loading, false);
  assert.equal(snapshot.records.get("a")!.status, "ready");
  assert.equal(snapshot.records.get("a")!.fingerprint, "fp2");
  run.host.dispose();
});

test("a hot load that fails still clears the marker", async () => {
  const fp1 = extensionEntryUrl("a", "fp1", "gui/index.js");
  const fp2 = extensionEntryUrl("a", "fp2", "gui/index.js");
  const modules = new Map<string, ExtensionModule>([[fp1, { activate: () => {} }]]);
  const run = marked([info("a", "fp1")], modules);
  await run.host.start();

  run.clearOrder();
  run.setList([info("a", "fp2")]);
  await run.host.refresh();
  assert.deepEqual(run.order, ["loading", `import ${fp2} with marker`, "ready"]);
  assert.equal(run.host.snapshot().records.get("a")!.status, "failed");
  assert.equal(run.host.snapshot().loading, false);
  run.host.dispose();
});

test("a refresh with nothing to import calls neither marker", async () => {
  const fp1 = extensionEntryUrl("a", "fp1", "gui/index.js");
  const modules = new Map<string, ExtensionModule>([[fp1, { activate: () => {} }]]);
  const run = marked([info("a", "fp1")], modules);
  await run.host.start();

  run.clearOrder();
  // Same fingerprint: nothing to import.
  await run.host.refresh();
  assert.deepEqual(run.order, []);

  // Disabled: registrations go away, but no entry is imported.
  run.setList([info("a", "fp1", false)]);
  await run.host.refresh();
  assert.deepEqual(run.order, []);
  assert.equal(run.host.snapshot().records.get("a")!.status, "disabled");
  run.host.dispose();
});

test("a change during startup waits for the first load: the marker stays and B activates once", async () => {
  const aUrl = extensionEntryUrl("a", "fp1", "gui/index.js");
  const bUrl = extensionEntryUrl("b", "fp1", "gui/index.js");
  let bActivations = 0;
  let releaseB!: () => void;
  const bGate = new Promise<void>((resolve) => {
    releaseB = resolve;
  });
  const modules = new Map<string, ExtensionModule>([
    [
      aUrl,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "A", render: () => null });
        },
      },
    ],
    [
      bUrl,
      {
        activate: () => {
          bActivations++;
          return bGate;
        },
      },
    ],
  ]);
  const run = marked([info("a", "fp1"), info("b", "fp1")], modules, 500);
  const starting = run.host.start();
  // Startup is inside B's activate, and the marker is up.
  await until(() => bActivations === 1, "B's first activate");
  assert.equal(run.host.snapshot().loading, true);
  assert.deepEqual(run.order, ["loading", `import ${aUrl} with marker`, `import ${bUrl} with marker`]);

  // Disabling A now queues a refresh behind startup, which is still waiting on B.
  run.setList([info("a", "fp1", false), info("b", "fp1")]);
  const disabling = run.host.disable("a");
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.ok(!run.order.includes("ready"), "the marker stays up until startup settled");
  assert.equal(bActivations, 1, "the queued refresh did not activate B a second time");

  releaseB();
  await disabling;
  await starting;

  assert.equal(bActivations, 1, "B was activated exactly once");
  assert.deepEqual(run.order, ["loading", `import ${aUrl} with marker`, `import ${bUrl} with marker`, "ready"]);
  const snapshot = run.host.snapshot();
  assert.equal(snapshot.loading, false);
  assert.equal(snapshot.records.get("a")!.status, "disabled");
  assert.equal(snapshot.records.get("b")!.status, "ready");
  run.host.dispose();
});

test("an activate that never settles times out, so a disable queued behind startup still completes", async () => {
  const aUrl = extensionEntryUrl("a", "fp1", "gui/index.js");
  const bUrl = extensionEntryUrl("b", "fp1", "gui/index.js");
  let bActivations = 0;
  const modules = new Map<string, ExtensionModule>([
    [
      aUrl,
      {
        activate: (ca) => {
          ca.tabs.register({ id: "panel", label: "A", render: () => null });
        },
      },
    ],
    [
      bUrl,
      {
        activate: () => {
          bActivations++;
          return new Promise<void>(() => {});
        },
      },
    ],
  ]);
  const run = marked([info("a", "fp1"), info("b", "fp1")], modules, 40);
  const starting = run.host.start();
  await until(() => bActivations === 1, "B's first activate");

  run.setList([info("a", "fp1", false), info("b", "fp1")]);
  await run.host.disable("a");
  await starting;

  const snapshot = run.host.snapshot();
  assert.equal(snapshot.records.get("a")!.status, "disabled");
  assert.equal(snapshot.records.get("b")!.status, "failed");
  assert.equal(snapshot.loading, false, "the marker is cleared after the timed-out load");
  assert.equal(run.order[run.order.length - 1], "ready");
  run.host.dispose();
});

test("a lost call result (404) rejects instead of polling forever", async () => {
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
  run.setCallError(404);
  await assert.rejects(ca.tools.call("report"), /调用结果已丢失/);
  assert.equal(run.counters.calls, 1);
  run.host.dispose();
});

test("a call result failing with another error keeps polling", async () => {
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
  run.setCallError(500);
  const call = ca.tools.call("report");
  await new Promise((resolve) => setTimeout(resolve, 30));
  run.setCallError(undefined);
  run.setCallStatus("ok");
  await call;
  assert.equal(run.counters.calls, 1);
  run.host.dispose();
});

test("a cleanup returned after activate timed out is run, not dropped", async () => {
  const url = extensionEntryUrl("a", "fp1", "gui/index.js");
  let release!: (cleanup: () => void) => void;
  const late = new Promise<() => void>((resolve) => {
    release = resolve;
  });
  const modules = new Map<string, ExtensionModule>([
    [
      url,
      {
        activate: () => late,
      },
    ],
  ]);
  const run = harness([info("a", "fp1")], modules);
  await run.host.start();
  assert.equal(run.host.snapshot().records.get("a")!.status, "failed");
  release(() => {
    throw new Error("late cleanup");
  });
  await new Promise((resolve) => setTimeout(resolve, 0));
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.ok(
    run.logs.some((line) => line.includes("超时后返回的清理函数出错")),
    run.logs.join("\n"),
  );
  run.host.dispose();
});

test("a v1 extension can only register cards for its own tools", async () => {
  const url = extensionEntryUrl("a", "fp1", "gui/index.js");
  const foreign = new Map<string, ExtensionModule>([
    [
      url,
      {
        activate: (ca) => {
          ca.toolCards.register("b__report", () => null);
        },
      },
    ],
  ]);
  const rejected = harness([info("a", "fp1")], foreign);
  await rejected.host.start();
  const record = rejected.host.snapshot().records.get("a")!;
  assert.equal(record.status, "failed");
  assert.match(record.error ?? "", /只能给自己的工具注册卡片/);
  assert.equal(rejected.host.snapshot().cards.size, 0);

  const own = new Map<string, ExtensionModule>([
    [
      url,
      {
        activate: (ca) => {
          ca.toolCards.register("report", () => null);
          ca.toolCards.register("a__other", () => null);
        },
      },
    ],
  ]);
  const accepted = harness([info("a", "fp1")], own);
  await accepted.host.start();
  assert.deepEqual([...accepted.host.snapshot().cards.keys()].sort(), ["a__other", "a__report"]);
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
