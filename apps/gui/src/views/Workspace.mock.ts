// Mock data for the agent workspace acceptance scenarios (docs/AGENT-WORKSPACE.md §8), for
// `preview:mock`: two projects with members (REPO and NOTES_REPO), independent conversations,
// the run edge cases §8 names, and the open items people have to answer. Scenarios in
// scenarios.ts show it; no view keeps a copy of its own.

import type { Agent } from "@bindings/Agent";
import type { HumanItem } from "@bindings/HumanItem";
import type { Run } from "@bindings/Run";
import type { RunDetail } from "@bindings/RunDetail";
import type { Task } from "@bindings/Task";
import { emptyState, type MockState } from "../dev/MockBridge";
import { NOTES_REPO, REPO, agent, ago, clone, planned, run, say, task, team, transcript, turn, uid } from "../dev/fixtures";
import { plugins } from "./Agents.mock";
import { quota, usage } from "./Usage.mock";

const HOME = "C:/Users/me/.crossagents";
const LONG_TOPIC =
  "帮我把这份三十页的中文需求文档逐条对照现有实现，找出没有覆盖到的验收条件，按优先级整理成一份可以直接发给产品同事的问题清单，每个问题都附上对应的代码位置和建议的回复，尽量不要遗漏任何一条";
const LONG_TITLE =
  "登录页加「用 GitHub 登录」和「用 Google 登录」两个按钮，密码表单保留在下面；深色与浅色主题下都检查对比度和焦点样式，窄窗口下按钮换行而不溢出";

/** An independent conversation: no project, so it lives under the default home's chats. */
function chat(name: string, topic: string, status: Agent["status"], minutesAgo: number): Agent {
  const a = agent({ name, plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status, topic, createdMinutesAgo: minutesAgo });
  a.home = `${HOME}/chats/${a.id}`;
  a.cwd = a.home;
  return a;
}

/** A run placed in a project, from a conversation (`origin`) or none (an old run). */
function placed(r: Run, repo: string, slug: string, origin: Agent | null): Run {
  return { ...r, repo, branch: `crossagents/${slug}`, origin_chat_id: origin?.id ?? null };
}

/** A question from a teammate, waiting for the person. */
function ask(from: Agent, text: string, options: string[], minutesAgo: number, runId: string | null, taskId: string | null): HumanItem {
  return {
    id: uid(),
    kind: "question",
    from: from.id,
    run_id: runId,
    task_id: taskId,
    text,
    detail: null,
    options,
    default: null,
    timeout_at: null,
    status: "open",
    answer: null,
    answered_at: null,
    action: null,
    created_at: ago(minutesAgo),
  };
}

