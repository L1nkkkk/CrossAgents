/** Cursor encodes its modes in model IDs; never manufacture an ID the CLI didn't offer. */
export function cursorMode(model: string) {
  let base = model;
  let fast = false;
  let thinking = false;
  let effort = "";
  let suffix: RegExpMatchArray | null;
  while ((suffix = base.match(/-(fast|thinking|minimal|low|medium|high|xhigh|max|none)$/))) {
    const value = suffix[1];
    base = base.slice(0, -suffix[0].length);
    if (value === "fast") fast = true;
    else if (value === "thinking") thinking = true;
    else effort = value;
  }
  return { base, fast, thinking, effort };
}

export function cursorVariants(models: readonly string[], model: string) {
  const base = cursorMode(model).base;
  return models.filter((m) => cursorMode(m).base === base);
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
