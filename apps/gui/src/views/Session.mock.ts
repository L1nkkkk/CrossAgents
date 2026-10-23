// Mock data for a one-on-one session (preview:mock): Nova (Claude) edits docs. One turn is done
// (its steps folded, a hub-computed diff under the reply), one runs while the script plays its
// steps, and two of your messages wait behind it.

import { emptyState, type MockState, type Step } from "../dev/MockBridge";
import { NOTES_REPO, agent, fileDiff, say, think, tool, transcript, turn, uid } from "../dev/fixtures";
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
  const nova = agent({ name: "nova", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status: "running", cwd: NOTES_REPO, profile: { display_name: "Nova", title: "文档", color: "violet" } });
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
