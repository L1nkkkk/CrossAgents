// The running extension host, wired to the hub and the shell. Kept apart from host.ts so the
// host's logic can be unit tested without the page's API client or event stream.

import * as React from "react";
import { api, hubRequest } from "../api";
import { onHubEvent } from "../hub";
import { createExtensionHost, detectSafeMode, reportLoading, type ExtensionHost, type ExtensionsSnapshot } from "./host.ts";

export const extensionHost: ExtensionHost = createExtensionHost({
  list: async () => (await api.listExtensions()).extensions,
  importModule: (url) => import(/* @vite-ignore */ url),
  request: (method, path, body) => hubRequest(method, path, body),
  callTool: (extension, tool, input) => api.extensionCall(extension, tool, { input }),
  getCall: (id) => api.extensionCallResult(id),
  cancelCall: (id) => api.cancelExtensionCall(id),
  disable: (name) => api.disableExtension(name),
  events: onHubEvent,
  safeMode: detectSafeMode,
  onLoading: () => reportLoading("loading"),
  onReady: () => reportLoading("ready"),
});

export function startExtensions(): Promise<void> {
  return extensionHost.start();
}

export const useExtensions = (): ExtensionsSnapshot =>
  React.useSyncExternalStore(extensionHost.subscribe, extensionHost.snapshot);

export const useExtensionTabs = () => useExtensions().tabs;

export const useToolCards = () => useExtensions().cards;
