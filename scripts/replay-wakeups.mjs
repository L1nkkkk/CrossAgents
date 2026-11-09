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
//      - from.kind = agent 且正文带 "\n---\ntask #<key> (<id>) · branch …" 的交付尾
//        注                       → batch，「通过检查和评审、等 lead 接受」。
//      - from.kind = agent 且正文以 "Review of #<key> …: approved" 开头
//                                   → batch，评审通过、等接受。
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
//      - "Task #… is now passed."                   → batch，等 accept。
//      - "Task #… is now merged."                   → silent，已接受，只是进展。
//      - "Progress from …"                          → silent，进度。
//      - "#… 已派给 …"                              → silent，已派出。
//      - "#… (id): given to … instead: …"            → silent，改派。
//      - "Task #… is now <其它状态>."                → silent，状态变化。
//      - quota 提示（"…% of its … left … dispatch more work"）
//                                                   → batch（新代码 quota.rs 用 Batch）。
//      - watch.triggered 里逐字相同、但上面都认不出来的行
//                                                   → decide，主人自己设的 watch。
//      - 认不出来的行                                → decide（保守：不把没认出来的通知省掉），
//        并在输出里单独计数。
//
//   旧日志里 lead 自己 watch 了每个任务，watch 行就是「Task #x is now passed / merged」这类
//   任务状态行，和新规则要发的任务状态通知是同一件事，所以按内容归类（passed → batch，
//   merged → silent），不按 watch 一律算 decide——否则「等接受」永远合并不了，估算会失真。
//   watch 只在内容认不出来时兜底按 decide。
//
// 推断的边界（输出里也会写）：
//   - Batch 还是 Unblock，要看「接受后会不会解锁下游任务」，旧日志没有依赖图（run.plan_submitted
//     只列任务 key），所以旧日志里「等接受」一律按 batch；Unblock 只在使用带分类的新事件时出现。
//     这会让估算偏保守（把本该立刻叫醒的也算进可合并的）。
//   - 旧日志没有 agent.tool_used，所以「没做决定」也是推断：回合时间内出现下列能归到该
//     agent 名下的效果，才算做了决定——接受（status→merged）、返工（passed→running）、
//     取消（→cancelled）、task_assign（task.handed_off 的 from 是它）、提交计划
//     （run.plan_submitted / run.status_changed→awaiting_approval）、开 run
//     （run.created 的 planner 是它）、落地（run.landed 的 by 是它）、写笔记 / 记问题
//     （note.written / issue.recorded 的 author 是它）、问人（human.asked 的 from 是它）、
//     给队友发消息（其它 agent 的 agent.input 的 from.agent_id 是它）。只读工具、进展汇报、
//     检查（checks_run / workspace_run）不算决定；推送、模型切换等没有 actor 的效果不算。
//   - 回合时间按 turn_started→turn_completed；用量增量按 agent.usage 的会话累计值做差，
//     遇到数字变小当作新会话。没有用量上报的回合 token 记为未知。
//   - 新规则模拟不模拟「新回合本身要跑多久」，只避开日志里实际存在的回合：通知到达时
//     agent 正在回合里，就叫醒顺延到那个回合结束（真实 hub 也会等回合结束再发下一轮）。
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
  }
  return { rounds, stats, sawUsage };
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
  merged: /^Task #\S+ is now merged\./,
  settledStatus: /^Task #\S+ is now (running|retrying|verifying|reviewing|waiting_answer|stalled|failed|cancelled)\./,
  reviewApproved: /^Review of #\S+.*: approved/,
  reviewChanges: /^Review of #\S+.*: changes needed/,
  progress: /^Progress from /,
  dispatched: /^#\S+ 已派给 /,
  rerouted: /^#\S+ \([0-9a-f-]{36}\): given to .* instead/,
  quota: /has \d+% of its .* left|resets? in .*: dispatch more work|dispatch more work (there|while)/,
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

