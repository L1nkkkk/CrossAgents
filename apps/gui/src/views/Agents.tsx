import { useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { AgentProfile } from "@bindings/AgentProfile";
import { api, errorText } from "../api";
import {
  AVATAR_COLORS,
  Avatar,
  ErrorBox,
  Field,
  Prose,
  ROLE_HUE,
  StatusChip,
  Vendor,
  agentTitle,
  agentTone,
  displayName,
  sessionHref,
  useAgents,
} from "../components";
import { useHubData } from "../hub";
import { useArmed } from "../parts";
import { ConnectExternal, ConnectInstructions } from "./External";
import type { ConnectInfo } from "@bindings/ConnectInfo";

/**
 * 设置 → 智能体: every agent the hub knows, external ones connected here. There is no team to
 * manage by hand: a Chat's agent brings in helpers itself (or the ones named with @), a Work's
 * lead recruits its own; this page is for looking them over, editing a card and connecting
 * agents that run elsewhere.
 */
export function AgentsPage() {
  const agents = useAgents();
  const [editing, setEditing] = useState<"external" | Agent>();
  const [connect, setConnect] = useState<ConnectInfo>();
  const list = (agents.data ?? []).filter((a) => !a.archived_at);

  return (
    <div className="team-page">
      <p className="settings-lead">
        这里列出 hub 里所有的 agent。不用手动组队：Chat 里的 agent 会按需要自己找帮手，也可以在消息里用 @ 点名；Work 的主导 agent 会自己招人。改名、换头像和人设也可以在会话里点头像。
      </p>
      <div className="row">
        <span className="chip">{list.length} 个</span>
        <div className="spacer" />
        {editing !== "external" && (
          <button className="ghost" onClick={() => setEditing("external")} title="让别处运行的 agent 通过 MCP 接入 hub">
            接入外部 agent
          </button>
        )}
      </div>
      {editing === "external" && <ConnectExternal onDone={() => setEditing(undefined)} />}
      {connect && <ConnectInstructions info={connect} onDone={() => setConnect(undefined)} />}
      <ErrorBox error={agents.error} />
      <TeamNotes />
      {agents.data && !list.length && (
        <div className="empty">
          <h1>还没有 agent</h1>
          <p>
            在 <a href="#/">首页</a> 发起一个 Chat 或 Work，用到的 agent 都会出现在这里。
          </p>
        </div>
      )}
      <div className="team-grid">
        {list.map((agent) =>
          typeof editing === "object" && editing.id === agent.id ? (
            <div key={agent.id} className="team-wide">
              <ProfileEditor agent={agent} onDone={() => setEditing(undefined)} />
            </div>
          ) : (
            <MemberCard
              key={agent.id}
              agent={agent}
              onEdit={() => setEditing(agent)}
              onConnect={() => api.connectAgent(agent.id).then(setConnect)}
            />
          ),
        )}
      </div>
    </div>
  );
}

/** Notes the agents keep across runs (note_write with scope "team"). */
function TeamNotes() {
  const notes = useHubData(() => api.notes("team"), [], (e) => e.type === "note.written");
  const list = notes.data?.notes ?? [];
  if (!list.length) return null;
  return (
    <details className="notes-panel">
      <summary>团队笔记 · {list.length}</summary>
      {list.map((n) => (
        <div key={n.key} className="note">
          <code>{n.key}</code>
          <Prose text={n.text} />
        </div>
      ))}
    </details>
  );
}

function MemberCard({ agent, onEdit, onConnect }: { agent: Agent; onEdit: () => void; onConnect: () => void }) {
  return (
    <div className="member">
      <div className="member-head">
        <Avatar agent={agent} size={40} status />
        <div className="member-who">
          <a className="member-name" href={sessionHref(agent)}>
            {displayName(agent)}
          </a>
          <span className="member-title">{agentTitle(agent)}</span>
        </div>
        <StatusChip status={agent.status} tone={agentTone(agent.status)} />
      </div>
      <p className={`member-persona ${agent.profile.persona ? "" : "muted"}`}>{agent.profile.persona || "还没有设定人设。"}</p>
      <div className="member-foot">
        <span className={`chip hue av-${ROLE_HUE[agent.role] ?? "slate"}`}>{agent.role}</span>
        {typeof (agent.config as Record<string, unknown> | null)?.spawned_by_run === "string" && (
          <a className="chip accent" href={`#/runs/${(agent.config as Record<string, string>).spawned_by_run}`} title="由 run 的 lead 招来的队友">
            run 招募
          </a>
        )}
        <Vendor plugin={agent.plugin} model={agent.model} title={agent.driver ?? undefined} />
        <span className="mono small muted member-addr" title={agent.cwd}>
          @{agent.name}
        </span>
        <div className="spacer" />
        {agent.plugin === "external" && (
          <button className="ghost" onClick={onConnect} title="重新生成令牌和 MCP 配置">
            连接配置
          </button>
        )}
        <button className="ghost" onClick={onEdit}>
          编辑
        </button>
      </div>
    </div>
  );
}

/** Edits an agent's card: name, face, persona, model. Agents are made by the start page or by a lead. */
export function ProfileEditor({ agent, onDone }: { agent: Agent; onDone: () => void }) {
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const [profile, setProfile] = useState<AgentProfile>(agent.profile);
  const [model, setModel] = useState<string>();
  const [name, setName] = useState(agent.name);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const remove = useArmed();
  const models = plugins.data?.find((p) => p.name === agent.plugin)?.models ?? [];
  const modelValue = model ?? agent.model ?? "";
  const set = (patch: Partial<AgentProfile>) => setProfile((p) => ({ ...p, ...patch }));
  const preview = { ...agent, name: name.trim() || agent.name, profile } as Agent;

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      // By id: a rename in the same save must not lose the agent.
      if (name.trim() && name.trim() !== agent.name) await api.renameAgent(agent.id, name.trim());
      await api.updateProfile(agent.id, { ...profile, display_name: null });
      if ((modelValue.trim() || null) !== agent.model) await api.setModel(agent.id, modelValue.trim() || null);
      onDone();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className="card form editor" onSubmit={submit}>
      <div className="editor-who">
        <Avatar agent={preview} size={56} />
        <div className="editor-face">
          <div className="row" style={{ gap: 8, alignItems: "flex-start" }}>
            <Field label={agent.name_source === "generated" ? "名字（自动起的，可以改）" : "名字"}>
              <input value={name} onChange={(e) => setName(e.target.value)} maxLength={32} placeholder="Luna" />
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
        <textarea rows={4} value={profile.persona ?? ""} onChange={(e) => set({ persona: e.target.value })} placeholder="不填也行；主导它的 agent 招人时会自己写。" />
      </Field>

      <ModelField value={modelValue} models={models} onChange={setModel} />
      <p className="muted small" style={{ margin: 0 }}>
        @{agent.name} · {agent.role} · {agent.plugin}
        {agent.driver ? ` · ${agent.driver}` : ""} · 新的人设在它下次开会话时生效；模型从下一轮起生效。
      </p>

      <ErrorBox error={error ?? plugins.error} />
      <div className="row">
        <button type="submit" className="primary" disabled={busy}>
          保存
        </button>
        <button type="button" className="ghost" onClick={() => onDone()}>
          取消
        </button>
        <div className="spacer" />
        <button
          type="button"
          className={`danger ${remove.armed ? "armed" : ""}`}
          disabled={busy}
          onClick={() =>
            remove.click(() =>
              api.deleteAgent(agent.id).then(
                () => onDone(),
                (err) => setError(errorText(err)),
              ),
            )
          }
        >
          {remove.armed ? `再点一次移除 @${agent.name}` : "移除"}
        </button>
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
