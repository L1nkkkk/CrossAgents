// The extension page's tests, without a browser: what state each extension reads as, that the
// approval screen submits the fingerprint it shows (never one it re-read), what it says when that
// version is replaced while the screen is open, and that an extension's own page or settings block
// fails alone (docs/EXTENSIONS.md §5, §6).

import assert from "node:assert/strict";
import { test } from "node:test";
import { createElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { ExtensionDiffResponse } from "@bindings/ExtensionDiffResponse";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionVersion } from "@bindings/ExtensionVersion";
import { ApprovalPanel, ExtensionRow, SettingsBlocks } from "./page/panels.ts";
import {
  AUTO_TRUST_HINT,
  accessText,
  approvalView,
  approveBlockedReason,
  approveFingerprint,
  autoTrustBlockedReason,
  badges,
  enableState,
  pluginRequestDraft,
  seenAfter,
  shortFingerprint,
  versionFacts,
} from "./page/model.ts";

const FP_OLD = "3f9a1c02".repeat(8);
const FP_NEW = "8e41d7aa".repeat(8);
const FP_NEWER = "b60c14e9".repeat(8);

function version(fingerprint: string, over: Partial<ExtensionVersion> = {}): ExtensionVersion {
  return {
    fingerprint,
    api: 1,
    description: "汇总今天各 run 的进展",
    access: "read",
    tools: [
      { name: "report", qualified_name: "standup__report", description: "汇总今天的进展", input_schema: { type: "object" }, groups: ["lead"], command: ["node", "report.mjs"], timeout_secs: 60 },
    ],
    hooks: [{ event: "run.landed", command: ["node", "notify.mjs"], timeout_secs: 60 }],
    service: { command: ["node", "server.mjs"] },
    prompts: [{ file: "lead.md", groups: ["lead"], bytes: 612 }],
    gui_entry: "gui/index.js",
    files: 4,
    bytes: 2140,
    taken_at: "2026-11-04T18:02:11Z",
    ...over,
  };
}

function info(over: Partial<ExtensionInfo> = {}): ExtensionInfo {
  return {
    name: "standup",
    source: "C:/Users/me/.crossagents/extensions/standup",
    enabled: true,
    auto_trust: false,
    current: version(FP_OLD),
    ...over,
  };
}

const pending = (fingerprint: string) => ({ version: version(fingerprint, { access: "write" }), requested_at: "2026-11-05T09:40:55Z", requested_by: "Rex" });

const diff: ExtensionDiffResponse = {
  name: "standup",
  fingerprint: FP_NEW,
  against: FP_OLD,
  diff: "diff --git a/extension.toml b/extension.toml\n-access = \"read\"\n+access = \"write\"\n",
  truncated: false,
  files: [
    { path: "extension.toml", change: "modified", added: 2, removed: 1, binary: false },
    { path: "gui/logo.png", change: "added", added: 0, removed: 0, binary: true },
  ],
  access: "write",
  access_changed: true,
};

const labels = (list: { label: string }[]) => list.map((b) => b.label);

test("each state the design names has a chip, and a problem does not hide 已启用", () => {
  assert.deepEqual(labels(badges(info(), { safeMode: false })), ["已启用"]);
  assert.deepEqual(labels(badges(info({ pending: pending(FP_NEW) }), { safeMode: false })), ["有更新", "已启用"]);
  assert.deepEqual(labels(badges(info({ enabled: false, current: undefined, pending: pending(FP_NEW) }), { safeMode: false })), ["待批准", "未启用"]);
  assert.deepEqual(labels(badges(info({ error: "extension exceeds 50 MB" }), { safeMode: false })), ["失败", "已启用"]);
  assert.deepEqual(labels(badges(info(), { safeMode: false, page: { status: "failed", error: "boom" } })), ["加载失败", "已启用"]);
  assert.deepEqual(labels(badges(info({ pending: pending(FP_NEW) }), { safeMode: true })), ["安全模式下未加载", "有更新", "已启用"]);
  assert.deepEqual(labels(badges(info({ auto_trust: true }), { safeMode: false })), ["自动信任更新", "已启用"]);
});

