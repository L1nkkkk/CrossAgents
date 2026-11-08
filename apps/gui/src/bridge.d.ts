// What the Electron preload (electron/preload.cts) exposes as `window.crossagents`. The
// main process does all hub I/O; the token never reaches the page.

export type HubResponse = { status: number; body: string };

/** A development build of the hub running where the GUI's own would (electron/localHub.cts). */
export type DevHub = { exe: string; running: string; shipped: string };

/** Where the GUI's own update stands (electron/updates.cts). Times are ISO strings. */
export type UpdateState =
  | { state: "manual"; current: string; reason: string }
  | { state: "idle"; current: string }
  | { state: "checking"; current: string }
  | { state: "latest"; current: string; checked_at: string }
  | { state: "downloading"; current: string; version: string; percent: number }
  | { state: "ready"; current: string; version: string }
  | { state: "error"; current: string; message: string; checked_at: string };

/**
 * A hub on another machine, reached over SSH (docs/MULTI-NODE.md §4.3). `host` may be an alias
 * of `~/.ssh/config`; `command` is the `crossagents` to run there when it is not found by itself.
 */
export type RemoteTarget = { host: string; user?: string; sshPort?: number; command?: string };
/** The SSH tunnel to the remote hub. `stopped`: it no longer retries (revoked, or re-pair needed). */
export type TunnelState = { status: "connecting" | "connected" | "offline"; localPort?: number; remotePort?: number; reason?: string; stopped?: boolean };
/** Local mode (this machine's hub) or remote mode (a paired hub elsewhere). Never carries the token. */
export type ConnectionState = {
  mode: "local" | "remote";
  /** Packaged: the hub executable is present; development: true. */
  bundledHub: boolean;
  remote?: { target: RemoteTarget; deviceName: string; deviceId: string; pairedAt: string };
  tunnel?: TunnelState;
  defaultDeviceName: string;
};
/** Pairing goes: find the hub there, `device add`, start the tunnel, check the token. */
export type PairStep = "locate" | "pair" | "tunnel" | "check";

export interface CrossAgentsBridge {
  /**
   * Which hub the GUI talks to (electron/remoteController.cts). Switching mode, pairing anew or
   * forgetting while remote reloads the window. Electron only.
   */
  connection?: {
    get(): Promise<ConnectionState>;
    /** Rejects with the reason (ssh's stderr as it is) when pairing fails. */
    pair(target: RemoteTarget, deviceName: string): Promise<ConnectionState>;
    use(mode: "local" | "remote"): Promise<ConnectionState>;
    reconnect(): Promise<ConnectionState>;
    /** Forgets the pairing here only; the device stays on the hub until revoked there. */
    forget(): Promise<ConnectionState>;
    onState(listener: (state: ConnectionState) => void): () => void;
    onPairStep(listener: (step: PairStep) => void): () => void;
  };
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
  /** The development hub running instead of the shipped one; null when it is ours (or remote). Packaged GUI only. */
  devHub?(): Promise<DevHub | null>;
  /** Stops the development hub and starts the shipped one. Rejects with the reason it could not (e.g. remote mode). */
  useShippedHub?(): Promise<void>;
  /** Asks for a folder in the system dialog; null when cancelled. Rejects in remote mode (the folder is on the hub's machine). */
  pickFolder?(): Promise<string | null>;
  /**
   * Opens a file of a scope (`scope` is its API prefix, e.g. `/v1/agents/Rex`; `path` is relative
   * to its root) with the system's default program. The main process first asks the hub's
   * `files/resolve` that the path is inside the workspace; rejects with the reason when not.
   */
  openPath?(scope: string, path: string): Promise<{ copy: boolean }>;
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
