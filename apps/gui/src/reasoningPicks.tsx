import type { PluginInfo } from "@bindings/PluginInfo";
import { Icon } from "./components";
import { ChipMenu, type MenuItem } from "./menu";
import { effortLabel, effortLevels, vendorLabel } from "./lib/models";
import { cursorModeLabel, cursorVariantsOrdered, usesVariantReasoning } from "./lib/reasoning";

/** Native effort is config; Cursor / Antigravity modes are real model IDs from the offered catalog. */
export function ReasoningPicks({ plugin, model, effort = "", variantsOnly = false, onChange }: {
  plugin?: PluginInfo; model: string | null; effort?: string; variantsOnly?: boolean;
  onChange: (model: string | null, effort: string) => void;
}) {
  if (!plugin) return null;
  if (usesVariantReasoning(plugin.name)) {
    if (model) {
      // Only IDs the CLI offered: each menu item is a real variant of this base model.
      const variants = cursorVariantsOrdered(plugin.models, model);
      if (variants.length > 1) {
        const items = variants.map((v): MenuItem => ({ kind: "item", key: v, label: cursorModeLabel(v), sub: v, on: v === model, onPick: () => onChange(v, "") }));
        return <ChipMenu icon={<Icon name="brain" size={13} />} title={`${vendorLabel(plugin.name)} 推理 · 仅列出 CLI 提供的模型变体`} label={`推理 ${cursorModeLabel(model)}`} items={items} up />;
      }
    }
    if (variantsOnly || plugin.name === "cursor") return null;
  }
  if (variantsOnly) return null;
  const levels = effortLevels(plugin.efforts_by_model?.[model ?? ""] ?? plugin.efforts);
  if (!levels.length) return null;
  const items: MenuItem[] = [{ kind: "item", key: "", label: "默认", on: !effort, onPick: () => onChange(model, "") },
    ...levels.map((l): MenuItem => ({ kind: "item", key: l.value, label: l.label, on: effort === l.value, onPick: () => onChange(model, l.value) }))];
  return <ChipMenu icon={<Icon name="brain" size={13} />} title="推理强度" label={`推理 ${effort ? effortLabel(effort) : "默认"}`} items={items} up />;
}
