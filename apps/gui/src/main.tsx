import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { startActivity } from "./activity";
import { startEventLog } from "./eventlog";
import { startExtensionsUi } from "./extensions/wire";
import { startEvents } from "./hub";
import { startJobs } from "./jobs";
import { startChatProfileSync } from "./lib/chatProfile";
import { startTheme } from "./lib/theme";
import { installTauriBridge } from "./tauriBridge";
import "./fonts.css";
import "./styles.css";

// `npm run preview:mock`: answer from made-up data before anything talks to the hub. The
// branch (and the dynamic import with it) is dropped from other builds.
let start = true;
if (__MOCK__) {
  const mock = await import("./dev/mock");
  start = mock.install();
}

if (start) {
  installTauriBridge();
  startTheme();
  startEvents();
  startChatProfileSync();
  startActivity();
  startJobs();
  startEventLog();
  // Extensions load beside the app: a failure marks that one failed, the page keeps running.
  startExtensionsUi();

  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
