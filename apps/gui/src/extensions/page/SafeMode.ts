// Safe mode, said out loud (docs/EXTENSIONS.md §5): why this window loaded no extension, and what
// to do about it. The shell decides it (electron/safeMode.cts) and is the only one that can say
// why, so the page asks the bridge and falls back to the host's own flag when there is no shell.

import { useEffect, useState } from "react";
import htm from "htm";
import { createElement } from "react";
import { getBridge } from "../../api";
import { Icon } from "../../components";
import { useExtensions } from "../defaults";

const html = htm.bind(createElement);

export type SafeModeState = { enabled: boolean; reason: string | null };

export function useSafeMode(): SafeModeState {
  const snapshot = useExtensions();
  const [state, setState] = useState<SafeModeState>();
  useEffect(() => {
    let live = true;
    const extensions = getBridge()?.extensions;
    const fallback = () => {
      if (live) setState({ enabled: snapshot.safeMode, reason: null });
    };
    if (!extensions) {
      fallback();
      return () => {
        live = false;
      };
    }
    extensions.safeMode().then((s) => {
      if (live) setState(s);
    }, fallback);
    return () => {
      live = false;
    };
  }, [snapshot.safeMode, snapshot.listed]);
  return state ?? { enabled: snapshot.safeMode, reason: null };
}

/** The banner the page shows while safe mode is on: the reason, and how out of it. */
export function SafeModeBanner({ state }: { state: SafeModeState }) {
  if (!state.enabled) return null;
  return html`<div className="ext-safe" role="status" data-safe-mode="on">
    <div className="ext-safe-head">
      <${Icon} name="info" size=${15} />
      <strong>安全模式：这次没有加载任何扩展</strong>
    </div>
    <p className="ext-safe-text">原因：${state.reason ?? "这个窗口在安全模式下启动，跳过了所有扩展。"}</p>
    <p className="ext-safe-text">
      退出：完全退出 CrossAgents 再重新打开（托盘菜单 → 退出）。如果又自动进到这里，说明上次加载扩展时页面没能就绪：在下面的列表里停用最近装的扩展，再重启。从命令行启动的话，去掉参数
      <code>--safe-mode</code>。
    </p>
    <p className="ext-safe-text muted">安全模式只影响这个窗口加载什么。hub 里的启用状态和待批准记录都还在，这一页照样能看、能改。</p>
  </div>`;
}
