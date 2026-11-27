// Mobile-optimized multi-format media & attachment helpers.
// Fetches files from Hub over any HubTransport (WebRTC P2P, iroh WASM, direct HTTPS)
// in 1.5 MiB chunks, builds Blob ObjectURLs with LRU eviction, and handles MIME detection
// when mobile OS pickers omit MIME types.

import type { HubTransport } from "./transport";

export type MediaKind = "image" | "audio" | "video" | "pdf" | "text" | "file";

export interface LoadedMobileMedia {
  url: string;
  blob: Blob;
  mime: string;
  name: string;
  size: number;
  kind: MediaKind;
  text?: string;
}

export const MOBILE_MAX_FILES = 8;
export const MOBILE_MAX_FILE_BYTES = 25 * 1024 * 1024;
export const MOBILE_MAX_TOTAL_BYTES = 40 * 1024 * 1024;

export const MOBILE_FILE_ACCEPT = [
  "image/*",
  "audio/*",
  "video/*",
  "application/pdf",
  "text/*",
  "application/json",
  ".txt,.md,.csv,.log,.json,.xml,.yaml,.yml,.toml,.sql,.rs,.ts,.tsx,.js,.py",
  ".mp3,.wav,.m4a,.ogg,.opus,.flac,.aac,.mp4,.webm,.mov,.m4v,.mkv",
  ".pdf,.doc,.docx,.xls,.xlsx,.ppt,.pptx,.zip,.gz,.tar,.rtf,.epub",
].join(",");

const EXT_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  svg: "image/svg+xml",
  pdf: "application/pdf",
  txt: "text/plain",
  md: "text/markdown",
  markdown: "text/markdown",
  csv: "text/csv",
  json: "application/json",
  xml: "application/xml",
  yaml: "application/yaml",
  yml: "application/yaml",
  toml: "application/toml",
  log: "text/plain",
  html: "text/html",
  htm: "text/html",
  css: "text/css",
  js: "text/javascript",
  ts: "text/plain",
  tsx: "text/plain",
  py: "text/plain",
  rs: "text/plain",
  go: "text/plain",
  sql: "application/sql",
  rtf: "application/rtf",
  mp3: "audio/mpeg",
  wav: "audio/wav",
  m4a: "audio/mp4",
  ogg: "audio/ogg",
  oga: "audio/ogg",
  opus: "audio/opus",
  weba: "audio/webm",
  flac: "audio/flac",
  aac: "audio/aac",
  mp4: "video/mp4",
  webm: "video/webm",
  mov: "video/quicktime",
  m4v: "video/x-m4v",
  ogv: "video/ogg",
  mkv: "video/x-matroska",
  zip: "application/zip",
  gz: "application/gzip",
  tar: "application/x-tar",
  doc: "application/msword",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xls: "application/vnd.ms-excel",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  ppt: "application/vnd.ms-powerpoint",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  epub: "application/epub+zip",
};

const CHUNK_BYTES = 1536 * 1024; // 1.5 MiB chunks for smooth WebRTC/iroh mobile transfers
const MAX_CACHE = 32;
const TEXT_PREVIEW_CAP = 384 * 1024;

const cache = new Map<string, LoadedMobileMedia>();
const inFlight = new Map<string, Promise<LoadedMobileMedia>>();

export function inferMime(name: string, declared: string): string {
  const bare = (declared || "").split(";")[0].trim().toLowerCase();
  if (bare && bare !== "application/octet-stream") return bare;
  const ext = (name.split(".").pop() || "").toLowerCase();
  return EXT_MIME[ext] ?? bare;
}

export function mediaKindOfMime(mime: string): MediaKind | undefined {
  const bare = (mime || "").split(";")[0].trim().toLowerCase();
  if (!bare) return undefined;
  if (bare.startsWith("image/")) return "image";
  if (bare.startsWith("audio/")) return "audio";
  if (bare.startsWith("video/")) return "video";
  if (bare === "application/pdf") return "pdf";
  if (
    bare.startsWith("text/") ||
    bare === "application/json" ||
    bare === "application/xml" ||
    bare === "application/yaml" ||
    bare === "application/toml" ||
    bare === "application/sql" ||
    bare === "application/rtf"
  ) {
    return "text";
  }
  if (
    bare === "application/zip" ||
    bare === "application/x-zip-compressed" ||
    bare === "application/gzip" ||
    bare === "application/x-tar" ||
    bare === "application/msword" ||
    bare === "application/vnd.ms-excel" ||
    bare === "application/vnd.ms-powerpoint" ||
    bare === "application/epub+zip" ||
    bare.startsWith("application/vnd.openxmlformats-officedocument.")
  ) {
    return "file";
  }
  return undefined;
}

