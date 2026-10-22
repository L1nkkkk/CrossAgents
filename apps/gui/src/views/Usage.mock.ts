// Mock data for the usage page (preview:mock): two subscriptions, one close to its 5-hour
// limit and one that hit its weekly limit; spend grouped four ways.

import type { AccountQuota } from "@bindings/AccountQuota";
import type { AccountUsageResponse } from "@bindings/AccountUsageResponse";
import type { QuotaResponse } from "@bindings/QuotaResponse";
import type { UsageGroup } from "@bindings/UsageGroup";
import type { UsageResponse } from "@bindings/UsageResponse";
import { emptyState, type MockState } from "../dev/MockBridge";
import { ago, team } from "../dev/fixtures";
import { plugins } from "./Agents.mock";

const HOUR = 3600;
const inMinutes = (m: number) => new Date(Date.now() + m * 60_000).toISOString();

function account(id: string, provider: string, label: string, status: Partial<AccountQuota["status"]>, agents: string[]): AccountQuota {
  return {
    account: { id, provider, label, home: `C:/Users/me/.crossagents/accounts/${id}`, created_at: ago(60 * 24 * 20) },
    status: {
      logged_in: true,
      auth_mode: "chatgpt",
      email: null,
      plan: null,
      subscription_until: null,
      rate_limits: [],
      limit_reached: false,
      credits: null,
      limits: [],
      reset_credits: [],
      source: "live",
      as_of: ago(1),
      note: null,
      ...status,
    },
    models: [],
    agents,
  };
}

export function quota(): QuotaResponse {
  return {
    providers: ["claude", "codex"],
    accounts: [
      account(
        "claude-max",
        "claude",
        "工作 · Max",
        {
          email: "me@example.com",
          plan: "max",
          rate_limits: [
            { name: "five_hour", used_percent: 93, window_seconds: 5 * HOUR, resets_at: inMinutes(38) },
            { name: "seven_day", used_percent: 61, window_seconds: 7 * 24 * HOUR, resets_at: inMinutes(60 * 52) },
          ],
          limits: [
            { id: "all", name: null, model: null, windows: [], reached: false },
            { id: "opus", name: "Opus", model: "claude-opus-4-1", windows: [{ name: "seven_day_opus", used_percent: 78, window_seconds: 7 * 24 * HOUR, resets_at: inMinutes(60 * 52) }], reached: false },
          ],
        },
        ["ada", "mia", "sam"],
      ),
      account(
        "codex-plus",
        "codex",
        "个人 · Plus",
        {
          plan: "plus",
          rate_limits: [
            { name: "primary", used_percent: 38, window_seconds: 5 * HOUR, resets_at: inMinutes(140) },
            { name: "secondary", used_percent: 100, window_seconds: 7 * 24 * HOUR, resets_at: inMinutes(60 * 30) },
          ],
          limit_reached: true,
          credits: { has_credits: true, unlimited: false, balance: "4.20" },
          note: "每周限额已用完，正在用额外额度",
        },
        ["rex"],
      ),
    ],
  };
}

export function usage(): Partial<Record<UsageGroup, UsageResponse>> {
  return {
    role: {
      by: "role",
      total_cost_usd: 6.8124,
      rows: [
        { key: "planner", agents: 1, sessions: 4, cost_usd: 3.1022, cost_reported: true, peak_context: 92_000 },
        { key: "worker", agents: 6, sessions: 11, cost_usd: 2.9811, cost_reported: true, peak_context: 61_000 },
        { key: "reviewer", agents: 1, sessions: 2, cost_usd: 0.7291, cost_reported: true, peak_context: 28_000 },
      ],
    },
    model: {
      by: "model",
      total_cost_usd: 6.8124,
      rows: [
        { key: "claude-opus-4-1", agents: 2, sessions: 6, cost_usd: 3.8313, cost_reported: true, peak_context: 92_000 },
        { key: "claude-sonnet-4-5", agents: 2, sessions: 4, cost_usd: 2.9811, cost_reported: true, peak_context: 44_000 },
        { key: "gpt-5-codex", agents: 4, sessions: 7, cost_usd: 0, cost_reported: false, peak_context: 61_000 },
      ],
    },
    run: { by: "run", total_cost_usd: 6.8124, rows: [{ key: "OAuth 登录", agents: 7, sessions: 15, cost_usd: 6.8124, cost_reported: true, peak_context: 92_000 }] },
    agent: {
      by: "agent",
      total_cost_usd: 6.8124,
      rows: [
        { key: "ada", agents: 1, sessions: 4, cost_usd: 3.1022, cost_reported: true, peak_context: 92_000 },
        { key: "mia-b", agents: 1, sessions: 2, cost_usd: 1.4211, cost_reported: true, peak_context: 44_000 },
        { key: "rex-a", agents: 1, sessions: 3, cost_usd: 0, cost_reported: false, peak_context: 61_000 },
      ],
    },
  };
}

export function accountUsage(): AccountUsageResponse {
  const day = (key: string, input: number, output: number) => ({
    key,
    totals: { sessions: 3, input_tokens: input, cached_input_tokens: Math.round(input * 0.7), output_tokens: output, reasoning_output_tokens: Math.round(output / 3) },
    cache_hit_rate: 0.7,
  });
  return {
    by: "day",
    rows: [day("2026-10-22", 1_840_000, 92_000), day("2026-10-21", 2_310_000, 120_400), day("2026-10-20", 960_000, 51_000)],
    total: { sessions: 9, input_tokens: 5_110_000, cached_input_tokens: 3_577_000, output_tokens: 263_400, reasoning_output_tokens: 87_800 },
    total_cache_hit_rate: 0.7,
  };
}

/** The usage page: the standing team, two accounts, a week of spend. */
export function usageScene(): MockState {
  const { ada, rex, mia, sam } = team();
  return { ...emptyState(), agents: [ada, rex, mia, sam], plugins: plugins(), quota: quota(), usage: usage(), accountUsage: accountUsage() };
}
