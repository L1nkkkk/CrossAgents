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

/** "claude" → "Claude"; unknown plugins are capitalised. */
export function vendorLabel(plugin: string): string {
  return plugin ? plugin[0].toUpperCase() + plugin.slice(1) : plugin;
}

/** The cheaper sibling a lead's workers use: a haiku / mini / flash model, else the lead's own. */
export function workerModelFor(models: string[], lead: string): string {
  return models.find((m) => /haiku|mini|flash|lite/i.test(m)) ?? lead;
}

/** A free agent name for `role` on `model`, e.g. "lead-opus-4-1", then "lead-opus-4-1-2". */
export function agentNameFor(role: "planner" | "worker", model: string, taken: Set<string>): string {
  const slug = prettyModel(model)
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  const base = `${role === "planner" ? "lead" : "worker"}-${slug || "agent"}`.slice(0, 28);
  if (!taken.has(base)) return base;
  for (let n = 2; ; n++) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}
