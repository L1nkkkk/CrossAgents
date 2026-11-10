// Who is operating a browser tab, and for how long (browser.tsx). An agent's tool finishes in a
// few hundred milliseconds — far too fast for a person to reach 接管 — so its「正在操作」bar
// stays for a while after the last request on that tab; a person's taking over lasts until they
// hand it back. Pure, so the timing is tested in browserControl.test.ts.

/** How long an agent's「正在操作」bar stays after its last request on a tab. */
export const AGENT_HOLD_MS = 15_000;

/** A tab's driver: an agent's tool (with the moment its bar goes), or the person who took it over. */
export type Hold = { kind: "agent"; agent: string; until: number } | { kind: "human" };

/**
 * `agent` made a request on the tab at `now`: its bar shows from now until 15 s after this
 * request, itself included (so each request pushes the deadline out). A person's taking over is
 * never overwritten: its requests are refused while they hold the tab.
 */
export function holdAfterRequest(hold: Hold | null, agent: string, now: number): Hold {
  if (hold?.kind === "human") return hold;
  return { kind: "agent", agent, until: now + AGENT_HOLD_MS };
}

/** Whether an agent's hold has run out at `now`; a person's never does. */
export const holdLapsed = (hold: Hold | null, now: number): boolean => hold?.kind === "agent" && now >= hold.until;
