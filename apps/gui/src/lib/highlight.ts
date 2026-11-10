// A small line-by-line highlighter for the file viewer: enough to read code, not a parser.
// Languages: rs, ts/tsx, js/jsx, json, md, toml, py. State that spans lines (block comments,
// multi-line strings, fenced code) is carried from one line to the next.

export type Lang = "rs" | "ts" | "js" | "json" | "md" | "toml" | "py" | "text";
export type TokKind = "kw" | "str" | "num" | "com" | "fn" | "type" | "punct" | "key" | "head" | "code" | "link" | "lit" | "plain";
export type Token = { k: TokKind; s: string };

export function langOf(path: string): Lang {
  const ext = path.toLowerCase().split("/").pop()!.split(".").pop() ?? "";
  switch (ext) {
    case "rs":
      return "rs";
    case "ts":
    case "tsx":
    case "mts":
    case "cts":
      return "ts";
    case "js":
    case "jsx":
    case "mjs":
    case "cjs":
      return "js";
    case "json":
    case "jsonc":
      return "json";
    case "md":
    case "markdown":
      return "md";
    case "toml":
      return "toml";
    case "py":
      return "py";
    default:
      return "text";
  }
}

const words = (s: string) => new Set(s.split(" "));
const KW: Partial<Record<Lang, Set<string>>> = {
  rs: words(
    "as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while",
  ),
  ts: words(
    "abstract as async await break case catch class const continue declare default delete do else enum export extends finally for from function get if implements import in instanceof interface keyof let namespace new of private protected public readonly return satisfies set static super switch this throw try type typeof var void while yield",
  ),
  js: words(
    "async await break case catch class const continue default delete do else export extends finally for from function get if import in instanceof let new of return set static super switch this throw try typeof var void while yield",
  ),
  py: words(
    "and as assert async await break class continue def del elif else except finally for from global if import in is lambda nonlocal not or pass raise return try while with yield",
  ),
};
const LIT = words("true false null undefined None True False NaN Infinity");

/** Carried between lines: inside a block comment, a multi-line string, a fenced block. */
export type HlState = { block?: "com" | "str" | "fence"; close?: string };

export function highlight(text: string, lang: Lang): Token[][] {
  const lines = text.split(/\r?\n/);
  const out: Token[][] = [];
  let st: HlState = {};
  for (const line of lines) {
    const r = highlightLine(line, lang, st);
    out.push(r.tokens);
    st = r.state;
  }
  return out;
}

export function highlightLine(line: string, lang: Lang, state: HlState = {}): { tokens: Token[]; state: HlState } {
  if (lang === "text") return { tokens: line ? [{ k: "plain", s: line }] : [], state: {} };
  if (lang === "md") return md(line, state);
  return code(line, lang, state);
}

function push(out: Token[], k: TokKind, s: string) {
  if (!s) return;
  const last = out[out.length - 1];
  if (last && last.k === k) last.s += s;
  else out.push({ k, s });
}