test("the approval screen submits exactly the fingerprint it shows", () => {
  const target = info({ pending: pending(FP_NEW) });
  const view = approvalView(target, FP_NEW);
  const html = renderToStaticMarkup(createElement(ApprovalPanel, { info: target, view, diff, busy: false, onApprove: () => {}, onLook: () => {} }));

  const shown = /data-fingerprint="([^"]+)"/.exec(html);
  assert.ok(shown, "the approval card marks the fingerprint it will submit");
  assert.equal(shown[1], FP_NEW);
  // The same one is on screen twice: the card marks it, and the sentence under the button prints it.
  assert.ok(html.includes(shortFingerprint(FP_NEW)), "the fingerprint is readable on the card");
  // And it is what the page hands the hub — not a version read again later.
  assert.equal(approveFingerprint(view), shown[1]);
  assert.equal(approveFingerprint(view), target.pending!.version.fingerprint);
  // The version's own facts, the diff and the access change are all on this one card.
  assert.ok(html.includes("standup__report") || html.includes("report"));
  assert.ok(html.includes("access 变了"));
  assert.ok(html.includes("extension.toml"));
  for (const value of ["report.mjs", "run.landed", "notify.mjs", "server.mjs"]) assert.ok(html.includes(value));
  assert.ok(html.includes("二进制"));
  assert.ok(html.includes("批准并启用"));
});

test("a pending record replaced while the card is open is not approved", () => {
  // Opened on FP_NEW; the hub has since recorded FP_NEWER.
  const target = info({ pending: pending(FP_NEWER) });
  const view = approvalView(target, FP_NEW);
  const html = renderToStaticMarkup(createElement(ApprovalPanel, { info: target, view, diff, busy: false, onApprove: () => {}, onLook: () => {} }));
  assert.equal(approveFingerprint(view), undefined);
  assert.ok(html.includes('data-approval="stale"'));
  assert.ok(html.includes("已经不在待批准里了"));
  assert.ok(!html.includes('data-action="approve"'), "the stale card offers no approve button");
  assert.ok(html.includes(shortFingerprint(FP_NEWER)), "it names the version that is waiting now");
  assert.match(approveBlockedReason(target, view) ?? "", /先看新的一版/);
});

test("a pending record that is gone says so instead of approving the next thing", () => {
  const target = info({ pending: undefined });
  const view = approvalView(target, FP_NEW);
  const html = renderToStaticMarkup(createElement(ApprovalPanel, { info: target, view, diff, busy: false, onApprove: () => {}, onLook: () => {} }));
  assert.equal(approveFingerprint(view), undefined);
  assert.ok(html.includes('data-approval="gone"'));
  assert.ok(html.includes("已经不在了"));
  assert.ok(!html.includes('data-action="approve"'));
});

test("a stopped extension can be re-enabled on its approved version while a new one waits", () => {
  // stopped, source changed, and the person only wants the version they already approved back:
  // the hub re-enables `current` without touching the pending record, so the page must offer it.
  const stopped = info({ enabled: false, current: version(FP_OLD), pending: pending(FP_NEW) });
  const state = enableState(stopped);
  assert.ok("fingerprint" in state, "with a version waiting, re-enabling the approved one is still offered");
  assert.equal(state.fingerprint, FP_OLD, "the fingerprint submitted is the approved one, not the waiting one");
  assert.match(state.title, /不换版本/);
  assert.match(state.title, /待批准的那一版不会因此启用/);

  const approved = enableState(info({ enabled: false }));
  assert.ok("fingerprint" in approved);
  assert.equal(approved.fingerprint, FP_OLD);

  // Nothing approved yet: there is nothing to re-enable, and the waiting version is only reachable
  // through the approval card.
  const never = enableState(info({ enabled: false, current: undefined, pending: pending(FP_NEW) }));
  assert.ok("reason" in never);
  assert.match(never.reason, /还没有批准过任何版本/);

  const on = enableState(info({ enabled: true }));
  assert.ok("reason" in on);
  assert.match(on.reason, /已经启用/);
});

