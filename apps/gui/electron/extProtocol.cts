// Validate before URL normalization, then build a hub asset path.
import { isAbsolute, win32 } from "node:path";

export function assetPath(url: string): string {
  const match = /^crossagents-ext:\/\/([a-z][a-z0-9-]*)\/([^/?#]+)\/([^?#]+)(?:\?[^#]*)?(?:#.*)?$/.exec(url);
  if (!match) throw new Error("invalid extension URL");
  const [, name, fingerprint, encoded] = match;
  if (!/^[a-zA-Z0-9_-]+$/.test(fingerprint)) throw new Error("invalid fingerprint");
  const path = decodeURIComponent(encoded);
  if (isAbsolute(path) || win32.isAbsolute(path) || /[\\:\0]/.test(path) || path.split("/").some((part) => part === ".." || part === "." || part === "")) {
    throw new Error("invalid extension path");
  }
  return `/v1/extensions/${name}/assets/${fingerprint}/${path.split("/").map(encodeURIComponent).join("/")}`;
}
