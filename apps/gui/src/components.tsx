import { useState, type ReactNode } from "react";
import type { Agent } from "@bindings/Agent";
import type { AgentStatus } from "@bindings/AgentStatus";
import type { RunStatus } from "@bindings/RunStatus";
import type { TaskStatus } from "@bindings/TaskStatus";
import { api } from "./api";
import { useHubData } from "./hub";

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
};

export const agentTone = (s: AgentStatus) => AGENT_TONE[s] ?? { color: "" };
export const runTone = (s: RunStatus) => RUN_TONE[s] ?? { color: "" };
export const taskTone = (s: TaskStatus) => TASK_TONE[s] ?? { color: "" };

const LABELS: Record<string, string> = {
  awaiting_approval: "待审批",
  waiting_answer: "等回答",
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

export function time(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString();
}

/** "刚刚", "5 分钟前", … then a date. */
export function ago(iso: string): string {
  const d = new Date(iso).getTime();
  if (Number.isNaN(d)) return iso;
  const s = Math.max(0, (Date.now() - d) / 1000);
  if (s < 60) return "刚刚";
  if (s < 3600) return `${Math.floor(s / 60)} 分钟前`;
  if (s < 86400) return `${Math.floor(s / 3600)} 小时前`;
  return new Date(d).toLocaleDateString();
}

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
  const blocks: ReactNode[] = [];
  const parts = text.split(/^```[^\n]*\n?/m);
  parts.forEach((part, i) => {
    if (i % 2 === 1) {
      blocks.push(<pre key={i}>{part.replace(/\n$/, "")}</pre>);
      return;
    }
    part
      .split(/\n{2,}/)
      .filter((p) => p.trim())
      .forEach((p, j) => blocks.push(<p key={`${i}-${j}`}>{inline(p.replace(/^\n+|\n+$/g, ""))}</p>));
  });
  return <div className="prose">{blocks}</div>;
}

function inline(text: string): ReactNode[] {
  return text.split(/(`[^`\n]+`)/).map((s, i) => (s.startsWith("`") && s.endsWith("`") && s.length > 2 ? <code key={i}>{s.slice(1, -1)}</code> : s));
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

/** Small inline icons (no icon font: the CSP allows only local resources). */
export function Icon({ name, size = 16 }: { name: "plus" | "send" | "stop" | "chart" | "cpu" | "close" | "branch" | "folder"; size?: number }) {
  const paths: Record<string, ReactNode> = {
    plus: <path d="M12 5v14M5 12h14" />,
    send: <path d="M12 19V5M5 12l7-7 7 7" />,
    stop: <rect x="7" y="7" width="10" height="10" rx="1.5" />,
    chart: <path d="M4 20h16M7 16v-5M12 16V8M17 16v-8" />,
    cpu: (
      <>
        <rect x="6" y="6" width="12" height="12" rx="2" />
        <path d="M9 2v3M15 2v3M9 19v3M15 19v3M2 9h3M2 15h3M19 9h3M19 15h3" />
      </>
    ),
    close: <path d="M6 6l12 12M18 6L6 18" />,
    branch: (
      <>
        <circle cx="6" cy="5" r="2" />
        <circle cx="6" cy="19" r="2" />
        <circle cx="18" cy="7" r="2" />
        <path d="M6 7v10M18 9c0 5-6 4-11 8" />
      </>
    ),
    folder: <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />,
  };
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {paths[name]}
    </svg>
  );
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

/** A stable colour for agents that did not pick one. */
function colorOf(agent: Agent): string {
  const picked = agent.profile?.color;
  if (picked && (AVATAR_COLORS as readonly string[]).includes(picked)) return picked;
  let h = 0;
  for (const c of agent.name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return AVATAR_COLORS[h % AVATAR_COLORS.length];
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

/** Starting points for a new teammate; everything stays editable. */
export const PERSONA_PRESETS: {
  key: string;
  label: string;
  role: "planner" | "worker" | "reviewer";
  title: string;
  avatar: string;
  color: string;
  persona: string;
}[] = [
  {
    key: "architect",
    label: "架构师",
    role: "planner",
    title: "架构师",
    avatar: "🧭",
    color: "indigo",
    persona:
      "You design before anyone codes. Read the codebase, find the smallest change that does the job, and split it into tasks with precise specs, clear file scopes and acceptance commands. Prefer boring, proven solutions.",
  },
  {
    key: "coder",
    label: "Coder",
    role: "worker",
    title: "Coder",
    avatar: "⌨️",
    color: "sky",
    persona:
      "You implement exactly what the spec asks, in the style of the surrounding code. Stay inside the allowed files, run the acceptance commands before reporting, and ask instead of guessing when the spec is unclear.",
  },
  {
    key: "designer",
    label: "设计师",
    role: "worker",
    title: "设计师",
    avatar: "🎨",
    color: "rose",
    persona:
      "You own the UI: layout, spacing, typography, colour and states (empty, loading, error). Keep to the existing design tokens, make every screen work in light and dark mode, and explain visual choices briefly.",
  },
  {
    key: "security",
    label: "安全审核专员",
    role: "reviewer",
    title: "安全审核专员",
    avatar: "🛡️",
    color: "amber",
    persona:
      "You review diffs for security: injection, auth and permission checks, secrets in code or logs, unsafe deserialization, path traversal, and risky dependencies. Point to the exact line, explain the attack, and suggest the fix. Do not nitpick style.",
  },
  {
    key: "tester",
    label: "测试工程师",
    role: "worker",
    title: "测试工程师",
    avatar: "🧪",
    color: "emerald",
    persona:
      "You write tests that pin down behaviour: the happy path, edge cases and the failure modes the spec mentions. Tests must be deterministic and fast; never weaken an assertion to make it pass.",
  },
  {
    key: "writer",
    label: "文档写手",
    role: "worker",
    title: "文档写手",
    avatar: "✍️",
    color: "violet",
    persona: "You write docs people can follow: short sentences, real commands, and examples that run. Update docs in the same change as the code they describe.",
  },
];

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