function md(line: string, state: HlState): { tokens: Token[]; state: HlState } {
  const out: Token[] = [];
  if (state.block === "fence") {
    // Closes only on a bare run of the same character at least as long (CommonMark).
    const close = /^\s*(`{3,}|~{3,})\s*$/.exec(line)?.[1];
    const ends = !!close && !!state.close && close[0] === state.close[0] && close.length >= state.close.length;
    push(out, "code", line);
    return { tokens: out, state: ends ? {} : state };
  }
  const fence = /^\s*(`{3,}|~{3,})/.exec(line);
  if (fence) {
    push(out, "code", line);
    return { tokens: out, state: { block: "fence", close: fence[1] } };
  }
  if (/^\s{0,3}#{1,6}\s/.test(line)) {
    push(out, "head", line);
    return { tokens: out, state: {} };
  }
  const lead = /^(\s*(?:[-*+]|\d+[.)]|>)\s+)/.exec(line);
  let i = 0;
  if (lead) {
    push(out, "punct", lead[1]);
    i = lead[1].length;
  }
  const re = /(`[^`]+`)|(\[[^\]]*\]\([^)]*\))|(\*\*[^*]+\*\*)/g;
  re.lastIndex = i;
  let m: RegExpExecArray | null;
  while ((m = re.exec(line))) {
    push(out, "plain", line.slice(i, m.index));
    push(out, m[1] ? "code" : m[2] ? "link" : "key", m[0]);
    i = m.index + m[0].length;
  }
  push(out, "plain", line.slice(i));
  return { tokens: out, state: {} };
}

function code(line: string, lang: Lang, state: HlState): { tokens: Token[]; state: HlState } {
  const out: Token[] = [];
  let i = 0;
  const n = line.length;
  // Continue what the previous line left open.
  if (state.block === "com" || state.block === "str") {
    const close = state.close!;
    const at = line.indexOf(close);
    if (at < 0) {
      push(out, state.block, line);
      return { tokens: out, state };
    }
    push(out, state.block, line.slice(0, at + close.length));
    i = at + close.length;
  }
  const kw = KW[lang];
  const slashCom = lang === "rs" || lang === "ts" || lang === "js" || lang === "json";
  const hashCom = lang === "py" || lang === "toml";
  // TOML: a `[table]` header or a `key =` at the start of the line.
  if (lang === "toml" && i === 0) {
    const head = /^\s*\[\[?[^\]]*\]\]?/.exec(line);
    if (head) {
      push(out, "head", head[0]);
      i = head[0].length;
    } else {
      const key = /^(\s*)([A-Za-z0-9_.\-"]+)(\s*=)/.exec(line);
      if (key) {
        push(out, "plain", key[1]);
        push(out, "key", key[2]);
        push(out, "punct", key[3]);
        i = key[0].length;
      }
    }
  }
  while (i < n) {
    const c = line[i];
    const rest = line.slice(i);
    if (slashCom && rest.startsWith("//")) {
      push(out, "com", rest);
      return { tokens: out, state: {} };
    }
    if (slashCom && rest.startsWith("/*")) {
      const end = line.indexOf("*/", i + 2);
      if (end < 0) {
        push(out, "com", rest);
        return { tokens: out, state: { block: "com", close: "*/" } };
      }
      push(out, "com", line.slice(i, end + 2));
      i = end + 2;
      continue;
    }
    if (hashCom && c === "#") {
      push(out, "com", rest);
      return { tokens: out, state: {} };
    }
    // Python and TOML triple-quoted strings may span lines.
    if ((lang === "py" || lang === "toml") && (rest.startsWith('"""') || rest.startsWith("'''"))) {
      const q = rest.slice(0, 3);
      const end = line.indexOf(q, i + 3);
      if (end < 0) {
        push(out, "str", rest);
        return { tokens: out, state: { block: "str", close: q } };
      }
      push(out, "str", line.slice(i, end + 3));
      i = end + 3;
      continue;
    }
    // A Rust lifetime or char: 'a vs 'x'.
    if (lang === "rs" && c === "'") {
      const ch = /^'(\\.|[^\\'])'/.exec(rest);
      if (ch) {
        push(out, "str", ch[0]);
        i += ch[0].length;
        continue;
      }
      const life = /^'[A-Za-z_]\w*/.exec(rest);
      if (life) {
        push(out, "type", life[0]);
        i += life[0].length;
        continue;
      }
    }
    if (c === '"' || c === "`" || (c === "'" && lang !== "rs")) {
      let j = i + 1;
      while (j < n && line[j] !== c) j += line[j] === "\\" ? 2 : 1;
      const s = line.slice(i, Math.min(j + 1, n));
      // JSON: a string followed by `:` is a key.
      const isKey = lang === "json" && /^\s*:/.test(line.slice(j + 1));
      push(out, isKey ? "key" : "str", s);
      if (j >= n && c === "`" && (lang === "ts" || lang === "js")) return { tokens: out, state: { block: "str", close: "`" } };
      i = j + 1;
      continue;
    }
    const num = /^(0x[0-9a-fA-F_]+|\d[\d_]*(\.\d+)?([eE][+-]?\d+)?)[a-zA-Z0-9]*/.exec(rest);
    if (num && !/\w/.test(line[i - 1] ?? "")) {
      push(out, "num", num[0]);
      i += num[0].length;
      continue;
    }
    const id = /^[A-Za-z_$][\w$]*/.exec(rest);
    if (id) {
      const w = id[0];
      const after = line.slice(i + w.length);
      let k: TokKind = "plain";
      if (kw?.has(w)) k = "kw";
      else if (LIT.has(w)) k = "lit";
      else if (lang === "rs" && after.startsWith("!")) k = "fn";
      else if (/^\s*\(/.test(after) && lang !== "json" && lang !== "toml") k = "fn";
      else if (/^[A-Z]/.test(w) && lang !== "json" && lang !== "toml") k = "type";
      push(out, k, w);
      i += w.length;
      continue;
    }
    if (lang === "py" && c === "@") {
      const dec = /^@[\w.]+/.exec(rest);
      if (dec) {
        push(out, "fn", dec[0]);
        i += dec[0].length;
        continue;
      }
    }
    if (lang === "rs" && c === "#" && rest.startsWith("#[")) {
      const end = line.indexOf("]", i);
      const s = end < 0 ? rest : line.slice(i, end + 1);
      push(out, "lit", s);
      i += s.length;
      continue;
    }
    push(out, /[{}()[\];,.:=<>+\-*/%&|!?^~]/.test(c) ? "punct" : "plain", c);
    i++;
  }
  return { tokens: out, state: {} };
}
