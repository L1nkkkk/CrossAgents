// Desktop-grade Markdown prose + syntax-tinted unified diff renderer for CrossAgents Mobile.
// Supports headings, lists, blockquotes, horizontal rules, tables, fenced code blocks with copy,
// and inline bold / italic / strikethrough / code / links — zero raw HTML injection.

import { useState, type ReactNode } from "react";
import { Check, Copy } from "lucide-react";
import { mediaKindOfPath } from "../media";
import type { HubTransport } from "../transport";
import type { FileChange } from "../types";
import { MobileMediaCard } from "./MobileMediaCard";

type Block =
  | { kind: "heading"; level: number; text: string }
  | { kind: "code"; lang: string; code: string }
  | { kind: "quote"; lines: string[] }
  | { kind: "ul"; items: string[] }
  | { kind: "ol"; items: string[] }
  | { kind: "table"; headers: string[]; rows: string[][] }
  | { kind: "hr" }
  | { kind: "p"; text: string };

function isTableRow(line: string): boolean {
  const t = line.trim();
  return t.startsWith("|") && t.endsWith("|") && t.length > 2;
}

function isTableSep(line: string): boolean {
  const t = line.trim();
  return /^\|?[\s:|-]+\|?$/.test(t) && t.includes("-");
}

function splitTableCells(line: string): string[] {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => c.trim());
}

function parseBlocks(text: string): Block[] {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i];
    const trimmed = line.trim();

    if (!trimmed) {
      i++;
      continue;
    }

    // Fenced code block
    const fence = trimmed.match(/^(`{3,}|~{3,})(.*)$/);
    if (fence) {
      const marker = fence[1][0];
      const len = fence[1].length;
      const lang = fence[2].trim();
      const buf: string[] = [];
      i++;
      while (i < lines.length) {
        const close = lines[i].trim();
        if (close.startsWith(marker.repeat(len)) && close.replace(new RegExp(`^${marker}+`), "").trim() === "") {
          i++;
          break;
        }
        buf.push(lines[i]);
        i++;
      }
      blocks.push({ kind: "code", lang, code: buf.join("\n") });
      continue;
    }

    // Horizontal rule
    if (/^([-*_])(\s*\1){2,}$/.test(trimmed)) {
      blocks.push({ kind: "hr" });
      i++;
      continue;
    }

    // Heading
    const head = trimmed.match(/^(#{1,6})\s+(.+)$/);
    if (head) {
      blocks.push({ kind: "heading", level: head[1].length, text: head[2].trim() });
      i++;
      continue;
    }

    // GFM Table
    if (isTableRow(line) && i + 1 < lines.length && isTableSep(lines[i + 1])) {
      const headers = splitTableCells(line);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length && isTableRow(lines[i])) {
        rows.push(splitTableCells(lines[i]));
        i++;
      }
      blocks.push({ kind: "table", headers, rows });
      continue;
    }

    // Blockquote
    if (trimmed.startsWith(">")) {
      const qLines: string[] = [];
      while (i < lines.length && lines[i].trim().startsWith(">")) {
        qLines.push(lines[i].trim().replace(/^>\s?/, ""));
        i++;
      }
      blocks.push({ kind: "quote", lines: qLines });
      continue;
    }

    // Unordered list
    if (/^\s*[-*+]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*[-*+]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*[-*+]\s+/, ""));
        i++;
      }
      blocks.push({ kind: "ul", items });
      continue;
    }

    // Ordered list
    if (/^\s*\d+[.)]\s+/.test(line)) {
      const items: string[] = [];
      while (i < lines.length && /^\s*\d+[.)]\s+/.test(lines[i])) {
        items.push(lines[i].replace(/^\s*\d+[.)]\s+/, ""));
        i++;
      }
      blocks.push({ kind: "ol", items });
      continue;
    }

    // Paragraph (stop at next blank line or structural block)
    const para: string[] = [line];
    i++;
    while (i < lines.length) {
      const next = lines[i];
      const nt = next.trim();
      if (
        !nt ||
        /^(`{3,}|~{3,})/.test(nt) ||
        /^#{1,6}\s+/.test(nt) ||
        /^([-*_])(\s*\1){2,}$/.test(nt) ||
        nt.startsWith(">") ||
        /^\s*[-*+]\s+/.test(next) ||
        /^\s*\d+[.)]\s+/.test(next) ||
        (isTableRow(next) && i + 1 < lines.length && isTableSep(lines[i + 1]))
      ) {
        break;
      }
      para.push(next);
      i++;
    }
    blocks.push({ kind: "p", text: para.join("\n") });
  }

  return blocks;
}

