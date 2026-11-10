// Mock data for a run (preview:mock): Ada plans "OAuth 登录" for Rex and Mia. Two phases:
// `plan` — the plan waits for approval; `running` — two tasks run, one waits to be checked, one
// failed, and Ada's conversation carries the orchestrator's messages and task cards.

import type { Agent } from "@bindings/Agent";
import type { CommitGraph } from "@bindings/CommitGraph";
import type { GraphRef } from "@bindings/GraphRef";
import type { Handoff } from "@bindings/Handoff";
import type { Run } from "@bindings/Run";
import type { HumanItem } from "@bindings/HumanItem";
import type { RunDetail } from "@bindings/RunDetail";
import type { RunTrace } from "@bindings/RunTrace";
import type { Task } from "@bindings/Task";
import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { REPO, agent, ago, clone, fileDiff, planned, run, say, task, team, think, tool, transcript, turn, uid } from "../dev/fixtures";
import { plugins } from "./Agents.mock";
import { quota, usage } from "./Usage.mock";

const REQUEST = "把登录页改成 OAuth（GitHub 和 Google），旧的密码登录保留。会话存储换成 Redis。";

const PLAN = [
  planned("a", "OAuth 回调与令牌交换", "实现 /auth/callback/{provider}：用授权码换 token，取用户信息，建立会话。GitHub 和 Google 两家。", "rex", [], ["src/auth/**"]),
  planned("b", "登录页加 OAuth 按钮", "登录页加「用 GitHub 登录」「用 Google 登录」两个按钮，密码表单保留在下面。深浅色都要好看。", "mia", [], ["web/src/pages/Login.tsx", "web/src/pages/login.css"]),
  planned("c", "会话存储迁到 Redis", "SessionStore 换成 Redis 实现，接口不变；本地开发没有 Redis 时回落到内存。", "rex", [], ["src/auth/session.rs", "Cargo.toml"]),
  planned("d", "旧会话数据迁移脚本", "写一次性脚本把 SQLite 里的会话迁到 Redis，可重复执行。", "rex", ["c"], ["scripts/migrate_sessions.rs"]),
];

const OAUTH_DIFF =
  fileDiff("src/auth/oauth.rs", 0, [], [
    "use crate::auth::session::SessionStore;",
    "",
    "pub enum Provider { GitHub, Google }",
    "",
    "/// Exchanges the authorization code for a token and opens a session.",
    "pub async fn callback(provider: Provider, code: &str, store: &dyn SessionStore) -> Result<Session> {",
    "    let token = exchange(provider, code).await?;",
    "    let user = fetch_user(provider, &token).await?;",
    "    store.open(user.id, token.expires_in).await",
    "}",
  ]) +
  fileDiff("src/auth/mod.rs", 3, ["pub mod password;"], ["pub mod oauth;", "pub mod password;"], ["pub mod session;"]);

const LOGIN_DIFF =
  fileDiff("web/src/pages/Login.tsx", 18, ["      <PasswordForm />"], [
    '      <div className="oauth">',
    '        <OAuthButton provider="github">用 GitHub 登录</OAuthButton>',
    '        <OAuthButton provider="google">用 Google 登录</OAuthButton>',
    "      </div>",
    '      <div className="or">或者用密码</div>',
    "      <PasswordForm />",
  ], ["    <main className=\"login\">"]) +
  fileDiff("web/src/pages/login.css", 40, [], [".oauth { display: grid; gap: 8px; }", ".or { color: var(--muted); text-align: center; }"], [".login { max-width: 360px; }"]);

const SESSION_DIFF =
  fileDiff("src/auth/session.rs", 12, ["pub struct MemoryStore { map: Mutex<HashMap<UserId, Session>> }"], [
    "pub struct RedisStore { pool: deadpool_redis::Pool }",
    "",
    "impl SessionStore for RedisStore {",
    "    async fn open(&self, user: UserId, ttl: u64) -> Result<Session> { /* SET EX */ }",
    "}",
  ]) + fileDiff("Cargo.toml", 22, [], ['deadpool-redis = "0.18"'], ["[dependencies]"]);

const BRIEF = (key: string, title: string) =>
  `Task ${key}: ${title}\n\n你在自己的 worktree 里工作：${REPO}/.crossagents/worktrees/${key}\n做完调用 task_complete 写总结；不清楚就 task_ask。`;

/** The plan waits for approval (`plan`), or the run is under way (`running`). */
/**
 * A run's trace as the hub folds it from events alone: each task's assignee joins its lane when
 * the task starts, and a task past running has handed in (`verifying`) and had its checks run.
 */
function startedTrace(detail: RunDetail): RunTrace {
  const empty = () => ({ agents: [] as string[], turns: [], commands: [], spent: { tokens: 0, cost_usd: 0 } });
  const tasks: RunTrace["tasks"] = detail.tasks.map((t) => {
    const lane = empty();
    const statuses: NonNullable<RunTrace["tasks"][number]["statuses"]> = [];
    const checks: RunTrace["tasks"][number]["checks"] = [];
    const who = t.assignee;
    if (who && t.status !== "pending") {
      lane.agents.push(who);
      statuses.push({ at: t.created_at, from: "pending", to: "running", agent: who });
      if (!["running", "waiting_answer", "stalled"].includes(t.status)) {
        const handedIn = t.report.checks.length ? ago(28) : t.updated_at;
        statuses.push({ at: handedIn, from: "running", to: "verifying", agent: who });
        if (t.report.checks.length) checks.push({ at: ago(27), passed: t.report.checks.every((c) => c.passed), checks: t.report.checks });
        statuses.push({ at: t.updated_at, from: "verifying", to: t.status, agent: who });
      }
    }
    return { task_id: t.id, key: t.key, title: t.title, status: t.status, failure: t.report.failure, lane, checks, rerouted: [], handoffs: t.handoffs ?? [], first_failure: null, statuses };
  });
  return { run_id: detail.run.id, lead: { ...empty(), agents: [detail.run.planner] }, tasks, spent: { tokens: 0, cost_usd: 0 } };
}

