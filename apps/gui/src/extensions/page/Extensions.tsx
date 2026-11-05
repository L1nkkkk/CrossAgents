// 扩展, as the person sees it (docs/EXTENSIONS.md §6, §7): every extension the hub knows with
// what state it is in, one page per extension for approving a waiting version, and the way to ask
// an agent for a new one. Nothing on this page enables an extension by itself: a new version is
// approved on the extension's own page, where its tools, instructions, panel and access are shown
// and the diff against the running one is read.
//
// The status chips are the states the design names — 已启用, 有更新, 待批准, 失败, and the
// safe-mode one — plus 自动信任更新 and 未启用, so nothing has to be guessed from an absence.
// The blocks themselves are in panels.ts, where the tests render them.

import { useState } from "react";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import { api, errorText } from "../../api";
import { ErrorBox } from "../../components";
import { useHubData } from "../../hub";
import * as draft from "../../lib/draft";
import { startNew } from "../../menubar";
import { useExtensions } from "../defaults";
import { handlersFor } from "./Host.tsx";
import { ExtensionRow, MakePlugin, SettingsBlocks } from "./panels.ts";
import { SafeModeBanner, useSafeMode } from "./SafeMode.ts";
import { enableState, pluginRequestDraft } from "./model.ts";

export function ExtensionsView() {
  const list = useHubData(() => api.listExtensions().then((r) => r.extensions), [], (e) => e.type === "extension.changed");
  const snapshot = useExtensions();
  const safe = useSafeMode();
  const [want, setWant] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState<string>();
  const rows = list.data ?? [];
  const blocks = [...snapshot.settings.values()];

  async function act(name: string, action: () => Promise<unknown>) {
    setBusy(name);
    setError(undefined);
    try {
      await action();
      list.reload();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy((current) => (current === name ? undefined : current));
    }
  }

  const enable = (info: ExtensionInfo) => {
    const state = enableState(info);
    if ("fingerprint" in state) void act(info.name, () => api.enableExtension(info.name, state.fingerprint));
  };
  const disable = (info: ExtensionInfo) => void act(info.name, () => api.disableExtension(info.name));

  /** A new conversation with the request already typed, so it can be read before it is sent. */
  const makePlugin = () => {
    const request = pluginRequestDraft(want);
    draft.write({ kind: "home" }, { text: request.text, title: request.title, held: [] });
    setWant("");
    startNew("chat");
  };

  return (
    <>
      <div className="topbar">
        <span className="title">扩展</span>
      </div>
      <div className="page">
        <div className="page-inner ext-page">
          <SafeModeBanner state={safe} />
          <p className="settings-lead">
            扩展是随手写的小工具、附加说明和面板：不编译、不重启，改完批一次就能用上。hub 只跑你批准过的版本；源目录变了，旧版本照旧跑，除非你在这里批准新的，或者为它打开了自动信任。
          </p>
          <MakePlugin want={want} onWant={setWant} onMake={makePlugin} />
          {error && <ErrorBox error={error} />}
          {list.error && <ErrorBox error={list.error} />}
          <section className="settings-section">
            <div className="settings-head">
              <h2>全部扩展</h2>
              <span className="chip small">{rows.length}</span>
              <span className="spacer" />
              <button className="ghost small" onClick={list.reload} disabled={list.loading}>
                {list.loading ? "读取中…" : "刷新"}
              </button>
            </div>
            {rows.map((info) => (
              <ExtensionRow
                key={info.name}
                info={info}
                page={snapshot.records.get(info.name)}
                safeMode={safe.enabled}
                busy={busy === info.name}
                onEnable={() => enable(info)}
                onDisable={() => disable(info)}
              />
            ))}
            {!rows.length && !list.loading && !list.error && <Empty />}
          </section>
          <SettingsBlocks blocks={blocks} handlersFor={handlersFor} />
        </div>
      </div>
    </>
  );
}

function Empty() {
  return (
    <div className="ext-empty">
      <p>还没有扩展。</p>
      <p className="muted small">
        每个扩展是 <span className="mono">~/.crossagents/extensions/&lt;名字&gt;/</span> 下的一个目录：
        <span className="mono">extension.toml</span> 写它能做什么，工具各是一个脚本，要面板就加 <span className="mono">gui/index.js</span>。写好之后 hub 每 2 秒看一次源目录，改动会作为「待批准」出现在这里。
      </p>
    </div>
  );
}
