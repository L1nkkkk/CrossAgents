// What a session lost on the way back (docs/tasks/S1.md §7 第 15 条) and the native resume
// command (docs/TODO.md §十五), for `preview:mock`. Restoring and sending go through the mock
// bridge's own routes (`bridge.request`), which say what the hub says (MockBridge `HUB_SAYS`).

import type { Agent } from "@bindings/Agent";
import type { LandMode } from "@bindings/LandMode";
import { HUB_SAYS, emptyState, type MockBridge, type MockState, type Step } from "../dev/MockBridge";
import { REPO, agent, ago, say, think, tool, transcript, turn } from "../dev/fixtures";

type Scene = { state: MockState; script: Step[]; route: string };

const CHECKOUT = `${REPO}/.crossagents/worktrees/7f3a9c21/integration`;
const RUN_TITLE = "OAuth 登录";

/** An earlier conversation, so restoring has something to restore. */
function history(a: Agent, minutesAgo: number) {
  return [
    turn({
      input: "回调页登录失败时现在是白屏，先看一下是哪里吞掉了错误",
      minutesAgo: minutesAgo + 30,
      items: [think("先找回调的处理函数。"), tool("Read src/auth/callback.ts", "read"), say("`handleCallback` 里 `catch` 之后什么都没做，所以是白屏。我记下了：下一步加错误页。")],
    }),
    turn({
      input: "好，等 run 里的 #b 合进来再继续",
      minutesAgo,
      items: [say(`好的。${a.name} 先停在这里。`)],
    }),
  ];
}

function scene(agents: Agent[], focus: Agent, minutesAgo: number): MockState {
  const state = emptyState();
  state.agents = agents;
  state.transcripts.set(focus.id, transcript(focus, history(focus, minutesAgo)));
  return state;
}

/** The person's own steps, through the bridge: restore, then the messages. */
function steps(a: Agent, messages: string[]): Step[] {
  const post = (bridge: MockBridge, path: string, body: unknown) => void bridge.request("POST", path, JSON.stringify(body));
  // A message to a stopped session waits ~2.5 s for it to start, then ~3.5 s for its turn.
  return [
    { at: 300, act: (_, b) => post(b, `/v1/agents/${a.id}/archive`, { archived: false }) },
    ...messages.map((text, i) => ({ at: 1200 + i * 7000, act: (_: MockState, b: MockBridge) => post(b, `/v1/agents/${a.id}/messages`, { text }) })),
  ];
}

/**
 * Pia worked in a run's checkout with a CLI that resumes only where it ran; the run was
 * deleted, so she went back to the workspace marked `context_lost`, and archived.
 * `messages`: what the person sends after restoring her (none: nothing is played).
 */
export function contextLost(messages: string[] | null = null): Scene {
  const pia = agent({ name: "pia", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status: "stopped", profile: { display_name: "Pia", title: "前端", color: "teal" } });
  pia.archived_at = ago(20);
  pia.config = { context_lost: HUB_SAYS.contextLost(RUN_TITLE, CHECKOUT, pia.name) };
  const state = scene([pia], pia, 90);
  return { state, script: messages ? steps(pia, messages) : [], route: `#/s/${pia.name}` };
}

/**
 * The lead copied for a run (its driver could not move), archived when the run ended; the
 * run's branch has since been deleted, so restoring rebuilds the checkout from `main`.
 */
export function codeLost(landed: { mode: LandMode; reference: string } | null, play = true): Scene {
  const lead = agent({
    name: "ada-oauth",
    plugin: "claude",
    model: "claude-opus-4-1",
    role: "planner",
    status: "stopped",
    cwd: CHECKOUT,
    home: REPO,
    profile: { display_name: "Ada（复制）", title: "主导", color: "indigo" },
  });
  lead.archived_at = ago(60);
  const state = scene([lead], lead, 180);
  state.codeLost = { [lead.id]: { branch: "crossagents/oauth-login", base: "main", ...(landed ? { landed } : {}) } };
  return { state, script: play ? steps(lead, []) : [], route: `#/s/${lead.name}` };
}

/** Sessions for the resume command: one that ran, one that never did, one whose driver resumes nothing. */
export function resumeScene(which: "ran" | "never" | "acp"): Scene {
  const nova = agent({ name: "nova", plugin: "claude", model: "claude-sonnet-4-5", role: "worker", status: "idle", profile: { display_name: "Nova", title: "全栈", color: "sky" } });
  const iris = agent({ name: "iris", plugin: "codex", model: "gpt-5-codex", role: "worker", status: "stopped", profile: { display_name: "Iris", title: "Coder", color: "rose" } });
  const gem = agent({ name: "gem", plugin: "mock", model: "gemini-3-pro", role: "worker", status: "stopped", profile: { display_name: "Gem", title: "ACP 队友", color: "amber" } });
  const state = emptyState();
  state.agents = [nova, iris, gem];
  state.transcripts.set(nova.id, transcript(nova, history(nova, 30)));
  state.transcripts.set(iris.id, transcript(iris, []));
  state.transcripts.set(gem.id, transcript(gem, history(gem, 50)));
  const focus = which === "ran" ? nova : which === "never" ? iris : gem;
  return { state, script: [], route: `#/s/${focus.name}` };
}
