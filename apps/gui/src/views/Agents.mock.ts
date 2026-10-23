// Mock data for the team page and the plugins it offers (preview:mock).

import type { PluginInfo } from "@bindings/PluginInfo";
import { capabilitiesOf } from "../dev/fixtures";

export const plugins = (): PluginInfo[] => [
  { name: "claude", models: ["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"], all_models: ["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"], shown: null, models_pinned: false, driver: "claude-native", capabilities: capabilitiesOf("claude"), efforts: ["low", "medium", "high", "xhigh", "max"] },
  { name: "codex", models: ["gpt-5-codex", "gpt-5", "gpt-5-mini"], all_models: ["gpt-5-codex", "gpt-5", "gpt-5-mini"], shown: null, models_pinned: false, driver: "codex-native", capabilities: capabilitiesOf("codex"), efforts: ["low", "medium", "high"] },
  { name: "mock", models: ["mock-haiku", "mock-opus"], all_models: ["mock-haiku", "mock-opus"], shown: null, models_pinned: false, driver: "acp-generic", capabilities: capabilitiesOf("mock"), efforts: [] },
];
