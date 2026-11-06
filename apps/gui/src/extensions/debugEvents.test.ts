import { test } from "node:test";
import { strict as assert } from "node:assert";
// The extension is plain JavaScript, loaded directly by Electron without a build.
// @ts-expect-error The independently loaded extension has no TypeScript declarations.
import { activate, agentsOf, createStore } from "../../../../extensions/debug/gui/index.js";

test("groups events like the built-in log", () => {
  assert.deepEqual(agentsOf({ type: "agent.output", data: { id: "a", author: "b" } }), ["a"]);
  assert.deepEqual(agentsOf({ type: "task.changed", data: { assignee: "a", author: "b", item: { from: "c" } } }), ["a", "b", "c"]);
  assert.deepEqual(agentsOf({ type: "other" }), []);
});

test("store keeps at most 1000 events per agent and survives subscriber removal", () => {
  const store = createStore();
  let changes = 0;
  const off = store.subscribe(() => changes++);
  for (let seq = 0; seq < 1001; seq++) store.add({ seq, type: "agent.output", data: { id: "a" } });
  off();
  store.add({ seq: 1001, type: "agent.output", data: { id: "a" } });
  assert.equal(changes, 1001);
  assert.equal(store.get("a").length, 1000);
  assert.equal(store.get("a")[0].seq, 2);
  assert.equal(createStore().get("a").length, 0);
});

test("offline first lookup retries and shows cached and new events after recovery", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const listeners = new Set<(event: any) => void>();
  let render: (context: { follow: string }) => any = () => undefined;
  let effect: () => (() => void) = () => () => {};
  const states: any[] = [];
  let cursor = 0;
  let online = false;
  let requests = 0;
  const ca = {
    events: { on: (fn: (event: any) => void) => { listeners.add(fn); return () => listeners.delete(fn); } },
    hub: { request: async () => { requests++; if (!online) throw new Error("offline"); return { agents: [{ id: "a", name: "Pilot" }] }; } },
    tabs: { register: (tab: { render: typeof render }) => { render = tab.render; } },
    createElement: (type: any, props: any, ...children: any[]) => typeof type === "function" ? type(props) : { type, props, children },
    useState: (initial: any) => { const index = cursor++; if (!(index in states)) states[index] = initial; return [states[index], (value: any) => { states[index] = value; }]; },
    useEffect: (fn: typeof effect) => { effect = fn; },
    useSyncExternalStore: (_subscribe: unknown, snapshot: () => unknown) => snapshot(),
  };
  activate(ca);
  const emit = (event: any) => listeners.forEach(fn => fn(event));
  emit({ seq: 1, type: "agent.output", data: { id: "a", text: "cached" } });
  cursor = 0; render({ follow: "Pilot" });
  const cleanup = effect();
  await Promise.resolve(); await Promise.resolve();
  online = true;
  emit({ seq: 2, type: "hub.started" });
  emit({ seq: 3, type: "agent.output", data: { id: "a", text: "new" } });
  t.mock.timers.tick(1000);
  await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
  cursor = 0;
  const output = JSON.stringify(render({ follow: "Pilot" }));
  assert.match(output, /cached/);
  assert.match(output, /new/);
  online = false;
  emit({ type: "agent.updated" });
  await Promise.resolve(); await Promise.resolve();
  cleanup();
  const afterCleanup = requests;
  t.mock.timers.tick(60000);
  await Promise.resolve();
  assert.equal(requests, afterCleanup);
});
