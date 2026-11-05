// The extension page's blocks, without JSX so the unit tests can render them as they are
// (node --test strips types but not JSX; boundary.ts is written the same way). `htm` binds to the
// page's own React, which is also what `ca.html` gives extensions.
//
// The approval card is the careful part: it prints the fingerprint it will submit, and offers the
// button only while that is still the version waiting for approval (docs/EXTENSIONS.md §6).

import htm from "htm";
import { createElement } from "react";
import type { ExtensionDiffResponse } from "@bindings/ExtensionDiffResponse";
import type { ExtensionInfo } from "@bindings/ExtensionInfo";
import type { ExtensionLogsResponse } from "@bindings/ExtensionLogsResponse";
import type { ExtensionVersion } from "@bindings/ExtensionVersion";
import { ExtensionBoundary, safeElement, type ExtensionFailureHandlers } from "../boundary.ts";
import type { PageRegistration, SettingsRegistration } from "../host";
import {
  accessText,
  approveBlockedReason,
  approveFingerprint,
  badges,
  bytesText,
  changeCount,
  changeLabel,
  diffLead,
  disableBlockedReason,
  enableState,
  groupsText,
  shortFingerprint,
  takenText,
  versionFacts,
  type ApprovalView,
  type Badge,
  type PageState,
} from "./model.ts";

const html = htm.bind(createElement);

/** How an extension's block reports a throw; the page builds these (Host.tsx, ui.tsx). */
export type HandlersFor = (extension: string, label?: string) => ExtensionFailureHandlers;

/** A hub or page error, in the page's own error style (the same as components.tsx ErrorBox). */
function errorBox(error?: string) {
  return error ? html`<div className="error">${error}</div>` : null;
}

/** The state chips of one extension. */
export function Chips({ badges: list }: { badges: Badge[] }) {
  return html`<div className="ext-badges">
    ${list.map((b) => html`<span key=${b.key} className=${`chip small ${b.tone === "off" ? "" : b.tone}`} title=${b.title}>${b.label}</span>`)}
  </div>`;
}

/** One version's facts, its description, its tools and the instructions it appends. */
export function VersionCard({ version }: { version: ExtensionVersion }) {
  const facts = versionFacts(version);
  const access = accessText(version.access);
  return html`<div className="ext-version">
    <dl className="ext-facts">
      <div>
        <dt>指纹</dt>
        <dd className="mono" title=${version.fingerprint}>${facts.short}</dd>
      </div>
      <div>
        <dt>接口</dt>
        <dd>api ${facts.api}</dd>
      </div>
      <div>
        <dt>access</dt>
        <dd>
          <span className=${`chip small ${version.access === "write" ? "warn" : ""}`}>${access.label}</span>
          <span className="muted small"> ${access.detail}</span>
        </dd>
      </div>
      <div>
        <dt>大小</dt>
        <dd>${facts.files}</dd>
      </div>
      <div>
        <dt>拍下时间</dt>
        <dd>${facts.takenAt}</dd>
      </div>
      <div>
        <dt>面板</dt>
        <dd className="mono">${facts.guiEntry ?? "没有面板"}</dd>
      </div>
    </dl>
    <div className="ext-desc">${version.description || "没有写说明。"}</div>
    <div className="ext-list-block">
      <div className="ext-list-head">
        <span>工具（${version.tools.length}）</span>
      </div>
      ${version.tools.length
        ? html`<ul className="ext-items">
            ${version.tools.map(
              (tool) => html`<li key=${tool.name} className="ext-item">
                <span className="mono">${tool.name}</span>
                <span className="ext-item-desc">${tool.description || "没有说明"}</span>
                <span className="ext-item-meta small muted">${groupsText(tool.groups)} · 最长 ${tool.timeout_secs} 秒</span>
              </li>`,
            )}
          </ul>`
        : html`<p className="muted small">这一版没有工具。</p>`}
    </div>
    <div className="ext-list-block">
      <div className="ext-list-head">
        <span>给模型的附加说明（${version.prompts.length}）</span>
      </div>
      ${version.prompts.length
        ? html`<ul className="ext-items">
            ${version.prompts.map(
              (prompt) => html`<li key=${`${prompt.file}:${prompt.groups.join(",")}`} className="ext-item">
                <span className="mono">${prompt.file}</span>
                <span className="ext-item-meta small muted">${bytesText(prompt.bytes)} · 给 ${groupsText(prompt.groups)}</span>
              </li>`,
            )}
          </ul>`
        : html`<p className="muted small">没有附加说明。</p>`}
    </div>
  </div>`;
}

