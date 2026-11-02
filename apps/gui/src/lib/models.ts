// Models as people see them, and the agents the start page makes for them. Picking a model in
// the composer is enough to start a run: the page reuses an agent with that plugin, model and
// role in the workspace, or creates one named after the model.

import { bareModel, familyKeys, modelFamily as familyOf } from "./modelFamily.ts";

/** "claude-opus-4-1" → "Opus 4.1", "gpt-5-codex" → "GPT-5 Codex", "mock-haiku" → "Haiku". */
export function prettyModel(model: string): string {
  const words = model.split(/[-_]/).filter(Boolean);
  if (words[0] === "claude" || words[0] === "mock") words.shift();
  const out: string[] = [];
  for (const w of words) {
    const last = out[out.length - 1];
    // Version digits join with dots: 4 1 → 4.1; "gpt" 5 → GPT-5.
    if (/^\d+$/.test(w) && last && /\d$/.test(last)) out[out.length - 1] = `${last}.${w}`;
    else if (/^\d+$/.test(w) && last === "GPT") out[out.length - 1] = `GPT-${w}`;
    else if (w === "gpt") out.push("GPT");
    else out.push(/^\d/.test(w) ? w : w[0].toUpperCase() + w.slice(1));
  }
  return out.join(" ") || model;
}

/** Vendors whose name is not just their plugin capitalised. */
const VENDOR_LABELS: Record<string, string> = { codebuddy: "CodeBuddy" };

/** "claude" → "Claude", "codebuddy" → "CodeBuddy"; unknown plugins are capitalised. */
export function vendorLabel(plugin: string): string {
  return VENDOR_LABELS[plugin] ?? (plugin ? plugin[0].toUpperCase() + plugin.slice(1) : plugin);
}

/** The login an agent runs on (its config `account`); absent: the vendor's default one. */
export function accountOf(agent: { config: unknown }): string | undefined {
  const v = agent.config && typeof agent.config === "object" ? (agent.config as Record<string, unknown>).account : undefined;
  return typeof v === "string" ? v : undefined;
}

/** The cheaper sibling a lead's workers use: a haiku / mini / flash model, else the lead's own. */
export function workerModelFor(models: string[], lead: string): string {
  return models.find((m) => /haiku|mini|flash|lite/i.test(m)) ?? lead;
}

const slugOf = (model: string) =>
  prettyModel(model)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "") || "agent";

/** `base`, else `base-2`, `base-3`…, whichever is free. */
function free(base: string, taken: Set<string>): string {
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}

/** A free agent name for `role` on `model`, e.g. "lead-opus-4-1", then "lead-opus-4-1-2". */
export function agentNameFor(role: "planner" | "worker", model: string, taken: Set<string>): string {
  return free(`${role === "planner" ? "lead" : "worker"}-${slugOf(model)}`.slice(0, 28), taken);
}

/** A free name for a new chat on `model`: "chat-sonnet-4-5", "chat-sonnet-4-5-2"… */
export function chatNameFor(model: string, taken: Set<string>): string {
  return free(`chat-${slugOf(model)}`.slice(0, 28), taken);
}

/** The group models no family claims; the family table itself lives in lib/modelFamily.ts. */
const OTHER = { key: "other", label: "其他" };

/** "tokenhub_deepseek-v4-flash" → DeepSeek, "hy3-preview" → 混元; anything unknown → 其他. */
export function modelFamily(model: string): { key: string; label: string } {
  const family = familyOf("", model);
  return family ? { key: family.key, label: family.name } : OTHER;
}

/** The first version number in a name: "claude-4.8-opus" → [4, 8], "kimi-k2.7-code" → [2, 7]. */
function versionOf(bare: string): number[] {
  return (bare.match(/\d+(?:\.\d+)*/)?.[0] ?? "").split(".").filter(Boolean).map(Number);
}

/**
 * Newest version first; at the same version by name, and a gateway's copy right after the
 * model itself ("glm-5.3-flash", then "tokenhub_glm-5.3-flash").
 */
function byVersionDesc(a: string, b: string): number {
  const [ba, bb] = [bareModel(a), bareModel(b)];
  const [va, vb] = [versionOf(ba), versionOf(bb)];
  for (let i = 0; i < Math.max(va.length, vb.length); i++) {
    const d = (vb[i] ?? 0) - (va[i] ?? 0);
    if (d) return d;
  }
  return ba.localeCompare(bb, "en") || a.length - b.length || a.localeCompare(b, "en");
}

/** `models` by family, families in a fixed order, 其他 last; each family newest first. */
export function groupModels(models: readonly string[]): { key: string; label: string; models: string[] }[] {
  const groups = new Map<string, { key: string; label: string; models: string[] }>();
  for (const m of models) {
    const f = modelFamily(m);
    const g = groups.get(f.key) ?? { ...f, models: [] };
    g.models.push(m);
    groups.set(f.key, g);
  }
  const order = [...familyKeys(), OTHER.key];
  return order.flatMap((k) => {
    const g = groups.get(k);
    if (!g) return [];
    g.models.sort(byVersionDesc);
    return [g];
  });
}

/**
 * The shown list after turning `models` on or off together, in `all`'s order (the hub keeps
 * that order, and the first shown model is the default). `null` means every model is shown;
 * an empty list is refused (`undefined`): a vendor keeps at least one.
 */
export function switchModels(all: readonly string[], on: ReadonlySet<string>, models: readonly string[], value: boolean): string[] | null | undefined {
  const set = new Set(models);
  const next = all.filter((m) => (value ? on.has(m) || set.has(m) : on.has(m) && !set.has(m)));
  if (!next.length) return undefined;
  return next.length === all.length ? null : next;
}

/** The shown list with only `models` left on (`all`'s order); `undefined` when that is none. */
export function onlyModels(all: readonly string[], models: readonly string[]): string[] | null | undefined {
  const set = new Set(models);
  const next = all.filter((m) => set.has(m));
  if (!next.length) return undefined;
  return next.length === all.length ? null : next;
}

/** A chat's title from its first message: the first line, at most `max` characters. */
export function chatTitle(text: string, max = 24): string {
  const line = text.trim().split(/\r?\n/)[0].trim();
  return line.length > max ? `${line.slice(0, max - 1)}…` : line;
}

/** Names for the effort levels CLIs use; any other level shows as itself. */
const EFFORT_LABELS: Record<string, string> = { minimal: "最低", low: "低", medium: "中", high: "高", xhigh: "超高", max: "最高" };

/** "high" → "高"; a level no CLI here uses yet keeps its own name. */
export function effortLabel(value: string): string {
  return EFFORT_LABELS[value] ?? value;
}

/**
 * A plugin's reasoning effort levels (`PluginInfo.efforts`, lowest first) with their names;
 * empty when it has none. The plugin declares them, so a new vendor needs no change here.
 */
export function effortLevels(efforts: readonly string[] | undefined): { value: string; label: string }[] {
  return (efforts ?? []).map((value) => ({ value, label: effortLabel(value) }));
}

/** The effort an agent's config asks for, if any. */
export function effortOf(config: unknown): string | undefined {
  const e = config && typeof config === "object" ? (config as Record<string, unknown>).effort : undefined;
  return typeof e === "string" && e ? e : undefined;
}
