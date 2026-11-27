// Model and reasoning effort in a conversation's composer: switch either between turns,
// with favorite models hoisted to the top and reasoning effort docked inside the model popup
// (plus Ctrl+Shift+M / Cmd+Shift+M to open the picker from anywhere in the session).

import { useEffect, useState } from "react";
import type { Agent } from "@bindings/Agent";
import { api, errorText } from "./api";
import { VendorMark } from "./components";
import { useNodePlugins } from "./nodes";
import { favoriteKey, toggleFavorite, useModelFavorites } from "./lib/modelFavorites";
import { accountOf, effortLabel, effortLevels, effortOf, prettyModel } from "./lib/models";
import { modelDisplay } from "./lib/modelDisplay";
import { cursorMode, reasoningModels, usesVariantReasoning } from "./lib/reasoning";
import { ReasoningPicks } from "./reasoningPicks";
import { useQuota } from "./views/Quota";
import { ChipMenu, type MenuItem } from "./menu";

export function ModelEffortPicks({ agent }: { agent: Agent }) {
  const plugins = useNodePlugins(agent.node);
  const [error, setError] = useState<string>();
  const [openSignal, setOpenSignal] = useState(0);
  const favorites = useModelFavorites();
  const quota = useQuota();
  const account = accountOf(agent);
  const display = (m: string) => modelDisplay(agent.plugin, m, quota.data?.accounts ?? [], account);
  const info = plugins.data?.find((p) => p.name === agent.plugin);
  const models = info?.models ?? [];
  const variantPlugin = usesVariantReasoning(agent.plugin);
  // A model the CLI reports levels for takes exactly those, which may be none at all; one it
  // does not name takes the plugin's list.
  const levels = effortLevels(info?.efforts_by_model?.[agent.model ?? ""] ?? info?.efforts);
  const effort = effortOf(agent.config);
  const activeEffort = effort && levels.some((l) => l.value === effort) ? effort : undefined;
  const label = activeEffort ? effortLabel(activeEffort) : undefined;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "m") {
        e.preventDefault();
        setOpenSignal((n) => n + 1);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const apply = (model: string | null, next?: string) =>
    api.setModel(agent.id, model, next).then(
      () => setError(undefined),
      (err) => setError(errorText(err)),
    );
  const pickModel = (m: string | null) => {
    const nextLevels = info?.efforts_by_model?.[m ?? ""] ?? info?.efforts ?? [];
    const clearEffort = effort && !nextLevels.includes(effort) ? "" : undefined;
    void apply(m, clearEffort);
  };

  const list = reasoningModels(agent.plugin, (agent.model && !models.includes(agent.model) ? [agent.model] : []).concat(models), agent.model);
  const toItem = (m: string, prefix = ""): MenuItem => {
    const fkey = favoriteKey(agent.plugin, m, account);
    return {
      kind: "item",
      key: `${prefix}${m}`,
      label: variantPlugin ? prettyModel(cursorMode(m).base) : display(m).label,
      sub: m,
      searchText: display(m).description ?? undefined,
      icon: <VendorMark plugin={agent.plugin} size={14} />,
      on: agent.model === m,
      starred: favorites.has(fkey),
      onToggleStar: () => toggleFavorite(fkey),
      onPick: () => pickModel(m),
    };
  };
  const favModels = list.filter((m) => favorites.has(favoriteKey(agent.plugin, m, account)));
  const modelItems: MenuItem[] = [
    ...(favModels.length > 0 ? [{ kind: "header", key: "h-fav", label: "常用模型" } satisfies MenuItem, ...favModels.map((m) => toItem(m, "fav/"))] : []),
    ...(favModels.length > 0 ? [{ kind: "header", key: "h-all", label: "全部模型" } satisfies MenuItem] : []),
    ...list.map((m) => toItem(m)),
    { kind: "item", key: "", label: "插件默认", sub: "由 CLI 自己决定", on: !agent.model, onPick: () => pickModel(null) },
  ];

  const effortFooter =
    levels.length > 0 ? (
      <>
        <span className="menu-footer-label">推理强度</span>
        <div className="effort-pills">
          <button type="button" className={`effort-pill ${!activeEffort ? "on" : ""}`} onClick={() => void apply(agent.model, "")}>
            默认
          </button>
          {levels.map((l) => (
            <button
              key={l.value}
              type="button"
              className={`effort-pill ${activeEffort === l.value ? "on" : ""}`}
              onClick={() => void apply(agent.model, l.value)}
            >
              {l.label}
            </button>
          ))}
        </div>
      </>
    ) : undefined;

  const modelLabel = agent.model
    ? variantPlugin
      ? prettyModel(cursorMode(agent.model).base)
      : display(agent.model).label
    : "插件默认";

  return (
    <>
      <ChipMenu
        icon={<VendorMark plugin={agent.plugin} size={13} />}
        label={label ? `${modelLabel} · ${label}` : modelLabel}
        title={`模型：${agent.model ?? "插件默认"}${label ? ` · 推理 ${label}` : ""} · 下一轮起生效（Ctrl+Shift+M）`}
        items={modelItems}
        searchable
        footer={effortFooter}
        openSignal={openSignal}
        up
      />
      {variantPlugin && <ReasoningPicks plugin={info} model={agent.model} variantsOnly onChange={(model, next) => void apply(model, next)} />}
      {error && (
        <span className="chip danger" title={error}>
          切换失败
        </span>
      )}
    </>
  );
}
