import { useCallback, useEffect, useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { AgentProfile } from "@bindings/AgentProfile";
import { RefreshCw } from "lucide-react";
import { api, errorText } from "../api";
import {
  AVATAR_COLORS,
  Avatar,
  CopyCommand,
  ErrorBox,
  Field,
  Prose,
  ROLE_HUE,
  StatusChip,
  Vendor,
  VendorMark,
  agentTitle,
  agentTone,
  displayName,
  sessionHref,
  useAgents,
} from "../components";
import { onHubEvent, useHubData } from "../hub";
import { useArmed } from "../parts";
import { AvatarChoice } from "./AvatarGallery";
import { ConnectExternal, ConnectInstructions } from "./External";
import { vendorLabel } from "../lib/models";
import type { ConnectInfo } from "@bindings/ConnectInfo";
import type { CliInfo } from "@bindings/CliInfo";

/**
 * 设置 → 智能体: every agent the hub knows, external ones connected here. There is no team to
 * manage by hand: a Chat's agent brings in helpers itself (or the ones named with @), a Work's
 * lead recruits its own; this page is for looking them over, editing a card and connecting
 * agents that run elsewhere.
 */
export function AgentsPage() {
  const agents = useAgents();
  const runs = useHubData(
    () => api.listRuns().then((r) => new Set(r.runs.map((x) => x.id))),
    [],
    (e) => e.type.startsWith("run."),
  );
  const [editing, setEditing] = useState<"external" | Agent>();
  const [connect, setConnect] = useState<ConnectInfo>();
  const list = (agents.data ?? []).filter((a) => !a.archived_at);
  // A run that was deleted no longer exists to link to (docs/tasks/R1.md §3.2).
  const runIds = runs.data ?? new Set<string>();

  return (
    <div className="team-page">
      <p className="settings-lead">
        这里列出 hub 里所有的 agent。不用手动组队：Chat 里的 agent 会按需要自己找帮手，也可以在消息里用 @ 点名；Work 的主导 agent 会自己招人。改名、换头像和人设也可以在会话里点头像。
      </p>
      <VendorClis />
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
              runIds={runIds}
              onEdit={() => setEditing(agent)}
              onConnect={() => api.connectAgent(agent.id).then(setConnect)}
            />
          ),
        )}
      </div>
    </div>
  );
}

/** Where a CLI came from, as `PluginInfo.cli.source` names it. */
const CLI_SOURCE: Record<string, string> = {
  config: "配置指定",
  env: "环境变量",
  path: "PATH",
  native: "原生安装",
  bundled: "桌面版自带",
};

/** How many times a row that says it is asking is read again before the poll gives up. */
const PROBE_POLLS = 40;

/**
 * The agent CLIs this machine has (docs/DESIGN.md §9.3): each vendor's version and where its
 * program came from, a button that installs or upgrades it, and why there is no button when
 * there is none. Nothing runs before the person confirms the exact command.
 *
 * The list is not only what the hub saw when it started: entering the page and coming back to
 * the window read it again, and the button asks the hub to look again and probe the versions,
 * so a CLI installed in a terminal shows up without restarting the hub.
 */
