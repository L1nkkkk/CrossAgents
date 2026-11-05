// The extension host (docs/EXTENSIONS.md §5): reads `GET /v1/extensions`, imports each enabled
// extension's GUI entry through `crossagents-ext://<name>/<fingerprint>/<path>`, and gives it a
// `ca` object. Everything registered through `ca` is recorded here, per extension, so a failed
// `activate`, a version switch or a disable leaves no residue behind — the extension never has
// to return a cleanup function for the things it registered, only for resources it opened.
//
// The page keeps running when one extension breaks: `activate` throwing or timing out marks
// that one failed and the others load as usual. `extension.changed` re-reads the list: an
// extension whose fingerprint moved is cleaned up first, then its new entry is imported, with
// no page reload (old modules stay in memory until the next full reload, as the design says).

import * as React from "react";
import htm from "htm";
import type { ExtensionCallAccepted } from "@bindings/ExtensionCallAccepted";
import type { ExtensionCallResponse } from "@bindings/ExtensionCallResponse";
import type { ExtensionCallStatus } from "@bindings/ExtensionCallStatus";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { JsonValue } from "@bindings/serde_json/JsonValue";
import type { HubEvent } from "../hub";
import { extTabOf, type ExtTab } from "./tabs.ts";
import { normalizeToolName, type ToolCallItem } from "./toolcards.ts";

/** How long one extension's entry may take to import, and its `activate` to finish. */
export const ACTIVATE_TIMEOUT_MS = 10_000;
/** How often a running call is checked when the event stream has not said it finished. */
export const CALL_POLL_MS = 2_000;
/** First wait before reading the extension list again when the hub was not up yet. */
export const RETRY_MS = 1_000;
export const MAX_RETRY_MS = 30_000;

export type ExtensionStatus = "loading" | "ready" | "failed" | "disabled";

/** What a tab (or a page, or a settings block) is called, and what its render function gets. */
export type ExtensionPanelContext = { follow?: string; extension: string };

export type ExtensionCardContext = {
  extension: string;
  call: ToolCallItem;
  /** `detail.arguments` parsed, when it was JSON. */
  args?: unknown;
  /** `detail.result` parsed when JSON, else the call's raw output. */
  result?: unknown;
  error?: string;
};

export type TabRegistration = {
  extension: string;
  tab: ExtTab;
  id: string;
  label: string;
  fingerprint: string;
  render: (context: ExtensionPanelContext) => React.ReactNode;
};

export type CardRegistration = {
  extension: string;
  /** `<extension>__<tool>`, or the bare tool name when the extension registered it so. */
  key: string;
  fingerprint: string;
  render: (context: ExtensionCardContext) => React.ReactNode;
};

export type PageRegistration = {
  extension: string;
  id: string;
  label: string;
  fingerprint: string;
  render: (context: ExtensionPanelContext) => React.ReactNode;
};

export type SettingsRegistration = PageRegistration;

/** `ca.tools.call`: a Promise that can also be cancelled; it is never retried. */
export type CancellablePromise<T> = Promise<T> & { cancel(): void };

export type ExtensionCa = {
  React: typeof React;
  html: (strings: TemplateStringsArray, ...values: unknown[]) => React.ReactNode;
  createElement: typeof React.createElement;
  Fragment: typeof React.Fragment;
  createContext: typeof React.createContext;
  useContext: typeof React.useContext;
  useState: typeof React.useState;
  useReducer: typeof React.useReducer;
  useEffect: typeof React.useEffect;
  useLayoutEffect: typeof React.useLayoutEffect;
  useMemo: typeof React.useMemo;
  useCallback: typeof React.useCallback;
  useRef: typeof React.useRef;
  useSyncExternalStore: typeof React.useSyncExternalStore;
  tabs: {
    register(definition: { id: string; label?: string; render: (context: ExtensionPanelContext) => React.ReactNode }): void;
  };
  pages: {
    register(definition: { id: string; label?: string; render: (context: ExtensionPanelContext) => React.ReactNode }): void;
  };
  settings: {
    register(definition: { id: string; label?: string; render: (context: ExtensionPanelContext) => React.ReactNode }): void;
  };
  toolCards: {
    register(tool: string, render: (context: ExtensionCardContext) => React.ReactNode): void;
  };
  styles: { add(path: string): void };
  hub: { request(method: "GET" | "POST" | "DELETE", path: string, body?: unknown): Promise<unknown> };
  events: {
    on(listener: (event: HubEvent) => void): () => void;
    on(type: string, listener: (event: HubEvent) => void): () => void;
  };
  tools: { call(tool: string, input?: JsonValue): CancellablePromise<JsonValue> };
};

