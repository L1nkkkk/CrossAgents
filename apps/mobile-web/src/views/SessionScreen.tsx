import { useEffect, useRef, useState } from "react";
import {
  ArrowLeft,
  ArrowUp,
  Check,
  ChevronUp,
  Copy,
  Hand,
  Paperclip,
  RefreshCw,
  Square,
  X,
} from "lucide-react";
import {
  AgentAvatar,
  agentTone,
  effortLabel,
  FamilyMark,
  groupModels,
  ModelMark,
  OsIcon,
  prettyModel,
  UserAvatar,
  vendorName,
} from "../brands";
import { DiffView, MarkdownView } from "../components/MarkdownView";
import { MobileMediaCard } from "../components/MobileMediaCard";
import {
  formatBytes,
  inferMime,
  mediaKindOfMime,
  mediaKindOfPath,
  MOBILE_FILE_ACCEPT,
  MOBILE_MAX_FILE_BYTES,
  MOBILE_MAX_FILES,
  MOBILE_MAX_TOTAL_BYTES,
} from "../media";
import {
  availableEfforts,
  extractErrorMessage,
  nodeDisplayName,
  prepareFileForUpload,
  relativeTime,
} from "../store";
import type { HubTransport } from "../transport";
import type {
  Agent,
  AttachmentUpload,
  HubInfo,
  HumanItem,
  PluginInfo,
  RemoteNodeInfo,
  ToolCallDetailResponse,
  ToolDetail,
  Transcript,
  TranscriptItem,
  UserProfile,
} from "../types";

const INITIAL_TURN_LIMIT = 12;
const OLDER_PAGE_SIZE = 15;

interface PendingAttachment {
  id: string;
  previewUrl?: string;
  size: number;
  upload: AttachmentUpload;
}

