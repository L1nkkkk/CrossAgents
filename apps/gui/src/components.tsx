import { createContext, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { useAvatarImage } from "./avatarImages";
import { modelFamily } from "./lib/modelFamily";
import { MODEL_ICONS, VENDOR_ICONS, type BrandIcon } from "./modelIcons";
import {
  Activity,
  Archive,
  ArchiveRestore,
  Brain,
  CircleUserRound,
  ArrowLeft,
  ArrowRight,
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
  Info,
  ListChecks,
  Menu,
  MessageCircle,
  Minimize2,
  PanelLeft,
  Paperclip,
  PanelRight,
  Pin,
  Plus,
  Rocket,
  Search,
  Settings,
  SlidersHorizontal,
  Square,
  Terminal,
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
import { codeFileTarget } from "./lib/filelinks";
import type { FileScope } from "./api";
import { openFile } from "./FileViewer";
import { useResolved } from "./fileResolve";
import { prettyModel } from "./lib/models";
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
    (e) => e.type === "agent.created" || e.type === "agent.updated" || e.type === "agent.removed" || e.type === "agent.status_changed",
  );
}

export function agentName(agents: Agent[] | undefined, id: string | null): string {
  if (!id) return "—";
  return agents?.find((a) => a.id === id)?.name ?? id.slice(0, 8);
}

/** A conversation's link, by id so a rename does not break it (a name still opens it). */
export const sessionHref = (agent: Agent | string) => `#/s/${encodeURIComponent(typeof agent === "string" ? agent : agent.id)}`;

/**
 * Agent prose: fenced code blocks become `<pre>`, inline backticks `<code>`, blank lines
 * split paragraphs. Deliberately small; no HTML is ever interpreted.
 */
export function Prose({ text }: { text: string }) {
  return <div className="prose">{renderBlocks(parseBlocks(text))}</div>;
}

/**
 * Long text kept to one line until asked for: a refused call's reason with a whole check log in
 * it, a run's summary, a failed step. The line reads as status icon · what it was · a one-line
 * gist; the icon — or the line — opens the whole thing below, in a bounded box that scrolls
 * rather than pushing the conversation away.
 */
export type OneLineTone = "ok" | "danger" | "warn" | "running";

export function OneLine({
  tone,
  icon,
  title,
  summary,
  className,
  children,
}: {
  tone: OneLineTone;
  icon?: ReactNode;
  title: string;
  summary?: string;
  className?: string;
  /** The full content. Without it the line cannot be opened and the icon is not a control. */
  children?: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  // Opened near the bottom of the conversation, the body would start off-screen: bring it in.
  const root = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (open) root.current?.scrollIntoView({ block: "nearest" });
  }, [open]);
  const more = children != null && children !== false;
  const toggle = () => more && setOpen(!open);
  const label = open ? "收起详情" : "展开详情";
  const words = (
    <>
      <span className="oneline-title">{title}</span>
      {summary && <span className="oneline-summary">{summary}</span>}
    </>
  );
  return (
    <div ref={root} className={`oneline ${tone} ${open ? "open" : ""} ${className ?? ""}`}>
      <div className="oneline-head">
        {more ? (
          <button className="oneline-icon" onClick={toggle} aria-expanded={open} aria-label={label} title={label}>
            {icon}
          </button>
        ) : (
          <span className="oneline-icon" aria-hidden="true">
            {icon}
          </span>
        )}
        {more ? (
          <button className="oneline-main" onClick={toggle} aria-expanded={open} title={summary}>
            {words}
          </button>
        ) : (
          <span className="oneline-main" title={summary}>
            {words}
          </span>
        )}
      </div>
      {open && <div className="oneline-body">{children}</div>}
    </div>
  );
}

/** The first non-empty line of `text`, trimmed. */
export function firstLine(text?: string | null): string {
  for (const line of (text ?? "").split(/\r?\n/)) {
    const s = line.trim();
    if (s) return s;
  }
  return "";
}

/** That first line cut at its first sentence, so a gist fits one line. */
export function firstSentence(text?: string | null): string {
  const line = firstLine(text);
  const match = line.match(/^.*?[。！？]|^.*?\.(?=\s|$)/);
  return (match ? match[0] : line).trim();
}

/** `text` when it says more than its first line (so there is something to open), else nothing. */
export function beyondFirstLine(text?: string | null): string | undefined {
  const full = (text ?? "").trim();
  return full && full !== firstLine(full) ? full : undefined;
}

/**
 * Whose files the paths in the prose below are relative to: a session's working directory, a
 * run's integration branch, a task's worktree. The innermost provider wins.
 */
const FileScopeContext = createContext<FileScope | null>(null);
export const FileScopeProvider = FileScopeContext.Provider;
export const useFileScope = () => useContext(FileScopeContext);

/** A markdown link to a file: a button opening the viewer (plain text without a scope). */
function FileLink({ path, line, children }: { path: string; line?: number; children: ReactNode }) {
  const scope = useFileScope();
  if (!scope) return <>{children}</>;
  return (
    <button className="file-link" title={line ? `${path} 第 ${line} 行` : path} onClick={() => openFile({ scope, path, line })}>
      {children}
    </button>
  );
}

