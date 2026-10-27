// The scenarios `preview:mock` can show (docs/tasks/V6-GUI.md §3). Each builds a fresh state,
// an event script to replay, and the page to open.

import { emptyHub } from "../views/Home.mock";
import { approvalRun, deleteRun, handoffRun, inPlaceRun, landingRun, oauthRun, rerouteRun, reuseRun, reviewRun, syncRun, traceRun } from "../views/Run.mock";
import { attachmentSession, chatWithRecruit, crashedSession, soloSession } from "../views/Session.mock";
import { codeLost, contextLost, resumeScene } from "../views/Continuity.mock";
import { usageScene } from "../views/Usage.mock";
import { agent, say, think, turn } from "./fixtures";
import { openFile } from "../FileViewer";
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
  { key: "handoff", label: "交接卡", hint: "每个任务下一条交接链：#a 派活、#c 返工后改派给 Mia、#d 升级给 Kai 还有问题没答、#b 额度改派（稍后到）；点节点展开卡片，Ada 的对话里是编排行", build: () => handoffRun() },
  { key: "reuse", label: "会话跨 run 复用", hint: "Rex 在一期本人做了 #a，二期又被指定用本人做 #c、此刻正在执行；一期 #b 的执行会话 Rex-b 已归档，可恢复。看 run 页「参与者」和 Rex 的会话页", build: () => reuseRun() },
  { key: "reuse-done", label: "参与过的 run", hint: "一期 run 页「参与者」：#a 是 Rex 本人（被打回、交付），#b 的执行会话 Rex-b 已归档，可以点开或恢复", build: () => reuseRun("a") },
  { key: "reuse-busy", label: "会话正在执行", hint: "Rex 的会话页：顶部写明正在执行二期 · 任务 #c（本人），带中断按钮", build: () => reuseRun("rex") },
  { key: "approval", label: "危险操作待批准", hint: "Rex 想 git push：需要你里出现「等你批准」，允许 / 拒绝，30 分钟没回答就拒绝", build: () => approvalRun() },
  { key: "trace", label: "失败追溯", hint: "run 页「追溯」：#d 失败、首个失败标红展开；#c 途中出错后通过，灰色折叠；每一轮、每条命令和花费", build: () => traceRun() },
  { key: "inplace", label: "无 git 的 run", hint: "文件夹没有 git：run 直接改文件、一次一个任务，可一键初始化 git", build: () => inPlaceRun() },
  { key: "workcard", label: "会话提议 Work", hint: "Nova 在单聊里用 work_start 提议到独立分支上做：卡片可展开交给 Work 的说明，开始 / 不用", build: () => workProposal() },
  { key: "delete-landed", label: "删除已落地的 run", hint: "删除确认框：主导和它的对话一起删，工作区和分支，已落地所以分支默认勾选", build: () => deleteRun("landed") },
  { key: "delete-unlanded", label: "删除未落地的 run", hint: "未落地：分支默认不勾，写明「未落地的改动会丢失」", build: () => deleteRun("unlanded") },
  { key: "delete-dirty", label: "删除时工作区有改动", hint: "点「删除」后 hub 返回 409 并列出没提交的文件，可以「仍然删除」", build: () => deleteRun("dirty") },
  { key: "files", label: "文件链接与查看器", hint: "回复里的路径（确认存在的才成链接）可点开只读查看器：左侧懒加载目录树，右侧行号、高亮、跳到第 12 行", build: () => filesScene() },
  { key: "solo", label: "单聊", hint: "Claude 会话：折叠的工具步骤、一轮进行中、两条排队", build: () => soloSession() },
  { key: "attach", label: "消息带附件", hint: "人发的消息带了截图和日志（点缩略图看大图）；输入框可粘贴截图、拖文件或点回形针，发送前能删", build: () => attachmentSession() },
  { key: "settings-src", label: "Claude 设置源", hint: "设置 → 智能体：点 Nova 编辑，「读取 Claude 设置」三档预设（缺省 / 全部 / 都不读）；点 Vega（CodeBuddy）编辑，标签说明它不读 CLAUDE.md", build: settingSources },
  { key: "recruit", label: "对话招队友", hint: "对话里招人：一次因为用模型名当名字被拒绝（卡片写明原因），一次招到 Iris，挂在对话下面", build: () => chatWithRecruit() },
  { key: "crash", label: "进程崩溃", hint: "单聊里 Claude CLI 崩溃退出：错误条和「需要你」写明怎么退出的", build: () => crashedSession() },
  { key: "context-lost", label: "上下文没延续（手动）", hint: "Pia 已归档、带 context_lost：自己点「恢复对话」，再发一条消息；顶部提示条要手动关", build: () => contextLost() },
  { key: "context-lost-restored", label: "恢复后：上下文带不回来", hint: "自动恢复 Pia：顶部提示「对话记录恢复了，但……下一条消息会开新会话」", build: () => contextLost([]) },
  { key: "context-lost-new", label: "发消息：这是新会话", hint: "恢复后发一条消息：再提示「这是新会话，没有带上之前的上下文」，标记随之清掉", build: () => contextLost(["接着把回调页的错误处理补上"]) },
  { key: "context-lost-cleared", label: "标记清掉后不再提示", hint: "第二条消息不再提示；点「终端恢复」可以复制命令了", build: () => contextLost(["接着把回调页的错误处理补上", "再加一个重试按钮"]) },
  { key: "code-lost", label: "代码没恢复（手动）", hint: "复制的主导已归档、run 分支已删：自己点「恢复对话」；「终端恢复」写明目录不在了", build: () => codeLost({ mode: "pr", reference: "https://github.com/acme/shop/pull/42" }, false) },
  { key: "code-lost-pr", label: "代码没恢复：开过 PR", hint: "恢复复制的主导：检出按 main 重建，提示 PR 不一定已合入", build: () => codeLost({ mode: "pr", reference: "https://github.com/acme/shop/pull/42" }) },
  { key: "code-lost-merge", label: "代码没恢复：squash 落地过", hint: "恢复复制的主导：提示没有核对落地的改动现在是否还在", build: () => codeLost({ mode: "squash", reference: "a1b2c3d" }) },
  { key: "resume-cmd", label: "复制原生恢复命令", hint: "Nova 的会话页点「终端恢复」：PowerShell 命令带目录、账号配置目录、代理和 --resume", build: () => resumeScene("ran") },
  { key: "resume-cmd-none", label: "恢复命令：没开过会话", hint: "Iris 从没开过会话：复制按钮置灰，写明原因", build: () => resumeScene("never") },
  { key: "resume-cmd-acp", label: "恢复命令：驱动不能恢复", hint: "ACP 队友：驱动不能恢复会话，复制按钮置灰", build: () => resumeScene("acp") },
  { key: "mismatch", label: "版本不匹配", hint: "hub 的接口是 v2：GUI 拒绝连接并说明原因", build: () => ({ state: { ...emptyHub(), api: "v2" }, script: [], route: "#/" }) },
  { key: "usage", label: "用量", hint: "两个账号，一个快到 5 小时限额，一个每周限额用完", build: () => ({ state: usageScene(), script: [], route: "#/usage" }) },
  { key: "cli-install-ok", label: "安装 CLI 成功", hint: "设置 → 智能体：CodeBuddy 没装，点「安装」先看确认框里的命令和来源；执行后输出滚动，装完提示去登录账号", build: () => cliScene("ok") },
  { key: "cli-install-fail", label: "安装 CLI 失败", hint: "安装命令以非 0 退出：滚动的输出和写明退出码的原因", build: () => cliScene("fail") },
  { key: "cli-install-missing", label: "装完但找不到", hint: "命令成功，但这台机器上找不到装好的程序：原因里列出找过的位置", build: () => cliScene("missing") },
  { key: "cli-upgrade", label: "升级 CLI", hint: "Codex 是 npm 装的：点「升级」，完成后版本号变化", build: () => cliScene("ok", "upgrade") },
  { key: "cli-bundled", label: "桌面版自带不可升级", hint: "Claude 来自桌面版：显示「随桌面版更新」，没有升级按钮", build: () => cliScene("ok", "install", false) },
];

