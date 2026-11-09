#!/usr/bin/env node
// 旧日志回放：按新通知分类和新叫醒规则，估算会少叫醒多少次。
//
// 用法：
//   node scripts/replay-wakeups.mjs [--agent <名字或 id>] [--from <ISO>] [--to <ISO>]
//                                   [--json] [--all] <events-*.jsonl ...>
//
// 做什么：
//   1. 读 events-*.jsonl（可多个文件），按 agent 和时间段重建回合：
//      agent.turn_started / agent.turn_completed / agent.input 的原文 /
//      agent.usage 的用量增量（会话内累计值之差）。
//   2. 给每个回合（= 一次唤醒）按新分类归类：silent / batch / unblock / decide /
//      run_done，以及不属于通知的 human / teammate / orchestrator（人的消息、队友消息、
//      协调器转来的请求，照旧立刻处理）。
//   3. 按新规则模拟：Silent 不叫醒（跟着下一次唤醒走）；Batch 在最后一条之后静默
//      60 秒或从第一条起最多 180 秒（先到为准）合并；Unblock / Decide / RunDone 立刻
//      叫醒；消息照旧立刻处理。同一条消息里的通知一起去。
//   4. 输出实际的唤醒次数和输入 token（按分类）、新规则下估计的唤醒次数、没做决定的
//      回合。
//
// 日志里有什么就直接用什么：
//   - 有 agent.round：触发它的消息、来源、带上的通知和分类、用量、工具、decided 都按事件
//     原样使用，不再推断。
//   - 有 notice.recorded：通知行、分类、时间按事件原样使用。
//
// 旧日志没有上面这些事件，下面是从通知原文和消息来源推断的规则（输出里标「推断」）。
// 推断规则按顺序匹配，先命中先用：
//
//   A. 消息（不是通知摘要）：
//      - from.kind = human            → human，人的消息照旧立刻处理。
//      - from.kind = orchestrator     → orchestrator，run 请求（人的原话转来），立刻。
//      - from.kind = agent 且正文带交付尾注（一条 --- 之后是 task #<key> (<id>) · branch）
//                                   → 等接受：默认 batch；敏感性测试里改按 unblock。
//      - from.kind = agent 且正文以 "Review of #<key> …: approved" 开头
//                                   → 等接受：同上。
//      - "Review of #<key> …: changes needed"
//                                   → silent，自动返工中，lead 不用动。
//      - 其它 from.kind = agent      → teammate，Agent 之间的 agent_send 消息，照旧立刻。
//   B. 通知摘要（"CrossAgents notices (things you asked to hear about):" 下的 "- " 行）：
//      - "All N task(s) of run … have ended"        → run_done。
//      - "Stage N of run … has ended"               → decide（新代码里阶段结束叫 lead）。
//      - "Task … is stalled" / "Task … failed" / "ended its turn without a verdict" /
//        "The hub restarted" / "Not landed:" / "cannot be opened:" / 返工超限 / bounce
//                                                   → decide，失败或要决定。
//      - "Reminder (…)"                             → decide，主人自己设的提醒。
//      - "The plan of run … was approved"           → decide。
//      - "Task #… is now passed."                   → 等接受：默认 batch；敏感性测试里改按 unblock。
//      - "Task #… is now waiting_answer."           → decide，等 lead 回答。
//      - "Task #… is now merged."                   → silent，已接受，只是进展。
//      - "Progress from …"                          → silent，进度。
//      - "#… 已派给 …"                              → silent，已派出。
//      - "#… (id): given to … instead: …"            → silent，改派。
//      - "Task #… is now <其它状态>."                → silent，状态变化。
//      - quota 的三类（crates/orchestrator/src/quota.rs）：
//          · 过期提示 "…% of its … left … dispatch more work"：Batch（65 行附近）
//          · 改派 "…: given to … instead: …"：Silent（92 行附近；本脚本按改派行归类）
//          · "its worker hit its quota (…), and no other account or teammate has quota
//            left."：Decide（130 行附近）
//      - watch.triggered 里逐字相同、但上面都认不出来的行
//                                                   → decide，主人自己设的 watch。
//      - 认不出来的行                                → decide（保守：不把没认出来的通知省掉），
//        并在输出里单独计数。
//
//   旧日志里 lead 自己 watch 了每个任务，watch 行就是「Task #x is now passed / merged」这类
//   任务状态行，和新规则要发的任务状态通知是同一件事，所以按内容归类（passed → 等接受，
//   merged → silent），不按 watch 一律算 decide——否则「等接受」永远合并不了，估算会失真。
//   watch 只在内容认不出来时兜底按 decide。输出里的「参考」把交付报告按现在的消息算
//   （立刻叫醒，见 crates/hub/src/notices.rs:224），用来看「报告还是消息」时会是多少。
//
// 推断的边界（输出里也会写）：
//   - 等接受（passed / 评审通过 / 交付报告）到底是 Batch 还是 Unblock，要看「接受后会不会
//     解锁下游任务」。旧日志没有依赖图（run.plan_submitted 只列任务 key；agent.tool_detail
//     里的 depends_on 只覆盖 8 个任务、3 个有依赖，重建不全），所以两种口径各算一次：
//       乐观：全部按 batch（能合并，叫醒更少、节省更多）
//       保守：全部按 unblock（立刻叫醒）
//     全按 batch 会高估节省，不是偏保守。
//   - 交付报告和评审通过的旧形态是 worker 的消息（现在的代码里会立刻叫醒），本脚本默认把
//     它们当新流程的「等接受」通知；这是最大的一个假设，输出里单列。
//   - 消息不合并：每条消息各自是一次唤醒（旧日志 165 个回合对应 165 个 message_id）；
//     忙时到达就顺延到前一个回合结束，但两条消息不会因为同时顺延而并成一次。
//     待带的通知可以跟着消息走。
//   - 「没做决定」有两个口径：事件口径（状态变化等效果，见下）和工具口径
//     （agent.tool_call + agent.tool_detail 里的 toolName，对照 crates/hub/src/rounds.rs
//     的 decides()）。有工具记录时以工具为准，没有才用事件；工具记录不全（只有 hub 工具
//     带名字，grep/glob 等驱动工具不算），输出给区间。
//     事件口径的决定信号：接受（status→merged）、返工（passed→running，可能是 hub 自动
//     返工）、取消（→cancelled）、task_assign（task.handed_off 的 from 是它）、提交计划
//     （run.plan_submitted / run.status_changed→awaiting_approval）、开 run
//     （run.created 的 planner 是它）、落地（run.landed 的 by 是它）、写笔记 / 记问题
//     （note.written / issue.recorded 的 author 是它）、问人（human.asked 的 from 是它）、
//     给队友发消息（其它 agent 的 agent.input 的 from.agent_id 是它）。只读工具、进展汇报、
//     检查不算决定；推送、模型切换等没有 actor 的效果不算。
//   - 回合时间按 turn_started→turn_completed；用量增量按 agent.usage 的会话累计值做差，
//     遇到数字变小当作新会话。没有用量上报的回合 token 记为未知。
//   - 新规则模拟不模拟「新回合本身要跑多久」，只避开日志里实际存在的回合：通知到达时
//     agent 正在回合里，就叫醒顺延到那个回合结束（真实 hub 也会等回合结束再发下一轮）。
//     所以同一条叫醒消息发出后、回合开始前（旧日志里 0.1–1 秒）到达的通知，本脚本不会并进去。
//   - Batch 边界：最后一条之后正好第 60 秒到达的通知，先叫醒、它算下一批（脚本里
//     wakeAt <= nextArrival 时先发唤醒）。第一条起第 180 秒同理。
//   - 被撤回的摘要（agent.input_withdrawn）里的行不进到达；它们通常会重新出现在后面的
//     消息里，输出里单独计数说明。
//
// 所有数字都是估算，不是精确值。

