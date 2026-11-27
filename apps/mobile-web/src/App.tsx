import { useCallback, useEffect, useRef, useState } from "react";
import { Bell, Gauge, MessageSquare } from "lucide-react";
import { registerServiceWorker } from "./push";
import {
  applyTranscriptEvent,
  clearPairing,
  loadAvatarCache,
  loadPairing,
  loadSnapshot,
  loadTranscriptCache,
  saveAvatarCache,
  savePairing,
  saveSnapshot,
  saveTranscriptCacheEntry,
} from "./store";
import {
  connectTransport,
  TransportError,
  type EventHandle,
  type HubTransport,
  type LinkStatus,
} from "./transport";
import type {
  Agent,
  ConnectionState,
  HubEvent,
  HubInfo,
  HumanItem,
  PairingRecord,
  QuotaResponse,
  RemoteNodeInfo,
  Transcript,
  UserProfile,
} from "./types";
import { HomeChatsView } from "./views/HomeChatsView";
import { InboxView } from "./views/InboxView";
import { NewChatModal } from "./views/NewChatModal";
import { PairingScreen } from "./views/PairingScreen";
import { SessionScreen } from "./views/SessionScreen";
import { StatusView } from "./views/StatusView";

type Tab = "chats" | "inbox" | "status";

export function App() {
  const [pairing, setPairing] = useState<PairingRecord | null>(() => loadPairing());
  const [connection, setConnection] = useState<ConnectionState>(() =>
    loadPairing() ? { kind: "connecting", detail: "正在连接中心…" } : { kind: "unpaired" },
  );

  const initialSnap = useRef(loadSnapshot());
  const [snapshotSavedAt, setSnapshotSavedAt] = useState<string | undefined>(initialSnap.current?.savedAt);
  const [hubInfo, setHubInfo] = useState<HubInfo | undefined>(initialSnap.current?.hubInfo);
  const [agents, setAgents] = useState<Agent[]>(initialSnap.current?.agents ?? []);
  const [questions, setQuestions] = useState<HumanItem[]>(initialSnap.current?.questions ?? []);
  const [nodes, setNodes] = useState<RemoteNodeInfo[]>(initialSnap.current?.nodes ?? []);
  const [quota, setQuota] = useState<QuotaResponse | undefined>(initialSnap.current?.quota);
  const [profile, setProfile] = useState<UserProfile | undefined>(initialSnap.current?.profile);

  const [transcripts, setTranscripts] = useState<Record<string, Transcript>>(() => loadTranscriptCache());
  const [avatarUrls, setAvatarUrls] = useState<Record<string, string>>(() => loadAvatarCache());

  const [tab, setTab] = useState<Tab>("chats");
  const [openAgentId, setOpenAgentId] = useState<string | null>(null);
  const [highlightQuestionId, setHighlightQuestionId] = useState<string>();
  const [newChatPreset, setNewChatPreset] = useState<{ node?: string; cwd?: string } | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [sessionRefreshKey, setSessionRefreshKey] = useState(0);

  const transportRef = useRef<HubTransport | null>(null);
  const [transport, setTransport] = useState<HubTransport | null>(null);
  const [linkStatus, setLinkStatus] = useState<LinkStatus | undefined>(undefined);
  const linkUnsubRef = useRef<(() => void) | null>(null);
  const sseRef = useRef<EventHandle | null>(null);
  const lastSeqRef = useRef<number | undefined>(undefined);
  const reconnectTimerRef = useRef<number | null>(null);
  const connectEpochRef = useRef(0);
  const pairingRef = useRef<PairingRecord | null>(pairing);
  pairingRef.current = pairing;

  const avatarUrlsRef = useRef<Record<string, string>>(avatarUrls);
  avatarUrlsRef.current = avatarUrls;
  const inFlightAvatarsRef = useRef<Set<string>>(new Set());

  const snapStateRef = useRef<{
    hubInfo?: HubInfo;
    agents: Agent[];
    questions: HumanItem[];
    nodes: RemoteNodeInfo[];
    quota?: QuotaResponse;
    profile?: UserProfile;
  }>({
    hubInfo: initialSnap.current?.hubInfo,
    agents: initialSnap.current?.agents ?? [],
    questions: initialSnap.current?.questions ?? [],
    nodes: initialSnap.current?.nodes ?? [],
    quota: initialSnap.current?.quota,
    profile: initialSnap.current?.profile,
  });

  const flushSnapshot = useCallback(() => {
    const savedAt = new Date().toISOString();
    setSnapshotSavedAt(savedAt);
    saveSnapshot({ savedAt, ...snapStateRef.current });
  }, []);

  useEffect(() => {
    void registerServiceWorker();
    const parseHash = () => {
      if (window.location.hash.startsWith("#inbox")) {
        setOpenAgentId(null);
        setTab("inbox");
        const q = new URLSearchParams(window.location.hash.split("?")[1] || "").get("q");
        if (q) setHighlightQuestionId(q);
      }
    };
    parseHash();
    window.addEventListener("hashchange", parseHash);

    const onSwMsg = (ev: MessageEvent) => {
      const msg = ev.data as { type?: string; question_id?: string } | undefined;
      if (msg?.type === "OPEN_QUESTION") {
        setOpenAgentId(null);
        setTab("inbox");
        if (msg.question_id) setHighlightQuestionId(msg.question_id);
      }
    };
    navigator.serviceWorker?.addEventListener("message", onSwMsg);
    return () => {
      window.removeEventListener("hashchange", parseHash);
      navigator.serviceWorker?.removeEventListener("message", onSwMsg);
    };
  }, []);

  const fetchAvatars = useCallback((t: HubTransport, list: Agent[], includeArchived = false) => {
    const queue: string[] = [];
    for (const a of list) {
      if (!includeArchived && a.archived_at) continue;
      const imgId = a.profile?.avatar_image;
      if (imgId && !avatarUrlsRef.current[imgId] && !inFlightAvatarsRef.current.has(imgId)) {
        inFlightAvatarsRef.current.add(imgId);
        queue.push(imgId);
      }
    }
    if (queue.length === 0) return;

    // Limit concurrent avatar fetches to 2 (and yield briefly on relay) so avatars never trigger
    // n0 relay rate-limiting or starve `/v1/agents/{id}/transcript` session loading.
    const startWorkers = () => {
      const workers = Math.min(2, queue.length);
      for (let w = 0; w < workers; w++) {
        void (async () => {
          while (queue.length > 0 && transportRef.current === t && !t.isClosed()) {
            const id = queue.shift()!;
            try {
              const res = await t.request("GET", `/v1/avatars/${encodeURIComponent(id)}`);
              if (res.status < 400) {
                const parsed = JSON.parse(res.body) as { mime: string; data: string };
                const dataUrl = `data:${parsed.mime};base64,${parsed.data}`;
                setAvatarUrls((prev) => {
                  const next = { ...prev, [id]: dataUrl };
                  avatarUrlsRef.current = next;
                  saveAvatarCache(next);
                  return next;
                });
              }
            } catch {
              // ignore individual avatar errors
            } finally {
              inFlightAvatarsRef.current.delete(id);
            }
          }
          // Release remaining queued ids if transport was replaced before finishing.
          while (queue.length > 0) {
            inFlightAvatarsRef.current.delete(queue.shift()!);
          }
        })();
      }
    };

    if (t.linkStatus?.kind === "p2p" || t.mode === "direct") {
      startWorkers();
    } else {
      window.setTimeout(startWorkers, 700);
    }
  }, []);

  /**
   * Progressive non-blocking sync: each endpoint updates UI the millisecond it resolves
   * (instead of stalling the chat list behind slower upstream `/v1/quota` checks).
   * Returns `false` if core requests fail so callers (e.g. `onWake`) know to reconnect.
   */
  const bootstrap = useCallback(
    async (t: HubTransport): Promise<boolean> => {
      setRefreshing(true);
      let coreOk = false;

      const agentsP = t
        .request("GET", "/v1/agents")
        .then((res) => {
          if (res.status < 400) {
            const nextAgents = (JSON.parse(res.body) as { agents: Agent[] }).agents ?? [];
            coreOk = true;
            snapStateRef.current.agents = nextAgents;
            setAgents(nextAgents);
            fetchAvatars(t, nextAgents, false);
          }
        })
        .catch(() => {});

      // Fetch ONLY open questions on bootstrap (~12 bytes vs ~111 KB gzip for full history) so
      // relay links stay fast and never trigger n0 relay rate-limiting; full history loads when
      // opening the Inbox tab.
      const questionsP = t
        .request("GET", "/v1/questions")
        .then((res) => {
          if (res.status < 400) {
            const openItems = (JSON.parse(res.body) as { items: HumanItem[] }).items ?? [];
            setQuestions((prev) => {
              const openIds = new Set(openItems.map((q) => q.id));
              const keptHistory = prev.filter((q) => q.status !== "open" && !openIds.has(q.id));
              const nextQuestions = [...openItems, ...keptHistory];
              snapStateRef.current.questions = nextQuestions;
              return nextQuestions;
            });
          }
        })
        .catch(() => {});

      const infoP = t
        .request("GET", "/v1/info")
        .then((res) => {
          if (res.status < 400) {
            const nextInfo = JSON.parse(res.body) as HubInfo;
            coreOk = true;
            snapStateRef.current.hubInfo = nextInfo;
            setHubInfo(nextInfo);
          }
        })
        .catch(() => {});

      const nodesP = t
        .request("GET", "/v1/nodes")
        .then((res) => {
          if (res.status < 400) {
            const nextNodes = (JSON.parse(res.body) as { nodes: RemoteNodeInfo[] }).nodes ?? [];
            snapStateRef.current.nodes = nextNodes;
            setNodes(nextNodes);
          }
        })
        .catch(() => {});

      const profileP = t
        .request("GET", "/v1/profile")
        .then((res) => {
          if (res.status < 400) {
            const nextProfile = JSON.parse(res.body) as UserProfile;
            snapStateRef.current.profile = nextProfile;
            setProfile(nextProfile);
          }
        })
        .catch(() => {});

      // Quota probes upstream providers on Hub and can take seconds; run completely off the
      // critical path so the conversation list refreshes immediately.
      void t
        .request("GET", "/v1/quota")
        .then((res) => {
          if (res.status < 400) {
            const nextQuota = JSON.parse(res.body) as QuotaResponse;
            snapStateRef.current.quota = nextQuota;
            setQuota(nextQuota);
            flushSnapshot();
          }
        })
        .catch(() => {});

      try {
        await Promise.all([agentsP, questionsP, infoP, nodesP, profileP]);
        if (coreOk) flushSnapshot();
        return coreOk;
      } finally {
        setRefreshing(false);
      }
    },
    [fetchAvatars, flushSnapshot],
  );

  const handleEvent = useCallback(
    (raw: string, t: HubTransport) => {
      let ev: HubEvent;
      try {
        ev = JSON.parse(raw) as HubEvent;
      } catch {
        return;
      }
      if (ev.seq && ev.seq > (lastSeqRef.current ?? 0)) {
        lastSeqRef.current = ev.seq;
      }
      const data = (ev.data ?? {}) as Record<string, unknown>;

      switch (ev.type) {
        case "agent.created":
        case "agent.updated": {
          const agent = data.agent as Agent | undefined;
          if (!agent) break;
          setAgents((prev) => {
            const idx = prev.findIndex((a) => a.id === agent.id);
            const next = idx >= 0 ? prev.map((a, i) => (i === idx ? agent : a)) : [agent, ...prev];
            snapStateRef.current.agents = next;
            return next;
          });
          fetchAvatars(t, [agent], true);
          break;
        }
        case "agent.removed": {
          const id = data.id as string;
          setAgents((prev) => {
            const next = prev.filter((a) => a.id !== id);
            snapStateRef.current.agents = next;
            return next;
          });
          break;
        }
        case "agent.status_changed": {
          const id = data.id as string;
          const to = data.to as Agent["status"];
          setAgents((prev) => {
            const next = prev.map((a) => (a.id === id ? { ...a, status: to } : a));
            snapStateRef.current.agents = next;
            return next;
          });
          break;
        }
        case "agent.model_changed": {
          const id = data.id as string;
          const model = (data.model as string | null) ?? null;
          setAgents((prev) => {
            const next = prev.map((a) => (a.id === id ? { ...a, model } : a));
            snapStateRef.current.agents = next;
            return next;
          });
          break;
        }
        case "human.asked":
        case "human.answered": {
          const item = data.item as HumanItem | undefined;
          if (!item) break;
          setQuestions((prev) => {
            const idx = prev.findIndex((q) => q.id === item.id);
            const next = idx >= 0 ? prev.map((q, i) => (i === idx ? item : q)) : [item, ...prev];
            snapStateRef.current.questions = next;
            return next;
          });
          break;
        }
        case "human.dismissed": {
          const id = data.id as string;
          setQuestions((prev) => {
            const next = prev.map((q) => (q.id === id ? { ...q, status: "dismissed" as const } : q));
            snapStateRef.current.questions = next;
            return next;
          });
          break;
        }
        case "profile.updated": {
          void t
            .request("GET", "/v1/profile")
            .then((r) => {
              if (r.status < 400) {
                const nextProfile = JSON.parse(r.body) as UserProfile;
                snapStateRef.current.profile = nextProfile;
                setProfile(nextProfile);
                flushSnapshot();
              }
            })
            .catch(() => {});
          break;
        }
        case "node.changed": {
          void t
            .request("GET", "/v1/nodes")
            .then((r) => {
              if (r.status < 400) {
                const nextNodes = (JSON.parse(r.body) as { nodes: RemoteNodeInfo[] }).nodes;
                snapStateRef.current.nodes = nextNodes;
                setNodes(nextNodes);
              }
            })
            .catch(() => {});
          break;
        }
        case "account.quota_updated":
        case "account.created":
        case "account.removed": {
          void t
            .request("GET", "/v1/quota")
            .then((r) => {
              if (r.status < 400) {
                const nextQuota = JSON.parse(r.body) as QuotaResponse;
                snapStateRef.current.quota = nextQuota;
                setQuota(nextQuota);
              }
            })
            .catch(() => {});
          break;
        }
        case "device.revoked": {
          const dev = data.device as { id?: string } | undefined;
          if (dev?.id && dev.id === pairingRef.current?.deviceId) {
            sseRef.current?.cancel();
            setConnection({ kind: "revoked", message: "该设备授权已在中心被吊销，请重新扫码配对" });
          }
          break;
        }
      }

      if (ev.type.startsWith("agent.")) {
        const targetId = (data.id as string | undefined) ?? (data.agent as Agent | undefined)?.id;
        if (targetId) {
          setTranscripts((prev) => {
            const cur = prev[targetId];
            if (!cur) return prev;
            const updated = applyTranscriptEvent(cur, ev);
            if (ev.type === "agent.turn_completed" || ev.type === "agent.input") {
              saveTranscriptCacheEntry(updated);
            }
            return { ...prev, [targetId]: updated };
          });
        }
      }
    },
    [fetchAvatars, flushSnapshot],
  );

  const teardown = useCallback(async () => {
    if (reconnectTimerRef.current) {
      window.clearTimeout(reconnectTimerRef.current);
      reconnectTimerRef.current = null;
    }
    linkUnsubRef.current?.();
    linkUnsubRef.current = null;
    setLinkStatus(undefined);
    sseRef.current?.cancel();
    sseRef.current = null;
    const prev = transportRef.current;
    transportRef.current = null;
    setTransport(null);
    if (prev) await prev.close();
  }, []);

  const connect = useCallback(
    async (record: PairingRecord) => {
      const epoch = ++connectEpochRef.current;
      await teardown();
      setConnection({
        kind: "connecting",
        detail: record.mode === "iroh" ? "正在经加密中继连接中心…" : `正在连接 ${record.host}…`,
      });

      try {
        const t = await connectTransport(record, (detail) => {
          if (epoch === connectEpochRef.current) {
            setConnection({ kind: "connecting", detail });
          }
        });
        if (epoch !== connectEpochRef.current) {
          await t.close();
          return;
        }
        transportRef.current = t;
        setTransport(t);
        setLinkStatus(t.linkStatus);
        linkUnsubRef.current =
          t.onLinkStatusChange?.((status) => {
            if (epoch === connectEpochRef.current) setLinkStatus(status);
          }) ?? null;
        setConnection({ kind: "connected", mode: t.mode });

        if (record.mode === "iroh" && t.connectedRelay && t.connectedRelay !== record.relay) {
          const updated: PairingRecord = { ...record, relay: t.connectedRelay };
          savePairing(updated);
          pairingRef.current = updated;
        }

        // Open SSE live event stream IN PARALLEL with progressive bootstrap so streaming replies
        // and status updates flow immediately without waiting for the full snapshot round-trip.
        const ssePromise = t
          .subscribeEvents(
            lastSeqRef.current,
            (raw) => handleEvent(raw, t),
            (err) => {
              if (epoch !== connectEpochRef.current) return;
              if (err.code === "unauthorized") {
                setConnection({ kind: "revoked", message: err.message });
                return;
              }
              setConnection({ kind: "hub_offline", message: "实时连接已断开，正在重连…" });
              reconnectTimerRef.current = window.setTimeout(() => {
                if (pairingRef.current && epoch === connectEpochRef.current) {
                  void connect(pairingRef.current);
                }
              }, 3000);
            },
          )
          .then((handle) => {
            if (epoch !== connectEpochRef.current) {
              handle.cancel();
              return;
            }
            sseRef.current = handle;
          })
          .catch((err) => {
            if (epoch !== connectEpochRef.current) return;
            const te = err instanceof TransportError ? err : new TransportError("hub_offline", String(err));
            if (te.code === "unauthorized") {
              setConnection({ kind: "revoked", message: te.message });
              return;
            }
            setConnection({ kind: "hub_offline", message: "实时事件流连接失败，正在重连…" });
            reconnectTimerRef.current = window.setTimeout(() => {
              if (pairingRef.current && epoch === connectEpochRef.current) {
                void connect(pairingRef.current);
              }
            }, 3000);
          });

        await Promise.all([bootstrap(t), ssePromise]);
      } catch (err) {
        if (epoch !== connectEpochRef.current) return;
        const te = err instanceof TransportError ? err : new TransportError("hub_offline", String(err));
        if (te.code === "unauthorized") {
          setConnection({ kind: "revoked", message: te.message });
        } else if (te.code === "relay_unreachable") {
          setConnection({ kind: "relay_unreachable", message: te.message });
        } else {
          setConnection({ kind: "hub_offline", message: te.message });
        }
      }
    },
    [bootstrap, handleEvent, teardown],
  );

  useEffect(() => {
    if (!pairing) {
      void teardown();
      setConnection({ kind: "unpaired" });
      return;
    }
    void connect(pairing);
    return () => {
      connectEpochRef.current++;
      void teardown();
    };
  }, [pairing]);

  useEffect(() => {
    let hiddenAt = 0;
    let wakeChecking = false;

    const onWake = async (forceProbe: boolean) => {
      if (document.visibilityState === "hidden") {
        hiddenAt = Date.now();
        return;
      }
      void registerServiceWorker();
      const rec = pairingRef.current;
      if (!rec || wakeChecking) return;
      const cur = transportRef.current;
      if (!cur || cur.isClosed()) {
        void connect(rec);
        return;
      }
      const sleptMs = hiddenAt > 0 ? Date.now() - hiddenAt : 0;
      hiddenAt = 0;

      // After >3s in background/lock-screen (or network change), probe the socket with tight
      // timeouts first so a dead iOS-reclaimed socket triggers immediate reconnect instead of
      // hanging `bootstrap()` for 35s.
      if ((forceProbe || sleptMs >= 3000) && cur.wakeCheck) {
        wakeChecking = true;
        try {
          const state = await cur.wakeCheck();
          if (transportRef.current !== cur) return;
          if (state === "dead") {
            if (pairingRef.current) void connect(pairingRef.current);
            return;
          }
        } finally {
          wakeChecking = false;
        }
      } else if (cur.linkStatus && cur.linkStatus.kind !== "p2p") {
        void cur.retryP2p?.();
      }

      void bootstrap(cur).then((ok) => {
        if (!ok && pairingRef.current && transportRef.current === cur) {
          void connect(pairingRef.current);
        } else if (ok) {
          setSessionRefreshKey((k) => k + 1);
        }
      });
    };

    const onVis = () => {
      void onWake(false);
    };
    const onOnline = () => {
      void onWake(true);
    };
    document.addEventListener("visibilitychange", onVis);
    window.addEventListener("online", onOnline);
    return () => {
      document.removeEventListener("visibilitychange", onVis);
      window.removeEventListener("online", onOnline);
    };
  }, [bootstrap, connect]);

  // Lazily fetch answered/dismissed question history only when opening the Inbox tab.
  useEffect(() => {
    if (tab !== "inbox" || !transport || transport.isClosed()) return;
    let cancelled = false;
    void transport
      .request("GET", "/v1/questions?all=true")
      .then((res) => {
        if (cancelled || res.status >= 400) return;
        const nextQuestions = (JSON.parse(res.body) as { items: HumanItem[] }).items ?? [];
        snapStateRef.current.questions = nextQuestions;
        setQuestions(nextQuestions);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [tab, transport]);

  if (!pairing) {
    return (
      <PairingScreen
        onPaired={(record) => {
          savePairing(record);
          lastSeqRef.current = undefined;
          setPairing(record);
        }}
      />
    );
  }

  const openQuestions = questions.filter((q) => q.status === "open");
  const isConnected = connection.kind === "connected";

  if (openAgentId) {
    return (
      <SessionScreen
        agentId={openAgentId}
        agent={agents.find((a) => a.id === openAgentId)}
        transcript={transcripts[openAgentId]}
        nodes={nodes}
        hubInfo={hubInfo}
        profile={profile}
        avatarUrls={avatarUrls}
        fallbackHost={pairing.host}
        pendingQuestions={openQuestions.filter((q) => q.from === openAgentId)}
        transport={transport}
        refreshKey={sessionRefreshKey}
        onBack={() => setOpenAgentId(null)}
        onLoadedTranscript={(t) => {
          setTranscripts((prev) => ({ ...prev, [t.agent.id]: t }));
          saveTranscriptCacheEntry(t);
          if (t.last_seq > (lastSeqRef.current ?? 0)) lastSeqRef.current = t.last_seq;
        }}
        onAnsweredQuestion={(updated) => {
          setQuestions((prev) => prev.map((q) => (q.id === updated.id ? updated : q)));
        }}
      />
    );
  }

  const banner =
    connection.kind === "connecting"
      ? { tone: "info", text: connection.detail, action: undefined }
      : connection.kind === "relay_unreachable"
        ? { tone: "warn", text: `中继不可达：${connection.message}`, action: "重试" }
        : connection.kind === "hub_offline"
          ? { tone: "warn", text: connection.message, action: "重连" }
          : connection.kind === "revoked"
            ? { tone: "danger", text: connection.message, action: "重新配对" }
            : null;

  return (
    <div className={`app-shell ${banner ? "has-banner" : ""}`}>
      {banner && (
        <div className={`status-banner ${banner.tone}`}>
          <span className="status-banner-msg">{banner.text}</span>
          {banner.action && (
            <button
              type="button"
              className="status-banner-btn"
              onClick={() => {
                if (connection.kind === "revoked") {
                  clearPairing();
                  setPairing(null);
                } else {
                  void connect(pairing);
                }
              }}
            >
              {banner.action}
            </button>
          )}
        </div>
      )}

      {tab === "chats" && (
        <HomeChatsView
          agents={agents}
          nodes={nodes}
          hubInfo={hubInfo}
          profile={profile}
          fallbackHost={pairing.host}
          openQuestions={openQuestions}
          avatarUrls={avatarUrls}
          snapshotSavedAt={snapshotSavedAt}
          isConnected={isConnected}
          linkStatus={linkStatus}
          transport={transport}
          refreshing={refreshing}
          onRefresh={() => (transport ? void bootstrap(transport) : void connect(pairing))}
          onOpenAgent={(id) => setOpenAgentId(id)}
          onOpenInbox={() => setTab("inbox")}
          onOpenStatus={() => setTab("status")}
          onOpenNewChat={(preset) => setNewChatPreset(preset ?? {})}
          onEnsureArchivedAvatars={() => {
            if (transport) fetchAvatars(transport, agents, true);
          }}
        />
      )}

      {tab === "inbox" && (
        <InboxView
          questions={questions}
          agents={agents}
          nodes={nodes}
          hubInfo={hubInfo}
          avatarUrls={avatarUrls}
          fallbackHost={pairing.host}
          highlightId={highlightQuestionId}
          transport={transport}
          isConnected={isConnected}
          onRefresh={() => (transport ? void bootstrap(transport) : void connect(pairing))}
          onUpdatedQuestion={(updated) => {
            setQuestions((prev) => prev.map((q) => (q.id === updated.id ? updated : q)));
          }}
        />
      )}

      {tab === "status" && (
        <StatusView
          pairing={pairingRef.current ?? pairing}
          connection={connection}
          linkStatus={linkStatus}
          hubInfo={hubInfo}
          profile={profile}
          nodes={nodes}
          quota={quota}
          transport={transport}
          onReconnect={() => void connect(pairingRef.current ?? pairing)}
          onRefresh={() => (transport ? void bootstrap(transport) : void connect(pairingRef.current ?? pairing))}
          onForget={() => {
            clearPairing();
            setPairing(null);
            setAgents([]);
            setQuestions([]);
            setNodes([]);
            setQuota(undefined);
            setProfile(undefined);
            setAvatarUrls({});
            avatarUrlsRef.current = {};
            setTranscripts({});
          }}
        />
      )}

      <nav className="tab-bar" aria-label="主导航">
        <button type="button" className={`tab-btn ${tab === "chats" ? "active" : ""}`} onClick={() => setTab("chats")}>
          <span className="tab-icon-wrap">
            <MessageSquare size={21} strokeWidth={tab === "chats" ? 2.1 : 1.7} />
          </span>
          <span>对话</span>
        </button>
        <button type="button" className={`tab-btn ${tab === "inbox" ? "active" : ""}`} onClick={() => setTab("inbox")}>
          <span className="tab-icon-wrap">
            <Bell size={21} strokeWidth={tab === "inbox" ? 2.1 : 1.7} />
            {openQuestions.length > 0 && <span className="badge-pill">{openQuestions.length}</span>}
          </span>
          <span>待办</span>
        </button>
        <button type="button" className={`tab-btn ${tab === "status" ? "active" : ""}`} onClick={() => setTab("status")}>
          <span className="tab-icon-wrap">
            <Gauge size={21} strokeWidth={tab === "status" ? 2.1 : 1.7} />
          </span>
          <span>概览</span>
        </button>
      </nav>

      {newChatPreset && transport && (
        <NewChatModal
          transport={transport}
          nodes={nodes}
          hubInfo={hubInfo}
          fallbackHost={pairing.host}
          initialNode={newChatPreset.node}
          initialCwd={newChatPreset.cwd}
          onClose={() => setNewChatPreset(null)}
          onCreated={(created) => {
            setAgents((prev) => [created, ...prev.filter((a) => a.id !== created.id)]);
            setNewChatPreset(null);
            setOpenAgentId(created.id);
          }}
        />
      )}
    </div>
  );
}
