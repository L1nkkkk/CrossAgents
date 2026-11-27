// Files sent with a message (docs/tasks/DOGFOOD.md §3) and media/documents referenced in replies:
// checked against the hub's limits before sending so the person hears why at once.
// Pure, tested in attachments.test.ts.

import type { AttachmentSupport } from "@bindings/AttachmentSupport";

/** The same limits as the hub (crates/api-types: MAX_ATTACHMENTS …). */
export const MAX_FILES = 8;
export const MAX_FILE_BYTES = 25 * 1024 * 1024;
export const MAX_TOTAL_BYTES = 40 * 1024 * 1024;

const BY_EXTENSION: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  svg: "image/svg+xml",
  pdf: "application/pdf",
  json: "application/json",
  md: "text/markdown",
  markdown: "text/markdown",
  csv: "text/csv",
  html: "text/html",
  htm: "text/html",
  css: "text/css",
  xml: "application/xml",
  rtf: "application/rtf",
  mp3: "audio/mpeg",
  wav: "audio/wav",
  m4a: "audio/mp4",
  ogg: "audio/ogg",
  oga: "audio/ogg",
  opus: "audio/opus",
  weba: "audio/webm",
  flac: "audio/flac",
  aac: "audio/aac",
  mp4: "video/mp4",
  webm: "video/webm",
  mov: "video/quicktime",
  m4v: "video/x-m4v",
  ogv: "video/ogg",
  mkv: "video/x-matroska",
  zip: "application/zip",
  gz: "application/gzip",
  tar: "application/x-tar",
  doc: "application/msword",
  docx: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  xls: "application/vnd.ms-excel",
  xlsx: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  ppt: "application/vnd.ms-powerpoint",
  pptx: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  epub: "application/epub+zip",
};

const IMAGE_MIMES = ["image/png", "image/jpeg", "image/gif", "image/webp", "image/bmp", "image/svg+xml"];
const AUDIO_MIMES = [
  "audio/mpeg",
  "audio/mp3",
  "audio/wav",
  "audio/x-wav",
  "audio/wave",
  "audio/mp4",
  "audio/x-m4a",
  "audio/m4a",
  "audio/ogg",
  "audio/opus",
  "audio/webm",
  "audio/flac",
  "audio/x-flac",
  "audio/aac",
];
const VIDEO_MIMES = [
  "video/mp4",
  "video/webm",
  "video/quicktime",
  "video/x-m4v",
  "video/ogg",
  "video/x-matroska",
];
const TEXT_APP_MIMES = [
  "application/json",
  "application/ld+json",
  "application/xml",
  "application/javascript",
  "application/x-javascript",
  "application/typescript",
  "application/yaml",
  "application/x-yaml",
  "application/toml",
  "application/sql",
  "application/x-sh",
  "application/rtf",
];
const FILE_MIMES = [
  "application/zip",
  "application/x-zip-compressed",
  "application/gzip",
  "application/x-gzip",
  "application/x-tar",
  "application/msword",
  "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
  "application/vnd.ms-excel",
  "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
  "application/vnd.ms-powerpoint",
  "application/vnd.openxmlformats-officedocument.presentationml.presentation",
  "application/epub+zip",
];

