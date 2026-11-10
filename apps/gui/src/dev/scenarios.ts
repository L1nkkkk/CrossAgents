// The scenarios `preview:mock` can show (docs/tasks/V6-GUI.md §3). Each builds a fresh state,
// an event script to replay, and the page to open.

import { emptyHub } from "../views/Home.mock";
import { extensionScene } from "./extensions.mock";
import { approvalRun, deleteRun, graphRun, handoffRun, inPlaceRun, landingRun, longResultsRun, oauthRun, pausedRun, rerouteRun, reuseRun, reviewRun, syncRun, traceRun } from "../views/Run.mock";
import { relayScene } from "../views/RunRelay.mock";
import { workRounds } from "../views/Work.mock";
import { swimlaneScene } from "../views/RunSwimlane.mock";
import { runCostScene } from "../views/RunCost.mock";
import type { BackgroundResponse } from "@bindings/BackgroundResponse";
import { attachmentSession, chatWithRecruit, contextUsageScene, crashedSession, failedEditSession, liveRun, liveSession, liveTaskSession, longNameScene, moderationSession, slashScene, soloSession, diagramSession, toolImagePathsSession, toolImagesSession } from "../views/Session.mock";
import { codeLost, contextLost, resumeScene } from "../views/Continuity.mock";
import { usageScene } from "../views/Usage.mock";
import { expertiseScene } from "../views/Expertise.mock";
import { lessonsScene } from "../views/Lessons/Lessons.mock";
import { agent, ago, capabilitiesOf, devices, NOTES_REPO, planned, REPO, run, say, team, think, transcript, turn, uid } from "./fixtures";
import { openFile } from "../FileViewer";
import { docksNow, openTool, placeTab, shutTab } from "../panes";
import { setBottomHeight, setSidebarWidth, setToolsWidth, SIDEBAR } from "../lib/widths";
import { newTerminal } from "../terminal";
import { openBrowserTab } from "../browser";
import type { AccountQuota } from "@bindings/AccountQuota";
import type { HumanItem } from "@bindings/HumanItem";
import type { PluginInfo } from "@bindings/PluginInfo";
import { wsKey } from "../workspace";
import { quota } from "../views/Usage.mock";
import { workspaceState } from "../views/Workspace.mock";
import type { MockState, Step } from "./MockBridge";
import { mockInviteCode } from "./MockBridge";
import { FEATURES } from "../lib/features";

export type Scenario = {
  key: string;
  label: string;
  hint: string;
  build: () => { state: MockState; script: Step[]; route: string };
};

