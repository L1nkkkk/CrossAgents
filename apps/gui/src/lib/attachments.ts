// Files sent with a message (docs/tasks/DOGFOOD.md §3): the hub's limits, checked before
// sending so the person hears why at once. Pure, tested in attachments.test.ts.

import type { AttachmentSupport } from "@bindings/AttachmentSupport";

/** The same limits as the hub (crates/api-types: MAX_ATTACHMENTS …). */
export const MAX_FILES = 8;
export const MAX_FILE_BYTES = 10 * 1024 * 1024;
export const MAX_TOTAL_BYTES = 20 * 1024 * 1024;

const BY_EXTENSION: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  pdf: "application/pdf",
  json: "application/json",
  md: "text/markdown",
  csv: "text/csv",
  html: "text/html",
  css: "text/css",
};

const IMAGE_MIMES = ["image/png", "image/jpeg", "image/gif", "image/webp"];
const FILE_EXTENSIONS = [".md", ".rs", ".ts", ".tsx", ".py", ".toml", ".yaml", ".yml", ".log"];
const ALL_ACCEPT = [...IMAGE_MIMES, "application/pdf", "application/json", "text/*", ...FILE_EXTENSIONS].join(",");

type Kind = "image" | "pdf" | "text";
const KINDS: Kind[] = ["image", "pdf", "text"];
const KIND_NAMES: Record<Kind, string> = { image: "图片", pdf: "PDF", text: "文本" };

/** A file's type: what the browser says, else by extension; code and logs count as plain text. */
export function mimeOf(name: string, type: string): string {
  if (type && type !== "application/octet-stream") return type;
  const ext = name.toLowerCase().split(".").pop() ?? "";
  if (BY_EXTENSION[ext]) return BY_EXTENSION[ext];
  return /^(txt|log|rs|ts|tsx|js|jsx|mjs|cjs|py|go|java|c|h|cpp|toml|yaml|yml|sh|ps1|sql|xml|ini|cfg)$/.test(ext) ? "text/plain" : type;
}

/** The hub's kind for a type: image, PDF or text; anything else it does not take. */
export function kindOf(mime: string): Kind | undefined {
  if (IMAGE_MIMES.includes(mime)) return "image";
  if (mime === "application/pdf") return "pdf";
  if (mime === "application/json" || mime.startsWith("text/")) return "text";
  return undefined;
}

/** Whether the hub takes files of this type (images, PDF, JSON, text). */
export function mimeAllowed(mime: string): boolean {
  return kindOf(mime) !== undefined;
}

export const isImage = (mime: string) => mime.startsWith("image/");

/** Why adding `adding` to `held` breaks a limit, or undefined. */
export function refusal(
  held: { size: number }[],
  adding: { name: string; mime: string; size: number }[],
  support?: AttachmentSupport | null,
  driver?: string | null,
): string | undefined {
  if (held.length + adding.length > MAX_FILES) return `一条消息最多带 ${MAX_FILES} 个文件`;
  for (const f of adding) {
    if (!mimeAllowed(f.mime)) return `${f.name}：这种文件发不了（${f.mime || "未知类型"}）；能发图片、PDF、JSON 和文本`;
    if (f.size > MAX_FILE_BYTES) return `${f.name} 有 ${mb(f.size)}，单个文件最多 ${mb(MAX_FILE_BYTES)}`;
  }
  const total = [...held, ...adding].reduce((n, f) => n + f.size, 0);
  if (total > MAX_TOTAL_BYTES) return `文件合计 ${mb(total)}，一条消息最多 ${mb(MAX_TOTAL_BYTES)}`;
  if (!support) return undefined;
  for (const f of adding) {
    const kind = kindOf(f.mime);
    if (!kind || support[kind]) continue;
    const what = takes(support);
    const noun = kind === "pdf" ? " PDF" : KIND_NAMES[kind];
    return `${f.name}：这个 agent 的驱动（${driver ?? "未知驱动"}）不收${noun}${what ? `，只收${what}` : "，什么文件都不收"}`;
  }
  return undefined;
}

/** What a picker should offer: only the kinds the agent takes, plus our text extensions. */
export function acceptFor(support?: AttachmentSupport | null): string {
  if (!support) return ALL_ACCEPT;
  const mimes: string[] = [];
  if (support.image) mimes.push(...IMAGE_MIMES);
  if (support.pdf) mimes.push("application/pdf");
  if (support.text) mimes.push("application/json", "text/*", ...FILE_EXTENSIONS);
  return mimes.join(",");
}

/** Why some kinds cannot be sent, when the agent takes only part of them; else undefined. */
export function supportNote(support?: AttachmentSupport | null, driver?: string | null): string | undefined {
  if (!support) return undefined;
  const missing = KINDS.filter((kind) => !support[kind]);
  if (!missing.length) return undefined;
  const what = takes(support);
  const tail = driver ? `（${driver}）` : "";
  if (!what) return `这个 agent 什么文件都不收${tail}`;
  return `这个 agent 只收${what}，${missing.map((kind) => KIND_NAMES[kind]).join("和")} 发不了${tail}`;
}

/** "图片和文本", "图片、PDF 和文本", or undefined when it takes none. */
function takes(support: AttachmentSupport): string | undefined {
  const names = KINDS.filter((kind) => support[kind]).map((kind) => KIND_NAMES[kind]);
  if (!names.length) return undefined;
  if (names.length === 1) return names[0];
  return `${names.slice(0, -1).join("、")}和${names[names.length - 1]}`;
}

/** "1.2 MB", "340 KB", "12 B". */
export function bytes(n: number): string {
  if (n >= 1024 * 1024) return mb(n);
  if (n >= 1024) return `${Math.round(n / 1024)} KB`;
  return `${n} B`;
}

function mb(n: number): string {
  return `${(n / (1024 * 1024)).toFixed(n >= 10 * 1024 * 1024 ? 0 : 1)} MB`;
}
