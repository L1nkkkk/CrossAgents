// Mock data for the team page and the plugins it offers (preview:mock).

import type { PluginInfo } from "@bindings/PluginInfo";
import { capabilitiesOf } from "../dev/fixtures";

export const plugins = (): PluginInfo[] => [
  { name: "claude", models: ["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"], driver: "claude-native", capabilities: capabilitiesOf("claude") },
  { name: "codex", models: ["gpt-5-codex", "gpt-5", "gpt-5-mini"], driver: "codex-native", capabilities: capabilitiesOf("codex") },
  { name: "mock", models: ["mock-haiku", "mock-opus"], driver: "acp-generic", capabilities: capabilitiesOf("mock") },
];
