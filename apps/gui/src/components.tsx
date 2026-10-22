import { useState, type ReactNode } from "react";
import {
  Activity,
  ArrowUp,
  ChartNoAxesColumn,
  Check,
  ChevronDown,
  ChevronRight,
  Columns2,
  Copy,
  Crown,
  ExternalLink,
  FileDiff,
  Folder,
  GitBranch,
  ListChecks,
  MessageCircle,
  PanelRight,
  Pin,
  Plus,
  Rocket,
  Search,
  Settings,
  SlidersHorizontal,
  Square,
  UserPlus,
  Users,
  X,
  type LucideIcon,
} from "lucide-react";
import type { Agent } from "@bindings/Agent";
import type { AgentStatus } from "@bindings/AgentStatus";
import type { RunStatus } from "@bindings/RunStatus";
import type { TaskStatus } from "@bindings/TaskStatus";
import { api } from "./api";
import { useHubData } from "./hub";
import { parseBlocks, parseInline, type Block, type Inline } from "./lib/markdown";
import { full, when } from "./lib/time";

/** How a status reads at a glance: a colour, and whether it is still moving. */
export type Tone = { color: "accent" | "ok" | "warn" | "danger" | "off" | ""; live?: boolean };

const AGENT_TONE: Record<AgentStatus, Tone> = {
  running: { color: "accent", live: true },
  starting: { color: "warn", live: true },
  idle: { color: "ok" },
  stopped: { color: "off" },
  error: { color: "danger" },
};

const RUN_TONE: Record<RunStatus, Tone> = {
  planning: { color: "accent", live: true },
  awaiting_approval: { color: "warn" },
  running: { color: "accent", live: true },
  paused: { color: "warn" },
  completed: { color: "ok" },
  failed: { color: "danger" },
  cancelled: { color: "off" },
};

const TASK_TONE: Record<TaskStatus, Tone> = {
  pending: { color: "off" },
  running: { color: "accent", live: true },
  waiting_answer: { color: "warn" },
  verifying: { color: "accent", live: true },
  retrying: { color: "accent", live: true },
  escalated: { color: "warn" },
  reviewing: { color: "accent", live: true },
  passed: { color: "ok" },
  merged: { color: "ok" },
  failed: { color: "danger" },
  cancelled: { color: "off" },
  stalled: { color: "warn" },
};

export const agentTone = (s: AgentStatus) => AGENT_TONE[s] ?? { color: "" };
export const runTone = (s: RunStatus) => RUN_TONE[s] ?? { color: "" };
export const taskTone = (s: TaskStatus) => TASK_TONE[s] ?? { color: "" };

const LABELS: Record<string, string> = {
  awaiting_approval: "待审批",
  waiting_answer: "等回答",
  paused: "已暂停",
  stalled: "停了 · 未上报",
  passed: "待检查",
};

export const statusLabel = (s: string) => LABELS[s] ?? s.replace(/_/g, " ");

export function Dot({ tone, title }: { tone: Tone; title?: string }) {
  return <span className={`dot ${tone.color} ${tone.live ? "pulse" : ""}`} title={title} />;
}

export function StatusChip({ status, tone }: { status: string; tone: Tone }) {
  const cls = tone.color === "off" || !tone.color ? "" : tone.color;
  return (
    <span className={`chip ${cls}`}>
      <Dot tone={tone} />
      {statusLabel(status)}
    </span>
  );
}

export function ErrorBox({ error }: { error?: string }) {
  return error ? <div className="error">{error}</div> : null;
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="field">
      <span>{label}</span>
      {children}
    </label>
  );
}

export function List({ items, empty = "—" }: { items: string[]; empty?: string }) {
  if (!items.length) return <span className="muted">{empty}</span>;
  return (
    <ul className="plain">
      {items.map((item, i) => (
        <li key={i}>
          <code>{item}</code>
        </li>
      ))}
    </ul>
  );
}

