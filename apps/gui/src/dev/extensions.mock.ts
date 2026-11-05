// The extensions a preview shows (docs/EXTENSIONS.md §6): one with an update waiting, one that
// has never been approved, one that runs untouched, and one whose source directory the hub cannot
// snapshot. Enough to see every chip and to walk the approval flow to the end.

import type { ExtensionDiffResponse } from "@bindings/ExtensionDiffResponse";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionLogsResponse } from "@bindings/ExtensionLogsResponse";
import type { ExtensionPromptInfo } from "@bindings/ExtensionPromptInfo";
import type { ExtensionToolInfo } from "@bindings/ExtensionToolInfo";
import type { ExtensionVersion } from "@bindings/ExtensionVersion";

/** A 64-hex fingerprint, as the hub's snapshot hash is. */
const fp = (seed: string) => seed.repeat(64).slice(0, 64);

const HOME = "C:/Users/me/.crossagents/extensions";

function version(v: Partial<ExtensionVersion> & Pick<ExtensionVersion, "fingerprint" | "description">): ExtensionVersion {
  return { api: 1, access: "read", tools: [], prompts: [], files: 3, bytes: 1840, taken_at: "2026-11-05T09:12:00Z", ...v };
}

const tool = (v: Partial<ExtensionToolInfo> & Pick<ExtensionToolInfo, "name" | "description">): ExtensionToolInfo => ({
  qualified_name: `standup__${v.name}`,
  input_schema: { type: "object", properties: {} },
  command: ["node", `${v.name}.mjs`],
  groups: ["lead", "conversation"],
  timeout_secs: 60,
  ...v,
});

const lead: ExtensionPromptInfo = { file: "lead.md", groups: ["lead"], bytes: 612 };
const team: ExtensionPromptInfo = { file: "team.md", groups: ["lead", "conversation"], bytes: 288 };

