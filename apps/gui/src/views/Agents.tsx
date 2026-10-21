import { useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { AgentProfile } from "@bindings/AgentProfile";
import type { Role } from "@bindings/Role";
import { api, errorText } from "../api";
import { navigate } from "../App";
import {
  AVATAR_COLORS,
  Avatar,
  ErrorBox,
  Field,
  PERSONA_PRESETS,
  StatusChip,
  agentTitle,
  agentTone,
  displayName,
  sessionHref,
  useAgents,
} from "../components";
import { useHubData } from "../hub";
import { useArmed } from "../parts";

/** The team: every agent as a colleague with a face, a title and a persona. */
export function AgentsView() {
  const agents = useAgents();
  const [editing, setEditing] = useState<"new" | Agent>();
  const list = agents.data ?? [];

  return (
    <>
      <div className="topbar">
        <span className="title">团队</span>
        <span className="chip">{list.length}</span>
        <div className="spacer" />
        {editing !== "new" && (
          <button className="primary" onClick={() => setEditing("new")}>
            添加队友
          </button>
        )}
      </div>
      <div className="page">
        <div className="page-inner team-page">
          {editing === "new" && <Editor onDone={(a) => (setEditing(undefined), a && navigate("s", a.name))} />}
          <ErrorBox error={agents.error} />
          {agents.data && !list.length && editing !== "new" && (
            <div className="empty">
              <h1>组建你的团队</h1>
              <p>一个负责拆计划的架构师，再加几个写代码、做设计、做审查的队友。</p>
            </div>
          )}
          <div className="team-grid">
            {list.map((agent) =>
              editing !== "new" && editing?.id === agent.id ? (
                <div key={agent.id} className="team-wide">
                  <Editor agent={agent} onDone={() => setEditing(undefined)} />
                </div>
              ) : (
                <MemberCard key={agent.id} agent={agent} onEdit={() => setEditing(agent)} />
              ),
            )}
          </div>
        </div>
      </div>
    </>
  );
}

function MemberCard({ agent, onEdit }: { agent: Agent; onEdit: () => void }) {
  return (
    <div className="member">
      <div className="member-head">
        <Avatar agent={agent} size={40} status />
        <div className="member-who">
          <a className="member-name" href={sessionHref(agent.name)}>
            {displayName(agent)}
          </a>
          <span className="member-title">{agentTitle(agent)}</span>
        </div>
        <StatusChip status={agent.status} tone={agentTone(agent.status)} />
      </div>
      <p className={`member-persona ${agent.profile.persona ? "" : "muted"}`}>{agent.profile.persona || "还没有设定人设。"}</p>
      <div className="member-foot">
        <span className="chip">{agent.role}</span>
        {typeof (agent.config as Record<string, unknown> | null)?.spawned_by_run === "string" && (
          <a className="chip accent" href={`#/runs/${(agent.config as Record<string, string>).spawned_by_run}`} title="由 run 的 lead 招来的队友">
            run 招募
          </a>
        )}
        <span className="chip" title={agent.driver ?? undefined}>
          {agent.plugin}
          {agent.model ? ` · ${agent.model}` : ""}
        </span>
        <span className="mono small muted member-addr" title={agent.cwd}>
          @{agent.name}
        </span>
        <div className="spacer" />
        <button className="ghost" onClick={onEdit}>
          编辑
        </button>
      </div>
    </div>
  );
}

const EMPTY: AgentProfile = { display_name: null, title: null, avatar: null, color: null, persona: null };

/** Adds a teammate (`agent` absent) or edits one's card. */
function Editor({ agent, onDone }: { agent?: Agent; onDone: (created?: Agent) => void }) {
  const creating = !agent;
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const [profile, setProfile] = useState<AgentProfile>(agent?.profile ?? EMPTY);
  const [preset, setPreset] = useState<string>();
  const [name, setName] = useState("");
  const [plugin, setPlugin] = useState("");
  const [model, setModel] = useState<string>();
  const [role, setRole] = useState<Role>("worker");
  const [cwd, setCwd] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const remove = useArmed();
  const pluginValue = agent?.plugin ?? (plugin || plugins.data?.[0]?.name || "");
  const models = plugins.data?.find((p) => p.name === pluginValue)?.models ?? [];
  // Until it is touched: the agent's model, or for a new agent the plugin's first one, so
  // a new agent never falls back to a default the account may not have.
  const modelValue = model ?? (agent ? agent.model ?? "" : models[0] ?? "");
  const set = (patch: Partial<AgentProfile>) => setProfile((p) => ({ ...p, ...patch }));

  function applyPreset(key: string) {
    const p = PERSONA_PRESETS.find((x) => x.key === key);
    if (!p) return;
    setPreset(key);
    setRole(p.role);
    set({ title: p.title, avatar: p.avatar, color: p.color, persona: p.persona });
    if (!name) setName(p.key);
  }

  // What the preview shows: the card being edited, on a stand-in agent.
  const preview: Agent = {
    ...(agent ?? {
      id: "preview",
      name: name || "new",
      plugin: pluginValue,
      driver: null,
      model: modelValue || null,
      role,
      cwd,
      config: null,
      status: "idle",
      created_at: new Date().toISOString(),
    }),
    profile,
  } as Agent;

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      if (agent) {
        await api.updateProfile(agent.name, profile);
        if ((modelValue.trim() || null) !== agent.model) await api.setModel(agent.name, modelValue.trim() || null);
        onDone();
      } else {
        const created = await api.createAgent({
          name: name.trim(),
          plugin: pluginValue,
          driver: null,
          model: modelValue.trim() || null,
          role,
          cwd: cwd.trim(),
          config: {},
          profile,
        });
        onDone(created);
      }
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="card form editor" onSubmit={submit}>
      {creating && (
        <div className="presets">
          {PERSONA_PRESETS.map((p) => (
            <button type="button" key={p.key} className={`preset ${preset === p.key ? "on" : ""}`} onClick={() => applyPreset(p.key)}>
              <span className="preset-emoji">{p.avatar}</span>
              {p.label}
            </button>
          ))}
        </div>
      )}

      <div className="editor-who">
        <Avatar agent={preview} size={56} />
        <div className="editor-face">
          <div className="row" style={{ gap: 8, alignItems: "flex-start" }}>
            <Field label="显示名">
              <input value={profile.display_name ?? ""} onChange={(e) => set({ display_name: e.target.value })} placeholder="Ada" />
            </Field>
            <Field label="头衔">
              <input value={profile.title ?? ""} onChange={(e) => set({ title: e.target.value })} placeholder="设计师" />
            </Field>
            <Field label="头像（emoji 或两个字母）">
              <input value={profile.avatar ?? ""} onChange={(e) => set({ avatar: e.target.value })} placeholder="🎨" maxLength={8} />
            </Field>
          </div>
          <div className="swatches" role="radiogroup" aria-label="头像颜色">
            {AVATAR_COLORS.map((c) => (
              <button
                type="button"
                key={c}
                className={`swatch av-${c} ${profile.color === c ? "on" : ""}`}
                onClick={() => set({ color: profile.color === c ? null : c })}
                aria-label={c}
                aria-checked={profile.color === c}
                role="radio"
              />
            ))}
          </div>
        </div>
      </div>

      <Field label="人设（专长与工作方式，会加入它的开场说明）">
        <textarea
          rows={4}
          value={profile.persona ?? ""}
          onChange={(e) => set({ persona: e.target.value })}
          placeholder="You review diffs for security: injection, auth checks, secrets in code…"
        />
      </Field>

      {creating ? (
        <>
          <div className="row" style={{ alignItems: "flex-start" }}>
            <Field label="地址名（小写，用来互相发消息）">
              <input value={name} onChange={(e) => setName(e.target.value)} required placeholder="ada" />
            </Field>
            <Field label="角色（决定能用哪些工具）">
              <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
                <option value="planner">planner · 拆计划、派活</option>
                <option value="worker">worker · 按规格干活</option>
                <option value="reviewer">reviewer · 审查 diff</option>
              </select>
            </Field>
          </div>
          <div className="row" style={{ alignItems: "flex-start" }}>
            <Field label="插件">
              <select value={pluginValue} onChange={(e) => setPlugin(e.target.value)}>
                {plugins.data?.map((p) => (
                  <option key={p.name} value={p.name}>
                    {p.name} · {p.driver}
                  </option>
                ))}
              </select>
            </Field>
            <ModelField value={modelValue} models={models} onChange={setModel} />
          </div>
          <Field label="工作目录（git 仓库）">
            <div className="row" style={{ gap: 6 }}>
              <input value={cwd} onChange={(e) => setCwd(e.target.value)} required placeholder="C:\path\to\repo" style={{ flex: 1 }} />
              {window.crossagents?.pickFolder && (
                <button type="button" className="ghost" onClick={async () => setCwd((await window.crossagents!.pickFolder!()) ?? cwd)}>
                  选择…
                </button>
              )}
            </div>
          </Field>
        </>
      ) : (
        <>
          <ModelField value={modelValue} models={models} onChange={setModel} />
          <p className="muted small" style={{ margin: 0 }}>
            @{agent.name} · {agent.role} · {agent.plugin}
            {agent.driver ? ` · ${agent.driver}` : ""} · 新的人设在它下次开会话时生效；模型从下一轮起生效。
          </p>
        </>
      )}

      <ErrorBox error={error ?? plugins.error} />
      <div className="row">
        <button type="submit" className="primary" disabled={busy || (creating && (!name.trim() || !pluginValue || !cwd.trim()))}>
          {creating ? "添加到团队" : "保存"}
        </button>
        <button type="button" className="ghost" onClick={() => onDone()}>
          取消
        </button>
        {agent && (
          <>
            <div className="spacer" />
            <button
              type="button"
              className={`danger ${remove.armed ? "armed" : ""}`}
              disabled={busy}
              onClick={() =>
                remove.click(() =>
                  api.deleteAgent(agent.name).then(
                    () => onDone(),
                    (err) => setError(errorText(err)),
                  ),
                )
              }
            >
              {remove.armed ? `再点一次移除 @${agent.name}` : "移出团队"}
            </button>
          </>
        )}
      </div>
    </form>
  );
}

/** The plugin's models as a list; free text when it lists none or the value is not one of them. */
function ModelField({ value, models, onChange }: { value: string; models: string[]; onChange: (m: string) => void }) {
  const listed = !value || models.includes(value);
  return (
    <Field label="模型（背后的大脑）">
      {models.length && listed ? (
        <select value={value} onChange={(e) => onChange(e.target.value)}>
          {models.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
          <option value="">插件默认</option>
        </select>
      ) : (
        <input value={value} onChange={(e) => onChange(e.target.value)} placeholder="插件默认" />
      )}
    </Field>
  );
}
