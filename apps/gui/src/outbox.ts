// Messages sent from this window, shown at once. The hub answers a message only once its
// agent is up, which takes seconds for one that was stopped; until the hub's own copy
// arrives in the transcript, the conversation shows the message from here.
//
// One message is one row (docs/tasks/DELIVERY-AUDIT.md §2, §8 P0-1): the bubble stays in the
// conversation from submit to the end, and its state changes in place — 发送中 → 排队中 · 原因 →
// 已插入 → 进行中 → 结束, or 失败（可重试）/ 已撤回. Only the hub's copy taking over moves it,
// and that copy is the same message id.
//
// A retry reuses `clientId`, the id fixed when this message was made: the hub answers a repeat
// with the first delivery instead of starting another turn (§4, §8 P0-3).

import { useEffect, useSyncExternalStore } from "react";
import type { AttachmentUpload } from "@bindings/AttachmentUpload";
import type { QueuedReason } from "@bindings/QueuedReason";
import type { TranscriptTurn } from "@bindings/TranscriptTurn";
import { api } from "./api";
import { deliveryFailure, deliveryHint } from "./lib/errors";

export type Outgoing = {
  key: number;
  agent: string;
  text: string;
  now?: boolean;
  /** Handed to the running turn instead of queued (`steer`). */
  steer?: boolean;
  /** Files sent with it. */
  attachments?: AttachmentUpload[];
  /** This message's own id, fixed at submit and reused by every retry. */
  clientId: string;
  /** The hub's id for it, once it took it. */
  id?: string;
  /** The hub queued it, and why (absent while it is still on its way). */
  queued?: QueuedReason;
  /** It went into the running turn. */
  steered?: boolean;
  /** Why it was not sent, in the person's words. */
  error?: string;
  /** The same failure in the hub's or the transport's own words (the `title`). */
  errorRaw?: string;
  /** The request may or may not have reached the hub: do not resend (a retry reuses the id). */
  outcomeUnknown?: boolean;
};

let items: Outgoing[] = [];
let next = 0;
const listeners = new Set<() => void>();

function set(change: (xs: Outgoing[]) => Outgoing[]) {
  items = change(items);
  listeners.forEach((l) => l());
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** A new message's own id (the hub's `MessageId` is a UUID). */
function clientId(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now().toString(16)}-${(++next).toString(16)}-${Math.random().toString(16).slice(2)}`;
}

async function deliver(item: Outgoing): Promise<void> {
  try {
    const attachments = item.attachments?.length ? item.attachments : undefined;
    // `steered: false` in the answer is not a failure: `queued_reason` says why it waits instead
    // (no turn runs, the driver cannot steer, …). It shows on the same row.
    const sent = await api.sendMessage(item.agent, {
      text: item.text,
      now: item.now || undefined,
      steer: item.steer || undefined,
      attachments,
      client_message_id: item.clientId,
    });
    set((xs) =>
      xs.map((x) =>
        x.key === item.key
          ? { ...x, id: sent.message_id, queued: sent.queued_reason, steered: sent.steered || undefined, error: undefined, errorRaw: undefined, outcomeUnknown: undefined }
          : x,
      ),
    );
  } catch (err) {
    const failure = deliveryFailure(err);
    // The message's own text and files are never logged (docs/tasks/DELIVERY-AUDIT.md §2-10).
    console.error("message delivery failed", { status: failure.status, code: failure.code, message: failure.message });
    set((xs) =>
      xs.map((x) =>
        x.key === item.key
          ? { ...x, error: deliveryHint(failure), errorRaw: failure.message, outcomeUnknown: failure.code === "outcome_unknown" }
          : x,
      ),
    );
    throw err;
  }
}

/**
 * Sends `text` to `agent`, showing it in that conversation right away. A failure stays there
 * with a retry, unless `takeBack` (the caller puts the text back where it was typed). `steer`
 * hands it to the running turn, which the hub may still queue.
 */
export async function sendMessage(agent: string, text: string, opts: { now?: boolean; steer?: boolean; takeBack?: boolean; attachments?: AttachmentUpload[] } = {}): Promise<void> {
  const item: Outgoing = { key: ++next, agent, text, now: opts.now, steer: opts.steer, attachments: opts.attachments, clientId: clientId() };
  set((xs) => [...xs, item]);
  try {
    await deliver(item);
  } catch (err) {
    if (opts.takeBack) dismiss(item.key);
    throw err;
  }
}

/** Sends the same message again, under the id it was made with: the hub cannot deliver it twice. */
export function retry(key: number) {
  const item = items.find((x) => x.key === key);
  if (!item) return;
  set((xs) => xs.map((x) => (x.key === key ? { ...x, error: undefined, errorRaw: undefined, outcomeUnknown: undefined } : x)));
  void deliver(item).catch(() => {});
}

export function dismiss(key: number) {
  set((xs) => xs.filter((x) => x.key !== key));
}

/** What `agent`'s conversation shows from here: messages its transcript does not have yet. */
export function useOutbox(agent: string | undefined, turns: TranscriptTurn[] | undefined): Outgoing[] {
  const all = useSyncExternalStore(subscribe, () => items);
  // A steered message is no turn of its own: it arrives as a `steer` item inside the running one.
  const arrived = (x: Outgoing) =>
    !!x.id && !!turns?.some((t) => t.message_id === x.id || t.items.some((i) => i.type === "steer" && i.message_id === x.id));
  // Once the hub's copy is in, ours goes for good: the transcript's turn is the row from now on,
  // queued or running, and it says what happened to it (including 已撤回).
  useEffect(() => {
    if (all.some((x) => x.agent === agent && arrived(x))) set((xs) => xs.filter((x) => !(x.agent === agent && arrived(x))));
  });
  return all.filter((x) => x.agent === agent && !arrived(x));
}