export function oauthRun(phase: "plan" | "running"): { state: MockState; script: Step[]; route: string } {
  const { ada, rex, mia, sam } = team();
  const r = run({ title: "OAuth 登录", request: REQUEST, planner: ada, worker: rex, team: [rex, mia, sam], status: phase === "plan" ? "awaiting_approval" : "running", plan: PLAN, minutesAgo: phase === "plan" ? 6 : 48 });
  const state: MockState = { ...emptyState(), plugins: plugins(), quota: quota(), usage: usage() };
  state.agents.push(ada, rex, mia, sam);

  // Ada: read the code, planned, submitted.
  const planning = turn({
    input: REQUEST,
    minutesAgo: phase === "plan" ? 6 : 48,
    took: 4,
    items: [
      think("先看现在的登录流程和会话存储在哪，再决定怎么拆。"),
      tool("Read src/auth/mod.rs", "read"),
      tool("Read src/auth/session.rs", "read"),
      tool("Search PasswordLogin in src", "search"),
      tool("Read web/src/pages/Login.tsx", "read"),
      say("现在密码登录和会话存储都在 `src/auth` 里，前端只有一个登录页。拆成四个任务：后端回调、登录页按钮、会话存储换 Redis，以及迁移旧会话的脚本（等存储那步合并后再做）。"),
      tool("plan_submit", "other", "completed", { arguments: JSON.stringify({ tasks: PLAN.map((p) => p.key) }) }),
      say("计划已提交，等你审批。"),
    ],
  });
  const adaTurns = [planning];

  if (phase === "running") {
    const rexA = clone(rex, "a", "running", `${REPO}/.crossagents/worktrees/a`);
    const miaB = clone(mia, "b", "running", `${REPO}/.crossagents/worktrees/b`);
    const rexC = clone(rex, "c", "idle", `${REPO}/.crossagents/worktrees/c`);
    const rexD = clone(rex, "d", "error", `${REPO}/.crossagents/worktrees/d`);
    state.agents.push(rexA, miaB, rexC, rexD);
    const [pa, pb, pc, pd] = PLAN;
    const ta = task(r, pa, { status: "running", assignee: rexA, template: rex, minutesAgo: 40, progress: "回调写好了，在补 token 过期的处理" });
    const tb = task(r, pb, { status: "running", assignee: miaB, template: mia, minutesAgo: 40 });
    const tc = task(r, pc, {
      status: "passed",
      assignee: rexC,
      template: rex,
      minutesAgo: 40,
      summary: "SessionStore 换成了 RedisStore，接口没变；`REDIS_URL` 没设置时回落到 MemoryStore。",
      diffStat: " Cargo.toml          |  1 +\n src/auth/session.rs | 18 +++++++++++-------\n 2 files changed, 12 insertions(+), 7 deletions(-)",
      checks: [
        { command: "cargo test -p auth", exit_code: 0, passed: true, output: "test result: ok. 14 passed" },
        { command: "npm test", exit_code: 0, passed: true, output: "Tests: 32 passed" },
      ],
    });
    const td = task(r, pd, {
      status: "failed",
      assignee: rexD,
      template: rex,
      minutesAgo: 30,
      failure: "验收失败：cargo test -p auth 有 2 个用例失败（migrate::tests::idempotent, migrate::tests::expired）。",
      checks: [{ command: "cargo test -p auth", exit_code: 101, passed: false, output: "failures:\n    migrate::tests::idempotent\n    migrate::tests::expired\n\ntest result: FAILED. 12 passed; 2 failed" }],
    });
    const detail: RunDetail = { run: r, tasks: [ta, tb, tc, td] };
    state.runs.push(detail);
    // The hub's trace puts each assignee on its task's lane as the task starts
    // (`task.status_changed` with an assignee); without it the swimlane said nobody had begun
    // while the task list showed two running.
    state.traces.set(r.id, startedTrace(detail));

    // Ada hears from the orchestrator: approved, c passed, d failed.
    const orch = (taskId?: string) => ({ kind: "orchestrator" as const, run_id: r.id, ...(taskId ? { task_id: taskId } : {}) });
    adaTurns.push(
      turn({ input: "Plan approved. Tasks a, b, c dispatched; d waits for c.", from: orch(), minutesAgo: 41, took: 1, items: [say("开工了：Rex 做 a 和 c，Mia 做 b。")] }),
      turn({
        input: "Task c passed acceptance (2/2 checks). Review the diff and merge it, or send it back.",
        from: orch(tc.id),
        minutesAgo: 14,
        took: 2,
        items: [tool("task_diff c", "other"), think("接口没变，回落逻辑也在。"), say("c 看过了：接口没动、本地回落到内存也有测试。等你确认后合并。")],
      }),
      turn({
        input: "Task d failed: cargo test -p auth exited 101 (2 failed).",
        from: orch(td.id),
        minutesAgo: 8,
        took: 2,
        items: [tool("task_logs d", "other"), say("d 失败是因为测试里的 fixtures 路径写死了 SQLite 文件。我会在 c 合并后让 Rex 重做，先不重试。")],
      }),
    );

    // Rex on a: one finished turn with changes, one under way (the script carries it on).
    const a1 = turn({
      input: BRIEF("a", pa.title),
      from: orch(ta.id),
      minutesAgo: 40,
      took: 12,
      items: [
        think("先看 auth 模块的结构和现有的 SessionStore 接口。"),
        tool("Read src/auth/mod.rs", "read"),
        tool("Read src/auth/session.rs", "read"),
        tool("Read Cargo.toml", "read"),
        tool("Edit src/auth/oauth.rs", "edit", "completed", { changes: [{ path: "src/auth/oauth.rs", kind: "add", diff: OAUTH_DIFF.split("diff --git a/src/auth/mod.rs")[0] }] }),
        tool("Edit src/auth/mod.rs", "edit"),
        tool("cargo test -p auth", "execute", "completed", { command: "cargo test -p auth", exit_code: 0, duration_ms: 8_400, output: "running 14 tests\n..............\ntest result: ok. 14 passed; 0 failed" }),
        say("回调和 token 交换写好了，`cargo test -p auth` 通过。接下来补 token 过期和 provider 报错的处理。"),
      ],
      diff: OAUTH_DIFF,
    });
    const editId = uid();
    const a2 = turn({
      input: "继续：token 过期、provider 返回错误时要给用户能看懂的提示。",
      from: { kind: "agent", agent_id: ada.id },
      minutesAgo: 3,
      running: true,
      items: [
        tool("Read src/auth/oauth.rs", "read"),
        { type: "tool_call", tool_call_id: editId, title: "Edit src/auth/oauth.rs", kind: "edit", status: "in_progress" },
      ],
    });
    state.transcripts.set(rexA.id, transcript(rexA, [a1, a2], 61_000));
    state.diffs.set(a1.turn.message_id, OAUTH_DIFF);

    const b1 = turn({
      input: BRIEF("b", pb.title),
      from: orch(tb.id),
      minutesAgo: 39,
      took: 9,
      items: [
        tool("Read web/src/pages/Login.tsx", "read"),
        tool("Read web/src/pages/login.css", "read"),
        tool("Edit web/src/pages/Login.tsx", "edit"),
        tool("Edit web/src/pages/login.css", "edit"),
        tool("npm run build", "execute", "completed", { command: "npm run build", exit_code: 0, duration_ms: 5_100 }),
        say("两个按钮加在密码表单上面，中间一条「或者用密码」分隔。深色下按钮用描边样式。"),
      ],
      diff: LOGIN_DIFF,
      source: "hub",
    });
    const b2 = turn({ input: "截个深色模式的图给我看看。", from: { kind: "agent", agent_id: ada.id }, minutesAgo: 2, running: true, items: [think("用 playwright 打开登录页，切到深色。")] });
    state.transcripts.set(miaB.id, transcript(miaB, [b1, b2], 44_000));
    state.diffs.set(b1.turn.message_id, LOGIN_DIFF);

    const c1 = turn({
      input: BRIEF("c", pc.title),
      from: orch(tc.id),
      minutesAgo: 38,
      took: 20,
      items: [tool("Read src/auth/session.rs", "read"), tool("Edit src/auth/session.rs", "edit"), tool("Edit Cargo.toml", "edit"), tool("cargo test -p auth", "execute", "completed", { exit_code: 0 }), tool("task_complete", "other"), say("完成，总结已提交。")],
      diff: SESSION_DIFF,
    });
    state.transcripts.set(rexC.id, transcript(rexC, [c1]));
    state.diffs.set(c1.turn.message_id, SESSION_DIFF);

    const d1 = turn({
      input: BRIEF("d", pd.title),
      from: orch(td.id),
      minutesAgo: 30,
      took: 6,
      items: [
        tool("Edit scripts/migrate_sessions.rs", "edit"),
        tool("cargo test -p auth", "execute", "failed", { command: "cargo test -p auth", exit_code: 101, duration_ms: 9_800, output: "failures:\n    migrate::tests::idempotent\n    migrate::tests::expired\n\ntest result: FAILED. 12 passed; 2 failed" }),
        { type: "error", message: "agent process exited unexpectedly (code 1)" },
      ],
    });
    state.transcripts.set(rexD.id, transcript(rexD, [d1]));

    // Ada asks something.
    const ask: HumanItem = {
      id: uid(),
      kind: "question",
      detail: null,
      from: ada.id,
      run_id: r.id,
      task_id: null,
      text: "旧的密码登录要不要在页面上加一句「即将下线」的提示？",
      options: ["加提示", "先不加"],
      default: "先不加",
      timeout_at: ago(-25),
      status: "open",
      answer: null,
      answered_at: null,
      action: null,
      created_at: ago(5),
    };
    state.questions.push(ask);

    // The agents' notes for each other, shown in the run's rail.
    state.notes = [
      {
        scope: "run",
        run_id: r.id,
        key: "oauth-约定",
        text: "回调地址统一用 `/auth/callback/{provider}`；错误提示写中文，别把 provider 的原文抛给用户。",
        author: ada.id,
        updated_at: ago(26),
      },
      {
        scope: "run",
        run_id: r.id,
        key: "redis-回落",
        text: "本地没有 Redis 时回落到内存；`REDIS_URL` 没设就不要在启动日志里警告。",
        author: rexC.id,
        updated_at: ago(18),
      },
      {
        scope: "run",
        run_id: r.id,
        key: "迁移脚本",
        text: "旧会话里的过期条目直接丢掉，不迁。",
        author: rexD.id,
        updated_at: ago(7),
      },
    ];

    state.transcripts.set(ada.id, transcript(ada, adaTurns, 92_000));
    return { state, script: runningScript(rexA.id, a2.turn.message_id, editId, ta.id, r.id, miaB.id), route: `#/runs/${r.id}` };
  }

  state.runs.push({ run: r, tasks: [] });
  state.transcripts.set(ada.id, transcript(ada, adaTurns, 52_000));
  return { state, script: [], route: `#/runs/${r.id}` };
}

