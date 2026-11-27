// System notices: a turn that ends while the window is not in front gets a desktop toast.
// The rule is one line — say it, and let the desktop backend drop it when the window has focus
// (src-tauri/src/desktop.rs). So switching to another app and back does not lose the fact that
// something finished, and sitting in the window does not produce toasts.

import { useEffect, useRef } from "react";
import { agentName, sessionHref, useAgents } from "./components";
import { onHubEvent } from "./hub";

/**
 * Says when a turn ends, and opens the conversation the person clicks. Mounted once, by `App`.
 */
export function useSystemNotices() {
  const agents = useAgents();
  // Read at event time, so a name that arrives later is still the one shown.
  const names = useRef(agents.data);
  names.current = agents.data;

  useEffect(
    () =>
      onHubEvent((event) => {
        if (event.type !== "agent.turn_completed") return;
        const { id, stop_reason } = event.data;
        // The person cancelled it: they know, and they are why it stopped.
        if (stop_reason === "cancelled") return;
        const failed = stop_reason === "error" || stop_reason === "interrupted";
        window.crossagents?.notify?.({
          title: agentName(names.current, id),
          body: failed ? "这一轮出错了，需要你看一下" : "跑完了",
          agentId: id,
        });
      }),
    [],
  );

  // Clicking the toast brings the window back (main.cts) and opens that conversation here.
  useEffect(
    () =>
      window.crossagents?.onNoticeOpen?.((agentId) => {
        window.location.hash = sessionHref(agentId);
      }),
    [],
  );
}
