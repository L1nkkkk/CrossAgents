import { useEffect, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { Role } from "@bindings/Role";
import { api, errorText } from "../api";
import { navigate } from "../App";
import { Badge, ErrorBox, Field, useAgents } from "../components";
import { useHubData, useTranscript } from "../hub";

export function AgentsView({ selected }: { selected?: string }) {
  const agents = useAgents();
  const [adding, setAdding] = useState(false);
  const list = agents.data ?? [];
  const agent = list.find((a) => a.name === selected || a.id === selected);

  return (
    <div className="split">
      <aside className="agent-list">
        <header className="page-header">
          <h1>Agents</h1>
          <button onClick={() => setAdding(!adding)}>{adding ? "收起" : "登记"}</button>
        </header>
        {adding && <NewAgentForm onCreated={(a) => (setAdding(false), navigate("agents", a.name))} />}
        <ErrorBox error={agents.error} />
        {agents.data && !list.length && <p className="muted">还没有 agent。</p>}
        {list.map((a) => (
          <a key={a.id} className={`agent-item ${a === agent ? "active" : ""}`} href={`#/agents/${encodeURIComponent(a.name)}`}>
            <div>
              <strong>{a.name}</strong> <Badge status={a.status} />
            </div>
            <div className="muted small">
              {a.role} · {a.plugin}
              {a.model ? `/${a.model}` : ""}
            </div>
          </a>
        ))}
      </aside>
      <section className="agent-pane">
        {agent ? <AgentConsole agent={agent} /> : <p className="muted">选择一个 agent 查看实时输出。</p>}
      </section>
    </div>
  );
}

function AgentConsole({ agent }: { agent: Agent }) {
  const lines = useTranscript(agent.id);
  const [text, setText] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const outputRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);

  useEffect(() => {
    const el = outputRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [lines]);

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(undefined);
    try {
      await action();
      return true;
    } catch (err) {
      setError(errorText(err));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function send(e?: FormEvent) {
    e?.preventDefault();
    if (!text.trim()) return;
    if (await run(() => api.sendMessage(agent.name, { text }))) setText("");
  }

  function onKey(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      void send();
    }
  }

  return (
    <div className="console">
      <header className="page-header">
        <h2>{agent.name}</h2>
        <Badge status={agent.status} />
        <span className="muted small">
          {agent.role} · {agent.plugin}
          {agent.model ? `/${agent.model}` : ""} · <code>{agent.cwd}</code>
        </span>
        <div className="spacer" />
        {agent.status === "running" && (
          <button disabled={busy} onClick={() => run(() => api.cancelAgent(agent.name))}>
            中断本轮
          </button>
        )}
        {agent.status !== "stopped" && (
          <button disabled={busy} onClick={() => run(() => api.stopAgent(agent.name))}>
            停止
          </button>
        )}
      </header>
      <div
        className="output"
        ref={outputRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {lines.length === 0 && <p className="muted">还没有输出。</p>}
        {lines.map((line, i) => (
          <div key={i} className={`line ${line.kind}`}>
            {line.text}
          </div>
        ))}
      </div>
      <ErrorBox error={error} />
      <form className="composer" onSubmit={send}>
        <textarea
          rows={3}
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
          placeholder="发消息（Enter 发送，Shift+Enter 换行）；agent 离线时会自动启动"
        />
        <button type="submit" className="primary" disabled={busy || !text.trim()}>
          发送
        </button>
      </form>
    </div>
  );
}

function NewAgentForm({ onCreated }: { onCreated: (agent: Agent) => void }) {
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const [name, setName] = useState("");
  const [plugin, setPlugin] = useState("");
  const [model, setModel] = useState("");
  const [role, setRole] = useState<Role>("worker");
  const [cwd, setCwd] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const pluginValue = plugin || plugins.data?.[0]?.name || "";
  const models = plugins.data?.find((p) => p.name === pluginValue)?.models ?? [];

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      const agent = await api.createAgent({
        name: name.trim(),
        plugin: pluginValue,
        driver: null,
        model: model.trim() || null,
        role,
        cwd: cwd.trim(),
        config: {},
      });
      onCreated(agent);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="card form" onSubmit={submit}>
      <Field label="名字">
        <input value={name} onChange={(e) => setName(e.target.value)} required />
      </Field>
      <div className="row">
        <Field label="插件">
          <select value={pluginValue} onChange={(e) => setPlugin(e.target.value)}>
            {plugins.data?.map((p) => (
              <option key={p.name}>{p.name}</option>
            ))}
          </select>
        </Field>
        <Field label="角色">
          <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
            <option value="planner">planner</option>
            <option value="worker">worker</option>
            <option value="reviewer">reviewer</option>
          </select>
        </Field>
      </div>
      <Field label="模型（可选）">
        <input list="models" value={model} onChange={(e) => setModel(e.target.value)} />
        <datalist id="models">
          {models.map((m) => (
            <option key={m} value={m} />
          ))}
        </datalist>
      </Field>
      <Field label="工作目录（git 仓库）">
        <input value={cwd} onChange={(e) => setCwd(e.target.value)} required placeholder="C:\path\to\repo" />
      </Field>
      <ErrorBox error={error ?? plugins.error} />
      <button type="submit" className="primary" disabled={busy || !name.trim() || !pluginValue || !cwd.trim()}>
        登记
      </button>
    </form>
  );
}