/** The workspace as the sidebar, the overview and the edge runs see it. */
export function workspaceState(): MockState {
  const { ada, rex, mia, sam } = team();

  // REPO: the standing team, and an archived member.
  const lin = agent({ name: "lin", plugin: "codebuddy", model: "glm-5.1", role: "worker", status: "stopped", home: REPO, cwd: REPO, profile: { display_name: "Lin", title: "测试", color: "violet" } });
  lin.archived_at = ago(60 * 24 * 5);

  // NOTES_REPO: its own planner and worker.
  const nora = agent({ name: "nora", plugin: "claude", model: "claude-opus-4-1", role: "planner", home: NOTES_REPO, cwd: NOTES_REPO, profile: { display_name: "Nora", title: "笔记整理", color: "teal" } });
  const pax = agent({ name: "pax", plugin: "codex", model: "gpt-5-codex", role: "worker", home: NOTES_REPO, cwd: NOTES_REPO, profile: { display_name: "Pax", title: "Coder", color: "sky" } });

  // Independent conversations, outside any project. Lyra's is archived.
  const nova = chat("Nova", "登录页的错误提示改人话", "idle", 90);
  const iris = chat("Iris", "CLI --json 字段梳理", "running", 30);
  const atlas = chat("Atlas", LONG_TOPIC, "idle", 200);
  const orion = chat("Orion", "CSV 导出的编码问题", "idle", 300);
  const lyra = chat("Lyra", "多租户隔离方案", "idle", 60 * 24 * 4);
  lyra.archived_at = ago(60 * 24 * 2);

  // A session made from Nova's configuration: Work created it fresh, so it is not Nova.
  const novaC = clone(nova, "c", "idle", `${REPO}/.crossagents/worktrees/c`);
  const rexD = clone(rex, "d", "error", `${REPO}/.crossagents/worktrees/d`);

  // "OAuth 登录": running. Rex is #a's assignee and #b's reviewer: one row in the member list.
  const planA = [
    planned("a", "OAuth 回调与令牌交换", "实现 /auth/callback/{provider}：用授权码换 token，取用户信息，建立会话。", "rex", [], ["src/auth/**"]),
    planned("b", LONG_TITLE, "登录页加两个 OAuth 按钮，密码表单保留在下面。", "mia", [], ["web/src/pages/Login.tsx", "web/src/pages/login.css"]),
    planned("c", "会话存储迁到 Redis", "SessionStore 换成 Redis 实现，接口不变。", "rex", [], ["src/auth/session.rs", "Cargo.toml"]),
    planned("d", "旧会话数据迁移脚本", "写一次性脚本把 SQLite 里的会话迁到 Redis，可重复执行。", "rex", ["c"], ["scripts/migrate_sessions.rs"]),
  ];
  const rA = placed(
    run({ title: "OAuth 登录", request: "把登录页改成 OAuth（GitHub 和 Google），旧的密码登录保留。", planner: ada, worker: rex, team: [rex, mia, sam], status: "running", plan: planA, minutesAgo: 48 }),
    REPO,
    "oauth-login",
    nova,
  );
  // #a: Rex himself (worker_fresh false). #b: Mia's, reviewed by Rex. #c: a fresh session from Nova's configuration. #d: fresh, failed.
  const tA: Task[] = [
    { ...task(rA, planA[0], { status: "running", assignee: rex, template: rex, minutesAgo: 40, progress: "回调写好了，在补 token 过期的处理" }), worker_fresh: false },
    {
      ...task(rA, planA[1], {
        status: "reviewing",
        assignee: mia,
        template: mia,
        minutesAgo: 35,
        summary: "两个按钮加好了，深浅色都看过。",
        checks: [{ command: "npm test", exit_code: 0, passed: true, output: "Tests: 32 passed" }],
      }),
      worker_fresh: false,
      reviewer: rex.id,
    },
    task(rA, planA[2], {
      status: "passed",
      assignee: novaC,
      template: nova,
      minutesAgo: 40,
      summary: "SessionStore 换成了 RedisStore，接口没变；`REDIS_URL` 没设置时回落到 MemoryStore。",
      checks: [
        { command: "cargo test -p auth", exit_code: 0, passed: true, output: "test result: ok. 14 passed" },
        { command: "npm test", exit_code: 0, passed: true, output: "Tests: 32 passed" },
      ],
    }),
    task(rA, planA[3], { status: "failed", assignee: rexD, template: rex, minutesAgo: 30, failure: "验收失败：cargo test -p auth 有 2 个用例失败。" }),
  ];

  // "登录页改版": waiting for the person to approve the plan.
  const planB = [
    planned("a", "登录页改版：布局与按钮", "重排登录页的布局，两个 OAuth 按钮放在密码表单上方。", "mia", [], ["web/src/pages/Login.tsx"]),
    planned("b", "错误提示文案改人话", "把登录页的错误提示改成人话，中英文都要有。", "mia", ["a"], ["web/src/i18n/**"]),
  ];
  const rB = placed(
    run({ title: "登录页改版", request: "登录页改版：布局和错误提示文案。", planner: ada, worker: mia, team: [mia, sam], status: "awaiting_approval", plan: planB, minutesAgo: 12 }),
    REPO,
    "login-redesign",
    atlas,
  );

  // "密码找回": an old run from before origin conversations were recorded, landed.
  const planC = [planned("a", "密码找回邮件", "给找回密码加邮件链接，链接 30 分钟有效。", "rex", [], ["src/auth/reset.rs"])];
  const rC = placed(
    run({ title: "密码找回", request: "给找回密码加邮件链接。", planner: ada, worker: rex, team: [rex], status: "completed", plan: planC, minutesAgo: 60 * 24 * 9 }),
    REPO,
    "password-reset",
    null,
  );
  rC.landed = { mode: "merge", by: ada.name, reference: "c0ffee1", at: ago(60 * 24 * 8) };
  rC.summary = "找回邮件已经能发，链接 30 分钟有效。";
  const tC = task(rC, planC[0], { status: "merged", template: rex, minutesAgo: 60 * 24 * 9, summary: "邮件模板和链接有效期都加好了。" });

  // "多租户隔离": the origin conversation (Lyra) is archived; paused.
  const planE1 = [planned("a", "租户表与 schema 隔离", "按租户拆 schema，迁移脚本待定。", "rex", [], ["src/tenant/**"])];
  const rE1 = placed(
    run({ title: "多租户隔离", request: "评估按租户拆 schema 的方案。", planner: ada, worker: rex, team: [rex], status: "paused", plan: planE1, minutesAgo: 60 * 5 }),
    REPO,
    "tenant-isolation",
    lyra,
  );

  // "登录限流": planning, no plan submitted yet.
  const rE2 = placed(
    run({ title: "登录限流", request: "给登录接口加限流，同一账号连续失败 5 次锁 10 分钟。", planner: ada, worker: rex, team: [rex, mia], status: "planning", plan: [], minutesAgo: 3 }),
    REPO,
    "login-rate-limit",
    iris,
  );

  // NOTES_REPO: a failed run, and a completed one that has not landed.
  const planE3 = [planned("a", "CSV 导出加 UTF-8 BOM", "导出的 CSV 加 BOM，Excel 打开不乱码。", "pax", [], ["src/export/**"])];
  const rE3 = placed(
    run({ title: "导出 CSV 的编码", request: "CSV 导出在 Excel 里中文乱码。", planner: nora, worker: pax, team: [pax], status: "failed", plan: planE3, minutesAgo: 180 }),
    NOTES_REPO,
    "csv-encoding",
    orion,
  );
  rE3.note = "计划被拒：任务 b 没有写验收命令，run 停在规划阶段。";

  const planE4 = [planned("a", "重建笔记索引", "按标题和标签重建全文索引。", "pax", [], ["index/**"])];
  const rE4 = placed(
    run({ title: "笔记索引重建", request: "全文索引很久没重建了。", planner: nora, worker: pax, team: [pax], status: "completed", plan: planE4, minutesAgo: 600 }),
    NOTES_REPO,
    "note-index",
    iris,
  );
  rE4.summary = "索引已重建，改动在 run 分支上，等你确认后再落地到 main。";
  const tE4 = task(rE4, planE4[0], { status: "passed", template: pax, minutesAgo: 600, summary: "索引重建完成，检查通过。" });

  const runs: RunDetail[] = [
    { run: rA, tasks: tA },
    { run: rB, tasks: [] },
    { run: rC, tasks: [tC] },
    { run: rE1, tasks: [] },
    { run: rE2, tasks: [] },
    { run: rE3, tasks: [] },
    { run: rE4, tasks: [tE4] },
  ];

  // Mia has two open questions: both must be found, and opening the session must not clear them.
  const questions = [
    // The two oldest, so they head the list: the sidebar's first rows exercise the question route.
    ask(mia, "按钮颜色要不要跟随系统深色设置？", ["跟随系统", "固定深色"], 420, rA.id, tA[1].id),
    ask(mia, "旧的验证码登录入口保留吗？", ["保留", "去掉"], 400, null, null),
  ];

  return {
    ...emptyState(),
    agents: [ada, rex, mia, sam, lin, nora, pax, nova, iris, atlas, orion, lyra, novaC, rexD],
    runs,
    transcripts: new Map([
      [nova.id, transcript(nova, [turn({ input: "登录页的错误提示太生硬，能改成人话吗？", minutesAgo: 90, took: 2, items: [say("可以。我先看一下现在的错误文案在哪里，再给你几版。")] })])],
      [atlas.id, transcript(atlas, [turn({ input: LONG_TOPIC, minutesAgo: 200, took: 3, items: [say("好，我先通读一遍文档，再逐条对照代码。")] })])],
    ]),
    questions,
    plugins: plugins(),
    quota: quota(),
    usage: usage(),
  };
}
