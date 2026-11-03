import type { PluginInfo } from "@bindings/PluginInfo";
import { Icon } from "./components";
import { ChipMenu, type MenuItem } from "./menu";
import { effortLabel, effortLevels } from "./lib/models";
import { cursorMode, cursorVariants, cursorWith } from "./lib/reasoning";

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
  const mode = cursorMode(model);
  const variants = cursorVariants(plugin.models, model);
  const modes = variants.map(cursorMode);
  const efforts = [...new Set(modes.map((m) => m.effort))];
  const menu = (title: string, label: string, values: { key: string; label: string; change: Parameters<typeof cursorWith>[2]; on: boolean }[]) => (
    <ChipMenu title={title} label={label} icon={<Icon name="brain" size={13} />} up items={values.map((v): MenuItem => {
      const next = cursorWith(plugin.models, model, v.change);
      return { kind: "item", key: v.key, label: v.label, sub: next, on: v.on, disabled: !next, onPick: () => next && onChange(next, "") };
    })} />
  );
  return <>
    {modes.some((m) => m.thinking) && menu("Cursor 思考模式 · 标准模式不保证关闭模型内部推理", mode.thinking ? "Thinking" : "标准模式", [
      { key: "standard", label: "标准模式（未标 Thinking）", change: { thinking: false }, on: !mode.thinking },
      { key: "thinking", label: "Thinking", change: { thinking: true }, on: mode.thinking },
    ])}
    {efforts.some(Boolean) && menu("Cursor 推理强度 · 仅列出可用模型变体", `推理 ${mode.effort ? effortLabel(mode.effort) : "默认"}`, efforts.map((e) => ({ key: e, label: e ? effortLabel(e) : "默认", change: { effort: e }, on: e === mode.effort })))}
    {modes.some((m) => m.fast !== mode.fast) && menu("Cursor Fast · 按 CLI 提供的模型变体切换", mode.fast ? "Fast 开" : "Fast 关", [
      { key: "off", label: "Fast 关", change: { fast: false }, on: !mode.fast },
      { key: "on", label: "Fast 开", change: { fast: true }, on: mode.fast },
    ])}
  </>;
}
