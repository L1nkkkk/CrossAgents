// How the composer reads Enter while a turn runs (docs/research/cutex-gui-teardown.md §2.1):
// Ctrl+Enter steers, Ctrl+Shift+Enter interrupts and sends, Enter queues. A driver that cannot
// steer falls back to the queue, and the hint says so instead of hiding the key. Pure, tested in
// steer.test.ts.

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
  /** A turn is running now. */
  running: boolean;
  /** The driver takes a message into the running turn (`capabilities.steer`). */
  steer: boolean;
}): ComposerAction | undefined {
  if (e.key !== "Enter") return undefined;
  if (e.ctrl && e.shift) return e.running ? "interrupt-send" : "send";
  if (e.ctrl) return !e.running ? "send" : e.steer ? "steer" : "queue";
  if (e.shift) return undefined;
  return e.running ? "queue" : "send";
}

/** Whether this driver can be given a message while it works. */
export function canSteer(capabilities: { steer: boolean } | null | undefined): boolean {
  return capabilities?.steer === true;
}

/** The line under the composer: what each key does, in this conversation's state. */
export function composerHint({ running, steer, interrupt }: { running: boolean; steer: boolean; interrupt: boolean }): string {
  if (!running) return "Enter 发送 · Shift+Enter 换行 · agent 离线时会自动启动";
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

/** What the empty box says: while a turn runs, what typing and Enter will do with it. */
export function composerPlaceholder({ name, running, steer }: { name: string; running: boolean; steer: boolean }): string {
  if (!running) return `给 ${name} 发消息，@ 点名协作者或模型，/ 看指令`;
  return steer
    ? `${name} 正在工作，Ctrl+Enter 插话，Enter 排队`
    : `${name} 正在工作，这个驱动不能插话，新消息会排在本轮之后`;
}
