// Which browser permissions the page gets. Kept apart from main.cts so a test can check it
// without Electron.

/**
 * Writing text to the clipboard, which the copy buttons need; Chromium still asks for a user
 * gesture. Everything else is refused, reading the clipboard included.
 */
export function permissionAllowed(permission: string): boolean {
  return permission === "clipboard-sanitized-write";
}
