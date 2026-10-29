// Mock data for a one-on-one session (preview:mock): Nova (Claude) edits docs. One turn is done
// (its steps folded, a hub-computed diff under the reply), one runs while the script plays its
// steps, and two of your messages wait behind it.

import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { NOTES_REPO, agent, contextParts, fileDiff, planned, run, say, task, think, tool, transcript, turn, uid } from "../dev/fixtures";
import { plugins } from "./Agents.mock";
import { quota } from "./Usage.mock";

const README_DIFF =
  fileDiff("README.md", 1, [], [
    "",
    "## 快速开始",
    "",
    "```sh",
    "crossagents hub start",
    "crossagents agent add ada --plugin claude --role planner",
    "crossagents run \"把登录页改成 OAuth\"",
    "```",
  ], ["# notes"]) +
  fileDiff("docs/cli.md", 30, ["`--json` 输出 JSON。"], ["`--json`：所有命令都支持，输出一行一个 JSON 对象，适合脚本处理。", "", "例如 `crossagents events --json | jq .type`。"]);

export function soloSession(): { state: MockState; script: Step[]; route: string } {
  const nova = agent({ name: "nova", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status: "running", cwd: NOTES_REPO, topic: "README 快速开始与 --json 说明", profile: { display_name: "Nova", title: "文档", color: "violet" } });
  const state: MockState = { ...emptyState(), agents: [nova], plugins: plugins(), quota: quota() };

  const t1 = turn({
    input: "给 README 加一个快速开始，顺便把 CLI 的 --json 说明补全。",
    minutesAgo: 26,
    took: 3,
    items: [
      think("先看 README 现在的结构，再找 CLI 里 --json 是怎么实现的。"),
      tool(`Read ${NOTES_REPO}/README.md`, "read"),
      tool("Search --json in crates/cli", "search"),
      tool(`Read ${NOTES_REPO}/crates/cli/src/main.rs`, "read"),
      tool(`Edit ${NOTES_REPO}/README.md`, "edit"),
      tool(`Edit ${NOTES_REPO}/docs/cli.md`, "edit"),
      tool("cargo run -q -- --help", "execute", "completed", { command: "cargo run -q -- --help", exit_code: 0, duration_ms: 2_300, output: "Usage: crossagents [OPTIONS] <COMMAND>\n\nOptions:\n      --json  Print JSON" }),
      say("加好了：README 开头多了「快速开始」，三条命令从起 hub 到发起 run。`docs/cli.md` 里补了 `--json` 的说明和一个 `jq` 的例子。\n\n```sh\ncrossagents events --json | jq .type\n```\n\n要不要顺便把 `--help` 的输出也贴进文档？"),
    ],
    diff: README_DIFF,
    source: "hub",
  });

  const readId = uid();
  const t2 = turn({
    input: "再加一张架构图的占位说明，放在快速开始后面。",
    minutesAgo: 1,
    running: true,
    items: [think("看看 docs 里有没有现成的架构说明可以引用。"), { type: "tool_call", tool_call_id: readId, title: `Read ${NOTES_REPO}/docs/DESIGN.md`, kind: "read", status: "in_progress" }],
  });
  const q1 = turn({ input: "改完跑一下 markdownlint。", minutesAgo: 0.5, queued: true, items: [] });
  const q2 = turn({ input: "最后把这次的改动总结发我。", minutesAgo: 0.2, queued: true, items: [] });
  state.transcripts.set(nova.id, transcript(nova, [t1, t2, q1, q2], 31_000));
  state.diffs.set(t1.turn.message_id, README_DIFF);

  return { state, script: soloScript(nova.id, t2.turn.message_id, readId, q1.turn.message_id), route: `#/s/${nova.name}` };
}

/**
 * Edits that failed (docs/TODO.md §九): `with-cli` could not find the block the model meant to
 * replace, so nothing changed. The rows say so and show no +N −M; the diff they carry is what the
 * model planned. The first one is opened, so the tool's own words are on screen; the second has no
 * `detail.error` and falls back to its result text; the third edit went through, for comparison.
 */
