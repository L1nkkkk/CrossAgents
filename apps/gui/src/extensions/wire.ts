// Connects the extension host to the rest of the page: tab labels, and pruning extension tabs
// for extensions that are no longer enabled. Kept out of host.ts so the host can be tested
// without localStorage or the pane store.

import { offerExtTab, setExtTabLabels, syncExtensionTabs } from "../panes";
import { extensionHost } from "./defaults";
import { readyTabs } from "./host.ts";

/** Subscribes to the host, then starts loading; call once, before the app renders. */
export function startExtensionsUi(): () => void {
  const apply = () => {
    const snapshot = extensionHost.snapshot();
    setExtTabLabels([...snapshot.tabs.values()].map((tab) => ({ tab: tab.tab, label: tab.label })));
    // Safe mode and a list that has not arrived leave the docks alone: an extension tab may
    // still be valid, and there is no registered tab to place yet.
    if (!snapshot.listed || snapshot.safeMode) return;
    syncExtensionTabs(snapshot.enabled);
    // A registered tab with no dock is offered on the right; one the person closed is
    // remembered in localStorage and waits in a dock's + menu instead.
    for (const tab of readyTabs(snapshot)) offerExtTab(tab);
  };
  const off = extensionHost.subscribe(apply);
  void extensionHost.start();
  return off;
}
