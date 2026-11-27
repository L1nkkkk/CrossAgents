// Official CrossAgents visual marks, avatars, OS icons, and AI vendor/model badges — aligned 1:1
// with Desktop GUI (`apps/gui/src/components.tsx`, `modelIcons.ts`, `osIcon.tsx`, `lib/models.ts`).

import { UserRound } from "lucide-react";
import claude from "@lobehub/icons-static-svg/icons/claude-color.svg";
import antigravity from "@lobehub/icons-static-svg/icons/antigravity-color.svg";
import cursor from "@lobehub/icons-static-svg/icons/cursor.svg";
import codebuddy from "@lobehub/icons-static-svg/icons/codebuddy-color.svg";
import codex from "@lobehub/icons-static-svg/icons/codex-color.svg";
import deepseek from "@lobehub/icons-static-svg/icons/deepseek-color.svg";
import gemini from "@lobehub/icons-static-svg/icons/gemini-color.svg";
import hunyuan from "@lobehub/icons-static-svg/icons/hunyuan-color.svg";
import kimi from "@lobehub/icons-static-svg/icons/kimi.svg";
import minimax from "@lobehub/icons-static-svg/icons/minimax-color.svg";
import openai from "@lobehub/icons-static-svg/icons/openai.svg";
import qwen from "@lobehub/icons-static-svg/icons/qwen-color.svg";
import step from "@lobehub/icons-static-svg/icons/stepfun-color.svg";
import glm from "@lobehub/icons-static-svg/icons/zhipu-color.svg";
import mimo from "@lobehub/icons-static-svg/icons/xiaomimimo.svg";
import grok from "@lobehub/icons-static-svg/icons/grok.svg";
import doubao from "@lobehub/icons-static-svg/icons/doubao-color.svg";
import wenxin from "@lobehub/icons-static-svg/icons/wenxin-color.svg";
import mistral from "@lobehub/icons-static-svg/icons/mistral-color.svg";
import yi from "@lobehub/icons-static-svg/icons/yi-color.svg";
import baichuan from "@lobehub/icons-static-svg/icons/baichuan-color.svg";
import internlm from "@lobehub/icons-static-svg/icons/internlm-color.svg";
import spark from "@lobehub/icons-static-svg/icons/spark-color.svg";
import sensenova from "@lobehub/icons-static-svg/icons/sensenova-color.svg";
import skywork from "@lobehub/icons-static-svg/icons/skywork-color.svg";
import longcat from "@lobehub/icons-static-svg/icons/longcat-color.svg";
import type { Agent, AgentStatus, RateWindow } from "./types";

type BrandIcon = { src: string; mono?: boolean };

const MODEL_ICONS: Record<string, BrandIcon> = {
  claude: { src: claude },
  openai: { src: openai, mono: true },
  deepseek: { src: deepseek },
  glm: { src: glm },
  step: { src: step },
  gemini: { src: gemini },
  qwen: { src: qwen },
  kimi: { src: kimi, mono: true },
  minimax: { src: minimax },
  hunyuan: { src: hunyuan },
  mimo: { src: mimo, mono: true },
  grok: { src: grok, mono: true },
  doubao: { src: doubao },
  wenxin: { src: wenxin },
  mistral: { src: mistral },
  yi: { src: yi, mono: true },
  baichuan: { src: baichuan },
  internlm: { src: internlm },
  spark: { src: spark },
  sensenova: { src: sensenova },
  skywork: { src: skywork },
  longcat: { src: longcat, mono: true },
};

const VENDOR_ICONS: Record<string, BrandIcon> = {
  antigravity: { src: antigravity },
  cursor: { src: cursor, mono: true },
  gemini: { src: gemini },
  claude: { src: claude },
  codex: { src: codex },
  codebuddy: { src: codebuddy },
};

const GATEWAY_PREFIX = /^(tokenhub|ext|azure|bedrock|vertex|openrouter)[_-]/;

