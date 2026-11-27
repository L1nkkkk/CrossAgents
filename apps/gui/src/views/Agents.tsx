import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { AgentProfile } from "@bindings/AgentProfile";
import { ArrowUpRight, RefreshCw } from "lucide-react";
import { api, errorText } from "../api";
import {
  AVATAR_COLORS,
  Avatar,
  CopyCommand,
  ErrorBox,
  Field,
  Prose,
  StatusChip,
  Vendor,
  VendorMark,
  agentTone,
  sessionLabel,
  sessionHref,
  useAgents,
} from "../components";
import { onHubEvent, useHubData, usePlugins } from "../hub";
import { useArmed } from "../parts";
import { AvatarChoice } from "./AvatarGallery";
import { ConnectExternal, ConnectInstructions } from "./External";
import { prettyModel, vendorLabel } from "../lib/models";
import { cursorMode, reasoningModels, usesVariantReasoning } from "../lib/reasoning";
import { installBusy, installingLabel, installingOf, installRefusedText } from "../lib/installState";
import { permissionOf, type PermissionChoice } from "../lib/permission";
import { useOperation } from "../lib/features";
import { EgressPicker, proxyOf } from "../egressPicks";
import { PermissionPicks } from "../permissionPicks";
import { unusableOf } from "../lib/unusable";
import type { ConnectInfo } from "@bindings/ConnectInfo";
import type { CliInfo } from "@bindings/CliInfo";