/**
 * Stopped for the person: the budget ran out mid-run. The conversation keeps one line saying
 * so (and the questions above the composer); the run's plan and facts stay in the rail.
 */
export function pausedRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("plan");
  const d = scene.state.runs[0];
  d.run = { ...d.run, status: "paused", note: "预算上限 $8 已经用完：先停在这里，加预算或取消这个 run。" };
  return { ...scene, script: [], route: `#/runs/${d.run.id}` };
}

/** The plan of a run in a folder without git: it works in place, one task at a time. */
export function inPlaceRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("plan");
  const d = scene.state.runs[0];
  d.run = { ...d.run, title: "简历润色", repo: "C:/Users/me/CV_Resume", in_place: true, base_branch: "", branch: "", config: { ...d.run.config, max_parallel: 1 } };
  return { ...scene, route: `#/runs/${d.run.id}` };
}

/** Rex's second turn on a goes on: the edit lands, tests run with output, the diff arrives. */
function runningScript(rex: string, message_id: string, editId: string, taskId: string, runId: string, mia: string): Step[] {
  const testId = uid();
  const out = (at: number, text: string): Step => ({ at, event: { type: "agent.tool_output", data: { id: rex, tool_call_id: testId, text } } });
  const diff =
    OAUTH_DIFF +
    fileDiff("src/auth/error.rs", 0, [], [
      "#[derive(Debug, thiserror::Error)]",
      "pub enum OAuthError {",
      '    #[error("登录已过期，请重新登录")]',
      "    Expired,",
      '    #[error("{provider} 拒绝了这次登录：{reason}")]',
      "    Denied { provider: String, reason: String },",
      "}",
    ]);
  return [
    { at: 1200, event: { type: "agent.tool_call_updated", data: { id: rex, tool_call_id: editId, title: null, status: "completed" } } },
    { at: 1800, event: { type: "agent.tool_call", data: { id: rex, tool_call_id: uid(), title: "Edit src/auth/error.rs", kind: "edit", status: "completed" } } },
    { at: 2600, event: { type: "agent.tool_call", data: { id: rex, tool_call_id: testId, title: "cargo test -p auth", kind: "execute", status: "in_progress" } } },
    out(3200, "   Compiling auth v0.3.0\n"),
    out(4200, "running 16 tests\n"),
    out(5000, "................\n"),
    { at: 5600, event: { type: "agent.tool_call_updated", data: { id: rex, tool_call_id: testId, title: null, status: "completed" } } },
    { at: 5650, event: { type: "agent.tool_detail", data: { id: rex, tool_call_id: testId, detail: { command: "cargo test -p auth", exit_code: 0, duration_ms: 3_100, output: "running 16 tests\n................\ntest result: ok. 16 passed", changes: [] } } } },
    { at: 6000, event: { type: "agent.turn_diff", data: { id: rex, diff, message_id } } },
    { at: 6400, event: { type: "task.progress", data: { run_id: runId, id: taskId, key: "a", text: "错误处理写完，测试 16/16 通过" } } },
    { at: 6800, event: { type: "agent.output", data: { id: rex, stream: "text", text: "过期和 provider 拒绝都有了对应的错误类型，" } } },
    { at: 7300, event: { type: "agent.output", data: { id: rex, stream: "text", text: "提示文案用中文。测试 16 个全过。" } } },
    { at: 7900, event: { type: "agent.turn_completed", data: { id: rex, message_id, stop_reason: "end_turn" } } },
    { at: 7950, event: { type: "agent.status_changed", data: { id: rex, from: "running", to: "idle" } } },
    { at: 3500, event: { type: "agent.tool_call", data: { id: mia, tool_call_id: uid(), title: "npx playwright screenshot --color-scheme=dark /login", kind: "execute", status: "in_progress" } } },
  ];
}

