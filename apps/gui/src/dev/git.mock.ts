// A small repository for the Git page (`#/git`) in `preview:mock`: local and remote
// branches, tags, a merge, a branch only on the remote, a local branch with no upstream, and
// a few uncommitted changes. MockBridge answers `GET /v1/git/*` from it through `mockGit`.
//
// Any directory the hub knows (an agent's cwd, a run's repository) is a project, as with the
// hub; only `GIT_ROOT` has history here, the others answer as not under version control.

import type { ChangesResponse } from "@bindings/ChangesResponse";
import type { GitBranch } from "@bindings/GitBranch";
import type { GitBranchesResponse } from "@bindings/GitBranchesResponse";
import type { GitCommitResponse } from "@bindings/GitCommitResponse";
import type { GitGraph } from "@bindings/GitGraph";
import type { GitGraphRef } from "@bindings/GitGraphRef";
import type { GitStatusResponse } from "@bindings/GitStatusResponse";
import type { GraphCommit } from "@bindings/GraphCommit";
import { REPO } from "./fixtures";

export const GIT_ROOT = REPO;

const sha = (seed: string) => (seed + "0123456789abcdef".repeat(3)).slice(0, 40);
const minutesAgo = (m: number) => new Date(Date.now() - m * 60_000).toISOString();

type MockCommit = { sha: string; parents: string[]; subject: string; body?: string; author: string; minutes: number; diff: string };

const ALICE = { author: "Alice Chen" };
const BOB = { author: "Bob Li" };
const CI = { author: "release-bot" };

const file = (path: string, hunk: string, added = false) =>
  [
    `diff --git a/${path} b/${path}`,
    added ? "new file mode 100644" : "index 3b18e51..a9c2f04 100644",
    added ? "--- /dev/null" : `--- a/${path}`,
    `+++ b/${path}`,
    hunk,
  ].join("\n");

const C = {
  merge: sha("a7e3c91d"),
  header: sha("b4d2e08f"),
  totals: sha("c19f7a3e"),
  model: sha("d58a2b6c"),
  hotfix: sha("e2c47f19"),
  search: sha("f6b13d8a"),
  release: sha("1a9e5c27"),
  checkout: sha("2c8d4f6b"),
  initial: sha("3e7b9a15"),
};

const COMMITS: MockCommit[] = [
  {
    sha: C.merge, parents: [C.header, C.totals], subject: "Merge branch 'feature/cart'", body: "购物车合计和数据模型进入主线。", ...ALICE, minutes: 35,
    diff: [
      file("src/cart/model.ts", "@@ -0,0 +1,6 @@\n+export interface CartLine {\n+  sku: string;\n+  qty: number;\n+  price: number;\n+}\n+export type Cart = CartLine[];", true),
      file("src/cart/totals.ts", "@@ -0,0 +1,4 @@\n+import type { Cart } from \"./model\";\n+\n+export const total = (cart: Cart) =>\n+  cart.reduce((sum, l) => sum + l.qty * l.price, 0);", true),
    ].join("\n"),
  },
  {
    sha: C.header, parents: [C.release], subject: "fix: header overlaps the search box on narrow screens", ...BOB, minutes: 80,
    diff: file("src/ui/header.css", "@@ -12,7 +12,8 @@ .header {\n   display: flex;\n   align-items: center;\n-  gap: 24px;\n+  gap: 12px;\n+  flex-wrap: wrap;\n   padding: 0 16px;\n }\n \n"),
  },
  {
    sha: C.totals, parents: [C.model], subject: "cart: compute totals", body: "合计按行累加；折扣留到下一步。", ...ALICE, minutes: 120,
    diff: file("src/cart/totals.ts", "@@ -0,0 +1,4 @@\n+import type { Cart } from \"./model\";\n+\n+export const total = (cart: Cart) =>\n+  cart.reduce((sum, l) => sum + l.qty * l.price, 0);", true),
  },
  {
    sha: C.model, parents: [C.release], subject: "cart: add the cart model", ...ALICE, minutes: 190,
    diff: file("src/cart/model.ts", "@@ -0,0 +1,6 @@\n+export interface CartLine {\n+  sku: string;\n+  qty: number;\n+  price: number;\n+}\n+export type Cart = CartLine[];", true),
  },
  {
    sha: C.hotfix, parents: [C.release], subject: "hotfix: login redirect loops after the session expires", ...BOB, minutes: 260,
    diff: file("src/auth/session.ts", "@@ -40,6 +40,9 @@ export function onExpired(router: Router) {\n   const back = router.current();\n-  router.go(\"/login?next=\" + back);\n+  if (back.startsWith(\"/login\")) return router.go(\"/login\");\n+  router.go(\"/login?next=\" + encodeURIComponent(back));\n }\n"),
  },
  {
    sha: C.search, parents: [C.checkout], subject: "wip: search suggestions", ...BOB, minutes: 600,
    diff: file("src/search/suggest.ts", "@@ -1,3 +1,8 @@\n export async function suggest(q: string) {\n-  return [];\n+  if (q.length < 2) return [];\n+  const res = await fetch(`/api/suggest?q=${encodeURIComponent(q)}`);\n+  return (await res.json()) as string[];\n }\n"),
  },
  {
    sha: C.release, parents: [C.checkout], subject: "release 0.3.0", ...CI, minutes: 60 * 26,
    diff: file("package.json", "@@ -1,6 +1,6 @@\n {\n   \"name\": \"shop\",\n-  \"version\": \"0.2.4\",\n+  \"version\": \"0.3.0\",\n   \"private\": true\n }\n"),
  },
  {
    sha: C.checkout, parents: [C.initial], subject: "checkout: pay with a saved card", body: "保存的卡在结账页直接可选。\n\nRefs: #41", ...ALICE, minutes: 60 * 50,
    diff: file("src/checkout/pay.ts", "@@ -8,4 +8,10 @@ export function pay(order: Order) {\n-  return openCardForm(order);\n+  const saved = cards.saved();\n+  if (saved.length) return chooseCard(order, saved);\n+  return openCardForm(order);\n }\n"),
  },
  {
    sha: C.initial, parents: [], subject: "initial commit", ...ALICE, minutes: 60 * 24 * 9,
    diff: file("README.md", "@@ -0,0 +1,3 @@\n+# shop\n+\n+A small web shop.", true),
  },
];

