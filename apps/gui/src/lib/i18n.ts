// UI strings (docs/tasks/V6-GUI.md §7). `t(key, vars)` looks a key up in the current locale and
// fills `{name}` placeholders. Only zh-CN exists so far; Sidebar.tsx, Home.tsx and strips.tsx
// are the first files moved over, the rest follow. Terms stay as they are in every locale:
// run, Planner, Worker, Reviewer, hub. i18n.test.ts checks that no key is missing.

const zhCN = {
  "common.clear": "清除",
  "common.more": "更多 ({n})",
  "common.less": "收起",
  "common.none": "无",

  "nav.team": "团队",
  "nav.usage": "用量",
  "nav.settings": "设置",

  "state.awaitingApproval": "待审批",
  "state.error": "出错",
  "state.done": "完成",

  "outcome.completed": "完成",
  "outcome.failed": "失败",
  "outcome.cancelled": "已取消",

  "sidebar.search": "搜索",
  "sidebar.searchLabel": "搜索 run、会话和队友",
  "sidebar.newRun": "新建 run",
  "sidebar.live": "进行中",
  "sidebar.sessions": "会话",
  "sidebar.recent": "最近",
  "sidebar.noMatch": "没有匹配「{query}」的 run、会话或队友。",
  "sidebar.empty": "还没有 run 和会话。先添加一个 Planner 和一个 Worker。",
  "sidebar.emptyIn": "{name} 里还没有 run 和会话。先添加一个 Planner 和一个 Worker。",
  "sidebar.addMate": "添加队友",
  "sidebar.working": "working",
  "sidebar.besideHint": "Ctrl+单击或中键：在旁边打开",
  "sidebar.openBeside": "在旁边打开",
  "sidebar.openBesideName": "在旁边打开 {name}",
  "sidebar.merged": "{merged}/{total} 已合并",

  "ws.all": "全部工作区",
  "ws.repos": "{n} 个仓库",
  "ws.empty": "添加队友时填写的工作目录会出现在这里。",

  "quota.mini": "额度 {pct}% · {window}",
  "quota.miniTitle": "{name} · {window}已用 {pct}%",

  "hub.connected": "hub 已连接",
  "hub.connecting": "连接中…",
  "hub.offline": "未连接：{reason}",

  "strip.needsYou": "需要你",
  "strip.doneCount": "{n} 个已完成",
  "strip.clearAll": "全部清除",
  "strip.dismiss": "不再提示 {name}",
  "strip.noTasks": "还没有任务",
  "strip.openRun": "run",

  "home.title": "今天让团队做什么？",
  "home.in": "在",
  "home.subtitle": "Planner 拆计划、按专长派活；{hint}。",
  "home.placeholder": "把登录页改成 OAuth，旧的密码登录保留",
  "home.planner": "Planner",
  "home.worker": "默认 Worker",
  "home.start": "发起 run",
  "home.startHint": "发起（Ctrl+Enter）",
  "home.preset": "托管程度",
  "home.team": "团队",
  "home.teamHint": "Planner 会按专长把任务派给他们，点一下可以让某人这次不参加",
  "home.more": "更多选项",
  "home.titleField": "标题（可选）",
  "home.titlePlaceholder": "不填则由需求生成",
  "home.escalate": "失败后升级到",
  "home.reviewer": "默认 Reviewer",
  "home.parallel": "并行数",
  "home.maxUsd": "预算上限（美元）",
  "home.maxMinutes": "时间上限（分钟）",
  "home.maxTokens": "Token 上限（千）",
  "home.unlimited": "不限",
  "home.budgetHint": "用到 80% 时 Planner 会收到提醒；用完时 run 暂停，等你决定加预算还是取消。",
  "home.notUsed": "（不用）",

  "onboard.label": "开始使用",
  "onboard.mates": "添加队友",
  "onboard.matesHint": "Claude、Codex 或其他 agent，各有专长",
  "onboard.planner": "选一位 Planner",
  "onboard.plannerHint": "用强模型拆计划、派活、验收",
  "onboard.run": "发起第一个 run",
  "onboard.runHint": "在下面写下要做的事",
} as const;

export type Key = keyof typeof zhCN;
export type Dictionary = Record<Key, string>;

export const dictionaries: Record<string, Partial<Dictionary>> = { "zh-CN": zhCN };

let locale = "zh-CN";

/** Switches the language; unknown locales fall back to zh-CN. */
export function setLocale(next: string) {
  locale = dictionaries[next] ? next : "zh-CN";
}

/** The string for `key`, with `{name}` placeholders filled from `vars`. */
export function t(key: Key, vars?: Record<string, string | number>): string {
  const text = dictionaries[locale]?.[key] ?? zhCN[key] ?? key;
  return vars ? text.replace(/\{(\w+)\}/g, (m, name: string) => (name in vars ? String(vars[name]) : m)) : text;
}

/** Keys of zh-CN that `dict` does not have. */
export function missingKeys(dict: Partial<Dictionary>): Key[] {
  return (Object.keys(zhCN) as Key[]).filter((k) => typeof dict[k] !== "string");
}

/** Whether `key` exists. */
export const hasKey = (key: string): key is Key => key in zhCN;
