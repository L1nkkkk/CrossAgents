// One judgement for a tool call: did it fail, why, and does it show the change it meant to make.
// Pure, tested in toolcall.test.ts. Both places that draw a call use it: a step in the
// conversation (ToolStep) and a card for the calls that change the team or the code (ActionCard).
//
// A call can fail two ways. The hub says so (`status === "failed"`), or the tool returns its
// refusal as an ordinary result — `with-cli` answers "模型返回的待替换代码块未匹配到整行" instead of
// applying an edit, and MCP tools wrap their reason in `content[0].text`. Either way what the
// detail carries as `changes` is the diff the model *meant* to make, not a change that happened:
// its +N −M would claim work that was never done.

import type { FileChange } from "@bindings/FileChange";
import type { ToolDetail } from "@bindings/ToolDetail";
import type { ToolStatus } from "@bindings/ToolStatus";

/** What drawing a call needs of it: a transcript item's tool call. */
export type Call = { status: ToolStatus; detail?: ToolDetail };

/** A tool result as words: the text inside an MCP `content` wrapper (Codex reports it as is), or the result itself. */
export function plainText(text?: string): string | undefined {
  try {
    const content = JSON.parse(text ?? "")?.content;
    if (Array.isArray(content)) return content.map((c: { text?: unknown }) => (typeof c?.text === "string" ? c.text : "")).join("\n");
  } catch {
    // Not JSON: already plain.
  }
  return text;
}

/** The fields of a tool's JSON arguments or result, or nothing when it is not JSON. */
export function fields(text?: string): Record<string, unknown> {
  if (!text) return {};
  try {
    const value = JSON.parse(text);
    // MCP results wrap the payload as content[0].text.
    const inner = value?.content?.[0]?.text;
    if (typeof inner === "string") return fields(inner) ?? {};
    return value && typeof value === "object" ? value : {};
  } catch {
    return {};
  }
}

/** The call failed: the status says so, or an error was recorded with it (a refusal can come back `completed`). */
export function toolFailed(call: Call): boolean {
  return call.status === "failed" || !!call.detail?.error;
}

/**
 * Why it failed, in the tool's words: `detail.error`, else the result's text — tools that report
 * failures as prose (MCP wrappers, `with-cli`) put nothing in `detail.error`. Undefined when there
 * is nothing to show. The text can be a successful call's result, so read it together with
 * `toolFailed`.
 */
export function toolError(call: Call): string | undefined {
  const error = call.detail?.error;
  if (error) return error;
  // A JSON object result says what happened in its fields; only a plain-text one is a message.
  const result = fields(call.detail?.result);
  return Object.keys(result).length ? undefined : plainText(call.detail?.result);
}

/** The files this call changed. Empty for a failed call: the diff it carries is the one it meant to make. */
export function toolChanges(call: Call): FileChange[] {
  return toolFailed(call) ? [] : (call.detail?.changes ?? []);
}

/** Whether the call shows `+N −M`: a failed call changed nothing, so it shows no counts. */
export function toolShowsStat(call: Call): boolean {
  return !toolFailed(call);
}
