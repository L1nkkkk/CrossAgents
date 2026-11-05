// Independent implementation of the built-in event log; one store per activation.
export function agentsOf(e) {
  const d = e.data;
  if (!d) return [];
  if (e.type.startsWith("agent.") && typeof d.id === "string") return [d.id];
  const ids = [];
  for (const key of ["assignee", "author", "owner", "by"]) if (typeof d[key] === "string") ids.push(d[key]);
  if (d.item && typeof d.item.from === "string") ids.push(d.item.from);
  return ids;
}

export function createStore() {
  const byAgent = new Map();
  const listeners = new Set();
  const empty = [];
  return {
    get: id => byAgent.get(id) ?? empty,
    subscribe: listener => { listeners.add(listener); return () => listeners.delete(listener); },
    add(e) {
      const ids = agentsOf(e);
      for (const id of ids) {
        const old = byAgent.get(id) ?? empty;
        byAgent.set(id, [...old.slice(-999), e]);
      }
      if (ids.length) listeners.forEach(listener => listener());
    },
  };
}

export function activate(ca) {
  const store = createStore();
  ca.events.on(e => store.add(e));
  const { createElement: h, useState, useEffect, useSyncExternalStore } = ca;
  function Panel({ follow }) {
    const [agents, setAgents] = useState([]);
    const [filter, setFilter] = useState("");
    const [open, setOpen] = useState();
    useEffect(() => {
      let live = true;
      const refresh = () => ca.hub.request("GET", "/v1/agents").then(result => { if (live) setAgents(result.agents); }).catch(() => {});
      refresh();
      const off = ca.events.on(e => { if (["agent.created", "agent.updated", "agent.removed"].includes(e.type)) refresh(); });
      return () => { live = false; off(); };
    }, []);
    const who = follow && agents.find(a => a.id === follow || a.name.toLowerCase() === follow.toLowerCase());
    const events = useSyncExternalStore(store.subscribe, () => store.get(who?.id));
    if (!follow) return h("p", { className: "muted small tool-blank" }, "打开一个会话，这里显示它的原始 hub 事件。");
    const f = filter.trim().toLowerCase();
    const shown = events.filter(e => !f || e.type.includes(f) || JSON.stringify(e).toLowerCase().includes(f)).slice(-300).reverse();
    return h("div", { className: "events-pane" },
      h("div", { className: "events-filter" }, who && h("span", { className: "tool-whose" }, who.name),
        h("input", { value: filter, onChange: e => setFilter(e.target.value), placeholder: "过滤：类型或内容，如 tool_call", "aria-label": "过滤事件" })),
      !events.length && h("p", { className: "muted small tool-blank" }, "打开页面以来还没有这个会话的事件。"),
      h("div", { className: "events-list" }, shown.map(e => {
        const d = e.data ?? {};
        const pick = ["title", "text", "status", "to", "message", "stop_reason", "stream"].map(k => d[k]).find(v => typeof v === "string");
        const summary = (pick ?? "").replace(/\s+/g, " ").slice(0, 80);
        return h("div", { key: e.seq, className: `ev ${open === e.seq ? "open" : ""}` },
          h("button", { className: "ev-line", onClick: () => setOpen(open === e.seq ? undefined : e.seq), title: new Date(e.ts).toLocaleString() },
            h("span", { className: "ev-seq" }, e.seq), h("span", { className: "ev-type" }, e.type), h("span", { className: "ev-sum" }, summary)),
          open === e.seq && h("pre", { className: "ev-json" }, JSON.stringify(e, null, 2)));
      })));
  }
  ca.tabs.register({ id: "events", label: "事件流", render: ({ follow }) => h(Panel, { follow, key: follow }) });
}