/** Fills a React-controlled input as a person would, so its `onChange` runs. */
function typeInto(selector: string, value: string) {
  const input = document.querySelector<HTMLInputElement | HTMLTextAreaElement>(selector);
  if (!input) throw new Error(`scenario: no ${selector}`);
  // React follows the value through the element's own setter, and a textarea has its own.
  const proto = input instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
  Object.getOwnPropertyDescriptor(proto, "value")!.set!.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

export const SCENARIOS: Scenario[] = [
  { key: "work-rounds", label: "工作组：三轮", hint: "同一个对话里连着三轮：第一轮已落地，第二轮没落地，第三轮叠在它上面；右侧「各轮」切换，对话里的分隔跟着走；侧栏里工作组落在各自的项目下，一个项目只列最近三个", build: () => {
    const scene = workRounds();
    // A work group lives under its project now, so the width has to leave its name readable:
    // start from the default whatever the last preview left behind.
    return { ...scene, script: [{ at: 200, act: () => setSidebarWidth(SIDEBAR.initial) }, ...scene.script] as Step[] };
  } },
  { key: "work-rounds-tight", label: "工作组：最窄侧栏", hint: "同一个场景在 200px 的侧栏里：项目下的工作组只标原地，worktree 只在 tooltip 里说，名字仍然读得出来", build: () => {
    const scene = workRounds();
    return { ...scene, script: [{ at: 200, act: () => setSidebarWidth(SIDEBAR.min) }, ...scene.script] as Step[] };
  } },
  { key: "swimlane", label: "Run 泳道图", hint: "一个 Worker 一根线：交接、提交、测试 ✓/✗、重试、改派（分叉）、合入、上下文压缩、模型切换、等你；点名字只看一根线", build: () => swimlaneScene() },
  { key: "swimlane-landed", label: "Run 泳道图 · 已落地", hint: "三条线都合入后落地，泳道以「落地」收尾", build: () => swimlaneScene(true) },
  { key: "swimlane-focus", label: "Run 泳道图 · 聚焦", hint: "只看 Rex 一根线，节点带时间", build: () => {
    const scene = swimlaneScene();
    const focus: Step = { at: 900, act: () => document.querySelector<HTMLButtonElement>('[data-card="run-swimlane"] .swim-head')?.click() };
    return { ...scene, script: [...scene.script, focus] };
  } },
  { key: "relay-needyou", label: "接力链 · 需要你 · 按产出命名", hint: "每条任务线一行接力链；问题在时间线里点选回答；折叠的步骤按产出命名，产物在下面", build: () => relayScene() },
  { key: "relay-needyou-steps", label: "按产出命名的步骤", hint: "折叠的步骤读作「改了 3 个文件 · 跑了测试：通过 · 出了 1 张图」，缩略图和路径就在那一行下面（这个问题已在别处答完，好让对话有地方）", build: () => {
    const scene = relayScene();
    // Answered elsewhere: with no open question the dock is short and the turn has room.
    return { ...scene, state: { ...scene.state, questions: [] }, script: [] };
  } },
  { key: "delivery", label: "消息送达状态", hint: "自己发的消息一行到底：排队原因、已撤回、失败可重试", build: () => {
    // A session with one turn running and messages waiting. The person's messages stay in the
    // chat with their state on the bubble (P0-1); only the orchestrator's waits list 接下来.
    const scene = soloSession();
    const nova = scene.state.agents[0];
    const t = scene.state.transcripts.get(nova.id)!;
    const waiting = t.turns.filter((x) => !x.started_at && !x.completed_at);
    Object.assign(waiting[0], { queued_reason: "owner_queue_ahead" });
    Object.assign(waiting[1], { queued_reason: "turn_running" });
    // One the person took back: the row stays, marked (P0-1).
    const back = turn({ input: "这条算了，撤回吧。", minutesAgo: 0.15, queued: true, items: [] });
    Object.assign(back.turn, { withdrawn: true });
    // An orchestration message waiting: this is what 接下来 lists.
    const orch = turn({ input: "再核对一遍文档里的命令。", from: { kind: "orchestrator", run_id: "run-delivery-preview" }, minutesAgo: 0.05, queued: true, items: [] });
    Object.assign(orch.turn, { queued_reason: "turn_running" });
    t.turns = [...t.turns, back.turn, orch.turn];
    t.delivery = { state: "turn", queued: 4 };
    // Type a message the mock refuses: its row says 失败 and offers 重试 (P2-6).
    const type = (text: string) => {
      const el = document.querySelector<HTMLTextAreaElement>(".composer textarea");
      if (!el) throw new Error("composer missing");
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(el, text);
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    };
    return { state: scene.state, route: scene.route, script: [{ at: 900, act: () => type("[fail] 这条发不出去：看文案和重试") }] };
  } },
  { key: "plan-long-search", label: "长计划中的模型搜索", hint: "浮层防裁切、筛选与清空后恢复高度", build: () => {
    const scene = SCENARIOS.find((s) => s.key === "plan-long")!.build();
    const search = (value: string) => {
      const input = document.querySelector<HTMLInputElement>(".who-pick-menu .menu-search");
      if (!input) throw new Error("long plan search missing");
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
      input.dispatchEvent(new Event("input", { bubbles: true }));
    };
    const verify = () => {
      const menu = document.querySelector<HTMLElement>(".who-pick-menu")!;
      const rect = menu.getBoundingClientRect();
      if (menu.parentElement !== document.body || rect.top < 0 || rect.bottom > window.innerHeight || rect.left < 0 || rect.right > window.innerWidth) throw new Error("long plan menu clipped");
      if (document.activeElement !== menu.querySelector(".menu-search")) throw new Error("long plan search lost focus");
    };
    return { ...scene, script: [...scene.script,
      { at: 1300, act: () => search("cursor") },
      { at: 1550, act: verify },
      { at: 1700, act: () => search("") },
      { at: 1950, act: () => { verify(); console.info("long plan portal search verified"); } },
    ] };
  } },
  { key: "cursor-reasoning", label: "Cursor 推理控件", hint: "基础模型与 Thinking / 强度 / Fast", build: () => {
    const state = usageScene();
    const cursor: PluginInfo = { ...state.plugins.find((p) => p.name === "codex")!, name: "cursor", driver: "cursor-native", efforts: [],
      models: ["claude-opus-5-high", "claude-opus-5-thinking-high", "claude-opus-5-thinking-max", "claude-opus-5-thinking-high-fast"], all_models: [] };
    cursor.all_models = cursor.models;
    state.plugins = [cursor];
    state.quota.accounts = [];
    localStorage.setItem("crossagents.mode", JSON.stringify("chat"));
    localStorage.setItem("crossagents.chatModel", JSON.stringify({ plugin: "cursor", model: "claude-opus-5-thinking-high" }));
    return { state, route: "#/", script: [] };
  } },
  { key: "plan-reasoning", label: "任务推理设置", hint: "Cursor 模式与逐任务强度", build: () => {
    const scene = oauthRun("plan");
    const state = scene.state;
    const cursor: PluginInfo = { ...state.plugins.find((p) => p.name === "codex")!, name: "cursor", driver: "cursor-native", efforts: [],
      models: ["claude-opus-5-high", "claude-opus-5-thinking-high", "claude-opus-5-thinking-max", "claude-opus-5-thinking-high-fast"], all_models: [] };
    cursor.all_models = cursor.models;
    state.plugins = [...state.plugins.filter((p) => p.name !== "cursor"), cursor];
    const worker = state.agents.find((a) => a.name.toLowerCase() === "rex")!;
    worker.plugin = "cursor";
    worker.model = "claude-opus-5-high";
    const originalIds = new Set(state.agents.map((a) => a.id));
    return { ...scene, script: [
      { at: 400, act: () => document.querySelector<HTMLButtonElement>('[data-task="a"] button[title^="Cursor 思考模式"]')?.click() },
      { at: 650, act: () => [...document.querySelectorAll<HTMLButtonElement>(".menu-item")].find((b) => b.querySelector(".menu-label")?.textContent === "Thinking")?.click() },
      { at: 900, act: () => document.querySelector<HTMLButtonElement>('[data-task="b"] button[title="推理强度"]')?.click() },
      { at: 1150, act: () => [...document.querySelectorAll<HTMLButtonElement>(".menu-item")].find((b) => b.querySelector(".menu-label")?.textContent === "高")?.click() },
      { at: 2100, act: () => [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.includes("批准并开工"))?.click() },
      { at: 2600, act: () => {
        const made = state.agents.filter((a) => !originalIds.has(a.id));
        if (!made.some((a) => a.plugin === "cursor" && a.model === "claude-opus-5-thinking-high")) throw new Error("Cursor task mode was not saved");
        if (!made.some((a) => a.plugin === "claude" && a.config && typeof a.config === "object" && !Array.isArray(a.config) && a.config.effort === "high")) throw new Error("native task effort was not saved");
        if (worker.model !== "claude-opus-5-high") throw new Error("task override mutated the shared worker");
        console.info("reasoning approval verified: Cursor mode and native effort saved in isolated workers");
      } },
    ] };
  } },
  { key: "model-search", label: "模型搜索", hint: "CLI 别名显示名与平台筛选", build: () => {
    const state = usageScene();
    const claude = state.plugins.find((p) => p.name === "claude")!;
    claude.models = ["opus", "sonnet", "claude-opus-5"];
    claude.all_models = claude.models;
    const account = state.quota.accounts.find((a) => a.account.provider === "claude")!;
    state.quota.accounts = state.quota.accounts.filter((a) => a.account.provider !== "claude" || a === account);
    account.models = [
      { id: "opus", name: "Opus 5.5", description: "For complex work", context_window: null },
      { id: "sonnet", name: "Sonnet 5.5", description: null, context_window: null },
      { id: "claude-opus-5", name: "Opus 5", description: null, context_window: null },
    ];
    return { state, route: "#/", script: [
      { at: 200, act: () => {
        localStorage.setItem("crossagents.mode", JSON.stringify("chat"));
        window.dispatchEvent(new CustomEvent("crossagents:mode", { detail: "chat" }));
      } },
      { at: 600, act: () => document.querySelector<HTMLButtonElement>('button[title="模型"]')?.click() },
      { at: 900, act: () => {
        const input = document.querySelector<HTMLInputElement>(".menu-search");
        if (!input) throw new Error("model search input missing");
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, "claude opus");
        input.dispatchEvent(new Event("input", { bubbles: true }));
      } },
      { at: 1100, act: () => {
        const labels = [...document.querySelectorAll(".menu-options .menu-label")].map((el) => el.textContent);
        if (labels[0] !== "Opus 5.5" || labels[1] !== "Opus 5" || labels.some((label) => label?.includes("Sonnet"))) throw new Error(`unexpected model matches: ${labels}`);
        const input = document.querySelector<HTMLInputElement>(".menu-search")!;
        if (document.activeElement !== input) throw new Error("search input should have focus");
        input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
      } },
      { at: 1300, act: () => {
        if (document.querySelector(".menu-item.cursor .menu-label")?.textContent !== "Opus 5") throw new Error("arrow navigation did not move to the filtered option");
      } },
    ] };
  } },
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
  { key: "cursor-account", label: "Cursor 账号", hint: "登录后读状态、邮箱和模型；额度留空，显示暂无额度数据", build: () => {
    const state = usageScene();
    return { state, route: "#/usage", script: [
      { at: 700, act: () => [...document.querySelectorAll<HTMLButtonElement>(".account-who")].find((b) => b.textContent?.includes("Cursor"))?.click() },
    ] };
  } },
  { key: "tool-image-paths", label: "工具参数图片路径", hint: "加载、拒绝与去重", build: () => toolImagePathsSession() },
  { key: "diagram", label: "回复里的图与嵌套代码块", hint: "```mermaid 画成图，悬停出现「源码」；```` 里套 ```text 仍是一个代码块", build: () => diagramSession() },
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
  { key: "task-runs", label: "任务的多轮运行", hint: "任务详情：共 3 次运行，平台故障与会话失效各自动重试一次，agent_error 停下并写明原因", build: () => {
    const scene = oauthRun("running");
    const detail = scene.state.runs[0];
    const task = detail.tasks[0];
    task.runs = [
      { index: 1, started_at: ago(40), ended_at: ago(36), outcome: "infrastructure", agent: task.assignee, message: null, note: "request timed out after 600s", retried: true },
      { index: 2, started_at: ago(35), ended_at: ago(31), outcome: "session_lost", agent: task.assignee, message: null, note: "context length exceeded: 400000 > 200000", retried: true },
      { index: 3, started_at: ago(30), ended_at: ago(26), outcome: "agent_error", agent: task.assignee, message: null, note: "agent_error: invalid api key", retried: false },
    ];
    task.status = "stalled";
    task.report.failure = "agent_error: invalid api key";
    return { ...scene, script: [], route: `#/runs/${detail.run.id}/tasks/${task.id}` };
  } },
  { key: "empty", label: "空 hub", hint: "第一次打开：没有队友、没有 run", build: () => ({ state: emptyHub(), script: [], route: "#/" }) },
  { key: "session-choice", label: "发起时选会话", hint: "首页 Work 的「会话」：新会话或已有会话；占用写明哪个 run，归档可恢复，不能换目录的置灰", build: () => {
    const { ada, rex, mia, sam } = team();
    mia.archived_at = ago(60 * 24 * 2);
    const state: MockState = { ...emptyHub(), agents: [ada, rex, mia, sam], quota: quota() };
    state.sessions = [
      { agent: ada, state: "idle", busy_with: null, last_execution: 'run "多租户隔离" #a (done)', can_use: true, reason: null, unguarded: false },
      { agent: rex, state: "busy", busy_with: 'Rex is busy: it works on #a of run "OAuth 登录" (01a1)', last_execution: 'run "OAuth 登录" #a (in progress)', can_use: false, reason: "busy", unguarded: false },
      { agent: mia, state: "archived", busy_with: null, last_execution: 'run "登录页改版" #b (done)', can_use: true, reason: "archived", unguarded: false },
      { agent: sam, state: "idle", busy_with: null, last_execution: null, can_use: false, reason: "cannot_move", unguarded: false },
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
      { agent: byName("rex"), state: "idle", busy_with: null, last_execution: 'run "多租户隔离" #a (done)', can_use: true, reason: null, unguarded: false },
      { agent: byName("mia"), state: "busy", busy_with: 'Mia is busy: it works on #b of run "登录页改版" (01a2)', last_execution: 'run "登录页改版" #b (in progress)', can_use: false, reason: "busy", unguarded: false },
      { agent: byName("sam"), state: "idle", busy_with: null, last_execution: null, can_use: true, reason: null, unguarded: false },
    ];
    return {
      ...scene,
      route: `#/runs/${scene.state.runs[0].run.id}`,
      script: [{ at: 900, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="a"] .chip-pick')?.click() }],
    };
  } },
  { key: "plan", label: "计划待审批", hint: "run 页：中间只有主导的对话和待审批的 Plan；右侧栏是计划、run 信息、策略与预算", build: () => oauthRun("plan") },
  { key: "unusable-cursor", label: "Cursor 在守卫 run 里用不了", hint: "Windows 上的 Cursor 不能跑守卫会话：Plan 的候选里它置灰、点不了，副行写着 hub 的原因；指派给它的任务在审批被拒绝后，原因就写在那一行", build: () => {
    const scene = oauthRun("plan");
    const state = scene.state;
    const lead = state.agents.find((a) => a.name === "Ada")!;
    // A Cursor teammate with no permission of its own: the run's approvals decide, as on a
    // real hub — this machine (Windows) cannot give it a guarded session.
    const cursor = agent({ name: "pia", plugin: "cursor", model: "claude-opus-5", role: "worker", profile: { display_name: "Pia", title: "Cursor · 前端", color: "teal" } });
    state.agents.push(cursor);
    const detail = state.runs[0];
    detail.run.config.team = [...detail.run.config.team, cursor.id];
    // The same PLAN objects are shared by every scenario: replace, never edit them.
    detail.run.plan = detail.run.plan.map((t, i) => (i === 0 ? { ...t, assignee: "pia" } : t));
    state.sessions = state.agents
      .filter((a) => a.id !== lead.id)
      .map((a) => ({ agent: a, state: "idle", busy_with: null, last_execution: null, can_use: true, reason: null, unguarded: false }));
    // A second plan waits in the same workspace under a different policy: only naming this
    // plan tells the hub which policy its candidates would be assigned under (and this run
    // is the guard one, so without `run=` Pia would look usable and nothing here greys out).
    const sam = state.agents.find((a) => a.name === "Sam")!;
    const mia = state.agents.find((a) => a.name === "Mia")!;
    const other = run({ title: "登录页改版", request: "登录页改版：布局和错误提示文案。", planner: sam, worker: mia, team: [mia], status: "awaiting_approval", plan: [planned("a", "登录页布局", "把表单和错误提示重新排一遍。", "mia")], minutesAgo: 18 });
    other.config.policy.approvals = "off";
    state.runs.push({ run: other, tasks: [] });
    const reason = "Cursor cannot use this permission on Windows";
    const verify = () => {
      const item = [...document.querySelectorAll<HTMLButtonElement>(".who-pick-menu .menu-item")].find((b) => b.textContent?.includes(reason));
      if (!item) throw new Error("the unusable candidate is not in the executor menu");
      if (!item.disabled) throw new Error("the unusable candidate can still be picked");
      if (!item.title?.includes(reason)) throw new Error("the unusable candidate has no tooltip");
      const row = document.querySelector<HTMLElement>(".plan-dock .plan-refused");
      if (!row?.textContent?.includes(reason)) throw new Error("the refused task has no reason on its row");
      if (!row.title.includes(reason)) throw new Error("the refused task has no tooltip");
      console.info("cursor guard run: unusable candidate and refusal verified");
    };
    return {
      ...scene,
      route: `#/s/${lead.id}`,
      script: [
        // Approve as it is: the hub refuses task a, and the reason lands on that row.
        { at: 500, act: () => document.querySelector<HTMLButtonElement>(".plan-dock footer button.primary")?.click() },
        // Then open the executor menu on the same task: the candidate is there, greyed out.
        { at: 1100, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="a"] .chip-pick')?.click() },
        { at: 1400, act: verify },
      ],
    };
  } },
  { key: "permission-card", label: "Cursor 的权限提醒", hint: "Agents 页：Windows 上权限不是「自动执行」的 Cursor 卡片写着 hub 的原因，旁边是直接打开它权限设置的链接", build: () => {
    const scene = oauthRun("plan");
    const cursor = agent({ name: "pia", plugin: "cursor", model: "claude-opus-5", role: "worker", profile: { display_name: "Pia", title: "Cursor · 前端", color: "teal" } });
    cursor.config = { permission: "deny" };
    scene.state.agents.push(cursor);
    const verify = () => {
      const link = [...document.querySelectorAll<HTMLButtonElement>(".member-unusable button")].find((b) => b.textContent?.includes("去设置权限"));
      if (!link) throw new Error("the unusable card has no link to the permission setting");
      console.info("the Cursor unusable card links to the permission setting");
    };
    return {
      ...scene,
      route: "#/agents",
      script: [
        { at: 700, act: () => document.querySelector(".member.unusable")?.scrollIntoView({ block: "center" }) },
        { at: 1200, act: verify },
      ],
    };
  } },
  { key: "permission", label: "每个 agent 的权限", hint: "Agents 页：Pia 的权限不是自动执行，Windows 上的 Cursor 就用不了——卡片写着原因，点「去设置权限」直接打开它的权限设置；Cursor 的「危险操作要我确认」置灰并写着 hub 的理由", build: () => {
    const scene = oauthRun("plan");
    const state = scene.state;
    const cursor = agent({ name: "pia", plugin: "cursor", model: "claude-opus-5", role: "worker", profile: { display_name: "Pia", title: "Cursor · 前端", color: "teal" } });
    // Not bypass: on Windows this machine cannot give it a usable session, so the card says why
    // and links to the choice that fixes it.
    cursor.config = { permission: "deny" };
    state.agents.push(cursor);
    const verify = () => {
      const group = document.querySelector<HTMLElement>(".permission-picks");
      if (!group) throw new Error("the permission setting is not in the editor");
      const radios = [...group.querySelectorAll<HTMLInputElement>('input[type="radio"]')];
      const guard = radios.find((i) => i.closest("label")?.textContent?.includes("危险操作要我确认"));
      if (!guard) throw new Error("the guard choice is missing");
      if (!guard.disabled) throw new Error("Cursor can still choose guard on Windows");
      if (!group.textContent?.includes("Cursor cannot ask before each action on Windows")) throw new Error("the greyed choice has no reason");
      const chosen = radios.find((i) => i.checked)?.closest("label")?.textContent ?? "";
      if (!chosen.includes("拒绝")) throw new Error("the agent's stored permission is not shown as chosen");
      console.info("permission setting: Cursor guard greyed out with the hub's reason");
    };
    return {
      ...scene,
      route: "#/agents",
      script: [
        { at: 700, act: () => document.querySelector(".member.unusable")?.scrollIntoView({ block: "center" }) },
        { at: 1000, act: () => {
          const link = [...document.querySelectorAll<HTMLButtonElement>(".member-unusable button")].find((b) => b.textContent?.includes("去设置权限"));
          if (!link) throw new Error("the unusable card has no link to the permission setting");
          link.click();
        } },
        { at: 1500, act: verify },
      ],
    };
  } },
  { key: "home-chat-permission", label: "新 Chat 的权限", hint: "首页新 Chat：输入框旁选「自动执行」，发送时写进新 agent 的 config.permission；这台 Windows 上的 Cursor 不能选「要我确认」", build: () => {
    const state = emptyHub();
    localStorage.setItem("crossagents.mode", JSON.stringify("chat"));
    const control = () => document.querySelector<HTMLSelectElement>(".home-composer .permission-compact select");
    const verify = () => {
      const made = state.agents[0];
      if (!made) throw new Error("the chat did not create an agent");
      if ((made.config as Record<string, unknown> | null)?.permission !== "bypass") throw new Error("the chosen permission did not reach the new agent");
      if (control()?.value !== "bypass") throw new Error("the pick does not show the choice");
      console.info("new chat: bypass written into config.permission");
    };
    return {
      state,
      route: "#/",
      script: [
        { at: 400, act: () => setSelect(control(), "bypass") },
        { at: 600, act: () => typeText(".home-composer textarea", "给登录页加一个错误提示") },
        { at: 800, act: () => document.querySelector<HTMLButtonElement>(".home-composer .send")?.click() },
        { at: 1900, act: verify },
      ],
    };
  } },
  { key: "home-work-permission", label: "Work 默认 Worker 的权限", hint: "首页 Work 的 ⚙：默认 Worker 旁选「拒绝」，发起前落到这个 worker 自己身上（mock 里 run 建不出来，权限设置已经写出）", build: () => {
    const { ada, rex, mia, sam } = team();
    const state: MockState = { ...emptyHub(), agents: [ada, rex, mia, sam], quota: quota() };
    localStorage.setItem("crossagents.mode", JSON.stringify("run"));
    localStorage.setItem("crossagents.workspace", wsKey(REPO));
    localStorage.removeItem("crossagents.lead");
    const worker = () => document.querySelectorAll<HTMLSelectElement>(".home-options select")[1];
    const verify = () => {
      if ((rex.config as Record<string, unknown> | null)?.permission !== "deny") throw new Error("the default worker's permission did not reach the agent");
      console.info("work: the default worker's permission reached the agent");
    };
    return {
      state,
      route: "#/",
      script: [
        { at: 400, act: () => document.querySelector<HTMLButtonElement>(".options-btn")?.click() },
        { at: 700, act: () => setSelect(worker(), "Rex") },
        { at: 900, act: () => setSelect(document.querySelector<HTMLSelectElement>(".home-options .permission-compact select"), "deny") },
        { at: 1100, act: () => typeText(".home-composer textarea", "把登录页的错误提示改成人话") },
        { at: 1300, act: () => document.querySelector<HTMLButtonElement>(".home-composer .send")?.click() },
        { at: 2300, act: verify },
      ],
    };
  } },
  { key: "plan-permission", label: "计划里给任务改权限", hint: "计划面板：每个任务的执行者旁有权限；在这条守卫 run 里给 #a 选「自动执行」，这一行立刻标出「不经确认」，批准前写进执行者自己的设置", build: () => {
    const scene = oauthRun("plan");
    const state = scene.state;
    const rex = state.agents.find((a) => a.name.toLowerCase() === "rex")!;
    const rowPick = () => document.querySelector<HTMLSelectElement>('.who-pick[data-task="a"] .permission-compact select');
    const verifyRow = () => {
      const row = document.querySelector<HTMLElement>('.who-pick[data-task="a"]');
      const chip = row && [...row.querySelectorAll<HTMLElement>(".chip")].find((c) => c.textContent?.includes("不经确认"));
      if (!chip) throw new Error("bypass does not mark the row as 不经确认");
      if (!chip.title.includes("不经确认")) throw new Error("the mark has no tooltip");
      console.info("plan: the row marks a bypassed assignee as 不经确认");
    };
    const verifyAgent = () => {
      if ((rex.config as Record<string, unknown> | null)?.permission !== "bypass") throw new Error("the row's choice did not reach the assignee");
      console.info("plan: the row's permission reached the assignee");
    };
    return {
      ...scene,
      route: `#/runs/${state.runs[0].run.id}`,
      script: [
        { at: 900, act: () => setSelect(rowPick(), "bypass") },
        { at: 1200, act: verifyRow },
        { at: 1500, act: () => document.querySelector<HTMLButtonElement>(".plan-dock footer button.primary")?.click() },
        { at: 2500, act: verifyAgent },
      ],
    };
  } },
  { key: "plan-permission-guard", label: "计划里 Cursor 不能选「要我确认」", hint: "计划面板：执行者是 Windows 上的 Cursor 时，这一行的「要我确认」置灰，旁边写明原因——和 Agents 页同一条规则", build: () => {
    const scene = oauthRun("plan");
    const state = scene.state;
    const pia = agent({ name: "pia", plugin: "cursor", model: "claude-opus-5", role: "worker", profile: { display_name: "Pia", title: "Cursor · 前端", color: "teal" } });
    state.agents.push(pia);
    const detail = state.runs[0];
    detail.run.config.team = [...detail.run.config.team, pia.id];
    detail.run.plan = detail.run.plan.map((t, i) => (i === 0 ? { ...t, assignee: "pia" } : t));
    state.sessions = [
      { agent: pia, state: "idle", busy_with: null, last_execution: null, can_use: true, reason: null, unguarded: false },
      ...state.agents.filter((a) => a.id !== pia.id && a.id !== detail.run.planner).map((a) => ({ agent: a, state: "idle" as const, busy_with: null, last_execution: null, can_use: true, reason: null, unguarded: false })),
    ];
    const verify = () => {
      const wrap = document.querySelector<HTMLElement>('.who-pick[data-task="a"] .permission-compact');
      const select = wrap?.querySelector<HTMLSelectElement>("select");
      if (!select) throw new Error("the Cursor row has no permission pick");
      const guard = [...select.options].find((o) => o.textContent?.includes("要我确认"));
      if (!guard) throw new Error("the guard choice is missing");
      if (!guard.disabled) throw new Error("Cursor can still choose guard on Windows");
      if (!wrap!.textContent?.includes("Windows 上的 Cursor 不能选")) throw new Error("the greyed choice has no reason beside it");
      console.info("plan: the compact guard choice is greyed with its reason");
    };
    return {
      ...scene,
      route: `#/runs/${detail.run.id}`,
      script: [{ at: 1200, act: verify }],
    };
  } },
  { key: "plan-unguarded", label: "不经确认的候选", hint: "Plan 的执行者菜单：这次 run 要求确认，但显式设成自动执行的候选带「不经确认」，tooltip 说明它会直接干", build: () => {
    const scene = oauthRun("plan");
    const state = scene.state;
    const lead = state.agents.find((a) => a.name === "Ada")!;
    const byName = (n: string) => state.agents.find((a) => a.name.toLowerCase() === n)!;
    const rex = byName("rex");
    const mia = byName("mia");
    // Explicit bypass: in this guard run these two act without asking, and the hub says so.
    rex.config = { ...(rex.config as object), permission: "bypass" };
    mia.config = { ...(mia.config as object), permission: "bypass" };
    state.sessions = [
      { agent: rex, state: "idle", busy_with: null, last_execution: 'run "多租户隔离" #a (done)', can_use: true, reason: null, unguarded: true },
      { agent: mia, state: "idle", busy_with: null, last_execution: null, can_use: true, reason: null, unguarded: true },
      { agent: byName("sam"), state: "idle", busy_with: null, last_execution: null, can_use: true, reason: null, unguarded: false },
    ];
    const verify = () => {
      const rows = [...document.querySelectorAll<HTMLButtonElement>(".who-pick-menu .menu-item")];
      const marked = rows.filter((r) => r.textContent?.includes("不经确认"));
      // Rex (a teammate), Mia, and the auto row (the run's default worker is Rex).
      if (marked.length !== 3) throw new Error(`expected 3 unguarded candidates, found ${marked.length}`);
      for (const row of marked) {
        const chip = row.querySelector<HTMLElement>(".menu-label .chip");
        if (!chip?.title.includes("不经确认")) throw new Error("the unguarded mark has no tooltip");
      }
      console.info("unguarded candidates are marked with their tooltip");
    };
    return {
      ...scene,
      route: `#/s/${lead.id}`,
      script: [
        { at: 800, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="a"] .chip-pick')?.click() },
        { at: 1300, act: verify },
      ],
    };
  } },
  { key: "unusable-model", label: "账号不再提供的模型", hint: "模型已经不在账号的名单里：Agents 页把那张卡片置灰，行内和 tooltip 都写原因；在会话里 @ 它同样置灰、点不了（用 --route 打开会话看 @ 菜单）", build: () => {
    const scene = soloSession();
    const state = scene.state;
    const hazel = agent({ name: "hazel", plugin: "with", model: "deepseek-v4.1-flash-sg", role: "worker", cwd: NOTES_REPO, profile: { display_name: "Hazel", title: "后端", color: "amber" } });
    state.agents.push(hazel);
    const why = "the account no longer offers deepseek-v4.1-flash-sg: the gateway answers 400 service info not found. Pick another model or another account.";
    state.unusable = { [hazel.id]: why };
    // `@` opens the mention menu: the same row greys out for the same reason. On the Agents
    // page (the default route) there is no composer, so this only runs for the session shot.
    const typeAt = () => {
      const el = document.querySelector<HTMLTextAreaElement>(".composer textarea");
      if (!el) return;
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(el, "@");
      el.dispatchEvent(new Event("input", { bubbles: true }));
    };
    const verify = () => {
      const card = document.querySelector<HTMLElement>(".member.unusable .member-unusable");
      const row = document.querySelector<HTMLButtonElement>(".mention-menu .menu-item.disabled");
      if (location.hash.startsWith("#/agents")) {
        if (!card) throw new Error("the stale-model agent is not greyed out on the Agents page");
        if (card.title !== why || !card.textContent?.includes(why)) throw new Error("the Agents page card has no reason");
      } else if (document.querySelector(".composer textarea")) {
        if (!row) throw new Error("the stale-model agent is not greyed out in the @ menu");
        if (!row.disabled || row.title !== why) throw new Error("the @ menu row has no reason or is still pickable");
      }
    };
    return { ...scene, route: "#/agents", script: [
      // The CLI list comes first; bring the greyed card into view at any width.
      { at: 600, act: () => document.querySelector(".member.unusable")?.scrollIntoView({ block: "center" }) },
      { at: 900, act: typeAt },
      { at: 1300, act: verify },
    ] };
  } },
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
        { at: 700, act: () => document.querySelector('.who-pick[data-task="i"]')?.scrollIntoView({ block: "center" }) },
        { at: 1000, act: () => document.querySelector<HTMLButtonElement>('.who-pick[data-task="i"] .chip-pick')?.click() },
      ],
    };
  } },
  { key: "running", label: "run 进行中", hint: "run 页：中间栏只有对话；右侧栏按任务分组（含交接链）、run 信息、策略、笔记，追溯和参与者收起", build: () => oauthRun("running") },
  // B3 (docs/tasks/SELF.md): the run page keeps this run's 「告诉你」 notices; the workspace has
  // another run's too, and that one must not appear below the conversation.
  { key: "run-notices", label: "run 页只显示本 run 的通知", hint: "工作区里还有另一个 run 的「告诉你」：这个 run 页只列本 run 的那一条", build: () => {
    const scene = oauthRun("running");
    const d = scene.state.runs[0];
    const notice = (from: string, run: string | null, text: string, minutesAgo: number): HumanItem => ({
      id: uid(), kind: "notice", from, run_id: run, task_id: null, text, detail: null, options: [], default: null,
      timeout_at: null, status: "open", answer: null, answered_at: null, action: null, created_at: ago(minutesAgo),
    });
    const someoneElse = scene.state.agents.find((a) => a.id !== d.run.planner)?.id ?? d.run.planner;
    scene.state.questions.push(
      notice(d.run.planner, d.run.id, "这个 run 的「告诉你」：落地前我会把 run 分支上的提交按任务归好，你只要看一眼总 diff。", 3),
      notice(someoneElse, uid(), "别的 run 的「告诉你」：这条只该出现在首页，不该出现在这个 run 页。", 2),
    );
    return scene;
  } },
  // A long 「告诉你」 (a run summary) above the composer: three lines, 展开全文, same column as the composer.
  { key: "long-notice", label: "很长的「告诉你」", hint: "会话页输入框上方的通知只显示三行，点「展开全文」看全部；卡片和输入框同宽", build: () => {
    const scene = soloSession();
    const nova = scene.state.agents[0];
    const text = Array.from({ length: 8 }, (_, i) => `第 ${i + 1} 段：已落地到 main（squash 2342d0b，5 项检查全过）。做了什么、没做什么、要你看什么，这一段写得很长很长，用来撑满好几行，看卡片会不会把整个对话挤出屏幕。`).join("\n");
    scene.state.questions.push({
      id: uid(), kind: "notice", from: nova.id, run_id: null, task_id: null, text, detail: null, options: [], default: null,
      timeout_at: null, status: "open", answer: null, answered_at: null, action: null, created_at: ago(1),
    });
    return { ...scene, route: `#/s/${nova.name}` };
  } },
  { key: "long-notice-open", label: "很长的「告诉你」展开后", hint: "展开全文后卡片最高约半屏，多出来的在卡片区里滚动，不把对话挤没", build: () => {
    const scene = SCENARIOS.find((x) => x.key === "long-notice")!.build();
    return { ...scene, script: [{ at: 600, act: () => document.querySelector<HTMLButtonElement>(".ask-more")?.click() }] };
  } },
  { key: "capability-no-turns", label: "没有对话的 agent 也报能力", hint: "这个 codex 队友在 mock 里没开过会话：输入框下面照样写明只收图片和文本，PDF 发不了（codex-native）", build: () => {
    const scene = soloSession();
    const nova = scene.state.agents[0];
    // No transcript: the mock's fallback is what the hub answers for a session never opened.
    const codex = { ...nova, plugin: "codex" as const, model: "gpt-5-codex", status: "idle" as const, topic: null };
    scene.state.agents = [codex];
    scene.state.transcripts.clear();
    return { state: scene.state, script: [], route: `#/s/${codex.name}` };
  } },
  { key: "run-cost", label: "run 的费用条", hint: "右侧栏「费用」：一根横条按 agent 与模型分段，同一 agent 的模型深浅分开；悬停一段看金额和 token，下面一行一段", build: () => runCostScene() },
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
  { key: "background", label: "后台看板", hint: "run 页工具列的「后台」：这个 run 的提醒（倒计时、备注、关联任务，可取消）、关注和验收中的 #c 排在前面；Nova 在别处的提醒和关注折叠在下面；约 8 秒后 Rex 的一条提醒到期，从看板上消失", build: () => backgroundScene() },
  { key: "activity", label: "活动流", hint: "工具列的「活动」：所有 agent 的回复、工具和消息排成一条时间线；类型和范围（某个 agent 及它的下属）可筛，点一条打开对应会话并定位到那一轮", build: () => activityScene() },
  { key: "inplace", label: "无 git 的 run", hint: "文件夹没有 git：run 直接改文件、一次一个任务，可一键初始化 git", build: () => inPlaceRun() },
  { key: "workcard", label: "会话提议 Work", hint: "Nova 在单聊里用 work_start 提议到独立分支上做：卡片可展开交给 Work 的说明，开始 / 不用", build: () => workProposal() },
  { key: "long-results", label: "长结果折叠", hint: "Ada 第一次落地被拒、理由里是整段 cargo test 日志：卡片只留一行（✕ · 落地 run · 对象和理由第一句），点图标或整行看完整理由和调用参数；run 页的总结同样只留一行", build: () => longResultsRun() },
  { key: "long-results-run", label: "长总结折叠（run 页）", hint: "完成卡片里的总结只留第一行，点图标或整行展开看完整总结", build: () => { const s = longResultsRun(); return { ...s, route: `#/runs/${s.state.runs[0].run.id}` }; } },
  { key: "delete-landed", label: "删除已落地的 run", hint: "删除确认框：主导和它的对话一起删，工作区和分支，已落地所以分支默认勾选", build: () => deleteRun("landed") },
  { key: "delete-unlanded", label: "删除未落地的 run", hint: "未落地：分支默认不勾，写明「未落地的改动会丢失」", build: () => deleteRun("unlanded") },
  { key: "delete-dirty", label: "删除时工作区有改动", hint: "点「删除」后 hub 返回 409 并列出没提交的文件，可以「仍然删除」", build: () => deleteRun("dirty") },
  { key: "files", label: "文件链接与查看器", hint: "回复里的路径（确认存在的才成链接）可点开只读查看器：左侧懒加载目录树，右侧行号、高亮、跳到第 12 行", build: () => filesScene() },
  { key: "files-markdown", label: "文件查看器（Markdown）", hint: "中间弹窗里的 Markdown 也有同一套大纲：短横线、悬停标题列表、当前章节", build: () => filesMarkdownScene() },
  { key: "terminal", label: "内置终端", hint: "对话下方的终端标签：回显输入；标签可拖到右侧，Ctrl+` 收起和打开，输入 exit 看退出后的样子", build: () => ({ ...filesScene(), script: [{ at: 600, act: () => void newTerminal() }] }) },
  // The built-in browser (run note browser/contract §3, the page half): two tabs, one per dock,
  // and an agent's request on the second, so its「正在操作」bar and 接管 are both on show.
  { key: "browser", label: "内置浏览器", hint: "右侧和底部各一个浏览器标签（可拖动、可关、可多开）：地址栏回车打开、后退/前进/刷新、加载状态，标签名取页面标题；底部那个写着 Nova 正在操作，可以「接管」，接管后 agent 的请求会被拒绝", build: () => {
    const scene = soloSession();
    let pricing = "";
    return {
      state: { ...scene.state, browserDelay: 30_000 },
      route: scene.route,
      script: [
        { at: 300, act: () => {
          // The preview keeps the dock layout between scenarios: start from two browser tabs only.
          const docks = docksNow();
          for (const tab of [...docks.right.tabs, ...docks.bottom.tabs]) shutTab(tab);
          openBrowserTab({ url: "https://developer.mozilla.org/zh-CN/docs/Web/HTML", dock: "right" });
        } },
        { at: 600, act: () => { pricing = openBrowserTab({ url: "https://example.com/pricing", dock: "bottom" }).id; } },
        { at: 1100, act: (_s, bridge) => bridge.emitBrowserRequest("browser_snapshot", { tab: pricing }, "Nova") },
      ] as Step[],
    };
  } },
  { key: "files-pane", label: "文件面板", hint: "工具列的「文件」：目录树，点开 docs/cli.md 是渲染后的 Markdown，可切回源码；文档里的相对链接在面板里打开", build: () => filesPaneScene() },
  { key: "files-pane-narrow", label: "文件面板（窄）", hint: "窄面板退回树和文件轮流显示；标签、面包屑和大纲都不溢出", build: () => filesPaneNarrow() },
  { key: "files-pane-wide", label: "文件面板（宽）", hint: "宽面板：左侧内容和标签、面包屑、大纲，右侧整棵文件树和过滤框；docs 与 crates 有改动标记", build: () => filesPaneWide() },
  { key: "files-pane-bottom", label: "文件面板（底部 dock）", hint: "文件面板拖到对话下方的底部 dock，宽度足够时仍左右并排", build: () => filesPaneBottom() },
  { key: "files-pane-search", label: "文件面板搜索", hint: "在过滤框输入 main，搜到未展开的 crates/cli/src/main.rs", build: () => filesPaneSearch() },
  { key: "files-pane-outline", label: "文件面板大纲", hint: "聚焦大纲的一条短横线，弹出完整标题列表，当前章节高亮", build: () => filesPaneOutline() },
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
  { key: "hooks", label: "hooks 默认关闭", hint: "设置 → 智能体：点 Nova 编辑，设置源下面的「允许 hooks（默认关闭）」没勾；勾上才会跑设置文件里的 hooks（仓库 .claude/settings.json 也算）", build: settingSources },
  { key: "recruit", label: "对话招队友", hint: "对话里招人：一次因为用模型名当名字被拒绝（一行写明对象和理由第一句，点 ✕ 看完整理由），一次招到 Iris，挂在对话下面", build: () => chatWithRecruit() },
  { key: "crash", label: "进程崩溃", hint: "单聊里 Claude CLI 崩溃退出：错误条和「需要你」写明怎么退出的", build: () => crashedSession() },
  { key: "moderation", label: "上下文被内容审查拦下", hint: "网关拒绝整段上下文：错误行写「上下文已被污染」，输入框上方给「新开会话」「压缩上下文」「仍要发送」", build: () => moderationSession() },
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
  { key: "mismatch", label: "版本不匹配", hint: "hub 的接口是 v1：GUI 拒绝连接并说明原因", build: () => ({ state: { ...emptyHub(), api: "v1" }, script: [], route: "#/" }) },
  { key: "usage-30", label: "用量：30 天", hint: "切换至 30 天", build: () => ({ state: usageScene(), route: "#/usage", script: [{ at: 400, act: () => [...document.querySelectorAll("button")].find((b) => b.textContent === "30 天")?.click() }] }) },
  { key: "usage-all", label: "用量：全部", hint: "切换至全部", build: () => ({ state: usageScene(), route: "#/usage", script: [{ at: 400, act: () => [...document.querySelectorAll("button")].find((b) => b.textContent === "全部")?.click() }] }) },
  { key: "usage-hover", label: "用量：每日明细", hint: "聚焦日期", build: () => ({ state: usageScene(), route: "#/usage", script: [{ at: 1200, act: () => document.querySelector<SVGElement>(".usage-svg rect[tabindex]")?.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })) }] }) },
  { key: "usage-empty", label: "用量：无数据", hint: "没有本地日志", build: () => ({ state: usageScene("empty"), script: [], route: "#/usage" }) },
  { key: "usage-issues", label: "用量：来源错误", hint: "未计价模型及来源读取失败", build: () => ({ state: usageScene("issues"), script: [], route: "#/usage" }) },
  { key: "usage-session", label: "用量：会话口径", hint: "无法确定单次请求", build: () => ({ state: usageScene("session"), script: [], route: "#/usage" }) },
  { key: "usage", label: "用量", hint: "两个账号，一个快到 5 小时限额，一个每周限额用完", build: () => ({ state: usageScene(), script: [], route: "#/usage" }) },
  { key: "expertise", label: "专长档案", hint: "个人/项目档案、超预算与黄红标记、带冲突的候选；脚本：派活推送 → 交活填结果 → 返工出候选 → 接受 → 专长卡变化", build: () => ({ ...expertiseScene(), route: "#/expertise" }) },
  { key: "lessons", label: "排障记录", hint: "项目的排障记录：搜索、按状态筛选、展开看试用记录与来源；含待复核与已被替换的记录", build: () => { const { state, script } = lessonsScene(); return { state, script, route: "#/lessons" }; } },
  { key: "lessons-run", label: "运行里的问题", hint: "run 页：记下的问题及其整理结果、未整理的「交给…整理」，失败任务附「可能相关」的排障记录；脚本：又记下一条问题", build: () => { const { state, script, runId } = lessonsScene(); return { state, script, route: `#/runs/${runId}` }; } },
  { key: "lessons-task", label: "失败任务的相关记录", hint: "失败任务详情：附「可能相关」的排障记录与命中的词", build: () => { const { state, runId, failedTask } = lessonsScene(); return { state, script: [], route: `#/runs/${runId}/tasks/${failedTask}` }; } },
  { key: "lessons-open", label: "排障记录：展开", hint: "展开待复核的一条：现象、条件、处理、每次试用的结果与来源", build: () => { const { state } = lessonsScene(); return { state, route: "#/lessons", script: [{ at: 800, act: () => document.querySelectorAll<HTMLElement>(".lesson-head")[3]?.click() }] }; } },
  { key: "lessons-projects", label: "项目绑定与合并", hint: "排障记录页底部：项目列表、身份待确认的一条、按 remote 或目录绑定、把两个项目合并", build: () => { const { state } = lessonsScene(); return { state, route: "#/lessons", script: [{ at: 600, act: () => document.querySelector(".project-panel")?.scrollIntoView({ block: "center" }) }] }; } },
  { key: "cli-install-ok", label: "安装 CLI 成功", hint: "设置 → 智能体：CodeBuddy 没装，点「安装」先看确认框里的命令和来源；执行后输出滚动，装完提示去登录账号", build: () => cliScene("ok") },
  { key: "cli-install-fail", label: "安装 CLI 失败", hint: "安装命令以非 0 退出：滚动的输出和写明退出码的原因", build: () => cliScene("fail") },
  { key: "cli-install-missing", label: "装完但找不到", hint: "命令成功，但这台机器上找不到装好的程序：原因里列出找过的位置", build: () => cliScene("missing") },
  { key: "cli-upgrade", label: "升级 CLI", hint: "Codex 是 npm 装的：点「升级」，完成后版本号变化", build: () => cliScene("ok", "upgrade") },
  { key: "cli-bundled", label: "桌面版自带不可升级", hint: "Claude 来自桌面版：显示「随桌面版更新」，没有升级按钮", build: () => cliScene("ok", "install", false) },
  { key: "cli-versions", label: "CLI 版本过低与未验证", hint: "设置 → 智能体：Codex 低于描述文件的最低版本（不注册、不提供模型），With 的版本不在验证过的范围内（仍可运行），Claude 在范围内", build: () => ({ state: emptyHub(), script: [], route: "#/agents" }) },
  { key: "agent-workspace-sidebar", label: "项目侧栏：成员与待办", hint: "项目 → 成员（Lin 已归档），独立会话，「需要你」两条（都是 Mia 的）；Rex 既是 #a 的执行者又是 #b 的评审，只出现一行", build: () => ({ state: workspaceState(), script: [], route: "#/" }) },
  { key: "agent-workspace-overview", label: "项目总览", hint: "点项目名进入总览：待审批、进行中、旧的已落地 run 各一个；Nova-c 是用 Nova 的配置新建的执行会话，#a 是 Rex 本人", build: () => ({ state: workspaceState(), script: [], route: `#/w/${encodeURIComponent(wsKey(REPO))}` }) },
  { key: "agent-workspace-edges", label: "边界 run", hint: "NOTES_REPO：失败的 run 和完成但未落地的 run（不能显示成完成）；REPO 里还有没提交 plan 的登录限流和来源已归档的多租户隔离", build: () => ({ state: workspaceState(), script: [], route: `#/w/${encodeURIComponent(wsKey(NOTES_REPO))}` }) },
  { key: "agent-workspace-history", label: "项目历史：删掉的 run", hint: "工作区「历史」：删掉的两轮标「已删除」、只读，摘要和任务仍在；没删的已结束 run 从这里也能打开", build: () => {
    localStorage.setItem("crossagents.workspace.tab", "history");
    return { state: workspaceState(), script: [], route: `#/w/${encodeURIComponent(wsKey(REPO))}` };
  } },
  { key: "agent-workspace-narrow", label: "窄窗口与长标题", hint: "Atlas 的长话题和 #b 的长中文标题：226px 侧栏下名字、状态和摘要仍可辨认，截断不挤坏布局；窄窗口请用截图调整宽度", build: () => ({ state: workspaceState(), script: [], route: "#/s/Atlas" }) },
  { key: "agent-workspace-auto", label: "自动审批：不出现待批卡片", hint: "三个 run 都在 awaiting_approval：登录页改版是我审批（进「需要你」），另两个是半托管与 auto_approve 的旧记录，都不该开审批卡片；Atlas 的会话里还该有一个完成未读（与待办分开计数）", build: () => {
    const state = workspaceState();
    const manual = state.runs.find((d) => d.run.status === "awaiting_approval");
    const atlas = state.agents.find((a) => a.name.toLowerCase() === "atlas");
    const iris = state.agents.find((a) => a.name.toLowerCase() === "iris");
    if (!manual || !atlas || !iris) throw new Error("agent-workspace-auto needs an awaiting run, Atlas and Iris");
    // The control plan stays in its lead's conversation; Atlas's session then holds only the
    // auto-approved records, so a card there can only be a phantom one.
    manual.run.origin_chat_id = null;
    // Two records the hub would not leave waiting: the plan policy is auto, or the legacy
    // auto_approve flag is set. They are planned by Atlas so its session has no manual plan to
    // show: any card there is the phantom one this scenario is about.
    const variant = (title: string, patch: (run: NonNullable<typeof manual>["run"]) => void) => {
      const copy = structuredClone(manual);
      copy.run.id = uid();
      copy.run.planner = atlas.id;
      copy.run.title = title;
      copy.run.branch = `crossagents/${title}`;
      copy.tasks = [];
      patch(copy.run);
      return copy;
    };
    const autoPolicy = variant("发布说明：半托管", (run) => {
      run.config.policy = { ...run.config.policy, plan_approval: "auto", land: "manual", human_ask: "timeout" };
    });
    const autoApprove = variant("发布说明：auto_approve", (run) => {
      run.config.auto_approve = true;
    });
    state.runs = [...state.runs, autoPolicy, autoApprove];
    // A turn the person asked for ends in a conversation they are not in: an unread mark, not a todo.
    const asked = uid();
    const script: Step[] = [
      { at: 650, event: { type: "agent.input", data: { id: iris.id, message_id: asked, text: "CSV 导出的编码问题查到了吗？", queued: true, from: { kind: "human" } } } },
      { at: 900, event: { type: "agent.turn_completed", data: { id: iris.id, message_id: asked, stop_reason: "end_turn" } } },
    ];
    return { state, script, route: `#/s/${atlas.id}` };
  } },
  { key: "extensions", label: "扩展：状态一栏", hint: "standup 有更新待人批准、changelog 从未启用、notes 停用了但可回到已批准的版本（还有一版等着）、big-export 的源目录超了 50 MB；下面是让 agent 做一个插件", build: () => {
    const state: MockState = { ...emptyHub(), ...extensionScene() };
    // Stopped, with a newer version waiting: 启用 re-enables the approved snapshot, 查看并启用 is
    // still the way to the waiting one.
    const notes = state.extensions?.find((x) => x.name === "notes");
    if (notes?.current) {
      notes.enabled = false;
      notes.pending = {
        version: { ...notes.current, fingerprint: "aa11bb22".repeat(8), description: "把对话里的决定记到 run 的笔记上，并带一份摘要", taken_at: "2026-11-05T10:05:00Z" },
        requested_at: "2026-11-05T10:05:03Z",
      };
    }
    return { state, script: [], route: "#/extensions" };
  } },
  { key: "extensions-approve", label: "扩展：批准界面", hint: "待批准的一版：工具、说明、面板、access 变化和与正在运行版本的差异，按钮提交的是卡片上印出的指纹", build: () => ({ state: { ...emptyHub(), ...extensionScene() }, script: [], route: "#/extensions/standup" }) },
  { key: "extensions-diff", label: "扩展：差异与批准按钮", hint: "滚到待批准的一版下半：逐条文件改动、unified diff，以及写着「批准并启用」的那一行", build: () => ({
    state: { ...emptyHub(), ...extensionScene() },
    script: [{ at: 900, act: () => document.querySelector('[data-approval="ready"] .ext-approve-actions')?.scrollIntoView({ block: "center" }) }],
    route: "#/extensions/standup",
  }) },
  { key: "extensions-approved", label: "扩展：批准之后", hint: "脚本点「批准并启用」：changelog 从「待批准」变成「已启用」，跑的就是刚才那一版（它没有面板，所以预览里不会去导入一个不存在的入口）", build: () => {
    const state: MockState = { ...emptyHub(), ...extensionScene() };
    const expected = state.extensions?.find((x) => x.name === "changelog")?.pending?.version.fingerprint;
    return {
      state,
      route: "#/extensions/changelog",
      script: [
        { at: 800, act: () => document.querySelector<HTMLButtonElement>('[data-action="approve"]')?.click() },
        {
          at: 1500,
          act: (s) => {
            const changelog = s.extensions?.find((x) => x.name === "changelog");
            if (!changelog?.enabled) throw new Error("changelog was not enabled after approving");
            if (changelog.pending) throw new Error("the pending record is still there after approving");
            if (changelog.current?.fingerprint !== expected) throw new Error("changelog still runs the old snapshot");
            console.info("extension approval verified: the shown fingerprint was approved");
          },
        },
      ],
    };
  } },
  { key: "extensions-stale", label: "扩展：待批准的一版被顶掉", hint: "看批准界面时源目录又变了：卡片改口说这一版已经不在待批准里、不再给按钮，只给「看新的一版」", build: () => ({
    state: { ...emptyHub(), ...extensionScene() },
    route: "#/extensions/standup",
    script: [
      {
        at: 900,
        act: (s, bridge) => {
          const standup = s.extensions?.find((x) => x.name === "standup");
          if (!standup?.pending) throw new Error("standup has nothing pending to replace");
          const version = { ...standup.pending.version, fingerprint: "c0ffee12".repeat(8), taken_at: new Date().toISOString() };
          standup.pending = { ...standup.pending, version, requested_at: new Date().toISOString() };
          bridge.emit({ type: "extension.changed", data: { name: "standup" } });
        },
      },
      {
        at: 1500,
        act: () => {
          if (!document.querySelector('[data-approval="stale"]')) throw new Error("the approval card did not go stale");
          if (document.querySelector('[data-action="approve"]')) throw new Error("the stale card still offers the approve button");
          console.info("stale approval verified: no button for a version that moved");
        },
      },
    ],
  }) },
  { key: "extensions-make-plugin", label: "扩展：让 agent 做一个插件", hint: "点「写进新对话」：开一个新对话，请求已经填进草稿（带 extension_api / extension_check / extension_request_enable），没有自动发送", build: () => ({
    state: { ...emptyHub(), ...extensionScene() },
    route: "#/extensions",
    script: [
      { at: 900, act: () => document.querySelector<HTMLButtonElement>('[data-action="make-plugin"]')?.click() },
      {
        at: 1800,
        act: () => {
          const text = document.querySelector<HTMLTextAreaElement>(".home-composer textarea")?.value ?? "";
          if (!text.includes("extension_api")) throw new Error("the request was not left in the draft");
          if (window.location.hash !== "#/") throw new Error(`the new conversation did not open: ${window.location.hash}`);
          console.info("plugin request verified: a draft, not a sent message");
        },
      },
    ],
  }) },
  { key: "extensions-safe", label: "扩展：安全模式", hint: "横幅写明为什么没加载扩展、怎么退出；列表里照样标出谁已启用、谁在等批准", build: () => ({
    state: { ...emptyHub(), ...extensionScene(), safeMode: { enabled: true, reason: "上次启动在加载扩展时未能就绪，已自动进入安全模式并跳过所有扩展。" } },
    script: [],
    route: "#/extensions",
  }) },
  { key: "hub-old", label: "旧 hub 0.2.5：没有功能清单", hint: "0.2.5 的 /v1/info 不带 features：Git、排障记录、工作组、应用到原目录都隐藏；侧栏底部和版本号 tooltip 写明「按版本推断」", build: () => {
    const scene = workRounds();
    return { ...scene, state: { ...scene.state, features: null, version: "0.2.5" } };
  } },
  { key: "hub-old-0.2.6", label: "旧 hub 0.2.6：只藏较晚的功能", hint: "0.2.6 的 /v1/info 不带 features，但 0.2.6 已有 Git 页和排障记录：只隐藏工作组、应用到原目录，提示写明是按版本推断的", build: () => {
    const scene = workRounds();
    return { ...scene, state: { ...scene.state, features: null, version: "0.2.6" } };
  } },
  { key: "hub-new", label: "新 hub：功能清单", hint: "新 hub 列出 features：Git、排障记录、工作组入口都在，工作组照常列出", build: () => {
    const scene = workRounds();
    return { ...scene, state: { ...scene.state, features: [...FEATURES] } };
  } },
  { key: "hub-old-apply", label: "旧 hub：没有「应用到原目录」", hint: "0.2.6 的 hub 不带功能清单，应用到原目录在 0.2.6 之后才有、从不按版本推断，所以仍隐藏；Git 和排障记录照常", build: () => {
    const scene = inPlaceRun();
    // The apply control also needs a git working copy in place (LEAD.md §11): the mock's
    // in-place run is a plain folder, so say it is a repository to put the button on screen.
    scene.state.runs[0].run.mode = { lead: "in_place", vcs: "git", owner: "person", retain: "keep" };
    return { ...scene, state: { ...scene.state, features: null, version: "0.2.6" } };
  } },
  { key: "hub-refused", label: "403：入口置灰", hint: "hub 用 403 拒绝（凭据级别不够）：「应用到原目录」变灰，用 hub 的话说明原因，不弹报错", build: () => {
    const scene = inPlaceRun();
    scene.state.runs[0].run.mode = { lead: "in_place", vcs: "git", owner: "person", retain: "keep" };
    return {
      ...scene,
      state: {
        ...scene.state,
        features: [...FEATURES],
        forbidden: { apply: "this credential has the read level; POST /v1/runs/…/apply needs write" },
      },
    };
  } },
  { key: "hub-refused-git", label: "403：Git 入口置灰", hint: "Git 页的请求被 403 拒绝：侧栏 Git 变灰、页面写明 hub 的原因，不弹报错", build: () => {
    const scene = workRounds();
    return {
      ...scene,
      route: "#/git",
      state: {
        ...scene.state,
        features: [...FEATURES],
        forbidden: { git: "an agent's session token reaches /v1/me only; GET /v1/git/branches needs the read level" },
      },
    };
  } },
  { key: "devices", label: "设置：设备", hint: "hub 签发过的设备：客户端和节点各一台，创建与最后在线按项目的时间规则；吊销过的那台淡显、排最后并写明吊销时间；点一次吊销会先变红说「再点一次吊销」", build: () => ({
    state: { ...emptyHub(), devices: devices() },
    script: [],
    route: "#/settings/devices",
  }) },
  { key: "devices-refused", label: "设置：设备（只读设备）", hint: "凭据不到 admin：点两次吊销被 403 拒绝后，吊销按钮置灰，页面上写明缺「管理」级别和 hub 的原话；列表照常看得见，入口也不置灰", build: () => ({
    state: {
      ...emptyHub(),
      devices: devices(),
      revokeForbidden: "this credential has the read level; DELETE /v1/devices/{id} needs admin",
      connection: { paired: { host: "centre.example", user: "me" } },
    },
    script: [
      { at: 1500, act: () => document.querySelector<HTMLButtonElement>('[data-action="revoke"]')?.click() },
      { at: 1800, act: () => document.querySelector<HTMLButtonElement>('button[data-action="revoke"].armed')?.click() },
    ],
    route: "#/settings/devices",
  }) },
  { key: "connection-local", label: "连接：本机（SSH）", hint: "设置 →「连接」→「连到别的电脑上的中心」→ SSH：现在连的是本机，可以在这里填中心的主机配对；下面写明配对拿到的是 write 设备，终端、批准扩展和改 agent 权限要 admin", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: {} },
      script: [
        { at: 300, act: () => document.querySelector<HTMLButtonElement>('[data-action="goal-join"]')?.click() },
        { at: 400, act: () => document.querySelector<HTMLButtonElement>('[data-action="method-ssh"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-host", label: "连接：当中心（内置打洞）", hint: "本机有 hub、没配对过：默认就是「让别的电脑连进来」。点「开启」后先是「连接中继中」，一会儿变「在线」；再点「生成配对码」，第三步出现配对码、复制按钮和倒计时", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: {}, link: { startMs: 1500, proxy: "http://127.0.0.1:7890" } },
      script: [
        { at: 600, act: () => document.querySelector<HTMLButtonElement>('[data-action="link-on"]')?.click() },
        { at: 3200, act: () => document.querySelector<HTMLButtonElement>('[data-action="link-invite"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-host-details", label: "连接：当中心（详情与完整配对码）", hint: "在线、生成过配对码后：展开「详情」看中继、代理和节点 id，点「显示全部」看完整配对码", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: {}, link: { enabled: true, startMs: 0, proxy: "http://127.0.0.1:7890" } },
      script: [
        { at: 600, act: () => document.querySelector<HTMLButtonElement>('[data-action="link-invite"]')?.click() },
        { at: 900, act: () => document.querySelector<HTMLDetailsElement>(".link-details")?.setAttribute("open", "") },
        { at: 1000, act: () => document.querySelector<HTMLButtonElement>('[data-action="code-toggle"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-host-stuck", label: "连接：当中心（中继连不上）", hint: "开启后一直「连接中继中」：说明里写经哪个代理、多半是代理不通或中继被墙，可以换代理或填自建中继；这时生成的码只带局域网地址，第二步写明这一点", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: {}, link: { enabled: true, stuck: true } },
      script: [],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-code", label: "连接：用配对码连接", hint: "Mac（不带 hub）打开连接页：默认「连到别的电脑上的中心」→「配对码」。粘贴后下面马上写出中心的名字、还剩多久过期；点「连接」后按三步走，连上后整页切到远程", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { bundledHub: false, stepMs: 1500 } },
      script: [
        { at: 500, act: () => typeInto('[data-field="code"]', mockInviteCode()) },
        { at: 1800, act: () => document.querySelector<HTMLButtonElement>('[data-action="pair-code"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-code-failed", label: "连接：配对码用不了", hint: "配对码已经被用过：原样显示中心的话，并给出下一步——在中心上重新生成一个", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { bundledHub: false, stepMs: 300, codeError: "这个配对码已经用过、过期，或中心的 hub 重启过：在中心上重新生成一个" } },
      script: [
        { at: 400, act: () => typeInto('[data-field="code"]', mockInviteCode()) },
        { at: 700, act: () => document.querySelector<HTMLButtonElement>('[data-action="pair-code"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-link-remote", label: "连接：打洞已连接", hint: "用配对码配对好以后：上面写远程 DESKTOP-CENTRE、已连接，旁边先是「经中继」，打通后变「直连」；下面写明是配对码（内置打洞）", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { paired: { host: "DESKTOP-CENTRE" }, via: "link", stepMs: 300, bundledHub: false } },
      script: [],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-pairing", label: "连接：配对中", hint: "填好主机点「配对并连接」：表单禁用，下面按 onPairStep 列出四步并标出正在做哪一步", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { stepMs: 5000 } },
      script: [
        { at: 200, act: () => document.querySelector<HTMLButtonElement>('[data-action="goal-join"]')?.click() },
        { at: 300, act: () => document.querySelector<HTMLButtonElement>('[data-action="method-ssh"]')?.click() },
        { at: 400, act: () => typeInto('[data-field="host"]', "centre.example") },
        { at: 700, act: () => document.querySelector<HTMLButtonElement>('[data-action="pair"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-remote", label: "连接：远程已连接", hint: "配对好以后：上面写远程 user@host、已连接和本机端口 → 中心端口；下面能切回本机、重新连接、两次点击忘记；侧栏底部写着远程 host", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { paired: { host: "centre.example", user: "me" }, stepMs: 200 } },
      script: [],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-offline", label: "连接：远程未连接", hint: "隧道断着：标红写明原因「未连接：…」，侧栏底部的远程 host 也标红，可以重新连接或切回本机", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { paired: { host: "centre.example", user: "me" }, stepMs: 200, offline: { reason: "ssh 退出了（退出码 255）" } } },
      script: [],
      route: "#/settings/connection",
    };
  } },
  { key: "connection-no-hub", label: "连接：安装包不带 hub", hint: "本机模式没有 hub，提示连接另一台电脑上的 hub", build: () => {
    const scene = workRounds();
    return { state: { ...scene.state, connection: { bundledHub: false } }, script: [], route: "#/settings/connection" };
  } },
  { key: "connection-pair-failed", label: "连接：配对失败", hint: "ssh 说主机密钥不认识：原样显示失败原因，并按原因给出下一步——先在终端里 ssh user@host 一次", build: () => {
    const scene = workRounds();
    return {
      state: { ...scene.state, connection: { stepMs: 200, pairError: "ssh 失败（退出码 255）\nHost key verification failed." } },
      script: [
        { at: 200, act: () => document.querySelector<HTMLButtonElement>('[data-action="goal-join"]')?.click() },
        { at: 300, act: () => document.querySelector<HTMLButtonElement>('[data-action="method-ssh"]')?.click() },
        { at: 400, act: () => typeInto('[data-field="host"]', "centre.example") },
        { at: 700, act: () => document.querySelector<HTMLButtonElement>('[data-action="pair"]')?.click() },
      ],
      route: "#/settings/connection",
    };
  } },
  { key: "devices-revoked", label: "设置：设备（吊销后）", hint: "点两次吊销第一台：列表刷新，它变淡、排到还在用的设备后面，并写上吊销时间", build: () => ({
    state: { ...emptyHub(), devices: devices() },
    script: [
      { at: 800, act: () => document.querySelector<HTMLButtonElement>('[data-action="revoke"]')?.click() },
      { at: 1200, act: () => document.querySelector<HTMLButtonElement>('button[data-action="revoke"].armed')?.click() },
      {
        at: 2200,
        act: () => {
          const rows = [...document.querySelectorAll<HTMLElement>(".device-row")];
          const revoked = rows.filter((r) => r.classList.contains("revoked"));
          if (revoked.length !== 2) throw new Error(`the revoked device did not join the list: ${revoked.length} faded rows`);
          const workstation = rows.find((r) => r.textContent?.includes("Windows 工作站"));
          const node = rows.find((r) => r.textContent?.includes("build-node"));
          if (!workstation?.classList.contains("revoked")) throw new Error("the revoked device still looks active");
          if (!node || !workstation || rows.indexOf(workstation) < rows.indexOf(node)) throw new Error("the revoked device is not after the active ones");
        },
      },
    ],
    route: "#/settings/devices",
  }) },
  { key: "devices-empty", label: "设置：设备（空）", hint: "还没有设备：说明在 hub 所在的机器上运行 crossagents device add <名字>，别的电脑在「设置 → 连接」里经 SSH 配对", build: () => ({
    state: emptyHub(),
    script: [],
    route: "#/settings/devices",
  }) },
  { key: "git", label: "Git 页", hint: "一个项目的分支（本地/远程）、带泳道的提交图（HEAD、分支、tag）、选中提交的详情和 diff，顶部是未提交改动数", build: () => ({
    state: { ...emptyHub(), agents: [agent({ name: "Ada", plugin: "claude", role: "worker", cwd: REPO })] },
    script: [],
    route: "#/git",
  }) },
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

/** The same reply, but the viewer opens a Markdown document, so its outline is on show. */
function filesMarkdownScene() {
  const scene = filesScene();
  const nova = scene.state.agents[0];
  return {
    ...scene,
    script: [{ at: 600, act: () => openFile({ scope: { kind: "agent", agent: nova.id }, path: "docs/RELEASE.md" }) }] as Step[],
  };
}

/** The OAuth run with the 后台 pane open: reminders, watches and #c under verification; Rex's reminder fires after ~8s. */
function backgroundScene() {
  const scene = oauthRun("running");
  const d = scene.state.runs[0];
  const runId = d.run.id;
  const byKey = (k: string) => d.tasks.find((t) => t.key === k)!;
  const [tb, tc] = [byKey("b"), byKey("c")];
  const named = (n: string) => scene.state.agents.find((a) => a.name === n)!;
  const [ada, rex, mia] = [named("Ada"), named("Rex"), named("Mia")];
  tc.status = "verifying";
  tc.updated_at = ago(2);
  const soon = (minutes: number) => new Date(Date.now() + minutes * 60_000).toISOString();
  const nova = "0190aaaa-0000-7000-8000-00000000n0va";
  const fires = "rem-rex-ci";
  const noHealth = { runs: [], paused_at: null, pause_reason: null };
  const background: BackgroundResponse = {
    reminders: [
      { reminder: { id: fires, owner: rex.id, due_at: new Date(Date.now() + 8_000).toISOString(), note: "看一眼 CI 有没有跑完", task: tc.id, only_if_open: true, wait_expired: false, created_at: ago(5) }, owner_name: rex.name, task_key: tc.key, run_id: runId },
      { reminder: { id: "rem-ada-check", owner: ada.id, due_at: soon(25), note: "#b 还没交就去问 Mia 卡在哪", task: tb.id, only_if_open: true, wait_expired: false, created_at: ago(20) }, owner_name: ada.name, task_key: tb.key, run_id: runId },
      { reminder: { id: "rem-mia-dark", owner: mia.id, due_at: soon(60 * 3 + 12), note: "深色模式的截图发给设计确认", task: null, only_if_open: false, wait_expired: false, created_at: ago(40) }, owner_name: mia.name, task_key: null, run_id: runId },
      { reminder: { id: "rem-nova-docs", owner: nova, due_at: soon(60 * 26), note: "明早把 docs/cli.md 的 --json 例子再核对一遍", task: null, only_if_open: false, wait_expired: false, created_at: ago(90) }, owner_name: "Nova", task_key: null, run_id: null },
    ],
    watches: [
      { watch: { id: "w-ada-c", owner: ada.id, target: { kind: "task", task_id: tc.id }, on: ["done", "failed"], created_at: ago(30), expires_at: null, chain: 0, health: noHealth }, owner_name: ada.name, target_name: `#${tc.key}`, run_id: runId },
      { watch: { id: "w-ada-mia", owner: ada.id, target: { kind: "agent", agent_id: mia.id }, on: ["idle", "stalled"], created_at: ago(28), expires_at: null, chain: 0, health: noHealth }, owner_name: ada.name, target_name: mia.name, run_id: runId },
      { watch: { id: "w-nova-iris", owner: nova, target: { kind: "agent", agent_id: "0190aaaa-0000-7000-8000-00000000iris" }, on: [], created_at: ago(70), expires_at: null, chain: 0, health: noHealth }, owner_name: "Nova", target_name: "Iris", run_id: null },
    ],
    verifying: [
      { task_id: tc.id, run_id: runId, run_title: d.run.title, key: tc.key, title: tc.title, assignee: tc.assignee, assignee_name: rex.name, checks: 3, since: tc.updated_at },
    ],
  };
  scene.state.background = background;
  return {
    ...scene,
    route: `#/runs/${runId}`,
    script: [
      ...scene.script,
      { at: 600, act: () => openTool("background") },
      {
        at: 8_500,
        act: (s: MockState) => {
          if (s.background) s.background = { ...s.background, reminders: s.background.reminders.filter((r) => r.reminder.id !== fires) };
        },
        event: { type: "reminder.fired", data: { owner: rex.id, reminder_id: fires, note: "看一眼 CI 有没有跑完" } },
      },
    ] as Step[],
  };
}

/** The 活动 pane: Ada leads, Rex and Mia were recruited by Ada, Kai by Rex; errors, tools and messages interleave. */
function activityScene() {
  const lead = agent({ name: "ada", plugin: "claude", model: "claude-opus-4-1", role: "planner", profile: { display_name: "Ada", title: "架构师", color: "indigo" } });
  const rex = agent({ name: "rex", plugin: "codex", model: "gpt-5-codex", role: "worker", recruitedBy: lead.id, profile: { display_name: "Rex", title: "Coder", color: "sky" } });
  const mia = agent({ name: "mia", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", recruitedBy: lead.id, profile: { display_name: "Mia", title: "设计师", color: "rose" } });
  const kai = agent({ name: "kai", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", recruitedBy: rex.id, profile: { display_name: "Kai", title: "测试", color: "teal" } });
  const state: MockState = { ...emptyHub(), agents: [lead, rex, mia, kai], quota: quota() };

  const t1 = turn({
    input: "把活动的接口补上，顺便看看 Rex 那边的进度。",
    minutesAgo: 8,
    took: 3,
    items: [say("我先看右侧栏的结构，再让 Rex 补筛选。")],
  });
  const t2 = turn({
    input: "先把类型筛选和范围筛选补上，错误单独标出来。",
    from: { kind: "agent", agent_id: lead.id },
    minutesAgo: 5,
    running: true,
    items: [say("筛选做成纯函数，先写测试。")],
  });
  const t3 = turn({
    input: "Rex 卡在测试上了，去帮它看一下。",
    minutesAgo: 2,
    running: true,
    items: [say("我去看构建日志。")],
  });
  const t4 = turn({
    input: "文档先不用动，等我这边接口定下来。",
    from: { kind: "agent", agent_id: lead.id },
    minutesAgo: 1,
    running: true,
    items: [say("收到，我先等。")],
  });
  // The turns arrive as events below (the feed only sees what came after the page loaded), so
  // the transcripts start empty instead of holding the same messages twice.
  state.transcripts.set(lead.id, transcript(lead, [], 24_000));
  state.transcripts.set(rex.id, transcript(rex, [], 18_000));
  state.transcripts.set(kai.id, transcript(kai, [], 12_000));
  state.transcripts.set(mia.id, transcript(mia, [], 9_000));

  const m1 = t1.turn.message_id;
  const m2 = t2.turn.message_id;
  const m3 = t3.turn.message_id;
  const m4 = t4.turn.message_id;

  return {
    state,
    route: `#/s/${lead.name}`,
    script: [
      { at: 300, act: () => openTool("activity") },
      { at: 500, event: { type: "agent.input", data: { id: lead.id, message_id: m1, text: "把活动的接口补上，顺便看看 Rex 那边的进度。", queued: false, from: { kind: "human" } } } },
      { at: 540, event: { type: "agent.turn_started", data: { id: lead.id, message_id: m1 } } },
      { at: 600, event: { type: "agent.output", data: { id: lead.id, stream: "thought", text: "先看 ToolColumn 和 panes。" } } },
      { at: 640, event: { type: "agent.output", data: { id: lead.id, stream: "text", text: "我先看右侧栏的结构，" } } },
      { at: 680, event: { type: "agent.output", data: { id: lead.id, stream: "text", text: "再让 Rex 补筛选。" } } },
      { at: 720, event: { type: "agent.tool_call", data: { id: lead.id, tool_call_id: "a-read", title: "Read apps/gui/src/ToolColumn.tsx", kind: "read", status: "in_progress" } } },
      { at: 760, event: { type: "agent.tool_call_updated", data: { id: lead.id, tool_call_id: "a-read", title: null, status: "completed" } } },
      { at: 800, event: { type: "agent.input", data: { id: rex.id, message_id: m2, text: "先把类型筛选和范围筛选补上，错误单独标出来。", queued: false, from: { kind: "agent", agent_id: lead.id } } } },
      { at: 840, event: { type: "agent.turn_started", data: { id: rex.id, message_id: m2 } } },
      { at: 880, event: { type: "agent.output", data: { id: rex.id, stream: "text", text: "筛选做成纯函数，先写测试。" } } },
      { at: 920, event: { type: "agent.tool_call", data: { id: rex.id, tool_call_id: "r-edit", title: "Edit apps/gui/src/lib/activityFeed.ts", kind: "edit", status: "completed" } } },
      { at: 960, event: { type: "agent.tool_call", data: { id: rex.id, tool_call_id: "r-test", title: "Run npm --prefix apps/gui test", kind: "execute", status: "in_progress" } } },
      { at: 1000, event: { type: "agent.tool_call_updated", data: { id: rex.id, tool_call_id: "r-test", title: null, status: "failed" } } },
      { at: 1040, event: { type: "agent.error", data: { id: rex.id, message: "活动测试失败：expected 2 entries, got 3" } } },
      { at: 1080, event: { type: "agent.input", data: { id: kai.id, message_id: m3, text: "Rex 卡在测试上了，去帮它看一下。", queued: false, from: { kind: "human" } } } },
      { at: 1120, event: { type: "agent.turn_started", data: { id: kai.id, message_id: m3 } } },
      { at: 1160, event: { type: "agent.output", data: { id: kai.id, stream: "text", text: "我去看构建日志。" } } },
      { at: 1200, event: { type: "agent.input", data: { id: mia.id, message_id: m4, text: "文档先不用动，等我这边接口定下来。", queued: false, from: { kind: "agent", agent_id: lead.id } } } },
      { at: 1240, event: { type: "agent.turn_started", data: { id: mia.id, message_id: m4 } } },
      { at: 1280, event: { type: "agent.output", data: { id: mia.id, stream: "text", text: "收到，我先等。" } } },
      {
        at: 1320,
        event: {
          type: "human.asked",
          data: {
            item: {
              id: "q-activity",
              kind: "question",
              from: lead.id,
              run_id: null,
              task_id: null,
              text: "活动流要不要默认只看错误？",
              detail: null,
              options: ["要", "不要"],
              default: null,
              timeout_at: null,
              status: "open",
              answer: null,
              answered_at: null,
              action: null,
              created_at: ago(0),
            },
          },
        },
      },
      { at: 1360, event: { type: "agent.error", data: { id: lead.id, message: "hub 断开了 3 秒（已自动重连）" } } },
    ] as Step[],
  };
}

/** The same conversation with the 文件 pane open in the tool column. */
function filesPaneScene() {
  const scene = filesScene();
  const nova = scene.state.agents[0];
  return { ...scene, script: [{ at: 600, act: () => openTool("files", { agent: nova.name }) }] };
}

/** Click a tree node by its label; scenarios use it to reach files the way a person would. */
function clickNode(name: string) {
  const node = [...document.querySelectorAll<HTMLButtonElement>(".fv-node")].find((button) => button.querySelector(".name")?.textContent === name);
  if (!node) throw new Error(`file tree node ${name} is missing`);
  node.click();
}

/** Type into the pane's whole-workspace filter. */
function searchFiles(value: string) {
  const input = document.querySelector<HTMLInputElement>(".fp-filter input");
  if (!input) throw new Error("file filter is missing");
  Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

/** A native select changed the way a person changes it (React keeps its own state). */
function setSelect(select: HTMLSelectElement | null, value: string) {
  if (!select) throw new Error("select is missing");
  Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value")!.set!.call(select, value);
  select.dispatchEvent(new Event("change", { bubbles: true }));
}

/** Text typed into a composer (React's onChange follows an input event). */
function typeText(selector: string, text: string) {
  const el = document.querySelector<HTMLTextAreaElement>(selector);
  if (!el) throw new Error(`${selector} is missing`);
  Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(el, text);
  el.dispatchEvent(new Event("input", { bubbles: true }));
}

/** The same pane in the default narrow right dock, with a Markdown file open. */
function filesPaneNarrow() {
  const scene = filesScene();
  const nova = scene.state.agents[0];
  return {
    ...scene,
    script: [
      { at: 350, act: () => setToolsWidth(420, true) },
      { at: 550, act: () => openTool("files", { agent: nova.name }) },
      { at: 850, act: () => clickNode("docs") },
      { at: 1150, act: () => clickNode("cli.md") },
    ] as Step[],
  };
}

/** The file pane opened wide enough for content and tree side by side, on two Markdown tabs. */
function filesPaneWide() {
  const scene = filesScene();
  const nova = scene.state.agents[0];
  return {
    ...scene,
    script: [
      { at: 350, act: () => setToolsWidth(860, true) },
      { at: 550, act: () => openTool("files", { agent: nova.name }) },
      { at: 850, act: () => clickNode("docs") },
      { at: 1150, act: () => clickNode("cli.md") },
      { at: 1450, act: () => clickNode("RELEASE.md") },
    ] as Step[],
  };
}

/** The same pane in the bottom dock, where width is plentiful but height is not. */
function filesPaneBottom() {
  const scene = filesScene();
  const nova = scene.state.agents[0];
  return {
    ...scene,
    script: [
      { at: 300, act: () => setBottomHeight(430) },
      { at: 500, act: () => placeTab("files", "bottom") },
      { at: 700, act: () => openTool("files", { agent: nova.name }) },
      { at: 1000, act: () => clickNode("docs") },
      { at: 1300, act: () => clickNode("cli.md") },
      { at: 1600, act: () => clickNode("RELEASE.md") },
    ] as Step[],
  };
}

/** A wide pane with the filter searching the whole workspace, not just opened folders. */
function filesPaneSearch() {
  const scene = filesPaneWide();
  return { ...scene, script: [...scene.script, { at: 1750, act: () => searchFiles("main") }] as Step[] };
}

/** The outline rail focused (a keyboard user's hover): the full heading list opens. */
function filesPaneOutline() {
  const scene = filesPaneWide();
  return {
    ...scene,
    script: [
      ...scene.script,
      { at: 1750, act: () => document.querySelector<HTMLElement>(".md-ol-line:nth-child(3)")?.focus() },
    ] as Step[],
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
