// Pure path/allowlist checks, shared with tests; no Electron dependency.
import { isAbsolute, relative, resolve, sep, win32 } from "node:path";
import { realpath } from "node:fs/promises";

export type ExtensionVersions = {
  name: string;
  current?: { fingerprint: string };
  previous_fingerprint?: string;
};

export function inside(root: string, file: string): boolean {
  const rel = relative(root, file);
  return rel !== "" && !isAbsolute(rel) && rel !== ".." && !rel.startsWith(`..${sep}`);
}

/** Inspect the raw path before URL normalization can erase encoded dot segments. */
export function extensionFile(home: string, url: string, versions: readonly ExtensionVersions[]): { root: string; file: string } {
  const match = /^crossagents-ext:\/\/([a-z][a-z0-9-]*)\/([^/?#]+)\/([^?#]+)(?:\?[^#]*)?(?:#.*)?$/.exec(url);
  if (!match) throw new Error("invalid extension URL");
  const [, name, fingerprint, encoded] = match;
  if (!/^[a-zA-Z0-9_-]+$/.test(fingerprint)) throw new Error("invalid fingerprint");
  const version = versions.find((entry) => entry.name === name);
  if (!version || (version.current?.fingerprint !== fingerprint && version.previous_fingerprint !== fingerprint)) {
    throw new Error("extension version is not current or previous");
  }
  const path = decodeURIComponent(encoded);
  // Reject Windows paths on every platform too (drive-relative paths and ADS included).
  if (isAbsolute(path) || win32.isAbsolute(path) || /[\\:\0]/.test(path) || path.split("/").some((part) => part === ".." || part === "")) {
    throw new Error("invalid extension path");
  }
  const root = resolve(home, "extension-versions", name, fingerprint);
  const file = resolve(root, path);
  if (!inside(root, file)) throw new Error("extension path escapes snapshot");
  return { root, file };
}

/** Also refuse symlinks/junctions that escape the snapshot on disk. */
export async function extensionRealFile(home: string, url: string, versions: readonly ExtensionVersions[]): Promise<string> {
  const { root, file } = extensionFile(home, url, versions);
  const [realRoot, realFile] = await Promise.all([realpath(root), realpath(file)]);
  if (!inside(realRoot, realFile)) throw new Error("extension file escapes snapshot");
  // The snapshot root itself must not be redirected outside extension-versions.
  const realVersions = await realpath(resolve(home, "extension-versions"));
  if (!inside(realVersions, realRoot)) throw new Error("extension snapshot escapes versions directory");
  return realFile;
}
