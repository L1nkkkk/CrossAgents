// Models as people see them, and the agents the start page makes for them. Picking a model in
// the composer is enough to start a run: the page reuses an agent with that plugin, model and
// role in the workspace, or creates one named after the model.

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
