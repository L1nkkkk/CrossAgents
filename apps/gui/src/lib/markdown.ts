// The Markdown agents write, parsed into a small tree the GUI renders with React (no HTML
// strings, so nothing in a reply can inject markup). Covers what replies use: headings,
// paragraphs, fenced code, bullet and numbered lists (nested by indentation), quotes, rules,
// tables, and inline code, bold, italic, strikethrough and links. Pure; tested in
// markdown.test.ts.

import { fileLinkTarget } from "./filelinks.ts";

export type Inline =
  | { t: "text"; text: string }
  | { t: "code"; text: string }
  | { t: "strong" | "em" | "del"; children: Inline[] }
  | { t: "link"; href: string; children: Inline[] }
  /** A link to a local file (relative to the reply's scope, or absolute); opens the viewer. */
  | { t: "file"; path: string; line?: number; children: Inline[] }
  /** `![alt](target)`: an embedded image, or a local file the hub is asked for. */
  | { t: "image"; alt: string; src?: string; path?: string }
  | { t: "br" };

export type ListItem = { text: string; children: Block[] };

export type Block =
  | { t: "p"; text: string }
  | { t: "h"; level: number; text: string }
  | { t: "code"; lang: string; text: string }
  | { t: "list"; ordered: boolean; start: number; items: ListItem[] }
  | { t: "quote"; blocks: Block[] }
  | { t: "hr" }
  | { t: "table"; head: string[]; align: ("left" | "center" | "right" | undefined)[]; rows: string[][] };