export function failedEditSession(): { state: MockState; script: Step[]; route: string } {
  const nova = agent({ name: "nova", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status: "idle", cwd: NOTES_REPO, topic: "README 标题改成中文", profile: { display_name: "Nova", title: "文档", color: "violet" } });
  const state: MockState = { ...emptyState(), agents: [nova], plugins: plugins(), quota: quota() };
  const mismatch = "模型返回的待替换代码块未匹配到整行";
  const planned = fileDiff("README.md", 12, ["## Quick start"], ["## 快速开始", ""]);
  const t1 = turn({
    input: "把 README 的标题改成中文，顺便把 CLI 的 --json 说明补全。",
    minutesAgo: 8,
    took: 2,
    items: [
      think("README 第一个二级标题是 `## Quick start`，照它写待替换的代码块。"),
      tool(`Read ${NOTES_REPO}/README.md`, "read"),
      tool(`Edit ${NOTES_REPO}/README.md`, "edit", "failed", {
        arguments: JSON.stringify({ path: "README.md", old_string: "## Quick start\n", new_string: "## 快速开始\n" }),
        result: mismatch,
        error: mismatch,
        changes: [{ path: "README.md", kind: "update", diff: planned }],
      }),
      tool(`MultiEdit ${NOTES_REPO}/README.md`, "edit", "failed", {
        result: mismatch,
        changes: [{ path: "README.md", kind: "update", diff: planned }],
      }),
      tool(`Edit ${NOTES_REPO}/docs/cli.md`, "edit", "completed", {
        changes: [{ path: "docs/cli.md", kind: "update", diff: fileDiff("docs/cli.md", 30, ["`--json` 输出 JSON。"], ["`--json`：所有命令都支持，输出一行一个 JSON 对象。"]) }],
      }),
      say("`docs/cli.md` 补好了。README 的标题改了两次都没成：with-cli 两次都回「模型返回的待替换代码块未匹配到整行」，文件没动，现在还是 `## Quick start`。"),
    ],
  });
  state.transcripts.set(nova.id, transcript(nova, [t1], 12_000));
  return {
    state,
    script: [{ at: 600, act: () => document.querySelector<HTMLButtonElement>(".tool-step.failed button.tool")?.click() }],
    route: `#/s/${nova.name}`,
  };
}

/**
 * A long command running under the conversation (`stream`: its output keeps arriving), or a turn
 * that stopped producing anything long ago (`quiet`: what the live line says after 30 seconds
 * without news). `quiet` needs the page to sit still: screenshot it with a wait of ~46s.
 */
export function liveSession(kind: "stream" | "quiet"): { state: MockState; script: Step[]; route: string } {
  const nova = agent({ name: "nova", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status: "running", cwd: NOTES_REPO, topic: "把三个包都构建一遍", profile: { display_name: "Nova", title: "文档", color: "violet" } });
  const state: MockState = { ...emptyState(), agents: [nova], plugins: plugins(), quota: quota() };
  const before = turn({
    input: "先看看构建脚本，然后把三个包按 core、cli、gui 的顺序都构建一遍。",
    minutesAgo: 6,
    took: 2,
    items: [think("工作区有三个包，按依赖顺序构建。"), tool(`Read ${NOTES_REPO}/package.json`, "read"), say("构建脚本是 `npm run build`，会先跑 tsc 再打包 vite。core 和 cli 都过了，现在构建 gui，vite 这一步最久。")],
  });
  const build = uid();
  const started = ["> crossagents-gui@0.1.8 build", "> tsc --noEmit && vite build", "vite v8.3.4 building for production...", "transforming (12/214) src/lib/workGroups.ts"].join("\n");
  const current = turn({
    input: "三个包都构建一遍。",
    minutesAgo: 1,
    running: true,
    items: [think("最后一个是 gui。"), { type: "tool_call", tool_call_id: build, title: "npm run build", kind: "execute", status: "in_progress", output: started, detail: { changes: [], command: "npm run build", cwd: NOTES_REPO } }],
  });
  state.transcripts.set(nova.id, transcript(nova, [before, current], 44_000));
  // Build output a couple of lines at a time, then a thought (which the conversation folds to
  // "推理", so the live line is the only place its words show), then output again.
  const lines = ["transforming (38/214) src/views/Session.tsx", "transforming (74/214) src/ToolColumn.tsx", "rendering chunks...", "computing gzip size...", "dist/assets/index-8ac14f2e.js  412.61 kB │ gzip: 128.40 kB"];
  const output = (at: number, text: string): Step => ({ at, event: { type: "agent.tool_output", data: { id: nova.id, tool_call_id: build, text: `${text}\n` } } });
  const script: Step[] =
    kind === "quiet"
      ? []
      : [
          ...lines.flatMap((line, i) => [output(900 + i * 900, line), output(1_350 + i * 900, `transforming (${40 + i * 8}/214) src/main.tsx`)]),
          { at: 5_200, event: { type: "agent.output", data: { id: nova.id, stream: "thought", text: "这次比上次慢，" } } },
          { at: 5_700, event: { type: "agent.output", data: { id: nova.id, stream: "thought", text: "大概是 chunk 变多了，跑完再看一眼 dist。" } } },
          output(6_400, "✓ 214 modules transformed."),
          output(7_200, "rendering chunks (3)..."),
        ];
  return { state, script, route: `#/s/${nova.name}` };
}

