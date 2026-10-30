// What a composer keeps while you are away: the text you typed and the files you attached.
// Switching to another page unmounts the composer, so without this the half-written message
// and its images are gone. Pure apart from the storage calls, which take the store as an
// argument, so the budget and the shape are tested without a page (draft.test.ts).

import type { Held } from "../attach";

/** Where a draft belongs: the home composer, or one agent's conversation. */
export type Where = { kind: "home" } | { kind: "session"; agent: string };

/** A composer's unsent state. `title` is only the home composer's; a session has none. */
export type Draft = { text: string; title: string; held: Held[] };

/** In storage: `preview` is left out (it repeats `data`) and rebuilt on the way out. */
type StoredFile = { name: string; mime: string; data: string; size: number };
type Stored = { text: string; title: string; files: StoredFile[] };

/**
 * How much JSON one draft may take. Browser storage is a few megabytes for the whole origin
 * and every draft shares it, so one draft full of images must not fill it: over this the text
 * is kept and the files are dropped, which the composer says out loud.
 */
export const MAX_DRAFT_BYTES = 1_500_000;

/** Keys of restored files start high: `useHeld` counts up from 1 for newly added ones. */
const RESTORED_KEY = 1_000_000;

/** A minimal localStorage; every call is guarded, so a stub in a test needs only these. */
export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

export function keyOf(where: Where): string {
  return where.kind === "home" ? "crossagents.draft.home" : `crossagents.draft.session.${where.agent}`;
}

export function isEmpty(draft: Draft): boolean {
  return !draft.text.trim() && !draft.title.trim() && draft.held.length === 0;
}

function pack(draft: Draft): Stored {
  return {
    text: draft.text,
    title: draft.title,
    files: draft.held.map(({ name, mime, data, size }) => ({ name, mime, data, size })),
  };
}

function isFile(v: unknown): v is StoredFile {
  const f = v as StoredFile;
  return !!f && typeof f.name === "string" && typeof f.mime === "string" && typeof f.data === "string";
}

/** A stored draft read back, or `undefined` when it is not one (an older or damaged value). */
export function unpack(raw: unknown): Draft | undefined {
  const s = raw as Stored;
  if (!s || typeof s !== "object" || typeof s.text !== "string") return undefined;
  const files = Array.isArray(s.files) ? s.files.filter(isFile) : [];
  return {
    text: s.text,
    title: typeof s.title === "string" ? s.title : "",
    held: files.map((f, i) => ({
      key: RESTORED_KEY + i,
      name: f.name,
      mime: f.mime,
      data: f.data,
      size: typeof f.size === "number" ? f.size : f.data.length,
      // Rebuilt rather than stored: an image preview is the same bytes again.
      preview: f.mime.startsWith("image/") ? `data:${f.mime};base64,${f.data}` : undefined,
    })),
  };
}

/**
 * The draft as it fits the budget: whole when it does, else the text alone, saying how many
 * files that cost. Dropping all of them rather than some keeps it predictable — you are told
 * the images are not kept, instead of guessing which survived.
 */
export function trim(draft: Draft, budget = MAX_DRAFT_BYTES): { stored: Stored; dropped: number } {
  const whole = pack(draft);
  if (JSON.stringify(whole).length <= budget) return { stored: whole, dropped: 0 };
  return { stored: { text: draft.text, title: draft.title, files: [] }, dropped: draft.held.length };
}

/** The draft kept for `where`, if any. Never throws: no storage, or a damaged value, is none. */
export function read(where: Where, store?: Store): Draft | undefined {
  try {
    const raw = (store ?? localStorage).getItem(keyOf(where));
    if (!raw) return undefined;
    const draft = unpack(JSON.parse(raw));
    return draft && !isEmpty(draft) ? draft : undefined;
  } catch {
    return undefined;
  }
}

/**
 * Keeps `draft` for `where`, or forgets it when there is nothing left to keep. `dropped` is
 * how many files did not fit; `kept` is false when storage refused it altogether (a private
 * window, or the origin is full), so the composer can say so instead of pretending.
 */
export function write(where: Where, draft: Draft, store?: Store, budget = MAX_DRAFT_BYTES): { kept: boolean; dropped: number } {
  if (isEmpty(draft)) {
    forget(where, store);
    return { kept: true, dropped: 0 };
  }
  const { stored, dropped } = trim(draft, budget);
  try {
    (store ?? localStorage).setItem(keyOf(where), JSON.stringify(stored));
    return { kept: true, dropped };
  } catch {
    return { kept: false, dropped: draft.held.length };
  }
}

export function forget(where: Where, store?: Store): void {
  try {
    (store ?? localStorage).removeItem(keyOf(where));
  } catch {
    // Nothing was kept; fine.
  }
}
