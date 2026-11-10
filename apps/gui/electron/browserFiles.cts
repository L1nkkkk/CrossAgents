// Local files for the built-in browser: a file's URL (`crossagents-file://<root>/<name>`), the
// folder it may read from, and the check that a request stays inside that folder. Pure, so the
// rules (`..`, drive letters, case on Windows) can be tested without Electron.

import { createHash } from "node:crypto";
import { posix, win32 } from "node:path";
import { FILE_SCHEME } from "./browserPolicy.cjs";

type PathApi = typeof win32;

function apiFor(platform: NodeJS.Platform): PathApi {
  return platform === "win32" ? win32 : posix;
}

/** Whether `target` is inside `root` (a subfolder, not the folder itself). Case-insensitive on Windows. */
export function isWithin(platform: NodeJS.Platform, root: string, target: string): boolean {
  const api = apiFor(platform);
  const rel = api.relative(root, target);
  return rel !== "" && rel !== ".." && !rel.startsWith(`..${api.sep}`) && !api.isAbsolute(rel);
}

/** How many registered folders are kept (and saved); the oldest go first. */
export const ROOTS_LIMIT = 200;

/** A registered folder: its id (see `rootIdOf`) and its absolute path. */
export type RootEntry = [id: string, dir: string];

export type FileRegistry = {
  /** Registers the folder of `absPath` as readable and returns the URL of that file. */
  register(absPath: string): string;
  /** The file a `crossagents-file://` URL names, or an error when it leaves its folder. */
  resolve(url: string): { root: string; path: string };
};

/** The URL root of a folder: a hash of the folder (case-folded on Windows). */
export function rootIdOf(platform: NodeJS.Platform, dir: string): string {
  const fold = platform === "win32" ? dir.toLowerCase() : dir;
  return createHash("sha256").update(fold).digest("hex").slice(0, 32);
}

/** The saved form of the registry: JSON, the newest `ROOTS_LIMIT` folders, oldest first. */
export function encodeRoots(entries: readonly RootEntry[]): string {
  const kept = entries.slice(-ROOTS_LIMIT).map(([id, dir]) => ({ id, dir }));
  return `${JSON.stringify({ version: 1, roots: kept }, null, 2)}\n`;
}

/**
 * Reads the saved form back. `text` is null when there is no file. An entry is kept only when its
 * id matches its folder and `isFolder` says the folder still exists; anything else is dropped.
 * Oldest first, at most `ROOTS_LIMIT`.
 */
export function decodeRoots(platform: NodeJS.Platform, text: string | null, isFolder: (dir: string) => boolean): RootEntry[] {
  if (text === null) return [];
  let data: unknown;
  try {
    data = JSON.parse(text);
  } catch {
    return [];
  }
  const list = (data as { roots?: unknown } | null)?.roots;
  if (!Array.isArray(list)) return [];
  const api = apiFor(platform);
  // A Map keeps one position per id: a repeated entry moves to the newest place.
  const kept = new Map<string, string>();
  for (const item of list) {
    if (!item || typeof item !== "object") continue;
    const { id, dir } = item as { id?: unknown; dir?: unknown };
    if (typeof id !== "string" || typeof dir !== "string") continue;
    if (!api.isAbsolute(dir) || rootIdOf(platform, dir) !== id || !isFolder(dir)) continue;
    kept.delete(id);
    kept.set(id, dir);
  }
  return Array.from(kept.entries()).slice(-ROOTS_LIMIT);
}

/**
 * Folders registered by `register`, keyed by `rootIdOf`, so the same folder always gives the same
 * URL root. `initial` restores saved folders (oldest first); `onChange` gets the whole list after
 * every registration, so it can be saved.
 */
export function createFileRegistry(
  platform: NodeJS.Platform,
  initial: readonly RootEntry[] = [],
  onChange?: (entries: RootEntry[]) => void,
): FileRegistry {
  const api = apiFor(platform);
  const roots = new Map<string, string>(initial);
  const entries = (): RootEntry[] => Array.from(roots.entries());
  return {
    register(absPath: string): string {
      if (!api.isAbsolute(absPath)) throw new Error("文件路径必须是绝对路径");
      const file = api.resolve(absPath);
      const dir = api.dirname(file);
      const base = api.basename(file);
      if (!base || base === "." || base === "..") throw new Error("不是一个文件");
      const id = rootIdOf(platform, dir);
      // Re-registering moves the folder to the newest place, so the oldest are the ones not used lately.
      const known = roots.get(id) ?? dir;
      roots.delete(id);
      roots.set(id, known);
      while (roots.size > ROOTS_LIMIT) roots.delete(roots.keys().next().value as string);
      onChange?.(entries());
      return `${FILE_SCHEME}://${id}/${encodeURIComponent(base)}`;
    },
    resolve(url: string): { root: string; path: string } {
      let parsed: URL;
      try {
        parsed = new URL(url);
      } catch {
        throw new Error("不是有效的文件地址");
      }
      if (parsed.protocol !== `${FILE_SCHEME}:`) throw new Error("不是本地文件地址");
      const root = roots.get(parsed.hostname);
      if (!root) throw new Error("这个文件所在的文件夹没有登记");
      const parts = parsed.pathname.split("/").slice(1);
      if (parts.length === 0 || parts.some((p) => p === "")) throw new Error("不是文件地址");
      const segments = parts.map((p) => {
        try {
          return decodeURIComponent(p);
        } catch {
          throw new Error("文件地址的编码有错");
        }
      });
      for (const seg of segments) {
        if (seg === "." || seg === ".." || /[/\\\0]/.test(seg) || (platform === "win32" && seg.includes(":"))) {
          throw new Error("路径越出了允许的文件夹");
        }
      }
      const path = api.join(root, ...segments);
      if (!isWithin(platform, root, path)) throw new Error("路径越出了允许的文件夹");
      return { root, path };
    },
  };
}

const MIME: ReadonlyMap<string, string> = new Map([
  [".html", "text/html; charset=utf-8"],
  [".htm", "text/html; charset=utf-8"],
  [".css", "text/css; charset=utf-8"],
  [".js", "text/javascript; charset=utf-8"],
  [".mjs", "text/javascript; charset=utf-8"],
  [".json", "application/json; charset=utf-8"],
  [".txt", "text/plain; charset=utf-8"],
  [".md", "text/markdown; charset=utf-8"],
  [".svg", "image/svg+xml"],
  [".png", "image/png"],
  [".jpg", "image/jpeg"],
  [".jpeg", "image/jpeg"],
  [".gif", "image/gif"],
  [".webp", "image/webp"],
  [".avif", "image/avif"],
  [".ico", "image/x-icon"],
  [".woff", "font/woff"],
  [".woff2", "font/woff2"],
  [".ttf", "font/ttf"],
  [".otf", "font/otf"],
  [".mp4", "video/mp4"],
  [".webm", "video/webm"],
  [".mp3", "audio/mpeg"],
  [".wav", "audio/wav"],
  [".pdf", "application/pdf"],
]);

/** The Content-Type a file is served with; octet-stream when the extension is not known. */
export function mimeFor(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  const ext = dot < 0 ? "" : name.slice(dot).toLowerCase();
  return MIME.get(ext) ?? "application/octet-stream";
}
