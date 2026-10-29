// `/` at the start of a composer: which commands to offer (src/slash.tsx draws and keys the
// menu). Pure, so the matching is tested (slash.test.ts).

export type SlashCommand = {
  /** Stable id: menu keys and `aria-activedescendant` targets. */
  key: string;
  /** As typed after the slash, without it. */
  name: string;
  /** One line saying what it does. */
  title: string;
  /** A second line: how it runs here, or what it takes. */
  sub?: string;
  /** The group it is listed under. */
  group: string;
  /** Why it cannot run now, in the words on screen; absent when it can. */
  blocked?: string;
};

/** A titled list: CrossAgents' own commands, then whatever the agent's CLI brings. */
export type SlashGroup = { key: string; label: string; commands: SlashCommand[] };

/** 0: the name itself · 1: the name starts with it · 2: the name has it · 3: the title has it ·
 * 4: no match (the text is a message, not a command). */
const rank = (c: SlashCommand, q: string) => {
  const name = c.name.toLowerCase();
  if (name === q) return 0;
  if (name.startsWith(q)) return 1;
  if (name.includes(q)) return 2;
  return c.title.toLowerCase().includes(q) ? 3 : 4;
};

/** What matches `query` (the text after the slash), best first; every command, in the order the
 * groups were given, while nothing is typed yet. */
export function matchCommands(groups: SlashGroup[], query: string): SlashCommand[] {
  const all = groups.flatMap((g) => g.commands);
  const q = query.trim().toLowerCase();
  if (!q) return all;
  return all.filter((c) => rank(c, q) < 4).sort((a, b) => rank(a, q) - rank(b, q));
}
