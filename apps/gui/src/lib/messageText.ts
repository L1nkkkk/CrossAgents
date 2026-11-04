// How a message reads in its bubble (views/SessionMessage.tsx): folded to its first stretch
// when long, then rendered as markdown. The cut is made on the original text, before any
// markdown is parsed, so it can land inside a code block or a link; the parser then sees one
// unclosed construct and keeps it as plain text rather than losing what came before it. Pure,
// so the cutting is tested here (messageText.test.ts) rather than through React.

/** Inputs longer than this start folded. */
export const FOLD_INPUT = 600;

/**
 * The folded view of `text`: its first `limit` characters, trailing whitespace trimmed, then an
 * ellipsis. A cut between the two halves of a surrogate pair (an emoji) is moved back one unit,
 * so the folded text is always valid UTF-16 and the markdown parser never sees half a glyph.
 */
export function foldText(text: string, limit = FOLD_INPUT): string {
  if (text.length <= limit) return text;
  let cut = limit;
  const last = text.charCodeAt(cut - 1);
  if (cut > 1 && last >= 0xd800 && last <= 0xdbff) cut--;
  return text.slice(0, cut).trimEnd() + "…";
}