import fs from "node:fs";
import readline from "node:readline";
import { pathToFileURL } from "node:url";

const QUIET_MS = 60_000;
const MAX_MS = 180_000;
const CLASS_RANK = { unblock: 0, decide: 1, run_done: 2, batch: 3, silent: 4, other: 5 };
const IMMEDIATE = new Set(["unblock", "decide", "run_done"]);
const MESSAGE_CLASSES = new Set(["human", "teammate", "orchestrator"]);

// crates/hub/src/rounds.rs 的 decides()：这一轮调用过其中任一工具就算做了决定
//（旧日志没有成败标记，不看成功与否）。
const DECIDES = new Set([
  "agent_send", "message_withdraw", "human_ask", "human_notify", "task_ask", "task_answer",
  "note_write", "lesson_create", "lesson_merge", "lesson_report", "branch_sync",
  "remind_set", "remind_cancel", "watch", "watch_cancel",
  "work_start", "run_next", "plan_submit", "run_land", "run_finish", "run_apply", "run_retro",
  "task_create", "task_update", "task_assign", "task_cancel", "task_complete", "task_fail",
  "task_accept", "task_verify", "task_rework", "task_drop", "review_request", "review_submit",
  "agent_spawn", "agent_retire", "agent_set_model", "agent_interrupt", "issue_keep",
  "expertise_triage", "extension_request_enable",
]);
// 只读的 hub 工具：出现它们说明这一轮的工具记录是齐的，但没做决定。
const READ_TOOLS = new Set([
  "task_get", "task_list", "note_read", "note_list", "lesson_get", "lesson_search",
  "checks_run", "usage_get", "issue_pending", "workspace_read", "workspace_diff",
  "inbox_read", "agent_list", "agent_get", "whoami", "expertise_search", "expertise_get",
  "remind_list", "watch_list",
]);
const HUB_TOOLS = new Set([...DECIDES, ...READ_TOOLS]);
const ALL_CLASSES = ["unblock", "decide", "run_done", "batch", "silent", "human", "teammate", "orchestrator", "other"];

const KEEP_TYPES = new Set([
  "agent.input",
  "agent.input_updated",
  "agent.steered",
  "agent.turn_started",
  "agent.turn_completed",
  "agent.session_started",
  "agent.usage",
  "agent.round",
  "agent.created",
  "agent.updated",
  "agent.tool_call",
  "agent.tool_detail",
  "agent.tool_used",
  "agent.prompt_via_file",
  "agent.status_changed",
  "notice.recorded",
  "notice.carried",
  "task.status_changed",
  "task.handed_off",
  "run.created",
  "run.plan_submitted",
  "run.status_changed",
  "run.landed",
  "note.written",
  "issue.recorded",
  "human.asked",
  "watch.triggered",
  "reminder.fired",
]);

// ---------------------------------------------------------------------------
// 读事件

function typeOf(line) {
  const at = line.indexOf('"type":"');
  if (at < 0) return null;
  const end = line.indexOf('"', at + 8);
  return end < 0 ? null : line.slice(at + 8, end);
}

/// 读若干 jsonl 文件，只留回放用得到的事件；坏行跳过。
export async function readEvents(paths) {
  const events = [];
  let bad = 0;
  let lines = 0;
  for (const path of paths) {
    const input = fs.createReadStream(path, "utf8");
    const rl = readline.createInterface({ input, crlfDelay: Infinity });
    for await (const line of rl) {
      if (!line.trim()) continue;
      lines++;
      const type = typeOf(line);
      if (!type || !KEEP_TYPES.has(type)) continue;
      let event;
      try {
        event = JSON.parse(line);
      } catch {
        bad++;
        continue;
      }
      event.ms = Date.parse(event.ts);
      if (Number.isNaN(event.ms)) continue;
      event.data ??= {};
      events.push(event);
    }
  }
  events.sort((a, b) => a.ms - b.ms || (a.seq ?? 0) - (b.seq ?? 0));
  return { events, lines, bad };
}

// ---------------------------------------------------------------------------
// 名字与参数

export function agentNames(events) {
  const names = new Map();
  for (const e of events) {
    if (e.type !== "agent.created" && e.type !== "agent.updated") continue;
    const a = e.data.agent ?? e.data;
    if (a?.id && a.name) names.set(a.id, a.name);
  }
  return names;
}

export function resolveAgent(events, spec) {
  const names = agentNames(events);
  const counts = new Map();
  for (const e of events) {
    if (e.type !== "agent.turn_started" || !e.data.id) continue;
    counts.set(e.data.id, (counts.get(e.data.id) ?? 0) + 1);
  }
  if (!spec) {
    let best = null;
    for (const [id, n] of counts) if (!best || n > best.n) best = { id, n };
    if (!best) throw new Error("日志里没有 agent.turn_started，无法判断看哪个 agent");
    return { id: best.id, name: names.get(best.id) ?? best.id, guessed: true };
  }
  if (counts.has(spec) || names.has(spec)) {
    return { id: spec, name: names.get(spec) ?? spec, guessed: false };
  }
  const lower = spec.toLowerCase();
  for (const [id, name] of names) {
    if (name.toLowerCase() === lower) return { id, name, guessed: false };
  }
  const candidates = [...counts]
    .sort((a, b) => b[1] - a[1])
    .slice(0, 8)
    .map(([id, n]) => `${names.get(id) ?? id} (${id}, ${n} 回合)`)
    .join("\n  ");
  throw new Error(`找不到 agent ${JSON.stringify(spec)}；日志里回合最多的有：\n  ${candidates}`);
}

// ---------------------------------------------------------------------------
// 回合重建

function newTokens() {
  return { input: 0, cached: 0, cacheWrite: 0, output: 0, reasoning: 0, requests: 0 };
}

function emptyUsage() {
  return { input: 0, cached: 0, cacheWrite: 0, output: 0, reasoning: 0, requests: 0 };
}

function usageOf(tokens) {
  return {
    input: tokens.input ?? 0,
    cached: tokens.cached_input ?? 0,
    cacheWrite: tokens.cache_write_input ?? 0,
    output: tokens.output ?? 0,
    reasoning: tokens.reasoning_output ?? 0,
    requests: tokens.requests ?? 0,
  };
}

const USAGE_KEYS = ["input", "cached", "cacheWrite", "output", "reasoning", "requests"];

/// 会话累计值的增量；数字变小当作新会话（从 0 起算）。
export function grow(prev, cur) {
  const fresh = !prev || cur.input < prev.input || cur.output < prev.output;
  const base = fresh ? emptyUsage() : prev;
  const delta = newTokens();
  for (const k of USAGE_KEYS) delta[k] = Math.max(0, (cur[k] ?? 0) - (base[k] ?? 0));
  return { delta, fresh };
}

function addTokens(into, delta) {
  for (const k of USAGE_KEYS) into[k] += delta[k];
}

