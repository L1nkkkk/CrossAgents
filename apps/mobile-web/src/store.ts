import type {
  Agent,
  AttachmentUpload,
  HubEvent,
  HubInfo,
  HumanItem,
  PairingRecord,
  PluginInfo,
  QuotaResponse,
  RemoteNodeInfo,
  Transcript,
  TranscriptItem,
  UserProfile,
} from "./types";

const PAIRING_KEY = "crossagents.mobile.pairing.v1";
const SNAPSHOT_KEY = "crossagents.mobile.snapshot.v1";
const AVATARS_KEY = "crossagents.mobile.avatars.v1";
const TRANSCRIPTS_KEY = "crossagents.mobile.transcripts.v1";
const COLLAPSED_KEY = "crossagents.mobile.collapsed.v1";

const MAX_CACHED_AVATARS = 24;
const MAX_CACHED_TRANSCRIPTS = 6;
const MAX_CACHED_TURNS = 35;
const MAX_CACHED_TOOL_OUTPUT = 6000;

export interface CachedSnapshot {
  savedAt: string;
  hubInfo?: HubInfo;
  agents: Agent[];
  questions: HumanItem[];
  nodes: RemoteNodeInfo[];
  quota?: QuotaResponse;
  profile?: UserProfile;
}

interface StoredTranscriptCache {
  order: string[];
  entries: Record<string, Transcript>;
}

