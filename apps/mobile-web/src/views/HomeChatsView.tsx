import { useEffect, useMemo, useState } from "react";
import {
  Bell,
  ChevronDown,
  ChevronRight,
  Folder,
  MessageSquare,
  Plus,
  RefreshCw,
  Search,
  X,
} from "lucide-react";
import {
  AgentAvatar,
  agentTone,
  BrandMark,
  ModelMark,
  OsIcon,
  prettyModel,
  UserAvatar,
  vendorName,
} from "../brands";
import {
  agentMachineKey,
  CHATS_GROUP_KEY,
  homeOf,
  isStandaloneChat,
  loadCollapsed,
  nodeDisplayName,
  relativeTime,
  saveCollapsed,
  shortWorkspace,
  wsKey,
  wsName,
} from "../store";
import type { HubTransport, LinkStatus } from "../transport";
import type { Agent, HubInfo, HumanItem, RemoteNodeInfo, UserProfile } from "../types";

interface MobileSearchHit {
  agent_id: string;
  agent_name: string;
  topic?: string | null;
  cwd: string;
  archived: boolean;
  message_id?: string | null;
  role: string;
  snippet: string;
  ts: string;
  seq: number;
}

type FilterMode = "active" | "running" | "archived";

const FOLDER_CAP = 5;

function statusRank(s: Agent["status"]): number {
  if (s === "running") return 0;
  if (s === "starting") return 1;
  if (s === "error") return 2;
  if (s === "idle") return 3;
  return 4;
}

interface ProjectFolder {
  key: string;
  foldKey: string;
  name: string;
  rawPath: string;
  isChats: boolean;
  agents: Agent[];
  pendingCount: number;
  runningCount: number;
  latestCreatedAt: string;
}

interface MachineTree {
  key: string;
  foldKey: string;
  title: string;
  os: string;
  online: boolean;
  isLocal: boolean;
  totalAgents: number;
  folders: ProjectFolder[];
}

