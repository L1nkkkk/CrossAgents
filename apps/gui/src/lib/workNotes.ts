// What became of each Work a conversation proposed, for a line in its timeline. The chat's
// model hears it with its next message, not as a turn of its own (docs/USAGE-TRIM.md §3.4).

import type { HumanItem } from "@bindings/HumanItem";

export type WorkDecision = {
  /** The proposal card's id. */
  id: string;
  /** When the person decided; a card closed before decisions were timed falls back to when it went up. */
  at: string;
  title: string;
  /** The run it started; `null` when the person declined or closed the card. */
  run_id: string | null;
};

/** The decided Work proposals of `agent`, oldest first. Open cards are not decided yet. */
export function workDecisions(items: HumanItem[], agent: string): WorkDecision[] {
  return items
    .filter((i) => i.action === "work" && i.from === agent && (i.status === "answered" || i.status === "dismissed"))
    .map((i) => ({ id: i.id, at: i.answered_at ?? i.created_at, title: i.text, run_id: i.status === "answered" ? (i.run_id ?? null) : null }))
    .sort((a, b) => a.at.localeCompare(b.at));
}