test("after approving A, a new B opens as ready and submits B", () => {
  // What the screen does while A is approved: seen is cleared, so A is not "what the person saw".
  const approvedA = info({ pending: pending(FP_NEW) });
  assert.equal(seenAfter(undefined, FP_NEW, approvedA), undefined);

  // B arrives: it is the version to look at, not a replacement for something seen.
  const pendingB = info({ pending: pending(FP_NEWER) });
  const seen = seenAfter(undefined, FP_NEW, pendingB);
  const view = approvalView(pendingB, seen);
  assert.equal(view.kind, "ready");
  assert.equal(approveFingerprint(view), FP_NEWER, "the card prints and submits B");
  const html = renderToStaticMarkup(createElement(ApprovalPanel, { info: pendingB, view, diff, busy: false, onApprove: () => {}, onLook: () => {} }));
  assert.ok(html.includes('data-approval="ready"'));
  assert.equal(/data-fingerprint="([^"]+)"/.exec(html)?.[1], FP_NEWER);

  // Looking at B and then getting C is still a replacement: that is what stale is for.
  const third = "1d7f5c31".repeat(8);
  assert.equal(seenAfter(FP_NEWER, FP_NEW, info({ pending: pending(third) })), FP_NEWER);
  assert.equal(approvalView(info({ pending: pending(third) }), FP_NEWER).kind, "stale");
});

test("auto-trust says what turning it on does, and is refused while the source is broken", () => {
  assert.match(AUTO_TRUST_HINT, /一并批准并启用/);
  assert.match(AUTO_TRUST_HINT, /不再逐次确认/);
  assert.equal(autoTrustBlockedReason(info()), undefined);
  const broken = autoTrustBlockedReason(info({ error: "extension exceeds 50 MB" }));
  assert.match(broken ?? "", /exceeds 50 MB/);
});

test("the list shows the waiting fingerprint and why a switch is off", () => {
  const html = renderToStaticMarkup(
    createElement(ExtensionRow, {
      info: info({ pending: pending(FP_NEW) }),
      safeMode: false,
      busy: false,
      onEnable: () => {},
      onDisable: () => {},
    }),
  );
  assert.ok(html.includes("查看并启用"));
  assert.ok(html.includes(shortFingerprint(FP_NEW)));
  assert.ok(html.includes(shortFingerprint(FP_OLD)));
  // No enable while it is already running: 停用 is the action here, and approving happens on
  // the extension's own page.
  assert.ok(html.includes('data-action="disable"'));
  assert.ok(!html.includes('data-action="enable"'));
});

test("a stopped extension offers 启用 next to 查看并启用, but approving still goes through the card", () => {
  const html = renderToStaticMarkup(
    createElement(ExtensionRow, {
      info: info({ enabled: false, pending: pending(FP_NEW) }),
      safeMode: false,
      busy: false,
      onEnable: () => {},
      onDisable: () => {},
    }),
  );
  assert.ok(html.includes("查看并启用"), "the approval card is still the way to the waiting version");
  assert.ok(html.includes('data-action="enable"'), "the approved version can be turned back on");
  assert.match(html, /title="重新启用已批准的 3f9a1c02…/, "it says it re-enables the approved one");
});

test("an extension's settings block is wrapped, and its failure stays in that block", () => {
  const registration = (label: string, render: () => ReactNode) => ({
    extension: "standup",
    id: label,
    label,
    fingerprint: FP_OLD,
    render,
  });
  const html = renderToStaticMarkup(
    createElement(SettingsBlocks, {
      handlersFor: () => ({ onDisable: () => {}, onError: () => {} }),
      blocks: [
        registration("站会", () => createElement("p", null, "设置在这里")),
        registration(
          "坏掉的一块",
          () => {
            throw new Error("boom from the settings block");
          },
        ),
      ],
    }),
  );
  assert.ok(html.includes("设置在这里"));
  assert.ok(html.includes("boom from the settings block"));
  assert.ok(html.includes("停用"));
  assert.ok(html.includes("坏掉的一块"), "the failure card names the block");
});

test("the request for a plugin is a draft with the extension tools in it", () => {
  const draft = pluginRequestDraft("把今天的 run 汇总成一段话");
  assert.ok(draft.text.includes("把今天的 run 汇总成一段话"));
  assert.ok(draft.text.includes("extension_api"));
  assert.ok(draft.text.includes("extension_check"));
  assert.ok(draft.text.includes("extension_request_enable"));
  assert.ok(!draft.text.includes("\n发送"), "the text is a request, not a send");
  assert.match(pluginRequestDraft("").text, /先问我/);
});

test("version facts name the access and what it allows", () => {
  const facts = versionFacts(version(FP_NEW, { access: "write", files: 6, bytes: 3118 }));
  assert.equal(facts.access, "write");
  assert.equal(facts.files, "6 个文件 · 3.0 KB");
  assert.equal(facts.guiEntry, "gui/index.js");
  assert.match(accessText("write").detail, /能读也能改/);
  assert.match(accessText("read").detail, /只能读/);
});