/**
 * The same running turn as `liveSession`, seen on a run page: the run's lead is the one building,
 * so the middle column's live line is what the run page shows while it works.
 */
export function liveRun(kind: "stream" | "quiet"): { state: MockState; script: Step[]; route: string } {
  const scene = liveSession(kind);
  const nova = scene.state.agents[0];
  const r = run({
    title: "三个包都构建一遍",
    request: "先看看构建脚本，然后把三个包按 core、cli、gui 的顺序都构建一遍。",
    planner: nova,
    worker: nova,
    team: [nova],
    status: "running",
    plan: [planned("a", "构建 core、cli、gui", "三个包都能构建通过，有报错就修。", nova.id)],
    minutesAgo: 12,
  });
  scene.state.runs.push({ run: r, tasks: [task(r, r.plan[0], { status: "running", assignee: nova, minutesAgo: 10, progress: "gui 的 vite 打包还在跑" })] });
  return { ...scene, route: `#/runs/${r.id}` };
}

/** Nova's CLI crashes mid-turn: the session shows how its process ended. */
export function crashedSession(): { state: MockState; script: Step[]; route: string } {
  const solo = soloSession();
  const id = solo.state.agents[0].id;
  const message = "claude crashed (exit code 3)\nstderr: Error: connect ECONNRESET 127.0.0.1:7890";
  return {
    ...solo,
    script: [
      { at: 1200, event: { type: "agent.exited", data: { id, cause: "crashed", code: 3, message } } },
      { at: 1250, event: { type: "agent.error", data: { id, message } } },
      { at: 1300, event: { type: "agent.status_changed", data: { id, from: "running", to: "error" } } },
    ],
  };
}

/** Nova's running turn: the read finishes, two edits, a lint run streams output, the reply arrives. */
function soloScript(id: string, message_id: string, readId: string, next: string): Step[] {
  const edit = uid();
  const lint = uid();
  const diff = fileDiff("README.md", 12, [], ["", "## 架构", "", "> 架构图待补，先看 [DESIGN.md](docs/DESIGN.md) §2 的组件说明。"], ["```"]);
  return [
    { at: 1000, event: { type: "agent.tool_call_updated", data: { id, tool_call_id: readId, title: null, status: "completed" } } },
    { at: 1600, event: { type: "agent.output", data: { id, stream: "thought", text: "DESIGN.md §2 有组件说明，占位里链过去就好。" } } },
    { at: 2400, event: { type: "agent.tool_call", data: { id, tool_call_id: edit, title: `Edit ${NOTES_REPO}/README.md`, kind: "edit", status: "in_progress" } } },
    { at: 3200, event: { type: "agent.tool_call_updated", data: { id, tool_call_id: edit, title: null, status: "completed" } } },
    { at: 3600, event: { type: "agent.tool_call", data: { id, tool_call_id: lint, title: "npx markdownlint-cli2 README.md", kind: "execute", status: "in_progress" } } },
    { at: 4200, event: { type: "agent.tool_output", data: { id, tool_call_id: lint, text: "markdownlint-cli2 v0.14.0\n" } } },
    { at: 4900, event: { type: "agent.tool_output", data: { id, tool_call_id: lint, text: "Finding: README.md\nLinting: 1 file(s)\n" } } },
    { at: 5600, event: { type: "agent.tool_output", data: { id, tool_call_id: lint, text: "Summary: 0 error(s)\n" } } },
    { at: 5900, event: { type: "agent.tool_call_updated", data: { id, tool_call_id: lint, title: null, status: "completed" } } },
    { at: 6300, event: { type: "agent.output", data: { id, stream: "text", text: "加在「快速开始」后面了，先放一句占位，" } } },
    { at: 6800, event: { type: "agent.output", data: { id, stream: "text", text: "链到 DESIGN.md §2。markdownlint 没有报错。" } } },
    { at: 7100, event: { type: "agent.turn_diff", data: { id, diff, message_id, source: "hub" } } },
    { at: 7200, event: { type: "agent.turn_completed", data: { id, message_id, stop_reason: "end_turn" } } },
    // The first waiting message goes next.
    { at: 7700, event: { type: "agent.turn_started", data: { id, message_id: next } } },
    { at: 8300, event: { type: "agent.tool_call", data: { id, tool_call_id: uid(), title: "npx markdownlint-cli2 \"**/*.md\"", kind: "execute", status: "in_progress" } } },
  ];
}

