// One extension's component, kept from taking the page down with it (docs/EXTENSIONS.md §5).
// Two layers, because React catches different failures at different times:
//
// - `safeElement` calls the registered render function itself; a throw there is recorded and
//   replaced by the failure card, while the siblings on the page still render.
// - `ExtensionBoundary` catches errors from children the render function returned (a nested
//   component or hook throwing later), showing the same card in that block only.
//
// Both say what happened and offer 停用: the person can stop the extension from the broken spot,
// rather than hunting for it in a list. Plain createElement, not JSX, so the unit tests can run
// this file without a build step.

import { Component, createElement, type ErrorInfo, type ReactNode } from "react";

export type ExtensionFailureHandlers = {
  /** Stops the extension (POST /v1/extensions/{name}/disable, then a reload). */
  onDisable: () => void;
  /** Records the error in the extension's log; the fallback shows it either way. */
  onError?: (error: unknown) => void;
  /** What the block was, as the person sees it (a tab's label, a card's tool name). */
  label?: string;
};

export function ExtensionFailure({ extension, error, handlers }: { extension: string; error: unknown; handlers: ExtensionFailureHandlers }) {
  const message = error instanceof Error ? (error.stack || error.message) : String(error);
  return createElement(
    "div",
    { className: "error ext-failure", role: "alert", style: { padding: "8px 10px" } },
    createElement("div", null, createElement("strong", null, `扩展 ${extension} 的${handlers.label ? `「${handlers.label}」` : "这部分"}出错了`)),
    createElement("pre", { className: "small", style: { whiteSpace: "pre-wrap", maxHeight: 180, overflow: "auto" } }, message),
    createElement(
      "button",
      { className: "small", onClick: handlers.onDisable, title: `停用扩展 ${extension}：它注册的面板、卡片和订阅都会被清理` },
      "停用",
    ),
  );
}

/**
 * Calls a registered render function, turning a throw into the failure card. Whatever the
 * function registered through `ca` is cleaned up by the host when the person disables it.
 */
export function safeElement(extension: string, render: () => ReactNode, handlers: ExtensionFailureHandlers): ReactNode {
  try {
    return render();
  } catch (error) {
    handlers.onError?.(error);
    return createElement(ExtensionFailure, { extension, error, handlers });
  }
}

type Props = {
  extension: string;
  handlers: ExtensionFailureHandlers;
  children: ReactNode;
};

type State = { error?: unknown };

/** Catches a throw from anything the registered component rendered. */
export class ExtensionBoundary extends Component<Props, State> {
  state: State = {};

  static getDerivedStateFromError(error: unknown): State {
    return { error };
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    this.props.handlers.onError?.(info.componentStack ? new Error(`${String(error)}\n${info.componentStack}`) : error);
  }

  render() {
    if (this.state.error !== undefined) {
      return createElement(ExtensionFailure, { extension: this.props.extension, error: this.state.error, handlers: this.props.handlers });
    }
    return this.props.children;
  }
}
