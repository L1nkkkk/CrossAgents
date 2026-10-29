// Mock data for the usage page (preview:mock): two subscriptions, one close to its 5-hour
// limit and one that hit its weekly limit, and a CodeBuddy login billed in credits; spend
// grouped four ways.

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
    providers: ["claude", "codex", "codebuddy"],
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
      // A second Claude login with no quota figures yet: model menus then group by login.
      account("claude-pro", "claude", "本机 Claude 登录", { plan: "pro", note: "no plan limits (API key or another provider)" }, []),
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
      // Billed in credits: one pool per credit group, each with what is left.
      {
        ...account(
          "codebuddy",
          "codebuddy",
          "本机 CodeBuddy 登录",
          {
            auth_mode: "personal",
            plan: "体验版",
            rate_limits: [
              { name: "套餐基础积分", used_percent: 6.8, window_seconds: null, resets_at: null, remaining: 466, total: 500 },
              { name: "平台奖励积分", used_percent: 0, window_seconds: null, resets_at: null, remaining: 2400, total: 2400 },
            ],
          },
          ["iris"],
        ),
        models: [
          { id: "glm-5.3", name: "GLM-5.3", description: null, context_window: null, credit_multiplier: 0.69 },
          { id: "hy4-preview", name: "Hy4 preview", description: null, context_window: null, credit_multiplier: 0.29 },
          { id: "hy3", name: "Hy3", description: null, context_window: null, credit_multiplier: 0 },
        ],
      },
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
  const day = (key: string, input: number, output: number, credits: number) => ({
    key,
    totals: { sessions: 3, input_tokens: input, cached_input_tokens: Math.round(input * 0.7), output_tokens: output, reasoning_output_tokens: Math.round(output / 3), credits },
    cache_hit_rate: 0.7,
  });
  return {
    by: "day",
    rows: [day("2026-10-22", 1_840_000, 92_000, 11.6), day("2026-10-21", 2_310_000, 120_400, 15.4), day("2026-10-20", 960_000, 51_000, 7)],
    total: { sessions: 9, input_tokens: 5_110_000, cached_input_tokens: 3_577_000, output_tokens: 263_400, reasoning_output_tokens: 87_800, credits: 34 },
    total_cache_hit_rate: 0.7,
  };
}

/** The usage page: the standing team, two accounts, a week of spend. */
export function usageScene(usageSeriesVariant: "normal" | "empty" | "issues" | "session" = "normal"): MockState {
  const { ada, rex, mia, sam } = team();
  return { ...emptyState(), agents: [ada, rex, mia, sam], plugins: plugins(), quota: quota(), usage: usage(), accountUsage: accountUsage(), usageSeriesVariant };
}

/** Fixture for the series endpoint, with range and source filtering. */
export function usageSeriesMock(range: string, group: string, source: string, variant = "normal") {
  const length = range === "all" ? 120 : range === "90d" ? 90 : range === "30d" ? 30 : 7;
  const days = Array.from({length}, (_,i) => {
    const date = new Date(); date.setDate(date.getDate() - length + i + 1);
    const scale = variant === "empty" ? 0 : source === "codex-plus" ? .4 : 1;
    const cached_input = Math.round((400000 + ((i * 7919 + i * i * 28411) % 1300000)) * scale);
    const new_input = Math.round((100000 + ((i * 4219 + i * i * 13711) % 500000)) * scale);
    const output = Math.round((50000 + ((i * 3127 + i * i * 7817) % 250000)) * scale);
    return {date: date.toISOString().slice(0,10), cached_input,new_input,output,reasoning:Math.round(output/3),cost_usd:scale ? (cached_input+new_input+output)/1000000*.8 : null};
  });
  const sum = (key: "cached_input" | "new_input" | "output") => days.reduce((a,d)=>a+d[key],0);
  const cached_input=sum("cached_input"), new_input=sum("new_input"), output=sum("output");
  return {range,group,source,granularity:variant === "session" ? "session" : "request",first_day:variant === "empty" ? null : days[0].date,days,groups:group === "total" ? [] : [{key:"a",label:group === "model" ? "claude-opus-4-1" : "CrossAgents",cached_input,new_input,output,cost_usd:days.reduce((a,d)=>a+(d.cost_usd??0),0)}],total:{cached_input,new_input,output,reasoning:Math.round(output/3),sessions:variant === "empty" ? 0 : length*3,turns:variant === "session" ? null : variant === "empty" ? 0 : length*8,max_request:variant === "session" || variant === "empty" ? null : 128000,cost_usd:variant === "empty" ? null : days.reduce((a,d)=>a+(d.cost_usd??0),0)},previous_total:range === "all" ? null : (cached_input+new_input+output)/1.44,unpriced_models:variant === "issues" ? ["unknown-model"] : [],sources:[{id:"claude-max",label:"工作 · Max",provider:"claude"},{id:"codex-plus",label:"个人 · Plus",provider:"codex",...(variant === "issues" ? {error:"无法读取本地日志"} : {})}],notes:[variant === "session" ? "按会话开始日统计；无法确定单次请求和轮数。" : "按请求日期统计；费用为公开 API 价格估算。"]};
}