const TEXT_EXTENSIONS = [".txt", ".md", ".csv", ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".toml", ".yaml", ".yml", ".log", ".sql", ".xml"];
const MEDIA_EXTENSIONS = [".mp3", ".wav", ".m4a", ".ogg", ".opus", ".flac", ".aac", ".mp4", ".webm", ".mov", ".m4v", ".mkv"];
const DOC_EXTENSIONS = [".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".zip", ".gz", ".tar", ".rtf", ".epub"];

const ALL_ACCEPT = [
  ...IMAGE_MIMES,
  "application/pdf",
  "audio/*",
  "video/*",
  "application/json",
  "text/*",
  ...TEXT_EXTENSIONS,
  ...MEDIA_EXTENSIONS,
  ...DOC_EXTENSIONS,
].join(",");

export type Kind = "image" | "pdf" | "text" | "audio" | "video" | "file";
type SupportKind = "image" | "pdf" | "text";
const SUPPORT_KINDS: SupportKind[] = ["image", "pdf", "text"];
const KIND_NAMES: Record<Kind, string> = {
  image: "图片",
  pdf: "PDF",
  text: "文本",
  audio: "音频",
  video: "视频",
  file: "文件",
};

/** A file's type: what the browser says, else by extension; code and logs count as plain text. */
export function mimeOf(name: string, type: string): string {
  const bare = type.split(";")[0]?.trim().toLowerCase() ?? "";
  if (bare && bare !== "application/octet-stream") return bare;
  const ext = name.toLowerCase().split(".").pop() ?? "";
  if (BY_EXTENSION[ext]) return BY_EXTENSION[ext];
  return /^(txt|log|rs|ts|tsx|js|jsx|mjs|cjs|py|go|java|c|h|cpp|cc|hpp|toml|yaml|yml|sh|ps1|sql|xml|ini|cfg)$/.test(ext)
    ? "text/plain"
    : bare;
}

/** The hub's kind for a type: image, PDF, text, audio, video or file; anything else it does not take. */
export function kindOf(mime: string): Kind | undefined {
  const bare = mime.split(";")[0]?.trim().toLowerCase() ?? "";
  if (IMAGE_MIMES.includes(bare)) return "image";
  if (bare === "application/pdf") return "pdf";
  if (AUDIO_MIMES.includes(bare)) return "audio";
  if (VIDEO_MIMES.includes(bare)) return "video";
  if (TEXT_APP_MIMES.includes(bare) || bare.startsWith("text/")) return "text";
  if (FILE_MIMES.includes(bare)) return "file";
  return undefined;
}

/** Classifies a local file path by extension for reply inline rendering. */
export function mediaKindOfPath(path: string): Kind | undefined {
  const clean = path.split(/[?#]/)[0] ?? path;
  const dot = clean.lastIndexOf(".");
  if (dot < 0 || dot === clean.length - 1) return undefined;
  const ext = clean.slice(dot + 1).toLowerCase();
  const mime = mimeOf(`x.${ext}`, "");
  return mime ? kindOf(mime) : undefined;
}

/** Whether the hub takes files of this type. */
export function mimeAllowed(mime: string): boolean {
  return kindOf(mime) !== undefined;
}

export const isImage = (mime: string) => kindOf(mime) === "image";
export const isAudio = (mime: string) => kindOf(mime) === "audio";
export const isVideo = (mime: string) => kindOf(mime) === "video";
export const isPdf = (mime: string) => kindOf(mime) === "pdf";
export const isText = (mime: string) => kindOf(mime) === "text";

/**
 * Model APIs refuse an image over 5 MB (base64 a third larger) or with a side over 8000 px, and
 * scale anything past ~2000 px down anyway. A picture past either bound is re-encoded smaller
 * before it is sent, rather than refused.
 */
export const IMAGE_SEND_BYTES = 3.5 * 1024 * 1024;
export const IMAGE_MAX_SIDE = 2560;

/**
 * Whether an image of this type, size and pixel dimensions should be shrunk before sending, and
 * to what dimensions. GIFs are left alone (re-encoding drops the animation).
 */
export function shrinkPlan(mime: string, size: number, width: number, height: number): { width: number; height: number } | undefined {
  if (!/^image\/(png|jpeg|webp)$/.test(mime)) return undefined;
  const side = Math.max(width, height);
  if (size <= IMAGE_SEND_BYTES && side <= IMAGE_MAX_SIDE) return undefined;
  const scale = Math.min(1, IMAGE_MAX_SIDE / side);
  return { width: Math.max(1, Math.round(width * scale)), height: Math.max(1, Math.round(height * scale)) };
}

/** The name a shrunk picture goes by: its own, with the extension of the type it became. */
export function shrunkName(name: string, mime: string): string {
  const ext = mime === "image/png" ? "png" : mime === "image/webp" ? "webp" : "jpg";
  const stem = name.includes(".") ? name.slice(0, name.lastIndexOf(".")) : name;
  return `${stem || "图片"}.${ext}`;
}

function allows(support: AttachmentSupport, kind: Kind): boolean {
  if (kind === "image") return support.image;
  if (kind === "pdf") return support.pdf;
  // Text, audio, video and common documents go via text prompt / local file reference.
  return support.text;
}

/** Why adding `adding` to `held` breaks a limit, or undefined. */
export function refusal(
  held: { size: number }[],
  adding: { name: string; mime: string; size: number }[],
  support?: AttachmentSupport | null,
  driver?: string | null,
): string | undefined {
  if (held.length + adding.length > MAX_FILES) return `一条消息最多带 ${MAX_FILES} 个文件`;
  for (const f of adding) {
    if (!mimeAllowed(f.mime)) {
      return `${f.name}：这种文件发不了（${f.mime || "未知类型"}）；支持图片、音频、视频、PDF、文本和常见文档`;
    }
    if (f.size > MAX_FILE_BYTES) return `${f.name} 有 ${mb(f.size)}，单个文件最多 ${mb(MAX_FILE_BYTES)}`;
  }
  const total = [...held, ...adding].reduce((n, f) => n + f.size, 0);
  if (total > MAX_TOTAL_BYTES) return `文件合计 ${mb(total)}，一条消息最多 ${mb(MAX_TOTAL_BYTES)}`;
  if (!support) return undefined;
  for (const f of adding) {
    const kind = kindOf(f.mime);
    if (!kind || allows(support, kind)) continue;
    const what = takes(support);
    const noun = kind === "pdf" ? " PDF" : KIND_NAMES[kind];
    return `${f.name}：这个 agent 的驱动（${driver ?? "未知驱动"}）不收${noun}${what ? `，只收${what}` : "，什么文件都不收"}`;
  }
  return undefined;
}

/** What a picker should offer: only the kinds the agent takes, plus our extensions. */
export function acceptFor(support?: AttachmentSupport | null): string {
  if (!support) return ALL_ACCEPT;
  const mimes: string[] = [];
  if (support.image) mimes.push(...IMAGE_MIMES);
  if (support.pdf) mimes.push("application/pdf");
  if (support.text) {
    mimes.push(
      "audio/*",
      "video/*",
      "application/json",
      "text/*",
      ...TEXT_EXTENSIONS,
      ...MEDIA_EXTENSIONS,
      ...DOC_EXTENSIONS,
    );
  }
  return mimes.join(",");
}

/** Why some kinds cannot be sent, when the agent takes only part of them; else undefined. */
export function supportNote(support?: AttachmentSupport | null, driver?: string | null): string | undefined {
  if (!support) return undefined;
  const missing = SUPPORT_KINDS.filter((kind) => !support[kind]);
  if (!missing.length) return undefined;
  const what = takes(support);
  const tail = driver ? `（${driver}）` : "";
  if (!what) return `这个 agent 什么文件都不收${tail}`;
  return `这个 agent 只收${what}，${missing.map((kind) => KIND_NAMES[kind]).join("和")} 发不了${tail}`;
}

/** "图片和文本", "图片、PDF 和文本", or undefined when it takes none. */
function takes(support: AttachmentSupport): string | undefined {
  const names = SUPPORT_KINDS.filter((kind) => support[kind]).map((kind) => KIND_NAMES[kind]);
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