export function extensionScene(): {
  extensions: ExtensionInfo[];
  extensionDiffs: Record<string, ExtensionDiffResponse>;
  extensionLogs: Record<string, ExtensionLogsResponse>;
} {
  const standupCurrent = version({
    fingerprint: fp("3f9a1c02"),
    description: "汇总今天各 run 的进展",
    access: "read",
    tools: [tool({ name: "report", description: "汇总今天的进展" })],
    prompts: [lead, team],
    service: { command: ["node", "server.mjs"] },
    files: 4,
    bytes: 2140,
    taken_at: "2026-11-04T18:02:11Z",
  });
  const standupPending = version({
    fingerprint: fp("8e41d7aa"),
    description: "汇总今天各 run 的进展，并为站会生成一份摘要",
    access: "write",
    tools: [
      tool({ name: "report", description: "汇总今天的进展" }),
      tool({ name: "digest", description: "把汇总写成一段可以直接念的摘要", groups: ["human"], timeout_secs: 120 }),
    ],
    prompts: [lead, team, { file: "standup.md", groups: ["lead"], bytes: 410 }],
    hooks: [{ event: "run.landed", command: ["node", "notify.mjs"], timeout_secs: 60 }],
    service: { command: ["node", "server.mjs"] },
    gui_entry: "gui/index.js",
    files: 6,
    bytes: 3118,
    taken_at: "2026-11-05T09:40:52Z",
  });
  const changelog = version({
    fingerprint: fp("b60c14e9"),
    description: "从提交里生成更新日志",
    tools: [tool({ name: "write_entry", description: "为这次发布写一条更新日志", groups: ["human"] })],
    prompts: [{ file: "notes.md", groups: ["lead"], bytes: 190 }],
    files: 3,
    bytes: 1420,
    taken_at: "2026-11-05T08:15:03Z",
  });
  const notes = version({
    fingerprint: fp("1d7f5c31"),
    description: "把对话里的决定记到 run 的笔记上",
    access: "write",
    tools: [tool({ name: "remember", description: "把一句话记成笔记", groups: ["human"] })],
    files: 2,
    bytes: 760,
    taken_at: "2026-11-03T14:30:20Z",
  });

  const extensions: ExtensionInfo[] = [
    {
      name: "standup",
      source: `${HOME}/standup`,
      enabled: true,
      auto_trust: false,
      current: standupCurrent,
      service: { state: "backoff", restarts: 2, last_exit: "exit code 1" },
      previous_fingerprint: undefined,
      pending: { version: standupPending, requested_at: "2026-11-05T09:40:55Z", requested_by: "Rex" },
    },
    {
      name: "changelog",
      source: `${HOME}/changelog`,
      enabled: false,
      auto_trust: false,
      pending: { version: changelog, requested_at: "2026-11-05T08:15:04Z" },
    },
    {
      name: "notes",
      source: `${HOME}/notes`,
      enabled: true,
      auto_trust: true,
      current: notes,
      previous_fingerprint: fp("77aa21bd"),
    },
    {
      name: "big-export",
      source: `${HOME}/big-export`,
      enabled: false,
      auto_trust: false,
      error: "extension exceeds 50 MB",
    },
  ];

  const extensionDiffs: Record<string, ExtensionDiffResponse> = {
    standup: {
      name: "standup",
      fingerprint: standupPending.fingerprint,
      against: standupCurrent.fingerprint,
      truncated: false,
      files: [
        { path: "extension.toml", change: "modified", added: 2, removed: 1, binary: false },
        { path: "report.mjs", change: "modified", added: 6, removed: 2, binary: false },
        { path: "digest.mjs", change: "added", added: 24, removed: 0, binary: false },
        { path: "standup.md", change: "added", added: 9, removed: 0, binary: false },
        { path: "gui/index.js", change: "added", added: 31, removed: 0, binary: false },
        { path: "gui/logo.png", change: "added", added: 0, removed: 0, binary: true },
      ],
      access: standupPending.access,
      access_changed: true,
      diff: [
        "diff --git a/extension.toml b/extension.toml",
        "--- a/extension.toml",
        "+++ b/extension.toml",
        "@@ -1,8 +1,9 @@",
        ' name = "standup"',
        " api = 1",
        '-description = "汇总今天各 run 的进展"',
        '-access = "read"',
        '+description = "汇总今天各 run 的进展，并为站会生成一份摘要"',
        '+access = "write"',
        "",
        " [[tools]]",
        ' name = "report"',
        " description = \"汇总今天的进展\"",
        " groups = [\"lead\", \"conversation\"]",
        " command = [\"node\", \"report.mjs\"]",
        "+",
        "+[[tools]]",
        "+name = \"digest\"",
        "+description = \"把汇总写成一段可以直接念的摘要\"",
        "+groups = [\"human\"]",
        "+command = [\"node\", \"digest.mjs\"]",
        "+timeout_secs = 120",
        "+",
        "+[gui]",
        "+entry = \"gui/index.js\"",
        "diff --git a/report.mjs b/report.mjs",
        "--- a/report.mjs",
        "+++ b/report.mjs",
        "@@ -12,9 +12,13 @@ const days = input.days ?? 1;",
        " const since = Date.now() - days * 86_400_000;",
        " const runs = await hub.get(`/v1/runs`);",
        "-const done = runs.filter((r) => r.status === \"completed\");",
        "-process.stdout.write(JSON.stringify({ runs: done.length }));",
        "+const done = runs.runs.filter((r) => r.status === \"completed\");",
        "+const lines = done.map((r) => `- ${r.title}：${r.summary ?? \"已完成\"}`);",
        "+process.stdout.write(JSON.stringify({ markdown: lines.join(\"\\\\n\") }));",
        "diff --git a/digest.mjs b/digest.mjs",
        "new file mode 100644",
        "--- /dev/null",
        "+++ b/digest.mjs",
        "@@ -0,0 +1,6 @@",
        "+export default async function run({ input }) {",
        "+  const text = input.text ?? \"\";",
        "+  return { spoken: text.replace(/^#+ /gm, \"\") };",
        "+}",
        "+",
        "+// 站会用的一段话",
      ].join("\n"),
    },
    changelog: {
      name: "changelog",
      fingerprint: changelog.fingerprint,
      truncated: false,
      files: [
        { path: "extension.toml", change: "added", added: 12, removed: 0, binary: false },
        { path: "write.mjs", change: "added", added: 18, removed: 0, binary: false },
        { path: "notes.md", change: "added", added: 6, removed: 0, binary: false },
      ],
      access: changelog.access,
      access_changed: false,
      diff: [
        "diff --git a/extension.toml b/extension.toml",
        "new file mode 100644",
        "--- /dev/null",
        "+++ b/extension.toml",
        "@@ -0,0 +1,12 @@",
        '+name = "changelog"',
        "+api = 1",
        '+description = "从提交里生成更新日志"',
        '+access = "read"',
        "+",
        "+[[tools]]",
        '+name = "write_entry"',
        '+description = "为这次发布写一条更新日志"',
        '+groups = ["human"]',
        '+command = ["node", "write.mjs"]',
      ].join("\n"),
    },
  };

  const standupLog = [
    "2026-11-05T09:31:02Z call report started (pid 24160)",
    "2026-11-05T09:31:02Z 读 /v1/runs：3 个 run，1 个已完成",
    "2026-11-05T09:31:03Z call report ok (1.2s)",
    "2026-11-05T09:38:44Z call report started (pid 27304)",
    "2026-11-05T09:38:45Z Error: hub 回答了 409：run R-0193 正在落地",
    "2026-11-05T09:38:45Z call report failed (0.8s, exit 1)",
  ].join("\n");

  const extensionLogs: Record<string, ExtensionLogsResponse> = {
    standup: { name: "standup", content: standupLog, truncated: false },
    notes: { name: "notes", content: "", truncated: false },
  };

  return { extensions, extensionDiffs, extensionLogs };
}