/// 每个回合：触发它的消息和原文、来源、时间、用量增量。
export function reconstructRounds(events, agentId, window = {}) {
  const from = window.from ?? -Infinity;
  const to = window.to ?? Infinity;
  const inputs = new Map(); // message_id -> {text, from, ms}
  const rounds = [];
  let open = null;
  let prevUsage = null;
  let sawUsage = false; // 整个日志里有没有用量上报
  const stats = { inputs: 0, withdrawn: 0, missingInput: 0, orphanUsage: 0 };

  const closeOpen = (ms) => {
    if (!open) return;
    open.end = ms;
    if (open.start >= from && open.start <= to) rounds.push(open);
    open = null;
  };
  for (const e of events) {
    const d = e.data ?? {};

    // 消息原文（先收着，turn_started 时取）。
    if (e.type === "agent.input" && d.id === agentId) {
      stats.inputs++;
      inputs.set(d.message_id, { text: d.text ?? "", from: d.from ?? null, ms: e.ms });
      continue;
    }
    if (e.type === "agent.input_updated" && d.id === agentId) {
      const m = inputs.get(d.message_id);
      if (m) m.text = d.text ?? "";
      continue;
    }
    if (e.type === "agent.input_withdrawn" && d.id === agentId) {
      stats.withdrawn++;
      inputs.delete(d.message_id);
      continue;
    }
    if (e.type === "agent.steered" && d.id === agentId) {
      if (open) {
        open.steered.push({ messageId: d.message_id, text: d.text ?? "", from: d.from ?? null });
      }
      continue;
    }

    if (e.type === "agent.session_started" && d.id === agentId) {
      prevUsage = emptyUsage();
      continue;
    }
    if (e.type === "agent.status_changed" && d.id === agentId && d.to === "starting") {
      prevUsage = emptyUsage();
      continue;
    }

    if (e.type === "agent.turn_started" && d.id === agentId) {
      closeOpen(e.ms);
      const msg = inputs.get(d.message_id);
      open = {
        agent: agentId,
        messageId: d.message_id,
        start: e.ms,
        end: null,
        text: msg?.text ?? "",
        from: msg?.from ?? null,
        inputMs: msg?.ms ?? e.ms,
        steered: [],
        tokens: null,
        roundEvent: null,
        tools: [],
        decided: null,
        decidedWhy: [],
      };
      if (!msg) stats.missingInput++;
      continue;
    }
    if (e.type === "agent.turn_completed" && d.id === agentId) {
      if (open && (d.message_id == null || d.message_id === open.messageId)) closeOpen(e.ms);
      continue;
    }

    if (e.type === "agent.usage" && d.id === agentId) {
      if (!d.tokens) continue;
      sawUsage = true;
      const cur = usageOf(d.tokens);
      const { delta } = grow(prevUsage, cur);
      prevUsage = cur;
      if (open) {
        open.tokens ??= newTokens();
        addTokens(open.tokens, delta);
      } else {
        stats.orphanUsage++;
      }
      continue;
    }

    if (e.type === "agent.round" && d.id === agentId) {
      const ids = d.message_ids ?? [];
      const target =
        (open && ids.includes(open.messageId) && open) ||
        rounds.find((r) => ids.includes(r.messageId)) ||
        null;
      if (target) {
        target.roundEvent = e;
        target.tools = d.tools ?? [];
        target.decided = Boolean(d.decided);
        target.promptViaFile = Boolean(d.prompt_via_file);
        // 没有 agent.usage 时（比如外部 agent），回合事件自己报的用量也算数。
        if (!target.tokens && d.tokens) {
          const t = usageOf(d.tokens);
          t.requests = d.tokens.requests ?? 0;
          target.tokens = t;
        }
      }
      continue;
    }
  }
  closeOpen(null);

  rounds.sort((a, b) => a.start - b.start);
  const calls = toolCallRecords(events, agentId);
  // 文字和来源先算好，分类在 replay 里补（旧日志的 watch 行要等事件都读完才认识）。
  for (const r of rounds) {
    r.digest = parseDigest(r.text);
    if (r.roundEvent) {
      const notices = r.roundEvent.data.notices ?? [];
      r.items = notices.map((n) => ({ line: n.line, class: n.class, why: "agent.round", inferred: false }));
      r.source = sourceFromRound(r.roundEvent.data.sources ?? []);
      r.class = roundClass(r);
    } else {
      r.items = [];
      r.source = sourceOfMessage(r.from, r.digest.isDigest);
    }
    attachTools(r, calls);
  }
  return { rounds, stats, sawUsage };
}

/// 这一轮调过哪些 hub 工具。名字在 agent.tool_detail 的 arguments.toolName 里；
/// agent.tool_call 只用来判断「这一轮确实有工具调用记录」（包括 grep/glob 这类读工具）。
function toolCallRecords(events, agentId) {
  const calls = [];
  const drive = [];
  for (const e of events) {
    if (e.data?.id !== agentId) continue;
    if (e.type === "agent.tool_call") {
      drive.push(e.ms);
      if (typeof e.data.title === "string" && HUB_TOOLS.has(e.data.title)) {
        calls.push({ ms: e.ms, name: e.data.title });
      }
      continue;
    }
    if (e.type === "agent.tool_detail") {
      let args = null;
      try {
        args = JSON.parse(e.data.detail?.arguments ?? "{}");
      } catch {
        args = null;
      }
      const name = typeof args?.toolName === "string" ? args.toolName : null;
      if (name && HUB_TOOLS.has(name)) calls.push({ ms: e.ms, name });
    }
  }
  return { calls, drive: drive.sort((a, b) => a - b) };
}

function attachTools(round, calls) {
  const end = round.end ?? round.start;
  const inside = (ms) => ms >= round.start && ms <= end;
  const names = [...new Set(calls.calls.filter((c) => inside(c.ms)).map((c) => c.name))];
  const has = names.length > 0 || calls.drive.some(inside);
  round.toolNames = names;
  round.hasToolCall = has;
  round.decidedTool = has ? names.some((n) => DECIDES.has(n)) : null;
}

function sourceFromRound(sources) {
  if (!sources.length) return "unknown";
  const kinds = new Set(sources.map((s) => s.kind));
  if (kinds.has("human")) return "human";
  if (kinds.has("reminder")) return "reminder";
  if (kinds.has("teammate")) return "teammate";
  if (kinds.has("orchestrator")) return "orchestrator";
  return "notice";
}

function sourceOfMessage(from, isDigest) {
  if (isDigest) return "notice";
  const kind = from?.kind;
  if (kind === "human") return "human";
  if (kind === "orchestrator") return "orchestrator";
  if (kind === "agent") return "teammate";
  return "unknown";
}

// ---------------------------------------------------------------------------
// 通知原文 -> 分类

/// 通知摘要里的每一条（第一行是 CrossAgents notices…，条目以 "- " 开头，续行并入上一条）。
export function parseDigest(text) {
  const t = (text ?? "").replace(/\r\n/g, "\n");
  if (!/^\s*CrossAgents notices\b/.test(t)) return { isDigest: false, items: [] };
  const items = [];
  let cur = null;
  for (const line of t.split("\n")) {
    if (line.startsWith("- ")) {
      if (cur) items.push(cur);
      cur = { line: line.slice(2) };
    } else if (cur) {
      cur.line += "\n" + line;
    }
  }
  if (cur) items.push(cur);
  return { isDigest: true, items: items.map((i) => ({ line: i.line.trimEnd() })) };
}

