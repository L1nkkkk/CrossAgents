// Files sent with a message (docs/tasks/DOGFOOD.md §3): the hub's limits, checked before
// sending so the person hears why at once. Pure, tested in attachments.test.ts.

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

/** A file's type: what the browser says, else by extension; code and logs count as plain text. */
export function mimeOf(name: string, type: string): string {
  if (type && type !== "application/octet-stream") return type;
  const ext = name.toLowerCase().split(".").pop() ?? "";
  if (BY_EXTENSION[ext]) return BY_EXTENSION[ext];
  return /^(txt|log|rs|ts|tsx|js|jsx|mjs|cjs|py|go|java|c|h|cpp|toml|yaml|yml|sh|ps1|sql|xml|ini|cfg)$/.test(ext) ? "text/plain" : type;
}

/** Whether the hub takes files of this type (images, PDF, JSON, text). */
export function mimeAllowed(mime: string): boolean {
  return ["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf", "application/json"].includes(mime) || mime.startsWith("text/");
}

export const isImage = (mime: string) => mime.startsWith("image/");

/** Why adding `adding` to `held` breaks a limit, or undefined. */
export function refusal(held: { size: number }[], adding: { name: string; mime: string; size: number }[]): string | undefined {
  if (held.length + adding.length > MAX_FILES) return `一条消息最多带 ${MAX_FILES} 个文件`;
  for (const f of adding) {
    if (!mimeAllowed(f.mime)) return `${f.name}：这种文件发不了（${f.mime || "未知类型"}）；能发图片、PDF、JSON 和文本`;
    if (f.size > MAX_FILE_BYTES) return `${f.name} 有 ${mb(f.size)}，单个文件最多 ${mb(MAX_FILE_BYTES)}`;
  }
  const total = [...held, ...adding].reduce((n, f) => n + f.size, 0);
  if (total > MAX_TOTAL_BYTES) return `文件合计 ${mb(total)}，一条消息最多 ${mb(MAX_TOTAL_BYTES)}`;
  return undefined;
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
