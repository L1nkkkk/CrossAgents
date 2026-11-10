// What an extension puts on the page outside its own tabs: a whole page of its own (`ca.pages`,
// reached from the sidebar). Both it and the settings blocks (`ca.settings`, in panels.ts) are the
// extension's code running inside this page, so both sit behind the boundary that keeps a throw
// to that block. The title lives inside the panel, not in a topbar of its own: at any window
// width the panel is what the person is looking at.

import { extensionHost, useExtensions } from "../defaults";
import type { ExtensionsSnapshot } from "../host";
import { RegisteredBody, type HandlersFor } from "./panels.ts";
import { explainedText } from "./model.ts";

/** How a block an extension registered reports a throw: its own log, and a 停用 that works. */
export const handlersFor: HandlersFor = (extension, label) => ({
  label,
  onDisable: () => {
    void extensionHost.disable(extension).catch(() => {
      // The host logs the hub's reason; the card stays up so the person can try again.
    });
  },
  onError: (error) => extensionHost.reportError(extension, error, label ? `渲染「${label}」出错` : "渲染出错"),
});

/** Why a registered page is not on screen, in the page's words. */
function missingPageText(extension: string, snapshot: ExtensionsSnapshot): string {
  if (snapshot.safeMode) return `安全模式：扩展 ${extension} 没有加载，它注册的页面也不在。退出安全模式后再打开这一页。`;
  if (snapshot.error) return `读取扩展列表失败：${snapshot.error}`;
  const view = snapshot.records.get(extension);
  if (!view) return snapshot.listed ? `扩展 ${extension} 不在 hub 的列表里：可能被删掉了。` : "正在等 hub 的扩展列表…";
  if (view.status === "loading") return `扩展 ${extension} 正在加载…`;
  if (view.status === "failed") return `扩展 ${extension} 加载失败：${view.error ? explainedText(view.error) : "原因未知"}`;
  if (view.status === "disabled") {
    // "disabled" here is the host's word for "nothing to load", which also covers an enabled
    // extension whose version has no panel entry: only the hub's own list says which it is.
    return snapshot.enabled.has(extension)
      ? `扩展 ${extension} 现在这个版本没有面板入口，所以它注册的页面不在。`
      : `扩展 ${extension} 现在没有启用，它注册的页面也收了。在「扩展」页里启用后再来。`;
  }
  return `扩展 ${extension} 没有注册这个页面。`;
}

export function ExtensionPageView({ extension, id }: { extension: string; id: string }) {
  const snapshot = useExtensions();
  const registration = snapshot.pages.get(`${extension}/${id}`);
  return (
    <div className="page">
      <div className="page-inner ext-host">
        <section className="ext-panel" data-ext-panel={extension}>
          <header className="ext-panel-head">
            <span className="ext-panel-title">{registration?.label ?? id}</span>
            <span className="muted small">扩展 {extension}</span>
            <span className="spacer" />
            <a className="ext-link small" href="#/extensions">
              管理扩展
            </a>
          </header>
          <div className="ext-panel-body">
            {registration ? (
              <RegisteredBody registration={registration} handlersFor={handlersFor} />
            ) : (
              <p className="muted small">{missingPageText(extension, snapshot)}</p>
            )}
          </div>
        </section>
      </div>
    </div>
  );
}