/**
 * Reviewing (docs/tasks/DOGFOOD.md §2): the run under way, opened on #c — reported done and
 * waiting to be checked — with its whole diff; comments on lines go to Rex, or send it back.
 */
export function reviewRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const byKey = (k: string) => d.tasks.find((t) => t.key === k)!;
  scene.state.taskDiffs.set(byKey("a").id, OAUTH_DIFF);
  scene.state.taskDiffs.set(byKey("b").id, LOGIN_DIFF);
  scene.state.taskDiffs.set(byKey("c").id, SESSION_DIFF);
  return { ...scene, script: [], route: `#/runs/${d.run.id}/tasks/${byKey("c").id}` };
}

/** Before landing: every task merged, the lead wrapping up; the run's total diff can be opened. */
export function landingRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  for (const t of d.tasks) {
    t.status = "merged";
    t.assignee = null;
  }
  d.run = { ...d.run, summary: null };
  scene.state.runDiffs.set(d.run.id, OAUTH_DIFF + LOGIN_DIFF + SESSION_DIFF);
  return { ...scene, script: [], route: `#/runs/${d.run.id}` };
}

/**
 * Landing waits (docs/tasks/DOGFOOD.md §4): main moved and conflicts with the run, so the hub
 * made #sync-main for Rex; the note says why, the run lands once it is merged.
 */
export function syncRun(): { state: MockState; script: Step[]; route: string } {
  const scene = landingRun();
  const d = scene.state.runs[0];
  const rex = scene.state.agents.find((a) => a.name === "Rex")!;
  const rexSync = clone(rex, "sync-main", "running", `${REPO}/.crossagents/worktrees/sync-main`);
  scene.state.agents.push(rexSync);
  const sync = task(
    d.run,
    planned("sync-main", "Resolve conflicts with main", "main gained commits after this run started, and they conflict with the run's changes, so the run cannot land. In your worktree, run `git merge main`, resolve the conflicts so that both sides' changes survive, run the tests, commit, and task_complete.\n\nConflicting files:\n- src/auth/session.rs", "rex", [], []),
    { status: "running", assignee: rexSync, template: rex, minutesAgo: 1, progress: "合并 main，在解决 session.rs 的冲突" },
  );
  d.tasks.push(sync);
  d.run = { ...d.run, note: "main moved and conflicts with the run in src/auth/session.rs; task #sync-main resolves it, land again once it is merged" };
  // The note is the point of this scene: the conversation starts at the top, where it is.
  return { ...scene, script: [{ at: 400, act: () => document.querySelector<HTMLElement>(".convo")?.scrollTo({ top: 0 }) }], route: `#/runs/${d.run.id}` };
}

/**
 * Quota (docs/tasks/DOGFOOD.md §5): Mia's account ran out mid-task, so #b went to a worker
 * on another account; the card says so (a few seconds in).
 */
export function rerouteRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const b = d.tasks.find((t) => t.key === "b")!;
  const reason = "runs on account claude-work instead: account claude-main (its 5h window is used up)";
  return {
    ...scene,
    script: [{ at: 1500, event: { type: "task.rerouted", data: { run_id: d.run.id, id: b.id, key: "b", to: b.assignee!, reason } } }],
    route: `#/runs/${d.run.id}`,
  };
}

/**
 * Approvals (docs/tasks/DOGFOOD.md §7): Rex, working on #a under the run's guard, wants to
 * push; the request waits in 需要你 with 允许 / 拒绝, declining by itself in 30 minutes.
 */
