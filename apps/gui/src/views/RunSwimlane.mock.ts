// Mock data for the run page's swimlane (§十三): three workers on four lines — Rex's #a with a
// failed test, a retry, a model switch and a compaction; Mia's #b, which she asked the person
// about and which then forked to Kai when her quota ran out (改派); Sam's #c waiting on the
// person (等你). `swimlane` in dev/scenarios.ts opens it while the run goes on;
// `swimlane-landed` once every task merged and the run landed.

import type { Agent } from "@bindings/Agent";
import type { CheckResult } from "@bindings/CheckResult";
import type { Handoff } from "@bindings/Handoff";
import type { LaneMark } from "@bindings/LaneMark";
import type { LaneTrace } from "@bindings/LaneTrace";
import type { RunTrace } from "@bindings/RunTrace";
import type { StatusTrace } from "@bindings/StatusTrace";
import type { Task } from "@bindings/Task";
import type { TaskStatus } from "@bindings/TaskStatus";
import type { TaskTrace } from "@bindings/TaskTrace";
import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { REPO, agent, ago, clone, planned, run, task, team, transcript, turn, say, uid } from "../dev/fixtures";
import { plugins } from "./Agents.mock";
import { quota, usage } from "./Usage.mock";

const REQUEST = "把登录页改成 OAuth（GitHub 和 Google），旧的密码登录保留；会话存储换成 Redis。";

const PLAN = [
  planned("a", "OAuth 回调与令牌交换", "实现 /auth/callback/{provider}：用授权码换 token，取用户信息，建立会话。", "rex", [], ["src/auth/**"]),
  planned("b", "登录页加 OAuth 按钮", "登录页加「用 GitHub 登录」「用 Google 登录」，密码表单保留在下面。", "mia", [], ["web/src/pages/Login.tsx"]),
  planned("c", "会话存储迁到 Redis", "SessionStore 换成 Redis 实现，接口不变；没有 REDIS_URL 时回落到内存。", "sam", ["a"], ["src/auth/session.rs"]),
];

const pass = (command: string): CheckResult => ({ command, exit_code: 0, passed: true, output: "test result: ok. 18 passed; 0 failed" });
const fail = (command: string): CheckResult => ({ command, exit_code: 101, passed: false, output: "failures:\n    oauth::tests::expired_code\n\ntest result: FAILED. 17 passed; 1 failed" });