export const DEFAULT_SCENARIO = "running";

/** Nova's reply names files; the viewer opens on main.rs at line 12. */
function filesScene() {
  const solo = soloSession();
  const nova = solo.state.agents[0];
  nova.status = "idle";
  const t = solo.state.transcripts.get(nova.id)!;
  const asked = turn({
    input: "--json 是在哪里解析的？顺便看看文档有没有写。",
    minutesAgo: 3,
    took: 1,
    items: [
      think("先找 CLI 入口，再对一下 docs。"),
      say(
        "在 [main.rs](crates/cli/src/main.rs:12) 里，`Args` 结构体的 `json` 字段就是全局的 `--json`。文档在 `docs/cli.md`，依赖版本见 `Cargo.toml`。\n\n另外 `docs/old-cli.md` 已经删掉了，不用再改；设计说明在 `docs/DESIGN.md`。",
      ),
    ],
  });
  t.turns = [t.turns[0], asked.turn];
  return {
    ...solo,
    script: [{ at: 600, act: () => openFile({ scope: { kind: "agent", agent: nova.id }, path: "crates/cli/src/main.rs", line: 12 }) }],
    route: `#/s/${nova.name}`,
  };
}

/** The solo chat, where Nova proposes doing the rest on a branch of its own. */
function workProposal() {
  const solo = soloSession();
  const nova = solo.state.agents[0];
  const item = {
    id: "w-0a1b2c3d4e",
    kind: "question" as const,
    from: nova.id,
    run_id: null,
    task_id: null,
    text: "README 快速开始与 --json 说明",
    detail: "给 README 加「快速开始」一节（安装、hub start、第一条 run），并把 CLI 所有子命令的 --json 输出格式补进 docs/cli.md。我们已经商定：示例用 PowerShell 写法，--json 的字段名照 api-types 原样列出。",
    options: ["开始", "不用"],
    default: null,
    timeout_at: null,
    status: "open" as const,
    answer: null,
    answered_at: null,
    action: "work" as const,
    created_at: new Date().toISOString(),
  };
  return { ...solo, state: { ...solo.state, questions: [...solo.state.questions, item] }, script: [] };
}

