// Small parts of the terminal tabs (src/terminal.tsx). Pure; tested in terminals.test.ts.

/** What one input request carries at most: at three bytes a character, under the hub's 64 KiB. */
export const MAX_BATCH_CHARS = 20_000;

/**
 * The next batch to send, taken from the front of `queue`: pieces typed since the last one went,
 * together. A paste over the limit goes in parts, without splitting a surrogate pair.
 */
export function takeBatch(queue: string[], max = MAX_BATCH_CHARS): string {
  let out = "";
  while (queue.length) {
    const room = max - out.length;
    const next = queue[0];
    if (next.length <= room) {
      out += next;
      queue.shift();
      continue;
    }
    if (out) break;
    const low = next.charCodeAt(room - 1);
    const cut = low >= 0xd800 && low <= 0xdbff ? room - 1 : room;
    queue[0] = next.slice(cut);
    return next.slice(0, cut);
  }
  return out;
}

export function fromBase64(data: string): Uint8Array {
  return Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
}

/** 终端 1, 终端 2… in each project, oldest first: `terms` comes in that order. */
export function termLabels(terms: { id: string; project: string }[]): Map<string, string> {
  const counts = new Map<string, number>();
  const labels = new Map<string, string>();
  for (const term of terms) {
    const n = (counts.get(term.project) ?? 0) + 1;
    counts.set(term.project, n);
    labels.set(term.id, n === 1 ? "终端" : `终端 ${n}`);
  }
  return labels;
}
