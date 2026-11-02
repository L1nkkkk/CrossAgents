// Which model family a session runs, for its mark in lists. The model decides, not the plugin:
// a Claude Code or CodeBuddy session can run DeepSeek, GLM or Step behind it. This is the one
// family table: the marks (modelIcons.ts), the settings groups (lib/models.ts) and ModelMark
// all read it, so a model looks the same everywhere.

export type ModelFamily = { key: string; name: string };

/**
 * Gateways put their own prefix on a model (`tokenhub_deepseek-v4`, `ext-glm-5.2`); the family
 * is the model's, not the gateway's.
 */
const GATEWAY_PREFIX = /^(tokenhub|ext|azure|bedrock|vertex|openrouter)[_-]/;

/** "tokenhub_ext-glm-5.2" → "glm-5.2": the model's own name, without gateway prefixes. */
export function bareModel(model: string): string {
  let bare = model.trim().toLowerCase();
  while (GATEWAY_PREFIX.test(bare)) bare = bare.replace(GATEWAY_PREFIX, "");
  return bare;
}

/** Families in the order lists show them; `key` is also the family's mark in modelIcons.ts. */
const FAMILIES: readonly (ModelFamily & { match: RegExp })[] = [
  { key: "claude", name: "Claude", match: /claude|anthropic|opus|sonnet|haiku|fable/ },
  { key: "openai", name: "OpenAI", match: /gpt|codex|^chatgpt|^o\d/ },
  { key: "gemini", name: "Gemini", match: /gemini/ },
  { key: "deepseek", name: "DeepSeek", match: /deepseek/ },
  { key: "glm", name: "GLM", match: /glm|zhipu/ },
  { key: "kimi", name: "Kimi", match: /kimi|moonshot/ },
  { key: "qwen", name: "Qwen", match: /qwen|qianwen|qwq/ },
  { key: "hunyuan", name: "混元", match: /hunyuan|^hy\d/ },
  { key: "minimax", name: "MiniMax", match: /minimax|abab/ },
  { key: "step", name: "Step", match: /^step|stepfun/ },
  { key: "mimo", name: "MiMo", match: /mimo|xiaomi/ },
  { key: "grok", name: "Grok", match: /grok|^xai/ },
  { key: "doubao", name: "豆包", match: /doubao|bytedance|^seed/ },
  { key: "wenxin", name: "文心", match: /wenxin|ernie/ },
  { key: "mistral", name: "Mistral", match: /mistral|mixtral|codestral|magistral|devstral/ },
  { key: "yi", name: "Yi", match: /^yi([-_.]|$)|01-ai/ },
  { key: "baichuan", name: "Baichuan", match: /baichuan/ },
  { key: "internlm", name: "InternLM", match: /internlm|intern-lm/ },
  { key: "spark", name: "星火", match: /spark|iflytek/ },
  { key: "sensenova", name: "SenseNova", match: /sensenova|sense-?chat/ },
  { key: "skywork", name: "Skywork", match: /skywork/ },
  { key: "longcat", name: "LongCat", match: /longcat/ },
];

/** Every family's key, in the order lists show them (the marks are keyed by these). */
export function familyKeys(): string[] {
  return FAMILIES.map((family) => family.key);
}

/** Families known by the plugin alone, for a session whose model is not recorded yet. */
const PLUGIN_FAMILIES: readonly { prefix: string; key: string }[] = [
  { prefix: "claude", key: "claude" },
  { prefix: "codex", key: "openai" },
];

/** The family of `model`, or of the plugin's own models when none is recorded; null when neither says. */
export function modelFamily(plugin: string, model: string | null | undefined): ModelFamily | null {
  const id = model ? bareModel(model) : "";
  if (id) return FAMILIES.find((family) => family.match.test(id)) ?? null;
  const p = plugin.trim().toLowerCase();
  const own = PLUGIN_FAMILIES.find((entry) => p.startsWith(entry.prefix));
  return (own && FAMILIES.find((family) => family.key === own.key)) ?? null;
}
