// Mock data for the run page's three additions (docs/research/magpie-lancer.md §2.4 §2.6 §2.7):
// the relay chain on each task line (`Rex ✓ → Ada → 你`), the 需要你 card standing in the timeline
// with its options, and a folded run of steps named by what it produced, thumbnails under it.
// `relay-needyou` in dev/scenarios.ts opens it.

import type { Agent } from "@bindings/Agent";
import type { Handoff } from "@bindings/Handoff";
import type { HumanItem } from "@bindings/HumanItem";
import type { RunDetail } from "@bindings/RunDetail";
import type { Task } from "@bindings/Task";
import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { REPO, agent, ago, clone, fileDiff, planned, run, say, task, team, think, tool, transcript, turn, uid } from "../dev/fixtures";
import { plugins } from "./Agents.mock";
import { quota, usage } from "./Usage.mock";

const REQUEST = "把登录页改成 OAuth（GitHub 和 Google），旧的密码登录保留；会话存储换成 Redis。";

const PLAN = [
  planned("a", "OAuth 回调与令牌交换", "实现 /auth/callback/{provider}：用授权码换 token，取用户信息，建立会话。", "rex", [], ["src/auth/**"]),
  planned("b", "登录页加 OAuth 按钮", "登录页加「用 GitHub 登录」「用 Google 登录」，密码表单保留在下面。", "mia", [], ["web/src/pages/Login.tsx"]),
  planned("c", "会话存储迁到 Redis", "SessionStore 换成 Redis 实现，接口不变；没有 REDIS_URL 时回落到内存。", "rex", [], ["src/auth/session.rs"]),
];

/** Two OAuth buttons, drawn as an SVG: the "picture" a step made, without a real file behind it. */
const SHOT =
  "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHdpZHRoPSIyNjAiIGhlaWdodD0iMTUwIj48cmVjdCB3aWR0aD0iMjYwIiBoZWlnaHQ9IjE1MCIgcng9IjEwIiBmaWxsPSIjMGYxNzJhIi8+PHJlY3QgeD0iMjYiIHk9IjM4IiB3aWR0aD0iMjA4IiBoZWlnaHQ9IjM0IiByeD0iOSIgZmlsbD0iIzM4YmRmOCIvPjx0ZXh0IHg9IjEzMCIgeT0iNjAiIGZvbnQtZmFtaWx5PSJzYW5zLXNlcmlmIiBmb250LXNpemU9IjE1IiBmaWxsPSIjMGIxMjIwIiB0ZXh0LWFuY2hvcj0ibWlkZGxlIj7nlKggR2l0SHViIOeZu+W9lTwvdGV4dD48cmVjdCB4PSIyNiIgeT0iODIiIHdpZHRoPSIyMDgiIGhlaWdodD0iMzQiIHJ4PSI5IiBmaWxsPSJub25lIiBzdHJva2U9IiM0NzU1NjkiLz48dGV4dCB4PSIxMzAiIHk9IjEwNCIgZm9udC1mYW1pbHk9InNhbnMtc2VyaWYiIGZvbnQtc2l6ZT0iMTUiIGZpbGw9IiNjYmQ1ZTEiIHRleHQtYW5jaG9yPSJtaWRkbGUiPueUqCBHb29nbGUg55m75b2VPC90ZXh0Pjwvc3ZnPg==";