export function loadPairing(): PairingRecord | null {
  try {
    const raw = localStorage.getItem(PAIRING_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as PairingRecord;
  } catch {
    return null;
  }
}

export function savePairing(record: PairingRecord): void {
  localStorage.setItem(PAIRING_KEY, JSON.stringify(record));
}

export function clearPairing(): void {
  localStorage.removeItem(PAIRING_KEY);
  localStorage.removeItem(SNAPSHOT_KEY);
  localStorage.removeItem(AVATARS_KEY);
  localStorage.removeItem(TRANSCRIPTS_KEY);
  localStorage.removeItem(COLLAPSED_KEY);
}

export function loadSnapshot(): CachedSnapshot | null {
  try {
    const raw = localStorage.getItem(SNAPSHOT_KEY);
    if (!raw) return null;
    return JSON.parse(raw) as CachedSnapshot;
  } catch {
    return null;
  }
}

export function saveSnapshot(snap: CachedSnapshot): void {
  try {
    localStorage.setItem(SNAPSHOT_KEY, JSON.stringify(snap));
  } catch {
    // quota exceeded - ignore
  }
}

export function loadAvatarCache(): Record<string, string> {
  try {
    const raw = localStorage.getItem(AVATARS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as Record<string, string>;
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

export function saveAvatarCache(avatars: Record<string, string>): void {
  const keys = Object.keys(avatars);
  const trimmed: Record<string, string> = {};
  for (const k of keys.slice(-MAX_CACHED_AVATARS)) {
    trimmed[k] = avatars[k];
  }
  const list = Object.keys(trimmed);
  while (list.length > 0) {
    try {
      localStorage.setItem(AVATARS_KEY, JSON.stringify(trimmed));
      return;
    } catch {
      const drop = list.shift();
      if (drop) delete trimmed[drop];
    }
  }
}

function compactTranscriptForCache(t: Transcript): Transcript {
  const turns = t.turns.slice(-MAX_CACHED_TURNS).map((turn) => ({
    ...turn,
    items: turn.items.map((item) => {
      if (item.type !== "tool_call") return item;
      const out =
        item.output && item.output.length > MAX_CACHED_TOOL_OUTPUT
          ? `${item.output.slice(0, MAX_CACHED_TOOL_OUTPUT)}\n…`
          : item.output;
      const detail = item.detail
        ? {
            ...item.detail,
            output:
              item.detail.output && item.detail.output.length > MAX_CACHED_TOOL_OUTPUT
                ? `${item.detail.output.slice(0, MAX_CACHED_TOOL_OUTPUT)}\n…`
                : item.detail.output,
            result:
              item.detail.result && item.detail.result.length > MAX_CACHED_TOOL_OUTPUT
                ? `${item.detail.result.slice(0, MAX_CACHED_TOOL_OUTPUT)}\n…`
                : item.detail.result,
          }
        : undefined;
      return { ...item, output: out, detail };
    }),
  }));
  return { ...t, turns };
}

export function loadTranscriptCache(): Record<string, Transcript> {
  try {
    const raw = localStorage.getItem(TRANSCRIPTS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as StoredTranscriptCache;
    return parsed?.entries && typeof parsed.entries === "object" ? parsed.entries : {};
  } catch {
    return {};
  }
}

export function saveTranscriptCacheEntry(transcript: Transcript): void {
  try {
    let store: StoredTranscriptCache = { order: [], entries: {} };
    const raw = localStorage.getItem(TRANSCRIPTS_KEY);
    if (raw) {
      const parsed = JSON.parse(raw) as StoredTranscriptCache;
      if (parsed?.entries && Array.isArray(parsed.order)) store = parsed;
    }
    const id = transcript.agent.id;
    store.order = [id, ...store.order.filter((x) => x !== id)].slice(0, MAX_CACHED_TRANSCRIPTS);
    const keep = new Set(store.order);
    for (const k of Object.keys(store.entries)) {
      if (!keep.has(k)) delete store.entries[k];
    }
    store.entries[id] = compactTranscriptForCache(transcript);

    while (store.order.length > 0) {
      try {
        localStorage.setItem(TRANSCRIPTS_KEY, JSON.stringify(store));
        return;
      } catch {
        const drop = store.order.pop();
        if (drop) delete store.entries[drop];
      }
    }
  } catch {
    // ignore storage failures
  }
}

export function loadCollapsed(): Set<string> {
  try {
    const raw = localStorage.getItem(COLLAPSED_KEY);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? new Set(parsed.filter((x): x is string => typeof x === "string")) : new Set();
  } catch {
    return new Set();
  }
}

export function saveCollapsed(collapsed: ReadonlySet<string>): void {
  try {
    localStorage.setItem(COLLAPSED_KEY, JSON.stringify([...collapsed]));
  } catch {
    // ignore
  }
}

/** Exact desktop workspace helpers (`apps/gui/src/lib/projectView.ts`). */
export const CHATS_GROUP_KEY = "__chats";

export function homeOf(agent: Agent): string {
  return agent.home || agent.cwd || "";
}

export function wsKey(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
}

export function wsName(path: string): string {
  return path.replace(/\\/g, "/").replace(/\/+$/, "").split("/").pop() || path;
}

export function isStandaloneChat(agent: Agent): boolean {
  const home = homeOf(agent);
  if (!home) return true;
  const parts = home.replace(/\\/g, "/").replace(/\/+$/, "").split("/");
  const [dotHome, chats, last] = parts.slice(-3);
  return (
    chats === "chats" &&
    (dotHome === ".crossagents" ||
      last === agent.id ||
      (last ?? "").toLowerCase() === agent.name.toLowerCase())
  );
}

export function agentMachineKey(agent: { node?: string | null }): string {
  const n = (agent.node ?? "").trim();
  return !n || n.toLowerCase() === "local" || n === "centre" ? "local" : n;
}

export function applyTranscriptEvent(current: Transcript, ev: HubEvent): Transcript {
  const data = (ev.data ?? {}) as Record<string, unknown>;
  const targetId = data.id as string | undefined;
  if (targetId && targetId !== current.agent.id) return current;

  const next: Transcript = {
    ...current,
    turns: current.turns.map((t) => ({ ...t, items: [...t.items] })),
    last_seq: Math.max(current.last_seq, ev.seq || 0),
  };

  const activeIndex = () => {
    for (let i = next.turns.length - 1; i >= 0; i--) {
      if (next.turns[i].started_at && !next.turns[i].completed_at) return i;
    }
    return next.turns.length - 1;
  };

  switch (ev.type) {
    case "agent.updated": {
      const agent = data.agent as Agent | undefined;
      if (agent && agent.id === next.agent.id) next.agent = agent;
      return next;
    }
    case "agent.status_changed": {
      if (typeof data.to === "string") {
        next.agent = { ...next.agent, status: data.to as Agent["status"] };
      }
      return next;
    }
    case "agent.delivery_changed": {
      if (data.delivery) next.delivery = data.delivery as Transcript["delivery"];
      return next;
    }
    case "agent.model_changed": {
      next.agent = { ...next.agent, model: (data.model as string | null) ?? null };
      return next;
    }
    case "agent.input": {
      const messageId = String(data.message_id ?? "");
      if (!messageId) return next;
      const existing = next.turns.findIndex((t) => t.message_id === messageId);
      if (existing >= 0) {
        next.turns[existing] = {
          ...next.turns[existing],
          input: String(data.text ?? next.turns[existing].input),
          queued_reason: data.queued_reason as string | undefined,
        };
      } else {
        next.turns.push({
          message_id: messageId,
          input: String(data.text ?? ""),
          from: (data.from as Transcript["turns"][number]["from"]) ?? null,
          sent_at: ev.ts,
          started_at: null,
          completed_at: null,
          stop_reason: null,
          items: [],
          attachments: data.attachments as Transcript["turns"][number]["attachments"],
          notices: data.notices as string[] | undefined,
          queued_reason: data.queued_reason as string | undefined,
          withdrawn: false,
        });
      }
      return next;
    }
    case "agent.input_withdrawn": {
      const idx = next.turns.findIndex((t) => t.message_id === data.message_id);
      if (idx >= 0) next.turns[idx] = { ...next.turns[idx], withdrawn: true };
      return next;
    }
    case "agent.turn_started": {
      const idx = next.turns.findIndex((t) => t.message_id === data.message_id);
      if (idx >= 0) {
        next.turns[idx] = { ...next.turns[idx], started_at: ev.ts, queued_reason: undefined };
      }
      return next;
    }
    case "agent.output": {
      const idx = activeIndex();
      if (idx < 0) return next;
      const stream = (data.stream === "thought" ? "thought" : "text") as "text" | "thought";
      const text = String(data.text ?? "");
      const items = next.turns[idx].items;
      const last = items[items.length - 1];
      if (last && last.type === "output" && last.stream === stream) {
        items[items.length - 1] = { type: "output", stream, text: last.text + text };
      } else {
        items.push({ type: "output", stream, text });
      }
      return next;
    }
    case "agent.tool_call": {
      const idx = activeIndex();
      if (idx < 0) return next;
      const item: TranscriptItem = {
        type: "tool_call",
        tool_call_id: String(data.tool_call_id ?? ""),
        title: String(data.title ?? "工具调用"),
        kind: (data.kind as string | null) ?? null,
        status: String(data.status ?? "in_progress"),
      };
      const items = next.turns[idx].items;
      const pos = items.findIndex((x) => x.type === "tool_call" && x.tool_call_id === item.tool_call_id);
      if (pos >= 0) items[pos] = item;
      else items.push(item);
      return next;
    }
    case "agent.tool_call_updated": {
      const idx = activeIndex();
      if (idx < 0) return next;
      const items = next.turns[idx].items;
      const pos = items.findIndex((x) => x.type === "tool_call" && x.tool_call_id === data.tool_call_id);
      if (pos >= 0 && items[pos].type === "tool_call") {
        items[pos] = {
          ...items[pos],
          title: (data.title as string | null) ?? items[pos].title,
          status: (data.status as string | null) ?? items[pos].status,
        };
      }
      return next;
    }
    case "agent.tool_output": {
      const idx = activeIndex();
      if (idx < 0) return next;
      const items = next.turns[idx].items;
      const pos = items.findIndex((x) => x.type === "tool_call" && x.tool_call_id === data.tool_call_id);
      if (pos >= 0 && items[pos].type === "tool_call") {
        items[pos] = {
          ...items[pos],
          output: (items[pos].output ?? "") + String(data.text ?? ""),
        };
      }
      return next;
    }
    case "agent.tool_detail": {
      const idx = activeIndex();
      if (idx < 0) return next;
      const items = next.turns[idx].items;
      const pos = items.findIndex((x) => x.type === "tool_call" && x.tool_call_id === data.tool_call_id);
      if (pos >= 0 && items[pos].type === "tool_call") {
        items[pos] = { ...items[pos], detail: data.detail as TranscriptItem extends { detail?: infer D } ? D : never };
      }
      return next;
    }
    case "agent.permission": {
      const idx = activeIndex();
      if (idx >= 0) {
        next.turns[idx].items.push({
          type: "permission",
          kind: String(data.kind ?? ""),
          title: String(data.title ?? "权限"),
          decision: String(data.decision ?? ""),
        });
      }
      return next;
    }
    case "agent.steered": {
      const idx = activeIndex();
      if (idx >= 0) {
        next.turns[idx].items.push({
          type: "steer",
          message_id: String(data.message_id ?? ""),
          text: String(data.text ?? ""),
          read: false,
        });
      }
      return next;
    }
    case "agent.warning": {
      const idx = activeIndex();
      if (idx >= 0) next.turns[idx].items.push({ type: "warning", message: String(data.message ?? "") });
      return next;
    }
    case "agent.error": {
      const msg = String(data.message ?? "");
      next.error = msg;
      const idx = activeIndex();
      if (idx >= 0) next.turns[idx].items.push({ type: "error", message: msg });
      return next;
    }
    case "agent.turn_completed": {
      const idx = next.turns.findIndex((t) => t.message_id === data.message_id);
      if (idx >= 0) {
        next.turns[idx] = {
          ...next.turns[idx],
          completed_at: ev.ts,
          stop_reason: String(data.stop_reason ?? "end_turn"),
        };
      }
      return next;
    }
    default:
      return next;
  }
}

export function relativeTime(iso?: string | null): string {
  if (!iso) return "";
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return "";
  const diff = Math.floor((Date.now() - ts) / 1000);
  if (diff < 45) return "刚刚";
  if (diff < 3600) return `${Math.floor(diff / 60)} 分钟前`;
  if (diff < 86400) return `${Math.floor(diff / 3600)} 小时前`;
  const d = new Date(ts);
  return `${d.getMonth() + 1}-${String(d.getDate()).padStart(2, "0")} ${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

export function resetCountdown(iso?: string | null): string | null {
  if (!iso) return null;
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return null;
  const diff = Math.floor((ts - Date.now()) / 1000);
  if (diff <= 0) return "即将重置";
  const days = Math.floor(diff / 86400);
  const hours = Math.floor((diff % 86400) / 3600);
  const mins = Math.floor((diff % 3600) / 60);
  if (days > 0) return `${days} 天 ${hours} 小时后重置`;
  if (hours > 0) return `${hours} 小时 ${mins} 分钟后重置`;
  return `${Math.max(1, mins)} 分钟后重置`;
}

export function shortWorkspace(agent: Agent): string {
  const raw = agent.home || agent.cwd || "";
  if (!raw) return "";
  const parts = raw.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? raw;
}

export function nodeDisplayName(
  nodeId: string | null | undefined,
  nodes: RemoteNodeInfo[],
  hubInfo?: HubInfo,
  fallbackHost?: string,
): string {
  const key = (nodeId ?? "").trim();
  if (!key || key === "local") {
    const local = nodes.find((n) => n.local);
    if (local) return local.hostname ? `${local.hostname}（中心）` : "中心电脑";
    if (hubInfo?.machine?.hostname) return `${hubInfo.machine.hostname}（中心）`;
    return fallbackHost || "中心电脑";
  }
  const found = nodes.find((n) => n.id === key || n.name === key || n.hostname === key);
  return found ? found.name || found.hostname : key;
}

export async function compressImageForUpload(
  file: File,
  maxDimension = 1600,
): Promise<{ previewUrl: string; upload: AttachmentUpload }> {
  const dataUrl = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error("读取图片失败"));
    reader.readAsDataURL(file);
  });

  const img = await new Promise<HTMLImageElement>((resolve, reject) => {
    const el = new Image();
    el.onload = () => resolve(el);
    el.onerror = () => reject(new Error("解码图片失败"));
    el.src = dataUrl;
  });

  let { width, height } = img;
  if (Math.max(width, height) > maxDimension) {
    const scale = maxDimension / Math.max(width, height);
    width = Math.round(width * scale);
    height = Math.round(height * scale);
  }

  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("浏览器不支持画布压缩");
  ctx.drawImage(img, 0, 0, width, height);

  const jpegUrl = canvas.toDataURL("image/jpeg", 0.82);
  const comma = jpegUrl.indexOf(",");
  const base64 = comma >= 0 ? jpegUrl.slice(comma + 1) : jpegUrl;
  const cleanName = (file.name || "photo").replace(/\.[^.]+$/, "") + ".jpg";

  return {
    previewUrl: jpegUrl,
    upload: {
      name: cleanName,
      mime: "image/jpeg",
      data: base64,
    },
  };
}

export async function prepareFileForUpload(
  file: File,
  mime: string,
): Promise<{ previewUrl?: string; size: number; upload: AttachmentUpload }> {
  if (/^image\/(png|jpeg|webp)$/i.test(mime)) {
    const compressed = await compressImageForUpload(file);
    return {
      previewUrl: compressed.previewUrl,
      size: Math.floor((compressed.upload.data.length * 3) / 4),
      upload: compressed.upload,
    };
  }
  const dataUrl = await new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result));
    reader.onerror = () => reject(new Error(`读取文件失败：${file.name}`));
    reader.readAsDataURL(file);
  });
  const comma = dataUrl.indexOf(",");
  const base64 = comma >= 0 ? dataUrl.slice(comma + 1) : dataUrl;
  return {
    previewUrl: mime.startsWith("image/") ? dataUrl : undefined,
    size: file.size,
    upload: {
      name: file.name || "attachment",
      mime,
      data: base64,
    },
  };
}

export function extractErrorMessage(status: number, body: string): string {
  try {
    const parsed = JSON.parse(body) as { error?: { message?: string } };
    if (parsed.error?.message) return parsed.error.message;
  } catch {
    // ignore
  }
  return body.trim() || `HTTP ${status}`;
}

export function availableEfforts(plugin: PluginInfo | undefined, model: string): string[] {
  if (!plugin) return [];
  if (model && plugin.efforts_by_model?.[model]) {
    return plugin.efforts_by_model[model];
  }
  return plugin.efforts ?? [];
}