export function bareModel(model: string): string {
  let bare = model.trim().toLowerCase();
  while (GATEWAY_PREFIX.test(bare)) bare = bare.replace(GATEWAY_PREFIX, "");
  return bare;
}

const FAMILIES: readonly { key: string; name: string; match: RegExp }[] = [
  { key: "claude", name: "Claude", match: /claude|anthropic|opus|sonnet|haiku|fable/ },
  { key: "openai", name: "OpenAI", match: /gpt|codex|^chatgpt|^o\d/ },
  { key: "gemini", name: "Gemini", match: /gemini/ },
  { key: "deepseek", name: "DeepSeek", match: /deepseek/ },
  { key: "glm", name: "GLM", match: /glm|zhipu/ },
  { key: "kimi", name: "Kimi", match: /kimi|moonshot/ },
  { key: "qwen", name: "Qwen", match: /qwen|qianwen|qwq/ },
  { key: "hunyuan", name: "混元", match: /hunyuan|^hy\d/ },
  { key: "minimax", name: "MiniMax", match: /minimax|abab/ },
  { key: "step", name: "Step", match: /^step|stepfun/ },
  { key: "mimo", name: "MiMo", match: /mimo|xiaomi/ },
  { key: "grok", name: "Grok", match: /grok|^xai/ },
  { key: "doubao", name: "豆包", match: /doubao|bytedance|^seed/ },
  { key: "wenxin", name: "文心", match: /wenxin|ernie/ },
  { key: "mistral", name: "Mistral", match: /mistral|mixtral|codestral|magistral|devstral/ },
  { key: "yi", name: "Yi", match: /^yi([-_.]|$)|01-ai/ },
  { key: "baichuan", name: "Baichuan", match: /baichuan/ },
  { key: "internlm", name: "InternLM", match: /internlm|intern-lm/ },
  { key: "spark", name: "星火", match: /spark|iflytek/ },
  { key: "sensenova", name: "SenseNova", match: /sensenova|sense-?chat/ },
  { key: "skywork", name: "Skywork", match: /skywork/ },
  { key: "longcat", name: "LongCat", match: /longcat/ },
];

export function modelFamily(plugin: string, model: string | null | undefined): { key: string; name: string } | null {
  const id = model ? bareModel(model) : "";
  if (id) return FAMILIES.find((f) => f.match.test(id)) ?? null;
  const p = plugin.trim().toLowerCase();
  if (p.startsWith("claude")) return FAMILIES[0];
  if (p.startsWith("codex")) return FAMILIES[1];
  if (p.startsWith("gemini")) return FAMILIES[2];
  return null;
}

export function vendorOf(plugin: string): string {
  const p = plugin.toLowerCase();
  if (p.startsWith("claude")) return "claude";
  if (p.startsWith("codex")) return "codex";
  if (p.startsWith("codebuddy")) return "codebuddy";
  if (p.startsWith("antigravity")) return "antigravity";
  if (p.startsWith("cursor")) return "cursor";
  if (p.startsWith("gemini")) return "gemini";
  return "other";
}

export function vendorName(plugin: string): string {
  const v = vendorOf(plugin);
  if (v === "claude") return "Claude";
  if (v === "codex") return "Codex";
  if (v === "codebuddy") return "CodeBuddy";
  if (v === "antigravity") return "Antigravity";
  if (v === "cursor") return "Cursor";
  if (v === "gemini") return "Gemini";
  return plugin ? plugin[0].toUpperCase() + plugin.slice(1) : plugin;
}