const RE = {
  runEnded: /^All \d+ task\(s\) of run \S+ have ended/,
  stageEnded: /^Stage \d+ of run \S+ has ended/,
  stalled: /^Task .* is stalled/,
  failed: /^Task .* failed/,
  lostTurn: /ended its turn without a verdict|ended a turn without (a verdict|reporting)/,
  restarted: /^The hub restarted/,
  notLanded: /^Not landed:/,
  nextFailed: /cannot be opened:/,
  overLimit: /\bbounc|返工超限|超出返工|超过返工/,
  planApproved: /^The plan of run .* was approved/,
  passed: /^Task #\S+ is now passed\./,
  waitingAnswer: /^Task #\S+ is now waiting_answer\./,
  merged: /^Task #\S+ is now merged\./,
  settledStatus: /^Task #\S+ is now (running|retrying|verifying|reviewing|stalled|failed|cancelled)\./,
  reviewApproved: /^Review of #\S+.*: approved/,
  reviewChanges: /^Review of #\S+.*: changes needed/,
  progress: /^Progress from /,
  dispatched: /^#\S+ 已派给 /,
  rerouted: /^#\S+ \([0-9a-f-]{36}\): given to .* instead/,
  quota: /has \d+% of its .* left|resets? in .*: dispatch more work|dispatch more work (there|while)/,
  quotaOut: /hit its quota .*no other account or teammate has quota left/,
  reminder: /^Reminder \(/,
};

/// 一行通知的分类。ctx.watchLines 是 watch.triggered 里出现过的行（内容认不出来时才兜底）。
export function classifyNoticeLine(line, ctx = {}) {
  const s = String(line).replace(/\s+/g, " ").trim();
  if (!s) return { class: "silent", why: "empty" };
  if (RE.reminder.test(s)) return { class: "decide", why: "reminder" };
  if (RE.runEnded.test(s)) return { class: "run_done", why: "run ended" };
  if (RE.stageEnded.test(s)) return { class: "decide", why: "stage ended" };
  if (RE.stalled.test(s) || RE.failed.test(s)) return { class: "decide", why: "failure" };
  if (RE.lostTurn.test(s)) return { class: "decide", why: "lost turn" };
  if (RE.restarted.test(s)) return { class: "decide", why: "restart" };
  if (RE.notLanded.test(s)) return { class: "decide", why: "landing failed" };
  if (RE.nextFailed.test(s)) return { class: "decide", why: "next step failed" };
  if (RE.overLimit.test(s)) return { class: "decide", why: "rework over limit" };
  if (RE.planApproved.test(s)) return { class: "decide", why: "plan approved" };
  if (RE.quotaOut.test(s)) return { class: "decide", why: "quota exhausted" };
  if (RE.waitingAnswer.test(s)) return { class: "decide", why: "waiting for the lead" };
  if (RE.passed.test(s)) return { class: "batch", why: "passed, awaiting acceptance" };
  if (RE.reviewApproved.test(s)) return { class: "batch", why: "review approved" };
  if (RE.reviewChanges.test(s)) return { class: "silent", why: "auto rework" };
  if (RE.merged.test(s)) return { class: "silent", why: "merged" };
  if (RE.progress.test(s)) return { class: "silent", why: "progress" };
  if (RE.dispatched.test(s)) return { class: "silent", why: "dispatched" };
  if (RE.rerouted.test(s)) return { class: "silent", why: "rerouted" };
  if (RE.quota.test(s)) return { class: "batch", why: "quota tip" };
  if (RE.settledStatus.test(s)) return { class: "silent", why: "status" };
  if (ctx.watchLines?.has(s)) return { class: "decide", why: "watch" };
  return { class: "decide", why: "unknown" };
}

const REPORT_TRAILER = /\ntask #\S+ \([0-9a-f-]{36}\) · branch /;

/// 不是通知摘要的一条消息本身属于哪一类（旧日志按消息来源和正文推断）。
export function classifyMessage(text, from) {
  const t = (text ?? "").replace(/\r\n/g, "\n");
  const kind = from?.kind;
  if (kind === "human") return { class: "human", why: "person" };
  if (kind === "orchestrator") return { class: "orchestrator", why: "run request (person)" };
  if (kind === "agent") {
    if (REPORT_TRAILER.test(t)) return { class: "batch", why: "report, awaiting acceptance" };
    if (RE.reviewApproved.test(t.trim())) return { class: "batch", why: "review approved" };
    if (RE.reviewChanges.test(t.trim())) return { class: "silent", why: "auto rework" };
    return { class: "teammate", why: "teammate message" };
  }
  return { class: "other", why: `unknown source ${kind ?? "?"}` };
}

export function roundClass(round) {
  if (round.roundEvent) {
    const source = round.source;
    if (source === "human" || source === "teammate" || source === "orchestrator") return source;
    if (!round.items.length) return "other";
    return maxClass(round.items.map((i) => i.class));
  }
  if (round.digest?.isDigest) {
    if (!round.items.length) return "other";
    return maxClass(round.items.map((i) => i.class));
  }
  return classifyMessage(round.text, round.from).class;
}

/// 给没有 agent.round 的回合补上逐行分类。旧日志的 watch 行要等这里才认识。
export function classifyRoundItems(rounds, watchLines) {
  for (const r of rounds) {
    if (r.roundEvent) continue;
    if (r.digest?.isDigest) {
      r.items = r.digest.items.map((item) => {
        const c = classifyNoticeLine(item.line, { watchLines });
        return { line: item.line, class: c.class, why: c.why, inferred: true };
      });
      r.class = r.items.length ? maxClass(r.items.map((i) => i.class)) : "other";
    } else {
      r.items = [];
      const c = classifyMessage(r.text, r.from);
      r.class = c.class;
      r.why = c.why;
    }
  }
}

export function maxClass(classes) {
  let best = "silent";
  for (const c of classes) {
    const rank = CLASS_RANK[c] ?? CLASS_RANK.other;
    if (rank < (CLASS_RANK[best] ?? 99)) best = c;
  }
  return best;
}

// ---------------------------------------------------------------------------
// 通知到达时间（旧日志：摘要文本的版本差）

/// 一条消息的摘要每行第一次出现的时刻：agent.input 时刻是初始版本，之后每次
/// agent.input_updated 里新出现的行按那次更新的时刻算。
export function digestArrivals(events, agentId) {
  const borne = new Map(); // message_id -> Map(line -> ms)
  for (const e of events) {
    if (e.type === "agent.input" && e.data.id === agentId) {
      const items = parseDigest(e.data.text).items;
      borne.set(e.data.message_id, new Map(items.map((i) => [i.line, e.ms])));
    } else if (e.type === "agent.input_updated" && e.data.id === agentId) {
      const before = borne.get(e.data.message_id);
      if (!before) continue;
      const next = new Map();
      for (const item of parseDigest(e.data.text).items) {
        next.set(item.line, before.has(item.line) ? before.get(item.line) : e.ms);
      }
      borne.set(e.data.message_id, next);
    }
  }
  return borne;
}

function watchLineSet(events, agentId) {
  const set = new Set();
  for (const e of events) {
    if (e.type !== "watch.triggered" || e.data.owner !== agentId) continue;
    if (e.data.line) set.add(String(e.data.line).replace(/\s+/g, " ").trim());
  }
  return set;
}

// ---------------------------------------------------------------------------
// 新规则模拟

function occupiedEnd(ms, turns) {
  let at = ms;
  for (let guard = 0; guard < 1000; guard++) {
    const inside = turns.find((t) => t.start <= at && at < t.end);
    if (!inside) return at;
    at = inside.end;
  }
  return at;
}

/// arrivals: [{ms, class, kind?, ...}]。kind = "message" 的一条消息各自算一次唤醒
/// （忙时就顺延到前一个回合结束，但两条消息不会并成一次）；其余按通知的类合并。
/// turns: [{start, end}] 是日志里实际存在的回合。
export function simulate(arrivals, turns, opts = {}) {
  const quiet = opts.quietMs ?? QUIET_MS;
  const maxWait = opts.maxMs ?? MAX_MS;
  const list = arrivals.filter((a) => Number.isFinite(a.ms)).sort((a, b) => a.ms - b.ms);
  const busy = turns
    .filter((t) => Number.isFinite(t.start) && Number.isFinite(t.end) && t.end > t.start)
    .sort((a, b) => a.start - b.start);

  let pending = []; // 待带的通知
  let first = null; // 最早一条 batch
  let last = null; // 最后一条 batch
  let queued = []; // 还没送出的消息：{a, due}
  const wakes = [];
  let silentRode = 0;
  let i = 0;

  const noticeDue = () => {
    let due = Infinity;
    for (const p of pending) if (IMMEDIATE.has(p.class)) due = Math.min(due, p.ms);
    if (first != null) due = Math.min(due, Math.min(first + maxWait, last + quiet));
    return due;
  };

  const enqueue = (a) => {
    const at = a.wakeMs ?? a.ms;
    queued.push({ a, due: Math.max(at, occupiedEnd(at, busy)) });
    queued.sort((x, y) => x.due - y.due);
  };

  const pushWake = (at, cls, notices) => {
    const classes = notices.map((n) => n.class);
    wakes.push({
      at,
      class: cls,
      notices: notices.length,
      silent: classes.filter((c) => c === "silent").length,
      classes: [...new Set(classes)].sort((a, b) => (CLASS_RANK[a] ?? 9) - (CLASS_RANK[b] ?? 9)),
    });
    silentRode += classes.filter((c) => c === "silent").length;
  };

  // 一条消息：一次唤醒，待带的通知跟着它走。
  const deliverMessage = (at) => {
    const idx = queued.findIndex((q) => q.due === at);
    const msg = queued.splice(idx, 1)[0].a;
    pushWake(at, msg.class, pending);
    pending = [];
    first = null;
    last = null;
  };

  const deliverNotices = (at) => {
    const classes = pending.map((p) => p.class);
    const cls = maxClass(classes.filter((c) => IMMEDIATE.has(c) || c === "batch"));
    pushWake(at, cls, pending);
    pending = [];
    first = null;
    last = null;
  };

  while (true) {
    const nextArrival = i < list.length ? list[i].ms : Infinity;
    const due = noticeDue();
    const noticeAt = due === Infinity ? Infinity : occupiedEnd(due, busy);
    const messageAt = queued.length ? queued[0].due : Infinity;
    const wakeAt = Math.min(noticeAt, messageAt);

    // 通知的正点 deadline 上到达的算下一批；被回合并延后的唤醒，顺延那一刻到的通知并进来
    //（它和唤醒一起到，真实 hub 也来不及分成两次）。
    const noticeDelayed = Number.isFinite(noticeAt) && noticeAt > due;
    const noticeTie = noticeDelayed ? wakeAt < nextArrival : wakeAt <= nextArrival;
    const messageTie = messageAt <= wakeAt && messageAt <= nextArrival;
    if ((Number.isFinite(noticeAt) && noticeTie) || (Number.isFinite(messageAt) && messageTie)) {
      // 同一时刻先送消息：它会把待带的通知一起带走。
      if (messageAt <= wakeAt) deliverMessage(messageAt);
      else deliverNotices(noticeAt);
      continue;
    }
    if (nextArrival === Infinity) break;

    const a = list[i++];
    if (a.kind === "message") {
      enqueue(a);
    } else {
      pending.push(a);
      if (a.class === "batch") {
        if (first == null) first = a.ms;
        last = a.ms;
      }
    }
  }

  const silentLeft = pending.filter((p) => p.class === "silent").length;
  return { wakes, silentRode, silentLeft, leftover: pending.length, queuedLeft: queued.length };
}

// ---------------------------------------------------------------------------
// 没做决定的回合（旧日志推断）

export function decisionSignals(events, agentId, names) {
  const planner = new Map(); // run_id -> planner
  for (const e of events) {
    if (e.type === "run.created" && e.data.run?.id) planner.set(e.data.run.id, e.data.run.planner);
  }
  const name = names.get(agentId);
  const signals = [];
  const add = (ms, why) => signals.push({ ms, why });
  for (const e of events) {
    const d = e.data ?? {};
    switch (e.type) {
      case "task.status_changed": {
        const to = d.to;
        const from = d.from;
        if (to === "merged") add(e.ms, "accept");
        else if (to === "cancelled") add(e.ms, "cancel");
        else if (from === "passed" && to === "running") add(e.ms, "rework");
        break;
      }
      case "task.handed_off":
        if (d.handoff?.from === agentId) add(e.ms, "task_assign");
        break;
      case "run.plan_submitted":
        if (planner.get(d.id) === agentId) add(e.ms, "plan_submit");
        break;
      case "run.status_changed":
        if (d.to === "awaiting_approval" && planner.get(d.id) === agentId) add(e.ms, "plan_submit");
        break;
      case "run.created":
        if (d.run?.planner === agentId) add(e.ms, "run_created");
        break;
      case "run.landed":
        if (name && d.landing?.by === name) add(e.ms, "land");
        else if (d.landing?.by === agentId) add(e.ms, "land");
        break;
      case "note.written":
        if (d.author === agentId) add(e.ms, "note_write");
        break;
      case "issue.recorded":
        if (d.author === agentId) add(e.ms, "issue_keep");
        break;
      case "human.asked":
        if (d.item?.from === agentId) add(e.ms, "human_ask");
        break;
      case "agent.input":
        if (d.id !== agentId && d.from?.kind === "agent" && d.from.agent_id === agentId) {
          add(e.ms, "agent_send");
        }
        break;
    }
  }
  signals.sort((a, b) => a.ms - b.ms);
  return signals;
}

function whyForRound(round, signals) {
  if (!signals.length) return [];
  const end = round.end ?? round.start;
  const why = new Set();
  // signals 已排序；二分找起点，再线性扫到回合结束。
  let lo = 0;
  let hi = signals.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (signals[mid].ms < round.start) lo = mid + 1;
    else hi = mid;
  }
  for (let k = lo; k < signals.length && signals[k].ms <= end; k++) why.add(signals[k].why);
  return [...why];
}

