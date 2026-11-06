// Notices that rode along with a person's message (docs/TODO.md §二十五 第 3 条): the hub
// merges a waiting digest into the message the agent reads, and the message says which
// lines came with it so the bubble can show them without putting them in its text.

/** The notice lines to show under a message's bubble, one per line, blank ones dropped. */
export function noticeLines(notices?: string[] | null): string[] {
  return (notices ?? []).map((line) => line.trim()).filter((line) => line.length > 0);
}
