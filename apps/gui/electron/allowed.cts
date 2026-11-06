// Which hub endpoints the page may reach through the main process (docs/API-AUTH.md §2, §4).
// Kept apart from main.cts so a test can check it against src/api.ts without Electron.

export type Level = "read" | "write" | "admin";

/**
 * What the page may ask the hub for. It mirrors `src/api.ts`: endpoints the GUI does not
 * call are refused here, so a page that got script injected cannot reach `/v1/shutdown`,
 * delete an account it never shows, or touch the agent-token routes. Adding a call to
 * `src/api.ts` means adding a row here; the refusal says so, and `allowed.test.cts` fails
 * until it is done.
 *
 * `level` is not enforced here: the hub checks the token's level itself (crates/hub/src/access.rs).
 * It is kept equal to the hub's table so the two can be compared at a glance.
 */
export const ALLOWED: readonly { method: string; path: RegExp; level: Level }[] = [
  { method: "GET", path: /^\/v1\/info$/, level: "read" },
  // The person's own terminals. `output` is not here: main.cts relays it, as it does events.
  { method: "GET", path: /^\/v1\/terminals$/, level: "admin" },
  { method: "POST", path: /^\/v1\/terminals$/, level: "admin" },
  { method: "DELETE", path: /^\/v1\/terminals\/[^/]+$/, level: "admin" },
  { method: "POST", path: /^\/v1\/terminals\/[^/]+\/(input|resize|takeover)$/, level: "admin" },
  { method: "GET", path: /^\/v1\/agents$/, level: "read" },
  { method: "POST", path: /^\/v1\/agents$/, level: "write" },
  // `?repo=` rides along; `route()` drops it before matching.
  { method: "GET", path: /^\/v1\/session-candidates$/, level: "read" },
  { method: "POST", path: /^\/v1\/agents\/[^/]+\/chat$/, level: "write" },
  { method: "DELETE", path: /^\/v1\/agents\/[^/]+$/, level: "write" },
  {
    method: "POST",
    path: /^\/v1\/agents\/[^/]+\/(start|stop|cancel|compact|model|name|topic|archive|profile|settings|connect)$/,
    level: "write",
  },
  { method: "POST", path: /^\/v1\/agents\/[^/]+\/messages$/, level: "write" },
  // Moves a waiting message to the front and interrupts the turn: the same message, same id.
  { method: "POST", path: /^\/v1\/agents\/[^/]+\/messages\/[^/]+\/send-now$/, level: "write" },
  { method: "DELETE", path: /^\/v1\/agents\/[^/]+\/messages\/[^/]+$/, level: "write" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/execution$/, level: "read" },
  { method: "POST", path: /^\/v1\/agents\/[^/]+\/execution\/interrupt$/, level: "write" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/resume-command$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/commands$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/transcript$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/turns\/[^/]+\/diff$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/attachments\/[^/]+$/, level: "read" },
  { method: "GET", path: /^\/v1\/avatars$/, level: "read" },
  { method: "POST", path: /^\/v1\/avatars$/, level: "write" },
  { method: "GET", path: /^\/v1\/avatars\/[^/]+$/, level: "read" },
  { method: "DELETE", path: /^\/v1\/avatars\/[^/]+$/, level: "write" },
  { method: "GET", path: /^\/v1\/notes$/, level: "read" },
  { method: "GET", path: /^\/v1\/background$/, level: "read" },
  { method: "DELETE", path: /^\/v1\/background\/reminders\/[^/]+$/, level: "write" },
  { method: "GET", path: /^\/v1\/plugins$/, level: "read" },
  { method: "POST", path: /^\/v1\/plugins\/[^/]+\/models$/, level: "write" },
  { method: "POST", path: /^\/v1\/plugins\/[^/]+\/(install|upgrade)$/, level: "admin" },
  // Extensions (docs/EXTENSIONS.md §3, §5). Enabling one and switching auto-trust are the
  // person's: the hub's admin level is the real check. A GUI-side call's tool name is bare.
  { method: "GET", path: /^\/v1\/extensions$/, level: "read" },
  { method: "GET", path: /^\/v1\/extensions\/[^/]+\/logs$/, level: "read" },
  { method: "POST", path: /^\/v1\/extensions\/[^/]+\/enable$/, level: "admin" },
  { method: "POST", path: /^\/v1\/extensions\/[^/]+\/disable$/, level: "write" },
  { method: "POST", path: /^\/v1\/extensions\/[^/]+\/auto-trust$/, level: "admin" },
  { method: "GET", path: /^\/v1\/extensions\/[^/]+\/pending\/diff$/, level: "read" },
  { method: "POST", path: /^\/v1\/extensions\/[^/]+\/tools\/[^/]+$/, level: "write" },
  { method: "GET", path: /^\/v1\/extensions\/calls\/[^/]+$/, level: "read" },
  { method: "DELETE", path: /^\/v1\/extensions\/calls\/[^/]+$/, level: "write" },
  // Looking for the CLIs again runs their version commands, as install and upgrade do.
  // `?plugin=` rides along; `route()` drops it before matching.
  { method: "POST", path: /^\/v1\/plugins\/refresh$/, level: "admin" },
  { method: "GET", path: /^\/v1\/runs$/, level: "read" },
  // What ended runs left in a workspace, deleted rounds too; `?repo=` rides along.
  { method: "GET", path: /^\/v1\/history$/, level: "read" },
  // Projects and their checkouts: the id a project's expertise profile is keyed by.
  { method: "GET", path: /^\/v1\/projects$/, level: "read" },
  { method: "POST", path: /^\/v1\/runs$/, level: "write" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+$/, level: "read" },
  // `?branches=&force=` rides along; `route()` drops it before matching.
  { method: "DELETE", path: /^\/v1\/runs\/[^/]+$/, level: "write" },
  { method: "POST", path: /^\/v1\/runs\/[^/]+\/(approve|reject|cancel|land|finish|policy)$/, level: "write" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/diff$/, level: "read" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/trace$/, level: "read" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/cost$/, level: "read" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/tasks\/[^/]+\/diff$/, level: "read" },
  { method: "POST", path: /^\/v1\/runs\/[^/]+\/tasks\/[^/]+\/review$/, level: "write" },
  { method: "POST", path: /^\/v1\/repos\/init$/, level: "write" },
  { method: "GET", path: /^\/v1\/usage$/, level: "read" },
  // `?range=&group=&source=` rides along; `route()` drops it before matching.
  { method: "GET", path: /^\/v1\/usage\/series$/, level: "read" },
  { method: "GET", path: /^\/v1\/expertise$/, level: "read" },
  { method: "GET", path: /^\/v1\/expertise\/(candidates|card|maintenance|export)$/, level: "read" },
  { method: "POST", path: /^\/v1\/expertise\/candidates\/[^/]+\/(accept|reject)$/, level: "write" },
  { method: "GET", path: /^\/v1\/questions$/, level: "read" },
  { method: "POST", path: /^\/v1\/questions\/[^/]+\/(answer|dismiss)$/, level: "write" },
  { method: "GET", path: /^\/v1\/quota$/, level: "read" },
  { method: "POST", path: /^\/v1\/accounts$/, level: "admin" },
  { method: "GET", path: /^\/v1\/accounts\/import$/, level: "read" },
  { method: "POST", path: /^\/v1\/accounts\/import$/, level: "admin" },
  { method: "DELETE", path: /^\/v1\/accounts\/[^/]+$/, level: "admin" },
  { method: "GET", path: /^\/v1\/accounts\/[^/]+\/usage$/, level: "read" },
  { method: "POST", path: /^\/v1\/accounts\/[^/]+\/login$/, level: "admin" },
  // Files of a scope: a session's cwd, a run's checkout, a task's worktree.
  { method: "GET", path: /^\/v1\/(agents\/[^/]+|runs\/[^/]+(\/tasks\/[^/]+)?)\/files\/(tree|content|status|search)$/, level: "read" },
  { method: "POST", path: /^\/v1\/(agents\/[^/]+|runs\/[^/]+(\/tasks\/[^/]+)?)\/files\/resolve$/, level: "read" },
  // An image a tool call named by path, for a thumbnail (agents only: it is the agent's cwd).
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/files\/image$/, level: "read" },
  // The commit graph and one commit's diff.
  { method: "GET", path: /^\/v1\/(runs|agents)\/[^/]+\/graph$/, level: "read" },
  { method: "GET", path: /^\/v1\/(runs|agents)\/[^/]+\/commits\/[^/]+\/diff$/, level: "read" },
];

/** How long the main process waits for an answer before giving up on the hub. */
export const REQUEST_TIMEOUT_MS = 30_000;
/** Compacting answers when the summary is written: minutes for a long history. */
export const COMPACT_TIMEOUT_MS = 15 * 60_000;

export function timeoutFor(method: string, path: string): number {
  return method === "POST" && /^\/v1\/agents\/[^/]+\/compact$/.test(route(path)) ? COMPACT_TIMEOUT_MS : REQUEST_TIMEOUT_MS;
}

/** The path without its query string: `/v1/notes?scope=run` matches `/v1/notes`. */
export function route(path: string): string {
  const i = path.indexOf("?");
  return i < 0 ? path : path.slice(0, i);
}

export function allowed(method: string, path: string): boolean {
  const only = route(path);
  return ALLOWED.some((r) => r.method === method && r.path.test(only));
}