/**
 * A chat that recruits (chat mode): Luna is asked to "summon a luna". Its first try names the
 * recruit after the model and is refused (the card says why); the second recruits Iris on that
 * model, who nests under the chat in the sidebar. Luna's reply says what was refused.
 */
export function chatWithRecruit(): { state: MockState; script: Step[]; route: string } {
  const chats = "C:/Users/me/.crossagents/chats";
  const luna = agent({ name: "Luna", plugin: "codex", model: "gpt-6-luna", role: "worker", cwd: `${chats}/luna`, topic: "帮我唤起一个 luna 写冒泡排序" });
  luna.name_source = "generated";
  const iris = agent({ name: "Iris", plugin: "codex", model: "gpt-6-luna", role: "worker", cwd: `${chats}/luna`, recruitedBy: luna.id, createdMinutesAgo: 3 });
  const state: MockState = { ...emptyState(), agents: [luna, iris], plugins: plugins(), quota: quota() };
  const refused =
    'conflict: not recruited: "Luna" is a vendor or model word, and names are for people; choose another name, or leave name out to get one (the model goes in model, not in the name)';
  const t1 = turn({
    input: "你能帮我唤起一个luna做一个写冒泡排序的任务吗？",
    minutesAgo: 4,
    took: 1,
    items: [
      tool("crossagents.agent_spawn", "mcp", "failed", { arguments: JSON.stringify({ name: "Luna", model: "gpt-6-luna" }), error: refused, changes: [] }),
      tool("crossagents.agent_spawn", "mcp", "completed", { arguments: JSON.stringify({ model: "gpt-6-luna" }), result: JSON.stringify({ agent: "Iris", id: iris.id, model: "gpt-6-luna", plugin: "codex" }), changes: [] }),
      tool("crossagents.agent_send", "mcp", "completed", { arguments: JSON.stringify({ to: "Iris", text: "请用 Python 写一个冒泡排序，附简短的输入输出示例。" }), changes: [] }),
      say(
        "用「Luna」这个名字招人被 hub 拒绝了：那是模型名，名字要像人名。我改成不指定名字，hub 给新队友起名 **Iris**，跑在 gpt-6-luna 上（就是你要的模型）。冒泡排序的任务已经发给 Iris，它写完会回给我。",
      ),
    ],
  });
  state.transcripts.set(luna.id, { ...transcript(luna, [t1], 12_000), conversation_cost_usd: 0.04 });
  state.transcripts.set(iris.id, transcript(iris, [], 2_000));
  return { state, script: [], route: `#/s/${luna.id}` };
}

/** A mock screenshot (an SVG, which the preview may show; the hub takes PNG, JPEG, GIF, WebP). */
const SHOT_SVG = `<svg xmlns="http://www.w3.org/2000/svg" width="480" height="300"><rect width="480" height="300" fill="#1c1c1f"/><rect x="0" y="0" width="120" height="300" fill="#141416"/><rect x="140" y="24" width="300" height="36" rx="8" fill="#2a2a30"/><rect x="140" y="80" width="220" height="14" rx="4" fill="#3a3a44"/><rect x="140" y="104" width="260" height="14" rx="4" fill="#3a3a44"/><rect x="140" y="240" width="320" height="40" rx="10" fill="none" stroke="#5b5bd6"/><text x="150" y="47" fill="#e5e5e5" font-family="sans-serif" font-size="14">登录页 · 深色</text></svg>`;

