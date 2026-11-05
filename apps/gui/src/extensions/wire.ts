// Connects the extension host to the rest of the page: tab labels, and pruning extension tabs
// for extensions that are no longer enabled. Kept out of host.ts so the host can be tested
// without localStorage or the pane store.

import { setExtTabLabels, syncExtensionTabs } from "../panes";
import { extensionHost } from "./defaults";

/** Subscribes to the host, then starts loading; call once, before the app renders. */
export function startExtensionsUi(): () => void {
  const apply = () => {
    const snapshot = extensionHost.snapshot();
    setExtTabLabels([...snapshot.tabs.values()].map((tab) => ({ tab: tab.tab, label: tab.label })));
    // Before the list arrives there is nothing to judge a saved `ext:` tab by: keep it.
    if (snapshot.listed) syncExtensionTabs(snapshot.enabled);
  };
  const off = extensionHost.subscribe(apply);
  void extensionHost.start();
  return off;
}
