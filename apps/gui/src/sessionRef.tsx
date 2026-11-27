// Read-only session references (`@session` and sidebar drag-and-drop into composers).
// Each reference is bound to an immutable `agentId`; display labels are purely visual so
// renaming a session never breaks a reference.

import { useCallback, useState, type DragEvent } from "react";
import type { Agent } from "@bindings/Agent";
import type { SessionRef } from "@bindings/SessionRef";
import type { SessionRefInput } from "@bindings/SessionRefInput";
import { Icon, sessionHref } from "./components";
import { openBeside } from "./panes";
import { homeOf, isChat, wsName } from "./workspace";
import { parseScopedName } from "./lib/agentName";

/** Custom drag payload MIME type carried by sidebar session rows (both active and archived). */
export const SESSION_DRAG_MIME = "application/x-crossagents-session";

export const MAX_SESSION_REFS = 6;

export type SessionRefMode = "summary" | "full" | "pointer";

export type HeldSessionRef = {
  agentId: string;
  name: string;
  topic?: string | null;
  project?: string | null;
  archived?: boolean;
  mode: SessionRefMode;
};

export type DraggedSessionPayload = {
  id: string;
  name: string;
  topic?: string | null;
  project?: string | null;
  archived?: boolean;
};

export function sessionPayloadOf(agent: Agent): DraggedSessionPayload {
  const project = isChat(agent) ? null : wsName(homeOf(agent));
  return {
    id: agent.id,
    name: agent.name,
    topic: agent.topic,
    project,
    archived: Boolean(agent.archived_at),
  };
}

export function encodeSessionDrag(dataTransfer: DataTransfer, agent: Agent): void {
  dataTransfer.setData(SESSION_DRAG_MIME, JSON.stringify(sessionPayloadOf(agent)));
}

export function hasSessionDrag(dataTransfer: DataTransfer | null): boolean {
  if (!dataTransfer) return false;
  return Array.from(dataTransfer.types).includes(SESSION_DRAG_MIME);
}

export function decodeSessionDrag(dataTransfer: DataTransfer | null): DraggedSessionPayload | null {
  if (!dataTransfer) return null;
  const raw = dataTransfer.getData(SESSION_DRAG_MIME);
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as Partial<DraggedSessionPayload>;
    if (!parsed || typeof parsed.id !== "string" || !parsed.id || typeof parsed.name !== "string") return null;
    return {
      id: parsed.id,
      name: parsed.name,
      topic: typeof parsed.topic === "string" ? parsed.topic : null,
      project: typeof parsed.project === "string" ? parsed.project : null,
      archived: Boolean(parsed.archived),
    };
  } catch {
    return null;
  }
}

const MODE_ORDER: readonly SessionRefMode[] = ["summary", "full", "pointer"];

const MODE_LABELS: Record<SessionRefMode, { short: string; title: string }> = {
  summary: { short: "摘要", title: "注入最近几轮核心对话 + 快照路径（点此切换为「全文」）" },
  full: { short: "全文", title: "注入更多轮次上下文 + 快照路径（点此切换为「仅定位」）" },
  pointer: { short: "仅定位", title: "仅注入会话元信息与 ID，由 Agent 按需查阅（点此切换为「摘要」）" },
};

/** Composer hook for holding referenced sessions added via `@` or sidebar drag-and-drop. */
export function useSessionRefs(onError?: (why: string | undefined) => void) {
  const [held, setHeld] = useState<HeldSessionRef[]>([]);
  const [dragOver, setDragOver] = useState(false);

  const addPayload = useCallback(
    (payload: DraggedSessionPayload) => {
      setHeld((prev) => {
        if (prev.some((x) => x.agentId === payload.id)) return prev;
        if (prev.length >= MAX_SESSION_REFS) {
          onError?.(`每条消息最多引用 ${MAX_SESSION_REFS} 个会话`);
          return prev;
        }
        onError?.(undefined);
        return [
          ...prev,
          {
            agentId: payload.id,
            name: payload.name,
            topic: payload.topic,
            project: payload.project,
            archived: payload.archived,
            mode: "summary",
          },
        ];
      });
    },
    [onError],
  );

  const add = useCallback(
    (agent: Agent) => {
      addPayload(sessionPayloadOf(agent));
    },
    [addPayload],
  );

  const remove = useCallback((agentId: string) => {
    setHeld((prev) => prev.filter((x) => x.agentId !== agentId));
  }, []);

  const cycleMode = useCallback((agentId: string) => {
    setHeld((prev) =>
      prev.map((x) => {
        if (x.agentId !== agentId) return x;
        const idx = MODE_ORDER.indexOf(x.mode);
        const next = MODE_ORDER[(idx + 1) % MODE_ORDER.length];
        return { ...x, mode: next };
      }),
    );
  }, []);

  const clear = useCallback(() => setHeld([]), []);

  const toInputs = useCallback(
    (): SessionRefInput[] => held.map((x) => ({ agent: x.agentId, mode: x.mode })),
    [held],
  );

  const onDragOver = useCallback((e: DragEvent) => {
    if (!hasSessionDrag(e.dataTransfer)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "copy";
    setDragOver(true);
  }, []);

  const onDragLeave = useCallback((e: DragEvent) => {
    if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
      setDragOver(false);
    }
  }, []);

  const onDrop = useCallback(
    (e: DragEvent): boolean => {
      setDragOver(false);
      const payload = decodeSessionDrag(e.dataTransfer);
      if (!payload) return false;
      e.preventDefault();
      e.stopPropagation();
      addPayload(payload);
      return true;
    },
    [addPayload],
  );

  return { held, add, addPayload, remove, cycleMode, clear, toInputs, dragOver, onDragOver, onDragLeave, onDrop };
}

