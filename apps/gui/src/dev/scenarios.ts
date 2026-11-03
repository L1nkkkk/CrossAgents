// The scenarios `preview:mock` can show (docs/tasks/V6-GUI.md §3). Each builds a fresh state,
// an event script to replay, and the page to open.

import { emptyHub } from "../views/Home.mock";
import { approvalRun, deleteRun, graphRun, handoffRun, inPlaceRun, landingRun, longResultsRun, oauthRun, pausedRun, rerouteRun, reuseRun, reviewRun, syncRun, traceRun } from "../views/Run.mock";
import { attachmentSession, chatWithRecruit, contextUsageScene, crashedSession, failedEditSession, liveRun, liveSession, liveTaskSession, longNameScene, slashScene, soloSession, toolImagePathsSession, toolImagesSession } from "../views/Session.mock";
import { codeLost, contextLost, resumeScene } from "../views/Continuity.mock";
import { usageScene } from "../views/Usage.mock";
import { expertiseScene } from "../views/Expertise.mock";
import { agent, ago, capabilitiesOf, NOTES_REPO, planned, REPO, say, team, think, turn } from "./fixtures";
import { openFile } from "../FileViewer";
import { openTool } from "../panes";
import { newTerminal } from "../terminal";
import type { AccountQuota } from "@bindings/AccountQuota";
import type { PluginInfo } from "@bindings/PluginInfo";
import { wsKey } from "../workspace";
import { quota } from "../views/Usage.mock";
import { workspaceState } from "../views/Workspace.mock";
import type { MockState, Step } from "./MockBridge";

export type Scenario = {
  key: string;
  label: string;
  hint: string;
  build: () => { state: MockState; script: Step[]; route: string };
};

