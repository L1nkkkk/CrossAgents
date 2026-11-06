// Mock data for a work group (docs/tasks/G1.md): one conversation with Ada across three rounds,
// each round a run of its own. The first landed, the second did not and the third is stacked on
// it, so the 各轮 pane shows all three outcomes; the conversation never changes page.

import type { Agent } from "@bindings/Agent";
import type { Landing } from "@bindings/Landing";
import type { PlannedTask } from "@bindings/PlannedTask";
import type { Run } from "@bindings/Run";
import type { RunStatus } from "@bindings/RunStatus";
import type { Work } from "@bindings/Work";
import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { REPO, ago, planned, run, say, task, team, think, tool, transcript, turn, uid } from "../dev/fixtures";
import { plugins } from "./Agents.mock";
import { quota, usage } from "./Usage.mock";

const DAY = 60 * 24;

type RoundInit = {
  title: string;
  request: string;
  minutesAgo: number;
  status: RunStatus;
  branch: string;
  base: string;
  plan: PlannedTask[];
  landed?: Landing;
  summary?: string;
};

/** A round of the same work: a run whose `work_id` is the group's, in a worktree of its own. */
function round(work: Work, planner: Agent, init: RoundInit, worker: Agent, members: Agent[]): Run {
  const base = run({
    title: init.title,
    request: init.request,
    planner,
    worker,
    team: members,
    status: init.status,
    plan: init.plan,
    minutesAgo: init.minutesAgo,
  });
  return {
    ...base,
    // The lead is the group's own session, not a copy per round.
    planner: work.lead,
    work_id: work.id,
    branch: init.branch,
    base_branch: init.base,
    landed: init.landed ?? null,
    summary: init.summary ?? null,
    mode: { lead: "worktree", vcs: "git", owner: "hub", retain: "keep" },
    created_at: ago(init.minutesAgo + 1),
    updated_at: ago(Math.max(0, init.minutesAgo - 45)),
  };
}