/** Renders a scoped name like `CA-Nyx-12` with crisp typographic hierarchy. */
export function ScopedNameLabel({ name, compact = false }: { name: string; compact?: boolean }) {
  const parsed = parseScopedName(name);
  if (!parsed) return <>{name}</>;
  if (compact) {
    return (
      <span className="scoped-name compact">
        <span className="scoped-word">{parsed.word}</span>
        <span className="scoped-seq">#{parsed.seq}</span>
      </span>
    );
  }
  return (
    <span className="scoped-name">
      <span className="scoped-code">{parsed.code}-</span>
      <span className="scoped-word">{parsed.word}</span>
      <span className="scoped-seq">-{parsed.seq}</span>
    </span>
  );
}

/** Held session reference chips above the composer textarea. */
export function HeldSessionRefs({
  refs,
  onRemove,
  onCycleMode,
}: {
  refs: HeldSessionRef[];
  onRemove: (agentId: string) => void;
  onCycleMode: (agentId: string) => void;
}) {
  if (!refs.length) return null;
  return (
    <div className="held-sessions" aria-label="已引用的会话">
      {refs.map((r) => {
        const meta = MODE_LABELS[r.mode];
        return (
          <span
            key={r.agentId}
            className="session-ref-chip"
            title={`${r.name}${r.project ? ` · ${r.project}` : ""}${r.topic ? `\n${r.topic}` : ""}${r.archived ? "\n（已归档）" : ""}`}
          >
            <Icon name="chat" size={12} />
            <a className="session-ref-link" href={sessionHref(r.agentId)} onClick={(e) => {
              if (e.ctrlKey || e.metaKey) {
                e.preventDefault();
                openBeside(r.agentId);
              }
            }}>
              <ScopedNameLabel name={r.name} />
              {r.topic && <span className="session-ref-topic">{r.topic}</span>}
            </a>
            <button
              type="button"
              className="session-ref-mode"
              onClick={() => onCycleMode(r.agentId)}
              title={meta.title}
            >
              {meta.short}
            </button>
            <button
              type="button"
              className="ghost icon"
              onClick={() => onRemove(r.agentId)}
              aria-label={`移除引用 ${r.name}`}
            >
              <Icon name="close" size={11} />
            </button>
          </span>
        );
      })}
    </div>
  );
}

/** Clickable session reference pills rendered on a sent user message bubble. */
export function TurnSessionRefs({ refs }: { refs?: SessionRef[] | HeldSessionRef[] }) {
  if (!refs?.length) return null;
  return (
    <div className="turn-session-refs" aria-label="引用的会话">
      {refs.map((r) => {
        const id = "agent_id" in r ? r.agent_id : r.agentId;
        const mode = (r.mode as SessionRefMode | null | undefined) ?? "summary";
        const modeLabel = MODE_LABELS[mode]?.short ?? "摘要";
        return (
          <span key={id} className="turn-session-ref-pill">
            <a
              href={sessionHref(id)}
              title={`打开引用会话 ${r.name}${r.project ? `（${r.project}）` : ""}${r.topic ? `：${r.topic}` : ""}\nCtrl/Cmd+点击分屏打开`}
              onClick={(e) => {
                if (e.ctrlKey || e.metaKey) {
                  e.preventDefault();
                  openBeside(id);
                }
              }}
            >
              <Icon name="chat" size={12} />
              <ScopedNameLabel name={r.name} />
              {r.topic && <span className="session-ref-topic">{r.topic}</span>}
              <span className="session-ref-badge">{modeLabel}</span>
            </a>
            <button
              type="button"
              className="ghost icon"
              onClick={() => openBeside(id)}
              title="在右侧分屏打开此会话"
              aria-label={`分屏打开 ${r.name}`}
            >
              <Icon name="beside" size={11} />
            </button>
          </span>
        );
      })}
    </div>
  );
}
