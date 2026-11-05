// Matching a transcript tool call to the card an extension registered for it
// (docs/EXTENSIONS.md §5). The same tool reaches the transcript under three names:
//
//   crossagents.extension_call   the built-in that forwards to an extension, with input.tool
//   crossagents.standup__report  the hub's own tool name, dotted
//   mcp__crossagents__standup__report   the same, as an MCP client prefixes it
//
// Cards are registered under the bare `<extension>__<tool>` name; a bare `report` is also
// accepted and qualified with the extension that registered it (host.ts).

import type { TranscriptItem } from "@bindings/TranscriptItem";

export type ToolCallItem = Extract<TranscriptItem, { type: "tool_call" }>;

const MCP_PREFIX = "mcp__crossagents__";
const DOT_PREFIX = "crossagents.";
/** The built-in tool that calls another extension's tool on the agent's behalf. */
const EXTENSION_CALL = "extension_call";

/**
 * `mcp__crossagents__agent_spawn` → `agent_spawn`, `crossagents.standup__report` →
 * `standup__report`. Anything else is returned trimmed, unchanged.
 */
export function normalizeToolName(raw: string): string {
  const name = raw.trim();
  if (name.startsWith(MCP_PREFIX)) return name.slice(MCP_PREFIX.length);
  if (name.startsWith(DOT_PREFIX)) return name.slice(DOT_PREFIX.length);
  return name;
}

/** JSON text from a tool detail, parsed when possible; undefined when absent or not JSON. */
export function parseToolJson(text?: string | null): unknown {
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

/**
 * The extension tool an `extension_call` call really names (`input.tool`), normalized. Undefined
 * for any other call, or one whose arguments do not carry a tool name.
 */
export function extensionCallTarget(item: ToolCallItem): string | undefined {
  if (normalizeToolName(item.title) !== EXTENSION_CALL) return undefined;
  const args = parseToolJson(item.detail?.arguments);
  if (!args || typeof args !== "object" || Array.isArray(args)) return undefined;
  const tool = (args as Record<string, unknown>).tool;
  return typeof tool === "string" && tool.trim() ? normalizeToolName(tool) : undefined;
}

/** The name a card is looked up by: the real extension tool for `extension_call`, else the title. */
export function cardToolName(item: ToolCallItem): string | undefined {
  return extensionCallTarget(item) ?? (normalizeToolName(item.title) || undefined);
}

/**
 * The card for `item`, if any. Exact name only: a card is stored as `<extension>__<tool>` and
 * must not answer for a built-in of the same bare name (`task_list`, `task_accept`).
 */
export function cardFor<V>(item: TranscriptItem, cards: ReadonlyMap<string, V>): V | undefined {
  if (item.type !== "tool_call") return undefined;
  const name = cardToolName(item);
  return name ? cards.get(name) : undefined;
}