/**
 * Files with a message (docs/tasks/DOGFOOD.md §3): the person sent a screenshot and a log
 * with their question; paste or drop more into the composer, or use the paperclip.
 */
export function attachmentSession(): { state: MockState; script: Step[]; route: string } {
  const solo = soloSession();
  const nova = solo.state.agents[0];
  nova.status = "idle";
  const t = solo.state.transcripts.get(nova.id)!;
  const shot = uid().replace(/-/g, "");
  const log = uid().replace(/-/g, "");
  const base = `C:/Users/me/.crossagents/attachments/${nova.id}`;
  solo.state.files.set(shot, { mime: "image/svg+xml", data: btoa(unescape(encodeURIComponent(SHOT_SVG))) });
  const asked = turn({
    input: "深色模式下登录页的按钮看不清，截图和控制台日志附上了，看看是哪里的样式。",
    minutesAgo: 4,
    took: 2,
    items: [think("截图里按钮描边和背景几乎同色；日志里有一条 CSS 变量没定义的警告。"), say("是 `--accent-border` 在深色主题里没定义，回落成了背景色。我在 `login.css` 里补上深色的值。")],
  });
  asked.turn.attachments = [
    { id: shot, name: "login-dark.png", mime: "image/png", size: 48_213, path: `${base}/${shot}` },
    { id: log, name: "console.log", mime: "text/plain", size: 1_380, path: `${base}/${log}` },
  ];
  t.turns = [t.turns[0], asked.turn];
  return { ...solo, script: [], route: `#/s/${nova.name}` };
}

/**
 * The solo chat with the context meter's popup open, at 20% of the window: what it holds in
 * words and 压缩上下文, which no longer waits for a threshold.
 */
export function contextUsageScene(): { state: MockState; script: Step[]; route: string } {
  const s = failedEditSession();
  const a = s.state.agents[0];
  const t = s.state.transcripts.get(a.id)!;
  const used = 40_000;
  t.usage = {
    ...t.usage!,
    used,
    tokens: { input: used * 3, cached_input: used * 2, output: Math.round(used / 4), reasoning_output: Math.round(used / 10), total: used * 3 + Math.round(used / 4) },
    parts: contextParts(used, t.usage!.size),
  };
  return {
    ...s,
    // Either entry: the topbar meter when the rail is away, its 上下文 card when it is there.
    script: [{ at: 600, act: () => document.querySelector<HTMLButtonElement>(".meter-btn, .ctx-rail-btn")?.click() }],
  };
}

/** The same chat with `/` typed into the composer, so the command menu is open; a running agent
 * shows /compact as unavailable and says why. */
export function slashScene(kind: "idle" | "running" | "unknown"): { state: MockState; script: Step[]; route: string } {
  const s = kind === "running" ? soloSession() : failedEditSession();
  if (kind !== "unknown") s.state.agentCommands = { [s.state.agents[0].id]: { known: true, commands: [
    { name: "review", description: "审查当前改动", kind: "command" },
    { name: "explain", description: "解释代码和设计", kind: "skill" },
    { name: "compact", kind: "command" },
  ] } };
  return {
    ...s,
    script: [
      {
        at: 700,
        act: () => {
          const box = document.querySelector<HTMLTextAreaElement>(".composer textarea");
          if (!box) return;
          // As a person typing would: the value React reads, then the input event it listens to.
          Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(box, "/");
          box.dispatchEvent(new Event("input", { bubbles: true }));
          box.focus();
        },
      },
    ],
  };
}

/**
 * A long agent name in a topbar that has every button (running, over half full): the name keeps
 * a short name's room and only its own overflow is cut — never squeezed out by the buttons.
 */
export function longNameScene(): { state: MockState; script: Step[]; route: string } {
  const s = soloSession();
  const a = s.state.agents[0];
  a.name = "Nova 文档与发布助手";
  a.name_source = "chosen";
  const t = s.state.transcripts.get(a.id)!;
  if (t.usage) t.usage = { ...t.usage, used: 130_000, parts: contextParts(130_000, t.usage.size) };
  return { ...s, route: `#/s/${encodeURIComponent(a.name)}` };
}