export function AgentsPage() {
  const agents = useAgents();
  const runs = useHubData(
    () => api.listRuns().then((r) => new Set(r.runs.map((x) => x.id))),
    [],
    (e) => e.type.startsWith("run."),
  );
  const [editing, setEditing] = useState<"external" | Agent>();
  // Opened from a card's 「去设置权限」: the editor scrolls to the permission choice and focuses it.
  const [focusPermission, setFocusPermission] = useState(false);
  const [connect, setConnect] = useState<ConnectInfo>();
  const [query, setQuery] = useState("");
  const list = (agents.data ?? []).filter((a) => !a.archived_at);
  const external = list.filter((a) => a.plugin === "external");
  const managed = list.filter((a) => a.plugin !== "external");
  const needle = query.trim().replace(/^@/, "").toLowerCase();
  const visible = managed
    .filter((agent) =>
      !needle || (typeof editing === "object" && editing.id === agent.id) ||
      [agent.topic, sessionLabel(agent), agent.name, agent.home || agent.cwd, agent.model, vendorLabel(agent.plugin)]
        .some((value) => value?.toLowerCase().includes(needle)),
    )
    .sort((a, b) => Date.parse(b.created_at) - Date.parse(a.created_at));
  // A run that was deleted no longer exists to link to (docs/tasks/R1.md §3.2).
  const runIds = runs.data ?? new Set<string>();
  const edit = (agent: Agent, atPermission = false) => {
    setFocusPermission(atPermission);
    setEditing(agent);
  };
  const done = () => {
    setFocusPermission(false);
    setEditing(undefined);
  };
  const renderAgent = (agent: Agent) =>
    typeof editing === "object" && editing.id === agent.id ? (
      <ProfileEditor key={agent.id} agent={agent} focusPermission={focusPermission} onDone={done} />
    ) : (
      <SessionRow
        key={agent.id}
        agent={agent}
        runIds={runIds}
        onEdit={() => edit(agent)}
        onPermission={() => edit(agent, true)}
        onConnect={() => api.connectAgent(agent.id).then(setConnect)}
      />
    );

  return (
    <div className="team-page">
      <VendorClis />
      <ErrorBox error={agents.error} />
      <section className="settings-section">
        <div className="settings-head">
          <h2>外部智能体</h2>
          <div className="spacer" />
          {editing !== "external" && (
            <button className="ghost" onClick={() => setEditing("external")} title="让别处运行的智能体通过 MCP 接入">
              接入外部智能体
            </button>
          )}
        </div>
        {editing === "external" && <ConnectExternal onDone={done} />}
        {connect && <ConnectInstructions info={connect} onDone={() => setConnect(undefined)} />}
        {external.length > 0 && <div className="settings-session-list">{external.map(renderAgent)}</div>}
        {agents.data && external.length === 0 && editing !== "external" && (
          <p className="setting-hint">还没有接入外部智能体。</p>
        )}
      </section>
      {agents.data && (
        <details className="settings-sessions">
          <summary>托管会话 <span className="chip">{managed.length}</span></summary>
          <div className="settings-session-content">
            <p className="setting-hint">点击会话名查看记录。给已停止的会话发送新消息时，系统会尝试恢复上下文。</p>
            {managed.length > 0 && (
              <input
                type="search"
                aria-label="搜索托管会话"
                placeholder="搜索名称、项目或模型"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
              />
            )}
            {visible.length > 0 ? (
              <div className="settings-session-list">{visible.map(renderAgent)}</div>
            ) : (
              <p className="setting-hint">{managed.length ? "没有匹配的会话。" : "还没有托管会话。"}</p>
            )}
          </div>
        </details>
      )}
      <TeamNotes />
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
 * The shared list revalidates on window focus and when an old page is reopened;
 * the button asks the hub to look again and probe the versions,
 * so a CLI installed in a terminal shows up without restarting the hub.
 */
function VendorClis() {
  const plugins = usePlugins();
  const [confirm, setConfirm] = useState<Job>();
  const [jobs, setJobs] = useState<Record<string, Progress>>({});
  // A click the hub refused because that plugin already has a job: said in words, per row.
  const [refused, setRefused] = useState<{ plugin: string; text: string }>();
  // The POST is out; the probes themselves show in each row's `probing`.
  const [asking, setAsking] = useState(false);
  const [askError, setAskError] = useState<string>();
  // The self-check POST is out for this plugin; afterwards the row's `self_checking` says it.
  const [checking, setChecking] = useState<string>();
  const [checkError, setCheckError] = useState<{ plugin: string; text: string }>();
  const reload = plugins.reload;

  const selfCheck = useCallback(
    (plugin: string) => {
      setChecking(plugin);
      setCheckError(undefined);
      api
        .selfCheckPlugin(plugin)
        .catch((err) => setCheckError({ plugin, text: errorText(err) }))
        .finally(() => {
          setChecking(undefined);
          reload();
        });
    },
    [reload],
  );

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

  // hub.ts shares focus/visibility refreshes with Home and the other pickers. These
  // only read the list; a full version probe remains the person's explicit action.

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
          // The job it said to wait for ended: the note has nothing left to say.
          setRefused((said) => (said?.plugin === plugin ? undefined : said));
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
    setRefused(undefined);
    try {
      if (job.action === "install") await api.installPlugin(job.plugin);
      else await api.upgradePlugin(job.plugin);
    } catch (err) {
      // Already running (409): no job of ours to show. Say to wait, and read the list again
      // so the row says what the hub is doing and keeps its buttons disabled.
      if (installBusy(err)) {
        setRefused({ plugin: job.plugin, text: installRefusedText(err, job.action) });
        reload();
        return;
      }
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
          // What the hub is doing for this CLI now: a job of this window, or one started
          // elsewhere and reported on `GET /v1/plugins`. Its buttons stay disabled either way.
          const busy = installingOf(cli, job);
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
                {/* What its description says about that version (docs/DESIGN.md §9.4):
                    below the minimum no agent is registered at all; outside the series the
                    driver was checked against it still runs, marked. */}
                {cli.below_minimum && (
                  <div>
                    <span
                      className="chip small danger"
                      title={`低于描述文件的最低版本，这个 agent 不会被注册；升级到 ${cli.min_version} 或更新后才能用`}
                    >
                      版本过低（需 ≥ {cli.min_version}）
                    </span>
                  </div>
                )}
                {!cli.below_minimum && <VersionStanding cli={cli} />}
              </div>
              <div className="cli-actions">
                {job ? (
                  <InstallProgress job={job} />
                ) : (
                  <>
                    {busy && (
                      <span className="chip small" title="hub 正在执行它的安装命令，等它结束">
                        {installingLabel(busy)}…
                      </span>
                    )}
                    {cli.installed ? (
                      <>
                        {cli.version && !cli.below_minimum && (
                          <button
                            className="ghost"
                            disabled={!!busy || cli.self_checking || checking === p.name}
                            onClick={() => selfCheck(p.name)}
                            title="用这个 CLI 开一个很短的会话（会用掉一点额度），看这个版本还能不能和 CrossAgents 对上"
                          >
                            {cli.self_checking || checking === p.name ? "自检中…" : "立即自检"}
                          </button>
                        )}
                        {cli.can_upgrade ? (
                          <button
                            className="ghost"
                            disabled={!!busy}
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
                          disabled={!!busy}
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
                  </>
                )}
              </div>
              {refused?.plugin === p.name && <div className="cli-done">{refused.text}</div>}
              {checkError?.plugin === p.name && <div className="cli-done bad">{checkError.text}</div>}
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

/**
 * How this version has done here (docs/DESIGN.md §9.4): runs on this machine say more than the
 * series the driver was verified on. A version that failed on the protocol says so and names
 * the last one that worked; one that ran says so; one nobody ran yet says that.
 */
function VersionStanding({ cli }: { cli: CliInfo }) {
  const series = (cli.verified ?? []).join("、");
  const when = (at?: string) => (at ? new Date(at).toLocaleString() : "");
  switch (cli.version_status) {
    case "protocol_failed": {
      const text = cli.last_ok_at
        ? `${cli.version} 最近一次运行在协议上失败了，可能与 CrossAgents 不兼容`
        : `CLI 升级到 ${cli.version} 后首次运行即失败，可能与 CrossAgents 不兼容`;
      return (
        <div className="setting-hint" title={cli.protocol_fail_message ?? undefined}>
          <span className="chip small danger">可能不兼容</span> {text}
          {cli.last_good_version ? `；上一个可用版本是 ${cli.last_good_version}` : ""}
          {cli.last_protocol_fail_at ? `（${when(cli.last_protocol_fail_at)}）` : ""}
        </div>
      );
    }
    case "known_good":
      return (
        <div>
          <span className="chip small ok" title={`在这台机器上跑通过：${when(cli.last_ok_at)}`}>
            已跑通
          </span>
        </div>
      );
    case "untried":
      return (
        <div>
          <span
            className="chip small warn"
            title={
              series
                ? `驱动只在 ${series} 上验证过；这个版本还没在这里跑过，第一次运行或「立即自检」后就知道了`
                : "这个版本还没在这里跑过，第一次运行或「立即自检」后就知道了"
            }
          >
            这个版本还没跑过
          </span>
        </div>
      );
    case "verified":
      return null;
    default:
      // A hub that does not report runs yet: only the series says anything.
      return cli.unverified ? (
        <div>
          <span className="chip small warn" title={`驱动只在 ${series} 上验证过，这个版本仍可运行，但可能对不上它的接口`}>
            未验证版本
          </span>
        </div>
      ) : null;
  }
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

function SessionRow({
  agent,
  runIds,
  onEdit,
  onPermission,
  onConnect,
}: {
  agent: Agent;
  /** The runs that still exist; a chip links to one of those only. */
  runIds: Set<string>;
  onEdit: () => void;
  /** Opens the editor at its 权限 choice: the fix for a Cursor agent that cannot guard here. */
  onPermission: () => void;
  onConnect: () => void;
}) {
  const spawnedBy = (agent.config as Record<string, unknown> | null)?.spawned_by_run;
  const runId = typeof spawnedBy === "string" ? spawnedBy : undefined;
  const label = agent.topic?.trim() || sessionLabel(agent);
  const workspace = agent.home || agent.cwd;
  const project = workspace.split(/[\\/]/).filter(Boolean).at(-1) ?? workspace;
  const unusable = unusableOf(agent);
  return (
    <article className="settings-session" data-unusable={unusable ? "" : undefined}>
      <div className="settings-session-row">
        <div className="settings-session-identity">
          <Avatar agent={agent} size={30} />
          <div className="settings-session-who">
            <a className="settings-session-link" href={sessionHref(agent)} title={`打开会话：${label}`}>
              <span>{label}</span>
              <ArrowUpRight size={14} aria-hidden />
            </a>
            <div className="settings-session-context">
              {project && <span className="muted" title={workspace}>{project}</span>}
              {runId && runIds.has(runId) && <a href={`#/runs/${runId}`}>所属工作</a>}
              <Vendor plugin={agent.plugin} model={agent.model} title={agent.driver ?? undefined} />
            </div>
          </div>
        </div>
        <div className="settings-session-actions">
          <StatusChip status={agent.status} tone={agentTone(agent.status)} />
          {agent.plugin === "external" && (
            <button className="ghost small" onClick={onConnect} title="重新生成令牌和 MCP 配置">
              连接配置
            </button>
          )}
          <button className="ghost small" onClick={onEdit} aria-label={`编辑 ${label} 的设置`}>设置</button>
        </div>
      </div>
      {unusable && (
        <p className="member-unusable" title={unusable}>
          <span className="chip small danger">不可用</span>
          <span className="member-unusable-text">{unusable}</span>
          {/* The Cursor-on-Windows reason names the fix: a link straight to this agent's 权限. */}
          {agent.plugin === "cursor" && unusable.includes("Cursor cannot use this permission on Windows") && (
            <button
              type="button"
              className="ghost small"
              style={{ flex: "none" }}
              onClick={onPermission}
              title="打开这个 agent 的权限设置：改成「自动执行」它就能在这台机器上用了"
            >
              去设置权限
            </button>
          )}
        </p>
      )}
    </article>
  );
}

/** Edits an agent's card: name, face, persona, model. Agents are made by the start page or by a lead. */
export function ProfileEditor({ agent, onDone, focusPermission }: { agent: Agent; onDone: () => void; focusPermission?: boolean }) {
  const plugins = usePlugins();
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
  const savedHooks = allowHooksOf(agent);
  const [hooks, setHooks] = useState(savedHooks);
  const savedPermission = permissionOf(agent);
  const [permission, setPermission] = useState<PermissionChoice>(savedPermission);
  const savedProxy = proxyOf(agent);
  const [proxy, setProxy] = useState<string | null>(savedProxy);
  // Settings (permission, hooks, setting sources) are admin since #retier; a write device's 403
  // greys these fields with the hub's reason instead of failing on save.
  const settings = useOperation("agent.settings");
  const settingsDenied = settings.refused ? `改 agent 的设置需要「管理」（admin）级别，这个凭据不够。hub 说：${settings.reason}` : undefined;
  const permissionRef = useRef<HTMLDivElement>(null);
  const set = (patch: Partial<AgentProfile>) => setProfile((p) => ({ ...p, ...patch }));
  const preview = { ...agent, name: name.trim() || agent.name, profile } as Agent;

  // Arriving from a card's 「去设置权限」: the choice is the point of opening this form.
  useEffect(() => {
    if (!focusPermission) return;
    const el = permissionRef.current;
    if (!el) return;
    el.scrollIntoView({ block: "center" });
    el.querySelector<HTMLInputElement>("input:checked:not(:disabled)")?.focus();
  }, [focusPermission]);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    try {
      // By id: a rename in the same save must not lose the agent.
      if (name.trim() && name.trim() !== agent.name) await api.renameAgent(agent.id, name.trim());
      await api.updateProfile(agent.id, { ...profile, display_name: null });
      if ((modelValue.trim() || null) !== agent.model) await api.setModel(agent.id, modelValue.trim() || null);
      const settingsChanged =
        !settingsDenied &&
        ((claudeLike && (sources !== savedSources || hooks !== savedHooks)) ||
          permission !== savedPermission ||
          proxy !== savedProxy);
      if (settingsChanged) {
        await api.agentSettings(agent.id, {
          // A plugin that reads no settings files keeps whatever it had; the field is required.
          setting_sources: claudeLike ? sources : savedSources,
          ...(claudeLike ? { allow_hooks: hooks } : {}),
          permission,
          proxy,
        });
      }
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
            <Field label="备注（可选）">
              <input value={profile.title ?? ""} onChange={(e) => set({ title: e.target.value })} placeholder="比如：管前端" />
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

      <Field label="给模型的开场说明（专长与工作方式）">
        <textarea rows={4} value={profile.persona ?? ""} onChange={(e) => set({ persona: e.target.value })} placeholder="不填也行；主导它的 agent 招人时会自己写。" />
      </Field>

      <ModelField value={modelValue} models={reasoningModels(agent.plugin, models, modelValue)} plugin={agent.plugin} onChange={setModel} />
      <Field label="网络出口（IP / 代理；保存后空闲会话会重启以生效）">
        <EgressPicker
          value={proxy}
          onChange={setProxy}
          defaultLabel="跟随账号 / 系统默认"
          disabled={!!settingsDenied}
          title={settingsDenied}
        />
      </Field>
      {claudeLike && (
        <Field label={`${agent.plugin === "codebuddy" ? SOURCES_LABEL.codebuddy : SOURCES_LABEL.claude}；保存后空闲的会话会重启并续上原对话，忙时等这轮结束后下次启动`}>
          <select value={sources ?? "default"} disabled={!!settingsDenied} title={settingsDenied} onChange={(e) => setSources(e.target.value === "default" ? null : e.target.value)}>
            {(agent.plugin === "codebuddy" ? CODEBUDDY_SOURCES : SETTING_SOURCES).map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </select>
        </Field>
      )}
      {claudeLike && (
        <label className="field-check" title={settingsDenied ?? "hooks 是设置文件里让 CLI 自己跑的命令，不经过 hub 的审批"} style={settingsDenied ? { opacity: 0.55 } : undefined}>
          <input type="checkbox" checked={hooks} disabled={!!settingsDenied} onChange={(e) => setHooks(e.target.checked)} />
          允许 hooks（默认关闭）：打开后设置文件里的 hooks 会在它的会话里运行；关闭时随 --settings 一起关掉，其余设置照读
        </label>
      )}
      {/* The agent's own answer for dangerous actions, over the run's approvals (perm-hub). */}
      <div className="field">
        <span>权限（危险操作怎么处理）</span>
        <PermissionPicks
          value={permission}
          onChange={setPermission}
          plugin={agent.plugin}
          groupRef={permissionRef}
          radioName={`permission-${agent.id}`}
        />
      </div>
      <p className="muted small" style={{ margin: 0 }}>
        @{agent.name} · {agent.plugin}
        {agent.driver ? ` · ${agent.driver}` : ""} · 新的开场说明在它下次开会话时生效；模型从下一轮起生效。
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

/** `config.allow_hooks` (SELF.md §B5): off unless someone turned it on for this agent. */
function allowHooksOf(agent: Agent): boolean {
  return (agent.config as Record<string, unknown> | null)?.allow_hooks === true;
}

/**
 * The plugin's models as a list; free text when it lists none or the value is not one of them.
 * Cursor hides its mode in the ID, so its rows show the base model while the value stays the
 * exact variant (`models` already holds one variant per base, the selected one when it is set).
 */
function ModelField({ value, models, plugin, onChange }: { value: string; models: string[]; plugin: string; onChange: (m: string) => void }) {
  const listed = !value || models.includes(value);
  return (
    <Field label="模型（背后的大脑）">
      {models.length && listed ? (
        <select value={value} onChange={(e) => onChange(e.target.value)}>
          {models.map((m) => (
            <option key={m} value={m}>
              {usesVariantReasoning(plugin) ? prettyModel(cursorMode(m).base) : m}
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