export function swimlaneScene(landed = false): { state: MockState; script: Step[]; route: string } {
  const { ada, rex, mia, sam } = team();
  const r = run({ title: "登录改造 · 泳道", request: REQUEST, planner: ada, worker: rex, team: [rex, mia, sam], status: landed ? "completed" : "running", plan: PLAN, minutesAgo: 55 });
  const state: MockState = { ...emptyState(), plugins: plugins(), quota: quota(), usage: usage() };
  const kai = agent({ name: "kai", plugin: "claude", model: "claude-opus-4-1", role: "worker", profile: { display_name: "Kai", title: "资深工程师", color: "emerald" } });
  const wt = (k: string) => `${REPO}/.crossagents/worktrees/${k}`;
  const rexA = clone(rex, "a", "idle", wt("a"));
  const miaB = clone(mia, "b", "idle", wt("b"));
  const kaiB = clone(kai, "b", landed ? "idle" : "running", wt("b"));
  const samC = clone(sam, "c", landed ? "idle" : "running", wt("c"));
  state.agents.push(ada, rex, mia, sam, kai, rexA, miaB, kaiB, samC);

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
  const moves = (who: Agent, steps: [number, TaskStatus, TaskStatus][]): StatusTrace[] => steps.map(([m, from, to]) => ({ at: ago(m), from, to, agent: who.id }));
  const mark = (who: Agent, m: number, kind: LaneMark["kind"], text: string): LaneMark => ({ at: ago(m), agent: who.id, kind, text });
  const lane = (agents: Agent[], usd: number, marks: LaneMark[]): LaneTrace => ({ agents: agents.map((a) => a.id), turns: [], commands: [], spent: { tokens: Math.round(usd * 90_000), cost_usd: usd }, marks });

  // #a, Rex: a model switch, a failed test and a retry, a compaction, then through.
  const ta = task(r, PLAN[0], { status: "merged", assignee: rexA, template: rex, minutesAgo: 50, summary: "回调、token 交换和会话建立都好了。", checks: [pass("cargo test -p auth")] });
  ta.handoffs = [card(ta, { kind: "assign", to: rexA, template: "rex", minutesAgo: 50, outcome: "delivered", summary: "两家 provider 都通了。" })];

  // #b, Mia: she asks the person, gets the answer, passes; then quota moves the rework to Kai.
  const tb = task(r, PLAN[1], { status: landed ? "merged" : "verifying", assignee: kaiB, template: kai, minutesAgo: 48, progress: "按钮对齐好了，跑验收中" });
  tb.handoffs = [
    card(tb, { kind: "assign", to: miaB, template: "mia", minutesAgo: 48, outcome: "reassigned", summary: "两个按钮加好了，深色模式是描边样式。" }),
    card(tb, {
      kind: "reroute",
      to: kaiB,
      template: "kai",
      from: miaB.id,
      from_name: mia.name,
      minutesAgo: 22,
      reason: "Mia 的账号额度用完了，返工改派给 Kai",
      summary: "按钮的位置还要和「或者用密码」那行对齐。",
      files: ["web/src/pages/Login.tsx"],
      context_tokens: 64_000,
      outcome: landed ? "delivered" : null,
    }),
  ];

  // #c, Sam: after #a merged; it waits on the person (or, once landed, was answered).
  const tc = task(r, PLAN[2], { status: landed ? "merged" : "waiting_answer", assignee: samC, template: sam, minutesAgo: 25, progress: "Redis 实现好了，等你定过期会话怎么迁" });
  tc.handoffs = [card(tc, { kind: "assign", to: samC, template: "sam", minutesAgo: 25, outcome: landed ? "delivered" : null })];
  if (!landed) tc.report = { ...tc.report, question: "已过期的旧会话要一起迁到 Redis 吗？还是直接丢掉？" };

  state.runs.push({ run: r, tasks: [ta, tb, tc] });

  const base = (t: Task) => ({ task_id: t.id, key: t.key, title: t.title, status: t.status, failure: null, rerouted: [], dispatches: [], first_failure: null });
  const traceA: TaskTrace = {
    ...base(ta),
    lane: lane([rexA], 0.62, [mark(rexA, 46, "model_changed", "sonnet → opus (high)"), mark(rexA, 35, "compacted", "172k → 41k")]),
    checks: [
      { at: ago(39), passed: false, checks: [fail("cargo test -p auth")] },
      { at: ago(30), passed: true, checks: [pass("cargo test -p auth")] },
    ],
    handoffs: ta.handoffs,
    first_failure: "`cargo test -p auth` exited 101",
    statuses: moves(rexA, [
      [50, "pending", "running"],
      [40, "running", "verifying"],
      [38, "verifying", "retrying"],
      [37, "retrying", "running"],
      [31, "running", "verifying"],
      [29, "verifying", "passed"],
      [28, "passed", "merged"],
    ]),
  };
  const traceB: TaskTrace = {
    ...base(tb),
    lane: lane([miaB, kaiB], 0.48, [mark(miaB, 44, "asked", "深色模式下按钮用描边还是实心？"), mark(miaB, 42, "answered", "描边，和密码表单一致"), mark(kaiB, 20, "model_changed", "默认 → opus")]),
    checks: [{ at: ago(32), passed: true, checks: [pass("npm test -- Login")] }, ...(landed ? [{ at: ago(11), passed: true, checks: [pass("npm test -- Login")] }] : [])],
    handoffs: tb.handoffs,
    rerouted: ["Mia 的账号额度用完了"],
    statuses: [
      ...moves(miaB, [
        [48, "pending", "running"],
        [33, "running", "verifying"],
      ]),
      ...moves(kaiB, landed ? [[13, "running", "verifying"], [9, "passed", "merged"]] : [[3, "running", "verifying"]]),
    ],
  };
  const traceC: TaskTrace = {
    ...base(tc),
    lane: lane([samC], 0.21, [mark(samC, 6, "asked", "已过期的旧会话要一起迁到 Redis 吗？"), ...(landed ? [mark(samC, 5, "answered", "一起迁过去")] : [])]),
    checks: landed ? [{ at: ago(7 - 3), passed: true, checks: [pass("cargo test -p auth")] }] : [],
    handoffs: tc.handoffs,
    statuses: moves(samC, landed ? [[25, "pending", "running"], [4.5, "running", "verifying"], [3.5, "passed", "merged"]] : [[25, "pending", "running"], [6, "running", "waiting_answer"]]),
  };
  const trace: RunTrace = {
    run_id: r.id,
    lead: lane([ada], 0.4, []),
    tasks: [traceA, traceB, traceC],
    spent: { tokens: 150_000, cost_usd: 1.71 },
    ...(landed ? { landed: { mode: "merge", by: "person", reference: "9f3c2ab", at: ago(2) } } : {}),
  };
  state.traces.set(r.id, trace);
  if (landed) r.landed = trace.landed!;

  state.transcripts.set(
    ada.id,
    transcript(ada, [turn({ input: REQUEST, minutesAgo: 55, took: 4, items: [say(landed ? "三条线都合入了，已落地到 main。" : "三条线都派出去了：#a 已合入；#b 改派给 Kai 在跑验收；#c 在等你定过期会话怎么迁。")] })]),
  );

  // The rail scrolls to the swimlane, so the picture shows it whole.
  const show: Step = { at: 600, act: () => document.querySelector<HTMLElement>('[data-card="run-swimlane"]')?.scrollIntoView({ block: "start" }) };
  return { state, script: [show], route: `#/runs/${r.id}` };
}
