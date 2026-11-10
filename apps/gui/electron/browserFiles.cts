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

export type FileRegistry = {
  /** Registers the folder of `absPath` as readable and returns the URL of that file. */
  register(absPath: string): string;
  /** The file a `crossagents-file://` URL names, or an error when it leaves its folder. */
  resolve(url: string): { root: string; path: string };
};

/**
 * Folders registered by `register`, keyed by a hash of the folder (case-folded on Windows), so
 * the same folder always gives the same URL root.
 */
export function createFileRegistry(platform: NodeJS.Platform): FileRegistry {
  const api = apiFor(platform);
  const roots = new Map<string, string>();
  const fold = (p: string) => (platform === "win32" ? p.toLowerCase() : p);
  return {
    register(absPath: string): string {
      if (!api.isAbsolute(absPath)) throw new Error("文件路径必须是绝对路径");
      const file = api.resolve(absPath);
      const dir = api.dirname(file);
      const base = api.basename(file);
      if (!base || base === "." || base === "..") throw new Error("不是一个文件");
      const id = createHash("sha256").update(fold(dir)).digest("hex").slice(0, 32);
      if (!roots.has(id)) roots.set(id, dir);
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