// ---------------------------------------------------------------------------
// 汇总

function tokensOf(round) {
  return round.tokens ?? null;
}

function sumTokens(rounds) {
  const sum = newTokens();
  let known = 0;
  let unknown = 0;
  for (const r of rounds) {
    const t = tokensOf(r);
    if (!t) {
      unknown++;
      continue;
    }
    known++;
    addTokens(sum, t);
  }
  return { sum, known, unknown };
}

export function summarize(rounds) {
  const by = new Map();
  for (const r of rounds) {
    const key = r.class;
    const row = by.get(key) ?? { class: key, rounds: [], sum: newTokens(), unknown: 0, known: 0 };
    row.rounds.push(r);
    const t = tokensOf(r);
    if (t) {
      row.known++;
      addTokens(row.sum, t);
    } else row.unknown++;
    by.set(key, row);
  }
  const total = sumTokens(rounds);
  return { by, total };
}

// ---------------------------------------------------------------------------
// 输出

function fmt(n) {
  if (n == null || Number.isNaN(n)) return "?";
  if (n >= 1e8) return (n / 1e6).toFixed(1) + "M";
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e3) return (n / 1e3).toFixed(0) + "K";
  return String(n);
}

function pad(s, n) {
  s = String(s);
  // 中文字符按两格算，够用即可。
  let width = 0;
  for (const ch of s) width += ch.codePointAt(0) > 0x2e7f ? 2 : 1;
  return s + " ".repeat(Math.max(0, n - width));
}

function clock(ms) {
  if (!Number.isFinite(ms)) return "-";
  const d = new Date(ms);
  const p = (n) => String(n).padStart(2, "0");
  return `${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())} ${p(d.getUTCHours())}:${p(d.getUTCMinutes())}`;
}

function iso(ms) {
  return Number.isFinite(ms) ? new Date(ms).toISOString() : "-";
}

function firstLine(round) {
  const text = round.digest?.isDigest
    ? round.digest.items.map((i) => i.line).join(" / ")
    : round.text;
  const s = String(text ?? "").replace(/\s+/g, " ").trim();
  return s.length > 64 ? s.slice(0, 61) + "…" : s || "(没有原文)";
}