export function mediaKindOfPath(path: string): MediaKind | undefined {
  const clean = path.split(/[?#]/)[0] || path;
  const dot = clean.lastIndexOf(".");
  if (dot <= 0 || dot === clean.length - 1) return undefined;
  const ext = clean.slice(dot + 1).toLowerCase();
  const mime = EXT_MIME[ext];
  return mime ? mediaKindOfMime(mime) : undefined;
}

export function formatBytes(n: number): string {
  if (n >= 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(n >= 10 * 1024 * 1024 ? 0 : 1)} MB`;
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

function decodeBase64(b64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(b64);
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes;
}

function putCache(key: string, entry: LoadedMobileMedia) {
  const prev = cache.get(key);
  if (prev) {
    if (prev.url !== entry.url) URL.revokeObjectURL(prev.url);
    cache.delete(key);
  }
  cache.set(key, entry);
  while (cache.size > MAX_CACHE) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey === undefined) break;
    const old = cache.get(oldestKey);
    cache.delete(oldestKey);
    if (old) URL.revokeObjectURL(old.url);
  }
}

export function getCachedMobileMedia(key: string): LoadedMobileMedia | undefined {
  const hit = cache.get(key);
  if (hit) {
    cache.delete(key);
    cache.set(key, hit);
  }
  return hit;
}

export function mobileMediaKey(agentId: string, path: string): string {
  return `m:${agentId}:${path}`;
}

export function mobileAttKey(agentId: string, attId: string): string {
  return `a:${agentId}:${attId}`;
}

function parseErr(status: number, body: string): string {
  try {
    const j = JSON.parse(body) as { error?: { message?: string } };
    if (j.error?.message) return j.error.message;
  } catch {
    // ignore
  }
  return body.trim() || `HTTP ${status}`;
}

export async function loadMobileAgentMedia(
  transport: HubTransport,
  agentId: string,
  path: string,
  onProgress?: (loaded: number, total: number) => void,
): Promise<LoadedMobileMedia> {
  const key = mobileMediaKey(agentId, path);
  const hit = getCachedMobileMedia(key);
  if (hit) return hit;

  const running = inFlight.get(key);
  if (running) return running;

  const task = (async (): Promise<LoadedMobileMedia> => {
    const parts: Uint8Array<ArrayBuffer>[] = [];
    let offset = 0;
    let total = 0;
    let mime = "application/octet-stream";
    let name = path.split(/[\\/]/).pop() || "file";
    let kind: MediaKind = mediaKindOfPath(path) ?? "file";

    while (true) {
      const url = `/v1/agents/${encodeURIComponent(agentId)}/files/media?path=${encodeURIComponent(path)}&offset=${offset}&limit=${CHUNK_BYTES}`;
      const res = await transport.request("GET", url);
      if (res.status >= 400) throw new Error(parseErr(res.status, res.body));
      const chunk = JSON.parse(res.body) as {
        name: string;
        mime: string;
        kind: MediaKind;
        size: number;
        data: string;
        eof: boolean;
      };
      mime = chunk.mime || mime;
      name = chunk.name || name;
      kind = chunk.kind || kind;
      total = chunk.size;
      const bytes = chunk.data ? decodeBase64(chunk.data) : new Uint8Array(new ArrayBuffer(0));
      parts.push(bytes);
      offset += bytes.byteLength;
      onProgress?.(offset, total);
      if (chunk.eof || bytes.byteLength === 0 || offset >= total) break;
    }

    const blob = new Blob(parts, { type: mime });
    const blobUrl = URL.createObjectURL(blob);
    let text: string | undefined;
    if (kind === "text" && blob.size <= TEXT_PREVIEW_CAP) {
      try {
        text = await blob.text();
      } catch {
        // ignore
      }
    }
    const entry: LoadedMobileMedia = { url: blobUrl, blob, mime, name, size: total || blob.size, kind, text };
    putCache(key, entry);
    return entry;
  })().finally(() => {
    inFlight.delete(key);
  });

  inFlight.set(key, task);
  return task;
}

export async function loadMobileAttachment(
  transport: HubTransport,
  agentId: string,
  attId: string,
  name: string,
  declaredMime: string,
  onProgress?: (loaded: number, total: number) => void,
): Promise<LoadedMobileMedia> {
  const key = mobileAttKey(agentId, attId);
  const hit = getCachedMobileMedia(key);
  if (hit) return hit;

  const running = inFlight.get(key);
  if (running) return running;

  const task = (async (): Promise<LoadedMobileMedia> => {
    const parts: Uint8Array<ArrayBuffer>[] = [];
    let offset = 0;
    let total = 0;
    let mime = inferMime(name, declaredMime) || "application/octet-stream";

    while (true) {
      const url = `/v1/agents/${encodeURIComponent(agentId)}/attachments/${encodeURIComponent(attId)}?offset=${offset}&limit=${CHUNK_BYTES}`;
      const res = await transport.request("GET", url);
      if (res.status >= 400) throw new Error(parseErr(res.status, res.body));
      const chunk = JSON.parse(res.body) as {
        mime: string;
        size: number;
        data: string;
        eof?: boolean;
      };
      if (chunk.mime && chunk.mime !== "text/plain") mime = chunk.mime;
      total = chunk.size;
      const bytes = chunk.data ? decodeBase64(chunk.data) : new Uint8Array(new ArrayBuffer(0));
      parts.push(bytes);
      offset += bytes.byteLength;
      onProgress?.(offset, total);
      if (chunk.eof !== false || bytes.byteLength === 0 || offset >= total) break;
    }

    const kind = mediaKindOfMime(mime) ?? mediaKindOfPath(name) ?? "file";
    const blob = new Blob(parts, { type: mime });
    const blobUrl = URL.createObjectURL(blob);
    let text: string | undefined;
    if (kind === "text" && blob.size <= TEXT_PREVIEW_CAP) {
      try {
        text = await blob.text();
      } catch {
        // ignore
      }
    }
    const entry: LoadedMobileMedia = { url: blobUrl, blob, mime, name, size: total || blob.size, kind, text };
    putCache(key, entry);
    return entry;
  })().finally(() => {
    inFlight.delete(key);
  });

  inFlight.set(key, task);
  return task;
}

export function triggerMobileDownload(url: string, filename: string) {
  const a = document.createElement("a");
  a.href = url;
  a.download = filename || "download";
  a.rel = "noopener";
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
}