/// arrivals: [{ms, class, line?}]，turns: [{start, end}]（实际回合，用来避免在回合中间叫醒）。
export function simulate(arrivals, turns, opts = {}) {
  const quiet = opts.quietMs ?? QUIET_MS;
  const maxWait = opts.maxMs ?? MAX_MS;
  const list = arrivals.filter((a) => Number.isFinite(a.ms)).sort((a, b) => a.ms - b.ms);
  const busy = turns
    .filter((t) => Number.isFinite(t.start) && Number.isFinite(t.end) && t.end > t.start)
    .sort((a, b) => a.start - b.start);

  let pending = [];
  let first = null;
  let last = null;
  const wakes = [];
  let silentRode = 0;
  let silentLeft = 0;
  let i = 0;

  const dueAt = () => {
    let due = Infinity;
    for (const p of pending) {
      if (IMMEDIATE.has(p.class) || MESSAGE_CLASSES.has(p.class)) due = Math.min(due, p.ms);
    }
    if (first != null) due = Math.min(due, Math.min(first + maxWait, last + quiet));
    return due;
  };

  const deliver = (at) => {
    if (!pending.length) return;
    const messages = pending.filter((p) => MESSAGE_CLASSES.has(p.class));
    const classes = pending.map((p) => p.class);
    let cls;
    if (messages.length) {
      // 消息最先到就按消息记（它本来就会叫醒）；否则取通知里最急的一类。
      cls = messages.some((m) => m.class === "human")
        ? "human"
        : messages.some((m) => m.class === "orchestrator")
          ? "orchestrator"
          : "teammate";
    } else {
      cls = maxClass(classes.filter((c) => IMMEDIATE.has(c) || c === "batch"));
    }
    wakes.push({
      at,
      class: cls,
      notices: pending.filter((p) => !MESSAGE_CLASSES.has(p.class)).length,
      silent: classes.filter((c) => c === "silent").length,
      classes: [...new Set(classes)].sort((a, b) => (CLASS_RANK[a] ?? 9) - (CLASS_RANK[b] ?? 9)),
    });
    silentRode += classes.filter((c) => c === "silent").length;
    pending = [];
    first = null;
    last = null;
  };

  while (true) {
    const nextArrival = i < list.length ? list[i].ms : Infinity;
    const due = dueAt();
    if (due === Infinity) {
      if (i >= list.length) break;
    } else {
      const wakeAt = occupiedEnd(due, busy);
      if (wakeAt <= nextArrival || i >= list.length) {
        deliver(wakeAt);
        continue;
      }
    }
    const a = list[i++];
    pending.push(a);
    if (a.class === "batch") {
      if (first == null) first = a.ms;
      last = a.ms;
    }
  }
  silentLeft = pending.filter((p) => p.class === "silent").length;

  return { wakes, silentRode, silentLeft, leftover: pending.length };
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

  // 新规则模拟
  const arrivalClasses = arrivalInfo.arrivals.map((a) => a.class);
  const { wakes, silentRode, silentLeft } = arrivalInfo.sim;
  const countBy = (list) => {
    const m = new Map();
    for (const w of list) m.set(w.class, (m.get(w.class) ?? 0) + 1);
    return m;
  };
  const wakeCount = countBy(wakes);
  const dropped = rounds.length - wakes.length;
  const pct = rounds.length ? ((dropped / rounds.length) * 100).toFixed(0) : "0";
  lines.push("== 新规则下（估算） ==");
  lines.push(
    `唤醒: ${wakes.length} 次（实际 ${rounds.length} 次，少 ${dropped} 次，${dropped >= 0 ? "-" : "+"}${Math.abs(Number(pct))}%）`,
  );
  lines.push(
    `  消息（照旧立刻）: 人 ${wakeCount.get("human") ?? 0} · 队友 ${wakeCount.get("teammate") ?? 0} · 协调器 ${wakeCount.get("orchestrator") ?? 0}`,
  );
  lines.push(
    `  立刻叫醒的通知: decide ${wakeCount.get("decide") ?? 0} · unblock ${wakeCount.get("unblock") ?? 0} · run_done ${wakeCount.get("run_done") ?? 0} · other ${wakeCount.get("other") ?? 0}`,
  );
  const batchNotices = arrivalClasses.filter((c) => c === "batch").length;
  const silentNotices = arrivalClasses.filter((c) => c === "silent").length;
  lines.push(
    `  batch: ${batchNotices} 条通知，由 batch 触发的唤醒 ${wakeCount.get("batch") ?? 0} 次` +
      (silentNotices ? ` · silent: ${silentNotices} 条没有单独叫醒（${silentRode} 条跟着唤醒走）` : ""),
  );
  const unknownNotices = arrivalInfo.arrivals.filter((a) => a.unknownWhy).length;
  if (unknownNotices) {
    lines.push(`  认不出来的通知: ${unknownNotices} 条，按 decide 处理（保守）`);
  }
  if (silentLeft) lines.push(`  窗口结束时还剩 ${silentLeft} 条 silent 没送出去`);
  if (!inferred) lines.push("  （分类来自事件；唤醒次数仍是按新规则模拟的估算）");
  lines.push("");

  // 没做决定
  const undecided = rounds.filter((r) => r.decided === false);
  const undecidedKnown = undecided.filter((r) => tokensOf(r));
  const undecidedTokens = undecidedKnown.reduce((a, r) => a + r.tokens.input, 0);
  const undecidedUnknown = undecided.length - undecidedKnown.length;
  const allTokens = rounds.filter((r) => tokensOf(r)).reduce((a, r) => a + r.tokens.input, 0);
  const share = allTokens ? ((undecidedTokens / allTokens) * 100).toFixed(0) : "0";
  lines.push("== 没做决定的回合（推断） ==");
  lines.push(
    `合计: ${undecided.length} / ${rounds.length} 回合（${rounds.length ? ((undecided.length / rounds.length) * 100).toFixed(0) : 0}%）` +
      ` · 输入 ${fmt(undecidedTokens)}（占 ${share}%）` +
      (undecidedUnknown ? ` · 其中 ${undecidedUnknown} 个没有用量` : ""),
  );
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
    inferred
      ? "说明: 以上全是「估算」；旧日志没有 agent.round / 通知分类事件，分类和「没做决定」都是从" +
          "通知原文和消息来源推断的。Batch/Unblock 的区分需要依赖图（旧日志没有），" +
          "旧日志里「等接受」一律按 batch，可能把该立刻叫醒的也算进可合并的。"
      : "说明: 以上全是「估算」；分类和「没做决定」来自事件，唤醒次数是按新规则模拟的。" +
          (arrivalInfo?.noticeRecorded ? "" : "Unblock 需要依赖图，这里只按事件给的分类。"),
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
  const wakeCount = {};
  for (const w of arrivalInfo.sim.wakes) wakeCount[w.class] = (wakeCount[w.class] ?? 0) + 1;
  const undecided = rounds.filter((r) => r.decided === false);
  return {
    agent,
    files,
    window: { from: iso(window.from), to: iso(window.to) },
    rounds: rounds.length,
    actual: { total: { input: total.sum.input, cached: total.sum.cached, output: total.sum.output }, byClass: actual },
    estimated: {
      wakes: arrivalInfo.sim.wakes.length,
      byTrigger: wakeCount,
      batchNotices: arrivalInfo.arrivals.filter((a) => a.class === "batch").length,
      silentNotices: arrivalInfo.arrivals.filter((a) => a.class === "silent").length,
      noticeTimes: arrivalInfo.noticeRecorded ? "notice.recorded" : "inferred from digests",
    },
    undecided: {
      count: undecided.length,
      input: undecided.reduce((a, r) => a + (r.tokens?.input ?? 0), 0),
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
      arrivals.push({ ms: e.ms, class: c, line: e.data.line, why: "notice.recorded" });
    }
    for (const e of events) {
      if (e.type !== "agent.input" || e.data.id !== agentId) continue;
      if (!keep(e.ms)) continue;
      if (e.data.from?.kind === "notice") continue; // notice.recorded 已经算过
      if (parseDigest(e.data.text).isDigest) continue;
      const c = classifyMessage(e.data.text, e.data.from);
      arrivals.push({ ms: e.ms, class: c.class, why: c.why });
    }
    return { arrivals, watchLines, noticeRecorded: true };
  }

  let unknownWhy = 0;
  for (const r of rounds) {
    if (r.roundEvent) {
      for (const n of r.items) {
        if (keep(r.start)) arrivals.push({ ms: r.start, class: n.class, line: n.line, why: "agent.round" });
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
          line: item.line,
          why: item.why,
          unknownWhy: item.why === "unknown",
        });
      }
      continue;
    }
    const c = classifyMessage(r.text, r.from);
    arrivals.push({ ms: r.inputMs ?? r.start, class: c.class, why: c.why });
  }
  return { arrivals: arrivals.filter((a) => keep(a.ms)), watchLines, noticeRecorded: false, unknownWhy };
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
    r.decided = why.length > 0;
    r.decidedWhy = why;
  }

  const arrivalInfo = arrivalsFor(events, rounds, agent.id, window);
  const sim = simulate(
    arrivalInfo.arrivals,
    rounds.map((r) => ({ start: r.start, end: r.end ?? r.start })),
  );
  return {
    agent,
    files,
    window,
    rounds,
    meta: { lines, bad, stats },
    arrivalInfo: { ...arrivalInfo, sim, all },
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