export function approvalRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const a = d.tasks.find((t) => t.key === "a")!;
  const rexA = scene.state.agents.find((x) => x.id === a.assignee)!;
  scene.state.questions.push({
    id: `a-${uid().slice(0, 10)}`,
    kind: "question",
    detail: null,
    from: rexA.id,
    run_id: d.run.id,
    task_id: a.id,
    text: `${rexA.name} 想要执行（pushes to a remote）：\ngit push origin crossagents/oauth-login/a`,
    options: ["允许", "拒绝"],
    default: "拒绝",
    timeout_at: ago(-29),
    status: "open",
    answer: null,
    answered_at: null,
    action: "approval",
    created_at: ago(1),
  });
  return { ...scene, route: "#/" };
}

/**
 * Deleting a run (docs/tasks/R1.md §3): the confirmation as it looks for a run that landed, one
 * that did not, and one whose checkout still has changes — the last answers 409 with the files
 * when the person clicks 删除 (the screenshot script does).
 */
export function deleteRun(kind: "landed" | "unlanded" | "dirty"): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  for (const t of d.tasks) {
    t.status = "merged";
    t.assignee = null;
  }
  const landed = kind === "landed";
  d.run = {
    ...d.run,
    status: "completed",
    summary: "四个任务都合了：OAuth 回调、登录页按钮、Redis 会话存储。迁移脚本留给下一个 run。",
    landed: landed ? { mode: "merge", by: "person", reference: "9f2c1ab", at: ago(20) } : null,
  };
  if (kind === "dirty") scene.state.dirty = { [d.run.id]: ["src/auth/session.rs", "web/src/pages/Login.tsx"] };
  return { ...scene, script: [], route: `#/runs/${d.run.id}/delete` };
}

/**
 * Long results kept short: Ada's first `run_land` was refused with the whole `cargo test` log in
 * the reason, and her closing summary runs to many lines. Both show their opening lines and
 * 展开全文; the route is Ada's conversation, the run page has the summary.
 */
export function longResultsRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  for (const t of d.tasks) {
    t.status = "merged";
    t.assignee = null;
  }
  const deps = "C:\\Users\\me\\.crossagents\\worktrees\\ab12cd34\\targets\\integration\\cargo\\debug\\deps";
  const log = [
    "    Finished `test` profile [unoptimized + debuginfo] target(s) in 1m 13s",
    ...["api_types", "e2e", "exec", "shim", "core", "exec_locks", "exec_tree", "lock_kill_race", "drivers", "hub", "api", "accounts", "acp", "landing"].map(
      (name, i) => `     Running tests\\${name}.rs (${deps}\\${name}-${(i * 7919).toString(16).padStart(16, "0")}.exe)`,
    ),
    "test the_checks_wait_for_the_leads_own_build ... FAILED",
    "error: test failed, to rerun pass `-p crossagents-mock-agent --test lead`",
  ].join("\n");
  const refused = `conflict: the project's checks (crossagents.toml) failed: 0/1 passed; failed: $ cargo test --workspace (exit 101)\n${log}\nFix it in your checkout and call run_land again. If a check itself is wrong, tell the person: they can still land or end the run from the run page.`;
  const ada = scene.state.agents.find((a) => a.id === d.run.planner)!;
  const t = scene.state.transcripts.get(ada.id)!;
  const landing = turn({
    input: "所有任务都合并了，落地吧。",
    minutesAgo: 2,
    took: 2,
    items: [
      tool("crossagents.run_land", "mcp", "failed", { arguments: JSON.stringify({ message: "feat: OAuth 登录" }), error: refused, changes: [] }),
      say("检查挂在 `lead.rs` 一个断言旧行为的测试上，我改写后再落地。"),
    ],
  });
  t.turns = [...t.turns, landing.turn];
  d.run = {
    ...d.run,
    status: "completed",
    landed: { mode: "merge", by: ada.name, reference: "4a19c00", at: ago(1) },
    summary: [
      "OAuth 登录已落地到 main（4a19c00，检查通过）。",
      "",
      "做了什么：",
      "- #a：/auth/callback/{provider} 用授权码换 token、取用户信息、建立会话，GitHub 和 Google 两家。",
      "- #b：登录页加了两个 OAuth 按钮，密码表单保留在下面，深浅色都核对过。",
      "- #c：SessionStore 换成 RedisStore，接口不变；`REDIS_URL` 没设置时回落到 MemoryStore。",
      "- #d：一次性迁移脚本，可重复执行。",
      "",
      "需要你知道的：",
      "- Google 的回调地址要在控制台里加上生产域名。",
      "- Redis 连接池默认 16，压测前可能要调。",
      "- 落地前检查挂在一个断言旧行为的测试上，已改写。",
    ].join("\n"),
  };
  return { ...scene, script: [], route: `#/s/${ada.name}` };
}

/**
 * Tracing a failure (docs/tasks/DOGFOOD.md §8): the run page's 追溯 shows #d's first failure
 * (its tests exited 101), every turn and command, and what each task cost.
 */