export type ExtensionModule = {
  activate?: (ca: ExtensionCa) => unknown;
  default?: (ca: ExtensionCa) => unknown;
};

/** One extension as the page sees it: the hub's state plus what it registered and logged. */
export type ExtensionView = {
  name: string;
  status: ExtensionStatus;
  error?: string;
  fingerprint?: string;
  tabs: readonly TabRegistration[];
  cards: readonly CardRegistration[];
  pages: readonly PageRegistration[];
  settings: readonly SettingsRegistration[];
  logs: readonly string[];
};

export type ExtensionsSnapshot = {
  revision: number;
  /** Names the hub says are enabled; extension tabs are pruned only for names not here. */
  enabled: ReadonlySet<string>;
  records: ReadonlyMap<string, ExtensionView>;
  tabs: ReadonlyMap<ExtTab, TabRegistration>;
  cards: ReadonlyMap<string, CardRegistration>;
  pages: ReadonlyMap<string, PageRegistration>;
  settings: ReadonlyMap<string, SettingsRegistration>;
  loading: boolean;
  safeMode: boolean;
  /** The extension list has been read once: only then can a saved `ext:` tab be judged. */
  listed: boolean;
  /** Why the last list could not be read; the page still runs. */
  error?: string;
};

export type ExtensionHostOptions = {
  list(): Promise<ExtensionInfo[]>;
  importModule(url: string): Promise<unknown>;
  request(method: "GET" | "POST" | "DELETE", path: string, body?: unknown): Promise<unknown>;
  callTool(extension: string, tool: string, input: JsonValue): Promise<ExtensionCallAccepted>;
  getCall(id: string): Promise<ExtensionCallResponse>;
  cancelCall(id: string): Promise<unknown>;
  disable(name: string): Promise<unknown>;
  /** The hub's `extension.changed` / `extension.call.finished` stream. */
  events?: (listener: (event: HubEvent) => void) => () => void;
  /** Resolved before anything is imported; `true` skips the extensions entirely. */
  safeMode?: () => boolean | Promise<boolean>;
  /** Awaited before an import, at startup and on every hot reload (the shell sets its marker). */
  onLoading?: () => void | Promise<void>;
  /** Awaited once a load settled, failures included (the shell clears its marker). */
  onReady?: () => void | Promise<void>;
  timeoutMs?: number;
  pollMs?: number;
  /** First wait before reading the list again; doubles up to `maxRetryMs`. */
  retryMs?: number;
  maxRetryMs?: number;
  /** Where a failure goes instead of the console (tests pass one to stay quiet). */
  log?: (line: string) => void;
};