/** "claude-opus-4-1" → "Opus 4.1", "gpt-5-codex" → "GPT-5 Codex". */
export function prettyModel(model: string): string {
  const words = model.split(/[-_]/).filter(Boolean);
  if (words[0] === "claude" || words[0] === "mock") words.shift();
  const out: string[] = [];
  for (const w of words) {
    const last = out[out.length - 1];
    if (/^\d+$/.test(w) && last && /\d$/.test(last)) out[out.length - 1] = `${last}.${w}`;
    else if (/^\d+(?:\.\d+)*$/.test(w) && last === "GPT") out[out.length - 1] = `GPT-${w}`;
    else if (w === "gpt") out.push("GPT");
    else out.push(/^\d/.test(w) ? w : w[0].toUpperCase() + w.slice(1));
  }
  return out.join(" ") || model;
}

export function groupModels(models: readonly string[]): { key: string; label: string; models: string[] }[] {
  const map = new Map<string, { key: string; label: string; models: string[] }>();
  for (const m of models) {
    const fam = modelFamily("", m);
    const key = fam?.key ?? "other";
    const label = fam?.name ?? "其他";
    const bucket = map.get(key) ?? { key, label, models: [] };
    bucket.models.push(m);
    map.set(key, bucket);
  }
  const order = [...FAMILIES.map((f) => f.key), "other"];
  return order.flatMap((k) => {
    const g = map.get(k);
    return g ? [g] : [];
  });
}

const EFFORT_LABELS: Record<string, string> = {
  none: "不推理",
  minimal: "最低",
  low: "低",
  medium: "中",
  high: "高",
  xhigh: "超高",
  max: "最高",
  ultra: "极致",
};

export function effortLabel(value: string): string {
  return EFFORT_LABELS[value] ?? value;
}

/** Desktop status tones & Chinese labels. */
export type StatusTone = { color: "accent" | "ok" | "warn" | "danger" | "off"; live?: boolean; label: string };

export function agentTone(status: AgentStatus): StatusTone {
  switch (status) {
    case "running":
      return { color: "accent", live: true, label: "进行中" };
    case "starting":
      return { color: "warn", live: true, label: "启动中" };
    case "idle":
      return { color: "ok", label: "空闲" };
    case "error":
      return { color: "danger", label: "出错" };
    default:
      return { color: "off", label: "已停止" };
  }
}

/** Exact desktop quota formatting (`apps/gui/src/lib/quota.ts`). */
export function windowLabel(w: Pick<RateWindow, "name" | "window_seconds">): string {
  const s = w.window_seconds;
  if (!s) return w.name === "primary" ? "主限额" : w.name === "secondary" ? "次限额" : w.name;
  if (s === 7 * 86400) return "每周限额";
  if (s % 86400 === 0) return `${s / 86400} 天限额`;
  if (s % 3600 === 0) return `${s / 3600} 小时限额`;
  return `${Math.round(s / 60)} 分钟限额`;
}

