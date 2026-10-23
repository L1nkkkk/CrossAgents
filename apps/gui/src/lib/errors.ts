// Agent errors as a person reads them: the hub's messages are for logs, these say what to do.

/** A short Chinese hint for a known error, else its first line. */
export function errorHint(message: string): string {
  const text = message.replace(/^agent error:\s*/i, "").trim();
  const missing = /cannot start (.+?): (?:program not found|.*cannot find the file|.*No such file)/i.exec(text);
  if (missing) {
    const program = missing[1].split(/[\\/]/).pop() ?? missing[1];
    return `找不到 ${program} 命令：这台电脑没装它的 CLI`;
  }
  if (/CLI is not installed/i.test(text)) return "这台电脑没装它的 CLI";
  if (/not logged in|login required|please (run )?\/?login|unauthorized|401/i.test(text)) return "账号没登录或登录已失效";
  const first = text.split("\n")[0];
  return first.length > 60 ? first.slice(0, 60) + "…" : first;
}