interface MediaCtx {
  agentId?: string;
  transport?: HubTransport | null;
  onZoomImage?: (url: string) => void;
}

function renderInline(text: string, ctx?: MediaCtx): ReactNode[] {
  const parts: ReactNode[] = [];
  const regex =
    /(`[^`\n]+`|!\[([^\]\n]*)\]\(([^)\s]+)\)|\[([^\]\n]+)\]\(([^)\s]+)\)|\*\*[^*\n]+\*\*|~~[^~\n]+~~|\*[^*\n]+\*|https?:\/\/[^\s)<>]+)/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null;
  let key = 0;

  while ((match = regex.exec(text)) !== null) {
    if (match.index > lastIndex) {
      parts.push(text.slice(lastIndex, match.index));
    }
    const token = match[0];
    if (token.startsWith("`") && token.endsWith("`")) {
      parts.push(
        <code key={key++} className="md-inline-code">
          {token.slice(1, -1)}
        </code>,
      );
    } else if (match[2] !== undefined && match[3] !== undefined) {
      // Embedded media / image: `![alt](target)`
      const alt = match[2];
      const target = match[3];
      if (/^https?:\/\//i.test(target)) {
        parts.push(
          <a key={key++} href={target} target="_blank" rel="noreferrer">
            {alt || target}
          </a>,
        );
      } else if (/^data:image\/[a-z0-9.+-]+;base64,/i.test(target)) {
        parts.push(
          <img
            key={key++}
            src={target}
            alt={alt}
            className="m-media-img"
            onClick={() => ctx?.onZoomImage?.(target)}
          />,
        );
      } else if (ctx?.agentId) {
        const cleanPath = target.replace(/^file:\/\//i, "").replace(/^\/([a-zA-Z]:\/)/, "$1");
        const kind = mediaKindOfPath(cleanPath) ?? "image";
        parts.push(
          <MobileMediaCard
            key={key++}
            source={{ kind: "agentMedia", agentId: ctx.agentId, path: cleanPath }}
            mediaKind={kind}
            label={alt || cleanPath}
            transport={ctx.transport ?? null}
            onZoomImage={ctx.onZoomImage}
          />,
        );
      } else {
        parts.push(
          <code key={key++} className="md-inline-code md-file-ref">
            {alt || target}
          </code>,
        );
      }
    } else if (match[4] !== undefined && match[5] !== undefined) {
      const label = match[4];
      const href = match[5];
      if (/^https?:\/\//i.test(href)) {
        parts.push(
          <a key={key++} href={href} target="_blank" rel="noreferrer">
            {label}
          </a>,
        );
      } else {
        const cleanPath = href.replace(/^file:\/\//i, "").replace(/^\/([a-zA-Z]:\/)/, "$1");
        const kind = mediaKindOfPath(cleanPath);
        if (ctx?.agentId && (kind === "audio" || kind === "video" || kind === "pdf")) {
          parts.push(
            <MobileMediaCard
              key={key++}
              source={{ kind: "agentMedia", agentId: ctx.agentId, path: cleanPath }}
              mediaKind={kind}
              label={label}
              transport={ctx.transport ?? null}
              onZoomImage={ctx.onZoomImage}
            />,
          );
        } else {
          // File/line reference from an agent (e.g. [a.rs](file:///...)) — render cleanly as code chip.
          parts.push(
            <code key={key++} className="md-inline-code md-file-ref">
              {label}
            </code>,
          );
        }
      }
    } else if (token.startsWith("**") && token.endsWith("**")) {
      parts.push(<strong key={key++}>{renderInline(token.slice(2, -2), ctx)}</strong>);
    } else if (token.startsWith("~~") && token.endsWith("~~")) {
      parts.push(<del key={key++}>{token.slice(2, -2)}</del>);
    } else if (token.startsWith("*") && token.endsWith("*")) {
      parts.push(<em key={key++}>{token.slice(1, -1)}</em>);
    } else {
      parts.push(
        <a key={key++} href={token} target="_blank" rel="noreferrer">
          {token}
        </a>,
      );
    }
    lastIndex = match.index + token.length;
  }
  if (lastIndex < text.length) parts.push(text.slice(lastIndex));
  return parts;
}

function CodeBlock({ lang, code }: { lang: string; code: string }) {
  const [copied, setCopied] = useState(false);

  const copy = () => {
    navigator.clipboard?.writeText(code).catch(() => {});
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="code-block">
      <div className="code-block-head">
        <span className="mono">{lang || "code"}</span>
        <button type="button" className={`code-copy-btn ${copied ? "copied" : ""}`} onClick={copy}>
          {copied ? <Check size={12} /> : <Copy size={12} />}
          <span>{copied ? "已复制" : "复制"}</span>
        </button>
      </div>
      <pre className="mono">{code}</pre>
    </div>
  );
}

export function MarkdownView({
  text,
  className = "",
  agentId,
  transport,
  onZoomImage,
}: {
  text: string;
  className?: string;
  agentId?: string;
  transport?: HubTransport | null;
  onZoomImage?: (url: string) => void;
}) {
  const blocks = parseBlocks(text);
  const ctx: MediaCtx = { agentId, transport, onZoomImage };
  return (
    <div className={`prose ${className}`}>
      {blocks.map((block, idx) => {
        switch (block.kind) {
          case "code":
            return <CodeBlock key={idx} lang={block.lang} code={block.code} />;
          case "heading":
            return block.level <= 2 ? (
              <h3 key={idx}>{renderInline(block.text, ctx)}</h3>
            ) : (
              <h4 key={idx}>{renderInline(block.text, ctx)}</h4>
            );
          case "hr":
            return <hr key={idx} />;
          case "quote":
            return (
              <blockquote key={idx}>
                {block.lines.map((l, j) => (
                  <p key={j}>{renderInline(l, ctx)}</p>
                ))}
              </blockquote>
            );
          case "ul":
            return (
              <ul key={idx}>
                {block.items.map((item, j) => (
                  <li key={j}>{renderInline(item, ctx)}</li>
                ))}
              </ul>
            );
          case "ol":
            return (
              <ol key={idx}>
                {block.items.map((item, j) => (
                  <li key={j}>{renderInline(item, ctx)}</li>
                ))}
              </ol>
            );
          case "table":
            return (
              <div key={idx} className="md-table-wrap">
                <table>
                  <thead>
                    <tr>
                      {block.headers.map((h, j) => (
                        <th key={j}>{renderInline(h, ctx)}</th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {block.rows.map((r, ri) => (
                      <tr key={ri}>
                        {r.map((cell, ci) => (
                          <td key={ci}>{renderInline(cell, ctx)}</td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
          case "p":
            return <p key={idx}>{renderInline(block.text, ctx)}</p>;
        }
      })}
    </div>
  );
}

/** Desktop-faithful unified diff viewer for tool call file changes. */
export function DiffView({ changes }: { changes: FileChange[] }) {
  return (
    <div className="diff">
      {changes.map((c, idx) => {
        const lines = (c.diff || "").split(/\r?\n/).slice(0, 120);
        return (
          <div key={idx} className="diff-file">
            <div className="diff-head mono">
              <span className="diff-path">{c.path}</span>
              <span className="chip tiny">{c.kind}</span>
            </div>
            {lines.length > 0 && lines[0] !== "" && (
              <div className="diff-body mono">
                {lines.map((line, li) => {
                  const cls = line.startsWith("+++ ") || line.startsWith("--- ")
                    ? "meta"
                    : line.startsWith("@@")
                      ? "hunk"
                      : line.startsWith("+")
                        ? "add"
                        : line.startsWith("-")
                          ? "del"
                          : "";
                  const sign = cls === "add" ? "+" : cls === "del" ? "−" : " ";
                  const body = cls === "add" || cls === "del" ? line.slice(1) : line;
                  return (
                    <div key={li} className={`dl ${cls}`}>
                      <span className="sign">{cls === "hunk" || cls === "meta" ? "" : sign}</span>
                      <span className="dl-text">{body}</span>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
