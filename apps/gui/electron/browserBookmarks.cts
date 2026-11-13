// Reads local Chrome / Edge plaintext Bookmarks files so the built-in browser can show the
// person's usual bookmark bar without requiring Google Account sync. Pure parsing and candidate
// path discovery are kept apart from fs so unit tests can verify them directly.

import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

export type BookmarkLink = {
  kind: "url";
  id: string;
  name: string;
  url: string;
};

export type BookmarkFolder = {
  kind: "folder";
  id: string;
  name: string;
  children: BookmarkLink[];
};

export type BookmarkEntry = BookmarkLink | BookmarkFolder;

type RawNode = {
  id?: unknown;
  type?: unknown;
  name?: unknown;
  url?: unknown;
  children?: unknown;
};

const MAX_BAR_ITEMS = 40;
const MAX_FOLDER_ITEMS = 60;

/** Only http(s) links belong on the bookmark bar; javascript: and file: are ignored. */
export function bookmarkUrlAllowed(url: unknown): url is string {
  if (typeof url !== "string" || !url.trim()) return false;
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" || parsed.protocol === "https:";
  } catch {
    return false;
  }
}

function toLink(node: RawNode, fallbackId: string): BookmarkLink | null {
  if (node.type !== "url" || !bookmarkUrlAllowed(node.url)) return null;
  const rawName = typeof node.name === "string" ? node.name.trim() : "";
  let name = rawName;
  if (!name) {
    try {
      name = new URL(node.url).host || node.url;
    } catch {
      name = node.url;
    }
  }
  const id = typeof node.id === "string" && node.id ? node.id : fallbackId;
  return { kind: "url", id, name, url: node.url };
}

function collectLinks(nodes: readonly unknown[], prefix: string): BookmarkLink[] {
  const out: BookmarkLink[] = [];
  const walk = (list: readonly unknown[], p: string) => {
    for (let i = 0; i < list.length && out.length < MAX_FOLDER_ITEMS; i++) {
      const item = list[i];
      if (!item || typeof item !== "object") continue;
      const node = item as RawNode;
      if (node.type === "url") {
        const link = toLink(node, `${p}-${i}`);
        if (link) out.push(link);
      } else if (node.type === "folder" && Array.isArray(node.children)) {
        walk(node.children, `${p}-${i}`);
      }
    }
  };
  walk(nodes, prefix);
  return out;
}

/**
 * Parses a Chromium `Bookmarks` JSON string into top-level bookmark-bar links and folders.
 * If `bookmark_bar` has nothing, falls back to `other`.
 */
export function parseChromeBookmarks(rawJson: string): BookmarkEntry[] {
  let parsed: { roots?: Record<string, { children?: unknown }> };
  try {
    parsed = JSON.parse(rawJson) as { roots?: Record<string, { children?: unknown }> };
  } catch {
    return [];
  }
  const roots = parsed?.roots;
  if (!roots || typeof roots !== "object") return [];
  const barChildren = Array.isArray(roots.bookmark_bar?.children) ? roots.bookmark_bar.children : [];
  const otherChildren = Array.isArray(roots.other?.children) ? roots.other.children : [];
  const source = barChildren.length > 0 ? barChildren : otherChildren;

  const entries: BookmarkEntry[] = [];
  for (let i = 0; i < source.length && entries.length < MAX_BAR_ITEMS; i++) {
    const item = source[i];
    if (!item || typeof item !== "object") continue;
    const node = item as RawNode;
    if (node.type === "url") {
      const link = toLink(node, `b-${i}`);
      if (link) entries.push(link);
    } else if (node.type === "folder" && Array.isArray(node.children)) {
      const children = collectLinks(node.children, `f-${i}`);
      if (!children.length) continue;
      const name = typeof node.name === "string" && node.name.trim() ? node.name.trim() : "文件夹";
      const id = typeof node.id === "string" && node.id ? node.id : `f-${i}`;
      entries.push({ kind: "folder", id, name, children });
    }
  }
  return entries;
}

/** Candidate Chromium `Bookmarks` files on the current platform, Chrome first, then Edge. */
export function candidateBookmarkPaths(
  platform: NodeJS.Platform = process.platform,
  env: NodeJS.ProcessEnv = process.env,
  home: string = homedir(),
): string[] {
  const profiles = ["Default", "Profile 1", "Profile 2", "Profile 3"];
  if (platform === "win32") {
    const local = env.LOCALAPPDATA || join(home, "AppData", "Local");
    const chromeRoot = join(local, "Google", "Chrome", "User Data");
    const edgeRoot = join(local, "Microsoft", "Edge", "User Data");
    return [
      ...profiles.map((p) => join(chromeRoot, p, "Bookmarks")),
      ...profiles.map((p) => join(edgeRoot, p, "Bookmarks")),
    ];
  }
  if (platform === "darwin") {
    const support = join(home, "Library", "Application Support");
    const chromeRoot = join(support, "Google", "Chrome");
    const edgeRoot = join(support, "Microsoft Edge");
    return [
      ...profiles.map((p) => join(chromeRoot, p, "Bookmarks")),
      ...profiles.map((p) => join(edgeRoot, p, "Bookmarks")),
    ];
  }
  const cfg = env.XDG_CONFIG_HOME || join(home, ".config");
  const chromeRoot = join(cfg, "google-chrome");
  const chromiumRoot = join(cfg, "chromium");
  return [
    ...profiles.map((p) => join(chromeRoot, p, "Bookmarks")),
    ...profiles.map((p) => join(chromiumRoot, p, "Bookmarks")),
  ];
}

/** Reads the first local Chrome / Edge Bookmarks file that has at least one bookmark. */
export function readLocalBookmarks(): { source: string | null; entries: BookmarkEntry[] } {
  for (const file of candidateBookmarkPaths()) {
    try {
      if (!existsSync(file)) continue;
      const entries = parseChromeBookmarks(readFileSync(file, "utf8"));
      if (entries.length > 0) return { source: file, entries };
    } catch {
      // Locked or unreadable profile: try the next candidate.
    }
  }
  return { source: null, entries: [] };
}
