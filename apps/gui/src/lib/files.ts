// Pure helpers for the file pane: Markdown headings and which one is in view, the workspace
// name filter, and git changes rolled up to their folders. UI in FileViewer.tsx; tested in
// files.test.ts.

import type { FileGitChange } from "@bindings/FileGitChange";
import { parseBlocks, type Block } from "./markdown.ts";

export type Heading = {
  /** 1–6. */
  level: number;
  /** The heading's text without Markdown markers, for the outline's labels. */
  text: string;
  /** Its place among the document's headings, from 0. */
  index: number;
};

/** Every heading, in reading order, outside fenced code; nested ones included. */
export function outlineOf(markdown: string): Heading[] {
  const out: Heading[] = [];
  collect(parseBlocks(markdown), out);
  return out;
}

function collect(blocks: Block[], out: Heading[]) {
  for (const block of blocks) {
    if (block.t === "h") out.push({ level: block.level, text: plainHeading(block.text), index: out.length });
    else if (block.t === "list") for (const item of block.items) collect(item.children, out);
    else if (block.t === "quote") collect(block.blocks, out);
  }
}

/** `## **Hello** [world](x)` → `Hello world`. */
function plainHeading(text: string): string {
  return text
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/[*_~`]/g, "")
    .trim();
}

/**
 * The heading whose section contains `at` (a scroll offset): the last one at or above it.
 * -1 when the first heading is still below.
 */
export function currentHeading(tops: number[], at: number): number {
  let current = -1;
  for (let i = 0; i < tops.length; i++) {
    if (tops[i] <= at) current = i;
    else break;
  }
  return current;
}

/** Paths containing `query` (case-insensitive), in order, at most `limit`. */
export function filterPaths(paths: string[], query: string, limit = 200): string[] {
  const needle = query.trim().toLowerCase();
  if (!needle) return [];
  return paths.filter((path) => path.toLowerCase().includes(needle)).slice(0, limit);
}

export type ChangeMark = { files: Map<string, FileGitChange>; dirs: Set<string> };

/** File changes by path, and every folder above one (for the tree's dot). */
export function changeIndex(entries: { path: string; change: FileGitChange }[]): ChangeMark {
  const files = new Map<string, FileGitChange>();
  const dirs = new Set<string>();
  for (const entry of entries) {
    files.set(entry.path, entry.change);
    const parts = entry.path.split("/");
    for (let i = 1; i < parts.length; i++) dirs.add(parts.slice(0, i).join("/"));
  }
  return { files, dirs };
}

/** The one-letter mark a changed file gets. */
export function changeMark(change: FileGitChange): string {
  return change === "added" ? "A" : change === "deleted" ? "D" : "M";
}

export const dirOf = (path: string) => (path.includes("/") ? path.slice(0, path.lastIndexOf("/")) : "");
export const baseOf = (path: string) => path.slice(path.lastIndexOf("/") + 1);