export function traceRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const byKey = (k: string) => d.tasks.find((t) => t.key === k)!;
  const lane = (agent: string, turns: number, usd: number, commands: RunTrace["lead"]["commands"] = []): RunTrace["lead"] => ({
    agents: [agent],
    turns: Array.from({ length: turns }, (_, i) => ({ agent, message_id: uid(), started_at: ago(40 - i * 10), completed_at: ago(38 - i * 10), stop_reason: "end_turn", error: null })),
    commands,
    spent: { tokens: Math.round(usd * 90_000), cost_usd: usd },
  });
  const cmd = (agent: string, command: string, exit: number, minutesAgo: number, output_tail: string | null = null, checker = false) => ({ agent, command, exit_code: exit, duration_ms: 8400, at: ago(minutesAgo), checker, output_tail });
  const td = byKey("d");
  const rexD = td.assignee!;
  const failedLane = lane(rexD, 1, 0.21, [cmd(rexD, "cargo build -p auth", 0, 29), cmd(rexD, "cargo test -p auth", 101, 28, "failures:\n    migrate::tests::idempotent\n    migrate::tests::expired\n\ntest result: FAILED. 12 passed; 2 failed")]);
  failedLane.turns[0] = { ...failedLane.turns[0], stop_reason: "error", error: "agent process exited unexpectedly (code 1)" };
  // #c failed once on the way, then passed: its first failure stays, in grey.
  const tc = byKey("c");
  const recoveredLane = lane(tc.assignee!, 2, 0.18, [cmd(tc.assignee!, "cargo test -p auth", 101, 33, "error[E0433]: failed to resolve: use of undeclared crate `redis`"), cmd(tc.assignee!, "cargo test -p auth", 0, 30)]);
  const tasks: RunTrace["tasks"] = d.tasks.map((t) => ({
    task_id: t.id,
    key: t.key,
    title: t.title,
    status: t.status,
    failure: t.report.failure,
    lane: t.key === "d" ? failedLane : t.key === "c" ? recoveredLane : lane(t.assignee!, t.key === "a" ? 2 : 1, t.key === "a" ? 0.34 : 0.12, [cmd(t.assignee!, "cargo test -p auth", 0, 30)]),
    checks: t.report.checks.length ? [{ at: ago(27), passed: t.report.checks.every((c) => c.passed), checks: t.report.checks }] : [],
    rerouted: [],
    handoffs: t.handoffs,
    first_failure: t.key === "d" || t.key === "c" ? "`cargo test -p auth` exited 101" : null,
  }));
  scene.state.traces.set(d.run.id, { run_id: d.run.id, lead: lane(d.run.planner, 3, 0.41), tasks, spent: { tokens: 120_000, cost_usd: 1.32 } });
  // The trace starts folded at the rail's end: this scene opens it and scrolls down to it.
  const open: Step = { at: 400, act: () => document.querySelector<HTMLButtonElement>('[data-card="run-trace"] .rail-head')?.click() };
  const show: Step = { at: 800, act: () => document.querySelector<HTMLElement>(".rail")?.scrollTo({ top: 99999 }) };
  return { ...scene, script: [open, show], route: `#/runs/${d.run.id}` };
}

/**
 * Handoff cards (docs/research/magpie-lancer.md §2.1): every task's chain of hands on the run
 * page and in Ada's conversation — #a its first worker (派活), #c given to Mia after a rework
 * (改派), #d escalated to Kai with a question still open (升级), and #b moving to another
 * account when Mia's ran out (额度改派, arriving a moment in).
 */
export function handoffRun(): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const r = d.run;
  const name = (id: string) => scene.state.agents.find((a) => a.id === id)!.name;
  const byKey = (k: string) => d.tasks.find((t) => t.key === k)!;
  const kai = agent({ name: "kai", plugin: "claude", model: "claude-opus-4-1", role: "worker", profile: { display_name: "Kai", title: "资深工程师", color: "emerald" } });
  const mia = scene.state.agents.find((a) => a.name === "Mia")!;
  const miaC = clone(mia, "c", "running", `${REPO}/.crossagents/worktrees/c`);
  const kaiD = clone(kai, "d", "running", `${REPO}/.crossagents/worktrees/d`);
  scene.state.agents.push(kai, miaC, kaiD);
  r.config = { ...r.config, escalate: kai.id, team: [...r.config.team, kai.id] };

  const card = (t: Task, { minutesAgo, ...init }: Partial<Handoff> & { kind: Handoff["kind"]; to: string; minutesAgo: number }): Handoff => ({
    id: uid(),
    run_id: r.id,
    task_id: t.id,
    task_key: t.key,
    from: r.planner,
    from_name: name(r.planner),
    to_name: name(t.worker_template ?? init.to),
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
    ...init,
  });

  for (const t of d.tasks) t.handoffs = [card(t, { kind: "assign", to: t.assignee!, minutesAgo: 40 })];

  // #c: Ada sent it back once, then gave it to Mia.
  const c = byKey("c");
  const rexC = c.assignee!;
  c.report = { ...c.report, rework: ["SessionStore 的接口别动：open/close 保持 async，调用方不改"] };
  c.status = "running";
  c.assignee = miaC.id;
  c.worker_template = mia.id;
  c.handoffs.push(
    card(c, {
      kind: "reassign",
      from: rexC,
      from_name: "Rex",
      to: miaC.id,
      to_name: "Mia",
      minutesAgo: 12,
      reason: "Rex 在 a 上忙不过来，c 的返工交给 Mia",
      files: ["src/auth/session.rs", "Cargo.toml", "src/auth/session_tests.rs"],
      summary: "SessionStore 换成了 RedisStore，接口没变；`REDIS_URL` 没设置时回落到 MemoryStore。",
      decisions: ["SessionStore 的接口别动：open/close 保持 async，调用方不改"],
      context_tokens: 61_000,
    }),
  );

  // #d: two failed attempts, escalated to Kai with Rex's question unanswered.
  const td = byKey("d");
  const rexD = td.assignee!;
  td.status = "running";
  // `escalated` stays false: nothing sets it since H1-3 took automatic escalation out; the
  // badge comes from the escalation card, as it does for a real run.
  td.assignee = kaiD.id;
  td.report = { ...td.report, failure: null };
  td.handoffs.push(
    card(td, {
      kind: "escalate",
      from: rexD,
      from_name: "Rex",
      to: kaiD.id,
      to_name: "Kai",
      minutesAgo: 6,
      reason: "验收两次没过：migrate::tests::idempotent、migrate::tests::expired 仍失败",
      files: ["scripts/migrate_sessions.rs", "src/auth/migrate.rs"],
      summary: "迁移脚本写好了，能跑；两个测试的 fixture 写死了 SQLite 文件路径，没修完。",
      open_questions: ["已过期的旧会话要迁过去吗，还是直接丢掉？"],
      context_tokens: 184_500,
    }),
  );

  // #b: Mia's account runs out; the card arrives with the event, a moment in.
  const b = byKey("b");
  const reroute = card(b, {
    kind: "reroute",
    from: b.assignee,
    from_name: "Mia",
    to: b.assignee!,
    to_name: "Mia",
    minutesAgo: 0,
    reason: "runs on account claude-work instead: account claude-main (its 5h window is used up)",
    files: ["web/src/pages/Login.tsx", "web/src/pages/login.css"],
    summary: "两个按钮加在密码表单上面，中间一条「或者用密码」分隔。",
    context_tokens: 142_000,
  });
  return {
    ...scene,
    script: [
      { at: 500, event: { type: "task.rerouted", data: { run_id: r.id, id: b.id, key: "b", to: b.assignee!, reason: reroute.reason! } } },
      { at: 520, event: { type: "task.handed_off", data: { run_id: r.id, handoff: reroute } } },
    ],
    route: `#/runs/${r.id}`,
  };
}