export function planLabel(plan: string | null): string | undefined {
  if (!plan) return undefined;
  return plan
    .split(/[_\s-]+/)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

export function quotaLevel(usedPercent: number, reached = false): "ok" | "warn" | "danger" {
  if (reached || usedPercent >= 90) return "danger";
  if (usedPercent >= 75) return "warn";
  return "ok";
}

export function resetText(resetsAt: string | null): string | undefined {
  if (!resetsAt) return undefined;
  const at = new Date(resetsAt);
  const now = Date.now();
  const ms = at.getTime() - now;
  if (Number.isNaN(ms)) return undefined;
  if (ms <= 0) return "即将重置";
  const mins = Math.round(ms / 60_000);
  if (mins < 60) return `${mins} 分钟后重置`;
  if (mins < 24 * 60) {
    const h = Math.floor(mins / 60);
    const m = mins % 60;
    return m ? `${h} 小时 ${m} 分后重置` : `${h} 小时后重置`;
  }
  const hm = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((startOfDay(at) - startOfDay(new Date(now))) / 86_400_000);
  if (days === 1) return `明天 ${hm} 重置`;
  const week = "日一二三四五六"[at.getDay()];
  return `${at.getMonth() + 1}月${at.getDate()}日 周${week} 重置`;
}

/** Exact desktop `<BrandMark />`: a diamond planner fanning out to two workers. */
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

function BrandImage({ icon, size, className = "" }: { icon: BrandIcon; size: number; className?: string }) {
  if (icon.mono) {
    const mask = `url("${icon.src}")`;
    return (
      <span
        className={`brand-mono ${className}`}
        style={{ width: size, height: size, maskImage: mask, WebkitMaskImage: mask }}
        aria-hidden="true"
      />
    );
  }
  return <img className={className} src={icon.src} width={size} height={size} alt="" aria-hidden="true" draggable={false} />;
}

export function VendorMark({ plugin, size = 14 }: { plugin: string; size?: number }) {
  const vendor = vendorOf(plugin);
  const icon = VENDOR_ICONS[vendor];
  if (icon) return <BrandImage icon={icon} size={size} className={`vendor-mark ${vendor}`} />;
  return (
    <svg className="vendor-mark other" width={size} height={size} viewBox="0 0 16 16" aria-hidden="true">
      <circle cx="8" cy="8" r="5.5" fill="none" stroke="currentColor" strokeWidth="1.5" />
    </svg>
  );
}

export function ModelMark({ plugin, model, size = 14 }: { plugin: string; model?: string | null; size?: number }) {
  const fam = modelFamily(plugin, model);
  const icon = fam && MODEL_ICONS[fam.key];
  return (
    <span className="model-mark" aria-hidden="true">
      {icon ? <BrandImage icon={icon} size={size} /> : <VendorMark plugin={plugin} size={size} />}
    </span>
  );
}

export function FamilyMark({ familyKey, size = 14 }: { familyKey: string; size?: number }) {
  const icon = MODEL_ICONS[familyKey];
  if (icon) return <BrandImage icon={icon} size={size} className="family-mark" />;
  return <VendorMark plugin="" size={size} />;
}

/** Exact desktop `<OsIcon />` geometry. */
export function OsIcon({ os, size = 14 }: { os?: string; size?: number }) {
  const kind = os === "windows" || os === "macos" || os === "linux" || os === "ios" || os === "android" ? os : "other";
  return (
    <svg className={`os-icon os-${kind}`} width={size} height={size} viewBox="0 0 16 16" fill="currentColor" aria-hidden="true" style={{ flexShrink: 0 }}>
      {kind === "windows" && (
        <>
          <path d="M1.5 2.9 6.8 2.2v5.1H1.5z" />
          <path d="M7.5 2.1 14.5 1.1v6.2H7.5z" />
          <path d="M1.5 8.1h5.3v5.1l-5.3-.7z" />
          <path d="M7.5 8.1h7v6.8l-7-1z" />
        </>
      )}
      {kind === "macos" && (
        <>
          <path d="M8 4.6c-1-.8-3.9-.9-4.9 1.6-.9 2.3.2 5.2 1.5 6.6.8.9 1.7 1.1 2.1.9.5-.2.9-.4 1.3-.4s.8.2 1.3.4c.4.2 1.3 0 2-.8.6-.6 1-1.4 1.2-1.9-1.8-.8-2.2-3.4-.1-4.5-.9-1.2-2.8-1.4-3.6-.9z" />
          <path d="M8.4 3.9c0-1.3.9-2.4 2.2-2.6.1 1.3-.8 2.5-2.2 2.6z" />
        </>
      )}
      {kind === "linux" && (
        <>
          <path d="M8 1.5c-1.7 0-2.6 1.4-2.6 3.1 0 1.2-.4 1.9-1.1 3.1-.8 1.4-1.3 2.9-.8 4.1.5 1 1.8.9 2.6 1.1.4.8 1.2 1.1 1.9 1.1s1.5-.3 1.9-1.1c.8-.2 2.1-.1 2.6-1.1.5-1.2 0-2.7-.8-4.1-.7-1.2-1.1-1.9-1.1-3.1 0-1.7-.9-3.1-2.6-3.1z" />
          <circle cx="6.7" cy="4.4" r=".7" fill="var(--surface, #fff)" />
          <circle cx="9.3" cy="4.4" r=".7" fill="var(--surface, #fff)" />
        </>
      )}
      {kind === "ios" && (
        <>
          <path fillRule="evenodd" d="M5 1h6a1.5 1.5 0 0 1 1.5 1.5v11A1.5 1.5 0 0 1 11 15H5a1.5 1.5 0 0 1-1.5-1.5v-11A1.5 1.5 0 0 1 5 1zm.2 1.6v9.8h5.6V2.6z" />
          <circle cx="8" cy="13.6" r=".6" />
        </>
      )}
      {kind === "android" && (
        <>
          <path d="M3.2 9.5a4.8 4.8 0 0 1 9.6 0z" />
          <rect x="3.2" y="10.2" width="9.6" height="4" rx="1.2" />
          <path d="M5 3.2 6.2 5M11 3.2 9.8 5" stroke="currentColor" strokeWidth="1" strokeLinecap="round" />
          <circle cx="6.2" cy="7.6" r=".6" fill="var(--surface, #fff)" />
          <circle cx="9.8" cy="7.6" r=".6" fill="var(--surface, #fff)" />
        </>
      )}
      {kind === "other" && (
        <>
          <path fillRule="evenodd" d="M2.5 2.5h11A1 1 0 0 1 14.5 3.5v7a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1zm.4 1.2v6.1h10.2V3.7z" />
          <path d="M5.5 13h5v1.2h-5z" />
          <path d="M7.4 11.5h1.2V13H7.4z" />
        </>
      )}
    </svg>
  );
}

const AVATAR_COLORS = ["indigo", "violet", "rose", "amber", "emerald", "teal", "sky", "slate"] as const;

function colorOf(agent: Agent): string {
  const picked = agent.profile?.color?.toLowerCase();
  if (picked && (AVATAR_COLORS as readonly string[]).includes(picked)) return picked;
  let h = 0;
  for (const c of agent.name) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return AVATAR_COLORS[h % (AVATAR_COLORS.length - 1)];
}

/** Exact desktop `<Avatar />`: av-* hue, real avatar image, emoji tint, corner presence dot. */
export function AgentAvatar({
  agent,
  imageUrl,
  size = 36,
  status = false,
}: {
  agent: Agent;
  imageUrl?: string;
  size?: number;
  status?: boolean;
}) {
  const avatar = agent.profile?.avatar;
  const text = avatar || Array.from(agent.name)[0]?.toUpperCase() || "?";
  const emoji = !!avatar && /\p{Extended_Pictographic}/u.test(avatar);
  const tone = agentTone(agent.status);
  return (
    <span
      className={`avatar av-${colorOf(agent)} ${emoji ? "emoji" : ""}`}
      style={{
        width: size,
        height: size,
        fontSize: Math.round(size * (emoji ? 0.56 : text.length > 1 ? 0.38 : 0.46)),
      }}
      aria-hidden="true"
    >
      {imageUrl ? <img className="avatar-img" src={imageUrl} alt="" draggable={false} /> : text}
      {status && tone.color !== "off" && (
        <span className={`presence ${tone.color} ${tone.live ? "live" : ""}`} />
      )}
    </span>
  );
}

export function UserAvatar({ image, size = 28 }: { image?: string | null; size?: number }) {
  const src = image ? (image.startsWith("data:") ? image : `data:image/webp;base64,${image}`) : null;
  return (
    <span className="avatar chat-avatar" style={{ width: size, height: size }} aria-hidden="true">
      {src ? (
        <img className="avatar-img" src={src} alt="" draggable={false} />
      ) : (
        <UserRound size={Math.round(size * 0.54)} strokeWidth={1.8} />
      )}
    </span>
  );
}
