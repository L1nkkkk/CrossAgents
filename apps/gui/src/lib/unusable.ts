// Why the hub says a session or template cannot take work here, and which planned task a
// refused approval is about. The reason is the hub's own words: the GUI shows it, it does not
// write its own (crates/orchestrator/src/approval.rs `unusable_for`, engine.rs `resolve_assignments`).

/**
 * `GET /v1/agents` and `GET /v1/session-candidates` carry an optional `unusable` reason on each
 * agent. Read structurally: an `Agent` from elsewhere in the GUI does not declare the field.
 */
export function unusableOf(agent: unknown): string | undefined {
  const why = (agent as { unusable?: unknown } | null | undefined)?.unusable;
  return typeof why === "string" && why.trim() ? why : undefined;
}

/**
 * The rejected tasks in a refused approval, by key. The hub answers 409 with
 * `the plan is invalid:` and one `task "a": <who>: <why>` line per task it cannot assign; each
 * line belongs on that task's row, not in a page-wide error. Lines about tasks the plan does not
 * have are left out.
 */
export function refusedTasks(message: string, keys: readonly string[]): Record<string, string> {
  const out: Record<string, string> = {};
  for (const line of message.split("\n")) {
    const m = /^task "([^"]+)":\s*(.+)$/.exec(line.trim());
    if (!m) continue;
    const why = m[2].trim();
    if (why && keys.includes(m[1])) out[m[1]] = why;
  }
  return out;
}
