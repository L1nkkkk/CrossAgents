// The scenarios `preview:mock` can show (docs/tasks/V6-GUI.md §3). Each builds a fresh state,
// an event script to replay, and the page to open.

import { emptyHub } from "../views/Home.mock";
import { approvalRun, inPlaceRun, landingRun, oauthRun, rerouteRun, reviewRun, syncRun, traceRun } from "../views/Run.mock";
import { attachmentSession, chatWithRecruit, crashedSession, soloSession } from "../views/Session.mock";
import { usageScene } from "../views/Usage.mock";
import { agent } from "./fixtures";
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
  { key: "review", label: "审查任务", hint: "#c 已报告完成：整个任务的 diff 带行号，点行号写意见，可发给 worker 或打回重做", build: () => reviewRun() },
  { key: "landing", label: "落地前总 diff", hint: "所有任务都已合并、等落地：展开「查看全部改动」看 run 分支对 main 的总 diff", build: () => landingRun() },
  { key: "sync", label: "落地等冲突解决", hint: "main 动过且和 run 冲突：hub 建了 #sync-main 交给 Rex，run 页写明原因，合并后再落地", build: () => syncRun() },
  { key: "reroute", label: "额度改派", hint: "Mia 的账号额度用完，#b 改到另一个账号；任务卡片写明原因", build: () => rerouteRun() },
  { key: "approval", label: "危险操作待批准", hint: "Rex 想 git push：需要你里出现「等你批准」，允许 / 拒绝，30 分钟没回答就拒绝", build: () => approvalRun() },
  { key: "trace", label: "失败追溯", hint: "run 页「追溯」：#d 失败、首个失败标红展开；#c 途中出错后通过，灰色折叠；每一轮、每条命令和花费", build: () => traceRun() },
  { key: "inplace", label: "无 git 的 run", hint: "文件夹没有 git：run 直接改文件、一次一个任务，可一键初始化 git", build: () => inPlaceRun() },
  { key: "solo", label: "单聊", hint: "Claude 会话：折叠的工具步骤、一轮进行中、两条排队", build: () => soloSession() },
  { key: "attach", label: "消息带附件", hint: "人发的消息带了截图和日志（点缩略图看大图）；输入框可粘贴截图、拖文件或点回形针，发送前能删", build: () => attachmentSession() },
  { key: "settings-src", label: "Claude 设置源", hint: "设置 → 智能体：点 Nova 编辑，「读取 Claude 设置」三档预设（缺省 / 全部 / 都不读）；点 Vega（CodeBuddy）编辑，标签说明它不读 CLAUDE.md", build: settingSources },
  { key: "recruit", label: "对话招队友", hint: "对话里招人：一次因为用模型名当名字被拒绝（卡片写明原因），一次招到 Iris，挂在对话下面", build: () => chatWithRecruit() },
  { key: "crash", label: "进程崩溃", hint: "单聊里 Claude CLI 崩溃退出：错误条和「需要你」写明怎么退出的", build: () => crashedSession() },
  { key: "mismatch", label: "版本不匹配", hint: "hub 的接口是 v2：GUI 拒绝连接并说明原因", build: () => ({ state: { ...emptyHub(), api: "v2" }, script: [], route: "#/" }) },
  { key: "usage", label: "用量", hint: "两个账号，一个快到 5 小时限额，一个每周限额用完", build: () => ({ state: usageScene(), script: [], route: "#/usage" }) },
];

export const DEFAULT_SCENARIO = "running";

/** A Claude agent and a CodeBuddy one: the setting-sources switch reads differently for each. */
function settingSources() {
  const solo = soloSession();
  const vega = agent({ name: "vega", plugin: "codebuddy", model: "glm-5.1", role: "worker", profile: { display_name: "Vega", title: "后端", color: "teal" } });
  return { ...solo, state: { ...solo.state, agents: [...solo.state.agents, vega] }, script: [], route: "#/agents" };
}
