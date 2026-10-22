// Model and reasoning effort as chips in a conversation's composer: switch either between
// turns, like Claude Code's /model and effort picker. The vendor stays (a session belongs to
// one CLI); the change applies from the next turn.

import { useState } from "react";
import type { Agent } from "@bindings/Agent";
import { api, errorText } from "./api";
import { Icon, VendorMark } from "./components";
import { useHubData } from "./hub";
import { effortLevels, effortOf, prettyModel } from "./lib/models";
import { ChipMenu, type MenuItem } from "./menu";

export function ModelEffortPicks({ agent }: { agent: Agent }) {
  const plugins = useHubData(() => api.listPlugins().then((r) => r.plugins), [], () => false);
  const [error, setError] = useState<string>();
  const models = plugins.data?.find((p) => p.name === agent.plugin)?.models ?? [];
  const levels = effortLevels(agent.plugin);
  const effort = effortOf(agent.config);
  const label = levels.find((l) => l.value === effort)?.label;

  const apply = (model: string | null, next?: string) =>
    api.setModel(agent.id, model, next).then(
      () => setError(undefined),
      (err) => setError(errorText(err)),
    );

  const modelItems: MenuItem[] = [
    ...(agent.model && !models.includes(agent.model) ? [agent.model] : []).concat(models).map(
      (m): MenuItem => ({
        kind: "item",
        key: m,
        label: prettyModel(m),
        sub: m,
        icon: <VendorMark plugin={agent.plugin} size={14} />,
        on: agent.model === m,
        onPick: () => void apply(m),
      }),
    ),
    { kind: "item", key: "", label: "插件默认", sub: "由 CLI 自己决定", on: !agent.model, onPick: () => void apply(null) },
  ];

  const effortItems: MenuItem[] = [
    { kind: "header", key: "h", label: "推理强度 · 下一轮起生效" },
    { kind: "item", key: "", label: "默认", sub: "模型自己的默认", on: !effort, onPick: () => void apply(agent.model, "") },
    ...levels.map(
      (l): MenuItem => ({
        kind: "item",
        key: l.value,
        label: l.label,
        sub: l.value,
        on: effort === l.value,
        onPick: () => void apply(agent.model, l.value),
      }),
    ),
  ];

  return (
    <>
      <ChipMenu
        icon={<VendorMark plugin={agent.plugin} size={13} />}
        label={agent.model ? prettyModel(agent.model) : "插件默认"}
        title={`模型：${agent.model ?? "插件默认"} · 下一轮起生效`}
        items={modelItems}
        up
      />
      {levels.length > 0 && (
        <ChipMenu icon={<Icon name="brain" size={13} />} label={label ? `推理 ${label}` : "推理 默认"} title="推理强度" items={effortItems} up />
      )}
      {error && (
        <span className="chip danger" title={error}>
          切换失败
        </span>
      )}
    </>
  );
}
