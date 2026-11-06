import type { PluginInfo } from "@bindings/PluginInfo";
import { Icon } from "./components";
import { ChipMenu, type MenuItem } from "./menu";
import { effortLabel, effortLevels } from "./lib/models";
import { cursorModeLabel, cursorVariantsOrdered } from "./lib/reasoning";

/** Native effort is config; Cursor modes are real model IDs from the offered catalog. */
export function ReasoningPicks({ plugin, model, effort = "", onChange }: {
  plugin?: PluginInfo; model: string | null; effort?: string;
  onChange: (model: string | null, effort: string) => void;
}) {
  if (!plugin) return null;
  if (plugin.name !== "cursor") {
    const levels = effortLevels(plugin.efforts);
    if (!levels.length) return null;
    const items: MenuItem[] = [{ kind: "item", key: "", label: "默认", on: !effort, onPick: () => onChange(model, "") },
      ...levels.map((l): MenuItem => ({ kind: "item", key: l.value, label: l.label, on: effort === l.value, onPick: () => onChange(model, l.value) }))];
    return <ChipMenu icon={<Icon name="brain" size={13} />} title="推理强度" label={`推理 ${effort ? effortLabel(effort) : "默认"}`} items={items} up />;
  }
  if (!model) return null;
  // Only IDs the CLI offered: each menu item is a real variant of this base model.
  const variants = cursorVariantsOrdered(plugin.models, model);
  if (variants.length <= 1) return null;
  const items = variants.map((v): MenuItem => ({ kind: "item", key: v, label: cursorModeLabel(v), sub: v, on: v === model, onPick: () => onChange(v, "") }));
  return <ChipMenu icon={<Icon name="brain" size={13} />} title="Cursor 推理 · 仅列出 CLI 提供的模型变体" label={`推理 ${cursorModeLabel(model)}`} items={items} up />;
}
