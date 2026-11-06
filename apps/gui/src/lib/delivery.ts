// How a message's delivery reads to a person (docs/tasks/DELIVERY-AUDIT.md §3): one sentence per
// reason code the hub sends, and one for each way a request can fail before the hub answers.
// Exhaustive over `QueuedReason`: a code this build does not know is shown exactly as it came,
// never folded into "被拒绝" (the hub adds variants, it never renames one).
//
// Everything here is pure; the mapping is checked variant by variant in delivery.test.ts.

import type { DeliveryState } from "@bindings/DeliveryState";
import type { QueuedReason } from "@bindings/QueuedReason";
import type { StopReason } from "@bindings/StopReason";

/** A request that never got a hub answer: no response at all, or none we can trust. */
export type DeliveryOutcome = "unreachable" | "outcome_unknown";

/**
 * One sentence per `QueuedReason`. `steer_write_failed` is the one that is neither sent nor
 * queued: the hub says nothing waits for it, so the sentence has to send the person back to
 * "send it again" (docs/tasks/DELIVERY-AUDIT.md §3, §4).
 */
const REASON_TEXT: Record<QueuedReason, string> = {
  turn_running: "本轮结束后发送",
  owner_queue_ahead: "前面还有你的消息在排队",
  external_agent: "它不在 hub 里跑，等它自己来取",
  runtime_no_steer: "这个驱动不能插话，已排到下一轮",
  no_active_turn: "这一轮刚结束，已排到下一轮",
  steer_write_failed: "没插进去，也没有排队；要发送请重发一次",
  archived: "这个对话已归档，恢复后才能发送",
  already_started: "这条已经开始，不能再移动或撤回",
  attachment_refused: "附件没有被接收，换一种或去掉再发",
  empty: "没有内容：写点字或加附件再发",
};

/** What a request that never reached the hub reads as. "Unknown" is not "failed". */
export const OUTCOME_TEXT: Record<DeliveryOutcome, string> = {
  unreachable: "没发出去：hub 没有响应",
  outcome_unknown: "不确定发没发出去，先别重发；重试会用同一条消息，不会重复发送",
};

/** Every reason code this build knows, for the exhaustive test. */
export const QUEUED_REASONS = Object.keys(REASON_TEXT) as QueuedReason[];

export function isReason(code: string | undefined): code is QueuedReason {
  return !!code && Object.prototype.hasOwnProperty.call(REASON_TEXT, code);
}

export function isOutcome(code: string | undefined): code is DeliveryOutcome {
  return code === "unreachable" || code === "outcome_unknown";
}

/**
 * The sentence for a hub reason code; a code from a newer hub is shown as it came, so the
 * person (and whoever they report it to) sees the truth instead of a made-up cause.
 */
export function reasonText(code: string): string {
  return isReason(code) ? REASON_TEXT[code] : code;
}

/** The sentence for a delivery with no hub answer. */
export function outcomeText(code: DeliveryOutcome): string {
  return OUTCOME_TEXT[code];
}

/** Whether the hub's state means a message would wait: only `idle` takes one at once. */
export function deliveryBusy(state: DeliveryState | undefined): boolean {
  return !!state && state !== "idle";
}

/**
 * Whether a finished turn needs the person: it failed, or the hub lost it and nobody knows the
 * outcome. A turn the person cancelled was their own doing and stays quiet — no unread mark, no
 * notice (docs/tasks/DELIVERY-AUDIT.md §5, §8 P2-7). Kept here, pure, so activity.ts (which
 * reads the event stream) only has to ask.
 */
export function turnNeedsAttention(stop: StopReason | null | undefined): boolean {
  return stop === "error" || stop === "interrupted";
}

/**
 * The 接下来 panel's subtitle: what "waiting" means for this session. The old fixed line ("本轮
 * 结束后…") was a lie whenever no turn ran — a starting session, or one that runs outside the
 * hub (docs/tasks/DELIVERY-AUDIT.md §1, §2).
 */
export function queueHeading(state: DeliveryState | undefined, name: string): string {
  switch (state) {
    case "turn":
      return `本轮结束后依次发给 ${name}`;
    case "starting":
      return `等 ${name} 启动好，依次发过去`;
    case "held":
      return `等 ${name} 自己来取`;
    default:
      return `排队中，随后发给 ${name}`;
  }
}