/**
 * Sessions belong to the workspace (docs/tasks/S1.md): run A is done — Rex worked on #a
 * himself, a new execution session (Rex-b) did #b and is archived now. Run B, led by the same
 * Ada, has Rex on #c again, himself, with run A's conversation; he is at it now.
 */
export function reuseRun(page: "b" | "a" | "rex" = "b"): { state: MockState; script: Step[]; route: string } {
  const { ada, rex, mia, sam } = team();
  const state: MockState = { ...emptyState(), plugins: plugins(), quota: quota(), usage: usage() };
  const [pa, pb, pc] = PLAN;
  const runA = run({ title: "OAuth 登录（一期）", request: REQUEST, planner: ada, worker: rex, team: [rex, mia, sam], status: "completed", plan: [pa, pb], minutesAgo: 300 });
  const runB = run({ title: "OAuth 登录（二期）", request: "接着一期：把会话存储换成 Redis。", planner: ada, worker: rex, team: [rex, mia, sam], status: "running", plan: [pc], minutesAgo: 30 });
  runB.branch = "crossagents/oauth-login-2";
  // Rex-b: the execution session started for #b, archived when it was merged.
  const rexB = { ...clone(rex, "b", "stopped", REPO), archived_at: ago(200) };
  state.agents.push(ada, rex, mia, sam, rexB);
  rex.cwd = `${REPO}/.crossagents/worktrees/c`;
  rex.status = "running";

  const card = (r: Run, t: Task, to: Agent, init: Partial<Handoff> & { minutesAgo: number }): Handoff => {
    const { minutesAgo, ...rest } = init;
    return {
      id: uid(),
      run_id: r.id,
      task_id: t.id,
      task_key: t.key,
      kind: "assign",
      from: ada.id,
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
  const ta = task(runA, { ...pa, assignee: "Rex", fresh: false }, { status: "merged", template: rex, minutesAgo: 280, summary: "回调写好了" });
  ta.worker_fresh = false;
  ta.handoffs = [card(runA, ta, rex, { minutesAgo: 280, fresh: false, outcome: "reworked" }), card(runA, ta, rex, { minutesAgo: 240, kind: "reassign", from: rex.id, from_name: rex.name, fresh: false, outcome: "delivered", reason: "返工后还是 Rex 本人接着做" })];
  const tb = task(runA, pb, { status: "merged", template: rex, minutesAgo: 280, summary: "登录页加了按钮" });
  tb.handoffs = [card(runA, tb, rexB, { minutesAgo: 280, outcome: "delivered" })];
  const tc = task(runB, { ...pc, assignee: "Rex", fresh: false }, { status: "running", assignee: rex, template: rex, minutesAgo: 20, progress: "RedisStore 写了一半" });
  tc.worker_fresh = false;
  tc.handoffs = [card(runB, tc, rex, { minutesAgo: 20, fresh: false })];
  state.runs.push({ run: runB, tasks: [tc] }, { run: runA, tasks: [ta, tb] });
  state.executions = {
    [rex.id]: { run_id: runB.id, run_title: runB.title, task_id: tc.id, task_key: tc.key, role: "worker", fresh: false, since: ago(20) },
    [ada.id]: { run_id: runB.id, run_title: runB.title, task_id: null, task_key: null, role: "lead", fresh: false, since: ago(30) },
  };
  state.transcripts.set(
    rex.id,
    transcript(rex, [
      turn({ input: "task #a：GitHub OAuth 回调", minutesAgo: 280, took: 30, items: [say("回调写好了。")] }),
      turn({ input: "task #c：把会话存储换成 Redis（接着一期）", minutesAgo: 20, took: 0, items: [say("记得一期的回调：SessionStore 还在内存里，我先换成 RedisStore。")] }),
    ], 0),
  );
  state.transcripts.set(rexB.id, transcript(rexB, [turn({ input: "task #b：登录页按钮", minutesAgo: 280, took: 20, items: [say("按钮加好了。")] })], 0));
  const route = page === "a" ? `#/runs/${runA.id}` : page === "rex" ? `#/s/${rex.id}` : `#/runs/${runB.id}`;
  // The participants start folded in the rail: these scenes open them.
  const open: Step = { at: 400, act: () => document.querySelector<HTMLButtonElement>('[data-card="run-participants"] .rail-head')?.click() };
  return { state, script: page === "rex" ? [] : [open], route };
}

/** A fake 40-hex sha, stable for a seed. */
function fakeSha(seed: string): string {
  let h = 2166136261;
  let out = "";
  while (out.length < 40) {
    for (const ch of seed + out.length) h = Math.imul(h ^ ch.charCodeAt(0), 16777619) >>> 0;
    out += h.toString(16).padStart(8, "0");
  }
  return out.slice(0, 40);
}

/**
 * The commit graph (`#/runs/<id>/graph`): main moved on after the run branched; #a and #b are
 * merged into the run branch (merge commits), #c is still at work on its own branch. Rex's #c
 * session gets a graph too (HEAD and local branches), shown in the tool column.
 */
export function graphRun(page: "run" | "session" = "run"): { state: MockState; script: Step[]; route: string } {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const byKey = (k: string) => d.tasks.find((t) => t.key === k)!;
  const [ta, tb, tc, td] = ["a", "b", "c", "d"].map(byKey);
  ta.status = "merged";
  ta.assignee = null;
  tb.status = "merged";
  tb.assignee = null;
  tc.status = "running";
  tc.progress = "RedisStore 写好了，在补本地回落";
  td.status = "pending";

  const base = d.run.base_branch || "main";
  const runBranch = d.run.branch || "crossagents/oauth-login";
  const branchOf = (k: string) => `${runBranch}/${k}`;
  const s = (name: string) => fakeSha(`oauth-${name}`);
  type C = { id: string; parents: string[]; subject: string; author: string; minutesAgo: number; diff?: string };
  const list: C[] = [
    { id: "c2", parents: ["c1"], subject: "会话存储：没有 REDIS_URL 时回落到内存", author: "Rex", minutesAgo: 2, diff: fileDiff("src/auth/session.rs", 30, ["    RedisStore::connect(url).await"], ["    match std::env::var(\"REDIS_URL\") {", "        Ok(url) => RedisStore::connect(&url).await,", "        Err(_) => Ok(Box::new(MemoryStore::default())),", "    }"], ["pub async fn store() -> Result<Box<dyn SessionStore>> {"]) },
    { id: "m2", parents: ["m1", "b2"], subject: `Merge #b: ${tb.title}`, author: "CrossAgents", minutesAgo: 6, diff: LOGIN_DIFF },
    { id: "b2", parents: ["b1"], subject: "深色模式下 OAuth 按钮改成描边样式", author: "Mia", minutesAgo: 10, diff: fileDiff("web/src/pages/login.css", 42, [], ["@media (prefers-color-scheme: dark) { .oauth button { background: none; border: 1px solid var(--line); } }"], [".or { color: var(--muted); text-align: center; }"]) },
    { id: "x1", parents: ["base"], subject: "README：修正本地启动命令", author: "Sam", minutesAgo: 15, diff: fileDiff("README.md", 8, ["cargo run -- serve"], ["cargo run -p server -- serve"], ["## 本地启动"]) },
    { id: "m1", parents: ["base", "a2"], subject: `Merge #a: ${ta.title}`, author: "CrossAgents", minutesAgo: 18, diff: OAUTH_DIFF },
    { id: "c1", parents: ["base"], subject: "SessionStore 换成 RedisStore，接口不变", author: "Rex", minutesAgo: 25, diff: SESSION_DIFF },
    { id: "b1", parents: ["base"], subject: "登录页加「用 GitHub / Google 登录」按钮", author: "Mia", minutesAgo: 30, diff: LOGIN_DIFF },
    { id: "a2", parents: ["a1"], subject: "token 过期和 provider 拒绝时给出中文提示", author: "Rex", minutesAgo: 32, diff: fileDiff("src/auth/error.rs", 0, [], ["#[derive(Debug, thiserror::Error)]", "pub enum OAuthError {", '    #[error("登录已过期，请重新登录")]', "    Expired,", "}"]) },
    { id: "a1", parents: ["base"], subject: "OAuth 回调：用授权码换 token 并建立会话", author: "Rex", minutesAgo: 38, diff: OAUTH_DIFF },
    { id: "base", parents: ["p1"], subject: "登录页支持「记住我」", author: "Sam", minutesAgo: 60 * 3 },
    { id: "p1", parents: ["p0"], subject: "密码登录加限流", author: "Sam", minutesAgo: 60 * 26 },
    { id: "p0", parents: [], subject: "初始化项目", author: "Sam", minutesAgo: 60 * 24 * 9, diff: fileDiff("README.md", 0, [], ["# shop", "", "## 本地启动", "", "cargo run -- serve"]) },
  ];

  // Long history and many local refs exercise scrolling and the collapsed legend.
  list[list.length - 1].parents = ["history-0"];
  for (let i = 0; i < 45; i++) list.push({ id: `history-${i}`, parents: i < 44 ? [`history-${i + 1}`] : [], subject: `历史提交 ${i + 1}`, author: "Sam", minutesAgo: 60 * (250 + i) });
  const extraRefs: GraphRef[] = Array.from({ length: 36 }, (_, i) => ({ name: `feature/archive-${i + 1}`, kind: "other", sha: s(`history-${i}`) }));

  const graphWith = (refs: GraphRef[], extra: Partial<CommitGraph> = {}): CommitGraph => ({
    ...extra,
    refs,
    commits: list.map((c) => ({
      sha: s(c.id),
      parents: c.parents.map(s),
      subject: c.subject,
      author: c.author,
      time: ago(c.minutesAgo),
      refs: refs.filter((r) => r.sha === s(c.id)).map((r) => r.name),
    })),
    truncated: false,
  });
  const taskRef = (t: Task, tip: string): GraphRef => ({ name: branchOf(t.key), kind: "task", sha: s(tip), task_key: t.key, task_title: t.title });
  const runGraph = graphWith(
    [
      { name: base, kind: "base", sha: s("x1") },
      { name: runBranch, kind: "run", sha: s("m2") },
      taskRef(ta, "a2"),
      taskRef(tb, "b2"),
      taskRef(tc, "c2"),
      ...extraRefs,
    ],
    { base_branch: base, run_branch: runBranch },
  );
  const graphs = new Map<string, CommitGraph>([[d.run.id, runGraph]]);
  const rexC = scene.state.agents.find((a) => a.cwd.endsWith("/worktrees/c"))!;
  graphs.set(
    rexC.id,
    graphWith([
      { name: "HEAD", kind: "head", sha: s("c2") },
      { name: branchOf("c"), kind: "other", sha: s("c2") },
      { name: runBranch, kind: "other", sha: s("m2") },
      { name: base, kind: "other", sha: s("x1") },
      ...extraRefs,
    ]),
  );
  scene.state.graphs = graphs;
  scene.state.commitDiffs = new Map(list.filter((c) => c.diff).map((c) => [s(c.id), c.diff!]));

  // Pick the merge of #b once the graph is in, so the picture shows a diff too.
  const pick = (index: number): Step => ({
    at: 700,
    act: () => (document.querySelectorAll<HTMLButtonElement>(".cgraph-row")[index] ?? null)?.click(),
  });
  if (page === "session") {
    const open: Step = { at: 400, act: () => document.querySelector<HTMLButtonElement>(".graph-toggle")?.click() };
    return { ...scene, script: [open, { ...pick(0), at: 900 }], route: `#/s/${rexC.id}` };
  }
  return { ...scene, script: [pick(1)], route: `#/runs/${d.run.id}/graph` };
}