export function relayScene(): { state: MockState; script: Step[]; route: string } {
  const { ada, rex, mia, sam } = team();
  const r = run({ title: "登录改造 · 三条任务线", request: REQUEST, planner: ada, worker: rex, team: [rex, mia, sam], status: "running", plan: PLAN, minutesAgo: 52 });
  const state: MockState = { ...emptyState(), plugins: plugins(), quota: quota(), usage: usage() };
  const kai = agent({ name: "kai", plugin: "claude", model: "claude-opus-4-1", role: "worker", profile: { display_name: "Kai", title: "资深工程师", color: "emerald" } });
  const rexA = clone(rex, "a", "idle", `${REPO}/.crossagents/worktrees/a`);
  const miaB = clone(mia, "b", "idle", `${REPO}/.crossagents/worktrees/b`);
  const kaiB = clone(kai, "b", "running", `${REPO}/.crossagents/worktrees/b`);
  state.agents.push(ada, rex, mia, sam, kai, rexA, miaB, kaiB);

  /** One handoff card of task `t`. */
  const card = (t: Task, init: Omit<Partial<Handoff>, "to"> & { kind: Handoff["kind"]; to: Agent; minutesAgo: number }): Handoff => {
    const { minutesAgo, to, ...rest } = init;
    return {
      id: uid(),
      run_id: r.id,
      task_id: t.id,
      task_key: t.key,
      from: r.planner,
      from_name: ada.name,
      to: to.id,
      to_name: to.name,
      reason: null,
      goal: t.spec.goal,
      files: t.spec.scope.allow,
      summary: null,
      decisions: t.spec.interfaces,
      open_questions: [],
      context_tokens: null,
      fresh: true,
      outcome: null,
      at: ago(minutesAgo),
      ...rest,
    };
  };

  // #a: done and handed back — `Rex ✓ → Ada`.
  const ta = task(r, PLAN[0], {
    status: "passed",
    assignee: rexA,
    template: rex,
    minutesAgo: 40,
    summary: "回调、token 交换和会话建立都好了；GitHub 和 Google 都试过，错误提示加了中文。",
    diffStat: " src/auth/oauth.rs | 68 ++++++++++\n src/auth/error.rs | 12 +++\n 2 files changed, 80 insertions(+)",
    checks: [{ command: "cargo test -p auth", exit_code: 0, passed: true, output: "test result: ok. 18 passed; 0 failed" }],
  });
  ta.handoffs = [
    card(ta, { kind: "assign", to: rexA, template: "rex", minutesAgo: 40, outcome: "delivered", summary: "两家 provider 都通了。" }),
  ];

  // #b: Mia put the buttons in, Kai took the rework, and it waits on the person — `Mia ✓ → Kai → 你`.
  const tb = task(r, PLAN[1], { status: "waiting_answer", assignee: kaiB, template: kai, minutesAgo: 36, progress: "按钮放好了，等你定过期会话怎么处理" });
  tb.report = { ...tb.report, question: "已过期的旧会话要一起迁过去吗？还是直接丢掉？" };
  tb.handoffs = [
    card(tb, { kind: "assign", to: miaB, template: "mia", minutesAgo: 36, outcome: "delivered", summary: "两个按钮加好了，深色模式是描边样式。" }),
    card(tb, {
      kind: "reassign",
      to: kaiB,
      template: "kai",
      from: miaB.id,
      from_name: mia.name,
      minutesAgo: 8,
      reason: "Mia 在 #b 上退出了，返工交给 Kai",
      summary: "按钮的位置还要和「或者用密码」那行对齐。",
      context_tokens: 42_000,
    }),
  ];

  // #c: nothing handed out yet — `Rex ○ (下一棒)`.
  const tc = task(r, PLAN[2], { status: "pending", template: rex, minutesAgo: 30 });

  const detail: RunDetail = { run: r, tasks: [ta, tb, tc] };
  state.runs.push(detail);

  // The lead's turn: reads, three files changed, a test that passed, a screenshot — then its
  // words, which fold the steps into one line by what they produced.
  const worked = turn({
    input: REQUEST,
    minutesAgo: 52,
    took: 6,
    items: [
      think("先看现在的登录流程和会话存储，再决定拆几块。"),
      tool("Read src/auth/mod.rs", "read"),
      tool("Read web/src/pages/Login.tsx", "read"),
      tool("Edit src/auth/oauth.rs", "edit", "completed", {
        changes: [
          { path: "src/auth/oauth.rs", kind: "update", diff: fileDiff("src/auth/oauth.rs", 0, [], ["pub async fn callback(provider: Provider, code: &str) -> Result<Session> {"]) },
          { path: "src/auth/error.rs", kind: "update", diff: fileDiff("src/auth/error.rs", 0, [], ["pub enum OAuthError { Expired }"]) },
          { path: "web/src/pages/Login.tsx", kind: "update", diff: fileDiff("web/src/pages/Login.tsx", 18, ["      <PasswordForm />"], ["      <OAuthButtons />", "      <PasswordForm />"]) },
        ],
      }),
      tool("cargo test -p auth", "execute", "completed", {
        command: "cargo test -p auth",
        exit_code: 0,
        duration_ms: 24_000,
        output: "running 18 tests\ntest result: ok. 18 passed; 0 failed; finished in 3.42s",
      }),
      tool("screenshot 登录页", "other", "completed", { result: JSON.stringify({ image: SHOT, note: "浅色模式" }) }),
      say("三条线都派出去了：#a 已交回来，等你接收；#b 的按钮在等你定过期会话怎么处理；#c 还没开工。"),
    ],
  });
  state.transcripts.set(ada.id, transcript(ada, [worked]));

  const inTwenty = new Date(Date.now() + 20 * 60_000).toISOString();
  const askBase: HumanItem = {
    id: "",
    kind: "question",
    from: ada.id,
    run_id: r.id,
    task_id: null,
    text: "",
    detail: null,
    options: [],
    default: null,
    timeout_at: null,
    status: "open",
    answer: null,
    answered_at: null,
    action: null,
    created_at: ago(6),
  };
  const ask = (init: Partial<HumanItem> & Pick<HumanItem, "text">): HumanItem => ({ ...askBase, id: uid(), ...init });
  state.questions = [
    ask({
      task_id: tb.id,
      text: "已过期的旧会话要一起迁过去吗？还是直接丢掉？",
      options: ["一起迁过去", "直接丢掉过期的"],
      default: "一起迁过去",
      timeout_at: inTwenty,
      created_at: ago(6),
    }),
    ask({
      text: "想在 docs/ 下补一页迁移说明，需要你批准。\n\npython scripts/new_doc.py docs/migrate.md",
      detail: "新增 docs/migrate.md：迁移步骤、回滚方式、过期会话怎么算。",
      options: ["批准", "不用了"],
      default: "批准",
      action: "approval",
      created_at: ago(3),
    }),
  ];

  return { state, script: [], route: `#/runs/${r.id}` };
}