/** The settings → agents page with the CLI section: what an install or upgrade did is the scenario. */
function cliScene(install: "ok" | "fail" | "missing", action: "install" | "upgrade" = "install", play = true) {
  const state: MockState = { ...emptyHub(), install };
  const plugin = action === "install" ? "codebuddy" : "codex";
  const argv =
    action === "install"
      ? ["npm", "install", "-g", "@tencent-ai/codebuddy-code"]
      : ["npm", "install", "-g", "@openai/codex@latest"];
  const ok = install === "ok";
  const reason =
    install === "fail"
      ? "it exited with code 1"
      : install === "missing"
        ? "the command succeeded, but the hub cannot find the installed program afterwards; it looked in: PATH: codebuddy (codebuddy.cmd); C:/Users/me/.local/bin/codebuddy (not there)"
        : undefined;
  // The events the hub would send, played without a click: a screenshot shows the result,
  // not just the button that starts it. They end well inside the screenshot's default wait,
  // so `npm run screenshot` needs no timeout of its own to catch the finished state.
  const steps: Step[] = [
    { at: 200, event: { type: "cli.install.started", data: { plugin, action, argv, described_by: "built-in" } } },
    { at: 400, event: { type: "cli.install.output", data: { plugin, action, line: `$ ${argv.join(" ")}`, dropped: 0 } } },
    {
      at: 600,
      event: {
        type: "cli.install.output",
        data: { plugin, action, line: install === "fail" ? "npm error code EACCES" : "added 1 package in 3s", dropped: 0 },
      },
    },
    {
      at: 800,
      act: (s) => {
        if (!ok) return;
        const p = s.plugins.find((x) => x.name === plugin);
        if (!p?.cli) return;
        p.cli.installed = true;
        p.cli.version = action === "install" ? "1.0.6" : "0.159.0";
        p.cli.source = "path";
        p.cli.installed_via = "npm";
        p.cli.can_upgrade = true;
        p.cli.upgrade_command = ["npm", "install", "-g", action === "install" ? "@tencent-ai/codebuddy-code@latest" : "@openai/codex@latest"];
      },
    },
    {
      at: 900,
      event: { type: "cli.install.finished", data: { plugin, action, ok, code: install === "fail" ? 1 : 0, duration_ms: 2400, reason, dropped: 0 } },
    },
  ];
  return { state, script: play ? steps : [], route: "#/agents" };
}

/** A Claude agent and a CodeBuddy one: the setting-sources switch reads differently for each. */
function settingSources() {
  const solo = soloSession();
  const vega = agent({ name: "vega", plugin: "codebuddy", model: "glm-5.1", role: "worker", profile: { display_name: "Vega", title: "后端", color: "teal" } });
  return { ...solo, state: { ...solo.state, agents: [...solo.state.agents, vega] }, script: [], route: "#/agents" };
}
