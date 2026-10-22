// Delegation levels as people pick them, mapped to RunPolicy (docs/tasks/H1.md §4). Pure,
// tested in policy.test.ts.

import type { RunPolicy } from "@bindings/RunPolicy";

export type Preset = "manual" | "semi" | "full";

export const PRESETS: { key: Preset; label: string; hint: string }[] = [
  { key: "manual", label: "我审批", hint: "计划等你批准；有问题等你回答；结果由你落地" },
  { key: "semi", label: "半托管", hint: "计划直接开工；问题 30 分钟没回就按默认；结果由你落地" },
  { key: "full", label: "全托管", hint: "直接开工，问题超时按默认，Planner 自己落地到主分支" },
];

export const DEFAULT_POLICY: RunPolicy = {
  plan_approval: "required",
  land: "manual",
  human_ask: "allow",
  ask_timeout_minutes: 30,
  budget: { max_tokens: null, max_usd: null, max_minutes: null },
  spawn: true,
  max_agents: 8,
  spawn_models: null,
  delegate: false,
};

export function presetPolicy(preset: Preset, base: RunPolicy = DEFAULT_POLICY): RunPolicy {
  switch (preset) {
    case "manual":
      return { ...base, plan_approval: "required", land: "manual", human_ask: "allow" };
    case "semi":
      return { ...base, plan_approval: "auto", land: "manual", human_ask: "timeout" };
    case "full":
      return { ...base, plan_approval: "auto", land: "auto", human_ask: "timeout" };
  }
}

/** The preset a policy matches, if any. */
export function presetOf(policy: RunPolicy): Preset | undefined {
  return PRESETS.map((p) => p.key).find((key) => {
    const p = presetPolicy(key, policy);
    return p.plan_approval === policy.plan_approval && p.land === policy.land && p.human_ask === policy.human_ask;
  });
}

/** "$5 · 2 小时" for the caps that are set; empty when none is. */
export function budgetText(budget: RunPolicy["budget"]): string {
  const parts: string[] = [];
  if (budget.max_usd != null) parts.push(`$${budget.max_usd}`);
  if (budget.max_tokens != null) parts.push(`${Math.round(budget.max_tokens / 1000)}k tokens`);
  if (budget.max_minutes != null) {
    const m = budget.max_minutes;
    parts.push(m >= 60 && m % 60 === 0 ? `${m / 60} 小时` : `${m} 分钟`);
  }
  return parts.join(" · ");
}

/** "还剩 12 分钟", "即将", for a question's timeout. */
export function remaining(at: string, now = Date.now()): string {
  const mins = Math.ceil((new Date(at).getTime() - now) / 60_000);
  if (mins <= 0) return "即将";
  if (mins < 60) return `${mins} 分钟后`;
  const h = Math.floor(mins / 60);
  return `${h} 小时 ${mins % 60} 分后`;
}
