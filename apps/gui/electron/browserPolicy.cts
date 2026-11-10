// Which <webview> tags the main process lets in, what a browser tab may load, which permissions it
// gets, and which proxy its session uses. Pure, so a test can check it without Electron
// (electron/browser.cts applies it; main.cts wires it).

import type { WebPreferences } from "electron";

/** The session every built-in browser tab runs in. Nothing else may use it. */
export const BROWSER_PARTITION = "persist:browser";
/**
 * A marker the page may set on each browser <webview>. Electron does not forward unknown
 * attributes to will-attach-webview, so it is informational only and never required.
 */
export const BROWSER_MARK = "data-crossagents-browser";
/** The scheme that serves local files to a browser tab (electron/browser.cts). */
export const FILE_SCHEME = "crossagents-file";

/**
 * Whether a `<webview>` being attached is one of the built-in browser's tabs: it must be on the
 * browser partition, attached by the app's own page (`hostIsApp`), and start at a page the tab
 * may show (empty or about:blank included). The main process decides `hostIsApp`.
 */
export function webviewAllowed(params: Readonly<Record<string, string | undefined>>, hostIsApp: boolean): boolean {
  if (!hostIsApp || params.partition !== BROWSER_PARTITION) return false;
  const src = params.src ?? "";
  return src === "" || src === "about:blank" || navigationAllowed(src);
}

/**
 * Forces the preferences of a browser tab, whatever the page asked for: no preload, no Node, a
 * sandboxed isolated world, web security on, and no webviews inside the tab.
 */
export function enforceGuestPreferences(prefs: WebPreferences): void {
  delete prefs.preload;
  Reflect.deleteProperty(prefs, "preloadURL");
  prefs.nodeIntegration = false;
  prefs.nodeIntegrationInSubFrames = false;
  prefs.contextIsolation = true;
  prefs.sandbox = true;
  prefs.webSecurity = true;
  prefs.allowRunningInsecureContent = false;
  prefs.webviewTag = false;
}

const TAB_PROTOCOLS: ReadonlySet<string> = new Set(["http:", "https:", `${FILE_SCHEME}:`]);

/** Where a browser tab may navigate: web pages and the local files it is given. */
export function navigationAllowed(url: string): boolean {
  try {
    return TAB_PROTOCOLS.has(new URL(url).protocol);
  } catch {
    return false;
  }
}

/**
 * Browser tabs get no permission at all: camera, microphone, location, notifications and clipboard
 * reads are refused, and so is everything else.
 */
export function browserPermissionAllowed(_permission: string): boolean {
  return false;
}

export type ProxySettings = { proxyRules: string; proxyBypassRules?: string };

/** `host:port` gets `http://`; a value that already has a scheme is kept. */
function withScheme(value: string): string {
  const trimmed = value.trim().replace(/\/+$/, "");
  return /^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;
}

/**
 * The proxy the browser uses, as the environment names it (the same order as the link program:
 * HTTPS_PROXY first, then ALL_PROXY, then HTTP_PROXY). Undefined when none is set: the session
 * then follows the system's proxy.
 */
export function browserProxy(env: NodeJS.ProcessEnv): ProxySettings | undefined {
  const keys = ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy", "HTTP_PROXY", "http_proxy"];
  const found = keys.map((k) => env[k]?.trim()).find((v): v is string => !!v);
  if (!found) return undefined;
  const bypass = (env.NO_PROXY ?? env.no_proxy ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean)
    .join(",");
  return { proxyRules: withScheme(found), ...(bypass ? { proxyBypassRules: bypass } : {}) };
}
