// The scenarios `preview:mock` can show (docs/tasks/V6-GUI.md §3). Each builds a fresh state,
// an event script to replay, and the page to open.

import { emptyHub } from "../views/Home.mock";
import { oauthRun } from "../views/Run.mock";
import { chatWithRecruit, soloSession } from "../views/Session.mock";
import { usageScene } from "../views/Usage.mock";
import type { MockState, Step } from "./MockBridge";

export type Scenario = {
  key: string;
  label: string;
  hint: string;
  build: () => { state: MockState; script: Step[]; route: string };
};

export const SCENARIOS: Scenario[] = [
  { key: "empty", label: "空 hub", hint: "第一次打开：没有队友、没有 run", build: () => ({ state: emptyHub(), script: [], route: "#/" }) },
  { key: "plan", label: "计划待审批", hint: "Planner + 两个 Worker，计划等你批", build: () => oauthRun("plan") },
  { key: "running", label: "run 进行中", hint: "两个任务在跑、一个待检查、一个失败；Planner 对话里有编排消息和任务卡片", build: () => oauthRun("running") },
  { key: "solo", label: "单聊", hint: "Claude 会话：折叠的工具步骤、一轮进行中、两条排队", build: () => soloSession() },
  { key: "recruit", label: "对话招队友", hint: "对话里招人：一次因为用模型名当名字被拒绝（卡片写明原因），一次招到 Iris，挂在对话下面", build: () => chatWithRecruit() },
  { key: "usage", label: "用量", hint: "两个账号，一个快到 5 小时限额，一个每周限额用完", build: () => ({ state: usageScene(), script: [], route: "#/usage" }) },
];

export const DEFAULT_SCENARIO = "running";
