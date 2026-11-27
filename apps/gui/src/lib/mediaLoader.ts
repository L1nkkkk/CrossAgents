// Chunked media & attachment loader for desktop (local & remote Tauri) GUI.
// Converts hub base64 chunks into Blob ObjectURLs (better seeking/memory for audio/video/PDF
// than `data:` URLs) with LRU eviction and one-click download support.

import type { MediaKind } from "@bindings/MediaKind";
import { api, errorText } from "../api";

export interface LoadedMedia {
  url: string;
  blob: Blob;
  mime: string;
  name: string;
  size: number;
  kind?: MediaKind;
  text?: string;
}

const CHUNK_BYTES = 3 * 1024 * 1024;
const MAX_CACHE_ENTRIES = 48;
const TEXT_PREVIEW_LIMIT = 512 * 1024;

const cache = new Map<string, LoadedMedia>();
const inFlight = new Map<string, Promise<LoadedMedia>>();

export function decodeBase64(b64: string): Uint8Array<ArrayBuffer> {
  const binary = atob(b64);
  const bytes = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

function storeCache(key: string, entry: LoadedMedia) {
  if (cache.has(key)) {
    const prev = cache.get(key)!;
    if (prev.url !== entry.url) URL.revokeObjectURL(prev.url);
    cache.delete(key);
  }
  cache.set(key, entry);
  while (cache.size > MAX_CACHE_ENTRIES) {
    const oldestKey = cache.keys().next().value;
    if (oldestKey === undefined) break;
    const oldest = cache.get(oldestKey);
    cache.delete(oldestKey);
    if (oldest) URL.revokeObjectURL(oldest.url);
  }
}

export function getCachedMedia(key: string): LoadedMedia | undefined {
  const hit = cache.get(key);
  if (hit) {
    cache.delete(key);
    cache.set(key, hit);
  }
  return hit;
}

export function mediaCacheKey(agent: string, path: string): string {
  return `media:${agent}:${path}`;
}

export function attachmentCacheKey(agent: string, id: string): string {
  return `att:${agent}:${id}`;
}

export async function loadAgentMedia(
  agent: string,
  path: string,
  onProgress?: (loaded: number, total: number) => void,
): Promise<LoadedMedia> {
  const key = mediaCacheKey(agent, path);
  const hit = getCachedMedia(key);
  if (hit) return hit;

  const existing = inFlight.get(key);
  if (existing) return existing;

  const task = (async (): Promise<LoadedMedia> => {
    const parts: Uint8Array<ArrayBuffer>[] = [];
    let offset = 0;
    let total = 0;
    let mime = "application/octet-stream";
    let name = path.split(/[\\/]/).pop() || "file";
    let kind: MediaKind | undefined;

    while (true) {
      const chunk = await api.agentMedia(agent, path, { offset, limit: CHUNK_BYTES });
      mime = chunk.mime;
      name = chunk.name || name;
      total = chunk.size;
      kind = chunk.kind;
      const decoded = chunk.data ? decodeBase64(chunk.data) : new Uint8Array(new ArrayBuffer(0));
      parts.push(decoded);
      offset += decoded.byteLength;
      onProgress?.(offset, total);
      if (chunk.eof || decoded.byteLength === 0 || offset >= total) break;
    }

    const blob = new Blob(parts, { type: mime });
    const url = URL.createObjectURL(blob);
    let text: string | undefined;
    if (kind === "text" && blob.size <= TEXT_PREVIEW_LIMIT) {
      try {
        text = await blob.text();
      } catch {
        // leave text undefined
      }
    }
    const entry: LoadedMedia = { url, blob, mime, name, size: total || blob.size, kind, text };
    storeCache(key, entry);
    return entry;
  })().finally(() => {
    inFlight.delete(key);
  });

  inFlight.set(key, task);
  return task;
}

export async function loadAttachmentBlob(
  agent: string,
  id: string,
  name: string,
  declaredMime?: string,
  onProgress?: (loaded: number, total: number) => void,
): Promise<LoadedMedia> {
  const key = attachmentCacheKey(agent, id);
  const hit = getCachedMedia(key);
  if (hit) return hit;

  const existing = inFlight.get(key);
  if (existing) return existing;

  const task = (async (): Promise<LoadedMedia> => {
    const parts: Uint8Array<ArrayBuffer>[] = [];
    let offset = 0;
    let total = 0;
    let mime = declaredMime || "application/octet-stream";

    while (true) {
      const chunk = await api.attachment(agent, id, { offset, limit: CHUNK_BYTES });
      if (chunk.mime && chunk.mime !== "text/plain") {
        mime = chunk.mime;
      } else if (!declaredMime) {
        mime = chunk.mime || mime;
      }
      total = chunk.size;
      const decoded = chunk.data ? decodeBase64(chunk.data) : new Uint8Array(new ArrayBuffer(0));
      parts.push(decoded);
      offset += decoded.byteLength;
      onProgress?.(offset, total);
      if (chunk.eof !== false || decoded.byteLength === 0 || offset >= total) break;
    }

    const blob = new Blob(parts, { type: mime });
    const url = URL.createObjectURL(blob);
    let text: string | undefined;
    if ((mime.startsWith("text/") || mime === "application/json") && blob.size <= TEXT_PREVIEW_LIMIT) {
      try {
        text = await blob.text();
      } catch {
        // ignore
      }
    }
    const entry: LoadedMedia = { url, blob, mime, name, size: total || blob.size, text };
    storeCache(key, entry);
    return entry;
  })().finally(() => {
    inFlight.delete(key);
  });

  inFlight.set(key, task);
  return task;
}

export function triggerDownload(url: string, filename: string) {
  const a = document.createElement("a");
  a.href = url;
  a.download = filename || "download";
  a.style.display = "none";
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
}

export function mediaErrorText(err: unknown): string {
  return errorText(err);
}