/** Three rounds of 登录与会话: landed, landed-nothing-yet, and one still running. */
export function workRounds(): { state: MockState; script: Step[]; route: string } {
  const { ada, rex, mia, sam } = team();
  const work: Work = {
    id: uid(),
    title: "登录与会话",
    repo: REPO,
    lead: ada.id,
    created_at: ago(DAY * 2 + 10),
    updated_at: ago(20),
  };

  const p1 = planned("a", "OAuth 回调与令牌交换", "实现 /auth/callback/{provider}：用授权码换 token，取用户信息，建立会话。", "rex", [], ["src/auth/**"]);
  const p2 = planned("a", "会话存储换 Redis", "SessionStore 换成 Redis 实现，接口不变；本地没有 Redis 时回落到内存。", "rex", [], ["src/auth/session.rs", "Cargo.toml"]);
  const p3 = planned("a", "深色模式的按钮对比度", "登录页在深色模式下按钮和背景对比不足，调整配色；OAuth 按钮的间距也收紧一点。", "mia", [], ["web/src/pages/login.css"]);

  const first = round(
    work,
    ada,
    {
      title: "OAuth 登录",
      request: "把登录页改成 OAuth（GitHub 和 Google），旧的密码登录保留。会话存储换成 Redis。",
      minutesAgo: DAY * 2,
      status: "completed",
      branch: "crossagents/oauth-login",
      base: "main",
      plan: [p1],
      summary: "OAuth 两个 provider 都通了，密码登录保留；会话存储还没动，留到下一轮。",
      landed: { mode: "merge", by: "Ada", reference: "9f2c1ab", at: ago(DAY * 2 - 50) },
    },
    rex,
    [rex, mia, sam],
  );
  const second = round(
    work,
    ada,
    {
      title: "会话迁到 Redis",
      request: "接着干：把会话存储从 SQLite 换到 Redis，本地没有 Redis 时回落到内存。",
      minutesAgo: DAY,
      status: "completed",
      branch: "crossagents/oauth-login-round-sessions",
      // Stacked on the first round's branch, which is why landing the third takes this one along.
      base: first.branch,
      plan: [p2],
      summary: "SessionStore 换成了 RedisStore，接口没变；`REDIS_URL` 没设置时回落到 MemoryStore。还没落地，下一轮接着它。",
    },
    rex,
    [rex, mia, sam],
  );
  const third = round(
    work,
    ada,
    {
      title: "深色模式对比度",
      request: "登录页在深色模式下按钮看不清，修一下对比度。顺便把 OAuth 按钮的间距调一下。",
      minutesAgo: 38,
      status: "running",
      branch: "crossagents/oauth-login-round-dark",
      base: second.branch,
      plan: [p3],
    },
    mia,
    [rex, mia, sam],
  );

  const state: MockState = { ...emptyState(), plugins: plugins(), quota: quota(), usage: usage() };
  state.works = [work];
  state.agents.push(ada, rex, mia, sam);
  ada.status = "running";

  const t1 = turn({
    input: first.request,
    minutesAgo: DAY * 2,
    took: 9,
    items: [
      think("先看现在的登录流程和会话存储在哪，再决定这一轮做到哪。"),
      tool("Read src/auth/mod.rs", "read"),
      tool("Read src/auth/session.rs", "read"),
      tool("Read web/src/pages/Login.tsx", "read"),
      say("回调、按钮、会话存储三块里，这一轮先做回调：`/auth/callback/{provider}` 两个 provider 共用一条路径，用户信息落在会话里。"),
      tool("plan_submit", "other", "completed"),
      say("做完了：GitHub 和 Google 都通，密码登录保留。会话存储留到下一轮。"),
    ],
  });
  const t2 = turn({
    input: second.request,
    minutesAgo: DAY,
    took: 7,
    items: [
      think("第二轮从第一轮的分支上接着走，不再从 main 开始。"),
      tool("Read src/auth/session.rs", "read"),
      tool("Edit src/auth/session.rs", "edit"),
      tool("Edit Cargo.toml", "edit"),
      say("SessionStore 换成了 RedisStore，接口没变。`REDIS_URL` 没设置时回落到 MemoryStore，本地开发不用装 Redis。"),
      say("这一轮没落地，改动留在自己的分支上；下一轮直接叠在上面。"),
    ],
  });
  const t3 = turn({
    input: third.request,
    minutesAgo: 38,
    running: true,
    items: [
      think("深色模式的问题是按钮底色和页面底色太接近，先把颜色变量找出来。"),
      tool("Read web/src/pages/login.css", "read"),
      tool("Edit web/src/pages/login.css", "edit", "in_progress"),
    ],
  });

  state.transcripts.set(ada.id, transcript(ada, [t1, t2, t3], 52_000));

  state.runs = [
    {
      run: first,
      tasks: [
        task(first, p1, {
          status: "merged",
          assignee: rex,
          template: rex,
          minutesAgo: DAY * 2 - 5,
          summary: "两个 provider 共用 /auth/callback/{provider}；令牌过期交给 SessionStore。",
          checks: [
            { command: "cargo test -p auth", exit_code: 0, passed: true, output: "test result: ok. 14 passed" },
            { command: "npm test", exit_code: 0, passed: true, output: "Tests: 32 passed" },
          ],
        }),
      ],
    },
    {
      run: second,
      tasks: [
        task(second, p2, {
          status: "merged",
          assignee: rex,
          template: rex,
          minutesAgo: DAY - 6,
          summary: "RedisStore 实现 SessionStore；没有 REDIS_URL 时回落 MemoryStore。",
          checks: [{ command: "cargo test -p auth", exit_code: 0, passed: true, output: "test result: ok. 16 passed" }],
        }),
      ],
    },
    {
      run: third,
      tasks: [
        task(third, p3, {
          status: "running",
          assignee: mia,
          template: mia,
          minutesAgo: 30,
          progress: "颜色变量改好了，在调按钮间距",
        }),
      ],
    },
  ];
  state.runCost = {
    [first.id]: { run_id: first.id, slices: [], tokens: 184_000, cost_usd: 1.42, notes: [] },
    [second.id]: { run_id: second.id, slices: [], tokens: 96_000, cost_usd: 0.63, notes: [] },
    [third.id]: { run_id: third.id, slices: [], tokens: 41_000, cost_usd: 0.21, notes: [] },
  };

  return { state, route: `#/work/${work.id}`, script: [] };
}
