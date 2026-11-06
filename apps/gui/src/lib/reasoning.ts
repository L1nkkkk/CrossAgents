import { effortLabel } from "./models.ts";

/** Cursor encodes its modes in model IDs; never manufacture an ID the CLI didn't offer. */
export function cursorMode(model: string) {
  let base = model;
  let fast = false;
  let thinking = false;
  let effort = "";
  let suffix: RegExpMatchArray | null;
  while ((suffix = base.match(/-(extra-high|fast|thinking|minimal|low|medium|high|xhigh|max|none)$/))) {
    const value = suffix[1];
    base = base.slice(0, -suffix[0].length);
    if (value === "fast") fast = true;
    else if (value === "thinking") thinking = true;
    else effort = value === "extra-high" ? "xhigh" : value;
  }
  return { base, fast, thinking, effort };
}

export function cursorVariants(models: readonly string[], model: string) {
  const base = cursorMode(model).base;
  return models.filter((m) => cursorMode(m).base === base);
}

/** One entry per base model in first-seen order, with every offered variant ID of that base. */
export function cursorFamilies(models: readonly string[]): { base: string; variants: string[] }[] {
  const families = new Map<string, string[]>();
  for (const m of models) {
    const base = cursorMode(m).base;
    const list = families.get(base);
    if (list) list.push(m);
    else families.set(base, [m]);
  }
  return [...families].map(([base, variants]) => ({ base, variants }));
}

/** "claude-opus-5-thinking-high-fast" → "高 · Thinking · Fast"; a bare base is "默认". */
export function cursorModeLabel(model: string): string {
  const mode = cursorMode(model);
  const parts = [mode.effort ? effortLabel(mode.effort) : "", mode.thinking ? "Thinking" : "", mode.fast ? "Fast" : ""].filter(Boolean);
  return parts.length ? parts.join(" · ") : "默认";
}

const EFFORT_ORDER = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/** A base's offered variants for a menu: no effort suffix first, then weakest to strongest; standard before Thinking; Fast off before on. */
export function cursorVariantsOrdered(models: readonly string[], model: string): string[] {
  const rank = (m: string) => {
    const mode = cursorMode(m);
    const e = mode.effort ? EFFORT_ORDER.indexOf(mode.effort) : -1;
    return [mode.effort && e === -1 ? EFFORT_ORDER.length : e, Number(mode.thinking), Number(mode.fast)];
  };
  return cursorVariants(models, model)
    .map((m, i) => ({ m, i, r: rank(m) }))
    .sort((a, b) => a.r[0] - b.r[0] || a.r[1] - b.r[1] || a.r[2] - b.r[2] || a.i - b.i)
    .map((x) => x.m);
}

/** One entry per base model, preserving the selected variant and the CLI's default order. */
export function reasoningModels(plugin: string, models: readonly string[], selected?: string | null): string[] {
  if (plugin !== "cursor") return [...models];
  const seen = new Set<string>();
  return models.filter((m) => {
    const base = cursorMode(m).base;
    if (seen.has(base)) return false;
    seen.add(base);
    return true;
  }).map((m) => selected && models.includes(selected) && cursorMode(selected).base === cursorMode(m).base ? selected : m);
}

/** Change one dimension while retaining others where possible; every result is offered. */
export function cursorWith(models: readonly string[], model: string, change: Partial<ReturnType<typeof cursorMode>>): string | undefined {
  const wanted = { ...cursorMode(model), ...change };
  const keys = Object.keys(change) as (keyof typeof wanted)[];
  return cursorVariants(models, model).filter((m) => {
    const mode = cursorMode(m);
    if (change.effort !== undefined && (mode.thinking !== wanted.thinking || mode.fast !== wanted.fast)) return false;
    if (change.fast !== undefined && (mode.thinking !== wanted.thinking || mode.effort !== wanted.effort)) return false;
    return keys.every((k) => mode[k] === wanted[k]);
  })
    .sort((a, b) => {
      const score = (m: string) => {
        const mode = cursorMode(m);
        return Number(mode.thinking !== wanted.thinking) * 4 + Number(mode.fast !== wanted.fast) * 2 + Number(mode.effort !== wanted.effort);
      };
      return score(a) - score(b);
    })[0];
}
