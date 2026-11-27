import type { ConnectionState, CrossAgentsBridge, HubResponse, NativeBrowserEvent, PairStep, UpdateState } from "./bridge";

type StreamFrame =
  | { type: "connected" }
  | { type: "event"; data: string; id: string };

type TauriCore = {
  invoke<T>(command: string, args: Record<string, unknown>): Promise<T>;
  Channel: new <T>(onmessage: (message: T) => void) => object;
};

/** Installs the Tauri implementation of the shared desktop bridge. */
export function installTauriBridge() {
  const tauri = (window as Window & { __TAURI__?: { core?: TauriCore } }).__TAURI__?.core;
  if (!tauri || window.crossagents) return;
  const { Channel, invoke } = tauri;

  const events = new Set<(data: string) => void>();
  const statuses = new Set<(status: string) => void>();
  const terminals = new Set<(id: string, data: string) => void>();
  const terminalStarts = new Map<string, Promise<void>>();
  const noticeListeners = new Set<(agent: string) => void>();
  const connectionListeners = new Set<(state: ConnectionState) => void>();
  const pairListeners = new Set<(step: PairStep) => void>();
  let connectionPoll: ReturnType<typeof setInterval> | undefined;
  let latestConnection = "";
  const emitConnection = (state: ConnectionState) => {
    const key = JSON.stringify(state);
    if (key !== latestConnection) {
      latestConnection = key;
      connectionListeners.forEach((listener) => listener(state));
    }
    return state;
  };
  const getConnection = () => invoke<ConnectionState>("connection_get", {}).then(emitConnection);
  const changeConnection = async (command: string, args: Record<string, unknown>, reload: boolean) => {
    const state = emitConnection(await invoke<ConnectionState>(command, args));
    if (reload) location.reload();
    return state;
  };
  const steps = () => new Channel<PairStep>((step) => pairListeners.forEach((listener) => listener(step)));
  const browserLinks = new Set<(url: string) => void>();
  const updateListeners = new Set<(state: UpdateState) => void>();
  const updateEvents = () => new Channel<UpdateState>((state) => updateListeners.forEach((listener) => listener(state)));
  const getUpdate = () => invoke<UpdateState>("updates_get", {});
  const checkUpdate = () => invoke<UpdateState>("updates_check", { events: updateEvents() });
  function browserAction<T>(id: number, action: Record<string, unknown>) {
    return invoke<T>("browser_action", { id, action });
  }
  let noticesRegistered = false;
  let started = false;
  let lastId: string | null = null;
  const setStatus = (status: string) => statuses.forEach((listener) => listener(status));

  async function relay() {
    for (;;) {
      setStatus("connecting");
      const frames = new Channel<StreamFrame>((frame) => {
        if (frame.type === "connected") {
          setStatus("connected");
        } else {
          if (frame.id) lastId = frame.id;
          events.forEach((listener) => listener(frame.data));
        }
      });
      try {
        await invoke("hub_events", { lastId, frames });
        setStatus("offline: event stream ended");
      } catch (error) {
        setStatus(`offline: ${String(error)}`);
      }
      await new Promise((resolve) => setTimeout(resolve, 2000));
    }
  }

  const bridge: CrossAgentsBridge = {
    updates: {
      get: getUpdate,
      check: checkUpdate,
      install() { void invoke("updates_install", { events: updateEvents() }).catch(console.warn); },
      onState(listener) {
        updateListeners.add(listener);
        return () => updateListeners.delete(listener);
      },
    },
    devHub: () => invoke("dev_hub", {}),
    useShippedHub: () => invoke("use_shipped_hub", {}),
    nativeBrowser: {
      create: (id, url, onEvent) => invoke("browser_create", {
        id, url, events: new Channel<NativeBrowserEvent>((event) => {
          if (event.kind === "open") browserLinks.forEach((listener) => listener(event.url));
          else onEvent(event);
        }),
      }),
      bounds: (id, bounds) => invoke("browser_bounds", { id, bounds }),
      navigate: (id, url) => invoke("browser_navigate", { id, url }),
      step: (id, action) => invoke("browser_step", { id, action }),
      style: (id, css) => invoke("browser_style", { id, css }),
      close: (id) => invoke("browser_close", { id }),
    },
    browser: {
      snapshot: (id, opts) => browserAction(id, {
        action: "snapshot",
        ...(opts?.selector ? { selector: opts.selector } : {}),
        ...(opts?.maxChars !== undefined ? { max_chars: opts.maxChars } : {}),
      }),
      click: (id, reference) => browserAction(id, { action: "click", reference }),
      type: (id, reference, text, submit = false, clear = true) => browserAction(id, { action: "type", reference, text, submit, clear }),
      select: (id, reference, value) => browserAction(id, { action: "select", reference, value }),
      press: (id, key) => browserAction(id, { action: "press", key }),
      scroll: (id, dy, reference) => browserAction(id, {
        action: "scroll",
        dy,
        ...(reference !== undefined ? { reference } : {}),
      }),
      probe: (id, cond) => browserAction(id, {
        action: "probe",
        ...(cond.text ? { text: cond.text } : {}),
        ...(cond.textGone ? { text_gone: cond.textGone } : {}),
        ...(cond.selector ? { selector: cond.selector } : {}),
        ...(cond.url ? { url: cond.url } : {}),
      }),
      screenshot: (id) => invoke("browser_screenshot", { id }),
      console: (id) => browserAction(id, { action: "console" }),
      fileUrl: (path) => invoke("browser_local_file_url", { path }),
      scopedFileUrl: (scope, path) => invoke("browser_file_url", { scope, path }),
      importSession: (id, file, key) => invoke("browser_import_session", { id, file, key }),
      bookmarks: () => invoke("browser_bookmarks", {}),
      openExternal: (url) => invoke("browser_open_external", { url }),
      chromeStatus: () => invoke("chrome_status", {}),
      openExtensionFolder: () => invoke("chrome_open_extension", {}),
      chromeActivate: (tab) => invoke("chrome_activate", { tab }),
      chromeAction: (input) => invoke("chrome_action", { input }),
      onOpenTab(listener) {
        browserLinks.add(listener);
        return () => browserLinks.delete(listener);
      },
    },
    tailscale: { status: () => invoke("tailscale_status", {}) },
    connection: {
      get: getConnection,
      pair: (target, deviceName) => changeConnection("connection_pair", { target, deviceName, steps: steps() }, true),
      pairCode: (code, deviceName) => changeConnection("connection_pair_code", { code, deviceName, steps: steps() }, true),
      use: (mode) => changeConnection("connection_use", { mode }, true),
      setShareNode: (enabled) => changeConnection("connection_set_share_node", { enabled }, false),
      reconnect: () => changeConnection("connection_reconnect", {}, false),
      forget: async () => changeConnection("connection_forget", {}, (await getConnection()).mode === "remote"),
      importPairing: (file, key) => changeConnection("connection_import_pairing", { file, key }, true),
      onState(listener) {
        connectionListeners.add(listener);
        connectionPoll ??= setInterval(() => { void getConnection().catch(console.warn); }, 2000);
        return () => {
          connectionListeners.delete(listener);
          if (connectionListeners.size === 0) {
            clearInterval(connectionPoll);
            connectionPoll = undefined;
          }
        };
      },
      onPairStep(listener) {
        pairListeners.add(listener);
        return () => pairListeners.delete(listener);
      },
    },
    extensions: {
      assetBase: location.protocol === "http:" ? "http://crossagents-ext.localhost" : "crossagents-ext://localhost",
      safeMode: () => invoke("extension_safe_mode", {}),
      loading: () => invoke("extension_loading", {}),
      ready: () => invoke("extension_ready", {}),
    },
    request: (method, path, body) => invoke<HubResponse>("hub_request", { method, path, body }),
    startEvents() {
      if (started) return;
      started = true;
      void relay();
    },
    onEvent(listener) {
      events.add(listener);
      return () => events.delete(listener);
    },
    onStatus(listener) {
      statuses.add(listener);
      return () => statuses.delete(listener);
    },
    pickFolder: () => invoke<string | null>("pick_folder", {}),
    openPath: (scope, path) => invoke<{ copy: boolean }>("open_path", { scope, path }),
    writeClipboard: (text) => invoke<void>("write_clipboard", { text }),
    setTheme: ({ dark }) => { void invoke("set_theme", { dark }).catch(console.warn); },
    command: (action) => { void invoke("window_command", { action }).catch(console.warn); },
    notify: (notice) => { void invoke("notify", { notice }).catch(console.warn); },
    onNoticeOpen(listener) {
      noticeListeners.add(listener);
      if (!noticesRegistered) {
        noticesRegistered = true;
        const channel = new Channel<string>((id) => noticeListeners.forEach((open) => open(id)));
        void invoke("listen_for_notice", { channel }).catch((error: unknown) => {
          noticesRegistered = false;
          console.warn(error);
        });
      }
      return () => noticeListeners.delete(listener);
    },
    terminalStart(id, connection) {
      const emit = (data: string) => terminals.forEach((listener) => listener(id, data));
      const frames = new Channel<string>(emit);
      const previous = terminalStarts.get(id) ?? Promise.resolve();
      const start = previous.then(() => invoke<void>("terminal_start", { id, connection, frames }))
        .catch(() => emit('{"type":"gone"}'));
      terminalStarts.set(id, start);
    },
    terminalStop(id) {
      const previous = terminalStarts.get(id) ?? Promise.resolve();
      const stop = previous.then(() => invoke<void>("terminal_stop", { id })).catch(() => undefined);
      terminalStarts.set(id, stop);
      void stop.then(() => {
        if (terminalStarts.get(id) === stop) terminalStarts.delete(id);
      });
    },
    onTerminal(listener) {
      terminals.add(listener);
      return () => terminals.delete(listener);
    },
  };
  window.crossagents = bridge;
  void getUpdate().then((state) => {
    if (state.state !== "manual") {
      setTimeout(() => void checkUpdate().catch(console.warn), 20_000);
      setInterval(() => void checkUpdate().catch(console.warn), 4 * 60 * 60 * 1000);
    }
  }).catch(console.warn);
}