/** Inline code: a link once the hub confirms it names a file of the scope's workspace. */
function InlineCode({ text }: { text: string }) {
  const scope = useFileScope();
  const target = scope ? codeFileTarget(text) : undefined;
  const found = useResolved(target ? scope : null, target?.path);
  if (!scope || !target || !found) return <code>{text}</code>;
  return (
    <button
      className="file-link code"
      title={target.line ? `打开 ${found} 第 ${target.line} 行` : `打开 ${found}`}
      onClick={() => openFile({ scope, path: found, line: target.line })}
    >
      {text}
    </button>
  );
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
        return <InlineCode key={i} text={s.text} />;
      case "file":
        return (
          <FileLink key={i} path={s.path} line={s.line}>
            {renderInline(s.children)}
          </FileLink>
        );
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
  back: ArrowLeft,
  forward: ArrowRight,
  sidebar: PanelLeft,
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
  menu: Menu,
  events: Activity,
  copy: Copy,
  settings: Settings,
  info: Info,
  sliders: SlidersHorizontal,
  check: Check,
  chat: MessageCircle,
  brain: Brain,
  archive: Archive,
  shrink: Minimize2,
  terminal: Terminal,
  restore: ArchiveRestore,
  account: CircleUserRound,
  addMate: UserPlus,
  planner: Crown,
  rocket: Rocket,
  open: ExternalLink,
  attach: Paperclip,
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

type Vendor = "claude" | "codex" | "codebuddy" | "other";

/** Which vendor runs a plugin (or account provider). */
export function vendorOf(plugin: string): Vendor {
  const p = plugin.toLowerCase();
  if (p.startsWith("claude")) return "claude";
  if (p.startsWith("codex")) return "codex";
  if (p.startsWith("codebuddy")) return "codebuddy";
  return "other";
}

/** "Claude", "Codex", "CodeBuddy", or the plugin's own name. */
export function vendorName(plugin: string): string {
  const v = vendorOf(plugin);
  return v === "claude" ? "Claude" : v === "codex" ? "Codex" : v === "codebuddy" ? "CodeBuddy" : plugin;
}

/** An official mark: as an image, or as a mask over the text colour when it is one-colour. */
function BrandImage({ icon, size, className = "" }: { icon: BrandIcon; size: number; className?: string }) {
  if (icon.mono) {
    const mask = `url("${icon.src}")`;
    return <span className={`brand-mono ${className}`} style={{ width: size, height: size, maskImage: mask, WebkitMaskImage: mask }} aria-hidden="true" />;
  }
  return <img className={className} src={icon.src} width={size} height={size} alt="" aria-hidden="true" draggable={false} />;
}

/** The vendor's official mark, or a ring for a plugin we have none for. */
export function VendorMark({ plugin, size = 12 }: { plugin: string; size?: number }) {
  const vendor = vendorOf(plugin);
  const icon = VENDOR_ICONS[vendor];
  if (icon) return <BrandImage icon={icon} size={size} className={`vendor-mark ${vendor}`} />;
  return (
    <svg className="vendor-mark other" width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

/** The mark of the model behind a session; the vendor's when the model's family is not known. */
export function ModelMark({ plugin, model, size = 13 }: { plugin: string; model?: string | null; size?: number }) {
  const family = modelFamily(plugin, model);
  const icon = family && MODEL_ICONS[family.key];
  return (
    <span className="model-mark" title={[family?.name ?? vendorName(plugin), model].filter(Boolean).join(" · ")}>
      {icon ? <BrandImage icon={icon} size={size} /> : <VendorMark plugin={plugin} size={size} />}
    </span>
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

/** What people and teammates call the agent ("Luna"); renamable, separate from its id and model. */
export const displayName = (agent: Agent) => agent.name;

/** Whether `key` (from a link or a pane) names this agent: its id, or its name ignoring case. */
export const isAgent = (agent: Agent, key: string | null | undefined) =>
  !!key && (agent.id === key || agent.name.toLowerCase() === key.toLowerCase());

/** Title if it has one, else the role. */
/** What sits under an agent's name: its title, else the model it runs on (a chat has no role to speak of). */
export const agentTitle = (agent: Agent) => agent.profile?.title || (agent.model ? prettyModel(agent.model) : agent.role);

/**
 * The agent's face: its emoji or initials on its colour. With `status`, a small dot at the
 * corner says whether it is working (like a presence dot in a chat app).
 */
export function Avatar({ agent, size = 20, status = false }: { agent: Agent; size?: number; status?: boolean }) {
  const avatar = agent.profile?.avatar;
  const text = avatar || Array.from(displayName(agent))[0]?.toUpperCase() || "?";
  const emoji = !!avatar && /\p{Extended_Pictographic}/u.test(avatar);
  const tone = agentTone(agent.status);
  const image = useAvatarImage(agent.profile?.avatar_image);
  return (
    <span
      className={`avatar av-${colorOf(agent)} ${emoji ? "emoji" : ""}`}
      style={{ width: size, height: size, fontSize: Math.round(size * (emoji ? 0.6 : text.length > 1 ? 0.4 : 0.5)) }}
      aria-hidden="true"
    >
      {image ? <img className="avatar-img" src={image} alt="" draggable={false} /> : text}
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
