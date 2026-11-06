// Mock data for the troubleshooting records (docs/tasks/LESSONS.md): a project's lessons with
// different outcomes and statuses (one up for review, one superseded), a run with three recorded
// problems (one became a lesson, one kept, one unsorted) and a failed task carrying
// related_lessons. The script records a fourth problem in the run.

import { emptyState, type MockState, type Step } from "../../dev/MockBridge";
import { ago, planned, REPO, run, task, team } from "../../dev/fixtures";
import type { Lesson, RunIssue } from "../../lessons/lessons";
import { selectWorkspace, wsKey } from "../../workspace";
import { quota } from "../Usage.mock";

const DAY = 60 * 24;
const PROJECT = wsKey(REPO);

type Init = Pick<Lesson, "id" | "title" | "symptom" | "keywords" | "conditions" | "resolution" | "outcome"> & Partial<Lesson>;

function lesson(runId: string, i: Init, daysOld: number, tries: Array<[Lesson["outcome"], string, number]>): Lesson {
  return {
    project_id: PROJECT,
    status: "active",
    superseded_by: null,
    sources: [{ run_id: runId, key: `issue/${i.id}/1` }],
    evidence: tries.map(([outcome, note, d], n) => ({
      run_id: runId,
      agent: n % 2 ? "mia" : "rex",
      node: "linkk-pc",
      at: ago(d * DAY),
      commit: n ? "4be19d2" : "9f2c1ab",
      dirty: n === 1 ? true : false,
      outcome,
      note,
    })),
    created_at: ago(daysOld * DAY),
    updated_at: ago((tries.at(-1)?.[2] ?? daysOld) * DAY),
    files: [],
    ...i,
  };
}

function lessons(runId: string): Lesson[] {
  return [
    lesson(
      runId,
      {
        id: "01a1c0d1-lnk1-7000-8000-000000000001",
        title: "Windows 上 cargo 链接报 LNK1104",
        symptom: "error: linking with `link.exe` failed: LINK : fatal error LNK1104: cannot open file 'target\\debug\\crossagents.exe'",
        keywords: ["LNK1104", "link.exe", "cargo"],
        conditions: "Windows 11，hub 还在运行时重新编译",
        resolution: "先停掉正在运行的 crossagents.exe（任务管理器或 `crossagents hub stop`），再编译。",
        outcome: "resolved",
      },
      12,
      [["resolved", "写下记录时", 12], ["resolved", "停掉 hub 后编过", 3]],
    ),
    lesson(
      runId,
      {
        id: "01a1c0d2-npm2-7000-8000-000000000002",
        title: "npm ci 报 EPERM 无法删除 node_modules",
        symptom: "npm ERR! code EPERM\nnpm ERR! syscall unlink",
        keywords: ["EPERM", "npm ci", "node_modules"],
        conditions: "Windows，GUI 开发服务器或编辑器占用文件",
        resolution: "关掉 vite 开发服务器后重试；仍不行就重启编辑器。",
        outcome: "workaround",
        files: ["apps/gui/package-lock.json"],
      },
      20,
      [["workaround", "写下记录时", 20], ["unresolved", "关了 vite 还是失败", 2]],
    ),
    lesson(
      runId,
      {
        id: "01a1c0d3-sql3-7000-8000-000000000003",
        title: "sqlite database is locked",
        symptom: "Error: database is locked (code 5, SQLITE_BUSY)",
        keywords: ["SQLITE_BUSY", "database is locked"],
        conditions: "两个测试同时打开同一个 hub.db",
        resolution: "测试各自用临时目录。",
        outcome: "resolved",
        status: "superseded",
        superseded_by: "01a1c0d4-sql4-7000-8000-000000000004",
      },
      40,
      [["resolved", "写下记录时", 40]],
    ),
    lesson(
      runId,
      {
        id: "01a1c0d4-sql4-7000-8000-000000000004",
        title: "sqlite SQLITE_BUSY：设 busy_timeout",
        symptom: "Error: database is locked (code 5, SQLITE_BUSY)",
        keywords: ["SQLITE_BUSY", "busy_timeout"],
        conditions: "多个连接写同一个 hub.db",
        resolution: "连接打开后执行 `PRAGMA busy_timeout = 5000`；测试仍各自用临时目录。",
        outcome: "unresolved",
        status: "review",
        files: ["crates/store-sqlite/src/lib.rs"],
      },
      8,
      [["resolved", "写下记录时", 8], ["unresolved", "CI 上还是偶发", 1]],
    ),
  ];
}

