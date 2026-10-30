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
  { method: "DELETE", path: /^\/v1\/agents\/[^/]+\/messages\/[^/]+$/, level: "write" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/execution$/, level: "read" },
  { method: "POST", path: /^\/v1\/agents\/[^/]+\/execution\/interrupt$/, level: "write" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/resume-command$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/commands$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/transcript$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/turns\/[^/]+\/diff$/, level: "read" },
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/attachments\/[^/]+$/, level: "read" },
  { method: "GET", path: /^\/v1\/notes$/, level: "read" },
  { method: "GET", path: /^\/v1\/plugins$/, level: "read" },
  { method: "POST", path: /^\/v1\/plugins\/[^/]+\/models$/, level: "write" },
  { method: "POST", path: /^\/v1\/plugins\/[^/]+\/(install|upgrade)$/, level: "admin" },
  { method: "GET", path: /^\/v1\/runs$/, level: "read" },
  { method: "POST", path: /^\/v1\/runs$/, level: "write" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+$/, level: "read" },
  // `?branches=&force=` rides along; `route()` drops it before matching.
  { method: "DELETE", path: /^\/v1\/runs\/[^/]+$/, level: "write" },
  { method: "POST", path: /^\/v1\/runs\/[^/]+\/(approve|reject|cancel|land|finish|policy)$/, level: "write" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/diff$/, level: "read" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/trace$/, level: "read" },
  { method: "GET", path: /^\/v1\/runs\/[^/]+\/tasks\/[^/]+\/diff$/, level: "read" },
  { method: "POST", path: /^\/v1\/runs\/[^/]+\/tasks\/[^/]+\/review$/, level: "write" },
  { method: "POST", path: /^\/v1\/repos\/init$/, level: "write" },
  { method: "GET", path: /^\/v1\/usage$/, level: "read" },
  // `?range=&group=&source=` rides along; `route()` drops it before matching.
  { method: "GET", path: /^\/v1\/usage\/series$/, level: "read" },
  { method: "GET", path: /^\/v1\/questions$/, level: "read" },
  { method: "POST", path: /^\/v1\/questions\/[^/]+\/(answer|dismiss)$/, level: "write" },
  { method: "GET", path: /^\/v1\/quota$/, level: "read" },
  { method: "POST", path: /^\/v1\/accounts$/, level: "admin" },
  { method: "DELETE", path: /^\/v1\/accounts\/[^/]+$/, level: "admin" },
  { method: "GET", path: /^\/v1\/accounts\/[^/]+\/usage$/, level: "read" },
  { method: "POST", path: /^\/v1\/accounts\/[^/]+\/login$/, level: "admin" },
  // Files of a scope: a session's cwd, a run's checkout, a task's worktree.
  { method: "GET", path: /^\/v1\/(agents\/[^/]+|runs\/[^/]+(\/tasks\/[^/]+)?)\/files\/(tree|content)$/, level: "read" },
  { method: "POST", path: /^\/v1\/(agents\/[^/]+|runs\/[^/]+(\/tasks\/[^/]+)?)\/files\/resolve$/, level: "read" },
  // An image a tool call named by path, for a thumbnail (agents only: it is the agent's cwd).
  { method: "GET", path: /^\/v1\/agents\/[^/]+\/files\/image$/, level: "read" },
  // The commit graph and one commit's diff.
  { method: "GET", path: /^\/v1\/(runs|agents)\/[^/]+\/graph$/, level: "read" },
  { method: "GET", path: /^\/v1\/(runs|agents)\/[^/]+\/commits\/[^/]+\/diff$/, level: "read" },
];

/** The path without its query string: `/v1/notes?scope=run` matches `/v1/notes`. */
export function route(path: string): string {
  const i = path.indexOf("?");
  return i < 0 ? path : path.slice(0, i);
}

export function allowed(method: string, path: string): boolean {
  const only = route(path);
  return ALLOWED.some((r) => r.method === method && r.path.test(only));
}