/** The waiting version, in the person's hands: what it declares, what changes, and one button. */
export function ApprovalPanel({
  info,
  view,
  diff,
  diffError,
  busy,
  requester,
  onApprove,
  onLook,
}: {
  info: ExtensionInfo;
  view: ApprovalView;
  diff?: ExtensionDiffResponse;
  diffError?: string;
  busy: boolean;
  /** The name of the agent that asked; the hub records only its id. */
  requester?: string;
  onApprove: (fingerprint: string) => void;
  onLook: (fingerprint?: string) => void;
}) {
  const blocked = approveBlockedReason(info, view);
  const fingerprint = approveFingerprint(view);

  if (view.kind === "stale" || view.kind === "gone") {
    return html`<section className="ext-card ext-approve" data-approval=${view.kind}>
      <div className="ext-card-head">
        <h2>待批准的版本</h2>
      </div>
      <p className="ext-stale" role="alert">
        ${view.kind === "stale"
          ? html`你看到的这一版（${shortFingerprint(view.seen)}）已经不在待批准里了：源目录又变过，现在是 ${shortFingerprint(view.version.fingerprint)}。先看新的一版，再决定要不要启用。`
          : html`你看到的这一版（${shortFingerprint(view.seen)}）已经不在了：它可能已经批准过，或者被新的一版顶掉。`}
      </p>
      <div className="ext-approve-actions">
        ${view.kind === "stale"
          ? html`<button className="primary" data-action="look" onClick=${() => onLook(view.version.fingerprint)}>看新的一版</button>`
          : html`<button className="ghost" onClick=${() => onLook(undefined)}>知道了</button>`}
      </div>
    </section>`;
  }

  const version = view.kind === "ready" ? view.version : info.pending?.version;
  if (!version) {
    return html`<section className="ext-card ext-approve" data-approval="none">
      <div className="ext-card-head">
        <h2>待批准的版本</h2>
      </div>
      <p className="muted small">没有等待批准的版本。</p>
    </section>`;
  }

  return html`<section className="ext-card ext-approve" data-approval="ready">
    <div className="ext-card-head">
      <h2>待批准的版本</h2>
      <span className="muted small">
        ${info.pending?.requested_by ? `由 ${requester ?? info.pending.requested_by} 发起` : "hub 自己发现的改动"} ·${takenText(info.pending?.requested_at ?? version.taken_at)}
      </span>
    </div>
    <${VersionCard} version=${version} />
    ${diff?.access_changed
      ? html`<div className="ext-access-change" role="note">
          <strong>access 变了：</strong>
          ${info.current ? `${accessText(info.current.access).label} → ` : ""}${accessText(version.access).label}。${accessText(version.access).detail}
        </div>`
      : null}
    <div className="ext-diff">
      <div className="ext-list-head">
        <span>和现在这一版的差异</span>
        ${diff ? html`<span className="muted small">${diffLead(diff)}</span>` : null}
      </div>
      ${diffError
        ? errorBox(diffError)
        : !diff
          ? html`<p className="muted small">正在读取差异…</p>`
          : html`<div className="ext-diff-body">
              ${diff.files.length
                ? html`<ul className="ext-files">
                    ${diff.files.map(
                      (f) => html`<li key=${f.path}>
                        <span className=${`ext-change ${f.change}`}>${changeLabel(f.change)}</span>
                        <span className="mono">${f.path}</span>
                        <span className="muted small">${changeCount(f)}</span>
                      </li>`,
                    )}
                  </ul>`
                : html`<p className="muted small">文件内容没有变（可能只改了 access 或说明）。</p>`}
              ${diff.diff ? html`<pre className="ext-diff-text">${diff.diff}</pre>` : null}
              ${diff.truncated ? html`<p className="muted small">差异太长，hub 只给到这里；上面的文件清单还是完整的。</p>` : null}
            </div>`}
    </div>
    <div className="ext-approve-actions" data-fingerprint=${fingerprint ?? ""}>
      <button
        className="primary"
        data-action="approve"
        disabled=${busy || !!blocked || !fingerprint}
        title=${blocked ?? `批准并启用 ${shortFingerprint(fingerprint ?? "")}：之后调用和界面都用这一版`}
        onClick=${() => {
          if (fingerprint) onApprove(fingerprint);
        }}
      >
        批准并启用
      </button>
      <span className="ext-hint">
        ${blocked ?? `批准的是这一版：指纹 ${shortFingerprint(fingerprint ?? "")}。之后源目录再变，还会在这里出现新的一版。`}
      </span>
    </div>
  </section>`;
}