/// 生成文本报告。
export function formatReport({ agent, files, window, rounds, meta, arrivalInfo }) {
  const lines = [];
  const inferred = !rounds.some((r) => r.roundEvent) && !arrivalInfo?.noticeRecorded;
  const tag = inferred
    ? "推断"
    : arrivalInfo?.noticeRecorded
      ? "直接（notice.recorded）"
      : "直接（agent.round）";
  lines.push("=== 旧日志回放：唤醒估算（估算） ===");
  lines.push(
    `agent: ${agent.name} (${agent.id})${agent.guessed ? " · 未指定 --agent，取回合最多的" : ""}`,
  );
  const shownFrom = Number.isFinite(window.from) ? window.from : (rounds[0]?.start ?? NaN);
  const shownTo = Number.isFinite(window.to)
    ? window.to
    : (rounds.at(-1)?.end ?? rounds.at(-1)?.start ?? NaN);
  lines.push(
    `窗口: ${iso(shownFrom)} – ${iso(shownTo)} · 文件: ${files.join(", ")} · 归类: ${tag}`,
  );
  const knownText = rounds.filter((r) => tokensOf(r)).length;
  lines.push(
    `回合: ${rounds.length}（有用量 ${knownText}，未知 ${rounds.length - knownText}）` +
      ` · 读入 ${meta.lines} 行，坏行 ${meta.bad}` +
      (meta.stats.missingInput ? ` · 没有原文的回合 ${meta.stats.missingInput}` : "") +
      (meta.stats.orphanUsage ? ` · 回合外用掉的上报 ${meta.stats.orphanUsage}` : ""),
  );
  if (arrivalInfo?.noticeRecorded) {
    lines.push("通知时间: notice.recorded（直接）");
  } else {
    lines.push("通知时间: 摘要文本首次出现的时刻（agent.input / input_updated，推断）");
  }
  lines.push("");

  const { by, total } = summarize(rounds);
  lines.push("== 实际的唤醒（按新分类） ==");
  lines.push(
    `${pad("分类", 14)}${pad("唤醒", 6)}${pad("占比", 8)}${pad("输入", 9)}${pad("缓存读", 9)}${pad("未命中", 9)}${pad("输出", 8)}`,
  );
  const order = ALL_CLASSES.filter((c) => by.has(c));
  for (const c of order) {
    const row = by.get(c);
    const share = rounds.length ? ((row.rounds.length / rounds.length) * 100).toFixed(1) + "%" : "-";
    lines.push(
      `${pad(c, 14)}${pad(String(row.rounds.length), 6)}${pad(share, 8)}` +
        `${pad(fmt(row.sum.input), 9)}${pad(fmt(row.sum.cached), 9)}` +
        `${pad(fmt(row.sum.input - row.sum.cached), 9)}${pad(fmt(row.sum.output), 8)}` +
        (row.unknown ? ` (${row.unknown} 未知)` : ""),
    );
  }
  lines.push(
    `${pad("合计", 14)}${pad(String(rounds.length), 6)}${pad("100%", 8)}` +
      `${pad(fmt(total.sum.input), 9)}${pad(fmt(total.sum.cached), 9)}` +
      `${pad(fmt(total.sum.input - total.sum.cached), 9)}${pad(fmt(total.sum.output), 8)}` +
      (total.unknown ? ` (${total.unknown} 未知)` : ""),
  );
  lines.push(
    `（输入含缓存读；未命中 = 输入 − 缓存读；一次唤醒按它带的最高一类通知归类）`,
  );
  lines.push("");

  // 新规则模拟：三种口径
  const sim = arrivalInfo.sims.batch;
  const simUnblock = arrivalInfo.sims.unblock;
  const simReports = arrivalInfo.sims.reports;
  const countBy = (list) => {
    const m = new Map();
    for (const w of list) m.set(w.class, (m.get(w.class) ?? 0) + 1);
    return m;
  };
  const wakeCount = countBy(sim.wakes);
  const pctOf = (n) => (rounds.length ? Math.abs((n / rounds.length) * 100).toFixed(0) : "0");
  const saving = (n) => {
    const d = rounds.length - n;
    if (d === 0) return `${n} 次（与实际持平）`;
    return `${n} 次（${d > 0 ? "少" : "多"} ${Math.abs(d)} 次，${d > 0 ? "-" : "+"}${pctOf(d)}%）`;
  };
  const arrivalClasses = arrivalInfo.arrivals.map((a) => a.class);
  const batchNotices = arrivalClasses.filter((c) => c === "batch").length;
  const silentNotices = arrivalClasses.filter((c) => c === "silent").length;
  const saved = rounds.length - sim.wakes.length;
  const savedParts = [];
  const savedNote = { silent: "（不再单独叫醒）", batch: "（合并）", decide: "（并入已有唤醒）" };
  for (const c of ALL_CLASSES) {
    const a = by.get(c)?.rounds.length ?? 0;
    const e = wakeCount.get(c) ?? 0;
    const d = a - e;
    if (d) savedParts.push(`${c} ${d}${d > 0 ? (savedNote[c] ?? "") : ""}`);
  }

  lines.push("== 新规则下（估算，区间） ==");
  lines.push(`实际 ${rounds.length} 次唤醒 · ${fmt(total.sum.input)} 输入。`);
  lines.push(`  乐观（「等接受」→ batch）: ${saving(sim.wakes.length)}`);
  lines.push(`  保守（「等接受」→ unblock）: ${saving(simUnblock.wakes.length)}`);
  lines.push(`  参考（交付报告仍按现在的消息，立刻叫醒）: ${saving(simReports.wakes.length)}`);
  lines.push(
    `  乐观分解: 人 ${wakeCount.get("human") ?? 0} · 队友 ${wakeCount.get("teammate") ?? 0} · 协调器 ${wakeCount.get("orchestrator") ?? 0}` +
      ` · decide ${wakeCount.get("decide") ?? 0} · unblock ${wakeCount.get("unblock") ?? 0}` +
      ` · run_done ${wakeCount.get("run_done") ?? 0} · batch ${wakeCount.get("batch") ?? 0}` +
      ` · other ${wakeCount.get("other") ?? 0}`,
  );
  lines.push(
    `  batch: ${batchNotices} 条通知 → ${wakeCount.get("batch") ?? 0} 次唤醒` +
      ` · silent: ${silentNotices} 条没有单独叫醒（${sim.silentRode} 条跟着唤醒走）`,
  );
  if (sim.silentLeft) lines.push(`  窗口结束时还剩 ${sim.silentLeft} 条 silent 没送出去`);
  const unknownNotices = arrivalInfo.arrivals.filter((a) => a.unknownWhy).length;
  if (unknownNotices) {
    lines.push(`  认不出来的通知: ${unknownNotices} 条，按 decide 处理（保守）`);
  }
  lines.push(`  节省构成（乐观，按分类差）: ${savedParts.join(" + ")} = ${saved} 次`);
  if (!inferred) lines.push("  （分类来自事件；唤醒次数仍是按新规则模拟的估算）");
  lines.push("");

  // 正式的假设：搬进输出，不只写在注释里
  lines.push("== 假设（改动其中任何一条，数字就会变） ==");
  lines.push(
    "1. 交付报告 / 评审通过算「等接受」：旧日志里它们是 worker 的消息，现在的代码会立刻叫醒" +
      "（crates/hub/src/notices.rs:224 的 watch 行也是 Decide）；本估算按新流程（#review-loop，" +
      "检查和评审都过了、hub 发「等接受」）当 batch。按现状算见上「参考」。",
  );
  lines.push(
    "2. watch 的 passed/merged 行按内容归类（passed → 等接受，merged → silent），不按 watch 一律 decide；" +
      "否则「等接受」永远合并不了。",
  );
  lines.push(
    "3. 依赖图只能部分重建：run.plan_submitted 只有任务 key；agent.tool_detail 的 task_get/task_list 结果里有" +
      " depends_on，但只覆盖少数任务（评审数到 8 个任务、3 个有依赖），重建不全。所以给区间（batch 乐观 / unblock 保守），" +
      "不用部分依赖算中间数。",
  );
  lines.push(
    "4. 新叫醒不占时间：只避开日志里实际存在的回合，新规则产生的回合自身多长没有模拟；" +
      "叫醒消息发出后、回合开始前到达的通知不会并进去。",
  );
  lines.push("5. 消息不合并：每条消息各自一次唤醒（忙时顺延到前一个回合结束），待带的通知跟着消息走。");
  lines.push(
    `6. 通知到达时间按摘要文本首次出现推断；被撤回的摘要 ${meta.stats.withdrawn ?? 0} 条里的行没有计入。`,
  );
  lines.push(
    "7. Batch 边界：最后一条之后正好第 60 秒（或第一条起第 180 秒）到达的通知，先叫醒，它算下一批。",
  );
  lines.push("");

  // 没做决定：事件口径和工具口径两个数都给
  const undecided = rounds.filter((r) => r.decided === false); // 合并口径
  const undecidedEvent = rounds.filter((r) => r.decidedEvent === false);
  const toolKnown = rounds.filter((r) => r.decidedTool !== null && r.decidedTool !== undefined);
  const undecidedTool = toolKnown.filter((r) => r.decidedTool === false);
  const tokensOfSet = (set) => set.filter((r) => tokensOf(r)).reduce((a, r) => a + r.tokens.input, 0);
  const allTokens = rounds.filter((r) => tokensOf(r)).reduce((a, r) => a + r.tokens.input, 0);
  const shareOf = (n) => (allTokens ? ((n / allTokens) * 100).toFixed(0) : "0");
  const rate = (n, d) => (d ? ((n / d) * 100).toFixed(0) : "0");

  lines.push("== 没做决定的回合（区间，推断） ==");
  if (rounds.some((r) => r.roundEvent)) {
    lines.push(
      `直接（agent.round 的 decided）: ${undecided.length} / ${rounds.length} 回合` +
        `（${rate(undecided.length, rounds.length)}%）· 输入 ${fmt(tokensOfSet(undecided))}（${shareOf(tokensOfSet(undecided))}%）`,
    );
  } else {
    lines.push(
      `事件口径（状态变化等效果）: ${undecidedEvent.length} / ${rounds.length} 回合（${rate(undecidedEvent.length, rounds.length)}%）` +
        ` · 输入 ${fmt(tokensOfSet(undecidedEvent))}（${shareOf(tokensOfSet(undecidedEvent))}%）`,
    );
    if (toolKnown.length) {
      lines.push(
        `工具口径（有工具记录的 ${toolKnown.length} 个）: ${undecidedTool.length} / ${toolKnown.length} 回合（${rate(undecidedTool.length, toolKnown.length)}%）` +
          ` · 输入 ${fmt(tokensOfSet(undecidedTool))}（${shareOf(tokensOfSet(undecidedTool))}%）`,
      );
    } else {
      lines.push("工具口径: 没有工具记录（日志里没有 agent.tool_call / tool_detail 里的工具名），只能用事件口径。");
    }
    lines.push(
      `合并（有工具记录按工具、没有按事件）: ${undecided.length} / ${rounds.length} 回合（${rate(undecided.length, rounds.length)}%）` +
        ` · 输入 ${fmt(tokensOfSet(undecided))}（${shareOf(tokensOfSet(undecided))}%）` +
        (rounds.length - toolKnown.length ? ` · ${rounds.length - toolKnown.length} 个没有工具记录` : ""),
    );
    lines.push(
      "（工具记录不全：只有 hub 工具带名字，grep/glob 等驱动工具不算决定；没有工具记录的回合用事件兜底。" +
        "事件里 passed→running 多半是 hub 自动返工，merged/cancelled 没有 actor，时间重叠就算到 lead 头上，所以事件口径偏低。）",
    );
  }
  const list = arrivalInfo.all ? undecided : undecided.slice(0, 30);
  if (list.length) {
    lines.push(`${pad("时间", 14)}${pad("唤醒分类", 12)}${pad("输入", 9)}${pad("未命中", 9)}首行`);
    for (const r of list) {
      const t = tokensOf(r);
      lines.push(
        `${pad(clock(r.start), 14)}${pad(r.class, 12)}${pad(t ? fmt(t.input) : "?", 9)}` +
          `${pad(t ? fmt(t.input - t.cached) : "?", 9)}${firstLine(r)}`,
      );
    }
    if (!arrivalInfo.all && undecided.length > list.length) {
      lines.push(`… 还有 ${undecided.length - list.length} 个，加 --all 全列`);
    }
  }
  lines.push("");
  lines.push(
    "说明: 以上全是「估算」。下界 = 「等接受」全按 batch（乐观，叫醒更少、节省更多），" +
      "上界 = 全按 unblock（保守）；旧日志没有完整依赖图，给不出中间数。",
  );
  return lines.join("\n");
}

