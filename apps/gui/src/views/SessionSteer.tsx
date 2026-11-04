import type { TranscriptItem } from "../session";

/** Transcript items that are not the agent's own output, shown inside its turn. */
export function SteerItem(_: { item: TranscriptItem }) {
  return null;
}
