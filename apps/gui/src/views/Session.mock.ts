// Mock data for a one-on-one session (preview:mock): Nova (Claude) edits docs. One turn is done
// (its steps folded, a hub-computed diff under the reply), one runs while the script plays its
// steps, and two of your messages wait behind it.

import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { NOTES_REPO, REPO, agent, ago, contextParts, fileDiff, planned, run, say, task, think, tool, transcript, turn, uid } from "../dev/fixtures";
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

/** Three common image result shapes in one conversation. */
export function toolImagesSession(): { state: MockState; script: Step[]; route: string } {
  const scene = soloSession();
  const nova = scene.state.agents[0];
  nova.status = "idle";
  const png = "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+eE39G/oPyz0eyjwHgongkiiiCaGWOKIJ4FEkkgmhVTSSCeDTLLIJodc8singEKKKKaEUsoop4JKqqimhlrqqKeBRppopoVW2ming0666KaHXvroZ4BBhhhmhFHGGGeCSaaYZoZZ5phngUWWWGaFVdZYZ4NNtthmh1322OeAQ4445oRTzjjngkuuuOaGW+6454FHnnjmhVfeeCfIB5988c0PYYFgKBAMhRNBJFFEE0MsccSTQCJJJJNCKmmkk0EmWWSTQy555FNAIUUUU0IpZZRTQSVVVFNDLXXU00AjTTTTQitttNNBJ11000MvffQzwCBDDDPCKGOMM8EkU0wzwyxzzLPAIksss8Iqa6yzwSZbbLPDLnvsc8AhRxxzwilnnHPBJVdcc8Mtd9zzwCNPPPPCK2+8E+SDT7745oewwEco8BEKJ4JIoogmhljiiCeBRJJIJoVU0kgng0yyyCaHXPLIp4BCiiimhFLKKKeCSqqopoZa6qingUaaaKaFVtpop4NOuuimh1766GeAQYYYZoRRxhhngkmmmGaGWeaYZ4FFllhmhVXWWGeDTbbYZodd9tjngEOOOOaEU84454JLrrjmhlvuuOeBR5545oVX3ngnyAeffPHND7+hf0P/YaE/Q4HPUDgRRBJFNDHEEkc8CSSSRDIppJJGOhlkkkU2OeSSRz4FFFJEMSWUUkY5FVRSRTU11FJHPQ000kQzLbTSRjsddNJFNz300kc/AwwyxDAjjDLGOBNMMsU0M8wyxzwLLLLEMiusssY6G2yyxTY77LLHPgcccsQxJ5xyxjkXXHLFNTfccsc9DzzyxDMvvPLGO0E++OSLb34IC3yFAl+hcCKIJIpoYogljngSSCSJZFJIJY10Msgki2xyyCWPfAoopIhiSiiljHIqqKSKamqopY56GmikiWZaaKWNdjropItueuilj34GGGSIYUYYZYxxJphkimlmmGWOeRZYZIllVlhljXU22GSLbXbYZY99DjjkiGNOOOWMcy645IprbrjljnseeOSJZ1545Y13gnzwyRff/PAb+jf0Hxb6OxT4DoUTQSRRRBNDLHHEk0AiSSSTQipppJNBJllkk0MueeRTQCFFFFNCKWWUU0ElVVRTQy111NNAI00000IrbbTTQSdddNNDL330M8AgQwwzwihjjDPBJFNMM8Msc8yzwCJLLLPCKmus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";
  const url = `data:image/png;base64,${png}`;
  const t = turn({ input: "查看这些图片。", minutesAgo: 1, took: 1, items: [
    tool("ViewImage image_url.png", "read", "completed", { result: JSON.stringify([{ type: "image_url", image_url: { url } }]) }),
    tool("ViewImage mcp.png", "read", "completed", { result: JSON.stringify([{ type: "image", data: png, mimeType: "image/png" }]) }),
    tool("ViewImage mixed.png", "read", "completed", { result: JSON.stringify([{ type: "text", text: "图片中的内容" }, { type: "image_url", image_url: { url } }]) }),
    say("已查看图片。"),
  ] });
  scene.state.transcripts.set(nova.id, transcript(nova, [t], 1000));
  return { ...scene, script: [{ at: 300, act: () => document.querySelector<HTMLButtonElement>(".steps-summary")?.click() }] };
}


/** Local paths: allowed, rejected, and duplicated by a tool result. */
export function toolImagePathsSession(): { state: MockState; script: Step[]; route: string } {
  const scene = soloSession();
  const nova = scene.state.agents[0];
  nova.status = "idle";
  const png = "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";
  const t = turn({ input: "查看本地图片。", minutesAgo: 1, took: 1, items: [
    tool("ViewImage /tmp/allowed.png", "read", "completed", { arguments: JSON.stringify({ image_path: "/tmp/allowed.png" }) }),
    tool("ViewImage /outside/denied.png", "read", "completed", { arguments: JSON.stringify({ path: "/outside/denied.png" }) }),
    tool("ViewImage /tmp/same.png", "read", "completed", { arguments: JSON.stringify({ image_path: "/tmp/same.png" }), result: `data:image/png;base64,${png}` }),
    say("已查看图片。"),
  ] });
  scene.state.transcripts.set(nova.id, transcript(nova, [t], 1000));
  return { ...scene, script: [
    { at: 300, act: () => document.querySelector<HTMLButtonElement>(".steps-summary")?.click() },
    { at: 500, act: () => document.querySelectorAll<HTMLButtonElement>(".tool-step > button.tool").forEach((button) => button.click()) },
  ] };
}