export function SessionScreen({
  agentId,
  agent,
  transcript,
  nodes,
  hubInfo,
  profile,
  avatarUrls,
  fallbackHost,
  pendingQuestions,
  transport,
  refreshKey = 0,
  onBack,
  onLoadedTranscript,
  onAnsweredQuestion,
}: {
  agentId: string;
  agent?: Agent;
  transcript?: Transcript;
  nodes: RemoteNodeInfo[];
  hubInfo?: HubInfo;
  profile?: UserProfile;
  avatarUrls: Record<string, string>;
  fallbackHost?: string;
  pendingQuestions: HumanItem[];
  transport: HubTransport | null;
  refreshKey?: number;
  onBack: () => void;
  onLoadedTranscript: (t: Transcript) => void;
  onAnsweredQuestion: (item: HumanItem) => void;
}) {
  const [draft, setDraft] = useState("");
  const [photos, setPhotos] = useState<PendingAttachment[]>([]);
  const [loading, setLoading] = useState(false);
  const [loadingOlder, setLoadingOlder] = useState(false);
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string>();
  const [attachmentUrls, setAttachmentUrls] = useState<Record<string, string>>({});
  const [toolDetails, setToolDetails] = useState<Record<string, { output?: string; detail?: ToolDetail }>>({});
  const [loadingTools, setLoadingTools] = useState<Record<string, boolean>>({});
  const [clientWindow, setClientWindow] = useState(INITIAL_TURN_LIMIT);
  const [zoomUrl, setZoomUrl] = useState<string>();
  const [modelModalOpen, setModelModalOpen] = useState(false);
  const [manualReload, setManualReload] = useState(0);

  const pendingMsgIdRef = useRef<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const messagesRef = useRef<HTMLDivElement>(null);
  const bottomRef = useRef<HTMLDivElement>(null);
  const initialScrollDoneRef = useRef(false);

  const activeAgent = transcript?.agent ?? agent;
  const agentImgUrl = activeAgent?.profile?.avatar_image ? avatarUrls[activeAgent.profile.avatar_image] : undefined;
  const nodeInfo = nodes.find((n) =>
    !activeAgent?.node || activeAgent.node === "local"
      ? n.local
      : n.id === activeAgent.node || n.name === activeAgent.node || n.hostname === activeAgent.node,
  );
  const nodeOs = nodeInfo?.os ?? hubInfo?.machine?.os ?? "windows";

  const isRunning =
    (transcript?.delivery && transcript.delivery.state !== "idle") ||
    activeAgent?.status === "running" ||
    activeAgent?.status === "starting";

  // Auto-grow composer textarea
  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 132)}px`;
  }, [draft]);

  useEffect(() => {
    setClientWindow(INITIAL_TURN_LIMIT);
    setToolDetails({});
    setLoadingTools({});
  }, [agentId]);

  useEffect(() => {
    if (!transport) return;
    let cancelled = false;
    setLoading(true);
    transport
      .request(
        "GET",
        `/v1/agents/${encodeURIComponent(agentId)}/transcript?limit=${INITIAL_TURN_LIMIT}&slim=true`,
      )
      .then((res) => {
        if (cancelled) return;
        if (res.status >= 400) throw new Error(extractErrorMessage(res.status, res.body));
        onLoadedTranscript(JSON.parse(res.body) as Transcript);
        setError(undefined);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [agentId, transport, refreshKey, manualReload]);

  // Visible slice: on new Hub (`has_more !== undefined`) server already sliced to latest turns;
  // on legacy Hub (full transcript returned), still window DOM to latest `clientWindow` turns!
  const allTurns = transcript?.turns ?? [];
  const serverPaginated = transcript?.has_more !== undefined;
  const visibleTurns = serverPaginated
    ? allTurns
    : allTurns.length > clientWindow
      ? allTurns.slice(-clientWindow)
      : allTurns;
  const hiddenClientCount = serverPaginated ? 0 : Math.max(0, allTurns.length - visibleTurns.length);
  const remainingCount = serverPaginated
    ? Math.max(0, (transcript?.total_turns ?? allTurns.length) - allTurns.length)
    : hiddenClientCount;
  const hasOlder = serverPaginated ? Boolean(transcript?.has_more) : hiddenClientCount > 0;

  async function loadOlder() {
    if (loadingOlder || !transcript) return;
    const container = messagesRef.current;
    const prevHeight = container?.scrollHeight ?? 0;
    const prevTop = container?.scrollTop ?? 0;
    const restoreScroll = () => {
      requestAnimationFrame(() => {
        if (!container) return;
        container.scrollTop = container.scrollHeight - prevHeight + prevTop;
      });
    };

    if (!serverPaginated) {
      setClientWindow((n) => n + OLDER_PAGE_SIZE);
      restoreScroll();
      return;
    }

    const oldest = transcript.turns[0];
    if (!oldest || !transport) return;
    setLoadingOlder(true);
    setError(undefined);
    try {
      const res = await transport.request(
        "GET",
        `/v1/agents/${encodeURIComponent(agentId)}/transcript?limit=${OLDER_PAGE_SIZE}&before=${encodeURIComponent(oldest.message_id)}&slim=true`,
      );
      if (res.status >= 400) throw new Error(extractErrorMessage(res.status, res.body));
      const older = JSON.parse(res.body) as Transcript;
      const seen = new Set(transcript.turns.map((t) => t.message_id));
      const uniqueOlder = older.turns.filter((t) => !seen.has(t.message_id));
      onLoadedTranscript({
        ...transcript,
        turns: [...uniqueOlder, ...transcript.turns],
        has_more: older.has_more ?? false,
        total_turns: older.total_turns ?? transcript.total_turns,
      });
      restoreScroll();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoadingOlder(false);
    }
  }

  function expandToolDetail(toolCallId: string) {
    if (!transport || !toolCallId || toolDetails[toolCallId] || loadingTools[toolCallId]) return;
    setLoadingTools((prev) => ({ ...prev, [toolCallId]: true }));
    void transport
      .request("GET", `/v1/agents/${encodeURIComponent(agentId)}/tools/${encodeURIComponent(toolCallId)}`)
      .then((res) => {
        if (res.status < 400) {
          const parsed = JSON.parse(res.body) as ToolCallDetailResponse;
          setToolDetails((prev) => ({
            ...prev,
            [toolCallId]: { output: parsed.output, detail: parsed.detail },
          }));
        }
      })
      .catch(() => {})
      .finally(() => {
        setLoadingTools((prev) => {
          const next = { ...prev };
          delete next[toolCallId];
          return next;
        });
      });
  }

  // Lazily fetch image attachments referenced in visible turns
  useEffect(() => {
    if (!transport || visibleTurns.length === 0) return;
    const toFetch: string[] = [];
    for (const turn of visibleTurns) {
      for (const att of turn.attachments ?? []) {
        if (att.mime.startsWith("image/") && !attachmentUrls[att.id]) {
          toFetch.push(att.id);
        }
      }
    }
    for (const attId of toFetch) {
      void transport
        .request("GET", `/v1/agents/${encodeURIComponent(agentId)}/attachments/${encodeURIComponent(attId)}`)
        .then((res) => {
          if (res.status < 400) {
            const parsed = JSON.parse(res.body) as { mime: string; data: string };
            setAttachmentUrls((prev) => ({
              ...prev,
              [attId]: `data:${parsed.mime};base64,${parsed.data}`,
            }));
          }
        })
        .catch(() => {});
    }
  }, [agentId, visibleTurns.length, transport]);

  const turnsCount = visibleTurns.length;
  const lastTurn = visibleTurns[turnsCount - 1];
  const lastTurnId = lastTurn?.message_id ?? "";
  const lastItemLen = lastTurn?.items.reduce((acc, it) => acc + (it.type === "output" ? it.text.length : 1), 0) ?? 0;
  const activeTool = lastTurn?.items
    .slice()
    .reverse()
    .find((it) => it.type === "tool_call" && it.status !== "completed" && it.status !== "failed");

  useEffect(() => {
    initialScrollDoneRef.current = false;
  }, [agentId]);

  // Only auto-scroll on initial open or when new content streams at the bottom (never when prepending older turns).
  useEffect(() => {
    if (!lastTurnId) return;
    if (!initialScrollDoneRef.current) {
      initialScrollDoneRef.current = true;
      bottomRef.current?.scrollIntoView({ behavior: "auto" });
    } else {
      bottomRef.current?.scrollIntoView({ behavior: "smooth" });
    }
  }, [lastTurnId, lastItemLen]);

  async function onPickFiles(files: FileList | null) {
    if (!files || files.length === 0) return;
    setError(undefined);
    const remaining = Math.max(0, MOBILE_MAX_FILES - photos.length);
    if (remaining === 0) {
      setError(`一条消息最多带 ${MOBILE_MAX_FILES} 个文件`);
      return;
    }
    const chosen = Array.from(files).slice(0, remaining);
    try {
      const next: PendingAttachment[] = [];
      let total = photos.reduce((acc, p) => acc + p.size, 0);
      for (const f of chosen) {
        const mime = inferMime(f.name, f.type);
        if (!mediaKindOfMime(mime)) {
          throw new Error(`${f.name}：暂不支持的文件类型（${mime || "未知"}）`);
        }
        if (f.size > MOBILE_MAX_FILE_BYTES) {
          throw new Error(`${f.name} 超过单个文件 ${formatBytes(MOBILE_MAX_FILE_BYTES)} 上限`);
        }
        const prepared = await prepareFileForUpload(f, mime);
        total += prepared.size;
        if (total > MOBILE_MAX_TOTAL_BYTES) {
          throw new Error(`附件总大小超过 ${formatBytes(MOBILE_MAX_TOTAL_BYTES)} 上限`);
        }
        next.push({ id: crypto.randomUUID(), ...prepared });
      }
      setPhotos((prev) => [...prev, ...next]);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  }

  async function send() {
    if (!transport || sending) return;
    const text = draft.trim();
    if (!text && photos.length === 0) return;

    const clientMessageId = pendingMsgIdRef.current ?? crypto.randomUUID();
    pendingMsgIdRef.current = clientMessageId;

    setSending(true);
    setError(undefined);
    try {
      const canSteer = Boolean(isRunning && transcript?.capabilities?.steer);
      const onlyImages = photos.length > 0 && photos.every((p) => p.upload.mime.startsWith("image/"));
      const fallbackText = onlyImages ? "请查看这张图片。" : "请查看我发送的附件。";
      const res = await transport.request("POST", `/v1/agents/${encodeURIComponent(agentId)}/messages`, {
        text: text || fallbackText,
        ...(canSteer ? { steer: true } : {}),
        ...(photos.length ? { attachments: photos.map((p) => p.upload) } : {}),
        client_message_id: clientMessageId,
      });
      if (res.status >= 400) throw new Error(extractErrorMessage(res.status, res.body));
      pendingMsgIdRef.current = null;
      setDraft("");
      setPhotos([]);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSending(false);
    }
  }

  async function cancelTurn() {
    if (!transport) return;
    try {
      await transport.request("POST", `/v1/agents/${encodeURIComponent(agentId)}/cancel`);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function sendNow(messageId: string) {
    if (!transport) return;
    try {
      await transport.request(
        "POST",
        `/v1/agents/${encodeURIComponent(agentId)}/messages/${encodeURIComponent(messageId)}/send-now`,
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  const hasInput = Boolean(draft.trim() || photos.length > 0);
  const activeEffort = typeof activeAgent?.config?.effort === "string" ? activeAgent.config.effort : "";
  const activeModelText = activeAgent?.model ? prettyModel(activeAgent.model) : activeAgent ? vendorName(activeAgent.plugin) : "";
  const tone = activeAgent ? agentTone(activeAgent.status) : null;

  return (
    <div className="app-shell session-shell">
      <div className="nav-bar">
        <button type="button" className="icon-btn" onClick={onBack} aria-label="返回">
          <ArrowLeft size={18} />
        </button>
        {activeAgent && <AgentAvatar agent={activeAgent} imageUrl={agentImgUrl} size={30} status />}
        <div className="nav-title-wrap">
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <h1 className="nav-title" style={{ fontSize: 15.5 }}>
              {activeAgent?.name ?? "对话"}
            </h1>
            {tone && tone.color !== "off" && (
              <span className={`chip tiny ${tone.color}`}>{tone.label}</span>
            )}
            {loading && transcript && (
              <span className="sync-pill" aria-label="同步中">
                <span className="sync-spinner" />
                同步中
              </span>
            )}
          </div>
          <div className="nav-subtitle">
            <OsIcon os={nodeOs} size={11} />
            <span>{nodeDisplayName(activeAgent?.node, nodes, hubInfo, fallbackHost)}</span>
            {activeAgent && (
              <>
                <span style={{ opacity: 0.45 }}>·</span>
                <ModelMark plugin={activeAgent.plugin} model={activeAgent.model} size={11} />
                <span style={{ overflow: "hidden", textOverflow: "ellipsis" }}>{activeModelText}</span>
              </>
            )}
          </div>
        </div>
        <button
          type="button"
          className="icon-btn"
          title="重新同步对话"
          aria-label="重新同步对话"
          disabled={loading || !transport}
          onClick={() => setManualReload((n) => n + 1)}
        >
          <RefreshCw size={16} style={loading ? { animation: "spin 1s linear infinite" } : undefined} />
        </button>
      </div>

      {error && (
        <div className="status-banner danger" style={{ paddingTop: 7 }}>
          <span className="status-banner-msg">{error}</span>
          <button
            type="button"
            className="status-banner-btn"
            onClick={() => {
              setError(undefined);
              setManualReload((n) => n + 1);
            }}
          >
            重试
          </button>
        </div>
      )}

      <div className="session-messages" ref={messagesRef}>
        {loading && !transcript ? (
          <div className="empty-state">
            <p>正在同步最近对话…</p>
          </div>
        ) : !transcript || visibleTurns.length === 0 ? (
          <div className="empty-state">
            {activeAgent && <AgentAvatar agent={activeAgent} imageUrl={agentImgUrl} size={48} />}
            <h3>{activeAgent?.name ?? "开始对话"}</h3>
            <p>在下方输入指令或发送截图即可开始。</p>
          </div>
        ) : (
          <>
            {hasOlder && (
              <div className="load-older-wrap">
                <button
                  type="button"
                  className="load-older-btn"
                  disabled={loadingOlder}
                  onClick={() => void loadOlder()}
                >
                  <ChevronUp size={13} />
                  <span>
                    {loadingOlder
                      ? "正在加载更早的对话…"
                      : remainingCount > 0
                        ? `加载更早的对话（还有 ${remainingCount} 轮）`
                        : "加载更早的对话"}
                  </span>
                </button>
              </div>
            )}
            {visibleTurns.map((turn) => {
            const isQueued = !turn.started_at && !turn.completed_at && !turn.withdrawn;
            const plainReply = turn.items
              .filter((it): it is Extract<TranscriptItem, { type: "output" }> => it.type === "output" && it.stream === "text")
              .map((it) => it.text)
              .join("\n\n");

            return (
              <div key={turn.message_id} className="turn-block">
                {(turn.input || (turn.attachments && turn.attachments.length > 0)) && (
                  <div className="person-turn">
                    <div className="person-turn-col">
                      {profile?.name?.trim() && (
                        <div className="person-turn-name">{profile.name.trim()}</div>
                      )}
                      {turn.attachments && turn.attachments.length > 0 && (
                        <div style={{ display: "flex", gap: 6, flexWrap: "wrap", justifyContent: "flex-end", width: "100%" }}>
                          {turn.attachments.map((att) => {
                            if (att.mime.startsWith("image/") && attachmentUrls[att.id]) {
                              return (
                                <img
                                  key={att.id}
                                  src={attachmentUrls[att.id]}
                                  alt={att.name}
                                  onClick={() => setZoomUrl(attachmentUrls[att.id])}
                                  style={{
                                    width: 112,
                                    height: 112,
                                    objectFit: "cover",
                                    borderRadius: 10,
                                    border: "1px solid var(--border)",
                                    cursor: "pointer",
                                  }}
                                />
                              );
                            }
                            const kind = mediaKindOfMime(att.mime) ?? mediaKindOfPath(att.name) ?? "file";
                            return (
                              <MobileMediaCard
                                key={att.id}
                                source={{ kind: "attachment", agentId, attId: att.id, mime: att.mime }}
                                mediaKind={kind}
                                label={att.name}
                                sizeHint={att.size}
                                transport={transport}
                                onZoomImage={setZoomUrl}
                              />
                            );
                          })}
                        </div>
                      )}
                      {turn.input && (
                        <div className={`bubble ${turn.withdrawn ? "withdrawn" : ""}`}>{turn.input}</div>
                      )}
                      <div className="bubble-meta">
                        {turn.withdrawn && <span>已撤回</span>}
                        {isQueued && (
                          <>
                            <span style={{ color: "var(--warn-text)" }}>排队中</span>
                            <button
                              type="button"
                              className="chip accent tiny"
                              onClick={() => void sendNow(turn.message_id)}
                            >
                              立即插队
                            </button>
                          </>
                        )}
                        <span>{relativeTime(turn.sent_at)}</span>
                      </div>
                    </div>
                    <UserAvatar image={profile?.avatar} size={26} />
                  </div>
                )}

                {turn.items.length > 0 && (
                  <div className="assistant-turn">
                    {activeAgent && (
                      <div className="assistant-head">
                        <AgentAvatar agent={activeAgent} imageUrl={agentImgUrl} size={20} />
                        <span className="assistant-head-name">{activeAgent.name}</span>
                        <span className="assistant-head-model">
                          <ModelMark plugin={activeAgent.plugin} model={activeAgent.model} size={11} />
                          <span>{activeModelText}</span>
                        </span>
                      </div>
                    )}
                    <div className="assistant-body">
                      {turn.items.map((item, idx) => (
                        <ItemView
                          key={idx}
                          item={item}
                          agentId={agentId}
                          transport={transport}
                          onZoomImage={setZoomUrl}
                          loadedTool={item.type === "tool_call" ? toolDetails[item.tool_call_id] : undefined}
                          loadingTool={item.type === "tool_call" ? Boolean(loadingTools[item.tool_call_id]) : false}
                          onExpandTool={expandToolDetail}
                        />
                      ))}
                    </div>
                    {plainReply && turn.completed_at && <CopyReplyRow text={plainReply} />}
                  </div>
                )}
              </div>
            );
          })}
          </>
        )}

        {transcript?.error && <div className="error-box">{transcript.error}</div>}

        {isRunning && (
          <div className="working-live">
            <div className="working-row">
              <span className="working-spark" aria-hidden="true">
                ✦
              </span>
              <span>{transcript?.delivery.state === "starting" ? "正在启动 Agent…" : "正在思考并执行…"}</span>
            </div>
            {activeTool && activeTool.type === "tool_call" && (
              <div className="working-now">{activeTool.title}</div>
            )}
          </div>
        )}
        <div ref={bottomRef} />
      </div>

      {pendingQuestions[0] && transport && (
        <InlineQuestionBanner
          question={pendingQuestions[0]}
          transport={transport}
          onAnswered={onAnsweredQuestion}
        />
      )}

      <div className="composer-dock">
        <div className="composer-box">
          {photos.length > 0 && (
            <div className="composer-photos">
              {photos.map((p) =>
                p.previewUrl ? (
                  <div key={p.id} className="composer-photo-item">
                    <img src={p.previewUrl} alt={p.upload.name} />
                    <button
                      type="button"
                      className="composer-photo-del"
                      onClick={() => setPhotos((prev) => prev.filter((x) => x.id !== p.id))}
                    >
                      <X size={11} />
                    </button>
                  </div>
                ) : (
                  <div key={p.id} className="composer-file-chip">
                    <span className="composer-file-ext">
                      {(p.upload.name.split(".").pop() || "FILE").slice(0, 4).toUpperCase()}
                    </span>
                    <span className="composer-file-name">{p.upload.name}</span>
                    <span className="composer-file-size">{formatBytes(p.size)}</span>
                    <button
                      type="button"
                      className="composer-file-del"
                      onClick={() => setPhotos((prev) => prev.filter((x) => x.id !== p.id))}
                    >
                      <X size={12} />
                    </button>
                  </div>
                ),
              )}
            </div>
          )}

          <textarea
            ref={textareaRef}
            className="composer-textarea"
            rows={1}
            placeholder={isRunning ? "继续补充或排队发送…" : `发消息给 ${activeAgent?.name ?? "Agent"}…`}
            value={draft}
            onChange={(e) => {
              setDraft(e.target.value);
              pendingMsgIdRef.current = null;
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
                e.preventDefault();
                void send();
              }
            }}
          />

          <div className="composer-toolbar">
            <input
              ref={fileInputRef}
              type="file"
              accept={MOBILE_FILE_ACCEPT}
              multiple
              style={{ display: "none" }}
              onChange={(e) => void onPickFiles(e.target.files)}
            />
            <button
              type="button"
              className="composer-tool-btn icon-only"
              disabled={photos.length >= MOBILE_MAX_FILES || sending || !transport}
              onClick={() => fileInputRef.current?.click()}
              title="选择图片、音频、视频、PDF 或文本文件"
            >
              <Paperclip size={15} />
            </button>

            {activeAgent && (
              <button
                type="button"
                className="composer-tool-btn"
                disabled={!transport}
                onClick={() => setModelModalOpen(true)}
                title="切换模型与推理力度"
              >
                <ModelMark plugin={activeAgent.plugin} model={activeAgent.model} size={13} />
                <span className="composer-model-label">
                  {activeModelText}
                  {activeEffort ? ` · ${effortLabel(activeEffort)}` : ""}
                </span>
              </button>
            )}

            {isRunning && !hasInput ? (
              <button
                type="button"
                className="composer-send-btn stop"
                onClick={() => void cancelTurn()}
                title="中断本轮"
              >
                <Square size={13} fill="currentColor" />
              </button>
            ) : (
              <button
                type="button"
                className="composer-send-btn"
                disabled={!hasInput || sending || !transport}
                onClick={() => void send()}
                title="发送"
              >
                <ArrowUp size={17} strokeWidth={2.2} />
              </button>
            )}
          </div>
        </div>
      </div>

      {zoomUrl && (
        <div
          className="qr-fullscreen"
          style={{ justifyContent: "center", alignItems: "center", padding: 16 }}
          onClick={() => setZoomUrl(undefined)}
        >
          <img src={zoomUrl} alt="预览" style={{ maxWidth: "100%", maxHeight: "90%", objectFit: "contain" }} />
        </div>
      )}

      {modelModalOpen && activeAgent && transport && (
        <ModelSwitcherModal
          agent={activeAgent}
          transport={transport}
          onClose={() => setModelModalOpen(false)}
          onSaved={(updated) => {
            if (transcript) onLoadedTranscript({ ...transcript, agent: updated });
            setModelModalOpen(false);
          }}
        />
      )}
    </div>
  );
}

function CopyReplyRow({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <div className="msg-copy-row">
      <button
        type="button"
        className={`msg-copy-btn ${copied ? "copied" : ""}`}
        onClick={() => {
          navigator.clipboard?.writeText(text).catch(() => {});
          setCopied(true);
          window.setTimeout(() => setCopied(false), 1500);
        }}
      >
        {copied ? <Check size={12} /> : <Copy size={12} />}
        <span>{copied ? "已复制" : "复制原文"}</span>
      </button>
    </div>
  );
}

function ItemView({
  item,
  agentId,
  transport,
  onZoomImage,
  loadedTool,
  loadingTool,
  onExpandTool,
}: {
  item: TranscriptItem;
  agentId: string;
  transport: HubTransport | null;
  onZoomImage: (url: string) => void;
  loadedTool?: { output?: string; detail?: ToolDetail };
  loadingTool?: boolean;
  onExpandTool: (toolCallId: string) => void;
}) {
  switch (item.type) {
    case "output":
      if (item.stream === "thought") {
        return (
          <details className="thought">
            <summary>思考过程（{item.text.length} 字）</summary>
            <div className="thought-body">{item.text}</div>
          </details>
        );
      }
      return <MarkdownView text={item.text} agentId={agentId} transport={transport} onZoomImage={onZoomImage} />;

    case "tool_call": {
      const done = item.status === "completed";
      const failed = item.status === "failed";
      const dotCls = done ? "ok" : failed ? "danger" : "accent pulse";
      const detail = item.detail ?? loadedTool?.detail;
      const output = detail?.output || item.output || loadedTool?.output || detail?.result;
      const changes = detail?.changes ?? [];
      const dur =
        detail?.duration_ms != null
          ? detail.duration_ms >= 1000
            ? `${(detail.duration_ms / 1000).toFixed(1)}s`
            : `${detail.duration_ms}ms`
          : null;
      const hasBody = Boolean(detail?.command || changes.length > 0 || output || detail?.error);
      const canExpand = hasBody || Boolean(item.has_detail);

      if (!canExpand) {
        return (
          <div className="tool-step">
            <div style={{ display: "flex", alignItems: "center", gap: 8, padding: "4px 0", fontFamily: "var(--font-mono)", fontSize: 12, color: "var(--text-2)" }}>
              <span className={`dot ${dotCls}`} />
              <span className="tool-title">{item.title}</span>
              {dur && <span className="muted" style={{ fontSize: 11 }}>{dur}</span>}
            </div>
          </div>
        );
      }

      return (
        <details
          className="tool-step"
          onToggle={(e) => {
            if (e.currentTarget.open && !hasBody && item.has_detail) {
              onExpandTool(item.tool_call_id);
            }
          }}
        >
          <summary>
            <span className={`dot ${dotCls}`} />
            <span className="tool-title">{item.title}</span>
            {dur && <span className="muted" style={{ fontSize: 11 }}>{dur}</span>}
            {failed && <span className="chip danger tiny">失败</span>}
          </summary>
          <div className="tool-detail">
            {loadingTool && !hasBody && <div className="tool-loading">正在加载工具输出…</div>}
            {detail?.command && <div className="tool-cmd">$ {detail.command}</div>}
            {changes.length > 0 && <DiffView changes={changes} />}
            {output && <pre className="tool-out mono">{output.slice(0, 3200)}</pre>}
            {detail?.error && <div className="error-box">{detail.error}</div>}
            {!loadingTool && !hasBody && <div className="tool-loading">暂无详细输出</div>}
          </div>
        </details>
      );
    }

    case "permission":
      return (
        <div className="chip" style={{ alignSelf: "flex-start" }}>
          权限「{item.title}」：{item.decision}
        </div>
      );

    case "steer":
      return (
        <div className="chip accent" style={{ borderRadius: 9, padding: "6px 10px", whiteSpace: "normal", alignSelf: "flex-start" }}>
          插话：{item.text}（{item.read ? "已读" : "待读"}）
        </div>
      );

    case "warning":
      return (
        <div className="chip warn" style={{ borderRadius: 9, padding: "7px 10px", whiteSpace: "normal" }}>
          {item.message}
        </div>
      );

    case "error":
      return <div className="error-box">{item.message}</div>;
  }
}

function InlineQuestionBanner({
  question,
  transport,
  onAnswered,
}: {
  question: HumanItem;
  transport: HubTransport;
  onAnswered: (item: HumanItem) => void;
}) {
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit(answer: string) {
    if (busy || !answer.trim()) return;
    setBusy(true);
    try {
      const res = await transport.request("POST", `/v1/questions/${encodeURIComponent(question.id)}/answer`, {
        answer: answer.trim(),
      });
      if (res.status < 400) {
        onAnswered(JSON.parse(res.body) as HumanItem);
        setReply("");
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="card"
      style={{
        margin: "0 12px 4px",
        borderColor: "color-mix(in srgb, var(--warn) 42%, var(--border))",
        borderLeft: "3px solid var(--warn)",
        boxShadow: "var(--shadow-float)",
        display: "flex",
        flexDirection: "column",
        gap: 8,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 12, fontWeight: 600, color: "var(--warn-text)" }}>
        <Hand size={14} />
        <span>Agent 需要你的决定</span>
      </div>
      <MarkdownView text={question.text} className="compact" />
      {question.options.length > 0 && (
        <div className="pill-wrap">
          {question.options.map((opt, idx) => (
            <button
              key={opt}
              type="button"
              className={`option-pill ${idx === 0 ? "first" : ""}`}
              disabled={busy}
              onClick={() => void submit(opt)}
            >
              {opt}
            </button>
          ))}
        </div>
      )}
      <div style={{ display: "flex", gap: 6 }}>
        <input
          className="field-input"
          style={{ padding: "7px 10px", fontSize: 13.5 }}
          placeholder="或输入自定义回复…"
          value={reply}
          onChange={(e) => setReply(e.target.value)}
          disabled={busy}
        />
        <button
          type="button"
          className="secondary-btn"
          style={{ padding: "7px 12px", fontSize: 13 }}
          disabled={busy || !reply.trim()}
          onClick={() => void submit(reply)}
        >
          回复
        </button>
      </div>
    </div>
  );
}

function ModelSwitcherModal({
  agent,
  transport,
  onClose,
  onSaved,
}: {
  agent: Agent;
  transport: HubTransport;
  onClose: () => void;
  onSaved: (agent: Agent) => void;
}) {
  const [plugin, setPlugin] = useState<PluginInfo>();
  const [model, setModel] = useState(agent.model ?? "");
  const [effort, setEffort] = useState(
    typeof agent.config?.effort === "string" ? agent.config.effort : "",
  );
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    const q = agent.node ? `?node=${encodeURIComponent(agent.node)}` : "";
    void transport.request("GET", `/v1/plugins${q}`).then((res) => {
      if (res.status < 400) {
        const parsed = JSON.parse(res.body) as { plugins: PluginInfo[] };
        setPlugin(parsed.plugins.find((p) => p.name === agent.plugin));
      }
    });
  }, [agent.node, agent.plugin, transport]);

  const models = plugin ? (plugin.models.length ? plugin.models : plugin.all_models) : [];
  const grouped = groupModels(models);
  const efforts = availableEfforts(plugin, model);

  function pickModel(nextModel: string) {
    setModel(nextModel);
    const nextEfforts = availableEfforts(plugin, nextModel);
    if (effort && !nextEfforts.includes(effort)) {
      setEffort("");
    }
  }

  async function save() {
    setSaving(true);
    try {
      const nextEffort = effort && efforts.includes(effort) ? effort : "";
      const res = await transport.request("POST", `/v1/agents/${encodeURIComponent(agent.id)}/model`, {
        model: model || null,
        effort: nextEffort,
      });
      if (res.status < 400) {
        onSaved(JSON.parse(res.body) as Agent);
      }
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div className="sheet-panel" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-head">
          <div style={{ display: "flex", alignItems: "center", gap: 7 }}>
            <ModelMark plugin={agent.plugin} model={model || agent.model} size={16} />
            <strong>切换模型 · {vendorName(agent.plugin)}</strong>
          </div>
          <button type="button" className="icon-btn" onClick={onClose}>
            <X size={17} />
          </button>
        </div>

        <div className="sheet-body" style={{ display: "flex", flexDirection: "column", gap: 14 }}>
          {efforts.length > 0 && (
            <div>
              <div style={{ fontSize: 12, color: "var(--muted)", marginBottom: 6 }}>推理力度</div>
              <div className="choice-grid">
                <button
                  type="button"
                  className={`choice-pill ${effort === "" ? "active" : ""}`}
                  onClick={() => setEffort("")}
                >
                  默认
                </button>
                {efforts.map((ef) => (
                  <button
                    key={ef}
                    type="button"
                    className={`choice-pill ${effort === ef ? "active" : ""}`}
                    onClick={() => setEffort(ef)}
                  >
                    {effortLabel(ef)}
                  </button>
                ))}
              </div>
            </div>
          )}

          <div>
            <div style={{ fontSize: 12, color: "var(--muted)", marginBottom: 6 }}>选择模型</div>
            <button
              type="button"
              className={`model-opt-row ${model === "" ? "active" : ""}`}
              onClick={() => pickModel("")}
            >
              <span>默认模型</span>
              {model === "" && <Check size={15} />}
            </button>
            {grouped.map((g) => (
              <div key={g.key}>
                <div className="model-group-head">
                  <FamilyMark familyKey={g.key} size={13} />
                  <span>{g.label}</span>
                </div>
                {g.models.map((m) => (
                  <button
                    key={m}
                    type="button"
                    className={`model-opt-row ${model === m ? "active" : ""}`}
                    onClick={() => pickModel(m)}
                  >
                    <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                      {prettyModel(m)}
                    </span>
                    {model === m && <Check size={15} />}
                  </button>
                ))}
              </div>
            ))}
          </div>

          <button type="button" className="primary-btn" disabled={saving} onClick={() => void save()}>
            {saving ? "保存中…" : "应用模型设置"}
          </button>
        </div>
      </div>
    </div>
  );
}
