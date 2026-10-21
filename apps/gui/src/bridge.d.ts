// What the Electron preload (electron/preload.cts) exposes as `window.crossagents`. The
// main process does all hub I/O; the token never reaches the page.

export type HubResponse = { status: number; body: string };

export interface CrossAgentsBridge {
  /** Forwards one API request to the hub. Rejects when the hub cannot be reached. */
  request(method: string, path: string, body: string | null): Promise<HubResponse>;
  /** (Re)starts relaying `GET /v1/events` from the beginning of history. */
  startEvents(): void;
  /** Raw `data` of each SSE message. Returns an unsubscribe function. */
  onEvent(listener: (data: string) => void): () => void;
  /** "connecting", "connected" or "offline: <reason>". Returns an unsubscribe function. */
  onStatus(listener: (status: string) => void): () => void;
}

declare global {
  interface Window {
    crossagents?: CrossAgentsBridge;
  }
}
