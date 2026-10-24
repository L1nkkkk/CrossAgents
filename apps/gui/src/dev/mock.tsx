// `npm run preview:mock` (docs/tasks/V6-GUI.md §3): installs MockBridge as `window.crossagents`
// before the app starts, so every request and the event stream are answered from a scenario;
// adds the scenario panel. Only main.tsx imports this, behind `__MOCK__`: other builds leave it
// out entirely.
//
// URL: `?scenario=<key>` picks the scenario (else the last one); `&narrow=1` shows the app in a
// 900px frame; `&embed=1` is that frame (no panel of its own).

import { createRoot } from "react-dom/client";
import { startTheme } from "../lib/theme";
import { MockBridge } from "./MockBridge";
import { MockPanel } from "./MockPanel";
import { DEFAULT_SCENARIO, SCENARIOS } from "./scenarios";
import "./mock.css";

const LAST = "crossagents.mock.scenario";
/** What the app keeps between page loads that a fresh scenario must not inherit. */
const KEPT = ["crossagents.theme", "crossagents.panes", LAST];
const NARROW_WIDTH = 900;

/** Switches scenario (or replays the current one): a fresh page with a fresh state. */
function go(scenario: string, narrow: boolean) {
  try {
    localStorage.setItem(LAST, scenario);
    for (const key of Object.keys(localStorage)) if (key.startsWith("crossagents.") && !KEPT.includes(key)) localStorage.removeItem(key);
  } catch {
    // Not remembered; the URL still says.
  }
  // No route here: ids are made up on every build, so the page takes the route from its own.
  window.location.assign(`${window.location.pathname}?scenario=${scenario}${narrow ? "&narrow=1" : ""}`);
}

function panel(current: string, narrow: boolean) {
  const host = document.createElement("div");
  host.id = "mock-panel";
  document.body.appendChild(host);
  createRoot(host).render(<MockPanel current={current} narrow={narrow} go={go} />);
}

/** Returns whether the app should start on this page (not in the narrow frame's outer page). */
export function install(): boolean {
  const params = new URLSearchParams(window.location.search);
  const embedded = params.get("embed") === "1";
  let saved: string | null = null;
  try {
    saved = localStorage.getItem(LAST);
  } catch {
    // Use the default.
  }
  const key = params.get("scenario") ?? saved ?? DEFAULT_SCENARIO;
  const scenario = SCENARIOS.find((s) => s.key === key) ?? SCENARIOS.find((s) => s.key === DEFAULT_SCENARIO)!;
  const narrow = !embedded && params.get("narrow") === "1";

  if (narrow) {
    // The app in a 900px frame, so its media queries see a narrow window.
    startTheme();
    const root = document.getElementById("root")!;
    root.className = "mock-stage";
    const frame = document.createElement("iframe");
    frame.title = "CrossAgents（900px）";
    frame.src = `${window.location.pathname}?scenario=${scenario.key}&embed=1${window.location.hash}`;
    frame.style.width = `${NARROW_WIDTH}px`;
    root.appendChild(frame);
    panel(scenario.key, true);
    return false;
  }

  const built = scenario.build();
  window.crossagents = new MockBridge(built.state, built.script);
  if (!window.location.hash) window.location.hash = built.route;
  if (!embedded) panel(scenario.key, false);
  return true;
}
