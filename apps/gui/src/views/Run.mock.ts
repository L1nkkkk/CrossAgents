// Mock data for a run (preview:mock): Ada plans "OAuth 登录" for Rex and Mia. Two phases:
// `plan` — the plan waits for approval; `running` — two tasks run, one waits to be checked, one
// failed, and Ada's conversation carries the orchestrator's messages and task cards.

import type { HumanItem } from "@bindings/HumanItem";
import type { RunDetail } from "@bindings/RunDetail";
import type { RunTrace } from "@bindings/RunTrace";
import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { REPO, ago, clone, fileDiff, planned, run, say, task, team, think, tool, transcript, turn, uid } from "../dev/fixtures";
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

    state.transcripts.set(ada.id, transcript(ada, adaTurns, 92_000));
    return { state, script: runningScript(rexA.id, a2.turn.message_id, editId, ta.id, r.id, miaB.id), route: `#/s/${ada.name}` };
  }

  state.runs.push({ run: r, tasks: [] });
  state.transcripts.set(ada.id, transcript(ada, adaTurns, 52_000));
  return { state, script: [], route: `#/s/${ada.name}` };
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
  return { ...scene, route: `#/runs/${d.run.id}` };
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
  const tasks: RunTrace["tasks"] = d.tasks.map((t) => ({
    task_id: t.id,
    key: t.key,
    title: t.title,
    status: t.status,
    failure: t.report.failure,
    lane: t.key === "d" ? failedLane : lane(t.assignee!, t.key === "a" ? 2 : 1, t.key === "a" ? 0.34 : 0.12, [cmd(t.assignee!, "cargo test -p auth", 0, 30)]),
    checks: t.report.checks.length ? [{ at: ago(27), passed: t.report.checks.every((c) => c.passed), checks: t.report.checks }] : [],
    rerouted: [],
    first_failure: t.key === "d" ? "`cargo test -p auth` exited 101" : null,
  }));
  scene.state.traces.set(d.run.id, { run_id: d.run.id, lead: lane(d.run.planner, 3, 0.41), tasks, spent: { tokens: 120_000, cost_usd: 1.32 } });
  return { ...scene, script: [], route: `#/runs/${d.run.id}` };
}
