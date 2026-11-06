// How the composer reads Enter while a message would wait (docs/research/cutex-gui-teardown.md
// §2.1, docs/tasks/DELIVERY-AUDIT.md §1): Ctrl+Enter steers, Ctrl+Shift+Enter interrupts and
// sends, Enter queues. "Busy" is the hub's delivery state — a turn running, a message waiting for
// its turn, a session being started, or one that runs outside the hub — not the agent's process
// status read a second way (§8 P1-4). Pure, tested in steer.test.ts.

import type { DeliveryState } from "@bindings/DeliveryState";

/** What a key press means to the composer. */
export type ComposerAction = "send" | "queue" | "steer" | "interrupt-send";

/**
 * What Enter with these modifiers does; `undefined` leaves the key to the textarea (a newline).
 *
 * The combinations are read before the bare Enter: Ctrl+Shift+Enter used to be tested as
 * `Enter && !shift`, so it never matched, while Ctrl+Enter (read as Enter) interrupted — the
 * opposite of what the hint said.
 */
export function composerAction(e: {
  key: string;
  ctrl?: boolean;
  shift?: boolean;
  /** A message sent now would wait: the hub's delivery state is not idle. */
  busy: boolean;
  /** The driver takes a message into the running turn (`capabilities.steer`). */
  steer: boolean;
}): ComposerAction | undefined {
  if (e.key !== "Enter") return undefined;
  if (e.ctrl && e.shift) return e.busy ? "interrupt-send" : "send";
  if (e.ctrl) return !e.busy ? "send" : e.steer ? "steer" : "queue";
  if (e.shift) return undefined;
  return e.busy ? "queue" : "send";
}

/** Whether this driver can be given a message while it works. */
export function canSteer(capabilities: { steer: boolean } | null | undefined): boolean {
  return capabilities?.steer === true;
}

/**
 * The line under the composer: what each key does, in this conversation's delivery state. A
 * session that runs outside the hub (`held`) never starts a turn of its own, so the old
 * "agent 离线时会自动启动" would be a lie there.
 */
export function composerHint({ state, steer, interrupt }: { state: DeliveryState; steer: boolean; interrupt: boolean }): string {
  if (state === "idle") return "Enter 发送 · Shift+Enter 换行 · agent 离线时会自动启动";
  if (state === "starting") return "Enter 排队 · Shift+Enter 换行 · 它的会话正在启动，起来后按顺序发";
  if (state === "held") return "Enter 排队 · Shift+Enter 换行 · 它不在 hub 里跑，会自己来取消息";
  const keys = [
    "Enter 排队",
    // A driver that cannot steer still gets the key named, with the reason: the hint explains
    // the fallback rather than dropping it.
    steer ? "Ctrl+Enter 插话" : "Ctrl+Enter 也排队（这个驱动不能插话）",
    "Ctrl+Shift+Enter 打断并发送",
  ];
  if (interrupt) keys.push("Esc 中断");
  return keys.join(" · ");
}

/** What the empty box says: while messages wait, what typing and Enter will do with it. */
export function composerPlaceholder({ name, state, steer }: { name: string; state: DeliveryState; steer: boolean }): string {
  switch (state) {
    case "idle":
      return `给 ${name} 发消息，@ 点名协作者或模型，/ 看指令`;
    case "starting":
      return `${name} 的会话正在启动，Enter 排队，起来后按顺序发`;
    case "held":
      return `${name} 不在 hub 里运行，消息会等它自己来取`;
    case "turn":
      return steer
        ? `${name} 正在工作，Ctrl+Enter 插话，Enter 排队`
        : `${name} 正在工作，这个驱动不能插话，新消息会排在本轮之后`;
  }
}
