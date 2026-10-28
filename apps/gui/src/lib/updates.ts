import { useEffect, useState } from "react";
import type { UpdateState } from "../bridge";

/**
 * Where the GUI's own update stands, kept current (electron/updates.cts). `null` where nothing
 * updates (a development build, the page in a browser); `undefined` until the first answer.
 */
export function useUpdateState(): [UpdateState | null | undefined, (state: UpdateState | null) => void] {
  const bridge = window.crossagents?.updates;
  const [state, setState] = useState<UpdateState | null>();
  useEffect(() => {
    if (!bridge) return setState(null);
    void bridge.get().then(setState, () => setState(null));
    return bridge.onState(setState);
  }, [bridge]);
  return [state, setState];
}
