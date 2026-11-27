// Scoped agent names (`CA-Nyx-12`) and semantic short-title helpers shared across the GUI.

export type ScopedName = {
  code: string;
  word: string;
  seq: string;
};

/**
 * Parses a generated scoped name like `CA-Nyx-12` or `笔记-Altair-3` into its parts.
 * Returns `null` for custom names (`Ada`), legacy names (`Nova 4`) or internal worker clones (`w-11cce4-...`).
 */
export function parseScopedName(name: string): ScopedName | null {
  const m = /^([A-Za-z\u4e00-\u9fff][A-Za-z0-9\u4e00-\u9fff]{0,5})-([A-Za-z]{2,18})-(\d+)$/.exec(name.trim());
  if (!m) return null;
  // Exclude internal single-letter worker/reviewer clone ids like `w-a-1` or `r-b-2`.
  if (m[1].length === 1 && (m[1] === "w" || m[1] === "r")) return null;
  return { code: m[1], word: m[2], seq: m[3] };
}

/** Compact label: `Nyx #12` inside a project list, or full `CA-Nyx-12` across projects. */
export function shortScopedLabel(name: string, inProjectGroup: boolean): string {
  const parsed = parseScopedName(name);
  if (!parsed) return name;
  return inProjectGroup ? `${parsed.word} #${parsed.seq}` : name;
}

const FILLER_PREFIXES = [
  "你看看你能不能",
  "你看看能不能",
  "你帮我看看",
  "帮我看看",
  "帮我查一下",
  "帮我一下",
  "麻烦帮我",
  "麻烦你",
  "请问一下",
  "请问",
  "请帮我",
  "请你",
  "你看看",
  "能不能",
  "可以帮我",
  "帮我",
  "我现在想要一个",
  "我现在想要",
  "我想要一个",
  "我想要",
  "我想",
  "你好啊",
  "你好呀",
  "你好",
  "您好",
  "哈喽",
  "在吗",
  "在不在",
  "好的",
  "好吧",
  "okay",
  "ok",
  "hello",
  "hey there",
  "hey",
  "hi there",
  "hi",
  "please help me",
  "can you help me",
  "could you help me",
  "could you please",
  "can you please",
  "would you please",
  "help me",
  "please",
  "can you",
  "could you",
];

const BARE_GREETINGS = new Set([
  "你好",
  "你好啊",
  "你好呀",
  "您好",
  "哈喽",
  "嗨",
  "在吗",
  "在不在",
  "测试",
  "测一下",
  "ok",
  "okay",
  "hi",
  "hello",
  "hey",
  "test",
  "ping",
  "yo",
  "好",
  "好的",
  "行",
  "嗯",
]);

function trimEdgePunct(s: string): string {
  return s.replace(/^[\s,，.。!！?？:：;；、\-—>#*]+|[\s,，.。!！?？:：;；、]+$/g, "");
}

function trimLeadPunct(s: string): string {
  return s.replace(/^[\s,，.。!！?？:：;；、\-—>#*]+/, "");
}

export function isBareGreeting(text: string): boolean {
  const cleaned = trimEdgePunct(text).toLowerCase();
  return !cleaned || BARE_GREETINGS.has(cleaned);
}

/**
 * Distills a concise semantic short title from a prompt, stripping markdown noise and polite
 * filler prefixes. Returns `undefined` for pure greetings (`"你好"`, `"hello"`, `"ok"`).
 */
export function semanticTopic(text: string, max = 36): string | undefined {
  const firstLine = text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .find((l) => l.length > 0 && !l.startsWith("```"));
  if (!firstLine) return undefined;
  let s = firstLine.replace(/^[#>\-*\s]+/, "").trim();
  if (isBareGreeting(s)) return undefined;

  for (let pass = 0; pass < 3; pass++) {
    const lower = s.toLowerCase();
    let matched = false;
    for (const prefix of FILLER_PREFIXES) {
      if (lower.startsWith(prefix)) {
        const rest = s.slice(prefix.length);
        if (/^[a-z\s]+$/.test(prefix) && /^[a-z0-9]/i.test(rest)) continue;
        const candidate = trimLeadPunct(rest).trim();
        if (candidate.length >= 2 && !isBareGreeting(candidate)) {
          s = candidate;
          matched = true;
          break;
        }
      }
    }
    if (!matched) break;
  }

  const collapsed = s.replace(/\s+/g, " ").trim();
  if (!collapsed || isBareGreeting(collapsed)) return undefined;
  return collapsed.length > max ? `${collapsed.slice(0, max - 1).trimEnd()}…` : collapsed;
}
