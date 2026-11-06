// A run's spend as one bar (docs/research/multica.md §十三): a segment per agent and model,
// its width the share of the run's cost (or of its tokens when nobody reported any), hovered
// or focused for the amount and the token counts behind it. The figures come from
// `GET /v1/runs/{key}/cost` (crates/hub/src/run_cost.rs): what the agents themselves reported.

import { useState } from "react";
import type { Agent } from "@bindings/Agent";
import type { RunCostSlice } from "@bindings/RunCostSlice";
import { api } from "./api";
import { ErrorBox, colorOf, hueOf, useAgents } from "./components";
import { useHubData } from "./hub";
import { prettyModel } from "./lib/models";
import { compactUsage } from "./lib/usage";
import "./costbar.css";

/** Dollars, keeping a third decimal only when the amount is under ten cents. */
const usd = (n: number) => `$${n.toFixed(n >= 0.1 ? 2 : 3)}`;
/** What to call the model in words. */
const modelName = (model: string) => (model === "default" ? "默认模型" : prettyModel(model));
const tokensOf = (slice: RunCostSlice) => slice.cached_input + slice.new_input + slice.output;
const sliceKey = (slice: RunCostSlice) => `${slice.agent}/${slice.model}`;

/** The hue the agent's segments and dots take: its own colour when it still exists. */
function hueOfSlice(slice: RunCostSlice, agents?: Agent[]) {
  const agent = agents?.find((a) => a.id === slice.agent);
  return agent ? colorOf(agent) : hueOf(slice.name || slice.agent);
}

/**
 * The run's cost bar: one bar, an agent's models next to each other and shaded apart, the
 * biggest spender first; below it the same segments as rows, so the numbers are readable
 * without hovering.
 */
export function RunCostBar({ runId }: { runId: string }) {
  const cost = useHubData(() => api.runCost(runId), [runId], (e) => e.type === "agent.usage");
  const agents = useAgents();
  const [hover, setHover] = useState<number>();
  const data = cost.data;
  const slices = data?.slices ?? [];

  // Widths follow money when every segment has it; otherwise tokens, so a segment without a
  // reported cost is not squeezed to nothing. A driver that reported neither leaves equal
  // segments.
  const money = slices.reduce((sum, slice) => sum + (slice.cost_usd ?? 0), 0);
  const byCost = slices.length > 0 && slices.every((slice) => slice.cost_usd !== null) && money > 0;
  const weights = slices.map((slice) => (byCost ? (slice.cost_usd ?? 0) : tokensOf(slice)));
  const weighted = weights.reduce((sum, n) => sum + n, 0);

  // Models of one agent share its hue, lightened in the order they appear.
  const shades = new Map<string, number>();
  const perAgent = new Map<string, number>();
  for (const slice of slices) {
    const seen = perAgent.get(slice.agent) ?? 0;
    shades.set(sliceKey(slice), Math.max(0.45, 1 - seen * 0.25));
    perAgent.set(slice.agent, seen + 1);
  }

  const describe = (slice: RunCostSlice) =>
    `${slice.name || slice.agent} · ${modelName(slice.model)} · ${
      slice.cost_usd === null ? "未上报金额" : usd(slice.cost_usd)
    } · ${compactUsage(tokensOf(slice))} token`;

  return (
    <div className="costbar">
      <ErrorBox error={cost.error} />
      {!data && !cost.error && <p className="muted small">加载中…</p>}
      {data && slices.length > 0 && (
        <>
          <div className="costbar-head">
            <strong>{data.cost_usd === null ? compactUsage(data.tokens) : usd(data.cost_usd)}</strong>
            <span className="muted small">
              {data.cost_usd === null ? "token（未上报金额）" : `${compactUsage(data.tokens)} token`}
              {" · "}
              {slices.length} 段
            </span>
          </div>
          <div className="costbar-track" role="group" aria-label="这个 run 的费用，按 agent 和模型分段">
            {slices.map((slice, i) => (
              <button
                key={sliceKey(slice)}
                type="button"
                className={`costbar-seg av-${hueOfSlice(slice, agents.data)} ${hover === i ? "on" : ""}`}
                style={{
                  width: `${weighted > 0 ? (weights[i] / weighted) * 100 : 100 / slices.length}%`,
                  opacity: shades.get(sliceKey(slice)),
                }}
                aria-label={describe(slice)}
                title={describe(slice)}
                onMouseEnter={() => setHover(i)}
                onMouseLeave={() => setHover(undefined)}
                onFocus={() => setHover(i)}
                onBlur={() => setHover(undefined)}
              />
            ))}
          </div>
          <div className="costbar-readout">
            {hover !== undefined && slices[hover] ? (
              <>
                <span className="costbar-read-name">
                  {slices[hover].name || slices[hover].agent}
                  <span className="muted"> · {modelName(slices[hover].model)}</span>
                </span>
                <span className="costbar-read-usd">
                  {slices[hover].cost_usd === null ? "未上报金额" : usd(slices[hover].cost_usd)}
                </span>
                <span className="muted small">
                  输入 {compactUsage(slices[hover].cached_input + slices[hover].new_input)}（缓存{" "}
                  {compactUsage(slices[hover].cached_input)}） · 输出{" "}
                  {compactUsage(slices[hover].output)} token
                </span>
              </>
            ) : (
              <span className="muted small">悬停或聚焦一段，看金额与 token</span>
            )}
          </div>
          <ul className="costbar-legend">
            {slices.map((slice, i) => (
              <li
                key={sliceKey(slice)}
                className={hover === i ? "on" : undefined}
                tabIndex={0}
                title={describe(slice)}
                onMouseEnter={() => setHover(i)}
                onMouseLeave={() => setHover(undefined)}
                onFocus={() => setHover(i)}
                onBlur={() => setHover(undefined)}
              >
                <i
                  className={`costbar-dot av-${hueOfSlice(slice, agents.data)}`}
                  style={{ opacity: shades.get(sliceKey(slice)) }}
                />
                <span className="costbar-name">{slice.name || slice.agent}</span>
                <span className="costbar-model">{modelName(slice.model)}</span>
                <span className="spacer" />
                <span className="costbar-tokens">{compactUsage(tokensOf(slice))}</span>
                <span className="costbar-usd">
                  {slice.cost_usd === null ? "—" : usd(slice.cost_usd)}
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
      {data?.notes.map((note, i) => (
        <p className="costbar-note" key={i}>
          {note}
        </p>
      ))}
    </div>
  );
}