/**
 * A session working a task for a run while its checks are in (docs/TODO.md §四 ②): Nova reviews
 * #b, so the session header — and its sidebar row — say how long this turn has run, what it does,
 * the task's own progress and how many of its checks passed, and the run's progress. Every count
 * is the hub's; there is no step total, so no percentage or bar is drawn.
 */
export function liveTaskSession(): { state: MockState; script: Step[]; route: string } {
  const nova = agent({ name: "nova", plugin: "claude", model: "claude-sonnet-4-5", role: "reviewer", status: "running", cwd: `${REPO}/.crossagents/worktrees/b`, topic: "审查 #b 登录页 OAuth 按钮", profile: { display_name: "Nova", title: "审查", color: "violet" } });
  const rex = agent({ name: "rex", plugin: "codex", model: "gpt-5-codex", role: "worker", status: "running", cwd: `${REPO}/.crossagents/worktrees/a`, topic: "回调与令牌交换", profile: { display_name: "Rex", title: "Coder", color: "sky" } });
  const state: MockState = { ...emptyState(), agents: [nova, rex], plugins: plugins(), quota: quota() };
  const plan = [
    planned("a", "OAuth 回调与令牌交换", "实现 /auth/callback/{provider}，GitHub 和 Google 两家。", rex.name),
    planned("b", "登录页加 OAuth 按钮", "登录页加两个按钮，密码表单保留。", rex.name, ["a"]),
    planned("c", "会话存储迁到 Redis", "SessionStore 换成 Redis 实现。", rex.name),
  ];
  const r = run({ title: "登录页改 OAuth", request: "把登录页改成 OAuth（GitHub 和 Google），旧的密码登录保留。", planner: rex, worker: rex, team: [nova, rex], status: "running", plan, minutesAgo: 40 });
  const check = (passed: boolean, command: string) => ({ command, exit_code: passed ? 0 : 1, passed, output: passed ? "" : "error: unused import `Session`" });
  const merged = task(r, plan[0], { status: "merged", assignee: rex, minutesAgo: 30, summary: "两家都通了，回调有集成测试。", checks: [check(true, "cargo test -p auth"), check(true, "npm test")] });
  const review = task(r, plan[1], { status: "reviewing", assignee: rex, minutesAgo: 20, progress: "两个按钮加好了，深浅色都看过", checks: [check(true, "cargo test -p auth"), check(true, "npm test"), check(false, "npx eslint web/src/pages/Login.tsx"), check(true, "npm run typecheck")] });
  review.reviewer = nova.id;
  const failed = task(r, plan[2], { status: "failed", assignee: rex, minutesAgo: 12, failure: "本地没有 Redis，连不上，先停在这里。" });
  state.runs.push({ run: r, tasks: [merged, review, failed] });
  state.executions = {
    [nova.id]: { run_id: r.id, run_title: r.title, task_id: review.id, task_key: review.key, role: "reviewer", fresh: true, since: ago(22) },
    [rex.id]: { run_id: r.id, run_title: r.title, task_id: null, task_key: null, role: "lead", fresh: false, since: ago(40) },
  };

  const t = turn({
    input: "看看 #b 的登录页改动，检查没过的那条 lint 是不是真问题。",
    minutesAgo: 3,
    running: true,
    items: [think("先看 diff，再对着 lint 的报错找那一行。"), tool(`Read ${REPO}/web/src/pages/Login.tsx`, "read", "in_progress")],
  });
  state.transcripts.set(nova.id, transcript(nova, [t], 24_000));
  const rexTurn = turn({ input: "回调写完了，跑一下测试。", minutesAgo: 6, running: true, items: [tool("cargo test -p auth", "execute", "in_progress", { command: "cargo test -p auth" }, "running 12 tests\n")] });
  state.transcripts.set(rex.id, transcript(rex, [rexTurn], 18_000));

  const read = t.turn.items[1];
  const lint = uid();
  return {
    state,
    script: [
      ...(read?.type === "tool_call" ? [{ at: 900, event: { type: "agent.tool_call_updated" as const, data: { id: nova.id, tool_call_id: read.tool_call_id, title: null, status: "completed" as const } } }] : []),
      { at: 1_400, event: { type: "agent.tool_call", data: { id: nova.id, tool_call_id: lint, title: "npx eslint web/src/pages/Login.tsx", kind: "execute", status: "in_progress" } } },
      { at: 1_900, event: { type: "agent.tool_output", data: { id: nova.id, tool_call_id: lint, text: "web/src/pages/Login.tsx\n  18:9  error  'Session' is defined but never used\n" } } },
    ],
    route: `#/s/${nova.name}`,
  };
}