export const SCENARIOS: Scenario[] = [
  { key: "codebuddy-sites", label: "CodeBuddy 双站点账号", hint: "独立账号和国内/国际站选择", build: () => {
    const state = usageScene();
    const template = state.quota.accounts.find((a) => a.account.provider === "codebuddy")!;
    state.quota.providers = ["codebuddy"];
    state.quota.accounts = (["china", "international"] as const).map((site) => ({
      ...template,
      account: { ...template.account, id: `codebuddy-${site}`, codebuddy_site: site, label: site === "china" ? "CodeBuddy 国内站" : "CodeBuddy 国际站" },
      status: { ...template.status, logged_in: false, rate_limits: [], plan: null, note: "请完成对应站点的本人登录" },
      agents: [],
    }));
    return { state, route: "#/usage", script: [
      { at: 400, act: () => [...document.querySelectorAll("button")].find((b) => b.textContent === "添加账号")?.click() },
      { at: 800, act: () => document.querySelector(".card.form")?.scrollIntoView({ block: "start" }) },
    ] };
  } },
  { key: "tool-image-paths", label: "工具参数图片路径", hint: "加载、拒绝与去重", build: () => toolImagePathsSession() },
  { key: "tool-images", label: "工具结果图片", hint: "图片结果与混合文字", build: () => toolImagesSession() },
  { key: "continuation-options", label: "持续推进设置", hint: "新建 Work 的持续推进开关及次数、间隔", build: () => {
    localStorage.setItem("crossagents.mode", JSON.stringify("run"));
    return { state: emptyHub(), route: "#/", script: [{ at: 300, act: () => document.querySelector<HTMLButtonElement>('button[aria-label="选项"]')?.click() }] };
  } },
  { key: "continuation", label: "持续推进已达上限", hint: "恢复次数已耗尽，显示下一步和负责人", build: () => {
    const scene = oauthRun("running");
    const run = scene.state.runs[0].run;
    run.config.policy.continuation = { max_attempts: 3, interval_seconds: 60, stuck_minutes: null };
    run.continuation = { owner: run.planner, phase: "exhausted", next_step: "已达到自动唤醒次数上限。请检查任务状态，或向主管发送进一步指示。", attempts: 3, next_at: null, last_attempt_at: new Date().toISOString(), delivery_error: null };
    return { ...scene, script: [], route: `#/runs/${run.id}` };
  } },
  { key: "empty", label: "空 hub", hint: "第一次打开：没有队友、没有 run", build: () => ({ state: emptyHub(), script: [], route: "#/" }) },
  { key: "session-choice", label: "发起时选会话", hint: "首页 Work 的「会话」：新会话或已有会话；占用写明哪个 run，归档可恢复，不能换目录的置灰", build: () => {
    const { ada, rex, mia, sam } = team();
    mia.archived_at = ago(60 * 24 * 2);
    const state: MockState = { ...emptyHub(), agents: [ada, rex, mia, sam], quota: quota() };
    state.sessions = [
      { agent: ada, state: "idle", busy_with: null, last_execution: 'run "多租户隔离" #a (done)', can_use: true, reason: null },
      { agent: rex, state: "busy", busy_with: 'Rex is busy: it works on #a of run "OAuth 登录" (01a1)', last_execution: 'run "OAuth 登录" #a (in progress)', can_use: false, reason: "busy" },
      { agent: mia, state: "archived", busy_with: null, last_execution: 'run "登录页改版" #b (done)', can_use: true, reason: "archived" },
      { agent: sam, state: "idle", busy_with: null, last_execution: null, can_use: false, reason: "cannot_move" },
    ];
    localStorage.setItem("crossagents.mode", JSON.stringify("run"));
    localStorage.setItem("crossagents.workspace", wsKey(REPO));
    return { state, route: "#/", script: [{ at: 900, act: () => document.querySelector<HTMLButtonElement>('button[title="会话"]')?.click() }] };
  } },
  { key: "kept-draft", label: "草稿暂存", hint: "首页输入框开着上次留下的文本和图片：离开本页再回来不会丢", build: () => {
    const { ada, rex } = team();
    const state: MockState = { ...emptyHub(), agents: [ada, rex], quota: quota() };
    localStorage.setItem("crossagents.mode", JSON.stringify("run"));
    localStorage.setItem("crossagents.workspace", wsKey(REPO));
    // A 1×1 PNG stands in for the screenshot that was pasted before leaving the page.
    const png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    localStorage.setItem(
      "crossagents.draft.home",
      JSON.stringify({ text: "把登录页的错误提示改成人话，顺便\n", title: "错误提示改人话", files: [{ name: "截图.png", mime: "image/png", data: png, size: 70 }] }),
    );
    return { state, route: "#/", script: [] };
  } },
  { key: "plan-models", label: "批计划：别家插件的模型", hint: "计划卡的人选：所有已装插件的模型按家族分组；没有登录的 Kimi 置灰并写明原因", build: () => {
    const scene = oauthRun("plan");
    const kimi: PluginInfo = {
      name: "kimi",
      models: ["kimi-k3", "kimi-k2.7-code"],
      all_models: ["kimi-k3", "kimi-k2.7-code"],
      shown: null,
      models_pinned: false,
      driver: "acp-generic",
      capabilities: capabilitiesOf("kimi"),
      efforts: [],
    };
    scene.state.plugins = [...scene.state.plugins, kimi];
    scene.state.quota.accounts = [...scene.state.quota.accounts, signedOutKimi()];
    return {
      ...scene,
      route: `#/runs/${scene.state.runs[0].run.id}`,
      // Show a cross-plugin pick taking effect: task #a's worker chosen from the With gateway.
      script: [
        { at: 800, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="a"] .chip-pick')?.click() },
        {
          at: 1000,
          act: () =>
            [...document.querySelectorAll<HTMLButtonElement>(".who-pick-menu .menu-item")]
              .find((b) => b.querySelector(".menu-sub")?.textContent === "gpt-6-sol" && !b.disabled)
              ?.click(),
        },
      ],
    };
  } },
  { key: "plan-picker", label: "批计划：挑执行者", hint: "计划卡 #a 的执行者菜单展开：队友带头像，副行是模型 · 状态 · 最近一次执行；下面是各家的新建模型", build: () => {
    const scene = oauthRun("plan");
    // Fixture agents are named by their display name ("Rex"); match any case.
    const byName = (n: string) => scene.state.agents.find((a) => a.name.toLowerCase() === n)!;
    scene.state.sessions = [
      { agent: byName("rex"), state: "idle", busy_with: null, last_execution: 'run "多租户隔离" #a (done)', can_use: true, reason: null },
      { agent: byName("mia"), state: "busy", busy_with: 'Mia is busy: it works on #b of run "登录页改版" (01a2)', last_execution: 'run "登录页改版" #b (in progress)', can_use: false, reason: "busy" },
      { agent: byName("sam"), state: "idle", busy_with: null, last_execution: null, can_use: true, reason: null },
    ];
    return {
      ...scene,
      route: `#/runs/${scene.state.runs[0].run.id}`,
      script: [{ at: 900, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="a"] .chip-pick')?.click() }],
    };
  } },
  { key: "plan", label: "计划待审批", hint: "run 页：中间只有主导的对话和待审批的 Plan；右侧栏是计划、run 信息、策略与预算", build: () => oauthRun("plan") },
  { key: "plan-long", label: "批计划：长列表里挑执行者", hint: "计划有九个任务、标题很长：最后一个任务的执行者菜单在可滚动的计划区里展开，不被裁掉，放不下时向上翻", build: () => {
    const scene = oauthRun("plan");
    const r = scene.state.runs[0].run;
    const more = ["e", "f", "g", "h", "i"].map((k, i) =>
      planned(k, `第 ${i + 5} 步：把登录相关的旧接口逐个迁到新的会话层并补齐回归测试（${k}）`, "接口不变，旧调用方逐个改过来，每改一个补一条回归测试。", i % 2 ? "mia" : "rex", ["c"], [`src/legacy/${k}/**`]),
    );
    r.plan = [...r.plan, ...more];
    return {
      ...scene,
      route: `#/runs/${r.id}`,
      script: [
        { at: 700, act: () => document.querySelector('.who-pick[data-task="i"]')?.scrollIntoView({ block: "end" }) },
        { at: 1000, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="i"] .chip-pick')?.click() },
      ],
    };
  } },
  { key: "running", label: "run 进行中", hint: "run 页：中间栏只有对话；右侧栏按任务分组（含交接链）、run 信息、策略、笔记，追溯和参与者收起", build: () => oauthRun("running") },
  { key: "paused", label: "run 已暂停", hint: "中间只有一行「已暂停」提示，展开看全文；计划、run 信息、策略都在右侧栏", build: () => pausedRun() },
  { key: "review", label: "审查任务", hint: "#c 已报告完成：整个任务的 diff 带行号，点行号写意见，可发给 worker 或打回重做", build: () => reviewRun() },
  { key: "landing", label: "落地前总 diff", hint: "所有任务都已合并、等落地：展开「查看全部改动」看 run 分支对 main 的总 diff", build: () => landingRun() },
  { key: "sync", label: "落地等冲突解决", hint: "main 动过且和 run 冲突：hub 建了 #sync-main 交给 Rex，run 页写明原因，合并后再落地", build: () => syncRun() },
  { key: "reroute", label: "额度改派", hint: "Mia 的账号额度用完，#b 改到另一个账号；任务卡片写明原因", build: () => rerouteRun() },
  { key: "handoff", label: "交接卡", hint: "右侧栏的任务列表里每个任务下一条交接链：#a 派活、#c 返工后改派给 Mia、#d 升级给 Kai 还有问题没答、#b 额度改派（稍后到）；点节点展开卡片", build: () => handoffRun() },
  { key: "reuse", label: "会话跨 run 复用", hint: "Rex 在一期本人做了 #a，二期又被指定用本人做 #c、此刻正在执行；一期 #b 的执行会话 Rex-b 已归档，可恢复。看 run 页「参与者」和 Rex 的会话页", build: () => reuseRun() },
  { key: "reuse-done", label: "参与过的 run", hint: "一期 run 页「参与者」：#a 是 Rex 本人（被打回、交付），#b 的执行会话 Rex-b 已归档，可以点开或恢复", build: () => reuseRun("a") },
  { key: "reuse-busy", label: "会话正在执行", hint: "Rex 的会话页：顶部写明正在执行二期 · 任务 #c（本人），带中断按钮", build: () => reuseRun("rex") },
  { key: "approval", label: "危险操作待批准", hint: "Rex 想 git push：需要你里出现「等你批准」，允许 / 拒绝，30 分钟没回答就拒绝", build: () => approvalRun() },
  { key: "trace", label: "失败追溯", hint: "run 页右侧栏的「追溯」：#d 失败、首个失败标红展开；#c 途中出错后通过，灰色折叠；每一轮、每条命令和花费", build: () => traceRun() },
  { key: "graph", label: "提交图", hint: "run 页的工具列「提交图」：main、run 分支和 #a #b #c 三个任务分支，合并提交是空心点；选中了 #b 的合并，下面是它的 diff，点文件名打开文件", build: () => graphRun() },
  { key: "graph-session", label: "会话的提交图", hint: "Rex 在 #c 的会话页：顶栏「提交图」在右侧工具列打开，HEAD 和本地分支，选中了最新的提交", build: () => graphRun("session") },
  { key: "inplace", label: "无 git 的 run", hint: "文件夹没有 git：run 直接改文件、一次一个任务，可一键初始化 git", build: () => inPlaceRun() },
  { key: "workcard", label: "会话提议 Work", hint: "Nova 在单聊里用 work_start 提议到独立分支上做：卡片可展开交给 Work 的说明，开始 / 不用", build: () => workProposal() },
  { key: "long-results", label: "长结果折叠", hint: "Ada 第一次落地被拒、理由里是整段 cargo test 日志：卡片只留一行（✕ · 落地 run · 对象和理由第一句），点图标或整行看完整理由和调用参数；run 页的总结同样只留一行", build: () => longResultsRun() },
  { key: "long-results-run", label: "长总结折叠（run 页）", hint: "完成卡片里的总结只留第一行，点图标或整行展开看完整总结", build: () => { const s = longResultsRun(); return { ...s, route: `#/runs/${s.state.runs[0].run.id}` }; } },
  { key: "delete-landed", label: "删除已落地的 run", hint: "删除确认框：主导和它的对话一起删，工作区和分支，已落地所以分支默认勾选", build: () => deleteRun("landed") },
  { key: "delete-unlanded", label: "删除未落地的 run", hint: "未落地：分支默认不勾，写明「未落地的改动会丢失」", build: () => deleteRun("unlanded") },
  { key: "delete-dirty", label: "删除时工作区有改动", hint: "点「删除」后 hub 返回 409 并列出没提交的文件，可以「仍然删除」", build: () => deleteRun("dirty") },
  { key: "files", label: "文件链接与查看器", hint: "回复里的路径（确认存在的才成链接）可点开只读查看器：左侧懒加载目录树，右侧行号、高亮、跳到第 12 行", build: () => filesScene() },
  { key: "terminal", label: "内置终端", hint: "对话下方的终端标签：回显输入；标签可拖到右侧，Ctrl+` 收起和打开，输入 exit 看退出后的样子", build: () => ({ ...filesScene(), script: [{ at: 600, act: () => void newTerminal() }] }) },
  { key: "files-pane", label: "文件面板", hint: "工具列的「文件」：目录树，点开 docs/cli.md 是渲染后的 Markdown，可切回源码；文档里的相对链接在面板里打开", build: () => filesPaneScene() },
  { key: "solo", label: "单聊", hint: "Claude 会话：折叠的工具步骤、一轮进行中、两条排队", build: () => soloSession() },
  { key: "ctx-menu", label: "上下文菜单（20%）", hint: "点顶栏（或右侧栏「上下文」）的用量：明细和「压缩上下文」，不必等到 50%", build: () => contextUsageScene() },
  { key: "slash-menu", label: "输入框 / 指令", hint: "Agent 空闲时打 /：CrossAgents 的指令在上，↑↓ 选，Enter 执行，Esc 关闭", build: () => slashScene("idle") },
  { key: "slash-unknown", label: "自带指令未知", hint: "驱动不报告自带指令", build: () => slashScene("unknown") },
  { key: "slash-blocked", label: "/compact 运行中不可用", hint: "Nova 正在工作：菜单里的 /compact 写明本轮结束后才能压缩", build: () => slashScene("running") },
  { key: "long-name", label: "长名字的顶栏", hint: "名字很长、正在执行、上下文 65%：顶栏先把终端恢复/提交图收成图标、Meter 只留百分比，名字仍认得出来", build: () => longNameScene() },
  { key: "live-stream", label: "运行中：输出还在来", hint: "正在跑的命令，实时输出只在工具那一行里（一次，不重复）；约 6 秒后出现推理流，LiveLine 显示推理的最后一行", build: () => liveSession("stream") },
  { key: "live-quiet", label: "运行中：45 秒没有新输出", hint: "同一处写明「已 N 秒没有新输出」（要等页面静置 30 秒以上；截图请用 wait≈46s）", build: () => liveSession("quiet") },
  { key: "live-run", label: "run 页：输出还在来", hint: "同一个运行中的一轮放在 run 页的中间栏：命令的实时输出只在工具行里一次，LiveLine 只留状态行", build: () => liveRun("stream") },
  { key: "live-run-quiet", label: "run 页：45 秒没有新输出", hint: "run 页同样写明「已 N 秒没有新输出」（截图请用 wait≈46s）", build: () => liveRun("quiet") },
  { key: "live-task", label: "会话在 run 里干任务", hint: "会话页头部与侧栏：本轮已跑多久、最近活动、任务进度、3/4 检查通过、run 的 1/3 已合并 · 1 进行中 · 1 失败", build: () => liveTaskSession() },
  { key: "tool-fail", label: "工具调用失败", hint: "编辑失败：失败步骤收成一行（✕ · 工具 · 失败原因第一行），点 ✕ 展开工具原话、退出码和参数；成功的编辑照旧", build: () => failedEditSession() },
  { key: "attach", label: "消息带附件", hint: "人发的消息带了截图和日志（点缩略图看大图）；输入框可粘贴截图、拖文件或点回形针，发送前能删", build: () => attachmentSession() },
  { key: "settings-src", label: "Claude 设置源", hint: "设置 → 智能体：点 Nova 编辑，「读取 Claude 设置」三档预设（缺省 / 全部 / 都不读）；点 Vega（CodeBuddy）编辑，标签说明它不读 CLAUDE.md", build: settingSources },
  { key: "recruit", label: "对话招队友", hint: "对话里招人：一次因为用模型名当名字被拒绝（一行写明对象和理由第一句，点 ✕ 看完整理由），一次招到 Iris，挂在对话下面", build: () => chatWithRecruit() },
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
  { key: "usage-30", label: "用量：30 天", hint: "切换至 30 天", build: () => ({ state: usageScene(), route: "#/usage", script: [{ at: 400, act: () => [...document.querySelectorAll("button")].find((b) => b.textContent === "30 天")?.click() }] }) },
  { key: "usage-all", label: "用量：全部", hint: "切换至全部", build: () => ({ state: usageScene(), route: "#/usage", script: [{ at: 400, act: () => [...document.querySelectorAll("button")].find((b) => b.textContent === "全部")?.click() }] }) },
  { key: "usage-hover", label: "用量：每日明细", hint: "聚焦日期", build: () => ({ state: usageScene(), route: "#/usage", script: [{ at: 1200, act: () => document.querySelector<SVGElement>(".usage-svg rect[tabindex]")?.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })) }] }) },
  { key: "usage-empty", label: "用量：无数据", hint: "没有本地日志", build: () => ({ state: usageScene("empty"), script: [], route: "#/usage" }) },
  { key: "usage-issues", label: "用量：来源错误", hint: "未计价模型及来源读取失败", build: () => ({ state: usageScene("issues"), script: [], route: "#/usage" }) },
  { key: "usage-session", label: "用量：会话口径", hint: "无法确定单次请求", build: () => ({ state: usageScene("session"), script: [], route: "#/usage" }) },
  { key: "usage", label: "用量", hint: "两个账号，一个快到 5 小时限额，一个每周限额用完", build: () => ({ state: usageScene(), script: [], route: "#/usage" }) },
  { key: "expertise", label: "专长档案", hint: "个人/项目档案、超预算与黄红标记、带冲突的候选；脚本：派活推送 → 交活填结果 → 返工出候选 → 接受 → 专长卡变化", build: () => ({ ...expertiseScene(), route: "#/expertise" }) },
  { key: "cli-install-ok", label: "安装 CLI 成功", hint: "设置 → 智能体：CodeBuddy 没装，点「安装」先看确认框里的命令和来源；执行后输出滚动，装完提示去登录账号", build: () => cliScene("ok") },
  { key: "cli-install-fail", label: "安装 CLI 失败", hint: "安装命令以非 0 退出：滚动的输出和写明退出码的原因", build: () => cliScene("fail") },
  { key: "cli-install-missing", label: "装完但找不到", hint: "命令成功，但这台机器上找不到装好的程序：原因里列出找过的位置", build: () => cliScene("missing") },
  { key: "cli-upgrade", label: "升级 CLI", hint: "Codex 是 npm 装的：点「升级」，完成后版本号变化", build: () => cliScene("ok", "upgrade") },
  { key: "cli-bundled", label: "桌面版自带不可升级", hint: "Claude 来自桌面版：显示「随桌面版更新」，没有升级按钮", build: () => cliScene("ok", "install", false) },
  { key: "agent-workspace-sidebar", label: "项目侧栏：成员与待办", hint: "项目 → 成员（Lin 已归档），独立会话，「需要你」两条（都是 Mia 的）；Rex 既是 #a 的执行者又是 #b 的评审，只出现一行", build: () => ({ state: workspaceState(), script: [], route: "#/" }) },
  { key: "agent-workspace-overview", label: "项目总览", hint: "点项目名进入总览：待审批、进行中、旧的已落地 run 各一个；Nova-c 是用 Nova 的配置新建的执行会话，#a 是 Rex 本人", build: () => ({ state: workspaceState(), script: [], route: `#/w/${encodeURIComponent(wsKey(REPO))}` }) },
  { key: "agent-workspace-edges", label: "边界 run", hint: "NOTES_REPO：失败的 run 和完成但未落地的 run（不能显示成完成）；REPO 里还有没提交 plan 的登录限流和来源已归档的多租户隔离", build: () => ({ state: workspaceState(), script: [], route: `#/w/${encodeURIComponent(wsKey(NOTES_REPO))}` }) },
  { key: "agent-workspace-history", label: "项目历史：删掉的 run", hint: "工作区「历史」：删掉的两轮标「已删除」、只读，摘要和任务仍在；没删的已结束 run 从这里也能打开", build: () => {
    localStorage.setItem("crossagents.workspace.tab", "history");
    return { state: workspaceState(), script: [], route: `#/w/${encodeURIComponent(wsKey(REPO))}` };
  } },
  { key: "agent-workspace-narrow", label: "窄窗口与长标题", hint: "Atlas 的长话题和 #b 的长中文标题：226px 侧栏下名字、状态和摘要仍可辨认，截断不挤坏布局；窄窗口请用截图调整宽度", build: () => ({ state: workspaceState(), script: [], route: "#/s/Atlas" }) },
];

export const DEFAULT_SCENARIO = "running";

/** A Kimi login that has never signed in: its plugin's models stay listed, disabled. */
function signedOutKimi(): AccountQuota {
  return {
    account: { id: "kimi-work", provider: "kimi", label: "Kimi 工作号", codebuddy_site: null, home: "C:/Users/me/.crossagents/accounts/kimi-work", created_at: ago(60 * 24 * 9) },
    status: {
      logged_in: false,
      auth_mode: null,
      email: null,
      plan: null,
      subscription_until: null,
      rate_limits: [],
      limit_reached: false,
      credits: null,
      limits: [],
      reset_credits: [],
      source: null,
      as_of: null,
      note: "请完成登录",
    },
    models: [],
    agents: [],
  };
}

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

/** The same conversation with the 文件 pane open in the tool column. */
function filesPaneScene() {
  const scene = filesScene();
  const nova = scene.state.agents[0];
  return { ...scene, script: [{ at: 600, act: () => openTool("files", { agent: nova.name }) }] };
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