/** Full time, for tooltips. */
export const time = full;

/** "刚刚", "12 分钟前", then the clock (lib/time.ts). */
export const ago = when;

/** All agents, kept fresh by agent events; for showing names instead of ids. */
export function useAgents() {
  return useHubData(
    () => api.listAgents().then((r) => r.agents),
    [],
    (e) => e.type === "agent.created" || e.type === "agent.removed" || e.type === "agent.status_changed",
  );
}

export function agentName(agents: Agent[] | undefined, id: string | null): string {
  if (!id) return "—";
  return agents?.find((a) => a.id === id)?.name ?? id.slice(0, 8);
}

export const sessionHref = (name: string) => `#/s/${encodeURIComponent(name)}`;

/**
 * Agent prose: fenced code blocks become `<pre>`, inline backticks `<code>`, blank lines
 * split paragraphs. Deliberately small; no HTML is ever interpreted.
 */
export function Prose({ text }: { text: string }) {
  return <div className="prose">{renderBlocks(parseBlocks(text))}</div>;
}

function renderBlocks(blocks: Block[]): ReactNode[] {
  return blocks.map((b, i) => {
    switch (b.t) {
      case "p":
        return <p key={i}>{renderInline(parseInline(b.text))}</p>;
      case "h": {
        const H = (["h3", "h3", "h4", "h5", "h6", "h6"] as const)[b.level - 1];
        return <H key={i}>{renderInline(parseInline(b.text))}</H>;
      }
      case "code":
        return <pre key={i}>{b.text}</pre>;
      case "list": {
        const items = b.items.map((item, j) => (
          <li key={j}>
            {renderInline(parseInline(item.text))}
            {item.children.length > 0 && renderBlocks(item.children)}
          </li>
        ));
        return b.ordered ? (
          <ol key={i} start={b.start}>
            {items}
          </ol>
        ) : (
          <ul key={i}>{items}</ul>
        );
      }
      case "quote":
        return <blockquote key={i}>{renderBlocks(b.blocks)}</blockquote>;
      case "hr":
        return <hr key={i} />;
      case "table":
        return (
          <div key={i} className="prose-table">
            <table>
              <thead>
                <tr>
                  {b.head.map((c, j) => (
                    <th key={j} style={{ textAlign: b.align[j] }}>
                      {renderInline(parseInline(c))}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {b.rows.map((row, r) => (
                  <tr key={r}>
                    {row.map((c, j) => (
                      <td key={j} style={{ textAlign: b.align[j] }}>
                        {renderInline(parseInline(c))}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
    }
  });
}

function renderInline(spans: Inline[]): ReactNode[] {
  return spans.map((s, i) => {
    switch (s.t) {
      case "text":
        return s.text;
      case "code":
        return <code key={i}>{s.text}</code>;
      case "strong":
        return <strong key={i}>{renderInline(s.children)}</strong>;
      case "em":
        return <em key={i}>{renderInline(s.children)}</em>;
      case "del":
        return <del key={i}>{renderInline(s.children)}</del>;
      case "link":
        return (
          <a key={i} href={s.href} target="_blank" rel="noreferrer">
            {renderInline(s.children)}
          </a>
        );
      case "br":
        return <br key={i} />;
    }
  });
}

/** `git diff --stat` with the +/- bars coloured. */
export function DiffStat({ text }: { text: string }) {
  return (
    <div className="diffstat">
      {text.split("\n").map((line, i) => {
        const m = line.match(/^(.*\|\s*\d+\s)(\+*)(-*)\s*$/);
        return (
          <div key={i}>
            {m ? (
              <>
                {m[1]}
                <span className="a">{m[2]}</span>
                <span className="d">{m[3]}</span>
              </>
            ) : (
              line || " "
            )}
          </div>
        );
      })}
    </div>
  );
}

const ICONS = {
  plus: Plus,
  send: ArrowUp,
  stop: Square,
  chart: ChartNoAxesColumn,
  team: Users,
  close: X,
  branch: GitBranch,
  folder: Folder,
  search: Search,
  chevron: ChevronDown,
  chevronRight: ChevronRight,
  pin: Pin,
  beside: Columns2,
  panel: PanelRight,
  diff: FileDiff,
  tasks: ListChecks,
  events: Activity,
  copy: Copy,
  settings: Settings,
  sliders: SlidersHorizontal,
  check: Check,
  chat: MessageCircle,
  addMate: UserPlus,
  planner: Crown,
  rocket: Rocket,
  open: ExternalLink,
} satisfies Record<string, LucideIcon>;

export type IconName = keyof typeof ICONS;

/** Line icons (lucide), one size and stroke everywhere; bundled, as the CSP allows only local resources. */
export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  const Glyph = ICONS[name];
  return <Glyph size={size} strokeWidth={1.75} absoluteStrokeWidth aria-hidden="true" className="icon" />;
}

/**
 * The sidebar's node language: shape says what a thing is (◆ planner, ● worker, ■ reviewer,
 * ◯ run), colour says its state, and a live state breathes.
 */
export type MarkShape = "planner" | "worker" | "reviewer" | "run";

export function Mark({ shape, tone, size = 10, title }: { shape: MarkShape; tone: Tone; size?: number; title?: string }) {
  const hollow = tone.color === "off" || !tone.color;
  const fill = hollow ? "none" : "currentColor";
  const stroke = { stroke: "currentColor", strokeWidth: hollow ? 1.6 : 0 };
  let body;
  switch (shape) {
    case "planner":
      body = <path d="M5 0.9 9.1 5 5 9.1 0.9 5Z" fill={fill} {...stroke} strokeLinejoin="round" />;
      break;
    case "reviewer":
      body = <rect x="1.3" y="1.3" width="7.4" height="7.4" rx="1.6" fill={fill} {...stroke} />;
      break;
    case "run":
      body = (
        <>
          <circle cx="5" cy="5" r="3.8" fill="none" stroke="currentColor" strokeWidth={1.8} />
          {!hollow && <circle cx="5" cy="5" r="1.4" fill="currentColor" />}
        </>
      );
      break;
    default:
      body = <circle cx="5" cy="5" r={hollow ? 3.4 : 3.8} fill={fill} {...stroke} />;
  }
  return (
    <svg className={`mark ${tone.color} ${tone.live ? "live" : ""}`} width={size} height={size} viewBox="0 0 10 10" aria-label={title} role="img">
      {title && <title>{title}</title>}
      {body}
    </svg>
  );
}

export const roleShape = (role: string): MarkShape => (role === "planner" || role === "reviewer" ? role : "worker");

/** The logo in the same language: a planner fanning out to two workers. */
export function BrandMark({ size = 22 }: { size?: number }) {
  return (
    <svg className="brand-mark" width={size} height={size} viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 9.5 6.5 17.5M12 9.5l5.5 8" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" opacity="0.45" />
      <path d="M12 2.5 16 6.5 12 10.5 8 6.5Z" fill="currentColor" />
      <circle cx="6" cy="18.5" r="3" fill="currentColor" />
      <circle cx="18" cy="18.5" r="3" fill="currentColor" opacity="0.7" />
    </svg>
  );
}

/** Avatar colours, matching `AgentProfile::COLORS` in the hub. */
export const AVATAR_COLORS = ["indigo", "violet", "rose", "amber", "emerald", "teal", "sky", "slate"] as const;

/** Each role in its own colour, wherever a role is shown. */
export const ROLE_HUE: Record<string, (typeof AVATAR_COLORS)[number]> = { planner: "violet", worker: "sky", reviewer: "amber" };

/** A stable colour from the palette for any text (a workspace path, an agent name). */
export function hueOf(text: string): (typeof AVATAR_COLORS)[number] {
  let h = 0;
  for (const c of text) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  // Slate is for "no colour"; generated colours skip it.
  return AVATAR_COLORS[h % (AVATAR_COLORS.length - 1)];
}

/** A stable colour for agents that did not pick one. */
export function colorOf(agent: Agent): string {
  const picked = agent.profile?.color;
  if (picked && (AVATAR_COLORS as readonly string[]).includes(picked)) return picked;
  return hueOf(agent.name);
}

type Vendor = "claude" | "codex" | "other";

/** Which vendor runs a plugin (or account provider). */
export function vendorOf(plugin: string): Vendor {
  const p = plugin.toLowerCase();
  if (p.startsWith("claude")) return "claude";
  if (p.startsWith("codex")) return "codex";
  return "other";
}

/** "Claude", "Codex", or the plugin's own name. */
export function vendorName(plugin: string): string {
  const v = vendorOf(plugin);
  return v === "claude" ? "Claude" : v === "codex" ? "Codex" : plugin;
}

/** The vendor's mark: a clay asterisk for Claude, an ink prompt for Codex, a ring otherwise. */
export function VendorMark({ plugin, size = 12 }: { plugin: string; size?: number }) {
  const vendor = vendorOf(plugin);
  return (
    <svg className={`vendor-mark ${vendor}`} width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
      {vendor === "claude" && (
        <g stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          {[0, 30, 60, 90, 120, 150].map((a) => (
            <line key={a} x1="8" y1="1.6" x2="8" y2="14.4" transform={`rotate(${a} 8 8)`} />
          ))}
        </g>
      )}
      {vendor === "codex" && (
        <g fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
          <rect x="1.5" y="1.5" width="13" height="13" rx="3.5" />
          <path d="M5 6l2.2 2L5 10M8.8 10.5h2.4" />
        </g>
      )}
      {vendor === "other" && <circle cx="8" cy="8" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.5" />}
    </svg>
  );
}

/** Who runs an agent: the vendor's mark, then the model (or the vendor's name). */
export function Vendor({ plugin, model, title }: { plugin: string; model?: string | null; title?: string }) {
  return (
    <span className="vendor" title={title ?? `${vendorName(plugin)}${model ? " · " + model : ""}`}>
      <VendorMark plugin={plugin} />
      <span className="model">{model || vendorName(plugin)}</span>
    </span>
  );
}

/** What people call the agent: its display name, else its address. */
export const displayName = (agent: Agent) => agent.profile?.display_name || agent.name;

/** Title if it has one, else the role. */
export const agentTitle = (agent: Agent) => agent.profile?.title || agent.role;

/**
 * The agent's face: its emoji or initials on its colour. With `status`, a small dot at the
 * corner says whether it is working (like a presence dot in a chat app).
 */
export function Avatar({ agent, size = 20, status = false }: { agent: Agent; size?: number; status?: boolean }) {
  const avatar = agent.profile?.avatar;
  const text = avatar || Array.from(displayName(agent))[0]?.toUpperCase() || "?";
  const emoji = !!avatar && /\p{Extended_Pictographic}/u.test(avatar);
  const tone = agentTone(agent.status);
  return (
    <span
      className={`avatar av-${colorOf(agent)} ${emoji ? "emoji" : ""}`}
      style={{ width: size, height: size, fontSize: Math.round(size * (emoji ? 0.6 : text.length > 1 ? 0.4 : 0.5)) }}
      aria-hidden="true"
    >
      {text}
      {status && tone.color !== "off" && <span className={`presence ${tone.color} ${tone.live ? "live" : ""}`} />}
    </span>
  );
}

/** A shell command with a copy button. */
export function CopyCommand({ command }: { command: string }) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // Clipboard may be unavailable; the command is visible to copy by hand.
    }
  };
  return (
    <button className="copy" onClick={copy} title="复制命令">
      <span className="mono">{command}</span>
      <span className="copy-state">{copied ? "已复制" : "复制"}</span>
    </button>
  );
}
