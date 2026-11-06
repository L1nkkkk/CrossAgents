// What the Electron preload (electron/preload.cts) exposes as `window.crossagents`. The
// main process does all hub I/O; the token never reaches the page.

export type HubResponse = { status: number; body: string };

/** A development build of the hub running where the GUI's own would (electron/localHub.cts). */
export type DevHub = { exe: string; running: string; shipped: string };

/** Where the GUI's own update stands (electron/updates.cts). Times are ISO strings. */
export type UpdateState =
  | { state: "idle"; current: string }
  | { state: "checking"; current: string }
  | { state: "latest"; current: string; checked_at: string }
  | { state: "downloading"; current: string; version: string; percent: number }
  | { state: "ready"; current: string; version: string }
  | { state: "error"; current: string; message: string; checked_at: string };

export interface CrossAgentsBridge {
  /** Main-process recovery state and durable extension startup reporting (Electron only). */
  extensions?: {
    safeMode(): Promise<{ enabled: boolean; reason: string | null }>;
    /** Await before importing/activating any extension, so a crash is remembered. */
    loading(): Promise<void>;
    /** Report once startup extension loading has settled, including handled failures. */
    ready(): Promise<void>;
  };
  /** Forwards one API request to the hub. Rejects when the hub cannot be reached. */
  request(method: string, path: string, body: string | null): Promise<HubResponse>;
  /** (Re)starts relaying `GET /v1/events`: live events, resuming by id after a drop. */
  startEvents(): void;
  /** Raw `data` of each SSE message. Returns an unsubscribe function. */
  onEvent(listener: (data: string) => void): () => void;
  /** "connecting", "connected" or "offline: <reason>". Returns an unsubscribe function. */
  onStatus(listener: (status: string) => void): () => void;
  /**
   * (Re)starts relaying a terminal's output (`GET /v1/terminals/{id}/output`) as `connection`,
   * reconnecting after a drop. A plain browser page has no relay, and so no terminals.
   */
  terminalStart?(id: string, connection: string): void;
  terminalStop?(id: string): void;
  /**
   * Each frame's raw `data` (`TerminalFrame`), and the relay's own `{"type":"reconnecting"}`
   * and `{"type":"gone"}`. Returns an unsubscribe function.
   */
  onTerminal?(listener: (id: string, data: string) => void): () => void;
  /** The development hub running instead of the shipped one; null when it is ours. Packaged GUI only. */
  devHub?(): Promise<DevHub | null>;
  /** Stops the development hub and starts the shipped one. Rejects with the reason it could not. */
  useShippedHub?(): Promise<void>;
  /** Asks for a folder in the system dialog; null when cancelled. */
  pickFolder?(): Promise<string | null>;
  /**
   * Opens a file of a scope (`scope` is its API prefix, e.g. `/v1/agents/Rex`; `path` is relative
   * to its root) with the system's default program. The main process first asks the hub's
   * `files/resolve` that the path is inside the workspace; rejects with the reason when not.
   */
  openPath?(scope: string, path: string): Promise<void>;
  /** The page's theme changed: the window frame follows it. */
  setTheme?(theme: { dark: boolean; background: string; foreground: string }): void;
  /** The GUI's update, and checking or installing it. `null`: this build does not update itself. */
  updates?: {
    get(): Promise<UpdateState | null>;
    check(): Promise<UpdateState | null>;
    install(): void;
    onState(listener: (state: UpdateState) => void): () => void;
  };
  /** A window command from the page's menu (electron/main.cts `COMMANDS`). */
  command?(name: "zoomIn" | "zoomOut" | "zoomReset" | "reload" | "devtools" | "fullscreen" | "quit"): void;
}

declare global {
  interface Window {
    crossagents?: CrossAgentsBridge;
  }
}