/** The lessons page, and a run with its recorded problems and a failed task. */
export function lessonsScene(): { state: MockState; script: Step[]; runId: string; failedTask: string } {
  const { ada, rex, mia, sam } = team();
  const plan = [
    planned("build", "Windows 构建", "让 Windows 上的 release 构建通过", rex.name),
    planned("gui", "GUI 依赖", "升级 GUI 依赖", mia.name),
  ];
  const r = run({ title: "修 Windows 构建", request: "Windows 上 release 构建失败，修好它", planner: ada, worker: rex, team: [ada, rex, mia, sam], status: "running", plan, minutesAgo: 90 });
  const list = lessons(r.id);
  const failed = task(r, plan[0], {
    status: "failed",
    assignee: rex,
    minutesAgo: 60,
    failure: "cargo build --release 失败：LINK : fatal error LNK1104: cannot open file 'target\\release\\crossagents.exe'",
  });
  failed.report.related_lessons = [{ id: list[0].id, title: list[0].title, matched: ["LNK1104", "link.exe"] }];
  const done = task(r, plan[1], { status: "merged", assignee: mia, minutesAgo: 50, summary: "升级了 vite 与 typescript" });
  const issue = (key: string, author: string, text: string, min: number, triage?: RunIssue["triage"]): RunIssue => ({ key, author, text, updated_at: ago(min), ...(triage ? { triage } : {}) });
  const issues: RunIssue[] = [
    issue(`issue/${failed.id}/a1`, rex.id, "问题：cargo 链接报 LNK1104\n环境：Windows 11\n处理：停掉 hub 后重编\n结果：已解决", 55, {
      issue: { run_id: r.id, key: `issue/${failed.id}/a1` },
      by: ada.id,
      at: ago(40),
      result: "lesson",
      lesson_id: list[0].id,
    }),
    issue(`issue/${done.id}/b2`, mia.id, "问题：npm 镜像超时一次\n处理：重试\n结果：已解决", 45, {
      issue: { run_id: r.id, key: `issue/${done.id}/b2` },
      by: ada.id,
      at: ago(30),
      result: "kept",
      why: "网络抖动，重试即可，不值得记",
    }),
    issue(`issue/${done.id}/c3`, mia.id, "问题：npm ci 报 EPERM: operation not permitted, unlink\n环境：Windows，vite 在跑\n尝试：直接重试\n处理：关掉 vite 后通过\n结果：临时绕过", 20),
  ];
  const state: MockState = {
    ...emptyState(),
    agents: [ada, rex, mia, sam],
    quota: quota(),
    runs: [{ run: r, tasks: [failed, done] }],
    lessons: {
      lessons: list,
      issues: { [r.id]: issues },
      // Review clues: the files moved on after the record's last check (LESSONS §4.4).
      changed: {
        [list[1].id]: ["apps/gui/package-lock.json"],
        [list[3].id]: ["crates/store-sqlite/src/lib.rs"],
      },
      // A checkout whose directory now points at another remote: the person decides.
      projects: [
        {
          id: `${PROJECT}-elsewhere`,
          name: "CrossAgents（另一个目录）",
          remotes: ["github.com/L1nkkkk/CrossAgents"],
          paths: [{ node: "local", path: `${REPO}-fork` }],
          unconfirmed_remote: "github.com/someone/CrossAgents",
        },
      ],
    },
  };
  selectWorkspace(PROJECT);
  const key = `issue/${failed.id}/d4`;
  const script: Step[] = [
    {
      at: 2500,
      act: (s) => {
        s.lessons!.issues[r.id].push(issue(key, rex.id, "问题：release 构建 SQLITE_BUSY: database is locked\n处理：设 busy_timeout\n结果：未解决", 0));
      },
      event: { type: "issue.recorded", data: { run_id: r.id, key, author: rex.id } },
    },
  ];
  return { state, script, runId: r.id, failedTask: failed.id };
}