const REFS: GitGraphRef[] = [
  { name: "HEAD", kind: "head", sha: C.merge },
  { name: "main", kind: "local", sha: C.merge },
  { name: "feature/cart", kind: "local", sha: C.totals },
  { name: "feature/search", kind: "local", sha: C.search },
  { name: "origin/main", kind: "remote", sha: C.header },
  { name: "origin/feature/cart", kind: "remote", sha: C.totals },
  { name: "origin/hotfix/login", kind: "remote", sha: C.hotfix },
  { name: "v0.3.0", kind: "tag", sha: C.release },
  { name: "v0.1.0", kind: "tag", sha: C.initial },
];

const BRANCHES: GitBranch[] = [
  { name: "main", remote: false, sha: C.merge, upstream: "origin/main", ahead: 3, behind: 0, current: true },
  { name: "feature/cart", remote: false, sha: C.totals, upstream: "origin/feature/cart", ahead: 0, behind: 0, current: false },
  { name: "feature/search", remote: false, sha: C.search, ahead: 0, behind: 0, current: false },
  { name: "origin/main", remote: true, sha: C.header, ahead: 0, behind: 0, current: false },
  { name: "origin/feature/cart", remote: true, sha: C.totals, ahead: 0, behind: 0, current: false },
  { name: "origin/hotfix/login", remote: true, sha: C.hotfix, ahead: 0, behind: 0, current: false },
];

function status(): GitStatusResponse {
  return {
    root: GIT_ROOT, head: C.merge, branch: "main", upstream: "origin/main", ahead: 3, behind: 0,
    entries: [
      { path: "src/cart/totals.ts", index: "unchanged", worktree: "modified", untracked: false, conflicted: false },
      { path: "src/cart/discount.ts", index: "added", worktree: "unchanged", untracked: false, conflicted: false },
      { path: "src/ui/Header.tsx", orig_path: "src/ui/header.tsx", index: "renamed", worktree: "modified", untracked: false, conflicted: false },
      { path: "notes/todo.txt", index: "unchanged", worktree: "unchanged", untracked: true, conflicted: false },
    ],
  };
}

function graph(limit: number): GitGraph {
  const refsAt = (s: string) => REFS.filter((r) => r.sha === s).map((r) => r.name);
  const commits: GraphCommit[] = COMMITS.map((c) => ({ sha: c.sha, parents: c.parents, subject: c.subject, author: c.author, time: minutesAgo(c.minutes), refs: refsAt(c.sha) }));
  return { root: GIT_ROOT, refs: REFS, commits: commits.slice(0, limit), truncated: commits.length > limit };
}

function commit(c: MockCommit): GitCommitResponse {
  const email = `${c.author.toLowerCase().replace(/[^a-z]+/g, ".")}@example.com`;
  return { root: GIT_ROOT, sha: c.sha, parents: c.parents, author: c.author, author_email: email, time: minutesAgo(c.minutes), subject: c.subject, message: c.body ? `${c.subject}\n\n${c.body}` : c.subject };
}

/**
 * The answer to `GET <pathname>` under `/v1/git/`, or undefined for another path. `roots`
 * are the directories the mock hub knows (agents' cwd, runs' repositories).
 */
export function mockGit(pathname: string, params: URLSearchParams, roots: string[]): { status: number; body: unknown } | undefined {
  if (!pathname.startsWith("/v1/git/")) return undefined;
  const error = (status: number, code: string, message: string) => ({ status, body: { error: { code, message } } });
  const path = (params.get("path") ?? "").replace(/\\/g, "/").replace(/\/+$/, "");
  if (!path) return error(400, "bad_request", "give the path of a project");
  if (!roots.some((r) => r.replace(/\\/g, "/").replace(/\/+$/, "") === path)) return error(403, "forbidden", `${path} is not a project the hub knows`);
  if (path !== GIT_ROOT) return error(409, "conflict", `${path} is not a git repository`);

  if (pathname === "/v1/git/status") return { status: 200, body: status() };
  if (pathname === "/v1/git/branches") {
    const body: GitBranchesResponse = { root: GIT_ROOT, branches: BRANCHES, head: "main", head_sha: C.merge };
    return { status: 200, body };
  }
  if (pathname === "/v1/git/graph") return { status: 200, body: graph(Number(params.get("limit") ?? 200)) };
  const m = pathname.match(/^\/v1\/git\/commits\/([^/]+)(\/diff)?$/);
  if (m) {
    const want = decodeURIComponent(m[1]);
    const found = COMMITS.find((c) => c.sha === want);
    if (!found) return error(404, "not_found", `commit ${want} not found`);
    if (!m[2]) return { status: 200, body: commit(found) };
    const body: ChangesResponse = { base: found.parents[0] ?? "", head: found.sha, diff: found.diff, truncated: false };
    return { status: 200, body };
  }
  return error(404, "not_found", `${pathname} not found`);
}