function VendorClis() {
  const plugins = useHubData(
    () => api.listPlugins().then((r) => r.plugins),
    [],
    (e) =>
      e.type.startsWith("account.") ||
      e.type === "plugin.changed" ||
      e.type.startsWith("cli.install."),
  );
  const [confirm, setConfirm] = useState<Job>();
  const [jobs, setJobs] = useState<Record<string, Progress>>({});
  // The POST is out; the probes themselves show in each row's `probing`.
  const [asking, setAsking] = useState(false);
  const [askError, setAskError] = useState<string>();
  const reload = plugins.reload;

  // Asking the hub to look again (`POST /v1/plugins/refresh`) forgets what it remembered for
  // every CLI -- including "this one could not answer just now", whose 60 s retry window is
  // what keeps a broken CLI from being asked again and again. So only the person asks: the
  // "重新检测" button. Nothing else on this page calls it.
  const refresh = useCallback(() => {
    setAsking(true);
    api
      .refreshPlugins()
      .then(() => setAskError(undefined))
      .catch((err) => setAskError(errorText(err)))
      // The probes answer one at a time on the event stream; this read picks up the ones
      // already out, so their rows say "asking" meanwhile.
      .finally(() => {
        setAsking(false);
        reload();
      });
  }, [reload]);

  // Entering the page and coming back to the window only read the list. Reading it is enough
  // for a CLI installed in a terminal to appear: a miss is never cached, and a file that
  // changed is asked its version again. Triggering a full look here instead kept asking the
  // CLIs -- and kept their rows saying "probing" for as long as the person kept alt-tabbing
  // back to the window.
  useEffect(() => {
    const onFocus = () => reload();
    const onVisible = () => {
      if (document.visibilityState === "visible") reload();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [reload]);

  useEffect(
    () =>
      onHubEvent((e) => {
        if (e.type === "cli.install.started") {
          const { plugin, action } = e.data;
          setJobs((all) => ({ ...all, [plugin]: { plugin, action, lines: [], dropped: 0 } }));
        } else if (e.type === "cli.install.output") {
          const { plugin, line, dropped } = e.data;
          setJobs((all) => {
            const job = all[plugin];
            if (!job) return all;
            // Only the tail is kept: an installer may print thousands of lines.
            return { ...all, [plugin]: { ...job, lines: [...job.lines, line].slice(-200), dropped } };
          });
        } else if (e.type === "cli.install.finished") {
          const { plugin, ok, reason, code } = e.data;
          setJobs((all) => {
            const job = all[plugin];
            if (!job) return all;
            return { ...all, [plugin]: { ...job, done: { ok, reason: reason ?? undefined, code: code ?? undefined } } };
          });
        }
      }),
    [],
  );

  const list = (plugins.data ?? []).filter((p) => p.cli);
  const installed = list.filter((p) => p.cli?.installed).length;
  // Asking, either because the request is out or because a row's version is being probed.
  const probing = list.some((p) => p.cli?.probing);
  const detecting = asking || probing;

  // A probe the hub started on its own (at startup) can end without an event reaching this
  // window; while a row still says it is asking, read the list again now and then. This only
  // reads: asking the hub to look again would start a new probe. The hub ends a probe within
  // five seconds (its timeout), so this stops by itself; the count is a safety net only.
  useEffect(() => {
    if (!probing) return;
    let left = PROBE_POLLS;
    const timer = setInterval(() => {
      reload();
      if (--left <= 0) clearInterval(timer);
    }, 1500);
    return () => clearInterval(timer);
  }, [probing, reload]);

  if (!list.length) return null;

  async function start(job: Job) {
    setConfirm(undefined);
    try {
      if (job.action === "install") await api.installPlugin(job.plugin);
      else await api.upgradePlugin(job.plugin);
    } catch (err) {
      setJobs((all) => ({
        ...all,
        [job.plugin]: { plugin: job.plugin, action: job.action, lines: [], dropped: 0, done: { ok: false, reason: errorText(err) } },
      }));
    }
  }

  return (
    <section className="settings-section">
      <div className="settings-head">
        <h2>本机的 CLI</h2>
        <span className="chip">
          {installed} / {list.length} 已安装
        </span>
        <div className="spacer" />
        <button
          className="ghost"
          onClick={() => refresh()}
          disabled={detecting}
          title="重新在 PATH 和安装目录里查找这些 CLI，并重新探测版本"
        >
          <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
            <RefreshCw
              size={13}
              aria-hidden
              style={detecting ? { animation: "spin 1s linear infinite" } : undefined}
            />
            {detecting ? "检测中…" : "重新检测"}
          </span>
        </button>
      </div>
      <ErrorBox error={plugins.error ?? askError} />
      <div className="setting-card">
        {list.map((p) => {
          const cli = p.cli!;
          const job = jobs[p.name];
          return (
            <div className="setting-line cli-line" key={p.name}>
              <div className="model-name">
                <div className="setting-label">
                  <VendorMark plugin={p.name} size={16} />
                  {vendorLabel(p.name)}
                </div>
                <div className="setting-hint" title={cli.path ?? undefined}>
                  {cli.installed
                    ? `${cli.probing ? "检测版本中…" : (cli.version ?? "版本未知")} · ${CLI_SOURCE[cli.source ?? ""] ?? cli.source ?? "来源未知"}`
                    : "没装"}
                </div>
              </div>
              <div className="cli-actions">
                {job ? (
                  <InstallProgress job={job} />
                ) : cli.installed ? (
                  <>
                    {cli.can_upgrade ? (
                      <button
                        className="ghost"
                        onClick={() =>
                          setConfirm({
                            plugin: p.name,
                            action: "upgrade",
                            argv: cli.upgrade_command ?? [],
                            described_by: cli.described_by,
                          })
                        }
                      >
                        升级
                      </button>
                    ) : (
                      upgradeNote(cli) && <span className="setting-hint">{upgradeNote(cli)}</span>
                    )}
                    {p.name === "claude" && cli.installed_via === "npm" && (
                      <span className="setting-hint">
                        npm 版已弃用，可以运行 <code>claude install</code> 迁到原生安装
                      </span>
                    )}
                    {cli.docs && (
                      <a href={cli.docs} target="_blank" rel="noreferrer" className="small">
                        文档
                      </a>
                    )}
                  </>
                ) : cli.install_command ? (
                  <>
                    <button
                      className="primary"
                      onClick={() =>
                        setConfirm({
                          plugin: p.name,
                          action: "install",
                          argv: cli.install_command ?? [],
                          described_by: cli.described_by,
                        })
                      }
                    >
                      安装
                    </button>
                    {cli.docs && (
                      <a href={cli.docs} target="_blank" rel="noreferrer" className="small">
                        文档
                      </a>
                    )}
                  </>
                ) : (
                  cli.install_hint && <CopyCommand command={cli.install_hint} />
                )}
              </div>
              {job?.done?.ok && (
                <div className="cli-done ok">
                  {job.action === "install" ? (
                    <>
                      装好了，下一步登录账号：<a href="#/usage">订阅与额度</a>
                    </>
                  ) : (
                    "升级完成"
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
      {confirm && <InstallConfirm job={confirm} onCancel={() => setConfirm(undefined)} onRun={() => void start(confirm)} />}
    </section>
  );
}

type Job = { plugin: string; action: "install" | "upgrade"; argv: string[]; described_by?: string };
type Progress = {
  plugin: string;
  action: string;
  lines: string[];
  dropped: number;
  done?: { ok: boolean; reason?: string; code?: number };
};

/** What a description says it came from, in words for the confirmation box. */
function describedBy(source?: string): string {
  if (!source || source === "built-in") return "内置描述";
  if (source === "config.toml") return "config.toml 里的覆盖";
  return source;
}

/** Why no upgrade button is offered for what was found. */
function upgradeNote(cli: CliInfo): string | undefined {
  if (cli.can_upgrade || !cli.installed) return undefined;
  if (cli.source === "bundled") return "随桌面版更新";
  if (cli.source === "config" || cli.source === "env") return "由你的配置指定";
  return "没有对应的升级命令";
}

/** The confirmation: the exact command, where its description came from, and a reminder. */
function InstallConfirm({ job, onCancel, onRun }: { job: Job; onCancel: () => void; onRun: () => void }) {
  const label = vendorLabel(job.plugin);
  return (
    <div className="dialog-backdrop" onMouseDown={onCancel}>
      <div className="dialog" role="dialog" aria-modal="true" onMouseDown={(e) => e.stopPropagation()}>
        <h2>{job.action === "install" ? `安装 ${label}` : `升级 ${label}`}</h2>
        <p className="setting-hint">会在这台电脑上执行下面的命令。命令只来自它的描述文件。</p>
        <pre className="cli-out">{job.argv.join(" ")}</pre>
        <p className="setting-hint">描述来自：{describedBy(job.described_by)}</p>
        <div className="row" style={{ justifyContent: "flex-end" }}>
          <button className="ghost" onClick={onCancel}>
            取消
          </button>
          <button className="primary" onClick={onRun}>
            执行
          </button>
        </div>
      </div>
    </div>
  );
}

/** What the running install printed, last lines first thing shown; the rest folds away. */
function InstallProgress({ job }: { job: Progress }) {
  const [expanded, setExpanded] = useState(false);
  const shown = expanded ? job.lines : job.lines.slice(-6);
  const running = !job.done;
  return (
    <div className="cli-job">
      <div className="cli-job-head">
        <span className="chip small">
          {running ? (job.action === "install" ? "安装中" : "升级中") : job.done?.ok ? "完成" : "失败"}
        </span>
        {job.dropped > 0 && <span className="muted small">{job.dropped} 行未显示</span>}
        {job.lines.length > 6 && (
          <button className="ghost small" onClick={() => setExpanded((x) => !x)}>
            {expanded ? "收起" : "展开全部"}
          </button>
        )}
      </div>
      <pre className="cli-out">{shown.join("\n") || "…"}</pre>
      {job.done && !job.done.ok && <div className="cli-done bad">{job.done.reason ?? "失败"}</div>}
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

function MemberCard({
  agent,
  runIds,
  onEdit,
  onConnect,
}: {
  agent: Agent;
  /** The runs that still exist; a chip links to one of those only. */
  runIds: Set<string>;
  onEdit: () => void;
  onConnect: () => void;
}) {
  const spawnedBy = (agent.config as Record<string, unknown> | null)?.spawned_by_run;
  const runId = typeof spawnedBy === "string" ? spawnedBy : undefined;
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
        {runId && runIds.has(runId) && (
          <a className="chip accent" href={`#/runs/${runId}`} title="由 run 的 lead 招来的队友">
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
  const claudeLike = agent.plugin === "claude" || agent.plugin === "codebuddy";
  const savedSources = settingSourcesOf(agent);
  const [sources, setSources] = useState<string | null>(savedSources);
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
      if (claudeLike && sources !== savedSources) await api.agentSettings(agent.id, { setting_sources: sources });
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
              <input value={profile.avatar ?? ""} onChange={(e) => set(e.target.value.trim() ? { avatar: e.target.value, avatar_image: null } : { avatar: e.target.value })} placeholder="🎨" maxLength={8} />
            </Field>
          </div>
          {/* Text with no picture also keeps the next upload from handing this session one. */}
          <AvatarChoice
            current={profile.avatar_image}
            onChoose={(id) => set({ avatar_image: id })}
            onText={() => set({ avatar_image: null, avatar: profile.avatar || Array.from(name.trim() || agent.name)[0]?.toUpperCase() || null })}
          />
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
      {claudeLike && (
        <Field label={`${agent.plugin === "codebuddy" ? SOURCES_LABEL.codebuddy : SOURCES_LABEL.claude}；保存后空闲的会话会重启并续上原对话，忙时等这轮结束后下次启动`}>
          <select value={sources ?? "default"} onChange={(e) => setSources(e.target.value === "default" ? null : e.target.value)}>
            {(agent.plugin === "codebuddy" ? CODEBUDDY_SOURCES : SETTING_SOURCES).map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
        </Field>
      )}
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

/**
 * Which of Claude Code's settings an agent loads (docs/tasks/DOGFOOD.md §6): presets of
 * `config.setting_sources`. With none, the hub still gives it the project's rules.
 */
const SETTING_SOURCES = [
  { value: "default", label: "项目与本地（缺省）：仓库的 CLAUDE.md 与 .claude/" },
  { value: "user,project,local", label: "全部：再加本机 ~/.claude 的个人设置与 hooks" },
  { value: "", label: "都不读：只用 CrossAgents 注入的项目规则（最可控）" },
];

/** CodeBuddy never reads CLAUDE.md, whichever it loads: the project's rules reach it from the hub. */
const CODEBUDDY_SOURCES = [
  { value: "default", label: "项目与本地（缺省）：仓库里 CodeBuddy 自己的设置" },
  { value: "user,project,local", label: "全部：再加本机 ~/.codebuddy 的个人设置与 hooks" },
  { value: "", label: "都不读：只用 CrossAgents 注入的项目规则（最可控）" },
];

const SOURCES_LABEL = {
  claude: "读取 Claude 设置（CLAUDE.md、.claude/ 规则、hooks、权限）",
  codebuddy: "读取 CodeBuddy 设置（hooks、权限等；它不读 CLAUDE.md，项目规则总由 CrossAgents 注入）",
};

function settingSourcesOf(agent: Agent): string | null {
  const value = (agent.config as Record<string, unknown> | null)?.setting_sources;
  return typeof value === "string" ? value : null;
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