// Fences as in CommonMark: a run of 3+ backticks or tildes, then an info string whose first word
// is the language (a backtick fence's info string has no backtick). It closes only on a bare run
// of the same character at least as long, so ```` can hold a ```text block.
const FENCE = /^([ \t]*)(`{3,}|~{3,})[ \t]*([^`]*?)[ \t]*$|^([ \t]*)(~{3,})[ \t]*(.*?)[ \t]*$/;
const closesFence = (line: string, marker: string) => {
  const m = line.match(/^[ \t]*(`{3,}|~{3,})[ \t]*$/);
  return !!m && m[1][0] === marker[0] && m[1].length >= marker.length;
};
const HEADING = /^(#{1,6})\s+(.*?)\s*#*\s*$/;
const RULE = /^\s*([-*_])(\s*\1){2,}\s*$/;
const ITEM = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/;
const QUOTE = /^\s*>\s?(.*)$/;
const TABLE_SEP = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/;

const indentOf = (s: string) => s.replace(/\t/g, "    ").length;
const cells = (line: string) =>
  line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => c.trim());

/** Block structure of `text`. */
export function parseBlocks(text: string): Block[] {
  const lines = text.replace(/\r\n?/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;
  let para: string[] = [];
  const flush = () => {
    if (para.length) blocks.push({ t: "p", text: para.join("\n") });
    para = [];
  };

  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(FENCE);
    if (fence) {
      flush();
      const indent = fence[1] ?? fence[4];
      const marker = fence[2] ?? fence[5];
      const info = fence[3] ?? fence[6];
      const strip = new RegExp(`^[ \\t]{0,${indent.length}}`);
      const body: string[] = [];
      i++;
      while (i < lines.length && !closesFence(lines[i], marker)) body.push(lines[i++].replace(strip, ""));
      i++;
      blocks.push({ t: "code", lang: info.split(/\s+/)[0], text: body.join("\n") });
      continue;
    }
    if (!line.trim()) {
      flush();
      i++;
      continue;
    }
    const heading = line.match(HEADING);
    if (heading) {
      flush();
      blocks.push({ t: "h", level: heading[1].length, text: heading[2] });
      i++;
      continue;
    }
    if (RULE.test(line)) {
      flush();
      blocks.push({ t: "hr" });
      i++;
      continue;
    }
    if (line.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes("-")) {
      flush();
      const head = cells(line);
      const align = cells(lines[i + 1]).map((c) => (c.startsWith(":") && c.endsWith(":") ? "center" : c.endsWith(":") ? "right" : c.startsWith(":") ? "left" : undefined));
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && lines[i].includes("|") && lines[i].trim()) rows.push(cells(lines[i++]));
      blocks.push({ t: "table", head, align, rows });
      continue;
    }
    if (QUOTE.test(line)) {
      flush();
      const body: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i])) body.push(lines[i++].match(QUOTE)![1]);
      blocks.push({ t: "quote", blocks: parseBlocks(body.join("\n")) });
      continue;
    }
    const item = line.match(ITEM);
    if (item) {
      flush();
      const [list, next] = parseList(lines, i);
      blocks.push(list);
      i = next;
      continue;
    }
    para.push(line);
    i++;
  }
  flush();
  return blocks;
}

/** A list starting at `start`: its items, with deeper-indented lines nested under them. */
function parseList(lines: string[], start: number): [Block, number] {
  const first = lines[start].match(ITEM)!;
  const base = indentOf(first[1]);
  const ordered = /\d/.test(first[2]);
  const items: ListItem[] = [];
  let i = start;
  while (i < lines.length) {
    const m = lines[i].match(ITEM);
    if (!m || indentOf(m[1]) !== base || /\d/.test(m[2]) !== ordered) break;
    const text = [m[3]];
    const nested: string[] = [];
    i++;
    // Continuation: lines indented past the marker, or blank lines followed by such lines.
    while (i < lines.length) {
      const l = lines[i];
      if (!l.trim()) {
        const after = lines.slice(i + 1).find((x) => x.trim());
        if (after !== undefined && indentOf(after.match(/^\s*/)![0]) > base) {
          nested.push("");
          i++;
          continue;
        }
        break;
      }
      const indent = indentOf(l.match(/^\s*/)![0]);
      if (indent <= base) break;
      if (!nested.length && !ITEM.test(l)) text.push(l.trim());
      else nested.push(l.slice(Math.min(l.length, base + 2)));
      i++;
    }
    items.push({ text: text.join("\n"), children: nested.length ? parseBlocks(nested.join("\n")) : [] });
  }
  return [{ t: "list", ordered, start: ordered ? parseInt(first[2], 10) : 1, items }, i];
}

const CJK = /[　-鿿＀-￯]/;

/**
 * A document's hard-wrapped lines as one paragraph: a line break reads as a space (nothing
 * between Chinese characters), unless the line ends in two spaces or `\`. A reply's line breaks
 * are meant, so only documents go through this.
 */
export function softBreaks(text: string): string {
  return text.split("\n").reduce((out, line) => {
    const hard = out.match(/( {2,}|\\)$/);
    if (hard) return `${out.slice(0, -hard[0].length)}\n${line.trimStart()}`;
    const next = line.trimStart();
    return out + (CJK.test(out.slice(-1)) && CJK.test(next.charAt(0)) ? "" : " ") + next;
  });
}

const INLINE = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([\s\S]+?)\*\*|__([\s\S]+?)__|~~([\s\S]+?)~~|\*([^\s*](?:[\s\S]*?[^\s*])?)\*|(?<![\w])_([^\s_](?:[\s\S]*?[^\s_])?)_(?![\w])|!\[([^\]]*)\]\(([^)\s]+)\)|\[([^\]]+)\]\(([^)\s]+)\)|\n/;

/** An image reference as a node; a web address stays a link, since the page loads no remote images. */
function imageNode(alt: string, target: string, all: string): Inline {
  if (/^data:image\/[a-z0-9.+-]+;base64,/i.test(target)) return { t: "image", alt, src: target };
  if (/^https?:/i.test(target)) return { t: "link", href: target, children: [{ t: "text", text: alt || target }] };
  const file = fileLinkTarget(target);
  return file ? { t: "image", alt, path: file.path } : { t: "text", text: all };
}

/** Inline spans of `text`. Links keep only http(s) and mailto targets and local files; others stay text. */
export function parseInline(text: string): Inline[] {
  const out: Inline[] = [];
  let rest = text;
  while (rest) {
    const m = rest.match(INLINE);
    if (!m || m.index === undefined) {
      out.push({ t: "text", text: rest });
      break;
    }
    if (m.index > 0) out.push({ t: "text", text: rest.slice(0, m.index) });
    const [all, , code, strong, strong2, del, em, em2, imageAlt, imageTarget, label, href] = m;
    if (imageTarget !== undefined) out.push(imageNode(imageAlt, imageTarget, all));
    else if (code !== undefined) out.push({ t: "code", text: code.trim() });
    else if (strong !== undefined || strong2 !== undefined) out.push({ t: "strong", children: parseInline(strong ?? strong2) });
    else if (del !== undefined) out.push({ t: "del", children: parseInline(del) });
    else if (em !== undefined || em2 !== undefined) out.push({ t: "em", children: parseInline(em ?? em2) });
    else if (label !== undefined) {
      if (/^(https?:|mailto:)/i.test(href)) out.push({ t: "link", href, children: parseInline(label) });
      else {
        const file = fileLinkTarget(href);
        if (file) out.push(file.line ? { t: "file", path: file.path, line: file.line, children: parseInline(label) } : { t: "file", path: file.path, children: parseInline(label) });
        else out.push({ t: "text", text: all });
      }
    } else out.push({ t: "br" });
    rest = rest.slice(m.index + all.length);
  }
  // Merge neighbouring text runs.
  return out.reduce<Inline[]>((acc, x) => {
    const last = acc[acc.length - 1];
    if (x.t === "text" && last?.t === "text") last.text += x.text;
    else acc.push(x);
    return acc;
  }, []);
}
