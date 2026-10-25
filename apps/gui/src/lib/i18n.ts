// UI strings (docs/tasks/V6-GUI.md §7). `t(key, vars)` looks a key up in the current locale and
// fills `{name}` placeholders. Only zh-CN exists so far; Sidebar.tsx, Home.tsx and strips.tsx
// are the first files moved over, the rest follow. Terms stay as they are in every locale:
// run, Planner, Worker, Reviewer, hub. i18n.test.ts checks that no key is missing.

const zhCN = {
  "common.clear": "清除",
  "common.more": "更多 ({n})",
  "common.less": "收起",
  "common.none": "无",

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
  "sidebar.new": "新建",
  "sidebar.newIn": "在 {name} 里新建",
  "sidebar.pinned": "置顶",
  "sidebar.pin": "置顶",
  "sidebar.archive": "归档（连同它招来的队友）",
  "sidebar.archived": "已归档 ({n})",
  "sidebar.restore": "恢复",
  "sidebar.unpin": "取消置顶",
  "sidebar.showTeam": "展开队友",
  "sidebar.hideTeam": "收起队友",
  "sidebar.noMatch": "没有匹配「{query}」的 run 或会话。",
  "sidebar.empty": "还没有会话。点「新建」开始。",
  "sidebar.besideHint": "Ctrl+单击或中键：在旁边打开",
  "sidebar.openBeside": "在旁边打开",
  "sidebar.merged": "{merged}/{total} 已合并",


  "quota.mini": "额度 {pct}% · {window}",
  "quota.miniTitle": "{name} · {window}已用 {pct}%",

  "hub.connected": "hub 已连接",
  "hub.connecting": "连接中…",
  "hub.offline": "未连接：{reason}",
  "hub.dev": "正在用开发构建的 hub，不是自带的 v{shipped}",
  "hub.devTitle": "{exe}\n换成自带的会重启 hub，正在跑的 agent 不受影响",
  "hub.useShipped": "换成自带的",
  "hub.switching": "切换中…",

  "strip.needsYou": "需要你",
  "strip.doneCount": "{n} 个已完成",
  "strip.clearAll": "全部清除",
  "strip.dismiss": "不再提示 {name}",
  "strip.noTasks": "还没有任务",
  "strip.openRun": "run",

  "home.title": "今天想做点什么？",
  "home.placeholder": "描述要做的事，比如：把登录页改成 OAuth，旧的密码登录保留",
  "home.mode": "怎么做",
  "home.chat": "Chat",
  "home.chatHint": "一个 agent 直接做，需要时自己找帮手；也可以在消息里用 @ 点名协作者或模型。可以不选项目",
  "home.run": "Work",
  "home.runHint": "交给主导的 agent：拆计划、招人派活，各自在独立分支里做，验收后合并。要选项目",
  "home.chatPlaceholder": "有什么可以帮你？输入 @ 点名协作者或模型",
  "home.model": "模型",
  "home.effort": "推理强度",
  "home.effortShort": "推理",
  "home.effortDefault": "默认",
  "home.defaultLogin": "默认登录",
  "home.noProject": "不选项目",
  "home.noProjectHint": "hub 给这次对话单独建一个目录",
  "home.send": "发送",
  "home.sendHint": "发送（Ctrl+Enter）",
  "home.chooseWorkspace": "选工作区",
  "home.pickFolder": "选择文件夹…",
  "home.lead": "谁来主导",
  "home.leadTitle": "主导",
  "home.existing": "已有的",
  "home.auto": "自动",
  "home.noModels": "hub 没有提供任何模型，先检查插件和账号。",
  "home.needWorkspace": "选一个工作区，agent 会在那里干活。",
  "home.options": "选项",
  "home.worker": "默认 Worker",
  "home.start": "发起 run",
  "home.startHint": "发起（Ctrl+Enter）",
  "home.preset": "托管程度",
  "home.titleField": "标题（可选）",
  "home.titlePlaceholder": "不填则由需求生成",
  "home.escalate": "失败后升级到",
  "home.reviewer": "默认 Reviewer",
  "home.parallel": "并行数",
  "home.maxUsd": "预算上限（美元）",
  "home.maxMinutes": "时间上限（分钟）",
  "home.maxTokens": "Token 上限（千）",
  "home.unlimited": "不限",
  "home.notUsed": "（不用）",
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
