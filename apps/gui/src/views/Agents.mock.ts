// Mock data for the team page and the plugins it offers (preview:mock).

import type { PluginInfo } from "@bindings/PluginInfo";
import { capabilitiesOf } from "../dev/fixtures";

const CLAUDE_DOCS = "https://docs.claude.com/en/docs/claude-code/setup";

export const plugins = (): PluginInfo[] => [
  {
    name: "claude",
    models: ["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"],
    all_models: ["claude-opus-4-1", "claude-sonnet-4-5", "claude-haiku-4-5"],
    shown: null,
    models_pinned: false,
    driver: "claude-native",
    capabilities: capabilitiesOf("claude"),
    efforts: ["low", "medium", "high", "xhigh", "max"],
    cli: {
      installed: true,
      path: "C:/Users/me/AppData/Local/Claude-Data/Claude/claude-code/2.1.304/build/claude.exe",
      source: "bundled",
      version: "2.1.304",
      install_hint: "powershell -NoProfile -ExecutionPolicy Bypass -Command \"irm https://claude.ai/install.ps1 | iex\"",
      docs: CLAUDE_DOCS,
      install_command: ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", "irm https://claude.ai/install.ps1 | iex"],
      can_upgrade: false,
      described_by: "built-in",
    },
  },
  // Installed, but the CLI did not answer `--version`: still installed, no version shown.
  {
    name: "codex",
    models: ["gpt-5-codex", "gpt-5", "gpt-5-mini"],
    all_models: ["gpt-5-codex", "gpt-5", "gpt-5-mini"],
    shown: null,
    models_pinned: false,
    driver: "codex-native",
    capabilities: capabilitiesOf("codex"),
    efforts: ["low", "medium", "high"],
    cli: {
      installed: true,
      path: "C:/Users/me/AppData/Local/Programs/nodejs/node-v24.21.0-win-x64/node_modules/@openai/codex/node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin/codex.exe",
      source: "path",
      install_hint: "npm install -g @openai/codex",
      docs: "https://github.com/openai/codex",
      installed_via: "npm",
      install_command: ["npm", "install", "-g", "@openai/codex"],
      upgrade_command: ["npm", "install", "-g", "@openai/codex@latest"],
      can_upgrade: true,
      described_by: "built-in",
    },
  },
  // Not installed: no models on offer, and the command that installs it.
  {
    name: "codebuddy",
    models: [],
    all_models: [],
    shown: null,
    models_pinned: false,
    driver: "codebuddy-native",
    capabilities: capabilitiesOf("codebuddy"),
    efforts: ["minimal", "low", "medium", "high", "xhigh", "max"],
    cli: {
      installed: false,
      install_hint: "npm install -g @tencent-ai/codebuddy-code",
      docs: "https://www.codebuddy.ai/",
      install_command: ["npm", "install", "-g", "@tencent-ai/codebuddy-code"],
      upgrade_command: ["npm", "install", "-g", "@tencent-ai/codebuddy-code@latest"],
      can_upgrade: false,
      described_by: "built-in",
    },
  },
  {
    name: "mock",
    models: ["mock-haiku", "mock-opus"],
    all_models: ["mock-haiku", "mock-opus"],
    shown: null,
    models_pinned: false,
    driver: "acp-generic",
    capabilities: capabilitiesOf("mock"),
    efforts: [],
  },
];