/** `crossagents-ext://<name>/<fingerprint>/<entry>`: the fingerprint in the path, not a query. */
export function extensionEntryUrl(name: string, fingerprint: string, entry: string): string {
  const path = entry
    .replace(/^\.?\//, "")
    .split("/")
    .map(encodeURIComponent)
    .join("/");
  return `crossagents-ext://${name}/${fingerprint}/${path}`;
}

export class ExtensionCallError extends Error {
  status: ExtensionCallStatus;
  stderr?: string;
  constructor(status: ExtensionCallStatus, message: string, stderr?: string) {
    super(message);
    this.name = "ExtensionCallError";
    this.status = status;
    this.stderr = stderr;
  }
}

type Rec = {
  name: string;
  status: ExtensionStatus;
  error?: string;
  fingerprint?: string;
  entry?: string;
  entryUrl?: string;
  /** False once cleaned up: a late `activate` may not register anything. */
  live: boolean;
  generation: number;
  tabs: TabRegistration[];
  cards: CardRegistration[];
  pages: PageRegistration[];
  settings: SettingsRegistration[];
  styles: { href: string; link?: HTMLLinkElement }[];
  offs: (() => void)[];
  cleanup?: () => void;
  logs: string[];
};

const textOf = (error: unknown): string => (error instanceof Error ? error.message : String(error));

function withTimeout<T>(promise: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

const STATUS_TEXT: Record<ExtensionCallStatus, string> = {
  running: "还在运行",
  ok: "完成",
  failed: "失败",
  cancelled: "已取消",
  timed_out: "超时",
};

export function createExtensionHost(options: ExtensionHostOptions) {
  const timeoutMs = options.timeoutMs ?? ACTIVATE_TIMEOUT_MS;
  const pollMs = options.pollMs ?? CALL_POLL_MS;
  const retryMs = options.retryMs ?? RETRY_MS;
  const maxRetryMs = options.maxRetryMs ?? MAX_RETRY_MS;
  const records = new Map<string, Rec>();
  const finished = new Set<string>();
  const waiters = new Map<string, Set<() => void>>();
  const listeners = new Set<() => void>();
  let revision = 0;
  let enabled = new Set<string>();
  let loading = false;
  let safeMode = false;
  let listed = false;
  let listError: string | undefined;
  let started = false;
  let acceptingChanges = false;
  let readyReported = false;
  let disposed = false;
  let starting: Promise<void> | undefined;
  let chain: Promise<void> = Promise.resolve();
  let retryTimer: ReturnType<typeof setTimeout> | undefined;
  let retryDelay = retryMs;
  let snapshot = build();

  function build(): ExtensionsSnapshot {
    const views = new Map<string, ExtensionView>();
    const tabs = new Map<ExtTab, TabRegistration>();
    const cards = new Map<string, CardRegistration>();
    const pages = new Map<string, PageRegistration>();
    const settings = new Map<string, SettingsRegistration>();
    for (const rec of records.values()) {
      views.set(rec.name, {
        name: rec.name,
        status: rec.status,
        error: rec.error,
        fingerprint: rec.fingerprint,
        tabs: [...rec.tabs],
        cards: [...rec.cards],
        pages: [...rec.pages],
        settings: [...rec.settings],
        logs: [...rec.logs],
      });
      for (const tab of rec.tabs) tabs.set(tab.tab, tab);
      for (const card of rec.cards) cards.set(card.key, card);
      for (const page of rec.pages) pages.set(`${rec.name}/${page.id}`, page);
      for (const block of rec.settings) settings.set(`${rec.name}/${block.id}`, block);
    }
    return {
      revision,
      enabled: new Set(enabled),
      records: views,
      tabs,
      cards,
      pages,
      settings,
      loading,
      safeMode,
      listed,
      error: listError,
    };
  }

  function publish() {
    revision++;
    snapshot = build();
    listeners.forEach((listener) => listener());
  }

  function logTo(rec: Rec, what: string, error: unknown) {
    const message = `${new Date().toISOString()} ${what}：${textOf(error)}`;
    rec.logs.push(message);
    if (rec.logs.length > 200) rec.logs.splice(0, rec.logs.length - 200);
    const line = `[extension ${rec.name}] ${what}：${textOf(error)}`;
    if (options.log) options.log(line);
    else console.error(line, error);
  }

  /** Records an error the page caught (a throwing render function) in the extension's log. */
  function reportError(name: string, error: unknown, what = "渲染出错") {
    const rec = records.get(name);
    if (rec) logTo(rec, what, error);
    else if (options.log) options.log(`[extension ${name}] ${what}：${textOf(error)}`);
    else console.error(`[extension ${name}] ${what}`, error);
  }

  /** Runs a cleanup an extension returned; a throw is the extension's log, not the page's. */
  function runOwnCleanup(rec: Rec, cleanup: () => void, what = "扩展自己的清理函数出错") {
    try {
      cleanup();
    } catch (error) {
      logTo(rec, what, error);
    }
  }

  /**
   * Removes everything `rec` registered: event subscriptions, styles, tabs, cards, pages,
   * settings and the cleanup an extension returned itself. Safe to call any number of times.
   */
  function cleanup(rec: Rec) {
    rec.live = false;
    rec.generation++;
    for (const off of rec.offs.splice(0)) {
      try {
        off();
      } catch (error) {
        logTo(rec, "取消订阅失败", error);
      }
    }
    for (const style of rec.styles.splice(0)) {
      try {
        style.link?.remove();
      } catch (error) {
        logTo(rec, "移除样式失败", error);
      }
    }
    rec.tabs = [];
    rec.cards = [];
    rec.pages = [];
    rec.settings = [];
    const own = rec.cleanup;
    rec.cleanup = undefined;
    if (own) runOwnCleanup(rec, own);
  }

  function makeCa(rec: Rec, generation: number): ExtensionCa {
    const guard = () => {
      if (!rec.live || rec.generation !== generation) throw new Error("扩展已经停用或切换了版本，注册被拒绝");
    };
    const fingerprint = rec.fingerprint ?? "";
    const entryUrl = rec.entryUrl ?? `crossagents-ext://${rec.name}/`;

    const addCard = (key: string, render: (context: ExtensionCardContext) => React.ReactNode) => {
      rec.cards = rec.cards.filter((card) => card.key !== key);
      rec.cards.push({ extension: rec.name, key, fingerprint, render });
    };

    return {
      React,
      html: htm.bind(React.createElement as never) as unknown as ExtensionCa["html"],
      createElement: React.createElement,
      Fragment: React.Fragment,
      createContext: React.createContext,
      useContext: React.useContext,
      useState: React.useState,
      useReducer: React.useReducer,
      useEffect: React.useEffect,
      useLayoutEffect: React.useLayoutEffect,
      useMemo: React.useMemo,
      useCallback: React.useCallback,
      useRef: React.useRef,
      useSyncExternalStore: React.useSyncExternalStore,
      tabs: {
        register(definition) {
          guard();
          if (!definition || typeof definition.id !== "string" || !definition.id || definition.id.includes("/")) {
            throw new Error("tab id 必须是非空字符串，且不含 /");
          }
          if (typeof definition.render !== "function") throw new Error("tab 需要一个 render 函数");
          const id = definition.id;
          rec.tabs = rec.tabs.filter((tab) => tab.id !== id);
          rec.tabs.push({
            extension: rec.name,
            tab: extTabOf(rec.name, id),
            id,
            label: definition.label?.trim() || id,
            fingerprint,
            render: definition.render,
          });
          publish();
        },
      },
      pages: {
        register(definition) {
          guard();
          if (!definition || typeof definition.id !== "string" || !definition.id) throw new Error("page id 必须是非空字符串");
          if (typeof definition.render !== "function") throw new Error("page 需要一个 render 函数");
          rec.pages = rec.pages.filter((page) => page.id !== definition.id);
          rec.pages.push({ extension: rec.name, id: definition.id, label: definition.label?.trim() || definition.id, fingerprint, render: definition.render });
          publish();
        },
      },
      settings: {
        register(definition) {
          guard();
          if (!definition || typeof definition.id !== "string" || !definition.id) throw new Error("settings id 必须是非空字符串");
          if (typeof definition.render !== "function") throw new Error("settings 需要一个 render 函数");
          rec.settings = rec.settings.filter((block) => block.id !== definition.id);
          rec.settings.push({ extension: rec.name, id: definition.id, label: definition.label?.trim() || definition.id, fingerprint, render: definition.render });
          publish();
        },
      },
      toolCards: {
        register(tool, render) {
          guard();
          const name = normalizeToolName(String(tool));
          if (!name || name.includes("/")) throw new Error(`工具名不能用：${tool}`);
          // v1: a card only for a tool of this extension. A bare name is qualified with it; a
          // qualified name must already carry it, so another extension's tool is refused.
          const key = name.includes("__") ? name : `${rec.name}__${name}`;
          if (!key.startsWith(`${rec.name}__`) || key.length === rec.name.length + 2) {
            throw new Error(`扩展 ${rec.name} 只能给自己的工具注册卡片：${tool}`);
          }
          if (typeof render !== "function") throw new Error("tool card 需要一个 render 函数");
          addCard(key, render);
          publish();
        },
      },
      styles: {
        add(path) {
          guard();
          if (typeof path !== "string" || !path) throw new Error("styles.add 需要一个路径");
          const href = new URL(path, entryUrl).href;
          let link: HTMLLinkElement | undefined;
          if (typeof document !== "undefined") {
            link = document.createElement("link");
            link.rel = "stylesheet";
            link.href = href;
            link.dataset.extension = rec.name;
            document.head.appendChild(link);
          }
          rec.styles.push({ href, link });
          publish();
        },
      },
      hub: {
        request: (method, path, body) => options.request(method, path, body),
      },
      events: {
        on(typeOrListener: string | ((event: HubEvent) => void), maybe?: (event: HubEvent) => void) {
          guard();
          const type = typeof typeOrListener === "string" ? typeOrListener : undefined;
          const listener = typeof typeOrListener === "function" ? typeOrListener : maybe;
          if (typeof listener !== "function") throw new Error("events.on 需要一个回调");
          const wrapped = (event: HubEvent) => {
            if (type && event.type !== type) return;
            try {
              listener(event);
            } catch (error) {
              logTo(rec, `事件 ${event.type} 的回调`, error);
            }
          };
          const off = options.events ? options.events(wrapped) : () => {};
          rec.offs.push(off);
          publish();
          return () => {
            const at = rec.offs.indexOf(off);
            if (at >= 0) rec.offs.splice(at, 1);
            off();
          };
        },
      },
      tools: {
        call: (tool, input) => callTool(rec, tool, input ?? null),
      },
    };
  }

  /**
   * The result of one call, awaited through the event stream with a poll as the fallback. A 404
   * means the hub forgot the call (it was restarted): that is reported, never polled forever.
   */
  function awaitResult(callId: string): { promise: Promise<ExtensionCallResponse>; cancel(): void } {
    let settle!: (response: ExtensionCallResponse) => void;
    let fail!: (error: unknown) => void;
    let unsub: () => void = () => {};
    const lost = () => new ExtensionCallError("failed", "调用结果已丢失（hub 可能重启过）；这次调用不会重试");
    const statusOf = (error: unknown) => (error as { status?: unknown } | null)?.status;
    const promise = new Promise<ExtensionCallResponse>((resolve, reject) => {
      let done = false;
      let timer: ReturnType<typeof setTimeout> | undefined;
      const stop = () => {
        if (timer) clearTimeout(timer);
        unsub();
      };
      const finish = (response: ExtensionCallResponse) => {
        if (done) return;
        done = true;
        stop();
        resolve(response);
      };
      const error = (reason: unknown) => {
        if (done) return;
        done = true;
        stop();
        reject(reason);
      };
      settle = finish;
      fail = error;
      unsub = subscribeFinished(callId, () => {
        options.getCall(callId).then(finish, (reason) => {
          if (statusOf(reason) === 404) error(lost());
        });
      });
      const check = () => {
        options.getCall(callId).then(
          (response) => {
            if (response.status === "running") timer = setTimeout(check, pollMs);
            else finish(response);
          },
          (reason) => {
            // Only "gone" ends the wait; a dropped connection is retried.
            if (statusOf(reason) === 404) error(lost());
            else timer = setTimeout(check, pollMs);
          },
        );
      };
      check();
    });
    return {
      promise,
      cancel: () => {
        void options.cancelCall(callId).then(
          (response) => settle(response as ExtensionCallResponse),
          () => fail(new ExtensionCallError("cancelled", "调用已取消")),
        );
      },
    };
  }

  function subscribeFinished(callId: string, listener: () => void): () => void {
    if (finished.has(callId)) {
      listener();
      return () => {};
    }
    let set = waiters.get(callId);
    if (!set) {
      set = new Set();
      waiters.set(callId, set);
    }
    set.add(listener);
    return () => {
      set?.delete(listener);
      if (set && set.size === 0) waiters.delete(callId);
    };
  }

  function callTool(rec: Rec, tool: string, input: JsonValue): CancellablePromise<JsonValue> {
    // Only this extension's own tools, by convention (docs §3): never another extension's.
    const bare = tool.startsWith(`${rec.name}__`) ? tool.slice(rec.name.length + 2) : tool;
    let cancelled = false;
    let callId: string | undefined;
    let waiter: { cancel(): void } | undefined;

    const promise = (async () => {
      const accepted = await options.callTool(rec.name, bare, input);
      callId = accepted.call_id;
      if (cancelled) {
        await options.cancelCall(callId).catch(() => {});
        throw new ExtensionCallError("cancelled", "调用已取消");
      }
      const result = awaitResult(callId);
      waiter = result;
      const response = await result.promise;
      if (cancelled) throw new ExtensionCallError("cancelled", "调用已取消");
      if (response.status === "ok") return response.output ?? null;
      throw new ExtensionCallError(response.status, response.error ?? STATUS_TEXT[response.status], response.stderr);
    })() as CancellablePromise<JsonValue>;

    promise.cancel = () => {
      cancelled = true;
      if (callId) waiter?.cancel();
    };
    return promise;
  }

  function safeModeNow(): boolean | Promise<boolean> {
    return options.safeMode?.() ?? false;
  }

  async function load(rec: Rec, fingerprint: string, entry: string) {
    const url = extensionEntryUrl(rec.name, fingerprint, entry);
    rec.entryUrl = url;
    rec.live = true;
    rec.generation++;
    const generation = rec.generation;
    const loaded = await withTimeout(options.importModule(url), timeoutMs, `加载 ${rec.name} 的入口超时`);
    const module = (loaded ?? {}) as ExtensionModule;
    const activate = typeof module.activate === "function" ? module.activate : typeof module.default === "function" ? module.default : undefined;
    if (!activate) throw new Error("入口没有导出 activate(ca)");
    const ca = makeCa(rec, generation);
    // A sync throw becomes a rejection, so both paths are handled alike.
    const activation = Promise.resolve().then(() => activate(ca));
    activation.then(
      (own) => {
        if (typeof own !== "function") return;
        // Normally the load below is still current and keeps it for cleanup(). After a timeout
        // or a version switch the generation moved on: run it now instead of dropping it, or a
        // timer the extension opened would never be cleared.
        if (generation === rec.generation && rec.live) rec.cleanup = own as () => void;
        else runOwnCleanup(rec, own as () => void, "超时后返回的清理函数出错");
      },
      () => {
        // The timeout (or the error itself) is reported by the await below.
      },
    );
    await withTimeout(activation, timeoutMs, `${rec.name} 的 activate 超时`);
  }

  /** The entry `info` wants running, if any; `undefined` means disabled or headless. */
  function wantedOf(info: ExtensionInfo): { fingerprint: string; entry: string } | undefined {
    const version = info.enabled ? info.current : undefined;
    return version?.gui_entry ? { fingerprint: version.fingerprint, entry: version.gui_entry } : undefined;
  }

  /**
   * Whether `apply(info)` would import an entry, i.e. whether a refresh is really a load. A
   * hot reload is marked with `onLoading`/`onReady` too: if the new entry hangs, the next start
   * must come up in safe mode instead of hanging once more.
   */
  function needsImport(info: ExtensionInfo): boolean {
    const wanted = wantedOf(info);
    if (!wanted) return false;
    const rec = records.get(info.name);
    return !(rec && rec.status === "ready" && rec.fingerprint === wanted.fingerprint && rec.entry === wanted.entry);
  }

  async function apply(info: ExtensionInfo) {
    let rec = records.get(info.name);
    if (!rec) {
      rec = {
        name: info.name,
        status: "loading",
        live: false,
        generation: 0,
        tabs: [],
        cards: [],
        pages: [],
        settings: [],
        styles: [],
        offs: [],
        logs: [],
      };
      records.set(info.name, rec);
    }
    const wanted = wantedOf(info);

    if (!wanted) {
      const had = rec.live || rec.tabs.length > 0 || rec.cards.length > 0 || rec.offs.length > 0 || rec.styles.length > 0;
      if (had) cleanup(rec);
      rec.status = "disabled";
      rec.error = info.error;
      rec.fingerprint = undefined;
      rec.entry = undefined;
      publish();
      return;
    }

    // Already running this exact snapshot: nothing to do. A changed fingerprint means the old
    // registrations go first, then the new entry is imported.
    if (rec.status === "ready" && rec.fingerprint === wanted.fingerprint && rec.entry === wanted.entry) {
      if (rec.error !== info.error) {
        rec.error = info.error;
        publish();
      }
      return;
    }

    cleanup(rec);
    rec.status = "loading";
    rec.fingerprint = wanted.fingerprint;
    rec.entry = wanted.entry;
    rec.error = undefined;
    rec.entryUrl = extensionEntryUrl(rec.name, wanted.fingerprint, wanted.entry);
    publish();

    try {
      await load(rec, wanted.fingerprint, wanted.entry);
      rec.status = "ready";
    } catch (error) {
      cleanup(rec);
      rec.status = "failed";
      rec.error = textOf(error);
      logTo(rec, "加载失败", error);
    }
    publish();
  }

  /** Reads the hub's list and reconciles `enabled`/records; throws when the hub cannot answer. */
  async function readList(): Promise<ExtensionInfo[]> {
    const list = await options.list();
    listError = undefined;
    listed = true;
    enabled = new Set(list.filter((info) => info.enabled).map((info) => info.name));
    const names = new Set(list.map((info) => info.name));
    for (const [name, rec] of records) {
      if (names.has(name)) continue;
      cleanup(rec);
      records.delete(name);
    }
    return list;
  }

  async function applyList(list: ExtensionInfo[]) {
    for (const info of list) await apply(info);
    publish();
  }

  const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

  /**
   * Startup: keep reading until the hub answers once. It may start after the page, and
   * `extension.changed` only comes when something changes, so nothing else would retry.
   */
  async function readListUntilSuccess(): Promise<ExtensionInfo[] | undefined> {
    for (;;) {
      try {
        const list = await readList();
        retryDelay = retryMs;
        return list;
      } catch (error) {
        if (disposed) return undefined;
        listError = textOf(error);
        publish();
        await sleep(retryDelay);
        retryDelay = Math.min(retryDelay * 2, maxRetryMs);
        if (disposed) return undefined;
      }
    }
  }

  /** One retry of a failed re-read, later, without piling timers up. */
  function scheduleRetry() {
    if (disposed || retryTimer) return;
    const delay = retryDelay;
    retryDelay = Math.min(retryDelay * 2, maxRetryMs);
    retryTimer = setTimeout(() => {
      retryTimer = undefined;
      void refresh();
    }, delay);
    (retryTimer as unknown as { unref?: () => void }).unref?.();
  }

  async function doRefresh() {
    let list: ExtensionInfo[];
    try {
      list = await readList();
    } catch (error) {
      listError = textOf(error);
      publish();
      scheduleRetry();
      return;
    }
    retryDelay = retryMs;
    // Nothing new to import: no marker, exactly like a startup with no `gui_entry`.
    if (!list.some(needsImport)) {
      await applyList(list);
      return;
    }
    try {
      await options.onLoading?.();
    } catch (error) {
      if (options.log) options.log(`扩展加载开始上报失败：${textOf(error)}`);
      else console.error("扩展加载开始上报失败", error);
    }
    loading = true;
    publish();
    try {
      await applyList(list);
    } finally {
      loading = false;
      publish();
      // Also on a hot load: the marker goes away once this load settled, failures included.
      await reportReady(true);
    }
  }

  /** Reads the list again; runs are serialised so two `extension.changed` events cannot race. */
  function refresh(): Promise<void> {
    const run = chain.then(() => doRefresh());
    chain = run.catch(() => {});
    return run;
  }

  /** Awaits `onReady`; `force` is for a hot load, whose marker its own start did not clear. */
  async function reportReady(force = false) {
    if (readyReported && !force) return;
    readyReported = true;
    try {
      await options.onReady?.();
    } catch (error) {
      if (options.log) options.log(`扩展加载就绪上报失败：${textOf(error)}`);
      else console.error("扩展加载就绪上报失败", error);
    }
  }

  async function start(): Promise<void> {
    if (started) return starting ?? Promise.resolve();
    started = true;
    safeMode = (await safeModeNow()) ?? false;
    if (safeMode) {
      publish();
      // The shell clears the marker from the last run even when we skip loading this time.
      await reportReady();
      return;
    }
    starting = (async () => {
      // The list comes first: no marker when the hub is slow to answer (and none at all when
      // there is nothing with a `gui_entry` to load), but the marker is always cleared after.
      const list = await readListUntilSuccess();
      if (!list) return;
      if (list.some((info) => info.enabled && info.current?.gui_entry)) {
        try {
          await options.onLoading?.();
        } catch (error) {
          if (options.log) options.log(`扩展加载开始上报失败：${textOf(error)}`);
          else console.error("扩展加载开始上报失败", error);
        }
        loading = true;
        publish();
      }
      try {
        await applyList(list);
      } finally {
        loading = false;
        acceptingChanges = true;
        publish();
        await reportReady();
      }
    })();
    await starting;
  }

  async function disable(name: string): Promise<void> {
    const rec = records.get(name);
    try {
      await options.disable(name);
    } catch (error) {
      if (rec) logTo(rec, "停用失败", error);
      throw error;
    }
    await refresh();
  }

  options.events?.((event) => {
    if (event.type === "extension.changed") {
      // Not before startup settled: that would import ahead of the shell's loading marker.
      if (acceptingChanges) void refresh();
      return;
    }
    if (event.type !== "extension.call.finished") return;
    const callId = event.data.call_id;
    finished.add(callId);
    const set = waiters.get(callId);
    if (set) for (const listener of [...set]) listener();
    if (finished.size > 500) {
      const oldest = finished.values().next().value;
      if (oldest !== undefined) finished.delete(oldest);
    }
  });

  return {
    start,
    refresh,
    disable,
    reportError,
    snapshot: () => snapshot,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    /** Takes everything down (used when leaving the page in tests). */
    dispose() {
      disposed = true;
      if (retryTimer) {
        clearTimeout(retryTimer);
        retryTimer = undefined;
      }
      for (const rec of records.values()) cleanup(rec);
      records.clear();
      publish();
    },
  };
}

export type ExtensionHost = ReturnType<typeof createExtensionHost>;

/**
 * The tabs of extensions that finished loading. A tab registered during an `activate` that may
 * still throw is not one yet: placing it in a dock then would outlive the failed load, in the
 * saved layout too.
 */
export function readyTabs(snapshot: ExtensionsSnapshot): ExtTab[] {
  return [...snapshot.tabs.values()].filter((tab) => snapshot.records.get(tab.extension)?.status === "ready").map((tab) => tab.tab);
}

// ---- Safe mode and the shell's loading marker --------------------------------------------

const extensionsBridge = () => (typeof window === "undefined" ? undefined : window.crossagents?.extensions);

/** `--safe-mode` or a crash on the last run, as the shell decides it; nothing loads then. */
export async function detectSafeMode(): Promise<boolean> {
  const extensions = extensionsBridge();
  if (!extensions) return false;
  try {
    return (await extensions.safeMode()).enabled;
  } catch {
    // The shell could not say: loading is the normal path.
    return false;
  }
}

/** Tells the shell the page reached loading / ready. `createExtensionHost` awaits both. */
export async function reportLoading(phase: "loading" | "ready"): Promise<void> {
  const extensions = extensionsBridge();
  if (!extensions) return;
  await (phase === "loading" ? extensions.loading() : extensions.ready());
}