export function toJSON({ agent, files, window, rounds, meta, arrivalInfo }) {
  const { by, total } = summarize(rounds);
  const actual = {};
  for (const [c, row] of by) {
    actual[c] = {
      wakes: row.rounds.length,
      input: row.sum.input,
      cached: row.sum.cached,
      output: row.sum.output,
      unknownTokens: row.unknown,
    };
  }
  const countBy = (list) => {
    const m = {};
    for (const w of list) m[w.class] = (m[w.class] ?? 0) + 1;
    return m;
  };
  const undecided = rounds.filter((r) => r.decided === false);
  const undecidedEvent = rounds.filter((r) => r.decidedEvent === false);
  const toolKnown = rounds.filter((r) => r.decidedTool !== null && r.decidedTool !== undefined);
  const undecidedTool = toolKnown.filter((r) => r.decidedTool === false);
  const tokensOfSet = (set) => set.reduce((a, r) => a + (r.tokens?.input ?? 0), 0);
  return {
    agent,
    files,
    window: { from: iso(window.from), to: iso(window.to) },
    rounds: rounds.length,
    actual: { total: { input: total.sum.input, cached: total.sum.cached, output: total.sum.output }, byClass: actual },
    estimated: {
      wakes: arrivalInfo.sims.batch.wakes.length,
      optimistic: arrivalInfo.sims.batch.wakes.length,
      conservative: arrivalInfo.sims.unblock.wakes.length,
      reportsAsMessages: arrivalInfo.sims.reports.wakes.length,
      byTrigger: countBy(arrivalInfo.sims.batch.wakes),
      batchNotices: arrivalInfo.arrivals.filter((a) => a.class === "batch").length,
      silentNotices: arrivalInfo.arrivals.filter((a) => a.class === "silent").length,
      noticeTimes: arrivalInfo.noticeRecorded ? "notice.recorded" : "inferred from digests",
    },
    undecided: {
      count: undecided.length,
      input: tokensOfSet(undecided),
      event: { count: undecidedEvent.length, input: tokensOfSet(undecidedEvent) },
      tool: { count: undecidedTool.length, of: toolKnown.length, input: tokensOfSet(undecidedTool) },
      rounds: undecided.map((r) => ({
        at: iso(r.start),
        class: r.class,
        input: r.tokens?.input ?? null,
        first: firstLine(r),
      })),
    },
    inferred: !rounds.some((r) => r.roundEvent) && !arrivalInfo.noticeRecorded,
  };
}

// ---------------------------------------------------------------------------
// 组装

