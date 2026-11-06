// Mock data for a run's cost bar (preview:mock): what the agents of the OAuth run reported.
// The bar itself is `apps/gui/src/costbar.tsx`; the hub answers the same shape from
// crates/hub/src/run_cost.rs.

import type { Agent } from "@bindings/Agent";
import type { RunCostResponse } from "@bindings/RunCostResponse";
import type { RunCostSlice } from "@bindings/RunCostSlice";
import { oauthRun } from "./Run.mock";

/** One agent on one model, as a driver reported it. */
export type Stretch = {
  agent: Agent;
  /** Defaults to the agent's own model. */
  model?: string;
  cached_input: number;
  new_input: number;
  output: number;
  /** `null`: the driver reported tokens but no cost. */
  cost_usd: number | null;
};

/** The run's cost bar out of the stretches its agents reported. */
export function runCostOf(runId: string, stretches: Stretch[]): RunCostResponse {
  const slices: RunCostSlice[] = stretches.map((stretch) => ({
    agent: stretch.agent.id,
    name: stretch.agent.name,
    model: stretch.model ?? stretch.agent.model ?? "default",
    cached_input: stretch.cached_input,
    new_input: stretch.new_input,
    output: stretch.output,
    cost_usd: stretch.cost_usd,
  }));
  const tokens = slices.reduce((sum, s) => sum + s.cached_input + s.new_input + s.output, 0);
  const cost = slices.some((s) => s.cost_usd !== null)
    ? slices.reduce((sum, s) => sum + (s.cost_usd ?? 0), 0)
    : null;
  const notes =
    slices.length === 0
      ? ["这个 run 还没有 agent 上报用量。"]
      : [
          ...(cost === null ? ["没有 agent 上报金额：分段按 token 计算，悬停看 token 数。"] : []),
          "金额和 token 是各 agent 自己上报的（会话内累计的差值），按上报当时的模型记账。",
        ];
  return { run_id: runId, slices, tokens, cost_usd: cost, notes };
}

/**
 * The OAuth run with the cost its agents reported: Ada planned, Rex did #a (#c on two models:
 * it switched from gpt-5 to gpt-5-codex mid-way) and failed #d, Mia did #b.
 */
export function runCostScene() {
  const scene = oauthRun("running");
  const detail = scene.state.runs[0];
  const named = (name: string) => scene.state.agents.find((a) => a.name === name)!;
  const [ada, rexA, miaB, rexC, rexD] = ["Ada", "Rex-a", "Mia-b", "Rex-c", "Rex-d"].map(named);
  scene.state.runCost = {
    [detail.run.id]: runCostOf(detail.run.id, [
      // Biggest spender first, as the hub sorts it: one agent's models next to each other.
      { agent: ada, cached_input: 640_000, new_input: 118_000, output: 26_400, cost_usd: 9.86 },
      { agent: rexA, cached_input: 1_460_000, new_input: 214_000, output: 61_000, cost_usd: 5.24 },
      { agent: rexC, cached_input: 720_000, new_input: 96_000, output: 22_000, cost_usd: 2.06 },
      { agent: rexC, model: "gpt-5", cached_input: 180_000, new_input: 31_000, output: 7_200, cost_usd: 0.48 },
      { agent: miaB, cached_input: 410_000, new_input: 88_000, output: 19_800, cost_usd: 1.72 },
      { agent: rexD, cached_input: 260_000, new_input: 42_000, output: 9_600, cost_usd: 0.74 },
    ]),
  };
  return {
    ...scene,
    script: [
      // The rail is long: bring the card into view and hover the worker's biggest segment.
      { at: 700, act: () => document.querySelector(".costbar")?.scrollIntoView({ block: "center" }) },
      {
        at: 1100,
        act: () =>
          document
            .querySelector<HTMLElement>(".costbar-seg:nth-child(2)")
            ?.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })),
      },
    ],
    route: `#/runs/${detail.run.id}`,
  };
}