/** Both logs, without fetching: the page's own errors and the hub's log file. */
export function LogsPanel({
  name,
  pageLogs,
  hub,
  hubError,
  loading,
  onLoad,
}: {
  name: string;
  pageLogs: readonly string[];
  hub?: ExtensionLogsResponse;
  hubError?: string;
  loading: boolean;
  onLoad: () => void;
}) {
  return html`<section className="ext-card ext-logs" data-ext-logs=${name}>
    <div className="ext-card-head">
      <h2>日志与最近错误</h2>
      <span className="spacer" />
      <button className="ghost small" onClick=${onLoad} disabled=${loading}>${loading ? "读取中…" : "刷新 hub 日志"}</button>
    </div>
    <div className="ext-log-block">
      <div className="ext-log-title small">页面 · 加载或渲染这个扩展时出的错（${pageLogs.length} 条）</div>
      ${pageLogs.length
        ? html`<pre className="ext-log">${pageLogs.join("\n")}</pre>`
        : html`<p className="muted small">页面这边还没有出错记录。带面板的扩展渲染出错时会记在这里。</p>`}
    </div>
    <div className="ext-log-block">
      <div className="ext-log-title small">hub · ${name}.log 的最后 64 KiB</div>
      ${hubError
        ? errorBox(hubError)
        : !hub
          ? html`<p className="muted small">${loading ? "读取中…" : "还没读过。"}</p>`
          : hub.content
            ? html`<div className="ext-log-body">
                ${hub.truncated ? html`<p className="muted small">更早的内容没有带过来（hub 只给最后 64 KiB）；轮转出去的旧日志不在这里。</p>` : null}
                <pre className="ext-log">${hub.content}</pre>
              </div>`
            : html`<p className="muted small">还没有调用过它的工具，日志是空的。</p>`}
    </div>
  </section>`;
}