/// 把回合变成模拟用的到达事件：通知按行、消息按条。窗口外的到达不要。
/// 每条带 kind（message / notice）、origin（report / review / message / notice）、
/// acceptance（等接受）、watch（watch.triggered 出现过的行），供三种口径变换。
export function arrivalsFor(events, rounds, agentId, window = {}) {
  const from = window.from ?? -Infinity;
  const to = window.to ?? Infinity;
  const keep = (ms) => ms >= from && ms <= to;
  const watchLines = watchLineSet(events, agentId);
  const recorded = events.filter((e) => e.type === "notice.recorded" && e.data.id === agentId);
  const useRecorded = recorded.length > 0;
  const arrivals = [];
  const linesByMessage = digestArrivals(events, agentId);

  if (useRecorded) {
    for (const e of recorded) {
      if (!keep(e.ms)) continue;
      const c = e.data.class && CLASS_RANK[e.data.class] != null ? e.data.class : "other";
      const topic = e.data.topic ?? "";
      arrivals.push({
        ms: e.ms,
        class: c,
        kind: "notice",
        line: e.data.line,
        why: "notice.recorded",
        acceptance: c === "batch" && !topic.startsWith("quota:"),
        watch: topic.startsWith("watch:") || topic.startsWith("reminder:"),
      });
    }
    for (const e of events) {
      if (e.type !== "agent.input" || e.data.id !== agentId) continue;
      if (!keep(e.ms)) continue;
      if (e.data.from?.kind === "notice") continue; // notice.recorded 已经算过
      if (parseDigest(e.data.text).isDigest) continue;
      const c = classifyMessage(e.data.text, e.data.from);
      const report = c.why === "report, awaiting acceptance";
      const review = /^Review of #/.test(String(e.data.text ?? "").trim());
      arrivals.push({
        ms: e.ms,
        class: c.class,
        kind: report || review ? "notice" : "message",
        origin: report ? "report" : review ? "review" : "message",
        acceptance: report || (review && c.class === "batch"),
        why: c.why,
      });
    }
    return { arrivals, watchLines, noticeRecorded: true };
  }

  let unknownWhy = 0;
  for (const r of rounds) {
    if (r.roundEvent) {
      for (const n of r.items) {
        if (!keep(r.start)) continue;
        const watch = watchLines.has(n.line.replace(/s+/g, " ").trim());
        arrivals.push({
          ms: r.start,
          class: n.class,
          kind: "notice",
          line: n.line,
          why: "agent.round",
          acceptance: n.class === "batch" && n.why !== "quota tip",
          watch,
        });
      }
      continue;
    }
    if (r.digest?.isDigest) {
      const borne = linesByMessage.get(r.messageId) ?? new Map();
      for (const item of r.items) {
        if (item.why === "unknown") unknownWhy++;
        arrivals.push({
          ms: borne.get(item.line) ?? r.inputMs ?? r.start,
          class: item.class,
          kind: "notice",
          line: item.line,
          why: item.why,
          unknownWhy: item.why === "unknown",
          acceptance: item.class === "batch" && item.why !== "quota tip",
          watch: watchLines.has(item.line.replace(/s+/g, " ").trim()),
        });
      }
      continue;
    }
    const c = classifyMessage(r.text, r.from);
    const report = c.why === "report, awaiting acceptance";
    const review = /^Review of #/.test(String(r.text ?? "").trim());
    arrivals.push({
      // 消息本身的到达是 input 的时刻；唤醒（回合开始）晚 0.1–1 秒，这期间到的通知
      // 会跟着这条消息走，和真实 hub 一样。
      ms: r.inputMs ?? r.start,
      wakeMs: r.start,
      class: c.class,
      kind: report || review ? "notice" : "message",
      origin: report ? "report" : review ? "review" : "message",
      acceptance: report || (review && c.class === "batch"),
      why: c.why,
    });
  }
  return { arrivals: arrivals.filter((a) => keep(a.ms)), watchLines, noticeRecorded: false, unknownWhy };
}

/// 三种口径：
///   batch   乐观——「等接受」全按 batch（默认）
///   unblock 保守——「等接受」全按 unblock（立刻叫醒）
///   reports 参考——交付报告仍按现在的消息（立刻叫醒），其余同乐观
export function scenarioArrivals(arrivals, mode) {
  if (mode === "batch") return arrivals;
  return arrivals.map((a) => {
    if (mode === "unblock" && a.acceptance) return { ...a, class: "unblock" };
    if (mode === "reports" && a.origin === "report") {
      return { ...a, kind: "message", class: "teammate" };
    }
    return a;
  });
}

export async function replay({ files, agentSpec, from, to, all = false }) {
  const { events, lines, bad } = await readEvents(files);
  const agent = resolveAgent(events, agentSpec);
  const window = { from: from == null ? -Infinity : Date.parse(from), to: to == null ? Infinity : Date.parse(to) };
  if (Number.isNaN(window.from) || Number.isNaN(window.to)) {
    throw new Error("--from / --to 要能解析成时间，例如 2026-11-07T00:00:00Z");
  }
  const { rounds, stats } = reconstructRounds(events, agent.id, window);
  classifyRoundItems(rounds, watchLineSet(events, agent.id));

  const names = agentNames(events);
  const signals = decisionSignals(events, agent.id, names);
  for (const r of rounds) {
    if (r.roundEvent) continue; // agent.round 自己说了算
    const why = whyForRound(r, signals);
    r.decidedEvent = why.length > 0;
    r.decidedWhy = why;
    // 有工具记录就以工具为准，没有才用事件推断。
    r.decided = r.decidedTool ?? r.decidedEvent;
  }
  for (const r of rounds) r.decidedEvent ??= r.decided;

  const arrivalInfo = arrivalsFor(events, rounds, agent.id, window);
  const turns = rounds.map((r) => ({ start: r.start, end: r.end ?? r.start }));
  const sims = {
    batch: simulate(scenarioArrivals(arrivalInfo.arrivals, "batch"), turns),
    unblock: simulate(scenarioArrivals(arrivalInfo.arrivals, "unblock"), turns),
    reports: simulate(scenarioArrivals(arrivalInfo.arrivals, "reports"), turns),
  };
  return {
    agent,
    files,
    window,
    rounds,
    meta: { lines, bad, stats },
    arrivalInfo: { ...arrivalInfo, sims, all },
  };
}

function parseArgs(argv) {
  const out = { files: [], all: false, json: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--agent" || a === "-a") out.agent = argv[++i];
    else if (a === "--from") out.from = argv[++i];
    else if (a === "--to") out.to = argv[++i];
    else if (a === "--all") out.all = true;
    else if (a === "--json") out.json = true;
    else if (a === "--help" || a === "-h") out.help = true;
    else if (a.startsWith("--")) throw new Error(`不认识的参数 ${a}`);
    else out.files.push(a);
  }
  return out;
}

const HELP = `用法: node scripts/replay-wakeups.mjs [--agent <名字或 id>] [--from <ISO>] [--to <ISO>] [--json] [--all] <events-*.jsonl ...>

读旧的事件日志，按新的通知分类和叫醒规则估算会少叫醒多少次。见脚本开头的注释。`;

async function main() {
  let args;
  try {
    args = parseArgs(process.argv.slice(2));
  } catch (err) {
    console.error(err.message);
    process.exit(2);
  }
  if (args.help || !args.files.length) {
    console.log(HELP);
    if (!args.files.length && !args.help) process.exit(2);
    return;
  }
  const result = await replay({
    files: args.files,
    agentSpec: args.agent,
    from: args.from,
    to: args.to,
    all: args.all,
  });
  if (args.json) console.log(JSON.stringify(toJSON(result), null, 2));
  else console.log(formatReport(result));
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((err) => {
    console.error(err.message);
    process.exit(1);
  });
}
