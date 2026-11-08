/** Detect the renderer's platform without a native bridge. */
export const isMac = typeof navigator !== "undefined" && /Macintosh|Mac OS X/i.test(navigator.userAgent);
export type ShortcutPlatform = "mac" | "other";
export const shortcutPlatform: ShortcutPlatform = isMac ? "mac" : "other";

/** Format existing key notation; Ctrl denotes Command on Mac unless control is explicit. */
export function shortcut(platform: ShortcutPlatform, keys: string, control = false): string {
  if (platform !== "mac") return keys;
  return keys.split(" / ").map((part) => {
    const pieces = part.split("+");
    const key = pieces.pop()!;
    const modifiers = new Set(pieces);
    return (modifiers.has("Ctrl") && control ? "⌃" : "") +
      (modifiers.has("Alt") ? "⌥" : "") +
      (modifiers.has("Shift") ? "⇧" : "") +
      (modifiers.has("Ctrl") && !control ? "⌘" : "") + key;
  }).join(" / ");
}