/** One extension in the list: its state, what it does now, and the two things one can do here. */
export function ExtensionRow({
  info,
  page,
  safeMode,
  busy,
  onEnable,
  onDisable,
}: {
  info: ExtensionInfo;
  page?: PageState;
  safeMode: boolean;
  busy: boolean;
  onEnable: () => void;
  onDisable: () => void;
}) {
  const pending = info.pending;
  // The description of what is waiting is the point of the row; the access is the one calls run with.
  const description = pending?.version.description ?? info.current?.description;
  const access = info.current?.access ?? pending?.version.access;
  const enable = enableState(info);
  const disableReason = disableBlockedReason(info);
  return html`<article className="ext-row" data-ext=${info.name}>
    <div className="ext-row-main">
      <div className="ext-row-head">
        <a className="ext-name" href=${`#/extensions/${encodeURIComponent(info.name)}`}>${info.name}</a>
        ${badges(info, { safeMode, page }).map(
          (b) => html`<span key=${b.key} className=${`chip small ${b.tone === "off" ? "" : b.tone}`} title=${b.title}>${b.label}</span>`,
        )}
      </div>
      <div className="ext-row-desc">${description || "没有写说明。"}</div>
      <div className="ext-row-meta small muted">
        ${info.current ? `正在运行 ${shortFingerprint(info.current.fingerprint)}` : "没有批准过任何版本"}${pending
          ? ` · 待批准 ${shortFingerprint(pending.version.fingerprint)}`
          : ""}${access ? ` · access ${accessText(access).label}` : ""}
      </div>
      ${info.error ? html`<div className="ext-row-error small" role="alert">${info.error}</div>` : null}
    </div>
    <div className="ext-row-actions">
      <a className="ext-link" href=${`#/extensions/${encodeURIComponent(info.name)}`} data-action="review">
        ${pending ? "查看并启用" : "查看"}
      </a>
      <button
        className=${info.enabled ? "ghost danger small" : "small"}
        data-action=${info.enabled ? "disable" : "enable"}
        disabled=${busy || (info.enabled ? !!disableReason : !("fingerprint" in enable))}
        title=${info.enabled ? disableReason : "reason" in enable ? enable.reason : enable.title}
        onClick=${info.enabled ? onDisable : onEnable}
      >
        ${info.enabled ? "停用" : "启用"}
      </button>
    </div>
  </article>`;
}

/** The way from "I want a plugin" to a conversation: a draft, never an automatic send. */
export function MakePlugin({ want, onWant, onMake }: { want: string; onWant: (value: string) => void; onMake: () => void }) {
  return html`<section className="ext-make">
    <div className="ext-make-text">
      <div className="ext-hint-title">让 agent 做一个插件</div>
      <p className="muted small">
        开一个新对话，把要求写进草稿，你看过再发。它会用 extension_api 看接口、写完用 extension_check 校验，再发起一条待批准记录——启用还是你在这里点。
      </p>
    </div>
    <div className="ext-make-form">
      <input
        value=${want}
        onChange=${(e: { target: { value: string } }) => onWant(e.target.value)}
        onKeyDown=${(e: { key: string }) => e.key === "Enter" && onMake()}
        placeholder="想让它做什么，例如：把今天的 run 汇总成一段话"
        aria-label="想让插件做什么"
      />
      <button className="primary" data-action="make-plugin" onClick=${onMake}>写进新对话</button>
    </div>
  </section>`;
}

/** One registered component, behind both layers of the boundary. */
export function RegisteredBody({ registration, handlersFor }: { registration: PageRegistration; handlersFor: HandlersFor }) {
  const handlers = handlersFor(registration.extension, registration.label);
  const children = safeElement(registration.extension, () => registration.render({ extension: registration.extension }), handlers);
  return html`<${ExtensionBoundary} key=${`${registration.extension}/${registration.id}@${registration.fingerprint}`} extension=${registration.extension} handlers=${handlers} children=${children} />`;
}

/** `ca.settings` blocks, shown under the extension list; each is titled inside its own card. */
export function SettingsBlocks({ blocks, handlersFor }: { blocks: readonly SettingsRegistration[]; handlersFor: HandlersFor }) {
  if (!blocks.length) return null;
  return html`<section className="settings-section ext-settings">
    <div className="settings-head">
      <h2>扩展设置</h2>
      <span className="chip small">${blocks.length}</span>
    </div>
    <p className="settings-lead">这些是已加载的扩展自己放上来的设置。扩展坏掉时，只有这一块会显示错误。</p>
    ${blocks.map(
      (block) => html`<div className="ext-panel" key=${`${block.extension}/${block.id}`} data-ext-panel=${`${block.extension}/${block.id}`}>
        <header className="ext-panel-head">
          <span className="ext-panel-title">${block.label}</span>
          <span className="muted small">扩展 ${block.extension}</span>
        </header>
        <div className="ext-panel-body">
          <${RegisteredBody} registration=${block} handlersFor=${handlersFor} />
        </div>
      </div>`,
    )}
  </section>`;
}