export function HomeChatsView({
  agents,
  nodes,
  hubInfo,
  profile,
  fallbackHost,
  openQuestions,
  avatarUrls,
  snapshotSavedAt,
  isConnected,
  linkStatus,
  transport,
  refreshing,
  onRefresh,
  onOpenAgent,
  onOpenInbox,
  onOpenStatus,
  onOpenNewChat,
  onEnsureArchivedAvatars,
}: {
  agents: Agent[];
  nodes: RemoteNodeInfo[];
  hubInfo?: HubInfo;
  profile?: UserProfile;
  fallbackHost?: string;
  openQuestions: HumanItem[];
  avatarUrls: Record<string, string>;
  snapshotSavedAt?: string;
  isConnected: boolean;
  linkStatus?: LinkStatus;
  transport: HubTransport | null;
  refreshing: boolean;
  onRefresh: () => void;
  onOpenAgent: (agentId: string) => void;
  onOpenInbox: () => void;
  onOpenStatus: () => void;
  onOpenNewChat: (preset?: { node?: string; cwd?: string }) => void;
  onEnsureArchivedAvatars: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<FilterMode>("active");
  const [collapsed, setCollapsed] = useState<Set<string>>(() => loadCollapsed());
  const [expandedFolders, setExpandedFolders] = useState<Record<string, boolean>>({});
  const [contentHits, setContentHits] = useState<MobileSearchHit[]>([]);
  const [contentSearching, setContentSearching] = useState(false);

  useEffect(() => {
    const q = query.trim();
    if (!transport || !isConnected || q.length < 2) {
      setContentHits([]);
      setContentSearching(false);
      return;
    }
    let live = true;
    setContentSearching(true);
    const timer = window.setTimeout(() => {
      void transport
        .request("GET", `/v1/search/sessions?q=${encodeURIComponent(q)}&archived=true&limit=12`)
        .then((res) => {
          if (!live) return;
          if (res.status < 400) {
            const parsed = JSON.parse(res.body) as { hits?: MobileSearchHit[] };
            setContentHits(parsed.hits ?? []);
          } else {
            setContentHits([]);
          }
          setContentSearching(false);
        })
        .catch(() => {
          if (!live) return;
          setContentHits([]);
          setContentSearching(false);
        });
    }, 240);
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [query, transport, isConnected]);

  useEffect(() => {
    if (filter === "archived") onEnsureArchivedAvatars();
  }, [filter, onEnsureArchivedAvatars]);

  const toggleCollapsed = (key: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      saveCollapsed(next);
      return next;
    });
  };

  const pendingByAgent = useMemo(() => {
    const map = new Map<string, number>();
    for (const q of openQuestions) {
      map.set(q.from, (map.get(q.from) ?? 0) + 1);
    }
    return map;
  }, [openQuestions]);

  const counts = useMemo(() => {
    let active = 0;
    let running = 0;
    let archived = 0;
    for (const a of agents) {
      if (a.archived_at) {
        archived++;
      } else {
        active++;
        if (a.status === "running" || a.status === "starting") running++;
      }
    }
    return { active, running, archived };
  }, [agents]);

  const searching = query.trim().length > 0;

  const filteredAgents = useMemo(() => {
    const q = query.trim().toLowerCase();
    return agents.filter((a) => {
      if (filter === "active" && a.archived_at) return false;
      if (filter === "running" && (a.archived_at || (a.status !== "running" && a.status !== "starting"))) {
        return false;
      }
      if (filter === "archived" && !a.archived_at) return false;
      if (!q) return true;
      const nName = nodeDisplayName(a.node, nodes, hubInfo, fallbackHost).toLowerCase();
      return (
        a.name.toLowerCase().includes(q) ||
        (a.topic ?? "").toLowerCase().includes(q) ||
        shortWorkspace(a).toLowerCase().includes(q) ||
        (a.model ?? "").toLowerCase().includes(q) ||
        a.plugin.toLowerCase().includes(q) ||
        nName.includes(q)
      );
    });
  }, [agents, filter, query, nodes, hubInfo, fallbackHost]);

  // 1:1 Desktop Sidebar tree (`buildMachineGroups` → Machine → Collapsible Project Folders + 独立会话)
  const machineTrees = useMemo<MachineTree[]>(() => {
    if (filter === "archived") return [];

    const machineKeys: string[] = ["local"];
    const seen = new Set<string>(machineKeys);
    for (const n of nodes) {
      if (n.local) continue;
      const k = n.id.trim();
      if (k && !seen.has(k)) {
        seen.add(k);
        machineKeys.push(k);
      }
    }
    for (const a of filteredAgents) {
      const k = agentMachineKey(a);
      if (!seen.has(k)) {
        seen.add(k);
        machineKeys.push(k);
      }
    }

    const trees: MachineTree[] = [];
    for (const mKey of machineKeys) {
      const mAgents = filteredAgents.filter((a) => agentMachineKey(a) === mKey);
      const nodeInfo = nodes.find((n) =>
        mKey === "local" ? n.local : n.id === mKey || n.name === mKey || n.hostname === mKey,
      );
      const isLocal = mKey === "local" || Boolean(nodeInfo?.local);
      const online = nodeInfo ? nodeInfo.online : isLocal;

      // Hide empty machines while searching or filtering running; keep online remote nodes visible on `active`
      if (mAgents.length === 0 && (searching || filter !== "active" || (!isLocal && !online))) {
        continue;
      }
      if (mAgents.length === 0 && isLocal && filteredAgents.length === 0) {
        continue;
      }

      const folderMap = new Map<string, { name: string; rawPath: string; isChats: boolean; list: Agent[] }>();
      for (const a of mAgents) {
        if (isStandaloneChat(a)) {
          const entry = folderMap.get(CHATS_GROUP_KEY) ?? {
            name: "独立会话",
            rawPath: "",
            isChats: true,
            list: [],
          };
          entry.list.push(a);
          folderMap.set(CHATS_GROUP_KEY, entry);
        } else {
          const raw = homeOf(a);
          const k = wsKey(raw);
          const entry = folderMap.get(k) ?? {
            name: wsName(raw),
            rawPath: raw,
            isChats: false,
            list: [],
          };
          entry.list.push(a);
          folderMap.set(k, entry);
        }
      }

      const folders: ProjectFolder[] = Array.from(folderMap.entries()).map(([pKey, entry]) => {
        entry.list.sort((a, b) => {
          const pa = pendingByAgent.get(a.id) ?? 0;
          const pb = pendingByAgent.get(b.id) ?? 0;
          if (pa !== pb) return pb - pa;
          const r = statusRank(a.status) - statusRank(b.status);
          if (r !== 0) return r;
          return (b.created_at || "").localeCompare(a.created_at || "") || a.id.localeCompare(b.id);
        });
        let pendingCount = 0;
        let runningCount = 0;
        let latestCreatedAt = "";
        for (const a of entry.list) {
          pendingCount += pendingByAgent.get(a.id) ?? 0;
          if (a.status === "running" || a.status === "starting") runningCount++;
          if ((a.created_at || "") > latestCreatedAt) latestCreatedAt = a.created_at || "";
        }
        return {
          key: pKey,
          foldKey: isLocal ? pKey : `${mKey}:${pKey}`,
          name: entry.name,
          rawPath: entry.rawPath,
          isChats: entry.isChats,
          agents: entry.list,
          pendingCount,
          runningCount,
          latestCreatedAt,
        };
      });

      // Keep standalone chats last (like Desktop `sidebarTree.ts`), otherwise prioritize active/recent repos.
      folders.sort((a, b) => {
        if (a.isChats !== b.isChats) return a.isChats ? 1 : -1;
        if ((a.pendingCount > 0) !== (b.pendingCount > 0)) return a.pendingCount > 0 ? -1 : 1;
        if ((a.runningCount > 0) !== (b.runningCount > 0)) return a.runningCount > 0 ? -1 : 1;
        return b.latestCreatedAt.localeCompare(a.latestCreatedAt) || a.name.localeCompare(b.name);
      });

      trees.push({
        key: mKey,
        foldKey: `machine:${mKey}`,
        title: nodeDisplayName(mKey, nodes, hubInfo, fallbackHost),
        os: nodeInfo?.os ?? (isLocal ? hubInfo?.machine?.os ?? "windows" : "macos"),
        online,
        isLocal,
        totalAgents: mAgents.length,
        folders,
      });
    }

    return trees;
  }, [filter, filteredAgents, nodes, hubInfo, fallbackHost, searching, pendingByAgent]);

  const archivedList = useMemo(() => {
    if (filter !== "archived") return [];
    return filteredAgents
      .slice()
      .sort((a, b) => (b.archived_at ?? b.created_at ?? "").localeCompare(a.archived_at ?? a.created_at ?? ""));
  }, [filter, filteredAgents]);

  return (
    <>
      <div className="nav-bar">
        <div className="nav-brand">
          <BrandMark size={21} />
          <div className="nav-title-wrap">
            <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
              <h1 className="nav-title">CrossAgents</h1>
              <span className={`dot ${isConnected ? "ok" : "warn"}`} />
            </div>
            {!isConnected && snapshotSavedAt ? (
              <div className="nav-subtitle">缓存快照 · {relativeTime(snapshotSavedAt)}</div>
            ) : (
              <div className="nav-subtitle">
                <span>{hubInfo?.machine?.hostname || fallbackHost || "中心在线"}</span>
                {isConnected && linkStatus?.kind === "p2p" ? (
                  <span className="p2p-inline-badge">
                    P2P{linkStatus.p2pInfo && linkStatus.p2pInfo.rttMs > 0 ? ` · ${linkStatus.p2pInfo.rttMs}ms` : ""}
                  </span>
                ) : isConnected && linkStatus?.p2pState === "connecting" ? (
                  <span className="p2p-inline-badge muted">直连中…</span>
                ) : isConnected && linkStatus?.kind === "relay" ? (
                  <span className="p2p-inline-badge muted">中继</span>
                ) : null}
              </div>
            )}
          </div>
        </div>
        <button
          type="button"
          className="nav-avatar-btn"
          title={profile?.name?.trim() || "个人概览"}
          aria-label={profile?.name?.trim() || "个人概览"}
          onClick={onOpenStatus}
        >
          <UserAvatar image={profile?.avatar} size={29} />
          <span className={`nav-avatar-dot ${isConnected ? "ok" : "warn"}`} aria-hidden="true" />
        </button>
        <button type="button" className="icon-btn" title="刷新" onClick={onRefresh} disabled={refreshing}>
          <RefreshCw size={16} style={refreshing ? { animation: "spin 1s linear infinite" } : undefined} />
        </button>
        <button
          type="button"
          className="icon-btn primary"
          title="新建对话"
          disabled={!isConnected}
          onClick={() => onOpenNewChat()}
        >
          <Plus size={18} />
        </button>
      </div>

      <div className="page-scroll">
        {openQuestions.length > 0 && (
          <button type="button" className="pending-callout" onClick={onOpenInbox}>
            <Bell size={16} style={{ color: "var(--warn)", flex: "none" }} />
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontSize: 13.5, fontWeight: 600, color: "var(--text)" }}>
                {openQuestions.length} 项待你审批或回复
              </div>
              <div
                style={{
                  fontSize: 12,
                  color: "var(--text-2)",
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  marginTop: 1,
                }}
              >
                {openQuestions[0].text}
              </div>
            </div>
            <ChevronRight size={15} style={{ color: "var(--muted)", flex: "none" }} />
          </button>
        )}

        <div style={{ display: "flex", flexDirection: "column", gap: 8, marginBottom: 10 }}>
          <div className="search-bar">
            <Search size={14} />
            <input
              placeholder="搜索对话、项目、模型或历史消息…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {query && (
              <button type="button" className="icon-btn" style={{ width: 24, height: 24 }} onClick={() => setQuery("")}>
                <X size={13} />
              </button>
            )}
          </div>

          <div className="segmented">
            <button type="button" className={filter === "active" ? "active" : ""} onClick={() => setFilter("active")}>
              活跃 ({counts.active})
            </button>
            <button type="button" className={filter === "running" ? "active" : ""} onClick={() => setFilter("running")}>
              进行中 ({counts.running})
            </button>
            <button type="button" className={filter === "archived" ? "active" : ""} onClick={() => setFilter("archived")}>
              归档 ({counts.archived})
            </button>
          </div>
        </div>

        {searching && query.trim().length >= 2 && (contentSearching || contentHits.length > 0) && (
          <section className="m-search-hits" aria-label="历史消息命中">
            <div className="m-search-hits-head">
              <span>历史消息命中</span>
              <span style={{ fontSize: 11, color: "var(--muted)" }}>
                {contentSearching ? "检索中…" : `${contentHits.length} 条`}
              </span>
            </div>
            {contentHits.map((hit, i) => (
              <button
                key={`${hit.agent_id}-${hit.seq}-${i}`}
                type="button"
                className="m-search-hit-row"
                onClick={() => onOpenAgent(hit.agent_id)}
              >
                <div className="m-search-hit-top">
                  <span className="m-search-hit-name">{hit.agent_name}</span>
                  <span className="chip tiny">{hit.role === "user" ? "你" : hit.role === "topic" ? "主题" : "AI"}</span>
                  {hit.archived && <span className="chip tiny">归档</span>}
                  <span style={{ flex: 1 }} />
                  <span style={{ fontSize: 11, color: "var(--muted)" }}>{relativeTime(hit.ts)}</span>
                </div>
                <div className="m-search-hit-snippet">{hit.snippet}</div>
              </button>
            ))}
          </section>
        )}

        {filter === "archived" ? (
          archivedList.length === 0 ? (
            <div className="empty-state">
              <h3>{query ? "没有匹配的归档对话" : "暂无归档对话"}</h3>
              <p>{query ? "试试换个关键词，或切换顶部分组。" : "归档后的历史会话会收纳在这里。"}</p>
            </div>
          ) : (
            <div className="agent-list">
              {archivedList.map((agent) => {
                const imgUrl = agent.profile?.avatar_image ? avatarUrls[agent.profile.avatar_image] : undefined;
                const ws = isStandaloneChat(agent) ? "独立会话" : shortWorkspace(agent);
                const modelLabel = agent.model ? prettyModel(agent.model) : vendorName(agent.plugin);
                return (
                  <button key={agent.id} type="button" className="agent-row" onClick={() => onOpenAgent(agent.id)}>
                    <AgentAvatar agent={agent} imageUrl={imgUrl} size={36} />
                    <div className="agent-meta">
                      <div className="agent-row-top">
                        <span className="agent-name">{agent.name}</span>
                        <span className="chip tiny">归档</span>
                        <span style={{ flex: 1 }} />
                        {agent.archived_at && (
                          <span style={{ fontSize: 11, color: "var(--muted)" }}>{relativeTime(agent.archived_at)}</span>
                        )}
                      </div>
                      {agent.topic && <div className="agent-topic">{agent.topic}</div>}
                      <div className="agent-sub">
                        <span className="agent-model-inline">
                          <ModelMark plugin={agent.plugin} model={agent.model} size={12} />
                          <span>{modelLabel}</span>
                        </span>
                        {ws && (
                          <>
                            <span style={{ opacity: 0.45 }}>·</span>
                            <span className="agent-ws-inline">
                              <Folder size={11} style={{ flex: "none" }} />
                              <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{ws}</span>
                            </span>
                          </>
                        )}
                      </div>
                    </div>
                  </button>
                );
              })}
            </div>
          )
        ) : machineTrees.length === 0 ? (
          <div className="empty-state">
            <h3>{query ? "没有匹配的对话" : filter === "running" ? "当前没有运行中的对话" : "暂无对话"}</h3>
            <p>
              {query
                ? "试试换个关键词，或切换顶部分组。"
                : "点右上角「+」在中心电脑或远程节点上开启一个新对话。"}
            </p>
          </div>
        ) : (
          <div className="tree-root">
            {machineTrees.map((machine) => {
              const machineClosed = !searching && collapsed.has(machine.foldKey);
              return (
                <section key={machine.key} className="machine-section">
                  <div className="machine-head">
                    <button
                      type="button"
                      className="machine-toggle"
                      aria-expanded={!machineClosed}
                      disabled={searching}
                      onClick={() => toggleCollapsed(machine.foldKey)}
                    >
                      {machineClosed ? (
                        <ChevronRight size={13} className="tree-chev" />
                      ) : (
                        <ChevronDown size={13} className="tree-chev" />
                      )}
                      <OsIcon os={machine.os} size={13} />
                      <span className="machine-title">{machine.title}</span>
                      <span className="machine-count">{machine.totalAgents}</span>
                      {!machine.online && <span className="chip warn tiny">离线</span>}
                    </button>
                    {isConnected && machine.online && (
                      <button
                        type="button"
                        className="tree-add-btn"
                        title={`在 ${machine.title} 上新建对话`}
                        aria-label={`在 ${machine.title} 上新建对话`}
                        onClick={() => onOpenNewChat({ node: machine.key })}
                      >
                        <Plus size={14} />
                      </button>
                    )}
                  </div>

                  {!machineClosed && (
                    <div className="machine-body">
                      {machine.folders.length === 0 ? (
                        <div className="machine-empty-card">
                          {machine.online ? "暂无会话 · 点右侧 + 新建" : "节点离线"}
                        </div>
                      ) : (
                        machine.folders.map((folder) => {
                          const folderClosed = !searching && collapsed.has(folder.foldKey);
                          const showAll = searching || Boolean(expandedFolders[folder.foldKey]);
                          const keepAlive = (a: Agent) =>
                            (pendingByAgent.get(a.id) ?? 0) > 0 ||
                            a.status === "running" ||
                            a.status === "starting" ||
                            a.status === "error";
                          const visibleAgents = showAll
                            ? folder.agents
                            : folder.agents.filter((a, i) => i < FOLDER_CAP || keepAlive(a));
                          const hiddenCount = folder.agents.length - visibleAgents.length;
                          const canCollapse =
                            !searching &&
                            folder.agents.filter((a, i) => i < FOLDER_CAP || keepAlive(a)).length < folder.agents.length;
                          const faces = folder.agents.slice(0, 3);

                          return (
                            <div key={folder.foldKey} className="project-card">
                              <div className={`project-head ${folderClosed ? "closed" : ""}`}>
                                <button
                                  type="button"
                                  className="project-toggle"
                                  aria-expanded={!folderClosed}
                                  disabled={searching}
                                  onClick={() => toggleCollapsed(folder.foldKey)}
                                >
                                  {folderClosed ? (
                                    <ChevronRight size={13} className="tree-chev" />
                                  ) : (
                                    <ChevronDown size={13} className="tree-chev" />
                                  )}
                                  {folder.isChats ? (
                                    <MessageSquare size={14} className="project-icon" />
                                  ) : (
                                    <Folder size={14} className="project-icon" />
                                  )}
                                  <span className="project-name">{folder.name}</span>
                                </button>

                                <div className="project-head-right">
                                  {folderClosed && faces.length > 0 && (
                                    <span className="face-pile" aria-hidden="true">
                                      {faces.map((a) => (
                                        <AgentAvatar
                                          key={a.id}
                                          agent={a}
                                          imageUrl={a.profile?.avatar_image ? avatarUrls[a.profile.avatar_image] : undefined}
                                          size={18}
                                        />
                                      ))}
                                    </span>
                                  )}
                                  {folder.pendingCount > 0 && (
                                    <span className="chip danger tiny">{folder.pendingCount} 待答</span>
                                  )}
                                  {folder.runningCount > 0 && (
                                    <span className="chip accent tiny">
                                      <span className="dot accent pulse" />
                                      {folder.runningCount}
                                    </span>
                                  )}
                                  <span className="project-count">{folder.agents.length}</span>
                                  {isConnected && machine.online && (
                                    <button
                                      type="button"
                                      className="tree-add-btn"
                                      title={`在 ${folder.name} 里新建`}
                                      aria-label={`在 ${folder.name} 里新建`}
                                      onClick={() =>
                                        onOpenNewChat({
                                          node: machine.key,
                                          cwd: folder.isChats ? "" : folder.rawPath,
                                        })
                                      }
                                    >
                                      <Plus size={14} />
                                    </button>
                                  )}
                                </div>
                              </div>

                              {!folderClosed && (
                                <div className="project-rows">
                                  {visibleAgents.map((agent) => {
                                    const pending = pendingByAgent.get(agent.id) ?? 0;
                                    const imgUrl = agent.profile?.avatar_image
                                      ? avatarUrls[agent.profile.avatar_image]
                                      : undefined;
                                    const modelLabel = agent.model ? prettyModel(agent.model) : vendorName(agent.plugin);
                                    const tone = agentTone(agent.status);
                                    return (
                                      <button
                                        key={agent.id}
                                        type="button"
                                        className="agent-row"
                                        onClick={() => onOpenAgent(agent.id)}
                                      >
                                        <AgentAvatar agent={agent} imageUrl={imgUrl} size={36} status />
                                        <div className="agent-meta">
                                          <div className="agent-row-top">
                                            <span className="agent-name">{agent.name}</span>
                                            <span style={{ flex: 1 }} />
                                            {pending > 0 ? (
                                              <span className="chip danger tiny">{pending} 待答</span>
                                            ) : tone.live || tone.color === "danger" ? (
                                              <span className={`chip tiny ${tone.color}`}>{tone.label}</span>
                                            ) : null}
                                          </div>
                                          {agent.topic && <div className="agent-topic">{agent.topic}</div>}
                                          <div className="agent-sub">
                                            <span className="agent-model-inline">
                                              <ModelMark plugin={agent.plugin} model={agent.model} size={12} />
                                              <span>{modelLabel}</span>
                                            </span>
                                          </div>
                                        </div>
                                      </button>
                                    );
                                  })}

                                  {canCollapse && (
                                    <button
                                      type="button"
                                      className="folder-more-btn"
                                      onClick={() =>
                                        setExpandedFolders((prev) => ({
                                          ...prev,
                                          [folder.foldKey]: !prev[folder.foldKey],
                                        }))
                                      }
                                    >
                                      {showAll ? "收起" : `更多 (${hiddenCount})`}
                                    </button>
                                  )}
                                </div>
                              )}
                            </div>
                          );
                        })
                      )}
                    </div>
                  )}
                </section>
              );
            })}
          </div>
        )}
      </div>
    </>
  );
}
