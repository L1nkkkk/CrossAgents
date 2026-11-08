// The window's `bridge.connection` state, for the pages and the sidebar's mark. Nothing in the
// page can change it directly: pairing and switching go through the main process, which reloads
// the window; `onState` carries the tunnel's ups and downs in between.

import { useEffect, useState } from "react";
import type { ConnectionState } from "../bridge";
import { getBridge } from "../api";

/** The connection state, or `undefined` where the bridge has none (a plain browser). */
export function useConnectionState(): ConnectionState | undefined {
  const bridge = getBridge()?.connection;
  const [state, setState] = useState<ConnectionState>();
  useEffect(() => {
    if (!bridge) return;
    let live = true;
    bridge.get().then(
      (next) => live && setState(next),
      () => {},
    );
    const off = bridge.onState((next) => setState(next));
    return () => {
      live = false;
      off();
    };
  }, [bridge]);
  return state;
}

/** Whether this window has a connection bridge at all: a desktop build, not a browser tab. */
export function canConnect(): boolean {
  return !!getBridge()?.connection;
}
