// Whether an agent error is the gateway's content moderation refusing the whole context.
//
// Nothing here was confirmed against a real error: `with-cli` is not installed on this machine
// (`~/background_agent_cli/bin`, PATH, the npm global tree and the With/WorkBuddy app folders
// hold no copy), and the hub's events and logs keep no `agent.error` text from that session —
// `agent.turn_completed` records only `stop_reason: "error"`. Every rule below is therefore a
// documented code or phrase from an API such a gateway fronts, and each is kept narrow on
// purpose: an ordinary error that merely says "moderation" or "content filter" must not be
// read as this. When a real message turns up, add it and mark it confirmed.

export type ModerationRule = {
  id: string;
  /** What the rule reads for, in a sentence. */
  what: string;
  pattern: RegExp;
  /** Where the wording comes from. */
  source: string;
  /** True only when a real gateway error was seen here; all of these are unconfirmed. */
  confirmed: boolean;
};

/**
 * The rule table, in the order it is tried. Sources are vendor API documentation, not this
 * gateway: none of them has been seen in a real CrossAgents error yet.
 */
export const MODERATION_RULES: readonly ModerationRule[] = [
  {
    id: "content-filter-code",
    what: "the error code `content_filter` (with `-` or `_`)",
    pattern: /\bcontent[_-]filter\b/i,
    source: "OpenAI / Azure OpenAI error code `content_filter`",
    confirmed: false,
  },
  {
    id: "content-policy-code",
    what: "the error code `content_policy_violation`",
    pattern: /\bcontent[_-]policy[_-]violation\b/i,
    source: "OpenAI error code `content_policy_violation`",
    confirmed: false,
  },
  {
    id: "azure-content-management",
    what: "Azure OpenAI's “content management policy” message",
    pattern: /\bcontent management policy\b/i,
    source: "Azure OpenAI 400: “prompt triggering Azure OpenAI's content management policy”",
    confirmed: false,
  },
  {
    id: "openai-safety-system",
    what: "OpenAI's “rejected as a result of our safety system”",
    pattern: /rejected as a result of our safety system/i,
    source: "OpenAI 400: “The request was rejected as a result of our safety system.”",
    confirmed: false,
  },
  {
    id: "dashscope-inspection",
    what: "DashScope's `data_inspection_failed` code",
    pattern: /\bdata[_-]inspection[_-]failed\b/i,
    source: "Alibaba DashScope error code `data_inspection_failed`",
    confirmed: false,
  },
  {
    id: "anthropic-output-filter",
    what: "Anthropic's “Output blocked by content filtering policy”",
    pattern: /output blocked by content filtering policy/i,
    source: "Anthropic API refusal message “Output blocked by content filtering policy”",
    confirmed: false,
  },
  {
    id: "inappropriate-content",
    what: "“inappropriate content”, only with a decision or an input subject around it",
    pattern: /\binappropriate content\b[^.\n]{0,24}\b(?:blocked|rejected|detected|flagged)\b|\b(?:input|prompt|request|message)\b[^.\n]{0,24}\binappropriate content\b/i,
    source: "DashScope 400: “Input data may contain inappropriate content.”",
    confirmed: false,
  },
  {
    id: "moderation-action",
    what: "the generic word `moderation`, only beside a decision (blocked / rejected / flagged / denied / failed)",
    pattern: /\bmoderation\b[^.\n]{0,24}\b(?:blocked|rejected|flagged|denied|failed)\b|\b(?:blocked|rejected|flagged|denied)\b[^.\n]{0,24}\bmoderation\b/i,
    source: "generic gateway wording; not confirmed, and deliberately not matched on the word alone",
    confirmed: false,
  },
  {
    id: "content-filter-phrase",
    what: "the phrase “content filter”, only beside a decision",
    pattern: /\bcontent filter\b[^.\n]{0,24}\b(?:blocked|rejected|flagged|denied)\b/i,
    source: "generic gateway wording; not confirmed, and deliberately not matched on the phrase alone",
    confirmed: false,
  },
  {
    id: "zh-content-review",
    what: "内容/文本/输入 + 审查/审核/安全 with a refusal, or 内容/文本/输入 + 不合规/违规",
    pattern:
      /(?:内容|文本|输入)(?:审查|审核|安全)[^。\n]{0,20}(?:未通过|不通过|拒绝|拦截|不合规|违规)|(?:拒绝|拦截|未通过)[^。\n]{0,20}(?:内容|文本|输入)(?:审查|审核|安全)|(?:内容|文本|输入)[^。\n]{0,12}(?:不合规|违规)/,
    source: "generic Chinese gateway wording; not confirmed",
    confirmed: false,
  },
  {
    id: "zh-sensitive",
    what: "敏感词/敏感内容/不当内容, only with a hit or a refusal",
    pattern: /(?:敏感词|敏感内容|不当内容)[^。\n]{0,20}(?:命中|拦截|拒绝|过滤|不合规|违规)/,
    source: "generic Chinese gateway wording; not confirmed",
    confirmed: false,
  },
];

/**
 * Whether `message` is the gateway's content-moderation refusal. `null`, empty and unrelated
 * errors are false: the caller then treats it as an ordinary error.
 */
export function isModerationError(message: string | null | undefined): boolean {
  if (!message) return false;
  return MODERATION_RULES.some((rule) => rule.pattern.test(message));
}
