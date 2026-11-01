// Which model family a session runs, for its mark in lists. The model decides, not the plugin:
// a Claude Code or CodeBuddy session can run DeepSeek, GLM or Step behind it.

export type ModelFamily = { key: string; name: string };

const FAMILIES: readonly (ModelFamily & { match: RegExp })[] = [
  { key: "claude", name: "Claude", match: /claude|opus|sonnet|haiku|fable/ },
  { key: "openai", name: "OpenAI", match: /gpt|codex|^o\d/ },
  { key: "deepseek", name: "DeepSeek", match: /deepseek/ },
  { key: "glm", name: "GLM", match: /glm|zhipu/ },
  { key: "step", name: "Step", match: /^step|stepfun/ },
  { key: "gemini", name: "Gemini", match: /gemini/ },
  { key: "qwen", name: "Qwen", match: /qwen|qianwen/ },
  { key: "kimi", name: "Kimi", match: /kimi|moonshot/ },
  { key: "minimax", name: "MiniMax", match: /minimax/ },
  { key: "hunyuan", name: "Hunyuan", match: /hunyuan/ },
];

/** The family of `model`, or of the plugin's own models when none is recorded; null when neither says. */
export function modelFamily(plugin: string, model: string | null | undefined): ModelFamily | null {
  const id = model?.trim().toLowerCase();
  const found = id && FAMILIES.find((family) => family.match.test(id));
  if (found) return found;
  if (id) return null;
  const p = plugin.toLowerCase();
  return FAMILIES.find((family) => (family.key === "claude" && p.startsWith("claude")) || (family.key === "openai" && p.startsWith("codex"))) ?? null;
}
