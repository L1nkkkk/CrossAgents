// Agents that run outside the hub (docs/tasks/H1.md H1-6): register one, then paste the MCP
// configuration into it. It joins the team with the same tools as everyone and reads its
// messages with inbox_read.

import { useState, type FormEvent } from "react";
import type { ConnectInfo } from "@bindings/ConnectInfo";
import type { Role } from "@bindings/Role";
import { api, errorText } from "../api";
import { ErrorBox, Field } from "../components";

export function ConnectExternal({ onDone }: { onDone: () => void }) {
  const [name, setName] = useState("");
  const [displayName, setDisplayName] = useState("");
  const [role, setRole] = useState<Role>("worker");
  const [cwd, setCwd] = useState("");
  const [info, setInfo] = useState<ConnectInfo>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      await api.createAgent({
        name: name.trim(),
        plugin: "external",
        driver: null,
        model: null,
        role,
        cwd: cwd.trim(),
        config: {},
        profile: { display_name: displayName.trim() || null, title: null, avatar: null, avatar_image: null, color: null, persona: null },
      });
      setInfo(await api.connectAgent(name.trim()));
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  if (info) return <ConnectInstructions info={info} onDone={onDone} />;
  return (
    <form className="card form" onSubmit={submit}>
      <p className="muted small" style={{ margin: 0 }}>
        已经在别处运行的 agent（你自己开的 Claude Code、Codex、Cursor 等）也能加入团队：在这里登记，把生成的 MCP 配置粘贴进去，它就能用和队友一样的工具，并用 inbox_read 收消息。
      </p>
      <div className="row" style={{ alignItems: "flex-start" }}>
        <Field label="地址名（小写）">
          <input value={name} onChange={(e) => setName(e.target.value)} required placeholder="my-claude" />
        </Field>
        <Field label="显示名">
          <input value={displayName} onChange={(e) => setDisplayName(e.target.value)} placeholder="我的 Claude" />
        </Field>
        <Field label="角色">
          <select value={role} onChange={(e) => setRole(e.target.value as Role)}>
            <option value="worker">worker</option>
            <option value="reviewer">reviewer</option>
            <option value="planner">planner</option>
          </select>
        </Field>
      </div>
      <Field label="它工作的目录（git 仓库）">
        <input value={cwd} onChange={(e) => setCwd(e.target.value)} required placeholder="C:\path\to\repo" />
      </Field>
      <ErrorBox error={error} />
      <div className="row">
        <button type="submit" className="primary" disabled={busy || !name.trim() || !cwd.trim()}>
          登记并生成配置
        </button>
        <button type="button" className="ghost" onClick={onDone}>
          取消
        </button>
      </div>
    </form>
  );
}

/** The configuration to paste, for either kind of client. */
export function ConnectInstructions({ info, onDone }: { info: ConnectInfo; onDone: () => void }) {
  const [kind, setKind] = useState<"json" | "toml">("json");
  const text = kind === "json" ? info.json_config : info.toml_config;
  return (
    <div className="card form">
      <strong>把下面的配置加进 @{info.agent} 的 MCP 设置</strong>
      <div className="segmented small" style={{ alignSelf: "flex-start" }}>
        <button type="button" className={kind === "json" ? "active" : ""} onClick={() => setKind("json")}>
          mcpServers JSON（Claude Code、Cursor…）
        </button>
        <button type="button" className={kind === "toml" ? "active" : ""} onClick={() => setKind("toml")}>
          config.toml（Codex）
        </button>
      </div>
      <pre className="connect-config">{text}</pre>
      <div className="row">
        <CopyText text={text} label="复制配置" />
        <CopyText text={info.instructions} label="复制角色说明" />
      </div>
      <p className="muted small" style={{ margin: 0 }}>
        令牌在 hub 重启或重新生成后失效。角色说明可以作为它的系统提示或第一条消息。
      </p>
      <div className="row">
        <button className="primary" onClick={onDone}>
          完成
        </button>
      </div>
    </div>
  );
}

function CopyText({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="ghost small"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        } catch {
          // The text is on screen to copy by hand.
        }
      }}
    >
      {copied ? "已复制" : label}
    </button>
  );
}
