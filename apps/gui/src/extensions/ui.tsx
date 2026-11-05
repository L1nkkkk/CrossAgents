// The extension host's UI: a registered tab as the tool column's body, and a registered tool
// card in a conversation. Both are isolated per extension (boundary.tsx): a throwing component
// shows its own failure card and offers 停用, while the rest of the page keeps rendering.

import { extensionOfTab, type ExtTab } from "./tabs";
import type { CardRegistration, ExtensionPanelContext } from "./host";
import { extensionHost, useExtensionTabs, useExtensions } from "./defaults";
import { ExtensionBoundary, safeElement, type ExtensionFailureHandlers } from "./boundary";
import { parseToolJson, type ToolCallItem } from "./toolcards";

function handlersFor(extension: string, label?: string): ExtensionFailureHandlers {
  return {
    label,
    onDisable: () => {
      void extensionHost.disable(extension).catch(() => {
        // The host logs the hub's reason; the card stays up so the person can try again.
      });
    },
    onError: (error) => extensionHost.reportError(extension, error, label ? `渲染「${label}」出错` : "渲染出错"),
  };
}

/** A tab an extension registered (`ext:<name>/<id>`), by that name. */
export function ExtensionTabBody({ tab, follow }: { tab: ExtTab; follow?: string }) {
  const tabs = useExtensionTabs();
  const snapshot = useExtensions();
  const registration = tabs.get(tab);
  if (registration) {
    const context: ExtensionPanelContext = { extension: registration.extension, follow };
    const handlers = handlersFor(registration.extension, registration.label);
    return (
      <ExtensionBoundary key={`${tab}@${registration.fingerprint}`} extension={registration.extension} handlers={handlers}>
        {safeElement(registration.extension, () => registration.render(context), handlers)}
      </ExtensionBoundary>
    );
  }
  const name = extensionOfTab(tab);
  const view = snapshot.records.get(name);
  const text = snapshot.safeMode
    ? `安全模式：扩展 ${name} 没有加载。`
    : !view && snapshot.error
      ? `读取扩展列表失败：${snapshot.error}`
      : !view || view.status === "loading"
        ? `扩展 ${name} 正在加载…`
        : view.status === "failed"
          ? `扩展 ${name} 加载失败：${view.error ?? "原因未知"}`
          : `扩展 ${name} 已停用。`;
  return <p className="muted small tool-blank">{text}</p>;
}

/** One tool call, drawn by the card its extension registered for it. */
export function ToolCardView({ card, call }: { card: CardRegistration; call: ToolCallItem }) {
  const context = {
    extension: card.extension,
    call,
    args: parseToolJson(call.detail?.arguments),
    result: parseToolJson(call.detail?.result) ?? call.output,
    error: call.detail?.error,
  };
  const handlers = handlersFor(card.extension, card.key);
  return (
    <ExtensionBoundary key={`${card.key}@${card.fingerprint}`} extension={card.extension} handlers={handlers}>
      {safeElement(card.extension, () => card.render(context), handlers)}
    </ExtensionBoundary>
  );
}
